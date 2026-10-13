use std::{
    backtrace::Backtrace,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    panic,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde::Serialize;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{self, Rotation};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

use crate::errors::AppResult;
use crate::forwarded_log::FORWARDED_LOG_FILE_PREFIX;
use crate::run_record::{ExitReason, RunRecordStore};

static LOG_FILE_PREFIX: &str = "diagnostics";
/// Panics are recorded here as well as in the rolling diagnostic log. The
/// rolling appender writes from a background thread, so a panic that takes the
/// process down with it can lose the buffered line; this file is written
/// synchronously from inside the hook and survives. The host-diagnostics
/// endpoint and the Support Bundle carry its newest 8 KB (ADR-0082).
pub static PANIC_FILE_NAME: &str = "panic.log";
static LOG_FILE_SUFFIX: &str = "log";

/// The `tracing` target for Admin lifecycle lines (process start, exit
/// reasons, ADR-0082), so a reader can pull the process boundaries out of the
/// diagnostic log with a single filter.
pub static LIFECYCLE_TARGET: &str = "openpiso_admin_lib::lifecycle";

/// Diagnostic files older than this are pruned on startup (ADR-0016 ~7-day cap).
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub target: String,
    pub pc_id: Option<String>,
    pub account_id: Option<String>,
}

/// Installs the diagnostic logging pipeline and returns the non-blocking
/// writer guard.
///
/// The caller must hold the guard for the life of the process (managed
/// application state): dropping it flushes the buffered lines to disk, which
/// is what lets the final lines before an exit reach the file. The appender
/// is built non-lossy, matching the Client, so lines are queued under
/// pressure rather than silently dropped (ADR-0082).
pub fn init_tracing(data_dir: &Path) -> Result<WorkerGuard, String> {
    let log_dir = data_dir.join("logs");
    fs::create_dir_all(&log_dir).map_err(|error| {
        format!(
            "could not create Admin log directory {}: {error}",
            log_dir.display()
        )
    })?;

    cleanup_old_logs(&log_dir, SystemTime::now());

    // Files are named `diagnostics.<date>.log` so the `.log` extension agrees
    // with the readers below; without the suffix the daily appender would emit
    // `diagnostics.<date>` and nothing would ever match.
    let file_appender = rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .build(&log_dir)
        .map_err(|error| format!("could not initialize diagnostic file appender: {error}"))?;
    let (non_blocking, guard) = NonBlockingBuilder::default()
        .lossy(false)
        .finish(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    registry()
        .with(
            // `flatten_event` lifts `message`, `pc_id`, and `account_id` to the
            // top level so the reader can find them; the default Json format
            // nests them under a `fields` object.
            fmt::Layer::new()
                .json()
                .flatten_event(true)
                .with_writer(non_blocking)
                .with_filter(env_filter.clone()),
        )
        .with(
            fmt::Layer::new()
                .with_writer(std::io::stdout)
                .with_filter(env_filter),
        )
        .try_init()
        .map_err(|error| format!("could not install diagnostic tracing subscriber: {error}"))?;

    Ok(guard)
}

/// Writes the startup line naming the app version and PID, so a restart is
/// visible in the diagnostic log itself rather than looking like an
/// uninterrupted run (ADR-0082).
pub fn log_process_start() {
    tracing::info!(
        target: LIFECYCLE_TARGET,
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "admin process started"
    );
}

/// Names why the process is about to end: the owner or OS closed a window.
pub fn log_window_close_requested(window_label: &str) {
    tracing::info!(
        target: LIFECYCLE_TARGET,
        window = window_label,
        "exit reason: window close requested"
    );
}

/// Names why the process is about to end: an application-initiated exit.
/// `exit_code` is `Some` only when the exit was requested programmatically.
pub fn log_application_exit_requested(exit_code: Option<i32>) {
    tracing::info!(
        target: LIFECYCLE_TARGET,
        exit_code = ?exit_code,
        "exit reason: application exit requested"
    );
}

/// Records panics to disk before the process dies.
///
/// Without this, a panic is invisible: the default hook writes to stderr, and a
/// release build is a `windows_subsystem = "windows"` binary with no console —
/// so the only evidence of the crash is Windows' "has stopped working" dialog.
/// The hook appends to `logs/panic.log` synchronously, then re-emits through
/// `tracing` so the record also reaches the rolling diagnostic log when the
/// process lives long enough to flush it. A panic that unwinds out of the
/// process-driving thread
/// — the one unwind path that ends the process — also completes the current
/// run's Run Record with the `panic` Exit Reason, synchronously, so the next
/// startup never misreads the crash as a suspected hard exit (ADR-0082).
///
/// That thread is identified by the [`std::thread::ThreadId`] captured when
/// the hook is installed. Admin installs it from Tauri's `setup` closure,
/// which runs on the thread that goes on to drive the event loop, so the
/// captured id is that thread's. Install it from anywhere else and the wrong
/// thread carries the authority. A
/// thread *name* proves nothing: any worker can be named `main`. Panics in
/// tokio tasks or worker threads are caught at their thread boundary, so the
/// process lives on; completing the Run Record there would end a run that is
/// still alive. Thread names stay in the written record as display evidence
/// only (ADR-0082).
pub fn install_panic_hook(log_dir: &Path, run_records: Arc<RunRecordStore>) {
    let panic_file = log_dir.join(PANIC_FILE_NAME);
    let process_driving_thread = std::thread::current().id();
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let record = format_panic_record(
            &now_rfc3339(),
            std::thread::current().name(),
            &panic_payload(info),
            info.location().map(|l| format!("{l}")).as_deref(),
            &Backtrace::force_capture().to_string(),
        );
        // Best-effort: a panicking process cannot usefully handle a failed
        // write, and panicking inside the hook would abort immediately.
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&panic_file)
        {
            let _ = file.write_all(record.as_bytes());
            let _ = file.flush();
        }
        if std::thread::current().id() == process_driving_thread {
            let _ = run_records.complete(ExitReason::Panic);
        }
        tracing::error!(
            target: "openpiso_admin_lib::panic",
            "panic: {record}"
        );
        previous(info);
    }));
}

fn panic_payload(info: &panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

fn format_panic_record(
    timestamp: &str,
    thread: Option<&str>,
    payload: &str,
    location: Option<&str>,
    backtrace: &str,
) -> String {
    format!(
        "--- panic {timestamp} thread={} location={} ---
{payload}
{backtrace}
",
        thread.unwrap_or("<unnamed>"),
        location.unwrap_or("<unknown>"),
    )
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn cleanup_old_logs(log_dir: &Path, now: SystemTime) {
    let Some(cutoff) = now.checked_sub(RETENTION) else {
        return;
    };
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if !is_prunable_log_file(&path) {
            continue;
        }
        if let Ok(metadata) = fs::metadata(&path) {
            if let Ok(modified) = metadata.modified() {
                if is_expired(modified, cutoff) {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
}

fn is_expired(modified: SystemTime, cutoff: SystemTime) -> bool {
    modified < cutoff
}

/// The reader predicate: only Admin's *own* diagnostic log. Forwarded
/// Diagnostics live in their own file and must never be interleaved into
/// this surface (ADR-0078).
fn is_diagnostic_file(path: &Path) -> bool {
    is_log_file_with_prefix(path, &[LOG_FILE_PREFIX])
}

/// The retention predicate: Admin's own diagnostics and the dedicated
/// forwarded log share the same rotation and seven-day retention (ADR-0078).
fn is_prunable_log_file(path: &Path) -> bool {
    is_log_file_with_prefix(path, &[LOG_FILE_PREFIX, FORWARDED_LOG_FILE_PREFIX])
}

/// A dated daily log file whose name starts with one of `prefixes`.
fn is_log_file_with_prefix(path: &Path, prefixes: &[&str]) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some(LOG_FILE_SUFFIX) {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| prefixes.iter().any(|prefix| name.starts_with(prefix)))
}

fn log_files(log_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_diagnostic_file(path))
        .collect();
    // Newest file name (latest date) first.
    files.sort_by(|a, b| b.as_os_str().cmp(a.as_os_str()));
    files
}

fn timestamp_in_range(timestamp: &str, from_date: Option<&str>, to_date: Option<&str>) -> bool {
    if from_date.is_none() && to_date.is_none() {
        return true;
    }
    let date = timestamp.get(..10).unwrap_or("");
    if let Some(from) = from_date {
        if date < from {
            return false;
        }
    }
    if let Some(to) = to_date {
        if date > to {
            return false;
        }
    }
    true
}

pub fn search_diagnostic_log(
    log_dir: &Path,
    filter: &DiagnosticFilterInput,
) -> AppResult<Vec<DiagnosticEntry>> {
    let files = log_files(log_dir);
    let limit = filter.limit.unwrap_or(200).clamp(1, 1_000) as usize;
    let mut entries = Vec::new();

    for file_path in &files {
        if entries.len() >= limit {
            break;
        }
        let Ok(file) = File::open(file_path) else {
            continue;
        };
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(entry) = parse_diagnostic_line(line, filter) {
                entries.push(entry);
                if entries.len() >= limit {
                    break;
                }
            }
        }
    }

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries)
}

fn parse_diagnostic_line(line: &str, filter: &DiagnosticFilterInput) -> Option<DiagnosticEntry> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let string_field = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);

    let level = string_field("level").unwrap_or_default();
    let timestamp = string_field("timestamp").unwrap_or_default();
    let message = string_field("message").unwrap_or_default();
    let target = string_field("target").unwrap_or_default();
    let pc_id = string_field("pc_id");
    let account_id = string_field("account_id");

    if !timestamp_in_range(
        &timestamp,
        filter.from_date.as_deref(),
        filter.to_date.as_deref(),
    ) {
        return None;
    }
    if let Some(f_level) = filter.level.as_deref() {
        if !level.eq_ignore_ascii_case(f_level) {
            return None;
        }
    }
    if let Some(search) = filter.search.as_deref() {
        let search = search.to_lowercase();
        if !message.to_lowercase().contains(&search) && !target.to_lowercase().contains(&search) {
            return None;
        }
    }
    if let Some(f_pc) = filter.pc_id.as_deref() {
        if pc_id.as_deref() != Some(f_pc) {
            return None;
        }
    }
    if let Some(f_account) = filter.account_id.as_deref() {
        if account_id.as_deref() != Some(f_account) {
            return None;
        }
    }

    Some(DiagnosticEntry {
        timestamp,
        level,
        message,
        target,
        pc_id,
        account_id,
    })
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DiagnosticFilterInput {
    pub limit: Option<i64>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub pc_id: Option<String>,
    pub account_id: Option<String>,
    pub level: Option<String>,
    pub search: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_record::RUN_RECORDS_FILE_NAME;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn tracing_guard_flushes_the_final_line_to_disk() {
        let dir = tempdir().expect("temp dir");
        let guard = init_tracing(dir.path()).expect("Admin tracing should initialize");

        tracing::info!(
            target: LIFECYCLE_TARGET,
            marker = "lifecycle-flush-issue-373",
            "the final line before exit"
        );
        drop(guard);

        let log_dir = dir.path().join("logs");
        let contents: String = log_files(&log_dir)
            .iter()
            .filter_map(|path| fs::read_to_string(path).ok())
            .collect();
        assert!(
            contents.contains("lifecycle-flush-issue-373"),
            "final buffered line should be flushed to the diagnostic file: {contents}"
        );
    }

    #[test]
    fn panic_record_names_thread_payload_and_location() {
        let record = format_panic_record(
            "2026-08-31T00:00:00Z",
            Some("coin-pipeline"),
            "attempt to subtract with overflow",
            Some("src/timers.rs:42:9"),
            "0: openpiso_admin_lib::timers::tick",
        );

        assert!(record.contains("thread=coin-pipeline"), "{record}");
        assert!(record.contains("location=src/timers.rs:42:9"), "{record}");
        assert!(
            record.contains("attempt to subtract with overflow"),
            "{record}"
        );
        assert!(
            record.contains("openpiso_admin_lib::timers::tick"),
            "{record}"
        );
    }

    /// The panic hook is process-global; these tests must not interleave.
    static PANIC_HOOK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Puts back the global panic hook the test found. `take_hook` resets to
    /// the *default* hook, which would silently discard the harness's own
    /// hook for every later test in the process; this keeps it, and keeps it
    /// reachable so `install_panic_hook` still chains onto it (ADR-0082).
    struct PanicHookGuard {
        previous: Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Sync + Send + 'static>,
    }

    impl PanicHookGuard {
        fn hold() -> Self {
            let previous: Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Sync + Send + 'static> =
                Arc::from(panic::take_hook());
            let chained = previous.clone();
            panic::set_hook(Box::new(move |info| chained(info)));
            Self { previous }
        }
    }

    impl Drop for PanicHookGuard {
        fn drop(&mut self) {
            let previous = self.previous.clone();
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }

    #[test]
    fn a_worker_thread_panic_writes_panic_log_but_leaves_the_run_open() {
        let _hook_serial = PANIC_HOOK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let _hook_guard = PanicHookGuard::hold();
        let dir = tempdir().expect("temp dir");
        let run_records = test_run_records(&dir);
        run_records
            .start(env!("CARGO_PKG_VERSION"))
            .expect("run record starts");
        install_panic_hook(dir.path(), run_records.clone());

        // Deliberately named `main`: any worker may carry that name, and the
        // name must carry no lifecycle authority.
        let worker = std::thread::Builder::new()
            .name("main".to_owned())
            .spawn(|| panic!("kiosk exploded"))
            .expect("thread spawns");
        assert!(worker.join().is_err(), "the worker panic must unwind");

        let written = fs::read_to_string(dir.path().join(PANIC_FILE_NAME))
            .expect("panic hook should have written panic.log");
        assert!(written.contains("kiosk exploded"), "{written}");
        assert!(written.contains("thread=main"), "{written}");

        // The panic never reached the thread the hook was installed on, so
        // the process lives and the run must stay open: a still-alive process
        // must never read back as ended.
        let history = run_records.load_history();
        let record = history.last().expect("started record");
        assert_eq!(record.ended_at, None);
        assert_eq!(record.exit_reason, None);
    }

    #[test]
    fn a_panic_on_the_installing_thread_completes_the_run_record() {
        let _hook_serial = PANIC_HOOK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let _hook_guard = PanicHookGuard::hold();
        let dir = tempdir().expect("temp dir");
        let run_records = test_run_records(&dir);
        run_records
            .start(env!("CARGO_PKG_VERSION"))
            .expect("run record starts");
        install_panic_hook(dir.path(), run_records.clone());

        // This thread installed the hook, so it stands in for the thread that
        // drives the real process, whatever that thread happens to be named.
        let died = std::panic::catch_unwind(|| panic!("kiosk blowup"));
        assert!(died.is_err(), "the panic must unwind");

        let history = run_records.load_history();
        let record = history.last().expect("started record");
        assert_eq!(record.exit_reason, Some(ExitReason::Panic));
        assert!(
            record.ended_at.is_some(),
            "the panicked run must not read back as a suspected hard exit"
        );
    }

    #[test]
    fn retention_sweep_leaves_the_run_record_file_alone() {
        let dir = tempdir().expect("temp dir");
        let run_records = test_run_records(&dir);
        run_records
            .start(env!("CARGO_PKG_VERSION"))
            .expect("run record starts");
        let log_dir = dir.path().join("logs");
        fs::create_dir_all(&log_dir).expect("log dir creates");

        // The sweep's predicate can never match a file outside `logs/`, and
        // the Run Record file deliberately lives outside it (ADR-0082).
        cleanup_old_logs(
            &log_dir,
            SystemTime::now() + RETENTION + Duration::from_secs(60),
        );

        assert!(
            !run_records.load_history().is_empty(),
            "the retention sweep must never remove Run Records"
        );
        assert!(!is_prunable_log_file(Path::new(RUN_RECORDS_FILE_NAME)));
    }

    fn test_run_records(dir: &tempfile::TempDir) -> Arc<RunRecordStore> {
        Arc::new(RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        ))
    }

    fn empty_filter() -> DiagnosticFilterInput {
        DiagnosticFilterInput {
            limit: None,
            from_date: None,
            to_date: None,
            pc_id: None,
            account_id: None,
            level: None,
            search: None,
        }
    }

    fn sample_line() -> &'static str {
        // Matches the flattened JSON the appender now writes.
        r#"{"timestamp":"2026-06-21T10:00:00.000000Z","level":"WARN","target":"openpiso_client","message":"Connection lost: timeout","pc_id":"pc-01","account_id":"acct-9"}"#
    }

    #[test]
    fn parses_flattened_top_level_fields() {
        let entry = parse_diagnostic_line(sample_line(), &empty_filter())
            .expect("a valid line should parse");
        assert_eq!(entry.level, "WARN");
        assert_eq!(entry.message, "Connection lost: timeout");
        assert_eq!(entry.target, "openpiso_client");
        assert_eq!(entry.pc_id.as_deref(), Some("pc-01"));
        assert_eq!(entry.account_id.as_deref(), Some("acct-9"));
    }

    #[test]
    fn applies_pc_account_level_search_and_date_filters() {
        let line = sample_line();

        let mut by_pc = empty_filter();
        by_pc.pc_id = Some("pc-02".to_owned());
        assert!(parse_diagnostic_line(line, &by_pc).is_none());

        let mut by_account = empty_filter();
        by_account.account_id = Some("acct-9".to_owned());
        assert!(parse_diagnostic_line(line, &by_account).is_some());

        let mut by_level = empty_filter();
        by_level.level = Some("warn".to_owned());
        assert!(parse_diagnostic_line(line, &by_level).is_some());
        by_level.level = Some("error".to_owned());
        assert!(parse_diagnostic_line(line, &by_level).is_none());

        let mut by_search = empty_filter();
        by_search.search = Some("LOST".to_owned());
        assert!(parse_diagnostic_line(line, &by_search).is_some());
        by_search.search = Some("nope".to_owned());
        assert!(parse_diagnostic_line(line, &by_search).is_none());

        let mut by_date = empty_filter();
        by_date.from_date = Some("2026-06-22".to_owned());
        assert!(parse_diagnostic_line(line, &by_date).is_none());
        by_date.from_date = Some("2026-06-20".to_owned());
        assert!(parse_diagnostic_line(line, &by_date).is_some());
    }

    #[test]
    fn short_timestamp_does_not_panic() {
        let line = r#"{"timestamp":"x","level":"INFO","target":"t","message":"m"}"#;
        let mut filter = empty_filter();
        filter.from_date = Some("2026-06-20".to_owned());
        // Out of range, but must not panic on the sub-slice.
        assert!(parse_diagnostic_line(line, &filter).is_none());
    }

    #[test]
    fn search_reads_only_suffixed_diagnostic_files() {
        let dir = std::env::temp_dir().join(format!("openpiso-diag-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let mut good = File::create(dir.join("diagnostics.2026-06-21.log")).expect("good file");
        writeln!(good, "{}", sample_line()).expect("write good line");
        // Wrong extension / wrong prefix must be ignored.
        fs::write(dir.join("diagnostics.2026-06-21"), sample_line()).expect("no-ext file");
        fs::write(dir.join("other.2026-06-21.log"), sample_line()).expect("foreign file");

        let found = search_diagnostic_log(&dir, &empty_filter()).expect("search succeeds");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "Connection lost: timeout");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_keeps_recent_and_targets_only_diagnostic_files() {
        let dir = std::env::temp_dir().join(format!("openpiso-diag-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let diag = dir.join("diagnostics.2026-06-21.log");
        let foreign = dir.join("openpiso.db");
        fs::write(&diag, sample_line()).expect("diag file");
        fs::write(&foreign, "not a log").expect("foreign file");

        // Real-time cutoff: nothing is old enough to prune.
        cleanup_old_logs(&dir, SystemTime::now());
        assert!(diag.exists(), "recent diagnostics must be kept");

        // Force every diagnostic file past the retention window by advancing the
        // clock; the non-diagnostic file must be left untouched.
        cleanup_old_logs(
            &dir,
            SystemTime::now() + RETENTION + Duration::from_secs(60),
        );
        assert!(!diag.exists(), "expired diagnostics must be pruned");
        assert!(
            foreign.exists(),
            "non-diagnostic files must never be touched"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_predicate_respects_cutoff() {
        let cutoff = SystemTime::now();
        assert!(is_expired(cutoff - Duration::from_secs(1), cutoff));
        assert!(!is_expired(cutoff + Duration::from_secs(1), cutoff));
    }

    #[test]
    fn forwarded_files_are_pruned_but_never_read_as_admin_diagnostics() {
        assert!(is_prunable_log_file(Path::new("forwarded.2026-09-05.log")));
        assert!(is_prunable_log_file(Path::new(
            "diagnostics.2026-09-05.log"
        )));
        assert!(!is_prunable_log_file(Path::new("openpiso.db")));
        assert!(!is_prunable_log_file(Path::new("panic.log")));

        // The reader predicate deliberately does not match the forwarded
        // file: forwarded lines must never be interleaved into Admin's own
        // diagnostic surface.
        assert!(is_diagnostic_file(Path::new("diagnostics.2026-09-05.log")));
        assert!(!is_diagnostic_file(Path::new("forwarded.2026-09-05.log")));
    }
}

//! Local Client diagnostics for installed builds (ADR-0016).

use std::{
    backtrace::Backtrace,
    fs::{self, OpenOptions},
    io::Write,
    panic,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        LazyLock,
    },
    time::{Duration, Instant, SystemTime},
};

use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{self, Rotation},
};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

const LOG_FILE_PREFIX: &str = "client";
const LOG_FILE_SUFFIX: &str = "log";
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_LOG_FILES: usize = 8;
/// Panics are recorded here as well as in the rolling diagnostic log. The
/// rolling appender writes from a background thread, so a panic that takes the
/// process down with it can lose the buffered line; this file is written
/// synchronously from inside the hook and survives.
const PANIC_FILE_NAME: &str = "panic.log";

/// Wall clock for the whole launch. Read it once at the top of `run()` so
/// every later `startup_milestone` is measured from the same origin; a boot
/// that shows a blank window for seconds is otherwise indistinguishable from
/// one that renders instantly.
static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);
static FRONTEND_READY_LOGGED: AtomicBool = AtomicBool::new(false);

/// Per-launch lifetime discriminator (issue #368). Fixed once per process,
/// at first use, so every lifecycle event in one launch carries the same
/// token; a fresh launch — even one the OS gave the same PID — derives a
/// different one. Diagnostic context only: it is not identity, not
/// authentication, and carries no authority.
static LAUNCH_TOKEN: LazyLock<String> =
    LazyLock::new(|| launch_token_for(std::process::id(), SystemTime::now()));

/// Start the launch clock. Call once, as early in `run()` as possible.
pub fn mark_process_start() {
    let _ = *PROCESS_START;
}

/// Milliseconds since `mark_process_start`.
pub fn since_process_start_ms() -> u64 {
    PROCESS_START.elapsed().as_millis() as u64
}

/// The OS process ID of this Client process. Paired with
/// [`launch_token`] it is the compact lifetime context every meaningful
/// Client lifecycle event carries (issue #368): the PID alone cannot tell a
/// long-lived process from a reused one, so a reused PID must never imply
/// one continuous process.
pub fn process_id() -> u32 {
    std::process::id()
}

/// The per-launch lifetime token: eight hex characters derived from the
/// launch wall-clock and the PID. Sibling tickets emit the same context on
/// their own admission/suppression/handoff evidence (ADR-0078's
/// process-lifetime context convention).
pub fn launch_token() -> &'static str {
    &LAUNCH_TOKEN
}

/// Pure derivation of the launch token so the format is pinned by tests:
/// fold the launch wall-clock into the PID and finish with a bit spread, so
/// two launches that somehow read the same clock still differ when their
/// PIDs differ, and a reused PID differs because the clock moved.
fn launch_token_for(pid: u32, now: SystemTime) -> String {
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let mut mixed = (nanos as u64) ^ u64::from(pid).rotate_left(32);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    mixed ^= mixed >> 33;
    format!("{:08x}", mixed as u32)
}

/// Record a named launch milestone with its offset from process start.
pub fn startup_milestone(milestone: &'static str) {
    tracing::info!(
        target: "openpiso_client::startup",
        milestone,
        elapsed_ms = since_process_start_ms(),
        process_id = process_id(),
        launch = launch_token(),
        "client startup milestone"
    );
}

/// Record the explicit frontend-ready handoff after the webview has booted and
/// Svelte has painted. Later calls are ignored so the log keeps one launch-time
/// datapoint.
pub fn mark_frontend_ready() {
    if FRONTEND_READY_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    startup_milestone("frontend_ready");
}

/// The directory [`init_tracing`] writes the Client's daily logs into, or
/// `None` when the platform exposes no `APPDATA`. The Diagnostic Request
/// slice reads its window back from the same directory. The startup
/// coordinator's harness override redirects the log directory along with
/// every other Client-local file so process tests stay self-contained.
pub(crate) fn log_dir() -> Option<PathBuf> {
    if let Some(dir) = crate::startup_coordinator::config_dir_override() {
        return Some(dir.join("logs"));
    }
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(|value| {
            PathBuf::from(value)
                .join("org.openpiso.client")
                .join("logs")
        })
}

/// The Client's dated log files under `log_dir`, oldest file name first.
pub(crate) fn client_log_files(log_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_client_log_file(path))
        .collect();
    files.sort();
    files
}

/// Initialize daily per-user logs and stdout mirroring.
pub fn init_tracing() -> Result<WorkerGuard, String> {
    let log_dir = log_dir().ok_or_else(|| "APPDATA is unavailable".to_owned())?;
    fs::create_dir_all(&log_dir).map_err(|error| {
        format!(
            "could not create Client log directory {}: {error}",
            log_dir.display()
        )
    })?;
    cleanup_old_logs(&log_dir, SystemTime::now());

    let appender = rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(&log_dir)
        .map_err(|error| format!("could not initialize Client log appender: {error}"))?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        .finish(appender);
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    registry()
        .with(
            fmt::Layer::new()
                .json()
                .flatten_event(true)
                .with_writer(writer)
                .with_filter(env_filter.clone()),
        )
        .with(
            fmt::Layer::new()
                .with_writer(std::io::stdout)
                .with_filter(env_filter),
        )
        // Diagnostic Forwarding (ADR-0078): capture WARN and ERROR lines into
        // the shared buffer when the release gate allows. `None` keeps the
        // subscriber shape unchanged in a gated-off development build.
        .with(crate::forwarding::forwarding_layer())
        .try_init()
        .map_err(|error| format!("could not install Client tracing subscriber: {error}"))?;

    Ok(guard)
}

/// Records panics to disk before the process dies (ADR-0082).
///
/// Without this, a panic is invisible: the default hook writes to stderr, and a
/// release build is a `windows_subsystem = "windows"` binary with no console —
/// so the only evidence of the crash is Windows' "has stopped working" dialog.
/// The hook appends to `logs/panic.log` synchronously, then re-emits through
/// `tracing` so the record also reaches the rolling diagnostic log — and, at
/// ERROR level, Diagnostic Forwarding — when the process lives long enough to
/// flush it. Install it even where no log directory resolves (`log_dir` is
/// `None`): the re-emit still happens, and only the file write is skipped. The
/// Client deliberately gains no Run Records to complete here (ADR-0082); the
/// hook only leaves evidence behind.
pub fn install_panic_hook(log_dir: Option<&Path>) {
    let panic_file = log_dir.map(|log_dir| log_dir.join(PANIC_FILE_NAME));
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
        if let Some(panic_file) = panic_file.as_deref() {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(panic_file)
            {
                let _ = file.write_all(record.as_bytes());
                let _ = file.flush();
            }
        }
        tracing::error!(
            target: "openpiso_client::panic",
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
        if !is_client_log_file(&path) {
            continue;
        }
        let expired = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}

fn is_client_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(&format!("{LOG_FILE_PREFIX}.")))
        .and_then(|name| name.strip_suffix(&format!(".{LOG_FILE_SUFFIX}")))
        .is_some_and(|date| !date.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn only_client_daily_logs_match_retention_filter() {
        assert!(is_client_log_file(Path::new("client.2026-08-01.log")));
        assert!(!is_client_log_file(Path::new("client.log")));
        assert!(!is_client_log_file(Path::new(
            "client-guard.2026-08-01.log"
        )));
        assert!(!is_client_log_file(Path::new("client.2026-08-01.txt")));
        // `panic.log` shares the directory with the rotating logs but must
        // never be swept by the retention pass.
        assert!(!is_client_log_file(Path::new("panic.log")));
    }

    #[test]
    fn panic_record_names_thread_payload_and_location() {
        let record = format_panic_record(
            "2026-09-09T00:00:00Z",
            Some("tokio-runtime-worker"),
            "attempt to subtract with overflow",
            Some("src/client_connection.rs:421:9"),
            "0: openpiso_client_lib::client_connection::tick",
        );

        assert!(record.contains("thread=tokio-runtime-worker"), "{record}");
        assert!(
            record.contains("location=src/client_connection.rs:421:9"),
            "{record}"
        );
        assert!(
            record.contains("attempt to subtract with overflow"),
            "{record}"
        );
        assert!(
            record.contains("openpiso_client_lib::client_connection::tick"),
            "{record}"
        );
    }

    /// The panic hook is process-global; these tests must not interleave.
    static PANIC_HOOK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_panic_file_timestamp_is_rfc3339() {
        chrono::DateTime::parse_from_rfc3339(&now_rfc3339())
            .expect("the panic record timestamp must parse as RFC3339");
    }

    #[test]
    fn repeated_panics_append_rather_than_overwrite() {
        let _hook_serial = PANIC_HOOK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("temp dir");
        install_panic_hook(Some(dir.path()));

        let first = std::panic::catch_unwind(|| panic!("first kiosk explosion"));
        assert!(first.is_err());
        let second = std::panic::catch_unwind(|| panic!("second kiosk explosion"));
        assert!(second.is_err());

        let written = fs::read_to_string(dir.path().join(PANIC_FILE_NAME))
            .expect("panic hook should have written panic.log");
        assert!(written.contains("first kiosk explosion"), "{written}");
        assert!(written.contains("second kiosk explosion"), "{written}");
        assert!(written.contains("diagnostics.rs"), "{written}");

        // The header names the timestamp, thread, and location, and the
        // backtrace follows the payload.
        let header = written.lines().next().expect("record header");
        assert!(header.starts_with("--- panic "), "{header}");
        assert!(header.contains("thread="), "{header}");
        assert!(header.contains("location="), "{header}");

        let _ = panic::take_hook();
    }

    #[test]
    fn the_panic_record_is_reemitted_through_tracing() {
        let _hook_serial = PANIC_HOOK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("temp dir");
        install_panic_hook(Some(dir.path()));

        let captured: Arc<std::sync::Mutex<Vec<u8>>> = Default::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer({
                let captured = Arc::clone(&captured);
                move || CapturingWriter(Arc::clone(&captured))
            })
            .finish();
        // A thread-local default wins over any global subscriber on this
        // thread, and the hook re-emits synchronously on the panicking thread.
        let _dispatch = tracing::subscriber::set_default(subscriber);

        let result = std::panic::catch_unwind(|| panic!("broadcast explosion"));
        assert!(result.is_err());

        let contents =
            String::from_utf8(captured.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .expect("subscriber output is UTF-8");
        assert!(contents.contains("ERROR"), "{contents}");
        assert!(contents.contains("panic: "), "{contents}");
        assert!(contents.contains("broadcast explosion"), "{contents}");

        let _ = panic::take_hook();
    }

    #[test]
    fn a_failed_panic_file_write_is_swallowed() {
        let _hook_serial = PANIC_HOOK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("temp dir");
        let missing = dir.path().join("no-such-directory");
        // `None` (no resolvable log directory) and a directory that does not
        // exist must both fail the write without panicking inside the hook.
        for log_dir in [None, Some(missing.as_path())] {
            install_panic_hook(log_dir);

            let result = std::panic::catch_unwind(|| panic!("doomed kiosk"));
            assert!(result.is_err());
            // Reaching this assertion at all is the proof: a panic inside a
            // panic hook aborts the process instead of unwinding.

            let _ = panic::take_hook();
        }
    }

    struct CapturingWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn at(secs: u64, nanos: u32) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
    }

    #[test]
    fn launch_token_is_compact_lowercase_hex_and_deterministic() {
        let token = launch_token_for(4242, at(1_780_000_000, 123_456_789));
        assert_eq!(token.len(), 8, "the convention is eight hex characters");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "eight lowercase hex characters: {token}"
        );
        assert_eq!(
            launch_token_for(4242, at(1_780_000_000, 123_456_789)),
            token,
            "the same launch inputs must derive the same token"
        );
    }

    #[test]
    fn a_reused_pid_does_not_repeat_a_token() {
        // Same PID, later wall clock: the reused-PID launch must not look
        // like a continuation of the earlier process.
        assert_ne!(
            launch_token_for(4242, at(1_780_000_000, 0)),
            launch_token_for(4242, at(1_780_000_001, 0)),
        );
        // Different PIDs at the same instant must differ too.
        assert_ne!(
            launch_token_for(4242, at(1_780_000_000, 0)),
            launch_token_for(4243, at(1_780_000_000, 0)),
        );
    }
}

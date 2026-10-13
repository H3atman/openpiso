//! Client startup coordination and the local Tamper Kill report handoff
//! (issues #366 / #367).
//!
//! This module owns the narrow local handoff/ingestion boundary that lets a
//! report-bearing launch hand a validated Tamper Kill report to the running
//! Client without initializing a second runtime. The future duplicate gate
//! (#366) calls into this boundary; this slice (#367) implements the
//! boundary, connects the receiver to normal Client operation, and proves
//! the sender from real child processes.
//!
//! ## Mechanism
//!
//! A file inbox beside the authoritative `tamper-kill-report.json`. The
//! sender (a launch that will not become the runtime) atomically writes one
//! small JSON file named by the report itself; the receiver (normal Client
//! operation) drains the inbox, merges each file into the authoritative
//! report, persists, and only then removes the inbox file. The sender never
//! touches the authoritative file, so an acknowledgement racing a new write
//! cannot erase the new report.
//!
//! The inbox is the smallest local-only mechanism that satisfies the
//! contract:
//!
//! - **Atomic persistence**: tmp-file-plus-rename writes (shared with
//!   `config_store`), so a successful sender exit means the report is
//!   durably recoverable even if the receiver is dead.
//! - **Bounded data**: [`MAX_HANDOFF_FILES`] files of at most
//!   [`MAX_HANDOFF_FILE_BYTES`] bytes each; the sender fails closed instead
//!   of dropping evidence.
//! - **Retry identity**: the file name is the report identity, so retrying
//!   one delivery rewrites the same file instead of duplicating it, and a
//!   bounded delivery ledger persisted alongside the authoritative report
//!   records every delivery already folded in. A delivery whose identity is
//!   in the ledger is a retry and is dropped, so neither coalescing, a
//!   restart, nor a completed acknowledgement can turn a repeated delivery
//!   into a new attempt. Distinct genuine reports keep the existing
//!   coalescing semantics.
//! - **Receiver lifetime changes**: inbox files survive receiver death; the
//!   next admitted owner drains them at startup before the first heartbeat.
//!
//! ## Trust boundary
//!
//! The inbox directory is the per-user Client config directory, writable by
//! the same standard user that can already write to the Guard pipe. Local
//! launch arguments and inbox files are **hints, never authority**: they
//! carry only the kill timestamp (stable report name) and attempt count,
//! are validated to positive integers at this Rust boundary, and attribution
//! stays the server's job (ADR-0048). This boundary claims no strength
//! beyond today's Guard launch arguments.
//!
//! ## Ordering guarantees
//!
//! - Persist before acknowledging: the sender exits success only after the
//!   inbox rename completes; the receiver removes an inbox file only after
//!   the merged authoritative report is persisted.
//! - Crash order: the authoritative file first, inbox delete second. The
//!   coalesced report and the delivery ledger are one atomic write, so a
//!   crash between the two leaves the inbox file for the successor, whose
//!   re-ingest is a no-op by delivery identity — the ledger already names
//!   that delivery.
//! - Capacity admission: the check-then-publish pair runs under an
//!   interprocess lock ([`ADMISSION_LOCK_NAME`]), so concurrent senders
//!   cannot each see spare capacity and collectively exceed
//!   [`MAX_HANDOFF_FILES`]. A sender that cannot determine capacity or
//!   claim the lock fails truthfully and retries later; a sender re-delivering
//!   an already-published report is admitted even at capacity.
//! - In-process serialization: inbox ingest and server-acknowledgement
//!   clearing share the connection-transition mutex, so clearing an older
//!   report cannot erase a newer one.
//! - Generation barrier: every ingested change bumps
//!   `tamper_kill_generation`, so an active SSE stream drops stale
//!   pre-report state until the authoritative heartbeat processes the report.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use crate::config_store::TamperKillReportState;
use crate::state::{PendingTamperKillReport, SharedState};

/// Guard relaunch flag carrying the kill timestamp and attempt count
/// (ADR-0048). Shared with the future duplicate gate; the sender and the
/// normal startup path parse the same arguments through [`parse_report_args`].
pub const REPORT_FLAG: &str = "--openpiso-tamper-kill";

/// Selects sender-only mode: validate the report arguments, submit them to
/// the local inbox, print a concise receipt, and exit without creating a
/// window, connecting to the Guard, sending a heartbeat, or starting
/// Diagnostic Forwarding. The future duplicate gate invokes the same submit
/// path; this flag exists so the sender is independently exercisable from
/// real child processes.
pub const HANDOFF_ONLY_FLAG: &str = "--openpiso-handoff-only";

/// Sender exit codes. `0` is a durable accept; anything else is a truthful
/// failure that must not be treated as a delivered report.
pub const HANDOFF_EXIT_ACCEPTED: i32 = 0;
/// The report arguments were missing or malformed. Retrying cannot succeed.
pub const HANDOFF_EXIT_INVALID: i32 = 2;
/// The validated report could not be persisted. Retrying later may succeed.
pub const HANDOFF_EXIT_UNPERSISTED: i32 = 3;

/// Directory holding validated-but-not-yet-ingested reports, beside the
/// authoritative `tamper-kill-report.json`.
pub const INBOX_DIR_NAME: &str = "tamper-handoff-inbox";
/// Upper bound on inbox files. Guard reports are rare; a full inbox means
/// the receiver has not drained for a very long time, so the sender fails
/// closed rather than dropping evidence.
pub const MAX_HANDOFF_FILES: usize = 32;
/// Upper bound on one inbox file's bytes. A valid report encodes to well
/// under a hundred bytes; anything larger is not a report.
pub const MAX_HANDOFF_FILE_BYTES: u64 = 4_096;
/// Upper bound on remembered delivery identities. Each entry is one
/// already-folded local delivery; the ledger is what lets a retry be
/// recognized after distinct reports coalesced, after a restart, and after
/// Admin acknowledged the report. Guard reports are rare, so this is far
/// above any real burst; the oldest identities are dropped first because a
/// retry always chases a recent delivery.
pub const MAX_DELIVERY_IDENTITIES: usize = 64;
/// Interprocess admission lock for the inbox capacity check. Creating a
/// directory is an atomic exclusive operation on every supported platform,
/// so exactly one sender at a time counts capacity and publishes. There is
/// exactly one such record, and it is reclaimed rather than accumulated.
pub const ADMISSION_LOCK_NAME: &str = ".admission.lock";
/// Names the process holding the admission lock. Its timestamp is what
/// staleness is measured against, and its contents make a stuck lock
/// diagnosable.
pub const ADMISSION_LOCK_HOLDER_NAME: &str = "holder";
/// How long a sender waits for the admission lock before failing truthfully.
const ADMISSION_LOCK_WAIT: Duration = Duration::from_secs(5);
/// A lock older than this belonged to a sender that died holding it.
const ADMISSION_LOCK_STALE: Duration = Duration::from_secs(30);
/// Poll interval while the lock is held by a live sender.
const ADMISSION_LOCK_POLL: Duration = Duration::from_millis(5);
/// Override for the config directory (tests and the process harness).
/// Production uses the platform default below.
pub const CONFIG_DIR_OVERRIDE_ENV: &str = "OPENPISO_CLIENT_CONFIG_DIR";
/// Client identifier shared with `tauri.conf.json`.
pub const CLIENT_IDENTIFIER: &str = "org.openpiso.client";

/// Validate Guard-supplied kill arguments at the Rust boundary.
/// Ordinary launches, report-free level checks, and malformed arguments
/// yield `None` and therefore can never manufacture a report.
pub fn parse_report_args(args: &[String]) -> Option<PendingTamperKillReport> {
    let index = args.iter().position(|arg| arg == REPORT_FLAG)?;
    let timestamp = args.get(index + 1)?.parse::<i64>().ok()?;
    let attempt_count = args.get(index + 2)?.parse::<i64>().ok()?;
    if timestamp <= 0 || attempt_count <= 0 {
        return None;
    }
    Some(PendingTamperKillReport {
        kill_timestamp_unix_ms: timestamp,
        attempt_count,
    })
}

/// Whether this launch requests sender-only handoff submission.
pub fn is_handoff_only_launch(args: &[String]) -> bool {
    args.iter().any(|arg| arg == HANDOFF_ONLY_FLAG)
}

/// Merge a newly arrived report into the pending one. Identical re-delivery
/// is a no-op (delivery identity); distinct reports coalesce with the
/// existing oldest-timestamp-wins semantics.
pub fn merge_reports(
    current: Option<PendingTamperKillReport>,
    incoming: Option<PendingTamperKillReport>,
) -> Option<PendingTamperKillReport> {
    match (current, incoming) {
        (Some(current), Some(newer)) => Some(current.coalesce(newer)),
        (Some(current), None) => Some(current),
        (None, Some(newer)) => Some(newer),
        (None, None) => None,
    }
}

/// The `CONFIG_DIR_OVERRIDE_ENV` value, when set to a non-empty path.
/// Tests and the process harness redirect every Client local file —
/// config, authoritative report, handoff inbox, logs — into one
/// isolated directory; production never sets the variable, so the
/// Tauri/platform directories below stay authoritative.
pub fn config_dir_override() -> Option<PathBuf> {
    std::env::var_os(CONFIG_DIR_OVERRIDE_ENV)
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
}

/// Resolve the config directory without a Tauri handle so the pre-runtime
/// sender and the running receiver agree on the inbox location. Tests
/// isolate persistence through [`CONFIG_DIR_OVERRIDE_ENV`].
pub fn resolve_config_dir() -> PathBuf {
    if let Some(dir) = config_dir_override() {
        return dir;
    }
    platform_config_dir()
}

fn platform_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            return PathBuf::from(app_data).join(CLIENT_IDENTIFIER);
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            let xdg = PathBuf::from(xdg);
            if !xdg.as_os_str().is_empty() {
                return xdg.join(CLIENT_IDENTIFIER);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join(CLIENT_IDENTIFIER);
        }
    }
    PathBuf::from(CLIENT_IDENTIFIER)
}

/// Inbox directory for a given config directory.
pub fn inbox_dir_for(config_dir: &Path) -> PathBuf {
    config_dir.join(INBOX_DIR_NAME)
}

/// Deterministic inbox file name: the report identity. Retrying one
/// delivery rewrites the same file instead of duplicating it.
fn inbox_file_name(report: &PendingTamperKillReport) -> String {
    format!(
        "report-{}-{}.json",
        report.kill_timestamp_unix_ms, report.attempt_count
    )
}

/// Durably submit a validated report to the local inbox. Success means the
/// report is recoverable by the running Client or its successor, not that
/// Admin acknowledged it. Never touches the authoritative report file.
pub fn submit_report(
    report: &PendingTamperKillReport,
    config_dir: &Path,
) -> Result<PathBuf, String> {
    if report.kill_timestamp_unix_ms <= 0 || report.attempt_count <= 0 {
        return Err("refusing to hand off an invalid Tamper Kill report".to_owned());
    }
    let inbox = inbox_dir_for(config_dir);
    fs::create_dir_all(&inbox).map_err(|error| {
        format!(
            "could not create Tamper Kill handoff inbox at {}: {error}",
            inbox.display()
        )
    })?;
    let path = inbox.join(inbox_file_name(report));
    let contents = serde_json::to_string_pretty(report)
        .map_err(|error| format!("could not encode report: {error}"))?;
    if contents.len() as u64 > MAX_HANDOFF_FILE_BYTES {
        return Err("refusing to hand off an oversized Tamper Kill report".to_owned());
    }
    // Capacity admission is a check-then-publish pair, so it has to be one
    // step across processes: without this lock several senders each see
    // spare capacity and all publish, taking the inbox past its documented
    // maximum (issue #371).
    let _admission = AdmissionLock::acquire(&inbox)?;
    if !path.exists() {
        let existing = count_inbox_files(&inbox)?;
        if existing >= MAX_HANDOFF_FILES {
            return Err(format!(
                "Tamper Kill handoff inbox is full ({existing} files); leaving the report unaccepted so it can be retried"
            ));
        }
    }
    crate::config_store::write_atomic_contents(&path, &contents).map_err(|error| {
        format!(
            "could not persist Tamper Kill handoff at {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

/// Count published inbox reports. An unreadable directory is a truthful
/// failure, never a count of zero: a sender must not admit a report it
/// cannot show there is room for.
fn count_inbox_files(inbox: &Path) -> Result<usize, String> {
    let entries = fs::read_dir(inbox).map_err(|error| {
        format!(
            "could not read the Tamper Kill handoff inbox at {}: {error}",
            inbox.display()
        )
    })?;
    let mut count = 0;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "could not enumerate the Tamper Kill handoff inbox at {}: {error}",
                inbox.display()
            )
        })?;
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            count += 1;
        }
    }
    Ok(count)
}

/// The inbox's interprocess admission lock. Held only across the capacity
/// check and the publish, which is a few filesystem operations.
///
/// Crash safety: a sender that dies holding the lock leaves one directory
/// behind, which the next sender reclaims once it is older than
/// [`ADMISSION_LOCK_STALE`]. There is never more than one lock record and
/// no per-attempt retry record, so a crash loop cannot grow storage.
struct AdmissionLock {
    path: PathBuf,
}

impl AdmissionLock {
    fn acquire(inbox: &Path) -> Result<Self, String> {
        let path = inbox.join(ADMISSION_LOCK_NAME);
        let deadline = Instant::now() + ADMISSION_LOCK_WAIT;
        loop {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let _ = fs::write(
                        path.join(ADMISSION_LOCK_HOLDER_NAME),
                        format!("{}", std::process::id()),
                    );
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "could not claim the Tamper Kill handoff admission lock at {}: {error}",
                        path.display()
                    ))
                }
            }
            if lock_is_stale(&path) {
                let _ = fs::remove_dir_all(&path);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "could not claim the Tamper Kill handoff admission lock at {} within {} seconds; leaving the report unaccepted so it can be retried",
                    path.display(),
                    ADMISSION_LOCK_WAIT.as_secs()
                ));
            }
            std::thread::sleep(ADMISSION_LOCK_POLL);
        }
    }
}

impl Drop for AdmissionLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Whether a sender staging file outlived any live sender. The bound is the
/// admission-lock wait, which is the longest a sender can be inside its
/// publish. An unreadable or future-dated timestamp reads as fresh, so a
/// live sender's staging file is never deleted under it.
fn staging_file_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > ADMISSION_LOCK_WAIT)
}

/// Whether an existing lock outlived any plausible holder. The holder file's
/// timestamp is the age; a lock whose holder file has not landed yet falls
/// back to the directory itself. An unreadable or future-dated timestamp is
/// treated as fresh, so a live holder is never evicted on a clock oddity.
fn lock_is_stale(path: &Path) -> bool {
    let holder = path.join(ADMISSION_LOCK_HOLDER_NAME);
    let probe = if holder.exists() {
        holder
    } else {
        path.to_owned()
    };
    fs::metadata(&probe)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > ADMISSION_LOCK_STALE)
}

/// One validated inbox file awaiting ingestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxReport {
    pub path: PathBuf,
    pub report: PendingTamperKillReport,
}

/// Read and validate every inbox file, oldest first. Corrupt, oversized, or
/// otherwise invalid files are removed after the fact by the caller-visible
/// [`drain_inbox`] cleanup so storage stays bounded; they never become
/// reports. A missing inbox is not an error.
pub fn drain_inbox(config_dir: &Path) -> Vec<InboxReport> {
    let inbox = inbox_dir_for(config_dir);
    let mut valid = Vec::new();
    let entries = match fs::read_dir(&inbox) {
        Ok(entries) => entries,
        Err(_) => return valid,
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            // Sender staging files, and anything else the sender never
            // published as a complete report. Only a *stale* staging file is
            // removed: a live sender's staging file is seconds old at most,
            // and deleting it under the sender would fail its rename for no
            // reason. Anything older outlived every bounded sender path.
            if path.extension().is_some_and(|ext| ext == "tmp") && staging_file_is_stale(&path) {
                let _ = fs::remove_file(&path);
            }
            continue;
        }
        let bytes = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(_) => continue,
        };
        if bytes > MAX_HANDOFF_FILE_BYTES {
            tracing::warn!(
                target: "openpiso_client::tamper_handoff",
                path = %path.display(),
                bytes,
                "removing oversized Tamper Kill handoff file; it cannot be a valid report"
            );
            let _ = fs::remove_file(&path);
            continue;
        }
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        match crate::config_store::decode_tamper_kill_report(&contents) {
            Ok(report) => valid.push(InboxReport { path, report }),
            Err(error) => {
                tracing::warn!(
                    target: "openpiso_client::tamper_handoff",
                    path = %path.display(),
                    %error,
                    "removing invalid Tamper Kill handoff file; it cannot be a valid report"
                );
                let _ = fs::remove_file(&path);
            }
        }
    }
    valid.sort_by(|a, b| {
        (
            a.report.kill_timestamp_unix_ms,
            a.report.attempt_count,
            &a.path,
        )
            .cmp(&(
                b.report.kill_timestamp_unix_ms,
                b.report.attempt_count,
                &b.path,
            ))
    });
    valid
}

/// Whether this delivery identity was already folded into the persisted
/// state. Identity is the delivered report itself — the pair the Guard
/// handed over — not the coalesced aggregate, which is why it survives
/// coalescing (issue #371).
fn already_delivered(state: &TamperKillReportState, delivery: PendingTamperKillReport) -> bool {
    state.delivered.contains(&delivery)
}

/// Record a delivery identity, keeping the ledger sorted, deduplicated, and
/// bounded to [`MAX_DELIVERY_IDENTITIES`] by dropping the oldest kills.
fn record_delivery(state: &mut TamperKillReportState, delivery: PendingTamperKillReport) {
    if state.delivered.contains(&delivery) {
        return;
    }
    state.delivered.push(delivery);
    state
        .delivered
        .sort_by_key(|entry| (entry.kill_timestamp_unix_ms, entry.attempt_count));
    let overflow = state
        .delivered
        .len()
        .saturating_sub(MAX_DELIVERY_IDENTITIES);
    if overflow > 0 {
        state.delivered.drain(0..overflow);
    }
}

/// Merge inbox files and any Guard relaunch argument into the authoritative
/// state. `persist` writes the whole state — coalesced report and delivery
/// ledger together, in one atomic write — and `remove_inbox` deletes an
/// inbox file; the production wiring passes the real file operations while
/// the harness observes the same ordering. Returns the new state and
/// whether a required persist succeeded (a failed persist leaves every
/// inbox file in place so the next drain retries it).
///
/// Deliveries whose identity is already in the ledger are retries: they are
/// dropped before folding, so neither a repeated delivery of one constituent
/// of a merged report nor a restart inside the persist-before-delete window
/// can inflate the attempt count. Genuinely new deliveries fold oldest-first,
/// so the kept kill timestamp is always the earliest death regardless of
/// arrival order.
fn merge_inbox_into(
    state: &TamperKillReportState,
    inbox: &[InboxReport],
    relaunched: Option<PendingTamperKillReport>,
    persist: &mut dyn FnMut(&TamperKillReportState) -> Result<(), String>,
    remove_inbox: &mut dyn FnMut(&Path),
) -> (TamperKillReportState, bool) {
    let deliveries = inbox
        .iter()
        .map(|inbound| inbound.report)
        .chain(relaunched)
        .filter(|delivery| !already_delivered(state, *delivery))
        .collect::<Vec<_>>();
    let mut candidates = deliveries.clone();
    candidates.extend(state.report());
    candidates.sort_by_key(|report| (report.kill_timestamp_unix_ms, report.attempt_count));
    let mut merged = None;
    for candidate in candidates {
        merged = merge_reports(merged, Some(candidate));
    }
    let mut next = state.clone().with_report(merged);
    for delivery in deliveries {
        record_delivery(&mut next, delivery);
    }
    if next != *state && persist(&next).is_err() {
        // Leave every inbox file in place: the report is still recoverable
        // and the next drain retries the persist.
        return (state.clone(), false);
    }
    for inbound in inbox {
        remove_inbox(&inbound.path);
    }
    (next, true)
}

/// Startup recovery: fold the inbox into the authoritative pending report
/// before the first heartbeat. When the inbox is empty and nothing new
/// arrived this is exactly the historical merge-then-conditionally-persist
/// behavior, including failing closed when the authoritative file cannot be
/// persisted; otherwise the merged report is persisted before any inbox file
/// is removed, so a crash mid-recovery stays recoverable.
pub fn recover_pending_report(
    main_path: Option<&Path>,
    config_dir: &Path,
    relaunched: Option<PendingTamperKillReport>,
) -> Result<Option<PendingTamperKillReport>, String> {
    let state = read_state(main_path);
    let inbox = drain_inbox(config_dir);
    let mut persist = |state: &TamperKillReportState| persist_main(main_path, state);
    let mut remove_inbox = |path: &Path| {
        let _ = fs::remove_file(path);
    };
    let (next, persisted_ok) =
        merge_inbox_into(&state, &inbox, relaunched, &mut persist, &mut remove_inbox);
    if !persisted_ok {
        return Err("could not persist recovered Tamper Kill report; inbox retained".to_owned());
    }
    if !inbox.is_empty() && next.report().is_some() {
        tracing::info!(
            target: openpiso_core::forwarding::CLIENT_TAMPER_TARGET,
            files = inbox.len(),
            "Tamper Kill report recovered through the local handoff; holding the connection until Admin acknowledges it"
        );
    }
    Ok(next.report())
}

/// The persisted authoritative state. A corrupt or unreadable file reads as
/// the empty state, matching the historical startup behavior of treating an
/// unusable report file as no pending report.
fn read_state(main_path: Option<&Path>) -> TamperKillReportState {
    let Some(path) = main_path else {
        return TamperKillReportState::default();
    };
    match crate::config_store::read_tamper_kill_state_file(path) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(
                target: "openpiso_client::config_store",
                path = %path.display(),
                %error,
                "could not read the Tamper Kill report file; treating it as no pending report"
            );
            TamperKillReportState::default()
        }
    }
}

/// Persist the merged startup state, failing closed like the historical
/// startup path when the authoritative file cannot be written. The report
/// and its delivery ledger land in one atomic write, so a crash can never
/// leave a report recorded as delivered without the report itself, or the
/// report without the identity that recognizes its retry.
fn persist_main(main_path: Option<&Path>, state: &TamperKillReportState) -> Result<(), String> {
    let Some(path) = main_path else {
        return Err("could not resolve Tamper Kill report path".to_owned());
    };
    crate::config_store::write_tamper_kill_state_file(path, state).map_err(|error| {
        format!(
            "could not persist Tamper Kill report at {}: {error}",
            path.display()
        )
    })
}

/// Runtime ingestion for the running Client: drain the inbox, merge into the
/// live pending state, persist before removing inbox files, and bump the
/// generation barrier when the pending report changed. Serialized with
/// server-acknowledgement clearing on the connection-transition mutex.
/// Returns the new pending report when it changed.
pub(crate) async fn ingest_due_reports(
    shared: &SharedState,
    config_dir: &Path,
    main_path: Option<&Path>,
) -> Option<PendingTamperKillReport> {
    let inbox = drain_inbox(config_dir);
    if inbox.is_empty() {
        return None;
    }
    let _transition = shared.connection_transition.lock().await;
    let current = *shared.pending_tamper_kill.read().await;
    // The live pending report is authoritative; the file supplies the
    // durable delivery ledger that recognizes a retry.
    let state = read_state(main_path).with_report(current);
    let mut persist = |state: &TamperKillReportState| {
        main_path
            .ok_or_else(|| "no authoritative report path".to_owned())
            .and_then(|path| {
                crate::config_store::write_tamper_kill_state_file(path, state).map_err(|error| {
                    format!(
                        "could not persist handed-off Tamper Kill report at {}: {error}",
                        path.display()
                    )
                })
            })
    };
    let mut remove_inbox = |path: &Path| {
        let _ = fs::remove_file(path);
    };
    let (next, persisted) = merge_inbox_into(&state, &inbox, None, &mut persist, &mut remove_inbox);
    let merged = next.report();
    let changed = merged != current;
    if !persisted {
        tracing::warn!(
            target: "openpiso_client::tamper_handoff",
            files = inbox.len(),
            "could not persist handed-off Tamper Kill report; inbox retained for retry"
        );
        return None;
    }
    if changed {
        *shared.pending_tamper_kill.write().await = merged;
        shared
            .tamper_kill_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            target: openpiso_core::forwarding::CLIENT_TAMPER_TARGET,
            files = inbox.len(),
            "Tamper Kill report received through the local handoff; holding the connection until Admin acknowledges it"
        );
    }
    if changed {
        merged
    } else {
        None
    }
}

/// Clear the pending report after the existing server acknowledgement.
/// Serialized with inbox ingestion so a delayed acknowledgement for an older
/// heartbeat cannot erase a newer handed-off report. Returns whether the
/// report was cleared.
pub(crate) async fn acknowledge_server_ack(
    shared: &SharedState,
    main_path: Option<&Path>,
    acknowledged: PendingTamperKillReport,
) -> bool {
    let _transition = shared.connection_transition.lock().await;
    if *shared.pending_tamper_kill.read().await != Some(acknowledged) {
        return false;
    }
    let Some(path) = main_path else {
        return false;
    };
    match crate::config_store::acknowledge_tamper_kill_report_file(path, acknowledged) {
        Ok(true) => {
            *shared.pending_tamper_kill.write().await = None;
            shared.tamper_kill_acknowledged.notify_waiters();
            true
        }
        Ok(false) => false,
        Err(error) => {
            tracing::warn!(
                target: "openpiso_client::config_store",
                "Could not clear tamper kill report after server ack: {error}"
            );
            false
        }
    }
}

/// Sender-only entrypoint for the top of the Client launch boundary. Returns
/// `Some(exit_code)` when this launch is a handoff submission (the caller
/// must exit immediately without initializing the runtime) and `None` for
/// ordinary launches, which proceed untouched.
pub fn maybe_run_handoff_sender(args: &[String]) -> Option<i32> {
    if !is_handoff_only_launch(args) {
        return None;
    }
    match parse_report_args(args) {
        None => {
            eprintln!(
                "openpiso-handoff: rejected invalid Tamper Kill report arguments; expected `{REPORT_FLAG} <kill_timestamp_unix_ms> <attempt_count>` with positive integers"
            );
            Some(HANDOFF_EXIT_INVALID)
        }
        Some(report) => {
            let config_dir = resolve_config_dir();
            match submit_report(&report, &config_dir) {
                Ok(_) => {
                    println!(
                        "openpiso-handoff: accepted kill_timestamp_unix_ms={} attempt_count={}",
                        report.kill_timestamp_unix_ms, report.attempt_count
                    );
                    Some(HANDOFF_EXIT_ACCEPTED)
                }
                Err(error) => {
                    eprintln!("openpiso-handoff: failed: {error}");
                    Some(HANDOFF_EXIT_UNPERSISTED)
                }
            }
        }
    }
}

/// Derive the Kill Report Age from the Client's two wall-clock readings.
/// A backwards clock step is a zero-age report, not an omitted report.
pub fn report_age_millis(kill_timestamp_unix_ms: i64, sent_at_unix_ms: i64) -> i64 {
    sent_at_unix_ms
        .saturating_sub(kill_timestamp_unix_ms)
        .max(0)
}

/// Heartbeat wire fields for a pending report: the stable timestamp name,
/// the freshly derived age, and the coalesced attempt count. Every retry
/// rederives the age without persisting it.
pub fn heartbeat_tamper_fields(
    pending: Option<PendingTamperKillReport>,
    sent_at_unix_ms: i64,
) -> (Option<String>, Option<i64>, Option<i64>) {
    match pending {
        Some(report) => (
            Some(unix_ms_to_rfc3339(report.kill_timestamp_unix_ms)),
            Some(report_age_millis(
                report.kill_timestamp_unix_ms,
                sent_at_unix_ms,
            )),
            Some(report.attempt_count),
        ),
        None => (None, None, None),
    }
}

/// Convert a Unix-millis timestamp to an RFC 3339 string suitable for the
/// heartbeat's `tamper_kill_timestamp` field.
pub fn unix_ms_to_rfc3339(unix_ms: i64) -> String {
    use chrono::TimeZone;
    // Saturate to chrono's supported range rather than panic on overflow.
    let clamped = unix_ms.clamp(
        chrono::Utc
            .timestamp_millis_opt(i64::MIN)
            .earliest()
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(i64::MIN),
        chrono::Utc
            .timestamp_millis_opt(i64::MAX)
            .latest()
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(i64::MAX),
    );
    match chrono::Utc.timestamp_millis_opt(clamped) {
        chrono::LocalResult::Single(dt) => dt.to_rfc3339(),
        _ => chrono::Utc
            .timestamp_opt(0, 0)
            .single()
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_owned()),
    }
}

/// Authoritative pending-report file inside an explicitly given config
/// directory. The harness and pre-Tauri paths use this; the running Client
/// resolves the same file through its Tauri handle.
pub fn main_report_path_for(config_dir: &Path) -> PathBuf {
    crate::config_store::tamper_report_path_for(config_dir)
}

/// Read the authoritative pending report, if any. Missing or unreadable
/// files read as no pending report, matching startup behavior.
pub fn read_main_report(path: &Path) -> Option<PendingTamperKillReport> {
    if !path.exists() {
        return None;
    }
    crate::config_store::decode_tamper_kill_report_file(path).ok()
}

/// Persist the authoritative pending report atomically.
pub fn write_main_report(path: &Path, report: &PendingTamperKillReport) -> Result<(), String> {
    crate::config_store::encode_tamper_kill_report_file(path, report).map_err(|error| {
        format!(
            "could not persist Tamper Kill report at {}: {error}",
            path.display()
        )
    })
}

/// Remove the authoritative report only when it is the report the server
/// acknowledged. A newer report survives a delayed acknowledgement for an
/// older heartbeat.
pub fn acknowledge_main_report(
    path: &Path,
    acknowledged: PendingTamperKillReport,
) -> Result<bool, String> {
    crate::config_store::acknowledge_tamper_kill_report_file(path, acknowledged)
}

/// Remove the inbox directory's published report files. Used by tests to
/// assert bounded retention; production drains through ingest/recovery.
pub fn clear_inbox(config_dir: &Path) -> io::Result<()> {
    let inbox = inbox_dir_for(config_dir);
    match fs::read_dir(&inbox) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    fs::remove_file(&path)?;
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(timestamp: i64, attempt: i64) -> PendingTamperKillReport {
        PendingTamperKillReport {
            kill_timestamp_unix_ms: timestamp,
            attempt_count: attempt,
        }
    }

    fn isolated_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openpiso-handoff-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create isolated handoff dir");
        dir
    }

    #[test]
    fn ordinary_launches_have_no_sender_outcome() {
        assert!(maybe_run_handoff_sender(&["openpiso-client.exe".to_owned()]).is_none());
        assert!(maybe_run_handoff_sender(&[
            "openpiso-client.exe".to_owned(),
            REPORT_FLAG.to_owned()
        ])
        .is_none());
        // A Guard relaunch without the handoff flag is a normal startup, not
        // a sender submission.
        assert!(maybe_run_handoff_sender(&[
            "openpiso-client.exe".to_owned(),
            REPORT_FLAG.to_owned(),
            "1750000000000".to_owned(),
            "3".to_owned(),
        ])
        .is_none());
    }

    #[test]
    fn report_free_level_checks_cannot_manufacture_a_report() {
        assert_eq!(
            parse_report_args(&[
                "openpiso-client.exe".to_owned(),
                HANDOFF_ONLY_FLAG.to_owned()
            ]),
            None
        );
        assert!(!is_handoff_only_launch(&["openpiso-client.exe".to_owned()]));
    }

    #[test]
    fn malformed_arguments_are_rejected_without_side_effects() {
        let dir = isolated_dir("malformed");
        for args in [
            vec![HANDOFF_ONLY_FLAG.to_owned(), REPORT_FLAG.to_owned()],
            vec![
                HANDOFF_ONLY_FLAG.to_owned(),
                REPORT_FLAG.to_owned(),
                "not-a-number".to_owned(),
                "1".to_owned(),
            ],
            vec![
                HANDOFF_ONLY_FLAG.to_owned(),
                REPORT_FLAG.to_owned(),
                "0".to_owned(),
                "1".to_owned(),
            ],
            vec![
                HANDOFF_ONLY_FLAG.to_owned(),
                REPORT_FLAG.to_owned(),
                "1750000000000".to_owned(),
                "-2".to_owned(),
            ],
        ] {
            assert_eq!(parse_report_args(&args), None);
        }
        assert!(!inbox_dir_for(&dir).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_redelivery_merges_to_itself() {
        let delivered = report(100, 2);
        assert_eq!(
            merge_reports(Some(delivered), Some(delivered)),
            Some(delivered)
        );
        assert_eq!(delivered.coalesce(delivered), delivered);
    }

    #[test]
    fn distinct_reports_keep_coalescing_semantics() {
        assert_eq!(
            merge_reports(Some(report(100, 2)), Some(report(500, 3))),
            Some(report(100, 3))
        );
        assert_eq!(
            merge_reports(None, Some(report(100, 1))),
            Some(report(100, 1))
        );
        assert_eq!(
            merge_reports(Some(report(100, 1)), None),
            Some(report(100, 1))
        );
        assert_eq!(merge_reports(None, None), None);
    }

    #[test]
    fn sender_persists_before_acknowledging_and_retry_rewrites_one_file() {
        let dir = isolated_dir("submit");
        let delivered = report(1_750_000_000_000, 3);
        let first = submit_report(&delivered, &dir).expect("submit report");
        assert!(first.exists());
        let second = submit_report(&delivered, &dir).expect("retry delivery");
        assert_eq!(first, second);
        assert_eq!(count_inbox_files(&inbox_dir_for(&dir)), Ok(1));
        let inbox = drain_inbox(&dir);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].report, delivered);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_reports_are_refused_without_touching_the_inbox() {
        let dir = isolated_dir("invalid");
        assert!(submit_report(&report(0, 1), &dir).is_err());
        assert!(submit_report(&report(100, 0), &dir).is_err());
        assert!(!inbox_dir_for(&dir).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_inbox_fails_closed_instead_of_dropping() {
        let dir = isolated_dir("full");
        let inbox = inbox_dir_for(&dir);
        fs::create_dir_all(&inbox).expect("create inbox");
        for index in 0..MAX_HANDOFF_FILES {
            fs::write(
                inbox.join(format!("report-{}-1.json", 1_000 + index)),
                serde_json::to_string(&report(1_000 + index as i64, 1)).expect("encode"),
            )
            .expect("seed inbox file");
        }
        assert!(submit_report(&report(9_999_999, 1), &dir).is_err());
        // Retrying an already-accepted report still succeeds: the file exists.
        assert!(submit_report(&report(1_000, 1), &dir).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_and_oversized_files_never_become_reports() {
        let dir = isolated_dir("corrupt");
        let inbox = inbox_dir_for(&dir);
        fs::create_dir_all(&inbox).expect("create inbox");
        fs::write(inbox.join("report-1-1.json"), "{not-json").expect("write corrupt");
        fs::write(inbox.join("report-2-1.json"), "x".repeat(8_192)).expect("write oversized");
        submit_report(&report(100, 1), &dir).expect("submit valid");
        let inbox_reports = drain_inbox(&dir);
        assert_eq!(inbox_reports.len(), 1);
        assert_eq!(inbox_reports[0].report, report(100, 1));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The durable inbox boundary enforces the same positive-integer
    /// contract as the command line: a zero or negative kill timestamp, or a
    /// non-positive attempt count, never becomes a pending report and never
    /// survives a drain (issue #371 finding 3).
    #[test]
    fn non_positive_inbox_timestamps_never_become_reports() {
        let dir = isolated_dir("invalid-timestamps");
        let main = crate::config_store::tamper_report_path_for(&dir);
        let inbox = inbox_dir_for(&dir);
        fs::create_dir_all(&inbox).expect("create inbox");
        let invalid = [
            (r#"{"kill_timestamp_unix_ms":0,"attempt_count":1}"#, "zero"),
            (
                r#"{"kill_timestamp_unix_ms":-1750000000000,"attempt_count":1}"#,
                "negative",
            ),
            (r#"{"kill_timestamp_unix_ms":0,"attempt_count":0}"#, "both"),
            (r#"{"attempt_count":1}"#, "missing-timestamp"),
            (r#"{"kill_timestamp_unix_ms":100}"#, "missing-attempts"),
            (
                r#"{"kill_timestamp_unix_ms":"100","attempt_count":1}"#,
                "typed",
            ),
        ];
        for (contents, label) in invalid {
            fs::write(inbox.join(format!("report-{label}.json")), contents).expect("seed");
        }
        assert!(
            drain_inbox(&dir).is_empty(),
            "an invalid inbox file must never be returned as a valid report"
        );
        assert_eq!(
            recover_pending_report(Some(&main), &dir, None).expect("recover"),
            None
        );
        assert!(
            !main.exists(),
            "no invalid file may become a pending report"
        );
        // Bounded storage: the invalid files are removed, not kept forever.
        assert_eq!(count_inbox_files(&inbox), Ok(0));
        // The sender refuses them at its own boundary too.
        assert!(submit_report(&report(0, 1), &dir).is_err());
        assert!(submit_report(&report(-1, 1), &dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Draining does not disturb a live sender's staging file, and still
    /// removes one left behind by a sender that died mid-publish.
    #[test]
    fn draining_leaves_live_staging_files_and_removes_abandoned_ones() {
        let dir = isolated_dir("staging");
        let inbox = inbox_dir_for(&dir);
        fs::create_dir_all(&inbox).expect("create inbox");
        let live = inbox.join("report-100-1.tmp");
        let abandoned = inbox.join("report-200-1.tmp");
        fs::write(&live, "{}").expect("write live staging file");
        fs::write(&abandoned, "{}").expect("write abandoned staging file");
        set_modified(
            &abandoned,
            SystemTime::now() - ADMISSION_LOCK_WAIT - Duration::from_secs(60),
        );
        assert!(drain_inbox(&dir).is_empty());
        assert!(
            live.exists(),
            "a live sender's staging file must survive a concurrent drain"
        );
        assert!(!abandoned.exists(), "an abandoned staging file is removed");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The admission lock is one bounded record: it is released after every
    /// submission, and a lock left behind by a dead sender is reclaimed
    /// rather than blocking the inbox forever.
    #[test]
    fn the_admission_lock_is_released_and_reclaimable() {
        let dir = isolated_dir("admission-lock");
        submit_report(&report(100, 1), &dir).expect("submit");
        let lock = inbox_dir_for(&dir).join(ADMISSION_LOCK_NAME);
        assert!(!lock.exists(), "the lock must not outlive a submission");
        // A live holder blocks admission, and the wait ends truthfully.
        fs::create_dir(&lock).expect("simulate a live holder");
        let holder = lock.join(ADMISSION_LOCK_HOLDER_NAME);
        fs::write(&holder, "1234").expect("write the holder record");
        let error = submit_report(&report(200, 1), &dir).expect_err("blocked");
        assert!(error.contains("admission lock"), "{error}");
        assert!(
            !inbox_dir_for(&dir)
                .join(inbox_file_name(&report(200, 1)))
                .exists(),
            "a blocked sender must not publish"
        );
        // Aged out: the next sender reclaims it.
        let stale = SystemTime::now() - ADMISSION_LOCK_STALE - Duration::from_secs(60);
        set_modified(&holder, stale);
        submit_report(&report(200, 1), &dir).expect("reclaim the stale lock and submit");
        assert!(!lock.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Backdate a path's modification time so the staleness policy can be
    /// exercised without waiting for it.
    fn set_modified(path: &Path, when: SystemTime) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for timestamping");
        file.set_modified(when).expect("backdate");
    }

    #[test]
    fn recovery_without_an_inbox_preserves_historical_behavior() {
        let dir = isolated_dir("recover-empty");
        let main = crate::config_store::tamper_report_path_for(&dir);
        // Nothing anywhere: no report, no file written.
        assert_eq!(
            recover_pending_report(Some(&main), &dir, None).expect("recover"),
            None
        );
        assert!(!main.exists());
        // Relaunch-only: persisted exactly as the old startup path did.
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(report(100, 2))).expect("recover"),
            Some(report(100, 2))
        );
        assert!(main.exists());
        // Identical relaunch does not rewrite or inflate.
        let before = fs::read_to_string(&main).expect("read main");
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(report(100, 2))).expect("recover"),
            Some(report(100, 2))
        );
        assert_eq!(fs::read_to_string(&main).expect("read main again"), before);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A historical report file — the two report fields and nothing else —
    /// still loads, still coalesces, and gains a ledger without losing the
    /// report it already held.
    #[test]
    fn a_historical_report_file_is_preserved_and_upgraded_in_place() {
        let dir = isolated_dir("historical");
        let main = crate::config_store::tamper_report_path_for(&dir);
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(&main, r#"{"kill_timestamp_unix_ms":100,"attempt_count":2}"#)
            .expect("seed historical report file");
        assert_eq!(read_main_report(&main), Some(report(100, 2)));
        // A genuinely new delivery still coalesces on top of it.
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(report(200, 1))).expect("recover"),
            Some(report(100, 3))
        );
        // And that delivery is now remembered, so its retry is inert.
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(report(200, 1))).expect("recover"),
            Some(report(100, 3))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_folds_inbox_reports_and_clears_only_after_persist() {
        let dir = isolated_dir("recover-inbox");
        let main = crate::config_store::tamper_report_path_for(&dir);
        submit_report(&report(200, 1), &dir).expect("submit inbox report");
        // A distinct relaunched report coalesces with the inbox one, oldest
        // timestamp wins.
        let merged =
            recover_pending_report(Some(&main), &dir, Some(report(300, 1))).expect("recover");
        assert_eq!(merged, Some(report(200, 2)));
        assert!(drain_inbox(&dir).is_empty());
        let persisted =
            crate::config_store::decode_tamper_kill_report_file(&main).expect("read merged");
        assert_eq!(persisted, report(200, 2));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_after_a_crash_between_persist_and_delete_is_a_noop() {
        let dir = isolated_dir("crash-window");
        let main = crate::config_store::tamper_report_path_for(&dir);
        let delivered = report(100, 1);
        submit_report(&delivered, &dir).expect("submit");
        let first = recover_pending_report(Some(&main), &dir, None).expect("recover");
        assert_eq!(first, Some(delivered));
        // Simulate the crash: the inbox file survives the persist.
        submit_report(&delivered, &dir).expect("rewrite inbox file");
        let second = recover_pending_report(Some(&main), &dir, None).expect("recover");
        assert_eq!(second, Some(delivered));
        assert!(drain_inbox(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Delivery identity survives coalescing: retrying either constituent of
    /// a merged report, repeatedly, changes nothing (issue #371 finding 1).
    #[test]
    fn retrying_a_constituent_of_a_merged_report_never_inflates_attempts() {
        let dir = isolated_dir("merged-retry");
        let main = crate::config_store::tamper_report_path_for(&dir);
        submit_report(&report(100, 1), &dir).expect("submit A");
        submit_report(&report(200, 1), &dir).expect("submit B");
        assert_eq!(
            recover_pending_report(Some(&main), &dir, None).expect("recover both"),
            Some(report(100, 2))
        );
        for _ in 0..3 {
            for constituent in [report(100, 1), report(200, 1)] {
                submit_report(&constituent, &dir).expect("retry a constituent");
                assert_eq!(
                    recover_pending_report(Some(&main), &dir, None).expect("recover retry"),
                    Some(report(100, 2)),
                    "a retry of {constituent:?} must not inflate the merged report"
                );
                assert!(drain_inbox(&dir).is_empty());
            }
        }
        // A genuinely distinct report still coalesces.
        submit_report(&report(300, 1), &dir).expect("submit C");
        assert_eq!(
            recover_pending_report(Some(&main), &dir, None).expect("recover C"),
            Some(report(100, 3))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A restart inside the persist-before-inbox-delete window with several
    /// reports present re-ingests every file and changes nothing.
    #[test]
    fn restart_inside_the_persist_window_re_ingests_every_file_as_a_noop() {
        let dir = isolated_dir("persist-window");
        let main = crate::config_store::tamper_report_path_for(&dir);
        let delivered = [report(100, 1), report(200, 1), report(300, 2)];
        for entry in delivered {
            submit_report(&entry, &dir).expect("submit");
        }
        let merged = recover_pending_report(Some(&main), &dir, None).expect("recover");
        assert_eq!(merged, Some(report(100, 3)));
        // The crash: every inbox file is back, exactly as a killed receiver
        // would have left them after the authoritative write landed.
        for entry in delivered {
            submit_report(&entry, &dir).expect("restore inbox file");
        }
        assert_eq!(
            recover_pending_report(Some(&main), &dir, None).expect("recover after restart"),
            merged
        );
        assert!(drain_inbox(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A Guard relaunch argument is a delivery too: repeating it across
    /// restarts, after other reports coalesced, is inert.
    #[test]
    fn a_repeated_relaunch_argument_is_a_retry_not_a_new_attempt() {
        let dir = isolated_dir("relaunch-retry");
        let main = crate::config_store::tamper_report_path_for(&dir);
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(report(200, 1))).expect("first"),
            Some(report(200, 1))
        );
        submit_report(&report(100, 1), &dir).expect("submit an older report");
        assert_eq!(
            recover_pending_report(Some(&main), &dir, None).expect("coalesce"),
            Some(report(100, 2))
        );
        // The same relaunch argument again, after coalescing moved the name.
        for _ in 0..3 {
            assert_eq!(
                recover_pending_report(Some(&main), &dir, Some(report(200, 1))).expect("retry"),
                Some(report(100, 2))
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// The ledger stays bounded, and the newest identities are the ones kept
    /// (a retry always chases a recent delivery).
    #[test]
    fn the_delivery_ledger_stays_bounded() {
        let dir = isolated_dir("ledger-bound");
        let main = crate::config_store::tamper_report_path_for(&dir);
        for index in 0..(MAX_DELIVERY_IDENTITIES as i64 + 10) {
            recover_pending_report(Some(&main), &dir, Some(report(1_000 + index, 1)))
                .expect("recover");
        }
        let state = crate::config_store::read_tamper_kill_state_file(&main).expect("read state");
        assert_eq!(state.delivered.len(), MAX_DELIVERY_IDENTITIES);
        assert_eq!(
            state.delivered.first().copied(),
            Some(report(1_000 + 10, 1)),
            "the oldest identities are the ones dropped"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ingest_feeds_pending_state_and_bumps_the_generation_once() {
        let dir = isolated_dir("ingest");
        let main = crate::config_store::tamper_report_path_for(&dir);
        let shared = SharedState::default();
        submit_report(&report(100, 1), &dir).expect("submit");
        let before = shared
            .tamper_kill_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        let ingested = ingest_due_reports(&shared, &dir, Some(&main)).await;
        assert_eq!(ingested, Some(report(100, 1)));
        assert_eq!(
            *shared.pending_tamper_kill.read().await,
            Some(report(100, 1))
        );
        assert_eq!(
            shared
                .tamper_kill_generation
                .load(std::sync::atomic::Ordering::Relaxed),
            before + 1
        );
        // Re-ingesting the same file after a simulated crash is a no-op with
        // no second generation bump.
        submit_report(&report(100, 1), &dir).expect("rewrite inbox file");
        assert_eq!(ingest_due_reports(&shared, &dir, Some(&main)).await, None);
        assert_eq!(
            shared
                .tamper_kill_generation
                .load(std::sync::atomic::Ordering::Relaxed),
            before + 1
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stale_server_ack_cannot_clear_a_newer_handed_off_report() {
        let dir = isolated_dir("stale-ack");
        let main = crate::config_store::tamper_report_path_for(&dir);
        let shared = SharedState::default();
        *shared.pending_tamper_kill.write().await = Some(report(100, 1));
        crate::config_store::encode_tamper_kill_report_file(&main, &report(100, 1))
            .expect("persist first");
        submit_report(&report(200, 1), &dir).expect("hand off newer");
        ingest_due_reports(&shared, &dir, Some(&main)).await;
        assert_eq!(
            *shared.pending_tamper_kill.read().await,
            Some(report(100, 2))
        );
        assert!(!acknowledge_server_ack(&shared, Some(&main), report(100, 1)).await);
        assert_eq!(
            *shared.pending_tamper_kill.read().await,
            Some(report(100, 2))
        );
        assert!(main.exists());
        assert!(acknowledge_server_ack(&shared, Some(&main), report(100, 2)).await);
        assert_eq!(*shared.pending_tamper_kill.read().await, None);
        // The acknowledged report is gone; the delivery ledger survives so a
        // late retry of an acknowledged delivery stays inert.
        let acknowledged_state =
            crate::config_store::read_tamper_kill_state_file(&main).expect("read state");
        assert_eq!(acknowledged_state.report(), None);
        assert!(acknowledged_state.delivered.contains(&report(200, 1)));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A delivery that arrives again after Admin acknowledged the report it
    /// belonged to has no new consequence: no pending report, no generation
    /// bump, nothing persisted (issue #371 finding 1).
    #[tokio::test]
    async fn a_retry_after_acknowledgement_has_no_new_consequence() {
        let dir = isolated_dir("ack-retry");
        let main = crate::config_store::tamper_report_path_for(&dir);
        let shared = SharedState::default();
        let delivered = report(100, 1);
        submit_report(&delivered, &dir).expect("hand off");
        assert_eq!(
            ingest_due_reports(&shared, &dir, Some(&main)).await,
            Some(delivered)
        );
        assert!(acknowledge_server_ack(&shared, Some(&main), delivered).await);
        let generation = shared
            .tamper_kill_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        // The Guard's sender retries the same delivery, twice, plus the same
        // report as a relaunch argument on the next startup.
        for _ in 0..2 {
            submit_report(&delivered, &dir).expect("retry");
            assert_eq!(ingest_due_reports(&shared, &dir, Some(&main)).await, None);
        }
        assert_eq!(*shared.pending_tamper_kill.read().await, None);
        assert_eq!(
            shared
                .tamper_kill_generation
                .load(std::sync::atomic::Ordering::Relaxed),
            generation
        );
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(delivered)).expect("recover"),
            None
        );
        // A genuinely new kill after the acknowledgement still lands.
        assert_eq!(
            recover_pending_report(Some(&main), &dir, Some(report(400, 1))).expect("recover new"),
            Some(report(400, 1))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn heartbeat_fields_rederive_age_without_persisting_it() {
        let pending = report(1_000, 1);
        let first = heartbeat_tamper_fields(Some(pending), 2_000);
        let retry = heartbeat_tamper_fields(Some(pending), 5_000);
        assert_eq!(first.0, retry.0);
        assert_eq!(first.1, Some(1_000));
        assert_eq!(retry.1, Some(4_000));
        assert_eq!(report_age_millis(4_500, 1_000), 0);
    }
}

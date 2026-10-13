//! Device-side Diagnostic Request (ADR-0078, #364).
//!
//! The top-up for a PC that is still alive: when the forwarded allowlist did
//! not carry enough, the owner asks this Client for more and Admin places a
//! single-use nonce on the Heartbeat response. The Client acts once per nonce
//! and uploads one windowed, byte-capped slice of its own full log — not the
//! whole day's file, not the retained set — through the existing batched
//! Forwarded Diagnostics endpoint, echoing the nonce so Admin stops
//! delivering the ask. What came back lands in Admin's forwarded log tagged
//! `client`, so the next Support Bundle includes it, and the slice's own
//! statement lines state plainly when the window or the byte cap truncated
//! it.
//!
//! The uploader is deliberately a separate loop from the Heartbeat — its own
//! HTTP client, its own cadence, like Diagnostic Forwarding — so building and
//! sending the slice can never delay or fail timer synchronisation.

use std::{collections::VecDeque, path::Path, sync::LazyLock, time::Duration};

use chrono::{DateTime, Utc};
use openpiso_core::forwarding::{
    truncate_message, ForwardedDiagnostic, ForwardedLevel, ForwardedSource,
    MAX_FORWARDED_BATCH_LINES,
};
use serde::Serialize;
use tracing::warn;

/// Target of the slice's own statement lines, so the Support Bundle can tell
/// the slice's frame from the Client's ordinary log lines.
pub(crate) const DIAGNOSTIC_REQUEST_TARGET: &str = "openpiso_client::diagnostic_request";

/// How far back the slice reaches, ending at the moment the Client acts.
/// One owner click must never become a whole-day upload; a quarter hour
/// covers the live symptom the ask was prompted by.
pub(crate) const SLICE_WINDOW_SECONDS: i64 = 900;

/// The slice's byte cap, measured on the serialised wire form of its lines.
/// This is what stops one owner click from stalling Heartbeats across the
/// shop; older lines within the window are dropped first, newest kept.
pub(crate) const SLICE_MAX_BYTES: usize = 64 * 1024;

/// Fallback cadence of the uploader loop; a fresh ask is picked up promptly
/// by [`SLICE_NOTIFY`] and this tick is only the safety net.
const TICK_SECONDS: u64 = 5;

/// Total request timeout for one slice batch, matching the forwarding
/// sender's stance: deliberately separate from the Heartbeat's client.
const SEND_TIMEOUT_SECONDS: u64 = 3;

/// The forwarder's own failure-note target — excluded from the forwarding
/// buffer, so a dead Admin cannot fill the ring with the uploader's own
/// complaints. Shared with the Diagnostic Forwarding sender.
use crate::forwarding::FORWARDING_TARGET;

static SLICE_STATE: LazyLock<std::sync::Mutex<SliceState>> = LazyLock::new(Default::default);
static SLICE_NOTIFY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

#[derive(Debug, Default)]
struct SliceState {
    /// The last nonce acted on, so a repeated Heartbeat response cannot cause
    /// a repeated upload: the Client acts exactly once per nonce.
    last_acted_nonce: Option<String>,
    /// A nonce awaiting its slice build at the next uploader tick.
    pending_nonce: Option<String>,
    /// The built slice still being uploaded (or retained for retry).
    work: Option<PendingSlice>,
}

#[derive(Debug)]
struct PendingSlice {
    nonce: String,
    batches: VecDeque<Vec<ForwardedDiagnostic>>,
}

impl SliceState {
    /// Act on a freshly delivered nonce: mark it acted-on and schedule the
    /// slice. Returns `false` when this nonce was already acted on, which is
    /// what makes a repeated Heartbeat response a no-op.
    fn act_on_nonce(&mut self, nonce: &str) -> bool {
        if self.last_acted_nonce.as_deref() == Some(nonce) {
            return false;
        }
        self.last_acted_nonce = Some(nonce.to_owned());
        self.pending_nonce = Some(nonce.to_owned());
        true
    }
}

fn state() -> std::sync::MutexGuard<'static, SliceState> {
    SLICE_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The Heartbeat loop saw the Diagnostic Request nonce on a response. Never
/// called with network or file work on the stack: the Heartbeat path only
/// records the ask, and the uploader builds and sends the slice on its own
/// task, so the upload cannot delay or fail Heartbeat handling.
pub(crate) fn on_heartbeat_nonce(nonce: &str) {
    if !crate::forwarding::forwarding_enabled() {
        return;
    }
    if state().act_on_nonce(nonce) {
        SLICE_NOTIFY.notify_one();
    }
}

/// The slice uploader loop. Runs alongside the Heartbeat loop in
/// `client_connection::run`, never inside it.
pub(crate) async fn run(
    shared: std::sync::Arc<crate::state::SharedState>,
    boot_mac: crate::client_connection::IdentityMac,
) {
    if !crate::forwarding::forwarding_enabled() {
        return;
    }
    let Some(log_dir) = crate::diagnostics::log_dir() else {
        warn!(
            target: FORWARDING_TARGET,
            "Diagnostic Request unavailable: the Client log directory could not be resolved"
        );
        return;
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(SEND_TIMEOUT_SECONDS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut interval = tokio::time::interval(Duration::from_secs(TICK_SECONDS));
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = SLICE_NOTIFY.notified() => {}
        }
        // Build a slice for a freshly delivered nonce, once per nonce. The
        // build freezes the window at act time so a retry re-sends the same
        // slice rather than a drifting one.
        let pending = { state().pending_nonce.take() };
        if let Some(nonce) = pending {
            let work = build_slice(
                &nonce,
                &log_dir,
                crate::client_connection::unix_time_millis(),
            );
            {
                state().work = Some(work);
            }
        }
        // Take the slice out while it uploads; what a failed send retains
        // goes back for the next attempt.
        let Some(work) = ({ state().work.take() }) else {
            continue;
        };
        let remaining = upload(&client, &shared, &boot_mac, &work).await;
        if !remaining.is_empty() {
            {
                state().work = Some(PendingSlice {
                    nonce: work.nonce,
                    batches: remaining,
                });
            }
        }
    }
}

#[derive(Serialize)]
struct SliceBatch<'a> {
    source: ForwardedSource,
    diagnostic_request_nonce: &'a str,
    lines: &'a [ForwardedDiagnostic],
}

/// Send the slice batch by batch; every batch echoes the nonce so Admin
/// consumes the ask on the first one. Returns whatever remains when a send
/// fails or is refused — the slice is retained for the next attempt, exactly
/// like a forwarded batch, and a failure here never reaches the Heartbeat.
async fn upload(
    client: &reqwest::Client,
    shared: &std::sync::Arc<crate::state::SharedState>,
    boot_mac: &crate::client_connection::IdentityMac,
    work: &PendingSlice,
) -> VecDeque<Vec<ForwardedDiagnostic>> {
    let mut batches = work.batches.clone();
    while let Some(batch) = batches.front().cloned() {
        let server_url = shared.config.read().await.server_url.clone();
        let mac = crate::client_connection::resolve_request_mac(boot_mac, &server_url);
        let url = format!(
            "{}/api/forwarded-diagnostics",
            server_url.trim_end_matches('/')
        );
        let payload = SliceBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: &work.nonce,
            lines: &batch,
        };
        let outcome = crate::client_connection::apply_mac_header(client.post(&url), &mac)
            .json(&payload)
            .send()
            .await;
        match outcome {
            // 2xx: these lines are on the Admin host; keep draining.
            Ok(response) if response.status().is_success() => {
                batches.pop_front();
            }
            Ok(response) => {
                warn!(
                    target: FORWARDING_TARGET,
                    "Admin refused the Diagnostic Request slice batch: {}",
                    response.status()
                );
                return batches;
            }
            Err(error) => {
                warn!(
                    target: FORWARDING_TARGET,
                    "Could not upload the Diagnostic Request slice: {error}"
                );
                return batches;
            }
        }
    }
    batches
}

/// Build the windowed, byte-capped slice answering one nonce: the newest
/// window lines within the byte cap, oldest dropped first, framed by
/// statement lines that say exactly what the window and the cap cut away.
/// The frame is uploaded even for an empty window, so a fulfilled ask is
/// never mistaken for a lost one.
fn build_slice(nonce: &str, log_dir: &Path, now_unix_ms: i64) -> PendingSlice {
    build_slice_with_cap(nonce, log_dir, now_unix_ms, SLICE_MAX_BYTES)
}

fn build_slice_with_cap(
    nonce: &str,
    log_dir: &Path,
    now_unix_ms: i64,
    slice_max_bytes: usize,
) -> PendingSlice {
    let window_start_unix_ms = now_unix_ms - SLICE_WINDOW_SECONDS * 1000;
    let (window_lines, excluded_earlier) = read_window(log_dir, window_start_unix_ms, now_unix_ms);

    // Keep the newest lines within the byte cap; the cap's own frame lines
    // are tiny and stay outside the accounting, so the statement can never
    // crowd out the evidence it describes.
    let mut kept: Vec<ForwardedDiagnostic> = Vec::new();
    let mut used_bytes = 0usize;
    let mut dropped_by_cap = 0usize;
    for line in window_lines.iter().rev() {
        let wire_bytes = serde_json::to_vec(line).map(|wire| wire.len()).unwrap_or(0);
        if !kept.is_empty() && used_bytes + wire_bytes > slice_max_bytes {
            dropped_by_cap += 1;
            continue;
        }
        used_bytes += wire_bytes;
        kept.push(line.clone());
    }
    kept.reverse();

    let mut lines = Vec::with_capacity(kept.len() + 3);
    lines.push(marker(
        now_unix_ms,
        format!(
            "Diagnostic Request slice (nonce {nonce}): window {} to {} UTC, {} of {} window lines uploaded",
            rfc3339(window_start_unix_ms),
            rfc3339(now_unix_ms),
            kept.len(),
            window_lines.len(),
        ),
    ));
    if dropped_by_cap > 0 {
        lines.push(marker(
            now_unix_ms,
            format!(
                "Diagnostic Request slice (nonce {nonce}): byte cap truncated the slice; {dropped_by_cap} older window lines were dropped"
            ),
        ));
    }
    if excluded_earlier > 0 {
        lines.push(marker(
            now_unix_ms,
            format!(
                "Diagnostic Request slice (nonce {nonce}): the window excluded {excluded_earlier} earlier log lines"
            ),
        ));
    }
    lines.extend(kept);

    let batches = lines
        .chunks(MAX_FORWARDED_BATCH_LINES)
        .map(<[ForwardedDiagnostic]>::to_vec)
        .collect();
    PendingSlice {
        nonce: nonce.to_owned(),
        batches,
    }
}

fn marker(now_unix_ms: i64, message: String) -> ForwardedDiagnostic {
    ForwardedDiagnostic::new(
        ForwardedLevel::Info,
        DIAGNOSTIC_REQUEST_TARGET,
        message,
        now_unix_ms,
    )
}

/// Read the Client's own daily logs and return the window's lines in capture
/// order plus how many earlier lines the window cut away. The two newest
/// files are the only ones that can hold window lines — a quarter-hour
/// window cannot span more — so they are parsed; every older retained file
/// is necessarily older than the window and is counted by raw line total, so
/// the "window excluded" statement never understates what it cut.
fn read_window(
    log_dir: &Path,
    window_start_unix_ms: i64,
    now_unix_ms: i64,
) -> (Vec<ForwardedDiagnostic>, usize) {
    let files = crate::diagnostics::client_log_files(log_dir);
    let windowed = files.iter().rev().take(2);
    let older = files.len().saturating_sub(2);
    let mut parsed = Vec::new();
    for path in windowed {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        parsed.extend(contents.lines().filter_map(parse_log_line));
    }
    let mut excluded_earlier = files
        .iter()
        .take(older)
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|contents| contents.lines().count())
        .sum::<usize>();
    // Files are named by date, so a name sort is already a time sort; the
    // stable sort only straightens ties.
    parsed.sort_by_key(|line| line.timestamp_unix_ms);
    let mut window_lines = Vec::new();
    for line in parsed {
        if line.timestamp_unix_ms < window_start_unix_ms {
            excluded_earlier += 1;
        } else if line.timestamp_unix_ms <= now_unix_ms {
            window_lines.push(line);
        }
    }
    (window_lines, excluded_earlier)
}

/// Parse one JSON log line from the Client's own store into a Forwarded
/// Diagnostic, keeping the level faithful — DEBUG stays DEBUG — and
/// reconstructing the message the way the forwarding layer would have
/// rendered it: the message field plus any flattened structured fields.
fn parse_log_line(raw: &str) -> Option<ForwardedDiagnostic> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let level = match value.get("level")?.as_str()? {
        "INFO" => ForwardedLevel::Info,
        "WARN" => ForwardedLevel::Warn,
        "ERROR" => ForwardedLevel::Error,
        "DEBUG" => ForwardedLevel::Debug,
        "TRACE" => ForwardedLevel::Trace,
        _ => return None,
    };
    let target = value.get("target")?.as_str()?.to_owned();
    let timestamp_unix_ms = parse_rfc3339_ms(value.get("timestamp")?.as_str()?)?;
    let message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let mut extra = Vec::new();
    for (key, field) in value.as_object()?.iter() {
        if matches!(
            key.as_str(),
            "timestamp" | "level" | "target" | "message" | "span" | "spans"
        ) {
            continue;
        }
        let rendered = match field.as_str() {
            Some(text) => text.to_owned(),
            None => field.to_string(),
        };
        extra.push(format!("{key}={rendered}"));
    }
    let message = crate::forwarding::render_message_with_context(message, extra);
    if message.trim().is_empty() {
        return None;
    }
    Some(ForwardedDiagnostic::new(
        level,
        target,
        truncate_message(&message),
        timestamp_unix_ms,
    ))
}

fn parse_rfc3339_ms(raw: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_millis())
}

fn rfc3339(unix_ms: i64) -> String {
    DateTime::from_timestamp_millis(unix_ms)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpiso_core::forwarding::MAX_FORWARDED_MESSAGE_BYTES;
    use tempfile::tempdir;

    /// Write a day of the Client's own JSON log store as `init_tracing`
    /// produces it: one JSON object per line, flattened event fields.
    fn write_log(dir: &Path, date: &str, lines: &[String]) {
        std::fs::write(
            dir.join(format!("client.{date}.log")),
            lines.join("\n") + "\n",
        )
        .expect("log file should write");
    }

    fn log_line(
        level: &str,
        target: &str,
        message: &str,
        timestamp: &str,
        extra: &[(&str, String)],
    ) -> String {
        let mut object = serde_json::json!({
            "timestamp": timestamp,
            "level": level,
            "target": target,
            "message": message,
        });
        if let Some(fields) = object.as_object_mut() {
            for (key, value) in extra {
                fields.insert((*key).to_owned(), serde_json::json!(value));
            }
        }
        object.to_string()
    }

    fn rfc3339_of(unix_ms: i64) -> String {
        DateTime::from_timestamp_millis(unix_ms)
            .expect("test timestamp should be representable")
            .to_rfc3339()
    }

    fn slice_lines(work: &PendingSlice) -> Vec<&ForwardedDiagnostic> {
        work.batches.iter().flatten().collect()
    }

    #[test]
    fn a_repeated_nonce_is_acted_on_exactly_once() {
        let mut state = SliceState::default();
        assert!(state.act_on_nonce("nonce-1"));
        assert!(
            !state.act_on_nonce("nonce-1"),
            "the same response repeating must not act twice"
        );
        assert!(state.act_on_nonce("nonce-2"));
        assert_eq!(state.pending_nonce.take(), Some("nonce-2".to_owned()));
    }

    #[test]
    fn the_slice_keeps_newest_lines_up_to_the_byte_cap_and_states_the_drop() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        // Sixty 1 KB lines inside the window: far past a tiny test cap.
        let mut file_lines = Vec::new();
        for index in 0..60 {
            file_lines.push(log_line(
                "INFO",
                "openpiso_client::test",
                &format!("line-{index}-{}", "x".repeat(1_000)),
                &rfc3339_of(now_ms - (60 - index) * 1_000),
                &[],
            ));
        }
        write_log(dir.path(), "2025-10-09", &file_lines);

        let tiny_cap = 8 * 1024;
        let work = build_slice_with_cap("nonce-1", dir.path(), now_ms, tiny_cap);
        let lines = slice_lines(&work);
        let statements: Vec<&str> = lines
            .iter()
            .filter(|line| line.target == DIAGNOSTIC_REQUEST_TARGET)
            .map(|line| line.message.as_str())
            .collect();
        assert_eq!(
            statements.len(),
            2,
            "frame plus cap statement: {statements:?}"
        );
        assert!(
            statements[0].contains(&format!("{} of 60 window lines uploaded", lines.len() - 2)),
            "frame states what was uploaded: {statements:?}"
        );
        assert!(
            statements[1].contains("byte cap truncated the slice"),
            "cap truncation is stated: {statements:?}"
        );
        assert!(
            statements[1].contains("older window lines were dropped"),
            "the oldest were dropped: {statements:?}"
        );
        // The kept lines are the newest ones, in capture order.
        let first_kept = &lines[2].message;
        assert!(
            first_kept.starts_with("line-"),
            "kept lines are evidence, not markers: {first_kept}"
        );
        let evidence: Vec<&ForwardedDiagnostic> = lines
            .iter()
            .copied()
            .filter(|line| line.target != DIAGNOSTIC_REQUEST_TARGET)
            .collect();
        let last_index: usize = evidence
            .last()
            .and_then(|line| line.message.split('-').nth(1))
            .and_then(|tail| tail.parse().ok())
            .expect("kept lines carry their index");
        assert_eq!(
            last_index, 59,
            "the newest line must always survive the cap"
        );
        // The whole upload honours the wire caps.
        assert!(!work.batches.is_empty());
        for batch in &work.batches {
            assert!(batch.len() <= MAX_FORWARDED_BATCH_LINES);
        }
    }

    #[test]
    fn the_window_excludes_earlier_lines_and_states_it() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        let window_start = now_ms - SLICE_WINDOW_SECONDS * 1000;
        write_log(
            dir.path(),
            "2025-10-09",
            &[
                log_line(
                    "INFO",
                    "openpiso_client::test",
                    "before the window",
                    &rfc3339_of(window_start - 60_000),
                    &[],
                ),
                log_line(
                    "ERROR",
                    "openpiso_client::test",
                    "inside the window",
                    &rfc3339_of(now_ms - 1_000),
                    &[],
                ),
            ],
        );
        let work = build_slice("nonce-1", dir.path(), now_ms);
        let lines = slice_lines(&work);
        assert_eq!(lines.len(), 3, "two markers plus one window line");
        assert!(
            lines[0].message.contains("window lines uploaded"),
            "{:?}",
            lines[0].message
        );
        assert!(
            lines[1]
                .message
                .contains("the window excluded 1 earlier log lines"),
            "window truncation is stated: {:?}",
            lines[1].message
        );
        assert_eq!(lines[2].message, "inside the window");
        assert_eq!(lines[2].level, ForwardedLevel::Error);
    }

    #[test]
    fn debug_lines_stay_debug_and_structured_fields_survive() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        write_log(
            dir.path(),
            "2025-10-09",
            &[
                log_line(
                    "DEBUG",
                    "openpiso_client::test",
                    "quiet evidence",
                    &rfc3339_of(now_ms - 1_000),
                    &[("waited_ms", "2".to_owned())],
                ),
                log_line(
                    "INFO",
                    "openpiso_client::startup",
                    "client startup milestone",
                    &rfc3339_of(now_ms - 500),
                    &[
                        ("milestone", "setup_entered".to_owned()),
                        ("elapsed_ms", "40".to_owned()),
                    ],
                ),
            ],
        );
        let work = build_slice("nonce-1", dir.path(), now_ms);
        let lines = slice_lines(&work);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].level, ForwardedLevel::Debug);
        assert_eq!(lines[1].message, "quiet evidence (waited_ms=2)");
        // Flattened structured fields render as `key=value`, in the JSON
        // object's (sorted) key order.
        assert_eq!(
            lines[2].message,
            "client startup milestone (elapsed_ms=40 milestone=setup_entered)"
        );
    }

    #[test]
    fn an_empty_window_still_answers_with_its_frame() {
        let dir = tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path()).expect("log dir");
        let work = build_slice("nonce-1", dir.path(), 1_760_000_090_000);
        let lines = slice_lines(&work);
        assert_eq!(lines.len(), 1, "the frame alone: {:?}", lines);
        assert!(lines[0].message.contains("0 of 0 window lines uploaded"));
        assert!(lines[0].message.contains("nonce nonce-1"));
        assert_eq!(work.batches.len(), 1);
    }

    #[test]
    fn oversized_lines_are_truncated_to_the_wire_limit() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        write_log(
            dir.path(),
            "2025-10-09",
            &[log_line(
                "WARN",
                "openpiso_client::test",
                &"y".repeat(MAX_FORWARDED_MESSAGE_BYTES + 500),
                &rfc3339_of(now_ms - 1_000),
                &[],
            )],
        );
        let work = build_slice("nonce-1", dir.path(), now_ms);
        let lines = slice_lines(&work);
        assert_eq!(lines.len(), 2, "frame plus the line");
        assert_eq!(
            lines[1].message.len(),
            MAX_FORWARDED_MESSAGE_BYTES,
            "the wire limit holds"
        );
    }

    #[test]
    fn the_slice_splits_into_batches_of_the_wire_cap() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        let mut file_lines = Vec::new();
        for index in 0..150 {
            file_lines.push(log_line(
                "INFO",
                "openpiso_client::test",
                &format!("line-{index}"),
                &rfc3339_of(now_ms - (150 - index) * 1_000),
                &[],
            ));
        }
        write_log(dir.path(), "2025-10-09", &file_lines);
        let work = build_slice("nonce-1", dir.path(), now_ms);
        assert_eq!(
            work.batches.len(),
            3,
            "151 lines (150 evidence + 1 frame) need three batches"
        );
        assert_eq!(work.batches[0].len(), MAX_FORWARDED_BATCH_LINES);
        assert_eq!(work.batches[2].len(), 23);
        // Capture order survives the split.
        assert!(work.batches[0][1].message.contains("line-"));
        assert!(work.batches[2]
            .last()
            .expect("tail")
            .message
            .ends_with("line-149"));
    }

    #[test]
    fn unparsable_lines_are_skipped_not_uploaded() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        write_log(
            dir.path(),
            "2025-10-09",
            &[
                "not json at all".to_owned(),
                log_line(
                    "WARN",
                    "openpiso_client::test",
                    "real evidence",
                    &rfc3339_of(now_ms - 1_000),
                    &[],
                ),
            ],
        );
        let work = build_slice("nonce-1", dir.path(), now_ms);
        let lines = slice_lines(&work);
        assert_eq!(lines.len(), 2, "frame plus one parseable line");
        assert_eq!(lines[1].message, "real evidence");
    }

    #[test]
    fn only_the_two_newest_log_files_are_scanned() {
        let dir = tempdir().expect("temp dir");
        let now_ms = 1_760_000_090_000;
        std::fs::create_dir_all(dir.path()).expect("log dir");
        // A young file and two older ones; only the newest two are read, so
        // the old evidence stays out of the window's scan.
        write_log(
            dir.path(),
            "2025-10-07",
            &[log_line(
                "WARN",
                "openpiso_client::test",
                "too old to scan",
                &rfc3339_of(now_ms - 1_000),
                &[],
            )],
        );
        write_log(
            dir.path(),
            "2025-10-08",
            &[log_line(
                "WARN",
                "openpiso_client::test",
                "just scanned",
                &rfc3339_of(now_ms - 2_000),
                &[],
            )],
        );
        write_log(
            dir.path(),
            "2025-10-09",
            &[log_line(
                "WARN",
                "openpiso_client::test",
                "newest",
                &rfc3339_of(now_ms - 1_000),
                &[],
            )],
        );
        let work = build_slice("nonce-1", dir.path(), now_ms);
        let messages: Vec<&str> = slice_lines(&work)
            .iter()
            .map(|line| line.message.as_str())
            .collect();
        assert!(
            messages.iter().any(|message| message.contains("newest")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("just scanned")),
            "{messages:?}"
        );
        assert!(
            !messages
                .iter()
                .any(|message| message.contains("too old to scan")),
            "the third-newest file is never parsed: {messages:?}"
        );
        // Its lines still count as what the window cut away, so the
        // statement cannot understate the exclusion.
        assert!(
            messages
                .iter()
                .any(|message| message.contains("the window excluded 1 earlier log lines")),
            "{messages:?}"
        );
    }
}

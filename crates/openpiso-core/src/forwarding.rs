//! Shared Diagnostic Forwarding buffer (ADR-0078).
//!
//! One pure type owns the forwarding logic both device-side senders — the
//! Client and the Client Guard — drive, so the two sides cannot drift in what
//! they consider meaningful: the qualification pipeline (WARN and ERROR
//! unconditionally, plus the named INFO allowlist under a rate cap), a
//! bounded ring, drop-oldest overflow that emits an explicit "N lines
//! dropped" marker so gaps are visible rather than silent, batch extraction,
//! and ack-gated retention. Lines stay until Admin returns a 2xx for the
//! batch carrying them, then are dropped; a failed or refused send retains
//! them for the next attempt.
//!
//! The type has no Windows, tokio, or network dependency.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Longest `message` a Forwarded Diagnostic may carry. The buffer enforces the
/// same limit the Admin endpoint accepts, so the buffer can never hold a line
/// Admin would refuse — a refused line could never be acknowledged and would
/// be resent forever.
pub const MAX_FORWARDED_MESSAGE_BYTES: usize = 2_000;

/// How many qualifying lines the buffer retains before dropping the oldest.
pub const DEFAULT_FORWARDING_BUFFER_LINES: usize = 256;

/// Longest batch a sender extracts in one send, so a burst recorded at once
/// still travels in bounded payloads.
pub const MAX_FORWARDED_BATCH_LINES: usize = 64;

/// Target of the synthetic drop marker so it is recognisable in the
/// forwarded log without being a real device log target.
pub const FORWARDING_BUFFER_TARGET: &str = "openpiso_forwarding";

/// Client target whose every INFO line is a startup milestone (ADR-0078).
pub const CLIENT_STARTUP_TARGET: &str = "openpiso_client::startup";

/// Client target of the Guard pipe connection and the Guard Coverage it
/// reports (ADR-0078).
pub const CLIENT_GUARD_TARGET: &str = "openpiso_client::guard_client";

/// Client target carrying only Offline Shutdown Countdown state changes
/// (ADR-0078).
pub const CLIENT_OFFLINE_SHUTDOWN_TARGET: &str = "openpiso_client::offline_shutdown";

/// Client target of Tamper Kill handling (ADR-0078).
pub const CLIENT_TAMPER_TARGET: &str = "openpiso_client::tamper";

/// Client Guard target of the pipe server itself, whose client-connect edge
/// the Guard observes (ADR-0078).
pub const GUARD_PIPE_TARGET: &str = "openpiso_client_guard::pipe";

/// Allowlisted INFO lines that may forward per rate window: the rate cap
/// above the allowlist (ADR-0078) bounds the wire under a pathological
/// logging loop. WARN, ERROR, and the overflow drop marker are exempt.
pub const INFO_RATE_CAP_LINES: usize = 60;

/// Width of the INFO rate window, in milliseconds.
pub const INFO_RATE_WINDOW_MS: i64 = 60_000;

/// The named INFO allowlist (ADR-0078): the events whose story a WARN-only
/// filter loses. WARN and ERROR never consult it and always forward.
///
/// A whole-target match forwards every INFO line that target emits — it is
/// used only for targets that exist to carry one kind of event. A
/// message-prefix match names one event inside a busier target, so wording
/// drift fails closed (the line stops forwarding) rather than open.
pub fn info_allowlisted(target: &str, message: &str) -> bool {
    // Client startup milestones.
    if target == CLIENT_STARTUP_TARGET {
        return true;
    }
    // Offline Shutdown Countdown arm and disarm.
    if target == CLIENT_OFFLINE_SHUTDOWN_TARGET {
        return true;
    }
    // Guard pipe connect and disconnect, as each side sees it. Both sides'
    // disconnect lines are WARN and forward unconditionally; these are the
    // connect edges.
    if target == CLIENT_GUARD_TARGET && message.starts_with("connected to Guard pipe") {
        return true;
    }
    if target == GUARD_PIPE_TARGET && message.starts_with("Client connected") {
        return true;
    }
    // Client Guard Coverage transitions, as reported to Admin.
    if target == CLIENT_GUARD_TARGET && message.starts_with("Client Guard Coverage") {
        return true;
    }
    // Tamper Kill.
    if target == CLIENT_TAMPER_TARGET && message.starts_with("Tamper Kill") {
        return true;
    }
    false
}

/// Severity of a Forwarded Diagnostic. WARN and ERROR always forward; INFO
/// forwards only when the named allowlist accepts it (ADR-0078). DEBUG and
/// TRACE are never captured by the senders' buffers — they exist for the
/// Diagnostic Request slice, which uploads a window of the device's full log
/// and must stay faithful to the levels it finds there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardedLevel {
    Info,
    Warn,
    Error,
    Debug,
    Trace,
}

impl ForwardedLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Which device surface produced a Forwarded Diagnostic. Admin tags every
/// forwarded line with it so the Support Bundle builder can filter without
/// guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardedSource {
    Client,
    Guard,
}

impl ForwardedSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Guard => "guard",
        }
    }
}

/// One log line that qualified for forwarding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardedDiagnostic {
    pub level: ForwardedLevel,
    pub target: String,
    pub message: String,
    /// Device wall-clock capture time, Unix milliseconds. Admin keeps it
    /// alongside its own receive time: a line may sit in the buffer across an
    /// Admin outage, so the receive time alone would misstate when it
    /// happened.
    pub timestamp_unix_ms: i64,
}

impl ForwardedDiagnostic {
    pub fn new(
        level: ForwardedLevel,
        target: impl Into<String>,
        message: impl Into<String>,
        timestamp_unix_ms: i64,
    ) -> Self {
        Self {
            level,
            target: target.into(),
            message: message.into(),
            timestamp_unix_ms,
        }
    }
}

/// A batch taken for sending. The caller holds it until the send resolves:
/// pass it to [`ForwardingBuffer::acknowledge`] on a 2xx response, or to
/// [`ForwardingBuffer::release`] on any failure so the lines stay for the
/// next attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedBatch {
    lines: Vec<ForwardedDiagnostic>,
    ack_watermark: u64,
}

impl ForwardedBatch {
    pub fn lines(&self) -> &[ForwardedDiagnostic] {
        &self.lines
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[derive(Debug)]
struct BufferedLine {
    seq: u64,
    diagnostic: ForwardedDiagnostic,
    /// Original diagnostics represented by a synthetic marker; zero for a real line.
    dropped_lines: u64,
}

/// Bounded ack-gated buffer of Forwarded Diagnostics, shared by both senders
/// (ADR-0078). WARN and ERROR always pass; INFO passes only through the named
/// allowlist and under the rate cap. Overflow drops the oldest lines and
/// records how many; the count is flushed as an explicit drop marker line so
/// gaps are visible.
#[derive(Debug)]
pub struct ForwardingBuffer {
    capacity: usize,
    lines: VecDeque<BufferedLine>,
    next_seq: u64,
    dropped_since_marker: u64,
    /// Evicted in-flight evidence is lost only if its send fails.
    evicted_in_flight: u64,
    /// Capture timestamps of the allowlisted INFO lines accepted within the
    /// current rate window. Rides the lines' own timestamps, so the cap needs
    /// no clock of its own.
    info_window: VecDeque<i64>,
    /// Sequence number of the last line of the batch currently in flight, so
    /// an acknowledgement drains exactly the lines Admin received even if
    /// overflow removed some of them while the send was running.
    ack_watermark: Option<u64>,
}

impl ForwardingBuffer {
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity >= 1,
            "forwarding buffer capacity must be at least 1"
        );
        Self {
            capacity,
            lines: VecDeque::new(),
            next_seq: 0,
            dropped_since_marker: 0,
            evicted_in_flight: 0,
            info_window: VecDeque::new(),
            ack_watermark: None,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_FORWARDING_BUFFER_LINES)
    }

    /// Record one diagnostic through the qualification pipeline (ADR-0078):
    /// WARN and ERROR always enter the ring; INFO only when the named
    /// allowlist accepts it and the rate cap has budget left; anything else
    /// is dropped at the door — the buffer never proactively forwards DEBUG
    /// or TRACE, which only the Diagnostic Request slice may carry. Messages
    /// longer than [`MAX_FORWARDED_MESSAGE_BYTES`] are truncated on a
    /// character boundary.
    pub fn record(&mut self, diagnostic: ForwardedDiagnostic) {
        let qualified = match diagnostic.level {
            ForwardedLevel::Warn | ForwardedLevel::Error => true,
            ForwardedLevel::Info => {
                info_allowlisted(&diagnostic.target, &diagnostic.message)
                    && self.info_rate_budget(diagnostic.timestamp_unix_ms)
            }
            ForwardedLevel::Debug | ForwardedLevel::Trace => false,
        };
        if !qualified {
            return;
        }
        let diagnostic = ForwardedDiagnostic {
            message: truncate_message(&diagnostic.message),
            ..diagnostic
        };
        self.push_line(diagnostic);
        while self.lines.len() > self.capacity {
            if let Some(evicted) = self.lines.pop_front() {
                let dropped = evicted.dropped_lines.max(1);
                if self
                    .ack_watermark
                    .is_some_and(|watermark| evicted.seq <= watermark)
                {
                    self.evicted_in_flight += dropped;
                } else {
                    self.dropped_since_marker += dropped;
                }
            }
        }
    }

    /// Whether the rate cap has budget for one more allowlisted INFO line
    /// captured at `timestamp_unix_ms`, sliding the window forward first. A
    /// clock that jumps backward only tightens the cap — stale entries refuse
    /// to drain — so it can never over-admit.
    fn info_rate_budget(&mut self, timestamp_unix_ms: i64) -> bool {
        let window_start = timestamp_unix_ms - INFO_RATE_WINDOW_MS;
        while self
            .info_window
            .front()
            .is_some_and(|captured| *captured <= window_start)
        {
            self.info_window.pop_front();
        }
        if self.info_window.len() >= INFO_RATE_CAP_LINES {
            return false;
        }
        self.info_window.push_back(timestamp_unix_ms);
        true
    }

    /// Take the next batch to send, oldest lines first. Only one batch may be
    /// in flight at a time; a second call before an ack or release returns
    /// `None`.
    pub fn take_batch(&mut self) -> Option<ForwardedBatch> {
        if self.ack_watermark.is_some() {
            return None;
        }
        if self.dropped_since_marker > 0 {
            self.push_drop_marker();
        }
        if self.lines.is_empty() {
            return None;
        }
        let take = self.lines.len().min(MAX_FORWARDED_BATCH_LINES);
        let lines: Vec<ForwardedDiagnostic> = self
            .lines
            .range(..take)
            .map(|buffered| buffered.diagnostic.clone())
            .collect();
        let ack_watermark = self.lines[take - 1].seq;
        self.ack_watermark = Some(ack_watermark);
        Some(ForwardedBatch {
            lines,
            ack_watermark,
        })
    }

    /// Release the lines Admin acknowledged with a 2xx for this batch. The
    /// watermark — not a count — decides what is drained, so lines recorded
    /// or dropped while the send was running are never mistaken for sent
    /// ones.
    pub fn acknowledge(&mut self, batch: &ForwardedBatch) {
        if self.ack_watermark != Some(batch.ack_watermark) {
            self.release();
            return;
        }
        while self
            .lines
            .front()
            .is_some_and(|buffered| buffered.seq <= batch.ack_watermark)
        {
            self.lines.pop_front();
        }
        self.ack_watermark = None;
        self.evicted_in_flight = 0;
    }

    /// Keep the batch's lines for the next attempt after a failed or refused
    /// send.
    pub fn release(&mut self) {
        self.ack_watermark = None;
        self.dropped_since_marker += std::mem::take(&mut self.evicted_in_flight);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn push_line(&mut self, diagnostic: ForwardedDiagnostic) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.lines.push_back(BufferedLine {
            seq,
            diagnostic,
            dropped_lines: 0,
        });
    }

    /// Emit one marker summarising every line dropped since the last marker,
    /// so a burst of overflow records a single explicit gap rather than a
    /// marker per line. The marker is prepended: the lines it describes are
    /// older than everything still in the ring.
    fn push_drop_marker(&mut self) {
        let dropped = std::mem::take(&mut self.dropped_since_marker);
        // The marker describes lines older than the whole ring, so it shares
        // the front line's sequence: an acknowledgement that drains up to the
        // batch watermark releases the marker with them.
        let seq = self
            .lines
            .front()
            .map(|buffered| buffered.seq)
            .unwrap_or(self.next_seq);
        self.lines.push_front(BufferedLine {
            seq,
            dropped_lines: dropped,
            diagnostic: ForwardedDiagnostic {
                level: ForwardedLevel::Warn,
                target: FORWARDING_BUFFER_TARGET.to_owned(),
                message: format!(
                    "{dropped} forwarded diagnostic{} dropped: device buffer overflow",
                    if dropped == 1 { "" } else { "s" }
                ),
                timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
            },
        });
    }
}

/// Truncate to the wire limit on a character boundary, never mid-codepoint.
/// Public because the Diagnostic Request slice (ADR-0078) applies the same
/// wire limit to the full-log lines it uploads, so the two paths cannot drift.
pub fn truncate_message(message: &str) -> String {
    if message.len() <= MAX_FORWARDED_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_FORWARDED_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(level: ForwardedLevel, message: &str) -> ForwardedDiagnostic {
        ForwardedDiagnostic::new(level, "openpiso_client::test", message, 1_000)
    }

    fn info_line(target: &str, message: &str) -> ForwardedDiagnostic {
        ForwardedDiagnostic::new(ForwardedLevel::Info, target, message, 1_000)
    }

    fn drain(buffer: &mut ForwardingBuffer) -> Vec<ForwardedDiagnostic> {
        let mut drained = Vec::new();
        while let Some(batch) = buffer.take_batch() {
            drained.extend(batch.lines().iter().cloned());
            buffer.acknowledge(&batch);
        }
        drained
    }

    #[test]
    fn each_named_info_event_qualifies_for_forwarding() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(info_line(
            CLIENT_STARTUP_TARGET,
            "client startup milestone (milestone=setup_entered elapsed_ms=40)",
        ));
        buffer.record(info_line(
            CLIENT_OFFLINE_SHUTDOWN_TARGET,
            "Offline Shutdown Countdown armed: the admin server is unreachable (grace_seconds=120)",
        ));
        buffer.record(info_line(
            CLIENT_OFFLINE_SHUTDOWN_TARGET,
            "Offline Shutdown Countdown cancelled: the admin server is reachable again",
        ));
        buffer.record(info_line(
            CLIENT_GUARD_TARGET,
            "connected to Guard pipe (waited_ms=2)",
        ));
        buffer.record(info_line(
            GUARD_PIPE_TARGET,
            "Client connected (process_id=42)",
        ));
        buffer.record(info_line(
            CLIENT_GUARD_TARGET,
            "Client Guard Coverage transitioned from covered to unavailable",
        ));
        buffer.record(info_line(
            CLIENT_TAMPER_TARGET,
            "Tamper Kill report received from the Guard",
        ));

        let batch = buffer.take_batch().expect("allowlisted lines should batch");
        assert_eq!(batch.len(), 7);
        assert!(batch
            .lines()
            .iter()
            .all(|line| line.level == ForwardedLevel::Info));
    }

    #[test]
    fn allowlisted_prefixes_survive_appended_lifetime_context() {
        // Issue #368: Client lifecycle lines carry compact
        // `(process_id=… launch=…)` context after the named prefix; the
        // prefix qualification must accept them unchanged.
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(info_line(
            CLIENT_GUARD_TARGET,
            "connected to Guard pipe (waited_ms=2 process_id=4242 launch=1a2b3c4d)",
        ));
        buffer.record(info_line(
            CLIENT_GUARD_TARGET,
            "Client Guard Coverage transitioned from covered to unavailable \
             (process_id=4242 launch=1a2b3c4d)",
        ));

        let batch = buffer.take_batch().expect("context lines should batch");
        assert_eq!(batch.len(), 2);
        assert!(batch
            .lines()
            .iter()
            .all(|line| line.message.contains("launch=1a2b3c4d")));
    }

    #[test]
    fn an_oversized_context_line_is_truncated_not_dropped() {
        // Qualification runs on the named prefix, then the wire limit
        // truncates: a lifecycle line whose context pushes it past the limit
        // still forwards, truncated.
        let mut buffer = ForwardingBuffer::with_default_capacity();
        let padding = "x".repeat(MAX_FORWARDED_MESSAGE_BYTES);
        buffer.record(info_line(
            CLIENT_GUARD_TARGET,
            &format!(
                "connected to Guard pipe (waited_ms=2 process_id=4242 launch=1a2b3c4d {padding})"
            ),
        ));

        let batch = buffer
            .take_batch()
            .expect("the truncated line should batch");
        assert_eq!(batch.len(), 1);
        let message = &batch.lines()[0].message;
        assert!(message.len() <= MAX_FORWARDED_MESSAGE_BYTES);
        assert!(
            message.starts_with("connected to Guard pipe ("),
            "the named prefix survives truncation: {message}"
        );
    }

    #[test]
    fn unlisted_info_never_enters_the_ring() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        // Routine traffic on targets the allowlist names, and elsewhere.
        buffer.record(info_line(
            CLIENT_GUARD_TARGET,
            "sent typed Guard pipe message (message=CleanExit)",
        ));
        buffer.record(info_line(GUARD_PIPE_TARGET, "pipe server starting"));
        buffer.record(info_line("openpiso_client", "theme refreshed"));
        // A named target whose message matches no event is still refused.
        buffer.record(info_line(
            CLIENT_TAMPER_TARGET,
            "tamper heartbeat acknowledged",
        ));

        assert!(buffer.is_empty(), "unlisted INFO must not buffer");
        assert!(buffer.take_batch().is_none());
    }

    #[test]
    fn warn_and_error_forward_even_where_the_allowlist_does_not_apply() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(line(ForwardedLevel::Warn, "anything at all"));
        buffer.record(line(ForwardedLevel::Error, "still anything"));
        // A WARN from an allowlisted target whose message matches no INFO
        // prefix forwards all the same.
        buffer.record(ForwardedDiagnostic::new(
            ForwardedLevel::Warn,
            CLIENT_GUARD_TARGET,
            "could not connect to Guard pipe (Guard may not be installed)",
            1_000,
        ));

        let batch = buffer.take_batch().expect("warn and error should batch");
        assert_eq!(batch.len(), 3);
        assert_eq!(batch.lines()[0].level, ForwardedLevel::Warn);
        assert_eq!(batch.lines()[1].level, ForwardedLevel::Error);
        assert_eq!(batch.lines()[2].level, ForwardedLevel::Warn);
    }

    #[test]
    fn the_rate_cap_bounds_a_pathological_info_loop() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        for index in 0..(INFO_RATE_CAP_LINES * 4) {
            buffer.record(info_line(CLIENT_STARTUP_TARGET, &format!("flood-{index}")));
        }

        let drained = drain(&mut buffer);
        assert_eq!(
            drained.len(),
            INFO_RATE_CAP_LINES,
            "only the capped window may forward"
        );
        assert!(drain(&mut buffer).is_empty(), "the excess is gone for good");
    }

    #[test]
    fn the_info_rate_window_slides_so_slow_traffic_is_never_capped() {
        let mut buffer = ForwardingBuffer::new(INFO_RATE_CAP_LINES * 5);
        for minute in 0..5i64 {
            for line_index in 0..INFO_RATE_CAP_LINES {
                buffer.record(ForwardedDiagnostic::new(
                    ForwardedLevel::Info,
                    CLIENT_STARTUP_TARGET,
                    "milestone",
                    minute * INFO_RATE_WINDOW_MS + line_index as i64,
                ));
            }
        }

        let drained = drain(&mut buffer);
        assert_eq!(
            drained.len(),
            INFO_RATE_CAP_LINES * 5,
            "each window gets fresh budget"
        );
    }

    #[test]
    fn a_backward_clock_only_tightens_the_rate_cap() {
        // A clock jump backward leaves newer entries in the "window": they
        // refuse to drain, so the cap can never over-admit.
        let mut buffer = ForwardingBuffer::with_default_capacity();
        for index in 0..INFO_RATE_CAP_LINES {
            buffer.record(ForwardedDiagnostic::new(
                ForwardedLevel::Info,
                CLIENT_STARTUP_TARGET,
                "before the jump",
                2_000_000 + index as i64,
            ));
        }
        buffer.record(ForwardedDiagnostic::new(
            ForwardedLevel::Info,
            CLIENT_STARTUP_TARGET,
            "after a backward jump",
            1_000_000,
        ));

        let drained = drain(&mut buffer);
        assert_eq!(drained.len(), INFO_RATE_CAP_LINES);
        assert!(
            drained.iter().all(|line| line.message == "before the jump"),
            "the post-jump line must be refused while the window is full"
        );
    }

    #[test]
    fn the_overflow_marker_survives_rate_cap_pressure() {
        let mut buffer = ForwardingBuffer::new(4);
        for index in 0..8 {
            buffer.record(line(ForwardedLevel::Error, &format!("line-{index}")));
        }
        for index in 0..(INFO_RATE_CAP_LINES * 2) {
            buffer.record(info_line(CLIENT_STARTUP_TARGET, &format!("flood-{index}")));
        }

        let batch = buffer.take_batch().expect("batch should be taken");
        assert_eq!(
            batch.lines()[0].target,
            FORWARDING_BUFFER_TARGET,
            "the overflow marker must not be lost to rate-cap pressure"
        );
        assert_eq!(
            batch.lines()[0].message,
            "64 forwarded diagnostics dropped: device buffer overflow"
        );
        let info_lines = batch
            .lines()
            .iter()
            .filter(|line| line.level == ForwardedLevel::Info)
            .count();
        assert!(
            info_lines <= INFO_RATE_CAP_LINES,
            "the flood contributes at most the capped window"
        );
    }

    #[test]
    fn warn_and_error_lines_qualify_for_forwarding() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(line(ForwardedLevel::Warn, "watch out"));
        buffer.record(line(ForwardedLevel::Error, "broken"));

        let batch = buffer.take_batch().expect("batch should be taken");
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.lines()[0].level, ForwardedLevel::Warn);
        assert_eq!(batch.lines()[0].message, "watch out");
        assert_eq!(batch.lines()[1].level, ForwardedLevel::Error);
        assert_eq!(batch.lines()[1].message, "broken");
        // Oldest line first so Admin's log reads chronologically.
        assert!(batch.lines()[0].timestamp_unix_ms <= batch.lines()[1].timestamp_unix_ms);
    }

    #[test]
    fn acknowledged_batch_is_released_and_newer_lines_remain() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(line(ForwardedLevel::Error, "first"));
        let batch = buffer.take_batch().expect("batch should be taken");
        buffer.record(line(ForwardedLevel::Error, "second"));

        buffer.acknowledge(&batch);
        assert_eq!(
            buffer.len(),
            1,
            "the ack releases exactly the batch; lines recorded meanwhile stay"
        );

        let next = buffer.take_batch().expect("next batch should be taken");
        assert_eq!(next.lines(), &[line(ForwardedLevel::Error, "second")]);
    }

    #[test]
    fn failed_send_retains_lines_for_the_next_attempt() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(line(ForwardedLevel::Error, "kept"));
        let batch = buffer.take_batch().expect("batch should be taken");

        buffer.release();
        let retried = buffer.take_batch().expect("retry batch should be taken");
        assert_eq!(retried, batch);
    }

    #[test]
    fn only_one_batch_is_in_flight_at_a_time() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        buffer.record(line(ForwardedLevel::Error, "one"));
        assert!(buffer.take_batch().is_some());
        assert!(buffer.take_batch().is_none(), "second take must be refused");

        buffer.release();
        assert!(buffer.take_batch().is_some());
    }

    #[test]
    fn overflow_drops_oldest_and_emits_one_marker_carrying_the_count() {
        let mut buffer = ForwardingBuffer::new(4);
        for index in 0..7 {
            buffer.record(line(ForwardedLevel::Error, &format!("line-{index}")));
        }

        let batch = buffer.take_batch().expect("batch should be taken");
        let messages: Vec<&str> = batch
            .lines()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert_eq!(
            messages,
            vec![
                "3 forwarded diagnostics dropped: device buffer overflow",
                "line-3",
                "line-4",
                "line-5",
                "line-6",
            ]
        );
        assert_eq!(
            batch.lines()[0].target,
            FORWARDING_BUFFER_TARGET,
            "the marker must be recognisable as a gap, not a device log line"
        );
    }

    #[test]
    fn marker_is_flushed_even_when_nothing_else_is_buffered() {
        // Overflow recorded while a previous batch is still unacked must
        // still surface: the next take flushes the cumulative count.
        let mut buffer = ForwardingBuffer::new(2);
        buffer.record(line(ForwardedLevel::Error, "a"));
        buffer.record(line(ForwardedLevel::Error, "b"));
        buffer.record(line(ForwardedLevel::Error, "c"));
        buffer.record(line(ForwardedLevel::Error, "d"));
        buffer.record(line(ForwardedLevel::Error, "e"));

        let batch = buffer.take_batch().expect("batch should be taken");
        assert_eq!(
            batch.lines()[0].message,
            "3 forwarded diagnostics dropped: device buffer overflow"
        );
        assert_eq!(
            batch.lines()[1].message,
            "d",
            "the marker precedes the lines that survived the overflow"
        );
    }

    #[test]
    fn failed_send_preserves_drop_counts_across_further_overflow() {
        for overflow_during_send in [false, true] {
            let mut buffer = ForwardingBuffer::new(2);
            for message in ["a", "b", "c", "d", "e"] {
                buffer.record(line(ForwardedLevel::Error, message));
            }
            let batch = buffer.take_batch().expect("batch");
            assert!(batch.lines()[0]
                .message
                .starts_with("3 forwarded diagnostics dropped"));
            if !overflow_during_send {
                buffer.release();
            }
            buffer.record(line(ForwardedLevel::Error, "f"));
            if overflow_during_send {
                buffer.release();
            }
            let retry = buffer.take_batch().expect("retry");
            assert_eq!(
                retry.lines()[0].message,
                "4 forwarded diagnostics dropped: device buffer overflow"
            );
            assert_eq!(retry.lines()[1].message, "e");
            assert_eq!(retry.lines()[2].message, "f");
            buffer.acknowledge(&retry);
            assert!(buffer.take_batch().is_none());
        }
    }

    #[test]
    fn acknowledged_overflow_marker_is_not_reported_again() {
        let mut buffer = ForwardingBuffer::new(2);
        for message in ["a", "b", "c", "d", "e"] {
            buffer.record(line(ForwardedLevel::Error, message));
        }
        let batch = buffer.take_batch().expect("batch");
        // Evict the marker and both in-flight lines, then one unsent line.
        for message in ["f", "g", "h"] {
            buffer.record(line(ForwardedLevel::Error, message));
        }
        buffer.acknowledge(&batch);
        let next = buffer.take_batch().expect("next batch");
        assert_eq!(
            next.lines()[0].message,
            "1 forwarded diagnostic dropped: device buffer overflow"
        );
        assert_eq!(next.lines()[1].message, "g");
        assert_eq!(next.lines()[2].message, "h");
    }

    #[test]
    fn overflow_across_a_batch_boundary_counts_only_undelivered_lines() {
        let capacity = MAX_FORWARDED_BATCH_LINES + 2;
        for succeeds in [false, true] {
            let mut buffer = ForwardingBuffer::new(capacity);
            for index in 0..capacity + 3 {
                buffer.record(line(ForwardedLevel::Error, &format!("old-{index}")));
            }
            let batch = buffer.take_batch().expect("batch");
            assert_eq!(batch.len(), MAX_FORWARDED_BATCH_LINES);
            for index in 0..capacity {
                buffer.record(line(ForwardedLevel::Error, &format!("new-{index}")));
            }
            if succeeds {
                buffer.acknowledge(&batch);
            } else {
                buffer.release();
            }
            let next = buffer.take_batch().expect("next batch");
            let lost = if succeeds { 3 } else { capacity + 3 };
            assert_eq!(
                next.lines()[0].message,
                format!("{lost} forwarded diagnostics dropped: device buffer overflow")
            );
            assert_eq!(next.len(), MAX_FORWARDED_BATCH_LINES);
            assert_eq!(next.lines()[1].message, "new-0");
        }
    }

    #[test]
    fn acknowledgement_after_overflow_keeps_lines_admin_never_received() {
        // The buffer holds 4 lines; the first 4 are taken for sending. While
        // that send is running, 4 newer lines overflow-drop the in-flight
        // ones out of the ring. The ack must drain nothing — the in-flight
        // clones are what Admin received, and the newer lines were never
        // sent — and a later batch must carry exactly those newer lines.
        let mut buffer = ForwardingBuffer::new(4);
        for index in 0..4 {
            buffer.record(line(ForwardedLevel::Error, &format!("sent-{index}")));
        }
        let batch = buffer.take_batch().expect("batch should be taken");
        assert_eq!(batch.len(), 4);

        for index in 0..4 {
            buffer.record(line(ForwardedLevel::Error, &format!("kept-{index}")));
        }

        buffer.acknowledge(&batch);
        assert_eq!(buffer.len(), 4, "unsent lines must survive the ack");

        let next = buffer.take_batch().expect("next batch should be taken");
        assert!(
            next.lines()
                .iter()
                .all(|line| line.target != FORWARDING_BUFFER_TARGET),
            "acknowledged lines are not lost diagnostics"
        );
        let messages: Vec<String> = next
            .lines()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        assert_eq!(
            messages,
            vec!["kept-0", "kept-1", "kept-2", "kept-3"],
            "the ack must not delete lines Admin never received"
        );
    }

    #[test]
    fn batch_is_capped_so_a_burst_travels_in_bounded_payloads() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        for index in 0..(MAX_FORWARDED_BATCH_LINES + 10) {
            buffer.record(line(ForwardedLevel::Error, &format!("line-{index}")));
        }

        let batch = buffer.take_batch().expect("batch should be taken");
        assert_eq!(batch.len(), MAX_FORWARDED_BATCH_LINES);
        assert_eq!(batch.lines()[0].message, "line-0");
        buffer.acknowledge(&batch);
        assert_eq!(buffer.len(), 10, "the tail waits for the next batch");
    }

    #[test]
    fn oversized_message_is_truncated_on_a_character_boundary() {
        let mut buffer = ForwardingBuffer::with_default_capacity();
        let multibyte = "é".repeat(MAX_FORWARDED_MESSAGE_BYTES / 2 + 10);
        buffer.record(line(ForwardedLevel::Error, &multibyte));

        let batch = buffer.take_batch().expect("batch should be taken");
        let message = &batch.lines()[0].message;
        assert!(message.len() <= MAX_FORWARDED_MESSAGE_BYTES);
        assert!(message.chars().all(|c| c == 'é'));
    }

    #[test]
    fn drop_marker_is_ack_gated_like_any_line() {
        // The marker is released together with the batch it opened.
        let mut buffer = ForwardingBuffer::new(2);
        buffer.record(line(ForwardedLevel::Error, "a"));
        buffer.record(line(ForwardedLevel::Error, "b"));
        buffer.record(line(ForwardedLevel::Error, "c"));
        buffer.record(line(ForwardedLevel::Error, "d"));

        let batch = buffer.take_batch().expect("batch should be taken");
        assert_eq!(
            batch.lines()[0].message,
            "2 forwarded diagnostics dropped: device buffer overflow"
        );
        buffer.acknowledge(&batch);
        assert!(
            buffer.is_empty(),
            "marker and acked lines leave the buffer together"
        );
    }

    #[test]
    fn slice_only_levels_round_trip_by_their_wire_labels() {
        // The Diagnostic Request slice must carry the full-log levels it finds,
        // never mapped onto INFO, and never refused by the wire format.
        for (level, wire) in [
            (ForwardedLevel::Debug, "debug"),
            (ForwardedLevel::Trace, "trace"),
        ] {
            let raw = serde_json::to_string(&level).expect("level should serialise");
            assert_eq!(raw, format!("\"{wire}\""));
            let parsed: ForwardedLevel =
                serde_json::from_str(&raw).expect("label should deserialise");
            assert_eq!(parsed, level);
            assert_eq!(parsed.label(), wire);
        }
    }
}

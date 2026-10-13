//! Guard-side Diagnostic Forwarding (ADR-0078).
//!
//! A tracing layer copies every WARN, ERROR, and INFO log line the Guard
//! produces into the shared bounded buffer in `openpiso-core`, which applies
//! the qualification pipeline: WARN and ERROR always qualify, INFO only
//! through the named allowlist and under its rate cap. The Guard gets no
//! network call of its own (ADR-0077): its batch rides the body of the poll
//! it already makes, so the poll thread — not a second sender — drains the
//! buffer here.
//!
//! Forwarding is gated to release builds; `OPENPISO_FORWARD_DIAGNOSTICS=1`
//! turns it on deliberately in a development build (ADR-0078). The capture
//! layer is installed only when the gate allows, so a gated-off build keeps
//! its subscriber shape and buffers nothing.

use std::sync::LazyLock;

use openpiso_core::forwarding::{ForwardedDiagnostic, ForwardedLevel, ForwardingBuffer};
use tracing::{field::Visit, Event, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

/// The shared device-side buffer. Held in a global so the tracing layer —
/// which runs on every thread that logs — and the poll thread meet here.
static FORWARD_BUFFER: LazyLock<std::sync::Mutex<ForwardingBuffer>> =
    LazyLock::new(|| std::sync::Mutex::new(ForwardingBuffer::with_default_capacity()));

/// Target of the forwarder's own failure notes. These must never re-enter the
/// buffer: notes produced while Admin is unreachable would fill the ring and
/// crowd out the evidence it exists to keep.
pub(crate) const FORWARDING_TARGET: &str = "openpiso_client_guard::forwarding";

/// Release-only gate with an explicit development opt-in (ADR-0078).
pub(crate) fn forwarding_enabled() -> bool {
    forwarding_enabled_for(
        cfg!(debug_assertions),
        std::env::var("OPENPISO_FORWARD_DIAGNOSTICS")
            .ok()
            .as_deref(),
    )
}

/// The gate as a pure decision so it can be tested without touching process
/// state: a development build forwards only when the environment variable is
/// exactly `1`; a release build forwards regardless of it.
fn forwarding_enabled_for(debug_build: bool, dev_override: Option<&str>) -> bool {
    !debug_build || dev_override == Some("1")
}

/// Capture one line into the buffer. Only the tracing layer (installed
/// exclusively when forwarding is enabled) calls this.
fn record_captured(level: ForwardedLevel, target: &str, message: impl Into<String>) {
    let diagnostic = ForwardedDiagnostic::new(level, target, message, crate::unix_time_millis());
    buffer().record(diagnostic);
}

fn buffer() -> std::sync::MutexGuard<'static, ForwardingBuffer> {
    FORWARD_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The tracing layer that feeds the buffer. Installed only when forwarding is
/// enabled; `None` otherwise so the subscriber shape is unchanged.
pub(crate) fn forwarding_layer() -> Option<ForwardingLayer> {
    forwarding_enabled().then_some(ForwardingLayer)
}

/// Copies WARN, ERROR, and INFO events into the forwarding buffer. The
/// buffer decides what actually qualifies: WARN and ERROR always, INFO only
/// through the named allowlist and under its rate cap (ADR-0078).
pub(crate) struct ForwardingLayer;

impl<S> Layer<S> for ForwardingLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // Level strings, not level ordering: DEBUG and TRACE never forward;
        // INFO reaches the buffer, which applies the allowlist and rate cap.
        let level = match metadata.level().as_str() {
            "INFO" => ForwardedLevel::Info,
            "WARN" => ForwardedLevel::Warn,
            "ERROR" => ForwardedLevel::Error,
            _ => return,
        };
        if metadata.target() == FORWARDING_TARGET {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor.render();
        if message.trim().is_empty() {
            return;
        }
        record_captured(level, metadata.target(), message);
    }
}

/// Collects the event's message field; events without one fall back to a
/// `key=value` summary of their remaining fields, so a structured-only event
/// still forwards something meaningful.
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    extra: Vec<String>,
}

impl MessageVisitor {
    /// The message field, with any structured context appended so a
    /// forwarded line still carries it; events without a message fall back
    /// to a `key=value` summary of their fields.
    fn render(self) -> String {
        let context = self.extra.join(" ");
        match (self.message, context.is_empty()) {
            (Some(message), true) => message,
            (Some(message), false) => format!("{message} ({context})"),
            (None, _) => context,
        }
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message.get_or_insert_with(|| value.to_owned());
        } else {
            self.extra.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // `Debug` for `fmt::Arguments` renders the same text as `Display`,
            // so a plain `warn!("message")` arrives unquoted.
            self.message.get_or_insert_with(|| format!("{value:?}"));
        } else {
            self.extra.push(format!("{}={:?}", field.name(), value));
        }
    }
}

/// Take the next batch to ride one poll, oldest lines first. The poll
/// interval is the only drain rate: a batch that stays unacknowledged is
/// released and re-taken on the next poll, so the remainder drains across
/// later polls (ADR-0078).
pub(crate) fn take_batch() -> Option<openpiso_core::forwarding::ForwardedBatch> {
    buffer().take_batch()
}

/// Release the lines Admin acknowledged with a 2xx poll response. A no-op
/// when no batch was sent.
pub(crate) fn acknowledge(batch: &Option<openpiso_core::forwarding::ForwardedBatch>) {
    if let Some(batch) = batch {
        buffer().acknowledge(batch);
    }
}

/// Keep the batch's lines for the next poll after a failed or refused send.
/// A no-op when no batch was sent.
pub(crate) fn release(batch: &Option<openpiso_core::forwarding::ForwardedBatch>) {
    if batch.is_some() {
        buffer().release();
    }
}

/// Test-only access to the real buffer and its serialising lock, for the
/// poll-round tests in `main.rs` that drive `poll_once` end to end.
#[cfg(test)]
pub(crate) use tests::{test_buffer, test_lock};

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::{prelude::*, registry};

    /// The real buffer and the serialising lock, for tests elsewhere in the
    /// binary (the poll-round tests in `main.rs`) that drive `poll_once`.
    #[cfg(test)]
    pub(crate) fn test_buffer() -> std::sync::MutexGuard<'static, ForwardingBuffer> {
        buffer()
    }

    #[cfg(test)]
    pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Release builds forward by default; a development build forwards only
    /// when `OPENPISO_FORWARD_DIAGNOSTICS=1` is set explicitly.
    #[test]
    fn forwarding_is_release_only_with_explicit_dev_opt_in() {
        assert!(!forwarding_enabled_for(true, None));
        assert!(!forwarding_enabled_for(true, Some("0")));
        assert!(!forwarding_enabled_for(true, Some("on")));
        assert!(forwarding_enabled_for(true, Some("1")));
        assert!(forwarding_enabled_for(false, None));
        assert!(forwarding_enabled_for(false, Some("0")));
        assert!(forwarding_enabled_for(false, Some("1")));
    }

    static LAYER_TEST_LOCK: LazyLock<std::sync::Mutex<()>> =
        LazyLock::new(|| std::sync::Mutex::new(()));

    #[test]
    fn guard_lines_qualify_through_the_shared_pipeline() {
        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        let subscriber = registry().with(ForwardingLayer);
        tracing::subscriber::with_default(subscriber, || {
            // A foreign-image rejection is WARN: it forwards even though no
            // Client is connected, because the pipe thread logs it itself.
            tracing::warn!(target: openpiso_core::forwarding::GUARD_PIPE_TARGET,
                process_id = 42,
                "rejecting foreign pipe client");
            // The Guard's own connect edge is allowlisted INFO.
            tracing::info!(target: openpiso_core::forwarding::GUARD_PIPE_TARGET,
                process_id = 42,
                "Client connected");
            // Routine INFO and DEBUG never qualify.
            tracing::info!(target: "openpiso_client_guard::pipe", "pipe server starting");
            tracing::debug!(target: "openpiso_client_guard::poll", "tick");
            // The forwarder's own notes must never re-enter the buffer.
            tracing::warn!(target: FORWARDING_TARGET, "could not send batch");
        });

        let mut guard = buffer();
        let batch = guard.take_batch().expect("captured lines should batch");
        let lines = batch.lines();
        assert_eq!(lines.len(), 2, "only WARN and allowlisted INFO stay");
        assert_eq!(lines[0].level, ForwardedLevel::Warn);
        assert_eq!(lines[0].target, "openpiso_client_guard::pipe");
        assert_eq!(
            lines[0].message,
            "rejecting foreign pipe client (process_id=42)"
        );
        assert_eq!(lines[1].level, ForwardedLevel::Info);
        assert_eq!(lines[1].message, "Client connected (process_id=42)");
        assert!(lines[0].timestamp_unix_ms > 0);
        assert!(guard.take_batch().is_none(), "nothing else buffered");
    }

    #[test]
    fn an_unacked_batch_is_released_and_retaken_on_the_next_poll() {
        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        let mut inner = ForwardingBuffer::with_default_capacity();
        inner.record(ForwardedDiagnostic::new(
            ForwardedLevel::Error,
            "openpiso_client_guard::poll",
            "poll failed",
            1_000,
        ));
        *buffer() = inner;

        let batch = take_batch().expect("batch should be taken");
        assert_eq!(batch.len(), 1);
        // A poll that got no usable answer keeps the lines for the next poll.
        release(&Some(batch.clone()));
        let retaken = take_batch().expect("batch should be retaken");
        assert_eq!(retaken, batch);
        // A 2xx poll response is the ack: the lines leave the buffer.
        acknowledge(&Some(retaken));
        assert!(take_batch().is_none());
        // Both helpers are no-ops without a batch, so a Guard that sent
        // nothing is never held by the single-flight invariant.
        release(&None);
        acknowledge(&None);
    }
}

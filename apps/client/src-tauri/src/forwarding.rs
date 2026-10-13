//! Device-side Diagnostic Forwarding (ADR-0078).
//!
//! A tracing layer copies every WARN, ERROR, and INFO log line the Client
//! produces into the shared bounded buffer in `openpiso-core`, which applies
//! the qualification pipeline: WARN and ERROR always qualify, INFO only
//! through the named allowlist and under its rate cap. A dedicated task
//! drains that buffer to Admin's batched Forwarded Diagnostics endpoint. The
//! sender deliberately shares nothing with the Heartbeat loop â€” its own HTTP
//! client, its own cadence â€” so a slow or failing diagnostic send can never
//! delay or fail timer synchronisation.
//!
//! Forwarding is gated to release builds; `OPENPISO_FORWARD_DIAGNOSTICS=1`
//! turns it on deliberately in a development build (ADR-0078).

use std::{sync::LazyLock, time::Duration};

use openpiso_core::forwarding::{ForwardedDiagnostic, ForwardedLevel, ForwardingBuffer};
use tracing::{field::Visit, warn, Event, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

/// The shared device-side buffer. Held in a global so the tracing layer â€”
/// which runs on every thread that logs â€” and the sender task meet here.
static FORWARD_BUFFER: LazyLock<std::sync::Mutex<ForwardingBuffer>> =
    LazyLock::new(|| std::sync::Mutex::new(ForwardingBuffer::with_default_capacity()));

/// Target of the forwarder's own failure notes. These must never re-enter the
/// buffer: "Admin is down" notes produced while Admin is down would fill the
/// ring and crowd out the evidence it exists to keep. The Diagnostic Request
/// uploader shares it.
pub(crate) const FORWARDING_TARGET: &str = "openpiso_client::forwarding";

/// How often the sender drains the buffer.
const FORWARD_INTERVAL_SECONDS: u64 = 5;

/// Total request timeout for a forwarded batch (connect + send). Deliberately
/// separate from the Heartbeat's connect-only client.
const SEND_TIMEOUT_SECONDS: u64 = 3;

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

/// Record a diagnostic that is not already a log line. Used for connection
/// loss, which carries no `tracing` event of its own.
pub(crate) fn record(level: ForwardedLevel, target: &str, message: impl Into<String>) {
    if !forwarding_enabled() {
        return;
    }
    record_captured(level, target, message);
}

/// Capture one line into the buffer without consulting the enable gate.
/// Only the tracing layer (installed exclusively when forwarding is enabled)
/// and the gated [`record`] call this.
fn record_captured(level: ForwardedLevel, target: &str, message: impl Into<String>) {
    let diagnostic = ForwardedDiagnostic::new(
        level,
        target,
        message,
        chrono::Utc::now().timestamp_millis(),
    );
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

/// The message field, with any structured context appended so a forwarded
/// line still carries it; a missing message falls back to a `key=value`
/// summary of its fields. Shared with the Diagnostic Request slice, which
/// re-renders stored JSON log lines and must not drift from this shape.
pub(crate) fn render_message_with_context(message: Option<String>, extra: Vec<String>) -> String {
    let context = extra.join(" ");
    match (message, context.is_empty()) {
        (Some(message), true) => message,
        (Some(message), false) => format!("{message} ({context})"),
        (None, _) => context,
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
        render_message_with_context(self.message, self.extra)
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

/// Drain the buffer to Admin's batched endpoint until acknowledged. Runs
/// alongside the Heartbeat loop in `client_connection::run`, never inside it.
pub(crate) async fn run(
    shared: std::sync::Arc<crate::state::SharedState>,
    boot_mac: crate::client_connection::IdentityMac,
) {
    if !forwarding_enabled() {
        return;
    }
    drain_forever(shared, boot_mac).await;
}

async fn drain_forever(
    shared: std::sync::Arc<crate::state::SharedState>,
    boot_mac: crate::client_connection::IdentityMac,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(SEND_TIMEOUT_SECONDS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut interval = tokio::time::interval(Duration::from_secs(FORWARD_INTERVAL_SECONDS));
    loop {
        interval.tick().await;
        let batch = { buffer().take_batch() };
        let Some(batch) = batch else {
            continue;
        };
        let server_url = shared.config.read().await.server_url.clone();
        let mac = crate::client_connection::resolve_request_mac(&boot_mac, &server_url);
        let url = format!(
            "{}/api/forwarded-diagnostics",
            server_url.trim_end_matches('/')
        );
        let payload = serde_json::json!({
            "source": openpiso_core::forwarding::ForwardedSource::Client,
            "lines": batch.lines(),
        });
        let outcome = crate::client_connection::apply_mac_header(client.post(&url), &mac)
            .json(&payload)
            .send()
            .await;
        match outcome {
            // 2xx is the ack: release exactly the lines Admin received.
            Ok(response) if response.status().is_success() => {
                buffer().acknowledge(&batch);
            }
            // Failed or refused: retain for the next attempt. An
            // unregistered PC (401) is refused every tick until the owner
            // registers it; the buffer's own bound keeps that harmless.
            Ok(response) => {
                buffer().release();
                warn!(
                    target: FORWARDING_TARGET,
                    "Admin refused the forwarded diagnostics batch: {}",
                    response.status()
                );
            }
            Err(error) => {
                buffer().release();
                warn!(
                    target: FORWARDING_TARGET,
                    "Could not forward diagnostics to admin server: {error}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpiso_core::forwarding::DEFAULT_FORWARDING_BUFFER_LINES;
    use tracing_subscriber::{prelude::*, registry};

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
    fn warn_error_and_allowlisted_info_reach_the_buffer_but_unlisted_info_does_not() {
        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        let subscriber = registry().with(ForwardingLayer);
        tracing::subscriber::with_default(subscriber, || {
            // Allowlisted INFO: a Guard pipe connect edge.
            tracing::info!(target: "openpiso_client::guard_client", waited_ms = 2, "connected to Guard pipe");
            tracing::warn!(target: "openpiso_client::test", "guard pipe wobbled");
            tracing::error!(target: "openpiso_client::tamper", "kill report rejected");
            // Unlisted INFO and DEBUG are refused by the buffer's pipeline.
            tracing::info!(target: "openpiso_client::test", "routine noise");
            tracing::debug!(target: "openpiso_client::test", "quiet noise");
            // The forwarder's own failure notes must never re-enter the
            // buffer, or an Admin outage would fill the ring with meta-noise.
            tracing::warn!(target: FORWARDING_TARGET, "could not send batch");
        });

        let mut guard = buffer();
        let batch = guard.take_batch().expect("captured lines should batch");
        let lines = batch.lines();
        assert_eq!(
            lines.len(),
            3,
            "only allowlisted INFO, WARN, and ERROR stay"
        );
        assert_eq!(lines[0].level, ForwardedLevel::Info);
        assert_eq!(lines[0].target, "openpiso_client::guard_client");
        assert_eq!(lines[0].message, "connected to Guard pipe (waited_ms=2)");
        assert_eq!(lines[1].level, ForwardedLevel::Warn);
        assert_eq!(lines[1].message, "guard pipe wobbled");
        assert_eq!(lines[2].level, ForwardedLevel::Error);
        assert_eq!(lines[2].message, "kill report rejected");
        assert!(lines[0].timestamp_unix_ms > 0);
    }

    #[test]
    fn structured_events_without_a_message_forward_a_field_summary() {
        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        let subscriber = registry().with(ForwardingLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "openpiso_client::test", attempts = 3, "retries exhausted");
        });

        let mut guard = buffer();
        let batch = guard.take_batch().expect("captured line should batch");
        assert_eq!(
            batch.lines()[0].message,
            "retries exhausted (attempts=3)",
            "structured context must survive forwarding"
        );
    }

    /// The compact process-lifetime context (issue #368) rides the existing
    /// structured-field rendering: an allowlisted lifecycle event emitted
    /// with `process_id`/`launch` fields keeps them in the forwarded message,
    /// through the same envelope, redaction, qualification, and rate limits
    /// as before.
    #[test]
    fn lifecycle_events_carry_their_process_lifetime_context_into_the_buffer() {
        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        let subscriber = registry().with(ForwardingLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "openpiso_client::startup",
                milestone = "setup_entered",
                elapsed_ms = 40u64,
                process_id = 4242u32,
                launch = "1a2b3c4d",
                "client startup milestone"
            );
            tracing::info!(
                target: "openpiso_client::guard_client",
                waited_ms = 2u64,
                process_id = 4242u32,
                launch = "1a2b3c4d",
                "connected to Guard pipe"
            );
            tracing::info!(
                target: "openpiso_client::guard_client",
                process_id = 4242u32,
                launch = "1a2b3c4d",
                "Client Guard Coverage transitioned from covered to unavailable"
            );
        });

        let mut guard = buffer();
        let batch = guard.take_batch().expect("lifecycle lines should batch");
        assert_eq!(batch.len(), 3, "all three lifecycle lines qualify");
        for line in batch.lines() {
            assert_eq!(line.level, ForwardedLevel::Info);
            assert!(
                line.message.contains("process_id=4242 launch=1a2b3c4d"),
                "the lifetime context must survive rendering: {}",
                line.message
            );
        }
    }

    /// A deterministic burst representing the thousands of temporary
    /// geometry/focus events the removed probes used to emit consumes none
    /// of the forwarded capacity (issue #368): the same OS event stream now
    /// qualifies nothing, meaningful Guard/startup evidence recorded under
    /// that workload stays available, and a deliberately induced real
    /// overflow still produces its explicit marker.
    #[test]
    fn a_geometry_focus_burst_consumes_no_forwarded_capacity() {
        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        let subscriber = registry().with(ForwardingLayer);
        tracing::subscriber::with_default(subscriber, || {
            // The workload the old a4f2 probes turned into WARN flood: every
            // OS resize/move/scale/focus edge of the window's lifetime, now
            // unlogged; plus the probe target at INFO in case anything ever
            // re-emits it.
            for index in 0..5_000u32 {
                tracing::debug!(
                    target: "openpiso_client::window_events",
                    w = 1920u32,
                    h = 1080u32,
                    focused = index % 2 == 0,
                    "window resized"
                );
                tracing::info!(
                    target: "openpiso_client::debug_a4f2",
                    focused = index % 2 == 0,
                    "focus changed"
                );
            }
            // Meaningful evidence under the same workload.
            tracing::info!(
                target: "openpiso_client::startup",
                milestone = "setup_finished",
                elapsed_ms = 900u64,
                process_id = 4242u32,
                launch = "1a2b3c4d",
                "client startup milestone"
            );
            tracing::info!(
                target: "openpiso_client::guard_client",
                process_id = 4242u32,
                launch = "1a2b3c4d",
                "Client Guard Coverage transitioned from unavailable to covered"
            );
            tracing::warn!(
                target: "openpiso_client::window_mode",
                "window operation failed (lock focus): access denied"
            );
        });

        {
            let mut guard = buffer();
            let batch = guard
                .take_batch()
                .expect("meaningful evidence should batch");
            let messages: Vec<&str> = batch
                .lines()
                .iter()
                .map(|line| line.message.as_str())
                .collect();
            assert_eq!(
                messages,
                vec![
                    "client startup milestone (milestone=setup_finished elapsed_ms=900 process_id=4242 launch=1a2b3c4d)",
                    "Client Guard Coverage transitioned from unavailable to covered (process_id=4242 launch=1a2b3c4d)",
                    "window operation failed (lock focus): access denied",
                ],
                "the burst consumes nothing; the meaningful evidence survives intact"
            );
            guard.acknowledge(&batch);
        }

        // A deliberately induced real overflow still gets its explicit
        // marker: genuine WARN traffic floods the ring, and the gap is
        // stated, never silent.
        {
            let mut guard = buffer();
            for _index in 0..(DEFAULT_FORWARDING_BUFFER_LINES * 2) {
                guard.record(ForwardedDiagnostic::new(
                    ForwardedLevel::Warn,
                    "openpiso_client::test",
                    "genuine overflow",
                    1_000,
                ));
            }
            let batch = guard.take_batch().expect("overflow batch");
            assert_eq!(batch.lines()[0].target, "openpiso_forwarding");
            assert!(
                batch.lines()[0]
                    .message
                    .ends_with("forwarded diagnostics dropped: device buffer overflow"),
                "the explicit overflow marker must surface: {}",
                batch.lines()[0].message
            );
        }
    }

    /// A refused batch must be re-sent unchanged and leave the buffer only on ack.
    #[test]
    fn a_refused_batch_is_retried_unchanged_and_released_only_when_acknowledged() {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use serde_json::Value;
        use std::sync::{Arc, Mutex};

        let _serial = LAYER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *buffer() = ForwardingBuffer::with_default_capacity();
        record_captured(ForwardedLevel::Error, "openpiso_client", "connection lost");

        let received: Arc<Mutex<Vec<Value>>> = Arc::default();
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let router = Router::new()
                .route(
                    "/api/forwarded-diagnostics",
                    post(
                        |State(received): State<Arc<Mutex<Vec<Value>>>>,
                         Json(body): Json<Value>| async move {
                            let mut received = received.lock().expect("received");
                            received.push(body);
                            if received.len() == 1 {
                                StatusCode::SERVICE_UNAVAILABLE
                            } else {
                                StatusCode::OK
                            }
                        },
                    ),
                )
                .with_state(received.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind test admin");
            let url = format!("http://{}", listener.local_addr().expect("addr"));
            tokio::spawn(async move { axum::serve(listener, router).await });

            let shared = std::sync::Arc::new(crate::state::SharedState::default());
            shared.config.write().await.server_url = url;
            let sender = tokio::spawn(drain_forever(shared, Some("aa:bb:cc:dd:ee:ff".to_owned())));

            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while !buffer().is_empty() || received.lock().expect("received").len() < 2 {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "sender never acknowledged the retry"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            sender.abort();
        });

        let received = received.lock().expect("received");
        assert_eq!(
            received.len(),
            2,
            "one refused attempt, one acknowledged retry"
        );
        assert_eq!(received[0]["lines"], received[1]["lines"]);
        let lines = received[1]["lines"].as_array().expect("lines");
        assert_eq!(lines.len(), 1, "no loss, no duplication");
        assert_eq!(lines[0]["message"], "connection lost");
    }
}

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tracing::{error, warn};

use crate::offline_shutdown;
use crate::shutdown;
use crate::state::{
    ActiveSession, CoinFeedback, CountdownAnchor, PendingTamperKillReport, SharedState, ShopStatus,
    ThemeRecord, TimerState,
};

const TIMER_UPDATED_EVENT: &str = "timer-updated";
const THEME_UPDATED_EVENT: &str = "theme-updated";
const COIN_ACCEPTED_EVENT: &str = "coin-accepted";

/// How often the Client re-fetches the public shop name (ADR-0027 §4.4).
/// Kept separate from the heartbeat so a slow `GET /api/shop` cannot stall
/// the authoritative timer stream.
const SHOP_NAME_REFRESH_SECONDS: u64 = 30;

fn default_power_state() -> String {
    "active".to_owned()
}

fn default_shutdown_grace_seconds() -> i64 {
    shutdown::DEFAULT_GRACE_SECONDS
}

fn default_offline_grace_seconds() -> i64 {
    crate::state::default_offline_grace_seconds()
}

#[derive(Debug, Deserialize)]
pub struct PcResponse {
    id: String,
    name: String,
    status: String,
    remaining_seconds: i64,
    guest_remaining_seconds: i64,
    timer_mode: String,
    active_session: Option<ActiveSession>,
    /// Older Admin responses omit this capability flag; preserve cached support.
    #[serde(default)]
    attendance_supported: Option<bool>,
    #[serde(default = "default_power_state")]
    power_state: String,
    #[serde(default = "default_shutdown_grace_seconds")]
    shutdown_grace_seconds: i64,
    #[serde(default)]
    theme_version: i64,
    /// Present as `"recorded"` when the server processed a Tamper Kill report
    /// from this heartbeat. The Client clears its pending report on receipt.
    #[serde(default)]
    tamper_kill_acknowledged: Option<TamperKillAck>,
    /// Server-resolved subject of the acknowledged Tamper Kill. It keeps an
    /// account kick with leftover Guest Time distinct from a guest kick.
    #[serde(default)]
    tamper_kill_subject: Option<TamperKillSubject>,
    /// Server-resolved Shop Status carried by the existing `pc.updated`
    /// state stream. The Client only renders this value.
    #[serde(default)]
    shop_status: Option<ShopStatus>,
    /// The Diagnostic Request nonce (ADR-0078): present only while an owner's
    /// ask for one windowed, byte-capped log slice is armed. Older servers
    /// omit the field without failing the Heartbeat; the Client acts once per
    /// nonce, on its own uploader task, never on this one.
    #[serde(default)]
    diagnostic_request_nonce: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TamperKillAck {
    Recorded,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TamperKillSubject {
    Account,
    Guest,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct ShopNameResponse {
    shop_name: String,
    #[serde(default = "default_shutdown_grace_seconds")]
    shutdown_grace_seconds: i64,
    #[serde(default = "default_offline_grace_seconds")]
    offline_grace_seconds: i64,
}

/// Body the server sends on a typed `pc_unregistered` rejection
/// (ADR-0031). The client reads `error == "pc_unregistered"` and
/// flips `connection_status` to `"unregistered"`; the Lock renders the
/// "ask the owner to register this PC" copy.
#[derive(Deserialize)]
struct UnregisteredRejection {
    error: String,
}

#[derive(Serialize)]
struct HeartbeatRequest {
    hostname: String,
    client_version: &'static str,
    client_time_unix_ms: i64,
    /// Client Guard Coverage diagnostic (ADR-0076): `covered` when this
    /// release Windows build holds its Guard pipe, `unavailable` when it
    /// does not, and `not_applicable` for debug and non-Windows builds.
    guard_coverage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tamper_kill_timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tamper_kill_report_age_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tamper_kill_attempt_count: Option<i64>,
}

/// Per-boot MAC the client sends in the `X-OpenPiso-Mac` header on
/// every identity request. Resolved once at connection-task start
/// (ADR-0031): the MAC of the interface that carries the route to the
/// admin server, falling back to the lowest-sorted physical MAC.
/// `None` when the platform refuses to enumerate any physical
/// adapter — the server then resolves by source IP only.
pub type IdentityMac = Option<String>;

/// Bound only the *connection* phase (TCP connect + TLS handshake), not the
/// whole request: the same `Client` drives the long-lived SSE event stream,
/// so a total request timeout would kill it mid-stream. A connect timeout
/// lets a misconfigured or unreachable `server_url` surface as a connection
/// error within this window instead of hanging silently — which is what
/// makes the Lock's reconnect affordance appear so the owner can fix the URL.
const CONNECT_TIMEOUT_SECONDS: u64 = 5;

pub(crate) fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub async fn run(shared: Arc<SharedState>, app: AppHandle) {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECONDS))
        .build()
        .unwrap_or_else(|_| Client::new());
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "openpiso-client".to_owned());

    // Resolve the boot MAC from the interface that carries the route
    // to the configured admin server. A first heartbeat may race a
    // freshly-typed `server_url`; the route walker uses whatever host
    // the config currently holds, and the boot-time identity is
    // cached here for the lifetime of the connection task.
    let boot_url = shared.config.read().await.server_url.clone();
    let boot_mac = crate::mac_address::collect_mac_for_admin(&admin_host_from_url(&boot_url));

    let initial_theme_url = shared.config.read().await.server_url.clone();
    let (theme_version_tx, theme_version_rx) = tokio::sync::watch::channel(None);

    // Local handoff inbox (issue #367). The sender persists without a
    // runtime; the running Client drains the same directory the sender
    // resolves, while the authoritative file stays on the Tauri path.
    let handoff_config_dir = crate::startup_coordinator::resolve_config_dir();
    let handoff_main_path = crate::config_store::tamper_kill_report_path(&app).ok();

    tokio::join!(
        run_heartbeats(
            Arc::clone(&shared),
            app.clone(),
            client.clone(),
            hostname,
            boot_mac.clone(),
            theme_version_tx,
        ),
        run_theme_updates(
            Arc::clone(&shared),
            client.clone(),
            app.clone(),
            initial_theme_url,
            theme_version_rx
        ),
        run_event_stream(
            Arc::clone(&shared),
            app.clone(),
            client.clone(),
            boot_mac.clone()
        ),
        run_shop_name_refresh(Arc::clone(&shared), client, app.clone()),
        // Diagnostic Forwarding (ADR-0078) is a separate loop on purpose: it
        // shares nothing with the Heartbeat's authority path, so a slow or
        // failing diagnostic send can never delay or fail timer
        // synchronisation. It resolves the boot identity, not the heartbeat.
        crate::forwarding::run(Arc::clone(&shared), boot_mac.clone()),
        // The Diagnostic Request uploader (ADR-0078) is a third loop for the
        // same reason: it answers the owner's ask off the Heartbeat path, on
        // its own connection and cadence.
        crate::diagnostic_request::run(Arc::clone(&shared), boot_mac),
        run_local_countdown(shared.clone(), app.clone()),
        run_handoff_ingest(shared, app, handoff_config_dir, handoff_main_path),
    );
}

/// Extract the host portion of an admin server URL for the
/// `route-to-host` walker. `http://192.168.1.10:3210` → `"192.168.1.10"`.
fn admin_host_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let after_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let host_port = after_scheme.split('/').next().unwrap_or("");
    let host = host_port
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(host_port);
    host.to_owned()
}

/// Re-resolve the boot MAC at the moment of the next heartbeat when
/// the configured admin URL has changed. The connection task
/// captures the URL at start, so a `set_client_config` repoint does
/// not invalidate the boot identity — the first heartbeat from the
/// new URL is allowed to walk the route against the new host.
pub(crate) fn resolve_request_mac(boot_mac: &IdentityMac, server_url: &str) -> IdentityMac {
    if boot_mac.is_some() {
        return boot_mac.clone();
    }
    crate::mac_address::collect_mac_for_admin(&admin_host_from_url(server_url))
}

/// Build the `X-OpenPiso-Mac` header request builder. Always called
/// from a context that holds an `IdentityMac` (either a boot-time
/// value or a per-request re-resolve) so the header is stable.
pub(crate) fn apply_mac_header(
    builder: reqwest::RequestBuilder,
    mac: &IdentityMac,
) -> reqwest::RequestBuilder {
    match mac {
        Some(value) => builder.header("X-OpenPiso-Mac", value),
        None => builder,
    }
}

async fn run_heartbeats(
    shared: Arc<SharedState>,
    app: AppHandle,
    client: Client,
    hostname: String,
    boot_mac: IdentityMac,
    theme_version_tx: tokio::sync::watch::Sender<Option<(String, i64)>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let mut previous_coverage = crate::guard_client::current_heartbeat_coverage();

    loop {
        interval.tick().await;
        // The Client Guard Coverage the heartbeat is about to report is part
        // of the story Admin sees; record its transitions so a flap or a
        // contradiction is diagnosable from the forwarded log alone
        // (ADR-0078's INFO allowlist).
        let coverage = crate::guard_client::current_heartbeat_coverage();
        if coverage != previous_coverage {
            tracing::info!(
                target: openpiso_core::forwarding::CLIENT_GUARD_TARGET,
                process_id = crate::diagnostics::process_id(),
                launch = crate::diagnostics::launch_token(),
                "Client Guard Coverage transitioned from {previous_coverage} to {coverage}"
            );
            previous_coverage = coverage;
        }
        let request_started_at = Instant::now();
        let config = shared.config.read().await.clone();
        let mac = resolve_request_mac(&boot_mac, &config.server_url);
        let endpoint = format!(
            "{}/api/identity/heartbeat",
            config.server_url.trim_end_matches('/')
        );
        let (pending, request_generation) = capture_tamper_request_state(&shared).await;
        let client_time_unix_ms = unix_time_millis();
        let (tamper_kill_timestamp, tamper_kill_report_age_ms, tamper_kill_attempt_count) =
            crate::startup_coordinator::heartbeat_tamper_fields(pending, client_time_unix_ms);

        let response = apply_mac_header(client.post(endpoint), &mac)
            .json(&HeartbeatRequest {
                hostname: hostname.clone(),
                client_version: env!("CARGO_PKG_VERSION"),
                client_time_unix_ms,
                guard_coverage: coverage,
                tamper_kill_timestamp,
                tamper_kill_report_age_ms,
                tamper_kill_attempt_count,
            })
            .send()
            .await;

        match response {
            Ok(response) => {
                let status = response.status();
                if status == reqwest::StatusCode::NOT_FOUND {
                    match response.json::<UnregisteredRejection>().await {
                        Ok(rejection) if rejection.error == "pc_unregistered" => {
                            set_unregistered(&shared, &app, request_started_at).await;
                        }
                        Ok(_) | Err(_) => {
                            set_connection_error(
                                &shared,
                                &app,
                                "unrecognized 404 from admin server".to_owned(),
                                request_started_at,
                            )
                            .await;
                        }
                    }
                } else if !status.is_success() {
                    let text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| format!("heartbeat returned {status}"));
                    set_connection_error(
                        &shared,
                        &app,
                        format!("heartbeat returned {status}: {text}"),
                        request_started_at,
                    )
                    .await;
                } else {
                    // The Admin accepted this identity, so this is the pair
                    // the Guard should poll with (ADR-0077). Reported here
                    // rather than at startup because it is only known to be
                    // correct once a heartbeat has succeeded with it.
                    crate::guard_client::report_endpoint(&config.server_url, mac.as_deref());
                    match response.json::<PcResponse>().await {
                        Ok(pc) => {
                            // The Diagnostic Request is diagnostic-only and
                            // deduplicated by nonce, so it is acted on even
                            // when a pending Tamper Kill report withholds
                            // authority application this round; recording it
                            // here costs the Heartbeat path nothing but a
                            // mutex flip (ADR-0078).
                            if let Some(nonce) = pc.diagnostic_request_nonce.as_deref() {
                                crate::diagnostic_request::on_heartbeat_nonce(nonce);
                            }
                            let tamper_kill_acknowledged = pc.tamper_kill_acknowledged;
                            let theme_version = pc.theme_version;
                            // While a relaunch report is pending, this is the
                            // only response allowed to establish connected
                            // authority. A successful response without the
                            // acknowledgement may be from an older server or
                            // an incomplete retry; keep the UI in Connecting
                            // rather than exposing a stale session snapshot.
                            if pending.is_some()
                                && tamper_kill_acknowledged != Some(TamperKillAck::Recorded)
                            {
                                warn!(
                                    target: "openpiso_client::tamper",
                                    "heartbeat succeeded without acknowledging the pending Tamper Kill report"
                                );
                                continue;
                            }
                            if !apply_heartbeat_state(&shared, &app, pc, request_generation).await {
                                warn!(
                                    target: "openpiso_client::tamper",
                                    "discarded heartbeat response from before a newer Tamper Kill report"
                                );
                                continue;
                            }
                            if tamper_kill_acknowledged == Some(TamperKillAck::Recorded) {
                                if let Some(acknowledged) = pending {
                                    // Serialized with local handoff ingestion:
                                    // a delayed ack for this heartbeat cannot
                                    // erase a newer handed-off report.
                                    let main_path =
                                        crate::config_store::tamper_kill_report_path(&app).ok();
                                    crate::startup_coordinator::acknowledge_server_ack(
                                        &shared,
                                        main_path.as_deref(),
                                        acknowledged,
                                    )
                                    .await;
                                }
                            }
                            theme_version_tx
                                .send_replace(Some((config.server_url.clone(), theme_version)));
                        }
                        Err(error) => {
                            set_connection_error(
                                &shared,
                                &app,
                                error.to_string(),
                                request_started_at,
                            )
                            .await
                        }
                    }
                }
            }
            Err(error) => {
                set_connection_error(&shared, &app, error.to_string(), request_started_at).await
            }
        }
    }
}

async fn run_theme_updates(
    shared: Arc<SharedState>,
    client: Client,
    app: AppHandle,
    initial_server_url: String,
    mut theme_versions: tokio::sync::watch::Receiver<Option<(String, i64)>>,
) {
    let mut current_key = None;
    if let Some(theme) = fetch_theme(&client, &initial_server_url).await {
        current_key = Some((initial_server_url, theme.version));
        *shared.theme.write().await = Some(theme.clone());
        emit_theme_updated(&app, &theme);
    }

    while theme_versions.changed().await.is_ok() {
        let next_key = theme_versions.borrow_and_update().clone();
        let Some((server_url, heartbeat_version)) = next_key else {
            continue;
        };
        if current_key == Some((server_url.clone(), heartbeat_version)) {
            continue;
        }
        if let Some(theme) = fetch_theme(&client, &server_url).await {
            current_key = Some((server_url, theme.version));
            *shared.theme.write().await = Some(theme.clone());
            emit_theme_updated(&app, &theme);
        }
    }
}

async fn fetch_theme(client: &Client, server_url: &str) -> Option<ThemeRecord> {
    let endpoint = format!("{}/api/theme", server_url.trim_end_matches('/'));
    match client
        .get(endpoint)
        .timeout(Duration::from_secs(CONNECT_TIMEOUT_SECONDS))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => match response.json().await {
            Ok(theme) => Some(theme),
            Err(error) => {
                warn!(target: "openpiso_client", "Could not parse theme response: {error}");
                None
            }
        },
        Ok(response) => {
            warn!(
                target: "openpiso_client",
                status = %response.status(),
                "Theme endpoint returned non-success; keeping cached theme"
            );
            None
        }
        Err(error) => {
            warn!(
                target: "openpiso_client",
                "Could not refresh theme (server unreachable?): {error}"
            );
            None
        }
    }
}

fn emit_theme_updated(app: &AppHandle, theme: &ThemeRecord) {
    if let Err(error) = app.emit(THEME_UPDATED_EVENT, theme) {
        warn!(target: "openpiso_client", "Could not emit theme update: {error}");
    }
}

async fn run_event_stream(
    shared: Arc<SharedState>,
    app: AppHandle,
    client: Client,
    boot_mac: IdentityMac,
) {
    loop {
        let request_started_at = Instant::now();
        let config = shared.config.read().await.clone();
        let mac = resolve_request_mac(&boot_mac, &config.server_url);
        let endpoint = format!(
            "{}/api/identity/events",
            config.server_url.trim_end_matches('/')
        );
        let stream_generation = prepare_event_stream_generation(&shared).await;

        let response = apply_mac_header(client.get(endpoint), &mac)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await;

        match response {
            Ok(response) => {
                let status = response.status();
                if status == reqwest::StatusCode::NOT_FOUND {
                    set_unregistered(&shared, &app, request_started_at).await;
                } else if !status.is_success() {
                    let text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| format!("event stream returned {status}"));
                    set_connection_error(
                        &shared,
                        &app,
                        format!("event stream returned {status}: {text}"),
                        request_started_at,
                    )
                    .await;
                } else {
                    match consume_event_stream(&shared, &app, response, stream_generation).await {
                        EventStreamResult::Closed => {
                            set_connection_error(
                                &shared,
                                &app,
                                "event stream closed while connected".to_owned(),
                                request_started_at,
                            )
                            .await;
                        }
                        EventStreamResult::TamperKillPending => {
                            // Drop the stream opened before a new pending
                            // report appeared. Reconnecting after the
                            // acknowledgement gives us a fresh initial
                            // snapshot instead of replaying stale state.
                        }
                    }
                }
            }
            Err(error) => {
                set_connection_error(&shared, &app, error.to_string(), request_started_at).await
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Keep the SSE channel behind the heartbeat authority barrier during a
/// Tamper Kill relaunch. The first SSE snapshot can describe the pre-kick
/// account, so it must not be allowed to win the race with the report.
async fn wait_for_tamper_kill_acknowledgement(shared: &SharedState) {
    loop {
        let notified = shared.tamper_kill_acknowledged.notified();
        if shared.pending_tamper_kill.read().await.is_none() {
            return;
        }
        notified.await;
    }
}

async fn capture_tamper_request_state(
    shared: &SharedState,
) -> (Option<PendingTamperKillReport>, u64) {
    let _transition = shared.connection_transition.lock().await;
    let pending = *shared.pending_tamper_kill.read().await;
    let generation = shared
        .tamper_kill_generation
        .load(std::sync::atomic::Ordering::Relaxed);
    (pending, generation)
}

async fn prepare_event_stream_generation(shared: &SharedState) -> u64 {
    loop {
        wait_for_tamper_kill_acknowledgement(shared).await;
        let _transition = shared.connection_transition.lock().await;
        if shared.pending_tamper_kill.read().await.is_some() {
            continue;
        }
        return shared
            .tamper_kill_generation
            .load(std::sync::atomic::Ordering::Relaxed);
    }
}

fn should_defer_event_stream_snapshot(
    pending: Option<PendingTamperKillReport>,
    stream_generation: u64,
    current_generation: u64,
) -> bool {
    pending.is_some() || stream_generation != current_generation
}

/// Refresh the cached `shop_name` from the public `GET /api/shop`
/// endpoint on connect and every `SHOP_NAME_REFRESH_SECONDS` after
/// (ADR-0027 §4.4). Kept off the heartbeat path so a slow shop endpoint
/// can never stall the authoritative timer stream. Failures are silent:
/// the cached value survives a transient server outage and the customer
/// keeps seeing the last-known shop name.
async fn run_shop_name_refresh(shared: Arc<SharedState>, client: Client, app: AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_secs(SHOP_NAME_REFRESH_SECONDS));
    // The first tick fires immediately so a brand-new connect populates
    // the cache before the customer ever sees the Lock header.
    interval.tick().await;
    loop {
        let config = shared.config.read().await.clone();
        let endpoint = format!("{}/api/shop", config.server_url.trim_end_matches('/'));
        match client.get(&endpoint).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<ShopNameResponse>().await {
                    Ok(payload) => {
                        let trimmed = payload.shop_name.trim();
                        if !trimmed.is_empty() {
                            *shared.shop_name.write().await = trimmed.to_owned();
                        }
                        if payload.shutdown_grace_seconds > 0 {
                            *shared.shutdown_grace_seconds.write().await =
                                payload.shutdown_grace_seconds;
                            let mut timer = shared.timer.write().await;
                            timer.shutdown_grace_seconds = payload.shutdown_grace_seconds;
                        }
                        if payload.offline_grace_seconds > 0 {
                            let mut config = shared.config.write().await;
                            if config.offline_grace_seconds != payload.offline_grace_seconds {
                                config.offline_grace_seconds = payload.offline_grace_seconds;
                                if let Err(error) = crate::config_store::save(&app, &config) {
                                    warn!(
                                        target: "openpiso_client::config_store",
                                        "Could not cache Offline Grace: {error}"
                                    );
                                }
                            }
                        }
                    }
                    Err(error) => {
                        warn!(
                            target: "openpiso_client",
                            "Could not parse shop name response: {error}"
                        );
                    }
                }
            }
            Ok(response) => {
                warn!(
                    target: "openpiso_client",
                    status = %response.status(),
                    "Shop name endpoint returned non-success; keeping cached value"
                );
            }
            Err(error) => {
                warn!(
                    target: "openpiso_client",
                    "Could not refresh shop name (server unreachable?): {error}"
                );
            }
        }
        interval.tick().await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventStreamResult {
    Closed,
    TamperKillPending,
}

async fn consume_event_stream(
    shared: &SharedState,
    app: &AppHandle,
    response: reqwest::Response,
    stream_generation: u64,
) -> EventStreamResult {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();

    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return EventStreamResult::Closed;
        };
        buffer.extend_from_slice(&chunk);
        if buffer.len() > 64 * 1024 {
            return EventStreamResult::Closed;
        }

        while let Some(end) = event_boundary(&buffer) {
            let event = buffer.drain(..end).collect::<Vec<_>>();
            let delimiter_length = if buffer.starts_with(b"\r\n\r\n") {
                4
            } else {
                2
            };
            buffer.drain(..delimiter_length);

            if let Some(pc) = parse_sse_event::<PcResponse>(&event, "pc.updated") {
                if should_defer_event_stream_snapshot(
                    *shared.pending_tamper_kill.read().await,
                    stream_generation,
                    shared
                        .tamper_kill_generation
                        .load(std::sync::atomic::Ordering::Relaxed),
                ) {
                    return EventStreamResult::TamperKillPending;
                }
                if !apply_event_stream_state(shared, app, pc, stream_generation).await {
                    return EventStreamResult::TamperKillPending;
                }
            } else if let Some(transaction) =
                parse_sse_event::<CoinFeedback>(&event, "coin.accepted")
            {
                let _ = app.emit(COIN_ACCEPTED_EVENT, transaction);
            }
        }
    }
    EventStreamResult::Closed
}

fn event_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| buffer.windows(2).position(|window| window == b"\n\n"))
}

fn parse_sse_event<T: serde::de::DeserializeOwned>(event: &[u8], expected_name: &str) -> Option<T> {
    let text = std::str::from_utf8(event).ok()?;
    let mut event_name = None;
    let mut data = Vec::new();

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
        }
    }

    if event_name != Some(expected_name) || data.is_empty() {
        return None;
    }
    serde_json::from_str(&data.join("\n")).ok()
}

/// Determine the authoritative `remaining_seconds` to expose on the
/// client timer.
///
/// Three cases:
/// 1. **Matching auth** — the client is authenticated for the server's
///    active session → use the server-reported (session-enriched) value.
/// 2. **Stale session** — the server has a live session binding but the
///    client has no credentials for it (previous customer walked away
///    without logging out) → only the PC's own guest time is freely
///    usable; the session-enriched time must not leak to the next person.
/// 3. **No session** — the server's reported time is the PC's own guest
///    time (no enrichment) → use it as-is.
fn authoritative_remaining_seconds(
    has_matching_auth: bool,
    has_active_session: bool,
    reported_remaining_seconds: i64,
    guest_remaining_seconds: i64,
) -> i64 {
    if has_matching_auth {
        reported_remaining_seconds
    } else if has_active_session {
        // Stale session: previous customer walked away without logging out.
        // Only guest time on the PC is freely usable; the session-enriched
        // time belongs to the other account.
        guest_remaining_seconds
    } else {
        reported_remaining_seconds
    }
}

fn auth_matches_session(auth: &crate::state::AccountAuthState, session: &ActiveSession) -> bool {
    session.id == auth.session.id && session.account_id == auth.account.id
}

fn should_lock_guest_after_tamper(pc: &PcResponse) -> bool {
    pc.tamper_kill_acknowledged == Some(TamperKillAck::Recorded)
        && pc.tamper_kill_subject == Some(TamperKillSubject::Guest)
        && pc.timer_mode == "guest"
        && pc.guest_remaining_seconds > 0
        && pc.power_state != "shutdown"
}

fn should_clear_account_auth_for_server_state(
    auth: &crate::state::AccountAuthState,
    active_session: Option<&ActiveSession>,
) -> bool {
    match active_session {
        Some(session) => !auth_matches_session(auth, session),
        None => auth.account.remaining_seconds > 0,
    }
}

pub(crate) async fn apply_server_state(shared: &SharedState, app: &AppHandle, pc: PcResponse) {
    let _ = apply_server_state_if_generation(shared, app, pc, None, false).await;
}

async fn apply_event_stream_state(
    shared: &SharedState,
    app: &AppHandle,
    pc: PcResponse,
    stream_generation: u64,
) -> bool {
    apply_server_state_if_generation(shared, app, pc, Some(stream_generation), true).await
}

async fn apply_heartbeat_state(
    shared: &SharedState,
    app: &AppHandle,
    pc: PcResponse,
    request_generation: u64,
) -> bool {
    apply_server_state_if_generation(shared, app, pc, Some(request_generation), false).await
}

async fn apply_server_state_if_generation(
    shared: &SharedState,
    app: &AppHandle,
    pc: PcResponse,
    expected_generation: Option<u64>,
    reject_pending: bool,
) -> bool {
    let _transition = shared.connection_transition.lock().await;
    let generation_changed = expected_generation.is_some_and(|expected| {
        expected
            != shared
                .tamper_kill_generation
                .load(std::sync::atomic::Ordering::Relaxed)
    });
    let tamper_kill_pending = reject_pending
        && expected_generation.is_some()
        && shared.pending_tamper_kill.read().await.is_some();
    if generation_changed || tamper_kill_pending {
        return false;
    }
    *shared.last_successful_connection.write().await = Some(Instant::now());
    // Adopt the server-resolved `pc_id` (ADR-0031 §2 / plan §4.4 / M4).
    // A heartbeat from a fresh install brings back a `pc_id` we did not
    // have before; an established client is re-bound only if the
    // server has changed the row's identity (e.g. a hardware swap was
    // cleared by the owner and re-bound by IP). Either way the
    // server's word wins and the cached value is updated and persisted
    // (no-op guarded via the M1 save path).
    let resolved_pc_id = pc.id.clone();
    let prior_pc_id = shared.config.read().await.pc_id.clone();
    let adopted = prior_pc_id.as_deref() != Some(resolved_pc_id.as_str());
    if adopted {
        let mut config = shared.config.write().await;
        if config.pc_id.as_deref() != Some(resolved_pc_id.as_str()) {
            config.pc_id = Some(resolved_pc_id.clone());
        }
        drop(config);
    }
    {
        let mut timer = shared.timer.write().await;
        timer.pc_id = Some(resolved_pc_id);
    }

    let shutdown_grace_seconds = pc.shutdown_grace_seconds.max(1);
    *shared.shutdown_grace_seconds.write().await = shutdown_grace_seconds;

    cancel_offline_shutdown(shared, app, "connected").await;
    manage_shutdown_deadline(shared, &pc.power_state).await;

    let snapshot = apply_timer_and_auth(shared, &pc).await;
    emit_timer_updated(app, snapshot);
    true
}

/// The app-free half of a heartbeat: fold the server's PC state into the
/// timer, reconcile `account_auth`, and re-anchor the local countdown.
/// Returns the timer snapshot the caller emits.
///
/// Split out of `apply_server_state` so the heartbeat/local-countdown
/// interleaving can be tested without a Tauri runtime.
pub(crate) async fn apply_timer_and_auth(shared: &SharedState, pc: &PcResponse) -> TimerState {
    let remaining_seconds = pc.remaining_seconds.max(0);
    // Phase 1: apply all timer fields from the server heartbeat.
    {
        let mut timer = shared.timer.write().await;
        timer.pc_name = pc.name.clone();
        timer.pc_status = pc.status.clone();
        timer.remaining_seconds = remaining_seconds;
        timer.guest_remaining_seconds = pc.guest_remaining_seconds.max(0);
        timer.timer_mode = pc.timer_mode.clone();
        if let Some(attendance_supported) = pc.attendance_supported {
            timer.attendance_supported = attendance_supported;
        }
        timer.active_username = pc
            .active_session
            .as_ref()
            .map(|session| session.username.clone());
        timer.connected = true;
        timer.connection_status = "connected".to_owned();
        timer.expired = timer.remaining_seconds == 0;
        timer.power_state = pc.power_state.clone();
        timer.shutdown_grace_seconds = pc.shutdown_grace_seconds.max(1);
        timer.shop_status = pc.shop_status.clone();
        if pc.tamper_kill_acknowledged == Some(TamperKillAck::Recorded) {
            timer.tamper_kill_pending = false;
        }
        timer.offline_shutdown_in_seconds = None;
        timer.error = None;
    }
    // Phase 2: sync account_auth — keep matching auth, clear stale auth.
    let mut account_auth = shared.account_auth.write().await;
    if let Some(auth) = account_auth.as_mut() {
        match pc.active_session.as_ref() {
            Some(session) if auth_matches_session(auth, session) => {
                auth.account.remaining_seconds = session.remaining_seconds;
                auth.account.points_balance_centi = session.points_balance_centi;
                auth.session = session.clone();
            }
            _ if should_clear_account_auth_for_server_state(auth, pc.active_session.as_ref()) => {
                *account_auth = None;
            }
            _ => {}
        }
    }
    let has_matching_auth = account_auth.is_some();
    drop(account_auth);
    if has_matching_auth && pc.active_session.is_some() {
        // Account sign-in is an explicit alternate path from the neutral
        // guest-kick Lock. Once the server confirms the new Active Session,
        // the account may use its own portable Remaining Time.
        *shared.guest_tamper_kick_pending.write().await = false;
        shared.timer.write().await.overlay_locked = false;
    }
    // Phase 3: stale-session guard (Bug #2).
    // If the server has a live session binding but the client has no
    // matching credentials, a previous customer walked away without
    // logging out. The server's `remaining_seconds` is the
    // session-enriched balance from that customer's account — don't
    // expose it as freely-usable guest time. Fall back to the PC's own
    // guest time so the next person sees only real guest time (if any).
    let snapshot = if !has_matching_auth && pc.active_session.is_some() {
        let authoritative = authoritative_remaining_seconds(
            false,
            true,
            remaining_seconds,
            pc.guest_remaining_seconds.max(0),
        );
        let mut timer = shared.timer.write().await;
        timer.remaining_seconds = authoritative;
        timer.expired = authoritative == 0;
        timer.active_username = None;
        timer.clone()
    } else {
        shared.timer.read().await.clone()
    };
    // A guest was kicked without losing its PC-bound Guest Time. Keep the
    // Client in the Fullscreen Lock until the customer explicitly continues;
    // this is the neutral customer-facing consequence of Tamper Kick.
    let snapshot = if should_lock_guest_after_tamper(pc) {
        *shared.guest_tamper_kick_pending.write().await = true;
        let mut timer = shared.timer.write().await;
        timer.overlay_locked = true;
        timer.clone()
    } else {
        snapshot
    };
    // Re-anchor the local countdown from the value the timer *ended up*
    // holding, never from the raw server field. Anchoring on the raw value
    // let the stale-session guard above be undone 250 ms later by
    // `project_local_countdown`, which re-projected the other account's
    // session time — the frontend derives Lock vs Widget from
    // `remaining_seconds` alone, so the view flipped Lock <-> Widget for as
    // long as the stale binding lived.
    *shared.countdown_anchor.write().await = Some(CountdownAnchor {
        remaining_seconds: snapshot.remaining_seconds,
        synchronized_at: std::time::Instant::now(),
    });
    snapshot
}

/// Clear owner unlock/pause state so a later offline transition cannot
/// inherit an unlocked panel from the previous outage.
pub(crate) async fn reset_owner_settings_state(shared: &SharedState) {
    *shared.owner_settings_unlocked.write().await = false;
    *shared.offline_shutdown_owner_unlocked.write().await = false;
    *shared.offline_shutdown_paused_at.write().await = None;
}

/// Cancel the client-side Offline Shutdown Countdown after a successful
/// connected server response. The server's next authoritative state then
/// decides whether the normal expiry grace countdown applies.
pub(crate) async fn cancel_offline_shutdown(
    shared: &SharedState,
    app: &AppHandle,
    next_connection_status: &str,
) {
    if !offline_shutdown::should_cancel(next_connection_status) {
        return;
    }
    let _transition = shared.offline_shutdown_transition.lock().await;
    cancel_offline_shutdown_state(shared).await;
    if let Err(error) = crate::config_store::clear_offline_shutdown_deadline(app) {
        warn!(target: "openpiso_client::config_store", "Could not clear Offline Shutdown deadline after reconnect: {error}");
    }
}

/// In-memory half of the reconnect cancellation, split out from the persisted
/// deadline so it is testable without an `AppHandle`.
///
/// This clears the *countdown*, not the owner gate: a heartbeat lands every 5s
/// and must not revoke a PIN unlock the owner is actively using. Only the
/// pause bookmark is dropped (there is no countdown left to pause); the
/// panel-open flags stay so a later disconnect still pauses the fresh
/// countdown. `resume_offline_shutdown` (panel closed) and
/// `reset_owner_settings_state` (server repoint) own the re-locking.
async fn cancel_offline_shutdown_state(shared: &SharedState) {
    disarm_offline_shutdown(shared, "the admin server is reachable again").await;
    *shared.offline_shutdown_paused_at.write().await = None;
    *shared.offline_shutdown_issued_at.write().await = None;
    shared.timer.write().await.offline_shutdown_in_seconds = None;
}

/// Clear the Offline Shutdown Countdown deadline and, when one was actually
/// armed, log the disarm edge so the story the arm edge opened closes in the
/// forwarded log (ADR-0078's INFO allowlist). Cancellation with nothing
/// armed — a routine heartbeat, an owner quit before any disconnect — stays
/// silent.
pub(crate) async fn disarm_offline_shutdown(shared: &SharedState, reason: &str) {
    let was_armed = shared.offline_shutdown_deadline.read().await.is_some();
    *shared.offline_shutdown_deadline.write().await = None;
    if was_armed {
        tracing::info!(
            target: openpiso_core::forwarding::CLIENT_OFFLINE_SHUTDOWN_TARGET,
            "Offline Shutdown Countdown cancelled: {reason}"
        );
    }
}

/// Arm or cancel the shutdown grace countdown when the server's power command
/// changes. Arming records a deadline one grace window away; any non-shutdown
/// command (including a top-up back to time remaining) cancels it and resets the
/// one-shot guard so a later expiry can shut down again (ADR-0022).
async fn manage_shutdown_deadline(shared: &SharedState, power_state: &str) {
    if power_state == "shutdown" {
        let mut deadline = shared.shutdown_deadline.write().await;
        if deadline.is_none() {
            let grace_seconds = (*shared.shutdown_grace_seconds.read().await).max(1) as u64;
            *deadline = Some(Instant::now() + Duration::from_secs(grace_seconds));
        }
    } else {
        *shared.shutdown_deadline.write().await = None;
        *shared.shutdown_issued_at.write().await = None;
    }
}

/// Arm the Offline Shutdown Countdown when the Client crosses from connected
/// to disconnected. Repeated transport failures do not reset its deadline,
/// and setup/connecting/unregistered/never-connected states cannot arm it.
async fn arm_offline_shutdown_if_needed(
    shared: &SharedState,
    app: &AppHandle,
    previously_connected: bool,
    next_connection_status: &str,
    grace_seconds: i64,
) {
    let now = Instant::now();
    let Some(deadline) = offline_shutdown::arm_deadline(
        previously_connected,
        next_connection_status,
        now,
        grace_seconds,
    ) else {
        return;
    };

    let _transition = shared.offline_shutdown_transition.lock().await;
    let owner_unlocked = *shared.offline_shutdown_owner_unlocked.read().await;
    let mut paused_at = shared.offline_shutdown_paused_at.write().await;
    let mut current_deadline = shared.offline_shutdown_deadline.write().await;
    if current_deadline.is_none() {
        *current_deadline = Some(deadline);
        *paused_at = offline_shutdown::pause_at_arm(owner_unlocked, now);
        *shared.offline_shutdown_issued_at.write().await = None;
        // The arm edge is part of the story Admin needs: it says this PC
        // crossed from connected to disconnected and will power off unless
        // the connection returns (ADR-0078's INFO allowlist).
        tracing::info!(
            target: openpiso_core::forwarding::CLIENT_OFFLINE_SHUTDOWN_TARGET,
            grace_seconds,
            "Offline Shutdown Countdown armed: the admin server is unreachable"
        );
        if let Some(deadline_unix_ms) =
            offline_shutdown::wall_clock_deadline(Some(deadline), now, unix_time_millis())
        {
            if let Err(error) =
                crate::config_store::save_offline_shutdown_deadline(app, deadline_unix_ms)
            {
                warn!(target: "openpiso_client::config_store", "Could not persist Offline Shutdown deadline: {error}");
            }
        }
        // An offline transition replaces an in-flight server expiry grace:
        // while the server is unreachable, Offline Grace is the only applicable
        // deadline.
        *shared.shutdown_deadline.write().await = None;
        *shared.shutdown_issued_at.write().await = None;
    }
}

/// Pause the Offline Shutdown Countdown while the owner-unlocked settings panel
/// is open. The PIN prompt never calls this function.
pub async fn pause_offline_shutdown(shared: &SharedState) -> Result<(), String> {
    let _transition = shared.offline_shutdown_transition.lock().await;
    if !*shared.owner_settings_unlocked.read().await {
        return Err("owner settings are not unlocked".to_owned());
    }
    *shared.offline_shutdown_owner_unlocked.write().await = true;
    let deadline = *shared.offline_shutdown_deadline.read().await;
    let mut paused_at = shared.offline_shutdown_paused_at.write().await;
    if offline_shutdown::should_pause(deadline, *paused_at) {
        *paused_at = Some(Instant::now());
    }
    Ok(())
}

/// Resume a paused Offline Shutdown Countdown, extending both the monotonic
/// deadline and its persisted wall-clock representation by the pause duration.
pub async fn resume_offline_shutdown(shared: &SharedState, app: &AppHandle) {
    let _transition = shared.offline_shutdown_transition.lock().await;
    let resumed_at = Instant::now();
    *shared.owner_settings_unlocked.write().await = false;
    *shared.offline_shutdown_owner_unlocked.write().await = false;
    let paused_at = shared.offline_shutdown_paused_at.write().await.take();
    let mut deadline = shared.offline_shutdown_deadline.write().await;
    let next_deadline = offline_shutdown::extend_for_pause(*deadline, paused_at, resumed_at);
    *deadline = next_deadline;
    if let Some(deadline_unix_ms) =
        offline_shutdown::wall_clock_deadline(next_deadline, resumed_at, unix_time_millis())
    {
        if let Err(error) =
            crate::config_store::save_offline_shutdown_deadline(app, deadline_unix_ms)
        {
            warn!(target: "openpiso_client::config_store", "Could not persist resumed Offline Shutdown deadline: {error}");
        }
    }
    drop(deadline);

    if let Some(deadline) = next_deadline {
        let remaining = shutdown::grace_remaining(deadline, resumed_at);
        let mut timer = shared.timer.write().await;
        if timer.offline_shutdown_in_seconds != Some(remaining) {
            timer.offline_shutdown_in_seconds = Some(remaining);
            let snapshot = timer.clone();
            drop(timer);
            emit_timer_updated(app, snapshot);
        }
    }
}

/// Drive the Offline Shutdown Countdown each local tick and, once its deadline
/// elapses while the Client remains disconnected, reuse the Rust-owned OS
/// shutdown path exactly once.
async fn process_offline_shutdown(shared: &SharedState, app: &AppHandle) {
    let deadline = *shared.offline_shutdown_deadline.read().await;
    let Some(deadline) = deadline else {
        let mut timer = shared.timer.write().await;
        if timer.offline_shutdown_in_seconds.is_some() {
            timer.offline_shutdown_in_seconds = None;
            let snapshot = timer.clone();
            drop(timer);
            emit_timer_updated(app, snapshot);
        }
        return;
    };

    let connection_status = shared.timer.read().await.connection_status.clone();
    if !matches!(connection_status.as_str(), "disconnected" | "connecting") {
        return;
    }
    if shared.offline_shutdown_paused_at.read().await.is_some() {
        return;
    }

    let now = Instant::now();
    let remaining = shutdown::grace_remaining(deadline, now);
    {
        let mut timer = shared.timer.write().await;
        if timer.offline_shutdown_in_seconds != Some(remaining) {
            timer.offline_shutdown_in_seconds = Some(remaining);
            let snapshot = timer.clone();
            drop(timer);
            emit_timer_updated(app, snapshot);
        }
    }

    // Re-read authority immediately before firing. A reconnect can race the
    // display update above; serialize with the connection transition so a
    // response being applied cannot leave a stale disconnected snapshot here.
    let _connection_transition = shared.connection_transition.lock().await;
    let _offline_transition = shared.offline_shutdown_transition.lock().await;
    let current_connection_status = shared.timer.read().await.connection_status.clone();
    // The paused guard is acquired first after the connection transition,
    // matching arm/cancel/resume lock order for the offline state.
    let paused_at = shared.offline_shutdown_paused_at.read().await;
    let deadline_guard = shared.offline_shutdown_deadline.write().await;
    let issued_at = *shared.offline_shutdown_issued_at.read().await;
    if offline_shutdown::should_execute(
        &current_connection_status,
        *deadline_guard,
        issued_at,
        Instant::now(),
    ) && paused_at.is_none()
    {
        *shared.offline_shutdown_issued_at.write().await = Some(Instant::now());
        drop(deadline_guard);
        drop(paused_at);
        shutdown::initiate_exit(shutdown::ClientExitReason::OfflineShutdown, shared, app).await;
    }
}

/// Drive the shutdown grace countdown each local tick: publish the remaining
/// grace seconds for the overlay and, once the deadline elapses while the server
/// still commands shutdown, perform the real OS power-off exactly once.
async fn process_shutdown_grace(shared: &SharedState, app: &AppHandle) {
    let deadline = *shared.shutdown_deadline.read().await;
    let Some(deadline) = deadline else {
        let mut timer = shared.timer.write().await;
        if timer.shutdown_in_seconds.is_some() {
            timer.shutdown_in_seconds = None;
            let snapshot = timer.clone();
            drop(timer);
            emit_timer_updated(app, snapshot);
        }
        return;
    };

    let now = Instant::now();
    let remaining = shutdown::grace_remaining(deadline, now);
    {
        let mut timer = shared.timer.write().await;
        if timer.shutdown_in_seconds != Some(remaining) {
            timer.shutdown_in_seconds = Some(remaining);
            let snapshot = timer.clone();
            drop(timer);
            emit_timer_updated(app, snapshot);
        }
    }

    // Re-read the server command under the same transition lock used by
    // heartbeat/SSE application. A top-up that cancels the deadline must win
    // over this tick, never an obsolete local read.
    let should_initiate = {
        let _transition = shared.connection_transition.lock().await;
        let current_deadline = *shared.shutdown_deadline.read().await;
        let power_state = shared.timer.read().await.power_state.clone();
        let issued_at = *shared.shutdown_issued_at.read().await;
        if shutdown::should_execute(&power_state, current_deadline, issued_at, now) {
            *shared.shutdown_issued_at.write().await = Some(now);
            true
        } else {
            false
        }
    };
    if should_initiate {
        shutdown::initiate_exit(shutdown::ClientExitReason::ExpiryPowerOff, shared, app).await;
    }
}

async fn run_local_countdown(shared: Arc<SharedState>, app: AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    loop {
        interval.tick().await;
        process_shutdown_grace(&shared, &app).await;
        process_offline_shutdown(&shared, &app).await;
        tick_local_countdown(&shared, &app).await;
    }
}

/// One projection tick of the local countdown: re-derive `remaining_seconds`
/// from the anchor the last heartbeat left behind. Split out of the loop so
/// the heartbeat/tick interleaving is testable.
async fn tick_local_countdown(shared: &SharedState, app: &AppHandle) {
    if let Some(snapshot) = project_local_countdown(shared).await {
        emit_timer_updated(app, snapshot);
    }
}

/// The app-free half of a countdown tick. Returns the timer snapshot to emit
/// when the projection actually moved `remaining_seconds`, `None` otherwise.
pub(crate) async fn project_local_countdown(shared: &SharedState) -> Option<TimerState> {
    let anchor = (*shared.countdown_anchor.read().await)?;
    let mut timer = shared.timer.write().await;
    if timer.pc_status != "in_use" || !(timer.connected || timer.timer_mode == "account") {
        return None;
    }
    let remaining_seconds = projected_remaining(anchor, std::time::Instant::now());
    if remaining_seconds == timer.remaining_seconds {
        return None;
    }
    timer.remaining_seconds = remaining_seconds;
    timer.expired = remaining_seconds == 0;
    Some(timer.clone())
}

/// Dev-only: seed the Fullscreen Lock's shutdown grace countdown so
/// `bun run dev:client` shows it by default, without driving a real session
/// to zero against a live admin. `scripts/dev.ts` sets
/// `OPENPISO_CLIENT_DEV_SHUTDOWN`; this is compiled only in debug builds and
/// never reaches a release binary. We set `power_state` and arm the grace
/// `deadline`; the connection task's `run_local_countdown` (started at launch
/// by the lib.rs auto-connect) ticks it down. If a real admin is connected,
/// its next heartbeat overwrites `power_state` and `manage_shutdown_deadline`
/// cancels the seed — authority still wins.
#[cfg(debug_assertions)]
pub async fn seed_dev_shutdown_countdown(shared: &Arc<SharedState>) {
    let grace_seconds = dev_shutdown_grace_seconds(shared).await;
    {
        let mut timer = shared.timer.write().await;
        timer.power_state = "shutdown".to_owned();
        timer.shutdown_grace_seconds = grace_seconds;
        timer.shutdown_in_seconds = Some(grace_seconds);
    }
    *shared.shutdown_deadline.write().await =
        Some(Instant::now() + Duration::from_secs(grace_seconds as u64));
    *shared.shutdown_grace_seconds.write().await = grace_seconds;
    tracing::info!(
        target: "openpiso_client::shutdown",
        grace_seconds,
        "OPENPISO_CLIENT_DEV_SHUTDOWN set: seeding the shutdown grace countdown (dev build only)"
    );
}

#[cfg(debug_assertions)]
async fn dev_shutdown_grace_seconds(shared: &Arc<SharedState>) -> i64 {
    let server_url = shared.config.read().await.server_url.clone();
    let endpoint = format!("{}/api/shop", server_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build();
    let Ok(client) = client else {
        return shutdown::DEFAULT_GRACE_SECONDS;
    };
    match client.get(&endpoint).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<ShopNameResponse>().await {
                Ok(payload) => payload.shutdown_grace_seconds.max(1),
                Err(error) => {
                    warn!(
                        target: "openpiso_client::shutdown",
                        "Could not parse dev shutdown grace from /api/shop: {error}"
                    );
                    shutdown::DEFAULT_GRACE_SECONDS
                }
            }
        }
        Ok(response) => {
            warn!(
                target: "openpiso_client::shutdown",
                status = %response.status(),
                "Dev shutdown grace endpoint returned non-success; using default"
            );
            shutdown::DEFAULT_GRACE_SECONDS
        }
        Err(error) => {
            warn!(
                target: "openpiso_client::shutdown",
                "Could not fetch dev shutdown grace from admin server; using default: {error}"
            );
            shutdown::DEFAULT_GRACE_SECONDS
        }
    }
}

fn projected_remaining(anchor: CountdownAnchor, now: std::time::Instant) -> i64 {
    let elapsed_seconds = now
        .saturating_duration_since(anchor.synchronized_at)
        .as_secs()
        .min(i64::MAX as u64) as i64;
    anchor
        .remaining_seconds
        .saturating_sub(elapsed_seconds)
        .max(0)
}

#[cfg(debug_assertions)]
pub(crate) async fn hold_tamper_kill_connection(shared: &SharedState, app: &AppHandle) {
    let previously_connected = {
        let timer = shared.timer.read().await;
        timer.connected && timer.connection_status == "connected"
    };
    let grace_seconds = shared.config.read().await.offline_grace_seconds;
    // The debug/demo injection can happen during a live session. Treat its
    // failed acknowledgement like any other connected-to-disconnected edge;
    // a real Guard relaunch starts from Connecting and has no such edge.
    arm_offline_shutdown_if_needed(
        shared,
        app,
        previously_connected,
        "disconnected",
        grace_seconds,
    )
    .await;
    keep_tamper_kill_connection_pending(shared, app).await;
}

/// Local handoff ingestion loop (issue #367). Polls the sender inbox and
/// feeds newly arrived reports into the pending state and generation
/// barrier. Needs no network, so reports stay durable and retry through the
/// existing heartbeat path during an Admin outage; clearing still requires
/// the server acknowledgement.
async fn run_handoff_ingest(
    shared: Arc<SharedState>,
    app: AppHandle,
    config_dir: std::path::PathBuf,
    main_path: Option<std::path::PathBuf>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        if crate::startup_coordinator::ingest_due_reports(
            &shared,
            &config_dir,
            main_path.as_deref(),
        )
        .await
        .is_some()
        {
            keep_tamper_kill_connection_pending(&shared, &app).await;
        }
    }
}

async fn keep_tamper_kill_connection_pending(shared: &SharedState, app: &AppHandle) {
    let mut timer = shared.timer.write().await;
    let already_connecting = !timer.connected
        && timer.connection_status == "connecting"
        && timer.error.is_none()
        && timer.offline_shutdown_in_seconds.is_none()
        && timer.tamper_kill_pending;
    if already_connecting {
        return;
    }
    timer.connected = false;
    timer.connection_status = "connecting".to_owned();
    timer.error = None;
    timer.shutdown_in_seconds = None;
    timer.tamper_kill_pending = true;
    timer.active_username = None;
    timer.timer_mode = "guest".to_owned();
    timer.remaining_seconds = timer.guest_remaining_seconds.max(0);
    timer.expired = timer.guest_remaining_seconds.max(0) == 0;
    let snapshot = timer.clone();
    drop(timer);
    emit_timer_updated(app, snapshot);
}

async fn set_connection_error(
    shared: &SharedState,
    app: &AppHandle,
    error: String,
    request_started_at: Instant,
) {
    {
        let _transition = shared.connection_transition.lock().await;
        if shared
            .last_successful_connection
            .read()
            .await
            .is_some_and(|successful_at| successful_at > request_started_at)
        {
            return;
        }
        if shared.pending_tamper_kill.read().await.is_some() {
            let config = shared.config.read().await.clone();
            let previously_connected = {
                let timer = shared.timer.read().await;
                timer.connected && timer.connection_status == "connected"
            };
            // A report raised during a live session must not suppress the
            // existing Offline Shutdown safety net if its acknowledgement
            // request cannot reach the admin. Startup reports are still only
            // Connecting: they have no prior connected edge to arm from.
            arm_offline_shutdown_if_needed(
                shared,
                app,
                previously_connected,
                "disconnected",
                config.offline_grace_seconds,
            )
            .await;
            keep_tamper_kill_connection_pending(shared, app).await;
            return;
        }
        let config = shared.config.read().await.clone();
        let previously_connected = {
            let timer = shared.timer.read().await;
            timer.connected && timer.connection_status == "connected"
        };
        arm_offline_shutdown_if_needed(
            shared,
            app,
            previously_connected,
            "disconnected",
            config.offline_grace_seconds,
        )
        .await;
        // Keep the credential while offline so a successful reconnect can restore
        // the same server-owned session. The disconnected Lock prevents any local
        // use, and the next authoritative response clears stale auth if the server
        // restarted or the session was otherwise ended.
        let connection_loss_message = {
            let mut timer = shared.timer.write().await;
            let message = format!("Could not reach the admin server: {error}");
            let changed = timer.error.as_deref() != Some(message.as_str());
            timer.connected = false;
            timer.connection_status = "disconnected".to_owned();
            timer.error = Some(message);
            timer.active_username = None;
            timer.timer_mode = "guest".to_owned();
            if previously_connected {
                timer.offline_shutdown_in_seconds = Some(config.offline_grace_seconds.max(1));
            }
            // Fall back to the PC's own guest time so the Lock counts down
            // honestly instead of showing the (unreachable) account balance.
            timer.remaining_seconds = timer.guest_remaining_seconds;
            timer.expired = timer.guest_remaining_seconds == 0;
            let snapshot = timer.clone();
            drop(timer);
            emit_timer_updated(app, snapshot);
            if changed {
                Some(format!("Connection lost: {error}"))
            } else {
                None
            }
        };
        // Forward the loss only when the reported error actually changed:
        // every failed heartbeat tick while Admin is down would otherwise
        // fill the forwarding ring with identical lines and crowd out the
        // evidence it exists to keep. The old single-entry endpoint
        // rate-limited server-side instead (ADR-0078).
        if let Some(message) = connection_loss_message {
            crate::forwarding::record(
                openpiso_core::forwarding::ForwardedLevel::Error,
                "openpiso_client",
                message,
            );
        }
    };
}

/// Flip the timer to the `"unregistered"` state (ADR-0031 §2). The
/// server returned the typed `pc_unregistered` rejection: the client
/// reached the admin, the admin is reachable, but the server does not
/// know this machine. The Lock renders the "ask the owner" copy and
/// retries quietly on the next tick. No `error` is set — the admin is
/// healthy, it just has no row for us.
async fn set_unregistered(shared: &SharedState, app: &AppHandle, request_started_at: Instant) {
    let _transition = shared.connection_transition.lock().await;
    if shared
        .last_successful_connection
        .read()
        .await
        .is_some_and(|successful_at| successful_at > request_started_at)
    {
        return;
    }
    if shared.pending_tamper_kill.read().await.is_some() {
        keep_tamper_kill_connection_pending(shared, app).await;
        return;
    }
    cancel_offline_shutdown(shared, app, "unregistered").await;
    let mut timer = shared.timer.write().await;
    timer.connected = false;
    timer.connection_status = "unregistered".to_owned();
    timer.offline_shutdown_in_seconds = None;
    timer.error = None;
    let snapshot = timer.clone();
    drop(timer);
    emit_timer_updated(app, snapshot);
}

fn emit_timer_updated(app: &AppHandle, timer: TimerState) {
    if let Err(error) = app.emit(TIMER_UPDATED_EVENT, timer) {
        error!(target: "openpiso_client", "Could not emit {TIMER_UPDATED_EVENT} to the client UI: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use crate::state::{AccountAuthState, AccountSummary, PendingTamperKillReport};

    use super::*;

    fn account_auth(session: ActiveSession) -> AccountAuthState {
        AccountAuthState {
            token: "token".to_owned(),
            account: AccountSummary {
                id: session.account_id.clone(),
                username: session.username.clone(),
                remaining_seconds: session.remaining_seconds,
                points_balance_centi: session.points_balance_centi,
                status: "active".to_owned(),
            },
            session,
        }
    }

    fn active_session(id: &str, account_id: &str, remaining_seconds: i64) -> ActiveSession {
        ActiveSession {
            id: id.to_owned(),
            account_id: account_id.to_owned(),
            username: "customer".to_owned(),
            pc_id: "pc-01".to_owned(),
            remaining_seconds,
            points_balance_centi: 0,
            claimable_guest_seconds: 0,
        }
    }

    #[derive(Deserialize)]
    struct LegacyPcResponse {
        id: String,
        name: String,
        status: String,
        remaining_seconds: i64,
        guest_remaining_seconds: i64,
        timer_mode: String,
        active_session: Option<ActiveSession>,
    }

    #[test]
    fn mixed_version_admin_and_client_payloads_are_compatible() {
        let old_response = r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null}"#;
        let old_client: PcResponse =
            serde_json::from_str(old_response).expect("pre-Attendance Admin response should parse");
        assert_eq!(old_client.attendance_supported, None);

        let new_response = r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null,"attendance_supported":true}"#;
        let new_client: PcResponse =
            serde_json::from_str(new_response).expect("new Admin response should parse");
        assert_eq!(new_client.attendance_supported, Some(true));

        let legacy_client: LegacyPcResponse = serde_json::from_str(new_response)
            .expect("pre-Attendance Client should ignore the new field");
        assert_eq!(legacy_client.id, "pc-01");
        assert_eq!(legacy_client.name, "PC 01");
        assert_eq!(legacy_client.status, "in_use");
        assert_eq!(legacy_client.remaining_seconds, 300);
        assert_eq!(legacy_client.guest_remaining_seconds, 300);
        assert_eq!(legacy_client.timer_mode, "guest");
        assert!(legacy_client.active_session.is_none());
    }

    #[tokio::test]
    async fn missing_attendance_flag_preserves_cached_support() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null}"#,
        )
        .expect("pre-Attendance response should parse");

        let snapshot = apply_timer_and_auth(&shared, &pc).await;
        assert!(!snapshot.attendance_supported);

        shared.timer.write().await.attendance_supported = true;
        let snapshot = apply_timer_and_auth(&shared, &pc).await;
        assert!(snapshot.attendance_supported);
        assert!(shared.timer.read().await.attendance_supported);
    }

    #[test]
    fn parses_pc_updated_sse_event() {
        let event = br#"event: pc.updated
data: {"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null,"shop_status":{"open":true,"next_change_at":"2030-06-21T22:00:00+08:00","reason":"schedule","last_call_active":true}}
"#;

        let pc = parse_sse_event::<PcResponse>(event, "pc.updated").expect("PC event should parse");
        assert_eq!(pc.id, "pc-01");
        assert_eq!(pc.remaining_seconds, 300);
        assert_eq!(
            pc.shop_status.as_ref().map(|status| status.open),
            Some(true)
        );
        assert_eq!(
            pc.shop_status
                .as_ref()
                .map(|status| status.last_call_active),
            Some(true)
        );
        assert_eq!(
            pc.shop_status
                .as_ref()
                .and_then(|status| status.next_change_at.as_deref()),
            Some("2030-06-21T22:00:00+08:00")
        );
    }

    #[tokio::test]
    async fn heartbeat_without_attendance_support_defaults_to_false_in_timer_state() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null}"#,
        )
        .expect("legacy heartbeat should parse");

        assert_eq!(pc.attendance_supported, None);

        let snapshot = apply_timer_and_auth(&shared, &pc).await;
        assert!(!shared.timer.read().await.attendance_supported);
        assert!(!snapshot.attendance_supported);
        let json = serde_json::to_value(snapshot).expect("timer state should serialize");
        assert_eq!(json["attendance_supported"], false);
    }

    #[tokio::test]
    async fn heartbeat_attendance_support_reaches_timer_state_json() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null,"attendance_supported":true}"#,
        )
        .expect("heartbeat with attendance support should parse");

        assert_eq!(pc.attendance_supported, Some(true));

        let snapshot = apply_timer_and_auth(&shared, &pc).await;
        assert!(shared.timer.read().await.attendance_supported);
        assert!(snapshot.attendance_supported);
        let json = serde_json::to_value(snapshot).expect("timer state should serialize");
        assert_eq!(json["attendance_supported"], true);
    }

    #[test]
    fn parses_coin_accepted_sse_event_with_promo_split() {
        let event = br#"event: coin.accepted
data: {"pc_id":"pc-01","seconds_added":2100,"coin_promo_bonus_seconds":600}
"#;

        let feedback = parse_sse_event::<CoinFeedback>(event, "coin.accepted")
            .expect("coin event should parse");
        assert_eq!(feedback.pc_id, "pc-01");
        assert_eq!(feedback.seconds_added, 2_100);
        assert_eq!(feedback.coin_promo_bonus_seconds, 600);
    }

    #[test]
    fn ignores_keep_alive_comments() {
        assert!(parse_sse_event::<PcResponse>(b": keep-alive", "pc.updated").is_none());
    }

    #[test]
    fn report_age_is_derived_at_send_time_and_clamped_when_clock_moves_backwards() {
        use crate::startup_coordinator::report_age_millis;
        assert_eq!(report_age_millis(1_000, 4_500), 3_500);
        assert_eq!(report_age_millis(4_500, 1_000), 0);
    }

    #[test]
    fn tamper_fields_rederive_age_for_each_retry_without_persisting_it() {
        use crate::startup_coordinator::heartbeat_tamper_fields;
        let pending = PendingTamperKillReport {
            kill_timestamp_unix_ms: 1_000,
            attempt_count: 1,
        };
        let first_send = heartbeat_tamper_fields(Some(pending), 2_000);
        let retry = heartbeat_tamper_fields(Some(pending), 5_000);

        assert_eq!(first_send.0, retry.0);
        assert_eq!(first_send.1, Some(1_000));
        assert_eq!(retry.1, Some(4_000));
        assert!(serde_json::to_value(pending)
            .expect("pending report should serialize")
            .get("report_age_ms")
            .is_none());
    }

    #[test]
    fn heartbeat_request_omits_tamper_fields_without_a_pending_report() {
        let body = serde_json::to_value(HeartbeatRequest {
            hostname: "TEST-PC".to_owned(),
            client_version: "0.1.0",
            client_time_unix_ms: 1_750_000_000_000,
            guard_coverage: crate::guard_client::current_heartbeat_coverage(),
            tamper_kill_timestamp: None,
            tamper_kill_report_age_ms: None,
            tamper_kill_attempt_count: None,
        })
        .expect("heartbeat request should serialize");

        assert_eq!(
            body,
            serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "client_time_unix_ms": 1_750_000_000_000_i64,
                // Debug builds always report not applicable (ADR-0076).
                "guard_coverage": crate::guard_client::current_heartbeat_coverage(),
            })
        );
    }

    #[test]
    fn heartbeat_request_includes_report_age_for_a_pending_report() {
        let body = serde_json::to_value(HeartbeatRequest {
            hostname: "TEST-PC".to_owned(),
            client_version: "0.1.0",
            client_time_unix_ms: 1_750_000_000_000,
            guard_coverage: crate::guard_client::current_heartbeat_coverage(),
            tamper_kill_timestamp: Some("report-name".to_owned()),
            tamper_kill_report_age_ms: Some(0),
            tamper_kill_attempt_count: Some(2),
        })
        .expect("heartbeat request should serialize");

        assert_eq!(body["tamper_kill_report_age_ms"], 0);
    }

    #[test]
    fn heartbeat_response_reads_theme_version() {
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"guest_remaining_seconds":300,"timer_mode":"guest","active_session":null,"theme_version":7,"tamper_kill_acknowledged":"recorded"}"#,
        )
        .expect("heartbeat should parse");

        assert_eq!(pc.theme_version, 7);
        assert!(pc.tamper_kill_acknowledged == Some(TamperKillAck::Recorded));
    }

    #[test]
    fn heartbeat_response_degrades_unrecognized_tamper_kill_subject_to_unknown() {
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":0,"guest_remaining_seconds":0,"timer_mode":"guest","active_session":null,"tamper_kill_acknowledged":"recorded","tamper_kill_subject":"none"}"#,
        )
        .expect("heartbeat with a newer subject should parse");

        assert_eq!(pc.tamper_kill_subject, Some(TamperKillSubject::Unknown));
    }

    #[tokio::test]
    async fn pending_tamper_kill_keeps_sse_behind_the_heartbeat_barrier() {
        let shared = Arc::new(SharedState::default());
        *shared.pending_tamper_kill.write().await = Some(PendingTamperKillReport {
            kill_timestamp_unix_ms: 1_750_000_000_000,
            attempt_count: 1,
        });
        let waiter = {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move { wait_for_tamper_kill_acknowledgement(&shared).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        *shared.pending_tamper_kill.write().await = None;
        shared.tamper_kill_acknowledged.notify_waiters();
        waiter
            .await
            .expect("SSE barrier should release after acknowledgement");
        assert!(!should_defer_event_stream_snapshot(None, 1, 1));
        assert!(should_defer_event_stream_snapshot(None, 1, 2));
    }

    #[tokio::test]
    async fn acknowledged_guest_tamper_kill_locks_preserved_guest_time() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":180,"guest_remaining_seconds":180,"timer_mode":"guest","active_session":null,"power_state":"active","tamper_kill_acknowledged":"recorded","tamper_kill_subject":"guest"}"#,
        )
        .expect("heartbeat body parses");

        let snapshot = apply_timer_and_auth(&shared, &pc).await;

        assert_eq!(snapshot.remaining_seconds, 180);
        assert!(snapshot.overlay_locked);
        assert_eq!(shared.timer.read().await.guest_remaining_seconds, 180);
    }

    #[tokio::test]
    async fn account_sign_in_clears_the_guest_kick_lock_after_server_confirmation() {
        let shared = SharedState::default();
        let session = active_session("session-1", "account-1", 180);
        *shared.timer.write().await = TimerState {
            overlay_locked: true,
            ..TimerState::default()
        };
        *shared.account_auth.write().await = Some(account_auth(session.clone()));
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":180,"guest_remaining_seconds":0,"timer_mode":"account","active_session":{"id":"session-1","account_id":"account-1","username":"customer","pc_id":"pc-01","remaining_seconds":180,"points_balance_centi":0,"claimable_guest_seconds":0},"power_state":"active"}"#,
        )
        .expect("heartbeat body parses");

        let snapshot = apply_timer_and_auth(&shared, &pc).await;

        assert!(!snapshot.overlay_locked);
        assert_eq!(snapshot.timer_mode, "account");
    }

    #[tokio::test]
    async fn acknowledged_account_tamper_kill_does_not_claim_guest_continuation() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":180,"guest_remaining_seconds":180,"timer_mode":"guest","active_session":null,"power_state":"active","tamper_kill_acknowledged":"recorded","tamper_kill_subject":"account"}"#,
        )
        .expect("heartbeat body parses");

        let snapshot = apply_timer_and_auth(&shared, &pc).await;

        assert!(!snapshot.overlay_locked);
    }

    #[tokio::test]
    async fn acknowledged_tamper_kill_with_no_guest_time_keeps_shutdown_authority() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":0,"guest_remaining_seconds":0,"timer_mode":"guest","active_session":null,"power_state":"shutdown","shutdown_grace_seconds":30,"tamper_kill_acknowledged":"recorded"}"#,
        )
        .expect("heartbeat body parses");

        let snapshot = apply_timer_and_auth(&shared, &pc).await;

        assert_eq!(snapshot.power_state, "shutdown");
        assert!(!snapshot.overlay_locked);
    }

    #[test]
    fn theme_record_requires_known_modes_and_round_trips_full_record() {
        let raw = r##"{"accent_color":"#16a34a","mode_web_admin":"light","mode_kiosk":"dark","mode_client":"dark","version":7}"##;
        let theme: ThemeRecord = serde_json::from_str(raw).expect("theme should parse");

        assert_eq!(theme.mode_client, crate::state::ThemeMode::Dark);
        assert_eq!(
            serde_json::to_string(&theme).expect("theme should serialize"),
            raw
        );
        assert!(serde_json::from_str::<ThemeRecord>(&raw.replace("dark", "system")).is_err());
    }

    #[test]
    fn projects_countdown_from_server_anchor() {
        let synchronized_at = Instant::now();
        let anchor = CountdownAnchor {
            remaining_seconds: 300,
            synchronized_at,
        };

        assert_eq!(
            projected_remaining(anchor, synchronized_at + Duration::from_millis(999)),
            300
        );
        assert_eq!(
            projected_remaining(anchor, synchronized_at + Duration::from_secs(1)),
            299
        );
        assert_eq!(
            projected_remaining(anchor, synchronized_at + Duration::from_secs(301)),
            0
        );
    }

    #[test]
    fn authoritative_time_with_matching_auth_uses_session_time() {
        assert_eq!(authoritative_remaining_seconds(true, true, 300, 0), 300);
        assert_eq!(authoritative_remaining_seconds(true, false, 300, 0), 300);
        assert_eq!(authoritative_remaining_seconds(true, true, 0, 120), 0);
    }

    #[test]
    fn authoritative_time_without_auth_but_stale_session_uses_guest_time() {
        assert_eq!(authoritative_remaining_seconds(false, true, 300, 0), 0);
        assert_eq!(authoritative_remaining_seconds(false, true, 300, 60), 60);
        assert_eq!(authoritative_remaining_seconds(false, true, 0, 0), 0);
    }

    #[test]
    fn authoritative_time_without_session_uses_reported_time() {
        assert_eq!(authoritative_remaining_seconds(false, false, 300, 0), 300);
        assert_eq!(authoritative_remaining_seconds(false, false, 120, 120), 120);
        assert_eq!(authoritative_remaining_seconds(false, false, 0, 0), 0);
    }

    /// A heartbeat that carries a *stale* account session — the server still
    /// has a session bound to this PC, but this Client holds no credentials
    /// for it (a relaunched Client after a Tamper Kill that was never kicked)
    /// — must not leave a countdown anchor that re-exposes the session's
    /// time. The frontend derives Lock vs Widget purely from
    /// `remaining_seconds`, so an anchor that disagrees with the timer flips
    /// the view back and forth for as long as the stale binding lives.
    #[tokio::test]
    async fn stale_session_heartbeat_leaves_no_countdown_that_re_exposes_session_time() {
        let shared = SharedState::default();
        let pc: PcResponse = serde_json::from_str(
            r#"{"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,
                "guest_remaining_seconds":0,"timer_mode":"account",
                "active_session":{"id":"session-1","account_id":"account-1",
                  "username":"customer","pc_id":"pc-01","remaining_seconds":300,
                  "points_balance_centi":0,"claimable_guest_seconds":0}}"#,
        )
        .expect("heartbeat body parses");

        apply_timer_and_auth(&shared, &pc).await;
        assert_eq!(
            shared.timer.read().await.remaining_seconds,
            0,
            "stale session time must not be exposed as usable time"
        );

        project_local_countdown(&shared).await;
        assert_eq!(
            shared.timer.read().await.remaining_seconds,
            0,
            "local countdown re-exposed the stale session's time: the view flips Lock -> Widget"
        );
    }

    #[test]
    fn zero_time_auth_survives_no_session_snapshot() {
        let auth = account_auth(active_session("session-1", "account-1", 0));

        assert!(!should_clear_account_auth_for_server_state(&auth, None,));
    }

    #[test]
    fn positive_time_auth_without_server_session_clears() {
        let auth = account_auth(active_session("session-1", "account-1", 300));

        assert!(should_clear_account_auth_for_server_state(&auth, None,));
    }

    #[test]
    fn mismatched_active_session_clears_auth_immediately() {
        let auth = account_auth(active_session("session-1", "account-1", 0));
        let other = active_session("session-2", "account-2", 300);

        assert!(should_clear_account_auth_for_server_state(
            &auth,
            Some(&other),
        ));
    }

    #[test]
    fn reconnect_reset_clears_owner_pause_before_later_arm() {
        tokio::runtime::Runtime::new()
            .expect("create test runtime")
            .block_on(async {
                let shared = SharedState::default();
                *shared.owner_settings_unlocked.write().await = true;
                *shared.offline_shutdown_owner_unlocked.write().await = true;
                *shared.offline_shutdown_paused_at.write().await = Some(Instant::now());

                reset_owner_settings_state(&shared).await;

                assert!(!*shared.owner_settings_unlocked.read().await);
                assert!(!*shared.offline_shutdown_owner_unlocked.read().await);
                assert!(shared.offline_shutdown_paused_at.read().await.is_none());
                let later_arm_at = Instant::now();
                let later_deadline =
                    offline_shutdown::arm_deadline(true, "disconnected", later_arm_at, 120)
                        .expect("later disconnect should arm");
                assert!(later_deadline > later_arm_at);
                assert_eq!(
                    offline_shutdown::pause_at_arm(
                        *shared.offline_shutdown_owner_unlocked.read().await,
                        later_arm_at,
                    ),
                    None
                );
            });
    }

    /// A heartbeat lands every 5s; it must not revoke an owner PIN unlock, or
    /// `set_autostart` / `set_windows_sign_out` start failing with "owner
    /// settings are not unlocked" while the panel is still open.
    #[test]
    fn connected_heartbeat_keeps_an_open_owner_unlock() {
        tokio::runtime::Runtime::new()
            .expect("create test runtime")
            .block_on(async {
                let shared = SharedState::default();
                *shared.owner_settings_unlocked.write().await = true;
                *shared.offline_shutdown_owner_unlocked.write().await = true;
                *shared.offline_shutdown_paused_at.write().await = Some(Instant::now());
                *shared.offline_shutdown_deadline.write().await = Some(Instant::now());

                cancel_offline_shutdown_state(&shared).await;

                assert!(*shared.owner_settings_unlocked.read().await);
                assert!(*shared.offline_shutdown_owner_unlocked.read().await);
                assert!(shared.offline_shutdown_deadline.read().await.is_none());
                assert!(shared.offline_shutdown_paused_at.read().await.is_none());
            });
    }

    #[test]
    fn parses_shop_name_response() {
        let raw = br#"{"shop_name": "Juan's Pisonet"}"#;
        let parsed: ShopNameResponse =
            serde_json::from_slice(raw).expect("shop name response should parse");
        assert_eq!(parsed.shop_name, "Juan's Pisonet");
        assert_eq!(
            parsed.shutdown_grace_seconds,
            shutdown::DEFAULT_GRACE_SECONDS
        );
        assert_eq!(parsed.offline_grace_seconds, 600);
    }
}

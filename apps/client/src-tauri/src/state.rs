use std::sync::{atomic::AtomicU64, Arc};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, Notify, RwLock},
    task::JoinHandle,
};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClientConfig {
    pub server_url: String,
    /// Server-assigned identity that the client caches (ADR-0031).
    /// `None` on a fresh install: the server resolves and adopts a
    /// `pc_id` on the first heartbeat, and the client persists it from
    /// then on. The hard-coded `pc-01` default is gone — the only
    /// way the client learns its `pc_id` is from the server.
    #[serde(default, deserialize_with = "deserialize_pc_id")]
    pub pc_id: Option<String>,
    /// Customer's persisted choice between the floating Widget and the Mini
    /// Bar while time is active (ADR-0029). Defaults to `"widget"` for a fresh
    /// install. The Fullscreen Lock is not a stored `view_mode` — it is
    /// always the source of truth whenever the §4.1 return-to-Lock triggers
    /// fire. The valid set is `"widget" | "mini"`; the type does not enforce
    /// it because the value crosses the Tauri command boundary as a string.
    #[serde(
        default = "default_view_mode",
        deserialize_with = "deserialize_view_mode"
    )]
    pub view_mode: String,
    /// Whether the Client should launch when this Windows account signs in.
    /// Defaults on so a fresh install is available after every customer sign-in.
    #[serde(default = "default_launch_at_startup")]
    pub launch_at_startup: bool,
    /// Whether the owner enabled Windows sign-out protection. The registry
    /// policy itself is applied by the release-Windows Rust adapter.
    #[serde(default)]
    pub disable_windows_sign_out: bool,
    /// Last-seen owner-configured Offline Grace duration. Kept in the local
    /// config so the offline shutdown path can use it while the server is
    /// unreachable (issue #115).
    #[serde(default = "default_offline_grace_seconds")]
    pub offline_grace_seconds: i64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct PendingTamperKillReport {
    pub kill_timestamp_unix_ms: i64,
    pub attempt_count: i64,
}

impl PendingTamperKillReport {
    /// Merge a newer Guard report into the pending one. The oldest kill
    /// timestamp is kept as the stable report name while the attempt count
    /// coalesces across relaunches.
    ///
    /// Merging a report with itself is a no-op: repeated local delivery of
    /// one report must not inflate the attempt count (issue #367). Delivery
    /// identity is equality of the report; Tamper Kill coalescing applies
    /// only to genuinely distinct reports.
    pub fn coalesce(self, newer: Self) -> Self {
        if self == newer {
            return self;
        }
        Self {
            kill_timestamp_unix_ms: self.kill_timestamp_unix_ms,
            attempt_count: self
                .attempt_count
                .saturating_add(1)
                .max(newer.attempt_count),
        }
    }
}

pub(crate) fn default_view_mode() -> String {
    "widget".to_owned()
}

pub(crate) fn default_offline_grace_seconds() -> i64 {
    600
}

pub(crate) fn default_launch_at_startup() -> bool {
    true
}

fn deserialize_view_mode<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Ok(normalize_view_mode(&value))
}

pub(crate) fn normalize_view_mode(value: &str) -> String {
    if value.eq_ignore_ascii_case("mini") {
        "mini".to_owned()
    } else {
        default_view_mode()
    }
}

/// Deserialize the `pc_id` field as `Option<String>`. Empty / missing
/// / whitespace-only values become `None` so a stale `pc-01` written
/// before ADR-0031 does not get a misleading cached identity.
fn deserialize_pc_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()))
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:3210".to_owned(),
            pc_id: None,
            view_mode: default_view_mode(),
            launch_at_startup: default_launch_at_startup(),
            disable_windows_sign_out: false,
            offline_grace_seconds: default_offline_grace_seconds(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ThemeRecord {
    pub accent_color: String,
    pub mode_web_admin: ThemeMode,
    pub mode_kiosk: ThemeMode,
    pub mode_client: ThemeMode,
    pub version: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ShopStatus {
    pub open: bool,
    pub next_change_at: Option<String>,
    pub reason: String,
    pub last_call_active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimerState {
    pub pc_id: Option<String>,
    pub pc_name: String,
    pub pc_status: String,
    pub remaining_seconds: i64,
    pub guest_remaining_seconds: i64,
    pub timer_mode: String,
    /// Whether the connected Admin supports Attendance.
    pub attendance_supported: bool,
    pub active_username: Option<String>,
    pub connected: bool,
    /// One of `disconnected`, `connecting`, `connected`, `unregistered`
    /// (ADR-0031 §2). The Lock renders a different copy for each
    /// state: "Registering this PC…", "ask the owner to register
    /// this PC", and the existing "Can't reach the server" — so a
    /// machine stuck waiting for its first bind is distinguishable
    /// from one the server explicitly rejected.
    pub connection_status: String,
    pub expired: bool,
    pub overlay_locked: bool,
    /// A durable Tamper Kill report is awaiting the authoritative heartbeat.
    /// The Lock uses this only to keep the pre-authority UI in Connecting;
    /// it never invents a shutdown command.
    pub tamper_kill_pending: bool,
    /// Server-commanded expiry action: "active", "warning", or "shutdown"
    /// (ADR-0022). The UI renders the low-time warning and shutdown grace
    /// overlay from this field.
    pub power_state: String,
    /// Seconds left in the shutdown grace countdown while `power_state` is
    /// "shutdown"; `None` otherwise.
    pub shutdown_in_seconds: Option<i64>,
    /// Seconds left in the Offline Shutdown Countdown after a connected Client
    /// loses the admin server; `None` unless that transition armed it.
    pub offline_shutdown_in_seconds: Option<i64>,
    /// Owner-configured full grace window for the current shutdown countdown.
    pub shutdown_grace_seconds: i64,
    /// Server-resolved Shop Status. The Client renders this value and never
    /// evaluates Shop Hours schedule semantics locally.
    pub shop_status: Option<ShopStatus>,
    pub error: Option<String>,
}

impl Default for TimerState {
    fn default() -> Self {
        Self {
            pc_id: None,
            pc_name: String::new(),
            pc_status: "offline".to_owned(),
            remaining_seconds: 0,
            guest_remaining_seconds: 0,
            timer_mode: "guest".to_owned(),
            attendance_supported: false,
            active_username: None,
            connected: false,
            connection_status: "disconnected".to_owned(),
            expired: false,
            overlay_locked: false,
            tamper_kill_pending: false,
            power_state: "active".to_owned(),
            shutdown_in_seconds: None,
            offline_shutdown_in_seconds: None,
            shutdown_grace_seconds: crate::shutdown::DEFAULT_GRACE_SECONDS,
            shop_status: None,
            error: None,
        }
    }
}

pub struct SharedState {
    pub config: RwLock<ClientConfig>,
    pub timer: RwLock<TimerState>,
    pub countdown_anchor: RwLock<Option<CountdownAnchor>>,
    pub connection_task: Mutex<Option<JoinHandle<()>>>,
    /// Serializes connection-state transitions so an older transport failure
    /// cannot overwrite a newer successful heartbeat/SSE response.
    pub connection_transition: Mutex<()>,
    pub last_successful_connection: RwLock<Option<Instant>>,
    pub account_auth: RwLock<Option<AccountAuthState>>,
    /// Instant at which the shutdown grace period elapses while the server
    /// commands a power-off; `None` when no shutdown is pending (ADR-0022).
    pub shutdown_deadline: RwLock<Option<Instant>>,
    /// When the OS shutdown was last issued, so it fires once per attempt and
    /// is issued again only if the machine is still running a retry window
    /// later (`shutdown::SHUTDOWN_RETRY_SECONDS`). `None` means never issued.
    pub shutdown_issued_at: RwLock<Option<Instant>>,
    /// Monotonic deadline for the Offline Shutdown Countdown. Its matching
    /// absolute wall-clock deadline lives in config-store so a relaunch can
    /// resume it (issue #118).
    pub offline_shutdown_deadline: RwLock<Option<Instant>>,
    /// Serializes offline deadline arm/cancel/pause/resume transitions.
    pub offline_shutdown_transition: Mutex<()>,
    /// Whether the Rust-side owner PIN verification has unlocked settings.
    /// Offline pause commands require this gate; the frontend clears it when
    /// the panel closes.
    pub owner_settings_unlocked: RwLock<bool>,
    /// Whether the owner-PIN-unlocked settings panel is open. This is kept
    /// separately from the pause start so a disconnect while the panel is
    /// already open starts paused immediately.
    pub offline_shutdown_owner_unlocked: RwLock<bool>,
    /// Start of the owner-unlocked settings pause, if any. The pause is
    /// deliberately in memory; resuming rewrites the extended deadline.
    pub offline_shutdown_paused_at: RwLock<Option<Instant>>,
    /// When the offline OS shutdown was last issued. Same one-shot-per-attempt
    /// rule as `shutdown_issued_at`.
    pub offline_shutdown_issued_at: RwLock<Option<Instant>>,
    /// Owner-configured shutdown grace period, refreshed from the admin server.
    pub shutdown_grace_seconds: RwLock<i64>,
    /// Owner-editable shop display name, fetched on connect/reconnect from
    /// the public `GET /api/shop` endpoint (ADR-0027 §4.4) and cached here so
    /// the Lock header renders before the next refresh. The default mirrors
    /// the admin's seeded value so a fresh client never shows empty.
    pub shop_name: RwLock<String>,
    /// Last record fetched by the heartbeat-driven theme task. Exposed only
    /// through a typed invoke so listener registration and startup delivery
    /// cannot race.
    pub theme: RwLock<Option<ThemeRecord>>,
    /// Cached strong (Argon2) owner-PIN hash for the **offline fallback**
    /// (ADR-0027 §3). Populated only after a *successful* online verify (the
    /// server returns the hash only to a caller that proved the PIN), held in
    /// memory only (never persisted), and consulted when the admin server is
    /// unreachable so the owner can still unlock settings / Quit. Cleared when
    /// the client is repointed at a different admin server.
    pub owner_pin_hash: RwLock<Option<String>>,
    /// Loaded from `tamper-kill-report.json` on startup; included in the next
    /// heartbeat; cleared only on server acknowledgement. Survives process death
    /// and server outage — retried until acknowledged.
    pub pending_tamper_kill: RwLock<Option<PendingTamperKillReport>>,
    /// Wakes the event stream after the heartbeat has applied and acknowledged
    /// the pending Tamper Kill report. SSE must not establish an authoritative
    /// state before that heartbeat, or it can replay the pre-kick account.
    pub tamper_kill_acknowledged: Notify,
    /// Rust-owned marker for the neutral guest Tamper Kick Lock. It prevents
    /// the generic overlay state from being cleared by an unrelated command.
    pub guest_tamper_kick_pending: RwLock<bool>,
    /// Changes whenever a new Tamper Kill barrier is introduced. An SSE
    /// connection captures this generation; a stream opened before a report
    /// appeared is discarded even if the report is acknowledged before its
    /// next buffered event is parsed.
    pub tamper_kill_generation: AtomicU64,
    /// Whether a persisted client config was loaded from disk at startup.
    /// `false` on a fresh install (no `client-config.json`) or when the file
    /// is unreadable/corrupt — the client is then running on the in-memory
    /// `ClientConfig::default()` (localhost) that no owner ever chose. The
    /// Lock reads this to surface the un-gated "set up this PC" reconnect
    /// form so a never-configured machine can be pointed at the real admin
    /// server without the offline owner-PIN deadlock (ADR-0027 §3). Flips to
    /// `true` the moment a config is successfully saved.
    pub configured: RwLock<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountSummary {
    pub id: String,
    pub username: String,
    pub remaining_seconds: i64,
    pub points_balance_centi: i64,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountAuthState {
    pub token: String,
    pub account: AccountSummary,
    pub session: ActiveSession,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActiveSession {
    pub id: String,
    pub account_id: String,
    pub username: String,
    pub pc_id: String,
    pub remaining_seconds: i64,
    pub points_balance_centi: i64,
    pub claimable_guest_seconds: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RedemptionTier {
    pub id: i64,
    pub points: i64,
    pub seconds: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RedemptionTierMenu {
    pub version: i64,
    pub tiers: Vec<RedemptionTier>,
}

/// Segments carry no probability weight (ADR-0047): every segment is `1/n`
/// likely and `1/n` of the circle. Serde's default of ignoring unknown fields
/// lets this client read an older admin's payload that still sends `weight`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinFeedback {
    pub pc_id: String,
    pub seconds_added: i64,
    pub coin_promo_bonus_seconds: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinPromoMenuTier {
    pub amount_cents: i64,
    pub bonus_seconds: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinPromoMenu {
    pub enabled: bool,
    pub tiers: Vec<CoinPromoMenuTier>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WheelSegmentRow {
    pub label: String,
    pub seconds: i64,
    pub points_centi: i64,
    pub free_spin: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WheelSettings {
    pub enabled: bool,
    pub free_spin_cap: i64,
    pub spin_duration: i64,
    pub version: i64,
    pub segments: Vec<WheelSegmentRow>,
    pub expected_payout_seconds: i64,
    pub expected_payout_points_centi: i64,
    pub point_value_seconds: i64,
    pub house_edge_basis_points: i64,
    pub unsafe_house_edge: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WheelSpinStep {
    pub segment_index: usize,
    pub label: String,
    pub seconds: i64,
    pub points_centi: i64,
    pub free_spin: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WheelSpinResult {
    pub seconds_won: i64,
    pub points_won_centi: i64,
    pub free_spins_triggered: i64,
    pub segments: Vec<WheelSegmentRow>,
    pub steps: Vec<WheelSpinStep>,
}

/// Server response for a spin: the refreshed account binding plus the outcome
/// the server already chose, which the client animation only replays.
#[derive(Debug, Clone, Deserialize)]
pub struct WheelSpinResponse {
    pub token: String,
    pub account: AccountSummary,
    pub session: ActiveSession,
    pub result: WheelSpinResult,
}

#[derive(Debug, Clone, Copy)]
pub struct CountdownAnchor {
    pub remaining_seconds: i64,
    pub synchronized_at: Instant,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            config: RwLock::new(ClientConfig::default()),
            timer: RwLock::new(TimerState::default()),
            countdown_anchor: RwLock::new(None),
            connection_task: Mutex::new(None),
            connection_transition: Mutex::new(()),
            last_successful_connection: RwLock::new(None),
            account_auth: RwLock::new(None),
            shutdown_deadline: RwLock::new(None),
            shutdown_issued_at: RwLock::new(None),
            offline_shutdown_deadline: RwLock::new(None),
            offline_shutdown_transition: Mutex::new(()),
            owner_settings_unlocked: RwLock::new(false),
            offline_shutdown_owner_unlocked: RwLock::new(false),
            offline_shutdown_paused_at: RwLock::new(None),
            offline_shutdown_issued_at: RwLock::new(None),
            shutdown_grace_seconds: RwLock::new(crate::shutdown::DEFAULT_GRACE_SECONDS),
            shop_name: RwLock::new("OpenPiso Shop".to_owned()),
            theme: RwLock::new(None),
            pending_tamper_kill: RwLock::new(None),
            tamper_kill_acknowledged: Notify::new(),
            guest_tamper_kick_pending: RwLock::new(false),
            tamper_kill_generation: AtomicU64::new(0),
            owner_pin_hash: RwLock::new(None),
            configured: RwLock::new(false),
        }
    }
}

pub struct AppState {
    pub shared: Arc<SharedState>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedState::default()),
        }
    }
}

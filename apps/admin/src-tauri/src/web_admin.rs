// M4 deliverable: the owner HTTP API on `:8080` (ADR-0024). This is the
// browser-facing analogue of the desktop console's Tauri commands. It runs as
// a SECOND Axum server, separate from the `:3210` client/device API in
// `server.rs`; owner endpoints are NEVER mounted on `:3210`.
//
// Auth is a PIN (ADR-0024) verified against the existing `owner_credentials`
// row. Login issues an Owner Session into the `OwnerTokens` store, carried in
// a hardened cookie with a session-bound CSRF token (ADR-0074). Every owner
// route is gated by the `OwnerSession` extractor; mutations also need the
// exact same-origin `Origin` and the CSRF header.
//
// Every handler reuses the exact same `owner`/`db`/`reports`/`accounts`/
// `audit`/`diagnostics` calls the Tauri commands use, so authority (money,
// time, points, routing) stays in Rust and there is no second implementation
// of any mutation.

use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::io::AsyncWriteExt;

use axum::{
    body::{to_bytes, Body},
    extract::{
        rejection::JsonRejection, ConnectInfo, DefaultBodyLimit, FromRef, FromRequest,
        FromRequestParts, Multipart, Path, Query, State,
    },
    http::{header, request::Parts, HeaderMap, HeaderName, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    account_import::{self, AccountImportReport},
    account_login_throttle::AccountLoginThrottle,
    accounts::{self, AccountCreator},
    app_update::UpdateState,
    audit::{self, NewAuditEvent},
    auth::{AccountTokens, OwnerGrant, OwnerTokens, OWNER_ABSOLUTE_LIMIT},
    autostart::AutostartPort,
    backup::{self, BackupCatalogEntry, BackupSet, PendingRestore, RestoreSource},
    coin_source::ConfiguredCoinSource,
    db,
    diagnostics::{self, DiagnosticEntry, DiagnosticFilterInput},
    errors::AppError,
    events::EventHub,
    guard_poll::GuardRegistry,
    login_throttle::{Decision, LoginThrottle},
    models::{
        AcceptorHealth, AccountDeletionResult, AccountReportInput, AdminAccountAdjustInput,
        AdminAccountAdjustPointsInput, AdminAccountDeleteInput, AdminAccountInput,
        AdminAccountMutationInput, AdminSettings, AdminSettingsInput, AuditFilterInput, AuditPage,
        BackupScheduleSettings, BackupScheduleSettingsInput, CalibrationStartInput,
        ClientRecoveryEscalationInput, CoinDeviceSettings, CoinDeviceSettingsInput,
        CoinDeviceTestResult, CoinPromoSettings, CoinPromoSettingsInput, CoinTransaction,
        CreateVoucherBatchInput, CreateVoucherInput, DashboardSnapshot, DiagnosticsModeInput,
        DiagnosticsModeStatus, EconomySettings, EconomySettingsInput, KioskConfig,
        ManualAdjustment, ManualAdjustmentInput, MutationAck, NotificationAcknowledgeInput,
        OwnerAccount, OwnerAuthResponse, OwnerPinInput, Pc, PcMonitoringEnabledInput,
        PcMonitoringStatus, PcMonitoringSuppression, PcMonitoringSuppressionCancelInput,
        PcMonitoringSuppressionCancelled, PcMonitoringSuppressionInput, PcRegisterInput, PcReport,
        PcReportInput, PcSettings, PcSettingsInput, PendingPointsTransfer, PointsTransferRequest,
        RedemptionTier, RedemptionTiersInput, ReportSummary, ReportSummaryInput,
        SalesReportOverview, ShopHoursSettingsInput, ShopHoursSettingsResponse, ShopOverrideInput,
        ShopRate, ShopRateInput, ShopStatus, ThemeSettings, ThemeSettingsInput, TodayReport,
        TransferCodeInput, Voucher, VoucherBatch, VoucherBatchDetail, VoucherBatchSummary,
        VoucherTemplate, VoucherTemplateInput, WheelSettings, WheelSettingsInput,
    },
    monitoring, owner, reports, serial_port_detection,
    sessions::{SessionStore, SignOutOutcome},
    telegram::{self, DetectedChat, SetupCandidate, TelegramStatus},
    terminal_selection::{CalibrationStatus, SelectionState, TerminalSelection},
};
use openpiso_core::audit::{AuditActor, AuditOutcome};
use openpiso_core::transfer::POINTS_TRANSFER_TTL_SECONDS;

/// Name of the owner session cookie (hardened, ADR-0074).
const OWNER_COOKIE: &str = "op_owner";
/// Header carrying the Owner Session's CSRF token on authenticated mutations.
const CSRF_HEADER: &str = "x-csrf-token";
const MAX_RESTORE_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const MAX_RESTORE_REQUEST_BYTES: usize = MAX_RESTORE_UPLOAD_BYTES + 64 * 1024;

type HttpResult<T> = Result<Json<T>, AppError>;

/// Shared state for the owner server. Holds the same handles `AppState` does
/// so handlers reuse identical authority paths.
#[derive(Clone)]
pub struct OwnerServerState {
    pub pool: SqlitePool,
    pub events: EventHub,
    pub owner_tokens: OwnerTokens,
    pub account_tokens: AccountTokens,
    pub sessions: SessionStore,
    pub autostart: Arc<dyn AutostartPort>,
    /// Anonymous public Release Feed (ADR-0086); tests inject an offline fake.
    pub release_feed: Arc<dyn crate::app_update::ReleaseFeed>,
    pub installer: Arc<dyn crate::app_update::Installer>,
    /// Check state shared with startup and six-hour automatic checks.
    pub app_update: Arc<crate::app_update::UpdateService>,
    pub log_dir: PathBuf,
    /// Primary filesystem directory for canonical SQL Backups.
    pub backups_dir: PathBuf,
    /// Boundary of this admin process run; used to initialize the Coin Debug
    /// Window and as the floor bundles never reach past.
    pub app_started_at: DateTime<Utc>,
    /// Start of the Coin Debug Window — the movable floor the Coin Debug Bundle
    /// covers. In-memory runtime state only (never persisted): initialized to
    /// `app_started_at`, advanced to "now" on a Coin Device save or a manual
    /// reset, and re-initialized to the process start on restart.
    pub debug_window_start: Arc<std::sync::RwLock<DateTime<Utc>>>,
    /// Simulator-backed Central Terminal selection (ADR-0023). The owner API's
    /// `coin/simulate` endpoint injects through the active selection — it can
    /// never supply a `pc_id`, mirroring the kiosk path.
    pub terminal_selection: Arc<TerminalSelection<ConfiguredCoinSource>>,
    /// Directory containing the web-admin static build output (Vite + Svelte).
    /// Served at `/admin` for the browser owner surface (ADR-0024 / M5).
    pub web_admin_dir: PathBuf,
    /// Per-IP brute-force throttle for the Web Admin PIN login. In-memory only
    /// (ephemeral by design); shared across requests.
    pub login_throttle: LoginThrottle,
    /// Shared with the client API so an owner Password Reset clears Account
    /// Login throttle state too (ADR-0061).
    pub account_login_throttle: AccountLoginThrottle,
    /// Host Reachability probe for PC Monitoring Check now (ADR-0076).
    /// Production wiring uses the Windows ICMP adapter; tests inject a
    /// deterministic fake through this seam.
    pub host_probe: Arc<dyn monitoring::HostProbe>,
    /// Telegram connect-and-protect service (ADR-0076 / #345). Production
    /// wiring uses the Bot API HTTPS transport and the Windows DPAPI token
    /// protector; tests inject fakes through the same seams.
    pub telegram: Arc<telegram::TelegramService>,
    /// Shared with the client API (`:3210`) so the Support Bundle Verdict can
    /// read the same Guard Poll facts (ADR-0077) the escalation uses.
    pub guards: GuardRegistry,
    /// Shared with the client API (`:3210`): the owner arm places the
    /// Diagnostic Request the Heartbeat delivers and the Client's slice
    /// answer consumes (ADR-0078).
    pub diagnostic_requests: crate::diagnostic_request::DiagnosticRequestRegistry,
    /// Admin Run Records (ADR-0082): the host-diagnostics endpoint and the
    /// Support Bundle read the same store, so the owner's screen and the
    /// bundle cannot disagree.
    pub run_records: Arc<crate::run_record::RunRecordStore>,
}

impl OwnerServerState {
    /// Admin's own health (ADR-0082): Run Records newest first plus the
    /// panic file's newest 8 KB. One collector serves the host-diagnostics
    /// endpoint and the Support Bundle's Admin Host section, so the owner's
    /// screen and the bundle cannot disagree.
    fn host_diagnostics(&self) -> crate::host_diagnostics::HostDiagnostics {
        crate::host_diagnostics::collect(&self.run_records, &self.log_dir)
    }

    /// Read the current Coin Debug Window start. A poisoned lock is recovered
    /// (the guarded value is a plain timestamp, always safe to read) rather than
    /// panicking on an authority path.
    fn current_debug_window_start(&self) -> DateTime<Utc> {
        *self
            .debug_window_start
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Advance the Coin Debug Window start to now and return it. Called on a
    /// Coin Device save and on a manual reset — the only two write paths. A
    /// poisoned lock is recovered so the advance always takes effect, keeping
    /// the returned value and a subsequent read in agreement.
    fn advance_debug_window(&self) -> DateTime<Utc> {
        let now = Utc::now();
        let mut start = self
            .debug_window_start
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *start = now;
        now
    }
}

impl FromRef<OwnerServerState> for OwnerTokens {
    fn from_ref(state: &OwnerServerState) -> Self {
        state.owner_tokens.clone()
    }
}

impl FromRef<OwnerServerState> for SqlitePool {
    fn from_ref(state: &OwnerServerState) -> Self {
        state.pool.clone()
    }
}

/// Owner-auth gate. Extracted on every protected route. A request without a
/// live Owner Session is rejected with 401; a mutation on a live session
/// without the exact same-origin `Origin` and its CSRF token is rejected with
/// 403 (ADR-0074). Safe reads need only the session and never refresh its
/// idle timer.
///
/// Only a denied *mutation* is audited (matching the desktop `require_owner`
/// behaviour). A denied read is not: the web admin polls read-only endpoints on
/// a timer, so auditing those turns any unauthenticated page left open — the
/// login screen included — into a steady `web_admin_access_denied` flood that
/// buries real denials. An unauthenticated read reveals nothing (the 401 is
/// returned before any handler runs), so there is nothing to audit.
pub struct OwnerSession {
    token: String,
}

/// Why an Owner Session check refused a request.
#[derive(Clone, Copy)]
enum OwnerDenial {
    /// No live session: missing, unknown, revoked, or expired cookie.
    Unauthenticated,
    /// Live session, but the mutation lacked the exact origin or CSRF token.
    Forbidden,
}

impl OwnerDenial {
    fn rejection(self) -> (StatusCode, String) {
        match self {
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "valid owner authentication is required".to_owned(),
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "same-origin request with a valid CSRF token is required".to_owned(),
            ),
        }
    }

    fn audit_summary(self) -> &'static str {
        match self {
            Self::Unauthenticated => "Web admin operation denied",
            Self::Forbidden => "Web admin mutation denied: origin or CSRF check failed",
        }
    }
}

/// Resolve the request's Owner Session and, for a mutation, enforce `Origin`
/// and CSRF. `Origin` must equal `http://<Host>` exactly: the Web Admin is
/// plain HTTP and single-origin, and forwarded headers are never trusted.
async fn authorize_owner(
    tokens: &OwnerTokens,
    method: &Method,
    headers: &HeaderMap,
) -> Result<OwnerSession, OwnerDenial> {
    let token = cookie_token(headers).ok_or(OwnerDenial::Unauthenticated)?;
    let csrf = tokens
        .csrf_for(&token)
        .await
        .ok_or(OwnerDenial::Unauthenticated)?;
    if !method.is_safe() {
        let same_origin = header_str(headers, header::ORIGIN)
            .zip(header_str(headers, header::HOST))
            .is_some_and(|(origin, host)| origin.eq_ignore_ascii_case(&format!("http://{host}")));
        let csrf_matches = header_str(headers, CSRF_HEADER).is_some_and(|sent| sent == csrf);
        if !(same_origin && csrf_matches) {
            return Err(OwnerDenial::Forbidden);
        }
    }
    Ok(OwnerSession { token })
}

fn header_str<K: header::AsHeaderName>(headers: &HeaderMap, name: K) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

async fn audit_owner_denial(pool: &SqlitePool, denial: OwnerDenial) {
    let _ = audit::append_standalone(
        pool,
        NewAuditEvent::simple(
            "web_admin_access_denied",
            AuditOutcome::Denied,
            AuditActor::Admin,
            denial.audit_summary(),
        ),
    )
    .await;
}

impl<S> FromRequestParts<S> for OwnerSession
where
    S: Send + Sync,
    OwnerTokens: FromRef<S>,
    SqlitePool: FromRef<S>,
{
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let tokens = OwnerTokens::from_ref(state);
        match authorize_owner(&tokens, &parts.method, &parts.headers).await {
            Ok(session) => Ok(session),
            Err(denial) => {
                if !parts.method.is_safe() {
                    audit_owner_denial(&SqlitePool::from_ref(state), denial).await;
                }
                Err(denial.rejection())
            }
        }
    }
}

pub async fn serve(state: OwnerServerState) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 8080))
        .await
        .map_err(|error| format!("could not bind 0.0.0.0:8080: {error}"))?;
    // Connect-info wiring gives the login handler the peer socket address for
    // per-IP throttling. LAN has no trusted reverse proxy, so the peer IP — not
    // a forwarded header — is the correct source.
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|error| error.to_string())
}

/// Serve a static asset, or fall back to the SPA shell (`index.html`) for
/// in-memory routes. Hardened against path traversal: any request whose path
/// contains a `..` (or a Windows drive/separator) segment never reaches the
/// filesystem join — it falls through to the shell instead of escaping
/// `web_admin_dir`. This route is intentionally unauthenticated (it is the
/// login shell), so traversal here would otherwise be an unauthenticated
/// arbitrary file read.
async fn web_admin_static(
    State(state): State<OwnerServerState>,
    uri: Uri,
) -> Result<Response<Body>, (StatusCode, String)> {
    let raw = uri
        .path()
        .trim_start_matches("/admin")
        .trim_start_matches('/');

    let traversal = raw
        .split('/')
        .any(|segment| segment == ".." || segment.contains('\\') || segment.contains(':'));

    if !raw.is_empty() && !traversal {
        let candidate = state.web_admin_dir.join(raw);
        if let Ok(bytes) = tokio::fs::read(&candidate).await {
            return static_response(content_type_for(&candidate), bytes);
        }
        // A concrete asset (it has a file extension) that does not exist is a
        // genuine 404 — never mask it with the SPA shell, which would serve
        // HTML under the asset's content type.
        if candidate.extension().is_some() {
            return Err((StatusCode::NOT_FOUND, "not found".to_owned()));
        }
    }

    // SPA fallback: the app routes in memory, so the root and any extensionless
    // (or traversal) path serves the shell.
    let index = state.web_admin_dir.join("index.html");
    let bytes = tokio::fs::read(&index)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "not found".to_owned()))?;
    static_response("text/html; charset=utf-8", bytes)
}

/// Map a file extension to a response content type.
fn content_type_for(file_path: &std::path::Path) -> &'static str {
    match file_path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// Build a static-asset response without panicking on a malformed builder.
fn static_response(
    content_type: &str,
    content: Vec<u8>,
) -> Result<Response<Body>, (StatusCode, String)> {
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(content))
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

pub(crate) fn router(state: OwnerServerState) -> Router {
    Router::new()
        // Auth surface (unauthenticated; login/setup verify the PIN).
        .route("/web-admin/session", get(web_admin_session))
        .route("/web-admin/setup", post(web_admin_setup))
        .route("/web-admin/login", post(web_admin_login))
        .route("/web-admin/credential", post(web_admin_rotate_credential))
        .route("/web-admin/logout", post(web_admin_logout))
        .route("/web-admin/activity", post(web_admin_activity))
        // Owner theme record (ADR-0035) — both methods require `OwnerSession`.
        // The *public* theme read the lock screen needs lives on `:3210`.
        .route("/api/theme", get(get_theme).put(update_theme))
        .route("/api/shop-status", get(public_shop_status))
        // Dashboard + live data.
        .route("/web-admin/dashboard", get(dashboard))
        .route("/web-admin/coin/simulate", post(simulate_coin))
        .route("/web-admin/transactions", get(transactions))
        .route("/web-admin/today", get(today_report))
        .route("/web-admin/pcs", get(list_pc_settings).post(register_pc))
        .route(
            "/web-admin/pcs/{id}",
            axum::routing::put(update_pc_settings).delete(remove_pc),
        )
        .route("/web-admin/pcs/{id}/sign-out", post(owner_sign_out_pc))
        // PC Monitoring manual Check now (ADR-0076 / #340): strictly
        // read-only Host Reachability + Client Reachability probes. Targets
        // are always server-owned registered IPs; no request body exists.
        .route(
            "/web-admin/pc-monitoring/check/{pc_id}",
            post(pc_monitoring_check_pc),
        )
        .route(
            "/web-admin/pc-monitoring/check-all",
            post(pc_monitoring_check_all),
        )
        // PC Monitoring continuous detection (ADR-0076 / #341): the status
        // read feeds the standing advisory; the toggle re-proves the current
        // Owner PIN and is audited.
        .route("/web-admin/pc-monitoring/status", get(pc_monitoring_status))
        .route(
            "/web-admin/pc-monitoring/enabled",
            axum::routing::put(set_pc_monitoring_enabled),
        )
        // Client Recovery Escalation (ADR-0077): the one PC Monitoring setting
        // that authorises changing a PC's power state. Read for the standing
        // advisory; the write re-proves the current Owner PIN and is audited.
        .route(
            "/web-admin/pc-monitoring/escalation",
            get(pc_recovery_escalation).put(set_pc_recovery_escalation),
        )
        // Per-PC Monitoring Suppression (ADR-0076 / #342): create-or-replace
        // and cancel. Both re-prove the current Owner PIN like the toggle.
        .route(
            "/web-admin/pc-monitoring/suppressions/{pc_id}",
            post(set_pc_monitoring_suppression),
        )
        .route(
            "/web-admin/pc-monitoring/suppressions/{pc_id}/cancel",
            post(cancel_pc_monitoring_suppression),
        )
        // Telegram connect-and-protect (ADR-0076 / #345): guided setup with a
        // successful test gate before anything is stored. Reads need only an
        // Owner Session; connecting or disconnecting re-proves the current
        // Owner PIN like every other monitoring mutation. The token is never
        // returned by any of these responses.
        .route("/web-admin/telegram/status", get(telegram_status))
        .route(
            "/web-admin/telegram/setup/token",
            post(submit_telegram_token),
        )
        .route(
            "/web-admin/telegram/setup/detect",
            post(detect_telegram_chat),
        )
        .route("/web-admin/telegram/connect", post(connect_telegram))
        .route("/web-admin/telegram/disconnect", post(disconnect_telegram))
        // Owner-editable shop identity + Client owner PIN (ADR-0027).
        .route(
            "/web-admin/settings",
            get(get_settings).put(update_settings),
        )
        .route("/web-admin/owner-pin", axum::routing::put(update_owner_pin))
        .route(
            "/web-admin/shop-rate",
            get(get_shop_rate).put(update_shop_rate),
        )
        // Points transfer approvals (ADR-0030).
        .route("/web-admin/transfer-requests", get(list_transfer_requests))
        .route(
            "/web-admin/transfer-requests/confirm",
            post(confirm_transfer_request),
        )
        .route(
            "/web-admin/transfer-requests/reject",
            post(reject_transfer_request),
        )
        // Points economy settings and Redemption Tiers (ADR-0039).
        .route("/web-admin/economy", get(get_economy).put(update_economy))
        .route(
            "/web-admin/redemption-tiers",
            get(get_redemption_tiers).put(update_redemption_tiers),
        )
        .route(
            "/web-admin/coin-promo",
            get(get_coin_promo).put(update_coin_promo),
        )
        .route("/web-admin/coin-promo/preview", post(preview_coin_promo))
        .route(
            "/web-admin/attendance/settings",
            get(get_attendance_settings).put(update_attendance_settings),
        )
        .route(
            "/web-admin/attendance/settings/preview",
            post(preview_attendance_settings),
        )
        .route(
            "/web-admin/shop-hours",
            get(get_shop_hours).put(update_shop_hours),
        )
        .route(
            "/web-admin/backup-schedule",
            get(get_backup_schedule).put(update_backup_schedule),
        )
        .route(
            "/web-admin/shop-override",
            post(set_shop_override).delete(clear_shop_override),
        )
        // Vouchers.
        .route(
            "/web-admin/vouchers",
            get(list_vouchers).post(create_voucher),
        )
        .route("/web-admin/vouchers/{id}/void", post(void_voucher_handler))
        // Voucher batches.
        .route(
            "/web-admin/voucher-batches",
            get(list_voucher_batches).post(create_voucher_batch_handler),
        )
        .route(
            "/web-admin/voucher-batches/{id}",
            get(get_voucher_batch_handler),
        )
        .route(
            "/web-admin/voucher-batches/{id}/void",
            post(void_voucher_batch_handler),
        )
        .route(
            "/web-admin/voucher-template",
            get(get_voucher_template).put(update_voucher_template),
        )
        // Spin the Wheel.
        .route("/web-admin/wheel", get(get_wheel).put(update_wheel))
        // Accounts.
        .route(
            "/web-admin/accounts",
            get(list_accounts).post(create_account),
        )
        .route("/web-admin/accounts/import", post(import_accounts))
        .route("/web-admin/accounts/reset", post(reset_account))
        .route("/web-admin/accounts/delete", post(delete_account))
        .route("/web-admin/accounts/adjust-time", post(adjust_account_time))
        .route(
            "/web-admin/accounts/adjust-points",
            post(adjust_account_points),
        )
        // Reports.
        .route("/web-admin/reports/summary", post(report_summary))
        .route("/web-admin/reports/overview", get(sales_report_overview))
        .route("/web-admin/reports/attendance", post(attendance_report))
        .route("/web-admin/reports/account", post(account_report))
        .route("/web-admin/reports/pc", post(pc_report))
        .route("/web-admin/points-liability", get(points_liability_centi))
        // Backups.
        .route(
            "/web-admin/backups",
            get(list_backups_handler).post(create_backup_handler),
        )
        .route(
            "/web-admin/backups/{filename}",
            get(download_backup_handler).delete(delete_backup_handler),
        )
        .route("/web-admin/restore", get(get_pending_restore_handler))
        .route(
            "/web-admin/restore",
            post(stage_restore_handler).layer(DefaultBodyLimit::max(MAX_RESTORE_REQUEST_BYTES)),
        )
        .route(
            "/web-admin/restore",
            axum::routing::delete(cancel_restore_handler),
        )
        // Audit + diagnostics.
        .route("/web-admin/audit/search", post(audit_search))
        .route("/web-admin/audit/export", post(audit_export))
        .route("/web-admin/audit/actors", get(audit_actors))
        .route("/web-admin/audit/event-types", get(audit_event_types))
        // Audit-derived owner Notifications (ADR-0058).
        .route("/web-admin/notifications", get(notifications_list))
        .route(
            "/web-admin/notifications/unread-count",
            get(notifications_unread_count),
        )
        .route("/web-admin/notifications/seen", post(notifications_seen))
        .route(
            "/web-admin/notifications/acknowledge",
            post(notifications_acknowledge_many),
        )
        .route(
            "/web-admin/notifications/{audit_id}/acknowledge",
            post(notifications_acknowledge),
        )
        .route("/web-admin/diagnostics/search", post(diagnostics_search))
        // Diagnostics Mode (ADR-0081): the shop-wide visibility flag for the
        // Diagnostics page, toggled from Settings → Security. Both methods
        // require an Owner Session; the write is audited. The flag is
        // visibility only — no capture, verbosity, or retention behavior
        // branches on it, and it is not a security boundary.
        .route(
            "/web-admin/diagnostics-mode",
            get(get_diagnostics_mode).put(set_diagnostics_mode),
        )
        // Rejection Credits (ADR-0069).
        .route(
            "/web-admin/rejection-credits/rejections",
            get(rejection_credit_rejections),
        )
        .route(
            "/web-admin/rejection-credits/denominations",
            get(rejection_credit_denominations),
        )
        .route(
            "/web-admin/rejection-credits",
            post(rejection_credit_create),
        )
        // Manual adjustments.
        .route("/web-admin/manual-adjustment", post(manual_adjustment))
        // Coin-device settings + test acceptor (M4).
        .route(
            "/web-admin/coin-device",
            get(coin_device_get).put(coin_device_update),
        )
        .route("/web-admin/coin-device/ports", get(coin_device_ports))
        .route("/web-admin/coin-device/test", post(coin_device_test))
        .route(
            "/web-admin/coin-device/calibration",
            get(coin_calibration_current).post(coin_calibration_start),
        )
        .route(
            "/web-admin/coin-device/calibration/{capture_id}",
            get(coin_calibration_status),
        )
        .route(
            "/web-admin/coin-device/calibration/{capture_id}/sample",
            post(coin_calibration_sample),
        )
        .route(
            "/web-admin/coin-device/calibration/{capture_id}/arm",
            post(coin_calibration_arm),
        )
        .route(
            "/web-admin/coin-device/calibration/{capture_id}/cancel",
            post(coin_calibration_cancel),
        )
        .route(
            "/web-admin/coin-device/calibration/{capture_id}/apply",
            post(coin_calibration_apply),
        )
        .route(
            "/web-admin/coin-device/debug-bundle",
            get(coin_debug_bundle_get),
        )
        .route("/web-admin/support-bundle", get(support_bundle_get))
        .route("/web-admin/host-diagnostics", get(host_diagnostics_get))
        .route(
            "/web-admin/support/diagnostic-request/{pc_id}",
            post(diagnostic_request_post),
        )
        .route(
            "/web-admin/coin-device/acceptor-health",
            get(acceptor_health_get),
        )
        .route(
            "/web-admin/coin-device/debug-window",
            get(coin_debug_window_get).post(coin_debug_window_reset),
        )
        .route("/web-admin/update", get(app_update_get))
        .route("/web-admin/update/check", post(app_update_check))
        .route("/web-admin/update/install", post(app_update_install))
        .route("/web-admin/update/rollback", post(app_update_rollback))
        .route("/web-admin/update/ack", post(app_update_ack))
        .route(
            "/web-admin/update/auto-check",
            axum::routing::put(app_update_auto_check),
        )
        // Central Terminal Autostart (ADR-0055) — reads and writes the live
        // registry state; debug builds are read-only.
        .route(
            "/web-admin/autostart",
            get(autostart_get).put(autostart_put),
        )
        // M5: serve the Vite + Svelte + shadcn-svelte owner web app as static
        // assets at `/admin`. SPA routing is handled by the handler (any
        // unmatched path serves `index.html`).
        .route("/admin", get(web_admin_static))
        .route("/admin/{*path}", get(web_admin_static))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Auth surface
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PinInput {
    pin: String,
}

#[derive(Debug, Serialize)]
struct WebAdminSession {
    configured: bool,
    authenticated: bool,
    /// Present only for a live session, so a reloaded browser can restore the
    /// CSRF token its HttpOnly cookie cannot carry.
    #[serde(skip_serializing_if = "Option::is_none")]
    csrf_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RotateCredentialInput {
    current_pin: String,
    new_pin: String,
}

/// First-run PIN set (ADR-0024). Rejected once a PIN already exists.
async fn web_admin_setup(
    State(state): State<OwnerServerState>,
    Json(input): Json<PinInput>,
) -> Result<([(HeaderName, String); 1], Json<OwnerAuthResponse>), AppError> {
    owner::setup(&state.pool, &input.pin).await?;
    let grant = state.owner_tokens.issue().await;
    Ok(session_response(grant))
}

async fn web_admin_rotate_credential(
    State(state): State<OwnerServerState>,
    session: OwnerSession,
    Json(input): Json<RotateCredentialInput>,
) -> HttpResult<serde_json::Value> {
    let _credential_guard = state.owner_tokens.credential_guard().await;
    owner::rotate(&state.pool, &input.current_pin, &input.new_pin).await?;
    state.owner_tokens.revoke_all_except(&session.token).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn web_admin_login(
    State(state): State<OwnerServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<PinInput>,
) -> Result<Response, AppError> {
    let ip = peer.ip();
    let now = Utc::now();

    // A locked-out IP is rejected before the PIN is even verified, so lockout
    // costs nothing to enforce and the PIN is never exercised under attack.
    if let Decision::Locked { retry_after_secs } = state.login_throttle.check(ip, now).await {
        return Ok(throttled_response(retry_after_secs));
    }

    let _credential_guard = state.owner_tokens.credential_guard().await;
    match owner::authenticate(&state.pool, &input.pin).await {
        Ok(()) => {
            state.login_throttle.record_success(ip).await;
            let grant = state.owner_tokens.issue().await;
            Ok(session_response(grant).into_response())
        }
        Err(error) => {
            // Audit only the transition into lockout, so a sustained attack
            // does not flood the log (user story 9).
            if let Some(lockout_seconds) = state.login_throttle.record_failure(ip, now).await {
                let mut event = NewAuditEvent::simple(
                    "owner_login_throttled",
                    AuditOutcome::Denied,
                    AuditActor::Admin,
                    "Web Admin PIN login throttled after repeated failures",
                );
                event.metadata = serde_json::json!({
                    "source_ip": ip.to_string(),
                    "consecutive_failures": crate::login_throttle::FAILURE_THRESHOLD,
                    "lockout_seconds": lockout_seconds,
                });
                let _ = audit::append_standalone(&state.pool, event).await;
            }
            Err(error)
        }
    }
}

/// HTTP 429 with a `Retry-After` header carrying the remaining lockout seconds.
/// The PIN is not verified for a throttled request.
fn throttled_response(retry_after_secs: i64) -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(header::RETRY_AFTER, retry_after_secs.to_string())
        .body(Body::from("too many login attempts; try again later"))
        .unwrap_or_else(|_| StatusCode::TOO_MANY_REQUESTS.into_response())
}

/// Logout is gated like any mutation while the session is live, so another
/// site cannot end it. With no live session (missing, revoked, or expired) it
/// only clears the cookie, idempotently.
async fn web_admin_logout(
    State(state): State<OwnerServerState>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    match authorize_owner(&state.owner_tokens, &method, &headers).await {
        Ok(session) => state.owner_tokens.revoke(&session.token).await,
        Err(OwnerDenial::Unauthenticated) => {}
        Err(denial @ OwnerDenial::Forbidden) => {
            audit_owner_denial(&state.pool, denial).await;
            return denial.rejection().into_response();
        }
    }
    (
        [(
            header::SET_COOKIE,
            format!("{OWNER_COOKIE}=; {OWNER_COOKIE_ATTRIBUTES}; Max-Age=0"),
        )],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// Explicit human pointer/keyboard/touch input (ADR-0074). The only request
/// that restarts the idle window; it is a mutation, so it needs CSRF too.
async fn web_admin_activity(
    State(state): State<OwnerServerState>,
    session: OwnerSession,
) -> Json<serde_json::Value> {
    state
        .owner_tokens
        .record_human_activity(&session.token)
        .await;
    Json(serde_json::json!({ "ok": true }))
}

/// Session bootstrap. Never refreshes activity; hands a live session's CSRF
/// token back so a reloaded browser can restore it without the owner token.
async fn web_admin_session(
    State(state): State<OwnerServerState>,
    headers: HeaderMap,
) -> HttpResult<WebAdminSession> {
    let configured = owner::is_configured(&state.pool).await?;
    let csrf_token = match cookie_token(&headers) {
        Some(token) => state.owner_tokens.csrf_for(&token).await,
        None => None,
    };
    Ok(Json(WebAdminSession {
        configured,
        authenticated: csrf_token.is_some(),
        csrf_token,
    }))
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

async fn get_theme(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<ThemeSettings> {
    db::theme_settings(&state.pool).await.map(Json)
}

async fn update_theme(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<ThemeSettingsInput>, JsonRejection>,
) -> HttpResult<ThemeSettings> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let theme = db::update_theme_settings(&state.pool, &input).await?;
    state.events.publish_theme_updated(theme.clone());
    Ok(Json(theme))
}

// ---------------------------------------------------------------------------
// Dashboard + live data
// ---------------------------------------------------------------------------

async fn dashboard(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<DashboardSnapshot> {
    let mut dashboard = db::dashboard(&state.pool).await?;
    dashboard.pcs = state.sessions.enrich_pcs(dashboard.pcs).await;
    Ok(Json(dashboard))
}

#[derive(Debug, Deserialize)]
struct SimulateInput {
    #[serde(default)]
    pulse_count: Option<i64>,
}

async fn simulate_coin(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<SimulateInput>,
) -> HttpResult<SelectionState> {
    state
        .terminal_selection
        .inject_simulator_pulses(input.pulse_count.unwrap_or(1))
        .await
        .map(Json)
}

#[derive(Debug, Deserialize)]
struct LimitQuery {
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct VoucherListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    batch_id: Option<String>,
    search: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VoucherBatchListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn transactions(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Query(query): Query<LimitQuery>,
) -> HttpResult<Vec<CoinTransaction>> {
    db::transactions(&state.pool, query.limit.unwrap_or(50))
        .await
        .map(Json)
}

async fn today_report(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<TodayReport> {
    db::today_report(&state.pool).await.map(Json)
}

async fn list_pc_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<PcSettings>> {
    db::pc_settings(&state.pool).await.map(Json)
}

async fn update_pc_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
    Json(input): Json<PcSettingsInput>,
) -> HttpResult<PcSettings> {
    let updated = db::update_pc_settings(&state.pool, &pc_id, &input).await?;
    // Only broadcast for a visible PC. `pc-updated` is an upsert on the kiosk
    // grid (App.svelte `applyPcUpdate`), which the payload's lack of a
    // visibility flag means it cannot filter — so a push for a hidden PC would
    // flash it back into the grid until the next full refetch. On unhide the
    // broadcast still fires, so the PC reappears immediately.
    if updated.visible {
        if let Ok(pc) = db::pc(&state.pool, &pc_id).await {
            state
                .events
                .publish_pc_updated(state.sessions.enrich_pc(pc).await);
        }
    }
    Ok(Json(updated))
}

/// Pre-register a PC by number/name + a static IP (ADR-0031 / plan §4
/// M5). The new row ships with `mac_address` NULL, so the next boot
/// from the matching IP binds it; until then, no client can claim it.
/// A duplicate `id` is a 409; the row only becomes visible to clients
/// after the first heartbeat binds it.
async fn register_pc(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<PcRegisterInput>,
) -> HttpResult<Pc> {
    let pc = db::create_pc(
        &state.pool,
        &input.id,
        &input.name,
        input.registered_ip.as_deref(),
    )
    .await?;
    state
        .events
        .publish_pc_updated(state.sessions.enrich_pc(pc.clone()).await);
    Ok(Json(pc))
}

/// `DELETE /web-admin/pcs/{id}` — hard-delete a clean PC row (issue #112 /
/// ADR-0044). Owner-auth like its sibling PC settings routes. The server is
/// the sole authority: it refuses (409) a row with coin/adjustment/audit
/// history, remaining Guest Time, or an online status, its message naming the
/// reason; an unknown id is 404. Success returns the usual `MutationAck`; the
/// UI reloads the PC list from it.
async fn remove_pc(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
) -> HttpResult<MutationAck> {
    db::remove_pc(&state.pool, &pc_id).await?;
    Ok(Json(MutationAck { ok: true }))
}

#[derive(Debug, Deserialize)]
struct SignOutInput {
    session_id: String,
}

#[derive(Debug, Serialize)]
struct SignOutResponse {
    signed_out: bool,
    username: Option<String>,
}

/// Owner Sign-Out: end the Active Session on a PC from the dashboard. The PC
/// need not be reachable — the server owns the binding (ADR-0002), and the
/// Client drops the stale session on its next heartbeat.
async fn owner_sign_out_pc(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
    Json(input): Json<SignOutInput>,
) -> HttpResult<SignOutResponse> {
    let outcome = state
        .sessions
        .owner_sign_out(&state.pool, &pc_id, &input.session_id)
        .await?;
    let response = match outcome {
        SignOutOutcome::SignedOut { username } => {
            if let Ok(pc) = db::pc(&state.pool, &pc_id).await {
                state
                    .events
                    .publish_pc_updated(state.sessions.enrich_pc(pc).await);
            }
            SignOutResponse {
                signed_out: true,
                username: Some(username),
            }
        }
        SignOutOutcome::AlreadyEnded => SignOutResponse {
            signed_out: false,
            username: None,
        },
    };
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// PC Monitoring manual checks (ADR-0076 / GitHub #340)
// ---------------------------------------------------------------------------

/// `POST /web-admin/pc-monitoring/check/{pc_id}` — non-mutating Check now for
/// one PC. The probe target is always the row's server-owned `registered_ip`;
/// the endpoint takes no body, so no caller can supply an address. Unknown
/// ids are 404s; hidden or addressless PCs return explicitly unavailable
/// facts, never healthy.
async fn pc_monitoring_check_pc(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
) -> HttpResult<monitoring::PcMonitoringCheck> {
    Ok(Json(
        monitoring::check_pc(&state.pool, &state.host_probe, pc_id.trim()).await?,
    ))
}

/// `POST /web-admin/pc-monitoring/check-all` — non-mutating Check now for
/// every eligible PC (visible with a registered address). One result per
/// eligible PC; nothing else is probed and nothing is written.
async fn pc_monitoring_check_all(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<monitoring::PcMonitoringCheck>> {
    Ok(Json(
        monitoring::check_all(&state.pool, &state.host_probe).await?,
    ))
}

/// `GET /web-admin/pc-monitoring/status` — shop-wide monitoring state for the
/// standing advisory (ADR-0076). Read-only.
async fn pc_monitoring_status(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<PcMonitoringStatus> {
    Ok(Json(PcMonitoringStatus {
        enabled: db::monitoring::enabled(&state.pool).await?,
        platform_supported: state.host_probe.supported(),
    }))
}

/// `PUT /web-admin/pc-monitoring/enabled` — shop-wide enable/disable.
/// Requires an authenticated Owner Session AND the current Owner PIN; the
/// change is audited. Disabling closes every active incident as
/// `monitoring_disabled` in one transaction with the setting flip; enabling
/// just flips the flag — the monitor grants a fresh full grace because it
/// holds no in-memory state while disabled. Non-Windows Admin refuses:
/// monitoring that cannot probe must not pretend to run.
async fn set_pc_monitoring_enabled(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<PcMonitoringEnabledInput>,
) -> HttpResult<PcMonitoringStatus> {
    if !state.host_probe.supported() {
        return Err(AppError::InvalidInput(
            "PC Monitoring requires a Windows Admin".to_owned(),
        ));
    }
    owner::authenticate(&state.pool, &input.current_pin).await?;
    let current = db::monitoring::enabled(&state.pool).await?;
    if current != input.enabled {
        if input.enabled {
            db::monitoring::enable(&state.pool).await?;
        } else {
            db::monitoring::disable_and_close_active(&state.pool).await?;
        }
    }
    // Echo the machine's verified state, never the request's claim.
    let enabled = db::monitoring::enabled(&state.pool).await?;
    Ok(Json(PcMonitoringStatus {
        enabled,
        platform_supported: true,
    }))
}

/// `GET /web-admin/pc-monitoring/escalation` — the shop-wide Client Recovery
/// Escalation policy (ADR-0077). Read-only; feeds the standing advisory that
/// stands while escalation is *enabled*.
async fn pc_recovery_escalation(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<db::monitoring::EscalationSettings> {
    Ok(Json(
        db::monitoring::escalation_settings(&state.pool).await?,
    ))
}

/// `PUT /web-admin/pc-monitoring/escalation` — enable, disable, or retune
/// Client Recovery Escalation. Requires an authenticated Owner Session AND the
/// current Owner PIN, and is audited.
///
/// The delay floor is enforced here and again on read: no request and no
/// stored value may authorise powering a PC off on less evidence than
/// ADR-0077 requires.
async fn set_pc_recovery_escalation(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<ClientRecoveryEscalationInput>,
) -> HttpResult<db::monitoring::EscalationSettings> {
    if !state.host_probe.supported() {
        return Err(AppError::InvalidInput(
            "Client Recovery Escalation requires a Windows Admin".to_owned(),
        ));
    }
    if !(db::monitoring::MIN_ESCALATION_DELAY_SECS..=db::monitoring::MAX_ESCALATION_DELAY_SECS)
        .contains(&input.delay_secs)
    {
        return Err(AppError::InvalidInput(format!(
            "Escalation Delay must be between {} and {} seconds",
            db::monitoring::MIN_ESCALATION_DELAY_SECS,
            db::monitoring::MAX_ESCALATION_DELAY_SECS
        )));
    }
    // Escalation is meaningless without the detection it rides on, and an
    // owner who enables it on a shop with monitoring off would get a setting
    // that silently does nothing.
    if input.enabled && !db::monitoring::enabled(&state.pool).await? {
        return Err(AppError::InvalidInput(
            "Enable PC Monitoring before Client Recovery Escalation".to_owned(),
        ));
    }
    owner::authenticate(&state.pool, &input.current_pin).await?;
    Ok(Json(
        db::monitoring::set_escalation_settings_with_audit(
            &state.pool,
            input.enabled,
            input.delay_secs,
        )
        .await?,
    ))
}

/// Validate a suppression target at the Rust boundary (#342): the PC must
/// exist and be visible — hidden PCs are not monitored, so suppressing one
/// would promise protection the shop never sees. Returns the display name.
async fn suppression_target(pool: &SqlitePool, pc_id: &str) -> crate::errors::AppResult<String> {
    let pc_id = pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("PC id is required".to_owned()));
    }
    let row = db::pcs::pc_monitoring_row(pool, pc_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    if !row.visible {
        return Err(AppError::InvalidInput(
            "Monitoring Suppression requires a visible registered PC".to_owned(),
        ));
    }
    Ok(row.name)
}

/// `POST /web-admin/pc-monitoring/suppressions/{pc_id}` — create or replace
/// the per-PC Monitoring Suppression (ADR-0076 / #342). Requires an
/// authenticated Owner Session AND the current Owner PIN; duration is
/// validated at this boundary against fixed v1 policy (1–60 minutes) and the
/// absolute expiry is computed by Admin, never taken from the caller.
/// Audited; touches no money, time, Points, session, or PC status.
async fn set_pc_monitoring_suppression(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
    Json(input): Json<PcMonitoringSuppressionInput>,
) -> HttpResult<PcMonitoringSuppression> {
    let pc_id = pc_id.trim();
    let pc_name = suppression_target(&state.pool, pc_id).await?;
    let duration_minutes = input.duration_minutes;
    if !(1..=monitoring::MAX_SUPPRESSION_MINUTES).contains(&duration_minutes) {
        return Err(AppError::InvalidInput(format!(
            "Monitoring Suppression must last between 1 and {} minutes",
            monitoring::MAX_SUPPRESSION_MINUTES
        )));
    }
    owner::authenticate(&state.pool, &input.current_pin).await?;
    let expires_at = db::monitoring::set_suppression_with_audit(
        &state.pool,
        pc_id,
        &pc_name,
        duration_minutes,
        Utc::now(),
    )
    .await?;
    Ok(Json(PcMonitoringSuppression {
        pc_id: pc_id.to_owned(),
        pc_name,
        expires_at,
    }))
}

/// `POST /web-admin/pc-monitoring/suppressions/{pc_id}/cancel` — cancel the
/// per-PC Monitoring Suppression. Same session + PIN step-up and boundary
/// validation as creation; cancelling with nothing active stays a success so
/// a stale dashboard row cannot turn a double click into an error.
async fn cancel_pc_monitoring_suppression(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
    Json(input): Json<PcMonitoringSuppressionCancelInput>,
) -> HttpResult<PcMonitoringSuppressionCancelled> {
    let pc_id = pc_id.trim();
    let pc_name = suppression_target(&state.pool, pc_id).await?;
    owner::authenticate(&state.pool, &input.current_pin).await?;
    let cancelled =
        db::monitoring::cancel_suppression_with_audit(&state.pool, pc_id, &pc_name).await?;
    Ok(Json(PcMonitoringSuppressionCancelled { cancelled }))
}

// ---------------------------------------------------------------------------
// Telegram connect-and-protect (ADR-0076 / GitHub #345)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct TelegramTokenInput {
    token: String,
}

#[derive(serde::Deserialize)]
struct TelegramConnectInput {
    /// Fallback path only: a manually typed private chat id. Absent means
    /// "use the detected chat".
    #[serde(default)]
    chat_id: Option<String>,
    current_pin: String,
}

#[derive(serde::Deserialize)]
struct TelegramPinInput {
    current_pin: String,
}

/// `GET /web-admin/telegram/status` — Settings state: connected destination,
/// last successful test, and platform availability. Contains no token by
/// construction.
async fn telegram_status(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<TelegramStatus> {
    Ok(Json(state.telegram.status().await?))
}

/// `POST /web-admin/telegram/setup/token` — verify a candidate bot token and
/// hold it in memory for confirmation. Persists nothing, so this step needs
/// only the Owner Session; the PIN gate guards the completing connect call.
async fn submit_telegram_token(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<TelegramTokenInput>,
) -> HttpResult<SetupCandidate> {
    Ok(Json(state.telegram.submit_token(&input.token).await?))
}

/// `POST /web-admin/telegram/setup/detect` — one explicit look for recent
/// private messages using the pending token. This setup-time call is the only
/// polling the integration ever performs.
async fn detect_telegram_chat(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<DetectedChat>> {
    Ok(Json(state.telegram.detect_chat().await?))
}

/// `POST /web-admin/telegram/connect` — confirm the destination (detected or
/// manual fallback), send the test message, and activate the new
/// configuration only if that test succeeds. Requires an authenticated Owner
/// Session AND the current Owner PIN; audited on success. A failed test
/// leaves the previously working configuration untouched.
async fn connect_telegram(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<TelegramConnectInput>,
) -> HttpResult<TelegramStatus> {
    owner::authenticate(&state.pool, &input.current_pin).await?;
    Ok(Json(
        state.telegram.connect(input.chat_id.as_deref()).await?,
    ))
}

/// `POST /web-admin/telegram/disconnect` — delete the protected token and
/// destination. Same session + PIN step-up as connecting; audited. Local PC
/// Monitoring is untouched.
async fn disconnect_telegram(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<TelegramPinInput>,
) -> HttpResult<MutationAck> {
    owner::authenticate(&state.pool, &input.current_pin).await?;
    let disconnected = state.telegram.disconnect().await?;
    Ok(Json(MutationAck { ok: disconnected }))
}

// ---------------------------------------------------------------------------
// Economy settings
// ---------------------------------------------------------------------------

async fn get_economy(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<EconomySettings> {
    db::economy_settings(&state.pool).await.map(Json)
}

async fn update_economy(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<EconomySettingsInput>,
) -> HttpResult<EconomySettings> {
    db::update_economy_settings(&state.pool, &input)
        .await
        .map(Json)
}

async fn get_redemption_tiers(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<RedemptionTier>> {
    db::redemption_tiers(&state.pool).await.map(Json)
}

async fn update_redemption_tiers(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<RedemptionTiersInput>, JsonRejection>,
) -> HttpResult<Vec<RedemptionTier>> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::update_redemption_tiers(&state.pool, &input)
        .await
        .map(Json)
}

async fn get_coin_promo(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<CoinPromoSettings> {
    db::coin_promo_settings(&state.pool).await.map(Json)
}

async fn preview_coin_promo(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<CoinPromoSettingsInput>, JsonRejection>,
) -> HttpResult<CoinPromoSettings> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::preview_coin_promo_settings(&state.pool, &input)
        .await
        .map(Json)
}

async fn update_coin_promo(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<CoinPromoSettingsInput>, JsonRejection>,
) -> HttpResult<CoinPromoSettings> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::update_coin_promo_settings(&state.pool, &input)
        .await
        .map(Json)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttendanceSettingsDraftInput {
    enabled: bool,
    daily_threshold_cents: i64,
    reward_minutes: i64,
    claim_window_months: i64,
}

impl From<AttendanceSettingsDraftInput> for openpiso_core::attendance::AttendanceSettingsDraft {
    fn from(input: AttendanceSettingsDraftInput) -> Self {
        Self {
            enabled: input.enabled,
            daily_threshold_cents: input.daily_threshold_cents,
            reward_minutes: input.reward_minutes,
            claim_window_months: input.claim_window_months,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttendanceSettingsUpdateInput {
    enabled: bool,
    daily_threshold_cents: i64,
    reward_minutes: i64,
    claim_window_months: i64,
    expected_version: i64,
}

impl From<AttendanceSettingsUpdateInput> for openpiso_core::attendance::AttendanceSettingsDraft {
    fn from(input: AttendanceSettingsUpdateInput) -> Self {
        Self {
            enabled: input.enabled,
            daily_threshold_cents: input.daily_threshold_cents,
            reward_minutes: input.reward_minutes,
            claim_window_months: input.claim_window_months,
        }
    }
}

#[derive(Serialize)]
struct AttendanceSettingsEffectiveChanges {
    enabled: &'static str,
    daily_threshold_cents: &'static str,
    reward_minutes: &'static str,
    claim_window_months: &'static str,
}

impl Default for AttendanceSettingsEffectiveChanges {
    fn default() -> Self {
        Self {
            enabled: "immediately_for_new_contributions",
            daily_threshold_cents: "next_unfrozen_daily_snapshot",
            reward_minutes: "next_unfrozen_daily_snapshot",
            claim_window_months: "next_earning_month",
        }
    }
}

#[derive(Serialize)]
struct AttendanceSettingsResponse {
    #[serde(flatten)]
    settings: db::attendance::AttendanceSettingsVersion,
    economics_advisory: Option<db::attendance_reports::AttendanceEconomicsAdvisory>,
    liability: db::attendance_reports::AttendanceLiability,
    next_earning_month: String,
    effective_changes: AttendanceSettingsEffectiveChanges,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<db::attendance::AttendanceSettingsUpdateOutcome>,
}

impl AttendanceSettingsResponse {
    fn from_snapshot(
        snapshot: db::attendance_reports::AttendanceSettingsSnapshot,
        outcome: Option<db::attendance::AttendanceSettingsUpdateOutcome>,
    ) -> Self {
        Self {
            settings: snapshot.settings,
            economics_advisory: snapshot.economics_advisory,
            liability: snapshot.liability,
            next_earning_month: snapshot.next_earning_month,
            effective_changes: AttendanceSettingsEffectiveChanges::default(),
            outcome,
        }
    }
}

async fn get_attendance_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<AttendanceSettingsResponse> {
    let snapshot = db::attendance_reports::attendance_settings_snapshot(
        &state.pool,
        Utc::now(),
        db::attendance::host_offset(),
    )
    .await?;
    Ok(Json(AttendanceSettingsResponse::from_snapshot(
        snapshot, None,
    )))
}

async fn preview_attendance_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<AttendanceSettingsDraftInput>, JsonRejection>,
) -> HttpResult<db::attendance::AttendanceSettingsPreview> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::attendance::preview_settings(&state.pool, input.into())
        .await
        .map(Json)
}

async fn update_attendance_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<AttendanceSettingsUpdateInput>, JsonRejection>,
) -> HttpResult<AttendanceSettingsResponse> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let expected_version = input.expected_version;
    let settings_update =
        db::attendance::update_settings(&state.pool, input.into(), expected_version).await?;
    let snapshot = db::attendance_reports::attendance_settings_snapshot(
        &state.pool,
        Utc::now(),
        db::attendance::host_offset(),
    )
    .await?;
    Ok(Json(AttendanceSettingsResponse::from_snapshot(
        snapshot,
        Some(settings_update.outcome),
    )))
}

// Shop Hours schedule persistence (ADR-0053).

async fn public_shop_status(State(state): State<OwnerServerState>) -> HttpResult<ShopStatus> {
    let status = db::shop_status(&state.pool).await?;
    state.events.set_shop_status(status.clone());
    Ok(Json(status))
}

async fn get_shop_hours(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<ShopHoursSettingsResponse> {
    let settings = db::shop_hours_settings(&state.pool).await?;
    let status = db::shop_status(&state.pool).await?;
    state.events.set_shop_status(status.clone());
    Ok(Json(ShopHoursSettingsResponse { settings, status }))
}

async fn update_shop_hours(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<ShopHoursSettingsInput>, JsonRejection>,
) -> HttpResult<ShopHoursSettingsResponse> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let settings = db::update_shop_hours(&state.pool, &input).await?;
    let status = db::shop_status(&state.pool).await?;
    state.events.set_shop_status(status.clone());
    publish_all_pcs(&state).await;
    Ok(Json(ShopHoursSettingsResponse { settings, status }))
}

async fn get_backup_schedule(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<BackupScheduleSettings> {
    db::backup_schedule_settings(&state.pool).await.map(Json)
}

async fn update_backup_schedule(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<BackupScheduleSettingsInput>, JsonRejection>,
) -> HttpResult<BackupScheduleSettings> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::update_backup_schedule(&state.pool, &input)
        .await
        .map(Json)
}

/// `GET /web-admin/diagnostics-mode` — the shop-wide Diagnostics Mode flag
/// (ADR-0081). Read-only; feeds the Settings → Security toggle.
async fn get_diagnostics_mode(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<DiagnosticsModeStatus> {
    Ok(Json(DiagnosticsModeStatus {
        enabled: db::diagnostics_mode::enabled(&state.pool).await?,
    }))
}

/// `PUT /web-admin/diagnostics-mode` — flip the shop-wide flag (ADR-0081).
/// Requires an authenticated Owner Session and is audited; no PIN step-up,
/// because the flag only reveals a redacted, read-only page and step-up is
/// reserved for authority actions. Nothing else branches on the value.
async fn set_diagnostics_mode(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<DiagnosticsModeInput>, JsonRejection>,
) -> HttpResult<DiagnosticsModeStatus> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    Ok(Json(DiagnosticsModeStatus {
        enabled: db::diagnostics_mode::set_enabled(&state.pool, input.enabled).await?,
    }))
}

async fn set_shop_override(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<ShopOverrideInput>, JsonRejection>,
) -> HttpResult<ShopHoursSettingsResponse> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let settings = db::set_shop_override(&state.pool, &input).await?;
    let status = db::shop_status(&state.pool).await?;
    state.events.set_shop_status(status.clone());
    publish_all_pcs(&state).await;
    Ok(Json(ShopHoursSettingsResponse { settings, status }))
}

async fn clear_shop_override(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<ShopHoursSettingsResponse> {
    let settings = db::clear_shop_override(&state.pool).await?;
    let status = db::shop_status(&state.pool).await?;
    state.events.set_shop_status(status.clone());
    publish_all_pcs(&state).await;
    Ok(Json(ShopHoursSettingsResponse { settings, status }))
}

async fn publish_all_pcs(state: &OwnerServerState) {
    if let Ok(pcs) = db::pcs(&state.pool).await {
        for pc in state.sessions.enrich_pcs(pcs).await {
            state.events.publish_pc_updated(pc);
        }
    }
}

// ---------------------------------------------------------------------------
// Shop settings + Client owner PIN (ADR-0027)
// ---------------------------------------------------------------------------

async fn get_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<AdminSettings> {
    let shop_name = db::shop_name(&state.pool).await?;
    let shutdown_grace_seconds = db::shutdown_grace_seconds(&state.pool).await?;
    let offline_grace_seconds = db::offline_grace_seconds(&state.pool).await?;
    Ok(Json(AdminSettings {
        shop_name,
        shutdown_grace_seconds,
        offline_grace_seconds,
    }))
}

async fn update_settings(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AdminSettingsInput>,
) -> HttpResult<AdminSettings> {
    let trimmed_shop_name = input.shop_name.trim();
    if trimmed_shop_name.is_empty() {
        return Err(AppError::InvalidInput("shop name is required".to_owned()));
    }
    if trimmed_shop_name.chars().count() > 80 {
        return Err(AppError::InvalidInput(
            "shop name must not exceed 80 characters".to_owned(),
        ));
    }
    let current_shutdown_grace_seconds = db::shutdown_grace_seconds(&state.pool).await?;
    let next_shutdown_grace_seconds = db::validate_shutdown_grace_seconds(
        input
            .shutdown_grace_seconds
            .unwrap_or(current_shutdown_grace_seconds),
    )?;
    let current_offline_grace_seconds = db::offline_grace_seconds(&state.pool).await?;
    let next_offline_grace_seconds = db::validate_offline_grace_seconds(
        input
            .offline_grace_seconds
            .unwrap_or(current_offline_grace_seconds),
    )?;

    let shop_name = db::set_shop_name(&state.pool, trimmed_shop_name).await?;
    state.events.publish_shop_name_updated(shop_name.clone());
    let shutdown_grace_seconds =
        db::set_shutdown_grace_seconds(&state.pool, next_shutdown_grace_seconds).await?;
    let offline_grace_seconds =
        db::set_offline_grace_seconds(&state.pool, next_offline_grace_seconds).await?;
    Ok(Json(AdminSettings {
        shop_name,
        shutdown_grace_seconds,
        offline_grace_seconds,
    }))
}

/// Write-only: the new PIN is hashed server-side and never read back
/// (ADR-0027). The Client's own gate verifies via `POST /api/owner/verify-pin`.
async fn update_owner_pin(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<OwnerPinInput>,
) -> HttpResult<MutationAck> {
    db::set_owner_pin(&state.pool, &input.new_pin).await?;
    Ok(Json(MutationAck { ok: true }))
}

async fn get_shop_rate(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<ShopRate> {
    db::shop_rate(&state.pool).await.map(Json)
}

async fn update_shop_rate(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<ShopRateInput>, JsonRejection>,
) -> HttpResult<ShopRate> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let rate = db::update_shop_rate(&state.pool, &input).await?;
    state.events.publish_shop_rate_updated(KioskConfig {
        rate_amount_cents: rate.amount_cents,
        rate_seconds: rate.seconds,
    });
    Ok(Json(rate))
}

// ---------------------------------------------------------------------------
// Points transfer approvals (ADR-0030)
// ---------------------------------------------------------------------------

/// List the pending Points Transfer Requests for the owner's approvals view.
/// Past-TTL rows are swept to `expired` first (on-read sweep, plan §5.3) so a
/// stale request never appears confirmable.
async fn list_transfer_requests(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<PendingPointsTransfer>> {
    db::expire_pending_points_transfer_requests(&state.pool, POINTS_TRANSFER_TTL_SECONDS).await?;
    accounts::list_pending_points_transfer_requests(&state.pool)
        .await
        .map(Json)
}

/// Approve a pending request by its 6-digit code. The points move atomically
/// and are re-validated against the live sender balance (ADR-0030).
async fn confirm_transfer_request(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<TransferCodeInput>,
) -> HttpResult<PointsTransferRequest> {
    let outcome = state
        .sessions
        .confirm_points_transfer(&state.pool, &input.code)
        .await?;
    for pc_id in &outcome.changed_pc_ids {
        publish_adjusted_pc(&state, Some(pc_id)).await;
    }
    Ok(Json(outcome.request))
}

/// Reject a pending request by its 6-digit code. No points move.
async fn reject_transfer_request(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<TransferCodeInput>,
) -> HttpResult<PointsTransferRequest> {
    accounts::reject_points_transfer(&state.pool, &input.code)
        .await
        .map(Json)
}

// ---------------------------------------------------------------------------
// Vouchers
// ---------------------------------------------------------------------------

async fn list_vouchers(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Query(query): Query<VoucherListQuery>,
) -> HttpResult<Vec<Voucher>> {
    db::vouchers(
        &state.pool,
        query.limit.unwrap_or(100),
        query.offset.unwrap_or(0),
        query.batch_id.as_deref(),
        query.search.as_deref(),
        query.status.as_deref(),
    )
    .await
    .map(Json)
}

async fn create_voucher(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<CreateVoucherInput>,
) -> HttpResult<Voucher> {
    db::create_voucher(&state.pool, &input).await.map(Json)
}

async fn void_voucher_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(id): Path<String>,
) -> HttpResult<Voucher> {
    db::void_voucher(&state.pool, &id).await.map(Json)
}

// ---------------------------------------------------------------------------
// Voucher batches
// ---------------------------------------------------------------------------

async fn list_voucher_batches(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Query(query): Query<VoucherBatchListQuery>,
) -> HttpResult<Vec<VoucherBatchSummary>> {
    db::list_voucher_batches(
        &state.pool,
        query.limit.unwrap_or(100),
        query.offset.unwrap_or(0),
    )
    .await
    .map(Json)
}

async fn create_voucher_batch_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<CreateVoucherBatchInput>,
) -> HttpResult<VoucherBatch> {
    db::create_voucher_batch(&state.pool, &input)
        .await
        .map(Json)
}

async fn get_voucher_batch_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(id): Path<String>,
) -> HttpResult<VoucherBatchDetail> {
    db::get_voucher_batch(&state.pool, &id).await.map(Json)
}

async fn void_voucher_batch_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(id): Path<String>,
) -> HttpResult<VoucherBatch> {
    db::void_voucher_batch(&state.pool, &id).await.map(Json)
}

async fn get_voucher_template(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<VoucherTemplate> {
    db::voucher_template(&state.pool).await.map(Json)
}

async fn update_voucher_template(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<VoucherTemplateInput>, JsonRejection>,
) -> HttpResult<VoucherTemplate> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::update_voucher_template(&state.pool, &input)
        .await
        .map(Json)
}

// ---------------------------------------------------------------------------
// Spin the Wheel
// ---------------------------------------------------------------------------

async fn get_wheel(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<WheelSettings> {
    db::wheel_settings(&state.pool).await.map(Json)
}

async fn update_wheel(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<WheelSettingsInput>,
) -> HttpResult<WheelSettings> {
    db::update_wheel_settings(&state.pool, &input)
        .await
        .map(Json)
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

async fn list_accounts(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<OwnerAccount>> {
    accounts::list(&state.pool)
        .await
        .map(|accounts| accounts.into_iter().map(OwnerAccount::from).collect())
        .map(Json)
}

async fn create_account(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AdminAccountInput>,
) -> HttpResult<OwnerAccount> {
    accounts::create(
        &state.pool,
        &input.username,
        &input.password,
        AccountCreator::Admin,
        None,
    )
    .await
    .map(OwnerAccount::from)
    .map(Json)
}

async fn import_accounts(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<serde_json::Value>, JsonRejection>,
) -> HttpResult<AccountImportReport> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let dry_run = input
        .get("dry_run")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| AppError::InvalidInput("dry_run must be a boolean".to_owned()))?;
    if dry_run {
        account_import::preview(&state.pool, input).await.map(Json)
    } else {
        account_import::commit(&state.pool, input).await.map(Json)
    }
}

async fn reset_account(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AdminAccountMutationInput>,
) -> HttpResult<OwnerAccount> {
    let password = input
        .password
        .as_deref()
        .ok_or_else(|| AppError::InvalidInput("new password is required".to_owned()))?;
    let account = accounts::reset_password(&state.pool, &input.account_id, password).await?;
    if let Ok((_, username_normalized)) =
        openpiso_core::account::normalize_username(&account.username)
    {
        state
            .account_login_throttle
            .record_success(&username_normalized)
            .await;
    }
    Ok(Json(OwnerAccount::from(account)))
}

async fn delete_account(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AdminAccountDeleteInput>,
) -> HttpResult<AccountDeletionResult> {
    let mut account_ids = input.account_ids;
    if let Some(account_id) = input.account_id {
        account_ids.push(account_id);
    }
    let deleted_ids = accounts::soft_delete_many_ids(&state.pool, &account_ids).await?;
    let deleted_count = deleted_ids.len();
    let mut changed_pc_ids = Vec::new();
    for account_id in &deleted_ids {
        if let Some(pc_id) = state.sessions.remove_account(account_id).await {
            if !changed_pc_ids.iter().any(|changed| changed == &pc_id) {
                changed_pc_ids.push(pc_id);
            }
        }
        state.account_tokens.revoke_account(account_id).await;
    }
    for pc_id in changed_pc_ids {
        if let Ok(pc) = db::pc(&state.pool, &pc_id).await {
            state
                .events
                .publish_pc_updated(state.sessions.enrich_pc(pc).await);
        }
    }
    Ok(Json(AccountDeletionResult { deleted_count }))
}

async fn adjust_account_time(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AdminAccountAdjustInput>,
) -> HttpResult<OwnerAccount> {
    let outcome = state
        .sessions
        .adjust_time(&state.pool, &input.account_id, input.delta)
        .await?;
    publish_adjusted_pc(&state, outcome.pc_id.as_deref()).await;
    Ok(Json(OwnerAccount::from(outcome.account)))
}

async fn adjust_account_points(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AdminAccountAdjustPointsInput>,
) -> HttpResult<OwnerAccount> {
    let outcome = state
        .sessions
        .adjust_points(&state.pool, &input.account_id, input.delta_centi)
        .await?;
    publish_adjusted_pc(&state, outcome.pc_id.as_deref()).await;
    Ok(Json(OwnerAccount::from(outcome.account)))
}

/// Re-publish a PC whose live session balance was just changed by an owner
/// action, so the connected client sees it on its next heartbeat.
async fn publish_adjusted_pc(state: &OwnerServerState, pc_id: Option<&str>) {
    let Some(pc_id) = pc_id else { return };
    if let Ok(pc) = db::pc(&state.pool, pc_id).await {
        state
            .events
            .publish_pc_updated(state.sessions.enrich_pc(pc).await);
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

async fn attendance_report(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<db::attendance_reports::AttendanceReportInput>, JsonRejection>,
) -> HttpResult<db::attendance_reports::AttendanceReport> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    db::attendance_reports::attendance_report(&state.pool, &input)
        .await
        .map(Json)
}

async fn report_summary(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<ReportSummaryInput>,
) -> HttpResult<ReportSummary> {
    reports::report_summary(
        &state.pool,
        input.period,
        input.from_date.as_deref(),
        input.to_date.as_deref(),
    )
    .await
    .map(Json)
}

async fn sales_report_overview(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<SalesReportOverview> {
    reports::sales_report_overview(&state.pool).await.map(Json)
}

async fn account_report(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AccountReportInput>,
) -> HttpResult<PcReport> {
    reports::account_report(
        &state.pool,
        &input.account_id,
        input.from_date.as_deref(),
        input.to_date.as_deref(),
    )
    .await
    .map(Json)
}

async fn pc_report(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<PcReportInput>,
) -> HttpResult<PcReport> {
    reports::pc_report(
        &state.pool,
        &input.pc_id,
        input.from_date.as_deref(),
        input.to_date.as_deref(),
    )
    .await
    .map(Json)
}

async fn points_liability_centi(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<i64> {
    reports::points_liability_total(&state.pool).await.map(Json)
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateBackupInput {
    #[serde(default)]
    backup_sets: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct CreatedBackup {
    filename: String,
    size_bytes: u64,
}

async fn list_backups_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<BackupCatalogEntry>> {
    backup::list_backups(&state.backups_dir).await.map(Json)
}

async fn create_backup_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    input: Result<Json<CreateBackupInput>, JsonRejection>,
) -> HttpResult<CreatedBackup> {
    let Json(input) = input.map_err(|error| AppError::InvalidInput(error.body_text()))?;
    let selected_sets = match input.backup_sets {
        Some(names) => {
            if names.is_empty() {
                return Err(AppError::InvalidInput(
                    "at least one Backup Set is required".to_owned(),
                ));
            }
            names
                .iter()
                .map(|name| {
                    BackupSet::parse_name(name).ok_or_else(|| {
                        AppError::InvalidInput(format!("unknown Backup Set: {name}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        None => BackupSet::all().to_vec(),
    };
    let artifact = backup::write_backup(
        &state.pool,
        &state.backups_dir,
        backup::BackupKind::Manual,
        &selected_sets,
    )
    .await?;
    let filename = artifact
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::InvalidInput("Backup filename is not valid UTF-8".to_owned()))?
        .to_owned();
    let size_bytes = tokio::fs::metadata(&artifact.path).await?.len();
    let mut event = NewAuditEvent::simple(
        "backup_created",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Manual Backup created",
    );
    event.metadata = serde_json::json!({
        "filename": filename,
        "size_bytes": size_bytes,
        "backup_sets": artifact
            .header
            .sets
            .iter()
            .map(|set| set.as_str())
            .collect::<Vec<_>>(),
    });
    audit::append_standalone(&state.pool, event).await?;
    Ok(Json(CreatedBackup {
        filename,
        size_bytes,
    }))
}

fn backup_path(directory: &FsPath, filename: &str) -> Result<PathBuf, AppError> {
    let valid_name = filename.starts_with("openpiso-")
        && filename.ends_with(".sql")
        && filename.len() <= 255
        && !filename.contains('/')
        && !filename.contains('\\')
        && filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
    if !valid_name {
        return Err(AppError::InvalidInput("invalid Backup filename".to_owned()));
    }
    Ok(directory.join(filename))
}

async fn download_backup_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(filename): Path<String>,
) -> Result<Response, AppError> {
    let path = backup_path(&state.backups_dir, &filename)?;
    let bytes = tokio::fs::read(&path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::NotFound("Backup not found".to_owned())
        } else {
            AppError::Io(error)
        }
    })?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/sql; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(bytes))
        .map_err(|error| {
            AppError::InvalidInput(format!("could not build download response: {error}"))
        })
}

async fn delete_backup_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(filename): Path<String>,
) -> HttpResult<serde_json::Value> {
    let path = backup_path(&state.backups_dir, &filename)?;
    tokio::fs::remove_file(&path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::NotFound("Backup not found".to_owned())
        } else {
            AppError::Io(error)
        }
    })?;
    if filename.starts_with("openpiso-pre_migration-") {
        let companion = path.with_extension("db");
        match tokio::fs::remove_file(companion).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::Io(error)),
        }
    }
    let mut event = NewAuditEvent::simple(
        "backup_deleted",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Backup deleted",
    );
    event.metadata = serde_json::json!({ "filename": filename });
    audit::append_standalone(&state.pool, event).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct RestoreInput {
    filename: Option<String>,
    path: Option<String>,
}

async fn get_pending_restore_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Option<PendingRestore>> {
    backup::pending_restore(&state.backups_dir).await.map(Json)
}

fn multipart_upload_error(error: axum::extract::multipart::MultipartError) -> AppError {
    if error.body_text() == "Request payload is too large" {
        AppError::InvalidInput("uploaded Backup exceeds the 256 MB size limit".to_owned())
    } else {
        AppError::InvalidInput(format!("invalid multipart upload: {error}"))
    }
}

async fn uploaded_restore_source(
    mut multipart: Multipart,
    directory: &FsPath,
) -> Result<RestoreSource, AppError> {
    tokio::fs::create_dir_all(directory).await?;
    let upload_path = directory
        .parent()
        .unwrap_or(directory)
        .join(format!(".openpiso-upload-{}.tmp", uuid::Uuid::new_v4()));

    let result = async {
        let mut output = tokio::fs::File::create(&upload_path).await?;
        let mut filename = None;
        let mut bytes_written = 0usize;
        while let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(multipart_upload_error)?
        {
            if field.name() != Some("file") || filename.is_some() {
                return Err(AppError::InvalidInput(
                    "multipart restore requires exactly one file field named `file`".to_owned(),
                ));
            }
            filename = Some(field.file_name().unwrap_or("uploaded.sql").to_owned());
            while let Some(chunk) = field.chunk().await.map_err(multipart_upload_error)? {
                bytes_written = bytes_written.saturating_add(chunk.len());
                if bytes_written > MAX_RESTORE_UPLOAD_BYTES {
                    return Err(AppError::InvalidInput(
                        "uploaded Backup exceeds the 256 MB size limit".to_owned(),
                    ));
                }
                output.write_all(&chunk).await?;
            }
        }
        output.flush().await?;
        let filename = filename.ok_or_else(|| {
            AppError::InvalidInput("multipart restore requires a `.sql` file upload".to_owned())
        })?;
        Ok(RestoreSource::Uploaded {
            path: upload_path.clone(),
            filename,
        })
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&upload_path).await;
    }
    result
}

async fn restore_source_from_request(
    request: Request<Body>,
    directory: &FsPath,
) -> Result<(RestoreSource, Option<PathBuf>), AppError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type.starts_with("multipart/form-data") {
        if request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > MAX_RESTORE_REQUEST_BYTES as u64)
        {
            return Err(AppError::InvalidInput(
                "uploaded Backup exceeds the 256 MB size limit".to_owned(),
            ));
        }
        let source = uploaded_restore_source(
            Multipart::from_request(request, &())
                .await
                .map_err(|error| {
                    AppError::InvalidInput(format!("invalid multipart upload: {error}"))
                })?,
            directory,
        )
        .await?;
        let path = match &source {
            RestoreSource::Uploaded { path, .. } => Some(path.clone()),
            _ => None,
        };
        return Ok((source, path));
    }

    let body = to_bytes(request.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|error| AppError::InvalidInput(format!("invalid restore request: {error}")))?;
    let input = serde_json::from_slice::<RestoreInput>(&body)
        .map_err(|error| AppError::InvalidInput(format!("invalid restore request: {error}")))?;
    let source = match (input.filename, input.path) {
        (Some(filename), None) => RestoreSource::Catalog(filename),
        (None, Some(path)) => RestoreSource::AbsolutePath(PathBuf::from(path)),
        (Some(_), Some(_)) => {
            return Err(AppError::InvalidInput(
                "provide either a Catalog filename or an absolute path, not both".to_owned(),
            ))
        }
        (None, None) => {
            return Err(AppError::InvalidInput(
                "a Catalog filename, absolute path, or uploaded file is required".to_owned(),
            ))
        }
    };
    Ok((source, None))
}

async fn stage_restore_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    request: Request<Body>,
) -> HttpResult<PendingRestore> {
    let (source, uploaded_path) = restore_source_from_request(request, &state.backups_dir).await?;
    let active_pc_ids = state.sessions.active_pc_ids().await;
    let pool = state.pool.clone();
    let backups_dir = state.backups_dir.clone();
    let pending_result = match tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                AppError::Conflict(format!("could not start restore worker: {error}"))
            })?
            .block_on(backup::stage_restore_owned(
                pool,
                backups_dir,
                source,
                active_pc_ids,
            ))
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(AppError::Conflict(format!(
            "restore worker stopped: {error}"
        ))),
    };

    let cleanup_result = if let Some(path) = uploaded_path {
        tokio::fs::remove_file(path).await
    } else {
        Ok(())
    };
    let pending = match (pending_result, cleanup_result) {
        (Ok(pending), Ok(())) => pending,
        (Ok(_), Err(error)) => {
            let _ = backup::cancel_pending_restore(&state.backups_dir).await;
            return Err(AppError::Io(error));
        }
        (Err(error), _) => return Err(error),
    };

    let final_blocking_pcs =
        backup::restore_blocking_pcs(state.pool.clone(), state.sessions.active_pc_ids().await)
            .await?;
    if !final_blocking_pcs.is_empty() {
        let _ = backup::cancel_pending_restore(&state.backups_dir).await;
        return Err(AppError::RestoreRejected {
            gate: "precondition".to_owned(),
            message: format!(
                "Restore is blocked while PCs have active use, Remaining Time, or an active account binding; blocking PCs: {}",
                final_blocking_pcs.join(", ")
            ),
            blocking_pcs: final_blocking_pcs,
        });
    }
    let mut event = NewAuditEvent::simple(
        "restore_requested",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Restore staged and requested for next launch",
    );
    event.metadata = serde_json::json!({
        "filename": pending.filename,
        "schema_version": pending.schema_version,
        "requested_at": pending.requested_at,
    });
    if let Err(error) = audit::append_standalone(&state.pool, event).await {
        let _ = backup::cancel_pending_restore(&state.backups_dir).await;
        return Err(error);
    }
    Ok(Json(pending))
}

async fn cancel_restore_handler(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<serde_json::Value> {
    let cancelled = backup::cancel_pending_restore(&state.backups_dir).await?;
    if cancelled {
        let event = NewAuditEvent::simple(
            "restore_cancelled",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Pending Restore cancelled",
        );
        audit::append_standalone(&state.pool, event).await?;
    }
    Ok(Json(serde_json::json!({ "cancelled": cancelled })))
}

// ---------------------------------------------------------------------------
// Audit + diagnostics
// ---------------------------------------------------------------------------

async fn audit_search(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(filter): Json<AuditFilterInput>,
) -> HttpResult<AuditPage> {
    let rows = reports::search_audit_log(&state.pool, &filter).await?;
    let total = reports::count_audit_log(&state.pool, &filter).await?;
    Ok(Json(AuditPage { rows, total }))
}

async fn audit_actors(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<String>> {
    reports::distinct_actors(&state.pool).await.map(Json)
}

async fn audit_event_types(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<String>> {
    reports::distinct_event_types(&state.pool).await.map(Json)
}

async fn audit_export(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(filter): Json<AuditFilterInput>,
) -> HttpResult<String> {
    reports::export_audit_csv(&state.pool, &filter)
        .await
        .map(Json)
}

async fn diagnostics_search(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(filter): Json<DiagnosticFilterInput>,
) -> HttpResult<Vec<DiagnosticEntry>> {
    diagnostics::search_diagnostic_log(&state.log_dir, &filter).map(Json)
}

#[derive(Debug, Deserialize)]
struct NotificationListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    severity: Option<String>,
    unread: Option<bool>,
}

fn notification_list_filter(
    query: NotificationListQuery,
) -> Result<db::NotificationListFilter, AppError> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AppError::InvalidInput(
            "notification limit must be between 1 and 200".to_owned(),
        ));
    }
    let offset = query.offset.unwrap_or(0);
    if offset < 0 {
        return Err(AppError::InvalidInput(
            "notification offset must be non-negative".to_owned(),
        ));
    }
    let severity = match query.severity.as_deref() {
        None => None,
        Some("alert") => Some(db::NotificationSeverityFilter::Alert),
        Some("notice") => Some(db::NotificationSeverityFilter::Notice),
        Some(_) => {
            return Err(AppError::InvalidInput(
                "notification severity must be alert or notice".to_owned(),
            ));
        }
    };
    let unread = match query.unread {
        None => None,
        Some(true) => Some(db::NotificationUnreadFilter::Unread),
        Some(false) => Some(db::NotificationUnreadFilter::Read),
    };
    Ok(db::NotificationListFilter {
        limit,
        offset,
        severity,
        unread,
    })
}

async fn notifications_list(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Query(query): Query<NotificationListQuery>,
) -> HttpResult<crate::models::NotificationListResponse> {
    db::list_notifications(&state.pool, notification_list_filter(query)?)
        .await
        .map(Json)
}

async fn notifications_unread_count(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<db::NotificationUnreadCount> {
    db::notification_unread_count(&state.pool).await.map(Json)
}

async fn notifications_seen(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<db::NotificationSeen> {
    db::mark_notifications_seen(&state.pool).await.map(Json)
}

async fn notifications_acknowledge(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(audit_id): Path<String>,
) -> HttpResult<db::AlertAcknowledgement> {
    db::acknowledge_notification(&state.pool, &audit_id)
        .await
        .map(Json)
}

async fn notifications_acknowledge_many(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<NotificationAcknowledgeInput>,
) -> HttpResult<db::BulkAlertAcknowledgement> {
    db::acknowledge_notifications(&state.pool, &input.audit_ids)
        .await
        .map(Json)
}

// ---------------------------------------------------------------------------
// Rejection Credits (ADR-0069)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RejectionListQuery {
    account_id: Option<String>,
    #[serde(default)]
    creditable_only: bool,
}

async fn rejection_credit_rejections(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Query(query): Query<RejectionListQuery>,
) -> HttpResult<db::CreditableRejectionList> {
    let account_id = query
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty());
    db::list_coin_batch_rejections(
        &state.pool,
        db::RejectionListFilter {
            account_id,
            creditable_only: query.creditable_only,
        },
    )
    .await
    .map(Json)
}

async fn rejection_credit_denominations(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<db::RejectionDenominations> {
    db::rejection_denominations(&state.pool).await.map(Json)
}

async fn rejection_credit_create(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<db::RejectionCreditInput>,
) -> HttpResult<db::RejectionCreditResult> {
    let (credit, session_pc_id) = state.sessions.credit_rejection(&state.pool, &input).await?;
    if !credit.dry_run {
        // A guest credit changes the PC row directly; an account credit changes
        // the bound session's PC. Either way the client sees it next heartbeat.
        let pc_id = session_pc_id.unwrap_or_else(|| credit.pc_id.clone());
        publish_adjusted_pc(&state, Some(&pc_id)).await;
    }
    Ok(Json(credit))
}

// ---------------------------------------------------------------------------
// Manual adjustments
// ---------------------------------------------------------------------------

async fn manual_adjustment(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<ManualAdjustmentInput>,
) -> HttpResult<ManualAdjustment> {
    let adjustment = db::create_manual_adjustment(&state.pool, &input).await?;
    if let Ok(pc) = db::pc(&state.pool, &adjustment.pc_id).await {
        state
            .events
            .publish_pc_updated(state.sessions.enrich_pc(pc).await);
    }
    Ok(Json(adjustment))
}

// ---------------------------------------------------------------------------
// Coin-device settings + test acceptor (M4)
// ---------------------------------------------------------------------------

async fn coin_device_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<CoinDeviceSettings> {
    db::coin_device_settings(&state.pool).await.map(Json)
}

async fn coin_device_update(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<CoinDeviceSettingsInput>,
) -> HttpResult<CoinDeviceSettings> {
    let current = db::coin_device_settings(&state.pool).await?;
    let candidate = CoinDeviceSettings {
        com_port: input
            .com_port
            .as_deref()
            .map(str::trim)
            .filter(|port| !port.is_empty())
            .map(str::to_owned),
        baud_rate: input.baud_rate,
        set_pin_line: input.set_pin_line.clone(),
        coin_pin_line: input.coin_pin_line.clone(),
        coin_pin_polarity: input.coin_pin_polarity.clone(),
        inter_coin_gap_ms: input.inter_coin_gap_ms,
        inactivity_timeout_seconds: input
            .inactivity_timeout_seconds
            .unwrap_or(current.inactivity_timeout_seconds),
        hard_cap_seconds: input.hard_cap_seconds.unwrap_or(current.hard_cap_seconds),
        pulse_map: input.pulse_map.clone(),
        calibration_required: current.calibration_required,
    };
    let source_config = db::coin_source_config_from_settings(&candidate)?;
    let current_source_config = db::coin_source_config_from_settings(&current)?;
    let selection_config = db::selection_config_from_settings(&candidate)?;
    let saved = state
        .terminal_selection
        .reconfigure_after(
            &candidate,
            source_config,
            selection_config,
            &current,
            current_source_config,
            None,
            || db::update_coin_device_settings(&state.pool, &input),
        )
        .await?;
    // A successful Coin Device save advances the Coin Debug Window so the next
    // bundle covers only coins inserted under the freshly-saved config. This is
    // the only settings write path that advances the window.
    state.advance_debug_window();
    Ok(Json(saved))
}

/// Enumerate host serial ports and merge the saved COM Port into the result.
/// The scan is read-only and requires an owner session.
async fn coin_device_ports(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Vec<serial_port_detection::DetectedSerialPort>> {
    let settings = db::coin_device_settings(&state.pool).await?;
    let detected = serial_port_detection::detect_serial_ports()?;
    Ok(Json(serial_port_detection::merge_saved(
        &detected,
        settings.com_port.as_deref(),
    )))
}

/// "Test acceptor" action. With no COM port configured the shop runs on the
/// simulator source, so the test is a no-op success. With a port configured we
/// attempt to open the serial handle (which inhibits the set pin on
/// construction AND on the immediate drop — fail-closed, ADR-0023) and report
/// the result. We never assert the set pin here, so no coin can leak.
///
/// The saved `set_pin_line` / `coin_pin_line` are already applied to the live
/// source: [`crate::coin_source::ConfiguredCoinSource::from_settings`] parses
/// them and opens the port via `open_with_lines`. What remains for on-site
/// bring-up is purely physical calibration (denominations + gap, bench-tested
/// per `hardware.md`), which needs the real acceptor and cannot be unit-tested.
async fn coin_device_test(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<CoinDeviceTestResult> {
    let settings = db::coin_device_settings(&state.pool).await?;
    let Some(port) = settings.com_port.clone() else {
        return Ok(Json(CoinDeviceTestResult {
            ok: true,
            simulator: true,
            message: "No COM port configured; running on the simulator source.".to_owned(),
        }));
    };
    let available = state.terminal_selection.is_source_available().await;
    let result = CoinDeviceTestResult {
        ok: available,
        simulator: false,
        message: if available {
            format!(
                "{port} is open using set={} and coin={}; the source is available.",
                settings.set_pin_line, settings.coin_pin_line
            )
        } else {
            format!("{port} is configured but the serial source is unavailable")
        },
    };
    Ok(Json(result))
}

async fn append_coin_calibration_audit(
    pool: &SqlitePool,
    event_type: &'static str,
    summary: &'static str,
    metadata: serde_json::Value,
) -> crate::errors::AppResult<()> {
    let mut event = NewAuditEvent::simple(
        event_type,
        AuditOutcome::Success,
        AuditActor::Admin,
        summary,
    );
    event.metadata = metadata;
    audit::append_standalone(pool, event).await.map(|_| ())
}

async fn coin_calibration_start(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<CalibrationStartInput>,
) -> HttpResult<CalibrationStatus> {
    let status = state
        .terminal_selection
        .start_calibration(input.target_amount_cents)
        .await?;
    if let Err(error) = append_coin_calibration_audit(
        &state.pool,
        "coin_calibration_started",
        "Physical coin calibration started",
        serde_json::json!({
            "capture_id": status.capture_id,
            "target_amount_cents": status.target_amount_cents,
            "required_samples": status.required_samples,
        }),
    )
    .await
    {
        if state
            .terminal_selection
            .cancel_calibration(&status.capture_id)
            .await
            .is_err()
        {
            let _ = state
                .terminal_selection
                .fail_calibration(&status.capture_id, format!("start audit failed: {error}"))
                .await;
        }
        return Err(error);
    }
    Ok(Json(status))
}

async fn coin_calibration_current(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<Option<CalibrationStatus>> {
    state
        .terminal_selection
        .current_calibration()
        .await
        .map(Json)
}

async fn coin_calibration_status(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(capture_id): Path<String>,
) -> HttpResult<CalibrationStatus> {
    state
        .terminal_selection
        .calibration_status(&capture_id)
        .await
        .map(Json)
}

async fn coin_calibration_sample(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(capture_id): Path<String>,
) -> HttpResult<CalibrationStatus> {
    let status = state
        .terminal_selection
        .record_calibration_sample(&capture_id)
        .await?;
    if status.phase == crate::terminal_selection::CalibrationPhase::Completed {
        if let Err(error) = append_coin_calibration_audit(
            &state.pool,
            "coin_calibration_completed",
            "Physical coin calibration samples completed",
            serde_json::json!({
                "capture_id": status.capture_id,
                "target_amount_cents": status.target_amount_cents,
                "sample_counts": status.sample_counts,
            }),
        )
        .await
        {
            // A completed in-memory capture must never remain Apply-eligible
            // if its completion audit could not be persisted. Keep the source
            // inhibited and force the owner to start a fresh capture.
            let _ = state
                .terminal_selection
                .fail_calibration(&capture_id, format!("completion audit failed: {error}"))
                .await;
            return Err(error);
        }
        state
            .terminal_selection
            .mark_calibration_completion_audited(&capture_id)
            .await?;
    }
    Ok(Json(status))
}

async fn coin_calibration_arm(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(capture_id): Path<String>,
) -> HttpResult<CalibrationStatus> {
    state
        .terminal_selection
        .arm_calibration_sample(&capture_id)
        .await
        .map(Json)
}

async fn coin_calibration_cancel(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(capture_id): Path<String>,
) -> HttpResult<CalibrationStatus> {
    state
        .terminal_selection
        .prepare_calibration_cancel(&capture_id)
        .await?;
    append_coin_calibration_audit(
        &state.pool,
        "coin_calibration_cancelled",
        "Physical coin calibration cancelled",
        serde_json::json!({ "capture_id": capture_id }),
    )
    .await?;
    state
        .terminal_selection
        .finish_calibration_cancel(&capture_id)
        .await
        .map(Json)
}

async fn coin_calibration_apply(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(capture_id): Path<String>,
) -> HttpResult<CoinDeviceSettings> {
    let (pulse_count, amount_cents) = state
        .terminal_selection
        .calibration_mapping(&capture_id)
        .await?;
    let current = db::coin_device_settings(&state.pool).await?;
    let clears_candidate_gate = current.calibration_required && amount_cents == 2000;
    let mut pulse_map = current
        .pulse_map
        .iter()
        .filter(|entry| !(clears_candidate_gate && entry.pulse_count == 20))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(existing) = pulse_map
        .iter()
        .find(|entry| entry.pulse_count == pulse_count)
    {
        if existing.amount_cents != amount_cents {
            return Err(AppError::Conflict(format!(
                "pulse count {pulse_count} already maps to {} cents; resolve the existing mapping first",
                existing.amount_cents
            )));
        }
    }
    if !pulse_map
        .iter()
        .any(|entry| entry.pulse_count == pulse_count)
    {
        pulse_map.push(crate::models::PulseMapEntry {
            pulse_count,
            amount_cents,
        });
    }
    let input = CoinDeviceSettingsInput {
        com_port: current.com_port.clone(),
        baud_rate: current.baud_rate,
        set_pin_line: current.set_pin_line.clone(),
        coin_pin_line: current.coin_pin_line.clone(),
        coin_pin_polarity: current.coin_pin_polarity.clone(),
        inter_coin_gap_ms: current.inter_coin_gap_ms,
        inactivity_timeout_seconds: Some(current.inactivity_timeout_seconds),
        hard_cap_seconds: Some(current.hard_cap_seconds),
        pulse_map,
    };
    let candidate = CoinDeviceSettings {
        com_port: input.com_port.clone(),
        baud_rate: input.baud_rate,
        set_pin_line: input.set_pin_line.clone(),
        coin_pin_line: input.coin_pin_line.clone(),
        coin_pin_polarity: input.coin_pin_polarity.clone(),
        inter_coin_gap_ms: input.inter_coin_gap_ms,
        inactivity_timeout_seconds: input
            .inactivity_timeout_seconds
            .unwrap_or(current.inactivity_timeout_seconds),
        hard_cap_seconds: input.hard_cap_seconds.unwrap_or(current.hard_cap_seconds),
        pulse_map: input.pulse_map.clone(),
        calibration_required: current.calibration_required && !clears_candidate_gate,
    };
    let saved = state
        .terminal_selection
        .reconfigure_after(
            &candidate,
            db::coin_source_config_from_settings(&candidate)?,
            db::selection_config_from_settings(&candidate)?,
            &current,
            db::coin_source_config_from_settings(&current)?,
            Some((&capture_id, pulse_count, amount_cents)),
            || {
                db::update_coin_device_settings_with_calibration_audit(
                    &state.pool,
                    &input,
                    &capture_id,
                    pulse_count,
                    amount_cents,
                    clears_candidate_gate,
                )
            },
        )
        .await?;
    state.advance_debug_window();
    Ok(Json(saved))
}

/// Build the owner-requested Coin Debug Bundle. All diagnostic selection,
/// verdict, redaction, and size-bound logic belongs to the builder.
async fn coin_debug_bundle_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<String> {
    let port_choices = match serial_port_detection::detect_serial_ports() {
        Ok(detected) => match db::coin_device_settings(&state.pool).await {
            Ok(settings) => Some(serial_port_detection::merge_saved(
                &detected,
                settings.com_port.as_deref(),
            )),
            Err(error) => {
                tracing::warn!(error = %error, "coin debug bundle could not read saved COM Port");
                Some(serial_port_detection::merge_saved(&detected, None))
            }
        },
        Err(_) => None,
    };
    Ok(Json(
        crate::coin_debug_bundle::build_coin_debug_bundle(
            &state.log_dir,
            &state.pool,
            state.current_debug_window_start(),
            port_choices.as_deref(),
            &crate::coin_debug_bundle::DebugBundleOptions::default(),
        )
        .await,
    ))
}

/// Build the owner-requested Support Bundle for one PC and one window
/// (ADR-0078 / #360). All diagnostic selection, merging, redaction, and
/// size-bound logic belongs to the builder; this handler only validates the
/// boundary and owns the window default. `incident_opened_at` anchors the
/// window around a PC Monitoring Incident (one hour before to fifteen
/// minutes after, #363) instead of ending it at generation time.
#[derive(Debug, Deserialize)]
struct SupportBundleQuery {
    pc_id: String,
    window_seconds: Option<i64>,
    incident_opened_at: Option<String>,
}

async fn support_bundle_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Query(query): Query<SupportBundleQuery>,
) -> HttpResult<String> {
    let pc_id = query.pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("pc_id is required".to_owned()));
    }
    // An incident anchor the boundary cannot parse is a request error, not a
    // silent fall-back to the unanchored window.
    let window_anchor = match query.incident_opened_at.as_deref() {
        None => None,
        Some(raw) => match crate::bundle_machinery::parse_flexible_utc(raw) {
            Some(anchor) => Some(anchor),
            None => {
                return Err(AppError::InvalidInput(
                    "incident_opened_at must be a UTC timestamp (RFC3339 or SQLite datetime)"
                        .to_owned(),
                ))
            }
        },
    };
    // An unknown PC is a boundary failure (404), never an empty bundle.
    db::pcs::pc(&state.pool, pc_id).await?;
    // The Verdict's Guard-poll evidence is in-memory state (ADR-0077); no log
    // row or SQLite record holds it, so the endpoint hands it over.
    let guard_facts = state.guards.facts(pc_id);
    // The Admin Host section comes from the same collector the
    // host-diagnostics endpoint serves (ADR-0082), so the bundle and the
    // owner's screen cannot disagree.
    let host = state.host_diagnostics();
    Ok(Json(
        crate::support_bundle::build_support_bundle(
            &state.log_dir,
            &state.log_dir,
            &state.pool,
            pc_id,
            &crate::support_bundle::SupportBundleOptions {
                window_seconds: query
                    .window_seconds
                    .unwrap_or(crate::support_bundle::DEFAULT_WINDOW_SECONDS),
                window_anchor,
                ..Default::default()
            },
            guard_facts.as_ref(),
            &host,
        )
        .await,
    ))
}

/// `GET /web-admin/host-diagnostics` — Admin's own health (ADR-0082 / #378):
/// Run Records newest first, each with its app version, PID, start, end, and
/// Exit Reason, including any run classified as a suspected hard exit, plus
/// the panic file's presence, last-modified time, and its newest 8 KB. The
/// same collector feeds the Support Bundle's Admin Host section.
///
/// Any authenticated Owner Session is served, with or without Diagnostics
/// Mode: the flag is visibility only and is never a boundary (ADR-0081).
async fn host_diagnostics_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<crate::host_diagnostics::HostDiagnostics> {
    Ok(Json(state.host_diagnostics()))
}

/// `POST /web-admin/support/diagnostic-request/{pc_id}` — the owner's ask for
/// more evidence from a live Client (ADR-0078, the Diagnostic Request). Admin
/// arms a single-use nonce that the Heartbeat response then carries; the
/// Client answers through the batched Forwarded Diagnostics endpoint and the
/// next Support Bundle includes the slice. An unknown PC is 404; a PC that is
/// offline or unreachable produces no error at all — the ask simply expires
/// unfulfilled after its TTL, so there is nothing to clean up and nothing to
/// notify. Owner Session only: the request grants no authority and causes no
/// durable write.
async fn diagnostic_request_post(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Path(pc_id): Path<String>,
) -> HttpResult<MutationAck> {
    let pc_id = pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("pc_id is required".to_owned()));
    }
    // An unknown PC is a boundary failure (404), never a silently doomed ask.
    db::pcs::pc(&state.pool, pc_id).await?;
    state.diagnostic_requests.arm(pc_id, chrono::Utc::now());
    Ok(Json(MutationAck { ok: true }))
}

/// The owner-facing S1 health snapshot. The timing fields are deliberately
/// inert until S2 supplies filtered inter-pulse timing evidence.
async fn acceptor_health_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<AcceptorHealth> {
    Ok(Json(AcceptorHealth {
        debug_window_start: state.current_debug_window_start().to_rfc3339(),
        timing_status: "unavailable_until_filtered",
        implausible_timing_rate: None,
    }))
}

/// The current Coin Debug Window start, reported to the owner surface. A
/// distinct runtime concept from the persisted `CoinDeviceSettings`.
#[derive(Serialize)]
struct CoinDebugWindow {
    /// RFC3339 UTC timestamp where the current window starts.
    since: String,
}

/// Read the current Coin Debug Window start.
async fn coin_debug_window_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<CoinDebugWindow> {
    Ok(Json(CoinDebugWindow {
        since: state.current_debug_window_start().to_rfc3339(),
    }))
}

/// Advance the Coin Debug Window start to now (manual reset). Destroys nothing —
/// only the next bundle's coverage narrows.
async fn coin_debug_window_reset(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<CoinDebugWindow> {
    let now = state.advance_debug_window();
    Ok(Json(CoinDebugWindow {
        since: now.to_rfc3339(),
    }))
}

// ---------------------------------------------------------------------------
// Central Terminal Autostart (ADR-0055)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AutostartResponse {
    enabled: bool,
    supported: bool,
    editable: bool,
    exe_path: String,
}

/// `GET /web-admin/autostart` — read the live Autostart state from the host
/// machine (registry on Windows, Launch Agent on macOS, .desktop on Linux).
/// The registry is the sole source of truth; there is no SQLite mirror
/// (ADR-0055).
async fn autostart_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<AutostartResponse> {
    Ok(Json(autostart_response(&state)))
}

#[derive(Debug, Deserialize)]
struct AutostartInput {
    enabled: bool,
}

async fn app_update_get(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<UpdateState> {
    Ok(Json(
        state.app_update.state(&state.pool, &state.sessions).await?,
    ))
}

async fn app_update_check(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<UpdateState> {
    let error = state.app_update.check(state.release_feed.as_ref()).await;
    let mut event = NewAuditEvent::simple(
        "app_update_checked",
        if error.is_some() {
            AuditOutcome::Failure
        } else {
            AuditOutcome::Success
        },
        AuditActor::Admin,
        "App Update checked",
    );
    event.metadata = serde_json::json!({ "error": error });
    audit::append_standalone(&state.pool, event).await?;
    Ok(Json(
        state.app_update.state(&state.pool, &state.sessions).await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppUpdateInstallInput {
    version: String,
    #[serde(default)]
    acknowledge_schema_change: bool,
}

async fn app_update_install(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AppUpdateInstallInput>,
) -> HttpResult<UpdateState> {
    let update = state.app_update.state(&state.pool, &state.sessions).await?;
    let release = (|| {
        let release = update
            .available
            .filter(|release| release.version == input.version)
            .ok_or_else(|| {
                AppError::InvalidInput("version is not the offered App Update".to_owned())
            })?;
        if release.raises_schema && !input.acknowledge_schema_change {
            return Err(AppError::InvalidInput(
                "App Update requires Schema Version acknowledgement".to_owned(),
            ));
        }
        Ok(release)
    })();
    app_update_handoff(&state, &input.version, release, false).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppUpdateRollbackInput {
    version: String,
    confirm_version: String,
}

async fn app_update_rollback(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AppUpdateRollbackInput>,
) -> HttpResult<UpdateState> {
    let update = state.app_update.state(&state.pool, &state.sessions).await?;
    let release = (|| {
        if input.confirm_version != input.version {
            return Err(AppError::InvalidInput(
                "Compatible Rollback confirmation must match exactly".to_owned(),
            ));
        }
        update
            .rollbacks
            .into_iter()
            .find(|release| release.version == input.version)
            .ok_or_else(|| {
                AppError::InvalidInput(
                    "version is not a current Compatible Rollback candidate".to_owned(),
                )
            })
    })();
    app_update_handoff(&state, &input.version, release, true).await
}

async fn app_update_handoff(
    state: &OwnerServerState,
    version: &str,
    release: crate::errors::AppResult<crate::app_update::Release>,
    rollback: bool,
) -> HttpResult<UpdateState> {
    let result = async {
        state
            .app_update
            .install(
                &state.pool,
                state.installer.as_ref(),
                &state.run_records,
                release?,
                rollback,
            )
            .await
    }
    .await;
    if let Err(error) = result {
        let mut event = NewAuditEvent::simple(
            if rollback {
                "app_update_rollback"
            } else {
                "app_update_install"
            },
            AuditOutcome::Failure,
            AuditActor::Admin,
            if rollback {
                "Compatible Rollback failed or rejected"
            } else {
                "App Update install failed or rejected"
            },
        );
        event.metadata = serde_json::json!({ "version": version, "error": error.to_string() });
        audit::append_standalone(&state.pool, event).await?;
        return Err(error);
    }
    Ok(Json(
        state.app_update.state(&state.pool, &state.sessions).await?,
    ))
}

async fn app_update_ack(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
) -> HttpResult<UpdateState> {
    if let Err(error) = db::app_update::acknowledge_result(&state.pool).await {
        let mut event = NewAuditEvent::simple(
            "app_update_result_acknowledged",
            AuditOutcome::Failure,
            AuditActor::Admin,
            "App Update acknowledgement rejected",
        );
        event.metadata = serde_json::json!({ "error": error.to_string() });
        audit::append_standalone(&state.pool, event).await?;
        return Err(error);
    }
    Ok(Json(
        state.app_update.state(&state.pool, &state.sessions).await?,
    ))
}

#[derive(Deserialize)]
struct AppUpdateAutoCheckInput {
    enabled: bool,
}

async fn app_update_auto_check(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AppUpdateAutoCheckInput>,
) -> HttpResult<UpdateState> {
    db::app_update::set_auto_check(&state.pool, input.enabled).await?;
    Ok(Json(
        state.app_update.state(&state.pool, &state.sessions).await?,
    ))
}

/// `PUT /web-admin/autostart` — write the Autostart enabled state and re-read
/// from the host machine. The response always reflects the machine state after
/// the write attempt, never an echo of the request.
///
/// A change while `editable` or `supported` is false is refused with a client
/// error and changes nothing. Every attempted change — success or failure —
/// appends an audit event.
async fn autostart_put(
    State(state): State<OwnerServerState>,
    _session: OwnerSession,
    Json(input): Json<AutostartInput>,
) -> Result<Json<AutostartResponse>, AppError> {
    if !state.autostart.supported() {
        return Err(AppError::InvalidInput(
            "Autostart is not supported on this platform".to_owned(),
        ));
    }
    if !state.autostart.editable() {
        return Err(AppError::InvalidInput(
            "Autostart is not editable in this build".to_owned(),
        ));
    }

    let write_result = state.autostart.set_enabled(input.enabled);

    // Re-read the machine state after the write attempt, regardless of
    // whether it succeeded. The response always reflects the actual state.
    let response = autostart_response(&state);

    let exe_path = state.autostart.exe_path();
    let (event_type, outcome, summary) = match &write_result {
        Ok(()) => (
            "autostart_changed",
            AuditOutcome::Success,
            "Central Terminal Autostart changed",
        ),
        Err(_) => (
            "autostart_change_failed",
            AuditOutcome::Failure,
            "Central Terminal Autostart change failed",
        ),
    };
    let mut event = NewAuditEvent::simple(event_type, outcome, AuditActor::Admin, summary);
    event.metadata = serde_json::json!({
        "enabled": input.enabled,
        "exe_path": exe_path,
    });
    audit::append_standalone(&state.pool, event).await?;

    // Surface the write error *after* auditing so the failure is always
    // recorded.
    write_result?;

    Ok(Json(response))
}

/// Build the standard Autostart response from the live port state.
fn autostart_response(state: &OwnerServerState) -> AutostartResponse {
    AutostartResponse {
        enabled: state.autostart.is_enabled(),
        supported: state.autostart.supported(),
        editable: state.autostart.editable(),
        exe_path: state.autostart.exe_path(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Cookie attributes shared by issue and clear. `Secure` is omitted while the
/// Web Admin is plain HTTP on the LAN (ADR-0074).
const OWNER_COOKIE_ATTRIBUTES: &str = "HttpOnly; SameSite=Strict; Path=/";

/// Build a `Set-Cookie` response for a freshly issued Owner Session. The
/// owner token goes only into the cookie; JSON carries just the CSRF token.
fn session_response(grant: OwnerGrant) -> ([(HeaderName, String); 1], Json<OwnerAuthResponse>) {
    (
        [(
            header::SET_COOKIE,
            format!(
                "{OWNER_COOKIE}={}; {OWNER_COOKIE_ATTRIBUTES}; Max-Age={}",
                grant.token,
                OWNER_ABSOLUTE_LIMIT.as_secs()
            ),
        )],
        Json(OwnerAuthResponse {
            csrf_token: grant.csrf,
        }),
    )
}

/// Extract the owner token from the `Cookie` header, if present.
fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        part.trim()
            .strip_prefix(&format!("{OWNER_COOKIE}="))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use crate::app_update::RELEASE_REPOSITORY;
    use sqlx::sqlite::SqlitePoolOptions;
    use uuid::Uuid;

    use super::*;
    use crate::{accounts::AccountCreator, terminal_selection::TerminalSelectionConfig};

    /// Build an owner-server state backed by an in-memory database. The
    /// terminal selection reuses the same pool so coin paths land in the
    /// canonical tables, mirroring the production wiring.
    async fn test_state() -> OwnerServerState {
        // Tests that do not exercise host evidence leave the Run Record file
        // and the panic file in the system temp directory, unread.
        test_state_in(&std::env::temp_dir()).await
    }

    /// [`test_state`] with the Admin data directory (the Run Record file's
    /// home and the parent of `logs/panic.log`) pointed at `data_dir`, so a
    /// test can seed host evidence without touching another test's.
    async fn test_state_in(data_dir: &std::path::Path) -> OwnerServerState {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");

        let sessions = SessionStore::default();
        let events = EventHub::without_ui();
        let coin_config = db::coin_source_config(&pool)
            .await
            .expect("coin source config should load");
        let source = ConfiguredCoinSource::simulator(coin_config).expect("simulator source");
        let terminal_selection = Arc::new(
            TerminalSelection::new(
                source,
                TerminalSelectionConfig::defaults(),
                pool.clone(),
                sessions.clone(),
                events.clone(),
            )
            .expect("terminal selection"),
        );
        let telegram_service = Arc::new(telegram::TelegramService::new(
            pool.clone(),
            Arc::new(telegram::testing::FakeTransport::new()),
            Arc::new(telegram::testing::FakeProtector::new(true)),
        ));
        OwnerServerState {
            pool,
            events,
            owner_tokens: OwnerTokens::default(),
            account_tokens: AccountTokens::default(),
            sessions,
            autostart: Arc::new(crate::autostart::FakeAutostart::new(
                false,
                "/test/openpiso-admin.exe",
            )),
            release_feed: Arc::new(crate::app_update::testing::FakeReleaseFeed::default()),
            installer: Arc::new(crate::app_update::testing::FakeInstaller::default()),
            app_update: Arc::new(crate::app_update::UpdateService::default()),
            log_dir: data_dir.join("logs"),
            backups_dir: std::env::temp_dir(),
            app_started_at: Utc::now(),
            debug_window_start: Arc::new(std::sync::RwLock::new(Utc::now())),
            terminal_selection,
            web_admin_dir: std::env::temp_dir(),
            login_throttle: LoginThrottle::default(),
            account_login_throttle: AccountLoginThrottle::default(),
            host_probe: Arc::new(crate::monitoring::testing::FakeHostProbe::default()),
            telegram: telegram_service,
            guards: crate::guard_poll::GuardRegistry::new(),
            diagnostic_requests: crate::diagnostic_request::DiagnosticRequestRegistry::new(),
            run_records: Arc::new(crate::run_record::RunRecordStore::new(
                data_dir.join(crate::run_record::RUN_RECORDS_FILE_NAME),
                Arc::new(crate::run_record::SystemClock),
            )),
        }
    }

    async fn spawn(state: OwnerServerState) -> String {
        state
            .app_update
            .startup(&state.pool, &state.run_records)
            .await
            .expect("App Update startup");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address")
            .to_string();
        tokio::spawn(async move {
            axum::serve(
                listener,
                router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test server should run");
        });
        address
    }

    async fn spawn_client_server(state: &OwnerServerState) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("client test listener should bind");
        let address = listener
            .local_addr()
            .expect("client listener should have an address")
            .to_string();
        let app = crate::server::router_with_throttle(
            state.pool.clone(),
            state.events.clone(),
            state.account_tokens.clone(),
            state.sessions.clone(),
            state.account_login_throttle.clone(),
            crate::guard_poll::GuardRegistry::new(),
            crate::forwarded_log::test_forwarded_log(),
            state.diagnostic_requests.clone(),
        );
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("client test server should run");
        });
        address
    }

    /// An Owner Session as the browser holds it: the HttpOnly cookie token,
    /// the in-memory CSRF token from the response body, and the page origin.
    struct OwnerAuth {
        token: String,
        csrf: String,
        origin: String,
    }

    /// Read a successful setup/login response the way the browser does.
    async fn owner_auth(response: reqwest::Response, address: &str) -> OwnerAuth {
        assert_eq!(response.status(), StatusCode::OK);
        let token = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix(&format!("{OWNER_COOKIE}=")))
            .and_then(|value| value.split(';').next())
            .expect("response should set the owner cookie")
            .to_owned();
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("auth response should be JSON");
        let csrf = body["csrf_token"]
            .as_str()
            .expect("auth response should carry a csrf_token")
            .to_owned();
        OwnerAuth {
            token,
            csrf,
            origin: format!("http://{address}"),
        }
    }

    /// First-run PIN set; returns the Owner Session to attach to requests.
    async fn setup_and_token(client: &reqwest::Client, address: &str, pin: &str) -> OwnerAuth {
        let response = client
            .post(format!("http://{address}/web-admin/setup"))
            .json(&serde_json::json!({ "pin": pin }))
            .send()
            .await
            .expect("setup should complete");
        owner_auth(response, address).await
    }

    async fn login_owner(client: &reqwest::Client, address: &str, pin: &str) -> OwnerAuth {
        let response = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": pin }))
            .send()
            .await
            .expect("login should complete");
        owner_auth(response, address).await
    }

    /// Attach an Owner Session exactly as the Web Admin does: cookie, its own
    /// origin, and the CSRF header. Negative cases build headers by hand.
    trait OwnerRequest {
        fn owner(self, auth: &OwnerAuth) -> Self;
    }

    impl OwnerRequest for reqwest::RequestBuilder {
        fn owner(self, auth: &OwnerAuth) -> Self {
            self.header(reqwest::header::COOKIE, cookie(&auth.token))
                .header(reqwest::header::ORIGIN, &auth.origin)
                .header(CSRF_HEADER, &auth.csrf)
        }
    }

    fn cookie(token: &str) -> String {
        format!("{OWNER_COOKIE}={token}")
    }

    #[tokio::test]
    async fn notification_handlers_enforce_auth_filters_and_acknowledgement_idempotency() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, summary, created_at)
             VALUES
             ('notification-alert', 'tamper_kill', 'success', 'system', 'alert', CURRENT_TIMESTAMP),
             ('notification-notice', 'shop_opened', 'success', 'system', 'notice', CURRENT_TIMESTAMP),
             ('notification-silent', 'login_failed', 'denied', 'account', 'silent', CURRENT_TIMESTAMP),
             ('notification-old', 'tamper_kill', 'success', 'system', 'old', datetime(CURRENT_TIMESTAMP, '-31 days'))",
        )
        .execute(&pool)
        .await
        .expect("notification fixtures should insert");
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        for path in [
            "/web-admin/notifications",
            "/web-admin/notifications/unread-count",
        ] {
            let response = client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .expect("unauthenticated read should complete");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'web_admin_access_denied'",
            )
            .fetch_one(&pool)
            .await
            .expect("denied read audit count"),
            0
        );

        let token = setup_and_token(&client, &address, "notification-pin").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let invalid = auth(client.get(format!(
            "http://{address}/web-admin/notifications?limit=201"
        )))
        .send()
        .await
        .expect("invalid list should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let list = auth(client.get(format!(
            "http://{address}/web-admin/notifications?limit=1&severity=alert&unread=true"
        )))
        .send()
        .await
        .expect("notification list should complete");
        assert_eq!(list.status(), StatusCode::OK);
        let list = list.json::<serde_json::Value>().await.expect("list JSON");
        assert_eq!(list["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(list["items"][0]["id"], "notification-alert");
        assert_eq!(list["items"][0]["severity"], "alert");
        assert_eq!(list["items"][0]["read"], false);
        assert_eq!(list["has_more"], false);

        let seen = auth(client.post(format!("http://{address}/web-admin/notifications/seen")))
            .send()
            .await
            .expect("seen should complete");
        assert_eq!(seen.status(), StatusCode::OK);
        assert!(
            seen.json::<serde_json::Value>().await.expect("seen JSON")["last_seen_at"].is_string()
        );

        let denied_ack = client
            .post(format!(
                "http://{address}/web-admin/notifications/notification-alert/acknowledge"
            ))
            .send()
            .await
            .expect("unauthenticated acknowledgement should complete");
        assert_eq!(denied_ack.status(), StatusCode::UNAUTHORIZED);

        let first = auth(client.post(format!(
            "http://{address}/web-admin/notifications/notification-alert/acknowledge"
        )))
        .send()
        .await
        .expect("first acknowledgement should complete");
        assert_eq!(first.status(), StatusCode::OK);
        let first = first.json::<serde_json::Value>().await.expect("ack JSON");
        let second = auth(client.post(format!(
            "http://{address}/web-admin/notifications/notification-alert/acknowledge"
        )))
        .send()
        .await
        .expect("repeat acknowledgement should complete")
        .json::<serde_json::Value>()
        .await
        .expect("repeat ack JSON");
        assert_eq!(second, first);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'notification_acknowledged'",
            )
            .fetch_one(&pool)
            .await
            .expect("ack audit count"),
            1
        );
        for id in [
            "notification-notice",
            "notification-silent",
            "notification-old",
            "missing",
        ] {
            let response = auth(client.post(format!(
                "http://{address}/web-admin/notifications/{id}/acknowledge"
            )))
            .send()
            .await
            .expect("invalid acknowledgement should complete");
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn rejection_credit_handlers_require_owner_preview_then_credit_once() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, pc_id, summary, metadata_json, created_at)
             VALUES ('rejection-1', 'coin_batch_rejected', 'failure', 'system', 'pc-01',
                     'rejected', '{\"pulse_count\":11}', CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .expect("rejection fixture should insert");
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        let denied = client
            .get(format!(
                "http://{address}/web-admin/rejection-credits/rejections"
            ))
            .send()
            .await
            .expect("unauthenticated read should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "rejection-pin").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        let listed = auth(client.get(format!(
            "http://{address}/web-admin/rejection-credits/rejections?creditable_only=true"
        )))
        .send()
        .await
        .expect("rejection list should complete");
        assert_eq!(listed.status(), StatusCode::OK);
        let listed = listed.json::<serde_json::Value>().await.expect("list JSON");
        assert_eq!(listed["items"][0]["rejection_audit_id"], "rejection-1");
        assert_eq!(listed["items"][0]["target_kind"], "guest");
        assert_eq!(listed["items"][0]["remaining_coins"], 1);

        let denominations = auth(client.get(format!(
            "http://{address}/web-admin/rejection-credits/denominations"
        )))
        .send()
        .await
        .expect("denominations should complete")
        .json::<serde_json::Value>()
        .await
        .expect("denominations JSON");
        assert_eq!(denominations["denominations_cents"][0], 100);

        let credit_url = format!("http://{address}/web-admin/rejection-credits");
        let body = |dry_run: bool| {
            serde_json::json!({
                "rejection_audit_id": "rejection-1",
                "denomination_cents": 100,
                "coins": 1,
                "dry_run": dry_run,
            })
        };

        // The preview writes nothing, so the owner reads the server's own
        // numbers before committing.
        let preview = auth(client.post(&credit_url).json(&body(true)))
            .send()
            .await
            .expect("preview should complete");
        assert_eq!(preview.status(), StatusCode::OK);
        let preview = preview
            .json::<serde_json::Value>()
            .await
            .expect("preview JSON");
        assert_eq!(preview["seconds_added"], 300);
        assert!(preview["audit_event_id"].is_null());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rejection_credits")
                .fetch_one(&pool)
                .await
                .expect("credit count"),
            0
        );

        let credited = auth(client.post(&credit_url).json(&body(false)))
            .send()
            .await
            .expect("credit should complete");
        assert_eq!(credited.status(), StatusCode::OK);
        let credited = credited
            .json::<serde_json::Value>()
            .await
            .expect("credit JSON");
        assert!(credited["audit_event_id"].is_string());
        assert_eq!(credited["amount_cents"], 100);

        // The count cap refuses the second credit on a per-batch rejection.
        let repeat = auth(client.post(&credit_url).json(&body(false)))
            .send()
            .await
            .expect("repeat credit should complete");
        assert_eq!(repeat.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rejection_credits")
                .fetch_one(&pool)
                .await
                .expect("credit count"),
            1
        );
    }

    #[tokio::test]
    async fn bulk_acknowledge_handler_requires_owner_validates_batch_and_skips_ineligible() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, summary, created_at)
             VALUES
             ('bulk-alert-a', 'tamper_kill', 'success', 'system', 'alert a', CURRENT_TIMESTAMP),
             ('bulk-alert-b', 'pc_removed', 'success', 'system', 'alert b', CURRENT_TIMESTAMP),
             ('bulk-notice', 'shop_opened', 'success', 'system', 'notice', CURRENT_TIMESTAMP),
             ('bulk-old', 'tamper_kill', 'success', 'system', 'old', datetime(CURRENT_TIMESTAMP, '-31 days'))",
        )
        .execute(&pool)
        .await
        .expect("bulk notification fixtures should insert");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/notifications/acknowledge");

        let denied = client
            .post(&url)
            .json(&serde_json::json!({ "audit_ids": ["bulk-alert-a"] }))
            .send()
            .await
            .expect("unauthenticated bulk acknowledgement should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "bulk-ack-pin").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        for body in [
            serde_json::json!({ "audit_ids": [] }),
            serde_json::json!({ "audit_ids": vec!["bulk-alert-a"; 201] }),
        ] {
            let response = auth(client.post(&url))
                .json(&body)
                .send()
                .await
                .expect("invalid bulk acknowledgement should complete");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        let acknowledged = auth(client.post(&url))
            .json(&serde_json::json!({
                "audit_ids": [
                    "bulk-alert-a",
                    "bulk-alert-b",
                    "bulk-notice",
                    "bulk-old",
                    "missing",
                ]
            }))
            .send()
            .await
            .expect("bulk acknowledgement should complete");
        assert_eq!(acknowledged.status(), StatusCode::OK);
        let acknowledged = acknowledged
            .json::<serde_json::Value>()
            .await
            .expect("bulk ack JSON");
        assert_eq!(acknowledged["acknowledged_count"], 2);

        let repeat = auth(client.post(&url))
            .json(&serde_json::json!({ "audit_ids": ["bulk-alert-a", "bulk-alert-b"] }))
            .send()
            .await
            .expect("repeat bulk acknowledgement should complete")
            .json::<serde_json::Value>()
            .await
            .expect("repeat bulk ack JSON");
        assert_eq!(repeat["acknowledged_count"], 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'notification_acknowledged'",
            )
            .fetch_one(&pool)
            .await
            .expect("bulk ack audit count"),
            2
        );
    }

    #[tokio::test]
    async fn backup_handlers_require_owner_and_manage_catalog_files_and_audit() {
        let mut state = test_state().await;
        let pool = state.pool.clone();
        let backup_dir =
            std::env::temp_dir().join(format!("openpiso-web-backups-{}", Uuid::new_v4()));
        state.backups_dir = backup_dir.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("http://{address}/web-admin/backups"))
                .send()
                .await
                .expect("unauthenticated catalog request")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let token = setup_and_token(&client, &address, "backup-pin").await;
        let created = client
            .post(format!("http://{address}/web-admin/backups"))
            .owner(&token)
            .json(&serde_json::json!({ "backup_sets": ["Accounts"] }))
            .send()
            .await
            .expect("Backup creation should complete");
        assert_eq!(created.status(), StatusCode::OK);
        let created = created
            .json::<serde_json::Value>()
            .await
            .expect("created Backup response");
        let filename = created["filename"].as_str().expect("Backup filename");
        assert!(created["size_bytes"].as_u64().is_some());

        let catalog = client
            .get(format!("http://{address}/web-admin/backups"))
            .owner(&token)
            .send()
            .await
            .expect("catalog should load")
            .json::<Vec<serde_json::Value>>()
            .await
            .expect("catalog JSON");
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0]["filename"], filename);
        assert_eq!(catalog[0]["backup_sets"], serde_json::json!(["Accounts"]));

        let download = client
            .get(format!("http://{address}/web-admin/backups/{filename}"))
            .owner(&token)
            .send()
            .await
            .expect("Backup download should complete");
        assert_eq!(download.status(), StatusCode::OK);
        assert_eq!(
            download
                .headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok()),
            Some(format!("attachment; filename=\"{filename}\"").as_str())
        );
        assert!(!download.bytes().await.expect("download body").is_empty());

        let deleted = client
            .delete(format!("http://{address}/web-admin/backups/{filename}"))
            .owner(&token)
            .send()
            .await
            .expect("Backup deletion should complete");
        assert_eq!(deleted.status(), StatusCode::OK);
        let remaining = client
            .get(format!("http://{address}/web-admin/backups"))
            .owner(&token)
            .send()
            .await
            .expect("catalog after deletion")
            .json::<Vec<serde_json::Value>>()
            .await
            .expect("catalog after deletion JSON");
        assert!(remaining.is_empty());
        let audits: (i64, i64) = sqlx::query_as(
            "SELECT
                SUM(event_type = 'backup_created'),
                SUM(event_type = 'backup_deleted')
             FROM audit_log",
        )
        .fetch_one(&pool)
        .await
        .expect("Backup audit rows");
        assert_eq!(audits, (1, 1));
        let _ = tokio::fs::remove_dir_all(backup_dir).await;
    }

    #[tokio::test]
    async fn restore_owner_api_stages_cancels_and_requires_authentication() {
        let mut state = test_state().await;
        let pool = state.pool.clone();
        let backup_dir =
            std::env::temp_dir().join(format!("openpiso-web-restore-{}", Uuid::new_v4()));
        state.backups_dir = backup_dir.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("http://{address}/web-admin/restore"))
                .send()
                .await
                .expect("unauthenticated restore request")
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let token = setup_and_token(&client, &address, "restore-pin").await;
        let created_response = client
            .post(format!("http://{address}/web-admin/backups"))
            .owner(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("full Backup creation should complete");
        let created_status = created_response.status();
        let created_body = created_response.text().await.expect("Backup body");
        assert_eq!(created_status, StatusCode::OK, "{created_body}");
        let created: serde_json::Value = serde_json::from_str(&created_body).expect("Backup JSON");
        let filename = created["filename"].as_str().expect("Backup filename");

        let staged = client
            .post(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .json(&serde_json::json!({ "filename": filename }))
            .send()
            .await
            .expect("restore staging should complete");
        assert_eq!(staged.status(), StatusCode::OK);
        assert_eq!(
            staged
                .json::<serde_json::Value>()
                .await
                .expect("pending JSON")["filename"],
            filename
        );

        let pending = client
            .get(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .send()
            .await
            .expect("pending restore should load")
            .json::<serde_json::Value>()
            .await
            .expect("pending restore JSON");
        assert_eq!(pending["filename"], filename);

        let cancelled = client
            .delete(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .send()
            .await
            .expect("pending restore cancellation should complete");
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(
            cancelled
                .json::<serde_json::Value>()
                .await
                .expect("cancellation JSON"),
            serde_json::json!({ "cancelled": true })
        );
        let pending_after = client
            .get(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .send()
            .await
            .expect("pending restore after cancellation")
            .json::<serde_json::Value>()
            .await
            .expect("pending restore after cancellation JSON");
        assert!(pending_after.is_null(), "{pending_after}");
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'restore_requested'",
        )
        .fetch_one(&pool)
        .await
        .expect("restore audit row");
        assert_eq!(audit_count, 1);
        let _ = tokio::fs::remove_dir_all(backup_dir).await;
    }

    #[tokio::test]
    async fn browser_restore_upload_uses_same_gate_and_cleans_temporary_file() {
        let mut state = test_state().await;
        let backup_dir =
            std::env::temp_dir().join(format!("openpiso-web-upload-{}", Uuid::new_v4()));
        state.backups_dir = backup_dir.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "upload-pin").await;

        let created = client
            .post(format!("http://{address}/web-admin/backups"))
            .owner(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("full Backup creation should complete")
            .json::<serde_json::Value>()
            .await
            .expect("Backup JSON");
        let filename = created["filename"]
            .as_str()
            .expect("Backup filename")
            .to_owned();
        let sql = tokio::fs::read(backup_dir.join(&filename))
            .await
            .expect("Backup bytes");

        let uploaded = client
            .post(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .multipart(
                reqwest::multipart::Form::new().part(
                    "file",
                    reqwest::multipart::Part::bytes(sql)
                        .file_name(filename.clone())
                        .mime_str("application/sql")
                        .expect("SQL content type"),
                ),
            )
            .send()
            .await
            .expect("uploaded restore should complete");
        assert_eq!(uploaded.status(), StatusCode::OK);
        assert_eq!(
            uploaded
                .json::<serde_json::Value>()
                .await
                .expect("pending upload JSON")["filename"],
            filename
        );
        assert!(!has_upload_temp_file(&backup_dir).await);

        let _ = client
            .delete(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .send()
            .await
            .expect("pending upload cancellation");

        let refused = client
            .post(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .multipart(
                reqwest::multipart::Form::new().part(
                    "file",
                    reqwest::multipart::Part::bytes(b"not a Backup".to_vec())
                        .file_name("not-a-backup.sql")
                        .mime_str("application/sql")
                        .expect("SQL content type"),
                ),
            )
            .send()
            .await
            .expect("invalid upload should complete");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        let refusal = refused
            .json::<serde_json::Value>()
            .await
            .expect("header refusal JSON");
        assert_eq!(refusal["gate"], "header");
        assert!(!has_upload_temp_file(&backup_dir).await);

        let invalid_filename = client
            .post(format!("http://{address}/web-admin/restore"))
            .owner(&token)
            .multipart(
                reqwest::multipart::Form::new().part(
                    "file",
                    reqwest::multipart::Part::bytes(
                        tokio::fs::read(backup_dir.join(&filename))
                            .await
                            .expect("Backup bytes for filename gate"),
                    )
                    .file_name("backup.txt")
                    .mime_str("application/sql")
                    .expect("SQL content type"),
                ),
            )
            .send()
            .await
            .expect("invalid upload filename should complete");
        assert_eq!(invalid_filename.status(), StatusCode::BAD_REQUEST);
        let refusal = invalid_filename
            .json::<serde_json::Value>()
            .await
            .expect("filename refusal JSON");
        assert_eq!(refusal["gate"], "header");
        assert!(!has_upload_temp_file(&backup_dir).await);
        let _ = tokio::fs::remove_dir_all(backup_dir).await;
    }

    async fn has_upload_temp_file(directory: &std::path::Path) -> bool {
        let parent = directory.parent().expect("Backup directory parent");
        let mut entries = tokio::fs::read_dir(parent)
            .await
            .expect("temporary directory should be readable");
        while let Some(entry) = entries
            .next_entry()
            .await
            .expect("temporary directory entry")
        {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".openpiso-upload-")
            {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn backup_schedule_owner_api_round_trips_and_requires_authentication() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("http://{address}/web-admin/backup-schedule"))
                .send()
                .await
                .expect("unauthenticated schedule request")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let token = setup_and_token(&client, &address, "schedule-pin").await;
        let defaults = client
            .get(format!("http://{address}/web-admin/backup-schedule"))
            .owner(&token)
            .send()
            .await
            .expect("schedule should load")
            .json::<serde_json::Value>()
            .await
            .expect("schedule JSON");
        assert_eq!(defaults["enabled"], true);
        assert_eq!(defaults["frequency"], "daily");
        assert_eq!(defaults["minute_of_day"], 240);
        assert_eq!(defaults["backup_sets"].as_array().map(Vec::len), Some(7));

        let updated = client
            .put(format!("http://{address}/web-admin/backup-schedule"))
            .owner(&token)
            .json(&serde_json::json!({
                "enabled": true,
                "frequency": "monthly",
                "minute_of_day": 23,
                "weekday": 0,
                "day_of_month": 31,
                "backup_sets": ["Accounts", "Ledger"],
                "retention_count": 30,
                "mirror_directory": "D:/backup-mirror"
            }))
            .send()
            .await
            .expect("schedule update should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let updated = updated
            .json::<serde_json::Value>()
            .await
            .expect("updated schedule JSON");
        assert_eq!(updated["frequency"], "monthly");
        assert_eq!(updated["day_of_month"], 31);
        assert_eq!(
            updated["backup_sets"],
            serde_json::json!(["Accounts", "Ledger"])
        );
        assert_eq!(updated["mirror_directory"], "D:/backup-mirror");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'backup_settings_changed'",
        )
        .fetch_one(&pool)
        .await
        .expect("schedule audit row");
        assert_eq!(audit_count, 1);
    }

    #[tokio::test]
    async fn owner_credential_rotation_enforces_proof_validation_audit_and_session_revocation() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let caller_token = setup_and_token(&client, &address, "old-pin").await;
        let other_token = login_owner(&client, &address, "old-pin").await;

        for (body, expected) in [
            (
                serde_json::json!({ "current_pin": "old-pin", "new_pin": "new-pin" }),
                StatusCode::UNAUTHORIZED,
            ),
            (
                serde_json::json!({ "current_pin": "wrong", "new_pin": "new-pin" }),
                StatusCode::UNAUTHORIZED,
            ),
            (
                serde_json::json!({ "current_pin": "old-pin", "new_pin": "abc" }),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let mut request = client
                .post(format!("http://{address}/web-admin/credential"))
                .json(&body);
            if body["current_pin"] != "old-pin" || body["new_pin"] == "abc" {
                request = request.owner(&caller_token);
            }
            assert_eq!(
                request.send().await.expect("denied rotation").status(),
                expected
            );
        }

        let old_login = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": "old-pin" }))
            .send()
            .await
            .expect("old PIN remains valid");
        assert_eq!(old_login.status(), StatusCode::OK);

        let changed = client
            .post(format!("http://{address}/web-admin/credential"))
            .owner(&caller_token)
            .json(&serde_json::json!({ "current_pin": "old-pin", "new_pin": "new-pin" }))
            .send()
            .await
            .expect("valid rotation");
        assert_eq!(changed.status(), StatusCode::OK);

        for (token, authenticated) in [(&caller_token, true), (&other_token, false)] {
            let session = client
                .get(format!("http://{address}/web-admin/session"))
                .owner(token)
                .send()
                .await
                .expect("session lookup")
                .json::<serde_json::Value>()
                .await
                .expect("session JSON");
            assert_eq!(session["authenticated"], authenticated);
        }
        for (pin, expected) in [
            ("old-pin", StatusCode::UNAUTHORIZED),
            ("new-pin", StatusCode::OK),
        ] {
            assert_eq!(
                client
                    .post(format!("http://{address}/web-admin/login"))
                    .json(&serde_json::json!({ "pin": pin }))
                    .send()
                    .await
                    .expect("post-rotation login")
                    .status(),
                expected
            );
        }

        let success: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_credential_changed'",
        )
        .fetch_one(&pool)
        .await
        .expect("success audit count");
        let denied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_credential_change_denied'",
        )
        .fetch_one(&pool)
        .await
        .expect("denial audit count");
        assert_eq!(success, 1);
        assert_eq!(denied, 1);
    }

    #[tokio::test]
    async fn credential_rotation_cannot_leave_a_concurrent_old_pin_login_authenticated() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let caller_token = setup_and_token(&client, &address, "old-pin").await;

        let login = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": "old-pin" }))
            .send();
        let rotate = client
            .post(format!("http://{address}/web-admin/credential"))
            .owner(&caller_token)
            .json(&serde_json::json!({ "current_pin": "old-pin", "new_pin": "new-pin" }))
            .send();
        let (login, rotate) = tokio::join!(login, rotate);
        let login = login.expect("concurrent login");
        assert_eq!(
            rotate.expect("concurrent rotation").status(),
            StatusCode::OK
        );

        if login.status() == StatusCode::OK {
            let token = owner_auth(login, &address).await;
            let session = client
                .get(format!("http://{address}/web-admin/session"))
                .owner(&token)
                .send()
                .await
                .expect("session lookup")
                .json::<serde_json::Value>()
                .await
                .expect("session JSON");
            assert_eq!(session["authenticated"], false);
        } else {
            assert_eq!(login.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn owner_wheel_api_sets_clears_and_reports_true_odds() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        let initial = client
            .get(format!("http://{address}/web-admin/wheel"))
            .owner(&token)
            .send()
            .await
            .expect("wheel read")
            .json::<serde_json::Value>()
            .await
            .expect("wheel JSON");
        assert!(initial["bokya_rate_basis_points"].is_null());
        assert_eq!(
            initial["segments"]
                .as_array()
                .expect("segments")
                .iter()
                .map(|segment| segment["probability_basis_points"]
                    .as_i64()
                    .expect("server probability"))
                .sum::<i64>(),
            10_000
        );

        let mut input = initial;
        input["bokya_rate_basis_points"] = serde_json::json!(7_500);
        let set = client
            .put(format!("http://{address}/web-admin/wheel"))
            .owner(&token)
            .json(&input)
            .send()
            .await
            .expect("wheel update");
        assert_eq!(set.status(), StatusCode::OK);
        let set = set.json::<serde_json::Value>().await.expect("set JSON");
        assert_eq!(set["bokya_rate_basis_points"], 7_500);
        assert_eq!(
            set["segments"]
                .as_array()
                .expect("segments")
                .iter()
                .map(|segment| segment["probability_basis_points"].as_i64().unwrap_or(0))
                .sum::<i64>(),
            10_000
        );

        input["bokya_rate_basis_points"] = serde_json::json!(0);
        let zero = client
            .put(format!("http://{address}/web-admin/wheel"))
            .owner(&token)
            .json(&input)
            .send()
            .await
            .expect("zero-rate update");
        assert_eq!(zero.status(), StatusCode::OK);
        assert_eq!(
            zero.json::<serde_json::Value>().await.expect("zero JSON")["bokya_rate_basis_points"],
            0
        );

        input["bokya_rate_basis_points"] = serde_json::Value::Null;
        let cleared = client
            .put(format!("http://{address}/web-admin/wheel"))
            .owner(&token)
            .json(&input)
            .send()
            .await
            .expect("wheel clear");
        assert_eq!(cleared.status(), StatusCode::OK);
        assert!(cleared
            .json::<serde_json::Value>()
            .await
            .expect("clear JSON")["bokya_rate_basis_points"]
            .is_null());

        let version: i64 = sqlx::query_scalar("SELECT version FROM wheel_settings WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("version");
        input["bokya_rate_basis_points"] = serde_json::json!(10_001);
        let invalid = client
            .put(format!("http://{address}/web-admin/wheel"))
            .owner(&token)
            .json(&input)
            .send()
            .await
            .expect("invalid update");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert!(invalid
            .text()
            .await
            .expect("error body")
            .contains("between 0 and 10000"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT version FROM wheel_settings WHERE id = 1")
                .fetch_one(&pool)
                .await
                .expect("unchanged version"),
            version
        );

        input["bokya_rate_basis_points"] = serde_json::json!(5_000);
        input["segments"] = serde_json::json!([{
            "label": "Winner",
            "seconds": 1,
            "points_centi": 0,
            "free_spin": false
        }]);
        let contradictory = client
            .put(format!("http://{address}/web-admin/wheel"))
            .owner(&token)
            .json(&input)
            .send()
            .await
            .expect("contradictory update");
        assert_eq!(contradictory.status(), StatusCode::BAD_REQUEST);
        assert!(contradictory
            .text()
            .await
            .expect("error body")
            .contains("requires at least one Bokya segment"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT version FROM wheel_settings WHERE id = 1")
                .fetch_one(&pool)
                .await
                .expect("unchanged version"),
            version
        );
    }

    /// Send a GET with the request target untouched (no client-side `..`
    /// normalisation) and return the status code and body.
    async fn raw_get(address: &str, target: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("raw connect");
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("raw request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("raw response");
        let (head, body) = response.split_once("\r\n\r\n").expect("raw response head");
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("raw status code");
        (status, body.to_owned())
    }

    #[tokio::test]
    async fn static_handler_rejects_traversal_and_missing_assets() {
        // Write a known shell into a temp web-admin dir plus a secret OUTSIDE it.
        let base = std::env::temp_dir().join("op_web_admin_static_test");
        let dir = base.join("dist");
        tokio::fs::create_dir_all(&dir).await.expect("dist dir");
        tokio::fs::write(dir.join("index.html"), b"<!doctype html>shell")
            .await
            .expect("index.html");
        tokio::fs::write(base.join("secret.txt"), b"TOP SECRET")
            .await
            .expect("secret");

        let mut state = test_state().await;
        state.web_admin_dir = dir.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        // Root serves the shell as HTML.
        let root = client
            .get(format!("http://{address}/admin"))
            .send()
            .await
            .expect("root should complete");
        assert_eq!(root.status(), StatusCode::OK);
        assert_eq!(
            root.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            root.text().await.expect("root body"),
            "<!doctype html>shell"
        );

        // An extensionless in-memory route also serves the shell.
        let route = client
            .get(format!("http://{address}/admin/dashboard"))
            .send()
            .await
            .expect("route should complete");
        assert_eq!(route.status(), StatusCode::OK);
        assert_eq!(
            route.text().await.expect("route body"),
            "<!doctype html>shell"
        );

        let nested_route = client
            .get(format!("http://{address}/admin/settings/coin-device"))
            .send()
            .await
            .expect("nested route should complete");
        assert_eq!(nested_route.status(), StatusCode::OK);
        assert_eq!(
            nested_route
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            nested_route.text().await.expect("nested route body"),
            "<!doctype html>shell"
        );

        // A missing concrete asset is a real 404, not the shell.
        let missing = client
            .get(format!("http://{address}/admin/assets/missing.js"))
            .send()
            .await
            .expect("missing should complete");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        // Path traversal never escapes web_admin_dir: it falls back to the
        // shell and never returns the out-of-tree secret. HTTP clients
        // normalise `..` away, so send the raw request line (`--path-as-is`).
        for target in ["/admin/../secret.txt", "/admin/assets/../../secret.txt"] {
            let (status, body) = raw_get(&address, target).await;
            assert_eq!(status, 200, "{target}: {body}");
            assert_eq!(body, "<!doctype html>shell", "{target}");
        }

        tokio::fs::remove_dir_all(&base).await.ok();
    }

    #[tokio::test]
    async fn auth_gate_blocks_without_cookie_and_allows_after_pin() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();

        // No cookie -> 401.
        let denied = client
            .get(format!("http://{address}/web-admin/dashboard"))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        // First-run PIN set issues a cookie token that unlocks the route.
        let token = setup_and_token(&client, &address, "1234").await;
        let allowed = client
            .get(format!("http://{address}/web-admin/dashboard"))
            .owner(&token)
            .send()
            .await
            .expect("request should complete");
        assert_eq!(allowed.status(), StatusCode::OK);
        let body = allowed
            .json::<serde_json::Value>()
            .await
            .expect("dashboard should be JSON");
        assert!(body["pcs"].as_array().is_some_and(|pcs| pcs.len() == 4));
    }

    #[tokio::test]
    async fn auth_gate_rejects_unauthenticated_mutations_without_side_effects() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let response = reqwest::Client::new()
            .post(format!("http://{address}/web-admin/manual-adjustment"))
            .header(reqwest::header::COOKIE, "op_owner=invalid")
            .json(&serde_json::json!({
                "pc_id": "pc-01",
                "seconds_delta": 300,
                "reason": "must not apply"
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("remaining time query");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn login_throttles_repeated_wrong_pins_with_retry_after() {
        let state = test_state().await;
        // The in-memory DB is shared with the spawned server (single-connection
        // clone), so the test can assert on the audit log directly.
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        setup_and_token(&client, &address, "secret-pin").await;

        // The first THRESHOLD wrong PINs are rejected as plain 401s.
        for _ in 0..crate::login_throttle::FAILURE_THRESHOLD {
            let bad = client
                .post(format!("http://{address}/web-admin/login"))
                .json(&serde_json::json!({ "pin": "nope" }))
                .send()
                .await
                .expect("login should complete");
            assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
        }

        // The next attempt is throttled — 429 with a positive Retry-After — and
        // the correct PIN is refused too, proving the PIN is not verified while
        // locked out.
        let throttled = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": "secret-pin" }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after: i64 = throttled
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .expect("throttled response carries a Retry-After header");
        assert!(retry_after > 0, "Retry-After should be positive");

        // The lockout was audited exactly once (per-transition, not per blocked
        // request — the correct-PIN attempt above was also blocked but must not
        // have added a second event).
        let throttle_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_login_throttled'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count query");
        assert_eq!(throttle_events, 1);
    }

    #[tokio::test]
    async fn login_verifies_pin_and_logout_revokes_cookie() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        setup_and_token(&client, &address, "secret-pin").await;

        // Wrong PIN is rejected.
        let bad = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": "nope" }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        // Correct PIN issues a fresh, working token.
        let login = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": "secret-pin" }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(login.status(), StatusCode::OK);
        let token = owner_auth(login, &address).await;

        // The token works, then logout revokes it.
        let ok = client
            .get(format!("http://{address}/web-admin/today"))
            .owner(&token)
            .send()
            .await
            .expect("request should complete");
        assert_eq!(ok.status(), StatusCode::OK);

        let logout = client
            .post(format!("http://{address}/web-admin/logout"))
            .owner(&token)
            .send()
            .await
            .expect("logout should complete");
        assert_eq!(logout.status(), StatusCode::OK);

        let revoked = client
            .get(format!("http://{address}/web-admin/today"))
            .owner(&token)
            .send()
            .await
            .expect("request should complete");
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn session_endpoint_reports_configured_and_authenticated() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();

        let before = client
            .get(format!("http://{address}/web-admin/session"))
            .send()
            .await
            .expect("session should complete")
            .json::<serde_json::Value>()
            .await
            .expect("session JSON");
        assert_eq!(before["configured"], false);
        assert_eq!(before["authenticated"], false);

        let token = setup_and_token(&client, &address, "1234").await;
        let after = client
            .get(format!("http://{address}/web-admin/session"))
            .owner(&token)
            .send()
            .await
            .expect("session should complete")
            .json::<serde_json::Value>()
            .await
            .expect("session JSON");
        assert_eq!(after["configured"], true);
        assert_eq!(after["authenticated"], true);
    }

    /// Controllable monotonic time behind `OwnerTokens`; sessions only ever see
    /// it through the clock the store was built with.
    #[derive(Clone)]
    struct TestClock {
        start: std::time::Instant,
        elapsed_ms: Arc<std::sync::atomic::AtomicU64>,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                start: std::time::Instant::now(),
                elapsed_ms: Arc::default(),
            }
        }

        fn advance(&self, by: std::time::Duration) {
            let millis = u64::try_from(by.as_millis()).expect("test duration fits");
            self.elapsed_ms
                .fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
        }

        fn tokens(&self) -> OwnerTokens {
            let clock = self.clone();
            OwnerTokens::with_clock(Arc::new(move || {
                clock.start
                    + std::time::Duration::from_millis(
                        clock.elapsed_ms.load(std::sync::atomic::Ordering::SeqCst),
                    )
            }))
        }
    }

    const MINUTE: std::time::Duration = std::time::Duration::from_secs(60);

    async fn status_of(client: &reqwest::Client, url: &str, auth: &OwnerAuth) -> StatusCode {
        client
            .get(url)
            .owner(auth)
            .send()
            .await
            .expect("owner read should complete")
            .status()
    }

    #[tokio::test]
    async fn owner_login_sets_a_hardened_cookie_and_returns_csrf_never_the_token() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();

        let setup = client
            .post(format!("http://{address}/web-admin/setup"))
            .json(&serde_json::json!({ "pin": "1234" }))
            .send()
            .await
            .expect("setup should complete");
        let login_attempt = client
            .post(format!("http://{address}/web-admin/login"))
            .json(&serde_json::json!({ "pin": "1234" }))
            .send()
            .await
            .expect("login should complete");

        for response in [setup, login_attempt] {
            assert_eq!(response.status(), StatusCode::OK);
            let set_cookie = response
                .headers()
                .get(reqwest::header::SET_COOKIE)
                .and_then(|value| value.to_str().ok())
                .expect("response should set a cookie")
                .to_owned();
            assert!(set_cookie.starts_with("op_owner="));
            for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=43200"] {
                assert!(set_cookie.contains(attribute), "{set_cookie}");
            }
            assert!(!set_cookie.contains("Secure"), "{set_cookie}");

            let cookie_token = set_cookie
                .strip_prefix("op_owner=")
                .and_then(|rest| rest.split(';').next())
                .expect("cookie token")
                .to_owned();
            let body = response.text().await.expect("auth body");
            let json: serde_json::Value = serde_json::from_str(&body).expect("auth JSON");
            assert!(json["csrf_token"]
                .as_str()
                .is_some_and(|csrf| !csrf.is_empty()));
            assert!(json.get("token").is_none(), "{body}");
            assert!(!body.contains(&cookie_token), "{body}");
        }
    }

    #[tokio::test]
    async fn authenticated_mutations_require_exact_origin_and_the_sessions_own_csrf() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let mine = setup_and_token(&client, &address, "1234").await;
        let other = login_owner(&client, &address, "1234").await;
        let url = format!("http://{address}/web-admin/manual-adjustment");
        let body = serde_json::json!({
            "pc_id": "pc-01", "seconds_delta": 300, "reason": "csrf probe"
        });
        let send = |origin: Option<&str>, csrf: Option<&str>| {
            let mut request = client
                .post(&url)
                .header(reqwest::header::COOKIE, cookie(&mine.token))
                .json(&body);
            if let Some(origin) = origin {
                request = request.header(reqwest::header::ORIGIN, origin);
            }
            if let Some(csrf) = csrf {
                request = request.header(CSRF_HEADER, csrf);
            }
            request.send()
        };

        let own_origin = mine.origin.clone();
        for (origin, csrf, label) in [
            (None, None, "cookie only"),
            (None, Some(mine.csrf.as_str()), "csrf without origin"),
            (Some(own_origin.as_str()), None, "origin without csrf"),
            (
                Some("http://evil.example"),
                Some(mine.csrf.as_str()),
                "cross-origin",
            ),
            (Some("null"), Some(mine.csrf.as_str()), "opaque origin"),
            (Some(own_origin.as_str()), Some("wrong"), "wrong csrf"),
            (
                Some(own_origin.as_str()),
                Some(other.csrf.as_str()),
                "another session's csrf",
            ),
        ] {
            let response = send(origin, csrf).await.expect("denied mutation");
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
        }
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("remaining time query");
        assert_eq!(remaining, 0, "denied mutations must not change authority");
        let denied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'web_admin_access_denied'",
        )
        .fetch_one(&pool)
        .await
        .expect("denial audit count");
        assert_eq!(denied, 7);

        let allowed = send(Some(own_origin.as_str()), Some(mine.csrf.as_str()))
            .await
            .expect("authorized mutation");
        assert_eq!(allowed.status(), StatusCode::OK);

        // Safe reads need only the cookie.
        let read = client
            .get(format!("http://{address}/web-admin/dashboard"))
            .header(reqwest::header::COOKIE, cookie(&mine.token))
            .send()
            .await
            .expect("cookie-only read");
        assert_eq!(read.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn session_bootstrap_restores_csrf_for_a_reloaded_browser_without_the_token() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let login = setup_and_token(&client, &address, "1234").await;
        let session_url = format!("http://{address}/web-admin/session");

        // A reload keeps only the HttpOnly cookie.
        let reloaded = client
            .get(&session_url)
            .header(reqwest::header::COOKIE, cookie(&login.token))
            .send()
            .await
            .expect("bootstrap")
            .text()
            .await
            .expect("bootstrap body");
        let json: serde_json::Value = serde_json::from_str(&reloaded).expect("bootstrap JSON");
        assert_eq!(json["authenticated"], true);
        assert_eq!(json["csrf_token"], login.csrf.as_str());
        assert!(!reloaded.contains(&login.token), "{reloaded}");

        let anonymous: serde_json::Value = client
            .get(&session_url)
            .send()
            .await
            .expect("anonymous bootstrap")
            .json()
            .await
            .expect("anonymous JSON");
        assert_eq!(anonymous["authenticated"], false);
        assert!(anonymous.get("csrf_token").is_none());
    }

    #[tokio::test]
    async fn only_explicit_human_activity_keeps_an_owner_session_alive() {
        let clock = TestClock::new();
        let mut state = test_state().await;
        state.owner_tokens = clock.tokens();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let dashboard = format!("http://{address}/web-admin/dashboard");
        let session_url = format!("http://{address}/web-admin/session");
        let activity = format!("http://{address}/web-admin/activity");

        // Polling and bootstrap reads never refresh the idle timer.
        let polled = setup_and_token(&client, &address, "1234").await;
        clock.advance(29 * MINUTE);
        assert_eq!(
            status_of(&client, &dashboard, &polled).await,
            StatusCode::OK
        );
        assert_eq!(
            status_of(&client, &session_url, &polled).await,
            StatusCode::OK
        );
        clock.advance(2 * MINUTE);
        assert_eq!(
            status_of(&client, &dashboard, &polled).await,
            StatusCode::UNAUTHORIZED
        );
        let expired: serde_json::Value = client
            .get(&session_url)
            .owner(&polled)
            .send()
            .await
            .expect("expired bootstrap")
            .json()
            .await
            .expect("expired bootstrap JSON");
        assert_eq!(expired["authenticated"], false);
        assert!(expired.get("csrf_token").is_none());

        // The activity path needs the same CSRF protection and, unauthorized,
        // does not keep the session alive.
        let unprotected = login_owner(&client, &address, "1234").await;
        clock.advance(29 * MINUTE);
        let denied = client
            .post(&activity)
            .header(reqwest::header::COOKIE, cookie(&unprotected.token))
            .send()
            .await
            .expect("activity without csrf");
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        clock.advance(2 * MINUTE);
        assert_eq!(
            status_of(&client, &dashboard, &unprotected).await,
            StatusCode::UNAUTHORIZED
        );

        // Human activity resets idle, but never the 12 hour absolute limit.
        let active = login_owner(&client, &address, "1234").await;
        for _ in 0..24 {
            clock.advance(29 * MINUTE);
            let response = client
                .post(&activity)
                .owner(&active)
                .send()
                .await
                .expect("human activity");
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(
            status_of(&client, &dashboard, &active).await,
            StatusCode::OK
        );
        clock.advance(29 * MINUTE);
        assert_eq!(
            status_of(&client, &dashboard, &active).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn logout_is_gated_for_valid_sessions_and_idempotent_without_one() {
        let clock = TestClock::new();
        let mut state = test_state().await;
        state.owner_tokens = clock.tokens();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let logout = format!("http://{address}/web-admin/logout");
        let dashboard = format!("http://{address}/web-admin/dashboard");
        let auth = setup_and_token(&client, &address, "1234").await;

        // A valid session cannot be logged out cross-site.
        let forged = client
            .post(&logout)
            .header(reqwest::header::COOKIE, cookie(&auth.token))
            .send()
            .await
            .expect("forged logout");
        assert_eq!(forged.status(), StatusCode::FORBIDDEN);
        assert_eq!(status_of(&client, &dashboard, &auth).await, StatusCode::OK);

        let done = client
            .post(&logout)
            .owner(&auth)
            .send()
            .await
            .expect("logout");
        assert_eq!(done.status(), StatusCode::OK);
        let cleared = done
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("logout clears the cookie")
            .to_owned();
        for attribute in [
            "op_owner=;",
            "HttpOnly",
            "SameSite=Strict",
            "Path=/",
            "Max-Age=0",
        ] {
            assert!(cleared.contains(attribute), "{cleared}");
        }
        assert_eq!(
            status_of(&client, &dashboard, &auth).await,
            StatusCode::UNAUTHORIZED
        );

        // Revoked, unknown, absent, and expired sessions all clear idempotently.
        let expiring = login_owner(&client, &address, "1234").await;
        clock.advance(31 * MINUTE);
        for cookie_header in [
            Some(cookie(&auth.token)),
            Some(cookie(&expiring.token)),
            Some(cookie("unknown")),
            None,
        ] {
            let mut request = client.post(&logout);
            if let Some(value) = cookie_header {
                request = request.header(reqwest::header::COOKIE, value);
            }
            let response = request.send().await.expect("idempotent logout");
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response
                .headers()
                .get(reqwest::header::SET_COOKIE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("Max-Age=0")));
        }
    }

    #[tokio::test]
    async fn shop_rate_owner_api_is_atomic_append_only_and_changes_future_coin_events() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let client_address = spawn_client_server(&state).await;
        let owner_address = spawn(state).await;
        let client = reqwest::Client::new();
        let rate_url = format!("http://{owner_address}/web-admin/shop-rate");

        let denied_read = client
            .get(&rate_url)
            .send()
            .await
            .expect("read should complete");
        assert_eq!(denied_read.status(), StatusCode::UNAUTHORIZED);
        let denied_write = client
            .put(&rate_url)
            .json(&serde_json::json!({ "amount_cents": 500, "seconds": 600 }))
            .send()
            .await
            .expect("write should complete");
        assert_eq!(denied_write.status(), StatusCode::UNAUTHORIZED);

        let before_coin = client
            .post(format!("http://{client_address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "dev-simulator",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": null,
                "signature": null
            }))
            .send()
            .await
            .expect("coin before rate change should complete");
        assert_eq!(before_coin.status(), StatusCode::OK);
        assert_eq!(
            before_coin
                .json::<serde_json::Value>()
                .await
                .expect("coin JSON")["seconds_added"],
            300
        );

        let token = setup_and_token(&client, &owner_address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let original = auth(client.get(&rate_url))
            .send()
            .await
            .expect("initial read should complete")
            .json::<serde_json::Value>()
            .await
            .expect("initial rate JSON");
        assert_eq!(original["amount_cents"], 100);
        assert_eq!(original["seconds"], 300);

        for invalid in [
            serde_json::json!({ "amount_cents": 99, "seconds": 1 }),
            serde_json::json!({ "amount_cents": 100_100, "seconds": 1 }),
            serde_json::json!({ "amount_cents": 101, "seconds": 1 }),
            serde_json::json!({ "amount_cents": 100, "seconds": 0 }),
            serde_json::json!({ "amount_cents": 100, "seconds": -1 }),
            serde_json::json!({ "amount_cents": 100, "seconds": 86_401 }),
            serde_json::json!({ "amount_cents": 100.5, "seconds": 1 }),
            serde_json::json!({ "amount_cents": 100, "seconds": 1.5 }),
        ] {
            let response = auth(client.put(&rate_url))
                .json(&invalid)
                .send()
                .await
                .expect("invalid rate write should complete");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        let saved = auth(
            client
                .put(&rate_url)
                .json(&serde_json::json!({ "amount_cents": 500, "seconds": 601 })),
        )
        .send()
        .await
        .expect("rate write should complete");
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(
            saved
                .json::<serde_json::Value>()
                .await
                .expect("saved rate JSON"),
            serde_json::json!({ "amount_cents": 500, "seconds": 601 })
        );

        let read_back = auth(client.get(&rate_url))
            .send()
            .await
            .expect("read-back should complete")
            .json::<serde_json::Value>()
            .await
            .expect("read-back JSON");
        assert_eq!(
            read_back,
            serde_json::json!({ "amount_cents": 500, "seconds": 601 })
        );

        let rates = sqlx::query_as::<_, (String, String, i64, i64, i64)>(
            "SELECT id, name, amount_cents, seconds, active FROM rates ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("rates should be retained");
        assert_eq!(rates.len(), 2);
        let previous = rates
            .iter()
            .find(|rate| rate.0 == "rate-default")
            .expect("seed rate");
        assert_eq!((previous.2, previous.3, previous.4), (100, 300, 0));
        let current = rates.iter().find(|rate| rate.4 == 1).expect("active rate");
        assert_eq!((current.2, current.3), (500, 601));
        assert_eq!(current.1, "₱5 = 10 min 1 sec");

        let metadata = sqlx::query_scalar::<_, String>(
            "SELECT metadata_json FROM audit_log
             WHERE event_type = 'shop_rate_updated' ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .expect("rate audit should exist");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("audit metadata");
        assert_eq!(
            metadata["before"],
            serde_json::json!({ "amount_cents": 100, "seconds": 300 })
        );
        assert_eq!(
            metadata["after"],
            serde_json::json!({ "amount_cents": 500, "seconds": 601 })
        );

        let kiosk = db::kiosk_config(&pool).await.expect("kiosk config");
        assert_eq!((kiosk.rate_amount_cents, kiosk.rate_seconds), (500, 601));

        let after_coin = client
            .post(format!("http://{client_address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "dev-simulator",
                "pc_id": "pc-01",
                "amount_cents": 500,
                "pulse_count": 5,
                "timestamp": null,
                "signature": null
            }))
            .send()
            .await
            .expect("coin after rate change should complete");
        assert_eq!(after_coin.status(), StatusCode::OK);
        assert_eq!(
            after_coin
                .json::<serde_json::Value>()
                .await
                .expect("coin JSON")["seconds_added"],
            601
        );

        let old_coin_seconds: i64 = sqlx::query_scalar(
            "SELECT seconds_added FROM coin_transactions WHERE amount_cents = 100 LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .expect("old coin should be retained");
        assert_eq!(old_coin_seconds, 300);
    }

    #[tokio::test]
    async fn economy_settings_contract_uses_tiers_and_omits_legacy_pair() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        let updated = client
            .put(format!("http://{address}/web-admin/economy"))
            .owner(&token)
            .json(&serde_json::json!({ "earn_cents_per_point": 2_000 }))
            .send()
            .await
            .expect("update should complete");
        assert_eq!(updated.status(), StatusCode::OK);

        let read = client
            .get(format!("http://{address}/web-admin/economy"))
            .owner(&token)
            .send()
            .await
            .expect("get should complete")
            .json::<serde_json::Value>()
            .await
            .expect("economy JSON");
        assert_eq!(read["earn_cents_per_point"], 2_000);
        assert_eq!(read["rate_amount_cents"], 100);
        assert_eq!(read["rate_seconds"], 300);
        assert_eq!(read["effective_bonus_basis_points"], 2_400);
        assert!(read.get("redeem_points").is_none());
        assert!(read.get("redeem_seconds").is_none());

        let legacy_pair = client
            .put(format!("http://{address}/web-admin/economy"))
            .owner(&token)
            .json(&serde_json::json!({
                "earn_cents_per_point": 2_000,
                "redeem_points": 5,
                "redeem_seconds": 7_200
            }))
            .send()
            .await
            .expect("legacy update should complete");
        assert_eq!(legacy_pair.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn redemption_tiers_round_trip_sorted_and_reject_fractional_points() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let url = format!("http://{address}/web-admin/redemption-tiers");

        let updated = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({
                "tiers": [
                    { "points": 20, "minutes": 120 },
                    { "points": 5, "minutes": 1 }
                ]
            }))
            .send()
            .await
            .expect("update should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let tiers = updated
            .json::<serde_json::Value>()
            .await
            .expect("tiers JSON");
        assert_eq!(tiers[0]["points"], 5);
        assert_eq!(tiers[0]["seconds"], 60);
        assert!(tiers[0]["id"].is_number());
        assert_eq!(tiers[1]["points"], 20);
        assert_eq!(tiers[1]["seconds"], 7_200);
        assert!(tiers[1]["id"].is_number());

        let fractional = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({
                "tiers": [{ "points": 5.5, "minutes": 1 }]
            }))
            .send()
            .await
            .expect("invalid update should complete");
        assert_eq!(fractional.status(), StatusCode::BAD_REQUEST);
        let fractional_minutes = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({
                "tiers": [{ "points": 5, "minutes": 1.5 }]
            }))
            .send()
            .await
            .expect("invalid update should complete");
        assert_eq!(fractional_minutes.status(), StatusCode::BAD_REQUEST);

        let unchanged = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("get should complete")
            .json::<serde_json::Value>()
            .await
            .expect("tiers JSON");
        assert_eq!(unchanged, tiers);
    }

    #[tokio::test]
    async fn coin_promo_round_trip_computes_sorted_combined_safety_and_audits() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let url = format!("http://{address}/web-admin/coin-promo");

        let denied = client.get(&url).send().await.expect("unauthenticated read");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let denied_write = client
            .put(&url)
            .json(&serde_json::json!({ "enabled": false, "tiers": [] }))
            .send()
            .await
            .expect("unauthenticated write");
        assert_eq!(denied_write.status(), StatusCode::UNAUTHORIZED);

        let response = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({
                "enabled": true,
                "tiers": [
                    { "amount_cents": 1_000, "bonus_minutes": 5 },
                    { "amount_cents": 500, "bonus_minutes": 10 }
                ]
            }))
            .send()
            .await
            .expect("Coin Promo update");
        assert_eq!(response.status(), StatusCode::OK);
        let saved = response
            .json::<serde_json::Value>()
            .await
            .expect("Coin Promo JSON");
        assert_eq!(saved["enabled"], true);
        assert_eq!(saved["tiers"][0]["amount_cents"], 500);
        assert_eq!(saved["tiers"][0]["bonus_seconds"], 600);
        assert_eq!(saved["tiers"][0]["effective_bonus_basis_points"], 4_000);
        assert_eq!(saved["tiers"][1]["amount_cents"], 1_000);
        assert_eq!(saved["tiers"][1]["effective_bonus_basis_points"], 1_000);
        assert_eq!(saved["account_worst_case_basis_points"], 4_960);
        assert_eq!(saved["unsafe_bonus_threshold_basis_points"], 2_000);
        assert_eq!(saved["unsafe_bonus"], true);

        let read = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("Coin Promo read")
            .json::<serde_json::Value>()
            .await
            .expect("Coin Promo JSON");
        assert_eq!(read, saved);
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'coin_promo_settings_updated'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(audits, 1);
    }

    #[tokio::test]
    async fn coin_promo_refuses_every_invalid_write_atomically_but_preview_does_not_save() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let url = format!("http://{address}/web-admin/coin-promo");
        let original = serde_json::json!({
            "enabled": true,
            "tiers": [{ "amount_cents": 500, "bonus_minutes": 1 }]
        });
        assert_eq!(
            auth(client.put(&url))
                .json(&original)
                .send()
                .await
                .expect("seed")
                .status(),
            StatusCode::OK
        );

        let invalid = [
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": 100, "bonus_minutes": 1 },
                { "amount_cents": 100, "bonus_minutes": 2 }
            ]}),
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": 0, "bonus_minutes": 1 }
            ]}),
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": -1, "bonus_minutes": 1 }
            ]}),
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": 100, "bonus_minutes": 0 }
            ]}),
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": 100, "bonus_minutes": -1 }
            ]}),
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": 100, "bonus_minutes": i64::MAX }
            ]}),
            serde_json::json!({ "enabled": false, "tiers": [
                { "amount_cents": 1, "bonus_minutes": i64::MAX / 60 }
            ]}),
        ];
        for body in invalid {
            let refused = auth(client.put(&url))
                .json(&body)
                .send()
                .await
                .expect("invalid update");
            assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        }

        let before_preview = auth(client.get(&url))
            .send()
            .await
            .expect("read before preview")
            .json::<serde_json::Value>()
            .await
            .expect("JSON");
        let preview = auth(client.post(format!("{url}/preview")))
            .json(&serde_json::json!({
                "enabled": false,
                "tiers": [{ "amount_cents": 1_000, "bonus_minutes": 10 }]
            }))
            .send()
            .await
            .expect("preview");
        assert_eq!(preview.status(), StatusCode::OK);
        let after_preview = auth(client.get(&url))
            .send()
            .await
            .expect("read after preview")
            .json::<serde_json::Value>()
            .await
            .expect("JSON");
        assert_eq!(after_preview, before_preview);
        assert_eq!(after_preview["enabled"], true);
        assert_eq!(after_preview["tiers"][0]["amount_cents"], 500);
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'coin_promo_settings_updated'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(audits, 1);
    }

    #[tokio::test]
    async fn shop_hours_owner_api_round_trips_the_complete_schedule_atomically() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let url = format!("http://{address}/web-admin/shop-hours");

        let denied = client.get(&url).send().await.expect("GET should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let days = (0..7)
            .map(|weekday| {
                serde_json::json!({
                    "weekday": weekday,
                    "mode": if weekday == 5 { "closed_all_day" } else { "open_period" },
                    "open_minute": if weekday == 5 { serde_json::Value::Null } else { serde_json::json!(1200) },
                    "close_minute": if weekday == 5 { serde_json::Value::Null } else { serde_json::json!(120) },
                })
            })
            .collect::<Vec<_>>();
        let saved = auth(client.put(&url))
            .json(&serde_json::json!({
                "enabled": true,
                "last_call_lead_minutes": 30,
                "days": days,
            }))
            .send()
            .await
            .expect("PUT should complete");
        assert_eq!(saved.status(), StatusCode::OK);
        let saved = saved
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON");
        assert_eq!(saved["enabled"], true);
        assert_eq!(saved["last_call_lead_minutes"], 30);
        assert_eq!(saved["days"].as_array().map(Vec::len), Some(7));
        assert_eq!(saved["days"][5]["mode"], "closed_all_day");
        assert_eq!(saved["override"], serde_json::Value::Null);

        let read = auth(client.get(&url))
            .send()
            .await
            .expect("GET should complete")
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON");
        assert_eq!(read, saved);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_hours_settings_updated'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(audit_count, 1);
    }

    #[tokio::test]
    async fn shop_override_owner_api_round_trips_clears_and_preserves_on_rejected_expiry() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let url = format!("http://{address}/web-admin/shop-override");
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();

        let denied = client
            .post(&url)
            .json(&serde_json::json!({
                "mode": "force_closed",
                "expires_at": future,
            }))
            .send()
            .await
            .expect("unauthenticated write should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let set = auth(client.post(&url))
            .json(&serde_json::json!({
                "mode": "force_closed",
                "expires_at": future,
            }))
            .send()
            .await
            .expect("set should complete");
        assert_eq!(set.status(), StatusCode::OK);
        let set_body = set.json::<serde_json::Value>().await.expect("set JSON");
        assert_eq!(set_body["override"]["mode"], "force_closed");
        assert_eq!(set_body["status"]["reason"], "override");
        assert_eq!(set_body["status"]["open"], false);

        let read = auth(client.get(format!("http://{address}/web-admin/shop-hours")))
            .send()
            .await
            .expect("read should complete")
            .json::<serde_json::Value>()
            .await
            .expect("read JSON");
        assert_eq!(read["override"]["mode"], "force_closed");

        let rejected = auth(client.post(&url))
            .json(&serde_json::json!({
                "mode": "force_open",
                "expires_at": (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            }))
            .send()
            .await
            .expect("past expiry should complete");
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        let preserved = auth(client.get(format!("http://{address}/web-admin/shop-hours")))
            .send()
            .await
            .expect("preserved read should complete")
            .json::<serde_json::Value>()
            .await
            .expect("preserved JSON");
        assert_eq!(preserved["override"]["mode"], "force_closed");

        let cleared = auth(client.delete(&url))
            .send()
            .await
            .expect("clear should complete");
        assert_eq!(cleared.status(), StatusCode::OK);
        let cleared_body = cleared
            .json::<serde_json::Value>()
            .await
            .expect("clear JSON");
        assert_eq!(cleared_body["override"], serde_json::Value::Null);
        assert_eq!(cleared_body["status"]["reason"], "schedule");

        let audit_rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT event_type, actor, metadata_json FROM audit_log
             WHERE event_type IN ('shop_override_set', 'shop_override_cleared')
             ORDER BY created_at",
        )
        .fetch_all(&pool)
        .await
        .expect("override audit rows should exist");
        assert_eq!(audit_rows.len(), 2);
        let set_audit = audit_rows
            .iter()
            .find(|row| row.0 == "shop_override_set")
            .expect("set audit row");
        assert_eq!(set_audit.1, "admin");
        let metadata: serde_json::Value =
            serde_json::from_str(&set_audit.2).expect("set metadata JSON");
        assert_eq!(metadata["mode"], "force_closed");
        assert!(metadata["expires_at"].is_string());
        let clear_audit = audit_rows
            .iter()
            .find(|row| row.0 == "shop_override_cleared")
            .expect("clear audit row");
        assert_eq!(clear_audit.1, "admin");
    }

    #[tokio::test]
    async fn representative_handler_per_group_succeeds_with_owner_cookie() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        // Vouchers group.
        let voucher = auth(client.post(format!("http://{address}/web-admin/vouchers")))
            .json(&serde_json::json!({
                "seconds_value": 300,
                "points_value_centi": 0,
                "sold_amount_cents": 0,
                "expires_at": null
            }))
            .send()
            .await
            .expect("voucher create should complete");
        assert_eq!(voucher.status(), StatusCode::OK);
        let voucher = voucher
            .json::<serde_json::Value>()
            .await
            .expect("voucher JSON");
        assert_eq!(voucher["kind"], "promo");
        let voided = auth(client.post(format!(
            "http://{address}/web-admin/vouchers/{}/void",
            voucher["id"].as_str().expect("voucher id")
        )))
        .send()
        .await
        .expect("voucher void should complete");
        assert_eq!(voided.status(), StatusCode::OK);

        // Accounts group.
        let accounts = auth(client.get(format!("http://{address}/web-admin/accounts")))
            .send()
            .await
            .expect("accounts list should complete");
        assert_eq!(accounts.status(), StatusCode::OK);

        // Reports group.
        let report = auth(client.post(format!("http://{address}/web-admin/reports/summary")))
            .json(&serde_json::json!({ "period": "day" }))
            .send()
            .await
            .expect("report summary should complete");
        assert_eq!(report.status(), StatusCode::OK);

        let sales_overview =
            auth(client.get(format!("http://{address}/web-admin/reports/overview")))
                .send()
                .await
                .expect("sales report overview should complete");
        assert_eq!(sales_overview.status(), StatusCode::OK);
        let sales_overview = sales_overview
            .json::<serde_json::Value>()
            .await
            .expect("sales report overview JSON");
        assert!(sales_overview["daily"]["from_date"].is_string());
        assert_eq!(sales_overview["pcs"].as_array().map(Vec::len), Some(4));

        // Audit group.
        let audit = auth(client.post(format!("http://{address}/web-admin/audit/search")))
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("audit search should complete");
        assert_eq!(audit.status(), StatusCode::OK);

        // Manual-adjustment group (money/time mutation).
        let adjustment = auth(client.post(format!("http://{address}/web-admin/manual-adjustment")))
            .json(&serde_json::json!({
                "pc_id": "pc-01",
                "seconds_delta": 300,
                "reason": "http smoke"
            }))
            .send()
            .await
            .expect("manual adjustment should complete");
        assert_eq!(adjustment.status(), StatusCode::OK);
        let body = adjustment
            .json::<serde_json::Value>()
            .await
            .expect("adjustment JSON");
        assert_eq!(body["seconds_delta"], 300);
    }

    #[tokio::test]
    async fn account_list_derives_last_login_and_preserves_it_for_deleted_accounts() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let account = accounts::create(
            &pool,
            "last-login-user",
            "login-password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("account should be created");
        let never_logged_in = accounts::create(
            &pool,
            "never-logged-in",
            "login-password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("never-logged-in account should be created");
        accounts::authenticate(&pool, "last-login-user", "login-password", None)
            .await
            .expect("successful login should be audited");
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, account_id, summary, created_at)
             VALUES (?, 'login_succeeded', 'success', 'account', ?, ?, '2099-01-01T00:00:00Z')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&account.id)
        .bind("Customer authenticated")
        .execute(&pool)
        .await
        .expect("newer successful login should be inserted");

        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let unauthorized = client
            .get(format!("http://{address}/web-admin/accounts"))
            .send()
            .await
            .expect("unauthenticated list should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "last-login-pin").await;
        let list = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("account list should complete");
        assert_eq!(list.status(), StatusCode::OK);
        let list_json = list
            .json::<serde_json::Value>()
            .await
            .expect("account list should be JSON");
        let account_json = list_json
            .as_array()
            .expect("account list should be an array")
            .iter()
            .find(|row| row["id"] == account.id)
            .cloned()
            .expect("listed account");
        assert_eq!(account_json["last_login_at"], "2099-01-01T00:00:00Z");
        assert!(matches!(
            accounts::authenticate(&pool, "last-login-user", "wrong-password", None).await,
            Err(AppError::Unauthorized(_))
        ));
        let never_json = list_json
            .as_array()
            .expect("account list should be an array")
            .iter()
            .find(|row| row["id"] == never_logged_in.id)
            .expect("never-logged-in account");
        assert!(never_json["last_login_at"].is_null());

        accounts::soft_delete(&pool, &account.id)
            .await
            .expect("account should be deleted");
        let deleted = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("deleted account list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("deleted account list should be JSON");
        let deleted_json = deleted
            .as_array()
            .expect("deleted account list should be an array")
            .iter()
            .find(|row| row["id"] == account.id)
            .expect("deleted account should remain listed by the server");
        assert_eq!(deleted_json["status"], "deleted");
        assert_eq!(deleted_json["last_login_at"], "2099-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn bulk_account_delete_is_atomic_and_revokes_live_access() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let tokens = state.account_tokens.clone();
        let sessions = state.sessions.clone();
        let mut pc_updates = state.events.subscribe();
        let first = accounts::create(
            &pool,
            "bulk-http-first",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("first account should be created");
        let second = accounts::create(
            &pool,
            "bulk-http-second",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("second account should be created");
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 600, points_balance_centi = 350 WHERE id = ?",
        )
        .bind(&first.id)
        .execute(&pool)
        .await
        .expect("first balance should be set");
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 90, points_balance_centi = 7 WHERE id = ?",
        )
        .bind(&second.id)
        .execute(&pool)
        .await
        .expect("second balance should be set");
        let (session, token) = sessions
            .login(&pool, &tokens, &first, "pc-01")
            .await
            .expect("account session should start");
        assert_eq!(session.account_id, first.id);

        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let owner_token = setup_and_token(&client, &address, "bulk-delete-pin").await;
        let response = client
            .post(format!("http://{address}/web-admin/accounts/delete"))
            .owner(&owner_token)
            .json(&serde_json::json!({ "account_ids": [first.id, second.id] }))
            .send()
            .await
            .expect("bulk deletion should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let deletion = response
            .json::<serde_json::Value>()
            .await
            .expect("deletion response");
        assert_eq!(deletion["deleted_count"], 2);
        assert!(tokens.get(&token).await.is_none());
        assert!(sessions.active_pc_ids().await.is_empty());

        for account_id in [&first.id, &second.id] {
            let state: (String, i64, i64) = sqlx::query_as(
                "SELECT status, remaining_seconds, points_balance_centi
                 FROM accounts WHERE id = ?",
            )
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("deleted account state");
            assert_eq!(state, ("deleted".to_owned(), 0, 0));
        }
        let deleted_audits: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT account_id, seconds_delta, points_delta_centi
             FROM audit_log WHERE event_type = 'account_deleted' ORDER BY account_id",
        )
        .fetch_all(&pool)
        .await
        .expect("deletion audit rows");
        assert_eq!(deleted_audits.len(), 2);
        assert!(deleted_audits.contains(&(first.id.clone(), Some(-600), Some(-350))));
        assert!(deleted_audits.contains(&(second.id.clone(), Some(-90), Some(-7))));
        let published = pc_updates.recv().await.expect("affected PC update");
        assert_eq!(published.id, "pc-01");
        assert!(published.active_session.is_none());

        let stale = accounts::create(
            &pool,
            "bulk-http-stale",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("stale test account should be created");
        sqlx::query("UPDATE accounts SET remaining_seconds = 120 WHERE id = ?")
            .bind(&stale.id)
            .execute(&pool)
            .await
            .expect("stale balance should be set");
        let failed = client
            .post(format!("http://{address}/web-admin/accounts/delete"))
            .owner(&owner_token)
            .json(&serde_json::json!({ "account_ids": [stale.id, "missing-account"] }))
            .send()
            .await
            .expect("stale deletion should complete");
        assert_eq!(failed.status(), StatusCode::NOT_FOUND);
        let stale_state: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM accounts WHERE id = ?")
                .bind(&stale.id)
                .fetch_one(&pool)
                .await
                .expect("stale account should remain");
        assert_eq!(stale_state, ("active".to_owned(), 120));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_delete_failed'",
            )
            .fetch_one(&pool)
            .await
            .expect("failure audit should exist"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_deleted'",
            )
            .fetch_one(&pool)
            .await
            .expect("successful deletion audits should remain unchanged"),
            2
        );
    }

    #[tokio::test]
    async fn account_import_preview_reports_rows_and_does_not_write() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let import_url = format!("http://{address}/web-admin/accounts/import");
        let password_hash = bcrypt::hash("import-password", 4).expect("bcrypt hash");
        let existing = accounts::create(
            &pool,
            "ExistingUser",
            "existing-password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("existing account should be created");
        let before = client
            .get(format!("http://{address}/web-admin/accounts"))
            .header(reqwest::header::COOKIE, cookie("missing"))
            .send()
            .await
            .expect("unauthenticated list should complete");
        assert_eq!(before.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "import-pin").await;
        let before = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("account list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("account list JSON");

        let payload = serde_json::json!({
            "dry_run": true,
            "accounts": {
                "CleanUser": {
                    "id": "clean-id",
                    "password": password_hash,
                    "credits": 2.5,
                    "points": 1.25,
                    "enabled": true,
                    "last": "2026-01-01T00:00:00"
                },
                "DustUser": {
                    "id": "dust-id",
                    "password": "b'$2b$12$.....................................................'",
                    "credits": 7.9e-10,
                    "points": 7.9e-10,
                    "enabled": true,
                    "last": null
                },
                "DisabledUser": {
                    "id": "disabled-id",
                    "password": "not-a-bcrypt-hash",
                    "credits": 10,
                    "points": 2,
                    "enabled": false,
                    "last": null
                },
                "BadPassword": {
                    "id": "bad-password-id",
                    "password": "not-a-bcrypt-hash",
                    "credits": 1,
                    "points": 1,
                    "enabled": true,
                    "last": null
                },
                "MissingPassword": {
                    "id": "missing-password-id",
                    "credits": 1,
                    "points": 1,
                    "enabled": true,
                    "last": null
                },
                "Bad/User": {
                    "id": "bad-username-id",
                    "password": "b'$2b$12$.....................................................'",
                    "credits": 1,
                    "points": 1,
                    "enabled": true,
                    "last": null
                },
                "": {
                    "id": "empty-username-id",
                    "password": "b'$2b$12$.....................................................'",
                    "credits": 1,
                    "points": 1,
                    "enabled": true,
                    "last": null
                },
                "ExistingUser": {
                    "id": "existing-id",
                    "password": "b'$2b$12$.....................................................'",
                    "credits": 3,
                    "points": 3,
                    "enabled": true,
                    "last": null
                },
                "Spaced User": {
                    "id": "spaced-id",
                    "password": "b'$2b$12$.....................................................'",
                    "credits": 1.5,
                    "points": 2.25,
                    "enabled": true,
                    "last": null
                }
            }
        });

        let unauthorized = client
            .post(&import_url)
            .json(&payload)
            .send()
            .await
            .expect("unauthenticated import should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let preview = client
            .post(&import_url)
            .owner(&token)
            .json(&payload)
            .send()
            .await
            .expect("import preview should complete");
        assert_eq!(preview.status(), StatusCode::OK);
        let report = preview
            .json::<serde_json::Value>()
            .await
            .expect("import report JSON");
        assert_eq!(report["totals"]["created"], 3);
        assert_eq!(report["totals"]["skipped"], 6);
        assert_eq!(report["totals"]["total_seconds"], 240);
        assert_eq!(report["totals"]["total_points_centi"], 350);

        let rows = report["rows"].as_array().expect("report rows");
        let row_for = |username: &str| {
            rows.iter()
                .find(|row| row["original_username"] == username)
                .expect("report row")
        };
        let spaced = row_for("Spaced User");
        assert_eq!(spaced["outcome"], "created");
        assert_eq!(spaced["final_username"], "Spaced_User");
        assert_eq!(spaced["remaining_seconds"], 90);
        assert_eq!(spaced["points_balance_centi"], 225);
        assert_eq!(row_for("DustUser")["remaining_seconds"], 0);
        assert_eq!(row_for("DustUser")["points_balance_centi"], 0);
        assert_eq!(row_for("DisabledUser")["skip_reason"], "disabled_at_source");
        assert_eq!(
            row_for("BadPassword")["skip_reason"],
            "invalid_password_hash"
        );
        assert_eq!(
            row_for("MissingPassword")["skip_reason"],
            "invalid_password_hash"
        );
        assert_eq!(row_for("Bad/User")["skip_reason"], "invalid_username");
        assert_eq!(row_for("")["skip_reason"], "invalid_username");
        assert_eq!(
            row_for("ExistingUser")["skip_reason"],
            "existing_account_conflict"
        );

        let after = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("account list after preview should complete")
            .json::<serde_json::Value>()
            .await
            .expect("account list after preview JSON");
        assert_eq!(after, before);
        assert_eq!(
            after
                .as_array()
                .expect("account list array")
                .iter()
                .filter(|account| account["id"] == existing.id)
                .count(),
            1
        );

        let commit = client
            .post(&import_url)
            .owner(&token)
            .json(&serde_json::json!({
                "dry_run": false,
                "accounts": payload["accounts"]
            }))
            .send()
            .await
            .expect("commit should complete");
        assert_eq!(commit.status(), StatusCode::OK);
        let committed_report = commit
            .json::<serde_json::Value>()
            .await
            .expect("commit report JSON");
        assert_eq!(committed_report, report);

        let imported_accounts = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("account list after commit should complete")
            .json::<serde_json::Value>()
            .await
            .expect("account list after commit JSON");
        let account_list = imported_accounts.as_array().expect("account list array");
        let imported = account_list
            .iter()
            .find(|account| account["username"] == "Spaced_User")
            .expect("imported account");
        assert_eq!(imported["imported_from"], "martec");
        assert!(imported["imported_at"].is_string());
        assert_eq!(imported["legacy_not_logged_in"], true);
        assert_ne!(imported["created_at"], "2026-01-01T00:00:00");

        let native = account_list
            .iter()
            .find(|account| account["id"] == existing.id)
            .expect("native account");
        assert_eq!(native["imported_from"], serde_json::Value::Null);
        assert_eq!(native["imported_at"], serde_json::Value::Null);
        assert_eq!(native["legacy_not_logged_in"], false);

        let audit_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                 SUM(event_type = 'account_created'),
                 SUM(event_type = 'account_import_completed')
             FROM audit_log
             WHERE metadata_json LIKE '%martec%'",
        )
        .fetch_one(&pool)
        .await
        .expect("import audit counts");
        assert_eq!(audit_counts, (3, 1));
        let summary_metadata: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log
             WHERE event_type = 'account_import_completed'",
        )
        .fetch_one(&pool)
        .await
        .expect("import summary metadata");
        let summary_metadata: serde_json::Value =
            serde_json::from_str(&summary_metadata).expect("summary metadata JSON");
        assert_eq!(summary_metadata["created_count"], 3);
        assert_eq!(summary_metadata["skipped_count"], 6);
        assert_eq!(summary_metadata["total_seconds"], 240);
        assert_eq!(summary_metadata["total_points_centi"], 350);

        let imported_audit: (i64, i64, i64, String) = sqlx::query_as(
            "SELECT seconds_delta, points_delta_centi, cash_flag, metadata_json
             FROM audit_log
             WHERE event_type = 'account_created'
               AND account_id = ?",
        )
        .bind(imported["id"].as_str().expect("imported account id"))
        .fetch_one(&pool)
        .await
        .expect("imported account audit");
        assert_eq!(
            (imported_audit.0, imported_audit.1, imported_audit.2),
            (90, 225, 0)
        );
        let imported_metadata: serde_json::Value =
            serde_json::from_str(&imported_audit.3).expect("import metadata JSON");
        assert_eq!(imported_metadata["source_system"], "martec");
        assert_eq!(imported_metadata["martec_account_id"], "spaced-id");
        assert_eq!(
            imported_metadata["martec_last_seen"],
            serde_json::Value::Null
        );
        assert_eq!(imported_metadata["original_username"], "Spaced User");

        let rerun_before = imported_accounts.clone();
        let rerun = client
            .post(&import_url)
            .owner(&token)
            .json(&serde_json::json!({
                "dry_run": false,
                "accounts": payload["accounts"]
            }))
            .send()
            .await
            .expect("rerun should complete");
        assert_eq!(rerun.status(), StatusCode::OK);
        let rerun_report = rerun
            .json::<serde_json::Value>()
            .await
            .expect("rerun report JSON");
        assert_eq!(rerun_report["totals"]["created"], 0);
        assert_eq!(rerun_report["totals"]["skipped"], 9);
        assert_eq!(
            rerun_report["rows"]
                .as_array()
                .expect("rerun rows")
                .iter()
                .find(|row| row["original_username"] == "Spaced User")
                .expect("rerun spaced row")["skip_reason"],
            "already_imported"
        );
        let rerun_after = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("account list after rerun should complete")
            .json::<serde_json::Value>()
            .await
            .expect("account list after rerun JSON");
        assert_eq!(rerun_after, rerun_before);

        let reset = client
            .post(format!("http://{address}/web-admin/accounts/reset"))
            .owner(&token)
            .json(&serde_json::json!({
                "account_id": imported["id"],
                "password": "new-imported-password"
            }))
            .send()
            .await
            .expect("password reset should complete");
        assert_eq!(reset.status(), StatusCode::OK);

        let after_reset = client
            .get(format!("http://{address}/web-admin/accounts"))
            .owner(&token)
            .send()
            .await
            .expect("account list after reset should complete")
            .json::<serde_json::Value>()
            .await
            .expect("account list after reset JSON");
        let reset_account = after_reset
            .as_array()
            .expect("account list array")
            .iter()
            .find(|account| account["id"] == imported["id"])
            .expect("reset account");
        assert_eq!(reset_account["imported_from"], "martec");
        assert!(reset_account["imported_at"].is_string());
        assert_eq!(reset_account["legacy_not_logged_in"], false);

        let malformed = client
            .post(&import_url)
            .owner(&token)
            .json(&serde_json::json!({
                "dry_run": true,
                "accounts": { "broken": {} }
            }))
            .send()
            .await
            .expect("malformed import should complete");
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn owner_password_reset_clears_account_login_lockout() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let account = accounts::create(
            &pool,
            "locked-reset-user",
            "old-password",
            AccountCreator::SelfService,
            Some("pc-01"),
        )
        .await
        .expect("Account should be created");
        accounts::create(
            &pool,
            "other-locked-user",
            "other-password",
            AccountCreator::SelfService,
            Some("pc-02"),
        )
        .await
        .expect("other Account should be created");
        let owner_address = spawn(state.clone()).await;
        let client_address = spawn_client_server(&state).await;
        let client = reqwest::Client::new();

        for _ in 0..crate::account_login_throttle::FAILURE_THRESHOLD {
            let response = client
                .post(format!("http://{client_address}/api/accounts/login"))
                .json(&serde_json::json!({
                    "username": "LOCKED-RESET-USER",
                    "password": "wrong-password",
                    "pc_id": "pc-01"
                }))
                .send()
                .await
                .expect("failed login should complete");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let locked = client
            .post(format!("http://{client_address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "locked-reset-user",
                "password": "old-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("locked login should complete");
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);

        for _ in 0..crate::account_login_throttle::FAILURE_THRESHOLD {
            let response = client
                .post(format!("http://{client_address}/api/accounts/login"))
                .json(&serde_json::json!({
                    "username": "other-locked-user",
                    "password": "wrong-password",
                    "pc_id": "pc-02"
                }))
                .send()
                .await
                .expect("other failed login should complete");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let owner_token = setup_and_token(&client, &owner_address, "1234").await;
        let reset = client
            .post(format!("http://{owner_address}/web-admin/accounts/reset"))
            .owner(&owner_token)
            .json(&serde_json::json!({
                "account_id": account.id,
                "password": "new-password"
            }))
            .send()
            .await
            .expect("password reset should complete");
        assert_eq!(reset.status(), StatusCode::OK);

        let login = client
            .post(format!("http://{client_address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "locked-reset-user",
                "password": "new-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("reset password login should complete");
        assert_eq!(login.status(), StatusCode::OK);

        let other_still_locked = client
            .post(format!("http://{client_address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "other-locked-user",
                "password": "other-password",
                "pc_id": "pc-02"
            }))
            .send()
            .await
            .expect("other locked login should complete");
        assert_eq!(other_still_locked.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn account_import_reuses_deleted_native_imported_and_self_created_usernames() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "reimport-pin").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let password = bcrypt::hash("import-password", 4).expect("bcrypt hash");
        let payload = |username: &str, source_id: &str| {
            serde_json::json!({
                "dry_run": true,
                "accounts": {
                    username: {
                        "id": source_id,
                        "password": password,
                        "credits": 2,
                        "points": 1,
                        "enabled": true,
                        "last": null
                    }
                }
            })
        };
        let import_url = format!("http://{address}/web-admin/accounts/import");

        let native = accounts::create(
            &pool,
            "NativeDeleted",
            "native-password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("native account");
        let self_created = accounts::create(
            &pool,
            "SelfDeleted",
            "self-password",
            AccountCreator::SelfService,
            None,
        )
        .await
        .expect("self-service account");

        let imported = auth(client.post(&import_url))
            .json(&serde_json::json!({
                "dry_run": false,
                "accounts": {
                    "ImportedDeleted": {
                        "id": "imported-original",
                        "password": password,
                        "credits": 2,
                        "points": 1,
                        "enabled": true,
                        "last": null
                    }
                }
            }))
            .send()
            .await
            .expect("initial import");
        assert_eq!(imported.status(), StatusCode::OK);

        let imported_id: String = sqlx::query_scalar(
            "SELECT id FROM accounts WHERE username_normalized = 'importeddeleted'",
        )
        .fetch_one(&pool)
        .await
        .expect("imported account id");

        for (username, old_id, source_id) in [
            ("NativeDeleted", native.id, "native-reimport"),
            ("ImportedDeleted", imported_id, "imported-reimport"),
            ("SelfDeleted", self_created.id, "self-reimport"),
        ] {
            let deleted = auth(client.post(format!("http://{address}/web-admin/accounts/delete")))
                .json(&serde_json::json!({ "account_id": old_id }))
                .send()
                .await
                .expect("delete account");
            assert_eq!(deleted.status(), StatusCode::OK);

            let preview_body = payload(username, source_id);
            let preview = auth(client.post(&import_url))
                .json(&preview_body)
                .send()
                .await
                .expect("re-import preview");
            assert_eq!(preview.status(), StatusCode::OK);
            let preview_report = preview
                .json::<serde_json::Value>()
                .await
                .expect("preview report");
            assert_eq!(preview_report["totals"]["created"], 1);
            assert_eq!(preview_report["rows"][0]["outcome"], "created");

            let mut commit_body = preview_body;
            commit_body["dry_run"] = serde_json::json!(false);
            let commit = auth(client.post(&import_url))
                .json(&commit_body)
                .send()
                .await
                .expect("re-import commit");
            assert_eq!(commit.status(), StatusCode::OK);
            assert_eq!(
                commit
                    .json::<serde_json::Value>()
                    .await
                    .expect("commit report"),
                preview_report
            );

            let active_id: String = sqlx::query_scalar(
                "SELECT id FROM accounts
                 WHERE username_normalized = ? AND status = 'active'",
            )
            .bind(username.to_ascii_lowercase())
            .fetch_one(&pool)
            .await
            .expect("new active account");
            assert_ne!(active_id, old_id);
            assert_eq!(
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT imported_from FROM accounts WHERE id = ?",
                )
                .bind(&active_id)
                .fetch_one(&pool)
                .await
                .expect("re-import provenance"),
                Some("martec".to_owned())
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM audit_log
                     WHERE account_id = ? AND event_type = 'account_created'",
                )
                .bind(&active_id)
                .fetch_one(&pool)
                .await
                .expect("re-import audit"),
                1
            );

            let repeated_preview_body = payload(username, "repeated-source");
            let repeated_preview = auth(client.post(&import_url))
                .json(&repeated_preview_body)
                .send()
                .await
                .expect("repeated preview");
            assert_eq!(repeated_preview.status(), StatusCode::OK);
            let repeated_report = repeated_preview
                .json::<serde_json::Value>()
                .await
                .expect("repeated preview report");
            assert_eq!(
                repeated_report["rows"][0]["skip_reason"],
                "already_imported"
            );

            let deleted_again =
                auth(client.post(format!("http://{address}/web-admin/accounts/delete")))
                    .json(&serde_json::json!({ "account_id": active_id }))
                    .send()
                    .await
                    .expect("delete re-imported account");
            assert_eq!(deleted_again.status(), StatusCode::OK);

            let manual = auth(client.post(format!("http://{address}/web-admin/accounts")))
                .json(&serde_json::json!({
                    "username": username,
                    "password": "manual-password"
                }))
                .send()
                .await
                .expect("manual create");
            assert_eq!(manual.status(), StatusCode::CONFLICT);

            let mut second_commit_body = payload(username, "second-reimport");
            second_commit_body["dry_run"] = serde_json::json!(false);
            let second_commit = auth(client.post(&import_url))
                .json(&second_commit_body)
                .send()
                .await
                .expect("second re-import");
            assert_eq!(second_commit.status(), StatusCode::OK);
            assert_eq!(
                second_commit
                    .json::<serde_json::Value>()
                    .await
                    .expect("second report")["rows"][0]["outcome"],
                "created"
            );
        }

        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                 SUM(username_normalized = 'nativedeleted'),
                 SUM(username_normalized = 'importeddeleted'),
                 SUM(username_normalized = 'selfdeleted')
             FROM accounts",
        )
        .fetch_one(&pool)
        .await
        .expect("retained account rows");
        assert_eq!(counts, (3, 3, 3));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_deleted'",
            )
            .fetch_one(&pool)
            .await
            .expect("deletion audits"),
            6
        );
    }

    #[tokio::test]
    async fn account_import_commit_rolls_back_on_unexpected_database_error() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::raw_sql(
            "CREATE TRIGGER reject_martec_import
             BEFORE INSERT ON accounts
             WHEN NEW.imported_from = 'martec'
             BEGIN SELECT RAISE(ABORT, 'forced import failure'); END;",
        )
        .execute(&pool)
        .await
        .expect("failure trigger should be created");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "rollback-pin").await;
        let payload = serde_json::json!({
            "dry_run": false,
            "accounts": {
                "RollbackUser": {
                    "id": "rollback-id",
                    "password": bcrypt::hash("rollback-password", 4).expect("bcrypt hash"),
                    "credits": 2,
                    "points": 1,
                    "enabled": true,
                    "last": null
                }
            }
        });
        let before_accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .expect("account count");
        let before_audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count");

        let response = client
            .post(format!("http://{address}/web-admin/accounts/import"))
            .owner(&token)
            .json(&payload)
            .send()
            .await
            .expect("failed import should complete");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts")
                .fetch_one(&pool)
                .await
                .expect("account count after rollback"),
            before_accounts
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
                .fetch_one(&pool)
                .await
                .expect("audit count after rollback"),
            before_audits
        );
    }

    #[tokio::test]
    async fn owner_theme_requires_auth_validates_and_bumps_version() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        let unauthorized = client
            .get(format!("http://{address}/api/theme"))
            .send()
            .await
            .expect("unauthorized GET should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "1234").await;
        let updated = client
            .put(format!("http://{address}/api/theme"))
            .owner(&token)
            .json(&serde_json::json!({
                "accent_color": "#AABBCC",
                "mode_web_admin": "dark",
                "mode_kiosk": "light",
                "mode_client": "dark",
                "version": 1
            }))
            .send()
            .await
            .expect("theme PUT should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let theme = updated
            .json::<serde_json::Value>()
            .await
            .expect("theme JSON");
        assert_eq!(theme["accent_color"], "#aabbcc");
        assert_eq!(theme["version"], 2);

        let invalid_mode = client
            .put(format!("http://{address}/api/theme"))
            .owner(&token)
            .json(&serde_json::json!({
                "accent_color": "#15803d",
                "mode_web_admin": "system",
                "mode_kiosk": "light",
                "mode_client": "light",
                "version": 2
            }))
            .send()
            .await
            .expect("invalid PUT should complete");
        assert_eq!(invalid_mode.status(), StatusCode::BAD_REQUEST);
    }

    /// A denied read must not be audited, but a denied mutation must be.
    ///
    /// Regression: the web admin polls `GET /api/theme` on a timer, so
    /// auditing denied reads turned any unauthenticated page left open into a
    /// steady `web_admin_access_denied` flood (four rows a minute, forever)
    /// with no login ever attempted.
    #[tokio::test]
    async fn denied_reads_are_not_audited_but_denied_mutations_are() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        // A stale cookie: the owner logged in before an admin-app restart wiped
        // the in-memory token set.
        let stale = cookie("00000000-0000-0000-0000-000000000000");

        for _ in 0..3 {
            let poll = client
                .get(format!("http://{address}/api/theme"))
                .header(reqwest::header::COOKIE, &stale)
                .send()
                .await
                .expect("theme poll should complete");
            assert_eq!(poll.status(), StatusCode::UNAUTHORIZED);
        }

        let denials = count_access_denied(&pool).await;
        assert_eq!(denials, 0, "a denied read must not write an audit row");

        // The mutation path keeps its audit trail.
        let write = client
            .put(format!("http://{address}/api/theme"))
            .header(reqwest::header::COOKIE, &stale)
            .json(&serde_json::json!({
                "accent_color": "#15803d",
                "mode_web_admin": "light",
                "mode_kiosk": "light",
                "mode_client": "light",
                "version": 1
            }))
            .send()
            .await
            .expect("theme PUT should complete");
        assert_eq!(write.status(), StatusCode::UNAUTHORIZED);

        let denials = count_access_denied(&pool).await;
        assert_eq!(denials, 1, "a denied mutation must write an audit row");
    }

    async fn count_access_denied(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'web_admin_access_denied'",
        )
        .fetch_one(pool)
        .await
        .expect("audit count should read")
    }

    #[tokio::test]
    async fn coin_debug_bundle_endpoint_requires_owner_and_returns_bundle() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/coin-device/debug-bundle");

        let denied = client
            .get(&url)
            .send()
            .await
            .expect("unauthenticated request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "1234").await;
        let response = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("authenticated request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let bundle = response
            .json::<String>()
            .await
            .expect("debug bundle should be a JSON string");
        assert!(bundle.starts_with("# Coin Debug Bundle"));
        assert!(bundle.contains("## Environment"));
        assert!(bundle.contains("## Verdict"));
    }

    #[tokio::test]
    async fn support_bundle_endpoint_requires_owner_and_returns_bundle() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/support-bundle");

        let denied = client
            .get(&url)
            .query(&[("pc_id", "pc-01")])
            .send()
            .await
            .expect("unauthenticated request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "1234").await;
        let response = client
            .get(&url)
            .query(&[("pc_id", "pc-01"), ("window_seconds", "3600")])
            .owner(&token)
            .send()
            .await
            .expect("authenticated request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let bundle = response
            .json::<String>()
            .await
            .expect("support bundle should be a JSON string");
        assert!(bundle.starts_with("# Support Bundle"));
        assert!(bundle.contains("## Environment"));
        assert!(bundle.contains("## Timeline"));
        assert!(bundle.contains("## Anomalies"));
        assert!(bundle.contains("## Raw Appendix"));
        assert!(bundle.contains("| PC | `pc-01` |"));

        // The default window applies when the parameter is absent.
        let default_window = client
            .get(&url)
            .query(&[("pc_id", "pc-01")])
            .owner(&token)
            .send()
            .await
            .expect("default-window request should complete");
        assert_eq!(default_window.status(), StatusCode::OK);

        // An unknown PC is refused at the boundary instead of rendering an
        // empty bundle.
        let unknown = client
            .get(&url)
            .query(&[("pc_id", "pc-99")])
            .owner(&token)
            .send()
            .await
            .expect("unknown PC request should complete");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let missing = client
            .get(&url)
            .query(&[("window_seconds", "3600")])
            .owner(&token)
            .send()
            .await
            .expect("missing pc_id request should complete");
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn support_bundle_incident_anchor_windows_the_bundle_around_the_incident() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/support-bundle");
        let token = setup_and_token(&client, &address, "1234").await;

        // A fixed anchor: one hour before to fifteen minutes after it, no
        // matter how long ago it fired.
        let anchored = client
            .get(&url)
            .query(&[
                ("pc_id", "pc-01"),
                ("incident_opened_at", "2026-08-01T10:00:00Z"),
            ])
            .owner(&token)
            .send()
            .await
            .expect("anchored request should complete");
        assert_eq!(anchored.status(), StatusCode::OK);
        let bundle = anchored
            .json::<String>()
            .await
            .expect("anchored bundle should be a JSON string");
        assert!(bundle.contains("2026-08-01T09:00:00.000000Z"));
        assert!(bundle.contains("2026-08-01T10:15:00.000000Z"));

        // An anchor the boundary cannot parse is a request error, never a
        // silent fall-back to the unanchored window.
        let invalid = client
            .get(&url)
            .query(&[("pc_id", "pc-01"), ("incident_opened_at", "yesterday")])
            .owner(&token)
            .send()
            .await
            .expect("invalid-anchor request should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    // ── Host diagnostics endpoint (ADR-0082 / #378) ─────────────────────

    #[tokio::test]
    async fn host_diagnostics_endpoint_requires_owner_and_reports_host_health() {
        let data_dir = tempfile::tempdir().expect("temp dir");
        let state = test_state_in(data_dir.path()).await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/host-diagnostics");

        let denied = client
            .get(&url)
            .send()
            .await
            .expect("unauthenticated request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "1234").await;
        let fetch = || client.get(&url).owner(&token).send();

        // An empty Run Record history and an absent panic file answer
        // cleanly rather than erroring.
        let response = fetch()
            .await
            .expect("authenticated request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("host health should be JSON");
        assert_eq!(body["runs"], serde_json::json!([]));
        assert_eq!(body["panic_file"]["exists"], serde_json::json!(false));
        assert_eq!(body["panic_file"]["newest_8kb"], serde_json::Value::Null);

        // Seed the file newest last; the endpoint reports newest first, and
        // a start with no end on a past run reads as a suspected hard exit.
        let base = Utc::now() - chrono::Duration::hours(3);
        let finished = crate::run_record::RunRecord {
            version: "0.8.0".to_owned(),
            pid: 100,
            started_at: base,
            ended_at: Some(base + chrono::Duration::hours(1)),
            exit_reason: Some(crate::run_record::ExitReason::ApplicationExit),
            app_update_result: None,
        };
        // Deliberately this process's own PID: Windows reuses PIDs, and the
        // endpoint must still read this dead run as a suspected hard exit
        // because it is not the record this run wrote.
        let unfinished = crate::run_record::RunRecord {
            version: "0.9.0".to_owned(),
            pid: std::process::id(),
            started_at: base + chrono::Duration::hours(2),
            ended_at: None,
            exit_reason: None,
            app_update_result: None,
        };
        crate::test_utils::seed_run_records(data_dir.path(), &[finished, unfinished]);

        let body = fetch()
            .await
            .expect("authenticated request should complete")
            .json::<serde_json::Value>()
            .await
            .expect("host health should be JSON");
        let runs = body["runs"].as_array().expect("runs is an array");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["version"], serde_json::json!("0.9.0"));
        assert_eq!(runs[0]["pid"], serde_json::json!(std::process::id()));
        assert_eq!(runs[0]["ended_at"], serde_json::Value::Null);
        assert_eq!(runs[0]["suspected_hard_exit"], serde_json::json!(true));
        assert_eq!(runs[1]["version"], serde_json::json!("0.8.0"));
        assert_eq!(
            runs[1]["exit_reason"],
            serde_json::json!("application_exit")
        );
        assert_eq!(runs[1]["suspected_hard_exit"], serde_json::json!(false));
        assert!(runs[1]["ended_at"].is_string());

        // The panic file's newest 8 KB ride the response; older bytes stay
        // on disk.
        let size_bytes =
            crate::test_utils::write_panic_slice_fixture(&data_dir.path().join("logs"));

        let body = fetch()
            .await
            .expect("authenticated request should complete")
            .json::<serde_json::Value>()
            .await
            .expect("host health should be JSON");
        let panic_file = &body["panic_file"];
        assert_eq!(panic_file["exists"], serde_json::json!(true));
        assert_eq!(panic_file["size_bytes"], serde_json::json!(size_bytes));
        assert!(panic_file["last_modified_at"].is_string());
        let slice = panic_file["newest_8kb"].as_str().expect("slice is text");
        assert_eq!(slice.len(), crate::host_diagnostics::PANIC_SLICE_BYTES);
        assert!(slice.contains("TAIL-MARKER crash evidence"));
        assert!(
            !slice.contains("OLD-EVIDENCE"),
            "bytes older than the newest 8 KB stay on disk"
        );
    }

    #[tokio::test]
    async fn host_diagnostics_endpoint_serves_identically_with_diagnostics_mode_on_and_off() {
        let data_dir = tempfile::tempdir().expect("temp dir");
        let state = test_state_in(data_dir.path()).await;
        state
            .run_records
            .start(env!("CARGO_PKG_VERSION"))
            .expect("the live run's record starts");
        let pool = state.pool.clone();
        db::diagnostics_mode::set_enabled(&pool, true)
            .await
            .expect("the flag enables");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/host-diagnostics");
        let token = setup_and_token(&client, &address, "1234").await;

        let on = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("on request should complete");
        assert_eq!(on.status(), StatusCode::OK);
        let on_body = on
            .json::<serde_json::Value>()
            .await
            .expect("host health should be JSON");

        db::diagnostics_mode::set_enabled(&pool, false)
            .await
            .expect("the flag disables");
        let off = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("off request should complete");
        assert_eq!(off.status(), StatusCode::OK);
        let off_body = off
            .json::<serde_json::Value>()
            .await
            .expect("host health should be JSON");

        assert_eq!(on_body, off_body, "the flag must never gate the endpoint");
        assert!(
            !on_body["runs"]
                .as_array()
                .expect("runs is an array")
                .is_empty(),
            "the comparison is over substantive host evidence"
        );
    }

    #[tokio::test]
    async fn diagnostic_request_endpoint_requires_owner_and_arms_a_known_pc() {
        let state = test_state().await;
        let diagnostic_requests = state.diagnostic_requests.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url =
            |pc_id: &str| format!("http://{address}/web-admin/support/diagnostic-request/{pc_id}");

        // Owner Session only — same gate as the Support Bundle itself.
        let denied = client
            .post(url("pc-01"))
            .send()
            .await
            .expect("unauthenticated request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            diagnostic_requests.pending("pc-01", chrono::Utc::now()),
            None,
            "a refused arm never places the ask"
        );

        let token = setup_and_token(&client, &address, "1234").await;
        let response = client
            .post(url("pc-01"))
            .owner(&token)
            .send()
            .await
            .expect("authenticated request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("ack should be JSON")["ok"],
            serde_json::json!(true)
        );
        assert!(
            diagnostic_requests
                .pending("pc-01", chrono::Utc::now())
                .is_some(),
            "the ask is armed for the Heartbeat to deliver"
        );

        // An unknown PC is a boundary failure, never a doomed ask.
        let unknown = client
            .post(url("pc-99"))
            .owner(&token)
            .send()
            .await
            .expect("unknown PC request should complete");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            diagnostic_requests.pending("pc-99", chrono::Utc::now()),
            None
        );
    }

    #[tokio::test]
    async fn calibration_endpoint_requires_owner() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let calibration_url = format!("http://{address}/web-admin/coin-device/calibration");

        assert_eq!(
            client
                .post(&calibration_url)
                .json(&serde_json::json!({ "target_amount_cents": 2000 }))
                .send()
                .await
                .expect("unauthenticated calibration request")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let token = setup_and_token(&client, &address, "1234").await;
        let calibration = client
            .post(&calibration_url)
            .owner(&token)
            .json(&serde_json::json!({ "target_amount_cents": 2000 }))
            .send()
            .await
            .expect("simulator calibration request");
        assert_eq!(calibration.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn acceptor_health_is_owner_authenticated_and_has_s1_shape() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let health_url = format!("http://{address}/web-admin/coin-device/acceptor-health");
        let obsolete_url = format!("http://{address}/web-admin/coin-device/unmapped");

        assert_eq!(
            client
                .get(&health_url)
                .send()
                .await
                .expect("unauthenticated health request")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let token = setup_and_token(&client, &address, "acceptor-health-pin").await;
        let obsolete = client
            .get(&obsolete_url)
            .owner(&token)
            .send()
            .await
            .expect("obsolete unmapped route should complete");
        assert_eq!(obsolete.status(), StatusCode::NOT_FOUND);

        let response = client
            .get(&health_url)
            .owner(&token)
            .send()
            .await
            .expect("authenticated health request");
        assert_eq!(response.status(), StatusCode::OK);
        let health = response
            .json::<serde_json::Value>()
            .await
            .expect("health JSON");
        assert_eq!(health.as_object().map(|object| object.len()), Some(3));
        assert!(health["debug_window_start"].as_str().is_some());
        assert_eq!(health["timing_status"], "unavailable_until_filtered");
        assert_eq!(health["implausible_timing_rate"], serde_json::Value::Null);
        assert!(health.get("items").is_none());
        assert!(health.get("pulse_count").is_none());
    }

    /// Read the `since` value from the debug-window endpoint as a comparable
    /// timestamp, asserting the request succeeded.
    async fn debug_window_since(
        client: &reqwest::Client,
        address: &str,
        token: &OwnerAuth,
    ) -> DateTime<Utc> {
        let response = client
            .get(format!(
                "http://{address}/web-admin/coin-device/debug-window"
            ))
            .owner(token)
            .send()
            .await
            .expect("debug-window GET should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let since = response
            .json::<serde_json::Value>()
            .await
            .expect("debug-window JSON")["since"]
            .as_str()
            .expect("since is a string")
            .to_owned();
        DateTime::parse_from_rfc3339(&since)
            .expect("since is RFC3339")
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn debug_window_get_and_reset_require_owner_and_advance() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/coin-device/debug-window");

        // Both verbs require an owner session.
        assert_eq!(
            client
                .get(&url)
                .send()
                .await
                .expect("GET should complete")
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .post(&url)
                .send()
                .await
                .expect("POST should complete")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let token = setup_and_token(&client, &address, "1234").await;
        let initial = debug_window_since(&client, &address, &token).await;

        // A manual reset advances the window to a strictly later start.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let reset = client
            .post(&url)
            .owner(&token)
            .send()
            .await
            .expect("reset POST should complete");
        assert_eq!(reset.status(), StatusCode::OK);
        let after_reset = debug_window_since(&client, &address, &token).await;
        assert!(after_reset > initial, "reset should advance the window");
    }

    #[tokio::test]
    async fn coin_device_save_advances_debug_window() {
        let state = test_state().await;
        sqlx::query("UPDATE settings SET value = 'false' WHERE key = 'coin_calibration_required'")
            .execute(&state.pool)
            .await
            .expect("disable fresh candidate gate for save test");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        let before = debug_window_since(&client, &address, &token).await;

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let saved = client
            .put(format!("http://{address}/web-admin/coin-device"))
            .owner(&token)
            .json(&serde_json::json!({
                "com_port": null,
                "baud_rate": 9600,
                "set_pin_line": "dtr",
                "coin_pin_line": "cts",
                "coin_pin_polarity": "normally_closed",
                "inter_coin_gap_ms": 250,
                "inactivity_timeout_seconds": 20,
                "hard_cap_seconds": 240,
                "pulse_map": [{ "pulse_count": 1, "amount_cents": 100 }]
            }))
            .send()
            .await
            .expect("save should complete");
        assert_eq!(saved.status(), StatusCode::OK);

        let after = debug_window_since(&client, &address, &token).await;
        assert!(
            after > before,
            "a Coin Device save should advance the window"
        );
    }

    #[tokio::test]
    async fn coin_device_settings_round_trip_and_test_acceptor() {
        let state = test_state().await;
        sqlx::query("UPDATE settings SET value = 'false' WHERE key = 'coin_calibration_required'")
            .execute(&state.pool)
            .await
            .expect("disable fresh candidate gate for generic settings test");
        let terminal_selection = state.terminal_selection.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        // Defaults mirror the M1 constants.
        let defaults = client
            .get(format!("http://{address}/web-admin/coin-device"))
            .owner(&token)
            .send()
            .await
            .expect("get should complete")
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON");
        assert!(defaults["com_port"].is_null());
        assert_eq!(defaults["inter_coin_gap_ms"], 600);
        assert_eq!(defaults["pulse_map"].as_array().map(|m| m.len()), Some(4));

        // Update with a calibrated map and gap; no COM port -> simulator.
        let updated = client
            .put(format!("http://{address}/web-admin/coin-device"))
            .owner(&token)
            .json(&serde_json::json!({
                "com_port": null,
                "baud_rate": 9600,
                "set_pin_line": "dtr",
                "coin_pin_line": "cts",
                "coin_pin_polarity": "normally_closed",
                "inter_coin_gap_ms": 250,
                "inactivity_timeout_seconds": 20,
                "hard_cap_seconds": 240,
                "pulse_map": [
                    { "pulse_count": 1, "amount_cents": 100 },
                    { "pulse_count": 2, "amount_cents": 200 }
                ]
            }))
            .send()
            .await
            .expect("update should complete");
        assert_eq!(updated.status(), StatusCode::OK);

        let read = client
            .get(format!("http://{address}/web-admin/coin-device"))
            .owner(&token)
            .send()
            .await
            .expect("get should complete")
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON");
        assert_eq!(read["set_pin_line"], "dtr");
        assert_eq!(read["coin_pin_line"], "cts");
        assert_eq!(read["coin_pin_polarity"], "normally_closed");
        assert_eq!(read["inter_coin_gap_ms"], 250);
        assert_eq!(read["inactivity_timeout_seconds"], 20);
        assert_eq!(read["hard_cap_seconds"], 240);
        assert_eq!(read["pulse_map"].as_array().map(|m| m.len()), Some(2));

        // The PUT applies to the running source and selection timers, not only SQLite.
        let opened = terminal_selection
            .open("pc-01")
            .await
            .expect("selection should open with updated timers");
        assert!(opened.inactivity_remaining_seconds <= 20);
        let simulated = client
            .post(format!("http://{address}/web-admin/coin/simulate"))
            .owner(&token)
            .json(&serde_json::json!({ "pulse_count": 2 }))
            .send()
            .await
            .expect("simulation should complete");
        assert_eq!(simulated.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(275)).await;
        let accepted = terminal_selection
            .try_accept_next_batch()
            .await
            .expect("configured batch should commit")
            .expect("two-pulse batch should close");
        assert_eq!(accepted.batch.pulse_count, 2);
        assert_eq!(accepted.transaction.amount_cents, 200);
        assert_eq!(accepted.transaction.seconds_added, 600);
        terminal_selection
            .close(crate::terminal_selection::CloseReason::Manual)
            .await
            .expect("selection should close");

        // Invalid line is rejected at the Rust boundary.
        let invalid = client
            .put(format!("http://{address}/web-admin/coin-device"))
            .owner(&token)
            .json(&serde_json::json!({
                "com_port": null,
                "baud_rate": 9600,
                "set_pin_line": "bogus",
                "coin_pin_line": "cts",
                "coin_pin_polarity": "normally_open",
                "inter_coin_gap_ms": 250,
                "pulse_map": [{ "pulse_count": 1, "amount_cents": 100 }]
            }))
            .send()
            .await
            .expect("update should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        // The test-acceptor action is a simulator no-op when no port is set.
        let test = client
            .post(format!("http://{address}/web-admin/coin-device/test"))
            .owner(&token)
            .send()
            .await
            .expect("test should complete")
            .json::<serde_json::Value>()
            .await
            .expect("test JSON");
        assert_eq!(test["ok"], true);
        assert_eq!(test["simulator"], true);
    }

    #[tokio::test]
    async fn coin_device_ports_requires_auth_and_pins_missing_saved_port() {
        let state = test_state().await;
        sqlx::query("UPDATE coin_device_settings SET com_port = ? WHERE id = 1")
            .bind("OPENPISO_MISSING_TEST_PORT")
            .execute(&state.pool)
            .await
            .expect("saved port should update");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/coin-device/ports");

        let denied = client
            .get(&url)
            .send()
            .await
            .expect("unauthenticated request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "1234").await;
        let response = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("authenticated request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let ports = response
            .json::<Vec<serial_port_detection::DetectedSerialPort>>()
            .await
            .expect("port list should match its response shape");
        let saved = ports
            .iter()
            .find(|port| port.name == "OPENPISO_MISSING_TEST_PORT")
            .expect("saved impossible port should be pinned");
        assert!(!saved.detected);
    }

    // -- Phase 2: shop settings, owner PIN, points-transfer approvals --------

    #[tokio::test]
    async fn settings_put_persists_and_is_reflected_by_public_shop_endpoint() {
        // Both servers share one pool in production; prove a PUT on :8080 is
        // visible through GET /api/shop on the :3210 client surface (plan §5.5).
        let state = test_state().await;
        let pool = state.pool.clone();
        let events = state.events.clone();
        let account_tokens = state.account_tokens.clone();
        let sessions = state.sessions.clone();
        let owner_address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &owner_address, "1234").await;

        let updated = client
            .put(format!("http://{owner_address}/web-admin/settings"))
            .owner(&token)
            .json(&serde_json::json!({
                "shop_name": "Barangay Net Café",
                "offline_grace_seconds": 240
            }))
            .send()
            .await
            .expect("settings PUT should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let read = client
            .get(format!("http://{owner_address}/web-admin/settings"))
            .owner(&token)
            .send()
            .await
            .expect("settings GET should complete")
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON");
        assert_eq!(read["shop_name"], "Barangay Net Café");
        assert_eq!(read["shutdown_grace_seconds"], 300);
        assert_eq!(read["offline_grace_seconds"], 240);

        // The kiosk reads the same committed value in-process through its
        // Tauri command path rather than through either HTTP server.
        assert_eq!(
            crate::commands::kiosk_shop_name(&pool)
                .await
                .expect("kiosk Shop Name read should succeed"),
            "Barangay Net Café"
        );

        // Spin up the client/device server on the same pool and read /api/shop.
        let client_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("client listener should bind");
        let client_address = client_listener
            .local_addr()
            .expect("client listener address")
            .to_string();
        tokio::spawn(async move {
            axum::serve(
                client_listener,
                crate::server::router(
                    pool,
                    events,
                    account_tokens,
                    sessions,
                    crate::forwarded_log::test_forwarded_log(),
                ),
            )
            .await
            .expect("client server should run");
        });
        let shop = client
            .get(format!("http://{client_address}/api/shop"))
            .send()
            .await
            .expect("shop GET should complete")
            .json::<serde_json::Value>()
            .await
            .expect("shop JSON");
        assert_eq!(shop["shop_name"], "Barangay Net Café");
        assert_eq!(shop["shutdown_grace_seconds"], 300);
        assert_eq!(shop["offline_grace_seconds"], 240);

        // An empty shop name is rejected at the Rust boundary.
        let invalid = client
            .put(format!("http://{owner_address}/web-admin/settings"))
            .owner(&token)
            .json(&serde_json::json!({ "shop_name": "   " }))
            .send()
            .await
            .expect("settings PUT should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let invalid_offline_grace = client
            .put(format!("http://{owner_address}/web-admin/settings"))
            .owner(&token)
            .json(&serde_json::json!({
                "shop_name": "Barangay Net Café",
                "offline_grace_seconds": 0
            }))
            .send()
            .await
            .expect("settings PUT should complete");
        assert_eq!(invalid_offline_grace.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn owner_pin_put_is_write_only_and_hashed() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        let response = client
            .put(format!("http://{address}/web-admin/owner-pin"))
            .owner(&token)
            .json(&serde_json::json!({ "new_pin": "987654" }))
            .send()
            .await
            .expect("owner-pin PUT should complete");
        assert_eq!(response.status(), StatusCode::OK);

        // The stored value is an Argon2 hash, never the plaintext, and the
        // server-side compare round-trips.
        let stored: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'owner_pin'")
                .fetch_one(&pool)
                .await
                .expect("owner_pin row");
        assert_ne!(stored, "987654");
        assert!(db::verify_owner_pin(&pool, "987654")
            .await
            .expect("verify should run"));
        assert!(!db::verify_owner_pin(&pool, "000000")
            .await
            .expect("verify should run"));

        // Unauthenticated callers cannot set the PIN.
        let denied = client
            .put(format!("http://{address}/web-admin/owner-pin"))
            .json(&serde_json::json!({ "new_pin": "111111" }))
            .send()
            .await
            .expect("owner-pin PUT should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    async fn funded_account(pool: &SqlitePool, username: &str, points: i64) -> String {
        let account = accounts::create(pool, username, "password", AccountCreator::Admin, None)
            .await
            .expect("account should be created");
        sqlx::query("UPDATE accounts SET points_balance_centi = ? WHERE id = ?")
            .bind(points)
            .bind(&account.id)
            .execute(pool)
            .await
            .expect("account funding");
        account.id
    }

    #[tokio::test]
    async fn transfer_requests_list_confirm_and_reject_round_trip() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        let sender = funded_account(&pool, "approve-sender", 10).await;
        let recipient = funded_account(&pool, "approve-recipient", 0).await;
        let to_confirm = accounts::request_points_transfer(&pool, &sender, "approve-recipient", 6)
            .await
            .expect("request should be created");

        // The owner sees the pending request with both usernames and the code.
        let listed = auth(client.get(format!("http://{address}/web-admin/transfer-requests")))
            .send()
            .await
            .expect("list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("list JSON");
        let rows = listed.as_array().expect("list array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["from_username"], "approve-sender");
        assert_eq!(rows[0]["to_username"], "approve-recipient");
        assert_eq!(rows[0]["points_centi"], 6);
        assert_eq!(rows[0]["code"], to_confirm.code);

        // Confirm moves the points atomically.
        let confirmed = auth(client.post(format!(
            "http://{address}/web-admin/transfer-requests/confirm"
        )))
        .json(&serde_json::json!({ "code": to_confirm.code }))
        .send()
        .await
        .expect("confirm should complete");
        assert_eq!(confirmed.status(), StatusCode::OK);
        let confirmed_body = confirmed
            .json::<serde_json::Value>()
            .await
            .expect("confirm JSON");
        assert_eq!(confirmed_body["status"], "confirmed");
        let balances: (i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT points_balance_centi FROM accounts WHERE id = ?),
               (SELECT points_balance_centi FROM accounts WHERE id = ?)",
        )
        .bind(&sender)
        .bind(&recipient)
        .fetch_one(&pool)
        .await
        .expect("balances");
        assert_eq!(balances, (4, 6));

        // A second confirm of the same code is refused (single-use).
        let replay = auth(client.post(format!(
            "http://{address}/web-admin/transfer-requests/confirm"
        )))
        .json(&serde_json::json!({ "code": to_confirm.code }))
        .send()
        .await
        .expect("confirm should complete");
        assert_eq!(replay.status(), StatusCode::CONFLICT);

        // Reject leaves points untouched and the list excludes resolved rows.
        let to_reject = accounts::request_points_transfer(&pool, &sender, "approve-recipient", 2)
            .await
            .expect("request should be created");
        let rejected = auth(client.post(format!(
            "http://{address}/web-admin/transfer-requests/reject"
        )))
        .json(&serde_json::json!({ "code": to_reject.code }))
        .send()
        .await
        .expect("reject should complete");
        assert_eq!(rejected.status(), StatusCode::OK);
        assert_eq!(
            rejected
                .json::<serde_json::Value>()
                .await
                .expect("reject JSON")["status"],
            "rejected"
        );
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender)
                .fetch_one(&pool)
                .await
                .expect("sender points");
        assert_eq!(sender_points, 4);

        let after = auth(client.get(format!("http://{address}/web-admin/transfer-requests")))
            .send()
            .await
            .expect("list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("list JSON");
        assert_eq!(after.as_array().map(|r| r.len()), Some(0));
    }

    #[tokio::test]
    async fn voucher_list_http_filters_by_batch_and_searches_normalized_code() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let create = |prefix: &str| {
            client
                .post(format!("http://{address}/web-admin/voucher-batches"))
                .owner(&token)
                .json(&serde_json::json!({
                    "prefix": prefix,
                    "suffix_length": 8,
                    "count": 2,
                    "seconds_value": 300,
                    "points_value_centi": 0,
                    "cash_amount_cents": 0,
                    "expires_at": null
                }))
                .send()
        };
        let first = create("FIRST")
            .await
            .expect("first batch creation should complete")
            .json::<serde_json::Value>()
            .await
            .expect("first batch JSON");
        let second = create("SECOND")
            .await
            .expect("second batch creation should complete")
            .json::<serde_json::Value>()
            .await
            .expect("second batch JSON");
        let first_id = first["id"].as_str().expect("first batch id");
        let second_id = second["id"].as_str().expect("second batch id");

        let filtered = client
            .get(format!(
                "http://{address}/web-admin/vouchers?batch_id={first_id}"
            ))
            .owner(&token)
            .send()
            .await
            .expect("filtered list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("filtered list JSON");
        assert_eq!(filtered.as_array().map(Vec::len), Some(2));
        assert!(filtered
            .as_array()
            .expect("voucher array")
            .iter()
            .all(|voucher| voucher["batch_id"] == first_id));

        let detail = client
            .get(format!(
                "http://{address}/web-admin/voucher-batches/{second_id}"
            ))
            .owner(&token)
            .send()
            .await
            .expect("batch detail should complete")
            .json::<serde_json::Value>()
            .await
            .expect("batch detail JSON");
        let code = detail["vouchers"][0]["code"]
            .as_str()
            .expect("voucher code");
        let fragment = code.replace('-', "")[2..8].to_ascii_lowercase();
        let searched = client
            .get(format!(
                "http://{address}/web-admin/vouchers?search={fragment}"
            ))
            .owner(&token)
            .send()
            .await
            .expect("searched list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("searched list JSON");
        assert_eq!(searched.as_array().map(Vec::len), Some(1));
        assert_eq!(searched[0]["code"], code);
    }

    #[tokio::test]
    async fn voucher_batch_create_http_happy_path() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        let created = auth(client.post(format!("http://{address}/web-admin/voucher-batches")))
            .json(&serde_json::json!({
                "prefix": "HT2P",
                "suffix_length": 8,
                "count": 3,
                "seconds_value": 600,
                "points_value_centi": 0,
                "cash_amount_cents": 0,
                "expires_at": null
            }))
            .send()
            .await
            .expect("create batch should complete");
        assert_eq!(created.status(), StatusCode::OK);
        let body = created
            .json::<serde_json::Value>()
            .await
            .expect("batch JSON");
        assert_eq!(body["prefix"], "HT2P");
        assert_eq!(body["count"], 3);
        assert_eq!(body["kind"], "promo");
        assert!(body["id"].is_string());
        assert!(body["voided_at"].is_null());

        // List includes the new batch.
        let listed = auth(client.get(format!("http://{address}/web-admin/voucher-batches")))
            .send()
            .await
            .expect("list should complete");
        assert_eq!(listed.status(), StatusCode::OK);
        let list = listed.json::<serde_json::Value>().await.expect("list JSON");
        assert!(list.as_array().is_some_and(|arr| !arr.is_empty()));
        assert_eq!(list[0]["redeemed_count"], 0);

        // Detail includes every generated card and progress.
        let batch_id = body["id"].as_str().expect("batch id");
        let detail = auth(client.get(format!(
            "http://{address}/web-admin/voucher-batches/{batch_id}"
        )))
        .send()
        .await
        .expect("detail should complete");
        assert_eq!(detail.status(), StatusCode::OK);
        let detail = detail
            .json::<serde_json::Value>()
            .await
            .expect("detail JSON");
        assert_eq!(detail["redeemed_count"], 0);
        assert_eq!(detail["vouchers"].as_array().map(Vec::len), Some(3));
        assert!(detail["vouchers"][0]["code"].is_string());

        let older = auth(client.get(format!(
            "http://{address}/web-admin/voucher-batches?limit=1&offset=1"
        )))
        .send()
        .await
        .expect("paginated list should complete")
        .json::<serde_json::Value>()
        .await
        .expect("paginated list JSON");
        assert_eq!(older.as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn voucher_batch_create_http_rejects_invalid_input() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let url = format!("http://{address}/web-admin/voucher-batches");

        // Zero values.
        let resp = auth(client.post(&url))
            .json(&serde_json::json!({
                "prefix": "BAD", "suffix_length": 8, "count": 1,
                "seconds_value": 0, "points_value_centi": 0, "cash_amount_cents": 0
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Bad prefix (too short).
        let resp = auth(client.post(&url))
            .json(&serde_json::json!({
                "prefix": "X", "suffix_length": 8, "count": 1,
                "seconds_value": 600, "points_value_centi": 0, "cash_amount_cents": 0
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Suffix length out of range.
        for suffix_length in [5, 17] {
            let resp = auth(client.post(&url))
                .json(&serde_json::json!({
                    "prefix": "BAD", "suffix_length": suffix_length, "count": 1,
                    "seconds_value": 600, "points_value_centi": 0, "cash_amount_cents": 0
                }))
                .send()
                .await
                .expect("request should complete");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }

        // Count out of range.
        let resp = auth(client.post(&url))
            .json(&serde_json::json!({
                "prefix": "BIG", "suffix_length": 8, "count": 1001,
                "seconds_value": 600, "points_value_centi": 0, "cash_amount_cents": 0
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn voucher_batch_void_http() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let account = crate::accounts::create(
            &pool,
            "batch-http-redeemer",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("account should be created");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        // Create a batch first.
        let created = auth(client.post(format!("http://{address}/web-admin/voucher-batches")))
            .json(&serde_json::json!({
                "prefix": "VOID", "suffix_length": 8, "count": 2,
                "seconds_value": 600, "points_value_centi": 0, "cash_amount_cents": 0
            }))
            .send()
            .await
            .expect("create should complete");
        assert_eq!(created.status(), StatusCode::OK);
        let batch_id = created
            .json::<serde_json::Value>()
            .await
            .expect("batch JSON")["id"]
            .as_str()
            .expect("batch id")
            .to_owned();

        let code: String = sqlx::query_scalar(
            "SELECT code FROM vouchers WHERE batch_id = ? ORDER BY code LIMIT 1",
        )
        .bind(&batch_id)
        .fetch_one(&pool)
        .await
        .expect("batch voucher should exist");
        db::redeem_voucher(&pool, &account.id, "pc-01", &code)
            .await
            .expect("voucher should redeem before void");

        let listed = auth(client.get(format!("http://{address}/web-admin/voucher-batches")))
            .send()
            .await
            .expect("list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("list JSON");
        assert_eq!(listed[0]["redeemed_count"], 1);
        assert_eq!(listed[0]["count"], 2);

        // Void it.
        let voided = auth(client.post(format!(
            "http://{address}/web-admin/voucher-batches/{batch_id}/void"
        )))
        .send()
        .await
        .expect("void should complete");
        assert_eq!(voided.status(), StatusCode::OK);
        let body = voided.json::<serde_json::Value>().await.expect("void JSON");
        assert!(body["voided_at"].is_string());

        let detail = auth(client.get(format!(
            "http://{address}/web-admin/voucher-batches/{batch_id}"
        )))
        .send()
        .await
        .expect("detail should complete")
        .json::<serde_json::Value>()
        .await
        .expect("detail JSON");
        assert_eq!(detail["redeemed_count"], 1);
        let statuses = detail["vouchers"]
            .as_array()
            .expect("vouchers array")
            .iter()
            .map(|voucher| voucher["status"].as_str().expect("voucher status"))
            .collect::<Vec<_>>();
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "redeemed")
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "voided")
                .count(),
            1
        );
        let persisted_voided_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM vouchers WHERE batch_id = ? AND status = 'voided'",
        )
        .bind(&batch_id)
        .fetch_one(&pool)
        .await
        .expect("voided status should be persisted");
        assert_eq!(persisted_voided_count, 1);

        let vouchers = auth(client.get(format!("http://{address}/web-admin/vouchers")))
            .send()
            .await
            .expect("voucher list should complete")
            .json::<serde_json::Value>()
            .await
            .expect("voucher list JSON");
        let statuses = vouchers
            .as_array()
            .expect("vouchers array")
            .iter()
            .map(|voucher| voucher["status"].as_str().expect("voucher status"))
            .collect::<Vec<_>>();
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "redeemed")
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "voided")
                .count(),
            1
        );

        let audit: (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), json_extract(metadata_json, '$.active_cards_voided')
             FROM audit_log WHERE event_type = 'voucher_batch_voided'",
        )
        .fetch_one(&pool)
        .await
        .expect("void audit should exist");
        assert_eq!(audit, (1, 1));

        // Double void is rejected.
        let again = auth(client.post(format!(
            "http://{address}/web-admin/voucher-batches/{batch_id}/void"
        )))
        .send()
        .await
        .expect("second void should complete");
        assert_eq!(again.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn voucher_template_http_is_owner_guarded_and_round_trips() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/voucher-template");

        let denied = client
            .get(&url)
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "1234").await;
        let initial = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("template GET should complete");
        assert_eq!(initial.status(), StatusCode::OK);
        let initial = initial
            .json::<serde_json::Value>()
            .await
            .expect("template JSON");
        assert!(initial["html"]
            .as_str()
            .is_some_and(|html| html.contains("{{code}}")));
        assert!(initial["default_css"]
            .as_str()
            .is_some_and(|css| css.contains("@page")));

        let updated = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({
                "html": "<article>{{code}}</article>",
                "css": "@page { margin: 5mm; }"
            }))
            .send()
            .await
            .expect("template PUT should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let updated = updated
            .json::<serde_json::Value>()
            .await
            .expect("template JSON");
        assert_eq!(updated["html"], "<article>{{code}}</article>");
        assert_eq!(updated["css"], "@page { margin: 5mm; }");

        let denied_update = client
            .put(&url)
            .json(&serde_json::json!({ "html": "x", "css": "y" }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied_update.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn voucher_batch_http_requires_auth() {
        let address = spawn(test_state().await).await;
        let client = reqwest::Client::new();

        // No cookie -> 401.
        let denied = client
            .get(format!("http://{address}/web-admin/voucher-batches"))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let denied = client
            .post(format!("http://{address}/web-admin/voucher-batches"))
            .json(&serde_json::json!({
                "prefix": "NOAUTH", "suffix_length": 8, "count": 1,
                "seconds_value": 600, "points_value_centi": 0, "cash_amount_cents": 0
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let denied = client
            .get(format!(
                "http://{address}/web-admin/voucher-batches/missing"
            ))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let denied = client
            .post(format!(
                "http://{address}/web-admin/voucher-batches/missing/void"
            ))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    // -- Remove PC (issue #112 / ADR-0044) -----------------------------------

    #[tokio::test]
    async fn owner_sign_out_http_ends_the_session_then_reports_already_ended() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let sessions = state.sessions.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        let account = crate::accounts::create(
            &pool,
            "juan",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await
        .expect("account should create");
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &account, "pc-01")
            .await
            .expect("login should succeed");

        let sign_out = |session_id: String| {
            client
                .post(format!("http://{address}/web-admin/pcs/pc-01/sign-out"))
                .owner(&token)
                .json(&serde_json::json!({ "session_id": session_id }))
                .send()
        };

        let first = sign_out(session.id.clone())
            .await
            .expect("sign-out should complete");
        assert_eq!(first.status(), StatusCode::OK);
        let body: serde_json::Value = first.json().await.expect("json body");
        assert_eq!(body["signed_out"], serde_json::json!(true));
        assert_eq!(body["username"], serde_json::json!("juan"));

        // The same click landing twice is an ordinary outcome, not an error.
        let second = sign_out(session.id.clone())
            .await
            .expect("sign-out should complete");
        assert_eq!(second.status(), StatusCode::OK);
        let body: serde_json::Value = second.json().await.expect("json body");
        assert_eq!(body["signed_out"], serde_json::json!(false));
        assert_eq!(body["username"], serde_json::json!(null));

        let sign_out_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_sign_out'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(sign_out_events, 1);
    }

    #[tokio::test]
    async fn owner_sign_out_http_requires_an_owner_session() {
        let state = test_state().await;
        let address = spawn(state).await;
        let unauthorized = reqwest::Client::new()
            .post(format!("http://{address}/web-admin/pcs/pc-01/sign-out"))
            .json(&serde_json::json!({ "session_id": "whatever" }))
            .send()
            .await
            .expect("sign-out should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn remove_pc_http_enforces_clean_row_rules_and_audits() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);

        // A clean seeded row deletes and appends one pc_removed audit event
        // whose FK pc_id is NULL but whose metadata carries the removed id.
        let ok = auth(client.delete(format!("http://{address}/web-admin/pcs/pc-01")))
            .send()
            .await
            .expect("delete should complete");
        assert_eq!(ok.status(), StatusCode::OK);
        let still_there: Option<String> =
            sqlx::query_scalar("SELECT id FROM pcs WHERE id = 'pc-01'")
                .fetch_optional(&pool)
                .await
                .expect("pc lookup");
        assert!(still_there.is_none());
        let (removed_events, null_fk): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),
                    SUM(CASE WHEN pc_id IS NULL THEN 1 ELSE 0 END)
             FROM audit_log WHERE event_type = 'pc_removed'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(removed_events, 1);
        assert_eq!(null_fk, 1);
        let meta_id: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.pc_id') FROM audit_log
             WHERE event_type = 'pc_removed'",
        )
        .fetch_one(&pool)
        .await
        .expect("metadata pc_id");
        assert_eq!(meta_id, "pc-01");

        // Coin history blocks with 409 and leaves the row in place.
        crate::db::simulate_coin(&pool, "pc-02", 100)
            .await
            .expect("coin should commit");
        let history = auth(client.delete(format!("http://{address}/web-admin/pcs/pc-02")))
            .send()
            .await
            .expect("delete should complete");
        assert_eq!(history.status(), StatusCode::CONFLICT);
        let survives: Option<String> = sqlx::query_scalar("SELECT id FROM pcs WHERE id = 'pc-02'")
            .fetch_optional(&pool)
            .await
            .expect("pc lookup");
        assert!(survives.is_some());

        // Remaining Guest Time blocks (no history: set directly).
        sqlx::query("UPDATE pcs SET remaining_seconds = 300 WHERE id = 'pc-03'")
            .execute(&pool)
            .await
            .expect("time set");
        let has_time = auth(client.delete(format!("http://{address}/web-admin/pcs/pc-03")))
            .send()
            .await
            .expect("delete should complete");
        assert_eq!(has_time.status(), StatusCode::CONFLICT);

        // Online status blocks (no history: set directly).
        sqlx::query("UPDATE pcs SET status = 'online' WHERE id = 'pc-04'")
            .execute(&pool)
            .await
            .expect("status set");
        let online = auth(client.delete(format!("http://{address}/web-admin/pcs/pc-04")))
            .send()
            .await
            .expect("delete should complete");
        assert_eq!(online.status(), StatusCode::CONFLICT);

        // An unknown id is a 404.
        let missing = auth(client.delete(format!("http://{address}/web-admin/pcs/pc-missing")))
            .send()
            .await
            .expect("delete should complete");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn remove_pc_http_deletes_bound_but_offline_mac() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "1234").await;

        // A pre-registered row whose MAC got bound on first boot, now offline
        // and history-free, removes fine — the bound MAC is not history.
        db::create_pc(&pool, "pc-77", "Counter 77", Some("192.168.1.77"))
            .await
            .expect("pre-register should succeed");
        sqlx::query("UPDATE pcs SET mac_address = 'AA:BB:CC:DD:EE:FF' WHERE id = 'pc-77'")
            .execute(&pool)
            .await
            .expect("mac bind");

        let ok = client
            .delete(format!("http://{address}/web-admin/pcs/pc-77"))
            .owner(&token)
            .send()
            .await
            .expect("delete should complete");
        assert_eq!(ok.status(), StatusCode::OK);
        let gone: Option<String> = sqlx::query_scalar("SELECT id FROM pcs WHERE id = 'pc-77'")
            .fetch_optional(&pool)
            .await
            .expect("pc lookup");
        assert!(gone.is_none());
    }

    // Stable targets on either side of the running binary, independent of version bumps.
    fn update_fixture_versions() -> (String, String) {
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("current version");
        let mut older = semver::Version::new(current.major, current.minor, current.patch);
        if older.patch > 0 {
            older.patch -= 1;
        } else if older.minor > 0 {
            older.minor -= 1;
        } else {
            older.major = older.major.checked_sub(1).expect("version above 0.0.0");
        }
        let newer = semver::Version::new(current.major, current.minor, current.patch + 1);
        (older.to_string(), newer.to_string())
    }

    fn update_release(version: &str, schema_version: i64) -> crate::app_update::Release {
        crate::app_update::Release {
            version: version.to_owned(),
            schema_version,
            notes: format!("Notes for {version}"),
            pub_date: "2026-10-13T00:00:00Z".parse().expect("date"),
            latest_json_url: format!(
                "https://github.com/{RELEASE_REPOSITORY}/releases/download/v{version}/latest.json"
            ),
            raises_schema: false,
        }
    }

    #[tokio::test]
    async fn app_update_install_requires_offered_version_and_schema_acknowledgement() {
        let mut state = test_state().await;
        state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Ok(vec![update_release("0.13.0", 59)])),
        ));
        let address = spawn(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "install-pin").await;
        client
            .post(format!("http://{address}/web-admin/update/check"))
            .owner(&auth)
            .send()
            .await
            .expect("check")
            .error_for_status()
            .expect("checked");
        for input in [
            serde_json::json!({ "version": "0.13.0" }),
            serde_json::json!({ "version": "0.13.0", "acknowledgeSchemaChange": false }),
            serde_json::json!({ "version": "0.12.0", "acknowledgeSchemaChange": true }),
        ] {
            let response = client
                .post(format!("http://{address}/web-admin/update/install"))
                .owner(&auth)
                .json(&input)
                .send()
                .await
                .expect("install gate");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{input}");
        }
        let response = client
            .post(format!("http://{address}/web-admin/update/install"))
            .owner(&auth)
            .json(&serde_json::json!({
                "version": "0.13.0", "acknowledgeSchemaChange": true,
            }))
            .send()
            .await
            .expect("acknowledged install");
        assert_eq!(response.status(), StatusCode::OK);
        let marker: String = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key = 'app_update_restart_marker'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("restart marker");
        let marker: serde_json::Value = serde_json::from_str(&marker).expect("marker JSON");
        assert_eq!(marker["fromVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(marker["targetVersion"], "0.13.0");
        assert!(marker["startedAt"].is_string());
        let audits: Vec<String> = sqlx::query_scalar("SELECT outcome FROM audit_log WHERE event_type = 'app_update_install' ORDER BY outcome")
            .fetch_all(&state.pool).await.expect("install audits");
        assert_eq!(audits, ["failure", "failure", "failure", "success"]);
    }

    #[tokio::test]
    async fn app_update_rollback_requires_exact_confirmation_and_current_candidate() {
        let mut state = test_state().await;
        state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Ok(vec![
                update_release("0.11.4", 58),
                update_release("0.11.3", 57),
                update_release("0.12.0", 58),
            ])),
        ));
        let address = spawn(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "rollback-pin").await;
        client
            .post(format!("http://{address}/web-admin/update/check"))
            .owner(&auth)
            .send()
            .await
            .expect("check")
            .error_for_status()
            .expect("checked");
        for (version, confirm) in [
            ("0.11.4", "v0.11.4"),
            ("0.11.3", "0.11.3"),
            ("0.12.0", "0.12.0"),
            ("0.1.0", "0.1.0"),
        ] {
            let response = client
                .post(format!("http://{address}/web-admin/update/rollback"))
                .owner(&auth)
                .json(&serde_json::json!({ "version": version, "confirmVersion": confirm }))
                .send()
                .await
                .expect("rollback gate");
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{version}/{confirm}"
            );
        }
        let response = client
            .post(format!("http://{address}/web-admin/update/rollback"))
            .owner(&auth)
            .json(&serde_json::json!({ "version": "0.11.4", "confirmVersion": "0.11.4" }))
            .send()
            .await
            .expect("compatible rollback");
        assert_eq!(response.status(), StatusCode::OK);
        let rows: Vec<String> = sqlx::query_scalar("SELECT outcome FROM audit_log WHERE event_type = 'app_update_rollback' ORDER BY outcome")
            .fetch_all(&state.pool).await.expect("rollback audits");
        assert_eq!(
            rows,
            ["failure", "failure", "failure", "failure", "success"]
        );
    }

    #[tokio::test]
    async fn app_update_restart_result_survives_until_ack_and_records_host_diagnostics() {
        for (target, ok) in [(env!("CARGO_PKG_VERSION"), true), ("0.13.0", false)] {
            let dir = tempfile::tempdir().expect("data directory");
            let state = test_state_in(dir.path()).await;
            let marker = serde_json::json!({
                "fromVersion": "0.11.4", "targetVersion": target,
                "startedAt": "2026-10-13T00:00:00Z",
            });
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES ('app_update_restart_marker', ?)",
            )
            .bind(marker.to_string())
            .execute(&state.pool)
            .await
            .expect("seed marker");
            state
                .run_records
                .start(env!("CARGO_PKG_VERSION"))
                .expect("run start");
            let address = spawn(state.clone()).await;
            let client = reqwest::Client::new();
            let auth = setup_and_token(&client, &address, "restart-pin").await;
            let body: serde_json::Value = client
                .get(format!("http://{address}/web-admin/update"))
                .owner(&auth)
                .send()
                .await
                .expect("state")
                .json()
                .await
                .expect("state JSON");
            assert_eq!(
                body["lastResult"],
                serde_json::json!({ "from": "0.11.4", "to": target, "ok": ok })
            );
            let history: serde_json::Value = serde_json::from_slice(
                &std::fs::read(dir.path().join("run_records.json")).expect("Run Records"),
            )
            .expect("record JSON");
            assert_eq!(history[0]["app_update_result"], body["lastResult"]);
            let diagnostics: serde_json::Value = client
                .get(format!("http://{address}/web-admin/host-diagnostics"))
                .owner(&auth)
                .send()
                .await
                .expect("diagnostics")
                .json()
                .await
                .expect("diagnostics JSON");
            assert_eq!(
                diagnostics["runs"][0]["app_update_result"],
                body["lastResult"]
            );
            let restarted_address = spawn(state.clone()).await;
            let restarted_auth = login_owner(&client, &restarted_address, "restart-pin").await;
            let repeated: serde_json::Value = client
                .get(format!("http://{restarted_address}/web-admin/update"))
                .owner(&restarted_auth)
                .send()
                .await
                .expect("repeated state")
                .json()
                .await
                .expect("JSON");
            assert_eq!(repeated["lastResult"], body["lastResult"]);
            let outcomes: Vec<String> = sqlx::query_scalar(
                "SELECT outcome FROM audit_log WHERE event_type = 'app_update_result'",
            )
            .fetch_all(&state.pool)
            .await
            .expect("outcome audit");
            assert_eq!(outcomes, [if ok { "success" } else { "failure" }]);
            let cleared: serde_json::Value = client
                .post(format!("http://{address}/web-admin/update/ack"))
                .owner(&auth)
                .send()
                .await
                .expect("ack")
                .error_for_status()
                .expect("acknowledged")
                .json()
                .await
                .expect("ack JSON");
            assert!(cleared.get("lastResult").is_none());
            let persisted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key IN ('app_update_restart_marker', 'app_update_last_result')")
                .fetch_one(&state.pool).await.expect("cleared settings");
            assert_eq!(persisted, 0);
            let ack_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'app_update_result_acknowledged'")
                .fetch_one(&state.pool).await.expect("ack audit");
            assert_eq!(ack_count, 1);
        }
    }

    #[tokio::test]
    async fn app_update_prepare_failures_are_retained_in_host_diagnostics() {
        let (older, newer) = update_fixture_versions();
        for (action, version) in [("install", newer.as_str()), ("rollback", older.as_str())] {
            let dir = tempfile::tempdir().expect("data directory");
            let mut state = test_state_in(dir.path()).await;
            state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
                std::sync::Mutex::new(Ok(vec![update_release(version, 58)])),
            ));
            state.installer = Arc::new(crate::app_update::testing::FakeInstaller {
                fail_prepare: true,
                ..Default::default()
            });
            state
                .run_records
                .start(env!("CARGO_PKG_VERSION"))
                .expect("run start");
            let address = spawn(state.clone()).await;
            let client = reqwest::Client::new();
            let auth = setup_and_token(&client, &address, "prepare-failure-pin").await;
            client
                .post(format!("http://{address}/web-admin/update/check"))
                .owner(&auth)
                .send()
                .await
                .expect("check")
                .error_for_status()
                .expect("checked");
            let response = client
                .post(format!("http://{address}/web-admin/update/{action}"))
                .owner(&auth)
                .json(&serde_json::json!({ "version": version, "confirmVersion": version }))
                .send()
                .await
                .expect("failed preparation");
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let markers: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM settings WHERE key = 'app_update_restart_marker'",
            )
            .fetch_one(&state.pool)
            .await
            .expect("marker count");
            assert_eq!(markers, 0);
            let expected = serde_json::json!({
                "from": env!("CARGO_PKG_VERSION"), "to": version, "ok": false,
            });
            let history: serde_json::Value = serde_json::from_slice(
                &std::fs::read(dir.path().join("run_records.json")).expect("Run Records"),
            )
            .expect("record JSON");
            assert_eq!(history[0]["app_update_result"], expected);
            let diagnostics: serde_json::Value = client
                .get(format!("http://{address}/web-admin/host-diagnostics"))
                .owner(&auth)
                .send()
                .await
                .expect("diagnostics")
                .json()
                .await
                .expect("diagnostics JSON");
            assert_eq!(diagnostics["runs"][0]["app_update_result"], expected);
            let failures: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = ? AND outcome = 'failure'",
            )
            .bind(format!("app_update_{action}"))
            .fetch_one(&state.pool)
            .await
            .expect("failure audit");
            assert_eq!(failures, 1);
        }
    }

    #[tokio::test]
    async fn app_update_install_failures_do_not_strand_the_owner() {
        let (_, newer) = update_fixture_versions();
        for fail_prepare in [true, false] {
            let mut state = test_state().await;
            state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
                std::sync::Mutex::new(Ok(vec![update_release(&newer, 58)])),
            ));
            state.installer = Arc::new(crate::app_update::testing::FakeInstaller {
                fail_prepare,
                fail_install: !fail_prepare,
                ..Default::default()
            });
            let address = spawn(state.clone()).await;
            let client = reqwest::Client::new();
            let auth = setup_and_token(&client, &address, "failed-install-pin").await;
            client
                .post(format!("http://{address}/web-admin/update/check"))
                .owner(&auth)
                .send()
                .await
                .expect("check")
                .error_for_status()
                .expect("checked");
            let response = client
                .post(format!("http://{address}/web-admin/update/install"))
                .owner(&auth)
                .json(&serde_json::json!({ "version": newer }))
                .send()
                .await
                .expect("failed install");
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let body: serde_json::Value = client
                .get(format!("http://{address}/web-admin/update"))
                .owner(&auth)
                .send()
                .await
                .expect("state")
                .json()
                .await
                .expect("JSON");
            assert!(body.get("download").is_none());
            if fail_prepare {
                assert!(body.get("lastResult").is_none());
                let markers: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM settings WHERE key = 'app_update_restart_marker'",
                )
                .fetch_one(&state.pool)
                .await
                .expect("marker count");
                assert_eq!(markers, 0);
            } else {
                assert_eq!(
                    body["lastResult"],
                    serde_json::json!({ "from": env!("CARGO_PKG_VERSION"), "to": newer, "ok": false })
                );
                let cleared = client
                    .post(format!("http://{address}/web-admin/update/ack"))
                    .owner(&auth)
                    .send()
                    .await
                    .expect("ack failure");
                assert_eq!(cleared.status(), StatusCode::OK);
            }
            let failures: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'app_update_install' AND outcome = 'failure'")
                .fetch_one(&state.pool).await.expect("failure audit");
            assert_eq!(failures, 1);
        }
    }

    #[tokio::test]
    async fn app_update_download_progress_blocks_duplicate_install_and_premature_ack() {
        let (_, newer) = update_fixture_versions();
        let mut state = test_state().await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        state.installer = Arc::new(crate::app_update::testing::FakeInstaller {
            download_gate: Some(gate.clone()),
            ..Default::default()
        });
        state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Ok(vec![update_release(&newer, 58)])),
        ));
        let address = spawn(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "download-pin").await;
        client
            .post(format!("http://{address}/web-admin/update/check"))
            .owner(&auth)
            .send()
            .await
            .expect("check")
            .error_for_status()
            .expect("checked");
        let request = client
            .post(format!("http://{address}/web-admin/update/install"))
            .owner(&auth)
            .json(&serde_json::json!({ "version": newer }));
        let install = tokio::spawn(async move { request.send().await.expect("install") });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let body: serde_json::Value = client
                    .get(format!("http://{address}/web-admin/update"))
                    .owner(&auth)
                    .send()
                    .await
                    .expect("progress")
                    .json()
                    .await
                    .expect("JSON");
                if body.get("download").is_some() {
                    assert_eq!(
                        body["download"],
                        serde_json::json!({ "targetVersion": newer, "percent": 25 })
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("download progress deadline");
        let duplicate = client
            .post(format!("http://{address}/web-admin/update/install"))
            .owner(&auth)
            .json(&serde_json::json!({ "version": newer }))
            .send()
            .await
            .expect("duplicate");
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        gate.add_permits(1);
        assert_eq!(
            install.await.expect("install task").status(),
            StatusCode::OK
        );
        let ack = client
            .post(format!("http://{address}/web-admin/update/ack"))
            .owner(&auth)
            .send()
            .await
            .expect("early ack");
        assert_eq!(ack.status(), StatusCode::CONFLICT);
        let marker_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM settings WHERE key = 'app_update_restart_marker'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("marker count");
        assert_eq!(marker_count, 1);
        let failures: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'app_update_result_acknowledged' AND outcome = 'failure'")
            .fetch_one(&state.pool).await.expect("ack failure audit");
        assert_eq!(failures, 1);
    }

    #[tokio::test]
    async fn app_update_check_offers_newest_stable_and_only_compatible_rollbacks() {
        let (older, newer) = update_fixture_versions();
        let mut newest = semver::Version::parse(&newer).expect("newer version");
        newest.patch += 1;
        let preview = format!(
            "{}.{}.{}-beta.1",
            newest.major,
            newest.minor,
            newest.patch + 1
        );
        let newest = newest.to_string();
        let mut state = test_state().await;
        let release = |version: &str, schema_version| crate::app_update::Release {
            version: version.to_owned(),
            schema_version,
            notes: format!("Notes for {version}"),
            pub_date: "2026-10-13T00:00:00Z".parse().expect("date"),
            latest_json_url: format!(
                "https://github.com/{RELEASE_REPOSITORY}/releases/download/v{version}/latest.json"
            ),
            raises_schema: false,
        };
        state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Ok(vec![
                release(&newer, 58),
                release("0.0.0", 58),
                release(&older, 57),
                release(&newest, 59),
                release(&older, 58),
                release(&older, 59),
                release(&preview, 58),
                release(env!("CARGO_PKG_VERSION"), 58),
            ])),
        ));
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "check-pin").await;
        let response = client
            .post(format!("http://{address}/web-admin/update/check"))
            .owner(&auth)
            .send()
            .await
            .expect("check");
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.expect("check JSON");
        assert_eq!(body["available"]["version"], newest);
        assert_eq!(body["available"]["raisesSchema"], true);
        assert_eq!(body["available"]["notes"], format!("Notes for {newest}"));
        assert_eq!(body["available"]["schemaVersion"], 59);
        assert!(body["lastCheckedAt"].is_string());
        let versions: Vec<_> = body["rollbacks"]
            .as_array()
            .expect("rollbacks")
            .iter()
            .map(|release| release["version"].as_str().expect("version"))
            .collect();
        assert_eq!(versions, [older.as_str(), "0.0.0"]);
        let state: serde_json::Value = client
            .get(format!("http://{address}/web-admin/update"))
            .owner(&auth)
            .send()
            .await
            .expect("state")
            .json()
            .await
            .expect("state JSON");
        assert_eq!(state, body);
    }

    #[tokio::test]
    async fn app_update_auto_check_is_owner_only_persisted_and_audited() {
        let state = test_state().await;
        let address = spawn(state.clone()).await;
        let client_address = spawn_client_server(&state).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "auto-check-pin").await;
        let toggle_url = format!("http://{address}/web-admin/update/auto-check");
        for (path, method) in [
            ("/web-admin/update", Method::GET),
            ("/web-admin/update/check", Method::POST),
            ("/web-admin/update/install", Method::POST),
            ("/web-admin/update/rollback", Method::POST),
            ("/web-admin/update/ack", Method::POST),
            ("/web-admin/update/auto-check", Method::PUT),
        ] {
            let unauth = client
                .request(method.clone(), format!("http://{address}{path}"))
                .json(&serde_json::json!({ "enabled": false }))
                .send()
                .await
                .expect("unauth");
            assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED, "{path}");
            let absent = client
                .request(method.clone(), format!("http://{client_address}{path}"))
                .json(&serde_json::json!({ "enabled": false }))
                .send()
                .await
                .expect("client route");
            assert_eq!(absent.status(), StatusCode::NOT_FOUND, "{path}");
            if method != Method::GET {
                let no_csrf = client
                    .request(method.clone(), format!("http://{address}{path}"))
                    .header(reqwest::header::COOKIE, cookie(&auth.token))
                    .header(reqwest::header::ORIGIN, &auth.origin)
                    .json(&serde_json::json!({ "enabled": false }))
                    .send()
                    .await
                    .expect("no csrf");
                assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN, "{path}");
                let bad_origin = client
                    .request(method, format!("http://{address}{path}"))
                    .header(reqwest::header::COOKIE, cookie(&auth.token))
                    .header(CSRF_HEADER, &auth.csrf)
                    .header(reqwest::header::ORIGIN, "http://foreign.example")
                    .json(&serde_json::json!({ "enabled": false }))
                    .send()
                    .await
                    .expect("bad origin");
                assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN, "{path}");
            }
        }
        let disabled = client
            .put(&toggle_url)
            .owner(&auth)
            .json(&serde_json::json!({ "enabled": false }))
            .send()
            .await
            .expect("disable");
        assert_eq!(disabled.status(), StatusCode::OK);
        let body: serde_json::Value = disabled.json().await.expect("toggle JSON");
        assert_eq!(body["autoCheck"], false);
        // A new Owner server/service over the same store still sees the preference.
        let mut restarted = state.clone();
        restarted.app_update = Arc::new(crate::app_update::UpdateService::default());
        let restarted_address = spawn(restarted).await;
        let restarted_auth = login_owner(&client, &restarted_address, "auto-check-pin").await;
        let body: serde_json::Value = client
            .get(format!("http://{restarted_address}/web-admin/update"))
            .owner(&restarted_auth)
            .send()
            .await
            .expect("restarted state")
            .json()
            .await
            .expect("JSON");
        assert_eq!(body["autoCheck"], false);
        let enabled = client
            .put(&toggle_url)
            .owner(&auth)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("enable");
        assert_eq!(enabled.status(), StatusCode::OK);
        assert_eq!(
            enabled
                .json::<serde_json::Value>()
                .await
                .expect("toggle JSON")["autoCheck"],
            true
        );
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'app_update_auto_check_changed' ORDER BY id",
        ).fetch_all(&state.pool).await.expect("toggle audit rows");
        assert_eq!(rows.len(), 2);
        let mut toggles: Vec<bool> = rows
            .iter()
            .map(|row| {
                serde_json::from_str::<serde_json::Value>(row).expect("metadata")["enabled"]
                    .as_bool()
                    .expect("enabled")
            })
            .collect();
        toggles.sort();
        assert_eq!(toggles, [false, true]);
    }

    #[tokio::test]
    async fn app_update_github_feed_reads_paginated_stable_release_metadata() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("feed listener");
        let base = format!("http://{}", listener.local_addr().expect("feed address"));
        let list_base = base.clone();
        let fixture = Router::new()
            .route(&format!("/repos/{RELEASE_REPOSITORY}/releases"), get(move |
                Query(query): Query<std::collections::HashMap<String, String>>, headers: HeaderMap,
            | {
                let base = list_base.clone();
                async move {
                    assert!(!headers.contains_key(header::AUTHORIZATION));
                    assert!(!headers.contains_key(header::COOKIE));
                    assert!(!headers.contains_key(CSRF_HEADER));
                    assert_eq!(query.len(), 2);
                    assert_eq!(query["per_page"], "100");
                    let listing = |version: &str| serde_json::json!({
                        "tag_name": format!("v{version}"), "draft": false, "prerelease": false,
                        "assets": [
                            { "name": "release.json", "browser_download_url": format!("{base}/v{version}/release.json") },
                            { "name": "latest.json", "browser_download_url": format!("{base}/v{version}/latest.json") },
                        ],
                    });
                    let releases = if query["page"] == "1" {
                        let mut releases = vec![listing("0.12.0")];
                        releases.extend((0..99).map(|_| serde_json::json!({
                            "tag_name": "v1.0.0", "draft": true, "prerelease": false, "assets": [],
                        })));
                        releases
                    } else {
                        vec![listing("0.13.0"), listing("0.11.4"), serde_json::json!({
                            "tag_name": "v2.0.0", "draft": false, "prerelease": true, "assets": [],
                        }), listing("9.0.0-beta.1")]
                    };
                    Json(releases)
                }
            }))
            .route("/{tag}/release.json", get(|Path(tag): Path<String>| async move {
                Json(serde_json::json!({
                    "version": tag.trim_start_matches('v'),
                    "schema_version": if tag == "v0.13.0" { 59 } else { 58 },
                    "notes": "Owner-facing release notes",
                    "pub_date": "2026-10-13T00:00:00Z",
                }))
            }));
        let fixture_task = tokio::spawn(async move {
            axum::serve(listener, fixture).await.expect("fixture");
        });
        let mut state = test_state().await;
        state.release_feed = Arc::new(
            crate::app_update::GitHubReleaseFeed::with_base_url(&base).expect("HTTP feed"),
        );
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "feed-pin").await;
        let response = client
            .post(format!("http://{address}/web-admin/update/check"))
            .owner(&auth)
            .send()
            .await
            .expect("check");
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.expect("state JSON");
        assert_eq!(body["available"]["version"], "0.13.0", "{body}");
        assert_eq!(body["available"]["notes"], "Owner-facing release notes");
        assert_eq!(
            body["available"]["latestJsonUrl"],
            format!("{base}/v0.13.0/latest.json")
        );
        assert_eq!(body["rollbacks"][0]["version"], "0.11.4");
        fixture_task.abort();
    }

    #[tokio::test]
    async fn app_update_background_checks_at_startup_and_six_hours_respect_preference() {
        let mut state = test_state().await;
        let feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Err("startup feed offline".to_owned())),
        ));
        state.release_feed = feed.clone();
        let address = spawn(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "background-pin").await;
        let task = tokio::spawn(crate::app_update::run(
            state.pool.clone(),
            state.app_update.clone(),
            state.release_feed.clone(),
        ));
        let url = format!("http://{address}/web-admin/update");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let body: serde_json::Value = client
                    .get(&url)
                    .owner(&auth)
                    .send()
                    .await
                    .expect("state")
                    .json()
                    .await
                    .expect("JSON");
                if body["lastError"] == "internal error: startup feed offline" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("startup check observable through Owner state");
        *feed.0.lock().expect("feed") = Err("six hour check".to_owned());
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(6 * 60 * 60)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        let body: serde_json::Value = client
            .get(&url)
            .owner(&auth)
            .send()
            .await
            .expect("state")
            .json()
            .await
            .expect("JSON");
        assert_eq!(body["lastError"], "internal error: six hour check");
        let disabled = client
            .put(format!("http://{address}/web-admin/update/auto-check"))
            .owner(&auth)
            .json(&serde_json::json!({ "enabled": false }))
            .send()
            .await
            .expect("disable");
        assert_eq!(disabled.status(), StatusCode::OK);
        *feed.0.lock().expect("feed") = Err("must not be fetched".to_owned());
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(6 * 60 * 60)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        let disabled_body: serde_json::Value = client
            .get(&url)
            .owner(&auth)
            .send()
            .await
            .expect("state")
            .json()
            .await
            .expect("JSON");
        assert_eq!(disabled_body["lastCheckedAt"], body["lastCheckedAt"]);
        assert_eq!(disabled_body["lastError"], "internal error: six hour check");
        task.abort();
        // Auto-check off also suppresses the startup check of a fresh service.
        let mut restarted = state.clone();
        restarted.app_update = Arc::new(crate::app_update::UpdateService::default());
        let task = tokio::spawn(crate::app_update::run(
            restarted.pool.clone(),
            restarted.app_update.clone(),
            restarted.release_feed.clone(),
        ));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let address = spawn(restarted).await;
        let auth = login_owner(&client, &address, "background-pin").await;
        let body: serde_json::Value = client
            .get(format!("http://{address}/web-admin/update"))
            .owner(&auth)
            .send()
            .await
            .expect("state")
            .json()
            .await
            .expect("JSON");
        assert_eq!(body["lastCheckedAt"], serde_json::Value::Null);
        task.abort();
    }

    #[tokio::test]
    async fn app_update_feed_errors_are_state_and_successful_recheck_clears_them() {
        let (_, newer) = update_fixture_versions();
        let mut state = test_state().await;
        let pool = state.pool.clone();
        let feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Err("feed offline".to_owned())),
        ));
        state.release_feed = feed.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "error-pin").await;
        let url = format!("http://{address}/web-admin/update/check");
        let response = client.post(&url).owner(&auth).send().await.expect("check");
        assert_eq!(response.status(), StatusCode::OK);
        let failed: serde_json::Value = response.json().await.expect("JSON");
        assert_eq!(failed["lastError"], "internal error: feed offline");
        assert!(failed["lastCheckedAt"].is_string());
        assert_eq!(failed["rollbacks"], serde_json::json!([]));
        assert!(failed.get("available").is_none());
        let stored: serde_json::Value = client
            .get(format!("http://{address}/web-admin/update"))
            .owner(&auth)
            .send()
            .await
            .expect("state")
            .json()
            .await
            .expect("JSON");
        assert_eq!(stored, failed);
        *feed.0.lock().expect("feed") = Ok(vec![update_release(&newer, 58)]);
        let successful: serde_json::Value = client
            .post(&url)
            .owner(&auth)
            .send()
            .await
            .expect("recheck")
            .json()
            .await
            .expect("JSON");
        assert!(successful.get("lastError").is_none());
        assert_eq!(successful["available"]["version"], newer);
        *feed.0.lock().expect("feed") = Err("feed offline again".to_owned());
        let failed_again: serde_json::Value = client
            .post(&url)
            .owner(&auth)
            .send()
            .await
            .expect("recheck")
            .json()
            .await
            .expect("JSON");
        assert!(failed_again.get("available").is_none());
        assert_eq!(failed_again["rollbacks"], serde_json::json!([]));
        let outcomes: Vec<String> = sqlx::query_scalar(
            "SELECT outcome FROM audit_log WHERE event_type = 'app_update_checked' ORDER BY outcome",
        ).fetch_all(&pool).await.expect("check audit");
        assert_eq!(outcomes, ["failure", "failure", "success"]);
    }

    #[tokio::test]
    async fn app_update_counts_running_guest_and_account_pcs_without_double_counting() {
        let state = test_state().await;
        for pc_id in ["pc-01", "pc-02", "pc-03"] {
            db::create_manual_adjustment(
                &state.pool,
                &ManualAdjustmentInput {
                    pc_id: pc_id.to_owned(),
                    seconds_delta: 120,
                    reason: None,
                },
            )
            .await
            .expect("guest time");
        }
        let account = accounts::create(
            &state.pool,
            "running",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await
        .expect("account");
        let account = accounts::adjust_time(&state.pool, &account.id, 240)
            .await
            .expect("account time");
        state
            .sessions
            .login(&state.pool, &state.account_tokens, &account, "pc-02")
            .await
            .expect("running account session");
        let zero = accounts::create(&state.pool, "zero", "password", AccountCreator::Admin, None)
            .await
            .expect("empty account");
        state
            .sessions
            .login(&state.pool, &state.account_tokens, &zero, "pc-03")
            .await
            .expect("empty account session");
        // PC 03's parked guest time is not currently running beneath the Account.
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "count-pin").await;
        let body: serde_json::Value = client
            .get(format!("http://{address}/web-admin/update"))
            .owner(&auth)
            .send()
            .await
            .expect("state")
            .json()
            .await
            .expect("JSON");
        assert_eq!(body["runningSessionCount"], 2);
    }

    #[tokio::test]
    async fn app_update_uses_applied_schema_max_and_semver_precedence() {
        let (older, newer) = update_fixture_versions();
        let current_build = format!("{}+build.2", env!("CARGO_PKG_VERSION"));
        let mut state = test_state().await;
        // Deliberately differ from the binary's migration constant: the route
        // must read the applied database history, not the build's known maximum.
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (59)")
            .execute(&state.pool)
            .await
            .expect("applied schema fixture");
        let release = |version: &str, schema_version| crate::app_update::Release {
            version: version.to_owned(),
            schema_version,
            notes: String::new(),
            pub_date: "2026-10-13T00:00:00Z".parse().expect("date"),
            latest_json_url: format!(
                "https://github.com/{RELEASE_REPOSITORY}/releases/download/fixture/latest.json"
            ),
            raises_schema: false,
        };
        state.release_feed = Arc::new(crate::app_update::testing::FakeReleaseFeed(
            std::sync::Mutex::new(Ok(vec![
                release(&newer, 59),
                release(&older, 59),
                release(&older, 58),
                release(&current_build, 59),
            ])),
        ));
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let auth = setup_and_token(&client, &address, "schema-pin").await;
        let body: serde_json::Value = client
            .post(format!("http://{address}/web-admin/update/check"))
            .owner(&auth)
            .send()
            .await
            .expect("check")
            .json()
            .await
            .expect("JSON");
        assert_eq!(body["currentSchemaVersion"], 59);
        assert_eq!(body["available"]["version"], newer);
        assert_eq!(body["available"]["raisesSchema"], false);
        let rollbacks = body["rollbacks"].as_array().expect("rollbacks");
        assert_eq!(rollbacks.len(), 1);
        assert_eq!(rollbacks[0]["version"], older);
        assert_eq!(rollbacks[0]["raisesSchema"], false);
    }

    #[tokio::test]
    async fn app_update_state_requires_owner_and_reports_installed_version() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/update");
        assert_eq!(
            client.get(&url).send().await.expect("state").status(),
            StatusCode::UNAUTHORIZED
        );
        let auth = setup_and_token(&client, &address, "update-pin").await;
        let response = client.get(&url).owner(&auth).send().await.expect("state");
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.expect("state JSON");
        assert_eq!(body["currentVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["currentSchemaVersion"], db::CURRENT_SCHEMA_VERSION);
        assert_eq!(body["autoCheck"], true);
        assert_eq!(body["lastCheckedAt"], serde_json::Value::Null);
        assert_eq!(body["runningSessionCount"], 0);
        assert_eq!(body["rollbacks"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn autostart_endpoint_returns_payload_and_requires_auth() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "autostart-pin").await;

        // Unauthenticated request is rejected.
        let unauth = client
            .get(format!("http://{address}/web-admin/autostart"))
            .send()
            .await
            .expect("autostart request");
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        // Authenticated request returns the exact values of all four fields.
        let response = client
            .get(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .send()
            .await
            .expect("autostart request");
        assert_eq!(response.status(), StatusCode::OK);
        let payload: serde_json::Value = response
            .json()
            .await
            .expect("autostart response should be JSON");
        // The fake autostart returns these exact values.
        assert_eq!(payload["enabled"], serde_json::json!(false));
        assert_eq!(payload["supported"], serde_json::json!(true));
        assert_eq!(payload["editable"], serde_json::json!(true));
        assert_eq!(payload["exe_path"], "/test/openpiso-admin.exe");
    }

    #[tokio::test]
    async fn autostart_put_enables_and_disables() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "toggle-pin").await;

        // Start disabled.
        let initial = client
            .get(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .send()
            .await
            .expect("initial get")
            .json::<serde_json::Value>()
            .await
            .expect("initial JSON");
        assert_eq!(initial["enabled"], serde_json::json!(false));

        // Enable.
        let enabled = client
            .put(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("enable request");
        assert_eq!(enabled.status(), StatusCode::OK);
        let payload: serde_json::Value = enabled.json().await.expect("enable response JSON");
        assert_eq!(payload["enabled"], serde_json::json!(true));

        // Disable.
        let disabled = client
            .put(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": false }))
            .send()
            .await
            .expect("disable request");
        assert_eq!(disabled.status(), StatusCode::OK);
        let payload: serde_json::Value = disabled.json().await.expect("disable response JSON");
        assert_eq!(payload["enabled"], serde_json::json!(false));

        // Audit rows: two changes, zero failures.
        let changes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'autostart_changed'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(changes, 2);
        let failures: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'autostart_change_failed'",
        )
        .fetch_one(&pool)
        .await
        .expect("failure count");
        assert_eq!(failures, 0);
    }

    #[tokio::test]
    async fn autostart_put_failure_leaves_state_unchanged_and_audits() {
        let mut state = test_state().await;
        // Replace the autostart with a failure-prone fake.
        let failing_autostart = Arc::new(crate::autostart::FakeAutostart::new(
            false,
            "/test/openpiso-admin.exe",
        ));
        failing_autostart.set_fail_writes(true);
        state.autostart = failing_autostart;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "fail-pin").await;

        let response = client
            .put(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("failing request");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // State remains unchanged.
        let fresh = client
            .get(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .send()
            .await
            .expect("get after failure")
            .json::<serde_json::Value>()
            .await
            .expect("fresh JSON");
        assert_eq!(fresh["enabled"], serde_json::json!(false));

        // Failure audit row exists.
        let failures: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'autostart_change_failed'",
        )
        .fetch_one(&pool)
        .await
        .expect("failure count");
        assert_eq!(failures, 1);
    }

    #[tokio::test]
    async fn autostart_put_refused_when_not_editable() {
        let mut state = test_state().await;
        // Wrap the fake in a thin adapter whose `editable()` returns false.
        struct ReadOnlyAutostart(crate::autostart::FakeAutostart);
        impl AutostartPort for ReadOnlyAutostart {
            fn is_enabled(&self) -> bool {
                self.0.is_enabled()
            }
            fn set_enabled(&self, enabled: bool) -> Result<(), AppError> {
                self.0.set_enabled(enabled)
            }
            fn supported(&self) -> bool {
                true
            }
            fn editable(&self) -> bool {
                false
            }
            fn exe_path(&self) -> String {
                self.0.exe_path()
            }
        }
        state.autostart = Arc::new(ReadOnlyAutostart(crate::autostart::FakeAutostart::new(
            false,
            "/test/openpiso-admin.exe",
        )));
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "ro-pin").await;

        let response = client
            .put(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("read-only request");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // State unchanged.
        let fresh = client
            .get(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .send()
            .await
            .expect("get after refusal")
            .json::<serde_json::Value>()
            .await
            .expect("fresh JSON");
        assert_eq!(fresh["enabled"], serde_json::json!(false));

        // No audit row was written (the change was refused, not attempted).
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type LIKE 'autostart_%'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn autostart_put_refused_when_not_supported() {
        struct UnsupportedAutostart(crate::autostart::FakeAutostart);
        impl AutostartPort for UnsupportedAutostart {
            fn is_enabled(&self) -> bool {
                self.0.is_enabled()
            }
            fn set_enabled(&self, enabled: bool) -> Result<(), AppError> {
                self.0.set_enabled(enabled)
            }
            fn supported(&self) -> bool {
                false
            }
            fn editable(&self) -> bool {
                true
            }
            fn exe_path(&self) -> String {
                self.0.exe_path()
            }
        }
        let mut state = test_state().await;
        state.autostart = Arc::new(UnsupportedAutostart(crate::autostart::FakeAutostart::new(
            false,
            "/test/openpiso-admin.exe",
        )));
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "unsupported-pin").await;

        let response = client
            .put(format!("http://{address}/web-admin/autostart"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("unsupported request");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn autostart_put_requires_owner_session() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        let response = client
            .put(format!("http://{address}/web-admin/autostart"))
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("unauthenticated put");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // --- PC Monitoring Check now (ADR-0076 / GitHub #340) ---

    use crate::monitoring::{testing::FakeHostProbe, HostProbeOutcome, UnsupportedPlatformProbe};

    async fn monitoring_test_state() -> (OwnerServerState, Arc<FakeHostProbe>) {
        let mut state = test_state().await;
        let probe = Arc::new(FakeHostProbe::default());
        state.host_probe = probe.clone();
        (state, probe)
    }

    async fn register_monitoring_pc(
        pool: &sqlx::SqlitePool,
        pc_id: &str,
        registered_ip: Option<&str>,
    ) {
        db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: registered_ip.map(str::to_owned),
                mac_address: None,
            },
        )
        .await
        .expect("PC settings update should succeed");
    }

    async fn audit_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(pool)
            .await
            .expect("audit count")
    }

    #[tokio::test]
    async fn pc_monitoring_endpoints_require_owner_session() {
        let (state, _probe) = monitoring_test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        for path in [
            "/web-admin/pc-monitoring/check/pc-01",
            "/web-admin/pc-monitoring/check-all",
        ] {
            let response = client
                .post(format!("http://{address}{path}"))
                .send()
                .await
                .expect("unauthenticated check should complete");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        // Denied mutations are audited like every other owner mutation.
        let denied = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'web_admin_access_denied'",
        )
        .fetch_one(&pool)
        .await
        .expect("denied audit count");
        assert_eq!(denied, 2);
    }

    #[tokio::test]
    async fn pc_monitoring_check_one_reports_facts_and_mutates_nothing() {
        let (state, probe) = monitoring_test_state().await;
        let pool = state.pool.clone();
        probe
            .script("192.168.4.10", HostProbeOutcome::Reply)
            .expect("script");
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;
        db::heartbeat_by_identity_only(&pool, "pc-01")
            .await
            .expect("heartbeat");
        let before = sqlx::query_as::<_, (String, i64, Option<String>)>(
            "SELECT status, remaining_seconds, last_seen_at FROM pcs WHERE id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await
        .expect("pc row");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "monitoring-pin").await;
        // Baseline after auth: login/setup legitimately write Audit rows.
        let audits_before = audit_count(&pool).await;

        let response = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/check/pc-01"
            ))
            .owner(&token)
            .send()
            .await
            .expect("check request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("check response should be JSON");
        assert_eq!(body["pc_id"], "pc-01");
        assert_eq!(
            body["host_reachability"],
            serde_json::json!({ "state": "reachable" })
        );
        assert_eq!(body["client_reachability"], "reachable");
        // The Guard fact is the last reported diagnostic; nothing reported
        // yet reads as unsupported, never fabricated (ADR-0076 / #344).
        assert_eq!(body["guard_coverage"], "unsupported");
        assert_eq!(probe.calls(), vec!["192.168.4.10".to_owned()]);

        // Strictly non-mutating: no Audit row, no PC status/time change.
        assert_eq!(audit_count(&pool).await, audits_before);
        let after = sqlx::query_as::<_, (String, i64, Option<String>)>(
            "SELECT status, remaining_seconds, last_seen_at FROM pcs WHERE id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await
        .expect("pc row after check");
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn pc_monitoring_check_one_never_reports_hidden_or_addressless_pc_healthy() {
        let (state, probe) = monitoring_test_state().await;
        let pool = state.pool.clone();
        // pc-01 registered but hidden; pc-02 visible without an address.
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;
        sqlx::query("UPDATE pcs SET visible = 0 WHERE id = 'pc-01'")
            .execute(&pool)
            .await
            .expect("hide pc-01");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "eligibility-pin").await;

        for (pc_id, reason) in [("pc-01", "pc_hidden"), ("pc-02", "registered_ip_missing")] {
            let response = client
                .post(format!(
                    "http://{address}/web-admin/pc-monitoring/check/{pc_id}"
                ))
                .owner(&token)
                .send()
                .await
                .expect("check request should complete");
            assert_eq!(response.status(), StatusCode::OK);
            let body = response
                .json::<serde_json::Value>()
                .await
                .expect("check response should be JSON");
            assert_eq!(
                body["host_reachability"],
                serde_json::json!({ "state": "unavailable", "reason": reason })
            );
        }
        // Neither ineligible PC was ever probed.
        assert!(probe.calls().is_empty());

        let missing = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/check/pc-99"
            ))
            .owner(&token)
            .send()
            .await
            .expect("unknown PC check should complete");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn pc_monitoring_check_reports_the_last_guard_diagnostic() {
        let (state, probe) = monitoring_test_state().await;
        let pool = state.pool.clone();
        probe
            .script("192.168.4.10", HostProbeOutcome::Reply)
            .expect("script");
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;
        db::heartbeat_by_identity_only(&pool, "pc-01")
            .await
            .expect("heartbeat");
        sqlx::query("UPDATE pcs SET last_guard_coverage = 'covered' WHERE id = 'pc-01'")
            .execute(&pool)
            .await
            .expect("store diagnostic");
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "guard-check-pin").await;

        let response = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/check/pc-01"
            ))
            .owner(&token)
            .send()
            .await
            .expect("check request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("check response should be JSON");
        assert_eq!(body["guard_coverage"], "covered");
    }

    #[tokio::test]
    async fn pc_monitoring_check_all_covers_each_eligible_pc_from_server_owned_ips() {
        let (state, probe) = monitoring_test_state().await;
        let pool = state.pool.clone();
        probe
            .script("192.168.4.101", HostProbeOutcome::Reply)
            .expect("script");
        register_monitoring_pc(&pool, "pc-02", Some("192.168.4.102")).await;
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.101")).await;
        register_monitoring_pc(&pool, "pc-03", Some("192.168.4.103")).await;
        sqlx::query("UPDATE pcs SET visible = 0 WHERE id = 'pc-03'")
            .execute(&pool)
            .await
            .expect("hide pc-03"); // pc-04 stays visible but addressless.
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "checkall-pin").await;
        // Baseline after auth: login/setup legitimately write Audit rows.
        let audits_before = audit_count(&pool).await;

        // The body is ignored by design — targets can never come from a caller.
        let response = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/check-all"
            ))
            .owner(&token)
            .json(&serde_json::json!({ "registered_ip": "10.9.9.9" }))
            .send()
            .await
            .expect("check-all should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let results = response
            .json::<Vec<serde_json::Value>>()
            .await
            .expect("check-all response should be a JSON array");
        let ids: Vec<&str> = results
            .iter()
            .map(|entry| entry["pc_id"].as_str().expect("pc_id"))
            .collect();
        assert_eq!(ids, vec!["pc-01", "pc-02"]);
        assert_eq!(
            results[0]["host_reachability"],
            serde_json::json!({ "state": "reachable" })
        );
        assert_eq!(
            results[1]["host_reachability"],
            serde_json::json!({ "state": "unreachable" })
        );
        // Only server-owned addresses of eligible PCs were probed; the
        // injected body address never was.
        assert_eq!(
            probe.calls(),
            vec!["192.168.4.101".to_owned(), "192.168.4.102".to_owned()]
        );
        // Check now writes nothing anywhere.
        assert_eq!(audit_count(&pool).await, audits_before);
    }

    #[tokio::test]
    async fn pc_monitoring_unsupported_platform_reports_unavailable_never_reachable() {
        let mut state = test_state().await;
        state.host_probe = Arc::new(UnsupportedPlatformProbe);
        let pool = state.pool.clone();
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "unsupported-pin").await;

        let response = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/check/pc-01"
            ))
            .owner(&token)
            .send()
            .await
            .expect("check request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("check response should be JSON");
        assert_eq!(
            body["host_reachability"],
            serde_json::json!({ "state": "unavailable", "reason": "platform_unsupported" })
        );
    }

    // -------------------------------------------------------------------
    // PC Monitoring continuous detection (ADR-0076 / #341)
    // -------------------------------------------------------------------

    /// Register `pc_id` as a visible PC with its own address and a live
    /// heartbeat, then age the heartbeat out so the Client reads unreachable.
    async fn seed_unreachable_pc(pool: &SqlitePool, pc_id: &str) {
        let octet: u8 = pc_id
            .split('-')
            .next_back()
            .and_then(|suffix| suffix.parse().ok())
            .unwrap_or(1);
        db::update_pc_settings(
            pool,
            pc_id,
            &PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: Some(format!("192.168.10.{octet}")),
                mac_address: None,
            },
        )
        .await
        .expect("PC settings should update");
        sqlx::query("UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = ?")
            .bind(pc_id)
            .execute(pool)
            .await
            .expect("heartbeat should age out");
    }

    #[tokio::test]
    async fn pc_monitoring_status_requires_owner_session() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{address}/web-admin/pc-monitoring/status"))
            .send()
            .await
            .expect("status request should complete");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn pc_monitoring_defaults_to_disabled_with_standing_advisory_state() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "monitor-pin").await;

        let status: serde_json::Value = client
            .get(format!("http://{address}/web-admin/pc-monitoring/status"))
            .owner(&token)
            .send()
            .await
            .expect("status request should complete")
            .json()
            .await
            .expect("status JSON");
        assert_eq!(status["enabled"], false);
        assert_eq!(status["platform_supported"], true);

        let dashboard: serde_json::Value = client
            .get(format!("http://{address}/web-admin/dashboard"))
            .owner(&token)
            .send()
            .await
            .expect("dashboard request should complete")
            .json()
            .await
            .expect("dashboard JSON");
        assert_eq!(dashboard["pc_monitoring"]["enabled"], false);
        assert_eq!(
            dashboard["pc_monitoring"]["active_incidents"],
            serde_json::json!({})
        );
    }

    /// Diagnostics Mode (ADR-0081) is Owner Session only, and only a denied
    /// *mutation* is audited — a denied read is not.
    #[tokio::test]
    async fn diagnostics_mode_requires_owner_session() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/web-admin/diagnostics-mode");

        let read = client
            .get(&url)
            .send()
            .await
            .expect("anonymous read should complete");
        assert_eq!(read.status(), StatusCode::UNAUTHORIZED);

        let write = client
            .put(&url)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("anonymous write should complete");
        assert_eq!(write.status(), StatusCode::UNAUTHORIZED);

        let denials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'web_admin_access_denied'",
        )
        .fetch_one(&pool)
        .await
        .expect("denial count");
        assert_eq!(denials, 1);
        let enabled: i64 =
            sqlx::query_scalar("SELECT enabled FROM diagnostics_mode_settings WHERE id = 1")
                .fetch_one(&pool)
                .await
                .expect("stored flag");
        assert_eq!(enabled, 0);
    }

    /// The get/set round trip: defaults off, flips under an Owner Session,
    /// is shop-wide (a second browser sees the same stored value), and
    /// audits every change with the new state in the metadata.
    #[tokio::test]
    async fn diagnostics_mode_round_trip_is_shop_wide_and_audited() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "diag-pin").await;
        let url = format!("http://{address}/web-admin/diagnostics-mode");

        async fn audit_states(pool: &sqlx::SqlitePool) -> Vec<(i64,)> {
            sqlx::query_as(
                "SELECT json_extract(metadata_json, '$.enabled') FROM audit_log
                 WHERE event_type = 'diagnostics_mode_updated'",
            )
            .fetch_all(pool)
            .await
            .expect("audit states should query")
        }

        let initial: serde_json::Value = client
            .get(&url)
            .owner(&token)
            .send()
            .await
            .expect("initial read should complete")
            .json()
            .await
            .expect("initial read JSON");
        assert_eq!(initial["enabled"], false);
        assert!(audit_states(&pool).await.is_empty());

        let enabled: serde_json::Value = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true }))
            .send()
            .await
            .expect("enable should complete")
            .json()
            .await
            .expect("enable JSON");
        assert_eq!(enabled["enabled"], true);

        // Shop-wide: a second browser — its own session from its own login,
        // sharing nothing with the first client — sees the stored value, not
        // a per-browser preference.
        let second_token = login_owner(&client, &address, "diag-pin").await;
        assert_ne!(second_token.token, token.token);
        let second_browser: serde_json::Value = reqwest::Client::new()
            .get(&url)
            .owner(&second_token)
            .send()
            .await
            .expect("second-browser read should complete")
            .json()
            .await
            .expect("second-browser JSON");
        assert_eq!(second_browser["enabled"], true);
        assert_eq!(audit_states(&pool).await, vec![(1,)]);

        let disabled: serde_json::Value = client
            .put(&url)
            .owner(&token)
            .json(&serde_json::json!({ "enabled": false }))
            .send()
            .await
            .expect("disable should complete")
            .json()
            .await
            .expect("disable JSON");
        assert_eq!(disabled["enabled"], false);
        assert_eq!(audit_states(&pool).await, vec![(1,), (0,)]);
    }

    /// Owner-server state whose Telegram service uses the given scripted
    /// doubles, for router-level Telegram tests.
    async fn test_state_with_telegram(
        transport: Arc<dyn telegram::TelegramTransport>,
        protector: Arc<dyn telegram::TokenProtector>,
    ) -> OwnerServerState {
        let mut state = test_state().await;
        state.telegram = Arc::new(telegram::TelegramService::new(
            state.pool.clone(),
            transport,
            protector,
        ));
        state
    }

    #[tokio::test]
    async fn telegram_setup_connect_and_disconnect_require_session_and_pin() {
        let transport = Arc::new(telegram::testing::FakeTransport::new());
        transport.add_bot("111:router-test-token", "router_bot");
        let state = test_state_with_telegram(
            transport.clone() as Arc<dyn telegram::TelegramTransport>,
            Arc::new(telegram::testing::FakeProtector::new(true)),
        )
        .await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        // No session cookie: every Telegram endpoint is a 401 before anything
        // else happens.
        let anonymous = client
            .get(format!("http://{address}/web-admin/telegram/status"))
            .send()
            .await
            .expect("anonymous status should complete");
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let token = setup_and_token(&client, &address, "telegram-pin").await;

        let submitted = client
            .post(format!("http://{address}/web-admin/telegram/setup/token"))
            .owner(&token)
            .json(&serde_json::json!({ "token": "111:router-test-token" }))
            .send()
            .await
            .expect("token submit should complete");
        assert_eq!(submitted.status(), StatusCode::OK);
        let candidate: serde_json::Value = submitted.json().await.expect("candidate JSON");
        assert_eq!(candidate["bot_username"], "router_bot");

        // The PIN gate on connect: a stolen session without the PIN can never
        // redirect alerts (user story 47).
        let denied = client
            .post(format!("http://{address}/web-admin/telegram/connect"))
            .owner(&token)
            .json(&serde_json::json!({ "chat_id": "515151", "current_pin": "wrong" }))
            .send()
            .await
            .expect("denied connect should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_config")
            .fetch_one(&pool)
            .await
            .expect("config count");
        assert_eq!(stored, 0);

        let connected = client
            .post(format!("http://{address}/web-admin/telegram/connect"))
            .owner(&token)
            .json(&serde_json::json!({ "chat_id": "515151", "current_pin": "telegram-pin" }))
            .send()
            .await
            .expect("connect should complete");
        assert_eq!(connected.status(), StatusCode::OK);
        let status: serde_json::Value = connected.json().await.expect("status JSON");
        assert_eq!(status["connected"], true);
        assert_eq!(status["destination"]["chat_id"], "515151");
        assert_eq!(status["destination"]["bot_username"], "router_bot");
        // Redaction at the boundary: the token appears nowhere in the response.
        assert!(!status.to_string().contains("router-test-token"));

        let denied_disconnect = client
            .post(format!("http://{address}/web-admin/telegram/disconnect"))
            .owner(&token)
            .json(&serde_json::json!({ "current_pin": "wrong" }))
            .send()
            .await
            .expect("denied disconnect should complete");
        assert_eq!(denied_disconnect.status(), StatusCode::UNAUTHORIZED);

        let disconnected = client
            .post(format!("http://{address}/web-admin/telegram/disconnect"))
            .owner(&token)
            .json(&serde_json::json!({ "current_pin": "telegram-pin" }))
            .send()
            .await
            .expect("disconnect should complete");
        assert_eq!(disconnected.status(), StatusCode::OK);
        let status: serde_json::Value = client
            .get(format!("http://{address}/web-admin/telegram/status"))
            .owner(&token)
            .send()
            .await
            .expect("status read")
            .json()
            .await
            .expect("status JSON");
        assert_eq!(status["connected"], false);
    }

    #[tokio::test]
    async fn enabling_monitoring_requires_the_current_owner_pin() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "monitor-pin").await;
        let put = |pin: &str| {
            client
                .put(format!("http://{address}/web-admin/pc-monitoring/enabled"))
                .owner(&token)
                .json(&serde_json::json!({ "enabled": true, "current_pin": pin }))
        };

        let denied = put("wrong-pin")
            .send()
            .await
            .expect("denied enable should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        // The refused PIN re-proof is itself audited (owner::authenticate).
        let failed_logins: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_login_failed'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert!(failed_logins >= 1);

        let allowed = put("monitor-pin")
            .send()
            .await
            .expect("allowed enable should complete");
        assert_eq!(allowed.status(), StatusCode::OK);
        let body: serde_json::Value = allowed.json().await.expect("status JSON");
        assert_eq!(body["enabled"], true);

        let status: serde_json::Value = client
            .get(format!("http://{address}/web-admin/pc-monitoring/status"))
            .owner(&token)
            .send()
            .await
            .expect("status read")
            .json()
            .await
            .expect("status JSON");
        assert_eq!(status["enabled"], true);
    }

    #[tokio::test]
    async fn disabling_monitoring_closes_active_incidents_as_disabled() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "monitor-pin").await;

        let enable = client
            .put(format!("http://{address}/web-admin/pc-monitoring/enabled"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true, "current_pin": "monitor-pin" }))
            .send()
            .await
            .expect("enable should complete");
        assert_eq!(enable.status(), StatusCode::OK);

        seed_unreachable_pc(&pool, "pc-02").await;
        db::monitoring::open_incident_with_audit(
            &pool,
            "pc-02",
            "Name pc-02",
            db::monitoring::IncidentKind::ClientReachabilityMismatch,
        )
        .await
        .expect("incident should open")
        .expect("open incident");

        let disable = client
            .put(format!("http://{address}/web-admin/pc-monitoring/enabled"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": false, "current_pin": "monitor-pin" }))
            .send()
            .await
            .expect("disable should complete");
        assert_eq!(disable.status(), StatusCode::OK);

        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await
                .expect("incident row");
        assert_eq!(reason.as_deref(), Some("monitoring_disabled"));
        let open_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pc_monitoring_incidents WHERE closed_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(open_count, 0);
        // The historical Alert remains while the closures and the toggle are
        // all audited.
        let alerts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_monitoring_incident_opened'",
        )
        .fetch_one(&pool)
        .await
        .expect("alert count");
        assert_eq!(alerts, 1);
        let closures: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_monitoring_incident_closed'",
        )
        .fetch_one(&pool)
        .await
        .expect("closure count");
        assert_eq!(closures, 1);
        let settings_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_monitoring_settings_updated'",
        )
        .fetch_one(&pool)
        .await
        .expect("settings event count");
        assert!(settings_events >= 1);
    }

    // --- Per-PC Monitoring Suppression (ADR-0076 / GitHub #342) ---

    async fn hide_monitoring_pc(pool: &sqlx::SqlitePool, pc_id: &str) {
        let settings = db::pc_settings(pool)
            .await
            .expect("pc settings")
            .into_iter()
            .find(|settings| settings.id == pc_id)
            .expect("seeded PC");
        db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: settings.name,
                visible: false,
                registered_ip: settings.registered_ip,
                mac_address: None,
            },
        )
        .await
        .expect("hiding the PC should succeed");
    }

    #[tokio::test]
    async fn suppression_endpoints_require_owner_session() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        let create = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/suppressions/pc-01"
            ))
            .json(&serde_json::json!({ "duration_minutes": 30, "current_pin": "x" }))
            .send()
            .await
            .expect("unauthenticated create should complete");
        assert_eq!(create.status(), StatusCode::UNAUTHORIZED);
        let cancel = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/suppressions/pc-01/cancel"
            ))
            .json(&serde_json::json!({ "current_pin": "x" }))
            .send()
            .await
            .expect("unauthenticated cancel should complete");
        assert_eq!(cancel.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn suppression_requires_the_current_owner_pin() {
        let state = monitoring_test_state().await.0;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "suppress-pin").await;
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;

        let denied = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/suppressions/pc-01"
            ))
            .owner(&token)
            .json(&serde_json::json!({ "duration_minutes": 30, "current_pin": "wrong-pin" }))
            .send()
            .await
            .expect("denied create should complete");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        // The wrong PIN re-proof is audited; no suppression row exists.
        let failed_logins: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_login_failed'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(failed_logins, 1);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pc_monitoring_suppressions")
            .fetch_one(&pool)
            .await
            .expect("suppression count");
        assert_eq!(rows, 0);

        let allowed = client
            .post(format!(
                "http://{address}/web-admin/pc-monitoring/suppressions/pc-01"
            ))
            .owner(&token)
            .json(&serde_json::json!({ "duration_minutes": 30, "current_pin": "suppress-pin" }))
            .send()
            .await
            .expect("allowed create should complete");
        assert_eq!(allowed.status(), StatusCode::OK);
        let body: serde_json::Value = allowed.json().await.expect("create JSON");
        assert_eq!(body["pc_id"], "pc-01");
        assert_eq!(body["pc_name"], "Name pc-01");
        let expires = body["expires_at"].as_str().expect("expires_at string");
        let parsed = chrono::DateTime::parse_from_rfc3339(expires).expect("RFC 3339 expiry");
        let remaining = chrono::DateTime::<chrono::Utc>::from(parsed) - chrono::Utc::now();
        assert!(
            remaining > chrono::Duration::minutes(29) && remaining <= chrono::Duration::minutes(30),
            "expiry must be Admin-computed about 30 minutes out, got {remaining:?}"
        );
    }

    #[tokio::test]
    async fn suppression_create_replace_cancel_round_trip_through_the_dashboard() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "round-trip-pin").await;
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;
        let url = |path: &str| format!("http://{address}/web-admin/pc-monitoring{path}");
        let post =
            |path: &str, body: serde_json::Value| client.post(url(path)).owner(&token).json(&body);

        async fn suppression_rows(pool: &sqlx::SqlitePool) -> i64 {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pc_monitoring_suppressions WHERE pc_id = 'pc-01'",
            )
            .fetch_one(pool)
            .await
            .expect("suppression count")
        }

        post(
            "/suppressions/pc-01",
            serde_json::json!({ "duration_minutes": 60, "current_pin": "round-trip-pin" }),
        )
        .send()
        .await
        .expect("create should complete");
        let first_expiry: String = sqlx::query_scalar(
            "SELECT expires_at FROM pc_monitoring_suppressions WHERE pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await
        .expect("first expiry");
        assert_eq!(suppression_rows(&pool).await, 1);

        // Replacing keeps one row per PC and moves the absolute expiry.
        let replaced = post(
            "/suppressions/pc-01",
            serde_json::json!({ "duration_minutes": 15, "current_pin": "round-trip-pin" }),
        )
        .send()
        .await
        .expect("replace should complete");
        assert_eq!(replaced.status(), StatusCode::OK);
        let second_expiry: String = sqlx::query_scalar(
            "SELECT expires_at FROM pc_monitoring_suppressions WHERE pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await
        .expect("second expiry");
        assert_ne!(first_expiry, second_expiry);
        assert_eq!(suppression_rows(&pool).await, 1);

        // The dashboard carries the active suppression and its expiry.
        let dashboard: serde_json::Value = client
            .get(url("/../dashboard"))
            .owner(&token)
            .send()
            .await
            .expect("dashboard read")
            .json()
            .await
            .expect("dashboard JSON");
        assert_eq!(
            dashboard["pc_monitoring"]["suppressions"]["pc-01"],
            second_expiry
        );

        // Cancelling removes it; a stale second click stays a success.
        for expected_rows in [1, 0] {
            let cancelled = post(
                "/suppressions/pc-01/cancel",
                serde_json::json!({ "current_pin": "round-trip-pin" }),
            )
            .send()
            .await
            .expect("cancel should complete");
            assert_eq!(cancelled.status(), StatusCode::OK);
            let body: serde_json::Value = cancelled.json().await.expect("cancel JSON");
            assert_eq!(body["cancelled"], expected_rows == 1);
            assert_eq!(suppression_rows(&pool).await, 0);
        }

        // Exactly one Notice per mutation: created, replaced, cancelled.
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_monitoring_suppression_updated'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(events, 3);
    }

    #[tokio::test]
    async fn suppression_validates_pc_and_duration_at_the_rust_boundary() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "boundary-pin").await;
        register_monitoring_pc(&pool, "pc-01", Some("192.168.4.10")).await;
        hide_monitoring_pc(&pool, "pc-02").await;
        let url = |path: &str| format!("http://{address}/web-admin/pc-monitoring{path}");

        let attempt =
            |path: &str, body: serde_json::Value| client.post(url(path)).owner(&token).json(&body);

        // Unknown PC is 404.
        let unknown = attempt(
            "/suppressions/pc-99",
            serde_json::json!({ "duration_minutes": 30, "current_pin": "boundary-pin" }),
        )
        .send()
        .await
        .expect("unknown pc should complete");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        // A hidden registered PC is refused.
        let hidden = attempt(
            "/suppressions/pc-02",
            serde_json::json!({ "duration_minutes": 30, "current_pin": "boundary-pin" }),
        )
        .send()
        .await
        .expect("hidden pc should complete");
        assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);

        // Duration policy is fixed at 1..=60 minutes in v1.
        for minutes in [0_i64, 61, -5] {
            let excessive = attempt(
                "/suppressions/pc-01",
                serde_json::json!({ "duration_minutes": minutes, "current_pin": "boundary-pin" }),
            )
            .send()
            .await
            .expect("invalid duration should complete");
            assert_eq!(excessive.status(), StatusCode::BAD_REQUEST);
        }

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_suppressions")
                .fetch_one(&pool)
                .await
                .expect("suppression count"),
            0
        );
    }

    /// Owner Quit (ADR-0076 / #342): the Client cannot grant itself
    /// server-owned suppression. The suppression endpoints exist only on the
    /// `:8080` owner surface behind an Owner Session + PIN step-up — the
    /// `:3210` client/device API has no such route at all.
    #[tokio::test]
    async fn client_api_surface_cannot_create_suppression() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn_client_server(&state).await;
        let client = reqwest::Client::new();

        for path in [
            "/web-admin/pc-monitoring/suppressions/pc-01",
            "/web-admin/pc-monitoring/suppressions/pc-01/cancel",
            "/web-admin/pc-monitoring/enabled",
        ] {
            let response = client
                .post(format!("http://{address}{path}"))
                .json(&serde_json::json!({ "current_pin": "123456" }))
                .send()
                .await
                .expect("client-surface request should complete");
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} must not exist on the client/device API"
            );
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_suppressions")
                .fetch_one(&pool)
                .await
                .expect("suppression count"),
            0
        );
    }

    #[tokio::test]
    async fn dashboard_badges_show_open_incidents_independently_of_acknowledgement() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "badge-pin").await;

        seed_unreachable_pc(&pool, "pc-02").await;
        db::monitoring::open_incident_with_audit(
            &pool,
            "pc-02",
            "Name pc-02",
            db::monitoring::IncidentKind::ClientReachabilityMismatch,
        )
        .await
        .expect("incident should open")
        .expect("open incident");

        let dashboard: serde_json::Value = client
            .get(format!("http://{address}/web-admin/dashboard"))
            .owner(&token)
            .send()
            .await
            .expect("dashboard request should complete")
            .json()
            .await
            .expect("dashboard JSON");

        assert_eq!(
            dashboard["pc_monitoring"]["active_incidents"]["pc-02"],
            serde_json::json!(["client_reachability_mismatch"])
        );
    }

    /// Host Probe Coverage (#343) rides the same HTTP state as every other
    /// kind: the dashboard badge reflects the durable episode, and disabling
    /// monitoring closes it as `monitoring_disabled` — never as recovered.
    #[tokio::test]
    async fn coverage_incident_shows_as_badge_and_closes_when_monitoring_disables() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "coverage-pin").await;

        let enable = client
            .put(format!("http://{address}/web-admin/pc-monitoring/enabled"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true, "current_pin": "coverage-pin" }))
            .send()
            .await
            .expect("enable should complete");
        assert_eq!(enable.status(), StatusCode::OK);

        seed_unreachable_pc(&pool, "pc-02").await;
        db::monitoring::open_incident_with_audit(
            &pool,
            "pc-02",
            "Name pc-02",
            db::monitoring::IncidentKind::HostProbeCoverage,
        )
        .await
        .expect("incident should open")
        .expect("open incident");

        let dashboard: serde_json::Value = client
            .get(format!("http://{address}/web-admin/dashboard"))
            .owner(&token)
            .send()
            .await
            .expect("dashboard request should complete")
            .json()
            .await
            .expect("dashboard JSON");
        assert_eq!(
            dashboard["pc_monitoring"]["active_incidents"]["pc-02"],
            serde_json::json!(["host_probe_coverage"])
        );

        let disable = client
            .put(format!("http://{address}/web-admin/pc-monitoring/enabled"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": false, "current_pin": "coverage-pin" }))
            .send()
            .await
            .expect("disable should complete");
        assert_eq!(disable.status(), StatusCode::OK);

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT close_reason FROM pc_monitoring_incidents WHERE kind = 'host_probe_coverage'",
        )
        .fetch_one(&pool)
        .await
        .expect("incident row");
        assert_eq!(reason.as_deref(), Some("monitoring_disabled"));
    }

    #[tokio::test]
    async fn attendance_owner_routes_validate_preview_and_version_settings() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let address = spawn(state).await;
        let client = reqwest::Client::new();

        for (method, path) in [
            (reqwest::Method::GET, "/web-admin/attendance/settings"),
            (
                reqwest::Method::POST,
                "/web-admin/attendance/settings/preview",
            ),
            (reqwest::Method::PUT, "/web-admin/attendance/settings"),
            (reqwest::Method::POST, "/web-admin/reports/attendance"),
        ] {
            let response = client
                .request(method, format!("http://{address}{path}"))
                .json(&serde_json::json!({
                    "enabled": false,
                    "daily_threshold_cents": 1000,
                    "reward_minutes": 60,
                    "claim_window_months": 1,
                    "expected_version": 1,
                    "period": "day"
                }))
                .send()
                .await
                .expect("unauthenticated Attendance request should complete");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }

        let token = setup_and_token(&client, &address, "attendance-pin").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let base = format!("http://{address}");

        let current = auth(client.get(format!("{base}/web-admin/attendance/settings")))
            .send()
            .await
            .expect("settings should load");
        assert_eq!(current.status(), StatusCode::OK);
        let current: serde_json::Value = current.json().await.expect("settings JSON");
        assert_eq!(current["version"], 1);
        assert_eq!(current["enabled"], false);
        assert_eq!(current["daily_threshold_cents"], 1000);
        assert_eq!(current["reward_seconds"], 3600);
        assert_eq!(current["claim_window_months"], 1);
        assert!(current["liability"]["outstanding_seconds"].is_number());
        assert_eq!(
            current["effective_changes"]["enabled"],
            "immediately_for_new_contributions"
        );
        assert_eq!(
            current["effective_changes"]["daily_threshold_cents"],
            "next_unfrozen_daily_snapshot"
        );
        assert_eq!(
            current["effective_changes"]["reward_minutes"],
            "next_unfrozen_daily_snapshot"
        );
        assert_eq!(
            current["effective_changes"]["claim_window_months"],
            "next_earning_month"
        );

        let floor_before: String =
            sqlx::query_scalar("SELECT utc_floor FROM attendance_clock_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .expect("clock floor should load");
        let preview = auth(client.post(format!("{base}/web-admin/attendance/settings/preview")))
            .json(&serde_json::json!({
                "enabled": true,
                "daily_threshold_cents": 2000,
                "reward_minutes": 30,
                "claim_window_months": 2
            }))
            .send()
            .await
            .expect("preview should complete");
        assert_eq!(preview.status(), StatusCode::OK);
        let preview: serde_json::Value = preview.json().await.expect("preview JSON");
        assert_eq!(preview["daily_threshold_cents"], 2000);
        assert_eq!(preview["reward_seconds"], 1800);
        assert!(preview["economics_advisory"]["effective_bonus"].is_object());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_settings_versions")
                .fetch_one(&pool)
                .await
                .expect("settings count"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT utc_floor FROM attendance_clock_state WHERE id = 1",
            )
            .fetch_one(&pool)
            .await
            .expect("clock floor should remain readable"),
            floor_before
        );

        for draft in [
            serde_json::json!({
                "enabled": true,
                "daily_threshold_cents": 0,
                "reward_minutes": 30,
                "claim_window_months": 2
            }),
            serde_json::json!({
                "enabled": true,
                "daily_threshold_cents": 2000,
                "reward_minutes": 30,
                "claim_window_months": 121
            }),
            serde_json::json!({
                "enabled": true,
                "daily_threshold_cents": 2000,
                "reward_minutes": 30,
                "claim_window_months": 2,
                "unexpected": true
            }),
        ] {
            let response =
                auth(client.post(format!("{base}/web-admin/attendance/settings/preview")))
                    .json(&draft)
                    .send()
                    .await
                    .expect("invalid preview should complete");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        let update_body = serde_json::json!({
            "enabled": true,
            "daily_threshold_cents": 2000,
            "reward_minutes": 30,
            "claim_window_months": 2,
            "expected_version": 1
        });
        let updated = auth(client.put(format!("{base}/web-admin/attendance/settings")))
            .json(&update_body)
            .send()
            .await
            .expect("settings update should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let updated: serde_json::Value = updated.json().await.expect("update JSON");
        assert_eq!(updated["outcome"], "updated");
        assert_eq!(updated["version"], 2);
        assert_eq!(updated["reward_seconds"], 1800);
        assert!(updated["effective_at"].is_string());

        let audit: (String, Option<String>, i64) = sqlx::query_as(
            "SELECT event_type, actor, COUNT(*) OVER ()
             FROM audit_log WHERE event_type = 'attendance_settings_updated'",
        )
        .fetch_one(&pool)
        .await
        .expect("settings audit event should exist");
        assert_eq!(
            audit,
            (
                "attendance_settings_updated".to_owned(),
                Some("admin".to_owned()),
                1
            )
        );

        let unchanged = auth(client.put(format!("{base}/web-admin/attendance/settings")))
            .json(&serde_json::json!({
                "enabled": true,
                "daily_threshold_cents": 2000,
                "reward_minutes": 30,
                "claim_window_months": 2,
                "expected_version": 1
            }))
            .send()
            .await
            .expect("identical stale update should complete");
        assert_eq!(unchanged.status(), StatusCode::OK);
        let unchanged: serde_json::Value = unchanged.json().await.expect("unchanged JSON");
        assert_eq!(unchanged["outcome"], "unchanged");
        assert_eq!(unchanged["version"], 2);

        let stale = auth(client.put(format!("{base}/web-admin/attendance/settings")))
            .json(&serde_json::json!({
                "enabled": true,
                "daily_threshold_cents": 3000,
                "reward_minutes": 30,
                "claim_window_months": 2,
                "expected_version": 1
            }))
            .send()
            .await
            .expect("stale update should complete");
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let stale: serde_json::Value = stale.json().await.expect("conflict JSON");
        assert_eq!(stale["current_version"], 2);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_settings_versions")
                .fetch_one(&pool)
                .await
                .expect("settings version count"),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_settings_updated'",
            )
            .fetch_one(&pool)
            .await
            .expect("settings audit count"),
            1
        );
    }

    #[tokio::test]
    async fn attendance_report_route_uses_validated_independent_pagination() {
        let state = test_state().await;
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "attendance-report-pin").await;
        let auth = |request: reqwest::RequestBuilder| request.owner(&token);
        let url = format!("http://{address}/web-admin/reports/attendance");

        let response = auth(client.post(&url))
            .json(&serde_json::json!({
                "period": "day",
                "month_limit": 7,
                "month_offset": 2,
                "account_limit": 13,
                "account_offset": 4
            }))
            .send()
            .await
            .expect("Attendance report should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let report: serde_json::Value = response.json().await.expect("Attendance report JSON");
        assert_eq!(report["month_page"]["limit"], 7);
        assert_eq!(report["month_page"]["offset"], 2);
        assert_eq!(report["account_page"]["limit"], 13);
        assert_eq!(report["account_page"]["offset"], 4);
        assert!(report["liability"]["outstanding_seconds"].is_number());

        for body in [
            serde_json::json!({"period": "day", "month_offset": -1}),
            serde_json::json!({"period": "day", "unknown": 1}),
        ] {
            let response = auth(client.post(&url))
                .json(&body)
                .send()
                .await
                .expect("invalid Attendance report should complete");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn non_windows_admin_refuses_to_enable_monitoring() {
        let mut state = test_state().await;
        let mut probe = crate::monitoring::testing::FakeHostProbe::default();
        probe.set_supported(false);
        state.host_probe = Arc::new(probe);
        let address = spawn(state).await;
        let client = reqwest::Client::new();
        let token = setup_and_token(&client, &address, "monitor-pin").await;

        let response = client
            .put(format!("http://{address}/web-admin/pc-monitoring/enabled"))
            .owner(&token)
            .json(&serde_json::json!({ "enabled": true, "current_pin": "monitor-pin" }))
            .send()
            .await
            .expect("refused enable should complete");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let status: serde_json::Value = client
            .get(format!("http://{address}/web-admin/pc-monitoring/status"))
            .owner(&token)
            .send()
            .await
            .expect("status read")
            .json()
            .await
            .expect("status JSON");
        assert_eq!(status["platform_supported"], false);
        assert_eq!(status["enabled"], false);
    }
}

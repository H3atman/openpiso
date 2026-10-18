use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        ConnectInfo, FromRef, FromRequestParts, Path, State,
    },
    http::{request::Parts, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::{stream, Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use tokio_stream::wrappers::BroadcastStream;
use tracing::warn;
use uuid::Uuid;

#[cfg(test)]
use crate::account_import;
use crate::{
    account_login_throttle::{self, AccountLoginThrottle},
    accounts::{self, AccountCreator},
    audit::{self, NewAuditEvent},
    auth::{AccountToken, AccountTokens},
    db,
    diagnostic_request::DiagnosticRequestRegistry,
    errors::AppError,
    events::EventHub,
    forwarded_log::{ForwardedDiagnosticBatch, ForwardedLogSink},
    guard_poll::{GuardFacts, GuardRegistry},
    models::{
        AccountAuthResponse, AccountCredentials, AccountPointsTransferInput, AccountTransferInput,
        ChangePasswordInput, ClientWheelSettings, CoinEventInput, CoinPromoMenu, CoinSourceClaim,
        CoinTransaction, GuardForwardedLines, GuardStateInput, GuardStateResponse, HealthStatus,
        HeartbeatInput, HeartbeatResponse, MutationAck, OwnerPinVerification, OwnerPinVerifyInput,
        Pc, PointsRedeemInput, PointsTransferRequestCreated, RedemptionTierMenu, ShopInfo,
        ShopStatus, TamperKillAck, ThemeSettings, VoucherRedeemInput, WheelSpinResponse,
    },
    sessions::{IdentityHeartbeatInput, SessionStore, TamperKillReport, TamperKillReportAge},
    shutdown,
};

type HttpResult<T> = Result<Json<T>, AppError>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttendanceClaimInput {
    request_id: String,
    earning_month: String,
}

const MAC_HEADER: &str = "x-openpiso-mac";

/// How often a Guard should poll. Matches the PC Monitoring cadence so the
/// Admin never waits longer for a Guard fact than for its own probe round.
const GUARD_POLL_INTERVAL_SECS: u64 = 10;

#[derive(Clone)]
struct ServerState {
    pool: SqlitePool,
    events: EventHub,
    account_tokens: AccountTokens,
    sessions: SessionStore,
    /// Forwarded Diagnostics sink (ADR-0078): accepted device batches are
    /// appended to the dedicated rotating forwarded log, never to SQLite.
    forwarded_log: Arc<ForwardedLogSink>,
    account_login_throttle: AccountLoginThrottle,
    /// Live Guard Poll facts and power-off orders (ADR-0077). Shared with the
    /// monitoring loop, which is the only writer of orders.
    guards: GuardRegistry,
    /// Armed Diagnostic Requests (ADR-0078). The owner arms one from the Web
    /// Admin; the Heartbeat delivers its nonce and the Forwarded Diagnostics
    /// endpoint consumes it when the Client's slice answer arrives.
    diagnostic_requests: DiagnosticRequestRegistry,
}

impl FromRef<ServerState> for AccountTokens {
    fn from_ref(state: &ServerState) -> Self {
        state.account_tokens.clone()
    }
}

// The client API threads one handle per concern; a bundle type would only
// hide the same list from the two callers that must agree on it.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    pool: SqlitePool,
    events: EventHub,
    account_tokens: AccountTokens,
    sessions: SessionStore,
    account_login_throttle: AccountLoginThrottle,
    guards: GuardRegistry,
    forwarded_log: Arc<ForwardedLogSink>,
    diagnostic_requests: DiagnosticRequestRegistry,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 3210))
        .await
        .map_err(|error| format!("could not bind 0.0.0.0:3210: {error}"))?;
    // `into_make_service_with_connect_info` makes the peer address
    // available to handlers via `ConnectInfo<SocketAddr>` (ADR-0031 /
    // plan Â§4 M2). The source IP is the only authority for identity
    // bootstrap; we never read it from a body or header.
    axum::serve(
        listener,
        router_with_throttle(
            pool,
            events,
            account_tokens,
            sessions,
            account_login_throttle,
            guards,
            forwarded_log,
            diagnostic_requests,
        )
        .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|error| error.to_string())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn router(
    pool: SqlitePool,
    events: EventHub,
    account_tokens: AccountTokens,
    sessions: SessionStore,
    forwarded_log: Arc<ForwardedLogSink>,
) -> Router {
    router_with_throttle(
        pool,
        events,
        account_tokens,
        sessions,
        AccountLoginThrottle::default(),
        GuardRegistry::new(),
        forwarded_log,
        DiagnosticRequestRegistry::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn router_with_throttle(
    pool: SqlitePool,
    events: EventHub,
    account_tokens: AccountTokens,
    sessions: SessionStore,
    account_login_throttle: AccountLoginThrottle,
    guards: GuardRegistry,
    forwarded_log: Arc<ForwardedLogSink>,
    diagnostic_requests: DiagnosticRequestRegistry,
) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/shop", get(shop_info))
        .route("/api/shop-status", get(shop_status_handler))
        .route("/api/theme", get(public_theme))
        .route("/api/owner/verify-pin", post(owner_verify_pin))
        .route("/api/pcs", get(list_pcs))
        .route("/api/pcs/{id}", get(get_pc))
        // Client identity endpoints (ADR-0031) are MAC-keyed: the
        // server resolves identity from the `X-OpenPiso-Mac` header
        // plus the connection peer address (never a body-supplied IP).
        // The `{pc_id}` path is gone from the client surface and lives
        // only on owner / web-admin routes.
        .route("/api/identity/heartbeat", post(identity_heartbeat))
        .route("/api/identity/disconnect", post(identity_disconnect))
        .route("/api/identity/events", get(identity_events))
        .route("/api/coin-events", post(coin_event))
        .route("/api/accounts/signup", post(account_signup))
        .route("/api/accounts/login", post(account_login))
        .route("/api/accounts/claim", post(account_claim))
        .route("/api/accounts/logout", post(account_logout))
        .route("/api/attendance", get(attendance_summary))
        .route(
            "/api/attendance/months/{earning_month}",
            get(attendance_month),
        )
        .route("/api/attendance/claims", post(attendance_claim))
        .route("/api/accounts/transfer", post(account_transfer))
        .route(
            "/api/accounts/transfer-points",
            post(account_transfer_points),
        )
        .route(
            "/api/accounts/change-password",
            post(account_change_password),
        )
        .route("/api/vouchers/redeem", post(voucher_redeem))
        .route("/api/points/redemption-tiers", get(points_redemption_tiers))
        .route("/api/coin-promo", get(coin_promo_menu))
        .route("/api/points/redeem", post(points_redeem))
        .route("/api/wheel/spin", post(wheel_spin))
        .route("/api/wheel/settings", get(wheel_settings_handler))
        // Forwarded Diagnostics batch endpoint (ADR-0078). Device-keyed like
        // the identity endpoints: the PC is resolved read-only from the
        // `X-OpenPiso-Mac` header and accepted lines are appended to the
        // dedicated forwarded log. Deliberately not the Heartbeat â€” a
        // diagnostic payload must never delay or fail timer synchronisation.
        .route(
            "/api/forwarded-diagnostics",
            post(receive_forwarded_diagnostics),
        )
        // Client Guard Poll (ADR-0077). MAC-identified like the heartbeat and
        // outbound-only: the Guard reports its own facts and reads back the
        // desired power state. It never creates or rebinds a PC row, and it
        // carries no session, money, time, or Points authority.
        .route("/api/guard/state", post(guard_state))
        .with_state(ServerState {
            pool,
            events,
            account_tokens,
            sessions,
            forwarded_log,
            account_login_throttle,
            guards,
            diagnostic_requests,
        })
}

/// Read the trimmed `X-OpenPiso-Mac` header value, when present. The
/// header is the *identity hint* and is always optional: an empty or
/// missing header still allows IP-bootstrap binding.
fn read_mac_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(MAC_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Parse the `Authorization: Bearer <token>` header (ADR-0032). Returns the
/// raw token string when the header is present and well-formed; `None`
/// otherwise. Mirrors `cookie_token` in `web_admin.rs` â€” no new dependency.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("Authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?
        .trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_owned())
}

/// Account-authentication gate for the LAN client/device API (ADR-0032).
/// Mirrors the owner server's `OwnerSession` but deliberately does **not**
/// audit denials â€” after dropping the `pc_id` cross-check, the only
/// account-side denial is "token not in the map," which occurs routinely
/// and benignly after logout, auto-kick (ADR-0002), or PC takeover
/// (ADR-0007). A future tamper-response feature may add a targeted
/// invalid-token signal here.
///
/// Handlers read `session.pc_id` / `session.account_id` from the resolved
/// session instead of from the request body. The raw token string is also
/// exposed because `account_logout` needs it to revoke the token.
pub struct AccountSession {
    pub token: AccountToken,
    pub raw_token: String,
}

impl<S> FromRequestParts<S> for AccountSession
where
    S: Send + Sync,
    AccountTokens: FromRef<S>,
{
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let tokens = AccountTokens::from_ref(state);
        let raw = bearer_token(&parts.headers).ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "missing or malformed Authorization header; expected 'Bearer <token>'".to_owned(),
            )
        })?;
        let account_token = tokens.get(&raw).await.ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "invalid or revoked account token".to_owned(),
            )
        })?;
        // Tamper-response note (ADR-0032 Â§5): denials are deliberately not
        // audited here. A future targeted invalid-token signal should be
        // added in this method, not in individual handlers.
        Ok(AccountSession {
            token: account_token,
            raw_token: raw,
        })
    }
}

/// Canonical "machine is unregistered" body (ADR-0031). The client
/// reads this exact body and maps it to the `"unregistered"` state.
fn unregistered_rejection() -> Response {
    (
        StatusCode::NOT_FOUND,
        serde_json::to_string(&json!({ "error": "pc_unregistered" }))
            .unwrap_or_else(|_| r#"{"error":"pc_unregistered"}"#.to_owned()),
    )
        .into_response()
}

async fn account_signup(
    State(state): State<ServerState>,
    Json(input): Json<AccountCredentials>,
) -> HttpResult<AccountAuthResponse> {
    match require_shop_open(&state.pool).await {
        Ok(()) => {}
        Err(AppError::ShopClosed) => {
            accounts::audit_shop_closed_signup(
                &state.pool,
                &input.username,
                input.pc_id.as_deref(),
            )
            .await?;
            return Err(AppError::ShopClosed);
        }
        Err(error) => return Err(error),
    }
    accounts::create(
        &state.pool,
        &input.username,
        &input.password,
        AccountCreator::SelfService,
        input.pc_id.as_deref(),
    )
    .await?;
    let account = accounts::authenticate_for_session(
        &state.pool,
        &input.username,
        &input.password,
        input.pc_id.as_deref(),
    )
    .await?;
    establish_session(&state, account, &input).await
}

async fn account_login(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<AccountCredentials>,
) -> HttpResult<AccountAuthResponse> {
    let normalized_username =
        accounts::active_username_normalized(&state.pool, &input.username).await?;
    let now = chrono::Utc::now();
    let source_ip = peer.ip();

    // Account login throttling is checked before Shop Hours and credential
    // verification. A locked Username therefore produces no shop-closed
    // audit row and never exercises its Password.
    if let Some(username_normalized) = normalized_username.as_deref() {
        if let crate::login_throttle::Decision::Locked { retry_after_secs } = state
            .account_login_throttle
            .check(username_normalized, now)
            .await
        {
            return Err(AppError::Throttled { retry_after_secs });
        }
    }

    match require_shop_open(&state.pool).await {
        Ok(()) => {}
        Err(AppError::ShopClosed) => {
            accounts::audit_shop_closed_login(&state.pool, &input.username, input.pc_id.as_deref())
                .await?;
            return Err(AppError::ShopClosed);
        }
        Err(error) => return Err(error),
    }
    let account = match accounts::authenticate_for_session(
        &state.pool,
        &input.username,
        &input.password,
        input.pc_id.as_deref(),
    )
    .await
    {
        Ok(account) => {
            if let Some(username_normalized) = normalized_username.as_deref() {
                state
                    .account_login_throttle
                    .record_success(username_normalized)
                    .await;
            }
            account
        }
        Err(error) => {
            if let (Some(username_normalized), true) = (
                normalized_username.as_deref(),
                matches!(&error, AppError::Unauthorized(_)),
            ) {
                if let Some(lockout_seconds) = state
                    .account_login_throttle
                    .record_failure(username_normalized, now)
                    .await
                {
                    accounts::audit_login_throttled(
                        &state.pool,
                        username_normalized,
                        &source_ip.to_string(),
                        input.pc_id.as_deref(),
                        account_login_throttle::FAILURE_THRESHOLD,
                        lockout_seconds,
                    )
                    .await?;
                }
            }
            return Err(error);
        }
    };
    establish_session(&state, account, &input).await
}

async fn establish_session(
    state: &ServerState,
    account: crate::models::Account,
    input: &AccountCredentials,
) -> HttpResult<AccountAuthResponse> {
    let pc_id = input
        .pc_id
        .as_deref()
        .ok_or_else(|| AppError::InvalidInput("PC ID is required".to_owned()))?;
    let (session, token) = state
        .sessions
        .login_after_authentication(&state.pool, &state.account_tokens, &account, pc_id)
        .await?;
    publish_all_pcs(state).await;
    Ok(Json(AccountAuthResponse {
        token,
        account,
        session,
    }))
}

async fn account_claim(
    State(state): State<ServerState>,
    session: AccountSession,
) -> HttpResult<AccountAuthResponse> {
    let claimed_session = state.sessions.claim(&state.pool, &session.token).await?;
    let account = accounts::get(&state.pool, &session.token.account_id).await?;
    publish_pc(&state, &session.token.pc_id).await;
    Ok(Json(AccountAuthResponse {
        token: session.raw_token,
        account,
        session: claimed_session,
    }))
}

async fn attendance_summary(
    State(state): State<ServerState>,
    session: Result<AccountSession, (StatusCode, String)>,
) -> HttpResult<db::attendance_client::AttendanceSummary> {
    let session = session.map_err(|_| AppError::AttendanceUnauthorized)?;
    state
        .sessions
        .validate_attendance_session(&session.token)
        .await
        .map_err(AppError::for_attendance_route)?;
    let snapshot =
        db::attendance_client::account_snapshot(&state.pool, &session.token.account_id, None)
            .await
            .map_err(AppError::for_attendance_route)?;
    Ok(Json(snapshot.summary))
}

async fn attendance_month(
    State(state): State<ServerState>,
    session: Result<AccountSession, (StatusCode, String)>,
    earning_month: Result<Path<String>, PathRejection>,
) -> HttpResult<db::attendance_client::AttendanceMonth> {
    let session = session.map_err(|_| AppError::AttendanceUnauthorized)?;
    state
        .sessions
        .validate_attendance_session(&session.token)
        .await
        .map_err(AppError::for_attendance_route)?;
    let Path(earning_month) = earning_month.map_err(|_| {
        AppError::AttendanceInvalidInput("earning_month must be canonical YYYY-MM".to_owned())
    })?;
    let snapshot = db::attendance_client::account_snapshot(
        &state.pool,
        &session.token.account_id,
        Some(&earning_month),
    )
    .await
    .map_err(AppError::for_attendance_route)?;
    if !snapshot
        .summary
        .calendar_months
        .iter()
        .any(|month| month.earning_month == earning_month)
    {
        return Err(AppError::AttendanceNotFound);
    }
    snapshot
        .requested_month
        .map(Json)
        .ok_or_else(|| AppError::Internal("Attendance month projection is missing".to_owned()))
}

async fn attendance_claim(
    State(state): State<ServerState>,
    session: Result<AccountSession, (StatusCode, String)>,
    body: Result<Json<AttendanceClaimInput>, JsonRejection>,
) -> HttpResult<db::attendance_client::AttendanceClaimResponse> {
    let session = session.map_err(|_| AppError::AttendanceUnauthorized)?;
    state
        .sessions
        .validate_attendance_session(&session.token)
        .await
        .map_err(AppError::for_attendance_route)?;
    let Json(input) = body.map_err(|_| {
        AppError::AttendanceInvalidInput(
            "body must contain request_id and earning_month only".to_owned(),
        )
    })?;
    let request_id = Uuid::parse_str(&input.request_id)
        .ok()
        .filter(|request_id| request_id.to_string() == input.request_id)
        .ok_or_else(|| {
            AppError::AttendanceInvalidInput(
                "request_id must be a canonical hyphenated UUID".to_owned(),
            )
        })?;
    let outcome = state
        .sessions
        .claim_attendance(
            &state.pool,
            &session.token,
            request_id,
            &input.earning_month,
        )
        .await
        .map_err(AppError::for_attendance_route)?;
    let claim = match outcome {
        db::attendance_claims::ClaimOutcome::Claimed { claim, .. } => claim,
        db::attendance_claims::ClaimOutcome::Denied { reason, .. } => {
            return Err(AppError::AttendanceClaimDenied {
                reason: reason.as_str().to_owned(),
            });
        }
    };
    publish_pc(&state, &session.token.pc_id).await;
    let snapshot = db::attendance_client::account_snapshot(
        &state.pool,
        &session.token.account_id,
        Some(&claim.earning_month),
    )
    .await
    .map_err(AppError::for_attendance_route)?;
    let month = snapshot
        .requested_month
        .map(|month| month.summary)
        .ok_or_else(|| AppError::Internal("Attendance claim projection is missing".to_owned()))?;
    Ok(Json(db::attendance_client::AttendanceClaimResponse {
        claim: db::attendance_client::AttendanceClaimFact {
            audit_event_id: claim.audit_event_id,
            request_id: claim.request_id,
            earning_month: claim.earning_month,
            claimed_seconds: claim.claimed_seconds,
            created_at: claim.created_at,
        },
        remaining_seconds: snapshot.remaining_seconds,
        totals: snapshot.summary.totals,
        month,
        as_of: snapshot.summary.as_of,
        admin_local_date: snapshot.summary.admin_local_date,
        admin_local_month: snapshot.summary.admin_local_month,
    }))
}

async fn account_logout(
    State(state): State<ServerState>,
    session: AccountSession,
) -> HttpResult<Pc> {
    state.sessions.logout(&state.pool, &session.token).await?;
    state.account_tokens.revoke(&session.raw_token).await;
    let pc = state
        .sessions
        .enrich_pc(db::pc(&state.pool, &session.token.pc_id).await?)
        .await;
    let pc = state.events.publish_pc_updated(pc);
    Ok(Json(pc))
}

/// Public, read-only shop identity for the Client Lock (ADR-0027) plus
/// non-secret UX timing. Never returns the PIN or any secret.
async fn shop_info(State(state): State<ServerState>) -> HttpResult<ShopInfo> {
    let shop_name = db::shop_name(&state.pool).await?;
    let shutdown_grace_seconds = db::shutdown_grace_seconds(&state.pool).await?;
    let offline_grace_seconds = db::offline_grace_seconds(&state.pool).await?;
    let theme_version = db::theme_version(&state.pool).await?;
    let wheel_enabled = db::wheel_enabled(&state.pool).await?;
    Ok(Json(ShopInfo {
        shop_name,
        shutdown_grace_seconds,
        offline_grace_seconds,
        theme_version,
        wheel_enabled,
    }))
}

async fn public_theme(State(state): State<ServerState>) -> HttpResult<ThemeSettings> {
    db::theme_settings(&state.pool).await.map(Json)
}

async fn shop_status_handler(State(state): State<ServerState>) -> HttpResult<ShopStatus> {
    db::shop_status(&state.pool).await.map(Json)
}

async fn require_shop_open(pool: &SqlitePool) -> Result<(), AppError> {
    if db::shop_status(pool).await?.open {
        Ok(())
    } else {
        Err(AppError::ShopClosed)
    }
}

/// Client-side owner gate (ADR-0027 Â§4.2). The Client posts the typed PIN and
/// the server compares it against the hashed `owner_pin` setting. A wrong or
/// unconfigured PIN is HTTP 200 with `ok: false` so the Client can tell "wrong
/// PIN" apart from a transport failure.
///
/// On a **successful** verify the response also carries the stored strong
/// hash so the Client can cache it for the offline fallback (ADR-0027 Â§3).
/// The hash is returned only to a caller that has just proven the PIN, never
/// on a wrong/unconfigured PIN, so an unauthenticated LAN client cannot pull
/// it. The live online check still compares server-side; the cached hash is
/// only used when the admin server is later unreachable.
async fn owner_verify_pin(
    State(state): State<ServerState>,
    Json(input): Json<OwnerPinVerifyInput>,
) -> HttpResult<OwnerPinVerification> {
    let ok = db::verify_owner_pin(&state.pool, &input.pin).await?;
    let hash = if ok {
        db::get_owner_pin_hash(&state.pool).await?
    } else {
        None
    };
    Ok(Json(OwnerPinVerification { ok, hash }))
}

/// Immediate, server-authoritative **Time Transfer** to another account
/// (ADR-0030). The recipient and amount are resolved/validated server-side;
/// the caller's account id comes from the verified token, never the body.
/// Returns the refreshed `AccountAuthState` for the sender.
async fn account_transfer(
    State(state): State<ServerState>,
    session: AccountSession,
    Json(input): Json<AccountTransferInput>,
) -> HttpResult<AccountAuthResponse> {
    let transfer = state
        .sessions
        .transfer_time(
            &state.pool,
            &session.token,
            &input.to_username,
            input.seconds,
        )
        .await?;
    let account = accounts::get(&state.pool, &session.token.account_id).await?;
    publish_pc(&state, &session.token.pc_id).await;
    // Refresh the recipient's PC too when they are signed in elsewhere, so the
    // credited time shows immediately instead of waiting for their next tick.
    if let Some(recipient_pc_id) = transfer.recipient_pc_id {
        if recipient_pc_id != session.token.pc_id {
            publish_pc(&state, &recipient_pc_id).await;
        }
    }
    Ok(Json(AccountAuthResponse {
        token: session.raw_token,
        account,
        session: transfer.sender,
    }))
}

/// Owner-confirmed **Points Transfer Request** (ADR-0030). Creates a durable
/// pending request and returns the single-use 6-digit `code` for the customer
/// to read to the owner. No points move until the owner confirms in web-admin.
async fn account_transfer_points(
    State(state): State<ServerState>,
    session: AccountSession,
    Json(input): Json<AccountPointsTransferInput>,
) -> HttpResult<PointsTransferRequestCreated> {
    accounts::request_points_transfer(
        &state.pool,
        &session.token.account_id,
        &input.to_username,
        input.points_centi,
    )
    .await
    .map(Json)
}

/// Customer self-service Password Change (ADR-0037). Keeps the Account Session
/// and Active Session valid and never exposes hashes or database diagnostics.
async fn account_change_password(
    State(state): State<ServerState>,
    session: AccountSession,
    Json(input): Json<ChangePasswordInput>,
) -> Response {
    let result = accounts::change_password(
        &state.pool,
        &session.token.account_id,
        &input.old_password,
        &input.new_password,
    )
    .await;

    password_change_response(&session.token.account_id, result)
}

fn password_change_response(
    account_id: &str,
    result: Result<crate::models::Account, AppError>,
) -> Response {
    // Build the response body directly from the match outcome so the
    // customer never sees a leaky implementation-level message.
    match result {
        Ok(_) => Json(MutationAck { ok: true }).into_response(),
        Err(AppError::Conflict(_)) => {
            (StatusCode::CONFLICT, accounts::PASSWORD_CHANGE_CONFLICT_MSG).into_response()
        }
        Err(AppError::NotFound(_)) => {
            (StatusCode::UNAUTHORIZED, "invalid username or password").into_response()
        }
        Err(
            error @ (AppError::Io(_)
            | AppError::Database(_)
            | AppError::Internal(_)
            | AppError::Password(_)
            | AppError::Pricing(_)
            | AppError::Window(_)),
        ) => {
            tracing::error!(
                account_id,
                error = %error,
                "Password Change failed"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "password change failed").into_response()
        }
        Err(AppError::Unauthorized(msg)) => (StatusCode::UNAUTHORIZED, msg).into_response(),
        Err(AppError::InvalidInput(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(AppError::ShopClosed) => (StatusCode::CONFLICT, "shop_closed").into_response(),
        Err(error @ AppError::Throttled { .. }) => error.into_response(),
        // Telegram can never be part of the account password-change surface.
        Err(AppError::TelegramUnavailable(_) | AppError::TelemetryUnavailable(_)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "password change failed").into_response()
        }
        Err(
            AppError::AttendanceSettingsConflict { .. }
            | AppError::AttendanceUnauthorized
            | AppError::AttendanceInvalidInput(_)
            | AppError::AttendanceNotFound
            | AppError::AttendanceClaimDenied { .. }
            | AppError::RestoreRejected { .. }
            | AppError::SpinPasswordRejected(_)
            | AppError::UnsupportedMigrationHistory(_),
        ) => (StatusCode::INTERNAL_SERVER_ERROR, "password change failed").into_response(),
    }
}

async fn voucher_redeem(
    State(state): State<ServerState>,
    session: AccountSession,
    Json(input): Json<VoucherRedeemInput>,
) -> HttpResult<AccountAuthResponse> {
    require_shop_open(&state.pool).await?;
    let claimed_session = state
        .sessions
        .redeem_voucher(&state.pool, &session.token, &input.code)
        .await?;
    let account = accounts::get(&state.pool, &session.token.account_id).await?;
    publish_pc(&state, &session.token.pc_id).await;
    Ok(Json(AccountAuthResponse {
        token: session.raw_token,
        account,
        session: claimed_session,
    }))
}

async fn points_redemption_tiers(
    State(state): State<ServerState>,
    _session: AccountSession,
) -> HttpResult<RedemptionTierMenu> {
    db::redemption_tier_menu(&state.pool).await.map(Json)
}

async fn coin_promo_menu(
    State(state): State<ServerState>,
    _session: AccountSession,
) -> HttpResult<CoinPromoMenu> {
    db::coin_promo_menu(&state.pool).await.map(Json)
}

async fn points_redeem(
    State(state): State<ServerState>,
    session: AccountSession,
    Json(input): Json<PointsRedeemInput>,
) -> HttpResult<AccountAuthResponse> {
    let claimed_session = state
        .sessions
        .redeem_points(&state.pool, &session.token, input.tier_id, input.version)
        .await?;
    let account = accounts::get(&state.pool, &session.token.account_id).await?;
    publish_pc(&state, &session.token.pc_id).await;
    Ok(Json(AccountAuthResponse {
        token: session.raw_token,
        account,
        session: claimed_session,
    }))
}

/// Optional body of `POST /api/wheel/spin`. Old clients send none.
#[derive(Deserialize)]
struct WheelSpinInput {
    password: Option<String>,
}

/// Spin Password Confirmation: when the owner requires it, the Account's
/// password must verify before the spin transaction starts. Failures share
/// the Account login throttle bucket, so a locked Username is refused before
/// its password is ever checked.
async fn require_spin_password(
    state: &ServerState,
    peer: SocketAddr,
    token: &AccountToken,
    password: Option<&str>,
) -> crate::errors::AppResult<()> {
    if !db::wheel_password_confirmation_required(&state.pool).await? {
        return Ok(());
    }
    let Some(password) = password.filter(|password| !password.is_empty()) else {
        return Err(AppError::SpinPasswordRejected("password_required"));
    };
    let username_normalized =
        accounts::active_username_normalized_by_id(&state.pool, &token.account_id).await?;
    let now = chrono::Utc::now();
    if let crate::login_throttle::Decision::Locked { retry_after_secs } = state
        .account_login_throttle
        .check(&username_normalized, now)
        .await
    {
        return Err(AppError::Throttled { retry_after_secs });
    }
    if accounts::active_account_password_matches(&state.pool, &token.account_id, password).await? {
        state
            .account_login_throttle
            .record_success(&username_normalized)
            .await;
        return Ok(());
    }
    if let Some(lockout_seconds) = state
        .account_login_throttle
        .record_failure(&username_normalized, now)
        .await
    {
        accounts::audit_login_throttled(
            &state.pool,
            &username_normalized,
            &peer.ip().to_string(),
            Some(&token.pc_id),
            account_login_throttle::FAILURE_THRESHOLD,
            lockout_seconds,
        )
        .await?;
    }
    Err(AppError::SpinPasswordRejected("invalid_password"))
}

async fn wheel_spin(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    session: AccountSession,
    body: axum::body::Bytes,
) -> HttpResult<WheelSpinResponse> {
    let input = if body.is_empty() {
        WheelSpinInput { password: None }
    } else {
        serde_json::from_slice::<WheelSpinInput>(&body)
            .map_err(|error| AppError::InvalidInput(error.to_string()))?
    };
    require_spin_password(&state, peer, &session.token, input.password.as_deref()).await?;
    let (claimed_session, result) = state
        .sessions
        .spin_wheel(&state.pool, &session.token)
        .await?;
    let account = accounts::get(&state.pool, &session.token.account_id).await?;
    publish_pc(&state, &session.token.pc_id).await;
    Ok(Json(WheelSpinResponse {
        token: session.raw_token,
        account,
        session: claimed_session,
        result,
    }))
}

async fn wheel_settings_handler(
    State(state): State<ServerState>,
    _session: AccountSession,
) -> HttpResult<ClientWheelSettings> {
    db::client_wheel_settings(&state.pool).await.map(Json)
}

/// Acknowledgement body for an accepted Forwarded Diagnostics batch. A 2xx
/// response is the ack: the device releases exactly the lines it sent and
/// retains everything else for the next attempt (ADR-0078).
#[derive(serde::Serialize)]
struct ForwardedDiagnosticsAck {
    accepted: usize,
}

/// Batched Forwarded Diagnostics intake (ADR-0078), replacing the minimal
/// single-entry `POST /api/diagnostics`. Device-authenticated the same way as
/// the identity endpoints: the PC is resolved read-only from the
/// `X-OpenPiso-Mac` header, an unrecognised device is refused, and a
/// malformed batch is rejected whole â€” never partially accepted. Accepted
/// lines are appended to the dedicated forwarded log tagged with the
/// server-resolved PC identity and the batch's source discriminator.
async fn receive_forwarded_diagnostics(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(batch): Json<ForwardedDiagnosticBatch>,
) -> HttpResult<ForwardedDiagnosticsAck> {
    let Some(mac) = read_mac_header(&headers) else {
        return Err(AppError::Unauthorized(
            "device authentication is required; expected X-OpenPiso-Mac".to_owned(),
        ));
    };
    let Some(pc_id) = db::pcs::pc_id_by_mac(&state.pool, &mac).await? else {
        return Err(AppError::Unauthorized(
            "device is not registered with this admin server".to_owned(),
        ));
    };
    batch.validate()?;
    let accepted = batch.lines.len();
    state
        .forwarded_log
        .append(&pc_id, batch.source, &batch.lines)?;
    // A batch echoing a Diagnostic Request nonce is the answer to the owner's
    // ask: stop delivering that nonce on Heartbeat responses. Consumed only
    // after the slice is durably in the forwarded log, so an append failure
    // cannot consume a request whose slice the device will retry.
    if let Some(nonce) = batch.diagnostic_request_nonce.as_deref() {
        state.diagnostic_requests.consume(&pc_id, nonce);
    }
    Ok(Json(ForwardedDiagnosticsAck { accepted }))
}

async fn health() -> Json<HealthStatus> {
    Json(HealthStatus {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn list_pcs(State(state): State<ServerState>) -> HttpResult<Vec<Pc>> {
    let pcs = db::pcs(&state.pool).await?;
    Ok(Json(
        state
            .sessions
            .enrich_pcs(pcs)
            .await
            .into_iter()
            .map(|pc| state.events.stamp_shop_status(pc))
            .collect(),
    ))
}

async fn get_pc(State(state): State<ServerState>, Path(pc_id): Path<String>) -> HttpResult<Pc> {
    let pc = db::pc(&state.pool, &pc_id).await?;
    Ok(Json(
        state
            .events
            .stamp_shop_status(state.sessions.enrich_pc(pc).await),
    ))
}

fn parse_tamper_kill_report_age(
    value: Option<serde_json::Value>,
) -> Result<TamperKillReportAge, AppError> {
    match value {
        None => Ok(TamperKillReportAge::Missing),
        Some(value) => match value.as_i64() {
            Some(age) if age < 0 => Err(AppError::InvalidInput(
                "tamper-kill report age must not be negative".to_owned(),
            )),
            Some(age) => Ok(TamperKillReportAge::Valid(age)),
            None => Ok(TamperKillReportAge::Invalid),
        },
    }
}

/// Validate and append the batch one Guard Poll carries (ADR-0078),
/// all-or-nothing. The 200 response is also the batch's acknowledgement, so a
/// poll that carries lines from a sender whose PC cannot be resolved is
/// refused with 401: those lines are never acknowledged for something Admin
/// never stored, and wait in the Guard's buffer for a poll Admin can
/// attribute.
async fn append_guard_batch(
    state: &ServerState,
    mac: Option<&str>,
    forwarded: GuardForwardedLines,
) -> Result<(), AppError> {
    let Some(mac) = mac else {
        return Err(AppError::Unauthorized(
            "device authentication is required; expected X-OpenPiso-Mac".to_owned(),
        ));
    };
    let Some(pc_id) = db::pcs::pc_id_by_mac(&state.pool, mac).await? else {
        return Err(AppError::Unauthorized(
            "device is not registered with this admin server".to_owned(),
        ));
    };
    let batch = ForwardedDiagnosticBatch {
        source: openpiso_core::forwarding::ForwardedSource::Guard,
        lines: forwarded.lines,
        // The Guard's forwarding rides its poll; only a Client slice echoes a
        // Diagnostic Request nonce.
        diagnostic_request_nonce: None,
    };
    batch.validate()?;
    state
        .forwarded_log
        .append(&pc_id, batch.source, &batch.lines)?;
    Ok(())
}

/// One Guard Poll (ADR-0077).
///
/// Deliberately narrow. It records what the Guard reports and answers with the
/// desired power state for that PC. It resolves the PC read-only by MAC — a
/// Guard may never create, bind, or rebind a `pcs` row the way the heartbeat
/// identity path can — and an unknown MAC is answered with "stay on" rather
/// than an error, because a Guard that cannot be identified must never be left
/// guessing.
///
/// It writes no session, money, Remaining Time, or Points value.
///
/// Forwarded Diagnostics ride the poll body (ADR-0078): the Guard gains no
/// second network call, so its batch is validated and appended to the
/// forwarded log inside this one request, all-or-nothing. A poll that carries
/// a batch cannot be answered with "stay on" before the device is identified:
/// lines that cannot be attributed to a PC must not be acknowledged, so an
/// unidentifiable or unregistered sender is refused with 401 and its lines
/// stay in its buffer for a later poll. The order semantics for a batchless
/// poll are exactly as before the widening.
async fn guard_state(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(input): Json<GuardStateInput>,
) -> HttpResult<GuardStateResponse> {
    let stay_on = GuardStateResponse {
        power_off: false,
        nonce: None,
        decided_at: None,
        poll_interval_secs: GUARD_POLL_INTERVAL_SECS,
    };
    let mac = read_mac_header(&headers);
    if let Some(forwarded) = input.forwarded {
        append_guard_batch(&state, mac.as_deref(), forwarded).await?;
    }
    // The batch is dealt with; from here the poll behaves exactly as it did
    // before the widening, including "stay on" for an unidentifiable sender.
    let pc_id = match mac.as_deref() {
        Some(mac) => db::pcs::pc_id_by_mac(&state.pool, mac).await?,
        None => None,
    };
    let Some(pc_id) = pc_id else {
        return Ok(Json(stay_on));
    };
    let facts = GuardFacts {
        received_at: chrono::Utc::now(),
        client_connected: input.client_connected,
        disarmed: input.disarmed,
        relaunch_attempts: input.relaunch_attempts.max(0),
    };
    let Some(order) = state.guards.record_poll(&pc_id, facts) else {
        return Ok(Json(stay_on));
    };
    Ok(Json(GuardStateResponse {
        power_off: true,
        nonce: Some(order.nonce),
        decided_at: Some(order.decided_at.to_rfc3339()),
        poll_interval_secs: GUARD_POLL_INTERVAL_SECS,
    }))
}

#[expect(
    clippy::result_large_err,
    reason = "axum rejections are Response values; Box<Response> is not IntoResponse"
)]
async fn identity_heartbeat(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<HeartbeatInput>,
) -> Result<Json<HeartbeatResponse>, Response> {
    if input.tamper_kill_timestamp.is_none()
        && (input.tamper_kill_attempt_count.is_some() || input.tamper_kill_report_age_ms.is_some())
    {
        return Err(AppError::InvalidInput(
            "tamper-kill report age and attempt count require a timestamp".to_owned(),
        )
        .into());
    }
    let report_age = parse_tamper_kill_report_age(input.tamper_kill_report_age_ms)?;
    if input
        .tamper_kill_attempt_count
        .is_some_and(|attempt| attempt <= 0)
    {
        return Err(AppError::InvalidInput(
            "tamper-kill attempt count must be positive".to_owned(),
        )
        .into());
    }

    let mac = read_mac_header(&headers);
    let received_at = chrono::Utc::now();
    let report = input
        .tamper_kill_timestamp
        .as_ref()
        .map(|kill_timestamp| TamperKillReport {
            kill_timestamp: kill_timestamp.clone(),
            report_age,
            attempt_count: input.tamper_kill_attempt_count.unwrap_or(1),
            received_at,
        });
    let heartbeat = match state
        .sessions
        .heartbeat_by_identity_with_report(
            &state.pool,
            &state.account_tokens,
            IdentityHeartbeatInput {
                mac: mac.as_deref(),
                source_ip: &peer.ip().to_string(),
                hostname: &input.hostname,
                client_version: &input.client_version,
                client_time_unix_ms: input.client_time_unix_ms,
                received_at,
                report,
                guard_coverage: input.guard_coverage,
            },
        )
        .await?
    {
        Some(heartbeat) => heartbeat,
        None => return Err(unregistered_rejection()),
    };
    let pc = heartbeat.pc;
    let (tamper_kill_acknowledged, tamper_kill_subject) = match heartbeat.tamper_kill_subject {
        Some(subject) => (
            Some(TamperKillAck::Recorded),
            Some(subject.as_str().to_owned()),
        ),
        None => (None, None),
    };
    let pc = state.sessions.enrich_pc(pc).await;
    // Return the same `Pc` the broadcast stamps with the authoritative power
    // command, so the heartbeat HTTP response and the SSE stream agree and the
    // client's grace countdown is never reset by a stale `active` (ADR-0034).
    let pc = state.events.publish_pc_updated(pc);
    let theme_version = db::theme_version(&state.pool).await?;
    // The Diagnostic Request rides in the shape of the acknowledgement fields
    // above: an optional, skip-when-absent value on an otherwise unchanged
    // response. It repeats until the Client's slice answer consumes it, and
    // the Client deduplicates by nonce, so repetition cannot cause repeated
    // uploads (ADR-0078).
    let diagnostic_request_nonce = state
        .diagnostic_requests
        .pending(&pc.id, chrono::Utc::now());
    Ok(Json(HeartbeatResponse {
        pc,
        attendance_supported: true,
        theme_version,
        tamper_kill_acknowledged,
        tamper_kill_subject,
        diagnostic_request_nonce,
    }))
}

/// MAC-keyed disconnect (ADR-0031). Same identity resolution as the
/// heartbeat, but does not require a body. Returns the authoritative PC
/// or a typed `pc_unregistered` rejection.
#[expect(
    clippy::result_large_err,
    reason = "axum rejections are Response values; Box<Response> is not IntoResponse"
)]
async fn identity_disconnect(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Pc>, Response> {
    let mac = read_mac_header(&headers);
    let pc =
        match db::disconnect_pc_by_identity(&state.pool, mac.as_deref(), &peer.ip().to_string())
            .await?
        {
            Some(pc) => pc,
            None => return Err(unregistered_rejection()),
        };
    // End any active account session bound to this PC (the PC is going
    // offline â€” the account should not remain tied to a dead connection).
    state.sessions.disconnect_pc(&pc.id).await;
    let pc = state.sessions.enrich_pc(pc).await;
    let pc = state.events.publish_pc_updated(pc);
    Ok(Json(pc))
}

/// HTTP wire format for the coin-event endpoint. The same fields arrive
/// as before; `device_id` + `signature` are consumed at the edge into a
/// [`CoinSourceClaim`] so the payment facts (`CoinEventInput`) carry only
/// payment facts (ADR-0033).
#[derive(serde::Deserialize)]
struct CoinEventHttpBody {
    device_id: String,
    pc_id: String,
    amount_cents: i64,
    #[serde(default)]
    pulse_count: Option<i64>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    signature: Option<String>,
}

async fn coin_event(
    State(state): State<ServerState>,
    Json(body): Json<CoinEventHttpBody>,
) -> HttpResult<CoinTransaction> {
    let input = CoinEventInput {
        pc_id: body.pc_id.trim().to_owned(),
        amount_cents: body.amount_cents,
        pulse_count: body.pulse_count,
        timestamp: body.timestamp.clone(),
    };
    db::validate_coin_event_input(&input)?;

    let coin_trace_id = Uuid::new_v4();
    let device_id = body.device_id.trim().to_owned();
    let status = db::shop_status(&state.pool).await?;
    if !status.open {
        warn!(
            target: "openpiso_admin_lib::coins",
            coin_trace_id = %coin_trace_id,
            pc_id = %input.pc_id,
            device_id = %device_id,
            amount_cents = input.amount_cents,
            pulse_count = ?input.pulse_count,
            reason = "shop_closed",
            error_code = "shop_closed",
            "Coin event refused while Shop Closed"
        );
        let mut event = NewAuditEvent::simple(
            "coin_denied",
            openpiso_core::audit::AuditOutcome::Denied,
            openpiso_core::audit::AuditActor::CoinDevice,
            "Coin denied while Shop Closed",
        );
        event.pc_id = Some(&input.pc_id);
        event.amount_cents = Some(input.amount_cents);
        event.metadata = json!({
            "reason": "shop_closed",
            "coin_trace_id": coin_trace_id.to_string(),
            "device_id": device_id,
            "pulse_count": input.pulse_count,
            "device_timestamp": input.timestamp,
            "next_change_at": status.next_change_at,
        });
        audit::append_standalone(&state.pool, event).await?;
        return Err(AppError::ShopClosed);
    }

    let claim = CoinSourceClaim::RemoteDevice {
        device_id: body.device_id,
        secret: body.signature.unwrap_or_default(),
    };
    let transaction = state
        .sessions
        .record_coin_event_with_trace(&state.pool, &input, &claim, Some(coin_trace_id))
        .await?;
    let pc = db::pc(&state.pool, &input.pc_id).await?;
    state
        .events
        .publish_pc_updated(state.sessions.enrich_pc(pc).await);
    state.events.publish_coin_accepted(transaction.clone());
    Ok(Json(transaction))
}

/// MAC-keyed SSE event stream (ADR-0031). Resolves the PC the same way
/// the heartbeat does and streams `pc.updated` events scoped to that
/// PC. An unregistered machine is a typed 404 with
/// `{"error":"pc_unregistered"}` â€” the client should not try to read a
/// stream from a rejection.
#[expect(
    clippy::result_large_err,
    reason = "axum rejections are Response values; Box<Response> is not IntoResponse"
)]
async fn identity_events(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let mac = read_mac_header(&headers);
    let resolved = match db::heartbeat_by_identity(
        &state.pool,
        mac.as_deref(),
        &peer.ip().to_string(),
        "sse-events",
        env!("CARGO_PKG_VERSION"),
    )
    .await?
    {
        Some(pc) => pc,
        None => return Err(unregistered_rejection()),
    };
    let receiver = state.events.subscribe();
    let coin_receiver = state.events.subscribe_coins();
    let mut initial_pc = state
        .events
        .stamp_shop_status(state.sessions.enrich_pc(resolved.clone()).await);
    // Stamp the initial snapshot with the same authoritative power command the
    // broadcast uses, so a reconnect at zero shows the shutdown countdown
    // immediately instead of a momentary `active` (ADR-0034).
    initial_pc.power_state =
        shutdown::power_state(&initial_pc.status, initial_pc.remaining_seconds);
    let updates_pc_id = resolved.id.clone();
    let updates = BroadcastStream::new(receiver).filter_map(move |result| {
        let updates_pc_id = updates_pc_id.clone();
        async move {
            match result {
                Ok(pc) if pc.id == updates_pc_id => Some(Ok(sse_pc_event(&pc))),
                Ok(_) | Err(_) => None,
            }
        }
    });
    let coins_pc_id = resolved.id.clone();
    let coins = BroadcastStream::new(coin_receiver).filter_map(move |result| {
        let coins_pc_id = coins_pc_id.clone();
        async move {
            match result {
                Ok(transaction) if transaction.pc_id == coins_pc_id => {
                    Some(Ok(sse_coin_event(&transaction)))
                }
                Ok(_) | Err(_) => None,
            }
        }
    });
    let initial = stream::once(async move { Ok(sse_pc_event(&initial_pc)) });

    Ok(
        Sse::new(initial.chain(stream::select(updates, coins))).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("keep-alive"),
        ),
    )
}

async fn publish_pc(state: &ServerState, pc_id: &str) {
    if let Ok(pc) = db::pc(&state.pool, pc_id).await {
        state
            .events
            .publish_pc_updated(state.sessions.enrich_pc(pc).await);
    }
}

async fn publish_all_pcs(state: &ServerState) {
    if let Ok(pcs) = db::pcs(&state.pool).await {
        for pc in state.sessions.enrich_pcs(pcs).await {
            state.events.publish_pc_updated(pc);
        }
    }
}

fn sse_coin_event(transaction: &CoinTransaction) -> Event {
    Event::default()
        .event("coin.accepted")
        .json_data(transaction)
        .unwrap_or_else(|_| Event::default().event("coin.accepted"))
}

fn sse_pc_event(pc: &Pc) -> Event {
    Event::default().event("pc.updated").data(
        serde_json::to_string(&crate::models::ClientPcResponse {
            pc,
            attendance_supported: true,
        })
        .unwrap_or_else(|_| "{}".to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::test_utils;

    fn test_events() -> EventHub {
        EventHub::without_ui()
    }

    fn shop_hours_input(
        enabled: bool,
        mode: crate::models::ShopHoursDayMode,
    ) -> crate::models::ShopHoursSettingsInput {
        crate::models::ShopHoursSettingsInput {
            enabled,
            last_call_lead_minutes: 15,
            days: (0..7)
                .map(|weekday| crate::models::ShopHoursDayInput {
                    weekday,
                    mode,
                    open_minute: None,
                    close_minute: None,
                })
                .collect(),
        }
    }

    async fn shop_status_response(
        enabled: bool,
        mode: crate::models::ShopHoursDayMode,
    ) -> serde_json::Value {
        let pool = test_utils::mem_test_pool().await;
        db::update_shop_hours(&pool, &shop_hours_input(enabled, mode))
            .await
            .expect("Shop Hours should save");
        let (address, server) = spawn_test_server(pool).await;
        let response = reqwest::Client::new()
            .get(format!("http://{address}/api/shop-status"))
            .send()
            .await
            .expect("Shop Status request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("Shop Status response should be JSON");
        server.abort();
        body
    }

    /// Test setup for a PC's registered IP / MAC through the production
    /// owner-settings path. `None` keeps the stored value, `Some(None)`
    /// clears it, `Some(Some(v))` sets it; name and visibility are untouched.
    async fn set_pc_identity(
        pool: &SqlitePool,
        pc_id: &str,
        registered_ip: Option<Option<&str>>,
        mac_address: Option<Option<&str>>,
    ) -> crate::errors::AppResult<()> {
        let current = db::pc_settings(pool)
            .await?
            .into_iter()
            .find(|pc| pc.id == pc_id)
            .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
        let overlay = |new: Option<Option<&str>>, old: Option<String>| match new {
            Some(value) => Some(value.unwrap_or_default().to_owned()),
            None => old,
        };
        db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: current.name,
                visible: current.visible,
                registered_ip: overlay(registered_ip, current.registered_ip),
                mac_address: overlay(mac_address, current.mac_address),
            },
        )
        .await?;
        Ok(())
    }

    /// Spawn a test server mounted with `ConnectInfo<SocketAddr>` (the
    /// production mount). Mirrors the production wiring so the
    /// `X-OpenPiso-Mac` identity endpoints can read the peer address
    /// the same way they do in dev/release builds.
    async fn spawn_test_server(pool: SqlitePool) -> (String, tokio::task::JoinHandle<()>) {
        let (address, handle, _, _) =
            spawn_test_server_with_throttle(pool, AccountLoginThrottle::default()).await;
        (address, handle)
    }

    async fn spawn_test_server_with_throttle(
        pool: SqlitePool,
        account_login_throttle: AccountLoginThrottle,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        AccountLoginThrottle,
        SessionStore,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("listener should have an address")
            .to_string();
        let sessions = SessionStore::default();
        let server_sessions = sessions.clone();
        let handle = tokio::spawn({
            let server_throttle = account_login_throttle.clone();
            async move {
                axum::serve(
                    listener,
                    router_with_throttle(
                        pool,
                        test_events(),
                        AccountTokens::default(),
                        server_sessions,
                        server_throttle,
                        GuardRegistry::new(),
                        crate::forwarded_log::test_forwarded_log(),
                        DiagnosticRequestRegistry::new(),
                    )
                    .into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .expect("test server should run");
            }
        });
        (address, handle, account_login_throttle, sessions)
    }

    async fn account_login_request(
        client: &reqwest::Client,
        address: &str,
        username: &str,
        password: &str,
    ) -> reqwest::Response {
        client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": username,
                "password": password,
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("Account login request should complete")
    }

    #[tokio::test]
    async fn account_login_throttle_refuses_locked_credentials_before_shop_hours() {
        let pool = test_utils::mem_test_pool().await;
        let account = accounts::create(
            &pool,
            "throttle-user",
            "correct-password",
            AccountCreator::SelfService,
            Some("pc-01"),
        )
        .await
        .expect("Account should be created");
        let audit_pool = pool.clone();
        let (address, server, throttle, _) =
            spawn_test_server_with_throttle(pool, AccountLoginThrottle::default()).await;
        let client = reqwest::Client::new();

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD {
            let response =
                account_login_request(&client, &address, "THROTTLE-USER", "wrong-password").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let _ = response
                .text()
                .await
                .expect("failure body should be readable");
        }

        let locked =
            account_login_request(&client, &address, "throttle-user", "wrong-password").await;
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = locked
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .expect("lockout should include Retry-After")
            .to_str()
            .expect("Retry-After should be text")
            .parse::<u64>()
            .expect("Retry-After should be seconds");
        assert!(retry_after > 0);
        let body = locked
            .text()
            .await
            .expect("lockout body should be readable");
        assert!(body.contains("try again in"));

        let throttled_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_login_throttled'",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("throttle audit count");
        assert_eq!(throttled_events, 1);
        let metadata_json: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log
             WHERE event_type = 'account_login_throttled'
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("throttle audit metadata");
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_json).expect("throttle metadata should be JSON");
        let audit_account_id: String = sqlx::query_scalar(
            "SELECT account_id FROM audit_log
             WHERE event_type = 'account_login_throttled'
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("throttle audit Account attribution");
        assert_eq!(audit_account_id, account.id);
        assert_eq!(metadata["username_normalized"], "throttle-user");
        assert_eq!(metadata["source_ip"], "127.0.0.1");
        assert_eq!(
            metadata["consecutive_failures"],
            account_login_throttle::FAILURE_THRESHOLD
        );
        assert_eq!(
            metadata["lockout_seconds"],
            account_login_throttle::LOCKOUT_SECONDS
        );
        assert!(!metadata_json.contains("wrong-password"));
        assert!(!metadata_json.contains("correct-password"));

        let correct_while_locked =
            account_login_request(&client, &address, "throttle-user", "correct-password").await;
        assert_eq!(correct_while_locked.status(), StatusCode::TOO_MANY_REQUESTS);

        db::update_shop_hours(
            &audit_pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await
        .expect("Shop Hours should save");
        let closed_while_locked =
            account_login_request(&client, &address, "throttle-user", "correct-password").await;
        assert_eq!(closed_while_locked.status(), StatusCode::TOO_MANY_REQUESTS);

        let failed_logins: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'login_failed' AND metadata_json LIKE '%bad_password%'",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("failed login audit count");
        assert_eq!(
            failed_logins,
            i64::from(account_login_throttle::FAILURE_THRESHOLD)
        );
        let shop_closed_logins: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'login_failed' AND metadata_json LIKE '%shop_closed%'",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("Shop Closed login audit count");
        assert_eq!(shop_closed_logins, 0);

        db::update_shop_hours(
            &audit_pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::Open24Hours),
        )
        .await
        .expect("Shop Hours should reopen");
        // Clear the active lockout through the same state operation used by a
        // successful login/password reset, then exercise a second HTTP transition.
        throttle.record_success("throttle-user").await;
        for _ in 0..account_login_throttle::FAILURE_THRESHOLD {
            let response =
                account_login_request(&client, &address, "throttle-user", "wrong-password").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let _ = response
                .text()
                .await
                .expect("second failure body should be readable");
        }
        let throttled_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_login_throttled'",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("second throttle audit count");
        assert_eq!(throttled_events, 2);
        server.abort();
    }

    #[tokio::test]
    async fn account_login_throttle_ignores_unknown_usernames_and_is_per_account() {
        let pool = test_utils::mem_test_pool().await;
        accounts::create(
            &pool,
            "known-user",
            "correct-password",
            AccountCreator::SelfService,
            Some("pc-01"),
        )
        .await
        .expect("first Account should be created");
        accounts::create(
            &pool,
            "second-user",
            "correct-password",
            AccountCreator::SelfService,
            Some("pc-01"),
        )
        .await
        .expect("second Account should be created");
        let audit_pool = pool.clone();
        let (address, server) = spawn_test_server(pool).await;
        let client = reqwest::Client::new();

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD + 2 {
            let response =
                account_login_request(&client, &address, "made-up-user", "wrong-password").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let _ = response
                .text()
                .await
                .expect("failure body should be readable");
        }
        let unknown_throttles: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_login_throttled'",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("throttle audit count");
        assert_eq!(unknown_throttles, 0);

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD {
            let response =
                account_login_request(&client, &address, "known-user", "wrong-password").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let _ = response
                .text()
                .await
                .expect("failure body should be readable");
        }
        let second_login =
            account_login_request(&client, &address, "second-user", "correct-password").await;
        assert_eq!(second_login.status(), StatusCode::OK);

        server.abort();
    }

    #[tokio::test]
    async fn account_login_success_clears_failures() {
        let pool = test_utils::mem_test_pool().await;
        let (address, server) = spawn_test_server(pool).await;
        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "reset-user",
                "password": "correct-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD - 1 {
            let response =
                account_login_request(&client, &address, "reset-user", "wrong-password").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let _ = response
                .text()
                .await
                .expect("failure body should be readable");
        }
        let success =
            account_login_request(&client, &address, "reset-user", "correct-password").await;
        assert_eq!(success.status(), StatusCode::OK);

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD {
            let response =
                account_login_request(&client, &address, "reset-user", "wrong-password").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let _ = response
                .text()
                .await
                .expect("failure body should be readable");
        }
        let locked = account_login_request(&client, &address, "reset-user", "wrong-password").await;
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);

        server.abort();
    }

    #[tokio::test]
    async fn shop_status_reports_all_days_closed_without_authentication() {
        let body = shop_status_response(true, crate::models::ShopHoursDayMode::ClosedAllDay).await;

        assert_eq!(body["open"], false);
        assert_eq!(body["reason"], "schedule");
        assert_eq!(body["last_call_active"], false);
        assert!(body.get("next_change_at").is_some());
        assert_eq!(body["next_change_at"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn shop_status_reports_all_days_open_24_hours() {
        let body = shop_status_response(true, crate::models::ShopHoursDayMode::Open24Hours).await;

        assert_eq!(body["open"], true);
        assert_eq!(body["reason"], "schedule");
        assert_eq!(body["last_call_active"], false);
        assert!(body.get("next_change_at").is_some());
        assert_eq!(body["next_change_at"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn disabled_shop_hours_reports_open_even_with_all_days_closed() {
        let body = shop_status_response(false, crate::models::ShopHoursDayMode::ClosedAllDay).await;

        assert_eq!(body["open"], true);
        assert_eq!(body["reason"], "schedule");
        assert_eq!(body["last_call_active"], false);
        assert!(body.get("next_change_at").is_some());
        assert_eq!(body["next_change_at"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn owner_routes_are_not_mounted_on_client_device_server() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(
                    pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    crate::forwarded_log::test_forwarded_log(),
                ),
            )
            .await
            .expect("test server should run");
        });

        let response = reqwest::Client::new()
            .get(format!("http://{address}/web-admin/dashboard"))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        server.abort();
    }

    #[tokio::test]
    async fn heartbeat_endpoint_returns_authoritative_pc_state() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        // Bind pc-01 to the test MAC so the identity endpoint recognises
        // it (ADR-0031). Mirrors a real first-boot bind.
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");

        let (address, server) = spawn_test_server(pool).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0"
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let pc = response
            .json::<serde_json::Value>()
            .await
            .expect("heartbeat response should be JSON");
        assert_eq!(pc["id"], "pc-01");
        assert_eq!(pc["status"], "online");
        assert_eq!(pc["remaining_seconds"], 0);
        // A connected PC at zero is commanded to shut down, and the heartbeat
        // HTTP response carries that command so it cannot disagree with the SSE
        // stream and reset the client's grace countdown (ADR-0034).
        assert_eq!(pc["power_state"], "shutdown");
        assert_eq!(pc["theme_version"], 1);
        assert_eq!(pc["attendance_supported"], true);
        assert!(pc["clock_difference_ms"].is_null());

        server.abort();
    }

    /// The Client decodes Admin's Attendance responses with the shared wire
    /// contract; decoding then re-encoding must reproduce the exact payload.
    fn assert_attendance_wire_roundtrip<T>(payload: &serde_json::Value)
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        let decoded: T = serde_json::from_value(payload.clone())
            .expect("Admin Attendance payload should decode with the shared wire contract");
        assert_eq!(
            &serde_json::to_value(decoded).expect("wire contract should serialize"),
            payload
        );
    }

    #[tokio::test]
    async fn attendance_summary_requires_a_live_account_session() {
        let pool = test_utils::mem_test_pool().await;
        let audit_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        let (address, server) = spawn_test_server(pool.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("http://{address}/api/attendance"))
            .send()
            .await
            .expect("Attendance request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("unauthorized response should be JSON");
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
        let audit_after_unauth = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        assert_eq!(audit_after_unauth, audit_before);

        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "attendance-revoked-session",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("Account signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("Account signup should return JSON");
        let token = auth["token"]
            .as_str()
            .expect("token should be a string")
            .to_owned();
        let logout = client
            .post(format!("http://{address}/api/accounts/logout"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("Account logout should complete");
        assert_eq!(logout.status(), StatusCode::OK);

        let audit_before_revoked = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        let revoked = client
            .get(format!("http://{address}/api/attendance"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("revoked Attendance request should complete");
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            revoked
                .json::<serde_json::Value>()
                .await
                .expect("revoked Attendance response should be JSON"),
            serde_json::json!({ "error": "unauthorized" })
        );
        let audit_after_revoked = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        assert_eq!(audit_after_revoked, audit_before_revoked);

        server.abort();
    }

    #[tokio::test]
    async fn attendance_summary_returns_account_scoped_calendar_and_clock() {
        let pool = test_utils::mem_test_pool().await;
        let (address, server) = spawn_test_server(pool).await;
        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "attendance-summary",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("Account signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("Account signup should return JSON");

        let response = client
            .get(format!("http://{address}/api/attendance"))
            .bearer_auth(auth["token"].as_str().expect("token should be a string"))
            .send()
            .await
            .expect("Attendance summary request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let summary = response
            .json::<serde_json::Value>()
            .await
            .expect("Attendance summary should be JSON");
        assert_attendance_wire_roundtrip::<openpiso_core::attendance_wire::AttendanceSummary>(
            &summary,
        );
        assert_eq!(summary["enabled"], false);
        assert_eq!(summary["today"]["status"], "ineligible");
        assert!(summary["today"]["earning_date"].is_string());
        assert_eq!(summary["totals"]["earned_seconds"], 0);
        let calendar_months = summary["calendar_months"]
            .as_array()
            .expect("calendar months should be an array");
        assert_eq!(calendar_months.len(), 12);
        assert_eq!(
            calendar_months[0]["earning_month"],
            summary["admin_local_month"]
        );
        assert!(calendar_months.windows(2).all(|months| {
            months[0]["earning_month"].as_str() > months[1]["earning_month"].as_str()
        }));
        assert_eq!(summary["older_claimable_months"], serde_json::json!([]));
        assert!(summary["as_of"].is_string());
        assert!(summary["admin_local_date"].is_string());
        assert!(summary["admin_local_month"].is_string());

        server.abort();
    }

    #[tokio::test]
    async fn attendance_routes_isolate_nonzero_facts_between_live_accounts() {
        let pool = test_utils::mem_test_pool().await;
        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (account_a_token, account_a_id) =
            signup(&client, &address, "attendance-isolation-a", "pc-01").await;
        let (account_b_token, _) =
            signup(&client, &address, "attendance-isolation-b", "pc-02").await;

        let offset = db::attendance::host_offset();
        let source_at = chrono::Utc::now();
        let earning_date = source_at.with_timezone(&offset).date_naive();
        test_utils::enable_attendance(&pool, 2, source_at, 100, 600)
            .await
            .expect("Attendance should be enabled for the test date");
        let mut tx = db::begin_immediate(&pool)
            .await
            .expect("earning transaction should begin");
        let mut source = NewAuditEvent::simple(
            "coin_accepted",
            openpiso_core::audit::AuditOutcome::Success,
            openpiso_core::audit::AuditActor::CoinDevice,
            "Accepted Attendance isolation test coin",
        );
        source.account_id = Some(&account_a_id);
        source.amount_cents = Some(100);
        let source_audit_event_id = audit::append(&mut tx, source)
            .await
            .expect("coin audit event should be inserted");
        db::attendance_earning::record_earning(
            &mut tx,
            db::attendance_earning::EarningInput {
                account_id: &account_a_id,
                source_audit_event_id: &source_audit_event_id,
                source_kind: db::attendance::SourceKind::AcceptedCoin,
                cents: 100,
                source_at,
                offset,
                earning_date,
                award: db::attendance::AwardSnapshot {
                    configuration_version: 2,
                    daily_threshold_cents: 100,
                    reward_seconds: 600,
                },
                original_rejection_audit_event_id: None,
                trigger_pc_id: Some("pc-01"),
            },
        )
        .await
        .expect("Account A should earn Attendance Time");
        tx.commit().await.expect("Attendance earning should commit");

        let summary_a = client
            .get(format!("http://{address}/api/attendance"))
            .bearer_auth(&account_a_token)
            .send()
            .await
            .expect("Account A summary should complete");
        assert_eq!(summary_a.status(), StatusCode::OK);
        let summary_a = summary_a
            .json::<serde_json::Value>()
            .await
            .expect("Account A summary should be JSON");
        assert_eq!(summary_a["totals"]["earned_seconds"], 600);

        let summary_b = client
            .get(format!("http://{address}/api/attendance"))
            .bearer_auth(&account_b_token)
            .send()
            .await
            .expect("Account B summary should complete");
        assert_eq!(summary_b.status(), StatusCode::OK);
        let summary_b = summary_b
            .json::<serde_json::Value>()
            .await
            .expect("Account B summary should be JSON");
        assert_eq!(summary_b["totals"]["earned_seconds"], 0);

        let earning_month = earning_date.format("%Y-%m");
        let month_path = format!("http://{address}/api/attendance/months/{earning_month}");
        let month_a = client
            .get(&month_path)
            .bearer_auth(&account_a_token)
            .send()
            .await
            .expect("Account A month should complete");
        assert_eq!(month_a.status(), StatusCode::OK);
        let month_a = month_a
            .json::<serde_json::Value>()
            .await
            .expect("Account A month should be JSON");
        assert_eq!(month_a["check_in_count"], 1);
        assert_eq!(month_a["earned_seconds"], 600);

        let month_b = client
            .get(&month_path)
            .bearer_auth(&account_b_token)
            .send()
            .await
            .expect("Account B month should complete");
        assert_eq!(month_b.status(), StatusCode::OK);
        let month_b = month_b
            .json::<serde_json::Value>()
            .await
            .expect("Account B month should be JSON");
        assert_eq!(month_b["check_in_count"], 0);
        assert_eq!(month_b["earned_seconds"], 0);

        server.abort();
    }

    #[tokio::test]
    async fn attendance_month_returns_calendar_and_rejects_invalid_or_out_of_range_keys() {
        let pool = test_utils::mem_test_pool().await;
        let (address, server) = spawn_test_server(pool).await;
        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "attendance-month",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("Account signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("Account signup should return JSON");
        let token = auth["token"].as_str().expect("token should be a string");

        let summary = client
            .get(format!("http://{address}/api/attendance"))
            .bearer_auth(token)
            .send()
            .await
            .expect("Attendance summary request should complete")
            .json::<serde_json::Value>()
            .await
            .expect("Attendance summary should be JSON");
        let current_month = summary["admin_local_month"]
            .as_str()
            .expect("Admin-local month should be a string");
        let month = client
            .get(format!(
                "http://{address}/api/attendance/months/{current_month}"
            ))
            .bearer_auth(token)
            .send()
            .await
            .expect("Attendance month request should complete");
        assert_eq!(month.status(), StatusCode::OK);
        let month = month
            .json::<serde_json::Value>()
            .await
            .expect("Attendance month should be JSON");
        assert_attendance_wire_roundtrip::<openpiso_core::attendance_wire::AttendanceMonth>(&month);
        let month_start =
            chrono::NaiveDate::parse_from_str(&format!("{current_month}-01"), "%Y-%m-%d")
                .expect("canonical month should parse");
        let next_month = month_start
            .checked_add_months(chrono::Months::new(1))
            .expect("next month should exist");
        assert_eq!(
            month["days"].as_array().map(Vec::len),
            Some((next_month - month_start).num_days() as usize)
        );
        assert_eq!(month["state"], serde_json::Value::Null);
        assert_eq!(month["claim_starts_at"], serde_json::Value::Null);
        assert_eq!(month["deadline_at_exclusive"], serde_json::Value::Null);
        assert_eq!(month["claim_starts_on"], serde_json::Value::Null);
        assert_eq!(month["deadline_date_inclusive"], serde_json::Value::Null);

        let invalid = client
            .get(format!(
                "http://{address}/api/attendance/months/not-a-month"
            ))
            .bearer_auth(token)
            .send()
            .await
            .expect("invalid Attendance month request should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let invalid = invalid
            .json::<serde_json::Value>()
            .await
            .expect("invalid Attendance month response should be JSON");
        assert_eq!(invalid["error"], "invalid_input");

        let malformed_segment = client
            .get(format!("http://{address}/api/attendance/months/%FF"))
            .bearer_auth(token)
            .send()
            .await
            .expect("malformed Attendance month path should complete");
        assert_eq!(malformed_segment.status(), StatusCode::BAD_REQUEST);
        let malformed_segment = malformed_segment
            .json::<serde_json::Value>()
            .await
            .expect("malformed Attendance month path response should be JSON");
        assert_eq!(malformed_segment["error"], "invalid_input");

        let zero_year = client
            .get(format!("http://{address}/api/attendance/months/0000-01"))
            .bearer_auth(token)
            .send()
            .await
            .expect("year-zero Attendance month request should complete");
        assert_eq!(zero_year.status(), StatusCode::BAD_REQUEST);
        let zero_year = zero_year
            .json::<serde_json::Value>()
            .await
            .expect("year-zero Attendance month response should be JSON");
        assert_eq!(zero_year["error"], "invalid_input");

        let outside_month = month_start
            .checked_sub_months(chrono::Months::new(12))
            .expect("out-of-range month should exist")
            .format("%Y-%m")
            .to_string();
        let outside = client
            .get(format!(
                "http://{address}/api/attendance/months/{outside_month}"
            ))
            .bearer_auth(token)
            .send()
            .await
            .expect("out-of-range Attendance month request should complete");
        assert_eq!(outside.status(), StatusCode::NOT_FOUND);
        let outside = outside
            .json::<serde_json::Value>()
            .await
            .expect("out-of-range Attendance month response should be JSON");
        assert_eq!(outside, serde_json::json!({ "error": "not_found" }));
        server.abort();
    }

    #[tokio::test]
    async fn attendance_claim_success_and_retry_refresh_session_and_projections() {
        use chrono::{Datelike, Offset, TimeZone};
        use openpiso_core::audit::{AuditActor, AuditOutcome};

        let pool = test_utils::mem_test_pool().await;
        let (address, server, _, sessions) =
            spawn_test_server_with_throttle(pool.clone(), AccountLoginThrottle::default()).await;
        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "attendance-claim-retry",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("Account signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("Account signup should return JSON");
        let token = auth["token"]
            .as_str()
            .expect("token should be a string")
            .to_owned();
        let account_id = auth["account"]["id"]
            .as_str()
            .expect("account ID should be a string")
            .to_owned();
        assert_eq!(
            sessions
                .active_session_for_pc("pc-01")
                .await
                .expect("signup should establish an Active Session")
                .remaining_seconds,
            0
        );

        let local_now = chrono::Local::now();
        let offset = local_now.offset().fix();
        let current_month_start =
            chrono::NaiveDate::from_ymd_opt(local_now.year(), local_now.month(), 1)
                .expect("current month start should be valid");
        let month_starts = [
            current_month_start
                .checked_sub_months(chrono::Months::new(1))
                .expect("previous month should exist"),
            current_month_start
                .checked_sub_months(chrono::Months::new(2))
                .expect("two-month-old period should exist"),
        ];
        let effective_at = offset
            .from_local_datetime(&month_starts[1].and_time(chrono::NaiveTime::MIN))
            .single()
            .expect("settings effective time should be unambiguous")
            .with_timezone(&chrono::Utc);
        test_utils::insert_attendance_settings_with_window(
            &pool,
            2,
            true,
            100,
            120,
            2,
            effective_at,
        )
        .await
        .expect("Attendance settings should be enabled for both earning months");

        for month_start in &month_starts {
            let earning_date = month_start
                .with_day(15)
                .expect("earning date should be valid");
            let source_at = offset
                .from_local_datetime(
                    &earning_date
                        .and_hms_opt(12, 0, 0)
                        .expect("source time should be valid"),
                )
                .single()
                .expect("source time should be unambiguous")
                .with_timezone(&chrono::Utc);
            let mut transaction = db::begin_immediate(&pool)
                .await
                .expect("Attendance earning transaction should begin");
            let mut source = NewAuditEvent::simple(
                "coin_accepted",
                AuditOutcome::Success,
                AuditActor::CoinDevice,
                "Accepted Attendance test coin",
            );
            source.account_id = Some(&account_id);
            source.pc_id = Some("pc-01");
            source.amount_cents = Some(100);
            let source_audit_event_id = audit::append(&mut transaction, source)
                .await
                .expect("source audit event should be appended");
            db::attendance_earning::record_earning(
                &mut transaction,
                db::attendance_earning::EarningInput {
                    account_id: &account_id,
                    source_audit_event_id: &source_audit_event_id,
                    source_kind: db::attendance::SourceKind::AcceptedCoin,
                    cents: 100,
                    source_at,
                    offset,
                    earning_date,
                    award: db::attendance::AwardSnapshot {
                        configuration_version: 2,
                        daily_threshold_cents: 100,
                        reward_seconds: 120,
                    },
                    original_rejection_audit_event_id: None,
                    trigger_pc_id: Some("pc-01"),
                },
            )
            .await
            .expect("accepted coin should create an Attendance Check-In");
            transaction
                .commit()
                .await
                .expect("Attendance earning transaction should commit");
        }

        let earning_months =
            month_starts.map(|month_start| month_start.format("%Y-%m").to_string());
        let request_id = Uuid::new_v4();
        let first = client
            .post(format!("http://{address}/api/attendance/claims"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "request_id": request_id.to_string(),
                "earning_month": earning_months[0]
            }))
            .send()
            .await
            .expect("Attendance claim should complete");
        assert_eq!(first.status(), StatusCode::OK);
        let first = first
            .json::<serde_json::Value>()
            .await
            .expect("successful Attendance claim should be JSON");
        assert_attendance_wire_roundtrip::<openpiso_core::attendance_wire::AttendanceClaimResponse>(
            &first,
        );
        assert_eq!(first["claim"]["request_id"], request_id.to_string());
        assert_eq!(first["claim"]["earning_month"], earning_months[0]);
        assert_eq!(first["claim"]["claimed_seconds"], 120);
        assert_eq!(first["remaining_seconds"], 120);
        assert_eq!(first["totals"]["claimed_seconds"], 120);
        assert_eq!(first["month"]["latest_claim_on"], first["admin_local_date"]);
        let original_claim = first["claim"].clone();
        assert_eq!(
            sessions
                .active_session_for_pc("pc-01")
                .await
                .expect("claim should retain the Active Session binding")
                .remaining_seconds,
            120
        );

        let second = client
            .post(format!("http://{address}/api/attendance/claims"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "request_id": Uuid::new_v4().to_string(),
                "earning_month": earning_months[1]
            }))
            .send()
            .await
            .expect("second Attendance claim should complete");
        assert_eq!(second.status(), StatusCode::OK);
        let second = second
            .json::<serde_json::Value>()
            .await
            .expect("second Attendance claim should be JSON");
        assert_eq!(second["remaining_seconds"], 240);
        assert_eq!(second["totals"]["claimed_seconds"], 240);
        assert_eq!(
            sessions
                .active_session_for_pc("pc-01")
                .await
                .expect("second claim should retain the Active Session binding")
                .remaining_seconds,
            240
        );

        let retry = client
            .post(format!("http://{address}/api/attendance/claims"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "request_id": request_id.to_string(),
                "earning_month": earning_months[0]
            }))
            .send()
            .await
            .expect("Attendance claim retry should complete");
        assert_eq!(retry.status(), StatusCode::OK);
        let retry = retry
            .json::<serde_json::Value>()
            .await
            .expect("Attendance claim retry should be JSON");
        assert_attendance_wire_roundtrip::<openpiso_core::attendance_wire::AttendanceClaimResponse>(
            &retry,
        );
        assert_eq!(retry["claim"], original_claim);
        assert_eq!(retry["remaining_seconds"], 240);
        assert_eq!(retry["totals"]["earned_seconds"], 240);
        assert_eq!(retry["totals"]["claimed_seconds"], 240);
        assert_eq!(retry["month"]["claimed_seconds"], 120);
        assert_eq!(retry["month"]["claim_count"], 1);
        assert_eq!(
            sessions
                .active_session_for_pc("pc-01")
                .await
                .expect("retry should retain the Active Session binding")
                .remaining_seconds,
            240
        );
        let claim_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attendance_claims")
            .fetch_one(&pool)
            .await
            .expect("claim records should be readable");
        assert_eq!(claim_count, 2);

        server.abort();
    }

    #[tokio::test]
    async fn attendance_claim_rejects_extra_body_fields_and_audits_domain_denial() {
        let pool = test_utils::mem_test_pool().await;
        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "attendance-claim",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("Account signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("Account signup should return JSON");
        let token = auth["token"].as_str().expect("token should be a string");
        let month = chrono::Utc::now().format("%Y-%m").to_string();
        let request_id = Uuid::new_v4();

        let audits_before_invalid: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        let invalid = client
            .post(format!("http://{address}/api/attendance/claims"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "request_id": request_id.to_string(),
                "earning_month": month,
                "unexpected": true
            }))
            .send()
            .await
            .expect("invalid Attendance claim should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let invalid = invalid
            .json::<serde_json::Value>()
            .await
            .expect("invalid Attendance claim response should be JSON");
        assert_eq!(invalid["error"], "invalid_input");
        let audits_after_invalid: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        assert_eq!(audits_after_invalid, audits_before_invalid);

        let before_forged: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT remaining_seconds FROM accounts WHERE username = 'attendance-claim'),
                    (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log)",
        )
        .fetch_one(&pool)
        .await
        .expect("claim state should be readable");
        let forged = client
            .post(format!("http://{address}/api/attendance/claims"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "request_id": request_id.to_string(),
                "earning_month": month,
                "claimed_seconds": 9_999,
                "claimed_at": "2099-01-01T00:00:00Z"
            }))
            .send()
            .await
            .expect("forged Attendance claim should complete");
        assert_eq!(forged.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            forged
                .json::<serde_json::Value>()
                .await
                .expect("forged Attendance claim response should be JSON")["error"],
            "invalid_input"
        );
        let after_forged: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT remaining_seconds FROM accounts WHERE username = 'attendance-claim'),
                    (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log)",
        )
        .fetch_one(&pool)
        .await
        .expect("claim state should be readable");
        assert_eq!(after_forged, before_forged);

        let denied = client
            .post(format!("http://{address}/api/attendance/claims"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "request_id": Uuid::new_v4().to_string(),
                "earning_month": month
            }))
            .send()
            .await
            .expect("domain-denied Attendance claim should complete");
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        let denied = denied
            .json::<serde_json::Value>()
            .await
            .expect("domain-denied Attendance claim response should be JSON");
        assert_eq!(denied["error"], "attendance_claim_denied");
        assert_eq!(denied["reason"], "month_not_found");
        let audits_after_denial: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit count should be readable");
        assert_eq!(audits_after_denial, audits_after_invalid + 1);
        server.abort();
    }

    #[tokio::test]
    async fn heartbeat_stores_signed_client_clock_difference() {
        let pool = test_utils::mem_test_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let admin_time = chrono::Utc::now().timestamp_millis();
        let response = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "client_time_unix_ms": admin_time - 90_000
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("heartbeat response should be JSON");
        let stored = body["clock_difference_ms"]
            .as_i64()
            .expect("clock difference should be returned");
        assert!(
            (89_000..=92_000).contains(&stored),
            "stored difference: {stored}"
        );

        let persisted: i64 =
            sqlx::query_scalar("SELECT clock_difference_ms FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("clock difference should be persisted");
        assert_eq!(persisted, stored);
        server.abort();
    }

    #[tokio::test]
    async fn heartbeat_stores_the_reported_guard_coverage_diagnostic() {
        for (sent, expected) in [
            ("covered", Some("covered")),
            ("unavailable", Some("unavailable")),
            ("not_applicable", Some("not_applicable")),
        ] {
            let pool = test_utils::mem_test_pool().await;
            set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
                .await
                .expect("pc-01 should bind to test MAC");
            let (address, server) = spawn_test_server(pool.clone()).await;
            let client = reqwest::Client::new();
            let response = client
                .post(format!("http://{address}/api/identity/heartbeat"))
                .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                .json(&serde_json::json!({
                    "hostname": "TEST-PC",
                    "client_version": "0.1.0",
                    "guard_coverage": sent
                }))
                .send()
                .await
                .expect("heartbeat request should complete");
            assert_eq!(response.status(), StatusCode::OK);

            let stored: Option<String> =
                sqlx::query_scalar("SELECT last_guard_coverage FROM pcs WHERE id = 'pc-01'")
                    .fetch_one(&pool)
                    .await
                    .expect("guard diagnostic should be persisted");
            assert_eq!(stored.as_deref(), expected, "diagnostic: {sent}");
            server.abort();
        }
    }

    #[tokio::test]
    async fn heartbeat_without_guard_field_resets_unknown_and_invalid_is_rejected() {
        let pool = test_utils::mem_test_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();

        // An older Client that omits the field reads as unsupported or
        // unknown â€” and a stale report from a previous Client build must not
        // survive the swap, so omission resets any stored diagnostic.
        sqlx::query("UPDATE pcs SET last_guard_coverage = 'unavailable' WHERE id = 'pc-01'")
            .execute(&pool)
            .await
            .expect("seed stale diagnostic");
        let response = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0"
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let stored: Option<String> =
            sqlx::query_scalar("SELECT last_guard_coverage FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("guard diagnostic column should exist");
        assert_eq!(stored, None);

        // A present but untyped value is rejected at the boundary.
        let response = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "guard_coverage": "totally-fine"
            }))
            .send()
            .await
            .expect("invalid heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        server.abort();
    }

    #[tokio::test]
    async fn heartbeat_attributes_clock_skewed_clients_from_report_age() {
        for (label, timestamp) in [
            (
                "client-hours-behind",
                (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339(),
            ),
            (
                "client-hours-ahead",
                (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339(),
            ),
        ] {
            let pool = test_utils::mem_test_pool().await;
            set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
                .await
                .expect("pc-01 should bind to test MAC");
            let (address, server) = spawn_test_server(pool.clone()).await;
            let client = reqwest::Client::new();
            let (_, account_id) = signup(&client, &address, label, "pc-01").await;
            sqlx::query(
                "UPDATE account_session_history
                 SET started_at = datetime('now', '-3 hours')
                 WHERE account_id = ?",
            )
            .bind(&account_id)
            .execute(&pool)
            .await
            .expect("session history should be moved before the report");

            let response = client
                .post(format!("http://{address}/api/identity/heartbeat"))
                .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                .json(&serde_json::json!({
                    "hostname": "TEST-PC",
                    "client_version": "0.1.0",
                    "client_time_unix_ms": chrono::Utc::now().timestamp_millis()
                        + if label == "client-hours-behind" { -7_200_000 } else { 7_200_000 },
                    "tamper_kill_timestamp": timestamp,
                    "tamper_kill_report_age_ms": 7_200_000,
                    "tamper_kill_attempt_count": 1
                }))
                .send()
                .await
                .expect("heartbeat request should complete");
            assert_eq!(response.status(), StatusCode::OK, "{label}");
            let body = response
                .json::<serde_json::Value>()
                .await
                .expect("heartbeat response should be JSON");
            assert_eq!(body["tamper_kill_acknowledged"], "recorded", "{label}");
            assert_eq!(body["tamper_kill_subject"], "account", "{label}");

            let audit_account: Option<String> = sqlx::query_scalar(
                "SELECT account_id FROM audit_log WHERE event_type = 'tamper_kill'",
            )
            .fetch_one(&pool)
            .await
            .expect("tamper audit should exist");
            assert_eq!(
                audit_account.as_deref(),
                Some(account_id.as_str()),
                "{label}"
            );
            let clock_difference: i64 =
                sqlx::query_scalar("SELECT clock_difference_ms FROM pcs WHERE id = 'pc-01'")
                    .fetch_one(&pool)
                    .await
                    .expect("clock difference should be persisted");
            if label == "client-hours-behind" {
                assert!(clock_difference > 7_000_000, "{label}: {clock_difference}");
            } else {
                assert!(clock_difference < -7_000_000, "{label}: {clock_difference}");
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn heartbeat_age_zero_anchors_at_receipt_and_retry_is_idempotent() {
        let pool = test_utils::mem_test_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (_, account_id) = signup(&client, &address, "age-zero", "pc-01").await;
        sqlx::query(
            "UPDATE account_session_history
             SET started_at = datetime('now', '-1 hour')
             WHERE account_id = ?",
        )
        .bind(&account_id)
        .execute(&pool)
        .await
        .expect("session history should be moved before the report");

        let payload = serde_json::json!({
            "hostname": "TEST-PC",
            "client_version": "0.1.0",
            "tamper_kill_timestamp": "device-clock-report-name",
            "tamper_kill_report_age_ms": 0,
            "tamper_kill_attempt_count": 2
        });
        for _ in 0..2 {
            let response = client
                .post(format!("http://{address}/api/identity/heartbeat"))
                .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                .json(&payload)
                .send()
                .await
                .expect("heartbeat request should complete");
            assert_eq!(response.status(), StatusCode::OK);
            let body = response
                .json::<serde_json::Value>()
                .await
                .expect("heartbeat response should be JSON");
            assert_eq!(body["tamper_kill_subject"], "account");
        }

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await
                .expect("tamper audit count");
        assert_eq!(count, 1);
        server.abort();
    }

    #[tokio::test]
    async fn heartbeat_age_boundaries_acknowledge_without_old_clock_fallback() {
        let pool = test_utils::mem_test_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (_, account_id) = signup(&client, &address, "age-boundaries", "pc-01").await;

        let too_old = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "tamper_kill_timestamp": "too-old-report",
                "tamper_kill_report_age_ms": 86_400_001
            }))
            .send()
            .await
            .expect("too-old heartbeat should complete");
        assert_eq!(too_old.status(), StatusCode::OK);
        let too_old_body = too_old
            .json::<serde_json::Value>()
            .await
            .expect("too-old response should be JSON");
        assert_eq!(too_old_body["tamper_kill_subject"], "unknown");
        let too_old_reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason')
             FROM audit_log
             WHERE event_type = 'tamper_kill'
               AND json_extract(metadata_json, '$.kill_timestamp') = 'too-old-report'",
        )
        .fetch_one(&pool)
        .await
        .expect("over-limit reason should be recorded");
        assert_eq!(too_old_reason, "invalid_report_age");

        let malformed_age = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "tamper_kill_timestamp": "malformed-age-report",
                "tamper_kill_report_age_ms": "not-a-duration"
            }))
            .send()
            .await
            .expect("malformed-age heartbeat should complete");
        assert_eq!(malformed_age.status(), StatusCode::OK);
        assert_eq!(
            malformed_age
                .json::<serde_json::Value>()
                .await
                .expect("malformed-age response should be JSON")["tamper_kill_subject"],
            "unknown"
        );
        let malformed_age_reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason')
             FROM audit_log
             WHERE event_type = 'tamper_kill'
               AND json_extract(metadata_json, '$.kill_timestamp') = 'malformed-age-report'",
        )
        .fetch_one(&pool)
        .await
        .expect("malformed-age reason should be recorded");
        assert_eq!(malformed_age_reason, "invalid_report_age");

        let no_age = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "tamper_kill_timestamp": "old-client-report"
            }))
            .send()
            .await
            .expect("old-client heartbeat should complete");
        assert_eq!(no_age.status(), StatusCode::OK);
        assert_eq!(
            no_age
                .json::<serde_json::Value>()
                .await
                .expect("old-client response should be JSON")["tamper_kill_subject"],
            "unknown"
        );
        let no_age_reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason')
             FROM audit_log
             WHERE event_type = 'tamper_kill'
               AND json_extract(metadata_json, '$.kill_timestamp') = 'old-client-report'",
        )
        .fetch_one(&pool)
        .await
        .expect("missing-age reason should be recorded");
        assert_eq!(no_age_reason, "missing_report_age");

        let missing_timestamp = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "tamper_kill_report_age_ms": 1
            }))
            .send()
            .await
            .expect("missing timestamp heartbeat should complete");
        assert_eq!(missing_timestamp.status(), StatusCode::BAD_REQUEST);

        let negative_age = client
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "tamper_kill_timestamp": "backwards-clock-report",
                "tamper_kill_report_age_ms": -1
            }))
            .send()
            .await
            .expect("negative age heartbeat should complete");
        assert_eq!(negative_age.status(), StatusCode::BAD_REQUEST);

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await
        .expect("tamper audit count");
        assert_eq!(audit_count, 0);
        let report_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await
                .expect("tamper report count");
        assert_eq!(report_count, 3);
        server.abort();
    }

    #[tokio::test]
    async fn event_endpoint_streams_initial_pc_state() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");

        let (address, server) = spawn_test_server(pool).await;

        let response = reqwest::Client::new()
            .get(format!("http://{address}/api/identity/events"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .expect("event request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );

        let mut stream = response.bytes_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("initial event should arrive promptly")
            .expect("stream should contain an event")
            .expect("event chunk should be readable");
        let event = String::from_utf8_lossy(&chunk);
        assert!(event.contains("event: pc.updated"));
        assert!(event.contains("\"id\":\"pc-01\""));
        assert!(event.contains("\"attendance_supported\":true"));

        server.abort();
    }

    #[tokio::test]
    async fn disconnect_endpoint_marks_pc_offline_without_losing_time() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        db::simulate_coin(&pool, "pc-01", 100)
            .await
            .expect("test time should be added");
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        db::heartbeat_by_identity_only(&pool, "pc-01")
            .await
            .expect("test PC should connect");

        let (address, server) = spawn_test_server(pool).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/disconnect"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .send()
            .await
            .expect("disconnect request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let pc = response
            .json::<serde_json::Value>()
            .await
            .expect("disconnect response should be JSON");
        assert_eq!(pc["status"], "offline");
        assert_eq!(pc["remaining_seconds"], 300);

        server.abort();
    }

    #[tokio::test]
    async fn simulator_coin_endpoint_adds_authoritative_time() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(
                    pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    crate::forwarded_log::test_forwarded_log(),
                ),
            )
            .await
            .expect("test server should run");
        });

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "dev-simulator",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": "2026-06-20T10:00:00Z",
                "signature": null
            }))
            .send()
            .await
            .expect("coin request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let created = response
            .json::<serde_json::Value>()
            .await
            .expect("coin response should be JSON");
        assert_eq!(created["source"], "simulator");
        assert_eq!(created["seconds_added"], 300);

        server.abort();
    }

    #[tokio::test]
    async fn closed_shop_refuses_coin_event_with_trace_and_denial_audit() {
        let pool = test_utils::mem_test_pool().await;
        db::update_shop_hours(
            &pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await
        .expect("Shop Hours should save");
        let audit_pool = pool.clone();
        let (address, server) = spawn_test_server(pool).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "dev-simulator",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": "2026-06-20T10:00:00Z",
                "signature": null
            }))
            .send()
            .await
            .expect("coin request should complete");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.text().await.expect("error body"), "shop_closed");

        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&audit_pool)
                .await
                .expect("PC should remain unchanged");
        assert_eq!(remaining, 0);
        let (event_type, outcome, metadata): (String, String, String) = sqlx::query_as(
            "SELECT event_type, outcome, metadata_json
             FROM audit_log WHERE event_type = 'coin_denied'",
        )
        .fetch_one(&audit_pool)
        .await
        .expect("denial audit should be present");
        assert_eq!(event_type, "coin_denied");
        assert_eq!(outcome, "denied");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata JSON");
        assert_eq!(metadata["reason"], "shop_closed");
        assert!(metadata["coin_trace_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        server.abort();
    }

    #[tokio::test]
    async fn open_shop_accepts_coin_event_and_disabled_shop_hours_stays_open() {
        for (enabled, mode) in [
            (true, crate::models::ShopHoursDayMode::Open24Hours),
            (false, crate::models::ShopHoursDayMode::ClosedAllDay),
        ] {
            let pool = test_utils::mem_test_pool().await;
            db::update_shop_hours(&pool, &shop_hours_input(enabled, mode))
                .await
                .expect("Shop Hours should save");
            let (address, server) = spawn_test_server(pool).await;
            let response = reqwest::Client::new()
                .post(format!("http://{address}/api/coin-events"))
                .json(&serde_json::json!({
                    "device_id": "dev-simulator",
                    "pc_id": "pc-01",
                    "amount_cents": 100,
                    "pulse_count": 1,
                    "timestamp": "2026-06-20T10:00:00Z",
                    "signature": null
                }))
                .send()
                .await
                .expect("coin request should complete");
            assert_eq!(response.status(), StatusCode::OK);
            let created = response
                .json::<serde_json::Value>()
                .await
                .expect("coin response should be JSON");
            assert_eq!(created["seconds_added"], 300);
            server.abort();
        }
    }

    #[tokio::test]
    async fn real_device_coin_endpoint_rejects_invalid_secret() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        sqlx::query(
            "INSERT INTO coin_devices (id, name, secret, active)
             VALUES ('device-01', 'Test Coin Device', 'test-secret', 1)",
        )
        .execute(&pool)
        .await
        .expect("test device should be inserted");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(
                    pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    crate::forwarded_log::test_forwarded_log(),
                ),
            )
            .await
            .expect("test server should run");
        });

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "device-01",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": "2026-06-20T10:00:00Z",
                "signature": "wrong-secret"
            }))
            .send()
            .await
            .expect("coin request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = client
            .post(format!("http://{address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "device-01",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": "2026-06-20T10:00:00Z",
                "signature": "test-secret"
            }))
            .send()
            .await
            .expect("authorized coin request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let created = response
            .json::<serde_json::Value>()
            .await
            .expect("coin response should be JSON");
        assert_eq!(created["source"], "coin_device");
        assert_eq!(created["seconds_added"], 300);

        server.abort();
    }

    #[tokio::test]
    async fn login_accepts_spaced_username_collapsing_whitespace_to_underscore() {
        let pool = test_utils::mem_test_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();

        // Sign up with an underscored username (no spaces allowed).
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "Juan_Smith",
                "password": "secret",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);

        // Login with spaces â€” the lenient normaliser collapses them to _.
        let login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "  Juan  Smith  ",
                "password": "secret",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(login.status(), StatusCode::OK);
        let body = login.json::<serde_json::Value>().await.expect("login JSON");
        assert_eq!(body["account"]["username"], "Juan_Smith");
        let account_id = body["account"]["id"]
            .as_str()
            .expect("login response account id");
        let login_successes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE account_id = ? AND event_type = 'login_succeeded'",
        )
        .bind(account_id)
        .fetch_one(&pool)
        .await
        .expect("successful login audit count");
        assert_eq!(login_successes, 2);
    }

    /// Login leniency must not leak into signup: no stored Username may ever
    /// contain a space, which is what makes a typed space unambiguous.
    #[tokio::test]
    async fn signup_rejects_a_username_containing_a_space() {
        let pool = test_utils::mem_test_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();

        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "Juan Smith",
                "password": "secret",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::BAD_REQUEST);
        let body = signup.text().await.expect("rejection body should be text");
        assert!(
            body.contains("username may contain only"),
            "rejection must be the Username rule, got: {body}"
        );

        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .expect("account count should be readable");
        assert_eq!(stored, 0, "a spaced username must not create an Account");
    }

    #[tokio::test]
    async fn signup_rejects_every_deleted_username_and_login_uses_active_history() {
        let pool = test_utils::mem_test_pool().await;
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
        account_import::commit(
            &pool,
            serde_json::json!({
                "dry_run": false,
                "accounts": {
                    "ImportedDeleted": {
                        "id": "imported-id",
                        "password": bcrypt::hash("imported-password", 4).expect("bcrypt hash"),
                        "credits": 1,
                        "points": 1,
                        "enabled": true,
                        "last": null
                    }
                }
            }),
        )
        .await
        .expect("imported account");
        let imported_id: String = sqlx::query_scalar(
            "SELECT id FROM accounts WHERE username_normalized = 'importeddeleted'",
        )
        .fetch_one(&pool)
        .await
        .expect("imported account id");

        accounts::soft_delete(&pool, &native.id)
            .await
            .expect("delete native account");
        accounts::soft_delete(&pool, &self_created.id)
            .await
            .expect("delete self-service account");
        accounts::soft_delete(&pool, &imported_id)
            .await
            .expect("delete imported account");

        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        for username in ["NativeDeleted", "ImportedDeleted", "SelfDeleted"] {
            let signup = client
                .post(format!("http://{address}/api/accounts/signup"))
                .json(&serde_json::json!({
                    "username": username,
                    "password": "new-password",
                    "pc_id": "pc-01"
                }))
                .send()
                .await
                .expect("signup should complete");
            assert_eq!(signup.status(), StatusCode::CONFLICT);
        }

        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash)
             VALUES (?, ?, ?, ?)",
        )
        .bind("active-native")
        .bind("NativeDeleted")
        .bind("nativedeleted")
        .bind(crate::auth::hash_password("active-password").expect("active hash"))
        .execute(&pool)
        .await
        .expect("active replacement");

        let old_login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "NativeDeleted",
                "password": "native-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("old login should complete");
        assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);
        let active_login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "nativedeleted",
                "password": "active-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("active login should complete");
        assert_eq!(active_login.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn account_signup_is_case_insensitive_and_returns_no_password() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(
                    pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    crate::forwarded_log::test_forwarded_log(),
                )
                .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test server should run");
        });

        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "Customer_01",
                "password": "secret-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let body = signup
            .json::<serde_json::Value>()
            .await
            .expect("signup response should be JSON");
        assert_eq!(body["account"]["username"], "Customer_01");
        assert!(body.get("password").is_none());
        assert!(body["token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()));

        let login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "customer_01",
                "password": "secret-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(login.status(), StatusCode::OK);

        server.abort();
    }

    #[tokio::test]
    async fn legacy_bcrypt_login_upgrades_credential_through_http_seam() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");

        // Insert an account with a legacy bcrypt hash (simulating import).
        let bcrypt_hash = bcrypt::hash("imported-pw", 4).expect("bcrypt hash should succeed");
        sqlx::query(
            "INSERT INTO accounts (
               id, username, username_normalized, password_hash, remaining_seconds,
               points_balance_centi, status, imported_from, imported_at
             ) VALUES (?1, ?2, ?2, ?3, 90, 225, 'active', 'martec', '2026-07-31T00:00:00Z')",
        )
        .bind("bcrypt-seam-id")
        .bind("seam_user")
        .bind(&bcrypt_hash)
        .execute(&pool)
        .await
        .expect("bcrypt account should insert");

        let (address, server) = spawn_test_server(pool.clone()).await;
        let client = reqwest::Client::new();

        // Login with the correct password â€” should succeed and upgrade.
        let login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "seam user",
                "password": "imported-pw",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(login.status(), StatusCode::OK);
        let login_body = login.json::<serde_json::Value>().await.expect("login JSON");
        assert_eq!(login_body["account"]["remaining_seconds"], 90);
        assert_eq!(login_body["account"]["points_balance_centi"], 225);

        // The stored hash is now Argon2.
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM accounts WHERE username = 'seam_user'")
                .fetch_one(&pool)
                .await
                .expect("stored hash should be readable");
        assert!(
            stored_hash.starts_with("$argon2"),
            "credential should have been upgraded to Argon2, got: {stored_hash}"
        );

        // The same password authenticates on the next login.
        let again = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "seam user",
                "password": "imported-pw",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("second login should complete");
        assert_eq!(again.status(), StatusCode::OK);

        // A wrong password is rejected.
        let bad = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "seam user",
                "password": "wrong-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("bad login should complete");
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        server.abort();
    }

    #[tokio::test]
    async fn account_claim_and_logout_preserve_authoritative_balances() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        db::simulate_coin(&pool, "pc-01", 100)
            .await
            .expect("test Guest Time should be added");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address");
        let server_pool = pool.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(
                    server_pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    crate::forwarded_log::test_forwarded_log(),
                ),
            )
            .await
            .expect("test server should run");
        });

        let client = reqwest::Client::new();
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "claim-user",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("signup response should be JSON");
        assert_eq!(auth["session"]["claimable_guest_seconds"], 300);
        let token = auth["token"]
            .as_str()
            .expect("auth response should contain a token");

        let claim = client
            .post(format!("http://{address}/api/accounts/claim"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("claim should complete");
        assert_eq!(claim.status(), StatusCode::OK);
        let claimed = claim
            .json::<serde_json::Value>()
            .await
            .expect("claim response should be JSON");
        assert_eq!(claimed["account"]["remaining_seconds"], 300);
        assert_eq!(claimed["session"]["claimable_guest_seconds"], 0);

        let logout = client
            .post(format!("http://{address}/api/accounts/logout"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("logout should complete");
        assert_eq!(logout.status(), StatusCode::OK);

        let account_remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE username = ?")
                .bind("claim-user")
                .fetch_one(&pool)
                .await
                .expect("account balance should be readable");
        let guest_remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("Guest Time should be readable");
        assert_eq!((account_remaining, guest_remaining), (300, 0));

        server.abort();
    }

    #[tokio::test]
    async fn forwarded_diagnostics_endpoint_refuses_unauthenticated_device() {
        let pool = seeded_pool().await;
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = forwarded_test_server(pool, log_dir.path()).await;
        let client = reqwest::Client::new();

        // No MAC header at all: the device never authenticated.
        let response = client
            .post(format!("http://{address}/api/forwarded-diagnostics"))
            .json(&serde_json::json!({
                "source": "client",
                "lines": [{ "level": "warn", "target": "openpiso_client", "message": "boom", "timestamp_unix_ms": 1_000 }]
            }))
            .send()
            .await
            .expect("forwarded diagnostics request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // A MAC the admin does not know: refused, and nothing is logged.
        let response = client
            .post(format!("http://{address}/api/forwarded-diagnostics"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:ff")
            .json(&serde_json::json!({
                "source": "client",
                "lines": [{ "level": "warn", "target": "openpiso_client", "message": "boom", "timestamp_unix_ms": 1_000 }]
            }))
            .send()
            .await
            .expect("forwarded diagnostics request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let entries = std::fs::read_dir(log_dir.path())
            .expect("log dir should exist")
            .count();
        assert_eq!(entries, 0, "refused devices must write nothing");

        server.abort();
    }

    #[tokio::test]
    async fn forwarded_diagnostics_endpoint_accepts_batch_into_forwarded_log() {
        let pool = seeded_pool().await;
        // Bind pc-01 to the test MAC so the endpoint recognises the device
        // (ADR-0031). Mirrors a real first-boot bind.
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = forwarded_test_server(pool, log_dir.path()).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/forwarded-diagnostics"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "source": "client",
                "lines": [
                    { "level": "warn", "target": "openpiso_client::tamper", "message": "guard pipe lost", "timestamp_unix_ms": 1_760_000_000_000i64 },
                    { "level": "info", "target": "openpiso_client::guard_client", "message": "connected to Guard pipe (waited_ms=2)", "timestamp_unix_ms": 1_760_000_030_000i64 },
                    { "level": "error", "target": "openpiso_client", "message": "heartbeat failed", "timestamp_unix_ms": 1_760_000_060_000i64 }
                ]
            }))
            .send()
            .await
            .expect("forwarded diagnostics request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let ack = response
            .json::<serde_json::Value>()
            .await
            .expect("ack should be JSON");
        assert_eq!(ack["accepted"], 3);

        // The lines land in a dedicated file â€” no Admin own-log file is
        // created beside it â€” and every line carries the server-resolved PC
        // identity and the source discriminator.
        let files: Vec<String> = std::fs::read_dir(log_dir.path())
            .expect("log dir should exist")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(files.len(), 1, "exactly the forwarded file: {files:?}");
        assert!(files[0].starts_with("forwarded."), "{files:?}");

        let written =
            std::fs::read_to_string(log_dir.path().join(&files[0])).expect("log readable");
        let parsed: Vec<serde_json::Value> = written
            .lines()
            .map(|line| serde_json::from_str(line).expect("each forwarded line is JSON"))
            .collect();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0]["pc_id"], "pc-01");
        assert_eq!(parsed[0]["source"], "client");
        assert_eq!(parsed[0]["level"], "warn");
        assert_eq!(parsed[0]["message"], "guard pipe lost");
        assert_eq!(
            parsed[1]["level"], "info",
            "an allowlisted connect edge lands too"
        );
        assert_eq!(
            parsed[1]["message"],
            "connected to Guard pipe (waited_ms=2)"
        );
        assert_eq!(parsed[2]["level"], "error");

        server.abort();
    }

    #[tokio::test]
    async fn forwarded_diagnostics_endpoint_rejects_malformed_batch_without_partial_acceptance() {
        let pool = seeded_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = forwarded_test_server(pool, log_dir.path()).await;
        let client = reqwest::Client::new();
        let post = |body: serde_json::Value| {
            let client = client.clone();
            let address = address.clone();
            async move {
                client
                    .post(format!("http://{address}/api/forwarded-diagnostics"))
                    .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                    .json(&body)
                    .send()
                    .await
                    .expect("forwarded diagnostics request should complete")
                    .status()
            }
        };
        let line = |level: &str, message: &str| {
            serde_json::json!({
                "level": level,
                "target": "openpiso_client",
                "message": message,
                "timestamp_unix_ms": 1_000i64,
            })
        };

        // An empty batch and an oversized line are malformed.
        assert_eq!(
            post(serde_json::json!({ "source": "client", "lines": [] })).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(serde_json::json!({
                "source": "client",
                "lines": [
                    line("warn", "fine"),
                    line("error", &"x".repeat(openpiso_core::forwarding::MAX_FORWARDED_MESSAGE_BYTES + 1))
                ]
            }))
            .await,
            StatusCode::BAD_REQUEST
        );
        // Unknown level and source values are refused by deserialisation.
        assert_eq!(
            post(serde_json::json!({ "source": "client", "lines": [line("fatal", "boom")] })).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            post(serde_json::json!({
                "source": "owner",
                "lines": [line("warn", "boom")]
            }))
            .await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        // A batch above the line cap is refused whole.
        assert_eq!(
            post(serde_json::json!({
                "source": "client",
                "lines": (0..openpiso_core::forwarding::MAX_FORWARDED_BATCH_LINES + 1)
                    .map(|index| line("warn", &format!("flood-{index}")))
                    .collect::<Vec<_>>()
            }))
            .await,
            StatusCode::BAD_REQUEST
        );

        let entries = std::fs::read_dir(log_dir.path())
            .expect("log dir should exist")
            .count();
        assert_eq!(
            entries, 0,
            "a malformed batch must not be partially accepted"
        );

        server.abort();
    }

    #[tokio::test]
    async fn a_diagnostic_request_is_delivered_on_the_heartbeat_and_consumed_once() {
        let pool = seeded_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let diagnostic_requests = DiagnosticRequestRegistry::new();
        let armed = diagnostic_requests.arm("pc-01", chrono::Utc::now());
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = diagnostic_test_server(
            pool,
            log_dir.path(),
            GuardRegistry::new(),
            diagnostic_requests,
        )
        .await;
        let client = reqwest::Client::new();
        let heartbeat = || {
            let client = client.clone();
            let address = address.clone();
            async move {
                client
                    .post(format!("http://{address}/api/identity/heartbeat"))
                    .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                    .json(&serde_json::json!({
                        "hostname": "TEST-PC",
                        "client_version": "0.1.0"
                    }))
                    .send()
                    .await
                    .expect("heartbeat should complete")
                    .json::<serde_json::Value>()
                    .await
                    .expect("heartbeat response should be JSON")
            }
        };

        // The armed ask rides the response, in the shape of the
        // acknowledgement fields, and the same response may repeat.
        assert_eq!(
            heartbeat().await["diagnostic_request_nonce"],
            serde_json::json!(armed.nonce)
        );
        assert_eq!(
            heartbeat().await["diagnostic_request_nonce"],
            serde_json::json!(armed.nonce),
            "the nonce repeats until the Client answers; the Client acts once per nonce"
        );

        // The Client's answer: a slice batch echoing the nonce through the
        // existing diagnostics endpoint.
        let slice = client
            .post(format!("http://{address}/api/forwarded-diagnostics"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "source": "client",
                "diagnostic_request_nonce": armed.nonce,
                "lines": [
                    { "level": "info", "target": "openpiso_client::diagnostic_request", "message": "Diagnostic Request slice", "timestamp_unix_ms": 1_000i64 },
                    { "level": "debug", "target": "openpiso_client", "message": "quiet evidence", "timestamp_unix_ms": 1_500i64 }
                ]
            }))
            .send()
            .await
            .expect("slice upload should complete");
        assert_eq!(slice.status(), StatusCode::OK);

        // Fulfilled: the ask stops riding the response, so a later Client
        // never acts again on a nonce it already answered.
        assert!(
            heartbeat().await.get("diagnostic_request_nonce").is_none(),
            "a consumed ask must not repeat"
        );

        // The slice landed in the forwarded log, tagged client, faithful to
        // the levels it found.
        let parsed = forwarded_log_records(log_dir.path());
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["source"], "client");
        assert_eq!(parsed[0]["level"], "info");
        assert_eq!(
            parsed[1]["level"], "debug",
            "the slice keeps DEBUG as DEBUG"
        );

        server.abort();
    }

    #[tokio::test]
    async fn a_foreign_nonce_and_a_malformed_slice_never_consume_the_ask() {
        let pool = seeded_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let diagnostic_requests = DiagnosticRequestRegistry::new();
        let armed = diagnostic_requests.arm("pc-01", chrono::Utc::now());
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = diagnostic_test_server(
            pool,
            log_dir.path(),
            GuardRegistry::new(),
            diagnostic_requests,
        )
        .await;
        let client = reqwest::Client::new();

        // A regular forwarding batch (no nonce) and a batch echoing a nonce
        // this PC was never armed with both leave the ask armed.
        for nonce in [None, Some("not-the-armed-nonce")] {
            let mut body = serde_json::json!({
                "source": "client",
                "lines": [
                    { "level": "warn", "target": "openpiso_client", "message": "boom", "timestamp_unix_ms": 1_000i64 }
                ]
            });
            if let Some(nonce) = nonce {
                body["diagnostic_request_nonce"] = serde_json::json!(nonce);
            }
            let response = client
                .post(format!("http://{address}/api/forwarded-diagnostics"))
                .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                .json(&body)
                .send()
                .await
                .expect("forwarded diagnostics request should complete");
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(
            client
                .post(format!("http://{address}/api/identity/heartbeat"))
                .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
                .json(&serde_json::json!({
                    "hostname": "TEST-PC",
                    "client_version": "0.1.0"
                }))
                .send()
                .await
                .expect("heartbeat should complete")
                .json::<serde_json::Value>()
                .await
                .expect("heartbeat response should be JSON")["diagnostic_request_nonce"],
            serde_json::json!(armed.nonce),
            "the ask survives batches that are not its answer"
        );

        server.abort();
    }

    /// Spawn a forwarded-diagnostics test server whose sink writes into
    /// `log_dir`, so the tests can assert on the file contents.
    async fn forwarded_test_server(
        pool: SqlitePool,
        log_dir: &std::path::Path,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address")
            .to_string();
        let sink = Arc::new(ForwardedLogSink::new(log_dir.to_path_buf()));
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                router(
                    pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    sink,
                ),
            )
            .await
            .expect("test server should run");
        });
        (address, handle)
    }

    /// Spawn a Guard Poll test server with a controlled Guard registry and a
    /// forwarded sink that writes into `log_dir`, so a test can plant an
    /// order and assert on both the response and the forwarded file.
    async fn guard_test_server(
        pool: SqlitePool,
        log_dir: &std::path::Path,
        guards: GuardRegistry,
    ) -> (String, tokio::task::JoinHandle<()>) {
        diagnostic_test_server(pool, log_dir, guards, DiagnosticRequestRegistry::new()).await
    }

    /// Spawn a test server with controlled Guard and Diagnostic Request
    /// registries and a forwarded sink that writes into `log_dir`, so a test
    /// can plant an order or an owner's ask and assert on the responses and
    /// the forwarded file.
    async fn diagnostic_test_server(
        pool: SqlitePool,
        log_dir: &std::path::Path,
        guards: GuardRegistry,
        diagnostic_requests: DiagnosticRequestRegistry,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address")
            .to_string();
        let sink = Arc::new(ForwardedLogSink::new(log_dir.to_path_buf()));
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                router_with_throttle(
                    pool,
                    test_events(),
                    AccountTokens::default(),
                    SessionStore::default(),
                    AccountLoginThrottle::default(),
                    guards,
                    sink,
                    diagnostic_requests,
                )
                .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test server should run");
        });
        (address, handle)
    }

    fn forwarded_log_records(log_dir: &std::path::Path) -> Vec<serde_json::Value> {
        let file: Vec<_> = std::fs::read_dir(log_dir)
            .expect("log dir should exist")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(file.len(), 1, "exactly the forwarded file: {file:?}");
        std::fs::read_to_string(&file[0])
            .expect("log readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each forwarded line is JSON"))
            .collect()
    }

    #[tokio::test]
    async fn guard_poll_accepts_a_widened_body_and_keeps_its_order_response() {
        let pool = seeded_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        // A Guard whose Client never connected still reports absence; the
        // Admin may already have decided to escalate. The widened poll must
        // neither hide nor change that order.
        let guards = GuardRegistry::new();
        guards.arm_power_off("pc-01", "order-nonce-1".to_owned(), chrono::Utc::now());
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = guard_test_server(pool, log_dir.path(), guards).await;
        let client = reqwest::Client::new();

        // The Guard rejects a foreign Client image and no Client is
        // connected; the rejection rides the poll it makes anyway.
        let response = client
            .post(format!("http://{address}/api/guard/state"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "client_connected": false,
                "disarmed": false,
                "relaunch_attempts": 2,
                "forwarded": {
                    "lines": [
                        { "level": "warn", "target": "openpiso_client_guard::pipe", "message": "rejecting foreign pipe client (process_id=99)", "timestamp_unix_ms": 1_760_000_000_000i64 },
                        { "level": "info", "target": "openpiso_client_guard::pipe", "message": "Client connected (process_id=42)", "timestamp_unix_ms": 1_760_000_030_000i64 }
                    ]
                }
            }))
            .send()
            .await
            .expect("guard poll should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let state = response
            .json::<serde_json::Value>()
            .await
            .expect("guard state should be JSON");
        assert_eq!(state["power_off"], true);
        assert_eq!(state["nonce"], "order-nonce-1");
        assert!(state["decided_at"].as_str().is_some());
        assert_eq!(state["poll_interval_secs"], 10);

        // The carried lines land tagged with the server-resolved PC identity
        // and source `guard`.
        let parsed = forwarded_log_records(log_dir.path());
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["pc_id"], "pc-01");
        assert_eq!(parsed[0]["source"], "guard");
        assert_eq!(parsed[0]["level"], "warn");
        assert_eq!(
            parsed[0]["message"],
            "rejecting foreign pipe client (process_id=99)"
        );
        assert_eq!(parsed[1]["source"], "guard");
        assert_eq!(parsed[1]["level"], "info");
        assert_eq!(parsed[1]["message"], "Client connected (process_id=42)");

        // A later poll without a batch reads the same desired state: the
        // order semantics are unchanged by the widening.
        let response = client
            .post(format!("http://{address}/api/guard/state"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "client_connected": false,
                "disarmed": false,
                "relaunch_attempts": 2
            }))
            .send()
            .await
            .expect("guard poll should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let state = response
            .json::<serde_json::Value>()
            .await
            .expect("guard state should be JSON");
        assert_eq!(state["power_off"], true);
        assert_eq!(state["nonce"], "order-nonce-1");
        assert_eq!(state["poll_interval_secs"], 10);
        assert_eq!(
            forwarded_log_records(log_dir.path()).len(),
            2,
            "a batchless poll forwards nothing"
        );

        server.abort();
    }

    #[tokio::test]
    async fn guard_poll_refuses_a_batch_it_cannot_attribute_and_malformed_batches_whole() {
        let pool = seeded_pool().await;
        set_pc_identity(&pool, "pc-01", None, Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should bind to test MAC");
        let log_dir = tempfile::tempdir().expect("forwarded log temp dir");
        let (address, server) = guard_test_server(pool, log_dir.path(), GuardRegistry::new()).await;
        let client = reqwest::Client::new();
        let post = |body: serde_json::Value, mac: Option<&str>| {
            let client = client.clone();
            let address = address.clone();
            let mac = mac.map(str::to_owned);
            async move {
                let mut request = client.post(format!("http://{address}/api/guard/state"));
                if let Some(mac) = mac {
                    request = request.header("X-OpenPiso-Mac", mac);
                }
                request
                    .json(&body)
                    .send()
                    .await
                    .expect("guard poll should complete")
                    .status()
            }
        };
        let line = |level: &str, message: &str| {
            serde_json::json!({
                "level": level,
                "target": "openpiso_client_guard::pipe",
                "message": message,
                "timestamp_unix_ms": 1_000i64,
            })
        };
        let widened = |lines: Vec<serde_json::Value>| {
            serde_json::json!({
                "client_connected": false,
                "disarmed": false,
                "relaunch_attempts": 0,
                "forwarded": { "lines": lines }
            })
        };

        // No MAC header and no batch: stay-on, exactly as before widening.
        let response = client
            .post(format!("http://{address}/api/guard/state"))
            .json(&serde_json::json!({
                "client_connected": false,
                "disarmed": false,
                "relaunch_attempts": 0
            }))
            .send()
            .await
            .expect("guard poll should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let state = response
            .json::<serde_json::Value>()
            .await
            .expect("guard state should be JSON");
        assert_eq!(state["power_off"], false);
        assert_eq!(state["poll_interval_secs"], 10);

        // A batch cannot be attributed without a MAC header.
        assert_eq!(
            post(widened(vec![line("warn", "boom")]), None).await,
            StatusCode::UNAUTHORIZED
        );
        // Nor from a MAC no registered PC holds.
        assert_eq!(
            post(
                widened(vec![line("warn", "boom")]),
                Some("ff:ee:dd:cc:bb:aa")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        // A malformed line rejects the whole poll at the Rust boundary.
        assert_eq!(
            post(
                widened(vec![line(
                    "warn",
                    &"x".repeat(openpiso_core::forwarding::MAX_FORWARDED_MESSAGE_BYTES + 1)
                )]),
                Some("aa:bb:cc:dd:ee:01")
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        // An unknown level is refused by deserialisation.
        assert_eq!(
            post(
                widened(vec![line("fatal", "boom")]),
                Some("aa:bb:cc:dd:ee:01")
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        // An empty batch is malformed too.
        assert_eq!(
            post(widened(Vec::new()), Some("aa:bb:cc:dd:ee:01")).await,
            StatusCode::BAD_REQUEST
        );

        let entries = std::fs::read_dir(log_dir.path())
            .expect("log dir should exist")
            .count();
        assert_eq!(
            entries, 0,
            "refused or malformed batches must write nothing"
        );

        // A valid batch from the registered Guard is accepted even while the
        // Guard reports stay-on facts.
        assert_eq!(
            post(
                widened(vec![line("warn", "client disconnected: pipe closed (EOF)")]),
                Some("aa:bb:cc:dd:ee:01")
            )
            .await,
            StatusCode::OK
        );
        let parsed = forwarded_log_records(log_dir.path());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["pc_id"], "pc-01");
        assert_eq!(parsed[0]["source"], "guard");

        server.abort();
    }

    // -- Phase 2 HTTP surface: shop, owner PIN, transfers, change-password ----

    /// Delegates to the shared in-memory test pool.
    async fn seeded_pool() -> SqlitePool {
        test_utils::mem_test_pool().await
    }

    async fn spawn_client_server(pool: SqlitePool) -> String {
        spawn_client_server_with_router(router(
            pool,
            test_events(),
            AccountTokens::default(),
            SessionStore::default(),
            crate::forwarded_log::test_forwarded_log(),
        ))
        .await
    }

    async fn spawn_client_server_with_router(router: Router) -> String {
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
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test server should run");
        });
        address
    }

    #[tokio::test]
    async fn shop_closed_gates_signup_login_and_voucher_without_login_failure_throttle() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, account_id) = signup(&client, &address, "closed-user", "pc-01").await;
        let voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await
        .expect("voucher should be created while Shop Open");
        db::update_shop_hours(
            &pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await
        .expect("Shop Hours should close");

        for (endpoint, body) in [
            (
                "/api/accounts/signup",
                serde_json::json!({
                    "username": "closed-new-user",
                    "password": "password",
                    "pc_id": "pc-02"
                }),
            ),
            (
                "/api/accounts/login",
                serde_json::json!({
                    "username": "closed-user",
                    "password": "wrong-password",
                    "pc_id": "pc-01"
                }),
            ),
        ] {
            let response = client
                .post(format!("http://{address}{endpoint}"))
                .json(&body)
                .send()
                .await
                .expect("gated account request should complete");
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(response.text().await.expect("error body"), "shop_closed");
        }

        let redemption = client
            .post(format!("http://{address}/api/vouchers/redeem"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({ "code": voucher.code }))
            .send()
            .await
            .expect("gated voucher request should complete");
        assert_eq!(redemption.status(), StatusCode::CONFLICT);
        assert_eq!(redemption.text().await.expect("error body"), "shop_closed");

        let login_denial: (String, String, String, String) = sqlx::query_as(
            "SELECT actor, account_id, actor_account_id, metadata_json
             FROM audit_log WHERE event_type = 'login_failed'",
        )
        .fetch_one(&pool)
        .await
        .expect("Shop Closed login denial should be audited");
        assert_eq!(login_denial.0, "account");
        assert_eq!(login_denial.1, account_id);
        assert_eq!(login_denial.2, account_id);
        let metadata: serde_json::Value =
            serde_json::from_str(&login_denial.3).expect("login denial metadata");
        assert_eq!(metadata["reason"], "shop_closed");
        let signup_denial: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'account_create_failed'",
        )
        .fetch_one(&pool)
        .await
        .expect("Shop Closed signup denial should be audited");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&signup_denial)
                .expect("signup denial metadata")["reason"],
            "shop_closed"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'login_failed'",
            )
            .fetch_one(&pool)
            .await
            .expect("login denial count"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM accounts WHERE username_normalized = 'closed-new-user'",
            )
            .fetch_one(&pool)
            .await
            .expect("signup should not create an account"),
            0
        );
    }

    #[tokio::test]
    async fn shop_open_accepts_signup_login_and_voucher_redemption() {
        let pool = seeded_pool().await;
        db::update_shop_hours(
            &pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::Open24Hours),
        )
        .await
        .expect("Shop Hours should be open all day");
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (_, _) = signup(&client, &address, "open-user", "pc-01").await;

        let login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "open-user",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("login should complete while Shop Open");
        assert_eq!(login.status(), StatusCode::OK);
        let login_token = login.json::<serde_json::Value>().await.expect("login JSON")["token"]
            .as_str()
            .expect("login token")
            .to_owned();

        let voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await
        .expect("voucher should be created");
        let redemption = client
            .post(format!("http://{address}/api/vouchers/redeem"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {login_token}"),
            )
            .json(&serde_json::json!({ "code": voucher.code }))
            .send()
            .await
            .expect("voucher should redeem while Shop Open");
        assert_eq!(redemption.status(), StatusCode::OK);

        db::update_shop_hours(
            &pool,
            &shop_hours_input(false, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await
        .expect("disabled Shop Hours should leave the shop open");
        let disabled_signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "disabled-user",
                "password": "password",
                "pc_id": "pc-02"
            }))
            .send()
            .await
            .expect("signup should work with disabled Shop Hours");
        assert_eq!(disabled_signup.status(), StatusCode::OK);
        let disabled_signup_body = disabled_signup
            .json::<serde_json::Value>()
            .await
            .expect("disabled signup JSON");
        let disabled_login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "disabled-user",
                "password": "password",
                "pc_id": "pc-02"
            }))
            .send()
            .await
            .expect("login should work with disabled Shop Hours");
        assert_eq!(disabled_login.status(), StatusCode::OK);
        let disabled_token = disabled_login
            .json::<serde_json::Value>()
            .await
            .expect("disabled login JSON")["token"]
            .as_str()
            .expect("disabled login token")
            .to_owned();
        let disabled_voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await
        .expect("disabled voucher should be created");
        let disabled_redemption = client
            .post(format!("http://{address}/api/vouchers/redeem"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {disabled_token}"),
            )
            .json(&serde_json::json!({ "code": disabled_voucher.code }))
            .send()
            .await
            .expect("voucher should work with disabled Shop Hours");
        assert_eq!(disabled_redemption.status(), StatusCode::OK);
        assert!(disabled_signup_body["token"].as_str().is_some());
    }

    #[tokio::test]
    async fn force_closed_override_gates_all_24_hour_shop_entry_and_new_value() {
        let pool = seeded_pool().await;
        db::update_shop_hours(
            &pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::Open24Hours),
        )
        .await
        .expect("Shop Hours should be open all day");
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, _) = signup(&client, &address, "override-closed-user", "pc-01").await;
        let voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await
        .expect("voucher should be created before the override");
        db::set_shop_override(
            &pool,
            &crate::models::ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceClosed,
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            },
        )
        .await
        .expect("force-close override should save");

        for (endpoint, body) in [
            (
                "/api/accounts/signup",
                serde_json::json!({
                    "username": "override-closed-new",
                    "password": "password",
                    "pc_id": "pc-02"
                }),
            ),
            (
                "/api/accounts/login",
                serde_json::json!({
                    "username": "override-closed-user",
                    "password": "password",
                    "pc_id": "pc-01"
                }),
            ),
        ] {
            let response = client
                .post(format!("http://{address}{endpoint}"))
                .json(&body)
                .send()
                .await
                .expect("override-gated account request should complete");
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(response.text().await.expect("error body"), "shop_closed");
        }

        let redemption = client
            .post(format!("http://{address}/api/vouchers/redeem"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({ "code": voucher.code }))
            .send()
            .await
            .expect("override-gated voucher request should complete");
        assert_eq!(redemption.status(), StatusCode::CONFLICT);
        assert_eq!(redemption.text().await.expect("error body"), "shop_closed");

        let coin = client
            .post(format!("http://{address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "dev-simulator",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": "2026-06-20T10:00:00Z",
                "signature": null
            }))
            .send()
            .await
            .expect("override-gated coin request should complete");
        assert_eq!(coin.status(), StatusCode::CONFLICT);
        assert_eq!(coin.text().await.expect("error body"), "shop_closed");
    }

    #[tokio::test]
    async fn force_open_override_opens_all_closed_shop_entry_and_new_value() {
        let pool = seeded_pool().await;
        db::update_shop_hours(
            &pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await
        .expect("Shop Hours should be closed all day");
        db::set_shop_override(
            &pool,
            &crate::models::ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceOpen,
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            },
        )
        .await
        .expect("force-open override should save");
        let voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await
        .expect("voucher should be created");
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();
        let (_, _) = signup(&client, &address, "override-open-user", "pc-01").await;

        let login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "override-open-user",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("override-open login should complete");
        assert_eq!(login.status(), StatusCode::OK);
        let login_token = login
            .json::<serde_json::Value>()
            .await
            .expect("override-open login JSON")["token"]
            .as_str()
            .expect("override-open login token")
            .to_owned();

        let redemption = client
            .post(format!("http://{address}/api/vouchers/redeem"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {login_token}"),
            )
            .json(&serde_json::json!({ "code": voucher.code }))
            .send()
            .await
            .expect("override-open voucher request should complete");
        assert_eq!(redemption.status(), StatusCode::OK);

        let coin = client
            .post(format!("http://{address}/api/coin-events"))
            .json(&serde_json::json!({
                "device_id": "dev-simulator",
                "pc_id": "pc-01",
                "amount_cents": 100,
                "pulse_count": 1,
                "timestamp": "2026-06-20T10:00:00Z",
                "signature": null
            }))
            .send()
            .await
            .expect("override-open coin request should complete");
        assert_eq!(coin.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allowed_account_actions_continue_while_shop_closed() {
        let pool = seeded_pool().await;
        db::simulate_coin(&pool, "pc-01", 100)
            .await
            .expect("Guest Time should be available for Claim");
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await
            .expect("wheel should be enabled for the test");
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, _) = signup(&client, &address, "allowed-user", "pc-01").await;
        let (_, recipient_id) = signup(&client, &address, "allowed-recipient", "pc-02").await;
        sqlx::query(
            "UPDATE accounts SET points_balance_centi = 3_000 WHERE username_normalized = 'allowed-user'",
        )
        .execute(&pool)
        .await
        .expect("account should be funded");
        db::update_shop_hours(
            &pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await
        .expect("Shop Hours should close");

        let auth_header = format!("Bearer {token}");
        let claim = client
            .post(format!("http://{address}/api/accounts/claim"))
            .header(reqwest::header::AUTHORIZATION, &auth_header)
            .send()
            .await
            .expect("Claim should work while Shop Closed");
        assert_eq!(claim.status(), StatusCode::OK);

        let menu = client
            .get(format!("http://{address}/api/points/redemption-tiers"))
            .header(reqwest::header::AUTHORIZATION, &auth_header)
            .send()
            .await
            .expect("Redemption Tier menu should work while Shop Closed")
            .json::<serde_json::Value>()
            .await
            .expect("tier menu JSON");
        let tier_id = menu["tiers"][0]["id"].as_i64().expect("tier id");
        let version = menu["version"].as_i64().expect("tier version");
        let redemption = client
            .post(format!("http://{address}/api/points/redeem"))
            .header(reqwest::header::AUTHORIZATION, &auth_header)
            .json(&serde_json::json!({ "tier_id": tier_id, "version": version }))
            .send()
            .await
            .expect("points redemption should work while Shop Closed");
        assert_eq!(redemption.status(), StatusCode::OK);

        let spin = client
            .post(format!("http://{address}/api/wheel/spin"))
            .header(reqwest::header::AUTHORIZATION, &auth_header)
            .send()
            .await
            .expect("wheel spin should work while Shop Closed");
        assert_eq!(spin.status(), StatusCode::OK);

        let transfer = client
            .post(format!("http://{address}/api/accounts/transfer-points"))
            .header(reqwest::header::AUTHORIZATION, &auth_header)
            .json(&serde_json::json!({
                "to_username": "allowed-recipient",
                "points_centi": 100
            }))
            .send()
            .await
            .expect("points transfer should work while Shop Closed");
        assert_eq!(transfer.status(), StatusCode::OK);
        assert!(transfer
            .json::<serde_json::Value>()
            .await
            .expect("transfer JSON")["id"]
            .as_str()
            .is_some());
        assert!(!recipient_id.is_empty());

        let logout = client
            .post(format!("http://{address}/api/accounts/logout"))
            .header(reqwest::header::AUTHORIZATION, auth_header)
            .send()
            .await
            .expect("logout should work while Shop Closed");
        assert_eq!(logout.status(), StatusCode::OK);
    }

    /// Sign up an account on a PC and return `(token, account_id)`.
    async fn signup(
        client: &reqwest::Client,
        address: &str,
        username: &str,
        pc_id: &str,
    ) -> (String, String) {
        let body = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": username,
                "password": "password",
                "pc_id": pc_id
            }))
            .send()
            .await
            .expect("signup should complete")
            .json::<serde_json::Value>()
            .await
            .expect("signup JSON");
        (
            body["token"].as_str().expect("token").to_owned(),
            body["account"]["id"]
                .as_str()
                .expect("account id")
                .to_owned(),
        )
    }

    #[tokio::test]
    async fn voucher_redemption_http_accepts_dashless_batch_code_and_recognises_cash() {
        let pool = seeded_pool().await;
        let batch = db::create_voucher_batch(
            &pool,
            &crate::models::CreateVoucherBatchInput {
                prefix: "HTTP".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 600,
                points_value_centi: 2,
                cash_amount_cents: Some(500),
                expires_at: None,
            },
        )
        .await
        .expect("batch should be created");
        let code: String = sqlx::query_scalar("SELECT code FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_one(&pool)
            .await
            .expect("batch code");
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, _) = signup(&client, &address, "http-voucher-user", "pc-01").await;

        let response = client
            .post(format!("http://{address}/api/vouchers/redeem"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "code": code.replace('-', "").to_ascii_lowercase()
            }))
            .send()
            .await
            .expect("voucher redemption should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("voucher response JSON");
        assert_eq!(body["account"]["remaining_seconds"], 600);
        assert_eq!(body["account"]["points_balance_centi"], 2);
        assert_eq!(body["session"]["remaining_seconds"], 600);
        let cash_audit: (i64, i64) = sqlx::query_as(
            "SELECT amount_cents, cash_flag FROM audit_log
             WHERE event_type = 'sold_voucher_redeemed'",
        )
        .fetch_one(&pool)
        .await
        .expect("cash redemption audit");
        assert_eq!(cash_audit, (500, 1));
    }

    #[tokio::test]
    async fn redemption_tier_menu_is_authenticated_sorted_and_quote_is_removed() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();
        let (token, _) = signup(&client, &address, "tier-menu-user", "pc-01").await;
        let endpoint = format!("http://{address}/api/points/redemption-tiers");

        let unauthenticated = client.get(&endpoint).send().await.expect("menu request");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let menu = client
            .get(&endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("menu request")
            .json::<serde_json::Value>()
            .await
            .expect("menu JSON");
        assert_eq!(menu["version"], 1);
        assert_eq!(menu["tiers"][0]["points"], 10);
        assert_eq!(menu["tiers"][0]["seconds"], 14_400);
        assert!(menu["tiers"][0]["id"].is_number());

        let removed_quote = client
            .get(format!("http://{address}/api/points/redemption-quote"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("removed quote request");
        assert_eq!(removed_quote.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn coin_promo_ladder_is_account_authenticated_and_hides_disabled_or_empty_tiers() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, _) = signup(&client, &address, "coin-promo-menu-user", "pc-01").await;
        let endpoint = format!("http://{address}/api/coin-promo");

        let unauthenticated = client.get(&endpoint).send().await.expect("ladder request");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        sqlx::query(
            "INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (1000, 1500), (500, 600)",
        )
        .execute(&pool)
        .await
        .expect("seed Coin Promo Tiers");

        let disabled = client
            .get(&endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("disabled ladder request")
            .json::<serde_json::Value>()
            .await
            .expect("disabled ladder JSON");
        assert_eq!(
            disabled,
            serde_json::json!({ "enabled": false, "tiers": [] })
        );

        sqlx::query("UPDATE coin_promo_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await
            .expect("enable Coin Promo");
        let enabled = client
            .get(&endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("enabled ladder request")
            .json::<serde_json::Value>()
            .await
            .expect("enabled ladder JSON");
        assert_eq!(
            enabled,
            serde_json::json!({
                "enabled": true,
                "tiers": [
                    { "amount_cents": 500, "bonus_seconds": 600 },
                    { "amount_cents": 1000, "bonus_seconds": 1500 }
                ]
            })
        );

        sqlx::query("DELETE FROM coin_promo_tiers")
            .execute(&pool)
            .await
            .expect("clear Coin Promo Tiers");
        let empty = client
            .get(&endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("empty ladder request")
            .json::<serde_json::Value>()
            .await
            .expect("empty ladder JSON");
        assert_eq!(empty, serde_json::json!({ "enabled": true, "tiers": [] }));
    }

    #[tokio::test]
    async fn shop_endpoint_returns_public_settings_and_reflects_edits() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();

        let default = client
            .get(format!("http://{address}/api/shop"))
            .send()
            .await
            .expect("shop should complete")
            .json::<serde_json::Value>()
            .await
            .expect("shop JSON");
        assert_eq!(default["shop_name"], "OpenPiso Shop");
        assert_eq!(default["shutdown_grace_seconds"], 300);
        assert_eq!(default["offline_grace_seconds"], 600);
        assert_eq!(default["theme_version"], 1);
        assert_eq!(default["wheel_enabled"], false);
        // The public read never leaks the PIN or secret settings.
        assert!(default.get("owner_pin").is_none());
        assert_eq!(default.as_object().map(|o| o.len()), Some(5));

        db::set_shop_name(&pool, "Juan's Pisonet")
            .await
            .expect("shop name should persist");
        let updated = client
            .get(format!("http://{address}/api/shop"))
            .send()
            .await
            .expect("shop should complete")
            .json::<serde_json::Value>()
            .await
            .expect("shop JSON");
        assert_eq!(updated["shop_name"], "Juan's Pisonet");
    }

    #[tokio::test]
    async fn public_theme_endpoint_is_unauthenticated() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let response = reqwest::get(format!("http://{address}/api/theme"))
            .await
            .expect("theme GET should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let theme = response
            .json::<serde_json::Value>()
            .await
            .expect("theme JSON");
        assert_eq!(theme["accent_color"], "#15803d");
        assert_eq!(theme["mode_client"], "light");
        assert_eq!(theme["version"], 1);
    }

    #[tokio::test]
    async fn verify_pin_compares_server_side_and_returns_hash_only_on_success() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();

        // Unconfigured PIN: a verify is HTTP 200 with ok=false and no hash.
        let unconfigured = client
            .post(format!("http://{address}/api/owner/verify-pin"))
            .json(&serde_json::json!({ "pin": "123456" }))
            .send()
            .await
            .expect("verify should complete");
        assert_eq!(unconfigured.status(), StatusCode::OK);
        let unconfigured_body = unconfigured
            .json::<serde_json::Value>()
            .await
            .expect("verify JSON");
        assert_eq!(unconfigured_body["ok"], false);
        assert!(unconfigured_body.get("hash").is_none());

        db::set_owner_pin(&pool, "246810")
            .await
            .expect("owner PIN should persist");

        let wrong = client
            .post(format!("http://{address}/api/owner/verify-pin"))
            .json(&serde_json::json!({ "pin": "000000" }))
            .send()
            .await
            .expect("verify should complete")
            .json::<serde_json::Value>()
            .await
            .expect("verify JSON");
        assert_eq!(wrong["ok"], false);
        // A wrong PIN never receives the hash â€” it is only handed to a caller
        // that has just proven the PIN (ADR-0027 Â§3).
        assert!(wrong.get("hash").is_none());

        let correct = client
            .post(format!("http://{address}/api/owner/verify-pin"))
            .json(&serde_json::json!({ "pin": "246810" }))
            .send()
            .await
            .expect("verify should complete")
            .json::<serde_json::Value>()
            .await
            .expect("verify JSON");
        assert_eq!(correct["ok"], true);
        // On success the strong hash is returned so the Client can cache it
        // for the offline fallback. It is a real Argon2 PHC string, never the
        // plaintext PIN.
        let hash = correct["hash"].as_str().expect("hash on success");
        assert!(hash.starts_with("$argon2"));
        assert_ne!(hash, "246810");
    }

    #[tokio::test]
    async fn time_transfer_endpoint_moves_seconds_and_returns_refreshed_state() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (sender_token, sender_id) = signup(&client, &address, "ht-sender", "pc-01").await;
        let (_recipient_token, recipient_id) =
            signup(&client, &address, "ht-recipient", "pc-02").await;
        sqlx::query("UPDATE accounts SET remaining_seconds = 600 WHERE id = ?")
            .bind(&sender_id)
            .execute(&pool)
            .await
            .expect("sender funding");

        let response = client
            .post(format!("http://{address}/api/accounts/transfer"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {sender_token}"),
            )
            .json(&serde_json::json!({
                "to_username": "ht-recipient",
                "seconds": 200
            }))
            .send()
            .await
            .expect("transfer should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("transfer JSON");
        assert_eq!(body["account"]["remaining_seconds"], 400);
        assert_eq!(body["session"]["remaining_seconds"], 400);

        let recipient_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&recipient_id)
                .fetch_one(&pool)
                .await
                .expect("recipient balance");
        assert_eq!(recipient_seconds, 200);

        // An unknown recipient is still a 404 â€” the server never trusts a body id.
        let bad_pc = client
            .post(format!("http://{address}/api/accounts/transfer"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {sender_token}"),
            )
            .json(&serde_json::json!({
                "to_username": "unknown-user",
                "seconds": 1
            }))
            .send()
            .await
            .expect("transfer should complete");
        assert_eq!(bad_pc.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn points_transfer_request_endpoint_returns_a_code_without_moving_points() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (sender_token, sender_id) = signup(&client, &address, "hp-sender", "pc-01").await;
        signup(&client, &address, "hp-recipient", "pc-02").await;
        sqlx::query("UPDATE accounts SET points_balance_centi = 8 WHERE id = ?")
            .bind(&sender_id)
            .execute(&pool)
            .await
            .expect("sender funding");

        let body = client
            .post(format!("http://{address}/api/accounts/transfer-points"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {sender_token}"),
            )
            .json(&serde_json::json!({
                "to_username": "hp-recipient",
                "points_centi": 5
            }))
            .send()
            .await
            .expect("request should complete")
            .json::<serde_json::Value>()
            .await
            .expect("request JSON");
        let code = body["code"].as_str().expect("code");
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));

        // Points are unchanged until the owner confirms in web-admin.
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender_id)
                .fetch_one(&pool)
                .await
                .expect("sender points");
        assert_eq!(sender_points, 8);
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM points_transfer_requests WHERE status = 'pending'",
        )
        .fetch_one(&pool)
        .await
        .expect("pending count");
        assert_eq!(pending, 1);
    }

    #[tokio::test]
    async fn change_password_endpoint_keeps_token_and_rejects_wrong_old_password() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();
        let (token, _id) = signup(&client, &address, "hc-user", "pc-01").await;

        // Wrong old password is a generic 401.
        let wrong = client
            .post(format!("http://{address}/api/accounts/change-password"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "old_password": "nope",
                "new_password": "brand-new-password"
            }))
            .send()
            .await
            .expect("change should complete");
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        // Correct old password rotates the hash and returns ok.
        let ok = client
            .post(format!("http://{address}/api/accounts/change-password"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "old_password": "password",
                "new_password": "brand-new-password"
            }))
            .send()
            .await
            .expect("change should complete");
        assert_eq!(ok.status(), StatusCode::OK);

        // The session token survived the change: the same token still passes
        // the auth gate (a second change with the now-current password works).
        let still_valid = client
            .post(format!("http://{address}/api/accounts/change-password"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "old_password": "brand-new-password",
                "new_password": "another-password"
            }))
            .send()
            .await
            .expect("change should complete");
        assert_eq!(still_valid.status(), StatusCode::OK);

        // The new credentials authenticate; the old ones do not.
        let new_login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "hc-user",
                "password": "another-password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(new_login.status(), StatusCode::OK);
        let old_login = client
            .post(format!("http://{address}/api/accounts/login"))
            .json(&serde_json::json!({
                "username": "hc-user",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("login should complete");
        assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn self_service_password_change_does_not_clear_login_throttle() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();
        let (token, _id) = signup(&client, &address, "change-while-locked", "pc-01").await;

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD {
            let failed =
                account_login_request(&client, &address, "change-while-locked", "wrong-password")
                    .await;
            assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
        }

        // Password Change is session-gated, not Account Login, so it remains
        // available while the Username is locked.
        let changed = client
            .post(format!("http://{address}/api/accounts/change-password"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "old_password": "password",
                "new_password": "changed-password"
            }))
            .send()
            .await
            .expect("Password Change should complete while locked");
        assert_eq!(changed.status(), StatusCode::OK);

        // Password Change does not clear the login bucket: even the new
        // correct Password remains refused until the lockout expires.
        let locked =
            account_login_request(&client, &address, "change-while-locked", "changed-password")
                .await;
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn stale_password_change_maps_to_exact_conflict_response() {
        let response = password_change_response(
            "stale-account",
            Err(AppError::Conflict(
                accounts::PASSWORD_CHANGE_CONFLICT_MSG.to_owned(),
            )),
        );
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("conflict body");
        assert_eq!(
            body.as_ref(),
            accounts::PASSWORD_CHANGE_CONFLICT_MSG.as_bytes()
        );
    }

    /// End-to-end HTTP seam for the stale-replacement 409 (ADR-0037): two
    /// concurrent Password Change requests race through the real endpoint on a
    /// production-like WAL pool. Argon2 keeps both requests verifying the same
    /// stored hash, so the loser hits the compare-and-set boundary and must map
    /// to 409 with the exact generic body â€” never SQLite text. If the loser
    /// happens to read *after* the winner's commit it legitimately gets a 401
    /// (wrong current password), so retry with a fresh server until the stale
    /// path is observed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_password_changes_map_stale_request_to_http_409() {
        let client = reqwest::Client::new();
        for attempt in 0..5 {
            let (pool, path) = test_utils::wal_test_pool("http-password-race").await;
            let address = spawn_client_server(pool.clone()).await;
            let (token, _id) = signup(&client, &address, "http-racer", "pc-01").await;

            let change = |new_password: &'static str| {
                let client = &client;
                let address = &address;
                let token = &token;
                async move {
                    let response = client
                        .post(format!("http://{address}/api/accounts/change-password"))
                        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
                        .json(&serde_json::json!({
                            "old_password": "password",
                            "new_password": new_password
                        }))
                        .send()
                        .await
                        .expect("change should complete");
                    let status = response.status();
                    let body = response.text().await.expect("change body");
                    (status, body)
                }
            };
            let (first, second) = tokio::join!(change("first-password"), change("second-password"));

            let winners =
                usize::from(first.0 == StatusCode::OK) + usize::from(second.0 == StatusCode::OK);
            assert_eq!(
                winners, 1,
                "exactly one request must win: first={first:?}, second={second:?}"
            );
            let loser = if first.0 == StatusCode::OK {
                &second
            } else {
                &first
            };
            pool.close().await;
            let _ = std::fs::remove_file(path);

            if loser.0 == StatusCode::CONFLICT {
                assert_eq!(loser.1, accounts::PASSWORD_CHANGE_CONFLICT_MSG);
                return;
            }
            assert_eq!(
                loser.0,
                StatusCode::UNAUTHORIZED,
                "loser must be 409 or 401, attempt {attempt}: {loser:?}"
            );
        }
        panic!("stale-replacement 409 was never observed over the HTTP seam");
    }

    #[tokio::test]
    async fn removed_account_password_change_has_generic_unauthorized_body_and_audit() {
        let pool = seeded_pool().await;
        let tokens = AccountTokens::default();
        let account_id = "missing-http-account";
        let token = tokens.issue(account_id, "pc-01", "removed-session").await;
        let address = spawn_client_server_with_router(router(
            pool.clone(),
            test_events(),
            tokens,
            SessionStore::default(),
            crate::forwarded_log::test_forwarded_log(),
        ))
        .await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("http://{address}/api/accounts/change-password"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "old_password": "old-password",
                "new_password": "new-password"
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.text().await.expect("unauthorized body"),
            "invalid username or password"
        );
        let denial_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_password_change_failed'
               AND account_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("denial audit count");
        assert_eq!(denial_count, 1);
    }

    #[tokio::test]
    async fn unexpected_password_change_error_has_generic_http_body() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, _) = signup(&client, &address, "http-failure", "pc-01").await;
        pool.close().await;

        let response = client
            .post(format!("http://{address}/api/accounts/change-password"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&serde_json::json!({
                "old_password": "password",
                "new_password": "new-password"
            }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.text().await.expect("error body"),
            "password change failed"
        );
    }

    // -- M3: MAC-keyed identity endpoints (ADR-0031) -----------------------

    #[tokio::test]
    async fn identity_heartbeat_binds_preregistered_pc_on_first_ip_match() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        // Owner pre-registers pc-01 by static IP, no MAC bound yet.
        set_pc_identity(&pool, "pc-01", Some(Some("127.0.0.1")), Some(None))
            .await
            .expect("pc-01 should be pre-registered");

        let (address, server) = spawn_test_server(pool.clone()).await;

        // First heartbeat from an unknown MAC against the pre-registered
        // IP binds the row's `mac_address` and returns pc-01.
        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0"
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("heartbeat response should be JSON");
        assert_eq!(body["id"], "pc-01");

        // The DB now stores the bound MAC for pc-01.
        let bound_mac: Option<String> =
            sqlx::query_scalar("SELECT mac_address FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("mac should be readable");
        assert_eq!(bound_mac.as_deref(), Some("aa:bb:cc:dd:ee:01"));

        server.abort();
    }

    #[tokio::test]
    async fn identity_heartbeat_identifies_pc_by_mac_after_binding() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        // pc-01 is already bound to a MAC; no `registered_ip` set.
        set_pc_identity(&pool, "pc-01", Some(None), Some(Some("aa:bb:cc:dd:ee:01")))
            .await
            .expect("pc-01 should be bound to a MAC");

        let (address, server) = spawn_test_server(pool).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0"
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("heartbeat response should be JSON");
        assert_eq!(body["id"], "pc-01");
        assert_eq!(body["status"], "online");

        server.abort();
    }

    #[tokio::test]
    async fn identity_heartbeat_tamper_failure_rolls_back_heartbeat_and_kick() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        set_pc_identity(&pool, "pc-01", Some(Some("127.0.0.1")), Some(None))
            .await
            .expect("pc-01 should be pre-registered");

        let previous_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(1);
        sqlx::query(
            "UPDATE pcs
             SET status = 'offline', remaining_seconds = 300, last_seen_at = ?
             WHERE id = 'pc-01'",
        )
        .bind(previous_heartbeat.to_rfc3339())
        .execute(&pool)
        .await
        .expect("PC state should be prepared");
        sqlx::query(
            "CREATE TRIGGER fail_tamper_kill
             BEFORE INSERT ON audit_log
             WHEN NEW.event_type = 'tamper_kill'
             BEGIN SELECT RAISE(ABORT, 'test tamper failure'); END",
        )
        .execute(&pool)
        .await
        .expect("tamper failure trigger should be created");

        let (address, server) = spawn_test_server(pool.clone()).await;
        let kill_timestamp = chrono::Utc::now().to_rfc3339();
        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:01")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0",
                "tamper_kill_timestamp": kill_timestamp,
                "tamper_kill_attempt_count": 1
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let (status, last_seen_at, remaining_seconds, mac_address): (
            String,
            Option<String>,
            i64,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, last_seen_at, remaining_seconds, mac_address
             FROM pcs WHERE id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await
        .expect("PC state should be readable");
        assert_eq!(status, "offline");
        assert_eq!(last_seen_at, Some(previous_heartbeat.to_rfc3339()));
        assert_eq!(remaining_seconds, 300);
        assert_eq!(mac_address, None, "identity binding should roll back");
        for event_type in ["pc_came_online", "tamper_kill", "tamper_kick"] {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = ?")
                    .bind(event_type)
                    .fetch_one(&pool)
                    .await
                    .expect("audit count should be readable");
            assert_eq!(count, 0, "{event_type} should roll back");
        }

        server.abort();
    }

    #[tokio::test]
    async fn identity_heartbeat_rejects_unknown_machine_with_typed_body() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");

        let (address, server) = spawn_test_server(pool).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:99")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0"
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("rejection body should be JSON");
        assert_eq!(body["error"], "pc_unregistered");

        server.abort();
    }

    #[tokio::test]
    async fn identity_heartbeat_rejects_new_mac_from_known_ip_when_already_bound() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        db::seed(&pool).await.expect("test data should seed");
        // pc-01 is bound to a MAC AND has a registered IP â€” a new MAC
        // from the same IP is hardware-replacement territory and must
        // be rejected until the owner clears the binding.
        set_pc_identity(
            &pool,
            "pc-01",
            Some(Some("127.0.0.1")),
            Some(Some("aa:bb:cc:dd:ee:01")),
        )
        .await
        .expect("pc-01 should be bound with a registered IP");

        let (address, server) = spawn_test_server(pool.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("http://{address}/api/identity/heartbeat"))
            .header("X-OpenPiso-Mac", "aa:bb:cc:dd:ee:99")
            .json(&serde_json::json!({
                "hostname": "TEST-PC",
                "client_version": "0.1.0"
            }))
            .send()
            .await
            .expect("heartbeat request should complete");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("rejection body should be JSON");
        assert_eq!(body["error"], "pc_unregistered");

        // The pre-existing binding is untouched.
        let stored_mac: Option<String> =
            sqlx::query_scalar("SELECT mac_address FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await
                .expect("mac should be readable");
        assert_eq!(stored_mac.as_deref(), Some("aa:bb:cc:dd:ee:01"));

        server.abort();
    }

    // -- AccountSession extractor tests (ADR-0032) -------------------------

    #[tokio::test]
    async fn account_session_rejects_missing_authorization_header() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("http://{address}/api/wheel/spin"))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn account_session_rejects_malformed_bearer_header() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("http://{address}/api/wheel/spin"))
            .header(reqwest::header::AUTHORIZATION, "NotBearer token")
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn account_session_rejects_unknown_or_revoked_token() {
        let pool = seeded_pool().await;
        let address = spawn_client_server(pool).await;
        let client = reqwest::Client::new();

        let unknown = client
            .post(format!("http://{address}/api/wheel/spin"))
            .header(reqwest::header::AUTHORIZATION, "Bearer nonexistent-token")
            .send()
            .await
            .expect("request should complete");
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);

        // Sign up, get a real token, then use it â€” this works.
        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "extractor-test",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let token = signup
            .json::<serde_json::Value>()
            .await
            .expect("signup JSON")["token"]
            .as_str()
            .expect("token")
            .to_owned();

        // Log out to revoke the token.
        let logout = client
            .post(format!("http://{address}/api/accounts/logout"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("logout should complete");
        assert_eq!(logout.status(), StatusCode::OK);

        // Now the revoked token is rejected.
        let revoked = client
            .post(format!("http://{address}/api/wheel/spin"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn account_session_valid_token_runs_handler() {
        let pool = seeded_pool().await;
        // Enable the wheel so the spin succeeds (disabled by default in migration 0005).
        sqlx::query("UPDATE wheel_settings SET enabled = 1, free_spin_cap = 3 WHERE id = 1")
            .execute(&pool)
            .await
            .expect("wheel settings enable");
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();

        let signup = client
            .post(format!("http://{address}/api/accounts/signup"))
            .json(&serde_json::json!({
                "username": "wheel-tester",
                "password": "password",
                "pc_id": "pc-01"
            }))
            .send()
            .await
            .expect("signup should complete");
        assert_eq!(signup.status(), StatusCode::OK);
        let auth = signup
            .json::<serde_json::Value>()
            .await
            .expect("signup JSON");
        let token = auth["token"].as_str().expect("token").to_owned();
        let account_id = auth["account"]["id"]
            .as_str()
            .expect("account id")
            .to_owned();

        let settings = client
            .get(format!("http://{address}/api/wheel/settings"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("settings should complete");
        assert_eq!(settings.status(), StatusCode::OK);
        let settings = settings
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON");
        assert!(settings.get("bokya_rate_basis_points").is_none());
        assert!(settings.get("shop_rate_point_value_seconds").is_none());
        assert!(settings.get("shop_rate_house_edge_basis_points").is_none());
        assert!(settings.get("unsafe_shop_rate_house_edge").is_none());
        assert!(settings["segments"]
            .as_array()
            .expect("segments")
            .iter()
            .all(|segment| segment.get("probability_basis_points").is_none()));

        sqlx::query("UPDATE wheel_settings SET bokya_rate_basis_points = 9000 WHERE id = 1")
            .execute(&pool)
            .await
            .expect("Bokya Rate update");

        // Fund the account so wheel spin works (spin costs 100 centipoints).
        sqlx::query("UPDATE accounts SET points_balance_centi = 1000 WHERE id = ?")
            .bind(&account_id)
            .execute(&pool)
            .await
            .expect("funding");

        let spin = client
            .post(format!("http://{address}/api/wheel/spin"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("spin should complete");
        assert_eq!(spin.status(), StatusCode::OK);
        let body = spin.json::<serde_json::Value>().await.expect("spin JSON");
        assert!(body["result"]["seconds_won"].as_i64().is_some());
        assert_eq!(body["token"], token);
        assert!(body.get("bokya_rate_basis_points").is_none());
        assert!(body["result"]["segments"]
            .as_array()
            .expect("spin segments")
            .iter()
            .all(|segment| segment.get("probability_basis_points").is_none()));
    }

    // -- Spin Password Confirmation ---------------------------------------

    /// Enable the wheel, optionally require password confirmation, and sign up
    /// a funded Account (1000 centi-points). Returns `(pool, address, token,
    /// account_id)`.
    async fn spin_password_fixture(required: bool) -> (SqlitePool, String, String, String) {
        let pool = seeded_pool().await;
        sqlx::query(
            "UPDATE wheel_settings SET enabled = 1, password_confirmation_required = ? WHERE id = 1",
        )
        .bind(i64::from(required))
        .execute(&pool)
        .await
        .expect("wheel settings should update");
        let address = spawn_client_server(pool.clone()).await;
        let client = reqwest::Client::new();
        let (token, account_id) = signup(&client, &address, "spin-pw-user", "pc-01").await;
        sqlx::query("UPDATE accounts SET points_balance_centi = 1000 WHERE id = ?")
            .bind(&account_id)
            .execute(&pool)
            .await
            .expect("account should be funded");
        (pool, address, token, account_id)
    }

    async fn spin_request(
        address: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> reqwest::Response {
        let request = reqwest::Client::new()
            .post(format!("http://{address}/api/wheel/spin"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        request.send().await.expect("spin request should complete")
    }

    async fn spin_balance_and_count(pool: &SqlitePool, account_id: &str) -> (i64, i64) {
        let balance: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(account_id)
                .fetch_one(pool)
                .await
                .expect("balance should be readable");
        let spins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wheel_spins")
            .fetch_one(pool)
            .await
            .expect("spin count should be readable");
        (balance, spins)
    }

    #[tokio::test]
    async fn spin_without_body_succeeds_when_password_confirmation_is_off() {
        let (pool, address, token, account_id) = spin_password_fixture(false).await;

        let response = spin_request(&address, &token, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let (_, spins) = spin_balance_and_count(&pool, &account_id).await;
        assert_eq!(spins, 1);

        // Current clients send `{}` when no password was entered.
        let response = spin_request(&address, &token, Some(serde_json::json!({}))).await;
        assert_eq!(response.status(), StatusCode::OK);

        // A supplied password is ignored while the setting is off.
        let response = spin_request(
            &address,
            &token,
            Some(serde_json::json!({ "password": "wrong" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn spin_requires_password_when_confirmation_is_on() {
        let (pool, address, token, account_id) = spin_password_fixture(true).await;

        for body in [
            None,
            Some(serde_json::json!({})),
            Some(serde_json::json!({ "password": "" })),
        ] {
            let response = spin_request(&address, &token, body).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = response
                .json::<serde_json::Value>()
                .await
                .expect("error JSON");
            assert_eq!(body["error"], "password_required");
        }
        assert_eq!(spin_balance_and_count(&pool, &account_id).await, (1000, 0));
    }

    #[tokio::test]
    async fn spin_rejects_wrong_password_without_charging() {
        let (pool, address, token, account_id) = spin_password_fixture(true).await;

        let response = spin_request(
            &address,
            &token,
            Some(serde_json::json!({ "password": "not-the-password" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("error JSON");
        assert_eq!(body["error"], "invalid_password");
        assert_eq!(spin_balance_and_count(&pool, &account_id).await, (1000, 0));
    }

    #[tokio::test]
    async fn spin_with_correct_password_charges_one_point() {
        let (pool, address, token, account_id) = spin_password_fixture(true).await;

        let response = spin_request(
            &address,
            &token,
            Some(serde_json::json!({ "password": "password" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let (_, spins) = spin_balance_and_count(&pool, &account_id).await;
        assert_eq!(spins, 1);
        let spent: i64 = sqlx::query_scalar("SELECT points_spent_centi FROM wheel_spins")
            .fetch_one(&pool)
            .await
            .expect("spin row should exist");
        assert_eq!(spent, 100);
    }

    #[tokio::test]
    async fn repeated_wrong_spin_passwords_lock_spin_and_login() {
        let (pool, address, token, account_id) = spin_password_fixture(true).await;

        for _ in 0..account_login_throttle::FAILURE_THRESHOLD {
            let response = spin_request(
                &address,
                &token,
                Some(serde_json::json!({ "password": "wrong-password" })),
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        // Locked: even the correct password is refused before verification.
        let spin = spin_request(
            &address,
            &token,
            Some(serde_json::json!({ "password": "password" })),
        )
        .await;
        assert_eq!(spin.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(spin_balance_and_count(&pool, &account_id).await, (1000, 0));

        // Same bucket as login.
        let client = reqwest::Client::new();
        let login = account_login_request(&client, &address, "spin-pw-user", "password").await;
        assert_eq!(login.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    async fn read_wheel_settings(address: &str, token: &str) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("http://{address}/api/wheel/settings"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("settings request should complete")
            .json::<serde_json::Value>()
            .await
            .expect("settings JSON")
    }

    #[tokio::test]
    async fn wheel_settings_expose_password_confirmation_required() {
        let (pool, address, token, _) = spin_password_fixture(false).await;

        assert_eq!(
            read_wheel_settings(&address, &token).await["password_confirmation_required"],
            false
        );
        sqlx::query("UPDATE wheel_settings SET password_confirmation_required = 1 WHERE id = 1")
            .execute(&pool)
            .await
            .expect("setting should update");
        assert_eq!(
            read_wheel_settings(&address, &token).await["password_confirmation_required"],
            true
        );
    }
}

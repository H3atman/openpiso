use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, State};
#[cfg(target_os = "windows")]
use tauri_plugin_autostart::ManagerExt;
use tracing::warn;

use crate::{
    client_connection, config_store,
    state::{AppState, ClientConfig, SharedState, ThemeRecord, TimerState},
    window_mode::{self, ViewMode},
};
use serde::{Deserialize, Serialize};

use crate::state::{
    AccountAuthState, CoinPromoMenu, RedemptionTierMenu, WheelSettings, WheelSpinResponse,
    WheelSpinResult,
};

/// Return the resolved `pc_id` for outgoing account/disconnect calls.
///
/// Identity is server-owned (ADR-0031); an account call cannot be made
/// until the server has adopted a `pc_id` for this machine. Returning
/// an error here keeps the existing command contract (the front-end
/// catches the error and shows the Lock copy) while preventing a
/// stray `""` from racing through the account endpoints.
async fn current_pc_id(shared: &SharedState) -> Result<String, String> {
    let pc_id = shared.config.read().await.pc_id.clone();
    pc_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "this PC has not been registered with the admin server yet".to_owned())
}

/// Customer-safe copy for a failed round-trip to the admin server.
const ADMIN_UNREACHABLE: &str = "could not reach admin server";

/// Sanitize a transport failure before it reaches the UI.
///
/// `reqwest::Error`'s `Display` embeds the full request URL, so returning it
/// verbatim would print the admin server address on the Lock, which every
/// customer sees. The detail goes to the local rolling log (ADR-0016) instead
/// — the only sink that survives this failure mode, unlike a forwarded
/// diagnostic, which would POST to the very host that just proved unreachable.
/// `{error:?}`
/// rather than `{error}` so the source chain (dns/tls/io) survives too, since
/// the returned string no longer carries any of it.
fn transport_error(operation: &str, error: &reqwest::Error) -> String {
    tracing::error!(
        target: "openpiso_client::commands",
        operation,
        "{ADMIN_UNREACHABLE}: {error:?}"
    );
    ADMIN_UNREACHABLE.to_owned()
}

async fn set_account_auth(shared: &SharedState, auth: AccountAuthState) {
    *shared.account_auth.write().await = Some(auth);
}

async fn clear_account_auth(shared: &SharedState) {
    *shared.account_auth.write().await = None;
}

#[derive(Serialize)]
struct AccountCredentials<'a> {
    username: &'a str,
    password: &'a str,
    pc_id: &'a str,
}

#[derive(Serialize)]
struct VoucherRedeemRequest<'a> {
    code: &'a str,
}

#[derive(Deserialize)]
struct PublicShopInfo {
    wheel_enabled: bool,
}

#[derive(Serialize)]
struct PointsRedeemRequest {
    tier_id: i64,
    version: i64,
}

#[derive(Serialize)]
struct TransferTimeRequest<'a> {
    to_username: &'a str,
    seconds: i64,
}

#[derive(Serialize)]
struct TransferPointsRequest<'a> {
    to_username: &'a str,
    points_centi: i64,
}

#[derive(Serialize)]
struct ChangePasswordRequest<'a> {
    old_password: &'a str,
    new_password: &'a str,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PointsTransferCodeResponse {
    id: String,
    code: String,
}

#[derive(Serialize)]
struct OwnerPinVerifyRequest<'a> {
    pin: &'a str,
}

#[derive(Deserialize)]
struct OwnerPinVerificationResponse {
    ok: bool,
    /// Strong (Argon2) `owner_pin` hash, returned by the admin only on a
    /// successful verify so the Client can cache it for the offline fallback
    /// (ADR-0027 §3). Absent on any failure.
    #[serde(default)]
    hash: Option<String>,
}

#[derive(Serialize)]
pub struct SetClientConfigResponse {
    config: ClientConfig,
    persist_warning: Option<String>,
}

#[derive(Serialize)]
pub struct SetLaunchAtStartupResponse {
    config: ClientConfig,
    persist_warning: Option<String>,
}

#[derive(Serialize)]
pub struct SetWindowsSignOutResponse {
    config: ClientConfig,
    persist_warning: Option<String>,
}

/// Decide whether `set_client_config` should write `client-config.json`.
///
/// ADR-0031 guards against no-op writes (only save when a field changed),
/// but that guard alone never creates the file on a fresh install where the
/// chosen config happens to equal the in-memory default — e.g. a local /
/// single-box setup pointing the client at the `http://127.0.0.1:3210`
/// default. The file's *absence* is what matters on first configure, so we
/// also persist whenever this machine has never been configured yet.
fn should_persist_config(changed: bool, configured: bool) -> bool {
    changed || !configured
}

#[tauri::command]
pub fn prepare_startup_handoff(app: AppHandle) -> Result<(), String> {
    // A hidden WebView2 can pause requestAnimationFrame. Reveal Tauri first,
    // but keep it outside the topmost band while Svelte paints behind the
    // native placeholder.
    let covered = crate::startup_placeholder::is_visible();
    window_mode::reveal_startup_window(&app, covered)?;
    if covered {
        crate::startup_placeholder::reassert_topmost()?;
    }
    Ok(())
}

#[tauri::command]
pub fn frontend_ready(app: AppHandle) -> Result<(), String> {
    crate::diagnostics::mark_frontend_ready();
    if crate::startup_placeholder::is_visible() {
        // The webview has now painted. Move it into the topmost band before
        // retiring the placeholder, so the desktop cannot appear between the
        // two windows.
        window_mode::complete_startup_handoff(&app)?;
    }
    crate::startup_placeholder::handoff()
}

#[tauri::command]
pub async fn get_client_config(state: State<'_, AppState>) -> Result<ClientConfig, String> {
    Ok(state.shared.config.read().await.clone())
}

/// Whether the running config came from a saved `client-config.json` (`true`)
/// or is the never-chosen in-memory default of a fresh/corrupt install
/// (`false`). The Lock uses this to show the un-gated setup form so an
/// unconfigured PC can be pointed at the admin server without hitting the
/// offline owner-PIN gate (ADR-0027 §3).
#[tauri::command]
pub async fn is_client_configured(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(*state.shared.configured.read().await)
}

#[tauri::command]
pub async fn set_client_config(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    view_mode: Option<String>,
) -> Result<SetClientConfigResponse, String> {
    let server_url = server_url.trim().trim_end_matches('/').to_owned();
    let parsed_url =
        reqwest::Url::parse(&server_url).map_err(|error| format!("invalid server URL: {error}"))?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err("server URL must use http or https".to_owned());
    }

    let server_url_changed = state.shared.config.read().await.server_url != server_url;
    if server_url_changed && *state.shared.configured.read().await {
        require_owner_settings_unlocked(&state.shared).await?;
    }

    let (config, changed, server_url_changed) = {
        let mut current = state.shared.config.write().await;
        let view_mode = match view_mode {
            Some(value) => ViewMode::from_config(&value).as_str().to_owned(),
            None => current.view_mode.clone(),
        };
        let next = ClientConfig {
            server_url,
            pc_id: current.pc_id.clone(),
            view_mode,
            launch_at_startup: current.launch_at_startup,
            disable_windows_sign_out: current.disable_windows_sign_out,
            offline_grace_seconds: current.offline_grace_seconds,
        };
        let changed = *current != next;
        if changed {
            *current = next.clone();
        }
        (next, changed, server_url_changed)
    };

    if server_url_changed {
        // Only disconnect and clear state when the server URL actually
        // changes. A view_mode-only toggle (mini ↔ widget) must preserve
        // the connection, timer, and auth — otherwise a double-click on the
        // MiniBar fires two `onclick` events, each calling this command,
        // and the first one disconnects + zeroes the timer, kicking the
        // customer to the Lock screen (bug #7).
        if let Err(error) = disconnect_and_notify(&state.shared).await {
            warn!(target: "openpiso_client::commands", "Could not notify previous admin server before reconfiguration: {error}");
        }
        clear_account_auth(&state.shared).await;
        *state.shared.countdown_anchor.write().await = None;
        *state.shared.shutdown_deadline.write().await = None;
        *state.shared.shutdown_issued_at.write().await = None;
        client_connection::disarm_offline_shutdown(
            &state.shared,
            "the owner pointed this Client at a different admin server",
        )
        .await;
        client_connection::reset_owner_settings_state(&state.shared).await;
        *state.shared.offline_shutdown_issued_at.write().await = None;
        if let Err(error) = config_store::clear_offline_shutdown_deadline(&app) {
            warn!(target: "openpiso_client::config_store", "Could not clear Offline Shutdown deadline after server repoint: {error}");
        }
        // Drop any cached offline owner-PIN hash: it belongs to the previous
        // admin server and must not unlock settings after a repoint (ADR-0027).
        *state.shared.owner_pin_hash.write().await = None;
        let mut timer = state.shared.timer.write().await;
        timer.pc_name.clear();
        timer.remaining_seconds = 0;
        timer.expired = false;
        timer.power_state = "active".to_owned();
        timer.shutdown_in_seconds = None;
        timer.offline_shutdown_in_seconds = None;
        timer.error = None;
        drop(timer);
    }

    let already_configured = *state.shared.configured.read().await;
    let persist_warning = if should_persist_config(changed, already_configured) {
        match config_store::save(&app, &config) {
            Ok(()) => {
                // A config now exists on disk: the Lock can drop the un-gated
                // "set up this PC" form (the owner has chosen a server).
                *state.shared.configured.write().await = true;
                None
            }
            Err(error) => {
                warn!(
                    target: "openpiso_client::commands",
                    "Settings were applied but could not be saved: {error}"
                );
                Some(format!(
                    "Settings applied, but could not be saved; they may not survive a reboot: {error}"
                ))
            }
        }
    } else {
        None
    };

    Ok(SetClientConfigResponse {
        config,
        persist_warning,
    })
}

/// Require a currently unlocked Owner PIN before changing owner settings.
/// The UI gate is only a presentation concern; mutation authority stays in
/// Rust so direct webview invocations cannot bypass it.
async fn require_owner_settings_unlocked(shared: &SharedState) -> Result<(), String> {
    if *shared.owner_settings_unlocked.read().await {
        Ok(())
    } else {
        Err("owner settings are not unlocked".to_owned())
    }
}

/// Apply the Windows sign-out policy before publishing the new local config.
/// The config is updated only after the registry adapter succeeds, so a UAC
/// decline cannot leave the UI claiming that protection is active.
#[tauri::command]
pub async fn set_windows_sign_out(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<SetWindowsSignOutResponse, String> {
    require_owner_settings_unlocked(&state.shared).await?;
    let previous = state.shared.config.read().await.clone();
    if previous.disable_windows_sign_out == enabled {
        return Ok(SetWindowsSignOutResponse {
            config: previous,
            persist_warning: None,
        });
    }

    crate::windows_sign_out::apply(enabled)?;

    let config = {
        let mut current = state.shared.config.write().await;
        current.disable_windows_sign_out = enabled;
        current.clone()
    };
    if let Err(error) = config_store::save(&app, &config) {
        let rollback_error = crate::windows_sign_out::apply(!enabled).err();
        *state.shared.config.write().await = previous;
        return Err(match rollback_error {
            Some(rollback_error) => format!(
                "Windows sign-out policy changed but local settings could not be saved ({error}); registry rollback also failed ({rollback_error})"
            ),
            None => format!(
                "Windows sign-out policy was not changed because local settings could not be saved: {error}"
            ),
        });
    }
    *state.shared.configured.write().await = true;

    Ok(SetWindowsSignOutResponse {
        config,
        persist_warning: None,
    })
}

/// Apply the persisted Launch at startup preference through the Tauri plugin.
/// The frontend never calls the plugin's raw commands; this Rust wrapper keeps
/// the OS registration and the local config change in one typed command.
#[cfg(target_os = "windows")]
pub(crate) fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|error| format!("could not enable Launch at startup: {error}"))
    } else {
        // Disabling an entry that is already absent fails with "not found";
        // that is the desired state, not a failure. A failed check falls
        // through so a real registry problem is still reported.
        if matches!(manager.is_enabled(), Ok(false)) {
            return Ok(());
        }
        manager
            .disable()
            .map_err(|error| format!("could not disable Launch at startup: {error}"))
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn apply_autostart(_app: &AppHandle, _enabled: bool) -> Result<(), String> {
    Err("Launch at startup is available only on Windows".to_owned())
}

#[tauri::command]
pub async fn set_launch_at_startup(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<SetLaunchAtStartupResponse, String> {
    require_owner_settings_unlocked(&state.shared).await?;
    apply_autostart(&app, enabled)?;

    let config = {
        let mut current = state.shared.config.write().await;
        current.launch_at_startup = enabled;
        current.clone()
    };
    let persist_warning = match config_store::save(&app, &config) {
        Ok(()) => None,
        Err(error) => {
            warn!(
                target: "openpiso_client::commands",
                "Launch at startup changed but could not be saved: {error}"
            );
            Some(format!(
                "Launch at startup changed, but could not be saved; it may not survive a reboot: {error}"
            ))
        }
    };

    Ok(SetLaunchAtStartupResponse {
        config,
        persist_warning,
    })
}

/// Spawn the admin connection task unless one is already running. Shared by
/// the `connect_to_server` command and the startup auto-connect (lib.rs setup)
/// so both go through the same idempotency guard and "connecting" status
/// update. Idempotent: a second call while the task is live is a no-op.
pub(crate) async fn start_connection(shared: &Arc<SharedState>, app: AppHandle) {
    let mut task = shared.connection_task.lock().await;
    if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }

    {
        let mut timer = shared.timer.write().await;
        timer.connection_status = "connecting".to_owned();
        timer.error = None;
    }
    *task = Some(tokio::spawn(client_connection::run(
        Arc::clone(shared),
        app,
    )));
}

#[tauri::command]
pub async fn connect_to_server(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    start_connection(&state.shared, app).await;
    Ok(())
}

#[tauri::command]
pub fn is_windows_platform() -> bool {
    cfg!(target_os = "windows")
}

#[tauri::command]
pub async fn disconnect_from_server(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    disconnect_and_notify(&state.shared).await?;
    config_store::clear_offline_shutdown_deadline(&app)
}

#[tauri::command]
pub async fn pause_offline_shutdown(state: State<'_, AppState>) -> Result<(), String> {
    client_connection::pause_offline_shutdown(&state.shared).await
}

#[tauri::command]
pub async fn resume_offline_shutdown(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    client_connection::resume_offline_shutdown(&state.shared, &app).await;
    Ok(())
}

#[tauri::command]
pub async fn get_timer_state(state: State<'_, AppState>) -> Result<TimerState, String> {
    Ok(state.shared.timer.read().await.clone())
}

#[tauri::command]
pub async fn sign_up_account(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<AccountAuthState, String> {
    authenticate_account(&state.shared, "signup", &username, &password)
        .await
        .map_err(account_auth_error_message)
}

#[tauri::command]
pub async fn login_account(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<AccountAuthState, AccountLoginError> {
    authenticate_account(&state.shared, "login", &username, &password).await
}

#[tauri::command]
pub async fn get_account_auth(
    state: State<'_, AppState>,
) -> Result<Option<AccountAuthState>, String> {
    Ok(state.shared.account_auth.read().await.clone())
}

#[tauri::command]
pub async fn claim_guest_time(state: State<'_, AppState>) -> Result<AccountAuthState, String> {
    mutate_account_session(&state.shared, "claim").await
}

#[tauri::command]
pub async fn redeem_voucher(
    state: State<'_, AppState>,
    code: String,
) -> Result<AccountAuthState, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before redeeming a voucher".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/vouchers/redeem",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .json(&VoucherRedeemRequest { code: &code })
        .send()
        .await
        .map_err(|error| transport_error("voucher redemption", &error))?;
    store_account_response(&state.shared, response, "voucher redemption failed").await
}

#[tauri::command]
pub async fn get_redemption_tiers(
    state: State<'_, AppState>,
) -> Result<RedemptionTierMenu, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before viewing Redemption Tiers".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/points/redemption-tiers",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .send()
        .await
        .map_err(|error| transport_error("Redemption Tier menu", &error))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "Redemption Tier menu failed".to_owned()));
    }
    response
        .json::<RedemptionTierMenu>()
        .await
        .map_err(|error| format!("invalid Redemption Tier menu: {error}"))
}

#[tauri::command]
pub async fn get_coin_promo_menu(state: State<'_, AppState>) -> Result<CoinPromoMenu, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before viewing the Coin Promo".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!("{}/api/coin-promo", config.server_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .send()
        .await
        .map_err(|error| transport_error("Coin Promo menu", &error))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "Coin Promo ladder failed".to_owned()));
    }
    response
        .json::<CoinPromoMenu>()
        .await
        .map_err(|error| format!("invalid Coin Promo ladder: {error}"))
}

#[tauri::command]
pub async fn redeem_points(
    state: State<'_, AppState>,
    tier_id: i64,
    version: i64,
) -> Result<AccountAuthState, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before redeeming points".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/points/redeem",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .json(&PointsRedeemRequest { tier_id, version })
        .send()
        .await
        .map_err(|error| transport_error("points redemption", &error))?;
    store_account_response(&state.shared, response, "points redemption failed").await
}

#[tauri::command]
pub async fn spin_wheel(state: State<'_, AppState>) -> Result<WheelSpinResult, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before spinning the wheel".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!("{}/api/wheel/spin", config.server_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .send()
        .await
        .map_err(|error| transport_error("wheel spin", &error))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "wheel spin failed".to_owned()));
    }
    let spin = response
        .json::<WheelSpinResponse>()
        .await
        .map_err(|error| format!("invalid wheel response: {error}"))?;
    set_account_auth(
        &state.shared,
        AccountAuthState {
            token: spin.token,
            account: spin.account,
            session: spin.session,
        },
    )
    .await;
    Ok(spin.result)
}

#[tauri::command]
pub async fn get_wheel_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    let config = state.shared.config.read().await.clone();
    let endpoint = format!("{}/api/shop", config.server_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(endpoint)
        .send()
        .await
        .map_err(|error| transport_error("shop settings", &error))?;
    if !response.status().is_success() {
        return Err("shop settings failed".to_owned());
    }
    response
        .json::<PublicShopInfo>()
        .await
        .map(|shop| shop.wheel_enabled)
        .map_err(|error| format!("invalid shop settings: {error}"))
}

#[tauri::command]
pub async fn get_wheel_settings(state: State<'_, AppState>) -> Result<WheelSettings, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before viewing wheel settings".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/wheel/settings",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .send()
        .await
        .map_err(|error| transport_error("wheel settings", &error))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "wheel settings failed".to_owned()));
    }
    response
        .json::<WheelSettings>()
        .await
        .map_err(|error| format!("invalid wheel settings: {error}"))
}

#[tauri::command]
pub async fn logout_account(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "no account is logged in".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/accounts/logout",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .send()
        .await
        .map_err(|error| transport_error("account logout", &error))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Session already ended server-side (tick expired it, or it was
        // never bound). Clear local auth so the UI can recover; the guest
        // timer will update on the next heartbeat.
        clear_account_auth(&state.shared).await;
        return Ok(());
    }
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "logout failed".to_owned()));
    }
    clear_account_auth(&state.shared).await;
    // Parse the server response body (Pc) and update the local timer
    // so the frontend immediately sees the guest timer state instead of
    // stale session-enriched values (Bug #6).
    match response.json::<client_connection::PcResponse>().await {
        Ok(pc) => client_connection::apply_server_state(&state.shared, &app, pc).await,
        Err(error) => {
            tracing::warn!(
                target: "openpiso_client::commands",
                "logout response parse failed: {error}; timer will update on next heartbeat"
            );
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn lock_overlay(state: State<'_, AppState>) -> Result<(), String> {
    state.shared.timer.write().await.overlay_locked = true;
    Ok(())
}

/// Resume preserved PC-bound Guest Time after a Tamper Kick. The customer
/// must explicitly acknowledge the neutral Fullscreen Lock; a top-up alone
/// must not silently unlock it.
#[tauri::command]
pub async fn continue_guest_session(state: State<'_, AppState>) -> Result<(), String> {
    let guest_kick_pending = *state.shared.guest_tamper_kick_pending.read().await;
    let mut timer = state.shared.timer.write().await;
    if !guest_kick_pending
        || !timer.overlay_locked
        || timer.timer_mode != "guest"
        || timer.guest_remaining_seconds <= 0
    {
        return Err("there is no paused Guest session to continue".to_owned());
    }
    timer.overlay_locked = false;
    drop(timer);
    *state.shared.guest_tamper_kick_pending.write().await = false;
    Ok(())
}

// ---------------------------------------------------------------------------
// Window modes (ADR-0029, plan §4.1 + §5/§7.2).
//
// The three commands reconfigure the single `main` window into the Fullscreen
// Lock, the floating Widget, or the Mini Bar. The Lock is *not* a stored
// `view_mode` — the §4.1 state machine in the frontend decides when to call
// `enter_fullscreen_mode`; we only persist the customer's persisted choice
// between Widget and Mini. Lock re-assertion on focus loss is the §7 / phase 7
// responsibility (kiosk OS-hardening), not this set of commands.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn enter_fullscreen_mode(app: AppHandle) -> Result<(), String> {
    let window = window_mode::main_window(&app)?;
    window_mode::apply_fullscreen_lock(&window)?;
    // Lock does not write a `view_mode`: the next call to enter the Widget
    // or Mini Bar reads the previously-stored value, and a customer who only
    // ever sees the Lock leaves the existing choice untouched.
    Ok(())
}

#[tauri::command]
pub async fn enter_widget_mode(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let window = window_mode::main_window(&app)?;
    window_mode::apply_widget(&window)?;
    if let Some(config) = window_mode::persist_view_mode(&state.shared, ViewMode::Widget).await {
        if let Err(error) = config_store::save(&app, &config) {
            warn!(target: "openpiso_client::commands", "Could not save Widget view mode: {error}");
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn enter_mini_mode(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let window = window_mode::main_window(&app)?;
    window_mode::apply_mini(&window)?;
    if let Some(config) = window_mode::persist_view_mode(&state.shared, ViewMode::Mini).await {
        if let Err(error) = config_store::save(&app, &config) {
            warn!(target: "openpiso_client::commands", "Could not save Mini Bar view mode: {error}");
        }
    }
    Ok(())
}

/// Tell the app-level keyboard-hook hardening whether the Fullscreen Lock is
/// the active view (ADR-0028, plan §7). The frontend's §4.1 view machine
/// calls this on every view change: while the Lock shows, escape chords (Win /
/// Alt-Tab / Alt-Esc / Ctrl-Esc / Alt-F4) are swallowed; during a paid
/// Widget / Mini session they pass through so the customer uses the PC
/// normally. No-op unless a release Windows build installed the hook.
#[tauri::command]
pub async fn set_kiosk_lock_active(active: bool) -> Result<(), String> {
    crate::kiosk_hardening::set_lock_active(active);
    Ok(())
}

/// Dev-only escape from the Fullscreen Lock's window restrictions. The
/// command is registered in all builds so the frontend API shape stays stable,
/// but release builds always reject it and do not include the window bypass
/// implementation.
#[tauri::command]
pub async fn dev_bypass_screen_restriction(app: AppHandle) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        crate::kiosk_hardening::set_lock_active(false);
        let window = window_mode::main_window(&app)?;
        window_mode::apply_dev_lock_bypass(&window)?;
        Ok(())
    }

    #[cfg(not(debug_assertions))]
    {
        let _ = app;
        Err("dev screen bypass is only available in debug builds".to_owned())
    }
}

/// Dev-only command to seed a pending Tamper Kill report for testing/demo.
/// Writes the report to the config store and loads it into shared state so
/// the next heartbeat includes it. Only available in debug builds.
#[tauri::command]
pub async fn dev_seed_tamper_kill_report(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let kill_timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let report = crate::state::PendingTamperKillReport {
            kill_timestamp_unix_ms,
            attempt_count: 1,
        };
        {
            let _transition = state.shared.connection_transition.lock().await;
            let mut pending = state.shared.pending_tamper_kill.write().await;
            let report = pending.map_or(report, |existing| existing.coalesce(report));
            config_store::save_tamper_kill_report(&app, &report)?;
            *pending = Some(report);
            state
                .shared
                .tamper_kill_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        client_connection::hold_tamper_kill_connection(&state.shared, &app).await;
        tracing::info!(
            target: "openpiso_client::commands",
            kill_timestamp_unix_ms,
            "Seeded Tamper Kill report for demo"
        );
        Ok(())
    }

    #[cfg(not(debug_assertions))]
    {
        let _ = (app, state);
        Err("dev_seed_tamper_kill_report is only available in debug builds".to_owned())
    }
}

/// Read the cached shop name. The frontend mounts the Lock and re-reads on
/// reconnect (plan §4.4); the actual `GET /api/shop` refresh lives in the
/// heartbeat path (phase 2), so this command is a thin cache read today and
/// the contract stays stable for the future refresh-on-reconnect wiring.
#[tauri::command]
pub async fn get_shop_name(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.shared.shop_name.read().await.clone())
}

/// Return the latest Rust-fetched theme after the frontend listener is ready.
/// `None` means the fetch is still pending or failed; the registered listener
/// will receive any later successful heartbeat-driven update.
#[tauri::command]
pub async fn get_theme(state: State<'_, AppState>) -> Result<Option<ThemeRecord>, String> {
    Ok(state.shared.theme.read().await.clone())
}

/// Verify a typed owner PIN against a cached Argon2 hash for the offline
/// fallback (ADR-0027 §3). Mirrors the admin's `auth::verify_password`
/// (`Argon2::default()`), so a hash the server produced verifies identically
/// here. A malformed cached hash verifies to `false` rather than panicking.
fn verify_pin_against_cached_hash(pin: &str, encoded_hash: &str) -> bool {
    use argon2::{
        password_hash::{PasswordHash, PasswordVerifier},
        Argon2,
    };
    PasswordHash::new(encoded_hash).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(pin.as_bytes(), &hash)
            .is_ok()
    })
}

/// Owner gate (ADR-0027 §2–§3). The Client posts the typed PIN to the admin
/// `POST /api/owner/verify-pin` endpoint, which compares it against the
/// hashed `owner_pin` setting server-side — the live PIN is never compared on
/// this untrusted machine while the server is reachable. Returns whether the
/// PIN is correct so the frontend can reveal the PIN-gated settings panel; a
/// wrong PIN is a normal `Ok(false)`.
///
/// **Offline fallback:** on a *successful* online verify the server also
/// returns the strong `owner_pin` hash, which we cache in memory. When the
/// admin server is later unreachable, we verify the typed PIN against that
/// cached hash with Argon2 so the owner can still get in (e.g. to repoint the
/// client during server troubleshooting). If nothing is cached yet — the
/// server has never been reached this session — offline verification fails
/// closed with an explanatory error; the separate Windows admin account
/// (ADR-0028) is the recovery path in that case.
#[tauri::command]
pub async fn verify_owner_pin(state: State<'_, AppState>, pin: String) -> Result<bool, String> {
    let pin = pin.trim().to_owned();
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/owner/verify-pin",
        config.server_url.trim_end_matches('/')
    );
    let send_result = reqwest::Client::new()
        .post(endpoint)
        .json(&OwnerPinVerifyRequest { pin: &pin })
        .send()
        .await;

    let response = match send_result {
        Ok(response) => response,
        Err(transport_error) => {
            // Server unreachable: fall back to the cached strong hash.
            let cached = state.shared.owner_pin_hash.read().await.clone();
            return match cached {
                Some(hash) => {
                    let verified = verify_pin_against_cached_hash(&pin, &hash);
                    if verified {
                        *state.shared.owner_settings_unlocked.write().await = true;
                    }
                    Ok(verified)
                }
                None => Err(format!(
                    "admin server is unreachable and no owner PIN is cached for offline \
                     unlock yet (verify once while online first): {transport_error}"
                )),
            };
        }
    };

    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "owner PIN verification failed".to_owned()));
    }
    let verification = response
        .json::<OwnerPinVerificationResponse>()
        .await
        .map_err(|error| format!("invalid owner PIN response: {error}"))?;
    // Cache the strong hash the server hands back on success so a later
    // server outage does not lock the owner out (ADR-0027 §3).
    if verification.ok {
        *state.shared.owner_settings_unlocked.write().await = true;
        if let Some(hash) = verification.hash {
            *state.shared.owner_pin_hash.write().await = Some(hash);
        }
    }
    Ok(verification.ok)
}

/// PIN-gated owner exit (ADR-0027 §2 / plan §7.2). Reachable only from the
/// unlocked, PIN-gated settings panel — there is no customer-facing Quit. We
/// notify the admin server of a clean disconnect first (so the PC is marked
/// offline without losing time), then ask Tauri to exit. The `ExitRequested`
/// handler in `lib.rs` also runs the notify, but doing it here makes the clean
/// shutdown deterministic regardless of exit-event ordering.
#[tauri::command]
pub async fn quit_client(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    require_owner_settings_unlocked(&state.shared).await?;
    crate::shutdown::initiate_exit(
        crate::shutdown::ClientExitReason::OwnerQuit,
        &state.shared,
        &app,
    )
    .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Account self-service actions (ADR-0030, plan §8).
// ---------------------------------------------------------------------------

/// Immediate **Time Transfer** to another account by username (ADR-0030).
/// The server debits the sender and credits the recipient atomically;
/// returns the refreshed `AccountAuthState` so the Widget updates balances.
#[tauri::command]
pub async fn transfer_time(
    state: State<'_, AppState>,
    to_username: String,
    seconds: i64,
) -> Result<AccountAuthState, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before transferring time".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/accounts/transfer",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .json(&TransferTimeRequest {
            to_username: &to_username,
            seconds,
        })
        .send()
        .await
        .map_err(|error| transport_error("time transfer", &error))?;
    store_account_response(&state.shared, response, "time transfer failed").await
}

/// Owner-confirmed **Points Transfer Request** (ADR-0030). Creates a pending
/// request and returns the single-use 6-digit code for the customer to read
/// to the owner. No points move until the owner confirms in web-admin.
#[tauri::command]
pub async fn request_points_transfer(
    state: State<'_, AppState>,
    to_username: String,
    points_centi: i64,
) -> Result<PointsTransferCodeResponse, String> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "log in before transferring points".to_owned())?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/accounts/transfer-points",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .json(&TransferPointsRequest {
            to_username: &to_username,
            points_centi,
        })
        .send()
        .await
        .map_err(|error| transport_error("points transfer", &error))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "points transfer request failed".to_owned()));
    }
    response
        .json::<PointsTransferCodeResponse>()
        .await
        .map_err(|error| format!("invalid points transfer response: {error}"))
}

/// Password Change outcome variants — no customer-facing English here.
/// Serialized as `{ "code": "<variant>" }` for the frontend to match.
#[derive(Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PasswordChangeError {
    Conflict,
    InternalError,
    CustomerMessage { message: String },
    Other,
}

fn change_password_error(status: reqwest::StatusCode, message: String) -> PasswordChangeError {
    match status {
        reqwest::StatusCode::CONFLICT => PasswordChangeError::Conflict,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR => PasswordChangeError::InternalError,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::BAD_REQUEST => {
            PasswordChangeError::CustomerMessage { message }
        }
        _ => PasswordChangeError::Other,
    }
}

/// Customer self-service **Password Change** (ADR-0037). Keeps the
/// Account Session and Active Session valid; returns a structured
/// error code for the frontend to map to display copy.
#[tauri::command]
pub async fn change_account_password(
    state: State<'_, AppState>,
    old_password: String,
    new_password: String,
) -> Result<(), PasswordChangeError> {
    let auth = state
        .shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or(PasswordChangeError::Other)?;
    let config = state.shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/accounts/change-password",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .json(&ChangePasswordRequest {
            old_password: &old_password,
            new_password: &new_password,
        })
        .send()
        .await
        .map_err(|_error| PasswordChangeError::Other)?;
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .text()
            .await
            .map_err(|_error| PasswordChangeError::Other)?;
        return Err(change_password_error(status, message));
    }
    Ok(())
}

async fn disconnect_shared(shared: &SharedState) -> Result<(), String> {
    let handle = shared.connection_task.lock().await.take();
    if let Some(handle) = handle {
        handle.abort();
    }
    *shared.countdown_anchor.write().await = None;
    *shared.shutdown_deadline.write().await = None;
    *shared.shutdown_issued_at.write().await = None;
    client_connection::disarm_offline_shutdown(
        shared,
        "the owner disconnected or reconfigured this Client",
    )
    .await;
    client_connection::reset_owner_settings_state(shared).await;
    *shared.offline_shutdown_issued_at.write().await = None;
    *shared.guest_tamper_kick_pending.write().await = false;
    clear_account_auth(shared).await;
    let mut timer = shared.timer.write().await;
    timer.connected = false;
    timer.connection_status = "disconnected".to_owned();
    timer.pc_status = "offline".to_owned();
    timer.power_state = "active".to_owned();
    timer.shutdown_in_seconds = None;
    timer.offline_shutdown_in_seconds = None;
    timer.active_username = None;
    timer.timer_mode = "guest".to_owned();
    timer.overlay_locked = false;
    timer.tamper_kill_pending = false;
    timer.remaining_seconds = timer.guest_remaining_seconds;
    timer.expired = timer.guest_remaining_seconds == 0;
    Ok(())
}

pub(crate) async fn disconnect_and_notify(shared: &SharedState) -> Result<(), String> {
    let should_notify = shared.timer.read().await.connected;
    let config = shared.config.read().await.clone();
    disconnect_shared(shared).await?;
    if !should_notify {
        return Ok(());
    }
    // Disconnect is MAC-keyed (ADR-0031): the path no longer carries
    // `pc_id`, and the server resolves identity from the MAC header +
    // peer address. A fresh install with no `pc_id` and no MAC will
    // silently no-op (the server rejects with `pc_unregistered` and
    // there is nothing to mark offline yet).
    let boot_url = config.server_url.clone();
    let mac = crate::mac_address::collect_mac_for_admin(&admin_host(&boot_url));
    let Some(mac) = mac else {
        return Ok(());
    };

    let endpoint = format!(
        "{}/api/identity/disconnect",
        config.server_url.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("could not create disconnect client: {error}"))?;
    let response = client
        .post(endpoint)
        .header("X-OpenPiso-Mac", mac)
        .send()
        .await
        .map_err(|error| format!("could not notify admin server of disconnect: {error}"))?;
    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| format!("disconnect returned {status}"));
        return Err(format!(
            "could not notify admin server of disconnect: {text}"
        ));
    }
    Ok(())
}

/// Extract the host portion of an admin server URL for MAC resolution.
fn admin_host(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let after_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let host_port = after_scheme.split('/').next().unwrap_or("");
    host_port
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(host_port)
        .to_owned()
}

/// Structured failure from the Client's Account login path. The lockout
/// variant preserves the server's `Retry-After` value for the customer UI;
/// other failures retain the server's existing message.
#[derive(Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AccountLoginError {
    Lockout { retry_after_seconds: u64 },
    Message { message: String },
}

fn account_auth_error_message(error: AccountLoginError) -> String {
    match error {
        AccountLoginError::Lockout {
            retry_after_seconds,
        } => format!("too many attempts; try again in {retry_after_seconds}s"),
        AccountLoginError::Message { message } => message,
    }
}

async fn authenticate_account(
    shared: &Arc<SharedState>,
    action: &str,
    username: &str,
    password: &str,
) -> Result<AccountAuthState, AccountLoginError> {
    let config = shared.config.read().await.clone();
    let pc_id = current_pc_id(shared)
        .await
        .map_err(|message| AccountLoginError::Message { message })?;
    let endpoint = format!(
        "{}/api/accounts/{action}",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .json(&AccountCredentials {
            username,
            password,
            pc_id: &pc_id,
        })
        .send()
        .await
        .map_err(|error| AccountLoginError::Message {
            message: transport_error(&format!("account {action}"), &error),
        })?;
    let status = response.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS && action == "login" {
            let retry_after_seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(0);
            return Err(AccountLoginError::Lockout {
                retry_after_seconds,
            });
        }
        return Err(AccountLoginError::Message {
            message: response
                .text()
                .await
                .unwrap_or_else(|_| "account request failed".to_owned()),
        });
    }
    let auth =
        response
            .json::<AccountAuthState>()
            .await
            .map_err(|error| AccountLoginError::Message {
                message: format!("invalid account response: {error}"),
            })?;
    set_account_auth(shared, auth.clone()).await;
    Ok(auth)
}

async fn mutate_account_session(
    shared: &Arc<SharedState>,
    action: &str,
) -> Result<AccountAuthState, String> {
    let auth = shared
        .account_auth
        .read()
        .await
        .clone()
        .ok_or_else(|| "no account is logged in".to_owned())?;
    let config = shared.config.read().await.clone();
    let endpoint = format!(
        "{}/api/accounts/{action}",
        config.server_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", auth.token),
        )
        .send()
        .await
        .map_err(|error| transport_error(&format!("account {action}"), &error))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "account session request failed".to_owned()));
    }
    let auth = response
        .json::<AccountAuthState>()
        .await
        .map_err(|error| format!("invalid account response: {error}"))?;
    set_account_auth(shared, auth.clone()).await;
    Ok(auth)
}

async fn store_account_response(
    shared: &Arc<SharedState>,
    response: reqwest::Response,
    fallback: &str,
) -> Result<AccountAuthState, String> {
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| fallback.to_owned()));
    }
    let auth = response
        .json::<AccountAuthState>()
        .await
        .map_err(|error| format!("invalid account response: {error}"))?;
    set_account_auth(shared, auth.clone()).await;
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::{
        password_hash::{PasswordHasher, SaltString},
        Argon2,
    };

    /// The reason `transport_error` exists: `reqwest::Error` prints the request
    /// URL, and these messages reach the customer-facing Lock. If this test's
    /// first assertion ever fails, reqwest stopped leaking and the sanitizer
    /// can be reconsidered.
    #[tokio::test]
    async fn transport_error_keeps_the_admin_address_out_of_customer_copy() {
        let failure = reqwest::Client::new()
            .get("http://127.0.0.1:9/api/accounts/login")
            .timeout(Duration::from_millis(250))
            .send()
            .await
            .expect_err("discard port must not answer");
        assert!(
            failure.to_string().contains("127.0.0.1:9"),
            "expected reqwest to embed the address, got: {failure}"
        );

        let message = transport_error("account login", &failure);
        assert_eq!(message, ADMIN_UNREACHABLE);
    }

    #[test]
    fn change_password_error_preserves_unauthorized_and_validation_copy() {
        use super::change_password_error;
        assert!(matches!(
            change_password_error(
                reqwest::StatusCode::UNAUTHORIZED,
                "invalid username or password".to_owned(),
            ),
            PasswordChangeError::CustomerMessage { message }
                if message == "invalid username or password"
        ));
        assert!(matches!(
            change_password_error(
                reqwest::StatusCode::BAD_REQUEST,
                "password must be between 4 and 128 characters".to_owned(),
            ),
            PasswordChangeError::CustomerMessage { message }
                if message == "password must be between 4 and 128 characters"
        ));
    }

    #[test]
    fn change_password_error_keeps_new_conflict_and_failure_variants() {
        use super::change_password_error;
        assert!(matches!(
            change_password_error(reqwest::StatusCode::CONFLICT, String::new()),
            PasswordChangeError::Conflict
        ));
        assert!(matches!(
            change_password_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, String::new()),
            PasswordChangeError::InternalError
        ));
        assert!(matches!(
            change_password_error(reqwest::StatusCode::BAD_GATEWAY, "proxy error".to_owned()),
            PasswordChangeError::Other
        ));
    }

    /// Hash a PIN the same way the admin does so the offline verifier sees a
    /// representative input (`Argon2::default()` + a PHC-encoded salt).
    fn hash(pin: &str) -> String {
        let salt = SaltString::encode_b64(b"openpiso-test-salt").expect("salt");
        Argon2::default()
            .hash_password(pin.as_bytes(), &salt)
            .expect("hash")
            .to_string()
    }

    #[test]
    fn first_configure_persists_even_when_value_matches_default() {
        // Fresh install: no `client-config.json` yet (`configured == false`).
        // The owner points the client at a server that happens to equal the
        // in-memory default (`http://127.0.0.1:3210` in local / single-box
        // setups), so `changed` is false — but the file must still be created,
        // otherwise nothing is ever persisted and the PC reverts to defaults
        // on the next boot. Regression for "client-config not generated on
        // Save-and-connect / Reconnect".
        assert!(should_persist_config(false, false));
    }

    #[test]
    fn redundant_save_after_configured_is_a_noop_write() {
        // ADR-0031 no-op-write guard still holds once a file exists.
        assert!(!should_persist_config(false, true));
    }

    #[test]
    fn changed_value_always_persists() {
        assert!(should_persist_config(true, true));
        assert!(should_persist_config(true, false));
    }

    #[tokio::test]
    async fn owner_setting_mutations_require_pin_unlock() {
        let state = AppState::default();
        assert!(require_owner_settings_unlocked(&state.shared)
            .await
            .is_err());

        *state.shared.owner_settings_unlocked.write().await = true;
        assert!(require_owner_settings_unlocked(&state.shared).await.is_ok());
    }

    #[test]
    fn offline_verifier_accepts_the_correct_pin_and_rejects_others() {
        let encoded = hash("246810");
        assert!(verify_pin_against_cached_hash("246810", &encoded));
        assert!(!verify_pin_against_cached_hash("000000", &encoded));
        // An empty or malformed cached hash never verifies (and never panics),
        // so a corrupt cache fails closed rather than unlocking settings.
        assert!(!verify_pin_against_cached_hash("246810", ""));
        assert!(!verify_pin_against_cached_hash("246810", "not-a-phc-hash"));
    }

    /// Real-command scenarios: `any_thread` exists only on Windows and Linux (macOS Wry needs the main thread).
    #[cfg(any(windows, target_os = "linux"))]
    mod real_command_tests {
        use super::*;
        use crate::state::{AccountSummary, ActiveSession};
        use tauri::Manager;

        fn session_enriched_timer_state() -> TimerState {
            TimerState {
                pc_id: Some("pc-01".to_owned()),
                pc_name: "PC 01".to_owned(),
                pc_status: "in_use".to_owned(),
                remaining_seconds: 300,
                guest_remaining_seconds: 0,
                timer_mode: "account".to_owned(),
                attendance_supported: false,
                active_username: Some("customer".to_owned()),
                connected: true,
                connection_status: "connected".to_owned(),
                expired: false,
                overlay_locked: false,
                tamper_kill_pending: false,
                power_state: "active".to_owned(),
                shutdown_in_seconds: None,
                offline_shutdown_in_seconds: None,
                shutdown_grace_seconds: 60,
                shop_status: None,
                error: None,
            }
        }

        fn mock_account_auth() -> AccountAuthState {
            AccountAuthState {
                token: "tok".to_owned(),
                account: AccountSummary {
                    id: "account-1".to_owned(),
                    username: "customer".to_owned(),
                    remaining_seconds: 300,
                    points_balance_centi: 0,
                    status: "active".to_owned(),
                },
                session: ActiveSession {
                    id: "session-1".to_owned(),
                    account_id: "account-1".to_owned(),
                    username: "customer".to_owned(),
                    pc_id: "pc-01".to_owned(),
                    remaining_seconds: 300,
                    points_balance_centi: 0,
                    claimable_guest_seconds: 0,
                },
            }
        }

        /// Commands only accept the Wry `AppHandle`, so tests need a real (window-less) app; Linux needs a display.
        fn real_app() -> &'static AppHandle {
            static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();
            APP.get_or_init(|| {
                let dir = tempfile::tempdir().expect("config dir");
                std::env::set_var(
                    crate::startup_coordinator::CONFIG_DIR_OVERRIDE_ENV,
                    dir.path(),
                );
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let mut context = tauri::generate_context!();
                    context.config_mut().app.windows.clear();
                    let app = tauri::Builder::default()
                        .any_thread()
                        .manage(AppState::default())
                        .build(context)
                        .expect("real Tauri app (needs a display on Linux)");
                    sender.send(app.handle().clone()).expect("handle receiver");
                    // The parked thread keeps the app and its config dir alive.
                    let _dir = dir;
                    loop {
                        std::thread::park();
                    }
                });
                receiver.recv().expect("real Tauri app handle")
            })
        }

        /// Serializes scenarios over the one managed `AppState`; clears saved config on exit.
        struct RealAppLock(#[allow(dead_code)] tokio::sync::MutexGuard<'static, ()>);

        impl Drop for RealAppLock {
            fn drop(&mut self) {
                let Some(dir) = crate::startup_coordinator::config_dir_override() else {
                    return;
                };
                for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        async fn lock_real_app() -> RealAppLock {
            static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            RealAppLock(SERIAL.lock().await)
        }

        /// Logout replies with the PC's Guest Time and no Active Session, like the real Admin.
        async fn spawn_admin() -> String {
            use axum::{routing::post, Json, Router};
            let router = Router::new()
                .route(
                    "/api/accounts/logout",
                    post(|| async {
                        Json(serde_json::json!({
                            "id": "pc-01",
                            "name": "PC 01",
                            "status": "online",
                            "remaining_seconds": 90,
                            "guest_remaining_seconds": 90,
                            "timer_mode": "guest",
                            "active_session": null,
                        }))
                    }),
                )
                .route("/api/identity/disconnect", post(|| async { "ok" }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind test admin");
            let url = format!("http://{}", listener.local_addr().expect("addr"));
            tokio::spawn(async move { axum::serve(listener, router).await });
            url
        }

        fn client_config(server_url: &str, view_mode: &str) -> ClientConfig {
            ClientConfig {
                server_url: server_url.to_owned(),
                pc_id: Some("pc-01".to_owned()),
                view_mode: view_mode.to_owned(),
                launch_at_startup: true,
                disable_windows_sign_out: false,
                offline_grace_seconds: 120,
            }
        }

        #[tokio::test]
        async fn logout_replaces_account_session_time_with_guest_time() {
            let _serial = lock_real_app().await;
            let app = real_app();
            let state = app.state::<AppState>();
            let server_url = spawn_admin().await;
            *state.shared.config.write().await = client_config(&server_url, "widget");
            *state.shared.timer.write().await = session_enriched_timer_state();
            *state.shared.account_auth.write().await = Some(mock_account_auth());

            logout_account(app.clone(), state.clone())
                .await
                .expect("logout succeeds");

            assert!(state.shared.account_auth.read().await.is_none());
            let timer = state.shared.timer.read().await.clone();
            assert_eq!(timer.remaining_seconds, 90, "Guest Time, not session time");
            assert_eq!(timer.timer_mode, "guest");
            assert_eq!(timer.active_username, None);
        }

        #[tokio::test]
        async fn changing_only_view_mode_keeps_time_connection_and_server() {
            let _serial = lock_real_app().await;
            let app = real_app();
            let state = app.state::<AppState>();
            let server_url = spawn_admin().await;
            *state.shared.config.write().await = client_config(&server_url, "mini");
            *state.shared.configured.write().await = true;
            *state.shared.timer.write().await = TimerState {
                pc_id: Some("pc-01".to_owned()),
                remaining_seconds: 300,
                guest_remaining_seconds: 300,
                connected: true,
                connection_status: "connected".to_owned(),
                ..Default::default()
            };

            let response = set_client_config(
                app.clone(),
                state.clone(),
                server_url.clone(),
                Some("widget".to_owned()),
            )
            .await
            .expect("view mode change succeeds");

            assert_eq!(response.config.view_mode, "widget");
            assert_eq!(response.config.server_url, server_url);
            let timer = state.shared.timer.read().await.clone();
            assert_eq!(timer.remaining_seconds, 300);
            assert!(timer.connected);
            assert_eq!(timer.connection_status, "connected");
        }

        #[tokio::test]
        async fn changing_the_server_url_disconnects_and_clears_stale_state() {
            let _serial = lock_real_app().await;
            let app = real_app();
            let state = app.state::<AppState>();
            let old_url = spawn_admin().await;
            let new_url = spawn_admin().await;
            *state.shared.config.write().await = client_config(&old_url, "widget");
            *state.shared.configured.write().await = true;
            *state.shared.owner_settings_unlocked.write().await = true;
            *state.shared.owner_pin_hash.write().await = Some("cached-hash".to_owned());
            *state.shared.timer.write().await = session_enriched_timer_state();
            *state.shared.account_auth.write().await = Some(mock_account_auth());
            *state.shared.countdown_anchor.write().await = Some(crate::state::CountdownAnchor {
                remaining_seconds: 300,
                synchronized_at: std::time::Instant::now(),
            });

            let response = set_client_config(app.clone(), state.clone(), new_url.clone(), None)
                .await
                .expect("server change succeeds");

            assert_eq!(response.config.server_url, new_url);
            let timer = state.shared.timer.read().await.clone();
            assert!(!timer.connected);
            assert_eq!(timer.connection_status, "disconnected");
            assert_eq!(timer.remaining_seconds, 0);
            assert!(timer.pc_name.is_empty());
            assert!(state.shared.account_auth.read().await.is_none());
            assert!(state.shared.countdown_anchor.read().await.is_none());
            assert!(state.shared.owner_pin_hash.read().await.is_none());
            assert!(!*state.shared.owner_settings_unlocked.read().await);
        }
    }
}

use chrono::Utc;
use tauri::{Manager, State};

use crate::{
    app_state::AppState,
    db,
    errors::{AppError, AppResult},
    kiosk_lock,
    models::{
        KioskConfig, KioskLockState, KioskStatus, KioskStatusKind, Pc, ShopStatus, ThemeSettings,
    },
    terminal_selection::{CloseReason, SelectionState},
};

/// Open the Central Terminal Selection Window for `pc_id` (ADR-0023). The
/// set pin is asserted server-side; coins accepted during the window are
/// credited to `pc_id` (the device payload never carries a `pc_id`).
/// A second open while one is active is rejected with `"terminal in use"`.
#[tauri::command]
pub async fn open_selection(
    state: State<'_, AppState>,
    pc_id: String,
) -> AppResult<SelectionState> {
    state.terminal_selection.open(&pc_id).await
}

/// Close the active Selection Window. The optional `reason` is one of
/// `"enter"`, `"inactivity"`, `"hard_cap"`, `"manual"`, or `"shutdown"`,
/// recorded for the broadcast snapshot. Defaults to `"manual"` so a bare
/// close from the kiosk still works.
#[tauri::command]
pub async fn close_selection(
    state: State<'_, AppState>,
    reason: Option<String>,
) -> AppResult<Option<SelectionState>> {
    let parsed = parse_close_reason(reason.as_deref());
    state.terminal_selection.close(parsed).await
}

/// Snapshot of the current Selection Window, or `None` if no window is
/// open. The frontend calls this on mount; subsequent updates arrive via
/// the `selection-state` Tauri event.
#[tauri::command]
pub async fn get_selection(state: State<'_, AppState>) -> AppResult<Option<SelectionState>> {
    Ok(state.terminal_selection.get().await)
}

/// Live, enriched PC list for the Central Terminal kiosk (M3, ADR-0023).
/// No owner auth: the kiosk is the customer-facing surface and the only
/// data it reads is the public PC timer / login state. Each entry is
/// the same `Pc` the owner dashboard sees; the kiosk then layers
/// `pc-updated` events on top so its grid stays live. Read-only — the
/// kiosk never mutates the PC list itself (per AGENTS, all money/time
/// mutations live in Rust).
#[tauri::command]
pub async fn list_pcs_kiosk(state: State<'_, AppState>) -> AppResult<Vec<Pc>> {
    let pcs = db::pcs(&state.pool).await?;
    Ok(state
        .sessions
        .enrich_pcs(pcs)
        .await
        .into_iter()
        .map(|pc| state.events.stamp_shop_status(pc))
        .collect())
}

/// Coin-source health for the Central Terminal kiosk (M3, ADR-0023). No
/// owner auth: the kiosk uses the result to choose between the normal
/// flow and the "out of order" screen. The M2 simulator is always
/// `available`; the M4 serial source will publish `out_of_order` when
/// the COM handle is poisoned or missing. The frontend can also subscribe
/// to the `kiosk-status` event for live changes.
#[tauri::command]
pub async fn get_kiosk_status(state: State<'_, AppState>) -> AppResult<KioskStatus> {
    Ok(current_kiosk_status(&state).await)
}

/// Live time-purchase rate for the Central Terminal kiosk preview (ADR-0008).
/// No owner auth: the rate is public (it is printed on the insert screen) and
/// read-only. The kiosk uses it to render the "equivalent time" preview so it
/// matches the owner's configured rate; the server still computes the
/// authoritative `seconds_added` when each coin commits.
#[tauri::command]
pub async fn get_kiosk_config(state: State<'_, AppState>) -> AppResult<KioskConfig> {
    db::kiosk_config(&state.pool).await
}

/// Current shop theme for the Central Terminal. Read directly from the
/// admin-owned SQLite database so kiosk startup does not depend on HTTP.
#[tauri::command]
pub async fn get_theme(state: State<'_, AppState>) -> AppResult<ThemeSettings> {
    db::theme_settings(&state.pool).await
}

/// Current Shop Name for the Central Terminal. Read directly from the
/// admin-owned SQLite database so kiosk startup does not depend on HTTP.
#[tauri::command]
pub async fn get_shop_name(state: State<'_, AppState>) -> AppResult<String> {
    kiosk_shop_name(&state.pool).await
}

/// Current server-resolved Shop Status for the Central Terminal. The kiosk
/// renders this value and never evaluates the saved schedule itself.
#[tauri::command]
pub async fn get_shop_status(state: State<'_, AppState>) -> AppResult<ShopStatus> {
    db::shop_status(&state.pool).await
}

pub(crate) async fn kiosk_shop_name(pool: &sqlx::SqlitePool) -> AppResult<String> {
    db::shop_name(pool).await
}

/// Current Kiosk Lock state (ADR-0056). The kiosk reads this on mount to
/// render its header signal. The boot value is `locked: true` unless pinning
/// the window failed during setup, in which case it honestly reports
/// unlocked rather than claiming a pin the OS refused.
#[tauri::command]
pub async fn get_kiosk_lock(state: State<'_, AppState>) -> AppResult<KioskLockState> {
    Ok(KioskLockState {
        locked: state.kiosk_lock.is_locked().await,
    })
}

/// Engage or release the Kiosk Lock (ADR-0056).
///
/// Releasing (`locked: false`) requires the owner PIN and is rate-limited by
/// the same backoff as the Web Admin login; engaging needs no PIN because
/// tightening the kiosk is always safe. The window flag is applied *before*
/// the in-memory state moves, so a failed window call can never leave the UI
/// claiming a state the OS does not have.
///
/// The PIN is verified here, in Rust — the webview only ever forwards the
/// typed characters and never sees the credential or the decision logic.
#[tauri::command]
pub async fn set_kiosk_lock(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    locked: bool,
    pin: Option<String>,
) -> AppResult<KioskLockState> {
    if locked {
        set_kiosk_always_on_top(&app, true)?;
        state.kiosk_lock.engage().await;
        kiosk_lock::audit_state_change(&state.pool, true).await?;
        return Ok(KioskLockState { locked: true });
    }

    kiosk_lock::authorize_release(
        &state.pool,
        &state.kiosk_lock,
        &pin.unwrap_or_default(),
        Utc::now(),
    )
    .await?;

    set_kiosk_always_on_top(&app, false)?;
    state.kiosk_lock.record_release_success().await;
    kiosk_lock::audit_state_change(&state.pool, false).await?;
    Ok(KioskLockState { locked: false })
}

/// Apply the always-on-top flag to the kiosk window. Shared with the setup
/// hook in `lib.rs`, which asserts the locked state at launch.
pub(crate) fn set_kiosk_always_on_top(app: &tauri::AppHandle, on_top: bool) -> AppResult<()> {
    let window = app
        .get_webview_window(KIOSK_WINDOW_LABEL)
        .ok_or_else(|| AppError::Window("kiosk window is not available".to_owned()))?;
    window
        .set_always_on_top(on_top)
        .map_err(|error| AppError::Window(format!("could not set always-on-top: {error}")))
}

/// The label of the Central Terminal window, as declared in
/// `tauri.conf.json`. Kept here so the lock and the setup hook cannot drift.
pub(crate) const KIOSK_WINDOW_LABEL: &str = "kiosk";

/// DEV-ONLY coin-insert simulator for the Central Terminal kiosk.
///
/// Compiled only in debug builds (`#[cfg(debug_assertions)]`), so it is
/// physically absent from `tauri build` release binaries — the production
/// kiosk surface stays exactly the M6 set in `generate_handler!` (`lib.rs`). It
/// injects `pulse_count` pulses (default 1) into the active Selection
/// Window's simulator source, letting the kiosk be driven end-to-end with
/// no coin hardware. The 500ms server poll then closes the batch and
/// commits it through the same `record_coin_event` path a real coin uses,
/// so this exercises the full money/time pipeline, not a UI shortcut.
///
/// Requires an active selection (the insert screen) and the simulator
/// source: on real serial hardware `inject_simulator_pulses` returns a
/// `Conflict`, and with no open window it returns a `Conflict` too. Only
/// pulse counts in the configured map (1, 5, 10, 20 on a fresh install)
/// commit; other counts fail-closed in the source.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn dev_inject_coin(
    state: State<'_, AppState>,
    pulse_count: Option<i64>,
) -> AppResult<SelectionState> {
    state
        .terminal_selection
        .inject_simulator_pulses(pulse_count.unwrap_or(1))
        .await
}

async fn current_kiosk_status(state: &State<'_, AppState>) -> KioskStatus {
    if state.terminal_selection.is_source_available().await {
        KioskStatus {
            kind: KioskStatusKind::Available,
            reason: String::new(),
        }
    } else {
        KioskStatus {
            kind: KioskStatusKind::OutOfOrder,
            reason: "Coin acceptor is unavailable".to_owned(),
        }
    }
}

fn parse_close_reason(reason: Option<&str>) -> CloseReason {
    match reason {
        Some("enter") => CloseReason::Enter,
        Some("inactivity") => CloseReason::Inactivity,
        Some("hard_cap") => CloseReason::HardCap,
        Some("shutdown") => CloseReason::Shutdown,
        _ => CloseReason::Manual,
    }
}

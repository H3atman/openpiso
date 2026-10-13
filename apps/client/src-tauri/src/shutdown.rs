use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::AppHandle;

use crate::state::SharedState;

/// Default shutdown grace window (seconds) shown to the customer before the PC
/// powers off at expiry. The admin server can override this per shop.
pub const DEFAULT_GRACE_SECONDS: i64 = 300;

/// How long after issuing an OS shutdown the Client waits before treating it
/// as having not taken effect and issuing it again. `shutdown.exe` reports
/// only that it was spawned, so a power-off that Windows then refuses, aborts,
/// or fails would otherwise leave the machine running forever with the one-shot
/// guard already spent.
pub const SHUTDOWN_RETRY_SECONDS: u64 = 20;

/// Why the Client is intentionally exiting. Every legitimate exit path routes
/// through this single typed chokepoint (`initiate_exit`) so the future Client
/// Guard (ADR-0048) has exactly one place to announce a clean exit and can
/// distinguish the owner quit from the two shutdown reasons.
pub use openpiso_core::guard::CleanExitReason as ClientExitReason;

/// Whether an authorized exit is currently in flight. Every legitimate exit
/// goes through `initiate_exit`, which raises this before anything else, so a
/// `WM_CLOSE` arriving from the taskbar context menu, Alt+F4, or the system
/// menu can be told apart from the owner PIN quit (ADR-0027), the expiry
/// power-off (ADR-0005), and the Offline Shutdown (ADR-0045).
///
/// This is only ever read by `should_allow_close`; it does not gate the exit
/// itself, so a stuck flag can never *prevent* a legitimate exit.
static EXIT_AUTHORIZED: AtomicBool = AtomicBool::new(false);

/// Whether window-level close refusal is enforced in this build. Mirrors the
/// `execute_os_shutdown` / `kiosk_hardening` gating: only an installed release
/// Windows build refuses `WM_CLOSE`, so `tauri dev` and non-Windows hosts can
/// always close the window normally and a developer is never trapped.
pub const CLOSE_REFUSAL_ENFORCED: bool = cfg!(all(windows, not(debug_assertions)));

/// Whether this exit reason needs the window close refusal lifted.
///
/// Only the owner PIN quit does: it exits through `app.exit`, which closes the
/// window itself. The two power-offs deliberately do **not** — the OS shutdown
/// is forced (`/f`), so Windows terminates the Client without needing its
/// consent, and lifting the refusal for them would leave the window closable
/// for the rest of the session if the power-off never actually happened.
pub fn authorizes_close(reason: ClientExitReason) -> bool {
    matches!(reason, ClientExitReason::OwnerQuit)
}

/// Mark an exit as authorized. Called by `initiate_exit` for the owner quit.
fn authorize_exit() {
    EXIT_AUTHORIZED.store(true, Ordering::SeqCst);
}

/// Whether an authorized exit is in flight right now.
pub fn exit_authorized() -> bool {
    EXIT_AUTHORIZED.load(Ordering::SeqCst)
}

/// Pure decision: may this `WindowEvent::CloseRequested` proceed?
///
/// A close is allowed when the build does not enforce refusal, or when an exit
/// authorized by `initiate_exit` is already in flight. Everything else — the
/// taskbar context menu's *Close window*, Alt+F4, the system menu, any third
/// party politely posting `WM_CLOSE` — is refused.
pub fn should_allow_close(exit_authorized: bool, enforced: bool) -> bool {
    !enforced || exit_authorized
}

/// Seconds left in the grace countdown for display, clamped at zero.
pub fn grace_remaining(deadline: Instant, now: Instant) -> i64 {
    deadline
        .saturating_duration_since(now)
        .as_secs()
        .min(i64::MAX as u64) as i64
}

/// Whether the real OS shutdown should fire now: the server still commands a
/// shutdown, the grace deadline has elapsed, and we have not already fired.
pub fn should_execute(
    power_state: &str,
    deadline: Option<Instant>,
    issued_at: Option<Instant>,
    now: Instant,
) -> bool {
    power_state == "shutdown"
        && deadline.is_some_and(|deadline| now >= deadline)
        && retry_is_due(issued_at, now)
}

/// Whether a power-off may be issued given the last attempt. Never issued
/// before the grace elapses, never re-issued inside the retry window, and
/// re-issued afterwards because a machine that is still running proves the
/// previous attempt did not power it off.
pub fn retry_is_due(issued_at: Option<Instant>, now: Instant) -> bool {
    match issued_at {
        None => true,
        Some(issued_at) => {
            now.saturating_duration_since(issued_at) >= Duration::from_secs(SHUTDOWN_RETRY_SECONDS)
        }
    }
}

/// The exact `shutdown.exe` arguments for a session power-off.
///
/// `/f` is load-bearing: without it Windows asks any app that does not close
/// in time whether to "shut down anyway", and a customer who picks *Cancel*
/// keeps a running PC. Same rationale as the Guard's Escalated Power Off
/// (ADR-0077).
pub fn shutdown_args() -> [&'static str; 6] {
    ["/s", "/f", "/t", "0", "/c", "OpenPiso session ended"]
}

/// Perform the real Windows OS shutdown. Release builds power the machine off;
/// debug and non-Windows builds only log, so a developer running against a dev
/// admin server is never powered off (ADR-0022). Shutdown authority lives here
/// in Rust and is never exposed to the Svelte frontend (ADR-0005).
pub fn execute_os_shutdown() -> Result<(), String> {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        std::process::Command::new("shutdown")
            .args(shutdown_args())
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    #[cfg(not(all(windows, not(debug_assertions))))]
    {
        tracing::info!(
            target: "openpiso_client::shutdown",
            args = ?shutdown_args(),
            "OS shutdown suppressed (debug or non-Windows build); a release build would power off now"
        );
        Ok(())
    }
}

/// Single chokepoint for all intentional Client exits. Each call site passes
/// the typed reason so the dispatch and any future Client Guard hook
/// (ADR-0048) see a uniform intent.
///
/// Before every exit path the Guard pipe is sent a typed clean-exit message so
/// the Guard does not treat the intentional shutdown as a Tamper Kill.
///
/// - `OwnerQuit`: notifies the admin server of a clean disconnect, then asks
///   Tauri to exit.
/// - `ExpiryPowerOff` / `OfflineShutdown`: performs the real OS shutdown,
///   logs the outcome, and forwards a diagnostic on failure.
pub async fn initiate_exit(reason: ClientExitReason, shared: &SharedState, app: &AppHandle) {
    // Raised before the Guard message so authorization and the clean-exit
    // notice can never diverge: no refused close can look like a clean exit,
    // and no exit is refused after its `CleanExit` has gone out.
    if authorizes_close(reason) {
        authorize_exit();
    }
    match reason {
        ClientExitReason::OwnerQuit => {
            crate::guard_client::send_clean_exit(ClientExitReason::OwnerQuit);
            if let Err(error) = crate::commands::disconnect_and_notify(shared).await {
                tracing::warn!(
                    target: "openpiso_client::shutdown",
                    "Could not notify admin server before quit: {error}"
                );
            }
            app.exit(0);
        }
        ClientExitReason::ExpiryPowerOff => {
            crate::guard_client::send_clean_exit(ClientExitReason::ExpiryPowerOff);
            let pc_id = shared.config.read().await.pc_id.clone().unwrap_or_default();
            match execute_os_shutdown() {
                Ok(()) => tracing::info!(
                    target: "openpiso_client::shutdown",
                    pc_id = %pc_id,
                    "Grace period elapsed; issued OS shutdown"
                ),
                // The `error!` line is forwarded by the Diagnostic Forwarding
                // layer (ADR-0078); no separate send is needed here.
                Err(error) => {
                    tracing::error!(
                        target: "openpiso_client::shutdown",
                        pc_id = %pc_id,
                        "OS shutdown command failed: {error}"
                    );
                }
            }
        }
        ClientExitReason::OfflineShutdown => {
            crate::guard_client::send_clean_exit(ClientExitReason::OfflineShutdown);
            let pc_id = shared.config.read().await.pc_id.clone().unwrap_or_default();
            match execute_os_shutdown() {
                Ok(()) => tracing::info!(
                    target: "openpiso_client::shutdown",
                    pc_id = %pc_id,
                    "Offline Grace elapsed; issued OS shutdown"
                ),
                // The `error!` line is forwarded by the Diagnostic Forwarding
                // layer (ADR-0078); no separate send is needed here.
                Err(error) => {
                    tracing::error!(
                        target: "openpiso_client::shutdown",
                        pc_id = %pc_id,
                        "Offline OS shutdown command failed: {error}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn an_unauthorized_close_is_refused_only_where_refusal_is_enforced() {
        // The kiosk case: taskbar right-click → Close, Alt+F4, system menu.
        assert!(!should_allow_close(false, true));
        // A legitimate exit is already in flight (owner PIN quit, expiry
        // power-off, Offline Shutdown): the window must be allowed to go.
        assert!(should_allow_close(true, true));
    }

    #[test]
    fn a_developer_is_never_trapped_by_the_refusal() {
        // `tauri dev` and non-Windows hosts do not enforce, so the window
        // closes normally whatever the authorization flag says.
        assert!(should_allow_close(false, false));
        assert!(should_allow_close(true, false));
    }

    #[test]
    fn the_authorization_flag_tracks_the_exit_chokepoint() {
        // The flag is global, so drive it through its own transitions rather
        // than assuming a starting value from another test.
        authorize_exit();
        assert!(exit_authorized());
        // A failed power-off leaves the session running: back to unclosable.
        EXIT_AUTHORIZED.store(false, Ordering::SeqCst);
        assert!(!exit_authorized());
        assert!(!should_allow_close(exit_authorized(), true));
    }

    #[test]
    fn refusal_is_enforced_only_in_a_release_windows_build() {
        assert_eq!(
            CLOSE_REFUSAL_ENFORCED,
            cfg!(all(windows, not(debug_assertions))),
            "refusal must follow the same gate as execute_os_shutdown"
        );
    }

    #[test]
    fn the_os_shutdown_is_forced_so_windows_never_offers_a_cancel() {
        // Without `/f` Windows shows the "these apps are preventing shutdown"
        // screen; a customer who picks Cancel keeps a usable PC on free time.
        assert!(
            shutdown_args().contains(&"/f"),
            "the session power-off must be forced: {:?}",
            shutdown_args()
        );
        assert!(shutdown_args().contains(&"/s"));
    }

    #[test]
    fn a_power_off_never_lifts_the_close_refusal() {
        // Only the owner PIN quit closes the window itself; a forced OS
        // shutdown does not need the Client's consent, and authorizing it
        // would leave the window closable if the power-off never happened.
        assert!(!authorizes_close(ClientExitReason::ExpiryPowerOff));
        assert!(!authorizes_close(ClientExitReason::OfflineShutdown));
        assert!(authorizes_close(ClientExitReason::OwnerQuit));
    }

    #[test]
    fn a_power_off_that_did_not_take_effect_is_issued_again() {
        let now = Instant::now();
        let deadline = now - Duration::from_secs(1);
        let issued_at = now - Duration::from_secs(SHUTDOWN_RETRY_SECONDS);
        // Still running a full retry window after the attempt: it failed.
        assert!(should_execute(
            "shutdown",
            Some(deadline),
            Some(issued_at),
            now
        ));
        // Inside the window, the one-shot guard still holds.
        assert!(!should_execute(
            "shutdown",
            Some(deadline),
            Some(now - Duration::from_secs(SHUTDOWN_RETRY_SECONDS - 1)),
            now
        ));
    }

    #[test]
    fn grace_counts_down_and_clamps_at_zero() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(30);
        assert_eq!(grace_remaining(deadline, now), 30);
        assert_eq!(grace_remaining(deadline, now + Duration::from_secs(25)), 5);
        assert_eq!(grace_remaining(deadline, now + Duration::from_secs(40)), 0);
    }

    #[test]
    fn fires_only_when_commanded_grace_elapsed_and_not_yet_invoked() {
        let now = Instant::now();
        let deadline = now - Duration::from_secs(1);
        assert!(should_execute("shutdown", Some(deadline), None, now));
        // Just issued: never fire twice inside the retry window.
        assert!(!should_execute("shutdown", Some(deadline), Some(now), now));
        // No longer commanded (topped up): do not fire.
        assert!(!should_execute("active", Some(deadline), None, now));
        // Grace not elapsed yet: do not fire.
        let future = now + Duration::from_secs(10);
        assert!(!should_execute("shutdown", Some(future), None, now));
        // No deadline: do not fire.
        assert!(!should_execute("shutdown", None, None, now));
    }
}

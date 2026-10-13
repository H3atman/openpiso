mod attendance;
mod client_connection;
mod commands;
mod config_store;
mod diagnostic_request;
mod diagnostics;
mod forwarding;
mod guard_client;
mod kiosk_hardening;
mod mac_address;
mod offline_shutdown;
mod shutdown;
mod startup_arbitration;
mod startup_placeholder;
mod state;
mod window_mode;
mod windows_sign_out;

/// Local Tamper Kill handoff boundary (issues #366 / #367). Public so the
/// process-level harness and the future duplicate gate exercise the
/// production sender/receiver path rather than reimplementing it.
pub mod startup_coordinator;

pub use state::PendingTamperKillReport;

use std::{sync::Arc, time::Instant};

use state::AppState;
use tauri::Manager;
use tracing::{error, warn};
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    // Test-only start barrier, at the very top so the harness can release
    // real senders and real contenders into the same instant. Production
    // never sets the environment variable and never waits.
    startup_arbitration::wait_for_start_barrier();
    // Sender-only handoff submissions exit here, before the placeholder,
    // keyboard hooks, Guard connection, Tauri runtime, or any network task
    // can start. Ordinary launches return `None` and proceed untouched.
    if let Some(exit_code) = startup_coordinator::maybe_run_handoff_sender(&args) {
        std::process::exit(exit_code);
    }
    // Startup arbitration (issues #366 / #369, ADR-0080). The atomic
    // check-and-own runs before the placeholder, keyboard hooks, Guard
    // connection, Tauri/WebView initialization, normal state writes, or any
    // networking. An admitted launch continues; a losing launch hands off
    // any Guard report durably and exits inertly; an uncertain coordination
    // failure exits nonzero without admitting anything. The harness's probe
    // flag substitutes an inert runtime behind this admission boundary.
    let outcome = startup_arbitration::coordinate(&args);
    if outcome != startup_arbitration::Outcome::Admitted {
        std::process::exit(startup_arbitration::handle_early_outcome(&outcome));
    }
    if startup_arbitration::is_probe_launch(&args) {
        std::process::exit(startup_arbitration::run_probe_owner());
    }
    diagnostics::mark_process_start();
    // Paint an opaque native window before Tauri enters WebView2's cold-start
    // path. The release-Windows adapter waits only for the HWND to be visible;
    // debug and non-Windows builds no-op so development cannot be trapped.
    let startup_placeholder_error = startup_placeholder::start().err();
    // Keep the non-blocking writer guard alive until app exit so installed
    // Client diagnostics flush to the per-user file.
    let _log_guard = match diagnostics::init_tracing() {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("Client durable logging unavailable: {error}");
            if let Err(fallback_error) = tracing_subscriber::fmt()
                .with_target(true)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .try_init()
            {
                eprintln!("Client stdout logging unavailable: {fallback_error}");
            }
            None
        }
    };
    // First durable startup line: this launch owns the runtime (issue #369).
    startup_arbitration::log_admitted_owner();
    // A Client panic must leave evidence behind (ADR-0082): the hook appends
    // `logs/panic.log` synchronously from inside the hook. Installation is
    // unconditional — where no log directory resolves (`APPDATA` missing) the
    // hook still re-emits the record through tracing and only the file write
    // is skipped; the write itself stays best-effort either way.
    diagnostics::install_panic_hook(diagnostics::log_dir().as_deref());
    if let Some(error) = startup_placeholder_error {
        warn!(target: "openpiso_client::startup", "Could not show native startup placeholder: {error}");
    } else if startup_placeholder::is_visible() {
        diagnostics::startup_milestone("native_placeholder_visible");
    }

    // Install the app-level keyboard-hook hardening (ADR-0028, plan §7). It
    // stays dormant until the frontend marks the Lock active, and is a no-op
    // outside a release Windows build.
    diagnostics::startup_milestone("logging_ready");
    kiosk_hardening::start();
    if startup_placeholder::is_visible() {
        // The placeholder is the Lock's temporary native face. Block the same
        // casual escape chords until the frontend view machine takes over.
        kiosk_hardening::set_lock_active(true);
    }

    // Connect to the Client Guard's named pipe (ADR-0048). The Client holds
    // the pipe open for its lifetime so the Guard detects process death.
    // On non-Windows / no Guard installed this is a silent no-op.
    guard_client::connect();

    let builder = tauri::Builder::default();
    #[cfg(target_os = "windows")]
    let builder = builder.plugin(
        tauri_plugin_autostart::Builder::new()
            .app_name("OpenPiso Client")
            .build(),
    );

    let app = builder
        .manage(AppState::default())
        // Refuse `WM_CLOSE` unless a legitimate exit is already in flight
        // (ADR-0048). Every window-level kill lever — the taskbar context
        // menu's *Close window*, Alt+F4, the system menu — converges on this
        // one event, so refusing it here closes all of them at once. A refused
        // close never terminates the process, so no `ExitRequested` fires, no
        // pipe disconnect reaches the Guard, and nothing looks like a clean
        // exit. Enforced only in a release Windows build.
        .on_window_event(|window, event| {
            // Ordinary geometry and focus changes (resized, moved, scale
            // change, focus) log nothing: the temporary high-volume probes
            // that used to WARN here went out with issue #368, and the
            // Forwarded Diagnostic buffer is for meaningful evidence. Actual
            // window operation failures still WARN from `window_mode`.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if !shutdown::should_allow_close(
                    shutdown::exit_authorized(),
                    shutdown::CLOSE_REFUSAL_ENFORCED,
                ) {
                    api.prevent_close();
                    warn!(
                        target: "openpiso_client::shutdown",
                        label = window.label(),
                        "refused an unauthorized window close; the Client exits only through the owner PIN quit, expiry power-off, or Offline Shutdown"
                    );
                }
            }
        })
        .setup(|app| {
            diagnostics::startup_milestone("setup_entered");
            let shared = Arc::clone(&app.state::<AppState>().shared);
            let handle = app.handle().clone();
            // The window is created hidden; the frontend's first IPC call
            // reveals it. This is the safety net for a webview that never
            // boots (ADR-0029 §4.1 keeps one window, so an invisible one
            // means no kiosk surface at all).
            window_mode::arm_startup_reveal_fallback(&handle);
            let loaded_config = config_store::load(&handle);
            let persisted_offline_deadline = if loaded_config.is_some() {
                config_store::load_offline_shutdown_deadline(&handle)
            } else {
                // A deadline without a valid chosen ClientConfig cannot belong
                // to a previously-connected machine; clear it so setup never
                // inherits an offline shutdown race.
                if let Err(error) = config_store::clear_offline_shutdown_deadline(&handle) {
                    warn!(target: "openpiso_client::config_store", "Could not clear orphaned Offline Shutdown deadline: {error}");
                }
                None
            };
            #[cfg(target_os = "windows")]
            let launch_at_startup = loaded_config
                .as_ref()
                .map(|config| config.launch_at_startup)
                .unwrap_or(true);
            let relaunched_tamper_report = config_store::load_guard_tamper_kill_report();
            // Recover any locally handed-off reports before the first
            // heartbeat (issue #367). With an empty inbox this is the
            // historical merge-then-conditionally-persist behavior; with
            // inbox files the merged report is persisted before any of them
            // is removed, so a crash mid-recovery stays recoverable. A
            // Tamper Kill is the story's opening line: Admin must see it
            // even if this relaunch dies before the acknowledgement lands
            // (ADR-0078's INFO allowlist).
            let main_report_path = config_store::tamper_kill_report_path(&handle).ok();
            let report = startup_coordinator::recover_pending_report(
                main_report_path.as_deref(),
                &startup_coordinator::resolve_config_dir(),
                relaunched_tamper_report,
            )
            .map_err(std::io::Error::other)?;
            if report.is_some() {
                tracing::info!(
                    target: openpiso_core::forwarding::CLIENT_TAMPER_TARGET,
                    "Tamper Kill report received from the Guard; holding the connection until Admin acknowledges it"
                );
            }

            // A Guard relaunch report is already durable: recovery persisted
            // it before this setup ran. Failing closed is unnecessary here;
            // the heartbeat path retries the persisted report until Admin
            // acknowledges it.
            tauri::async_runtime::block_on(async {
                *shared.pending_tamper_kill.write().await = report;
                let mut timer = shared.timer.write().await;
                timer.tamper_kill_pending = report.is_some();
                if report.is_some() {
                    // The first heartbeat is an acknowledgement barrier: do
                    // not briefly surface Disconnected/Insert Coin while it
                    // is still in flight. Preserve any resumed Offline
                    // Shutdown countdown for the safety loop.
                    timer.connected = false;
                    timer.connection_status = "connecting".to_owned();
                    timer.error = None;
                }
                drop(timer);
                if report.is_some() {
                    shared
                        .tamper_kill_generation
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
            #[cfg(target_os = "windows")]
            if let Err(error) = commands::apply_autostart(&handle, launch_at_startup) {
                warn!(target: "openpiso_client::startup", "Could not apply Launch at startup setting: {error}");
            }
            tauri::async_runtime::spawn(async move {
                if let Some(config) = loaded_config {
                    let pc_id = config.pc_id.clone();
                    *shared.config.write().await = config;
                    shared.timer.write().await.pc_id = pc_id;
                    // A config file existed and parsed: this machine has been
                    // set up before. A fresh/corrupt install leaves this
                    // `false`, and the Lock surfaces the un-gated setup form.
                    *shared.configured.write().await = true;
                }
                if let Some(persisted) = persisted_offline_deadline {
                        let now = Instant::now();
                        let decision = offline_shutdown::resume_deadline(
                            persisted.deadline_unix_ms,
                            client_connection::unix_time_millis(),
                            now,
                        );
                        *shared.offline_shutdown_deadline.write().await = Some(decision.deadline);
                        *shared.offline_shutdown_issued_at.write().await = None;
                        shared.timer.write().await.offline_shutdown_in_seconds =
                            Some(shutdown::grace_remaining(decision.deadline, now));
                        if decision.expired_on_launch {
                            let deadline_unix_ms = client_connection::unix_time_millis()
                                .saturating_add(
                                    offline_shutdown::EXPIRED_REWARNING_SECONDS as i64 * 1_000,
                                );
                            if let Err(error) = config_store::save_offline_shutdown_deadline(
                                &handle,
                                deadline_unix_ms,
                            ) {
                                warn!(target: "openpiso_client::config_store", "Could not persist Offline Shutdown re-warning deadline: {error}");
                            }
                        }
                }
                // Auto-connect to the configured admin server on launch so the
                // client reaches it without the owner re-opening settings. The
                // `pc_id` is server-owned (ADR-0031 / M4) — on a fresh install
                // it is `None` and the Lock renders "Registering this PC…"
                // until the first heartbeat adopts one. A failed reach still
                // degrades to the Lock with a banner; the typed
                // `pc_unregistered` rejection is the "ask the owner" copy.
                commands::start_connection(&shared, handle.clone()).await;
                // Dev-only: when `bun run dev:client` sets
                // OPENPISO_CLIENT_DEV_SHUTDOWN, seed the Fullscreen Lock's
                // shutdown grace countdown so it is visible by default for UI
                // testing. Compiled only in debug builds; the connection task
                // started above provides the local countdown ticker.
                #[cfg(debug_assertions)]
                if std::env::var_os("OPENPISO_CLIENT_DEV_SHUTDOWN").is_some() {
                    client_connection::seed_dev_shutdown_countdown(&shared).await;
                }
            });
            diagnostics::startup_milestone("setup_finished");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::prepare_startup_handoff,
            commands::frontend_ready,
            commands::get_client_config,
            commands::is_client_configured,
            commands::set_client_config,
            commands::is_windows_platform,
            commands::connect_to_server,
            commands::disconnect_from_server,
            commands::pause_offline_shutdown,
            commands::resume_offline_shutdown,
            commands::get_timer_state,
            attendance::get_attendance_summary,
            attendance::get_attendance_month,
            attendance::claim_attendance,
            commands::sign_up_account,
            commands::login_account,
            commands::get_account_auth,
            commands::claim_guest_time,
            commands::redeem_voucher,
            commands::get_redemption_tiers,
            commands::get_coin_promo_menu,
            commands::redeem_points,
            commands::spin_wheel,
            commands::get_wheel_enabled,
            commands::get_wheel_settings,
            commands::logout_account,
            commands::lock_overlay,
            commands::continue_guest_session,
            // Window modes (ADR-0029, plan §5).
            commands::enter_fullscreen_mode,
            commands::enter_widget_mode,
            commands::enter_mini_mode,
            commands::get_shop_name,
            commands::get_theme,
            // Owner PIN gate + Quit (ADR-0027 §2, plan §6).
            commands::verify_owner_pin,
            commands::quit_client,
            commands::set_launch_at_startup,
            commands::set_windows_sign_out,
            // Kiosk hardening (ADR-0028, plan §7).
            commands::set_kiosk_lock_active,
            commands::dev_bypass_screen_restriction,
            commands::dev_seed_tamper_kill_report,
            // Account self-service actions (ADR-0030, plan §8).
            commands::transfer_time,
            commands::request_points_transfer,
            commands::change_account_password
        ])
        .build(tauri::generate_context!());
    diagnostics::startup_milestone("app_built");

    match app {
        Ok(app) => app.run(|app_handle, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                // Intentional Client exits announce their typed reason through
                // shutdown::initiate_exit. The Guard learns about an actual
                // Windows shutdown from SERVICE_CONTROL_SHUTDOWN; this event
                // also covers ordinary app close and must not masquerade as
                // an OS shutdown notice.
                let shared = Arc::clone(&app_handle.state::<AppState>().shared);
                if let Err(error) =
                    tauri::async_runtime::block_on(commands::disconnect_and_notify(&shared))
                {
                    error!(target: "openpiso_client::shutdown", "Could not notify admin server during client shutdown: {error}");
                }
            }
        }),
        Err(error) => error!(target: "openpiso_client::startup", "OpenPiso client failed to start: {error}"),
    }
}

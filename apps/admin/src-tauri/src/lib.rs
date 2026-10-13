mod account_import;
mod account_login_throttle;
mod accounts;
mod app_state;
mod app_update;
#[cfg(feature = "test-harness")]
pub mod attendance_harness;
mod audit;
mod auth;
pub mod autostart;
pub mod backup;
mod backup_scheduler;
mod bundle_machinery;
mod coin_debug_bundle;
mod coin_source;
mod commands;
mod db;
mod diagnostic_request;
mod diagnostics;
mod errors;
mod events;
mod forwarded_log;
mod guard_poll;
mod host_diagnostics;
mod kiosk_lock;
mod login_throttle;
mod models;
mod monitoring;
mod owner;
mod pc_identity;
mod reports;
mod run_record;
mod serial_port_detection;
mod server;
mod sessions;
mod shutdown;
mod support_bundle;
mod telegram;
mod telegram_delivery;
mod telemetry;
mod terminal_selection;
#[cfg(any(test, feature = "test-harness"))]
#[cfg_attr(not(test), allow(dead_code))]
mod test_utils;
mod timers;
mod web_admin;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use app_state::AppState;
use tauri::Manager;
use tracing::error;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Single-instance guard: a second launch focuses the existing kiosk
        // window and exits instead of creating a duplicate process (ADR-0055
        // precondition, GitHub #204).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::default(),
            None::<Vec<&'static str>>,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app.get_webview_window("kiosk").map(|window| {
                let _ = window.set_focus();
            });
        }))
        // Lifecycle logging (ADR-0082): name the reason while the process can
        // still write it, so closing the Kiosk window never reads like a
        // crash.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                diagnostics::log_window_close_requested(window.label());
                // Only the Kiosk window's close begins shutdown — the Run
                // Record must not end while the process lives on (the Kiosk
                // is the app's only window today).
                if window.label() == "kiosk" {
                    complete_current_run(
                        window.app_handle(),
                        run_record::ExitReason::WindowClosed,
                    );
                }
            }
        })
        .setup(|app| {
            let app_started_at = chrono::Utc::now();
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("could not resolve app data directory: {error}"))?;
            fs::create_dir_all(&data_dir)
                .map_err(|error| format!("could not create app data directory: {error}"))?;
            // ADR-0082: hold the non-blocking writer guard in application
            // state for the life of the process instead of forgetting it, so
            // the final buffered lines flush on the way out. If durable
            // logging cannot start, refuse to start, like every other
            // data-directory failure in this closure: a kiosk that runs
            // without diagnostics is blind exactly when evidence matters.
            let guard = diagnostics::init_tracing(&data_dir)
                .map_err(|error| format!("could not initialize diagnostic logging: {error}"))?;
            app.manage(guard);
            // ADR-0082: the Run Record store lives at the data-directory root,
            // outside the rotating log directory, so the retention sweep never
            // removes the history it holds.
            let run_records = std::sync::Arc::new(run_record::RunRecordStore::new(
                data_dir.join(run_record::RUN_RECORDS_FILE_NAME),
                std::sync::Arc::new(run_record::SystemClock),
            ));
            app.manage(run_records.clone());
            diagnostics::install_panic_hook(&data_dir.join("logs"), run_records.clone());
            // Classify the previous run first — a newest record with a start
            // and no end is a suspected hard exit, a plainly stated absence of
            // evidence, never a claim about the cause — then append this run's
            // record. Failures are logged, never fatal: the record holds no
            // money, time, session, or Points data, so losing it costs
            // history, not correctness.
            match run_records.start(env!("CARGO_PKG_VERSION")) {
                Ok(run_record::PreviousRunEnd::SuspectedHardExit) => {
                    tracing::warn!(
                        target: diagnostics::LIFECYCLE_TARGET,
                        "previous run has a start and no end: suspected hard exit; no evidence of the cause"
                    );
                }
                Ok(run_record::PreviousRunEnd::Completed(reason)) => {
                    let named = reason
                        .map(run_record::ExitReason::as_str)
                        .unwrap_or("reason not recorded");
                    tracing::info!(
                        target: diagnostics::LIFECYCLE_TARGET,
                        "previous run ended: {named}"
                    );
                }
                Ok(run_record::PreviousRunEnd::None) => {}
                Err(error) => {
                    tracing::warn!(
                        target: diagnostics::LIFECYCLE_TARGET,
                        "could not write the Admin Run Record: {error}"
                    );
                }
            }
            diagnostics::log_process_start();
            let database_path = data_dir.join("openpiso.db");
            let backups_dir = data_dir.join("backups");
            fs::create_dir_all(&backups_dir)
                .map_err(|error| format!("could not create Backup directory: {error}"))?;
            let applied_restore = tauri::async_runtime::block_on(backup::apply_pending_restore(
                &database_path,
                &backups_dir,
            ))
            .map_err(|error| format!("could not apply Pending Restore: {error}"))?;
            let pool = tauri::async_runtime::block_on(db::connect(&database_path, &backups_dir))
                .map_err(|error| error.to_string())?;
            let offset = db::attendance::host_offset();
            tauri::async_runtime::block_on(db::attendance_earning::prepare_startup_state(
                &pool,
                chrono::Utc::now(),
                offset,
            ))
            .map_err(|error| format!("could not prepare Attendance earning state: {error}"))?;
            tauri::async_runtime::block_on(sessions::SessionStore::recover_open_sessions(&pool))
                .map_err(|error| format!("could not recover Account Session history: {error}"))?;
            if let Some(pending) = applied_restore {
                let mut event = audit::NewAuditEvent::simple(
                    "restore_applied",
                    openpiso_core::audit::AuditOutcome::Success,
                    openpiso_core::audit::AuditActor::Admin,
                    "Pending Restore applied on launch",
                );
                event.metadata = serde_json::json!({
                    "filename": pending.filename,
                    "schema_version": pending.schema_version,
                    "requested_at": pending.requested_at,
                });
                tauri::async_runtime::block_on(audit::append_standalone(&pool, event))
                    .map_err(|error| format!("could not append restore audit event: {error}"))?;
                if let Some(mirror_error) = pending.mirror_error.as_deref() {
                    tauri::async_runtime::block_on(db::backup_schedule::set_mirror_error(
                        &pool,
                        Some(mirror_error),
                    ))
                    .map_err(|error| {
                        format!("could not preserve Backup mirror advisory: {error}")
                    })?;
                }
            }
            let events = events::EventHub::new(app.handle().clone());
            let initial_shop_status = tauri::async_runtime::block_on(db::shop_status(&pool))
                .map_err(|error| format!("could not resolve initial Shop Status: {error}"))?;
            events.set_shop_status(initial_shop_status);
            // M6 / ADR-0024: the desktop owner console is retired, so the
            // `OwnerTokens` handle is owned only by the `:8080` web-admin
            // server. The Tauri webview is now exclusively the customer
            // Central Terminal kiosk; the owner surface lives at
            // `http://<host>:8080/admin`.
            let owner_tokens = auth::OwnerTokens::default();
            let account_tokens = auth::AccountTokens::default();
            let sessions = sessions::SessionStore::default();
            // M2 wires the configured coin source — simulator by default per
            // the `dev-simulator` seed (AGENTS.md), or the real serial
            // source once M4/M6 settings are calibrated on-site. The set
            // pin starts inhibited (fail-closed at construction) and
            // re-inhibits on every close path. Constructed here so a
            // failure surfaces during setup rather than at first command
            // invocation.
            let (coin_audit_sender, coin_audit_receiver) = tokio::sync::mpsc::unbounded_channel();
            let coin_device_settings =
                tauri::async_runtime::block_on(db::coin_device_settings(&pool))
                    .map_err(|error| format!("could not load coin-device settings: {error}"))?;
            let coin_source_config = db::coin_source_config_from_settings(&coin_device_settings)
                .map_err(|error| format!("could not validate coin-device settings: {error}"))?;
            let selection_config = db::selection_config_from_settings(&coin_device_settings)
                .map_err(|error| format!("could not validate selection timers: {error}"))?;
            let terminal_source =
                coin_source::ConfiguredCoinSource::from_settings_with_audit_sender(
                    coin_source_config,
                    &coin_device_settings,
                    Some(coin_audit_sender.clone()),
                )
                .map_err(|error| format!("could not build terminal selection source: {error}"))?;
            let terminal_selection = Arc::new(
                terminal_selection::TerminalSelection::new_with_coin_audit_sender(
                    terminal_source,
                    selection_config,
                    pool.clone(),
                    sessions.clone(),
                    events.clone(),
                    Some(coin_audit_sender),
                )
                .map_err(|error| format!("could not initialise terminal selection: {error}"))?,
            );
            let log_dir = data_dir.join("logs");
            // Forwarded Diagnostics (ADR-0078) share the log directory with
            // Admin's own diagnostics but never its file.
            let forwarded_log = Arc::new(forwarded_log::ForwardedLogSink::new(log_dir.clone()));
            // ADR-0056: the kiosk boots pinned above every other window.
            // `tauri.conf.json` already declares `alwaysOnTop`, so the window
            // is born on top with no flicker; this re-assertion is what keeps
            // the config and the in-memory `KioskLock` from drifting if either
            // side is changed alone. A failure is logged, not fatal — an
            // unpinned kiosk is still a usable one — but the lock is then
            // seeded *unlocked*, so the header never claims a pin the OS
            // refused to apply.
            let kiosk_pinned = match commands::set_kiosk_always_on_top(app.handle(), true) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        target: "openpiso_admin_lib::kiosk_lock",
                        "could not assert kiosk always-on-top at launch: {error}"
                    );
                    false
                }
            };
            app.manage(AppState {
                pool: pool.clone(),
                sessions: sessions.clone(),
                events: events.clone(),
                terminal_selection: terminal_selection.clone(),
                kiosk_lock: kiosk_lock::KioskLock::new(kiosk_pinned),
            });

            let account_login_throttle = account_login_throttle::AccountLoginThrottle::default();
            // Guard Poll state (ADR-0077) is shared by exactly two parties: the
            // `:3210` handler that records facts, and the monitoring loop that
            // is the only writer of power-off orders.
            let guards = guard_poll::GuardRegistry::new();
            // Diagnostic Request state (ADR-0078) is shared the same way: the
            // `:3210` Heartbeat delivers the armed nonce, the Forwarded
            // Diagnostics endpoint consumes it, and the `:8080` owner surface
            // arms it.
            let diagnostic_requests = diagnostic_request::DiagnosticRequestRegistry::new();
            let server_pool = pool.clone();
            let server_events = events.clone();
            let server_sessions = sessions.clone();
            let server_account_tokens = account_tokens.clone();
            let monitoring_account_tokens = account_tokens.clone();
            let server_account_login_throttle = account_login_throttle.clone();
            let server_guards = guards.clone();
            let server_diagnostic_requests = diagnostic_requests.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = server::serve(
                    server_pool,
                    server_events,
                    server_account_tokens,
                    server_sessions,
                    server_account_login_throttle,
                    server_guards,
                    forwarded_log,
                    server_diagnostic_requests,
                )
                .await
                {
                    error!("OpenPiso local server stopped: {error}");
                }
            });

            // M5: resolve the web-admin static assets directory. In development
            // the path is relative to the crate; in Tauri production builds the
            // files are bundled via `tauri.conf.json` resources (M6).
            let dev_web_admin_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("web-admin")
                .join("dist");
            let resource_dir = app
                .path()
                .resource_dir()
                .map_err(|error| format!("could not resolve resource directory: {error}"))?;
            let web_admin_dir = resolve_web_admin_dir(&dev_web_admin_dir, &resource_dir);

            // M4: the owner HTTP API on `:8080` (ADR-0024), separate from the
            // `:3210` client/device API above. Owner endpoints are never
            // mounted on `:3210`; PIN auth is enforced server-side here.
            let autostart: std::sync::Arc<dyn autostart::AutostartPort> =
                match autostart::PluginAutostart::new() {
                    Ok(plugin) => std::sync::Arc::new(plugin),
                    Err(error) => {
                        tracing::warn!(
                            target: "openpiso_admin_lib::autostart",
                            "Autostart plugin initialization failed, using fallback: {error}"
                        );
                        std::sync::Arc::new(crate::autostart::FakeAutostart::new(
                            false,
                            std::env::current_exe()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| "unknown".to_owned()),
                        ))
                    }
                };

            let telegram_transport = telegram::production_transport()?;

            let telemetry_transport = telemetry::production_config().and_then(|config| {
                match telemetry::HttpTelemetryTransport::new(config.host) {
                    Ok(transport) => Some((config.token, transport)),
                    Err(error) => {
                        tracing::error!(target: "openpiso_admin_lib::telemetry", "Telemetry transport unavailable: {error}");
                        None
                    }
                }
            });

            let owner_state = web_admin::OwnerServerState {
                pool: pool.clone(),
                events: events.clone(),
                owner_tokens,
                account_tokens,
                sessions: sessions.clone(),
                autostart,
                release_feed: Arc::new(app_update::GitHubReleaseFeed::new()?),
                installer: Arc::new(app_update::TauriInstaller::new(app.handle().clone())),
                app_update: Arc::new(app_update::UpdateService::default()),
                log_dir,
                backups_dir: backups_dir.clone(),
                app_started_at,
                debug_window_start: std::sync::Arc::new(std::sync::RwLock::new(app_started_at)),
                terminal_selection: terminal_selection.clone(),
                web_admin_dir,
                login_throttle: Default::default(),
                account_login_throttle,
                host_probe: monitoring::production_probe(),
                telegram: Arc::new(telegram::TelegramService::new(
                    pool.clone(),
                    telegram_transport.clone(),
                    telegram::production_protector(),
                )),
                // Shared with the client API: the Support Bundle Verdict
                // reads the same Guard Poll facts (ADR-0077).
                guards: guards.clone(),
                // Shared with the client API: the owner arm places the ask the
                // Heartbeat delivers and the slice answer consumes
                // (ADR-0078).
                diagnostic_requests,
                // ADR-0082: the host-diagnostics endpoint and the Support
                // Bundle read the same Run Record store, so the owner's
                // screen and the bundle cannot disagree.
                run_records: run_records.clone(),
            };
            tauri::async_runtime::block_on(owner_state.app_update.startup(&pool, &run_records))?;
            tauri::async_runtime::spawn(app_update::run(
                pool.clone(), owner_state.app_update.clone(), owner_state.release_feed.clone(),
            ));
            tauri::async_runtime::spawn(async move {
                if let Err(error) = web_admin::serve(owner_state).await {
                    error!("OpenPiso owner server stopped: {error}");
                }
            });
            let monitoring_sessions = sessions.clone();
            tauri::async_runtime::spawn(timers::run(pool.clone(), events.clone(), sessions));
            let app_pool = pool.clone();
            tauri::async_runtime::spawn(run_coin_source_audit_consumer(
                pool.clone(),
                coin_audit_receiver,
            ));
            // Scheduled Backups deliberately have their own 30-second task;
            // Backup file I/O must never delay the one-second money/time timer.
            tauri::async_runtime::spawn(backup_scheduler::run(pool.clone(), backups_dir));

            // PC Monitoring (ADR-0076): its own ten-second probe cadence on a
            // dedicated task, so mismatch detection can never delay authority
            // writes. Startup marks downtime-spanning incidents interrupted.
            monitoring::MonitoringService::new(
                app_pool,
                monitoring::production_probe(),
                Arc::new(monitoring::SystemClock),
            )
            // Client Recovery Escalation (ADR-0077) rides the same loop. It
            // stays inert until the owner enables it with the Owner PIN.
            .with_escalation(monitoring::EscalationDeps {
                sessions: monitoring_sessions,
                tokens: monitoring_account_tokens,
                guards: guards.clone(),
            })
            .spawn();

            // Telegram incident delivery (#346): its own ten-second outbox
            // sweep on a dedicated task, outside the money and time authority
            // timer. Sends only committed outbox rows; local history stays
            // canonical whether or not Telegram is reachable.
            telegram_delivery::DeliveryService::new(
                pool.clone(),
                telegram_transport,
                telegram::production_protector(),
                Arc::new(telegram_delivery::SystemClock),
            )
            .spawn();

            // Telemetry: its own outbox sweep outside Heartbeats,
            // timers, coins, sessions, and startup authority. PostHog
            // responses never change local behaviour or block shutdown.
            if let Some((token, transport)) = telemetry_transport {
                telemetry::TelemetryService::new(
                    pool.clone(),
                    Arc::new(transport),
                    Arc::new(telemetry::SystemClock),
                    token.to_owned(),
                )
                .spawn();
            }

            // Terminal selection poll loop: a 500ms tick that drives both
            // the inactivity/hard-cap auto-close and the coin-source read.
            // The selected-PC commit path is the same `record_coin_event`
            // already used by the simulator command (ADR-0008 unchanged).
            tauri::async_runtime::spawn(run_terminal_selection_poll(
                terminal_selection.clone(),
                events.clone(),
            ));
            // Publish the initial kiosk status so the M3 window's first
            // paint matches the real coin-source state without waiting on
            // a poll. The M2 simulator is always `available`; the M4 swap
            // is the only producer of `out_of_order` here.
            tauri::async_runtime::spawn(publish_initial_kiosk_status(terminal_selection, events));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_selection,
            commands::close_selection,
            commands::get_selection,
            commands::list_pcs_kiosk,
            commands::get_kiosk_status,
            commands::get_kiosk_config,
            commands::get_theme,
            commands::get_shop_name,
            commands::get_shop_status,
            commands::get_kiosk_lock,
            commands::set_kiosk_lock,
            // DEV-ONLY: the coin-insert simulator command exists only in
            // debug builds, so the release kiosk surface stays the M6 set.
            #[cfg(debug_assertions)]
            commands::dev_inject_coin
        ])
        .build(tauri::generate_context!());

    match app {
        Ok(app) => app.run(|app_handle, event| {
            // Lifecycle logging (ADR-0082): an application-initiated exit
            // names its reason before the process goes.
            if let tauri::RunEvent::ExitRequested { code, .. } = event {
                diagnostics::log_application_exit_requested(code);
                complete_current_run(app_handle, run_record::ExitReason::ApplicationExit);
            }
        }),
        Err(error) => error!("OpenPiso failed to start: {error}"),
    }
}

/// Completes this run's Run Record on a shutdown-path event (ADR-0082). The
/// completion is idempotent in the store, so the window-close completion
/// wins over the later exit request for the same run. Failures are logged
/// and swallowed: the record holds history, not authority.
fn complete_current_run(app: &tauri::AppHandle, reason: run_record::ExitReason) {
    if let Some(records) = app.try_state::<std::sync::Arc<run_record::RunRecordStore>>() {
        if let Err(error) = records.complete(reason) {
            tracing::warn!(
                target: diagnostics::LIFECYCLE_TARGET,
                "could not complete the Admin Run Record: {error}"
            );
        }
    }
}

/// Writes Coin Source availability and sampling messages to the Audit Log.
/// The sampler thread owns no database handle, so this task is the only
/// writer. Errors are logged and swallowed: an audit failure must not kill
/// the bridge.
async fn run_coin_source_audit_consumer(
    pool: sqlx::SqlitePool,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<coin_source::CoinSourceAuditMessage>,
) {
    while let Some(message) = receiver.recv().await {
        let event = match message {
            coin_source::CoinSourceAuditMessage::Availability { available, reason } => {
                let mut event = audit::NewAuditEvent::simple(
                    if available {
                        "coin_device_recovered"
                    } else {
                        "coin_device_unavailable"
                    },
                    if available {
                        openpiso_core::audit::AuditOutcome::Success
                    } else {
                        openpiso_core::audit::AuditOutcome::Failure
                    },
                    openpiso_core::audit::AuditActor::System,
                    if available {
                        "Coin acceptor recovered"
                    } else {
                        "Coin acceptor became unavailable"
                    },
                );
                event.metadata = serde_json::json!({ "reason": reason });
                event
            }
            coin_source::CoinSourceAuditMessage::SamplingAnomaly { reason, count } => {
                let mut event = audit::NewAuditEvent::simple(
                    "coin_sampling_anomaly",
                    openpiso_core::audit::AuditOutcome::Failure,
                    openpiso_core::audit::AuditActor::System,
                    coin_source::sampling_anomaly_summary(&reason, count),
                );
                event.metadata = serde_json::json!({ "reason": reason, "count": count });
                event
            }
        };
        if let Err(error) = audit::append_standalone(&pool, event).await {
            tracing::warn!(
                target: "openpiso_admin_lib::coin_source",
                "Coin Source audit append failed: {error}"
            );
        }
    }
}

/// Background tick for the Central Terminal Selection. Drains accepted
/// coins and applies inactivity/hard-cap timeouts so the kiosk UI does not
/// have to drive the loop itself. Errors are logged and swallowed: a
/// transient DB failure must not kill the poll task.
async fn run_terminal_selection_poll(
    selection: Arc<terminal_selection::TerminalSelection<coin_source::ConfiguredCoinSource>>,
    events: events::EventHub,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    // The first tick fires immediately; skip it so we don't race the
    // initial `open_selection` and over-eagerly call `tick` on an empty
    // selection.
    interval.tick().await;
    let mut last_available: Option<bool> = None;
    loop {
        interval.tick().await;
        if let Err(error) = selection.poll_calibration().await {
            tracing::warn!(
                target: "openpiso_admin_lib::terminal_selection",
                "calibration poll failed: {error}"
            );
        }
        match selection.retry_closing_drain().await {
            Ok(true) => {
                tracing::info!(
                    target: "openpiso_admin_lib::terminal_selection",
                    "Selection close finished after retrying in-flight drain"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    target: "openpiso_admin_lib::terminal_selection",
                    "selection close drain retry failed: {error}"
                );
            }
        }
        if let Some(reason) = selection.tick().await {
            tracing::info!(
                target: "openpiso_admin_lib::terminal_selection",
                "Selection window auto-closed ({})",
                reason.as_str()
            );
        }
        if let Err(error) = selection.try_accept_next_batch().await {
            tracing::warn!(
                target: "openpiso_admin_lib::terminal_selection",
                "accept-next-batch failed: {error}"
            );
        }
        // Watch the source for a change in availability so the M3 kiosk
        // can switch to the "out of order" screen (M4) or back to
        // "available" once the COM port recovers. The simulator never
        // flips, so this is a one-shot in production today.
        let available = selection.is_source_available().await;
        if last_available != Some(available) {
            last_available = Some(available);
            publish_kiosk_status(&selection, &events).await;
        }
    }
}

/// Publish the kiosk status once at startup so the M3 window does not
/// have to wait for the first poll cycle to learn the source's state.
/// Errors are logged and swallowed: a transient emit failure must not
/// block the rest of setup.
async fn publish_initial_kiosk_status(
    selection: Arc<terminal_selection::TerminalSelection<coin_source::ConfiguredCoinSource>>,
    events: events::EventHub,
) {
    publish_kiosk_status(&selection, &events).await;
}

fn resolve_web_admin_dir(dev_dir: &Path, resource_dir: &Path) -> PathBuf {
    if dev_dir.exists() {
        dev_dir.to_owned()
    } else {
        resource_dir.join("web-admin").join("dist")
    }
}

async fn publish_kiosk_status(
    selection: &Arc<terminal_selection::TerminalSelection<coin_source::ConfiguredCoinSource>>,
    events: &events::EventHub,
) {
    let status = if selection.is_source_available().await {
        models::KioskStatus {
            kind: models::KioskStatusKind::Available,
            reason: String::new(),
        }
    } else {
        models::KioskStatus {
            kind: models::KioskStatusKind::OutOfOrder,
            reason: "Coin acceptor is unavailable".to_owned(),
        }
    };
    events.publish_kiosk_status(status);
}

#[cfg(test)]
mod packaged_web_admin_tests {
    use super::*;

    #[test]
    fn release_uses_bundled_web_admin_shell() {
        let missing_dev_dir = Path::new("__openpiso_missing_web_admin_dist__");
        let resource_dir = Path::new("resources");

        assert_eq!(
            resolve_web_admin_dir(missing_dev_dir, resource_dir),
            resource_dir.join("web-admin").join("dist")
        );

        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid Tauri config");
        assert_eq!(
            config["bundle"]["resources"]["../../web-admin/dist/"],
            "web-admin/dist/"
        );
    }
}

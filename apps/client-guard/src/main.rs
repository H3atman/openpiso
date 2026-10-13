//! OpenPiso Client Guard — Windows SYSTEM service (ADR-0048).
//!
//! The Guard runs as `OpenPisoClientGuard` under SYSTEM. It hosts a named
//! pipe that the Client holds open for its lifetime; a dropped pipe without
//! a clean-exit message is a Tamper Kill that triggers relaunch.
//!
//! Uses raw Win32 FFI to avoid dependency on the `windows` crate's shifting
//! API surface.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

mod arming;
mod classification;
mod diagnostics;
mod endpoint;
mod forwarding;
mod pipe;
mod poll;
mod power;
mod relaunch;
mod relauncher;
mod shared;

use shared::GuardShared;

static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
/// When Windows last said the session is ending (pre-shutdown or session
/// logoff), as milliseconds on [`guard_clock`] plus one; zero means no notice.
/// Only the Service Control Manager writes it, so a customer cannot forge it.
static END_SESSION_NOTICE_AT: AtomicU64 = AtomicU64::new(0);
static GUARD_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Monotonic time since the Guard started, shared by the control handler and
/// the loop so a notice and a disconnect can be ordered.
fn guard_clock() -> Duration {
    GUARD_EPOCH.get_or_init(Instant::now).elapsed()
}

#[cfg(windows)]
fn record_end_session_notice() {
    let millis = u64::try_from(guard_clock().as_millis()).unwrap_or(u64::MAX - 1);
    END_SESSION_NOTICE_AT.store(millis.saturating_add(1), Ordering::SeqCst);
}

fn end_session_notice_at() -> Option<Duration> {
    match END_SESSION_NOTICE_AT.load(Ordering::SeqCst) {
        0 => None,
        stored => Some(Duration::from_millis(stored - 1)),
    }
}

fn clear_end_session_notice() {
    END_SESSION_NOTICE_AT.store(0, Ordering::SeqCst);
}

pub(crate) fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn main() {
    // Keep the non-blocking writer guard alive until service exit so queued
    // events flush to the machine-wide diagnostic log.
    let _log_guard = match diagnostics::init_tracing() {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("Client Guard durable logging unavailable: {error}");
            if let Err(fallback_error) = tracing_subscriber::fmt()
                .with_target(true)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .try_init()
            {
                eprintln!("Client Guard stderr logging unavailable: {fallback_error}");
            }
            None
        }
    };

    info!(target: "openpiso_client_guard", "Client Guard starting");

    #[cfg(windows)]
    run_service();

    #[cfg(not(windows))]
    run_guard_loop();
}

// ── Raw Win32 FFI for SCM (advapi32) ─────────────────────────────────

#[cfg(windows)]
mod service_ffi {
    #![allow(nonstandard_style, dead_code)]

    use std::ffi::c_void;

    #[allow(clippy::upper_case_acronyms)]
    pub type HANDLE = isize;
    #[allow(clippy::upper_case_acronyms)]
    pub type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    pub type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    pub type LPCWSTR = *const u16;
    #[allow(clippy::upper_case_acronyms)]
    pub type LPWSTR = *mut u16;
    #[allow(clippy::upper_case_acronyms)]
    pub type LPVOID = *mut c_void;
    pub const FALSE: BOOL = 0;
    pub const NO_ERROR: DWORD = 0;
    pub const ERROR_CALL_NOT_IMPLEMENTED: DWORD = 120;
    pub const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: DWORD = 1063;

    // Service types
    pub const SERVICE_WIN32_OWN_PROCESS: DWORD = 0x00000010;
    pub const SERVICE_RUNNING: DWORD = 0x00000004;
    pub const SERVICE_STOPPED: DWORD = 0x00000001;
    pub const SERVICE_ACCEPT_STOP: DWORD = 0x00000001;
    pub const SERVICE_ACCEPT_SESSIONCHANGE: DWORD = 0x00000080;
    pub const SERVICE_ACCEPT_PRESHUTDOWN: DWORD = 0x00000100;
    pub const SERVICE_CONTROL_STOP: DWORD = 0x00000001;
    pub const SERVICE_CONTROL_SESSIONCHANGE: DWORD = 0x0000000E;
    pub const SERVICE_CONTROL_PRESHUTDOWN: DWORD = 0x0000000F;
    pub const WTS_SESSION_LOGOFF: DWORD = 0x6;

    #[repr(C)]
    pub struct SERVICE_TABLE_ENTRYW {
        pub lpServiceName: LPWSTR,
        pub lpServiceProc: Option<unsafe extern "system" fn(DWORD, *mut LPWSTR)>,
    }

    #[repr(C)]
    pub struct SERVICE_STATUS {
        pub dwServiceType: DWORD,
        pub dwCurrentState: DWORD,
        pub dwControlsAccepted: DWORD,
        pub dwWin32ExitCode: DWORD,
        pub dwServiceSpecificExitCode: DWORD,
        pub dwCheckPoint: DWORD,
        pub dwWaitHint: DWORD,
    }

    pub type SERVICE_STATUS_HANDLE = HANDLE;

    const _: () = {
        assert!(std::mem::size_of::<SERVICE_STATUS>() == 28);
        assert!(
            std::mem::size_of::<SERVICE_TABLE_ENTRYW>()
                == if cfg!(target_pointer_width = "64") {
                    16
                } else {
                    8
                }
        );
    };

    #[link(name = "advapi32")]
    unsafe extern "system" {
        pub fn StartServiceCtrlDispatcherW(
            lpServiceStartTable: *const SERVICE_TABLE_ENTRYW,
        ) -> BOOL;

        pub fn RegisterServiceCtrlHandlerExW(
            lpServiceName: LPCWSTR,
            lpHandlerProc: Option<unsafe extern "system" fn(DWORD, DWORD, LPVOID, LPVOID) -> DWORD>,
            lpContext: LPVOID,
        ) -> SERVICE_STATUS_HANDLE;

        pub fn SetServiceStatus(
            hServiceStatus: SERVICE_STATUS_HANDLE,
            lpServiceStatus: *mut SERVICE_STATUS,
        ) -> BOOL;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetLastError() -> DWORD;
    }
}

#[cfg(windows)]
const fn should_run_standalone(dispatch_error: u32) -> bool {
    dispatch_error == service_ffi::ERROR_FAILED_SERVICE_CONTROLLER_CONNECT
}

#[cfg(windows)]
fn run_service() {
    use service_ffi::*;

    let dispatch_table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: to_wide_leaked("OpenPisoClientGuard"),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];

    unsafe {
        let result = StartServiceCtrlDispatcherW(dispatch_table.as_ptr());
        if result == FALSE {
            let error = GetLastError();
            if should_run_standalone(error) {
                warn!(target: "openpiso_client_guard", error, "not started by the Service Control Manager; running standalone");
                run_guard_loop();
            } else {
                error!(target: "openpiso_client_guard", error, "StartServiceCtrlDispatcherW failed");
            }
        }
    }
}

/// Convert a string to a boxed, leaked wide pointer (never freed, process lifetime).
#[cfg(windows)]
fn to_wide_leaked(s: &str) -> *mut u16 {
    let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    Box::leak(v.into_boxed_slice()).as_mut_ptr()
}

#[cfg(windows)]
extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    use service_ffi::*;

    unsafe extern "system" fn control_handler(
        dw_control: DWORD,
        dw_event_type: DWORD,
        _lp_event_data: LPVOID,
        _lp_context: LPVOID,
    ) -> DWORD {
        match dw_control {
            SERVICE_CONTROL_STOP => {
                info!(target: "openpiso_client_guard", "control: stop");
                SHOULD_STOP.store(true, Ordering::SeqCst);
                NO_ERROR
            }
            SERVICE_CONTROL_PRESHUTDOWN => {
                info!(target: "openpiso_client_guard", "control: pre-shutdown");
                // Windows usually ends the Client before this arrives; the
                // loop's end-session grace waits for it before classifying.
                record_end_session_notice();
                SHOULD_STOP.store(true, Ordering::SeqCst);
                NO_ERROR
            }
            SERVICE_CONTROL_SESSIONCHANGE => {
                if dw_event_type == WTS_SESSION_LOGOFF {
                    info!(target: "openpiso_client_guard", "control: session logoff");
                    // A sign-out sends no shutdown control. The Guard keeps
                    // running so it guards the next logon.
                    record_end_session_notice();
                }
                NO_ERROR
            }
            _ => ERROR_CALL_NOT_IMPLEMENTED,
        }
    }

    let name = to_wide_leaked("OpenPisoClientGuard");

    let status_handle =
        unsafe { RegisterServiceCtrlHandlerExW(name, Some(control_handler), std::ptr::null_mut()) };

    if status_handle == 0 {
        let error = unsafe { GetLastError() };
        error!(target: "openpiso_client_guard", error, "RegisterServiceCtrlHandlerExW failed");
        return;
    }

    let mut status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_RUNNING,
        // Pre-shutdown replaces shutdown: a service cannot accept both.
        dwControlsAccepted: SERVICE_ACCEPT_STOP
            | SERVICE_ACCEPT_PRESHUTDOWN
            | SERVICE_ACCEPT_SESSIONCHANGE,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };

    if unsafe { SetServiceStatus(status_handle, &mut status) } == FALSE {
        let error = unsafe { GetLastError() };
        error!(target: "openpiso_client_guard", error, "SetServiceStatus(SERVICE_RUNNING) failed");
        return;
    }

    info!(target: "openpiso_client_guard", "service_main entered");

    run_guard_loop();

    status.dwCurrentState = SERVICE_STOPPED;
    if unsafe { SetServiceStatus(status_handle, &mut status) } == FALSE {
        let error = unsafe { GetLastError() };
        error!(target: "openpiso_client_guard", error, "SetServiceStatus(SERVICE_STOPPED) failed");
    }

    info!(target: "openpiso_client_guard", "service_main exiting");
}

fn tracked_process_state(
    process: &mut Option<relauncher::ClientProcess>,
) -> (relaunch::TrackedProcessState, Option<String>) {
    let status = process.as_ref().map(relauncher::ClientProcess::is_running);
    match status {
        None => (relaunch::TrackedProcessState::Absent, None),
        Some(Ok(true)) => (relaunch::TrackedProcessState::Running, None),
        Some(Ok(false)) => {
            *process = None;
            (relaunch::TrackedProcessState::Exited, None)
        }
        Some(Err(error)) => (
            relaunch::TrackedProcessState::StatusUnavailable,
            Some(error),
        ),
    }
}

/// How often the Guard polls Admin. Matches the PC Monitoring cadence, so a
/// fact is never older than one probe round when the Admin reads it.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// One poll round against the cached endpoint (ADR-0077): the Guard's facts
/// and, riding the same request, at most one bounded batch of Forwarded
/// Diagnostics (ADR-0078). The Guard gains no second network call.
///
/// `None` means "no cached endpoint, or no usable answer" — the PC stays on,
/// nothing is logged about forwarding, and a carried batch is not
/// acknowledged, so its lines wait for the next poll.
fn poll_once(shared: &GuardShared) -> Option<poll::GuardState> {
    let (url, mac) = shared.poll_target()?;
    let batch = forwarding::take_batch();
    let Some(state) = poll::poll(
        &url,
        &mac,
        shared.report(),
        batch.as_ref().map(|b| b.lines()),
    ) else {
        forwarding::release(&batch);
        return None;
    };
    forwarding::acknowledge(&batch);
    Some(state)
}

/// Report to Admin and act on what comes back (ADR-0077).
///
/// Runs on its own thread so no network delay can hold up a relaunch, and
/// fails silent by design: with no endpoint, no Admin, or no answer, the PC
/// simply stays on.
fn spawn_poll_thread(shared: Arc<GuardShared>) {
    let spawned = std::thread::Builder::new()
        .name("openpiso-guard-poll".to_owned())
        .spawn(move || loop {
            if SHOULD_STOP.load(Ordering::Relaxed) {
                info!(target: "openpiso_client_guard::poll", "poll thread stopping");
                return;
            }
            std::thread::sleep(POLL_INTERVAL);

            let Some(state) = poll_once(&shared) else {
                continue;
            };
            if !power::should_power_off(
                state.power_off,
                state.nonce.as_deref(),
                shared
                    .endpoint()
                    .and_then(|cached| cached.acted_nonce)
                    .as_deref(),
            ) {
                continue;
            }
            let Some(nonce) = state.nonce else {
                continue;
            };

            // Record the action before taking it. A power off that succeeds
            // is the last thing this process does, so a nonce written
            // afterwards would never be written at all, and the same order
            // would be acted on again after the reboot.
            if let Some(updated) = shared.record_acted_nonce(&nonce) {
                if let Err(error) = endpoint::save(&updated) {
                    error!(
                        target: "openpiso_client_guard::poll",
                        error = %error,
                        "could not record the power-off nonce; refusing to act"
                    );
                    continue;
                }
            }
            warn!(
                target: "openpiso_client_guard::poll",
                %nonce,
                enforced = power::POWER_OFF_ENFORCED,
                "Admin ordered an Escalated Power Off"
            );
            if let Err(error) = power::power_off() {
                error!(
                    target: "openpiso_client_guard::poll",
                    error = %error,
                    "Escalated Power Off failed"
                );
            }
        });
    if let Err(error) = spawned {
        error!(
            target: "openpiso_client_guard::poll",
            %error,
            "could not start the poll thread; local relaunch continues"
        );
    }
}

/// The main guard orchestration loop.
///
/// The pipe listener emits a disconnect event as soon as ReadFile observes
/// EOF/broken-pipe. The timeout below is only a service-stop/backoff wake-up;
/// connected Client liveness is never discovered by polling a process or flag.
fn run_guard_loop() {
    use std::sync::mpsc;

    let state = Arc::new(pipe::GuardState::new());
    let (events_tx, events_rx) = mpsc::channel();

    info!(target: "openpiso_client_guard", "guard loop starting");

    let pipe_state = Arc::clone(&state);
    if let Err(error) = std::thread::Builder::new()
        .name("openpiso-guard-pipe".to_owned())
        .spawn(move || {
            pipe::listen(&pipe_state, &events_tx);
        })
    {
        error!(target: "openpiso_client_guard", %error, "could not start pipe listener");
        return;
    }

    // Client Recovery Escalation (ADR-0077): the facts this loop observes,
    // and the endpoint the Client will report. Both halves are optional at
    // startup — a Guard with no cached endpoint still relaunches Clients.
    let shared = Arc::new(GuardShared::new(endpoint::load()));
    spawn_poll_thread(Arc::clone(&shared));

    let mut arm_state = arming::ArmState::initial();
    let mut last_level_check = Instant::now();
    let mut consecutive_deaths = 0_u32;
    let mut coalesce_window_start: Option<u64> = None;
    let mut next_relaunch: Option<Instant> = None;
    let mut pending_relaunch: Option<(i64, i64)> = None;
    let mut relaunched_client: Option<relauncher::ClientProcess> = None;
    let mut relaunch_retries = 0_u32;

    loop {
        if SHOULD_STOP.load(Ordering::Relaxed) {
            state.should_stop.store(true, Ordering::Relaxed);
            info!(target: "openpiso_client_guard", "guard loop stopping");
            break;
        }

        let wait = next_relaunch
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(1));

        match events_rx.recv_timeout(wait) {
            Ok(pipe::PipeEvent::ClientConnected { process_id }) => {
                // A fresh Client re-arms the Guard after a PIN-gated owner
                // quit and cancels any pending retry.
                arm_state = arming::transition(arm_state, arming::ArmEvent::ClientStarted);
                // Only this exact relaunched process clears its handle. A
                // different valid Client may connect while the tracked child
                // is still alive but unconnected; retaining the handle keeps
                // its later disconnect from reopening duplicate launches.
                if relaunched_client
                    .as_ref()
                    .is_some_and(|process| process.process_id() == process_id)
                {
                    relaunched_client = None;
                }
                // A process that connects and immediately crashes remains in
                // the current coalescing window and receives backoff.
                next_relaunch = None;
                pending_relaunch = None;
                relaunch_retries = 0;
                clear_end_session_notice();
                shared.set_client_connected(true);
                shared.set_disarmed(false);
                info!(target: "openpiso_client_guard", "Client liveness established");
            }
            Ok(pipe::PipeEvent::AdminEndpoint { url, mac }) => {
                // Cached so the poll survives the Client that reported it,
                // including across a Guard restart on a PC whose Client never
                // starts again.
                if let Some(updated) = shared.learn_endpoint(&url, &mac) {
                    if let Err(error) = endpoint::save(&updated) {
                        warn!(
                            target: "openpiso_client_guard",
                            error = %error,
                            "could not cache the Admin endpoint; polling continues from memory"
                        );
                    }
                }
            }
            Ok(pipe::PipeEvent::ClientDisconnected { message }) => {
                shared.set_client_connected(false);
                // Stamp the death before any grace wait so attribution uses
                // the instant the pipe dropped, not when it was classified.
                let kill_timestamp = unix_time_millis();
                let disconnected_at = guard_clock();
                let (classification, excused_by_grace) = classification::classify_disconnect(
                    message,
                    || classification::notice_is_current(end_session_notice_at(), disconnected_at),
                    Instant::now,
                    std::thread::sleep,
                );
                if excused_by_grace {
                    info!(
                        target: "openpiso_client_guard",
                        "Windows end-session notice (see the preceding control line) arrived within the grace; not a Tamper Kill"
                    );
                }
                clear_end_session_notice();
                arm_state = arming::transition(
                    arm_state,
                    arming::ArmEvent::ClientDisconnected(classification),
                );
                shared.set_disarmed(arm_state == arming::ArmState::StoodDown);

                if !arming::should_relaunch(arm_state, classification) {
                    coalesce_window_start = None;
                    consecutive_deaths = 0;
                    next_relaunch = None;
                    pending_relaunch = None;
                    relaunch_retries = 0;
                    info!(
                        target: "openpiso_client_guard",
                        ?classification,
                        ?arm_state,
                        "Client exit accepted"
                    );
                    continue;
                }

                // Anything unexplained, including a crash, is intentionally a
                // Tamper Kill. The Guard cannot tell a crash from End task.
                let death_time = u64::try_from(kill_timestamp.max(0)).unwrap_or(0) / 1_000;
                let inside_window = coalesce_window_start
                    .is_some_and(|start| relaunch::inside_coalesce_window(death_time, start));
                if !inside_window {
                    coalesce_window_start = Some(relaunch::start_coalesce_window(death_time));
                    consecutive_deaths = 0;
                }
                consecutive_deaths = consecutive_deaths.saturating_add(1);
                let report = (kill_timestamp, i64::from(consecutive_deaths));
                pending_relaunch = Some(report);
                relaunch_retries = 0;
                let delay = relaunch::next_relaunch_delay(consecutive_deaths);
                next_relaunch = Some(Instant::now() + delay);
                if delay.is_zero() {
                    let (process_state, status_error) =
                        tracked_process_state(&mut relaunched_client);
                    if !relaunch::should_attempt_relaunch(process_state) {
                        if let Some(error) = status_error {
                            error!(
                                target: "openpiso_client_guard",
                                consecutive = consecutive_deaths,
                                error = %error,
                                "could not determine relaunched Client state; suppressing duplicate launch"
                            );
                        } else {
                            warn!(
                                target: "openpiso_client_guard",
                                consecutive = consecutive_deaths,
                                "relaunched Client is still running but has not connected; suppressing duplicate launch"
                            );
                        }
                    } else {
                        if process_state == relaunch::TrackedProcessState::Exited {
                            info!(
                                target: "openpiso_client_guard",
                                consecutive = consecutive_deaths,
                                "relaunched Client exited before connecting"
                            );
                        }
                        info!(
                            target: "openpiso_client_guard",
                            consecutive = consecutive_deaths,
                            "Tamper Kill detected; relaunching Client"
                        );
                        shared.record_relaunch_attempt();
                        match relauncher::relaunch(Some(relauncher::TamperKillArgs {
                            kill_timestamp_unix_ms: report.0,
                            attempt_count: report.1,
                        })) {
                            Ok(process) => relaunched_client = Some(process),
                            Err(error) => {
                                error!(
                                    target: "openpiso_client_guard",
                                    consecutive = consecutive_deaths,
                                    error = %error,
                                    "relaunch failed"
                                );
                            }
                        }
                    }
                    // If the launch failed or the process crashes before
                    // reconnecting, retry forever; a crash-loop has no
                    // terminal give-up state.
                    relaunch_retries = 1;
                    let retry_delay = relaunch::next_relaunch_delay(relaunch_retries + 1);
                    next_relaunch = Some(Instant::now() + retry_delay);
                } else {
                    info!(
                        target: "openpiso_client_guard",
                        consecutive = consecutive_deaths,
                        delay = ?delay,
                        "Tamper Kill detected; relaunch backed off"
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Level-triggered relaunch (ADR-0077). The edge trigger below
                // still owns every death it sees; this only catches the Client
                // that never connected, which produces no disconnect at all.
                if Instant::now().duration_since(last_level_check) >= relaunch::LEVEL_CHECK_INTERVAL
                {
                    last_level_check = Instant::now();
                    let relaunch_in_flight = next_relaunch.is_some() || pending_relaunch.is_some();
                    if relaunch::should_level_relaunch(
                        shared.client_connected(),
                        arm_state == arming::ArmState::Armed,
                        relaunch_in_flight,
                    ) {
                        let (process_state, status_error) =
                            tracked_process_state(&mut relaunched_client);
                        if relaunch::should_attempt_relaunch(process_state) {
                            info!(
                                target: "openpiso_client_guard",
                                "no Client holds the pipe; relaunching on the level check"
                            );
                            shared.record_relaunch_attempt();
                            match relauncher::relaunch(None) {
                                Ok(process) => relaunched_client = Some(process),
                                Err(error) => {
                                    error!(
                                        target: "openpiso_client_guard",
                                        error = %error,
                                        "level-check relaunch failed"
                                    );
                                }
                            }
                        } else if let Some(error) = status_error {
                            error!(
                                target: "openpiso_client_guard",
                                error = %error,
                                "could not determine relaunched Client state on the level check"
                            );
                        }
                    }
                }

                let Some(deadline) = next_relaunch else {
                    continue;
                };
                if Instant::now() < deadline {
                    continue;
                }
                let Some((kill_timestamp, attempt_count)) = pending_relaunch else {
                    continue;
                };

                let (process_state, status_error) = tracked_process_state(&mut relaunched_client);
                if !relaunch::should_attempt_relaunch(process_state) {
                    if let Some(error) = status_error {
                        error!(
                            target: "openpiso_client_guard",
                            consecutive = consecutive_deaths,
                            error = %error,
                            "could not determine relaunched Client state; suppressing duplicate launch"
                        );
                    } else {
                        warn!(
                            target: "openpiso_client_guard",
                            consecutive = consecutive_deaths,
                            "relaunched Client is still running but has not connected; suppressing duplicate launch"
                        );
                    }
                    relaunch_retries = relaunch_retries.saturating_add(1);
                    let retry_delay =
                        relaunch::next_relaunch_delay(relaunch_retries.saturating_add(1));
                    next_relaunch = Some(Instant::now() + retry_delay);
                    continue;
                }
                if process_state == relaunch::TrackedProcessState::Exited {
                    info!(
                        target: "openpiso_client_guard",
                        consecutive = consecutive_deaths,
                        "relaunched Client exited before connecting"
                    );
                }

                info!(
                    target: "openpiso_client_guard",
                    consecutive = consecutive_deaths,
                    "relaunching Client after backoff"
                );
                shared.record_relaunch_attempt();
                match relauncher::relaunch(Some(relauncher::TamperKillArgs {
                    kill_timestamp_unix_ms: kill_timestamp,
                    attempt_count,
                })) {
                    Ok(process) => relaunched_client = Some(process),
                    Err(error) => {
                        error!(
                            target: "openpiso_client_guard",
                            consecutive = consecutive_deaths,
                            error = %error,
                            "relaunch failed"
                        );
                    }
                }
                relaunch_retries = relaunch_retries.saturating_add(1);
                let retry_delay = relaunch::next_relaunch_delay(relaunch_retries.saturating_add(1));
                next_relaunch = Some(Instant::now() + retry_delay);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    info!(target: "openpiso_client_guard", "guard loop exited");
}

#[cfg(test)]
mod poll_round_tests {
    use super::*;
    use openpiso_core::forwarding::{ForwardedDiagnostic, ForwardedLevel, ForwardingBuffer};

    /// Reset the real forwarding buffer this binary's tracing layer and poll
    /// thread share. Call with `forwarding::test_lock()` held; the lock
    /// serialises against the layer tests in `forwarding.rs`, which drive the
    /// same global.
    fn reset_buffer() {
        *forwarding::test_buffer() = ForwardingBuffer::with_default_capacity();
    }

    fn one_line() -> ForwardedDiagnostic {
        ForwardedDiagnostic::new(
            ForwardedLevel::Warn,
            openpiso_core::forwarding::GUARD_PIPE_TARGET,
            "rejecting foreign pipe client (process_id=99)",
            1_760_000_000_000,
        )
    }

    fn endpoint_at(url: String) -> GuardShared {
        GuardShared::new(endpoint::GuardEndpoint {
            admin_url: Some(url),
            mac: Some("aa:bb:cc:dd:ee:ff".to_owned()),
            ..endpoint::GuardEndpoint::default()
        })
    }

    #[test]
    fn a_guard_with_no_cached_endpoint_forwards_nothing() {
        let _serial = forwarding::test_lock();
        reset_buffer();
        forwarding::test_buffer().record(one_line());

        let shared = GuardShared::new(endpoint::GuardEndpoint::default());
        assert_eq!(poll_once(&shared), None, "no endpoint means no poll");
        assert!(
            forwarding::test_buffer().take_batch().is_some(),
            "an unpolled Guard must not have drained its batch"
        );
    }

    #[test]
    fn an_unreachable_endpoint_keeps_the_batch_for_the_next_poll() {
        let _serial = forwarding::test_lock();
        reset_buffer();
        forwarding::test_buffer().record(one_line());

        let shared = endpoint_at("http://127.0.0.1:1".to_owned());
        assert_eq!(poll_once(&shared), None, "an unreachable Admin is no order");
        assert!(
            forwarding::test_buffer().take_batch().is_some(),
            "a failed poll must not acknowledge its batch"
        );
    }

    #[test]
    fn a_two_hundred_answer_carries_the_batch_and_releases_it() {
        let _serial = forwarding::test_lock();
        reset_buffer();
        forwarding::test_buffer().record(one_line());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().expect("accept one poll");
            // The Guard sends the request as one write; read until the
            // declared body length has arrived.
            let mut raw = Vec::new();
            let mut chunk = [0_u8; 8192];
            let request = loop {
                let read = stream.read(&mut chunk).expect("read request");
                raw.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0);
                    if body.len() >= length {
                        break text;
                    }
                }
            };
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 43\r\n\r\n\
                      {\"power_off\":false,\"poll_interval_secs\":10}",
                )
                .expect("write response");
            request
        });

        let shared = endpoint_at(format!("http://127.0.0.1:{port}"));
        let state = poll_once(&shared).expect("a 200 answer");
        assert!(!state.power_off, "stay-on is not an order");

        // The batch rode the one request; the 200 was its acknowledgement.
        let request = server.join().expect("server");
        assert!(request.starts_with("POST /api/guard/state HTTP/1.1\r\n"));
        assert!(request.contains("X-OpenPiso-Mac: aa:bb:cc:dd:ee:ff\r\n"));
        assert!(request.contains(
            "\"forwarded\":{\"lines\":[{\"level\":\"warn\",\"target\":\"openpiso_client_guard::pipe\",\
             \"message\":\"rejecting foreign pipe client (process_id=99)\",\
             \"timestamp_unix_ms\":1760000000000}]}"
        ));
        assert!(
            forwarding::test_buffer().take_batch().is_none(),
            "the ack must have released the lines"
        );
    }
}

#[cfg(all(test, windows))]
mod service_tests {
    use super::*;

    #[test]
    fn standalone_mode_is_only_for_a_non_scm_launch() {
        assert!(should_run_standalone(
            service_ffi::ERROR_FAILED_SERVICE_CONTROLLER_CONNECT
        ));
        assert!(!should_run_standalone(5));
        assert!(!should_run_standalone(0));
    }
}

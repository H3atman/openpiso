//! Client-side Client Guard pipe connection (ADR-0048).
//!
//! The Client connects at startup and holds the write-only pipe handle for its
//! lifetime. It can announce typed intent, but it never classifies its own
//! death; the Guard does that after observing the connection disappear.
//!
//! The connect attempt retries in the background until it succeeds, and never
//! gives up. The Client autostarts from an `HKCU\...\Run` key while the Guard
//! is an `auto` service, so the two race at logon and a `Run` key cannot
//! express a service dependency. Losing that race once would otherwise leave
//! the machine unguarded for the whole session: the Guard's liveness model is
//! edge-triggered on pipe events, so a Client that never connects produces no
//! disconnect, no relaunch, and no further signal.

use openpiso_core::guard::{CleanExitReason, GuardMessage};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\OpenPisoClientGuard";

/// Fast retries cover the logon race and the brief gap while the Guard rebuilds
/// its pipe instance after a relaunch; the slow tail covers a Guard installed,
/// repaired, or started later in the session.
#[cfg_attr(not(windows), allow(dead_code))]
const FAST_RETRY_INTERVAL: Duration = Duration::from_secs(1);
#[cfg_attr(not(windows), allow(dead_code))]
const FAST_RETRY_PHASE: Duration = Duration::from_secs(30);
#[cfg_attr(not(windows), allow(dead_code))]
const SLOW_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Global pipe handle. `None` means the Guard service is absent or unreachable.
static PIPE_HANDLE: Mutex<Option<isize>> = Mutex::new(None);

/// The `(admin URL, MAC)` pair already reported over the current pipe
/// connection (ADR-0077). Cleared whenever the pipe drops, so a reconnected
/// Guard — including a freshly relaunched one — is told again.
static REPORTED_ENDPOINT: Mutex<Option<(String, String)>> = Mutex::new(None);

/// The Guard coverage outcome this build reports with each Heartbeat
/// (ADR-0076 / #344). A release Windows Client reports its real held pipe
/// connection; debug builds and non-Windows platforms can never hold one and
/// report `not_applicable` so development can never create a production
/// protection Alert. Strictly diagnostic: Admin grants it no authority.
pub fn heartbeat_coverage(holds_connection: bool) -> &'static str {
    if cfg!(not(windows)) || cfg!(debug_assertions) {
        "not_applicable"
    } else if holds_connection {
        "covered"
    } else {
        "unavailable"
    }
}

/// What this process reports right now: whether it holds its Guard pipe.
#[cfg(windows)]
pub fn current_heartbeat_coverage() -> &'static str {
    let holds = PIPE_HANDLE
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false);
    heartbeat_coverage(holds)
}

/// Non-Windows builds can never hold a Guard connection.
#[cfg(not(windows))]
pub fn current_heartbeat_coverage() -> &'static str {
    heartbeat_coverage(false)
}

/// How long to wait before the next connect attempt, given time spent so far.
#[cfg_attr(not(windows), allow(dead_code))]
const fn retry_interval(elapsed: Duration) -> Duration {
    if elapsed.as_secs() < FAST_RETRY_PHASE.as_secs() {
        FAST_RETRY_INTERVAL
    } else {
        SLOW_RETRY_INTERVAL
    }
}

/// Start connecting to the Guard's named pipe. Call once at startup; returns
/// immediately and keeps retrying on a background thread until connected.
#[cfg(windows)]
pub fn connect() {
    if let Err(error) = std::thread::Builder::new()
        .name("openpiso-guard-connect".to_owned())
        .spawn(connect_loop)
    {
        tracing::warn!(
            target: "openpiso_client::guard_client",
            %error,
            process_id = crate::diagnostics::process_id(),
            launch = crate::diagnostics::launch_token(),
            "could not start Guard pipe connect thread; trying once inline"
        );
        if let Err(error) = connect_once() {
            tracing::warn!(
                target: "openpiso_client::guard_client",
                %error,
                process_id = crate::diagnostics::process_id(),
                launch = crate::diagnostics::launch_token(),
                "inline Guard pipe connect attempt failed"
            );
        }
    }
}

#[cfg(windows)]
fn connect_loop() {
    let started = std::time::Instant::now();
    let mut warned = false;

    loop {
        match connect_once() {
            Ok(()) => {
                tracing::info!(
                    target: "openpiso_client::guard_client",
                    waited_ms = started.elapsed().as_millis() as u64,
                    process_id = crate::diagnostics::process_id(),
                    launch = crate::diagnostics::launch_token(),
                    "connected to Guard pipe"
                );
                // A new connection is a new Guard instance as far as we can
                // tell, so nothing it was told before still holds.
                forget_reported_endpoint();
                monitor_connected_pipe();
            }
            Err(error) if !warned => {
                warned = true;
                tracing::warn!(
                    target: "openpiso_client::guard_client",
                    %error,
                    process_id = crate::diagnostics::process_id(),
                    launch = crate::diagnostics::launch_token(),
                    "could not connect to Guard pipe (Guard may not be installed); retrying in the background"
                );
            }
            Err(error) => tracing::debug!(
                target: "openpiso_client::guard_client",
                %error,
                "Guard pipe connect retry failed"
            ),
        }

        std::thread::sleep(retry_interval(started.elapsed()));
    }
}

#[cfg(windows)]
fn monitor_connected_pipe() {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let handle = match PIPE_HANDLE.lock() {
            Ok(guard) => match *guard {
                Some(handle) => handle,
                None => return,
            },
            Err(_) => return,
        };
        if !pipe_is_alive(handle) {
            clear_pipe_handle(handle);
            forget_reported_endpoint();
            tracing::warn!(
                target: "openpiso_client::guard_client",
                process_id = crate::diagnostics::process_id(),
                launch = crate::diagnostics::launch_token(),
                "Guard pipe closed; reconnecting"
            );
            return;
        }
    }
}

#[cfg(windows)]
fn pipe_is_alive(handle: isize) -> bool {
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetNamedPipeInfo(
            hNamedPipe: isize,
            lpFlags: *mut DWORD,
            lpOutBufferSize: *mut DWORD,
            lpInBufferSize: *mut DWORD,
            lpMaxInstances: *mut DWORD,
        ) -> BOOL;
    }

    let mut flags = 0;
    let mut out_buffer_size = 0;
    let mut in_buffer_size = 0;
    let mut max_instances = 0;
    unsafe {
        GetNamedPipeInfo(
            handle,
            &mut flags,
            &mut out_buffer_size,
            &mut in_buffer_size,
            &mut max_instances,
        ) != 0
    }
}

#[cfg(windows)]
fn clear_pipe_handle(handle: isize) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CloseHandle(hObject: isize) -> i32;
    }

    if let Ok(mut guard) = PIPE_HANDLE.lock() {
        if *guard == Some(handle) {
            *guard = None;
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// One connect attempt. Returns the Win32 error when `CreateFileW` fails.
#[cfg(windows)]
fn connect_once() -> Result<(), String> {
    #[allow(clippy::upper_case_acronyms)]
    type HANDLE = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;

    const INVALID_HANDLE_VALUE: HANDLE = -1;
    // Named-pipe client write rights: FILE_GENERIC_WRITE's individual rights
    // minus FILE_APPEND_DATA (0x4, aliased to FILE_CREATE_PIPE_INSTANCE), plus
    // FILE_READ_ATTRIBUTES, which CreateFile requires for a pipe client open.
    // This deliberately excludes FILE_READ_DATA.
    const CLIENT_PIPE_WRITE_RIGHTS: DWORD = 0x00120192;
    const OPEN_EXISTING: DWORD = 3;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: DWORD,
            dwShareMode: DWORD,
            lpSecurityAttributes: *const std::ffi::c_void,
            dwCreationDisposition: DWORD,
            dwFlagsAndAttributes: DWORD,
            hTemplateFile: HANDLE,
        ) -> HANDLE;
        fn CloseHandle(hObject: HANDLE) -> BOOL;
        fn GetLastError() -> DWORD;
    }

    let wide: Vec<u16> = PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        // No `WaitNamedPipeW`: it only waits for a free instance of a pipe that
        // already exists, so in the race that matters — the Guard has not yet
        // called `CreateNamedPipeW` — it fails immediately with
        // ERROR_FILE_NOT_FOUND rather than waiting. The retry loop is the wait.
        let handle = CreateFileW(
            wide.as_ptr(),
            CLIENT_PIPE_WRITE_RIGHTS,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        );

        if handle == INVALID_HANDLE_VALUE {
            return Err(format!("CreateFileW failed: {}", GetLastError()));
        }

        match PIPE_HANDLE.lock() {
            Ok(mut guard) => {
                if let Some(old) = *guard {
                    let _ = CloseHandle(old);
                }
                *guard = Some(handle);
                Ok(())
            }
            Err(_) => {
                let _ = CloseHandle(handle);
                Err("Guard pipe handle lock poisoned".to_owned())
            }
        }
    }
}

#[cfg(not(windows))]
pub fn connect() {}

/// Send the typed clean-exit message before an intentional Client exit.
pub fn send_clean_exit(reason: CleanExitReason) {
    send_message(GuardMessage::CleanExit(reason));
}

/// Tell the Guard where Admin is and which MAC that Admin knows this PC by
/// (ADR-0077).
///
/// Called after a heartbeat the Admin accepted, so the pair reported is the
/// one that actually works. The Guard has no configuration of its own and
/// cannot read the Client's per-user config, so this is the only way it learns
/// either value — and it is a fact, not an instruction: the Guard still
/// decides every local action for itself.
///
/// Silent and cheap when there is nothing to do: no MAC, no pipe, or the same
/// pair already reported over this connection.
pub fn report_endpoint(server_url: &str, mac: Option<&str>) {
    let Some(mac) = mac else {
        return;
    };
    let url = server_url.trim_end_matches('/').to_owned();
    let mac = mac.to_owned();

    let connected = PIPE_HANDLE
        .lock()
        .map(|handle| handle.is_some())
        .unwrap_or(false);
    if !connected {
        return;
    }
    let Ok(mut reported) = REPORTED_ENDPOINT.lock() else {
        return;
    };
    if reported.as_ref() == Some(&(url.clone(), mac.clone())) {
        return;
    }
    send_message(GuardMessage::AdminEndpoint {
        url: url.clone(),
        mac: mac.clone(),
    });
    *reported = Some((url, mac));
}

/// Forget what was reported, so the next heartbeat tells the Guard again.
#[cfg(windows)]
fn forget_reported_endpoint() {
    if let Ok(mut reported) = REPORTED_ENDPOINT.lock() {
        *reported = None;
    }
}

/// Close the pipe handle.
#[allow(dead_code)]
pub fn disconnect() {
    if let Ok(mut guard) = PIPE_HANDLE.lock() {
        if let Some(handle) = *guard {
            #[cfg(not(windows))]
            let _ = handle;
            #[cfg(windows)]
            unsafe {
                #[link(name = "kernel32")]
                unsafe extern "system" {
                    fn CloseHandle(hObject: isize) -> i32;
                }
                let _ = CloseHandle(handle);
            }
            *guard = None;
        }
    }
}

fn send_message(message: GuardMessage) {
    let handle = match PIPE_HANDLE.lock() {
        Ok(guard) => match *guard {
            Some(handle) => handle,
            None => {
                tracing::warn!(
                    target: "openpiso_client::guard_client",
                    ?message,
                    "cannot send Guard pipe message: not connected"
                );
                return;
            }
        },
        Err(_) => return,
    };
    #[cfg(not(windows))]
    let _ = handle;

    #[cfg(windows)]
    {
        #[allow(clippy::upper_case_acronyms)]
        type BOOL = i32;
        #[allow(clippy::upper_case_acronyms)]
        type DWORD = u32;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn WriteFile(
                hFile: isize,
                lpBuffer: *const std::ffi::c_void,
                nNumberOfBytesToWrite: DWORD,
                lpNumberOfBytesWritten: *mut DWORD,
                lpOverlapped: *const std::ffi::c_void,
            ) -> BOOL;
            fn GetLastError() -> DWORD;
        }

        let encoded = message.encode();
        unsafe {
            let mut written: DWORD = 0;
            let result = WriteFile(
                handle,
                encoded.as_ptr() as *const std::ffi::c_void,
                encoded.len() as DWORD,
                &mut written,
                std::ptr::null(),
            );
            if result == 0 {
                let error = GetLastError();
                tracing::warn!(
                    target: "openpiso_client::guard_client",
                    ?message,
                    error,
                    "WriteFile to Guard pipe failed"
                );
                clear_pipe_handle(handle);
            } else if written != encoded.len() as DWORD {
                tracing::warn!(
                    target: "openpiso_client::guard_client",
                    ?message,
                    written,
                    expected = encoded.len(),
                    "WriteFile to Guard pipe completed a short write"
                );
            } else {
                tracing::info!(
                    target: "openpiso_client::guard_client",
                    ?message,
                    "sent typed Guard pipe message"
                );
            }
        }
    }

    #[cfg(not(windows))]
    let _ = message;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logon_race_is_retried_every_second() {
        assert_eq!(retry_interval(Duration::ZERO), FAST_RETRY_INTERVAL);
        assert_eq!(retry_interval(Duration::from_secs(29)), FAST_RETRY_INTERVAL);
    }

    #[test]
    fn the_tail_slows_down_but_never_stops() {
        assert_eq!(
            retry_interval(FAST_RETRY_PHASE),
            SLOW_RETRY_INTERVAL,
            "the fast phase ends at its own boundary"
        );
        assert_eq!(
            retry_interval(Duration::from_secs(86_400)),
            SLOW_RETRY_INTERVAL,
            "a Guard repaired hours into a session is still picked up"
        );
    }

    #[test]
    fn debug_and_non_windows_builds_never_claim_guard_coverage() {
        if cfg!(debug_assertions) || cfg!(not(windows)) {
            assert_eq!(heartbeat_coverage(true), "not_applicable");
            assert_eq!(heartbeat_coverage(false), "not_applicable");
        }
    }

    #[cfg(all(windows, not(debug_assertions)))]
    #[test]
    fn release_windows_reports_its_real_held_connection() {
        assert_eq!(heartbeat_coverage(true), "covered");
        assert_eq!(heartbeat_coverage(false), "unavailable");
    }
}

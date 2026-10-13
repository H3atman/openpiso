//! Session-0 relaunch logic (ADR-0048).
#![allow(dead_code)]
//!
//! Re-launches the Client executable into the active console session.

use std::path::{Path, PathBuf};

/// The exact Client process created by the Guard.
///
/// Holding its process handle avoids ambiguous image-name scans and lets the
/// retry loop distinguish a failed launch from a live Client that has not yet
/// connected to the pipe.
pub struct ClientProcess {
    process_id: u32,
    #[cfg(all(windows, not(debug_assertions)))]
    handle: isize,
}

impl ClientProcess {
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Whether this exact process is still alive.
    #[cfg(all(windows, not(debug_assertions)))]
    pub fn is_running(&self) -> Result<bool, String> {
        const WAIT_OBJECT_0: u32 = 0;
        const WAIT_TIMEOUT: u32 = 258;
        const WAIT_FAILED: u32 = u32::MAX;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetLastError() -> u32;
            fn WaitForSingleObject(hHandle: isize, dwMilliseconds: u32) -> u32;
        }

        match unsafe { WaitForSingleObject(self.handle, 0) } {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => Ok(true),
            WAIT_FAILED => Err(format!("WaitForSingleObject failed: {}", unsafe {
                GetLastError()
            })),
            result => Err(format!("WaitForSingleObject returned {result}")),
        }
    }

    #[cfg(not(all(windows, not(debug_assertions))))]
    pub const fn is_running(&self) -> Result<bool, String> {
        Ok(false)
    }
}

#[cfg(all(windows, not(debug_assertions)))]
impl Drop for ClientProcess {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CloseHandle(hObject: isize) -> i32;
        }

        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// File name of the installed Client executable.
///
/// Tauri names the main binary after the Client's **Cargo package name**
/// (`openpiso-client`), not after `productName` — so the installed file is
/// `openpiso-client.exe`, never `OpenPiso Client.exe`. `client_exe_name_matches_cargo_package`
/// pins that contract so a rename cannot silently break relaunch again.
pub const CLIENT_EXE: &str = "openpiso-client.exe";

/// Pure seam: the Client path implied by the directory the Guard runs from.
///
/// Tauri places configured resources under `$INSTDIR\resources`, while the
/// Client executable is installed at `$INSTDIR`.
pub fn client_path_in(guard_dir: &Path) -> PathBuf {
    let mut install_dir = guard_dir.to_path_buf();
    if install_dir
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("resources"))
    {
        install_dir.pop();
    }
    install_dir.join(CLIENT_EXE)
}

/// The installed Client path, verified to exist.
///
/// Both relaunch and pipe-client validation go through here, so they can never
/// disagree about which image is the Client.
pub fn resolve_client_path() -> Result<PathBuf, String> {
    let guard_dir = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "Guard executable has no parent directory".to_owned())?;
    let client_path = client_path_in(&guard_dir);
    if !client_path.is_file() {
        return Err(format!(
            "Client executable not found at {}",
            client_path.display()
        ));
    }
    Ok(client_path)
}

/// The Tamper Kill report a relaunched Client is asked to file, when there is
/// one. `None` is a level-triggered relaunch: the Client simply starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TamperKillArgs {
    pub kill_timestamp_unix_ms: i64,
    pub attempt_count: i64,
}

/// Relaunch the Client into the active console session.
///
/// On non-Windows or debug builds, logs and returns `Ok` without launching.
#[cfg(all(windows, not(debug_assertions)))]
pub fn relaunch(report: Option<TamperKillArgs>) -> Result<ClientProcess, String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[allow(clippy::upper_case_acronyms)]
    type HANDLE = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type LPCWSTR = *const u16;
    #[allow(clippy::upper_case_acronyms)]
    type LPVOID = *mut c_void;

    #[repr(C)]
    struct StartupInfoW {
        cb: DWORD,
        lp_reserved: *mut u16,
        lp_desktop: *mut u16,
        lp_title: *mut u16,
        dw_x: DWORD,
        dw_y: DWORD,
        dw_x_size: DWORD,
        dw_y_size: DWORD,
        dw_x_count_chars: DWORD,
        dw_y_count_chars: DWORD,
        dw_fill_attribute: DWORD,
        dw_flags: DWORD,
        w_show_window: u16,
        cb_reserved_2: u16,
        lp_reserved_2: *mut u8,
        h_std_input: HANDLE,
        h_std_output: HANDLE,
        h_std_error: HANDLE,
    }

    #[repr(C)]
    struct ProcessInformation {
        h_process: HANDLE,
        h_thread: HANDLE,
        process_id: DWORD,
        thread_id: DWORD,
    }

    const _: () = {
        assert!(
            std::mem::size_of::<StartupInfoW>()
                == if cfg!(target_pointer_width = "64") {
                    104
                } else {
                    68
                }
        );
        assert!(
            std::mem::size_of::<ProcessInformation>()
                == if cfg!(target_pointer_width = "64") {
                    24
                } else {
                    16
                }
        );
    };

    const FALSE: BOOL = 0;
    const TOKEN_ASSIGN_PRIMARY: DWORD = 0x0001;
    const TOKEN_DUPLICATE: DWORD = 0x0002;
    const TOKEN_QUERY: DWORD = 0x0008;
    const TOKEN_PRIMARY: DWORD = 1;
    const SECURITY_IMPERSONATION: DWORD = 2;
    const CREATE_UNICODE_ENVIRONMENT: DWORD = 0x00000400;

    // --- kernel32 ---
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CloseHandle(hObject: HANDLE) -> BOOL;
        fn GetLastError() -> DWORD;
        fn WTSGetActiveConsoleSessionId() -> DWORD;
    }

    // --- wtsapi32 ---
    #[link(name = "wtsapi32")]
    unsafe extern "system" {
        fn WTSQueryUserToken(SessionId: DWORD, phToken: *mut HANDLE) -> BOOL;
    }

    // --- advapi32 ---
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn DuplicateTokenEx(
            ExistingTokenHandle: HANDLE,
            DesiredAccess: DWORD,
            TokenAttributes: LPVOID,
            ImpersonationLevel: DWORD,
            TokenType: DWORD,
            DuplicateTokenHandle: *mut HANDLE,
        ) -> BOOL;
        fn CreateProcessAsUserW(
            hToken: HANDLE,
            lpApplicationName: LPCWSTR,
            lpCommandLine: *mut u16,
            lpProcessAttributes: LPVOID,
            lpThreadAttributes: LPVOID,
            bInheritHandles: BOOL,
            dwCreationFlags: DWORD,
            lpEnvironment: LPVOID,
            lpCurrentDirectory: LPCWSTR,
            lpStartupInfo: *const StartupInfoW,
            lpProcessInformation: *mut ProcessInformation,
        ) -> BOOL;
    }

    // --- userenv ---
    #[link(name = "userenv")]
    unsafe extern "system" {
        fn CreateEnvironmentBlock(
            lpEnvironment: *mut LPVOID,
            hToken: HANDLE,
            bInherit: BOOL,
        ) -> BOOL;
        fn DestroyEnvironmentBlock(lpEnvironment: LPVOID) -> BOOL;
    }

    let client_path = resolve_client_path()?;
    let wide_path: Vec<u16> = client_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let client_dir = client_path
        .parent()
        .ok_or_else(|| "Client executable has no parent directory".to_owned())?;
    let wide_client_dir: Vec<u16> = client_dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == 0xFFFFFFFF {
            return Err("no active console session".to_owned());
        }

        // A SYSTEM token with its session id changed is still SYSTEM. Query
        // the token of the user attached to the active console instead, so the
        // relaunched Client has the user's profile, environment, and desktop.
        let mut user_token: HANDLE = 0;
        if WTSQueryUserToken(session_id, &mut user_token) == FALSE {
            return Err(format!("WTSQueryUserToken failed: {}", GetLastError()));
        }

        let desired_token_access = TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY;
        let mut duplicate_token: HANDLE = 0;
        if DuplicateTokenEx(
            user_token,
            desired_token_access,
            ptr::null_mut(),
            SECURITY_IMPERSONATION,
            TOKEN_PRIMARY,
            &mut duplicate_token,
        ) == FALSE
        {
            let error = GetLastError();
            let _ = CloseHandle(user_token);
            return Err(format!("DuplicateTokenEx failed: {error}"));
        }
        let _ = CloseHandle(user_token);

        let mut environment: LPVOID = ptr::null_mut();
        if CreateEnvironmentBlock(&mut environment, duplicate_token, FALSE) == FALSE {
            let error = GetLastError();
            let _ = CloseHandle(duplicate_token);
            return Err(format!("CreateEnvironmentBlock failed: {error}"));
        }

        let command_line = match report {
            Some(TamperKillArgs {
                kill_timestamp_unix_ms,
                attempt_count,
            }) => format!(
                "\"{}\" --openpiso-tamper-kill {} {}",
                client_path.display(),
                kill_timestamp_unix_ms,
                attempt_count.max(1),
            ),
            // A level-triggered relaunch (ADR-0077) saw an absent Client, not
            // a death. It must not hand the Client a report to file, or the
            // Admin would record a Tamper Kill nobody committed.
            None => format!("\"{}\"", client_path.display()),
        };
        let mut command_line: Vec<u16> = command_line
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut desktop = "winsta0\\default"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let startup_info = StartupInfoW {
            cb: std::mem::size_of::<StartupInfoW>() as DWORD,
            lp_reserved: ptr::null_mut(),
            lp_desktop: desktop.as_mut_ptr(),
            lp_title: ptr::null_mut(),
            dw_x: 0,
            dw_y: 0,
            dw_x_size: 0,
            dw_y_size: 0,
            dw_x_count_chars: 0,
            dw_y_count_chars: 0,
            dw_fill_attribute: 0,
            dw_flags: 0,
            w_show_window: 0,
            cb_reserved_2: 0,
            lp_reserved_2: ptr::null_mut(),
            h_std_input: 0,
            h_std_output: 0,
            h_std_error: 0,
        };
        let mut process_info = ProcessInformation {
            h_process: 0,
            h_thread: 0,
            process_id: 0,
            thread_id: 0,
        };

        let result = CreateProcessAsUserW(
            duplicate_token,
            wide_path.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            FALSE,
            CREATE_UNICODE_ENVIRONMENT,
            environment,
            wide_client_dir.as_ptr(),
            &startup_info,
            &mut process_info,
        );
        let error = if result == FALSE {
            Some(GetLastError())
        } else {
            None
        };

        let _ = DestroyEnvironmentBlock(environment);
        let _ = CloseHandle(duplicate_token);

        if let Some(error) = error {
            Err(format!("CreateProcessAsUserW failed: {error}"))
        } else {
            let _ = CloseHandle(process_info.h_thread);
            Ok(ClientProcess {
                process_id: process_info.process_id,
                handle: process_info.h_process,
            })
        }
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
pub fn relaunch(_report: Option<TamperKillArgs>) -> Result<ClientProcess, String> {
    tracing::info!(
        target: "openpiso_client_guard::relauncher",
        "relaunch suppressed (debug or non-Windows build)"
    );
    Ok(ClientProcess { process_id: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    // `C:\…` paths have no parent structure on non-Windows hosts.
    #[cfg(windows)]
    #[test]
    fn client_path_resolves_next_to_the_install_dir_from_resources() {
        let path = client_path_in(Path::new(r"C:\Program Files\OpenPiso Client\resources"));
        assert_eq!(
            path,
            Path::new(r"C:\Program Files\OpenPiso Client").join(CLIENT_EXE)
        );
    }

    #[test]
    fn client_path_resolves_beside_a_guard_run_from_the_install_dir() {
        let path = client_path_in(Path::new(r"C:\Program Files\OpenPiso Client"));
        assert_eq!(
            path,
            Path::new(r"C:\Program Files\OpenPiso Client").join(CLIENT_EXE)
        );
    }

    /// The mismatch this constant caused (Guard expected `OpenPiso Client.exe`,
    /// the installer wrote `openpiso-client.exe`) is only detectable against the
    /// Client's Cargo package name — Tauri names the main binary after it, and
    /// `mainBinaryName` would be the only override.
    #[test]
    fn client_exe_name_matches_cargo_package() {
        let manifest = include_str!("../../client/src-tauri/Cargo.toml");
        let package_name = manifest
            .lines()
            .find_map(|line| line.trim().strip_prefix("name = "))
            .map(|value| value.trim().trim_matches('"'))
            .expect("Client Cargo.toml declares a package name");
        assert_eq!(CLIENT_EXE, format!("{package_name}.exe"));

        let tauri_config = include_str!("../../client/src-tauri/tauri.conf.json");
        assert!(
            !tauri_config.contains("mainBinaryName"),
            "tauri.conf.json sets mainBinaryName; CLIENT_EXE must follow it instead of the package name"
        );
    }
}

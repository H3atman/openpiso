//! Named-pipe transport for the Client Guard (ADR-0048).
//!
//! The Guard owns a one-way, message-mode named pipe. Its ACL gives SYSTEM
//! full access and the interactive user write access only; the Client cannot
//! read the server end or create/impersonate one. A dropped connection emits a
//! single event, so liveness is event-driven rather than process polling.

use openpiso_core::guard::GuardMessage;
#[cfg(windows)]
use openpiso_core::guard::MAX_ADMIN_ENDPOINT_BYTES;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
#[cfg(any(windows, test))]
use std::time::{Duration, Instant};

#[cfg(windows)]
pub const PIPE_NAME: &str = r"\\.\pipe\OpenPisoClientGuard";

/// Only the Windows listener emits these; other hosts still match on them.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug)]
pub enum PipeEvent {
    ClientConnected {
        process_id: u32,
    },
    ClientDisconnected {
        message: Option<GuardMessage>,
    },
    /// The Client reported where Admin is and what MAC it is known by
    /// (ADR-0077). Unlike every other message this one arrives *during* the
    /// connection rather than announcing its end.
    AdminEndpoint {
        url: String,
        mac: String,
    },
}

pub struct GuardState {
    pub should_stop: Arc<AtomicBool>,
}

impl GuardState {
    pub fn new() -> Self {
        Self {
            should_stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[cfg(any(windows, test))]
const FOREIGN_CLIENT_LOG_INTERVAL: Duration = Duration::from_secs(30);

#[cfg(any(windows, test))]
#[derive(Default)]
struct ForeignClientLogLimiter {
    last_report: Option<Instant>,
    suppressed: u64,
}

#[cfg(any(windows, test))]
impl ForeignClientLogLimiter {
    fn observe(&mut self, now: Instant) -> Option<u64> {
        let should_report = self
            .last_report
            .is_none_or(|last| now.saturating_duration_since(last) >= FOREIGN_CLIENT_LOG_INTERVAL);
        if should_report {
            let suppressed = self.suppressed;
            self.last_report = Some(now);
            self.suppressed = 0;
            Some(suppressed)
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            None
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::time::Duration;
    use tracing::{error, info, warn};

    use super::{
        ForeignClientLogLimiter, GuardMessage, GuardState, Instant, PipeEvent,
        MAX_ADMIN_ENDPOINT_BYTES, PIPE_NAME,
    };

    // ── Raw Win32 FFI ────────────────────────────────────────────────

    #[allow(clippy::upper_case_acronyms)]
    type HANDLE = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type LPVOID = *mut c_void;
    #[allow(clippy::upper_case_acronyms)]
    type LPCWSTR = *const u16;
    #[allow(clippy::upper_case_acronyms)]
    type LPWSTR = *mut u16;

    const FALSE: BOOL = 0;
    const INVALID_HANDLE_VALUE: HANDLE = -1;
    const ERROR_PIPE_CONNECTED: DWORD = 535;
    const ERROR_MORE_DATA: DWORD = 234;

    const PIPE_ACCESS_INBOUND: DWORD = 0x00000001;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: DWORD = 0x00080000;
    const PIPE_TYPE_MESSAGE: DWORD = 0x00000004;
    const PIPE_READMODE_MESSAGE: DWORD = 0x00000002;
    const PIPE_WAIT: DWORD = 0x00000000;
    const PIPE_UNLIMITED_INSTANCES: DWORD = 255;
    const NMPWAIT_USE_DEFAULT_WAIT: DWORD = 0x00000000;
    const SDDL_REVISION_1: DWORD = 1;
    // Named-pipe client write rights: FILE_GENERIC_WRITE's individual rights
    // minus FILE_APPEND_DATA (0x4, aliased to FILE_CREATE_PIPE_INSTANCE), plus
    // FILE_READ_ATTRIBUTES, which CreateFile requires for a pipe client open.
    // This deliberately excludes FILE_READ_DATA.
    const CLIENT_PIPE_WRITE_RIGHTS: DWORD = 0x00120192;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            StringSecurityDescriptor: LPCWSTR,
            StringSDRevision: DWORD,
            SecurityDescriptor: *mut LPVOID,
            SecurityDescriptorSize: *mut DWORD,
        ) -> BOOL;

        #[cfg(test)]
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            SecurityDescriptor: LPVOID,
            RequestedStringSDRevision: DWORD,
            SecurityInformation: DWORD,
            StringSecurityDescriptor: *mut LPWSTR,
            StringSecurityDescriptorLen: *mut DWORD,
        ) -> BOOL;

        #[cfg(test)]
        fn GetSecurityInfo(
            handle: HANDLE,
            ObjectType: DWORD,
            SecurityInfo: DWORD,
            ppsidOwner: *mut LPVOID,
            ppsidGroup: *mut LPVOID,
            ppDacl: *mut LPVOID,
            ppSacl: *mut LPVOID,
            ppSecurityDescriptor: *mut LPVOID,
        ) -> DWORD;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateNamedPipeW(
            lpName: LPCWSTR,
            dwOpenMode: DWORD,
            dwPipeMode: DWORD,
            nMaxInstances: DWORD,
            nOutBufferSize: DWORD,
            nInBufferSize: DWORD,
            nDefaultTimeOut: DWORD,
            lpSecurityAttributes: *mut SecurityAttributes,
        ) -> HANDLE;
        fn ConnectNamedPipe(hNamedPipe: HANDLE, lpOverlapped: LPVOID) -> BOOL;
        fn DisconnectNamedPipe(hNamedPipe: HANDLE) -> BOOL;
        fn ReadFile(
            hFile: HANDLE,
            lpBuffer: LPVOID,
            nNumberOfBytesToRead: DWORD,
            lpNumberOfBytesRead: *mut DWORD,
            lpOverlapped: LPVOID,
        ) -> BOOL;
        fn GetNamedPipeClientProcessId(Pipe: HANDLE, ClientProcessId: *mut DWORD) -> BOOL;
        fn OpenProcess(dwDesiredAccess: DWORD, bInheritHandle: BOOL, dwProcessId: DWORD) -> HANDLE;
        fn QueryFullProcessImageNameW(
            hProcess: HANDLE,
            dwFlags: DWORD,
            lpExeName: LPWSTR,
            lpdwSize: *mut DWORD,
        ) -> BOOL;
        fn GetLastError() -> DWORD;
        fn CloseHandle(hObject: HANDLE) -> BOOL;
        fn LocalFree(hMem: HANDLE) -> HANDLE;

        #[cfg(test)]
        fn CreateFileW(
            lpFileName: LPCWSTR,
            dwDesiredAccess: DWORD,
            dwShareMode: DWORD,
            lpSecurityAttributes: *mut SecurityAttributes,
            dwCreationDisposition: DWORD,
            dwFlagsAndAttributes: DWORD,
            hTemplateFile: HANDLE,
        ) -> HANDLE;

        #[cfg(test)]
        fn WriteFile(
            hFile: HANDLE,
            lpBuffer: *const c_void,
            nNumberOfBytesToWrite: DWORD,
            lpNumberOfBytesWritten: *mut DWORD,
            lpOverlapped: LPVOID,
        ) -> BOOL;
    }

    #[repr(C)]
    struct SecurityAttributes {
        n_length: DWORD,
        security_descriptor: LPVOID,
        inherit_handle: BOOL,
    }

    const _: () = {
        assert!(
            std::mem::size_of::<SecurityAttributes>()
                == if cfg!(target_pointer_width = "64") {
                    24
                } else {
                    12
                }
        );
    };

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    enum ClientVerificationError {
        ForeignImage {
            process_id: DWORD,
            actual: String,
            expected: String,
        },
        Inspection {
            process_id: Option<DWORD>,
            error: String,
        },
    }

    struct PipeInstance(HANDLE);

    impl PipeInstance {
        fn create() -> Result<Self, String> {
            Self::create_at(PIPE_NAME)
        }

        fn create_at(pipe_name: &str) -> Result<Self, String> {
            let name = to_wide(pipe_name);
            // SYSTEM owns the server end. IU receives FILE_GENERIC_WRITE's
            // individual rights except FILE_CREATE_PIPE_INSTANCE (0x4).
            let sddl = to_wide(&format!(
                "D:(A;;GA;;;SY)(A;;0x{CLIENT_PIPE_WRITE_RIGHTS:08x};;;IU)"
            ));

            unsafe {
                let mut security_descriptor: LPVOID = std::ptr::null_mut();
                let mut security_descriptor_size: DWORD = 0;
                let ret = ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut security_descriptor,
                    &mut security_descriptor_size,
                );
                if ret == FALSE {
                    return Err(format!(
                        "ConvertStringSecurityDescriptorToSecurityDescriptorW failed: {}",
                        GetLastError()
                    ));
                }

                let mut security_attributes = SecurityAttributes {
                    n_length: std::mem::size_of::<SecurityAttributes>() as DWORD,
                    security_descriptor,
                    inherit_handle: FALSE,
                };
                let handle = CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    4096,
                    NMPWAIT_USE_DEFAULT_WAIT,
                    &mut security_attributes,
                );
                let create_error = if handle == INVALID_HANDLE_VALUE {
                    Some(GetLastError())
                } else {
                    None
                };

                let _ = LocalFree(security_descriptor as HANDLE);

                if let Some(error) = create_error {
                    return Err(format!("CreateNamedPipeW failed: {error}"));
                }

                Ok(Self(handle))
            }
        }

        #[cfg(test)]
        fn dacl_sddl(&self) -> Result<String, String> {
            const SE_KERNEL_OBJECT: DWORD = 6;
            const DACL_SECURITY_INFORMATION: DWORD = 0x0000_0004;

            unsafe {
                let mut descriptor: LPVOID = std::ptr::null_mut();
                let error = GetSecurityInfo(
                    self.0,
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut descriptor,
                );
                if error != 0 {
                    return Err(format!("GetSecurityInfo failed: {error}"));
                }

                let mut sddl: LPWSTR = std::ptr::null_mut();
                let mut sddl_length: DWORD = 0;
                let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut sddl,
                    &mut sddl_length,
                );
                let convert_error = if converted == FALSE {
                    Some(GetLastError())
                } else {
                    None
                };
                let result = if let Some(error) = convert_error {
                    Err(format!(
                        "ConvertSecurityDescriptorToStringSecurityDescriptorW failed: {error}"
                    ))
                } else {
                    let wide = std::slice::from_raw_parts(sddl, sddl_length as usize);
                    Ok(String::from_utf16_lossy(wide)
                        .trim_end_matches('\0')
                        .to_owned())
                };

                if !sddl.is_null() {
                    let _ = LocalFree(sddl as HANDLE);
                }
                let _ = LocalFree(descriptor as HANDLE);
                result
            }
        }

        fn connect(&self) -> Result<(), String> {
            unsafe {
                if ConnectNamedPipe(self.0, std::ptr::null_mut()) == FALSE {
                    let error = GetLastError();
                    if error != ERROR_PIPE_CONNECTED {
                        return Err(format!("ConnectNamedPipe failed: {error}"));
                    }
                }
                Ok(())
            }
        }

        fn verify_client_image(&self) -> Result<DWORD, ClientVerificationError> {
            const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;

            unsafe {
                let mut client_pid = 0;
                if GetNamedPipeClientProcessId(self.0, &mut client_pid) == FALSE {
                    return Err(ClientVerificationError::Inspection {
                        process_id: None,
                        error: format!("GetNamedPipeClientProcessId failed: {}", GetLastError()),
                    });
                }

                let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, client_pid);
                if process == 0 {
                    return Err(ClientVerificationError::Inspection {
                        process_id: Some(client_pid),
                        error: format!("OpenProcess failed: {}", GetLastError()),
                    });
                }

                let mut path_buffer = [0_u16; 32_768];
                let mut path_length = path_buffer.len() as DWORD;
                let result = QueryFullProcessImageNameW(
                    process,
                    0,
                    path_buffer.as_mut_ptr(),
                    &mut path_length,
                );
                let error = if result == FALSE {
                    Some(GetLastError())
                } else {
                    None
                };
                let _ = CloseHandle(process);

                if let Some(error) = error {
                    return Err(ClientVerificationError::Inspection {
                        process_id: Some(client_pid),
                        error: format!("QueryFullProcessImageNameW failed: {error}"),
                    });
                }

                let actual = String::from_utf16_lossy(&path_buffer[..path_length as usize]);
                let expected = crate::relauncher::resolve_client_path().map_err(|error| {
                    ClientVerificationError::Inspection {
                        process_id: Some(client_pid),
                        error,
                    }
                })?;
                let expected = String::from_utf16_lossy(
                    &expected.as_os_str().encode_wide().collect::<Vec<_>>(),
                );
                if actual.eq_ignore_ascii_case(&expected) {
                    Ok(client_pid)
                } else {
                    Err(ClientVerificationError::ForeignImage {
                        process_id: client_pid,
                        actual,
                        expected,
                    })
                }
            }
        }

        /// Read one message. `Err` means the pipe is gone — that, and only
        /// that, is a disconnect. `Ok(None)` is a message this Guard does not
        /// understand: worth logging, never worth inventing a death over.
        fn read_message(&self) -> Result<Option<GuardMessage>, String> {
            unsafe {
                // Sized for the largest defined message (the Admin endpoint),
                // plus its tag. A longer write is rejected as oversized rather
                // than silently truncated into something decodable.
                let mut buffer = [0_u8; MAX_ADMIN_ENDPOINT_BYTES + 1];
                let mut read: DWORD = 0;
                let result = ReadFile(
                    self.0,
                    buffer.as_mut_ptr() as LPVOID,
                    buffer.len() as DWORD,
                    &mut read,
                    std::ptr::null_mut(),
                );

                if result == FALSE {
                    let error = GetLastError();
                    if error == ERROR_MORE_DATA {
                        return Ok(None);
                    }
                    return Err(format!("ReadFile failed (pipe broken): {error}"));
                }
                if read == 0 {
                    return Err("pipe closed (EOF)".to_owned());
                }

                Ok(GuardMessage::decode(&buffer[..read as usize]))
            }
        }
    }

    impl Drop for PipeInstance {
        fn drop(&mut self) {
            unsafe {
                let _ = DisconnectNamedPipe(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn listen(state: &GuardState, events: &Sender<PipeEvent>) {
        info!(target: "openpiso_client_guard::pipe", "pipe server starting");
        let mut foreign_client_logs = ForeignClientLogLimiter::default();

        loop {
            if state.should_stop.load(Ordering::Relaxed) {
                info!(target: "openpiso_client_guard::pipe", "stopping");
                return;
            }

            let instance = match PipeInstance::create() {
                Ok(instance) => instance,
                Err(error) => {
                    error!(target: "openpiso_client_guard::pipe", "create: {error}");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };

            if let Err(error) = instance.connect() {
                warn!(target: "openpiso_client_guard::pipe", "connect: {error}");
                continue;
            }

            let process_id = match instance.verify_client_image() {
                Ok(process_id) => process_id,
                Err(ClientVerificationError::ForeignImage {
                    process_id,
                    actual,
                    expected,
                }) => {
                    if let Some(suppressed_connections) =
                        foreign_client_logs.observe(Instant::now())
                    {
                        warn!(
                            target: "openpiso_client_guard::pipe",
                            process_id,
                            actual_image = %actual,
                            expected_image = %expected,
                            suppressed_connections,
                            "rejecting foreign pipe client"
                        );
                    }
                    continue;
                }
                Err(ClientVerificationError::Inspection { process_id, error }) => {
                    error!(
                        target: "openpiso_client_guard::pipe",
                        ?process_id,
                        error = %error,
                        "could not verify pipe client; rejecting connection"
                    );
                    continue;
                }
            };
            info!(target: "openpiso_client_guard::pipe", process_id, "Client connected");
            if events
                .send(PipeEvent::ClientConnected { process_id })
                .is_err()
            {
                return;
            }

            // The Client may send several messages over one connection: its
            // Admin endpoint on connect, and at most one exit announcement at
            // the end. Only a broken pipe ends the loop, so an endpoint
            // message can never be mistaken for a death.
            let mut exit_message = None;
            loop {
                match instance.read_message() {
                    Ok(Some(GuardMessage::AdminEndpoint { url, mac })) => {
                        info!(
                            target: "openpiso_client_guard::pipe",
                            %url,
                            "Client reported its Admin endpoint"
                        );
                        if events.send(PipeEvent::AdminEndpoint { url, mac }).is_err() {
                            return;
                        }
                    }
                    Ok(Some(message)) => {
                        info!(target: "openpiso_client_guard::pipe", ?message, "typed pipe message received");
                        exit_message = Some(message);
                    }
                    Ok(None) => {
                        warn!(
                            target: "openpiso_client_guard::pipe",
                            "unknown Guard pipe message ignored"
                        );
                    }
                    Err(error) => {
                        warn!(target: "openpiso_client_guard::pipe", "client disconnected: {error}");
                        break;
                    }
                }
            }

            if events
                .send(PipeEvent::ClientDisconnected {
                    message: exit_message,
                })
                .is_err()
            {
                return;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pipe_object_carries_the_intended_dacl() {
            const ERROR_ACCESS_DENIED: DWORD = 5;
            const OPEN_EXISTING: DWORD = 3;

            let name = format!(
                r"\\.\pipe\OpenPisoClientGuardAclTest-{}",
                std::process::id()
            );
            let pipe = PipeInstance::create_at(&name).expect("create test pipe");

            assert_eq!(
                pipe.dacl_sddl().expect("read pipe DACL"),
                "D:(A;;FA;;;SY)(A;;0x120192;;;IU)"
            );

            let wide_name = to_wide(&name);
            unsafe {
                // The interactive token can connect with the exact right the
                // Client requests.
                let client = CreateFileW(
                    wide_name.as_ptr(),
                    CLIENT_PIPE_WRITE_RIGHTS,
                    0,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    0,
                );
                let client_error = GetLastError();
                assert_ne!(
                    client, INVALID_HANDLE_VALUE,
                    "Client write open failed: {client_error}"
                );
                let message = [1_u8];
                let mut written = 0;
                assert_ne!(
                    WriteFile(
                        client,
                        message.as_ptr() as *const c_void,
                        message.len() as DWORD,
                        &mut written,
                        std::ptr::null_mut(),
                    ),
                    FALSE,
                    "Client WriteFile failed: {}",
                    GetLastError()
                );
                assert_eq!(written, message.len() as DWORD);
                let _ = CloseHandle(client);

                // It cannot add a server instance: that requires
                // FILE_CREATE_PIPE_INSTANCE (0x4), absent from the IU ACE.
                let competing = CreateNamedPipeW(
                    wide_name.as_ptr(),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    4096,
                    NMPWAIT_USE_DEFAULT_WAIT,
                    std::ptr::null_mut(),
                );
                let error = GetLastError();
                if competing != INVALID_HANDLE_VALUE {
                    let _ = CloseHandle(competing);
                }
                assert_eq!(competing, INVALID_HANDLE_VALUE);
                assert_eq!(error, ERROR_ACCESS_DENIED);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use tracing::info;

    use super::{GuardState, PipeEvent};

    pub fn listen(state: &GuardState, _events: &Sender<PipeEvent>) {
        info!(target: "openpiso_client_guard::pipe", "pipe: non-Windows, no-op");
        while !state.should_stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}

pub use platform::listen;

#[cfg(test)]
mod log_limiter_tests {
    use super::*;

    #[test]
    fn foreign_client_logs_immediately_then_summarizes_suppressed_rejections() {
        let start = Instant::now();
        let mut limiter = ForeignClientLogLimiter::default();

        assert_eq!(limiter.observe(start), Some(0));
        assert_eq!(limiter.observe(start + Duration::from_secs(1)), None);
        assert_eq!(limiter.observe(start + Duration::from_secs(29)), None);
        assert_eq!(
            limiter.observe(start + FOREIGN_CLIENT_LOG_INTERVAL),
            Some(2)
        );
        assert_eq!(limiter.observe(start + Duration::from_secs(31)), None);
        assert_eq!(
            limiter.observe(start + FOREIGN_CLIENT_LOG_INTERVAL * 2),
            Some(1)
        );
    }
}

//! Client startup arbitration (issues #366 / #369, ADR-0080).
//!
//! Exactly one admitted Client owns runtime startup for the supported shop
//! PC. This module is the gate at the very top of the Client launch
//! boundary, before the native placeholder, keyboard hooks, Guard
//! connection, Tauri/WebView initialization, normal state writes, or any
//! networking. It deliberately supplements — never replaces — the Client
//! Guard's exact-child process-handle/PID liveness gate, and it consumes
//! the durable local handoff boundary ([`crate::startup_coordinator`],
//! ADR-0079) so a losing launch carrying a Tamper Kill report preserves the
//! report instead of dropping it.
//!
//! ## Mechanism
//!
//! A lifetime-scoped, machine-wide (`Global\`) Win32 mutex with a fixed
//! installed-Client name. `CreateMutexW` plus a zero-timeout
//! `WaitForSingleObject` is the atomic check-and-own step: the kernel
//! serializes competing processes, so a check-then-start race cannot admit
//! two runtimes. The name carries no working directory, shortcut,
//! executable path, or version component, so none of those can admit a
//! second participating production runtime. The `Global\` namespace means
//! a Client in another Windows session competes for the same object
//! instead of racing for the machine-wide Guard pipe under a
//! session-local name; a conflicting-session loser simply exits inertly —
//! there is no takeover and no process termination.
//!
//! Ownership is the kernel object itself: the handle is explicitly
//! non-inheritable (`bInheritHandle = FALSE`), so WebView helpers or child
//! processes cannot keep ownership alive, and the OS releases (or marks
//! abandoned) when the owning process exits. Ownership is retained until
//! teardown completes — the handle lives in a process static and is never
//! closed. A successor acquires cleanly after clean exit, startup crash,
//! or runtime crash, with no stale-file cleanup and no PID-based
//! decisions: mutex abandonment is the safe re-evaluation of ownership
//! after a death, and a PID alone is never an ownership token.
//!
//! ## Outcomes
//!
//! Three distinct outcomes, never collapsed:
//!
//! - **Admitted owner**: continues into normal startup unchanged.
//! - **Handled duplicate**: exits promptly and inertly — no placeholder,
//!   window, hook, Guard pipe, Heartbeat/SSE/Diagnostic Forwarding task,
//!   and no unrelated configuration/report/countdown writes. If the launch
//!   carries validated Guard report arguments, the report is durably
//!   handed off through the ADR-0079 inbox *before* the handled-success
//!   exit; an unpersistable handoff is a truthful failure (exit code 3),
//!   never a benign duplicate. Malformed arguments can manufacture no
//!   report.
//! - **Coordination failure**: denied access, an incompatible object with
//!   the coordination name, or an uncertain wait outcome is diagnosed and
//!   exits nonzero. It must not admit a second runtime and must not pose
//!   as a benign duplicate.
//!
//! ## Platform gating
//!
//! Enforced in release Windows builds. Debug builds and non-Windows hosts
//! stay inert so `tauri dev`, multiple dev instances, and the
//! `not_applicable` Client Guard Coverage behavior are untouched. Two
//! test-only overrides (production never sets them) let the process
//! harness exercise the *production* coordination adapter in a debug
//! build: [`ENFORCE_OVERRIDE_ENV`] activates the gate and
//! [`NAME_OVERRIDE_ENV`] isolates the object name. The
//! [`PROBE_FLAG`] launch substitutes an inert runtime behind the
//! production admission boundary — the harness's fast contender for
//! startup-race stress that never boots Tauri, WebView, or the kiosk
//! effects.
//!
//! ## Diagnostics
//!
//! Every outcome emits one compact evidence line with process-lifetime
//! context (PID plus the per-launch token from issue #368) on the
//! `openpiso_client::startup` target and a `openpiso-arbitration:` console
//! line. Rejected processes are rate-bounded by construction: exactly one
//! line per process, and they never start their own network forwarder.
//! The admitted owner records its admission among the durable startup
//! milestones.

#[cfg(windows)]
use crate::startup_coordinator::parse_report_args;
use crate::startup_coordinator::{resolve_config_dir, submit_report, HANDOFF_EXIT_UNPERSISTED};
use crate::state::PendingTamperKillReport;

/// Test/dev-only launch flag that substitutes an inert runtime behind the
/// production admission boundary. The process harness uses it to run the
/// production entrypoint ordering — sender check, then arbitration — with
/// real competing child processes and real native objects, without booting
/// Tauri, WebView2, or any destructive kiosk effect.
pub const PROBE_FLAG: &str = "--openpiso-arbitration-probe";

/// Test-only override for the coordination object name, so harness scenarios
/// isolate their native objects from each other and from any installed
/// Client. Production never sets it.
pub const NAME_OVERRIDE_ENV: &str = "OPENPISO_CLIENT_ARBITRATION_NAME";

/// Test-only activation of the production gate inside a debug Windows build,
/// so the harness exercises the same code path release enforces. Production
/// never sets it; debug and non-Windows builds stay inert without it.
pub const ENFORCE_OVERRIDE_ENV: &str = "OPENPISO_CLIENT_ARBITRATION_ENFORCE";

/// Test-only named-event barrier: a launch waits for this event at the very
/// top of the entrypoint, before the sender check and before arbitrating, so
/// the harness can release real senders and real contenders simultaneously.
/// Production never sets it.
pub const PROBE_START_EVENT_ENV: &str = "OPENPISO_CLIENT_PROBE_START_EVENT";

/// Test-only probe hold time in milliseconds: how long an admitted probe
/// owner holds ownership before exiting cleanly (`0` parks until killed, for
/// owner-death injection). Production never sets it.
pub const PROBE_HOLD_ENV: &str = "OPENPISO_CLIENT_PROBE_HOLD_MS";

/// Default probe hold: an admitted probe owner exits promptly on its own so
/// a successor can acquire without external coordination.
pub const PROBE_HOLD_DEFAULT_MS: u64 = 400;

/// Test-only directory, inside the isolated config directory, where an
/// admitted probe owner records the instant it was admitted. Together with
/// the exit time the OS reports for the same process, that gives the harness
/// each admitted runtime's *lifetime*, which is what proves two admitted
/// runtimes never overlapped — including in owner-death rounds, where a
/// killed owner cannot report its own release (issue #371 finding 5).
///
/// Only a [`PROBE_FLAG`] launch writes here, so production writes nothing.
/// The recorded instant is harness evidence in integer milliseconds; it
/// touches no balance, timer, or authority value.
pub const PROBE_ADMISSION_DIR: &str = "arbitration-admissions";

/// The production coordination object. `Global\` makes it machine-wide so a
/// production Client in another Windows session cannot compete for the
/// machine-wide Guard pipe under a session-local name. The name is fixed:
/// no working-directory, shortcut, executable-rename, or version component
/// can admit a second participating production runtime.
pub const RUNTIME_OBJECT_NAME: &str = "Global\\OpenPiso-Client-Runtime";

/// A handled duplicate is a non-event: exit success after any required
/// durable report handoff.
pub const EXIT_DUPLICATE_HANDLED: i32 = 0;
/// Coordination could not be trusted (denied access, incompatible object,
/// uncertain wait). Never silent admission, never a benign duplicate.
pub const EXIT_COORDINATION_FAILURE: i32 = 4;

/// The three meaningful startup outcomes. Admission and duplicate rejection
/// are distinct from an uncertain coordination failure by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Admitted,
    // Only the Windows ownership object produces these two.
    #[cfg_attr(not(windows), allow(dead_code))]
    Duplicate(Option<PendingTamperKillReport>),
    #[cfg_attr(not(windows), allow(dead_code))]
    CoordinationFailure(String),
}

/// Whether the production gate is enforced for this build and host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    #[cfg_attr(not(windows), allow(dead_code))]
    Enforced,
    Inert,
}

/// Pure enforcement policy: release Windows enforces; debug Windows enforces
/// only under the test override; non-Windows never enforces.
fn enforcement_from(debug_build: bool, override_value: Option<std::ffi::OsString>) -> Enforcement {
    #[cfg(windows)]
    {
        if !debug_build || override_value.as_deref() == Some(std::ffi::OsStr::new("1")) {
            return Enforcement::Enforced;
        }
    }
    let _ = (debug_build, override_value);
    Enforcement::Inert
}

/// The enforcement policy for this build and host.
pub fn enforcement() -> Enforcement {
    enforcement_from(
        cfg!(debug_assertions),
        std::env::var_os(ENFORCE_OVERRIDE_ENV),
    )
}

/// The coordination object name for this launch: the test override when set,
/// otherwise the fixed production identity.
pub fn object_name() -> String {
    if let Some(name) = std::env::var_os(NAME_OVERRIDE_ENV) {
        let name = name.to_string_lossy().into_owned();
        if !name.is_empty() {
            return name;
        }
    }
    RUNTIME_OBJECT_NAME.to_owned()
}

/// Whether this launch requests the harness's inert probe runtime.
pub fn is_probe_launch(args: &[String]) -> bool {
    args.iter().any(|arg| arg == PROBE_FLAG)
}

/// Map one native acquisition onto the three outcomes. The report arguments
/// ride the duplicate outcome so the losing branch can hand them off; the
/// Win32 error rides the failure outcome so real operation/error codes stay
/// distinguishable.
#[cfg(windows)]
fn map_acquisition(acquisition: platform::Acquisition, args: &[String]) -> Outcome {
    match acquisition {
        platform::Acquisition::Owned => Outcome::Admitted,
        platform::Acquisition::Busy => Outcome::Duplicate(parse_report_args(args)),
        platform::Acquisition::Failed(error) => Outcome::CoordinationFailure(format!(
            "could not consult the Client runtime ownership object: Win32 error {error}"
        )),
    }
}

/// Arbitrate this launch. Inert builds admit unconditionally, preserving
/// debug and non-Windows development behavior. Enforced builds perform the
/// atomic check-and-own against the coordination object.
pub fn coordinate(args: &[String]) -> Outcome {
    if enforcement() == Enforcement::Inert {
        return Outcome::Admitted;
    }
    #[cfg(windows)]
    {
        map_acquisition(platform::acquire(&object_name()), args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Outcome::Admitted
    }
}

/// The early exit for a non-admitted launch: durable evidence, the required
/// report handoff for a losing launch that carries one, and a truthful exit
/// code. The caller must exit with the returned code without initializing
/// any further runtime. The `Admitted` arm is defensive only — the caller
/// never routes an admitted launch here.
pub fn handle_early_outcome(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Admitted => EXIT_DUPLICATE_HANDLED,
        Outcome::Duplicate(report) => {
            let mut handoff_error = None;
            if let Some(report) = report {
                if let Err(error) = submit_report(report, &resolve_config_dir()) {
                    handoff_error = Some(error);
                }
            }
            emit_duplicate_evidence(report.is_some(), handoff_error.as_deref());
            if handoff_error.is_some() {
                HANDOFF_EXIT_UNPERSISTED
            } else {
                EXIT_DUPLICATE_HANDLED
            }
        }
        Outcome::CoordinationFailure(error) => {
            emit_coordination_failure_evidence(error);
            EXIT_COORDINATION_FAILURE
        }
    }
}

/// One compact admitted-owner line among the durable startup milestones,
/// emitted right after the runtime's tracing is ready. The PID pairs with
/// the per-launch token so a reused PID never reads as one continuous
/// process (issue #368's lifetime context convention).
pub fn log_admitted_owner() {
    tracing::info!(
        target: "openpiso_client::startup",
        process_id = crate::diagnostics::process_id(),
        launch = crate::diagnostics::launch_token(),
        object = %object_name(),
        "client startup arbitration: this launch owns the runtime"
    );
}

/// Wait on the harness start barrier at the top of the entrypoint. No-op
/// unless the test-only environment sets the event name. Non-Windows hosts
/// have no named-event primitive and no enforced gate, so the barrier is
/// inert there — the whole harness path is Windows machinery.
pub fn wait_for_start_barrier() {
    let Some(name) = std::env::var_os(PROBE_START_EVENT_ENV)
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
    else {
        return;
    };
    #[cfg(windows)]
    {
        platform::wait_for_named_event(&name);
    }
    #[cfg(not(windows))]
    {
        let _ = name;
    }
}

/// The inert probe runtime for an admitted launch: emit the admission
/// evidence, hold ownership for the configured hold time (or until killed),
/// then exit cleanly so a successor can acquire. Returns the process exit
/// code; the caller exits immediately.
pub fn run_probe_owner() -> i32 {
    record_probe_admission();
    emit_probe_admitted_evidence();
    match probe_hold_ms() {
        0 => std::thread::sleep(std::time::Duration::from_secs(u64::MAX)),
        milliseconds => std::thread::sleep(std::time::Duration::from_millis(milliseconds)),
    }
    EXIT_DUPLICATE_HANDLED
}

/// Record this admitted probe owner's admission instant, before it holds
/// ownership, so the harness can see the admission even for an owner that is
/// later killed. Best effort: a probe that cannot write its marker still
/// emits the admitted evidence line, and the harness's own assertions treat
/// a missing marker as a missing admission rather than as a pass.
fn record_probe_admission() {
    let dir = crate::startup_coordinator::resolve_config_dir().join(PROBE_ADMISSION_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let process_id = std::process::id();
    let admitted_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0);
    let _ = std::fs::write(
        dir.join(format!("admission-{process_id}.json")),
        format!("{{\"process_id\":{process_id},\"admitted_at_unix_ms\":{admitted_at_unix_ms}}}"),
    );
}

fn probe_hold_ms() -> u64 {
    std::env::var(PROBE_HOLD_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(PROBE_HOLD_DEFAULT_MS)
}

/// Emit the duplicate-suppression evidence once per rejected process:
/// a durable per-user log line with lifetime context, plus the
/// `openpiso-arbitration:` console line the process harness parses. Exactly
/// one line per rejected process is the rate bound; rejected processes
/// never start a network forwarder.
fn emit_duplicate_evidence(carried_report: bool, handoff_error: Option<&str>) {
    let handoff = match handoff_error {
        Some(_) => "failed",
        None if carried_report => "accepted",
        None => "none",
    };
    console_line(&format!(
        "duplicate-suppressed process_id={} launch={} handoff={handoff}",
        crate::diagnostics::process_id(),
        crate::diagnostics::launch_token(),
    ));
    with_local_tracing(|_guard| match handoff_error {
        Some(error) => tracing::warn!(
            target: "openpiso_client::startup",
            process_id = crate::diagnostics::process_id(),
            launch = crate::diagnostics::launch_token(),
            handoff,
            error = %error,
            "client startup arbitration: duplicate launch suppressed, but its Tamper Kill report could not be handed off durably"
        ),
        None => tracing::info!(
            target: "openpiso_client::startup",
            process_id = crate::diagnostics::process_id(),
            launch = crate::diagnostics::launch_token(),
            handoff,
            "client startup arbitration: duplicate launch suppressed; the running Client keeps the runtime"
        ),
    });
}

/// Emit the coordination-failure evidence: durable, with the underlying
/// error, and never posed as a benign duplicate.
fn emit_coordination_failure_evidence(error: &str) {
    console_line(&format!(
        "coordination-failure process_id={} launch={} error={error}",
        crate::diagnostics::process_id(),
        crate::diagnostics::launch_token(),
    ));
    with_local_tracing(|_guard| {
        tracing::warn!(
            target: "openpiso_client::startup",
            process_id = crate::diagnostics::process_id(),
            launch = crate::diagnostics::launch_token(),
            error,
            "client startup arbitration: coordination failed; no runtime was admitted"
        );
    });
}

/// Emit the probe owner's admission evidence through the same compact
/// convention.
fn emit_probe_admitted_evidence() {
    console_line(&format!(
        "admitted process_id={} launch={} object={}",
        crate::diagnostics::process_id(),
        crate::diagnostics::launch_token(),
        object_name(),
    ));
    with_local_tracing(|_guard| {
        tracing::info!(
            target: "openpiso_client::startup",
            process_id = crate::diagnostics::process_id(),
            launch = crate::diagnostics::launch_token(),
            object = %object_name(),
            "client startup arbitration: probe owner admitted"
        );
    });
}

/// One `openpiso-arbitration:` line on stderr. Stderr survives both the
/// harness's pipes and a GUI subsystem's absence of a console sink; the
/// durable copy of the same outcome goes to the per-user log.
fn console_line(line: &str) {
    eprintln!("openpiso-arbitration: {line}");
}

/// Initialize the durable local log just long enough to emit one evidence
/// line. The guard must be dropped before the caller's `std::process::exit`:
/// exiting never runs destructors, so a guard left alive would lose the
/// buffered line.
fn with_local_tracing(emit: impl FnOnce(&tracing_appender::non_blocking::WorkerGuard)) {
    match crate::diagnostics::init_tracing() {
        Ok(guard) => {
            emit(&guard);
            drop(guard);
        }
        Err(error) => {
            console_line(&format!("durable-evidence-unavailable error={error}"));
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::Mutex;

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{
                CloseHandle, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
            },
            Security::SECURITY_ATTRIBUTES,
            System::Threading::{
                CreateMutexW, OpenEventW, WaitForSingleObject, INFINITE,
                SYNCHRONIZATION_SYNCHRONIZE,
            },
        },
    };

    /// The result of one atomic check-and-own against the coordination
    /// object.
    pub(super) enum Acquisition {
        Owned,
        Busy,
        Failed(u32),
    }

    /// Handles of acquired ownership objects, retained for the lifetime of
    /// the acquiring thread — the thread that runs the whole Tauri event
    /// loop. A Win32 mutex is thread-scoped: if that thread ever exited
    /// while the process lived, the OS would abandon (release) the ownership
    /// regardless of this handle, which is exactly the lifetime contract —
    /// ownership never outlives the runtime that holds it. The handle is
    /// never closed and never inherited by a child.
    static RETAINED_OWNERSHIP: Mutex<Vec<isize>> = Mutex::new(Vec::new());
    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn security_attributes() -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            // Explicitly non-inheritable: WebView helpers, spawned children,
            // and test fixtures must not keep Client ownership alive.
            bInheritHandle: false.into(),
        }
    }

    /// Atomically check and take runtime ownership. `CreateMutexW` with a
    /// zero-timeout wait is one kernel-serialized step: a competing launch
    /// cannot slip between existence and ownership. `WAIT_ABANDONED` means
    /// the previous owner died — acquiring is the safe re-evaluation of
    /// ownership after owner death, with no stale file to clean up and no
    /// PID-reuse decision.
    pub(super) fn acquire(name: &str) -> Acquisition {
        let encoded = wide(name);
        let security = security_attributes();
        let handle = match unsafe { CreateMutexW(Some(&security), false, PCWSTR(encoded.as_ptr())) }
        {
            Ok(handle) => handle,
            Err(_) => return Acquisition::Failed(unsafe { GetLastError() }.0),
        };
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {
                retain_ownership(handle);
                Acquisition::Owned
            }
            WAIT_TIMEOUT => {
                // The running Client owns the runtime. Drop the loser's
                // open handle; ownership stays with the winner.
                let _ = unsafe { CloseHandle(handle) };
                Acquisition::Busy
            }
            _ => Acquisition::Failed(unsafe { GetLastError() }.0),
        }
    }

    /// `bInheritHandle = FALSE` is the non-inheritance contract; this keeps
    /// the acquired handle alive for the rest of the process.
    fn retain_ownership(handle: HANDLE) {
        RETAINED_OWNERSHIP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(handle.0 as isize);
    }

    /// The probe start barrier: block until the harness signals the named
    /// event, so real contenders race simultaneously. Only the test-only
    /// environment reaches this; a missing or malformed event must be
    /// visible, never silently ignored, or the harness's rounds would
    /// degenerate into unsynchronized races.
    pub(super) fn wait_for_named_event(name: &str) {
        let encoded = wide(name);
        let handle =
            unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(encoded.as_ptr())) };
        let Ok(handle) = handle else {
            eprintln!(
                "openpiso-arbitration: probe start barrier {name} could not be opened; proceeding without it"
            );
            return;
        };
        unsafe { WaitForSingleObject(handle, INFINITE) };
        let _ = unsafe { CloseHandle(handle) };
    }

    /// The production adapter creates its ownership handle with
    /// `bInheritHandle = FALSE`; this pins the contract so a future refactor
    /// cannot silently make Client ownership inheritable.
    #[cfg(test)]
    pub(super) fn owned_handle_is_inheritable(name: &str) -> bool {
        use windows::Win32::Foundation::{GetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
        let encoded = wide(name);
        let security = security_attributes();
        let handle = unsafe { CreateMutexW(Some(&security), false, PCWSTR(encoded.as_ptr())) };
        let Ok(handle) = handle else {
            return false;
        };
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {}
            _ => return false,
        }
        let mut flags = 0u32;
        let queried = unsafe { GetHandleInformation(handle, &mut flags) };
        let inheritable =
            queried.is_ok() && (HANDLE_FLAGS(flags) & HANDLE_FLAG_INHERIT) == HANDLE_FLAG_INHERIT;
        let _ = unsafe { CloseHandle(handle) };
        inheritable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn enforcement_policy_is_release_windows_only() {
        // Release Windows enforces unconditionally.
        assert_eq!(
            enforcement_from(false, None),
            Enforcement::Enforced,
            "a release Windows build must enforce the gate"
        );
        // A debug Windows build is inert unless the test override activates
        // the production gate.
        assert_eq!(enforcement_from(true, None), Enforcement::Inert);
        assert_eq!(
            enforcement_from(true, Some("1".into())),
            Enforcement::Enforced
        );
        assert_eq!(enforcement_from(true, Some("0".into())), Enforcement::Inert);
    }

    /// A non-Windows host never enforces, in either build profile and with
    /// or without the test override: there is no coordination object and the
    /// Client Guard Coverage behavior there stays `not_applicable`.
    #[cfg(not(windows))]
    #[test]
    fn enforcement_policy_is_inert_off_windows() {
        assert_eq!(enforcement_from(false, None), Enforcement::Inert);
        assert_eq!(enforcement_from(true, None), Enforcement::Inert);
        assert_eq!(
            enforcement_from(false, Some("1".into())),
            Enforcement::Inert
        );
        assert_eq!(enforcement_from(true, Some("1".into())), Enforcement::Inert);
    }

    /// Off Windows the gate admits every launch, so `tauri dev` and multiple
    /// development instances are untouched by #369.
    #[cfg(not(windows))]
    #[test]
    fn non_windows_coordination_admits_every_launch() {
        assert_eq!(
            coordinate(&["openpiso-client".to_owned()]),
            Outcome::Admitted
        );
    }

    /// Serializes every test that reads or mutates the name-override
    /// environment variable, so parallel test threads cannot race
    /// `object_name()`.
    fn name_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static NAME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        NAME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One unique isolated object name for the adapter tests.
    #[cfg(windows)]
    fn unique_test_object(label: &str) -> String {
        format!(
            "Local\\openpiso-unit-arbitration-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or(0)
        )
    }

    #[test]
    fn production_object_name_is_machine_wide_and_stable() {
        let _env = name_env_lock();
        assert!(
            RUNTIME_OBJECT_NAME.starts_with("Global\\"),
            "the coordination object must be machine-wide, got {RUNTIME_OBJECT_NAME}"
        );
        let leaf = RUNTIME_OBJECT_NAME.trim_start_matches("Global\\");
        assert!(
            !leaf.contains('\\')
                && !leaf.contains(".exe")
                && !leaf.contains('%')
                && !leaf.contains(std::path::MAIN_SEPARATOR),
            "the coordination name must not depend on the executable, environment, or path"
        );
        // Without the test override the launch resolves the fixed production
        // identity, so working directory, shortcut, rename, or version cannot
        // admit a second participating runtime.
        assert_eq!(object_name(), RUNTIME_OBJECT_NAME);
    }

    #[test]
    fn the_name_override_is_harness_isolation_only() {
        let _env = name_env_lock();
        std::env::set_var(NAME_OVERRIDE_ENV, "Local\\harness-isolated");
        assert_eq!(object_name(), "Local\\harness-isolated");
        std::env::remove_var(NAME_OVERRIDE_ENV);
        assert_eq!(object_name(), RUNTIME_OBJECT_NAME);
    }

    #[cfg(windows)]
    #[test]
    fn acquisitions_map_to_the_three_outcomes() {
        let report_args = vec![
            "openpiso-client.exe".to_owned(),
            crate::startup_coordinator::REPORT_FLAG.to_owned(),
            "1750000000000".to_owned(),
            "3".to_owned(),
        ];
        assert_eq!(
            map_acquisition(platform::Acquisition::Owned, &report_args),
            Outcome::Admitted
        );
        assert_eq!(
            map_acquisition(platform::Acquisition::Busy, &report_args),
            Outcome::Duplicate(parse_report_args(&report_args))
        );
        // Malformed report arguments on a loser can manufacture no report.
        let malformed = vec![
            "openpiso-client.exe".to_owned(),
            crate::startup_coordinator::REPORT_FLAG.to_owned(),
            "not-a-number".to_owned(),
        ];
        assert_eq!(
            map_acquisition(platform::Acquisition::Busy, &malformed),
            Outcome::Duplicate(None)
        );
        // An uncertain acquisition is a truthful failure with the error
        // preserved.
        let mapped = map_acquisition(platform::Acquisition::Failed(5), &report_args);
        match mapped {
            Outcome::CoordinationFailure(error) => {
                assert!(
                    error.contains("5"),
                    "the Win32 error code must survive: {error}"
                );
            }
            other => {
                panic!("a failed acquisition must not admit or pose as a duplicate: {other:?}")
            }
        }
    }

    #[test]
    fn duplicate_outcomes_exit_success_and_failures_exit_nonzero() {
        assert_eq!(
            handle_early_outcome(&Outcome::Admitted),
            EXIT_DUPLICATE_HANDLED
        );
        assert_eq!(
            handle_early_outcome(&Outcome::Duplicate(None)),
            EXIT_DUPLICATE_HANDLED
        );
        assert_eq!(
            handle_early_outcome(&Outcome::CoordinationFailure("denied".to_owned())),
            EXIT_COORDINATION_FAILURE,
            "a coordination failure must not pose as a handled duplicate"
        );
    }

    #[test]
    fn probe_launch_detection_matches_only_the_probe_flag() {
        assert!(is_probe_launch(&[
            "openpiso-client.exe".to_owned(),
            PROBE_FLAG.to_owned()
        ]));
        assert!(!is_probe_launch(&["openpiso-client.exe".to_owned()]));
        assert!(!is_probe_launch(&[
            "openpiso-client.exe".to_owned(),
            crate::startup_coordinator::HANDOFF_ONLY_FLAG.to_owned()
        ]));
    }

    #[cfg(windows)]
    #[test]
    fn owned_ownership_handle_is_not_inheritable() {
        let name = unique_test_object("inherit");
        assert!(
            !platform::owned_handle_is_inheritable(&name),
            "the production adapter must create a non-inheritable ownership handle"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_second_contender_in_the_same_process_finds_the_object_busy() {
        let name = unique_test_object("busy");
        assert!(matches!(
            platform::acquire(&name),
            platform::Acquisition::Owned
        ));
        // Mutex ownership is per-thread: a second thread's acquire is busy.
        let waiter_name = name.clone();
        let second = std::thread::spawn(move || platform::acquire(&waiter_name));
        assert!(matches!(
            second.join().expect("join contender"),
            platform::Acquisition::Busy
        ));
    }
}

/// Central Terminal Autostart port (ADR-0055).
///
/// The `AutostartPort` trait exposes the host machine's real Autostart
/// state — whether the admin binary is registered to launch at Windows
/// logon via the per-user `Run` key. The registry is the sole source of
/// truth; there is no SQLite mirror.
///
/// Two implementations exist:
///
/// - `PluginAutostart` — production wrapper around `auto_launch::AutoLaunch`.
/// - `FakeAutostart` — in-memory fake for HTTP and component tests.
use auto_launch::{AutoLaunch, AutoLaunchBuilder};

use crate::errors::AppError;

/// Host-level Autostart state for the Central Terminal.
pub trait AutostartPort: Send + Sync {
    /// Whether the Central Terminal is currently registered to launch at logon.
    fn is_enabled(&self) -> bool;

    /// Set whether the Central Terminal is registered to launch at logon.
    fn set_enabled(&self, enabled: bool) -> Result<(), AppError>;

    /// Whether the platform supports Autostart (Windows, macOS, Linux).
    fn supported(&self) -> bool;

    /// Whether the current build may edit the Autostart configuration.
    /// `false` in debug builds to prevent stale Run-key entries pointing
    /// into a build tree (ADR-0055).
    fn editable(&self) -> bool;

    /// The executable path that is (or would be) registered. Always
    /// `current_exe()` regardless of the enabled state.
    fn exe_path(&self) -> String;
}

/// Production Autostart implementation backed by `auto_launch::AutoLaunch`.
pub struct PluginAutostart {
    inner: AutoLaunch,
    exe_path: String,
}

impl PluginAutostart {
    /// Create a new `PluginAutostart` targeting the current executable.
    pub fn new() -> Result<Self, AppError> {
        let exe_path = std::env::current_exe().map_err(|e| {
            AppError::InvalidInput(format!("cannot resolve current executable path: {e}"))
        })?;
        let exe_str = exe_path.to_string_lossy().to_string();
        let app_name = env!("CARGO_PKG_NAME").to_string();

        let mut builder = AutoLaunchBuilder::new();
        builder.set_app_name(&app_name);
        builder.set_app_path(&exe_str);
        let inner = builder
            .build()
            .map_err(|e| AppError::InvalidInput(format!("autostart setup failed: {e}")))?;

        Ok(Self {
            inner,
            exe_path: exe_str,
        })
    }
}

impl AutostartPort for PluginAutostart {
    fn is_enabled(&self) -> bool {
        self.inner.is_enabled().unwrap_or(false)
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), AppError> {
        if enabled {
            self.inner
                .enable()
                .map_err(|e| AppError::InvalidInput(format!("failed to enable autostart: {e}")))
        } else {
            self.inner
                .disable()
                .map_err(|e| AppError::InvalidInput(format!("failed to disable autostart: {e}")))
        }
    }

    fn supported(&self) -> bool {
        auto_launch::AutoLaunch::is_support()
    }

    fn editable(&self) -> bool {
        !cfg!(debug_assertions)
    }

    fn exe_path(&self) -> String {
        self.exe_path.clone()
    }
}

/// In-memory fake for tests. Never touches the registry.
pub struct FakeAutostart {
    enabled: std::sync::Mutex<bool>,
    exe_path: String,
    fail_writes: std::sync::atomic::AtomicBool,
}

impl FakeAutostart {
    pub fn new(enabled: bool, exe_path: impl Into<String>) -> Self {
        Self {
            enabled: std::sync::Mutex::new(enabled),
            exe_path: exe_path.into(),
            fail_writes: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Directly set the enabled state for test setup without going through
    /// the port interface.
    pub fn set_enabled_for_test(&self, enabled: bool) {
        *self.enabled.lock().unwrap() = enabled;
    }

    /// When `true`, `set_enabled` returns an error and does not change the
    /// stored state — simulating a failed underlying OS write.
    pub fn set_fail_writes(&self, fail: bool) {
        self.fail_writes
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }
}

impl AutostartPort for FakeAutostart {
    fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), AppError> {
        if self.fail_writes.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(AppError::InvalidInput(
                "simulated autostart write failure".to_owned(),
            ));
        }
        *self.enabled.lock().unwrap() = enabled;
        Ok(())
    }

    fn supported(&self) -> bool {
        true
    }

    fn editable(&self) -> bool {
        true
    }

    fn exe_path(&self) -> String {
        self.exe_path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_autostart_reports_configured_state() {
        let enabled = FakeAutostart::new(true, "/test/openpiso-admin.exe");
        assert!(enabled.is_enabled());
        assert!(enabled.supported());
        assert!(enabled.editable());
        assert_eq!(enabled.exe_path(), "/test/openpiso-admin.exe");

        let disabled = FakeAutostart::new(false, "/test/other.exe");
        assert!(!disabled.is_enabled());
        assert_eq!(disabled.exe_path(), "/test/other.exe");
    }

    #[test]
    fn fake_autostart_set_enabled_updates_in_memory() {
        let autostart = FakeAutostart::new(false, "/test/openpiso-admin.exe");
        assert!(!autostart.is_enabled());

        autostart.set_enabled(true).expect("enable should succeed");
        assert!(autostart.is_enabled());

        autostart
            .set_enabled(false)
            .expect("disable should succeed");
        assert!(!autostart.is_enabled());
    }

    #[test]
    fn fake_autostart_set_enabled_is_idempotent() {
        let autostart = FakeAutostart::new(true, "/test/openpiso-admin.exe");
        autostart.set_enabled(true).expect("double-enable");
        assert!(autostart.is_enabled());

        autostart.set_enabled(false).expect("disable");
        autostart.set_enabled(false).expect("double-disable");
        assert!(!autostart.is_enabled());
    }

    #[test]
    fn fake_autostart_fail_writes_leaves_state_unchanged() {
        let autostart = FakeAutostart::new(false, "/test/openpiso-admin.exe");
        autostart.set_fail_writes(true);

        let result = autostart.set_enabled(true);
        assert!(result.is_err());
        assert!(!autostart.is_enabled(), "state must not change on failure");

        // Toggle fail off and verify the write now works.
        autostart.set_fail_writes(false);
        autostart.set_enabled(true).expect("should now succeed");
        assert!(autostart.is_enabled());
    }

    #[test]
    fn plugin_autostart_targets_the_running_executable() {
        let autostart = PluginAutostart::new().expect("plugin autostart");
        let current = std::env::current_exe().expect("current exe");
        assert_eq!(autostart.exe_path(), current.to_string_lossy());
    }

    #[test]
    fn editable_is_false_in_debug_builds() {
        let autostart = PluginAutostart::new().expect("plugin autostart");
        #[cfg(debug_assertions)]
        assert!(!autostart.editable());
        #[cfg(not(debug_assertions))]
        assert!(autostart.editable());
    }
}

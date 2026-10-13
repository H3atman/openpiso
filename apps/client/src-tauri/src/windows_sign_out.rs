//! Optional Windows policy hardening for the owner-controlled sign-out toggle.
//!
//! The registry/elevation adapter is compiled only for release Windows builds.
//! The desired policy itself stays platform-independent so it can be tested
//! without mutating a developer machine.

#[cfg(any(test, all(windows, not(debug_assertions))))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryHive {
    CurrentUser,
    LocalMachine,
}

#[cfg(any(test, all(windows, not(debug_assertions))))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryValue {
    pub hive: RegistryHive,
    pub key: &'static str,
    pub name: &'static str,
    pub value: u32,
}

#[cfg(any(test, all(windows, not(debug_assertions))))]
const USER_NO_LOGOFF: RegistryValue = RegistryValue {
    hive: RegistryHive::CurrentUser,
    key: r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer",
    name: "NoLogoff",
    value: 1,
};
#[cfg(any(test, all(windows, not(debug_assertions))))]
const USER_DISABLE_LOCK: RegistryValue = RegistryValue {
    hive: RegistryHive::CurrentUser,
    key: r"Software\Microsoft\Windows\CurrentVersion\Policies\System",
    name: "DisableLockWorkstation",
    value: 1,
};
#[cfg(any(test, all(windows, not(debug_assertions))))]
const MACHINE_HIDE_FAST_USER_SWITCHING: RegistryValue = RegistryValue {
    hive: RegistryHive::LocalMachine,
    key: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
    name: "HideFastUserSwitching",
    value: 1,
};

#[cfg(any(test, all(windows, not(debug_assertions))))]
const WRITTEN_VALUES: [RegistryValue; 3] = [
    USER_NO_LOGOFF,
    USER_DISABLE_LOCK,
    MACHINE_HIDE_FAST_USER_SWITCHING,
];

/// Return the exact Windows policy values owned by the toggle.
///
/// `false` means stock Windows behavior: the adapter deletes only the values
/// listed for the enabled state. It deliberately does not include power-option
/// policies, so Restart and Shut down remain available in Ctrl+Alt+Del.
#[cfg(any(test, all(windows, not(debug_assertions))))]
pub fn desired_registry_state(enabled: bool) -> Vec<RegistryValue> {
    if enabled {
        WRITTEN_VALUES.to_vec()
    } else {
        Vec::new()
    }
}

pub fn apply(enabled: bool) -> Result<(), String> {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        windows_impl::apply(enabled)
    }

    #[cfg(not(all(windows, not(debug_assertions))))]
    {
        let _ = enabled;
        Err("Disable Windows sign-out is available only in a release Windows build".to_owned())
    }
}

#[cfg(all(windows, not(debug_assertions)))]
mod windows_impl {
    use super::{desired_registry_state, RegistryHive, RegistryValue, WRITTEN_VALUES};
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, process::Command};

    use windows::Win32::{
        Foundation::{CloseHandle, HWND},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
            WindowsAndMessaging::SW_HIDE,
        },
    };

    fn run_reg(arguments: &[&str]) -> Result<(), String> {
        let output = Command::new("reg.exe")
            .args(arguments)
            .output()
            .map_err(|error| format!("could not run Windows registry tool: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if detail.is_empty() {
            format!("Windows registry tool exited with {}", output.status)
        } else {
            detail
        })
    }

    fn registry_path(value: RegistryValue) -> String {
        let hive = match value.hive {
            RegistryHive::CurrentUser => "HKCU",
            RegistryHive::LocalMachine => "HKLM",
        };
        format!(r"{hive}\{}", value.key)
    }

    fn add_user_value(value: RegistryValue) -> Result<(), String> {
        let path = registry_path(value);
        run_reg(&[
            "ADD",
            &path,
            "/v",
            value.name,
            "/t",
            "REG_DWORD",
            "/d",
            &value.value.to_string(),
            "/f",
        ])
    }

    fn delete_user_value(value: RegistryValue) -> Result<(), String> {
        let path = registry_path(value);
        match run_reg(&["DELETE", &path, "/v", value.name, "/f"]) {
            Ok(()) => Ok(()),
            Err(error) if error.contains("unable to find") || error.contains("cannot find") => {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Run one machine-level `reg.exe` operation through the Windows UAC
    /// consent UI. There is exactly one machine value, so one enable/disable
    /// action produces one prompt.
    fn run_elevated_reg(arguments: &[&str]) -> Result<(), String> {
        let file = wide("reg.exe");
        let parameters = wide(&arguments.join(" "));
        let verb = wide("runas");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: HWND::default(),
            lpVerb: windows::core::PCWSTR(verb.as_ptr()),
            lpFile: windows::core::PCWSTR(file.as_ptr()),
            lpParameters: windows::core::PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe {
            ShellExecuteExW(&mut info).map_err(|error| {
                if error.code().0 == -2147023673 {
                    "Windows UAC consent was declined; the sign-out protection was not changed"
                        .to_owned()
                } else {
                    format!("could not request Windows UAC consent: {error}")
                }
            })?;
            let process = info.hProcess;
            if process.is_invalid() {
                return Err("Windows UAC did not return a registry process".to_owned());
            }
            WaitForSingleObject(process, INFINITE);
            let mut exit_code = 1;
            GetExitCodeProcess(process, &mut exit_code)
                .map_err(|error| format!("could not read elevated registry result: {error}"))?;
            CloseHandle(process).ok();
            if exit_code == 0 {
                Ok(())
            } else {
                Err(format!(
                    "elevated Windows registry tool exited with code {exit_code}"
                ))
            }
        }
    }

    fn apply_machine_value(value: RegistryValue) -> Result<(), String> {
        let path = registry_path(value);
        run_elevated_reg(&[
            "ADD",
            &path,
            "/v",
            value.name,
            "/t",
            "REG_DWORD",
            "/d",
            &value.value.to_string(),
            "/f",
        ])
    }

    fn delete_machine_value(value: RegistryValue) -> Result<(), String> {
        let path = registry_path(value);
        match run_elevated_reg(&["DELETE", &path, "/v", value.name, "/f"]) {
            Ok(()) => Ok(()),
            Err(error) if error.contains("unable to find") || error.contains("cannot find") => {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn apply(enabled: bool) -> Result<(), String> {
        let machine_value = WRITTEN_VALUES[2];
        if !enabled {
            for value in desired_registry_state(true)
                .into_iter()
                .filter(|value| value.hive == RegistryHive::CurrentUser)
            {
                delete_user_value(value)?;
            }
            if let Err(error) = delete_machine_value(machine_value) {
                // Keep the user-level policies in place if the owner declines
                // the only elevated operation; protection must not half-disable.
                for value in WRITTEN_VALUES
                    .into_iter()
                    .filter(|value| value.hive == RegistryHive::CurrentUser)
                {
                    let _ = add_user_value(value);
                }
                return Err(error);
            }
            Ok(())
        } else {
            for value in desired_registry_state(true)
                .into_iter()
                .filter(|value| value.hive == RegistryHive::CurrentUser)
            {
                add_user_value(value)?;
            }
            if let Err(error) = apply_machine_value(machine_value) {
                for value in WRITTEN_VALUES
                    .into_iter()
                    .filter(|value| value.hive == RegistryHive::CurrentUser)
                {
                    let _ = delete_user_value(value);
                }
                return Err(error);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_state_is_stock_windows_with_no_owned_values() {
        assert!(desired_registry_state(false).is_empty());
    }

    #[test]
    fn enabled_state_contains_only_the_three_required_policies() {
        assert_eq!(
            desired_registry_state(true),
            vec![
                RegistryValue {
                    hive: RegistryHive::CurrentUser,
                    key: r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer",
                    name: "NoLogoff",
                    value: 1,
                },
                RegistryValue {
                    hive: RegistryHive::CurrentUser,
                    key: r"Software\Microsoft\Windows\CurrentVersion\Policies\System",
                    name: "DisableLockWorkstation",
                    value: 1,
                },
                RegistryValue {
                    hive: RegistryHive::LocalMachine,
                    key: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
                    name: "HideFastUserSwitching",
                    value: 1,
                },
            ]
        );
    }

    #[test]
    fn enabled_state_does_not_disable_power_options() {
        let names: Vec<_> = desired_registry_state(true)
            .into_iter()
            .map(|value| value.name)
            .collect();
        assert!(!names.contains(&"NoClose"));
        assert!(!names.contains(&"ShutdownWithoutLogon"));
    }
}

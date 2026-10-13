//! App-level soft kiosk hardening (ADR-0028, plan §7).
//!
//! A Windows low-level keyboard hook (`WH_KEYBOARD_LL`) swallows the common
//! escape chords — Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+F4 — **but only while
//! the Fullscreen Lock is showing**. Once the customer has paid time and is in
//! the Widget / Mini Bar they use the PC normally, so the hook must not block
//! those chords during a session, only while the Lock blocks. The frontend
//! flips `set_lock_active` from its §4.1 view machine.
//!
//! The single exception is **Alt+F4 while the Client is the foreground
//! window**, which is swallowed in every view mode: it is the one chord that
//! kills the Client from the Widget or the Mini Bar. Because the hook is
//! global, the foreground test is what keeps the customer able to Alt+F4 their
//! own windows during a paid session.
//!
//! This is the *secondary*, best-effort layer. The robust lockdown is the
//! Assigned Access / Shell Launcher v2 provisioning (ADR-0028), documented in
//! `docs/installation.md`. Ctrl+Alt+Del (the Secure Attention Sequence) can
//! never be swallowed by app code and is out of reach here by design.
//!
//! Like `execute_os_shutdown`, the real hook installs only in a **release
//! Windows** build, so a developer is never trapped by the Lock during `tauri
//! dev`; the pure `should_block_key` decision is unit-tested on every platform.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the Lock is currently the active view. The hook reads this to
/// decide whether to swallow escape chords; it starts `false` so nothing is
/// blocked until the frontend asserts the Lock.
static LOCK_ACTIVE: AtomicBool = AtomicBool::new(false);

// Virtual-key codes we care about (kept as raw constants so the decision
// function and its tests compile on every platform, independent of the
// `windows` crate). See the Win32 "Virtual-Key Codes" reference. These — and
// `should_block_key` — are consumed only by the release-Windows hook and the
// unit tests (the test module and the hook are both children of this module,
// so they reach these private items directly), so they are compiled only for
// those configs to stay dead-code clean in a plain debug/non-Windows build.
#[cfg(any(test, all(windows, not(debug_assertions))))]
const VK_TAB: u32 = 0x09;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const VK_ESCAPE: u32 = 0x1B;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const VK_F4: u32 = 0x73;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const VK_LWIN: u32 = 0x5B;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const VK_RWIN: u32 = 0x5C;

/// Pure decision: should the hook swallow this key event?
///
/// Two scopes, because `WH_KEYBOARD_LL` is a *global* hook — whatever it
/// swallows is swallowed for every window on the desktop, not just ours:
///
/// - **Alt+F4 is blocked in every view mode**, but only while the Client is the
///   foreground window. Alt+F4 closes whatever has focus, so it is the one
///   escape chord that survives into the Widget and Mini Bar; scoping it to the
///   foreground keeps the paying customer able to Alt+F4 their own browser and
///   games during a session. This is belt-and-braces over the `WM_CLOSE`
///   refusal in `shutdown::should_allow_close`, which already covers Alt+F4.
/// - **The full chord set is blocked only while the Lock is active**: either
///   Windows key (opens Start / switches shell focus), Alt+Tab (window switch),
///   Alt+Esc (cycle windows), Ctrl+Esc (Start without the Windows key), and
///   Alt+F4 regardless of foreground. During a paid session the customer uses
///   the PC normally, so these all pass through.
///
/// A bare Tab / Esc / F4 (no modifier) is left alone so the Lock's own form
/// controls keep working.
#[cfg(any(test, all(windows, not(debug_assertions))))]
pub fn should_block_key(
    vk_code: u32,
    alt_down: bool,
    ctrl_down: bool,
    lock_active: bool,
    client_foreground: bool,
) -> bool {
    if vk_code == VK_F4 && alt_down && client_foreground {
        return true;
    }
    if !lock_active {
        return false;
    }
    match vk_code {
        VK_LWIN | VK_RWIN => true,
        VK_TAB => alt_down,
        VK_ESCAPE => alt_down || ctrl_down,
        VK_F4 => alt_down,
        _ => false,
    }
}

/// Tell the hook whether the Lock is currently showing. Called from the
/// `set_kiosk_lock_active` Tauri command as the frontend's §4.1 view machine
/// enters / leaves the Lock. Cheap and lock-free, safe to call on every view
/// change.
pub fn set_lock_active(active: bool) {
    LOCK_ACTIVE.store(active, Ordering::Relaxed);
}

/// Install the low-level keyboard hook. Real install happens only on a release
/// Windows build (mirroring `execute_os_shutdown`); other targets/builds log
/// once and no-op so dev and non-Windows hosts are never affected.
pub fn start() {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        windows_impl::install();
    }
    #[cfg(not(all(windows, not(debug_assertions))))]
    {
        tracing::info!(
            target: "openpiso_client::kiosk_hardening",
            "keyboard-hook hardening suppressed (debug or non-Windows build); a release Windows build would install it"
        );
    }
}

#[cfg(all(windows, not(debug_assertions)))]
mod windows_impl {
    use super::{should_block_key, LOCK_ACTIVE, VK_F4};
    use std::sync::atomic::Ordering;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW,
        GetWindowThreadProcessId, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
        MSG, WH_KEYBOARD_LL,
    };

    /// Spawn the dedicated thread that owns the hook. A low-level keyboard
    /// hook requires the installing thread to pump a message loop, so this
    /// thread installs the hook and then blocks in `GetMessageW` for the app's
    /// lifetime.
    pub fn install() {
        std::thread::Builder::new()
            .name("openpiso-kiosk-hook".to_owned())
            .spawn(|| unsafe {
                let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                    Ok(hook) => hook,
                    Err(error) => {
                        tracing::warn!(
                            target: "openpiso_client::kiosk_hardening",
                            "could not install keyboard hook: {error}; relying on Assigned Access provisioning"
                        );
                        return;
                    }
                };
                tracing::info!(
                    target: "openpiso_client::kiosk_hardening",
                    "keyboard-hook hardening active"
                );
                let mut message = MSG::default();
                // The hook callback fires on this thread; the loop only needs
                // to keep it pumping. `GetMessageW` returns <= 0 on WM_QUIT /
                // error, at which point we let the thread (and hook) end.
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    let _ = DispatchMessageW(&message);
                }
                let _ = hook;
            })
            .ok();
    }

    /// Whether the foreground window belongs to this process. The Client owns
    /// exactly one window (ADR-0029 §4.1), so comparing the owning process is
    /// equivalent to comparing the `HWND` and needs nothing captured at setup.
    /// Any failure to resolve the owner answers `false`, so an unreadable
    /// foreground never widens what the global hook swallows.
    fn client_is_foreground() -> bool {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.0.is_null() {
                return false;
            }
            let mut process_id: u32 = 0;
            if GetWindowThreadProcessId(foreground, Some(&mut process_id)) == 0 {
                return false;
            }
            process_id != 0 && process_id == GetCurrentProcessId()
        }
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let lock_active = LOCK_ACTIVE.load(Ordering::Relaxed);
            let alt_down = info.flags.0 & LLKHF_ALTDOWN.0 != 0;
            // The low-level struct does not carry Ctrl state, so read it live.
            let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            // Only Alt+F4 consults the foreground, so only Alt+F4 pays for the
            // two Win32 calls — every other keystroke skips them.
            let client_foreground = info.vkCode == VK_F4 && alt_down && client_is_foreground();
            if should_block_key(
                info.vkCode,
                alt_down,
                ctrl_down,
                lock_active,
                client_foreground,
            ) {
                // Swallow: returning a non-zero value stops the chord from
                // reaching the shell or any other window.
                return LRESULT(1);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_blocked_while_the_lock_is_inactive_and_we_are_not_foreground() {
        // During a paid session the customer uses the PC normally — even the
        // escape chords pass through.
        assert!(!should_block_key(VK_LWIN, false, false, false, false));
        assert!(!should_block_key(VK_TAB, true, false, false, false));
        assert!(!should_block_key(VK_F4, true, false, false, false));
    }

    #[test]
    fn escape_chords_are_blocked_only_with_their_modifier_on_the_lock() {
        // Windows keys are always escape attempts on the Lock.
        assert!(should_block_key(VK_LWIN, false, false, true, false));
        assert!(should_block_key(VK_RWIN, false, false, true, false));
        // Alt+Tab / Alt+Esc / Alt+F4 need Alt; Ctrl+Esc needs Ctrl.
        assert!(should_block_key(VK_TAB, true, false, true, false));
        assert!(should_block_key(VK_ESCAPE, true, false, true, false));
        assert!(should_block_key(VK_ESCAPE, false, true, true, false));
        assert!(should_block_key(VK_F4, true, false, true, false));
    }

    #[test]
    fn bare_keys_pass_through_so_lock_form_controls_keep_working() {
        // Without a modifier these are ordinary keys the Lock's inputs use.
        assert!(!should_block_key(VK_TAB, false, false, true, true));
        assert!(!should_block_key(VK_ESCAPE, false, false, true, true));
        assert!(!should_block_key(VK_F4, false, false, true, true));
        // An unrelated key (e.g. 'A' = 0x41) is never blocked.
        assert!(!should_block_key(0x41, true, true, true, true));
    }

    #[test]
    fn alt_f4_is_blocked_in_every_view_mode_when_the_client_is_foreground() {
        // Widget / Mini Bar: the Lock is down, but Alt+F4 on our own window
        // would still kill the Client, so it is swallowed.
        assert!(should_block_key(VK_F4, true, false, false, true));
    }

    #[test]
    fn the_customer_can_still_alt_f4_their_own_windows_during_a_session() {
        // The hook is global. With the Lock down and the customer's browser or
        // game in the foreground, Alt+F4 must reach it untouched.
        assert!(!should_block_key(VK_F4, true, false, false, false));
        // Foreground alone is not a blocking reason for anything else — the
        // customer can still leave the Widget with Alt+Tab or the Windows key.
        assert!(!should_block_key(VK_TAB, true, false, false, true));
        assert!(!should_block_key(VK_LWIN, false, false, false, true));
        assert!(!should_block_key(VK_ESCAPE, false, true, false, true));
    }
}

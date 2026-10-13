//! Native startup placeholder shown while WebView2 cold-starts.
//!
//! The real window exists only in an installed release build on Windows. It
//! starts before Tauri creates WebView2, covers the virtual desktop, ignores
//! ordinary close requests, and yields only after the frontend-ready command
//! has successfully revealed the Tauri Lock.

#[cfg(any(test, all(windows, not(debug_assertions))))]
const WM_CLOSE: u32 = 0x0010;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const WM_DESTROY: u32 = 0x0002;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const WM_PAINT: u32 = 0x000F;
#[cfg(any(test, all(windows, not(debug_assertions))))]
const WM_APP_CLOSE_PLACEHOLDER: u32 = 0x8001;

#[cfg(any(test, all(windows, not(debug_assertions))))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageAction {
    Paint,
    Ignore,
    Destroy,
    Quit,
    Default,
}

#[cfg(any(test, all(windows, not(debug_assertions))))]
fn message_action(message: u32) -> MessageAction {
    match message {
        WM_PAINT => MessageAction::Paint,
        WM_CLOSE => MessageAction::Ignore,
        WM_APP_CLOSE_PLACEHOLDER => MessageAction::Destroy,
        WM_DESTROY => MessageAction::Quit,
        _ => MessageAction::Default,
    }
}

#[cfg(all(windows, not(debug_assertions)))]
mod platform {
    use super::{message_action, MessageAction, WM_APP_CLOSE_PLACEHOLDER};
    use std::{
        mem::MaybeUninit,
        sync::{mpsc, Mutex},
        thread::{self, JoinHandle},
    };
    use windows::{
        core::w,
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
                FillRect, SelectObject, SetBkMode, SetTextColor, UpdateWindow, CLEARTYPE_QUALITY,
                CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_BOTTOM, DT_CENTER,
                DT_SINGLELINE, DT_TOP, FW_BOLD, HGDIOBJ, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
                TRANSPARENT,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                HiDpi::{
                    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                    GetClientRect, GetMessageW, GetSystemMetrics, PostMessageW, PostQuitMessage,
                    RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow,
                    TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, MSG,
                    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
                    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_SHOW,
                    WINDOW_EX_STYLE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
                },
            },
        },
    };

    const BACKGROUND: COLORREF = COLORREF(0x000A_0A0A);
    const FOREGROUND: COLORREF = COLORREF(0x00FA_FAFA);
    const MUTED: COLORREF = COLORREF(0x0070_7070);

    struct PlaceholderThread {
        hwnd: isize,
        thread: JoinHandle<()>,
    }

    static PLACEHOLDER: Mutex<Option<PlaceholderThread>> = Mutex::new(None);

    pub fn start() -> Result<(), String> {
        // This is the process's first HWND. Tao normally sets Per-Monitor-V2
        // during its event-loop initialization, which is too late once this
        // window exists; establish the same context first so mixed-DPI virtual
        // desktop dimensions and Tauri geometry stay physical-pixel correct.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .map_err(|error| format!("could not enable Per-Monitor-V2 DPI awareness: {error}"))?;

        let mut placeholder = PLACEHOLDER
            .lock()
            .map_err(|_| "startup placeholder state lock poisoned".to_owned())?;
        if placeholder.is_some() {
            return Ok(());
        }

        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("openpiso-startup-placeholder".to_owned())
            .spawn(move || run_window(ready_tx))
            .map_err(|error| format!("could not start placeholder UI thread: {error}"))?;

        let hwnd = match ready_rx.recv() {
            Ok(Ok(hwnd)) => hwnd,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(error) => {
                let _ = thread.join();
                return Err(format!(
                    "placeholder UI thread exited before showing a window: {error}"
                ));
            }
        };

        *placeholder = Some(PlaceholderThread { hwnd, thread });
        Ok(())
    }

    pub fn is_visible() -> bool {
        PLACEHOLDER.lock().is_ok_and(|placeholder| {
            placeholder
                .as_ref()
                .is_some_and(|placeholder| !placeholder.thread.is_finished())
        })
    }

    pub fn reassert_topmost() -> Result<(), String> {
        let state = PLACEHOLDER
            .lock()
            .map_err(|_| "startup placeholder state lock poisoned".to_owned())?;
        let Some(placeholder) = state.as_ref() else {
            return Ok(());
        };
        unsafe {
            SetWindowPos(
                HWND(placeholder.hwnd as *mut _),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .map_err(|error| format!("could not keep startup placeholder topmost: {error}"))
        }
    }

    pub fn handoff() -> Result<(), String> {
        let mut state = PLACEHOLDER
            .lock()
            .map_err(|_| "startup placeholder state lock poisoned".to_owned())?;
        let Some(placeholder) = state.as_ref() else {
            return Ok(());
        };

        if placeholder.thread.is_finished() {
            let Some(placeholder) = state.take() else {
                return Err("startup placeholder state changed during handoff".to_owned());
            };
            let _ = placeholder.thread.join();
            return Err("startup placeholder exited before frontend handoff".to_owned());
        }

        unsafe {
            PostMessageW(
                Some(HWND(placeholder.hwnd as *mut _)),
                WM_APP_CLOSE_PLACEHOLDER,
                WPARAM(0),
                LPARAM(0),
            )
            .map_err(|error| format!("could not close startup placeholder: {error}"))?;
        }
        let Some(placeholder) = state.take() else {
            return Err("startup placeholder state changed during handoff".to_owned());
        };
        drop(state);
        placeholder
            .thread
            .join()
            .map_err(|_| "startup placeholder UI thread panicked during handoff".to_owned())
    }

    fn run_window(ready_tx: mpsc::SyncSender<Result<isize, String>>) {
        let result = unsafe { create_window() };
        let hwnd = match result {
            Ok(hwnd) => hwnd,
            Err(error) => {
                let _ = ready_tx.send(Err(error));
                return;
            }
        };
        if ready_tx.send(Ok(hwnd.0 as isize)).is_err() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }

        let mut message = MSG::default();
        loop {
            let outcome = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if outcome.0 <= 0 {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    unsafe fn create_window() -> Result<HWND, String> {
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| format!("GetModuleHandleW failed: {error}"))?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("OpenPisoStartupPlaceholder"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(format!(
                "RegisterClassW failed: {}",
                windows::core::Error::from_win32()
            ));
        }

        let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if width <= 0 || height <= 0 {
            return Err(format!(
                "Windows reported an invalid virtual desktop size {width}x{height}"
            ));
        }

        let extended_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0);
        let hwnd = unsafe {
            CreateWindowExW(
                extended_style,
                w!("OpenPisoStartupPlaceholder"),
                w!("OpenPiso Client"),
                WS_POPUP,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|error| format!("CreateWindowExW failed: {error}"))?;

        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_SHOWWINDOW,
            )
            .map_err(|error| format!("SetWindowPos failed: {error}"))?;
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(hwnd)
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message_action(message) {
            MessageAction::Paint => {
                unsafe { paint(hwnd) };
                LRESULT(0)
            }
            MessageAction::Ignore => LRESULT(0),
            MessageAction::Destroy => {
                let _ = unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            MessageAction::Quit => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            MessageAction::Default => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    unsafe fn paint(hwnd: HWND) {
        let mut paint = MaybeUninit::<PAINTSTRUCT>::zeroed();
        let hdc = unsafe { BeginPaint(hwnd, paint.as_mut_ptr()) };
        let paint = unsafe { paint.assume_init() };
        let mut rect = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
            let _ = unsafe { EndPaint(hwnd, &paint) };
            return;
        }

        let brush = unsafe { CreateSolidBrush(BACKGROUND) };
        unsafe {
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = SetBkMode(hdc, TRANSPARENT);
        }

        let middle = rect.top + (rect.bottom - rect.top) / 2;
        let mut title_rect = RECT {
            bottom: middle - 8,
            ..rect
        };
        unsafe {
            draw_text(
                hdc,
                "OpenPiso",
                &mut title_rect,
                -52,
                FW_BOLD.0 as i32,
                FOREGROUND,
                DT_BOTTOM,
            );
        }

        let mut note_rect = RECT {
            top: middle + 12,
            ..rect
        };
        unsafe {
            draw_text(hdc, "STARTING", &mut note_rect, -18, 400, MUTED, DT_TOP);
        }
        let _ = unsafe { EndPaint(hwnd, &paint) };
    }

    unsafe fn draw_text(
        hdc: windows::Win32::Graphics::Gdi::HDC,
        text: &str,
        rect: &mut RECT,
        height: i32,
        weight: i32,
        color: COLORREF,
        vertical: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
    ) {
        let font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                u32::from(DEFAULT_PITCH.0),
                w!("Segoe UI"),
            )
        };
        let previous = unsafe { SelectObject(hdc, HGDIOBJ(font.0)) };
        let _ = unsafe { SetTextColor(hdc, color) };
        let mut text: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            DrawTextW(hdc, &mut text, rect, DT_CENTER | DT_SINGLELINE | vertical);
            let _ = SelectObject(hdc, previous);
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
}

#[cfg(all(windows, not(debug_assertions)))]
pub use platform::{handoff, is_visible, reassert_topmost, start};

#[cfg(not(all(windows, not(debug_assertions))))]
pub fn start() -> Result<(), String> {
    Ok(())
}

#[cfg(not(all(windows, not(debug_assertions))))]
pub const fn is_visible() -> bool {
    false
}

#[cfg(not(all(windows, not(debug_assertions))))]
pub fn reassert_topmost() -> Result<(), String> {
    Ok(())
}

#[cfg(not(all(windows, not(debug_assertions))))]
pub fn handoff() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_close_is_ignored_until_the_client_is_ready() {
        assert_eq!(message_action(WM_CLOSE), MessageAction::Ignore);
    }

    #[test]
    fn only_the_private_handoff_message_destroys_the_placeholder() {
        assert_eq!(
            message_action(WM_APP_CLOSE_PLACEHOLDER),
            MessageAction::Destroy
        );
        assert_eq!(message_action(WM_DESTROY), MessageAction::Quit);
        assert_eq!(message_action(WM_PAINT), MessageAction::Paint);
        assert_eq!(message_action(0x1234), MessageAction::Default);
    }
}

//! Client window geometry for the three-format window model
//! (ADR-0029). The fullscreen lock, the 320×480 widget, and the 100×30 mini
//! bar are the *same* `main` window reconfigured by these commands. Only the
//! frontend router and the §4.1 state machine in `App.svelte` decide which
//! one is current; this module owns the geometry, decorations, and
//! always-on-top rules plus the persisted `view_mode`.
//!
//! The Mini Bar is positioned at the **top-center of the current monitor's
//! work area**, never at a hard-coded pixel. The pure `mini_bar_origin`
//! function below isolates that arithmetic from Tauri's `Monitor` so the
//! math is unit-testable without an event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{LogicalSize, Manager, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewWindow};
use tracing::warn;

use crate::state::{normalize_view_mode, ClientConfig, SharedState};

/// How long after `setup` the startup reveal waits for the frontend before
/// showing the window anyway. The measured gap from `setup_entered` to
/// `frontend_ready` is ~350ms even on a cold diskless boot (the slow part is
/// the WebView2 load, which happens *before* setup), so this is a failure
/// path, not a normal one: it only fires if the Svelte app never mounts.
const STARTUP_REVEAL_FALLBACK: Duration = Duration::from_secs(10);

/// Whether the one-shot startup reveal has already run.
static STARTUP_WINDOW_REVEALED: AtomicBool = AtomicBool::new(false);

/// Floating Widget window size in **logical** (CSS) pixels (ADR-0029 §4.1,
/// amended for bug #12: 320×480 so the transactional dialogs fit; was
/// 200×300).
/// Must match the `h-[…px] w-[…px]` classes on the Widget's root
/// `<section>` in `Widget.svelte` — pinned by
/// `apps/client/scripts/widget-geometry.test.ts`.
///
/// These are logical pixels on purpose: sizing the Widget in *physical*
/// pixels shrank the CSS viewport on DPI-scaled displays (256×384 at 125%,
/// 213×320 at 150%), so the fixed 320×480 layout could not fit and the tab
/// panel scrolled. See the ADR-0029 amendment of 2026-08-03.
pub const WIDGET_WIDTH: f64 = 320.0;
pub const WIDGET_HEIGHT: f64 = 480.0;

/// Mini Bar window size in pixels (ADR-0029 §4.1).
///
/// Known issue: unlike the Widget these are still applied as *physical*
/// pixels, so the bar's CSS viewport shrinks on DPI-scaled displays
/// (80×24 at 125%). Left as-is deliberately — see the ADR-0029 amendment
/// of 2026-08-03.
pub const MINI_WIDTH: f64 = 100.0;
pub const MINI_HEIGHT: f64 = 30.0;

/// The valid `ClientConfig.view_mode` values. The persisted value is a
/// `String` because it crosses the Tauri command boundary; this enum is
/// the only place that maps the wire form to a typed view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Widget,
    Mini,
}

impl ViewMode {
    pub fn from_config(raw: &str) -> Self {
        if normalize_view_mode(raw) == "mini" {
            ViewMode::Mini
        } else {
            // The lock is not a stored `view_mode` (it is always the source
            // of truth when the §4.1 return triggers fire), so any non-mini
            // stored value — including stale data from before the field
            // existed — falls back to the Widget default.
            ViewMode::Widget
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Widget => "widget",
            ViewMode::Mini => "mini",
        }
    }
}

/// Compute the Mini Bar's top-left origin so the bar is horizontally
/// centered within the supplied monitor's work area and flush with the top
/// of that area. The work area is the rectangle the OS reports as "usable
/// desktop" — it already excludes the taskbar/dock so the bar never sits
/// underneath it. All math uses the monitor's physical pixels because
/// Tauri's `set_position` accepts a `Position::Physical` natively and the
/// scale-factor transform is the frontend's job for layout, not ours.
pub fn mini_bar_origin(work_area: &PhysicalRect<i32, u32>) -> (i32, i32) {
    let width_i32 = i32::try_from(work_area.size.width).unwrap_or(i32::MAX);
    let x = work_area.position.x + (width_i32 - MINI_WIDTH as i32) / 2;
    let y = work_area.position.y;
    (x, y)
}

/// Apply the Fullscreen Lock geometry (ADR-0029 §4.1): true fullscreen,
/// undecorated, always-on-top, focused. We use `set_fullscreen(true)` rather
/// than `maximize` + `set_decorations(false)` because a borderless-maximized
/// window on Windows only fills the work area, leaving the taskbar painted
/// above the Lock. True fullscreen is the only mode that covers the taskbar,
/// which the kiosk Lock requires.
///
/// This is also the focus-loss re-assertion path (ADR-0027 §1): the
/// frontend re-invokes it whenever the Lock loses focus, so it must be
/// idempotent. We only call `set_fullscreen(true)` when the window is not
/// already fullscreen to avoid a needless un-/re-fullscreen flicker on every
/// re-assertion.
pub fn apply_fullscreen_lock(window: &WebviewWindow) -> Result<(), String> {
    window
        .set_decorations(false)
        .map_err(window_error("lock decorations"))?;
    window
        .set_always_on_top(true)
        .map_err(window_error("lock always-on-top"))?;
    // Ensure the window is resizable before fullscreening: `apply_widget`
    // sets `set_resizable(false)` and on Windows `set_fullscreen(true)` on
    // a non-resizable window is a known wry/tao failure mode (bug #11).
    window
        .set_resizable(true)
        .map_err(window_error("lock resizable"))?;
    let already_fullscreen = window
        .is_fullscreen()
        .map_err(window_error("lock is-fullscreen"))?;
    if !already_fullscreen {
        window
            .set_fullscreen(true)
            .map_err(window_error("lock fullscreen"))?;
    }
    window.show().map_err(window_error("lock show"))?;
    window.set_focus().map_err(window_error("lock focus"))?;
    Ok(())
}

/// Apply the floating Widget geometry: 320×480 logical pixels, undecorated,
/// non-resizable, always-on-top, centered on screen. We exit fullscreen and
/// `unmaximize` first so a leftover Lock state does not clip the 320×480 size.
pub fn apply_widget(window: &WebviewWindow) -> Result<(), String> {
    window
        .set_fullscreen(false)
        .map_err(window_error("widget exit fullscreen"))?;
    window
        .set_size(LogicalSize::new(WIDGET_WIDTH, WIDGET_HEIGHT))
        .map_err(window_error("widget size"))?;
    window
        .set_decorations(false)
        .map_err(window_error("widget decorations"))?;
    window
        .set_always_on_top(true)
        .map_err(window_error("widget always-on-top"))?;
    window
        .unmaximize()
        .map_err(window_error("widget unmaximize"))?;
    window
        .set_resizable(false)
        .map_err(window_error("widget resizable"))?;
    window.center().map_err(window_error("widget center"))?;
    window.show().map_err(window_error("widget show"))?;
    window.set_focus().map_err(window_error("widget focus"))?;
    Ok(())
}

/// Apply the Mini Bar geometry: 100×30, undecorated, non-resizable,
/// always-on-top, anchored at the top-center of the **current monitor's**
/// work area (ADR-0029 §4.1: never a hard-coded pixel). Falls back to
/// centering on the primary monitor if the current monitor cannot be
/// resolved, and to centering on the primary if even that fails, so a
/// transient monitor-API failure can never leave the bar off-screen.
pub fn apply_mini(window: &WebviewWindow) -> Result<(), String> {
    window
        .set_fullscreen(false)
        .map_err(window_error("mini exit fullscreen"))?;
    window
        .set_size(PhysicalSize::new(MINI_WIDTH, MINI_HEIGHT))
        .map_err(window_error("mini size"))?;
    window
        .set_decorations(false)
        .map_err(window_error("mini decorations"))?;
    window
        .set_always_on_top(true)
        .map_err(window_error("mini always-on-top"))?;
    window
        .unmaximize()
        .map_err(window_error("mini unmaximize"))?;
    window
        .set_resizable(false)
        .map_err(window_error("mini resizable"))?;

    let monitor = window
        .current_monitor()
        .map_err(window_error("mini current monitor"))?
        .or_else(|| {
            warn!(
                target: "openpiso_client::window_mode",
                "current monitor unavailable; falling back to primary monitor for the Mini Bar"
            );
            None
        })
        .or_else(|| match window.primary_monitor() {
            Ok(Some(monitor)) => Some(monitor),
            Ok(None) => None,
            Err(error) => {
                warn!(
                    target: "openpiso_client::window_mode",
                    "primary monitor unavailable: {error}; Mini Bar will be centered instead"
                );
                None
            }
        });

    if let Some(monitor) = monitor {
        let (x, y) = mini_bar_origin(monitor.work_area());
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(window_error("mini position"))?;
    } else {
        window
            .center()
            .map_err(window_error("mini center fallback"))?;
    }

    window.show().map_err(window_error("mini show"))?;
    window.set_focus().map_err(window_error("mini focus"))?;
    Ok(())
}

/// Debug-only developer escape from the Fullscreen Lock geometry. This does
/// not change timer/account authority; it only makes the Client window a
/// normal desktop window so `tauri dev` cannot trap the developer behind the
/// lock surface while testing expired/shutdown states.
#[cfg(debug_assertions)]
pub fn apply_dev_lock_bypass(window: &WebviewWindow) -> Result<(), String> {
    window
        .set_fullscreen(false)
        .map_err(window_error("dev bypass exit fullscreen"))?;
    window
        .set_always_on_top(false)
        .map_err(window_error("dev bypass always-on-top"))?;
    window
        .set_decorations(true)
        .map_err(window_error("dev bypass decorations"))?;
    window
        .set_resizable(true)
        .map_err(window_error("dev bypass resizable"))?;
    // `skipTaskbar` is a static `tauri.conf.json` flag so the production
    // behavior has one source of truth and cannot drift across the three
    // per-mode geometry calls. It therefore also applies in `tauri dev`;
    // restoring the taskbar button belongs here, with the rest of the
    // "give me a normal desktop window" escape.
    window
        .set_skip_taskbar(false)
        .map_err(window_error("dev bypass skip-taskbar"))?;
    window
        .set_size(PhysicalSize::new(960.0, 720.0))
        .map_err(window_error("dev bypass size"))?;
    window.center().map_err(window_error("dev bypass center"))?;
    window.show().map_err(window_error("dev bypass show"))?;
    window
        .set_focus()
        .map_err(window_error("dev bypass focus"))?;
    Ok(())
}

/// Resolve the main `webview` window from the Tauri app handle. The Client
/// only ever owns one window (the §4.1 "one window, reconfigured" decision
/// in ADR-0029), so a `get_webview_window("main")` lookup is the single
/// entry point.
pub fn main_window(app: &tauri::AppHandle) -> Result<WebviewWindow, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "main window is not available".to_owned())
}

/// Reveal the main window once the frontend has mounted.
///
/// The window is created hidden (`"visible": false` in `tauri.conf.json`)
/// because on a diskless client the cold WebView2 load takes tens of seconds
/// before `setup` even runs — a visible window would spend that whole time as
/// a blank always-on-top fullscreen rectangle over the customer's screen. The
/// frontend's explicit `prepare_startup_handoff` IPC call begins the covered
/// reveal; `frontend_ready` retires the native placeholder after two frames.
///
/// Idempotent: only the first call shows the window, so the later per-mode
/// `show()` calls in `apply_*` are unaffected and a re-mount cannot re-show a
/// window the app deliberately hid.
pub fn reveal_startup_window(app: &tauri::AppHandle, covered: bool) -> Result<(), String> {
    if STARTUP_WINDOW_REVEALED.load(Ordering::Relaxed) {
        return Ok(());
    }
    main_window(app).and_then(|window| {
        if covered {
            window
                .set_always_on_top(false)
                .map_err(window_error("covered startup always-on-top"))?;
        }
        window.show().map_err(window_error("startup show"))?;
        if !covered {
            window.set_focus().map_err(window_error("startup focus"))?;
        }
        Ok(())
    })?;
    STARTUP_WINDOW_REVEALED.store(true, Ordering::Relaxed);
    Ok(())
}

/// Put the painted Tauri window above the placeholder before the native HWND
/// is retired. Every production Client view is topmost by ADR-0029.
pub fn complete_startup_handoff(app: &tauri::AppHandle) -> Result<(), String> {
    let window = main_window(app)?;
    window
        .set_always_on_top(true)
        .map_err(window_error("startup handoff always-on-top"))?;
    window
        .set_focus()
        .map_err(window_error("startup handoff focus"))
}

/// Arm the fallback that reveals the window even if the frontend never calls
/// in. A release-Windows build with the native placeholder keeps that safe
/// window visible instead. Other builds retain the former blank-window fallback.
///
/// Skipped under the startup coordinator's test/harness config override
/// (ADR-0079): a process running against an isolated config directory is a
/// harness receiver, not a kiosk install, and must not reveal a fullscreen
/// window over the developer's desktop.
pub fn arm_startup_reveal_fallback(app: &tauri::AppHandle) {
    if crate::startup_coordinator::config_dir_override().is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_REVEAL_FALLBACK).await;
        if crate::startup_placeholder::is_visible() {
            warn!(
                target: "openpiso_client::startup",
                "frontend did not complete startup handoff within {}s; keeping the native startup placeholder visible",
                STARTUP_REVEAL_FALLBACK.as_secs()
            );
            return;
        }
        if STARTUP_WINDOW_REVEALED.load(Ordering::Relaxed) {
            return;
        }
        warn!(
            target: "openpiso_client::startup",
            "frontend did not report ready within {}s; revealing the window anyway",
            STARTUP_REVEAL_FALLBACK.as_secs()
        );
        if let Err(error) = reveal_startup_window(&app, false) {
            warn!(target: "openpiso_client::startup", "Could not reveal the startup window: {error}");
        }
    });
}

/// Persist the supplied view mode to `ClientConfig.view_mode`. Lock is not
/// a stored `view_mode`; the three geometry commands write either
/// `widget` or `mini`, and the Lock never writes here.
pub async fn persist_view_mode(shared: &SharedState, view_mode: ViewMode) -> Option<ClientConfig> {
    let mut config = shared.config.write().await;
    if config.view_mode == view_mode.as_str() {
        return None;
    }
    config.view_mode = view_mode.as_str().to_owned();
    Some(config.clone())
}

fn window_error(label: &'static str) -> impl FnOnce(tauri::Error) -> String + 'static {
    move |error| {
        warn!(
            target: "openpiso_client::window_mode",
            "window operation failed ({label}): {error}"
        );
        format!("window operation failed ({label}): {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(x, y),
            size: PhysicalSize::new(width, height),
        }
    }

    #[test]
    fn mini_bar_centers_within_the_work_area_and_anchors_to_its_top() {
        // 1920×1080 work area at (0, 0): bar is 100 px wide so its left
        // edge sits at (1920 - 100) / 2 = 910 and its top edge at y = 0.
        let (x, y) = mini_bar_origin(&rect(0, 0, 1920, 1080));
        assert_eq!((x, y), (910, 0));
    }

    #[test]
    fn mini_bar_respects_a_non_zero_work_area_origin() {
        // Secondary monitor placed to the right of a 1366×768 primary:
        // the bar lives on the secondary's top-center, not the primary's.
        let (x, y) = mini_bar_origin(&rect(1366, 200, 1920, 1080));
        assert_eq!((x, y), (1366 + (1920 - 100) / 2, 200));
    }

    #[test]
    fn mini_bar_keeps_an_integer_pixel_origin() {
        // Odd-width work areas still center: (1024 - 100) / 2 = 462.
        let (x, y) = mini_bar_origin(&rect(0, 0, 1024, 720));
        assert_eq!((x, y), (462, 0));
    }

    #[test]
    fn view_mode_round_trip_recognises_widget_and_mini() {
        assert_eq!(ViewMode::from_config("widget"), ViewMode::Widget);
        assert_eq!(ViewMode::from_config("mini"), ViewMode::Mini);
        assert_eq!(ViewMode::from_config("MINI"), ViewMode::Mini);
        // Unknown / lock / stale values fall back to the default.
        assert_eq!(ViewMode::from_config("lock"), ViewMode::Widget);
        assert_eq!(ViewMode::from_config(""), ViewMode::Widget);
        assert_eq!(ViewMode::Widget.as_str(), "widget");
        assert_eq!(ViewMode::Mini.as_str(), "mini");
    }
}

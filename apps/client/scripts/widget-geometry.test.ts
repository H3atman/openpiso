import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/**
 * Widget geometry regression pins: the Widget dialogs must
 * fit the widget window. Confirmed root cause: dialog cards + `p-8` overlay
 * padding sized for large screens inside a fixed 200×300 window, with a
 * non-scrolling, unsafe-centered overlay that clipped tall dialogs at both
 * edges. Fix = A+C hybrid: 320×480 widget + compact, scrollable, safe-
 * centered WidgetOverlay.
 *
 * The headless-browser Widget matrix exercises real layout at the real window
 * size. These fast source-level tests additionally pin:
 * - Rust `WIDGET_WIDTH`/`WIDGET_HEIGHT` match Widget.svelte's size classes
 *   (drift between them re-introduces clipping);
 * - WidgetOverlay stays a scroll container with safe centering and compact
 *   padding.
 */

const CLIENT_DIR = resolve(import.meta.dir, "..");

function read(relPath: string): string {
  return readFileSync(resolve(CLIENT_DIR, relPath), "utf-8");
}

describe("widget geometry stays in sync (bug #12)", () => {
  test("Rust WIDGET_* constants match Widget.svelte size classes", () => {
    const rust = read("src-tauri/src/window_mode.rs");
    const widthMatch = rust.match(/WIDGET_WIDTH:\s*f64\s*=\s*(\d+)/);
    const heightMatch = rust.match(/WIDGET_HEIGHT:\s*f64\s*=\s*(\d+)/);
    expect(
      widthMatch,
      "WIDGET_WIDTH must exist in window_mode.rs",
    ).not.toBeNull();
    expect(
      heightMatch,
      "WIDGET_HEIGHT must exist in window_mode.rs",
    ).not.toBeNull();

    const widget = read("src/views/Widget.svelte");
    const classMatch = widget.match(/h-\[(\d+)px\]\s+w-\[(\d+)px\]/);
    expect(
      classMatch,
      "Widget.svelte root <section> must declare h-[..px] w-[..px]",
    ).not.toBeNull();

    const [, cssH, cssW] = classMatch as RegExpMatchArray;
    expect(cssW, "Widget.svelte width class must match WIDGET_WIDTH").toBe(
      (widthMatch as RegExpMatchArray)[1],
    );
    expect(cssH, "Widget.svelte height class must match WIDGET_HEIGHT").toBe(
      (heightMatch as RegExpMatchArray)[1],
    );
  });

  test("Widget is sized in logical pixels, not physical", () => {
    const rust = read("src-tauri/src/window_mode.rs");
    expect(
      /LogicalSize::new\(WIDGET_WIDTH,\s*WIDGET_HEIGHT\)/.test(rust),
      "apply_widget must size the Widget in logical (CSS) pixels: PhysicalSize shrinks the CSS viewport on DPI-scaled displays (256×384 at 125%), which no fixed 320×480 layout can fit",
    ).toBe(true);
    expect(
      /PhysicalSize::new\(WIDGET_WIDTH/.test(rust),
      "apply_widget must not return to PhysicalSize for the Widget",
    ).toBe(false);
  });

  test("WidgetOverlay scrolls and safe-centers tall dialogs", () => {
    const overlay = read("src/lib/components/WidgetOverlay.svelte");
    expect(
      overlay.includes("overflow-y-auto"),
      "WidgetOverlay must be a vertical scroll container for tall transactional dialogs",
    ).toBe(true);
    expect(
      overlay.includes("place-items-center-safe"),
      "WidgetOverlay must use safe centering: plain `place-items-center` pushes an overflowing card's top above the scroll origin, unreachable",
    ).toBe(true);
    expect(
      /\bp-8\b/.test(overlay),
      "WidgetOverlay must not return to p-8 padding (wastes 64px of a small window)",
    ).toBe(false);
  });
});

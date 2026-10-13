import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

/**
 * Bug #9: the MiniBar's expand affordance must not depend on a `click`
 * event. The bar is a `data-tauri-drag-region`, and Tauri's drag-region
 * script invokes `start_dragging` on mousedown — the native window-move
 * loop then swallows the matching mouseup, so `click` never fires on this
 * element with a real mouse. Expand must trigger on the second press of a
 * double-click (`mousedown` with `detail === 2`), which is always
 * delivered, and stop propagation so the press does not also start a drag
 * or toggle maximize via the document-level drag script.
 *
 * The behavioral seam (physical mouse input against the Tauri runtime)
 * is not CI-runnable, so this test pins the source pattern instead. The
 * full loop lives in the bug #9 diagnosis notes (SendInput double-click +
 * CDP assertion against a dev client).
 */

const source = () =>
  readFileSync(new URL("../src/views/MiniBar.svelte", import.meta.url), "utf-8");

describe("bug-9 mini bar expands on double-click despite the drag region", () => {
  test("expand does not use a click handler on the drag-region element", () => {
    expect(
      /onclick=/.test(source()),
      "MiniBar must not bind onclick — the drag region swallows click events",
    ).toBe(false);
  });

  test("expand triggers on mousedown with detail === 2 and stops propagation", () => {
    const src = source();
    expect(/onmousedown=/.test(src)).toBe(true);
    expect(/detail === 2/.test(src)).toBe(true);
    expect(/stopPropagation\(\)/.test(src)).toBe(true);
  });

  test("keyboard expand (Enter/Space) is preserved", () => {
    expect(/onkeydown=/.test(source())).toBe(true);
  });
});

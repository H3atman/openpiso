import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

/**
 * Bug #11: the dev "Bypass lock" escape (`devLockBypassed`) must be a
 * one-shot for the *current* Lock. When it stayed set after the client
 * left the Lock (coin insert / login → Widget), every later Lock entry
 * skipped `enterFullscreenMode()`, so the Lock rendered inside the
 * 200×300 widget window — the reported symptom.
 *
 * The behavioral seam (native window geometry against the Tauri runtime)
 * is not CI-runnable, so this test pins the source pattern instead: the
 * mode `$effect` must reset `devLockBypassed` whenever the view leaves
 * the Lock. The full loop is a stub-admin + CDP + Win32 GetWindowRect
 * harness (bypass → widget → expiry) from the bug #11 diagnosis notes.
 */

const source = () =>
  readFileSync(new URL("../src/App.svelte", import.meta.url), "utf-8");

describe("bug-11 dev lock bypass re-arms when leaving the lock", () => {
  test("the view $effect resets devLockBypassed on non-lock views", () => {
    expect(
      /nextView !== "lock" && devLockBypassed/.test(source()),
      "App.svelte must clear devLockBypassed when the view leaves the Lock",
    ).toBe(true);
    expect(/devLockBypassed = false;/.test(source())).toBe(true);
  });

  test("fullscreen transition failures stay surfaced, not swallowed", () => {
    expect(
      /void enterFullscreenMode\(\)/.test(
        source().replace(/reassertLockOnBlur[\s\S]*?\n  }/, ""),
      ),
      "the mode $effect must not fire-and-forget enterFullscreenMode",
    ).toBe(false);
    expect(/Failed to enter fullscreen lock/.test(source())).toBe(true);
  });
});

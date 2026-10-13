import { describe, expect, test } from "bun:test";
import { readFileSync, existsSync, writeFileSync, unlinkSync } from "node:fs";
import { resolve } from "node:path";

/**
 * Issues #29/#30: Extract WidgetOverlay shared component and refactor
 * transactional Widget dialogs to use it.
 *
 * Source-structure tests verify that:
 * - WidgetOverlay.svelte exists at the expected path
 * - Transactional dialog sites use WidgetOverlay
 *
 * Issue #31: Constraint documentation + pre-commit check script
 * - Script exits 0 against clean codebase
 * - Script exits 1 against a violating file
 */

const COMPONENTS_DIR = resolve(import.meta.dir, "../src/lib/components");
const VIEWS_DIR = resolve(import.meta.dir, "../src/views");
const SCRIPTS_DIR = resolve(import.meta.dir);

function runCheckScript(): {
  exitCode: number;
  stdout: string;
  stderr: string;
} {
  const result = Bun.spawnSync([
    process.execPath,
    "run",
    resolve(SCRIPTS_DIR, "check-widget-overlay.ts"),
  ]);
  return {
    exitCode: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

function readWidget(): string {
  return readFileSync(resolve(VIEWS_DIR, "Widget.svelte"), "utf-8");
}

function readDialog(name: string): string {
  return readFileSync(resolve(COMPONENTS_DIR, name), "utf-8");
}

describe("WidgetOverlay component extraction (issues #29/#30)", () => {
  test("WidgetOverlay.svelte exists in src/lib/components", () => {
    const path = resolve(COMPONENTS_DIR, "WidgetOverlay.svelte");
    expect(existsSync(path), "WidgetOverlay.svelte must exist").toBe(true);
  });

  test("Widget keeps transactional overlays but owner settings is inline", () => {
    const source = readWidget();
    expect(
      source.includes("import WidgetOverlay from"),
      "Widget.svelte must import WidgetOverlay",
    ).toBe(true);

    const usageMatches = source.match(/<WidgetOverlay/g);
    expect(
      usageMatches,
      "Widget.svelte must use <WidgetOverlay> at least once",
    ).not.toBeNull();
    expect(source).toContain('<WidgetOverlay title="Confirm Claim"');
    expect(source).not.toContain('<WidgetOverlay title="Owner settings"');
    expect(source).not.toContain('<WidgetOverlay title="Log in"');
    expect(source).not.toContain("AuthForm from");
  });

  test("each dialog component imports and uses WidgetOverlay", () => {
    const dialogs = [
      "ChangePasswordDialog.svelte",
      "TransferTimeDialog.svelte",
      "TransferPointsDialog.svelte",
      "RedeemTiersDialog.svelte",
    ];

    for (const name of dialogs) {
      const source = readDialog(name);
      expect(
        source.includes("import WidgetOverlay from"),
        `${name} must import WidgetOverlay`,
      ).toBe(true);
      expect(
        source.includes("<WidgetOverlay"),
        `${name} must use <WidgetOverlay>`,
      ).toBe(true);
    }
  });
});

describe("check-widget-overlay.ts pre-commit check (issue #31)", () => {
  test("passes against clean codebase (exit code 0)", () => {
    const { exitCode, stdout } = runCheckScript();
    expect(exitCode, `Script failed with output:\n${stdout}`).toBe(0);
  }, 15_000);

  test("fails against a violating file (exit code 1)", () => {
    const violatingFile = resolve(
      COMPONENTS_DIR,
      "_TEMPORARY_VIOLATION.svelte",
    );
    try {
      writeFileSync(
        violatingFile,
        '<div class="fixed inset-0 z-20">violation</div>\n',
        "utf-8",
      );

      const { exitCode, stdout } = runCheckScript();
      expect(exitCode).toBe(1);
      expect(stdout).toMatch(/raw fixed CSS/i);
      expect(stdout).toMatch(/_TEMPORARY_VIOLATION\.svelte/);
    } finally {
      if (existsSync(violatingFile)) {
        unlinkSync(violatingFile);
      }
    }
  }, 15_000);
});

// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import AppUpdateHarness from "./AppUpdateHarness.svelte";

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return {
    ApiError: actual.ApiError,
    UnauthenticatedError: actual.UnauthenticatedError,
    ThrottledError: actual.ThrottledError,
    getAppUpdate: vi.fn(),
    checkAppUpdate: vi.fn(),
    setAppUpdateAutoCheck: vi.fn(),
    installAppUpdate: vi.fn(),
    rollbackAppUpdate: vi.fn(),
    restartAppUpdate: vi.fn(),
    acknowledgeAppUpdate: vi.fn(),
  };
});
vi.mock("svelte-sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

import * as api from "../api";
import { toast } from "svelte-sonner";

const current = {
  currentVersion: "0.11.5",
  currentSchemaVersion: 58,
  autoCheck: true,
  lastCheckedAt: null,
  rollbacks: [],
  runningSessionCount: 0,
};
const release = {
  version: "0.12.0",
  schemaVersion: 59,
  notes: "## New\n\n- Better shop controls",
  pubDate: "2026-10-13T00:00:00Z",
  latestJsonUrl: "https://example.test/latest.json",
  raisesSchema: true,
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getAppUpdate).mockResolvedValue(current);
});

afterEach(async () => {
  cleanup();
  // Bits UI delays body-scroll-lock teardown.
  await new Promise<void>((resolve) => setTimeout(resolve, 30));
});

describe("App Update", () => {
  // Keep first: jsdom's :focus-visible tracks focus history across the shared
  // window, so after earlier tests Tab focus may no longer count as visible.
  it("opens the row tooltip on hover but not on keyboard focus", async () => {
    render(AppUpdateHarness);
    const row = await screen.findByRole("button", {
      name: "App Update · v0.11.5",
    });
    while (document.activeElement !== row) await userEvent.tab();
    expect(row.matches(":focus-visible")).toBe(true);
    const summary = "View the installed version and check for App Updates.";
    // Let any focus-opened tooltip render before asserting it stayed closed.
    await new Promise<void>((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByText(summary)).toBeNull();
    await userEvent.hover(row);
    expect(await screen.findByText(summary)).toBeTruthy();
  });
  it("downloads first and restarts only on the explicit action", async () => {
    const offered = {
      ...current,
      available: { ...release, raisesSchema: false, schemaVersion: 58 },
    };
    const ready = {
      ...offered,
      ready: { targetVersion: "0.12.0", rollback: false },
    };
    vi.mocked(api.getAppUpdate)
      .mockResolvedValueOnce(offered)
      .mockResolvedValue(ready);
    vi.mocked(api.installAppUpdate).mockResolvedValue(ready);
    vi.mocked(api.restartAppUpdate).mockRejectedValue(
      new api.ApiError(500, "App Update install: launch failed"),
    );
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "Update available · v0.12.0" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    await userEvent.click(
      screen.getByRole("button", { name: "Download update" }),
    );
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith(
        "v0.12.0 is ready. Restart the Admin to install it.",
      ),
    );
    expect(screen.getByText("v0.12.0 is downloaded and verified.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Install" })).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
    expect(api.restartAppUpdate).not.toHaveBeenCalled();
    // A server error response means no handoff: surface it, allow a retry.
    await userEvent.click(
      screen.getByRole("button", { name: "Restart and install" }),
    );
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        "App Update install: launch failed",
      ),
    );
    expect(
      screen
        .getByRole("button", { name: "Restart and install" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });
  it("polls download progress while install waits and prevents another install", async () => {
    const offered = {
      ...current,
      available: { ...release, raisesSchema: false, schemaVersion: 58 },
    };
    vi.mocked(api.getAppUpdate)
      .mockResolvedValueOnce(offered)
      .mockResolvedValue({
        ...offered,
        download: { targetVersion: "0.12.0", percent: 62 },
      });
    vi.mocked(api.installAppUpdate).mockImplementation(
      () => new Promise(() => {}),
    );
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "Update available · v0.12.0" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    expect(screen.queryByRole("checkbox")).toBeNull();
    await userEvent.click(
      screen.getByRole("button", { name: "Download update" }),
    );
    const progress = await screen.findByRole("progressbar", {
      name: "Downloading v0.12.0",
    });
    expect(progress.getAttribute("aria-valuenow")).toBe("62");
    expect(
      screen
        .getByRole("button", { name: "Download update" })
        .hasAttribute("disabled"),
    ).toBe(true);
  });
  it("treats the Admin going away after restart as the restart, not an error", async () => {
    const ready = {
      ...current,
      available: { ...release, raisesSchema: false, schemaVersion: 58 },
      ready: { targetVersion: "0.12.0", rollback: false },
    };
    const gone = new TypeError(
      "NetworkError when attempting to fetch resource.",
    );
    let serverUp = true;
    let dropRestart: (error: Error) => void = () => {};
    vi.mocked(api.getAppUpdate).mockImplementation(() =>
      serverUp ? Promise.resolve(ready) : Promise.reject(gone),
    );
    vi.mocked(api.restartAppUpdate).mockImplementation(
      () => new Promise((_, reject) => (dropRestart = reject)),
    );
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "Update available · v0.12.0" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Restart and install" }),
    );
    // The Installer handoff exits the Admin: the POST and every poll now fail.
    serverUp = false;
    dropRestart(gone);
    const calls = vi.mocked(api.getAppUpdate).mock.calls.length;
    await waitFor(
      () =>
        expect(vi.mocked(api.getAppUpdate).mock.calls.length).toBeGreaterThan(
          calls + 1,
        ),
      { timeout: 3_000 },
    );
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain(
        "Waiting for the Admin to restart",
      ),
    );
    expect(screen.queryByRole("alert")).toBeNull();
    expect(toast.error).not.toHaveBeenCalled();
  });
  it("persists auto-check off and toasts manual check failures", async () => {
    vi.mocked(api.setAppUpdateAutoCheck).mockResolvedValue({
      ...current,
      autoCheck: false,
    });
    vi.mocked(api.checkAppUpdate).mockResolvedValue({
      ...current,
      autoCheck: false,
      lastError: "Release Feed unavailable",
    });
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "App Update · v0.11.5" }),
    );
    await userEvent.click(
      screen.getByRole("switch", { name: "Automatic checks" }),
    );
    await waitFor(() =>
      expect(api.setAppUpdateAutoCheck).toHaveBeenCalledWith({
        enabled: false,
      }),
    );
    expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe(
      "false",
    );
    vi.mocked(toast.success).mockClear();
    await userEvent.click(screen.getByRole("button", { name: "Check now" }));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "Release Feed unavailable",
    );
    expect(toast.error).toHaveBeenCalledWith("Release Feed unavailable");
    expect(toast.success).not.toHaveBeenCalled();
  });
  it("renders Markdown notes without executable HTML or remote images", async () => {
    vi.mocked(api.getAppUpdate).mockResolvedValue({
      ...current,
      available: {
        ...release,
        notes:
          '## New\n\n**Better controls**\n\n<script>alert(1)</script><img src="https://tracker.test/pixel" onerror="alert(1)"><a href="javascript:alert(1)">Bad link</a>',
      },
    });
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "Update available · v0.12.0" }),
    );
    expect(screen.getByRole("heading", { name: "New" })).toBeTruthy();
    const notes = screen.getByTestId("app-update-notes");
    expect(notes.querySelector("strong")?.textContent).toBe("Better controls");
    expect(
      notes.querySelector("script, img, [onerror], [href^='javascript:']"),
    ).toBeNull();
  });
  it.each([
    [true, "0.12.0", "Updated to v0.12.0", "success"],
    [false, "0.11.5", "Update failed, still on v0.11.5", "error"],
  ] as const)(
    "toasts the resolved restart result and acknowledges it (ok=%s)",
    async (ok, currentVersion, message, kind) => {
      vi.mocked(api.getAppUpdate).mockResolvedValue({
        ...current,
        currentVersion,
        lastResult: { from: "0.11.5", to: "0.12.0", ok },
      });
      vi.mocked(api.acknowledgeAppUpdate).mockResolvedValue({
        ...current,
        currentVersion,
      });
      render(AppUpdateHarness);
      await waitFor(() => expect(toast[kind]).toHaveBeenCalledWith(message));
      await waitFor(() =>
        expect(api.acknowledgeAppUpdate).toHaveBeenCalledOnce(),
      );
    },
  );
  it("requires the exact Compatible Rollback version", async () => {
    const rollback = {
      ...release,
      version: "0.11.4",
      schemaVersion: 58,
      raisesSchema: false,
    };
    vi.mocked(api.getAppUpdate).mockResolvedValue({
      ...current,
      rollbacks: [rollback],
      runningSessionCount: 2,
    });
    vi.mocked(api.rollbackAppUpdate).mockResolvedValue({
      ...current,
      download: { targetVersion: "0.11.4", percent: 0 },
    });
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "App Update · v0.11.5" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Compatible Rollback" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Roll back to v0.11.4" }),
    );
    const confirm = screen.getByRole("button", {
      name: "Download rollback",
    });
    expect(
      screen.getByText("2 PCs have running time.", { exact: false }),
    ).toBeTruthy();
    const input = screen.getByRole("textbox", {
      name: "Type 0.11.4 to confirm",
    });
    await userEvent.type(input, "v0.11.4");
    expect(confirm.hasAttribute("disabled")).toBe(true);
    await userEvent.clear(input);
    await userEvent.type(input, "0.11.4");
    await userEvent.click(confirm);
    await waitFor(() =>
      expect(api.rollbackAppUpdate).toHaveBeenCalledWith({
        version: "0.11.4",
        confirmVersion: "0.11.4",
      }),
    );
  });
  it("requires schema acknowledgement but running PCs only warn", async () => {
    vi.mocked(api.getAppUpdate).mockResolvedValue({
      ...current,
      available: release,
      runningSessionCount: 2,
    });
    vi.mocked(api.installAppUpdate).mockResolvedValue({
      ...current,
      download: { targetVersion: "0.12.0", percent: 0 },
    });
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "Update available · v0.12.0" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    const confirm = screen.getByRole("button", { name: "Download update" });
    expect(confirm.hasAttribute("disabled")).toBe(true);
    expect(
      screen.getByText("2 PCs have running time.", { exact: false }),
    ).toBeTruthy();
    expect(screen.getByText("Fullscreen Lock", { exact: false })).toBeTruthy();
    await userEvent.click(
      screen.getByRole("checkbox", { name: "I understand" }),
    );
    await userEvent.click(confirm);
    await waitFor(() =>
      expect(api.installAppUpdate).toHaveBeenCalledWith({
        version: "0.12.0",
        acknowledgeSchemaChange: true,
      }),
    );
  });
  it("shows the installed version, then the offered version after Check now", async () => {
    vi.mocked(api.checkAppUpdate).mockResolvedValue({
      ...current,
      available: release,
    });
    render(AppUpdateHarness);
    const row = await screen.findByRole("button", {
      name: "App Update · v0.11.5",
    });
    await userEvent.click(row);
    await userEvent.click(screen.getByRole("button", { name: "Check now" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Update available · v0.12.0" }),
      ).toBeTruthy(),
    );
  });
});

// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import AppUpdateHarness from "./AppUpdateHarness.svelte";

vi.mock("../api", () => ({
  getAppUpdate: vi.fn(),
  checkAppUpdate: vi.fn(),
  setAppUpdateAutoCheck: vi.fn(),
  installAppUpdate: vi.fn(),
  rollbackAppUpdate: vi.fn(),
  acknowledgeAppUpdate: vi.fn(),
}));
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
  it("keeps installs blocked after handoff while awaiting restart", async () => {
    const offered = {
      ...current,
      available: { ...release, raisesSchema: false, schemaVersion: 58 },
    };
    vi.mocked(api.getAppUpdate).mockResolvedValue(offered);
    vi.mocked(api.installAppUpdate).mockResolvedValue(offered);
    render(AppUpdateHarness);
    await userEvent.click(
      await screen.findByRole("button", { name: "Update available · v0.12.0" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    await userEvent.click(
      screen.getByRole("button", { name: "Install and restart" }),
    );
    await waitFor(() =>
      expect(
        screen
          .getByRole("button", { name: "Install" })
          .hasAttribute("disabled"),
      ).toBe(true),
    );
    expect(screen.getByRole("status").textContent).toContain(
      "Waiting for the Admin to restart",
    );
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
      screen.getByRole("button", { name: "Install and restart" }),
    );
    const progress = await screen.findByRole("progressbar", {
      name: "Downloading v0.12.0",
    });
    expect(progress.getAttribute("aria-valuenow")).toBe("62");
    expect(
      screen
        .getByRole("button", { name: "Install and restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
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
      name: "Roll back and restart",
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
    const confirm = screen.getByRole("button", { name: "Install and restart" });
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

// @vitest-environment jsdom
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import SettingsIcon from "@lucide/svelte/icons/settings";
import { settingsSections } from "$lib/routing";
import { createMatchMedia } from "../../test/match-media";
import AppSidebarHarness from "./AppSidebarHarness.svelte";
import * as api from "../api";
import { toast } from "svelte-sonner";

vi.mock("../api", () => ({
  getAppUpdate: vi.fn(),
  acknowledgeAppUpdate: vi.fn(),
}));
vi.mock("svelte-sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));
const updateState = {
  currentVersion: "0.11.5",
  currentSchemaVersion: 58,
  autoCheck: true,
  lastCheckedAt: null,
  rollbacks: [],
  runningSessionCount: 0,
};
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getAppUpdate).mockResolvedValue(updateState);
});
// Bits UI schedules body-scroll-lock cleanup for 24ms after teardown.
const BODY_SCROLL_LOCK_CLEANUP_MS = 30;

const settingsItem = {
  id: "/admin/settings",
  title: "Settings",
  icon: SettingsIcon,
  children: settingsSections.map((section) => ({
    id: `/admin/settings/${section.id}`,
    title: section.label,
  })),
};

function setMatchMedia(matches: boolean): void {
  vi.stubGlobal("matchMedia", createMatchMedia(matches));
}

function renderSidebar(activeId: string) {
  setMatchMedia(true);
  const onSelect = vi.fn((event: MouseEvent) => event.preventDefault());
  const view = render(AppSidebarHarness, {
    props: {
      items: [settingsItem],
      activeId,
      onSelect,
    },
  });

  return { view, onSelect };
}

async function openMobileSidebar(): Promise<void> {
  fireEvent.click(screen.getByRole("button", { name: "Toggle Sidebar" }));
  await tick();
}

async function toggleSettings(): Promise<void> {
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  await tick();
}

afterEach(async () => {
  const dialog = screen.queryByRole("dialog");
  const closeButton = screen.queryByRole("button", { name: "Close" });
  if (dialog?.getAttribute("data-state") === "open" && closeButton) {
    fireEvent.click(closeButton);
    await tick();
    await new Promise<void>((resolve) => setTimeout(resolve, 30));
  }
  await new Promise<void>((resolve) =>
    setTimeout(resolve, BODY_SCROLL_LOCK_CLEANUP_MS),
  );
  cleanup();
  await new Promise<void>((resolve) =>
    setTimeout(resolve, BODY_SCROLL_LOCK_CLEANUP_MS),
  );
  vi.unstubAllGlobals();
});

describe("AppSidebar mobile App Update", () => {
  it("reports restart results without opening the mobile sidebar, then opens the panel by tap", async () => {
    vi.mocked(api.getAppUpdate).mockResolvedValue({
      ...updateState,
      lastResult: { from: "0.11.4", to: "0.11.5", ok: true },
    });
    vi.mocked(api.acknowledgeAppUpdate).mockResolvedValue(updateState);
    renderSidebar("/admin/dashboard");
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith("Updated to v0.11.5"),
    );
    await openMobileSidebar();
    fireEvent.click(
      await screen.findByRole("button", { name: "App Update · v0.11.5" }),
    );
    await tick();
    expect(
      screen
        .getByRole("dialog", { name: "App Update" })
        .getAttribute("data-state"),
    ).toBe("open");
    expect(screen.getByRole("button", { name: "Check now" })).toBeTruthy();
  });
});

describe("AppSidebar mobile Settings submenu", () => {
  it("renders every Settings section in the phone navigation sheet", async () => {
    renderSidebar("/admin/dashboard");
    await openMobileSidebar();
    await toggleSettings();

    expect(
      screen.getAllByRole("link").map((link) => link.textContent?.trim()),
    ).toEqual(settingsSections.map((section) => section.label));
  });

  it.each([
    ["the Settings route", "/admin/settings"],
    ["a Settings section route", "/admin/settings/shop"],
  ])("expands when the active route is %s", async (_description, activeId) => {
    renderSidebar(activeId);
    await openMobileSidebar();

    expect(screen.getByRole("link", { name: "Shop" })).toBeTruthy();
  });

  it("lets the parent toggle without navigating or closing the sheet", async () => {
    const { onSelect } = renderSidebar("/admin/dashboard");
    await openMobileSidebar();

    await toggleSettings();
    expect(onSelect).not.toHaveBeenCalled();
    expect(screen.getByRole("link", { name: "Shop" })).toBeTruthy();

    await toggleSettings();
    expect(screen.queryByRole("link", { name: "Shop" })).toBeNull();
    expect(screen.getByRole("button", { name: "Settings" })).toBeTruthy();
  });

  it("navigates through a child and closes the sheet", async () => {
    const { onSelect } = renderSidebar("/admin/dashboard");
    await openMobileSidebar();
    await toggleSettings();
    fireEvent.click(screen.getByRole("link", { name: "Shop" }));
    await tick();

    expect(onSelect).toHaveBeenCalledWith(
      expect.any(MouseEvent),
      "/admin/settings/shop",
    );
    expect(screen.getByRole("dialog").getAttribute("data-state")).toBe(
      "closed",
    );
  });
});

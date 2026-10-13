// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("$lib/api", () => ({
  UnauthenticatedError: class UnauthenticatedError extends Error {},
  getSession: vi.fn(),
  getTheme: vi.fn(),
  getNotificationUnreadCount: vi.fn(),
  listNotifications: vi.fn(),
  getDiagnosticsMode: vi.fn(),
  setDiagnosticsMode: vi.fn(),
  ownerLogout: vi.fn(),
  reportHumanActivity: vi.fn(),
  listPcSettings: vi.fn(),
  getSupportBundle: vi.fn(),
  requestDiagnostics: vi.fn(),
  getCoinDebugBundle: vi.fn(),
  getCoinDebugWindow: vi.fn(),
  resetCoinDebugWindow: vi.fn(),
  getHostDiagnostics: vi.fn(),
  getAppUpdate: vi.fn(),
}));

import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "$lib/api";
import App from "./App.svelte";

const THEME = { version: 1 } as never;

function resolveShellMocks() {
  vi.mocked(api.getSession).mockResolvedValue({ authenticated: true } as never);
  vi.mocked(api.getTheme).mockResolvedValue(THEME);
  vi.mocked(api.getAppUpdate).mockResolvedValue({
    currentVersion: "0.11.5",
    currentSchemaVersion: 58,
    autoCheck: true,
    lastCheckedAt: null,
    rollbacks: [],
    runningSessionCount: 0,
  });
  vi.mocked(api.getNotificationUnreadCount).mockResolvedValue({
    unread_count: 0,
  } as never);
  vi.mocked(api.listNotifications).mockResolvedValue({
    items: [],
    has_more: false,
  } as never);
  vi.mocked(api.listPcSettings).mockResolvedValue([]);
  vi.mocked(api.getCoinDebugWindow).mockResolvedValue({
    since: "2026-08-12T10:00:00Z",
  } as never);
}

// Each route screen is a lazy chunk, so a first paint here waits on a module
// import rather than a mocked fetch. Give those waits room under a loaded
// machine instead of racing the default one-second budget.
const SLOW = { timeout: 20_000 };

/** The root shell owns Diagnostics Mode for the whole browser (ADR-0081). */
describe("Root shell Diagnostics Mode", { timeout: 60_000 }, () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    window.history.replaceState(null, "", "/admin/diagnostics");
    vi.clearAllMocks();
    resolveShellMocks();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("explains a failed first read on the direct Diagnostics route and recovers on retry", async () => {
    vi.mocked(api.getDiagnosticsMode).mockRejectedValueOnce(
      new Error("host unreachable"),
    );
    render(App);

    // Loading text would read as a slow response rather than a failure.
    await waitFor(
      () => expect(screen.getByText(/host unreachable/)).toBeTruthy(),
      SLOW,
    );
    expect(screen.queryByText(/Loading Diagnostics Mode/)).toBeNull();
    expect(screen.queryByText(/Diagnostics Mode is off/)).toBeNull();

    vi.mocked(api.getDiagnosticsMode).mockResolvedValue({ enabled: true });
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(screen.getByRole("tablist")).toBeTruthy(), SLOW);
    expect(screen.queryByText(/host unreachable/)).toBeNull();
  });

  it("keeps the last known value when a later poll fails", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(api.getDiagnosticsMode).mockResolvedValueOnce({ enabled: true });
    render(App);
    await waitFor(() => expect(screen.getByRole("tablist")).toBeTruthy(), SLOW);

    vi.mocked(api.getDiagnosticsMode).mockRejectedValue(new Error("poll failed"));
    await vi.advanceTimersByTimeAsync(15_000);

    // The known surface must not flicker away over a transient failure.
    await waitFor(
      () => expect(api.getDiagnosticsMode).toHaveBeenCalledTimes(2),
      SLOW,
    );
    expect(screen.getByRole("tablist")).toBeTruthy();
    // Kept, but not silently: the surface below may be stale, so the page
    // names the failure and offers the retry (#382).
    expect(
      screen.getByText(/Diagnostics Mode could not be read: poll failed/),
    ).toBeTruthy();
  });

  it("applies a change made in another browser on the next poll", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(api.getDiagnosticsMode).mockResolvedValueOnce({ enabled: false });
    render(App);
    await waitFor(
      () => expect(screen.getByText(/Diagnostics Mode is off/)).toBeTruthy(),
      SLOW,
    );
    expect(
      screen.queryByRole("link", { name: "Diagnostics" }),
    ).toBeNull();

    vi.mocked(api.getDiagnosticsMode).mockResolvedValue({ enabled: true });
    await vi.advanceTimersByTimeAsync(15_000);

    await waitFor(() => expect(screen.getByRole("tablist")).toBeTruthy(), SLOW);
    await waitFor(
      () => expect(screen.getByRole("link", { name: "Diagnostics" })).toBeTruthy(),
      SLOW,
    );
  });

  it("never lets an older poll overwrite a local toggle's answer", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    window.history.replaceState(null, "", "/admin/settings/security");
    vi.mocked(api.getDiagnosticsMode).mockResolvedValueOnce({ enabled: false });
    vi.mocked(api.setDiagnosticsMode).mockResolvedValue({ enabled: true });
    render(App);
    const control = await waitFor(() => screen.getByRole("switch"), SLOW);

    // A second poll goes out and stalls; the operator toggles meanwhile.
    let answerPoll: (value: { enabled: boolean }) => void = () => {};
    vi.mocked(api.getDiagnosticsMode).mockReturnValueOnce(
      new Promise((resolve) => {
        answerPoll = resolve;
      }),
    );
    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(
      () => expect(api.getDiagnosticsMode).toHaveBeenCalledTimes(2),
      SLOW,
    );

    await userEvent.click(control);
    await waitFor(
      () => expect(screen.getByRole("link", { name: "Diagnostics" })).toBeTruthy(),
      SLOW,
    );

    // The stalled poll read the world before the toggle, so its answer is
    // stale and must not flip the flag back (#382).
    answerPoll({ enabled: false });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(screen.getByRole("link", { name: "Diagnostics" })).toBeTruthy();
  });

  it("publishes a local Security toggle to the sidebar immediately", async () => {
    window.history.replaceState(null, "", "/admin/settings/security");
    vi.mocked(api.getDiagnosticsMode).mockResolvedValue({ enabled: false });
    vi.mocked(api.setDiagnosticsMode).mockResolvedValue({ enabled: true });
    render(App);

    const control = await waitFor(() => screen.getByRole("switch"), SLOW);
    expect(screen.queryByRole("link", { name: "Diagnostics" })).toBeNull();

    await userEvent.click(control);

    // One browser, one value: the nav item appears without waiting for a poll.
    await waitFor(
      () => expect(screen.getByRole("link", { name: "Diagnostics" })).toBeTruthy(),
      SLOW,
    );
  });
});

// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getCoinDeviceSettings: vi.fn(),
  getCoinDevicePorts: vi.fn(),
  getAcceptorHealth: vi.fn(),
  getCurrentCoinCalibration: vi.fn(),
}));

vi.mock("../../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../../api";
import { reportActionFailure } from "../../notifications";
import CoinDeviceSection from "./CoinDeviceSection.svelte";

const coinDevice = {
  com_port: null,
  baud_rate: 9600,
  set_pin_line: "dtr",
  coin_pin_line: "cts",
  coin_pin_polarity: "normally_open",
  inter_coin_gap_ms: 600,
  inactivity_timeout_seconds: 15,
  hard_cap_seconds: 180,
  pulse_map: [{ pulse_count: 1, amount_cents: 100 }],
  calibration_required: false,
};

const health = {
  debug_window_start: "2026-08-12T10:00:00Z",
  timing_status: "unavailable_until_filtered" as const,
  implausible_timing_rate: null,
};

function renderSection(diagnosticsMode: boolean | null = null) {
  return render(CoinDeviceSection, { props: { diagnosticsMode } });
}

function resolveDefaultMocks() {
  vi.mocked(api.getCoinDeviceSettings).mockResolvedValue(coinDevice);
  vi.mocked(api.getCoinDevicePorts).mockResolvedValue([]);
  vi.mocked(api.getCurrentCoinCalibration).mockResolvedValue(null);
  vi.mocked(api.getAcceptorHealth).mockResolvedValue(health);
}

describe("Acceptor Health", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    resolveDefaultMocks();
  });

  it("shows a loading state while health is being fetched", async () => {
    let resolveHealth!: (value: typeof health) => void;
    vi.mocked(api.getAcceptorHealth).mockReturnValue(
      new Promise((resolve) => {
        resolveHealth = resolve;
      }),
    );

    const view = renderSection();
    expect(view.getByText("Loading Acceptor Health…")).toBeTruthy();

    resolveHealth(health);
    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );
  });

  it("renders unavailable timing and the current debug window without an event list", async () => {
    const view = renderSection();

    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );
    expect(
      view.getByText("Unavailable until filtered timing (S2)"),
    ).toBeTruthy();
    expect(view.getByText("Not reported in S1")).toBeTruthy();
    expect(view.queryByText("Recent unmapped pulses")).toBeNull();
    expect(view.queryByText("No automatic credit")).toBeNull();
    expect(view.queryByText("No unmapped pulse batches observed.")).toBeNull();
  });

  it("refreshes health from the owner API", async () => {
    const user = userEvent.setup();
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );

    expect(view.getByText("Not reported in S1")).toBeTruthy();

    vi.mocked(api.getAcceptorHealth).mockResolvedValueOnce({
      ...health,
      implausible_timing_rate: 12,
    });
    await user.click(
      view.getByRole("button", { name: "Refresh Acceptor Health" }),
    );

    await waitFor(() => expect(view.getByText("12%")).toBeTruthy());
    expect(view.queryByText("Not reported in S1")).toBeNull();
  });

  it("keeps health load failures inline and does not show a toast", async () => {
    vi.mocked(api.getAcceptorHealth).mockRejectedValueOnce(
      new Error("health unavailable"),
    );
    const view = renderSection();

    await waitFor(() =>
      expect(
        view.getByText("Unable to load Acceptor Health: health unavailable"),
      ).toBeTruthy(),
    );
    expect(reportActionFailure).not.toHaveBeenCalled();
  });
});

describe("moved debug controls (issue #380)", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    resolveDefaultMocks();
  });

  it("renders neither the Coin Debug Bundle copy control nor the debug window controls", async () => {
    const view = renderSection();

    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );
    // The copy control and the window reset moved to the Diagnostics Coin tab;
    // the window label went with them. Acceptor Health's window read stays.
    expect(
      view.queryByRole("button", { name: "Copy Coin Debug Bundle" }),
    ).toBeNull();
    expect(
      view.queryByRole("button", { name: "Reset Coin Debug Window" }),
    ).toBeNull();
    expect(view.queryByText(/Debug window since/)).toBeNull();
  });
});

describe("Diagnostics pointer (issue #380)", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    resolveDefaultMocks();
  });

  it("points to Diagnostics while Diagnostics Mode is on", async () => {
    const view = renderSection(true);

    await waitFor(() =>
      expect(
        view.getByRole("link", { name: "Diagnostics" }).getAttribute("href"),
      ).toBe("/admin/diagnostics"),
    );
    const pointer = view.getByText(/coin evidence tools/);
    const text = pointer.textContent?.replace(/\s+/g, " ") ?? "";
    expect(text).toContain("Coin Debug Bundle");
    expect(text).toContain("Coin Debug Window");
  });

  it("shows no pointer while the flag is off", async () => {
    const view = renderSection(false);

    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );
    expect(view.queryByRole("link", { name: "Diagnostics" })).toBeNull();
    expect(view.queryByText(/coin evidence tools/)).toBeNull();
  });

  it("shows no pointer while the flag is not yet known", async () => {
    const view = renderSection(null);

    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );
    expect(view.queryByRole("link", { name: "Diagnostics" })).toBeNull();
    expect(view.queryByText(/coin evidence tools/)).toBeNull();
  });

  it("follows the shared value while mounted, without remounting", async () => {
    const view = renderSection(false);

    await waitFor(() =>
      expect(view.getByText("unavailable_until_filtered")).toBeTruthy(),
    );
    expect(view.queryByRole("link", { name: "Diagnostics" })).toBeNull();

    // Another surface in this browser turned Diagnostics Mode on.
    await view.rerender({ diagnosticsMode: true });
    await waitFor(() =>
      expect(view.getByRole("link", { name: "Diagnostics" })).toBeTruthy(),
    );

    // And off again: the pointer must not advertise a gated page.
    await view.rerender({ diagnosticsMode: false });
    await waitFor(() =>
      expect(view.queryByRole("link", { name: "Diagnostics" })).toBeNull(),
    );
  });
});

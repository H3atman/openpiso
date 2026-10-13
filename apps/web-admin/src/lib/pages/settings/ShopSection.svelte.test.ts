// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getSettings: vi.fn(async () => ({
    shop_name: "Test Shop",
    shutdown_grace_seconds: 30,
    offline_grace_seconds: 120,
  })),
  getShopRate: vi.fn(async () => ({ amount_cents: 100, seconds: 300 })),
  updateShopRate: vi.fn(async (input: { amount_cents: number; seconds: number }) => input),
  getEconomySettings: vi.fn(async () => ({
    earn_cents_per_point: 5000,
    rate_amount_cents: 100,
    rate_seconds: 600,
    effective_bonus_basis_points: 0,
    unsafe_bonus_threshold_basis_points: 5000,
    unsafe_bonus: false,
  })),
  getRedemptionTiers: vi.fn(async () => [
    { id: 1, points: 10, seconds: 600 },
    { id: 2, points: 50, seconds: 3600 },
  ]),
  getCoinDeviceSettings: vi.fn(async () => ({
    com_port: null,
    baud_rate: 9600,
    set_pin_line: "dtr",
    coin_pin_line: "cts",
    coin_pin_polarity: "normally_open",
    inter_coin_gap_ms: 200,
    inactivity_timeout_seconds: 15,
    hard_cap_seconds: 180,
    pulse_map: [
      { pulse_count: 1, amount_cents: 100 },
      { pulse_count: 5, amount_cents: 500 },
    ],
    calibration_required: false,
  })),
  getShopHours: vi.fn(async () => ({
    enabled: false,
    last_call_lead_minutes: 15,
    days: Array.from({ length: 7 }, (_, weekday) => ({
      weekday,
      mode: "open_period",
      open_minute: weekday === 0 ? 540 : 480,
      close_minute: 1320,
    })),
    override: null,
    status: { open: true, next_change_at: null, reason: "schedule", last_call_active: false },
  })),
  updateShopHours: vi.fn(),
}));

vi.mock("../../theme", () => ({ applyThemeRecord: vi.fn() }));

import * as api from "../../api";
import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

describe("Shop Settings", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllEnvs();
  });

  beforeEach(() => {
    document.body.innerHTML = "";
    vi.mocked(api.getShopRate).mockResolvedValue({ amount_cents: 100, seconds: 300 });
    vi.mocked(api.getCoinDeviceSettings).mockResolvedValue({
      com_port: null,
      baud_rate: 9600,
      set_pin_line: "dtr",
      coin_pin_line: "cts",
      coin_pin_polarity: "normally_open",
      inter_coin_gap_ms: 200,
      inactivity_timeout_seconds: 15,
      hard_cap_seconds: 180,
      pulse_map: [
        { pulse_count: 1, amount_cents: 100 },
        { pulse_count: 5, amount_cents: 500 },
      ],
      calibration_required: false,
    });
    vi.mocked(api.updateShopRate).mockClear();
    vi.mocked(api.getEconomySettings).mockClear();
  });

  it("renders Shop Rate between Shop and Shop Hours and saves edited values", async () => {
    const user = userEvent.setup();
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => expect((view.getByLabelText("Amount (whole pesos)") as HTMLInputElement).value).toBe("1"));

    expect(Array.from(view.container.querySelectorAll("h3")).map((heading) => heading.textContent?.trim())).toEqual([
      "Shop",
      "Shop Rate",
      "Shop Hours",
    ]);
    const initialPreview = await waitFor(() => view.getByRole("status"));
    expect(initialPreview.textContent?.replace(/\s+/g, " ")).toContain("₱1 buys 5 min.");
    expect(initialPreview.textContent?.replace(/\s+/g, " ")).toContain("₱5 buys 25 min.");
    await user.clear(view.getByLabelText("Amount (whole pesos)"));
    await user.type(view.getByLabelText("Amount (whole pesos)"), "5");
    await user.clear(view.getByLabelText("Seconds"));
    await user.type(view.getByLabelText("Seconds"), "601");
    expect(view.getByText(/= 10 min 1 sec/)).toBeTruthy();

    vi.mocked(api.updateShopRate).mockResolvedValueOnce({ amount_cents: 700, seconds: 720 });
    await user.click(view.getByRole("button", { name: "Save Shop Rate" }));
    await waitFor(() => expect(api.updateShopRate).toHaveBeenCalledWith({ amount_cents: 500, seconds: 601 }));
    expect((view.getByLabelText("Amount (whole pesos)") as HTMLInputElement).value).toBe("7");
    expect((view.getByLabelText("Seconds") as HTMLInputElement).value).toBe("720");
    expect(view.getByText(/= 12 min/)).toBeTruthy();
  });

  it("refreshes Points economy figures after a successful Shop Rate save", async () => {
    const user = userEvent.setup();
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => expect(view.getByLabelText("Amount (whole pesos)")).toBeTruthy());

    vi.mocked(api.updateShopRate).mockResolvedValueOnce({ amount_cents: 500, seconds: 601 });
    vi.mocked(api.getEconomySettings).mockResolvedValueOnce({
      earn_cents_per_point: 5000,
      rate_amount_cents: 500,
      rate_seconds: 601,
      effective_bonus_basis_points: 0,
      unsafe_bonus_threshold_basis_points: 5000,
      unsafe_bonus: false,
    });

    await user.click(view.getByRole("button", { name: "Save Shop Rate" }));
    await waitFor(() => expect(api.updateShopRate).toHaveBeenCalled());

    await view.rerender({ section: "points" });
    await waitFor(() => expect(api.getEconomySettings).toHaveBeenCalledOnce());
    expect(view.getByText("0.99%")).toBeTruthy();
    expect(view.getAllByText("1.19%")).toHaveLength(2);
  });

  it("shows truncation losses without blocking a Shop Rate save", async () => {
    const user = userEvent.setup();
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => expect(view.getByLabelText("Amount (whole pesos)")).toBeTruthy());

    await user.clear(view.getByLabelText("Amount (whole pesos)"));
    await user.type(view.getByLabelText("Amount (whole pesos)"), "5");
    await user.clear(view.getByLabelText("Seconds"));
    await user.type(view.getByLabelText("Seconds"), "601");

    const advisory = await waitFor(() => view.getByRole("status"));
    const advisoryText = advisory.textContent?.replace(/\s+/g, " ") ?? "";
    expect(advisoryText).toContain("₱1 buys 2 min. Loses 0.20 sec");
    expect(advisoryText).toContain("₱5 buys 10 min 1 sec");
    expect(advisoryText).toContain("advisory only");

    await user.click(view.getByRole("button", { name: "Save Shop Rate" }));
    await waitFor(() =>
      expect(api.updateShopRate).toHaveBeenCalledWith({ amount_cents: 500, seconds: 601 }),
    );
  });

  it("reports a Shop Rate load failure inline", async () => {
    vi.mocked(api.getShopRate).mockRejectedValueOnce(new Error("rate unavailable"));
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => expect(view.getByText("rate unavailable")).toBeTruthy());
  });

  it("copies one Shop Hours row to every weekday", async () => {
    const user = userEvent.setup();
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => expect(view.getByLabelText("Monday Open Period start time")).toBeTruthy());

    await user.click(view.getAllByRole("button", { name: "Copy to all days" })[0]);

    const times = Array.from(view.container.querySelectorAll<HTMLInputElement>("input[type=\"time\"]"));
    expect(times).toHaveLength(14);
    expect(times.filter((input) => input.value === "09:00")).toHaveLength(7);
  });

  it("prefills the override expiry with the supplied boundary in the browser's local time", async () => {
    // 22:00+08:00 is 14:00Z, which is 10:00 in New York (EDT, UTC-4).
    vi.stubEnv("TZ", "America/New_York");
    vi.mocked(api.getShopHours).mockResolvedValueOnce({
      enabled: true,
      last_call_lead_minutes: 15,
      days: Array.from({ length: 7 }, (_, weekday) => ({
        weekday,
        mode: "open_period",
        open_minute: 480,
        close_minute: 1320,
      })),
      override: null,
      status: {
        open: true,
        next_change_at: "2099-06-21T22:00:00+08:00",
        reason: "schedule",
        last_call_active: false,
      },
    });
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => {
      expect((view.getByLabelText("Expires at") as HTMLInputElement).value).toBe(
        "2099-06-21T10:00",
      );
    });
  });

  it("falls back to one hour from now when no boundary is supplied", async () => {
    vi.stubEnv("TZ", "America/New_York");
    // Only Date is faked so waitFor's timers keep running. 12:30Z is 07:30 EST; +1h is 08:30.
    vi.useFakeTimers({ toFake: ["Date"], now: new Date("2099-01-15T12:30:00Z") });
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => {
      expect((view.getByLabelText("Expires at") as HTMLInputElement).value).toBe(
        "2099-01-15T08:30",
      );
    });
  });

  it("renders and refreshes the server-resolved Shop Status after save", async () => {
    const user = userEvent.setup();
    const view = render(SettingsPageHarness, { props: { section: "shop" } });
    await waitFor(() => expect(view.getByText("Shop Open")).toBeTruthy());

    vi.mocked(api.updateShopHours).mockResolvedValueOnce({
      enabled: true,
      last_call_lead_minutes: 15,
      days: Array.from({ length: 7 }, (_, weekday) => ({
        weekday,
        mode: "closed_all_day",
      })),
      override: null,
      status: {
        open: false,
        next_change_at: null,
        reason: "schedule",
        last_call_active: false,
      },
    });

    await user.click(view.getByRole("button", { name: "Save Shop Hours" }));
    await waitFor(() => expect(view.getByText("Shop Closed")).toBeTruthy());
  });
});

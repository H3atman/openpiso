// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getWheelSettings: vi.fn(),
  updateWheelSettings: vi.fn(),
}));

vi.mock("../../theme", () => ({ applyThemeRecord: vi.fn() }));

import * as api from "../../api";
import { render, waitFor } from "@testing-library/svelte";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

type WheelSettings = Awaited<ReturnType<typeof api.getWheelSettings>>;

function wheel(shopRatePointValueSeconds: number): WheelSettings {
  return {
    enabled: true,
    bokya_rate_basis_points: null,
    free_spin_cap: 0,
    spin_duration: 5,
    password_confirmation_required: false,
    version: 1,
    segments: [
      { label: "Prize", seconds: 300, points_centi: 0, free_spin: false, probability_basis_points: 5000 },
      { label: "Try again", seconds: 0, points_centi: 0, free_spin: false, probability_basis_points: 5000 },
    ],
    expected_payout_seconds: 150,
    expected_payout_points_centi: 0,
    point_value_seconds: 1440,
    house_edge_basis_points: 8958,
    unsafe_house_edge: false,
    shop_rate_point_value_seconds: shopRatePointValueSeconds,
    shop_rate_house_edge_basis_points: shopRatePointValueSeconds > 0
      ? Math.round(((shopRatePointValueSeconds - 150) / shopRatePointValueSeconds) * 10_000)
      : 0,
    unsafe_shop_rate_house_edge: shopRatePointValueSeconds <= 150,
  };
}

describe("Wheel Shop Rate advisory", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.mocked(api.getWheelSettings).mockReset();
  });

  it("shows a standing advisory when the active Shop Rate makes the stored wheel unsafe", async () => {
    vi.mocked(api.getWheelSettings).mockResolvedValueOnce(wheel(100));

    const view = render(SettingsPageHarness, { props: { section: "wheel" } });

    await waitFor(() =>
      expect(view.getByText("This wheel is unsafe under the active Shop Rate.")).toBeTruthy(),
    );
    expect(view.getByText(/earned point currently buys only/)).toBeTruthy();
  });

  it("clears the advisory when a later load reports a safe Shop Rate", async () => {
    vi.mocked(api.getWheelSettings)
      .mockResolvedValueOnce(wheel(100))
      .mockResolvedValueOnce(wheel(500));

    const view = render(SettingsPageHarness, { props: { section: "wheel" } });
    await waitFor(() =>
      expect(view.getByText("This wheel is unsafe under the active Shop Rate.")).toBeTruthy(),
    );

    view.unmount();
    const reloaded = render(SettingsPageHarness, { props: { section: "wheel" } });
    await waitFor(() => expect(reloaded.queryByText("This wheel is unsafe under the active Shop Rate.")).toBeNull());
  });
});

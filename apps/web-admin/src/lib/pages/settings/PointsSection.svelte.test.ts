// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
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
}));

vi.mock("../../theme", () => ({ applyThemeRecord: vi.fn() }));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

async function renderTable(label: string) {
  const user = userEvent.setup();
  const view = render(SettingsPageHarness, { props: { section: "points" } });
  const inputs = () =>
    Array.from(view.container.querySelectorAll<HTMLInputElement>(`input[aria-label="${label}"]`));
  await waitFor(() => expect(inputs().length).toBe(2));
  return { user, inputs };
}

describe("Points Settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("keeps focus in the Redemption Tier points field while a digit reorders the row", async () => {
    const { user, inputs } = await renderTable("Tier points");

    const edited = inputs()[0];
    await user.click(edited);
    await user.type(edited, "0");

    expect(edited.value).toBe("100");
    expect(document.activeElement).toBe(edited);
  });
});

// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getCoinPromo: vi.fn(async () => ({
    enabled: true,
    tiers: [
      { amount_cents: 100, bonus_seconds: 60, effective_bonus_basis_points: 0 },
      { amount_cents: 500, bonus_seconds: 300, effective_bonus_basis_points: 0 },
    ],
    account_worst_case_basis_points: 0,
    unsafe_bonus_threshold_basis_points: 5000,
    unsafe_bonus: false,
  })),
  previewCoinPromo: vi.fn(async () => {
    throw new Error("preview disabled in test");
  }),
}));

vi.mock("../../theme", () => ({ applyThemeRecord: vi.fn() }));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

async function renderTable(label: string) {
  const user = userEvent.setup();
  const view = render(SettingsPageHarness, { props: { section: "coin-promo" } });
  const inputs = () =>
    Array.from(view.container.querySelectorAll<HTMLInputElement>(`input[aria-label="${label}"]`));
  await waitFor(() => expect(inputs().length).toBe(2));
  return { user, inputs };
}

describe("Coin Promo Settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("keeps focus in the Coin Promo amount field while a digit reorders the row", async () => {
    const { user, inputs } = await renderTable("Coin amount in pesos");

    // Row 0 holds ₱1; appending a digit makes it ₱12, which sorts past the ₱5 row.
    const edited = inputs()[0];
    await user.click(edited);
    await user.type(edited, "2");

    expect(edited.value).toBe("12");
    expect(document.activeElement).toBe(edited);
  });
});

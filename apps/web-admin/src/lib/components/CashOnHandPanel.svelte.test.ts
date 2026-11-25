// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({ getCashOnHand: vi.fn(), collectCash: vi.fn() }));
vi.mock("svelte-sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/svelte";
import { toast } from "svelte-sonner";
import * as api from "../api";
import CashOnHandPanel from "./CashOnHandPanel.svelte";

const cashOnHand = {
  coin_box: {
    uncollected_cents: 12_500,
    last_collection: { amount_cents: 4_000, collected_at: "2026-11-20 08:30:00" },
  },
  voucher_cash: { uncollected_cents: 0, last_collection: null },
};

describe("Cash on hand panel", () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.getCashOnHand).mockResolvedValue(cashOnHand);
  });

  it("shows Uncollected and last collected per card, with Never before the first", async () => {
    const view = render(CashOnHandPanel);
    const coinBox = within(await view.findByRole("region", { name: "Coin Box" }));
    expect(coinBox.getByText("₱125.00")).toBeTruthy();
    expect(coinBox.getByText(/Last collected/)).toBeTruthy();
    expect(coinBox.getByText(/₱40\.00/)).toBeTruthy();

    const voucher = within(view.getByRole("region", { name: "Voucher Cash" }));
    expect(voucher.getByText("₱0.00")).toBeTruthy();
    expect(voucher.getByText(/Never/)).toBeTruthy();
  });

  it("disables Collect at ₱0 and enables it otherwise", async () => {
    const view = render(CashOnHandPanel);
    const coinBox = within(await view.findByRole("region", { name: "Coin Box" }));
    const voucher = within(view.getByRole("region", { name: "Voucher Cash" }));
    expect((coinBox.getByRole("button", { name: "Collect" }) as HTMLButtonElement).disabled).toBe(false);
    expect((voucher.getByRole("button", { name: "Collect" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("confirms the amount, collects only the kind, and reports the recorded amount", async () => {
    // The Admin fixes the amount at confirm time: more coins arrived while the dialog was open.
    vi.mocked(api.collectCash).mockResolvedValue({
      kind: "coin_box",
      amount_cents: 13_000,
      collected_at: "2026-11-26 10:00:00",
    });
    const view = render(CashOnHandPanel);
    const coinBox = within(await view.findByRole("region", { name: "Coin Box" }));
    await fireEvent.click(coinBox.getByRole("button", { name: "Collect" }));

    const dialog = within(view.getByRole("dialog"));
    expect(dialog.getByText(/₱125\.00/)).toBeTruthy();
    expect(api.collectCash).not.toHaveBeenCalled();

    vi.mocked(api.getCashOnHand).mockResolvedValue({
      ...cashOnHand,
      coin_box: {
        uncollected_cents: 0,
        last_collection: { amount_cents: 13_000, collected_at: "2026-11-26 10:00:00" },
      },
    });
    await fireEvent.click(dialog.getByRole("button", { name: "Collect" }));

    await waitFor(() => expect(api.collectCash).toHaveBeenCalledWith("coin_box"));
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith(expect.stringContaining("₱130.00")),
    );
    expect(view.queryByRole("dialog")).toBeNull();
    await waitFor(() =>
      expect(
        (within(view.getByRole("region", { name: "Coin Box" })).getByRole("button", {
          name: "Collect",
        }) as HTMLButtonElement).disabled,
      ).toBe(true),
    );
  });

  it("collects nothing when the dialog is cancelled", async () => {
    const view = render(CashOnHandPanel);
    const coinBox = within(await view.findByRole("region", { name: "Coin Box" }));
    await fireEvent.click(coinBox.getByRole("button", { name: "Collect" }));
    await fireEvent.click(within(view.getByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(view.queryByRole("dialog")).toBeNull();
    expect(api.collectCash).not.toHaveBeenCalled();
  });
});

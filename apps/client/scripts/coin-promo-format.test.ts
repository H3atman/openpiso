import { describe, expect, test } from "bun:test";
import { formatCoinPromoTier, formatCoinCredit } from "../src/lib/format";

describe("Coin Promo display formatting", () => {
  test("formats integer cents and whole seconds only at display time", () => {
    expect(formatCoinPromoTier({ amount_cents: 500, bonus_seconds: 600 })).toBe(
      "₱5 → +10 min",
    );
    expect(formatCoinPromoTier({ amount_cents: 1050, bonus_seconds: 90 })).toBe(
      "₱10.50 → +1 min 30 sec",
    );
  });

  test("states credited total and Coin Promo portion", () => {
    expect(formatCoinCredit(2_100, 600)).toBe(
      "35 min credited · 10 min Coin Promo",
    );
    expect(formatCoinCredit(300, 0)).toBe("5 min credited");
  });
});

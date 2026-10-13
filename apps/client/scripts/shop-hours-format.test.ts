import { describe, expect, test } from "bun:test";

import { formatShopClosingTime } from "../src/lib/format";

describe("Last Call formatting", () => {
  test("renders the Admin host's scheduled closing time", () => {
    expect(formatShopClosingTime("2026-08-03T22:00:00+08:00")).toBe("10:00 PM");
  });

  test("uses a safe fallback when Shop Status has no change time", () => {
    expect(formatShopClosingTime(null)).toBe("soon");
  });
});

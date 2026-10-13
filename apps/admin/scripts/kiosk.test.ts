import { describe, expect, test } from "bun:test";

import {
  canToggleKioskLock,
  equivalentTimeSeconds,
  insertScreenStateLine,
  isAvailablePc,
  kioskLockSignal,
  statusTone,
  unavailabilityReason,
} from "../src/lib/kiosk";
import { formatShopClosingTime, formatShopNextChange } from "../src/lib/format";
import type { KioskConfig, Pc, PcStatus } from "../src/lib/types";

function pc(status: PcStatus): Pc {
  return {
    id: "pc-01",
    name: "PC 01",
    status,
    remaining_seconds: status === "in_use" ? 300 : 0,
    guest_remaining_seconds: status === "in_use" ? 300 : 0,
    timer_mode: "guest",
    active_session: null,
    last_seen_at: null,
  };
}

describe("Central Terminal insert-screen state", () => {
  test("shows the keypad PC number and Guest Time without a Username", () => {
    const guestPc = pc("in_use");
    guestPc.name = "Window seat";

    expect(insertScreenStateLine(guestPc)).toBe(
      "PC 01 · In use · Remaining Time: 00:05:00",
    );
  });

  test("shows the Active Session Username", () => {
    const activeSessionPc = pc("in_use");
    activeSessionPc.timer_mode = "account";
    activeSessionPc.active_session = {
      id: "session-01",
      account_id: "account-01",
      username: "Maria",
      pc_id: activeSessionPc.id,
      remaining_seconds: 300,
      points_balance_centi: 400,
      claimable_guest_seconds: 0,
    };

    activeSessionPc.remaining_seconds = 0;

    expect(insertScreenStateLine(activeSessionPc, 240)).toBe(
      "PC 01 · In use · Remaining Time: 00:04:00 · Logged in: Maria",
    );
  });

  test.each(["online", "offline", "locked"] as const)(
    "keeps a positive-time %s PC plain",
    (status) => {
      const selectedPc = pc(status);
      selectedPc.remaining_seconds = 300;

      expect(insertScreenStateLine(selectedPc)).toBeNull();
    },
  );
});

describe("Central Terminal Shop Rate preview", () => {
  const rate: KioskConfig = { rate_amount_cents: 100, rate_seconds: 300 };

  test("uses the current rate for the read-only equivalent-time preview", () => {
    expect(equivalentTimeSeconds(500, rate)).toBe(1500);
    expect(equivalentTimeSeconds(500, { ...rate, rate_seconds: 600 })).toBe(3000);
  });

  test("truncates fractional preview seconds like the server", () => {
    expect(
      equivalentTimeSeconds(100, {
        rate_amount_cents: 300,
        rate_seconds: 10,
      }),
    ).toBe(3);
  });
});

describe("Central Terminal Shop Status", () => {
  test("renders the server-provided next status change without resolving schedule state", () => {
    expect(formatShopNextChange("2026-08-03T08:00:00+08:00")).toContain(
      "Next status change",
    );
    expect(formatShopNextChange(null)).toBe("No scheduled status change.");
  });

  test("formats the server-provided scheduled closing time for Last Call", () => {
    expect(formatShopClosingTime("2026-08-03T22:00:00+08:00")).toBe("10:00 PM");
    expect(formatShopClosingTime(null)).toBe("soon");
  });
});

describe("Central Terminal PC availability", () => {
  test.each([
    ["online", true],
    ["in_use", true],
    ["offline", false],
    ["locked", false],
  ] as const)("treats %s as selectable: %s", (status, expected) => {
    expect(isAvailablePc(pc(status))).toBe(expected);
  });

  test("returns rejection reasons only for blocked statuses", () => {
    expect(unavailabilityReason(pc("offline"), "1")).toBe("PC 01 is offline.");
    expect(unavailabilityReason(pc("locked"), "1")).toBe("PC 01 is locked.");
    expect(unavailabilityReason(null, "1")).toBe("No PC 01 on this terminal.");
  });

  test.each([
    ["online", "Available", "bg-emerald-500"],
    ["in_use", "In use — top-up OK", "bg-amber-500"],
    ["offline", "Offline", "bg-zinc-400"],
    ["locked", "Locked", "bg-rose-500"],
  ] as const)("maps %s to its kiosk label and tone", (status, label, dot) => {
    expect(statusTone(status)).toMatchObject({ label, dot });
  });
});

describe("Kiosk Lock signal and gesture (ADR-0056)", () => {
  test("locked reads as the calm, normal state", () => {
    const signal = kioskLockSignal(true);

    expect(signal.label).toBe("Kiosk · Locked");
    expect(signal.dot).toContain("bg-primary");
    expect(signal.description).toBe("Kiosk is pinned above other windows");
  });

  test("unlocked is visibly different but not an error tone", () => {
    const signal = kioskLockSignal(false);

    expect(signal.label).toBe("Kiosk · Unlocked");
    expect(signal.dot).not.toBe(kioskLockSignal(true).dot);
    // The kiosk faces customers: the anomaly must not read as a fault.
    expect(signal.dot).not.toContain("destructive");
    expect(signal.dot).not.toContain("rose");
  });

  test("the unlock gesture is live only on the grid", () => {
    expect(canToggleKioskLock("grid", false)).toBe(true);
    // A live Selection Window means a customer is feeding coins against a
    // server-side inactivity timer; no owner gesture may interrupt it.
    expect(canToggleKioskLock("grid", true)).toBe(false);
    expect(canToggleKioskLock("insert", true)).toBe(false);
    expect(canToggleKioskLock("insert", false)).toBe(false);
  });
});

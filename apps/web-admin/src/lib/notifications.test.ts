import { beforeEach, describe, expect, it, vi } from "vitest";

const { success, error } = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
}));

vi.mock("svelte-sonner", () => ({ toast: { success, error } }));

import { reportActionFailure, reportActionSuccess } from "./notifications";

describe("action notifications", () => {
  beforeEach(() => vi.clearAllMocks());

  it("reports save copy with glossary vocabulary unchanged", () => {
    reportActionSuccess("save", "Redemption Tiers");

    expect(success).toHaveBeenCalledWith("Redemption Tiers saved", {
      duration: 4_000,
    });
  });

  it("adds the Coin Debug Window reset description to a Coin Device save", () => {
    reportActionSuccess("save", "Coin Device", "coin-debug-window-reset");

    expect(success).toHaveBeenCalledWith("Coin Device saved", {
      description: "Coin Debug Window reset",
      duration: 4_000,
    });
  });

  it("distinguishes an account created with the default password", () => {
    reportActionSuccess("create", "Account", "default-password");

    expect(success).toHaveBeenCalledWith("Account created with default password.", {
      duration: 4_000,
    });
  });

  it.each([
    ["reset", "Coin Debug Window", "Coin Debug Window reset"],
    ["copy", "Coin Debug Bundle", "Coin Debug Bundle copied"],
    ["register", "PC", "PC registered"],
    ["edit", "PC", "PC updated"],
    ["remove", "PC", "PC removed"],
    ["create", "Voucher", "Voucher created"],
    ["void", "Voucher Batch", "Voucher Batch voided"],
    ["export", "Voucher Batch", "Voucher Batch exported"],
    ["print", "Voucher Batch", "Voucher Batch printed"],
    ["adjust", "Points", "Points adjusted"],
    ["approve", "Points Transfer Request", "Points Transfer Request approved"],
    ["reject", "Points Transfer Request", "Points Transfer Request rejected"],
    ["complete", "Password Reset", "Password Reset completed"],
    ["delete", "Account", "Account deleted"],
  ] as const)("reports %s success copy", (action, subject, title) => {
    reportActionSuccess(action, subject);

    expect(success).toHaveBeenCalledWith(title, { duration: 4_000 });
  });

  it("reports an approval failure with glossary vocabulary unchanged", () => {
    reportActionFailure(
      "approve",
      "Points Transfer Request",
      new Error("Request expired"),
    );

    expect(error).toHaveBeenCalledWith(
      "Could not approve Points Transfer Request",
      {
        description: "Request expired",
        duration: 8_000,
      },
    );
  });

  it("reports an Error message with action-specific failure copy", () => {
    reportActionFailure("remove", "PC", new Error("PC is online"));

    expect(error).toHaveBeenCalledWith("Could not remove PC", {
      description: "PC is online",
      duration: 8_000,
    });
  });

  it("reports a sensible message for a non-Error thrown value", () => {
    reportActionFailure("save", "Owner PIN", "Request rejected");

    expect(error).toHaveBeenCalledWith("Could not save Owner PIN", {
      description: "Request rejected",
      duration: 8_000,
    });
  });
});

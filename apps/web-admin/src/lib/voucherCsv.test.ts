import { describe, expect, test } from "vitest";
import { buildVoucherBatchCsv } from "./voucherCsv";
import type { VoucherBatchDetail } from "./types";

function batch(): VoucherBatchDetail {
  return {
    id: "batch-1",
    name: "aB7x2",
    prefix: "SHOP",
    suffix_length: 8,
    count: 1,
    kind: "sold",
    seconds_value: 3600,
    points_value_centi: 125,
    cash_amount_cents: 5050,
    expires_at: "2027-01-02T03:04:05Z",
    created_at: "2026-07-20T10:00:00Z",
    voided_at: null,
    voided_audit_event_id: null,
    redeemed_count: 1,
    vouchers: [
      {
        id: "voucher-1",
        code: "SHOP-ABCD-2345",
        kind: "sold",
        status: "redeemed",
        seconds_value: 3600,
        points_value_centi: 125,
        sold_amount_cents: 5050,
        created_at: "2026-07-20T10:00:01Z",
        sold_at: null,
        redeemed_by_account_id: "account-1",
        redeemed_by_username: "Alice, \"Ace\"",
        redeemed_at: "2026-07-21T11:00:00Z",
        expires_at: "2027-01-02T03:04:05Z",
        batch_id: "batch-1",
        batch_name: "aB7x2",
        batch_prefix: "SHOP",
      },
    ],
  };
}

const header = "code,kind,time_seconds,points,amount_pesos,expires_at,status,redeemed_by,redeemed_at,created_at";

describe("buildVoucherBatchCsv", () => {
  test("prepends a UTF-8 BOM and preserves the specified column order", () => {
    const csv = buildVoucherBatchCsv(batch());

    expect(csv.startsWith("\uFEFF")).toBe(true);
    expect(csv.slice(1).split("\r\n")[0]).toBe(header);
  });

  test("formats values and quotes commas and double quotes", () => {
    expect(buildVoucherBatchCsv(batch())).toContain(
      'SHOP-ABCD-2345,sold,3600,1.25,50.50,2027-01-02T03:04:05Z,redeemed,"Alice, ""Ace""",2026-07-21T11:00:00Z,2026-07-20T10:00:01Z',
    );
  });

  test.each(["=cmd", "+cmd", "-cmd", "@cmd", "\tcmd", "\rcmd"])(
    "escapes formula-injection cell %j",
    (username) => {
      const value = batch();
      value.vouchers[0].redeemed_by_username = username;

      expect(buildVoucherBatchCsv(value)).toContain(`'${username}`);
    },
  );
});

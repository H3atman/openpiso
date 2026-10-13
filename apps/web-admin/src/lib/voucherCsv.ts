import { formatCenti } from "./format";
import type { VoucherBatchDetail } from "./types";

const COLUMNS = [
  "code",
  "kind",
  "time_seconds",
  "points",
  "amount_pesos",
  "expires_at",
  "status",
  "redeemed_by",
  "redeemed_at",
  "created_at",
] as const;

function csvCell(value: string): string {
  const safe = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  return /[",\r\n]/.test(safe) ? `"${safe.replaceAll('"', '""')}"` : safe;
}

export function buildVoucherBatchCsv(batch: VoucherBatchDetail): string {
  const rows = batch.vouchers.map((voucher) =>
    [
      voucher.code,
      voucher.kind,
      String(voucher.seconds_value),
      formatCenti(voucher.points_value_centi),
      formatCenti(voucher.sold_amount_cents),
      voucher.expires_at ?? "",
      voucher.status,
      voucher.redeemed_by_username ?? "",
      voucher.redeemed_at ?? "",
      voucher.created_at,
    ]
      .map(csvCell)
      .join(","),
  );

  return `\uFEFF${[COLUMNS.join(","), ...rows].join("\r\n")}`;
}

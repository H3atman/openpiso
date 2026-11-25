// Sales Report date ranges: presets, bucket size, and the client-side fill of
// the sparse buckets the server returns. All dates are Admin business dates
// (`YYYY-MM-DD`); arithmetic runs in UTC so the browser's zone never shifts one.
import type { DailySalesPoint, ReportBucket } from "./types";

export type SalesPreset = "today" | "week" | "month" | "last30";
export type SalesBucket = "day" | "week" | "month";

/** Longest range (in days) drawn as day bars, and as week bars. */
export const MAX_DAY_BUCKET_DAYS = 92;
export const MAX_WEEK_BUCKET_DAYS = 366;

const DAY_MS = 86_400_000;

function toMs(date: string): number {
  return Date.parse(`${date}T00:00:00Z`);
}

function fromMs(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

function addDays(date: string, days: number): string {
  return fromMs(toMs(date) + days * DAY_MS);
}

/** The Monday on or before `date`. */
function mondayOf(date: string): string {
  const sinceMonday = (new Date(toMs(date)).getUTCDay() + 6) % 7;
  return addDays(date, -sinceMonday);
}

/** True for a real calendar date written as `YYYY-MM-DD`. */
export function isIsoDate(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(value) && fromMs(toMs(value)) === value;
}

/** The inclusive range a preset covers, relative to the Admin's business today. */
export function presetRange(preset: SalesPreset, today: string): [string, string] {
  switch (preset) {
    case "today":
      return [today, today];
    case "week": {
      const monday = mondayOf(today);
      return [monday, addDays(monday, 6)];
    }
    case "month": {
      const first = `${today.slice(0, 7)}-01`;
      const nextMonth = new Date(toMs(first));
      nextMonth.setUTCMonth(nextMonth.getUTCMonth() + 1);
      return [first, addDays(fromMs(nextMonth.getTime()), -1)];
    }
    case "last30":
      return [addDays(today, -29), today];
  }
}

/** Returns why a custom range cannot be requested, or null when it can. */
export function rangeError(from: string, to: string): string | null {
  if (!from || !to) return "Choose both dates.";
  if (!isIsoDate(from) || !isIsoDate(to)) return "Dates must be real calendar dates.";
  if (from > to) return "From date must not be after To date.";
  return null;
}

/** Bars per day up to 92 days, per week up to 366 days, per month beyond. */
export function bucketFor(from: string, to: string): SalesBucket {
  const days = (toMs(to) - toMs(from)) / DAY_MS + 1;
  if (days <= MAX_DAY_BUCKET_DAYS) return "day";
  if (days <= MAX_WEEK_BUCKET_DAYS) return "week";
  return "month";
}

/** Each bucket's label as it comes from the server, in order, for the range. */
function bucketLabels(bucket: SalesBucket, from: string, to: string): string[] {
  const labels: string[] = [];
  if (bucket === "month") {
    const [fromYear, fromMonth] = from.split("-").map(Number) as [number, number];
    const [toYear, toMonth] = to.split("-").map(Number) as [number, number];
    for (let index = fromYear * 12 + fromMonth - 1; index <= toYear * 12 + toMonth - 1; index++) {
      labels.push(`${Math.floor(index / 12)}-${String((index % 12) + 1).padStart(2, "0")}`);
    }
    return labels;
  }
  const step = bucket === "week" ? 7 : 1;
  const start = bucket === "week" ? mondayOf(from) : from;
  for (let date = start; date <= to; date = addDays(date, step)) labels.push(date);
  return labels;
}

/**
 * Expands the sparse server buckets into one point per day, Monday week or
 * month in the range, zero where there were no sales. Buckets that start after
 * the Admin's business `today` are left off: they are unwritten, not zero.
 */
export function fillSeries(
  bucket: SalesBucket,
  from: string,
  to: string,
  buckets: readonly ReportBucket[],
  today: string,
): DailySalesPoint[] {
  const byLabel = new Map(buckets.map((entry) => [entry.period_label, entry]));
  return bucketLabels(bucket, from, to)
    .map((label) => ({ label, date: bucket === "month" ? `${label}-01` : label }))
    .filter(({ date }) => date <= today)
    .map(({ label, date }) => ({
      date,
      coin_cents: byLabel.get(label)?.coin_sales_cents ?? 0,
      voucher_cents: byLabel.get(label)?.voucher_sales_cents ?? 0,
    }));
}

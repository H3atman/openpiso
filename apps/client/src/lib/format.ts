import type { ShopStatus } from "./types";

export function formatAttendanceMonth(earningMonth: string): string {
  const match = /^(\d{4})-(\d{2})$/.exec(earningMonth);
  if (!match) return earningMonth;
  const year = Number(match[1]);
  const month = Number(match[2]);
  if (!year || month < 1 || month > 12) return earningMonth;
  return new Intl.DateTimeFormat("en", {
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  }).format(new Date(Date.UTC(year, month - 1, 1)));
}

export function formatAttendanceDate(
  date: string,
  adminLocalDate: string,
): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!match) return date;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (!year || month < 1 || month > 12 || day < 1 || day > 31) return date;
  const adminYear = Number(adminLocalDate.slice(0, 4));
  return new Intl.DateTimeFormat("en", {
    month: "short",
    day: "numeric",
    ...(year !== adminYear ? { year: "numeric" } : {}),
    timeZone: "UTC",
  }).format(new Date(Date.UTC(year, month - 1, day)));
}

export function attendanceProgressPercent(
  cappedCents: number,
  thresholdCents: number,
): number {
  return Math.min(
    100,
    Math.max(0, Math.floor((Math.max(0, cappedCents) / Math.max(1, thresholdCents)) * 100)),
  );
}

export function formatPcNumber(pcId: string | null | undefined): string {
  return pcId?.replace(/^pc-/, "").padStart(2, "0") ?? "…";
}

/**
 * Points display with exactly two decimals everywhere (ADR-0038). Wire
 * values are integer centipoints; this is the only place that divides
 * by 100.
 */
export function formatPoints(pointsCenti: number): string {
  const sign = pointsCenti < 0 ? "-" : "";
  const abs = Math.abs(Math.trunc(pointsCenti));
  return `${sign}${Math.floor(abs / 100)}.${(abs % 100).toString().padStart(2, "0")}`;
}

/**
 * Parse a customer-typed points amount (at most two decimal places) into
 * integer centipoints. Returns `null` for anything else — the caller never
 * sends a sub-centipoint or non-numeric amount to the server (ADR-0038).
 */
export function parsePointsToCenti(input: string): number | null {
  const match = /^(\d+)(?:\.(\d{1,2}))?$/.exec(input.trim());
  if (!match) return null;
  const whole = Number(match[1]);
  const fraction = Number((match[2] ?? "").padEnd(2, "0") || "0");
  return whole * 100 + fraction;
}

export function formatDuration(totalSeconds: number): string {
  const safeSeconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(safeSeconds / 3600);
  const minutes = Math.floor((safeSeconds % 3600) / 60);
  const seconds = safeSeconds % 60;
  return [hours, minutes, seconds]
    .map((value) => value.toString().padStart(2, "0"))
    .join(":");
}

export function formatProseDuration(totalSeconds: number): string {
  const safeSeconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(safeSeconds / 3600);
  const minutes = Math.floor((safeSeconds % 3600) / 60);
  const seconds = safeSeconds % 60;
  const parts = [
    hours ? `${hours} hour${hours === 1 ? "" : "s"}` : "",
    minutes ? `${minutes} min` : "",
    seconds ? `${seconds} sec` : "",
  ].filter(Boolean);
  return parts.join(" ") || "0 sec";
}

export function formatPeso(cents: number): string {
  const whole = Math.floor(cents / 100);
  const fraction = cents % 100;
  return `₱${whole}${fraction ? `.${fraction.toString().padStart(2, "0")}` : ""}`;
}

export function formatCoinPromoTier(tier: {
  amount_cents: number;
  bonus_seconds: number;
}): string {
  return `${formatPeso(tier.amount_cents)} → +${formatProseDuration(tier.bonus_seconds)}`;
}

export function formatCoinCredit(
  secondsAdded: number,
  coinPromoBonusSeconds: number,
): string {
  const credited = `${formatProseDuration(secondsAdded)} credited`;
  return coinPromoBonusSeconds > 0
    ? `${credited} · ${formatProseDuration(coinPromoBonusSeconds)} Coin Promo`
    : credited;
}

export function formatShopClosingTime(value: string | null): string {
  if (!value) return "soon";
  const match = /T(\d{2}):(\d{2})/.exec(value);
  if (!match) return "soon";
  const hour = Number(match[1]);
  const period = hour >= 12 ? "PM" : "AM";
  return `${hour % 12 || 12}:${match[2]} ${period}`;
}

/**
 * Render the server-resolved Shop Status change without converting the
 * timestamp to the Client PC's timezone. The RFC 3339 local clock fields are
 * from the Admin host, which is the authority for Shop Hours.
 */
export function formatShopClosed(status: ShopStatus | null): string {
  if (!status?.next_change_at) return "Shop Closed — no opening is scheduled.";
  const match = /T(\d{2}):(\d{2})/.exec(status.next_change_at);
  if (!match) return "Shop Closed — opening time unavailable.";
  const hour = Number(match[1]);
  const minute = match[2];
  const period = hour >= 12 ? "PM" : "AM";
  const displayHour = hour % 12 || 12;
  const time = `${displayHour}:${minute} ${period}`;
  return status.reason === "override"
    ? `Shop Closed until ${time}`
    : `Shop Closed — opens ${time}`;
}

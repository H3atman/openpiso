import type {
  ClientReachability,
  GuardCoverageFact,
  HostExitReason,
  HostReachabilityFact,
  IncidentKind,
} from "./types";

const pesoFormatter = new Intl.NumberFormat("en-PH", {
  style: "currency",
  currency: "PHP",
  minimumFractionDigits: 2,
});

export function formatMoney(cents: number): string {
  return pesoFormatter.format(cents / 100);
}

/**
 * Money for a chart axis tick, where the exact `formatMoney` string is too wide
 * to spend plot width on: whole pesos drop the decimals and thousands read as
 * `₱1.5k`. Anything that is not a whole peso keeps the exact format, so cents
 * are shortened away only when there are none.
 */
export function formatMoneyCompact(cents: number): string {
  if (!Number.isInteger(cents) || cents % 100 !== 0) return formatMoney(cents);
  const pesos = cents / 100;
  if (Math.abs(pesos) < 1_000) return `₱${pesos.toLocaleString("en-PH")}`;
  return `₱${Number((pesos / 1_000).toFixed(1))}k`;
}

/**
 * Points display with exactly two decimals everywhere (ADR-0038). Wire
 * values are integer centipoints; this is the only place that divides
 * by 100.
 */
export function formatCenti(value: number): string {
  const sign = value < 0 ? "-" : "";
  const abs = Math.abs(Math.trunc(value));
  return `${sign}${Math.floor(abs / 100)}.${(abs % 100).toString().padStart(2, "0")}`;
}

export function formatPoints(pointsCenti: number): string {
  return formatCenti(pointsCenti);
}

/**
 * Parse an owner-typed points amount (at most two decimal places) into
 * integer centipoints; `null` for anything else (ADR-0038).
 */
export function parsePointsToCenti(input: string): number | null {
  const match = /^(\d+)(?:\.(\d{1,2}))?$/.exec(input.trim());
  if (!match) return null;
  const whole = Number(match[1]);
  const fraction = Number((match[2] ?? "").padEnd(2, "0") || "0");
  const value = whole * 100 + fraction;
  return Number.isSafeInteger(value) ? value : null;
}

/** Parse a non-negative peso amount exactly into integer cents. */
export function parsePesosToCents(input: string): number | null {
  const match = /^(\d+)(?:\.(\d{1,2}))?$/.exec(input.trim());
  if (!match) return null;
  const whole = Number(match[1]);
  const fraction = Number((match[2] ?? "").padEnd(2, "0") || "0");
  const value = whole * 100 + fraction;
  return Number.isSafeInteger(value) ? value : null;
}

/** Plain, locale-free peso text for editable amount fields. */
export function formatCentsAsPesosInput(cents: number): string {
  return (cents / 100).toString();
}

/** Effective bonus in basis points, shown with two decimal places. */
export function formatBonusPercent(basisPoints: number): string {
  return `${(basisPoints / 100).toFixed(2)}%`;
}

/** Formats the frozen inclusive Admin-local claim date without timezone conversion. */
export function formatAttendanceDeadline(inclusiveAdminDate: string): string {
  return formatReportDate(inclusiveAdminDate);
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

/** Prose duration using the glossary's canonical h/min/sec units. */
export function formatProseDuration(totalSeconds: number): string {
  const safeSeconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(safeSeconds / 3_600);
  const minutes = Math.floor((safeSeconds % 3_600) / 60);
  const seconds = safeSeconds % 60;
  const parts: string[] = [];
  if (hours > 0) parts.push(`${hours} ${hours === 1 ? "hour" : "hours"}`);
  if (minutes > 0) parts.push(`${minutes} min`);
  if (seconds > 0 || parts.length === 0) parts.push(`${seconds} sec`);
  return parts.join(" ");
}

export function formatSignedProseDuration(totalSeconds: number): string {
  const seconds = Math.trunc(totalSeconds);
  const sign = seconds < 0 ? "-" : seconds > 0 ? "+" : "";
  return `${sign}${formatProseDuration(Math.abs(seconds))}`;
}

/** Exact decimal rendering for a truncated fraction of one second. */
export function formatFractionalSeconds(remainder: number, denominator: number): string {
  const digits: string[] = [];
  let remaining = remainder;
  for (let i = 0; i < 6 && remaining !== 0; i += 1) {
    remaining *= 10;
    digits.push(Math.floor(remaining / denominator).toString());
    remaining %= denominator;
  }
  return `0.${digits.join("").padEnd(2, "0")}${remaining === 0 ? "" : "…"} sec`;
}

export function formatSignedDuration(totalSeconds: number): string {
  const magnitude = Math.abs(Math.trunc(totalSeconds));
  const sign = magnitude === 0 ? "" : totalSeconds < 0 ? "-" : "+";
  return `${sign}${formatDuration(magnitude)}`;
}

/** Formats a signed Admin-minus-Client wall-clock difference for the Owner. */
export function formatClockDifference(milliseconds: number): string {
  const seconds = Math.round(Math.abs(milliseconds) / 1_000);
  const direction = milliseconds > 0 ? "behind" : "ahead of";
  return `${formatProseDuration(seconds)} ${direction} the Admin clock`;
}

export function formatDayDuration(totalSeconds: number): string {
  const safeSeconds = Math.max(0, Math.floor(totalSeconds));
  const days = Math.floor(safeSeconds / 86_400);
  const hours = Math.floor((safeSeconds % 86_400) / 3_600);
  const minutes = Math.floor((safeSeconds % 3_600) / 60);
  const seconds = safeSeconds % 60;
  return [days, hours, minutes, seconds]
    .map((value) => value.toString().padStart(2, "0"))
    .join(":");
}

export function formatTransactionTime(value: string): string {
  const normalized = value.includes("T") ? value : `${value.replace(" ", "T")}Z`;
  return new Intl.DateTimeFormat("en-PH", {
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(normalized));
}

export function statusLabel(status: string): string {
  return status.replaceAll("_", " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

export function formatDate(value: string): string {
  const normalized = value.includes("T") ? value : `${value.replace(" ", "T")}Z`;
  return new Intl.DateTimeFormat("en-PH", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date(normalized));
}

export function formatDateShort(value: string): string {
  const normalized = value.includes("T") ? value : `${value.replace(" ", "T")}Z`;
  return new Intl.DateTimeFormat("en-PH", {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(new Date(normalized));
}

const reportMonthFormatter = new Intl.DateTimeFormat("en-PH", {
  month: "short",
});

function reportDateParts(value: string): [number, number, number] {
  const [year, month, day] = value.split("-").map(Number);
  return [year, month, day];
}

function reportMonthName(month: number): string {
  return reportMonthFormatter.format(new Date(2000, month - 1, 1));
}

const monthNameFormatter = new Intl.DateTimeFormat("en-PH", { month: "long" });

/** Full month names, January first, for owner-facing month pickers. */
export const MONTH_NAMES = Array.from({ length: 12 }, (_, index) =>
  monthNameFormatter.format(new Date(2000, index, 1)),
);

function pad2(value: number): string {
  return value.toString().padStart(2, "0");
}

export function lastDayOfMonth(year: number, month: number): number {
  return new Date(year, month, 0).getDate();
}

/** A local `YYYY-MM-DD` business date, built without UTC date shifting. */
export function isoDate(year: number, month: number, day: number): string {
  return `${year}-${pad2(month)}-${pad2(day)}`;
}

/** The inclusive business-date bounds of one calendar month. */
export function monthDateRange(year: number, month: number): [string, string] {
  return [isoDate(year, month, 1), isoDate(year, month, lastDayOfMonth(year, month))];
}

/** One business date, formatted without UTC date shifting. */
export function formatReportDate(date: string): string {
  const [year, month, day] = reportDateParts(date);
  return `${reportMonthName(month)} ${day}, ${year}`;
}

/** Formats server-owned inclusive report ranges without UTC date shifting. */
export function formatSalesDateRange(
  fromDate: string | null,
  toDate: string | null,
): string {
  if (!fromDate && !toDate) return "All time";
  const date = fromDate ?? toDate;
  if (!date) return "All time";
  if (!fromDate || !toDate) return formatDateShort(`${date}T00:00:00`);

  const [fromYear, fromMonth, fromDay] = reportDateParts(fromDate);
  const [toYear, toMonth, toDay] = reportDateParts(toDate);
  if (fromDate === toDate) {
    return formatReportDate(fromDate);
  }
  if (fromYear === toYear && fromMonth === toMonth) {
    return `${reportMonthName(fromMonth)} ${fromDay}–${toDay}, ${toYear}`;
  }
  if (fromYear === toYear) {
    return `${reportMonthName(fromMonth)} ${fromDay}–${reportMonthName(toMonth)} ${toDay}, ${toYear}`;
  }
  return `${reportMonthName(fromMonth)} ${fromDay}, ${fromYear}–${reportMonthName(toMonth)} ${toDay}, ${toYear}`;
}

interface AuditSummaryRow {
  summary: string;
  actor: string;
  actor_account_id: string | null;
  actor_account_username: string | null;
  account_id: string | null;
}

export function formatAuditSummary(row: AuditSummaryRow): string {
  if (
    row.actor !== "account" ||
    row.actor_account_id === null ||
    row.actor_account_id === row.account_id
  ) {
    return row.summary;
  }
  return `${row.summary} — by ${row.actor_account_username ?? "(unknown account)"}`;
}

export function formatEventType(value: string): string {
  return value.replaceAll("_", " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

export function formatDay(value: string): string {
  return new Intl.DateTimeFormat("en-PH", { dateStyle: "full" }).format(
    new Date(`${value}T00:00:00Z`),
  );
}

// --- PC Monitoring Check now (ADR-0076) ---

function hostUnavailabilityCopy(reason: HostReachabilityFact["reason"]): string {
  switch (reason) {
    case "platform_unsupported":
      return "not Windows Admin";
    case "pc_hidden":
      return "PC is hidden";
    case "registered_ip_invalid":
      return "registered IP is not valid";
    case "registered_ip_missing":
    default:
      return "no registered IP";
  }
}

/** Inline Check-now copy for the Host Reachability fact. Unavailable states
 *  always name the reason so missing evidence never reads as healthy. */
export function hostReachabilityLabel(fact: HostReachabilityFact): string {
  switch (fact.state) {
    case "reachable":
      return "Host: reachable";
    case "unreachable":
      return "Host: no reply";
    case "unavailable":
      return `Host: unavailable (${hostUnavailabilityCopy(fact.reason)})`;
  }
}

/** Inline Check-now copy for the heartbeat verdict. */
export function clientReachabilityLabel(reachability: ClientReachability): string {
  return reachability === "reachable" ? "Client: reachable" : "Client: no heartbeat";
}

/** Inline Check-now copy for the last reported Guard diagnostic (ADR-0076 /
 *  #344). Unsupported means no Heartbeat has reported the field yet — an
 *  older Client, never a fabricated verdict. */
export function guardCoverageLabel(guardCoverage: GuardCoverageFact): string {
  switch (guardCoverage) {
    case "covered":
      return "Guard: covered";
    case "unavailable":
      return "Guard: unavailable";
    case "not_applicable":
      return "Guard: not applicable";
    case "unsupported":
      return "Guard: unsupported or unknown";
  }
}

/** Owner-facing name of a PC Monitoring Incident kind, used by dashboard
 *  badges. Evidence for review, never an accusation (ADR-0076). */
export function incidentKindLabel(kind: IncidentKind): string {
  switch (kind) {
    case "client_reachability_mismatch":
      return "Client Reachability Mismatch";
    case "host_probe_coverage":
      return "Host Probe Coverage";
    case "client_guard_coverage":
      return "Client Guard Coverage";
  }
}

/** Owner-facing name of a recorded Exit Reason (ADR-0082). A normal exit is
 *  named for what it was, never dressed up as a crash; a start with no end
 *  is not a recorded reason at all and renders as a suspected hard exit. */
export function exitReasonLabel(reason: HostExitReason): string {
  switch (reason) {
    case "window_closed":
      return "Window closed";
    case "application_exit":
      return "Application exit";
    case "panic":
      return "Panic";
  }
}

/** One-sentence explanation attached to each incident badge. Host Probe
 *  Coverage explicitly says what missing coverage does and does not mean:
 *  unmonitored, not offline and not tampered (ADR-0076). Guard guidance
 *  points to Guard and installation checks; Admin never repairs services. */
export function incidentKindHint(kind: IncidentKind): string {
  switch (kind) {
    case "client_reachability_mismatch":
      return "The PC answers on the LAN but its Client stopped reporting. Evidence for review, not proof of tampering.";
    case "host_probe_coverage":
      return "This PC is running, but Admin can no longer test its reachability. Missing coverage means unmonitored — not offline or tampered.";
    case "client_guard_coverage":
      return "This PC's Client reports no Guard connection. Check the Guard service and Client installation on that PC — Admin detects only and never repairs services.";
  }
}

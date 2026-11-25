// Screenshot harness entry: mounts the real App shell at /admin/reports with
// `window.fetch` answered from fixtures, so no Admin server is needed.
// Query: `scenario=busy|empty` (default busy), `theme=light|dark`.
// The business date is 2026-11-16 (as in the Dashboard harness), but the
// chart month comes from the browser clock, so daily buckets are generated
// for whatever range the page requests. Driven by
// scripts/reports-screenshots.ts, which waits on `data-harness-served`.
import { mount } from "svelte";
import App from "../App.svelte";
import { applyThemeRecord } from "$lib/theme";
import { isoDate, lastDayOfMonth } from "$lib/format";
import type {
  AppUpdateState,
  AttendanceAccountRow,
  AttendanceEarningMonthRow,
  AttendanceFlowBucket,
  AttendanceReport,
  AttendanceReportInput,
  AttendanceReportPeriod,
  DiagnosticsMode,
  NotificationListResponse,
  NotificationUnreadCount,
  ReportBucket,
  ReportSummary,
  SalesReportOverview,
  SalesReportPc,
  ThemeMode,
  ThemeRecord,
  WebAdminSessionResponse,
} from "$lib/types";
import "../app.css";

const params = new URLSearchParams(window.location.search);
const mode: ThemeMode = params.get("theme") === "dark" ? "dark" : "light";
const scenario = params.get("scenario") ?? "busy";
if (scenario !== "busy" && scenario !== "empty") throw new Error(`unknown harness scenario: ${scenario}`);
const empty = scenario === "empty";

const theme: ThemeRecord = {
  accent_color: "#15803d",
  mode_web_admin: mode,
  mode_kiosk: "light",
  mode_client: "light",
  version: 1,
};

const session: WebAdminSessionResponse = {
  configured: true,
  authenticated: true,
  csrf_token: "harness-csrf",
};

// --- Sales ---

const businessToday = "2026-11-16";

/** Cash sales for one business date: the shop is closed on some days. */
function daySales(date: string): number {
  if (date === businessToday) return 184_500;
  const day = Number(date.slice(8));
  if (day % 9 === 4) return 0;
  return 120_000 + ((day * 37) % 11) * 9_000;
}

/** One bucket per date in the inclusive range (`YYYY-MM-DD`). */
function dailyBuckets(fromDate: string, toDate: string): ReportBucket[] {
  const buckets: ReportBucket[] = [];
  for (let date = new Date(`${fromDate}T00:00:00Z`); ; date.setUTCDate(date.getUTCDate() + 1)) {
    const label = isoDate(date.getUTCFullYear(), date.getUTCMonth() + 1, date.getUTCDate());
    if (label > toDate) return buckets;
    const cash = daySales(label);
    // Roughly a fifth of each day's sales is counter voucher cash.
    const voucher = Math.floor(cash / 500) * 100;
    buckets.push({
      period_label: label,
      coin_sales_cents: cash - voucher,
      voucher_sales_cents: voucher,
      cash_revenue_cents: cash,
      free_time_dispensed_seconds: 0,
      points_minted_by_wheel_centi: 0,
    });
  }
}

function summary(fromDate: string, toDate: string): ReportSummary {
  const buckets = empty ? [] : dailyBuckets(fromDate, toDate);
  return {
    period: "day",
    from_date: fromDate,
    to_date: toDate,
    coin_sales_cents: buckets.reduce((sum, bucket) => sum + bucket.coin_sales_cents, 0),
    voucher_sales_cents: buckets.reduce((sum, bucket) => sum + bucket.voucher_sales_cents, 0),
    cash_revenue_cents: buckets.reduce((sum, bucket) => sum + bucket.cash_revenue_cents, 0),
    free_time_dispensed_seconds: 0,
    points_minted_by_wheel_centi: 0,
    points_liability_centi: 0,
    buckets,
  };
}

// 2026-11-16 is a Monday, so the week so far is just today.
const dailyCents = daySales(businessToday);
const monthlyCents = dailyBuckets("2026-11-01", businessToday).reduce(
  (sum, bucket) => sum + bucket.cash_revenue_cents,
  0,
);
const totalCents = monthlyCents + 12_406_000;

// Each PC's share of every period's sales; PC 06 has never sold.
const shares = [0.2, 0.16, 0.15, 0.13, 0.12, 0, 0.14, 0.1];
/** Splits `cents` by `shares` in whole pesos, rounding into the first PC. */
function split(cents: number): number[] {
  const parts = shares.map((share) => Math.floor((cents * share) / 100) * 100);
  parts[0] += cents - parts.reduce((sum, part) => sum + part, 0);
  return parts;
}
const [daily, monthly, total] = [split(dailyCents), split(monthlyCents), split(totalCents)];
const pcs: SalesReportPc[] = shares.map((_, index) => ({
  id: `pc-0${index + 1}`,
  name: `PC 0${index + 1}`,
  daily_sales_cents: daily[index],
  weekly_sales_cents: daily[index],
  monthly_sales_cents: monthly[index],
  total_sales_cents: total[index],
}));

const overview: SalesReportOverview = {
  daily: { from_date: businessToday, to_date: businessToday, cash_revenue_cents: empty ? 0 : dailyCents },
  weekly: { from_date: businessToday, to_date: "2026-11-22", cash_revenue_cents: empty ? 0 : dailyCents },
  monthly: { from_date: "2026-11-01", to_date: businessToday, cash_revenue_cents: empty ? 0 : monthlyCents },
  total: { from_date: null, to_date: null, cash_revenue_cents: empty ? 0 : totalCents },
  pcs: empty ? [] : pcs,
};

// --- Attendance ---

const usernames = [
  "maria.santos", "jun_r", "kat23", "paolo.d", "liza_m", "mark.t", "ana_b", "rico88",
  "jessa.c", "ben_v", "carla.o", "dino_p", "ella.r",
];
const states: AttendanceEarningMonthRow["state"][] = ["earning", "claimable", "claimed", "expired", "forfeited"];
const earningMonths = ["2026-11", "2026-10", "2026-09"];
// One hour per Attendance Check-In; the claim window is the month after.
const monthRows: AttendanceEarningMonthRow[] = earningMonths.flatMap((month, monthIndex) =>
  usernames.slice(0, 9 - monthIndex).map((username, index): AttendanceEarningMonthRow => {
    const state = monthIndex === 0 ? "earning" : states[1 + ((index + monthIndex) % 4)];
    const earned = (3 + ((index * 5) % 9)) * 3_600;
    const claimed = state === "claimed" ? earned : state === "claimable" ? Math.floor(earned / 3) : 0;
    const [year, monthNumber] = month.split("-").map(Number);
    // Earning months are Sep-Nov 2026: claims open in Oct-Dec, so only the
    // exclusive deadline after a December claim month wraps the year.
    const claimMonth = monthNumber + 1;
    const deadline = claimMonth === 12 ? isoDate(year + 1, 1, 1) : isoDate(year, claimMonth + 1, 1);
    return {
      account_id: `account-${username}`,
      username,
      earning_month: month,
      state,
      claim_starts_at: `${isoDate(year, claimMonth, 1)}T00:00:00+08:00`,
      deadline_at_exclusive: `${deadline}T00:00:00+08:00`,
      claim_deadline_local_date: isoDate(year, claimMonth, lastDayOfMonth(year, claimMonth)),
      earned_seconds: earned,
      claimed_seconds: claimed,
      outstanding_seconds: state === "earning" || state === "claimable" ? earned - claimed : 0,
      expired_seconds: state === "expired" ? earned : 0,
      forfeited_seconds: state === "forfeited" ? earned : 0,
    };
  }),
);
const accountRows: AttendanceAccountRow[] = usernames.map((username) => {
  const rows = monthRows.filter((row) => row.username === username);
  const sum = (key: "earned_seconds" | "claimed_seconds" | "outstanding_seconds" | "expired_seconds") =>
    rows.reduce((total, row) => total + row[key], 0);
  return {
    account_id: `account-${username}`,
    username,
    check_ins: sum("earned_seconds") / 3_600,
    earned_seconds: sum("earned_seconds"),
    claimed_seconds: sum("claimed_seconds"),
    outstanding_seconds: sum("outstanding_seconds"),
    claimable_seconds: rows
      .filter((row) => row.state === "claimable")
      .reduce((total, row) => total + row.outstanding_seconds, 0),
    expired_seconds: sum("expired_seconds"),
  };
});

const flowLabels: Record<AttendanceReportPeriod, string[]> = {
  day: ["2026-11-10", "2026-11-11", "2026-11-12", "2026-11-13", "2026-11-14", "2026-11-15", "2026-11-16"],
  week: ["2026-10-19", "2026-10-26", "2026-11-02", "2026-11-09", "2026-11-16"],
  month: ["2026-09", "2026-10", "2026-11"],
};

/** A page of `rows` as the server pages them; the harness pages by 10 so the pager shows. */
function page<T>(rows: T[], offset = 0) {
  const limit = 10;
  return { rows: rows.slice(offset, offset + limit), total: rows.length, limit, offset, has_more: offset + limit < rows.length };
}

function attendance(input: AttendanceReportInput): AttendanceReport {
  const buckets: AttendanceFlowBucket[] = empty
    ? []
    : flowLabels[input.period].map((label, index) => ({
        period_label: label,
        check_ins: 6 + ((index * 7) % 9),
        earned_seconds: (6 + ((index * 7) % 9)) * 3_600,
        claimed_seconds: ((index * 3) % 5) * 3_600,
        expired_seconds: index === 0 ? 7_200 : 0,
        forfeited_seconds: index === 1 ? 3_600 : 0,
      }));
  const flowTotal = (key: Exclude<keyof AttendanceFlowBucket, "period_label">) =>
    buckets.reduce((sum, bucket) => sum + bucket[key], 0);
  const months = empty ? [] : monthRows;
  const accounts = empty ? [] : accountRows;
  const outstanding = months.reduce((sum, row) => sum + row.outstanding_seconds, 0);
  return {
    period: input.period,
    from_date: input.from_date ?? null,
    to_date: input.to_date ?? null,
    as_of: "2026-11-16T07:00:00+08:00",
    admin_local_date: businessToday,
    admin_local_month: "2026-11",
    flow_totals: {
      check_ins: flowTotal("check_ins"),
      earned_seconds: flowTotal("earned_seconds"),
      claimed_seconds: flowTotal("claimed_seconds"),
      expired_seconds: flowTotal("expired_seconds"),
      forfeited_seconds: flowTotal("forfeited_seconds"),
    },
    flow_buckets: buckets,
    lifetime_totals: {
      earned_seconds: months.reduce((sum, row) => sum + row.earned_seconds, 0),
      claimed_seconds: months.reduce((sum, row) => sum + row.claimed_seconds, 0),
      outstanding_seconds: outstanding,
      expired_seconds: months.reduce((sum, row) => sum + row.expired_seconds, 0),
      forfeited_seconds: months.reduce((sum, row) => sum + row.forfeited_seconds, 0),
    },
    liability: {
      outstanding_seconds: outstanding,
      claimable_seconds: accounts.reduce((sum, row) => sum + row.claimable_seconds, 0),
    },
    month_page: page(months, input.month_offset),
    account_page: page(accounts, input.account_offset),
    economics_advisory: empty
      ? null
      : {
          daily_threshold_cents: 2_000,
          reward_seconds: 3_600,
          rate_amount_cents: 100,
          rate_seconds: 300,
          effective_bonus: { kind: "finite", base_seconds: 6_000, basis_points: 6_000 },
          unsafe_bonus: false,
        },
    clock_regression_advisory: false,
  };
}

// --- Shell ---

const unreadCount: NotificationUnreadCount = { unread_count: empty ? 0 : 3 };
const alerts: NotificationListResponse = { items: [], has_more: false };
const diagnosticsMode: DiagnosticsMode = { enabled: false };
const appUpdate: AppUpdateState = {
  currentVersion: "0.12.6",
  currentSchemaVersion: 1,
  autoCheck: true,
  lastCheckedAt: "2026-11-16 06:00:00",
  rollbacks: [],
  runningSessionCount: 4,
};

// Static answers, plus the two POSTs whose answer depends on the request body.
const fixtures: Record<string, (request: Request) => unknown> = {
  "GET /web-admin/session": () => session,
  "GET /api/theme": () => theme,
  "GET /web-admin/notifications/unread-count": () => unreadCount,
  "GET /web-admin/notifications": () => alerts,
  "GET /web-admin/diagnostics-mode": () => diagnosticsMode,
  "GET /web-admin/update": () => appUpdate,
  "GET /web-admin/reports/overview": () => overview,
  "POST /web-admin/reports/summary": async (request) => {
    const body = (await request.json()) as { from_date: string; to_date: string };
    return summary(body.from_date, body.to_date);
  },
  "POST /web-admin/reports/attendance": async (request) =>
    attendance((await request.json()) as AttendanceReportInput),
  "POST /web-admin/activity": () => ({ ok: true }),
};

window.fetch = async (input, init) => {
  const request = new Request(input, init);
  const key = `${request.method} ${new URL(request.url).pathname}`;
  const fixture = fixtures[key];
  if (!fixture) {
    console.warn(`unstubbed request: ${key}`);
    return new Response(`no fixture for ${key}`, { status: 404 });
  }
  if (key === "GET /web-admin/reports/overview") document.documentElement.dataset.harnessServed = "true";
  return Response.json(await fixture(request));
};

applyThemeRecord(theme);
history.replaceState(null, "", "/admin/reports");
mount(App, { target: document.getElementById("app")! });

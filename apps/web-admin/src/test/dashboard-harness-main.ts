// Screenshot harness entry: mounts the real App shell at /admin/dashboard with
// `window.fetch` answered from fixtures, so no Admin server is needed.
// Query: `scenario=busy|quiet|empty` (default busy), `theme=light|dark`.
// Driven by scripts/dashboard-screenshots.ts, which waits on the
// `data-harness-*` markers set on <html> below.
import { mount } from "svelte";
import App from "../App.svelte";
import { applyThemeRecord } from "$lib/theme";
import type {
  ActiveSession,
  AppUpdateState,
  CoinTransaction,
  DashboardSnapshot,
  DiagnosticsMode,
  NotificationListResponse,
  NotificationUnreadCount,
  Pc,
  PcMonitoringCheck,
  ThemeMode,
  ThemeRecord,
  WebAdminSessionResponse,
} from "$lib/types";
import "../app.css";

const params = new URLSearchParams(window.location.search);
const mode: ThemeMode = params.get("theme") === "dark" ? "dark" : "light";
const scenario = params.get("scenario") ?? "busy";

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

const idle = {
  remaining_seconds: 0,
  guest_remaining_seconds: 0,
  timer_mode: "guest",
  power_state: "active",
  shutdown_grace_seconds: 30,
  active_session: null,
  last_seen_at: "2026-11-16 06:59:55",
  clock_difference_ms: 120,
} satisfies Partial<Pc>;

function member(pcId: string, username: string, remaining: number): ActiveSession {
  return {
    id: `session-${pcId}`,
    account_id: `account-${username}`,
    username,
    pc_id: pcId,
    remaining_seconds: remaining,
    points_balance_centi: 1_250,
    claimable_guest_seconds: 0,
  };
}

const pcs: Pc[] = [
  {
    ...idle,
    id: "pc-01",
    name: "PC 01",
    status: "in_use",
    remaining_seconds: 5_400,
    timer_mode: "account",
    active_session: member("pc-01", "maria.santos", 5_400),
  },
  {
    ...idle,
    id: "pc-02",
    name: "PC 02",
    status: "in_use",
    remaining_seconds: 1_800,
    guest_remaining_seconds: 1_800,
  },
  {
    ...idle,
    id: "pc-03",
    name: "PC 03",
    status: "in_use",
    remaining_seconds: 2_700,
    timer_mode: "account",
    active_session: member("pc-03", "jun_r", 2_700),
  },
  { ...idle, id: "pc-04", name: "PC 04", status: "online" },
  { ...idle, id: "pc-05", name: "PC 05", status: "online", clock_difference_ms: 95_000 },
  {
    ...idle,
    id: "pc-06",
    name: "PC 06",
    status: "offline",
    last_seen_at: "2026-11-15 21:12:03",
    clock_difference_ms: null,
  },
  {
    ...idle,
    id: "pc-07",
    name: "PC 07",
    status: "in_use",
    remaining_seconds: 240,
    timer_mode: "account",
    power_state: "warning",
    active_session: member("pc-07", "kat23", 240),
  },
  { ...idle, id: "pc-08", name: "PC 08", status: "locked" },
];

// 100 cents buys 300 seconds; accounts earn 1 point per 10 pesos.
function coin(
  id: string,
  pc: Pc,
  amountCents: number,
  username: string | null,
  createdAt: string,
): CoinTransaction {
  return {
    id,
    pc_id: pc.id,
    pc_name: pc.name,
    account_id: username ? `account-${username}` : null,
    account_username: username,
    amount_cents: amountCents,
    seconds_added: (amountCents / 100) * 300,
    coin_promo_bonus_seconds: 0,
    points_earned_centi: username ? amountCents / 10 : 0,
    source: "serial",
    target_kind: username ? "account" : "guest",
    created_at: createdAt,
  };
}

const recentTransactions: CoinTransaction[] = [
  coin("tx-08", pcs[0], 2_000, "maria.santos", "2026-11-16 06:58:10"),
  coin("tx-07", pcs[1], 500, null, "2026-11-16 06:52:44"),
  coin("tx-06", pcs[6], 100, "kat23", "2026-11-16 06:47:02"),
  coin("tx-05", pcs[2], 1_000, "jun_r", "2026-11-16 06:31:19"),
  coin("tx-04", pcs[1], 500, null, "2026-11-16 06:20:55"),
  coin("tx-03", pcs[3], 2_000, null, "2026-11-16 05:58:30"),
  coin("tx-02", pcs[0], 1_000, "maria.santos", "2026-11-16 05:40:12"),
  coin("tx-01", pcs[4], 500, null, "2026-11-16 05:15:48"),
];

// Monitoring is disabled while escalation, incidents, and a suppression are
// present: the types allow it and it shows both advisories in one render.
const busy: DashboardSnapshot = {
  shop_name: "Kanto Pisonet",
  pcs,
  recent_transactions: recentTransactions,
  today: {
    coin_sales_cents: 154_500,
    voucher_sales_cents: 30_000,
    cash_revenue_cents: 184_500,
  },
  pc_monitoring: {
    enabled: false,
    escalation_enabled: true,
    active_incidents: {
      "pc-03": ["client_reachability_mismatch", "client_guard_coverage"],
    },
    suppressions: { "pc-04": "2026-11-16 07:30:00" },
  },
};

// The everyday state: monitoring on, escalation off, nothing to flag.
const quiet: DashboardSnapshot = {
  ...busy,
  pcs: pcs.map((pc) => ({ ...pc, clock_difference_ms: pc.clock_difference_ms == null ? null : 120 })),
  pc_monitoring: {
    enabled: true,
    escalation_enabled: false,
    active_incidents: {},
    suppressions: {},
  },
};

const empty: DashboardSnapshot = {
  ...quiet,
  pcs: [],
  recent_transactions: [],
  today: { coin_sales_cents: 0, voucher_sales_cents: 0, cash_revenue_cents: 0 },
};

const scenarios: Record<string, DashboardSnapshot> = { busy, quiet, empty };
const dashboard = scenarios[scenario];
if (!dashboard) throw new Error(`unknown harness scenario: ${scenario}`);

// Check All PCs: mostly healthy, plus a mismatch, an unreachable PC, and one
// with no Host Reachability verdict.
const healthy = {
  host_reachability: { state: "reachable" },
  client_reachability: "reachable",
  guard_coverage: "covered",
} satisfies Partial<PcMonitoringCheck>;
const checkResults: PcMonitoringCheck[] = dashboard.pcs.map((pc): PcMonitoringCheck => {
  const base = { ...healthy, pc_id: pc.id, pc_name: pc.name };
  switch (pc.id) {
    case "pc-03":
      return { ...base, client_reachability: "unreachable", guard_coverage: "unavailable" };
    case "pc-06":
      return {
        ...base,
        host_reachability: { state: "unreachable" },
        client_reachability: "unreachable",
        guard_coverage: "unsupported",
      };
    case "pc-08":
      return {
        ...base,
        host_reachability: { state: "unavailable", reason: "registered_ip_missing" },
        guard_coverage: "not_applicable",
      };
    default:
      return base;
  }
});

const unreadCount: NotificationUnreadCount = { unread_count: 3 };
const alerts: NotificationListResponse = { items: [], has_more: false };
const diagnosticsMode: DiagnosticsMode = { enabled: false };
const appUpdate: AppUpdateState = {
  currentVersion: "0.12.5",
  currentSchemaVersion: 1,
  autoCheck: true,
  lastCheckedAt: "2026-11-16 06:00:00",
  rollbacks: [],
  runningSessionCount: 4,
};

const fixtures: Record<string, unknown> = {
  "GET /web-admin/session": session,
  "GET /api/theme": theme,
  "GET /web-admin/notifications/unread-count": unreadCount,
  "GET /web-admin/notifications": alerts,
  "GET /web-admin/diagnostics-mode": diagnosticsMode,
  "GET /web-admin/dashboard": dashboard,
  "GET /web-admin/update": appUpdate,
  "POST /web-admin/pc-monitoring/check-all": checkResults,
  "POST /web-admin/activity": { ok: true },
};

window.fetch = async (input, init) => {
  const request = new Request(input, init);
  const key = `${request.method} ${new URL(request.url).pathname}`;
  if (!(key in fixtures)) {
    console.warn(`unstubbed request: ${key}`);
    return new Response(`no fixture for ${key}`, { status: 404 });
  }
  if (key === "GET /web-admin/dashboard") document.documentElement.dataset.harnessServed = "true";
  return Response.json(fixtures[key]);
};

document.documentElement.dataset.harnessPcs = String(dashboard.pcs.length);

applyThemeRecord(theme);
history.replaceState(null, "", "/admin/dashboard");
mount(App, { target: document.getElementById("app")! });

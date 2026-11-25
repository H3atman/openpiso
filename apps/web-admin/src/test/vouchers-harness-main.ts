// Screenshot harness entry: mounts the real App shell at /admin/vouchers with
// `window.fetch` answered from fixtures, so no Admin server is needed.
// Query: `tab=vouchers|batches|template` (default vouchers; the page keeps its
// tab in component state, so the harness clicks the tab once data has loaded),
// `editor=html|css|script` (template editor tab, default html),
// `scenario=default|empty`, `theme=light|dark`.
// Driven by scripts/vouchers-screenshots.ts, which waits on the
// `data-harness-served` marker set on <html> once the page's requests have been
// answered and the requested tab is showing.
import { mount } from "svelte";
import App from "../App.svelte";
import { applyThemeRecord } from "$lib/theme";
import type {
  AdminSettings,
  AppUpdateState,
  DiagnosticsMode,
  EconomySettings,
  NotificationListResponse,
  NotificationUnreadCount,
  ThemeMode,
  ThemeRecord,
  Voucher,
  VoucherBatchDetail,
  VoucherBatchSummary,
  VoucherTemplate,
  WebAdminSessionResponse,
} from "$lib/types";
import "../app.css";

const params = new URLSearchParams(window.location.search);
const mode: ThemeMode = params.get("theme") === "dark" ? "dark" : "light";
const scenario = params.get("scenario") ?? "default";
if (scenario !== "default" && scenario !== "empty") throw new Error(`unknown harness scenario: ${scenario}`);
const empty = scenario === "empty";
const tabParam = params.get("tab") ?? "vouchers";
if (tabParam !== "vouchers" && tabParam !== "batches" && tabParam !== "template")
  throw new Error(`unknown harness tab: ${tabParam}`);
const editorParam = params.get("editor") ?? "html";
if (editorParam !== "html" && editorParam !== "css" && editorParam !== "script")
  throw new Error(`unknown harness editor: ${editorParam}`);

// --- Shell ---

const theme: ThemeRecord = {
  accent_color: "#15803d",
  mode_web_admin: mode,
  mode_kiosk: "light",
  mode_client: "light",
  version: 1,
};
const session: WebAdminSessionResponse = { configured: true, authenticated: true, csrf_token: "harness-csrf" };
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
const adminSettings: AdminSettings = {
  shop_name: "Kanto Pisonet",
  shutdown_grace_seconds: 30,
  offline_grace_seconds: 120,
};
// PHP 1 buys 5 minutes; PHP 50 spent earns 1 point.
const economy: EconomySettings = {
  earn_cents_per_point: 5_000,
  rate_amount_cents: 100,
  rate_seconds: 300,
  effective_bonus_basis_points: 600,
  unsafe_bonus_threshold_basis_points: 1_000,
  unsafe_bonus: false,
};

// --- Voucher Template: the shipped defaults (db/settings.rs) ---

const DEFAULT_HTML = String.raw`<article class="voucher-card" data-price="{{price}}">
  <header><strong>{{shop_name}}</strong><span>{{prefix}}</span></header>
  <div class="voucher-code">{{code}}</div>
  <dl>
    <div><dt>Time</dt><dd>{{time}}</dd></div>
    <div><dt>Points</dt><dd>{{points}}</dd></div>
    <div><dt>Price</dt><dd>{{price}}</dd></div>
    <div><dt>Expires</dt><dd>{{expires_at}}</dd></div>
  </dl>
</article>`;

const DEFAULT_CSS = String.raw`@page { size: A4 portrait; margin: 10mm; }
* { box-sizing: border-box; }
body { margin: 0; color: #111827; font-family: Arial, sans-serif; }
.voucher-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 6mm; }
.voucher-card { min-height: 58mm; padding: 6mm; border: 1px dashed #64748b; border-radius: 3mm; break-inside: avoid; page-break-inside: avoid; }
.voucher-card header { display: flex; justify-content: space-between; gap: 4mm; font-size: 10pt; text-transform: uppercase; }
.voucher-code { margin: 8mm 0; font: 700 18pt monospace; text-align: center; letter-spacing: 0.08em; }
.voucher-card dl { display: grid; grid-template-columns: repeat(2, 1fr); gap: 3mm 6mm; margin: 0; }
.voucher-card dl div { display: flex; justify-content: space-between; gap: 2mm; }
.voucher-card[data-tier="low"] .voucher-code { border-top: 4px solid #15803d; }
.voucher-card[data-tier="high"] .voucher-code { border-top: 4px solid #6b1212; }
.voucher-card dt { color: #64748b; }
.voucher-card dd { margin: 0; font-weight: 700; }
@media print { .voucher-card { break-inside: avoid; page-break-inside: avoid; } }`;

const DEFAULT_SCRIPT = String.raw`// Tier each card by price: under 50 is low, 50 and over is high, free or unparseable gets none.
for (const card of document.querySelectorAll("[data-price]")) {
  const price = Number(card.dataset.price.replace(/[^\d.]/g, "")); // NaN unless the whole string is a number
  if (price > 0) card.dataset.tier = price < 50 ? "low" : "high";
}`;

const voucherTemplate: VoucherTemplate = {
  html: DEFAULT_HTML,
  css: DEFAULT_CSS,
  script: DEFAULT_SCRIPT,
  default_html: DEFAULT_HTML,
  default_css: DEFAULT_CSS,
  default_script: DEFAULT_SCRIPT,
};

// --- Batches: PHP 5, PHP 50 and PHP 100 sold batches plus a voided promo batch ---

const batchBase = {
  suffix_length: 8,
  expires_at: "2027-06-30T15:59:00Z",
  voided_at: null,
  voided_audit_event_id: null,
};
const batchList: VoucherBatchSummary[] = empty
  ? []
  : [
      {
        ...batchBase,
        id: "vb-premium",
        name: "Premium \u20B1100",
        prefix: "PREM",
        count: 5,
        kind: "sold",
        seconds_value: 36_000,
        points_value_centi: 200,
        cash_amount_cents: 10_000,
        created_at: "2026-11-15T02:10:00Z",
        redeemed_count: 1,
      },
      {
        ...batchBase,
        id: "vb-regular",
        name: "Regular \u20B150",
        prefix: "REG",
        count: 8,
        kind: "sold",
        seconds_value: 15_000,
        points_value_centi: 100,
        cash_amount_cents: 5_000,
        created_at: "2026-11-12T03:30:00Z",
        redeemed_count: 3,
      },
      {
        ...batchBase,
        id: "vb-weekend",
        name: "Weekend \u20B15",
        prefix: "WKND",
        count: 12,
        kind: "sold",
        seconds_value: 1_500,
        points_value_centi: 10,
        cash_amount_cents: 500,
        created_at: "2026-11-08T01:00:00Z",
        redeemed_count: 7,
      },
      {
        ...batchBase,
        id: "vb-giveaway",
        name: "Giveaway",
        prefix: "FREE",
        count: 6,
        kind: "promo",
        seconds_value: 1_800,
        points_value_centi: 0,
        cash_amount_cents: 0,
        expires_at: null,
        created_at: "2026-10-30T05:45:00Z",
        voided_at: "2026-11-02T08:20:00Z",
        voided_audit_event_id: "ae-0001",
        redeemed_count: 1,
      },
    ];

const redeemers = ["juan", "maria", "pedro", "ana", "kuya_rey", "liza", "mark"];
const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/** Deterministic 8-character suffix for card `index` of a batch. */
function suffix(index: number): string {
  return Array.from({ length: 8 }, (_, i) => alphabet[(index * 7 + i * 5 + i * i) % alphabet.length]).join("");
}
function batchCards(batch: VoucherBatchSummary): Voucher[] {
  return Array.from({ length: batch.count }, (_, index): Voucher => {
    const status: Voucher["status"] =
      index < batch.redeemed_count ? "redeemed" : batch.voided_at ? "voided" : "active";
    const redeemer = redeemers[index % redeemers.length];
    return {
      id: `${batch.id}-${index}`,
      code: `${batch.prefix}-${suffix(index + batch.count)}`,
      kind: batch.kind,
      status,
      seconds_value: batch.seconds_value,
      points_value_centi: batch.points_value_centi,
      sold_amount_cents: batch.cash_amount_cents,
      created_at: batch.created_at,
      sold_at: batch.kind === "sold" ? batch.created_at : null,
      redeemed_by_account_id: status === "redeemed" ? `acct-${redeemer}` : null,
      redeemed_by_username: status === "redeemed" ? redeemer : null,
      redeemed_at: status === "redeemed" ? "2026-11-16T06:40:00Z" : null,
      expires_at: batch.expires_at,
      batch_id: batch.id,
      batch_name: batch.name,
      batch_prefix: batch.prefix,
    };
  });
}
const batchDetails = new Map(batchList.map((batch): [string, VoucherBatchDetail] => [
  batch.id,
  { ...batch, vouchers: batchCards(batch) },
]));

// --- Vouchers: single cards plus the newest cards of each batch, newest first ---

const single = (
  id: string,
  code: string,
  status: Voucher["status"],
  kind: Voucher["kind"],
  seconds: number,
  pointsCenti: number,
  cents: number,
  created: string,
): Voucher => ({
  id,
  code,
  kind,
  status,
  seconds_value: seconds,
  points_value_centi: pointsCenti,
  sold_amount_cents: cents,
  created_at: created,
  sold_at: kind === "sold" ? created : null,
  redeemed_by_account_id: status === "redeemed" ? "acct-juan" : null,
  redeemed_by_username: status === "redeemed" ? "juan" : null,
  redeemed_at: status === "redeemed" ? "2026-11-16T07:05:00Z" : null,
  expires_at: null,
  batch_id: null,
  batch_name: null,
  batch_prefix: null,
});
const voucherList: Voucher[] = empty
  ? []
  : [
      single("v-1", "OP-K7M2QX4D", "active", "sold", 3_600, 20, 1_200, "2026-11-16T05:20:00Z"),
      single("v-2", "OP-H9T5WB3N", "redeemed", "sold", 1_800, 10, 600, "2026-11-16T03:00:00Z"),
      single("v-3", "OP-R4C8ZJ6F", "voided", "promo", 900, 0, 0, "2026-11-15T09:45:00Z"),
      single("v-4", "OP-P2L7YD5G", "active", "promo", 1_200, 0, 0, "2026-11-14T11:10:00Z"),
      ...[...batchDetails.values()].flatMap((detail) => detail.vouchers.slice(-3)),
    ].sort((a, b) => b.created_at.localeCompare(a.created_at));

/** The vouchers endpoint's `status`, `batch_id` and `search` filters. */
function filterVouchers(query: URLSearchParams): Voucher[] {
  const status = query.get("status");
  const batchId = query.get("batch_id");
  const search = (query.get("search") ?? "").toUpperCase();
  return voucherList.filter(
    (v) =>
      (!status || v.status === status) &&
      (!batchId || v.batch_id === batchId) &&
      v.code.includes(search),
  );
}

const fixtures: Record<string, unknown> = {
  "GET /web-admin/session": session,
  "GET /api/theme": theme,
  "GET /web-admin/notifications/unread-count": unreadCount,
  "GET /web-admin/notifications": alerts,
  "GET /web-admin/diagnostics-mode": diagnosticsMode,
  "GET /web-admin/update": appUpdate,
  "POST /web-admin/activity": { ok: true },
  "GET /web-admin/settings": adminSettings,
  "GET /web-admin/economy": economy,
  "GET /web-admin/voucher-template": voucherTemplate,
  "GET /web-admin/voucher-batches": batchList,
};

// Requests the page issues on mount; the page is ready once all are answered.
const waiting = new Set([
  "GET /web-admin/vouchers",
  "GET /web-admin/voucher-batches",
  "GET /web-admin/voucher-template",
  "GET /web-admin/settings",
  "GET /web-admin/economy",
]);

/** Shows the requested tab (and template editor), then marks the page ready. */
function showRequestedTab(): void {
  const click = (selector: string, label?: string): void => {
    const el = [...document.querySelectorAll<HTMLElement>(selector)].find(
      (candidate) => label === undefined || candidate.textContent?.trim() === label,
    );
    if (!el) throw new Error(`harness: nothing matches ${selector} ${label ?? ""}`);
    el.click();
  };
  if (tabParam !== "vouchers") click("main [role=tab]", tabParam === "batches" ? "Batches" : "Template");
  // Svelte renders the clicked tab on the next microtask flush; the editor tabs exist only after it.
  setTimeout(() => {
    if (tabParam === "template" && editorParam !== "html") click(`#voucher-template-tab-${editorParam}`);
    setTimeout(() => (document.documentElement.dataset.harnessServed = "true"), 50);
  }, 50);
}

window.fetch = async (input, init) => {
  const request = new Request(input, init);
  const url = new URL(request.url);
  const key = `${request.method} ${url.pathname}`;
  let body: unknown;
  if (key === "GET /web-admin/vouchers") body = filterVouchers(url.searchParams);
  else if (request.method === "GET" && url.pathname.startsWith("/web-admin/voucher-batches/"))
    body = batchDetails.get(decodeURIComponent(url.pathname.split("/").pop()!));
  else if (key in fixtures) body = fixtures[key];
  if (body === undefined) {
    console.warn(`unstubbed request: ${key}`);
    return new Response(`no fixture for ${key}`, { status: 404 });
  }
  if (waiting.delete(key) && waiting.size === 0) {
    // Let this response render, then switch to the requested tab.
    setTimeout(showRequestedTab, 50);
  }
  return Response.json(body);
};

applyThemeRecord(theme);
history.replaceState(null, "", "/admin/vouchers");
mount(App, { target: document.getElementById("app")! });

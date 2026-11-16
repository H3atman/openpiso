// Screenshot harness entry: mounts the real App shell on one Settings section
// with `window.fetch` answered from fixtures, so no Admin server is needed.
// Query: `section=<settings section id>|index` (index is the bare
// /admin/settings path), `theme=light|dark`, `scenario=default|empty`.
// Driven by scripts/settings-screenshots.ts, which waits on the
// `data-harness-served` marker set on <html> once every GET the section
// issues on mount has been answered.
import { mount } from "svelte";
import App from "../App.svelte";
import { applyThemeRecord } from "$lib/theme";
import { settingsSections, type SettingsSection } from "$lib/routing";
import type {
  AcceptorHealth,
  AdminSettings,
  AppUpdateState,
  AttendanceSettings,
  AttendanceSettingsPreview,
  AutostartResponse,
  CalibrationStatus,
  ClientRecoveryEscalation,
  CoinDeviceSettings,
  CoinPromoSettings,
  DetectedSerialPort,
  DiagnosticsMode,
  EconomySettings,
  NotificationListResponse,
  NotificationUnreadCount,
  PcMonitoringStatus,
  PcSettings,
  RedemptionTier,
  ShopHoursDay,
  ShopHoursSettings,
  ShopRate,
  TelegramStatus,
  ThemeMode,
  ThemeRecord,
  WebAdminSessionResponse,
  WheelSettings,
} from "$lib/types";
import "../app.css";

const params = new URLSearchParams(window.location.search);
const mode: ThemeMode = params.get("theme") === "dark" ? "dark" : "light";
const scenario = params.get("scenario") ?? "default";
if (scenario !== "default" && scenario !== "empty") throw new Error(`unknown harness scenario: ${scenario}`);
const empty = scenario === "empty";
const sectionParam = params.get("section") ?? "points";
const section = settingsSections.find((entry) => entry.id === sectionParam)?.id;
if (!section && sectionParam !== "index") throw new Error(`unknown harness section: ${sectionParam}`);

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

// --- Points: ₱1 buys 5 minutes; ₱50 spent earns 1 point ---

const economy: EconomySettings = {
  earn_cents_per_point: 5_000,
  rate_amount_cents: 100,
  rate_seconds: 300,
  effective_bonus_basis_points: 600,
  unsafe_bonus_threshold_basis_points: 1_000,
  unsafe_bonus: false,
};
const redemptionTiers: RedemptionTier[] = empty
  ? []
  : [
      { id: 1, points: 5, seconds: 3_600 },
      { id: 2, points: 10, seconds: 7_200 },
      { id: 3, points: 20, seconds: 18_000 },
    ];

// --- Coin Promo: bonus minutes for account coins ---

const coinPromo: CoinPromoSettings = empty
  ? {
      enabled: false,
      tiers: [],
      account_worst_case_basis_points: 0,
      unsafe_bonus_threshold_basis_points: 2_500,
      unsafe_bonus: false,
    }
  : {
      enabled: true,
      tiers: [
        { amount_cents: 1_000, bonus_seconds: 300, effective_bonus_basis_points: 1_000 },
        { amount_cents: 2_000, bonus_seconds: 900, effective_bonus_basis_points: 1_500 },
        { amount_cents: 5_000, bonus_seconds: 2_700, effective_bonus_basis_points: 1_800 },
      ],
      account_worst_case_basis_points: 2_400,
      unsafe_bonus_threshold_basis_points: 2_500,
      unsafe_bonus: false,
    };

// --- Attendance: spend ₱20 in a day, earn 30 minutes ---

const attendanceAdvisory = {
  daily_threshold_cents: 2_000,
  reward_seconds: 1_800,
  rate_amount_cents: 100,
  rate_seconds: 300,
  effective_bonus: { kind: "finite", base_seconds: 6_000, basis_points: 3_000 },
  unsafe_bonus: false,
} satisfies AttendanceSettings["economics_advisory"];
const attendance: AttendanceSettings = {
  version: 3,
  enabled: !empty,
  daily_threshold_cents: 2_000,
  reward_seconds: 1_800,
  claim_window_months: 1,
  effective_at: "2026-10-01 00:00:00",
  economics_advisory: attendanceAdvisory,
  liability: empty
    ? { outstanding_seconds: 0, claimable_seconds: 0 }
    : { outstanding_seconds: 12_600, claimable_seconds: 5_400 },
  next_earning_month: "2026-12",
  effective_changes: {
    enabled: "immediately_for_new_contributions",
    daily_threshold_cents: "next_unfrozen_daily_snapshot",
    reward_minutes: "next_unfrozen_daily_snapshot",
    claim_window_months: "next_earning_month",
  },
};
const attendancePreview: AttendanceSettingsPreview = {
  enabled: attendance.enabled,
  daily_threshold_cents: attendance.daily_threshold_cents,
  reward_seconds: attendance.reward_seconds,
  claim_window_months: attendance.claim_window_months,
  economics_advisory: attendanceAdvisory,
};

// --- Wheel: 8 equal slices; a point redeems for 15 minutes ---

const wheelSegments: WheelSettings["segments"] = [
  { label: "5 mins", seconds: 300, points_centi: 0, free_spin: false, probability_basis_points: 1_250 },
  { label: "Try again", seconds: 0, points_centi: 0, free_spin: false, probability_basis_points: 1_250 },
  { label: "10 mins", seconds: 600, points_centi: 0, free_spin: false, probability_basis_points: 1_250 },
  { label: "Free spin", seconds: 0, points_centi: 0, free_spin: true, probability_basis_points: 1_250 },
  { label: "Try again", seconds: 0, points_centi: 0, free_spin: false, probability_basis_points: 1_250 },
  { label: "1 point", seconds: 0, points_centi: 100, free_spin: false, probability_basis_points: 1_250 },
  { label: "15 mins", seconds: 900, points_centi: 0, free_spin: false, probability_basis_points: 1_250 },
  { label: "Try again", seconds: 0, points_centi: 0, free_spin: false, probability_basis_points: 1_250 },
];
const wheel: WheelSettings = {
  enabled: !empty,
  bokya_rate_basis_points: null,
  free_spin_cap: 3,
  spin_duration: 5,
  password_confirmation_required: true,
  version: empty ? 1 : 6,
  segments: empty ? [] : wheelSegments,
  expected_payout_seconds: empty ? 0 : 257,
  expected_payout_points_centi: empty ? 0 : 14,
  point_value_seconds: 900,
  house_edge_basis_points: empty ? 10_000 : 7_143,
  unsafe_house_edge: false,
  shop_rate_point_value_seconds: 15_000,
  shop_rate_house_edge_basis_points: empty ? 10_000 : 9_829,
  unsafe_shop_rate_house_edge: false,
};

// --- Coin Device: CH340 USB-serial acceptor on COM3 ---

const coinDevice: CoinDeviceSettings = {
  com_port: empty ? null : "COM3",
  baud_rate: 9_600,
  set_pin_line: "dtr",
  coin_pin_line: "cts",
  coin_pin_polarity: "normally_open",
  inter_coin_gap_ms: 200,
  inactivity_timeout_seconds: 15,
  hard_cap_seconds: 180,
  pulse_map: [
    { pulse_count: 1, amount_cents: 100 },
    { pulse_count: 5, amount_cents: 500 },
    { pulse_count: 10, amount_cents: 1_000 },
    { pulse_count: 20, amount_cents: 2_000 },
  ],
  calibration_required: empty,
};
const coinPorts: DetectedSerialPort[] = empty
  ? []
  : [
      { name: "COM3", label: "USB-SERIAL CH340 (COM3)", detected: true },
      { name: "COM1", label: "Communications Port (COM1)", detected: true },
    ];
const acceptorHealth: AcceptorHealth = {
  debug_window_start: "2026-11-15T23:00:00Z",
  timing_status: "unavailable_until_filtered",
  implausible_timing_rate: null,
};
const currentCalibration: CalibrationStatus | null = null;

// --- Shop: name, rate, and opening hours (weekday 0 is Monday) ---

const adminSettings: AdminSettings = {
  shop_name: "Kanto Pisonet",
  shutdown_grace_seconds: 30,
  offline_grace_seconds: 120,
};
const shopRate: ShopRate = { amount_cents: 100, seconds: 300 };
const shopHours: ShopHoursSettings = {
  enabled: !empty,
  last_call_lead_minutes: 15,
  days: [0, 1, 2, 3, 4, 5, 6].map((weekday): ShopHoursDay =>
    weekday === 6
      ? { weekday, mode: "open_period", open_minute: 600, close_minute: 1_320 }
      : weekday === 5
        ? { weekday, mode: "open_24_hours" }
        : { weekday, mode: "open_period", open_minute: 480, close_minute: 1_380 },
  ),
  override: null,
  status: {
    open: true,
    next_change_at: empty ? null : "2026-11-16T15:00:00Z",
    reason: "schedule",
    last_call_active: false,
  },
};

// --- PCs: eight stations on 192.168.1.101-108; PC 08 awaits its first boot ---

const pcs: PcSettings[] = empty
  ? []
  : Array.from({ length: 8 }, (_, index) => {
      const n = index + 1;
      const id = `pc-0${n}`;
      return {
        id,
        name: `PC 0${n}`,
        visible: n !== 6,
        registered_ip: `192.168.1.${100 + n}`,
        mac_address: n === 8 ? null : `3C:7C:3F:A1:0${n}:${(16 * n).toString(16).toUpperCase().padStart(2, "0")}`,
      };
    });

// --- PC Monitoring, Telegram, Autostart ---

const monitoring: PcMonitoringStatus = { enabled: !empty, platform_supported: true };
const escalation: ClientRecoveryEscalation = { enabled: false, delay_secs: 300 };
const telegram: TelegramStatus = empty
  ? { platform_supported: true, connected: false }
  : {
      platform_supported: true,
      connected: true,
      destination: {
        bot_username: "KantoPisonetAlertsBot",
        chat_title: "Kanto Pisonet Owner",
        chat_id: "548213907",
        connected_at: "2026-10-02T03:14:00Z",
      },
      last_test: { verified_at: "2026-10-02T03:14:05Z" },
      delivery: {
        last_sent_at: "2026-11-15T13:42:10Z",
        pending_count: 0,
        queued_count: 0,
        retrying_count: 0,
        expired_count: 0,
      },
    };
const autostart: AutostartResponse = {
  enabled: true,
  supported: true,
  editable: true,
  exe_path: "C:\\Program Files\\OpenPiso Admin\\openpiso-admin.exe",
};

const fixtures: Record<string, unknown> = {
  "GET /web-admin/session": session,
  "GET /api/theme": theme,
  "GET /web-admin/notifications/unread-count": unreadCount,
  "GET /web-admin/notifications": alerts,
  "GET /web-admin/diagnostics-mode": diagnosticsMode,
  "GET /web-admin/update": appUpdate,
  "POST /web-admin/activity": { ok: true },
  "GET /web-admin/economy": economy,
  "GET /web-admin/redemption-tiers": redemptionTiers,
  "GET /web-admin/coin-promo": coinPromo,
  "POST /web-admin/coin-promo/preview": coinPromo,
  "GET /web-admin/attendance/settings": attendance,
  "POST /web-admin/attendance/settings/preview": attendancePreview,
  "GET /web-admin/wheel": wheel,
  "GET /web-admin/coin-device": coinDevice,
  "GET /web-admin/coin-device/ports": coinPorts,
  "GET /web-admin/coin-device/acceptor-health": acceptorHealth,
  "GET /web-admin/coin-device/calibration": currentCalibration,
  "GET /web-admin/settings": adminSettings,
  "GET /web-admin/shop-rate": shopRate,
  "GET /web-admin/shop-hours": shopHours,
  "GET /web-admin/pcs": pcs,
  "GET /web-admin/pc-monitoring/status": monitoring,
  "GET /web-admin/pc-monitoring/escalation": escalation,
  "GET /web-admin/telegram/status": telegram,
  "GET /web-admin/autostart": autostart,
};

// Requests each section issues on mount (including Coin Promo's debounced
// preview); the page is ready once all have been answered. Diagnostics Mode
// and the theme are shell requests that Security and Appearance render from.
const expected: Record<SettingsSection, string[]> = {
  points: ["GET /web-admin/economy", "GET /web-admin/redemption-tiers"],
  "coin-promo": ["GET /web-admin/coin-promo", "POST /web-admin/coin-promo/preview"],
  attendance: ["GET /web-admin/attendance/settings"],
  wheel: ["GET /web-admin/wheel"],
  "coin-device": [
    "GET /web-admin/coin-device",
    "GET /web-admin/coin-device/ports",
    "GET /web-admin/coin-device/acceptor-health",
    "GET /web-admin/coin-device/calibration",
  ],
  shop: [
    "GET /web-admin/settings",
    "GET /web-admin/shop-hours",
    "GET /web-admin/shop-rate",
    "GET /web-admin/coin-device",
  ],
  pcs: ["GET /web-admin/pcs"],
  "pc-monitoring": ["GET /web-admin/pc-monitoring/status", "GET /web-admin/pc-monitoring/escalation"],
  telegram: ["GET /web-admin/telegram/status"],
  autostart: ["GET /web-admin/autostart"],
  security: ["GET /web-admin/diagnostics-mode"],
  appearance: ["GET /api/theme"],
};
const waiting = new Set(section ? expected[section] : ["GET /web-admin/session"]);

window.fetch = async (input, init) => {
  const request = new Request(input, init);
  const key = `${request.method} ${new URL(request.url).pathname}`;
  if (!(key in fixtures)) {
    console.warn(`unstubbed request: ${key}`);
    return new Response(`no fixture for ${key}`, { status: 404 });
  }
  if (waiting.delete(key) && waiting.size === 0) {
    // Mark after this response has been consumed and rendered.
    setTimeout(() => (document.documentElement.dataset.harnessServed = "true"), 50);
  }
  return Response.json(fixtures[key]);
};

applyThemeRecord(theme);
history.replaceState(null, "", section ? `/admin/settings/${section}` : "/admin/settings");
mount(App, { target: document.getElementById("app")! });

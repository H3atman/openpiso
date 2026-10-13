export interface ClientConfig {
  server_url: string;
  /**
   * Server-assigned identity the client caches (ADR-0031). `null` on a
   * fresh install: the server adopts a `pc_id` on the first heartbeat
   * and the client persists it from then on. The owner no longer types
   * a PC ID — that field is gone from the settings UI.
   */
  pc_id: string | null;
  /**
   * Customer's persisted choice between the floating Widget and the Mini
   * Bar while time is active (ADR-0029 §4.1). Defaults to `"widget"` for
   * a fresh install; the Fullscreen Lock is not a stored value.
   */
  view_mode: ViewMode;
  /** Whether the Client launches at Windows sign-in for this user. */
  launch_at_startup: boolean;
  /** Whether the owner enabled Windows sign-out protection. */
  disable_windows_sign_out: boolean;
}

export interface SetClientConfigResponse {
  config: ClientConfig;
  persist_warning: string | null;
}

export type SetLaunchAtStartupResponse = SetClientConfigResponse;
export type SetWindowsSignOutResponse = SetClientConfigResponse;

/** Stored View Mode (ADR-0029). The lock is derived state, never a value. */
export type ViewMode = "widget" | "mini";

/**
 * Three logical Client views (ADR-0029). The router in `App.svelte` mounts
 * exactly one of these at a time; the chosen view drives a window-mode
 * command (`enter_fullscreen_mode` / `enter_widget_mode` / `enter_mini_mode`)
 * which reconfigures the single `main` window. `"lock"` is the default for
 * logged-out, expired, no-usable-time, and shutdown states.
 */
export type ClientView = "lock" | "widget" | "mini";

/**
 * Connection status values emitted by the Client (ADR-0031). Drives the
 * Lock's three-state copy: "Registering this PC…", "ask the owner to
 * register this PC", and the existing "Can't reach the server".
 */
export type ConnectionStatus =
  | "disconnected"
  | "connecting"
  | "connected"
  | "unregistered";

export interface ShopStatus {
  open: boolean;
  next_change_at: string | null;
  reason: "schedule" | "override";
  last_call_active: boolean;
}

export interface TimerState {
  pc_id: string | null;
  pc_name: string;
  pc_status: string;
  remaining_seconds: number;
  guest_remaining_seconds: number;
  timer_mode: "guest" | "account";
  active_username: string | null;
  connected: boolean;
  connection_status: ConnectionStatus;
  expired: boolean;
  overlay_locked: boolean;
  /** A Tamper Kill report is awaiting the authoritative heartbeat. */
  tamper_kill_pending?: boolean;
  power_state: "active" | "warning" | "shutdown";
  shutdown_in_seconds: number | null;
  offline_shutdown_in_seconds: number | null;
  shutdown_grace_seconds: number;
  /** Server-resolved Shop Status; the Client never evaluates the schedule. */
  shop_status?: ShopStatus | null;
  /** Admin heartbeat feature flag; absent means the Admin predates Daily Reward. */
  attendance_supported?: boolean;
  error: string | null;
}

export interface AttendanceClock {
  as_of: string;
  admin_local_date: string;
  admin_local_month: string;
}

export interface AttendanceTotals {
  earned_seconds: number;
  claimed_seconds: number;
  outstanding_seconds: number;
  claimable_seconds: number;
  expired_seconds: number;
  forfeited_seconds: number;
}

export type AttendanceMonthState =
  | "earning"
  | "claimable"
  | "claimed"
  | "expired"
  | "forfeited";

export interface AttendanceMonthSummary {
  earning_month: string;
  state: AttendanceMonthState | null;
  check_in_count: number;
  earned_seconds: number;
  claimed_seconds: number;
  outstanding_seconds: number;
  claimable_seconds: number;
  expired_seconds: number;
  forfeited_seconds: number;
  claim_starts_at: string | null;
  deadline_at_exclusive: string | null;
  claim_starts_on: string | null;
  deadline_date_inclusive: string | null;
  claim_count: number;
  latest_claim_at: string | null;
  /** Admin-local claim date; optional when talking to an older Admin. */
  latest_claim_on?: string | null;
}

export type AttendanceTodayStatus = "checked_in" | "in_progress" | "ineligible";

export interface AttendanceSummary extends AttendanceClock {
  enabled: boolean;
  today: {
    earning_date: string;
    status: AttendanceTodayStatus;
    capped_cents: number;
    daily_threshold_cents: number;
    reward_seconds: number;
  };
  totals: AttendanceTotals;
  calendar_months: AttendanceMonthSummary[];
  older_claimable_months: AttendanceMonthSummary[];
}

export type AttendanceDayStatus =
  | "checked_in"
  | "missed"
  | "ineligible"
  | "future"
  | "today_progress";

export interface AttendanceDay {
  earning_date: string;
  status: AttendanceDayStatus;
  capped_cents: number | null;
  daily_threshold_cents: number | null;
  reward_seconds: number | null;
}

export interface AttendanceMonth extends AttendanceMonthSummary, AttendanceClock {
  days: AttendanceDay[];
}

export type AttendanceClaimDenialReason =
  | "month_not_found"
  | "not_yet_claimable"
  | "nothing_outstanding"
  | "expired"
  | "request_id_conflict";

export interface AttendanceClaimError {
  error: string;
  message: string;
  reason?: AttendanceClaimDenialReason;
  status_code?: number;
}

export interface AttendanceClaimResponse extends AttendanceClock {
  claim: {
    audit_event_id: string;
    request_id: string;
    earning_month: string;
    claimed_seconds: number;
    created_at: string;
  };
  remaining_seconds: number;
  totals: AttendanceTotals;
  month: AttendanceMonthSummary;
}

export interface Account {
  id: string;
  username: string;
  remaining_seconds: number;
  points_balance_centi: number;
  status: "active" | "deleted";
}

export interface AccountAuth {
  token: string;
  account: Account;
  session: ActiveSession;
}

export interface ActiveSession {
  id: string;
  account_id: string;
  username: string;
  pc_id: string;
  remaining_seconds: number;
  points_balance_centi: number;
  claimable_guest_seconds: number;
}

export interface RedemptionTier {
  id: number;
  points: number;
  seconds: number;
}

export interface RedemptionTierMenu {
  version: number;
  tiers: RedemptionTier[];
}

/** Segments carry no probability weight (ADR-0047): every segment is `1/n`
 * likely and `1/n` of the circle, so the wheel states its own odds. */
export interface CoinFeedback {
  pc_id: string;
  seconds_added: number;
  coin_promo_bonus_seconds: number;
}

export interface CoinPromoMenuTier {
  amount_cents: number;
  bonus_seconds: number;
}

export interface CoinPromoMenu {
  enabled: boolean;
  tiers: CoinPromoMenuTier[];
}

export interface WheelSegmentRow {
  label: string;
  seconds: number;
  points_centi: number;
  free_spin: boolean;
}

export interface WheelSettings {
  enabled: boolean;
  free_spin_cap: number;
  spin_duration: number;
  version: number;
  segments: WheelSegmentRow[];
  expected_payout_seconds: number;
  expected_payout_points_centi: number;
  point_value_seconds: number;
  house_edge_basis_points: number;
  unsafe_house_edge: boolean;
}

export interface WheelSpinStep {
  segment_index: number;
  label: string;
  seconds: number;
  points_centi: number;
  free_spin: boolean;
}

export interface WheelSpinResult {
  seconds_won: number;
  points_won_centi: number;
  free_spins_triggered: number;
  segments: WheelSegmentRow[];
  steps: WheelSpinStep[];
}

/**
 * Response from `POST /api/accounts/transfer-points` (ADR-0030). The `code`
 * is a single-use, time-boxed 6-digit number the customer reads to the
 * owner; the owner enters it in web-admin to confirm the points transfer.
 */
export interface PointsTransferCode {
  id: string;
  code: string;
}

/** Structured Account login failure emitted by the Rust command. */
export interface AccountLoginError {
  code?: string;
  message?: string;
  retry_after_seconds?: number;
}

/**
 * Structured Password Change failure emitted by the `change_account_password`
 * command (ADR-0037). Mapped to customer display copy in `api.ts`.
 */
export interface PasswordChangeError {
  code?: string;
  message?: string;
}

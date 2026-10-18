export type PcStatus = "offline" | "online" | "in_use" | "locked";

export interface Pc {
  id: string;
  name: string;
  status: PcStatus;
  remaining_seconds: number;
  guest_remaining_seconds: number;
  timer_mode: "guest" | "account";
  power_state: "active" | "warning" | "shutdown";
  shutdown_grace_seconds: number;
  active_session: ActiveSession | null;
  last_seen_at: string | null;
  /** Admin receipt time minus the latest Client clock reading. */
  clock_difference_ms?: number | null;
}

/** Outcome of an Owner Sign-Out. `signed_out: false` means the session had
 *  already ended (expired, auto-kicked, or taken over) before the click
 *  landed — an ordinary outcome, not a failure. */
export interface SignOutResult {
  signed_out: boolean;
  username: string | null;
}

// --- PC Monitoring Check now (ADR-0076) ---

export type HostReachabilityState = "reachable" | "unreachable" | "unavailable";

/** Why no truthful Host Reachability verdict exists. Typed now so later
 *  Guard coverage work never changes the response shape. */
export type HostUnavailabilityReason =
  | "platform_unsupported"
  | "pc_hidden"
  | "registered_ip_missing"
  | "registered_ip_invalid";

export interface HostReachabilityFact {
  state: HostReachabilityState;
  reason?: HostUnavailabilityReason;
}

export type ClientReachability = "reachable" | "unreachable";

/** The last Client Guard Coverage diagnostic a Heartbeat reported (ADR-0076
 *  / #344): `covered`/`unavailable` from a release Windows Client's real held
 *  Guard connection, `not_applicable` for debug and non-Windows Clients, and
 *  `unsupported` when no Heartbeat has reported the field yet (older Client —
 *  unknown, never a false Alert). */
export type GuardCoverageFact =
  | "covered"
  | "unavailable"
  | "not_applicable"
  | "unsupported";

export interface PcMonitoringCheck {
  pc_id: string;
  pc_name: string;
  host_reachability: HostReachabilityFact;
  client_reachability: ClientReachability;
  guard_coverage: GuardCoverageFact;
}

// --- PC Monitoring continuous detection (ADR-0076) ---

/** Typed incident kind; Admin opens mismatch and Host Probe Coverage kinds. */
export type IncidentKind =
  | "client_reachability_mismatch"
  | "host_probe_coverage"
  | "client_guard_coverage";

/** Shop-wide monitoring state behind the standing advisory. */
export interface PcMonitoringStatus {
  enabled: boolean;
  /** False on non-Windows Admin, which can never run the monitor. */
  platform_supported: boolean;
}

/**
 * Shop-wide Client Recovery Escalation policy (ADR-0077). The one PC
 * Monitoring setting that authorises changing a PC's power state.
 */
export interface ClientRecoveryEscalation {
  enabled: boolean;
  /** Seconds a Guard must be visibly failing before its PC is powered off. */
  delay_secs: number;
}

/** One active per-PC Monitoring Suppression as returned by a mutation. */
export interface MonitoringSuppression {
  pc_id: string;
  pc_name: string;
  /** Absolute expiry instant (RFC 3339 UTC), computed by Admin. */
  expires_at: string;
}

// --- Telegram connect-and-protect (ADR-0076) ---

/** Connected Telegram destination identity. Never carries the bot token. */
export interface TelegramDestination {
  bot_username: string;
  chat_title: string;
  chat_id: string;
  connected_at: string;
}

/** When the stored configuration last passed a real test send. */
export interface TelegramLastTest {
  verified_at: string;
}

/** Incident-delivery health for the connected integration (#346, #347). */
export interface TelegramDeliveryHealth {
  /** When a message was last confirmed sent, if ever. */
  last_sent_at?: string;
  /** The most recent sanitized delivery error, if any. */
  last_error?: string;
  last_error_at?: string;
  /** Delivery items still queued: queued plus retrying. */
  pending_count: number;
  /** Pending items the worker has never attempted. */
  queued_count: number;
  /** Pending items rewaiting after at least one failed attempt. */
  retrying_count: number;
  /** Items abandoned unsent after their 24-hour retry window. */
  expired_count: number;
}

/** Full Telegram integration state for Settings. */
export interface TelegramStatus {
  platform_supported: boolean;
  connected: boolean;
  /** Bot username of a verified but unconfirmed setup candidate. */
  pending_bot_username?: string;
  destination?: TelegramDestination;
  last_test?: TelegramLastTest;
  delivery?: TelegramDeliveryHealth;
}

/** A token verified against the Bot API, awaiting chat confirmation. */
export interface TelegramSetupCandidate {
  bot_username: string;
}

/** One private chat that recently messaged the bot during setup. */
export interface DetectedTelegramChat {
  chat_id: string;
  title: string;
}

/** Dashboard slice: the toggle plus open incidents per PC id. Badges reflect
 *  these durable episodes independently of Notification acknowledgement.
 *  `suppressions` maps pc_id to the absolute RFC 3339 UTC expiry. */
export interface DashboardPcMonitoring {
  enabled: boolean;
  active_incidents: Record<string, IncidentKind[]>;
  suppressions: Record<string, string>;
  /** Whether OpenPiso is currently authorised to power PCs off (ADR-0077). */
  escalation_enabled: boolean;
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

export interface CoinTransaction {
  id: string;
  pc_id: string;
  pc_name: string;
  account_id: string | null;
  account_username: string | null;
  amount_cents: number;
  seconds_added: number;
  coin_promo_bonus_seconds: number;
  points_earned_centi: number;
  source: string;
  target_kind: "guest" | "account";
  created_at: string;
}

export interface EconomySettings {
  earn_cents_per_point: number;
  rate_amount_cents: number;
  rate_seconds: number;
  effective_bonus_basis_points: number;
  unsafe_bonus_threshold_basis_points: number;
  unsafe_bonus: boolean;
}

export interface EconomySettingsInput {
  earn_cents_per_point: number;
}

export interface RedemptionTier {
  id: number;
  points: number;
  seconds: number;
}

export interface RedemptionTierInput {
  points: number;
  minutes: number;
}

export interface RedemptionTiersInput {
  tiers: RedemptionTierInput[];
}

export interface CoinPromoTier {
  amount_cents: number;
  bonus_seconds: number;
  effective_bonus_basis_points: number;
}

export interface CoinPromoSettings {
  enabled: boolean;
  tiers: CoinPromoTier[];
  account_worst_case_basis_points: number;
  unsafe_bonus_threshold_basis_points: number;
  unsafe_bonus: boolean;
}

export interface CoinPromoTierInput {
  amount_cents: number;
  bonus_minutes: number;
}

export interface CoinPromoSettingsInput {
  enabled: boolean;
  tiers: CoinPromoTierInput[];
}

export interface Voucher {
  id: string;
  code: string;
  kind: "sold" | "promo";
  status: "active" | "redeemed" | "voided";
  seconds_value: number;
  points_value_centi: number;
  sold_amount_cents: number;
  created_at: string;
  sold_at: string | null;
  redeemed_by_account_id: string | null;
  redeemed_by_username: string | null;
  redeemed_at: string | null;
  expires_at: string | null;
  batch_id: string | null;
  batch_name: string | null;
  batch_prefix: string | null;
}

export interface VoucherBatch {
  id: string;
  name: string;
  prefix: string;
  suffix_length: number;
  count: number;
  kind: "sold" | "promo";
  seconds_value: number;
  points_value_centi: number;
  cash_amount_cents: number;
  expires_at: string | null;
  created_at: string;
  voided_at: string | null;
  voided_audit_event_id: string | null;
}

export interface VoucherBatchSummary extends VoucherBatch {
  redeemed_count: number;
}

export interface VoucherBatchDetail extends VoucherBatchSummary {
  vouchers: Voucher[];
}

export interface VoucherTemplate {
  html: string;
  css: string;
  default_html: string;
  default_css: string;
}

export interface VoucherTemplateInput {
  html: string;
  css: string;
}

export interface CreateVoucherBatchInput {
  prefix: string;
  suffix_length: number;
  count: number;
  seconds_value: number;
  points_value_centi: number;
  cash_amount_cents: number;
  expires_at: string | null;
}

export interface CreateVoucherInput {
  prefix?: string;
  suffix_length?: number;
  seconds_value: number;
  points_value_centi: number;
  sold_amount_cents: number | null;
  expires_at: string | null;
}

/** Segments carry no stored weight and always occupy `1/n` of the circle. */
export interface WheelSegmentRow {
  label: string;
  seconds: number;
  points_centi: number;
  free_spin: boolean;
}

export interface OwnerWheelSegmentRow extends WheelSegmentRow {
  probability_basis_points: number;
}

export interface WheelSettings {
  enabled: boolean;
  bokya_rate_basis_points: number | null;
  free_spin_cap: number;
  spin_duration: number;
  password_confirmation_required: boolean;
  version: number;
  segments: OwnerWheelSegmentRow[];
  expected_payout_seconds: number;
  expected_payout_points_centi: number;
  point_value_seconds: number;
  house_edge_basis_points: number;
  unsafe_house_edge: boolean;
  shop_rate_point_value_seconds: number;
  shop_rate_house_edge_basis_points: number;
  unsafe_shop_rate_house_edge: boolean;
}

export interface WheelSettingsInput {
  enabled: boolean;
  bokya_rate_basis_points: number | null;
  free_spin_cap: number;
  spin_duration: number;
  password_confirmation_required: boolean;
  segments: WheelSegmentRow[];
}

export interface TodayReport {
  sales_cents: number;
  transaction_count: number;
  seconds_sold: number;
}

export interface DashboardSnapshot {
  shop_name: string;
  pcs: Pc[];
  recent_transactions: CoinTransaction[];
  today: TodayReport;
  pc_monitoring: DashboardPcMonitoring;
}

export interface Account {
  id: string;
  username: string;
  remaining_seconds: number;
  points_balance_centi: number;
  status: "active" | "deleted";
  created_at: string;
  deleted_at: string | null;
  last_login_at: string | null;
  imported_from: string | null;
  imported_at: string | null;
  legacy_not_logged_in: boolean;
}

export interface AccountDeletionResult {
  deleted_count: number;
}

export type AccountImportOutcome = "created" | "skipped";

export type AccountImportSkipReason =
  | "already_imported"
  | "existing_account_conflict"
  | "disabled_at_source"
  | "invalid_username"
  | "invalid_password_hash"
  | "duplicate_username";

export interface AccountImportRow {
  outcome: AccountImportOutcome;
  original_username: string;
  final_username?: string;
  remaining_seconds: number;
  points_balance_centi: number;
  skip_reason?: AccountImportSkipReason;
}

export interface AccountImportTotals {
  created: number;
  skipped: number;
  total_seconds: number;
  total_points_centi: number;
}

export interface AccountImportReport {
  rows: AccountImportRow[];
  totals: AccountImportTotals;
}

/** Setup/login response. The owner token itself only ever travels in the
 *  HttpOnly cookie; the CSRF token is kept in `api.ts` memory. */
export interface OwnerAuth {
  csrf_token: string;
}

export type ReportPeriod = "day" | "week" | "month";

export interface ReportBucket {
  period_label: string;
  cash_revenue_cents: number;
  free_time_dispensed_seconds: number;
  points_minted_by_wheel_centi: number;
}

export interface ReportSummary {
  period: ReportPeriod;
  from_date: string;
  to_date: string;
  cash_revenue_cents: number;
  free_time_dispensed_seconds: number;
  points_minted_by_wheel_centi: number;
  points_liability_centi: number;
  buckets: ReportBucket[];
}

/** One day of the daily sales trend, derived from `ReportSummary.buckets`. */
export interface DailySalesPoint {
  date: string;
  cents: number;
}

export interface PcReport {
  id: string;
  name: string;
  cash_revenue_cents: number;
  seconds_sold: number;
  free_time_dispensed_seconds: number;
  from_date: string;
  to_date: string;
}

export interface SalesReportPeriod {
  from_date: string | null;
  to_date: string | null;
  cash_revenue_cents: number;
}

export interface SalesReportPc {
  id: string;
  name: string;
  daily_sales_cents: number;
  weekly_sales_cents: number;
  monthly_sales_cents: number;
  total_sales_cents: number;
}

export interface SalesReportOverview {
  daily: SalesReportPeriod;
  weekly: SalesReportPeriod;
  monthly: SalesReportPeriod;
  total: SalesReportPeriod;
  pcs: SalesReportPc[];
}

export interface AttendanceEffectiveBonus {
  kind: "finite";
  base_seconds: number;
  basis_points: number;
}

export interface AttendanceEconomicsAdvisory {
  daily_threshold_cents: number;
  reward_seconds: number;
  rate_amount_cents: number;
  rate_seconds: number;
  effective_bonus: AttendanceEffectiveBonus | { kind: "infinite" };
  unsafe_bonus: boolean;
}

export interface AttendanceLiability {
  outstanding_seconds: number;
  claimable_seconds: number;
}

export type AttendanceSettingsEffectiveChange =
  | "immediately_for_new_contributions"
  | "next_unfrozen_daily_snapshot"
  | "next_earning_month";

export interface AttendanceSettingsEffectiveChanges {
  enabled: AttendanceSettingsEffectiveChange;
  daily_threshold_cents: AttendanceSettingsEffectiveChange;
  reward_minutes: AttendanceSettingsEffectiveChange;
  claim_window_months: AttendanceSettingsEffectiveChange;
}

export interface AttendanceSettings {
  version: number;
  enabled: boolean;
  daily_threshold_cents: number;
  reward_seconds: number;
  claim_window_months: number;
  effective_at: string;
  economics_advisory: AttendanceEconomicsAdvisory | null;
  liability: AttendanceLiability;
  next_earning_month: string;
  effective_changes: AttendanceSettingsEffectiveChanges;
  outcome?: "updated" | "unchanged";
}

export interface AttendanceSettingsDraft {
  enabled: boolean;
  daily_threshold_cents: number;
  reward_minutes: number;
  claim_window_months: number;
}

export interface AttendanceSettingsUpdate extends AttendanceSettingsDraft {
  expected_version: number;
}

export interface AttendanceSettingsPreview {
  enabled: boolean;
  daily_threshold_cents: number;
  reward_seconds: number;
  claim_window_months: number;
  economics_advisory: AttendanceEconomicsAdvisory | null;
}

export type AttendanceReportPeriod = "day" | "week" | "month";

export interface AttendanceReportInput {
  period: AttendanceReportPeriod;
  from_date?: string;
  to_date?: string;
  month_limit?: number;
  month_offset?: number;
  account_limit?: number;
  account_offset?: number;
}

export interface AttendanceFlowTotals {
  check_ins: number;
  earned_seconds: number;
  claimed_seconds: number;
  expired_seconds: number;
  forfeited_seconds: number;
}

export interface AttendanceFlowBucket extends AttendanceFlowTotals {
  period_label: string;
}

export interface AttendanceLifetimeTotals {
  earned_seconds: number;
  claimed_seconds: number;
  outstanding_seconds: number;
  expired_seconds: number;
  forfeited_seconds: number;
}

export interface AttendanceEarningMonthRow {
  account_id: string;
  username: string;
  earning_month: string;
  state: "earning" | "claimable" | "claimed" | "expired" | "forfeited";
  claim_starts_at: string;
  deadline_at_exclusive: string;
  claim_deadline_local_date: string;
  earned_seconds: number;
  claimed_seconds: number;
  outstanding_seconds: number;
  expired_seconds: number;
  forfeited_seconds: number;
}

export interface AttendanceAccountRow {
  account_id: string;
  username: string;
  check_ins: number;
  earned_seconds: number;
  claimed_seconds: number;
  outstanding_seconds: number;
  claimable_seconds: number;
  expired_seconds: number;
}

export interface AttendancePage<T> {
  rows: T[];
  total: number;
  limit: number;
  offset: number;
  has_more: boolean;
}

export interface AttendanceReport {
  period: AttendanceReportPeriod;
  from_date: string | null;
  to_date: string | null;
  as_of: string;
  admin_local_date: string;
  admin_local_month: string;
  flow_totals: AttendanceFlowTotals;
  flow_buckets: AttendanceFlowBucket[];
  lifetime_totals: AttendanceLifetimeTotals;
  liability: AttendanceLiability;
  month_page: AttendancePage<AttendanceEarningMonthRow>;
  account_page: AttendancePage<AttendanceAccountRow>;
  economics_advisory: AttendanceEconomicsAdvisory | null;
  clock_regression_advisory: boolean;
}

export type AuditCategory =
  | "auth"
  | "money"
  | "time"
  | "points"
  | "admin"
  | "system";

export interface AuditFilter {
  limit?: number;
  offset?: number;
  from_date?: string;
  to_date?: string;
  pc_id?: string;
  account_id?: string;
  actor?: string[];
  outcome?: string;
  category?: AuditCategory | string;
  event_type?: string[];
  search?: string;
}

export interface AuditRow {
  id: string;
  event_type: string;
  outcome: string;
  actor: string;
  actor_account_id: string | null;
  actor_account_username: string | null;
  account_id: string | null;
  account_username: string | null;
  pc_id: string | null;
  pc_name: string | null;
  amount_cents: number | null;
  seconds_delta: number | null;
  attendance_seconds_delta: number | null;
  points_delta_centi: number | null;
  cash_flag: boolean;
  category: string;
  summary: string;
  metadata_json: string;
  created_at: string;
}

/** One page of audit rows plus the total matching the same filters. */
export interface AuditPage {
  rows: AuditRow[];
  total: number;
}

export type NotificationSeverity = "alert" | "notice";

export interface NotificationItem {
  id: string;
  severity: NotificationSeverity;
  event_type: string;
  outcome: string;
  actor: string;
  actor_account_id: string | null;
  actor_account_username: string | null;
  account_id: string | null;
  account_username: string | null;
  pc_id: string | null;
  pc_name: string | null;
  /** Typed PC Monitoring incident data for generic incident events. */
  incident_kind?: IncidentKind | null;
  amount_cents: number | null;
  seconds_delta: number | null;
  points_delta_centi: number | null;
  summary: string;
  created_at: string;
  read: boolean;
  acknowledged_at: string | null;
}

export interface NotificationListResponse {
  items: NotificationItem[];
  has_more: boolean;
}

export interface NotificationUnreadCount {
  unread_count: number;
}

export interface NotificationSeen {
  last_seen_at: string;
}

export interface NotificationAcknowledgement {
  acknowledged_at: string;
}

export interface BulkNotificationAcknowledgement {
  acknowledged_count: number;
}

/**
 * A Coin Batch Rejection the owner may repair with a Rejection Credit
 * (ADR-0069). Both subjects are frozen at the rejection, so `account_id` and
 * `pc_id` are what was recorded then — never who occupies the PC now.
 */
export interface CoinBatchRejection {
  rejection_audit_id: string;
  created_at: string;
  pc_id: string | null;
  pc_name: string | null;
  account_id: string | null;
  account_username: string | null;
  target_kind: "account" | "guest";
  /** The recorded Account has since been deleted; the credit will be refused. */
  account_deleted: boolean;
  /** Null on a suppression summary, which reports batches rather than pulses. */
  pulse_count: number | null;
  cap_coins: number;
  credited_coins: number;
  remaining_coins: number;
}

export interface CoinBatchRejectionList {
  items: CoinBatchRejection[];
}

export interface RejectionDenominations {
  denominations_cents: number[];
}

export interface RejectionCreditResult {
  dry_run: boolean;
  rejection_audit_id: string;
  /** Null on a dry run: nothing was written. */
  audit_event_id: string | null;
  target_kind: "account" | "guest";
  account_id: string | null;
  account_username: string | null;
  pc_id: string;
  pc_name: string;
  denomination_cents: number;
  coins: number;
  amount_cents: number;
  /** Total credited, including any Coin Promo bonus. */
  seconds_added: number;
  coin_promo_bonus_seconds: number;
  points_earned_centi: number;
  created_at: string | null;
}

export interface ManualAdjustment {
  id: string;
  pc_id: string;
  pc_name: string;
  seconds_delta: number;
  requested_seconds_delta: number;
  reason: string | null;
  audit_event_id: string;
  created_at: string;
}

export interface ManualAdjustmentInput {
  pc_id: string;
  seconds_delta: number;
  reason?: string | null;
}

export interface DetectedSerialPort {
  name: string;
  label: string;
  detected: boolean;
}

export interface CoinDeviceSettings {
  com_port: string | null;
  baud_rate: number;
  set_pin_line: string;
  coin_pin_line: string;
  coin_pin_polarity: string;
  inter_coin_gap_ms: number;
  inactivity_timeout_seconds: number;
  hard_cap_seconds: number;
  pulse_map: PulseMapEntry[];
  calibration_required: boolean;
}

export interface PulseMapEntry {
  pulse_count: number;
  amount_cents: number;
}

export interface CoinDeviceSettingsInput {
  com_port: string | null;
  baud_rate: number;
  set_pin_line: string;
  coin_pin_line: string;
  coin_pin_polarity: string;
  inter_coin_gap_ms: number;
  inactivity_timeout_seconds: number;
  hard_cap_seconds: number;
  pulse_map: PulseMapEntry[];
}

export interface CoinDeviceTestResult {
  ok: boolean;
  simulator: boolean;
  message: string;
}

export type CalibrationPhase =
  | "capturing"
  | "completed"
  | "cancelled"
  | "expired"
  | "failed";

export interface CalibrationStatus {
  capture_id: string;
  target_amount_cents: number;
  phase: CalibrationPhase;
  sample_counts: number[];
  sample_armed: boolean;
  current_pulse_count: number;
  required_samples: number;
  started_at_unix_ms: number;
  idle_deadline_unix_ms: number;
  absolute_deadline_unix_ms: number;
  failure_reason: string | null;
}

/** Owner-facing S1 Acceptor Health snapshot. */
export interface AcceptorHealth {
  /** RFC3339 UTC start of the current Coin Debug Window. */
  debug_window_start: string;
  timing_status: "unavailable_until_filtered";
  /** S1 does not run the Implausible Timing detector. */
  implausible_timing_rate: number | null;
}

/** Markdown produced by the owner-authenticated Coin Debug Bundle endpoint. */
export type CoinDebugBundle = string;

/** Current start of the Coin Debug Window (the span the bundle covers). */
export interface CoinDebugWindow {
  /** RFC3339 UTC timestamp where the current window starts. */
  since: string;
}

export interface WebAdminSession {
  configured: boolean;
  authenticated: boolean;
}

/** `GET /web-admin/session` as sent; `csrf_token` is present only when
 *  authenticated and is consumed by `api.ts`, never exposed to screens. */
export interface WebAdminSessionResponse extends WebAdminSession {
  csrf_token?: string;
}

// ---------------------------------------------------------------------------
// Shop settings + Client owner PIN (ADR-0027)
// ---------------------------------------------------------------------------

/// Public read shape of the owner-editable shop settings. The web-admin
/// `GET /web-admin/settings` endpoint returns this; the canonical `settings`
/// SQLite table is the source of truth. Never carries the owner PIN hash.
export interface AdminSettings {
  shop_name: string;
  shutdown_grace_seconds: number;
  offline_grace_seconds: number;
}

/// Body of `PUT /web-admin/settings`. The owner can change the shop name
/// here; the value is validated server-side (non-empty, ≤80 characters) and
/// persisted with a `shop_name_updated` audit event.
export interface AdminSettingsInput {
  shop_name: string;
  shutdown_grace_seconds: number;
  offline_grace_seconds: number;
}

// ---------------------------------------------------------------------------
// Diagnostics Mode (ADR-0081)
// ---------------------------------------------------------------------------

/**
 * Shop-wide Diagnostics Mode flag behind the Diagnostics page. Visibility
 * only — never a security boundary and it grants no authority.
 */
export interface DiagnosticsMode {
  enabled: boolean;
}

// ---------------------------------------------------------------------------
// Host diagnostics (ADR-0082)
// ---------------------------------------------------------------------------

/// Why one Run Record ended: an owner or OS closing the window, an
/// application-initiated exit, or a Rust panic. Absent while the run is
/// unfinished — see `suspected_hard_exit` instead.
export type HostExitReason = "window_closed" | "application_exit" | "panic";

/// One Admin Run Record from `GET /web-admin/host-diagnostics`, newest first.
export interface HostRunRecord {
  version: string;
  pid: number;
  started_at: string;
  ended_at: string | null;
  exit_reason: HostExitReason | null;
  /// A start with no end on any run but the live one: the process died
  /// without reaching its own shutdown path. An absence of evidence stated
  /// plainly, never a claim about the cause.
  suspected_hard_exit: boolean;
}

/// The panic file's evidence: `logs/panic.log`'s presence, size, last-written
/// time, and its newest 8 KB (lossy UTF-8; the full file stays on disk).
export interface PanicFileEvidence {
  exists: boolean;
  size_bytes: number;
  last_modified_at: string | null;
  newest_8kb: string | null;
}

/// Admin's own health for the Server tab (`GET /web-admin/host-diagnostics`).
/// Empty runs and an absent panic file are stated absences, never errors.
export interface HostDiagnostics {
  runs: HostRunRecord[];
  panic_file: PanicFileEvidence;
}

export interface ShopRate {
  amount_cents: number;
  seconds: number;
}

export interface ShopRateInput {
  amount_cents: number;
  seconds: number;
}

export type ShopHoursDayMode = "open_period" | "closed_all_day" | "open_24_hours";

export interface ShopHoursDay {
  weekday: number;
  mode: ShopHoursDayMode;
  open_minute?: number;
  close_minute?: number;
}

export interface ShopHoursOverride {
  mode: "force_open" | "force_closed";
  expires_at: string;
}

export interface ShopStatus {
  open: boolean;
  next_change_at: string | null;
  reason: "schedule" | "override";
  last_call_active: boolean;
}

export interface ShopHoursSettings {
  enabled: boolean;
  last_call_lead_minutes: number;
  days: ShopHoursDay[];
  override: ShopHoursOverride | null;
  status: ShopStatus;
}

export interface ShopHoursSettingsInput {
  enabled: boolean;
  last_call_lead_minutes: number;
  days: ShopHoursDay[];
}

export interface ShopOverrideInput {
  mode: "force_open" | "force_closed";
  expires_at: string;
}

/// Body of `PUT /web-admin/owner-pin`. Write-only: the new PIN is hashed
/// server-side and never read back (ADR-0027). Validated at the Rust
/// boundary. There is intentionally no `current_pin` field — the endpoint
/// is a rotate, not a verify.
export interface OwnerPinInput {
  new_pin: string;
}

/** Body of `POST /web-admin/credential` (ADR-0050). */
export interface WebAdminPinInput {
  current_pin: string;
  new_pin: string;
}

// ---------------------------------------------------------------------------
// PC settings (ADR-0031)
// ---------------------------------------------------------------------------

/// One PC row from `GET /web-admin/pcs`. Carries the durable identity
/// fields the owner pre-registers and binds:
///   - `registered_ip` is the static IP the next boot uses to bind the
///     row (NULL until the owner fills it in).
///   - `mac_address` is the NIC that the server bound to the row (NULL
///     until the first heartbeat from the matching IP lands). The
///     owner clears this field to allow a hardware/NIC replacement to
///     re-bind.
export interface PcSettings {
  id: string;
  name: string;
  visible: boolean;
  registered_ip: string | null;
  mac_address: string | null;
}

/// Body of `PUT /web-admin/pcs/{id}` (the existing PC edit endpoint,
/// extended for ADR-0031). `registered_ip` and `mac_address` are
/// optional; pass an empty string to clear the binding.
export interface PcSettingsInput {
  name: string;
  visible: boolean;
  registered_ip?: string | null;
  mac_address?: string | null;
}

/// Body of `POST /web-admin/pcs` — pre-register a new PC by
/// number/name + a static IP (ADR-0031 / plan §4 M5). The row lands
/// with `mac_address` NULL so the next boot from that IP binds it.
export interface PcRegisterInput {
  id: string;
  name: string;
  registered_ip?: string | null;
}

// ---------------------------------------------------------------------------
// Points Transfer approvals (ADR-0030)
// ---------------------------------------------------------------------------

export type PointsTransferStatus =
  | "pending"
  | "confirmed"
  | "rejected"
  | "expired";

/// One pending Points Transfer Request, enriched with both usernames, as
/// returned by `GET /web-admin/transfer-requests`. The `code` is the
/// single-use 6-digit code the customer reads off their Client screen and
/// reads aloud to the owner; the owner enters that code to confirm or
/// reject. `created_at` is the SQLite CURRENT_TIMESTAMP string.
export interface PendingPointsTransfer {
  id: string;
  from_account_id: string;
  from_username: string;
  to_account_id: string;
  to_username: string;
  points_centi: number;
  code: string;
  created_at: string;
}

/// A resolved Points Transfer Request, returned by
/// `POST /web-admin/transfer-requests/confirm` and `…/reject`. Carries the
/// final status and `resolved_at` timestamp.
export interface PointsTransferRequest {
  id: string;
  from_account_id: string;
  to_account_id: string;
  points_centi: number;
  code: string;
  status: PointsTransferStatus;
  created_at: string;
  resolved_at: string | null;
}

/// Body of `POST /web-admin/transfer-requests/{confirm,reject}` — the
/// single-use 6-digit code the customer read aloud (ADR-0030).
export interface TransferCodeInput {
  code: string;
}

export type BackupScheduleFrequency = "daily" | "weekly" | "monthly" | "interval";

export interface BackupScheduleSettings {
  enabled: boolean;
  frequency: BackupScheduleFrequency;
  minute_of_day: number;
  weekday: number;
  day_of_month: number;
  backup_sets: string[];
  retention_count: number;
  mirror_directory: string | null;
  mirror_error: string | null;
  next_due_at: string | null;
  interval_seconds?: number;
}

export interface BackupScheduleSettingsInput {
  enabled: boolean;
  frequency: BackupScheduleFrequency;
  minute_of_day: number;
  weekday: number;
  day_of_month: number;
  backup_sets: string[];
  retention_count: number;
  mirror_directory: string | null;
  interval_seconds?: number;
}

export interface BackupCatalogEntry {
  filename: string;
  kind: "manual" | "scheduled" | "pre_migration" | "pre_restore";
  created_at: string;
  schema_version: number;
  size_bytes: number;
  backup_sets: string[];
  catch_up: boolean;
}

export interface CreatedBackup {
  filename: string;
  size_bytes: number;
}

export interface PendingRestore {
  filename: string;
  schema_version: number;
  requested_at: string;
  mirror_error: string | null;
}

export interface RestoreSource {
  filename?: string;
  path?: string;
}

// ---------------------------------------------------------------------------
// Central Terminal Autostart (ADR-0055)
// ---------------------------------------------------------------------------

/// Live Autostart state read from the host machine (registry on Windows).
/// The registry is the sole source of truth; there is no SQLite mirror.
export interface AutostartResponse {
  enabled: boolean;
  supported: boolean;
  editable: boolean;
  exe_path: string;
}

// Admin App Update (ADR-0086); all values come from the Owner server.
export interface AppUpdateRelease {
  version: string;
  schemaVersion: number;
  notes: string;
  pubDate: string;
  latestJsonUrl: string;
  raisesSchema: boolean;
}

export interface AppUpdateState {
  currentVersion: string;
  currentSchemaVersion: number;
  autoCheck: boolean;
  lastCheckedAt: string | null;
  lastError?: string;
  available?: AppUpdateRelease;
  rollbacks: AppUpdateRelease[];
  runningSessionCount: number;
  download?: { targetVersion: string; percent: number };
  lastResult?: { from: string; to: string; ok: boolean };
}

// Centralized theme (ADR-0035): the raw record is defined next to the ramp
// generator in @openpiso/ui; re-exported here so app code has one types entry.
export type { ThemeMode, ThemeRecord } from "@openpiso/ui";

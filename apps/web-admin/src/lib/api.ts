import type {
  Account,
  AccountDeletionResult,
  AccountImportReport,
  AcceptorHealth,
  AdminSettings,
  AttendanceReport,
  AttendanceReportInput,
  AttendanceSettings,
  AttendanceSettingsDraft,
  AttendanceSettingsPreview,
  AttendanceSettingsUpdate,
  AdminSettingsInput,
  AuditFilter,
  AuditPage,
  AutostartResponse,
  AppUpdateState,
  BulkNotificationAcknowledgement,
  CoinBatchRejectionList,
  CoinDebugBundle,
  CoinDebugWindow,
  CalibrationStatus,
  ClientRecoveryEscalation,
  CoinDeviceSettings,
  CoinDeviceSettingsInput,
  CoinDeviceTestResult,
  CoinPromoSettings,
  CoinPromoSettingsInput,
  CreatedBackup,
  BackupCatalogEntry,
  PendingRestore,
  RestoreSource,
  BackupScheduleSettings,
  BackupScheduleSettingsInput,
  CreateVoucherBatchInput,
  CreateVoucherInput,
  DashboardSnapshot,
  DetectedSerialPort,
  DetectedTelegramChat,
  DiagnosticsMode,
  EconomySettings,
  EconomySettingsInput,
  HostDiagnostics,
  ManualAdjustment,
  ManualAdjustmentInput,
  MonitoringSuppression,
  NotificationAcknowledgement,
  NotificationListResponse,
  NotificationSeen,
  NotificationSeverity,
  NotificationUnreadCount,
  OwnerAuth,
  OwnerPinInput,
  Pc,
  PcMonitoringCheck,
  PcMonitoringStatus,
  PcRegisterInput,
  PcSettings,
  PcSettingsInput,
  PendingPointsTransfer,
  PointsTransferRequest,
  RedemptionTier,
  RedemptionTiersInput,
  RejectionCreditResult,
  RejectionDenominations,
  ReportPeriod,
  ReportSummary,
  SalesReportOverview,
  ShopHoursSettings,
  ShopHoursSettingsInput,
  ShopOverrideInput,
  ShopRate,
  ShopRateInput,
  ShopStatus,
  SignOutResult,
  TelegramSetupCandidate,
  TelegramStatus,
  ThemeRecord,
  TransferCodeInput,
  Voucher,
  VoucherBatch,
  VoucherBatchDetail,
  VoucherBatchSummary,
  VoucherTemplate,
  VoucherTemplateInput,
  WebAdminPinInput,
  WebAdminSession,
  WebAdminSessionResponse,
  WheelSettings,
  WheelSettingsInput,
} from "./types";

const BASE = "";

/** Fallback lockout window when a 429 omits a usable `Retry-After` header. */
const DEFAULT_RETRY_AFTER_SECONDS = 60;

/** Thrown when a request is rate-limited (HTTP 429). Carries the server's
 *  `Retry-After` seconds so the UI can count down until the next attempt. The
 *  user-facing countdown is rendered by the caller, not this message. */
export class ThrottledError extends Error {
  retryAfterSeconds: number;
  constructor(retryAfterSeconds: number) {
    super("Too many attempts.");
    this.name = "ThrottledError";
    this.retryAfterSeconds = retryAfterSeconds;
  }
}

/** Thrown when the server rejects a request for want of a valid owner session
 *  (HTTP 401). Owner tokens live in the admin app's memory, so a restart
 *  invalidates every session while the browser still holds the cookie — the app
 *  must notice and fall back to the login screen instead of polling a session
 *  that no longer exists. The server's message is preserved for the UI. */
export class UnauthenticatedError extends Error {
  constructor(message: string) {
    super(message || "Owner authentication is required.");
    this.name = "UnauthenticatedError";
  }
}

export class RestoreRejectedError extends Error {
  readonly gate: string;
  readonly blockingPcs: string[];

  constructor(gate: string, message: string, blockingPcs: string[]) {
    super(message);
    this.name = "RestoreRejectedError";
    this.gate = gate;
    this.blockingPcs = blockingPcs;
  }
}

export class ApiError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "ApiError";
  }
}

export class AttendanceSettingsConflictError extends Error {
  readonly currentVersion: number;

  constructor(currentVersion: number) {
    super(`Attendance settings changed; current version is ${currentVersion}`);
    this.name = "AttendanceSettingsConflictError";
    this.currentVersion = currentVersion;
  }
}

export class AttendanceSettingsValidationError extends ApiError {
  constructor(message: string) {
    super(400, message);
    this.name = "AttendanceSettingsValidationError";
  }
}

/** The Owner Session's CSRF token (ADR-0074). Held only in memory: login/setup
 *  and session bootstrap provide it, logout and a 401 clear it. */
let csrfToken: string | null = null;

const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const headers = new Headers(options?.headers);
  if (!(options?.body instanceof FormData)) {
    headers.set("Content-Type", "application/json");
  }
  if (csrfToken && !SAFE_METHODS.has((options?.method ?? "GET").toUpperCase())) {
    headers.set("X-CSRF-Token", csrfToken);
  }
  const response = await fetch(`${BASE}${url}`, {
    credentials: "include",
    ...options,
    headers,
  });
  if (!response.ok) {
    if (response.status === 429) {
      const retryAfter = Number(response.headers.get("Retry-After"));
      throw new ThrottledError(
        Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter : DEFAULT_RETRY_AFTER_SECONDS,
      );
    }
    const text = await response.text();
    if (response.status === 401) {
      csrfToken = null;
      throw new UnauthenticatedError(text);
    }
    try {
      const payload: unknown = JSON.parse(text);
      if (
        typeof payload === "object" &&
        payload !== null &&
        "error" in payload &&
        typeof payload.error === "string" &&
        (("gate" in payload && typeof payload.gate === "string") || "blocking_pcs" in payload)
      ) {
        const blockingPcs =
          "blocking_pcs" in payload &&
          Array.isArray(payload.blocking_pcs) &&
          payload.blocking_pcs.every((value): value is string => typeof value === "string")
            ? payload.blocking_pcs
            : [];
        throw new RestoreRejectedError(
          "gate" in payload && typeof payload.gate === "string" ? payload.gate : "unknown",
          payload.error,
          blockingPcs,
        );
      }
      if (
        typeof payload === "object" &&
        payload !== null &&
        "error" in payload &&
        payload.error === "attendance_settings_stale" &&
        "current_version" in payload &&
        typeof payload.current_version === "number" &&
        Number.isSafeInteger(payload.current_version)
      ) {
        throw new AttendanceSettingsConflictError(payload.current_version);
      }
    } catch (caught) {
      if (
        caught instanceof RestoreRejectedError ||
        caught instanceof AttendanceSettingsConflictError
      ) throw caught;
    }
    throw new ApiError(response.status, text || `Request failed: ${response.status}`);
  }
  return response.json() as Promise<T>;
}

// ---------------------------------------------------------------------------
// Session / auth
// ---------------------------------------------------------------------------

/// Session bootstrap: also restores the in-memory CSRF token after a reload
/// (the cookie is HttpOnly), or forgets a stale one when no session exists.
export async function getSession(): Promise<WebAdminSession> {
  const { csrf_token, ...session } = await request<WebAdminSessionResponse>(
    "/web-admin/session",
  );
  csrfToken = session.authenticated ? (csrf_token ?? null) : null;
  return session;
}

export async function setupOwner(pin: string): Promise<void> {
  const auth = await request<OwnerAuth>("/web-admin/setup", {
    method: "POST",
    body: JSON.stringify({ pin }),
  });
  csrfToken = auth.csrf_token;
}

export async function ownerLogin(pin: string): Promise<void> {
  const auth = await request<OwnerAuth>("/web-admin/login", {
    method: "POST",
    body: JSON.stringify({ pin }),
  });
  csrfToken = auth.csrf_token;
}

export async function ownerLogout(): Promise<void> {
  try {
    await request<{ ok: boolean }>("/web-admin/logout", { method: "POST" });
  } finally {
    csrfToken = null;
  }
}

/// `POST /web-admin/activity` — tell the server a human is using this Owner
/// Session. The only request that refreshes the idle timer; polls never do.
export function reportHumanActivity(): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>("/web-admin/activity", { method: "POST" });
}

// ---------------------------------------------------------------------------
// Dashboard + live data
// ---------------------------------------------------------------------------

export function getDashboard(): Promise<DashboardSnapshot> {
  return request<DashboardSnapshot>("/web-admin/dashboard");
}

/// `POST /web-admin/pcs/{pc_id}/sign-out` — Owner Sign-Out: end the Active
/// Session on a PC. Keyed on `session_id` so a stale dashboard row cannot
/// eject whoever took the seat since; that case comes back
/// `signed_out: false` rather than as an error.
export function signOutPcSession(
  pcId: string,
  sessionId: string,
): Promise<SignOutResult> {
  return request<SignOutResult>(`/web-admin/pcs/${encodeURIComponent(pcId)}/sign-out`, {
    method: "POST",
    body: JSON.stringify({ session_id: sessionId }),
  });
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

export function listBackups(): Promise<BackupCatalogEntry[]> {
  return request<BackupCatalogEntry[]>("/web-admin/backups");
}

export function createBackup(backupSets: string[]): Promise<CreatedBackup> {
  return request<CreatedBackup>("/web-admin/backups", {
    method: "POST",
    body: JSON.stringify({ backup_sets: backupSets }),
  });
}

export function downloadBackupUrl(filename: string): string {
  return `${BASE}/web-admin/backups/${encodeURIComponent(filename)}`;
}

export function deleteBackup(filename: string): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(`/web-admin/backups/${encodeURIComponent(filename)}`, {
    method: "DELETE",
  });
}

export function getBackupSchedule(): Promise<BackupScheduleSettings> {
  return request<BackupScheduleSettings>("/web-admin/backup-schedule");
}

export function updateBackupSchedule(
  input: BackupScheduleSettingsInput,
): Promise<BackupScheduleSettings> {
  return request<BackupScheduleSettings>("/web-admin/backup-schedule", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

export function getPendingRestore(): Promise<PendingRestore | null> {
  return request<PendingRestore | null>("/web-admin/restore");
}

export function stageRestore(source: RestoreSource): Promise<PendingRestore> {
  return request<PendingRestore>("/web-admin/restore", {
    method: "POST",
    body: JSON.stringify(source),
  });
}

export function uploadBackup(file: File): Promise<PendingRestore> {
  const form = new FormData();
  form.append("file", file, file.name);
  return request<PendingRestore>("/web-admin/restore", {
    method: "POST",
    body: form,
  });
}

export function cancelPendingRestore(): Promise<{ cancelled: boolean }> {
  return request<{ cancelled: boolean }>("/web-admin/restore", {
    method: "DELETE",
  });
}

// ---------------------------------------------------------------------------
// Economy settings
// ---------------------------------------------------------------------------

export function getEconomySettings(): Promise<EconomySettings> {
  return request<EconomySettings>("/web-admin/economy");
}

export function updateEconomySettings(
  input: EconomySettingsInput,
): Promise<EconomySettings> {
  return request<EconomySettings>("/web-admin/economy", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

export function getRedemptionTiers(): Promise<RedemptionTier[]> {
  return request<RedemptionTier[]>("/web-admin/redemption-tiers");
}

export function updateRedemptionTiers(
  input: RedemptionTiersInput,
): Promise<RedemptionTier[]> {
  return request<RedemptionTier[]>("/web-admin/redemption-tiers", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

export function getCoinPromo(): Promise<CoinPromoSettings> {
  return request<CoinPromoSettings>("/web-admin/coin-promo");
}

export function previewCoinPromo(input: CoinPromoSettingsInput): Promise<CoinPromoSettings> {
  return request<CoinPromoSettings>("/web-admin/coin-promo/preview", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export function updateCoinPromo(input: CoinPromoSettingsInput): Promise<CoinPromoSettings> {
  return request<CoinPromoSettings>("/web-admin/coin-promo", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

export function getAttendanceSettings(): Promise<AttendanceSettings> {
  return request<AttendanceSettings>("/web-admin/attendance/settings");
}

export function previewAttendanceSettings(
  input: AttendanceSettingsDraft,
): Promise<AttendanceSettingsPreview> {
  return request<AttendanceSettingsPreview>("/web-admin/attendance/settings/preview", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export async function updateAttendanceSettings(
  input: AttendanceSettingsUpdate,
): Promise<AttendanceSettings> {
  try {
    return await request<AttendanceSettings>("/web-admin/attendance/settings", {
      method: "PUT",
      body: JSON.stringify(input),
    });
  } catch (caught) {
    if (caught instanceof ApiError && caught.status === 400) {
      throw new AttendanceSettingsValidationError(caught.message);
    }
    throw caught;
  }
}

// ---------------------------------------------------------------------------
// Vouchers
// ---------------------------------------------------------------------------

export function createVoucher(input: CreateVoucherInput): Promise<Voucher> {
  return request<Voucher>("/web-admin/vouchers", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export function listVouchers(
  limit = 100,
  filters: {
    offset?: number;
    batchId?: string;
    search?: string;
    status?: Voucher["status"];
  } = {},
): Promise<Voucher[]> {
  const query = new URLSearchParams({ limit: String(limit) });
  if (filters.offset) query.set("offset", String(filters.offset));
  if (filters.batchId) query.set("batch_id", filters.batchId);
  if (filters.search) query.set("search", filters.search);
  if (filters.status) query.set("status", filters.status);
  return request<Voucher[]>(`/web-admin/vouchers?${query}`);
}

export function createVoucherBatch(
  input: CreateVoucherBatchInput,
): Promise<VoucherBatch> {
  return request<VoucherBatch>("/web-admin/voucher-batches", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export function listVoucherBatches(
  limit = 100,
  offset = 0,
): Promise<VoucherBatchSummary[]> {
  return request<VoucherBatchSummary[]>(
    `/web-admin/voucher-batches?limit=${limit}&offset=${offset}`,
  );
}

export function getVoucherBatch(id: string): Promise<VoucherBatchDetail> {
  return request<VoucherBatchDetail>(
    `/web-admin/voucher-batches/${encodeURIComponent(id)}`,
  );
}

export function voidVoucher(id: string): Promise<Voucher> {
  return request<Voucher>(
    `/web-admin/vouchers/${encodeURIComponent(id)}/void`,
    { method: "POST" },
  );
}

export function voidVoucherBatch(id: string): Promise<VoucherBatch> {
  return request<VoucherBatch>(
    `/web-admin/voucher-batches/${encodeURIComponent(id)}/void`,
    {
      method: "POST",
    },
  );
}

export function getVoucherTemplate(): Promise<VoucherTemplate> {
  return request<VoucherTemplate>("/web-admin/voucher-template");
}

export function updateVoucherTemplate(
  input: VoucherTemplateInput,
): Promise<VoucherTemplate> {
  return request<VoucherTemplate>("/web-admin/voucher-template", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Spin the Wheel
// ---------------------------------------------------------------------------

export function getWheelSettings(): Promise<WheelSettings> {
  return request<WheelSettings>("/web-admin/wheel");
}

export function updateWheelSettings(
  input: WheelSettingsInput,
): Promise<WheelSettings> {
  return request<WheelSettings>("/web-admin/wheel", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

export function listAccounts(): Promise<Account[]> {
  return request<Account[]>("/web-admin/accounts");
}

export function createAccount(
  username: string,
  password: string,
): Promise<Account> {
  return request<Account>("/web-admin/accounts", {
    method: "POST",
    body: JSON.stringify({ username, password }),
  });
}

export function importAccounts(
  payload: unknown,
  dryRun: boolean,
): Promise<AccountImportReport> {
  return request<AccountImportReport>("/web-admin/accounts/import", {
    method: "POST",
    body: JSON.stringify({ accounts: payload, dry_run: dryRun }),
  });
}

export function resetAccountPassword(
  accountId: string,
  password: string,
): Promise<Account> {
  return request<Account>("/web-admin/accounts/reset", {
    method: "POST",
    body: JSON.stringify({ account_id: accountId, password }),
  });
}

export function deleteAccounts(
  accountIds: string[],
): Promise<AccountDeletionResult> {
  return request<AccountDeletionResult>("/web-admin/accounts/delete", {
    method: "POST",
    body: JSON.stringify({ account_ids: accountIds }),
  });
}

/// `POST /web-admin/accounts/adjust-time` — owner grants (positive) or
/// deducts (negative) an account's remaining seconds. The server clamps the
/// result at zero and audits the applied delta.
export function adjustAccountTime(
  accountId: string,
  delta: number,
): Promise<Account> {
  return request<Account>("/web-admin/accounts/adjust-time", {
    method: "POST",
    body: JSON.stringify({ account_id: accountId, delta }),
  });
}

/// `POST /web-admin/accounts/adjust-points` — owner grants or deducts an
/// account's points balance in integer centipoints (ADR-0038). Clamped at
/// zero, applied delta audited.
export function adjustAccountPoints(
  accountId: string,
  deltaCenti: number,
): Promise<Account> {
  return request<Account>("/web-admin/accounts/adjust-points", {
    method: "POST",
    body: JSON.stringify({ account_id: accountId, delta_centi: deltaCenti }),
  });
}

// ---------------------------------------------------------------------------
// Rejection Credits (ADR-0069)
// ---------------------------------------------------------------------------

/**
 * `GET /web-admin/rejection-credits/rejections` — Coin Batch Rejections inside
 * the 30-day window. `accountId` narrows to rejections whose **recorded**
 * Account is that account; credited rows stay listed unless `creditableOnly`.
 */
export function listCoinBatchRejections(
  options: { accountId?: string; creditableOnly?: boolean } = {},
): Promise<CoinBatchRejectionList> {
  const query = new URLSearchParams();
  if (options.accountId) query.set("account_id", options.accountId);
  if (options.creditableOnly) query.set("creditable_only", "true");
  return request<CoinBatchRejectionList>(
    `/web-admin/rejection-credits/rejections?${query}`,
  );
}

/**
 * `GET /web-admin/rejection-credits/denominations` — the distinct denominations
 * of the retained current Pulse Map. The only legal source for a historical
 * Rejection Credit amount; automatic Coin Batch authority is pulse-sum based.
 */
export function getRejectionDenominations(): Promise<RejectionDenominations> {
  return request<RejectionDenominations>(
    "/web-admin/rejection-credits/denominations",
  );
}

/**
 * `POST /web-admin/rejection-credits` — credit a rejected coin to the target
 * frozen at the rejection. `dryRun` previews the server-computed seconds and
 * points without writing, so the confirmation never recomputes pricing here.
 */
export function creditRejection(input: {
  rejectionAuditId: string;
  denominationCents: number;
  coins: number;
  dryRun?: boolean;
}): Promise<RejectionCreditResult> {
  return request<RejectionCreditResult>("/web-admin/rejection-credits", {
    method: "POST",
    body: JSON.stringify({
      rejection_audit_id: input.rejectionAuditId,
      denomination_cents: input.denominationCents,
      coins: input.coins,
      dry_run: input.dryRun ?? false,
    }),
  });
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

export function getReportSummary(
  period: ReportPeriod,
  fromDate?: string,
  toDate?: string,
): Promise<ReportSummary> {
  return request<ReportSummary>("/web-admin/reports/summary", {
    method: "POST",
    body: JSON.stringify({
      period,
      from_date: fromDate ?? null,
      to_date: toDate ?? null,
    }),
  });
}

/// `GET /web-admin/reports/overview` — the current sales cards and all-PC
/// sales breakdown used by the Sales Report page.
export function getSalesReportOverview(): Promise<SalesReportOverview> {
  return request<SalesReportOverview>("/web-admin/reports/overview");
}

export function getAttendanceReport(
  input: AttendanceReportInput,
): Promise<AttendanceReport> {
  return request<AttendanceReport>("/web-admin/reports/attendance", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

export function fetchAuditActors(): Promise<string[]> {
  return request<string[]>("/web-admin/audit/actors");
}

export function fetchAuditEventTypes(): Promise<string[]> {
  return request<string[]>("/web-admin/audit/event-types");
}

export function searchAuditLog(filter: AuditFilter): Promise<AuditPage> {
  return request<AuditPage>("/web-admin/audit/search", {
    method: "POST",
    body: JSON.stringify(filter),
  });
}

export function exportAuditLogCsv(filter: AuditFilter): Promise<string> {
  return request<string>("/web-admin/audit/export", {
    method: "POST",
    body: JSON.stringify(filter),
  });
}

// ---------------------------------------------------------------------------
// Notifications (ADR-0058)

export function listNotifications(
  options: {
    limit?: number;
    offset?: number;
    severity?: NotificationSeverity;
    unread?: boolean;
  } = {},
): Promise<NotificationListResponse> {
  const query = new URLSearchParams();
  query.set("limit", String(options.limit ?? 50));
  query.set("offset", String(options.offset ?? 0));
  if (options.severity) query.set("severity", options.severity);
  if (options.unread !== undefined) query.set("unread", String(options.unread));
  return request<NotificationListResponse>(`/web-admin/notifications?${query}`);
}

export function getNotificationUnreadCount(): Promise<NotificationUnreadCount> {
  return request<NotificationUnreadCount>("/web-admin/notifications/unread-count");
}

export function markNotificationsSeen(): Promise<NotificationSeen> {
  return request<NotificationSeen>("/web-admin/notifications/seen", {
    method: "POST",
  });
}

export function acknowledgeNotification(auditId: string): Promise<NotificationAcknowledgement> {
  return request<NotificationAcknowledgement>(
    `/web-admin/notifications/${encodeURIComponent(auditId)}/acknowledge`,
    { method: "POST" },
  );
}

/** Server cap on ids per bulk acknowledgement request. */
const ACKNOWLEDGE_BATCH_SIZE = 200;

/**
 * Acknowledges the given Alerts. Longer lists are sent as sequential batches
 * because the server caps one request at {@link ACKNOWLEDGE_BATCH_SIZE} ids;
 * each batch is atomic on its own.
 */
export async function acknowledgeNotifications(
  auditIds: string[],
): Promise<BulkNotificationAcknowledgement> {
  let acknowledged_count = 0;
  for (let start = 0; start < auditIds.length; start += ACKNOWLEDGE_BATCH_SIZE) {
    const batch = await request<BulkNotificationAcknowledgement>(
      "/web-admin/notifications/acknowledge",
      {
        method: "POST",
        body: JSON.stringify({
          audit_ids: auditIds.slice(start, start + ACKNOWLEDGE_BATCH_SIZE),
        }),
      },
    );
    acknowledged_count += batch.acknowledged_count;
  }
  return { acknowledged_count };
}

// ---------------------------------------------------------------------------
// Manual adjustments
// ---------------------------------------------------------------------------

export function createManualAdjustment(
  input: ManualAdjustmentInput,
): Promise<ManualAdjustment> {
  return request<ManualAdjustment>("/web-admin/manual-adjustment", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Coin-device settings
// ---------------------------------------------------------------------------

export function getCoinDeviceSettings(): Promise<CoinDeviceSettings> {
  return request<CoinDeviceSettings>("/web-admin/coin-device");
}

export function getCoinDevicePorts(): Promise<DetectedSerialPort[]> {
  return request<DetectedSerialPort[]>("/web-admin/coin-device/ports");
}

export function updateCoinDeviceSettings(
  input: CoinDeviceSettingsInput,
): Promise<CoinDeviceSettings> {
  return request<CoinDeviceSettings>("/web-admin/coin-device", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

export function testCoinDevice(): Promise<CoinDeviceTestResult> {
  return request<CoinDeviceTestResult>("/web-admin/coin-device/test", {
    method: "POST",
  });
}

export function startCoinCalibration(
  targetAmountCents: number,
): Promise<CalibrationStatus> {
  return request<CalibrationStatus>("/web-admin/coin-device/calibration", {
    method: "POST",
    body: JSON.stringify({ target_amount_cents: targetAmountCents }),
  });
}

export function getCurrentCoinCalibration(): Promise<CalibrationStatus | null> {
  return request<CalibrationStatus | null>(
    "/web-admin/coin-device/calibration",
  );
}

export function getCoinCalibration(
  captureId: string,
): Promise<CalibrationStatus> {
  return request<CalibrationStatus>(
    `/web-admin/coin-device/calibration/${encodeURIComponent(captureId)}`,
  );
}

export function recordCoinCalibrationSample(
  captureId: string,
): Promise<CalibrationStatus> {
  return request<CalibrationStatus>(
    `/web-admin/coin-device/calibration/${encodeURIComponent(captureId)}/sample`,
    { method: "POST" },
  );
}

export function armCoinCalibrationSample(
  captureId: string,
): Promise<CalibrationStatus> {
  return request<CalibrationStatus>(
    `/web-admin/coin-device/calibration/${encodeURIComponent(captureId)}/arm`,
    { method: "POST" },
  );
}

export function cancelCoinCalibration(
  captureId: string,
): Promise<CalibrationStatus> {
  return request<CalibrationStatus>(
    `/web-admin/coin-device/calibration/${encodeURIComponent(captureId)}/cancel`,
    { method: "POST" },
  );
}

export function applyCoinCalibration(
  captureId: string,
): Promise<CoinDeviceSettings> {
  return request<CoinDeviceSettings>(
    `/web-admin/coin-device/calibration/${encodeURIComponent(captureId)}/apply`,
    { method: "POST" },
  );
}

export function getCoinDebugBundle(): Promise<CoinDebugBundle> {
  return request<CoinDebugBundle>("/web-admin/coin-device/debug-bundle");
}

/** `GET /web-admin/coin-device/acceptor-health` — the owner-facing S1
 * health contract. Timing remains unavailable until filtered timing lands. */
export function getAcceptorHealth(): Promise<AcceptorHealth> {
  return request<AcceptorHealth>("/web-admin/coin-device/acceptor-health");
}

export function getCoinDebugWindow(): Promise<CoinDebugWindow> {
  return request<CoinDebugWindow>("/web-admin/coin-device/debug-window");
}

export function resetCoinDebugWindow(): Promise<CoinDebugWindow> {
  return request<CoinDebugWindow>("/web-admin/coin-device/debug-window", {
    method: "POST",
  });
}

// ---------------------------------------------------------------------------
// Central Terminal Autostart (ADR-0055)
// ---------------------------------------------------------------------------

/// `GET /web-admin/autostart` — read the live Autostart state from the
/// host machine. The registry is the sole source of truth.
export function getAutostart(): Promise<AutostartResponse> {
  return request<AutostartResponse>("/web-admin/autostart");
}

/// `PUT /web-admin/autostart` — write the Autostart enabled state. The
/// response always reflects the machine state after the write, never an
/// echo of the request. Refused with a client error while `editable` or
/// `supported` is false.
export function updateAutostart(input: {
  enabled: boolean;
}): Promise<AutostartResponse> {
  return request<AutostartResponse>("/web-admin/autostart", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Admin App Update (ADR-0086), Owner routes on :8080 only.
// ---------------------------------------------------------------------------

export function getAppUpdate(): Promise<AppUpdateState> {
  return request<AppUpdateState>("/web-admin/update");
}

export function checkAppUpdate(): Promise<AppUpdateState> {
  return request<AppUpdateState>("/web-admin/update/check", { method: "POST" });
}

export function setAppUpdateAutoCheck(input: {
  enabled: boolean;
}): Promise<AppUpdateState> {
  return request<AppUpdateState>("/web-admin/update/auto-check", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

export function acknowledgeAppUpdate(): Promise<AppUpdateState> {
  return request<AppUpdateState>("/web-admin/update/ack", { method: "POST" });
}

export function rollbackAppUpdate(input: {
  version: string;
  confirmVersion: string;
}): Promise<AppUpdateState> {
  return request<AppUpdateState>("/web-admin/update/rollback", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export function installAppUpdate(input: {
  version: string;
  acknowledgeSchemaChange?: boolean;
}): Promise<AppUpdateState> {
  return request<AppUpdateState>("/web-admin/update/install", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Theme (ADR-0035)
// ---------------------------------------------------------------------------

/// `GET /api/theme` (:8080, OwnerSession) — the raw shop theme record.
export function getTheme(): Promise<ThemeRecord> {
  return request<ThemeRecord>("/api/theme");
}

/// `PUT /api/theme` — full-record write (no partial patch); the server bumps
/// `version` atomically and returns the fresh record.
export function updateTheme(input: ThemeRecord): Promise<ThemeRecord> {
  return request<ThemeRecord>("/api/theme", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// Shop settings + Client owner PIN (ADR-0027)
// ---------------------------------------------------------------------------

/// `GET /web-admin/settings` — returns the non-secret `shop_name` the
/// web-admin (and the public `GET /api/shop` for the Clients) read.
export function getSettings(): Promise<AdminSettings> {
  return request<AdminSettings>("/web-admin/settings");
}

/// `PUT /web-admin/settings` — owner edits `shop_name`, the expiry shutdown
/// grace, and Offline Grace; all are validated at the Rust boundary.
export function updateSettings(
  input: AdminSettingsInput,
): Promise<AdminSettings> {
  return request<AdminSettings>("/web-admin/settings", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

/// `GET /web-admin/diagnostics-mode` — the shop-wide Diagnostics Mode flag
/// (ADR-0081): visibility only, defaulting to off.
export function getDiagnosticsMode(): Promise<DiagnosticsMode> {
  return request<DiagnosticsMode>("/web-admin/diagnostics-mode");
}

/// `PUT /web-admin/diagnostics-mode` — flip the shop-wide flag. Requires an
/// Owner Session only (no PIN step-up, ADR-0081) and is audited.
export function setDiagnosticsMode(enabled: boolean): Promise<DiagnosticsMode> {
  return request<DiagnosticsMode>("/web-admin/diagnostics-mode", {
    method: "PUT",
    body: JSON.stringify({ enabled }),
  });
}

/// `GET /web-admin/shop-rate` — the active owner-configured paid-time exchange.
export function getShopRate(): Promise<ShopRate> {
  return request<ShopRate>("/web-admin/shop-rate");
}

/// `PUT /web-admin/shop-rate` — appends a new rate and retires the old one.
export function updateShopRate(input: ShopRateInput): Promise<ShopRate> {
  return request<ShopRate>("/web-admin/shop-rate", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

/// `GET /api/shop-status` — public, server-resolved Shop Status.
export function getShopStatus(): Promise<ShopStatus> {
  return request<ShopStatus>("/api/shop-status");
}

/// `GET /web-admin/shop-hours` — complete owner-authenticated Shop Hours
/// settings, including all seven weekday rows and the current override state.
export function getShopHours(): Promise<ShopHoursSettings> {
  return request<ShopHoursSettings>("/web-admin/shop-hours");
}

/// `PUT /web-admin/shop-hours` — atomically replaces the complete weekly
/// schedule and Shop Hours settings.
export function updateShopHours(
  input: ShopHoursSettingsInput,
): Promise<ShopHoursSettings> {
  return request<ShopHoursSettings>("/web-admin/shop-hours", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

/// `POST /web-admin/shop-override` — sets a temporary force with an absolute
/// RFC 3339 expiry instant.
export function setShopOverride(
  input: ShopOverrideInput,
): Promise<ShopHoursSettings> {
  return request<ShopHoursSettings>("/web-admin/shop-override", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

/// `DELETE /web-admin/shop-override` — clears the current Shop Override.
export function clearShopOverride(): Promise<ShopHoursSettings> {
  return request<ShopHoursSettings>("/web-admin/shop-override", {
    method: "DELETE",
  });
}

/// `PUT /web-admin/owner-pin` — write-only; the new PIN is hashed server-side
/// and never read back (ADR-0027). The endpoint intentionally takes no
/// `current_pin` — it is a rotate, not a verify.
export function setOwnerPin(input: OwnerPinInput): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>("/web-admin/owner-pin", {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

/// `POST /web-admin/credential` — rotate the Web Admin PIN after the owner
/// proves knowledge of the current PIN (ADR-0050).
export function rotateWebAdminPin(
  input: WebAdminPinInput,
): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>("/web-admin/credential", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

// ---------------------------------------------------------------------------
// PC settings (ADR-0031)
// ---------------------------------------------------------------------------

/// `GET /web-admin/pcs` — list every PC row with the durable identity
/// fields the owner pre-registers / binds. The new `registered_ip` and
/// `mac_address` columns live alongside the existing `name` and
/// `visible`.
export function listPcSettings(): Promise<PcSettings[]> {
  return request<PcSettings[]>("/web-admin/pcs");
}

/// `PUT /web-admin/pcs/{id}` — update a PC's display name, terminal
/// visibility, and identity fields (ADR-0031). Pass an empty
/// `registered_ip` or `mac_address` to clear the binding; omit the
/// fields to leave them unchanged.
export function updatePcSettings(
  id: string,
  input: PcSettingsInput,
): Promise<PcSettings> {
  return request<PcSettings>(`/web-admin/pcs/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: JSON.stringify(input),
  });
}

/// `POST /web-admin/pcs` — pre-register a PC by number/name + a
/// static IP (ADR-0031 / plan §4 M5). The new row ships with
/// `mac_address` NULL, so the next boot from that IP binds it.
export function registerPc(input: PcRegisterInput): Promise<Pc> {
  return request<Pc>("/web-admin/pcs", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

/// `DELETE /web-admin/pcs/{id}` — hard-delete a clean PC row (issue #112 /
/// ADR-0044). The server is the sole authority: a row with coin/adjustment/
/// audit history, remaining Guest Time, or an online status is refused with a
/// 409 whose message names the reason; an unknown id is 404. Those messages
/// surface in the PCs tab's per-row error area.
export function removePc(id: string): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(`/web-admin/pcs/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

// ---------------------------------------------------------------------------
// PC Monitoring Check now (ADR-0076)
// ---------------------------------------------------------------------------

/// `POST /web-admin/pc-monitoring/check/{pc_id}` — non-mutating Check now
/// for one PC: Host Reachability (live ICMP probe) plus Client Reachability
/// (heartbeat verdict). The server picks the probe target from the PC's own
/// registered IP; no address can be supplied from here.
export function checkPcMonitoring(pcId: string): Promise<PcMonitoringCheck> {
  return request<PcMonitoringCheck>(
    `/web-admin/pc-monitoring/check/${encodeURIComponent(pcId)}`,
    { method: "POST" },
  );
}

/// `POST /web-admin/pc-monitoring/check-all` — Check now for every eligible
/// PC (visible with a registered IP). One result per eligible PC.
export function checkAllPcMonitoring(): Promise<PcMonitoringCheck[]> {
  return request<PcMonitoringCheck[]>("/web-admin/pc-monitoring/check-all", {
    method: "POST",
  });
}

/// `GET /web-admin/pc-monitoring/status` — shop-wide monitoring state for
/// the standing advisory (ADR-0076).
export function getPcMonitoringStatus(): Promise<PcMonitoringStatus> {
  return request<PcMonitoringStatus>("/web-admin/pc-monitoring/status");
}

/// `PUT /web-admin/pc-monitoring/enabled` — shop-wide enable/disable. The
/// current Owner PIN is re-proved server-side; disabling closes active
/// incidents as monitoring-disabled.
export function setPcMonitoringEnabled(
  enabled: boolean,
  currentPin: string,
): Promise<PcMonitoringStatus> {
  return request<PcMonitoringStatus>("/web-admin/pc-monitoring/enabled", {
    method: "PUT",
    body: JSON.stringify({ enabled, current_pin: currentPin }),
  });
}

/// `GET /web-admin/pc-monitoring/escalation` — the shop-wide Client Recovery
/// Escalation policy (ADR-0077).
export function getClientRecoveryEscalation(): Promise<ClientRecoveryEscalation> {
  return request<ClientRecoveryEscalation>("/web-admin/pc-monitoring/escalation");
}

/// `PUT /web-admin/pc-monitoring/escalation` — enable, disable, or retune
/// Client Recovery Escalation. The current Owner PIN is re-proved
/// server-side, and the server clamps the delay to its own floor.
export function setClientRecoveryEscalation(
  enabled: boolean,
  delaySecs: number,
  currentPin: string,
): Promise<ClientRecoveryEscalation> {
  return request<ClientRecoveryEscalation>("/web-admin/pc-monitoring/escalation", {
    method: "PUT",
    body: JSON.stringify({
      enabled,
      delay_secs: delaySecs,
      current_pin: currentPin,
    }),
  });
}

/// `POST /web-admin/pc-monitoring/suppressions/{pc_id}` — create or replace
/// the per-PC Monitoring Suppression. Requires the current Owner PIN; the
/// absolute expiry is computed by Admin (1–60 minutes, fixed policy).
export function setPcMonitoringSuppression(
  pcId: string,
  durationMinutes: number,
  currentPin: string,
): Promise<MonitoringSuppression> {
  return request<MonitoringSuppression>(
    `/web-admin/pc-monitoring/suppressions/${encodeURIComponent(pcId)}`,
    {
      method: "POST",
      body: JSON.stringify({ duration_minutes: durationMinutes, current_pin: currentPin }),
    },
  );
}

/// `POST /web-admin/pc-monitoring/suppressions/{pc_id}/cancel` — cancel the
/// per-PC Monitoring Suppression. `cancelled` is false when nothing was
/// active (already expired), which stays a success.
export function cancelPcMonitoringSuppression(
  pcId: string,
  currentPin: string,
): Promise<{ cancelled: boolean }> {
  return request<{ cancelled: boolean }>(
    `/web-admin/pc-monitoring/suppressions/${encodeURIComponent(pcId)}/cancel`,
    { method: "POST", body: JSON.stringify({ current_pin: currentPin }) },
  );
}

// ---------------------------------------------------------------------------
// Support Bundle (ADR-0078)
// ---------------------------------------------------------------------------

/// `GET /web-admin/support-bundle?pc_id={id}&window_seconds={seconds}` —
/// the owner-gated Support Bundle for one PC over one window. Owner Session
/// only, no PIN step-up: the bundle is redacted and read-only.
/// `windowSeconds` defaults to Admin's own six-hour default when omitted;
/// the server clamps any wider request. `incidentOpenedAt` anchors the
/// window around a PC Monitoring Incident instead — one hour before to
/// fifteen minutes after it — so a hours-old incident still returns the
/// evidence that explains it.
export function getSupportBundle(
  pcId: string,
  windowSeconds?: number,
  incidentOpenedAt?: string,
): Promise<string> {
  const query = new URLSearchParams({ pc_id: pcId });
  if (windowSeconds !== undefined) query.set("window_seconds", String(windowSeconds));
  if (incidentOpenedAt !== undefined) query.set("incident_opened_at", incidentOpenedAt);
  return request<string>(`/web-admin/support-bundle?${query}`);
}

/// `POST /web-admin/support/diagnostic-request/{pc_id}` — place the
/// Diagnostic Request (ADR-0078): the Heartbeat response then carries a
/// single-use nonce asking the live Client to upload one windowed,
/// byte-capped slice of its own log, which the next Support Bundle includes.
/// Owner Session only. A PC that is offline or unreachable produces no error
/// here — the ask simply expires unfulfilled.
export function requestDiagnostics(pcId: string): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(
    `/web-admin/support/diagnostic-request/${encodeURIComponent(pcId)}`,
    { method: "POST" },
  );
}

/// `GET /web-admin/host-diagnostics` — Admin's own health (ADR-0082) for the
/// Server tab: Run Records newest first plus the panic file's newest 8 KB.
/// Owner Session only; served identically with Diagnostics Mode on and off
/// (ADR-0081). Never errors on absence — an empty history and a missing
/// panic file answer cleanly.
export function getHostDiagnostics(): Promise<HostDiagnostics> {
  return request<HostDiagnostics>("/web-admin/host-diagnostics");
}

// ---------------------------------------------------------------------------
// Telegram connect-and-protect (ADR-0076)
// ---------------------------------------------------------------------------

/// `GET /web-admin/telegram/status` — connected destination, last successful
/// test, and platform availability. The bot token is never returned.
export function getTelegramStatus(): Promise<TelegramStatus> {
  return request<TelegramStatus>("/web-admin/telegram/status");
}

/// `POST /web-admin/telegram/setup/token` — verify a candidate bot token and
/// hold it server-side in memory for confirmation. Nothing is persisted here.
export function submitTelegramToken(token: string): Promise<TelegramSetupCandidate> {
  return request<TelegramSetupCandidate>("/web-admin/telegram/setup/token", {
    method: "POST",
    body: JSON.stringify({ token }),
  });
}

/// `POST /web-admin/telegram/setup/detect` — one explicit look for recent
/// private messages using the pending token. The only polling the integration
/// ever performs.
export function detectTelegramChat(): Promise<DetectedTelegramChat[]> {
  return request<DetectedTelegramChat[]>("/web-admin/telegram/setup/detect", {
    method: "POST",
  });
}

/// `POST /web-admin/telegram/connect` — confirm the destination and send the
/// test message; the new configuration activates only if the test succeeds.
/// Requires the current Owner PIN. `chatId` is the manual fallback; omitting
/// it uses the detected chat.
export function connectTelegram(
  chatId: string | undefined,
  currentPin: string,
): Promise<TelegramStatus> {
  return request<TelegramStatus>("/web-admin/telegram/connect", {
    method: "POST",
    body: JSON.stringify({ chat_id: chatId, current_pin: currentPin }),
  });
}

/// `POST /web-admin/telegram/disconnect` — delete the protected token and
/// destination. Local PC Monitoring is unaffected. Requires the current PIN.
export function disconnectTelegram(currentPin: string): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>("/web-admin/telegram/disconnect", {
    method: "POST",
    body: JSON.stringify({ current_pin: currentPin }),
  });
}

// ---------------------------------------------------------------------------
// Points Transfer approvals (ADR-0030)
// ---------------------------------------------------------------------------

/// `GET /web-admin/transfer-requests` — list pending Points Transfer
/// Requests. Past-TTL rows are swept to `expired` server-side first, so a
/// stale request never appears confirmable (plan §5.3).
export function listTransferRequests(): Promise<PendingPointsTransfer[]> {
  return request<PendingPointsTransfer[]>("/web-admin/transfer-requests");
}

/// `POST /web-admin/transfer-requests/confirm` — approve a pending request
/// by its 6-digit code. Points are re-validated against the live sender
/// balance and moved atomically server-side.
export function confirmTransferRequest(
  input: TransferCodeInput,
): Promise<PointsTransferRequest> {
  return request<PointsTransferRequest>(
    "/web-admin/transfer-requests/confirm",
    {
      method: "POST",
      body: JSON.stringify(input),
    },
  );
}

/// `POST /web-admin/transfer-requests/reject` — reject a pending request by
/// its 6-digit code. No points move.
export function rejectTransferRequest(
  input: TransferCodeInput,
): Promise<PointsTransferRequest> {
  return request<PointsTransferRequest>("/web-admin/transfer-requests/reject", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

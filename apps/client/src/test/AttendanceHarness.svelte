<script lang="ts">
  import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
  import { emit } from "@tauri-apps/api/event";
  import { tick } from "svelte";
  import type {
    AccountAuth,
    AttendanceClaimError,
    AttendanceSummary,
    ClientConfig,
    TimerState,
  } from "$lib/types";
  import App from "../App.svelte";

  interface HarnessController {
    readonly calls: { command: string; args: unknown }[];
    readonly attendanceRequestArgs: unknown[];
    setTimer: (remainingSeconds: number) => Promise<void>;
    acceptCoin: (cappedCents: number) => Promise<void>;
    setAttendanceToday: (
      enabled: boolean,
      status: "checked_in" | "in_progress" | "ineligible",
      cappedCents: number,
    ) => Promise<void>;
    heartbeat: () => Promise<void>;
    setAttendanceSupported: (supported: boolean) => Promise<void>;
    setOffline: (offline: boolean) => void;
    holdNextAttendanceClaims: (count: number) => void;
    rejectNextAttendanceClaim: () => Promise<void>;
    failNextAttendanceClaim: (error: AttendanceClaimError) => void;
    restoreClaimableAugust: () => Promise<void>;
    setAccount: (accountId: string | null) => Promise<void>;
  }

  const calls: { command: string; args: unknown }[] = [];
  let timer: TimerState & { attendance_supported?: boolean } = {
    pc_id: "pc-07",
    pc_name: "PC 07",
    pc_status: "online",
    remaining_seconds: 3600,
    guest_remaining_seconds: 300,
    timer_mode: "account",
    active_username: "alex",
    connected: true,
    connection_status: "connected",
    expired: false,
    overlay_locked: false,
    power_state: "active",
    shutdown_in_seconds: null,
    offline_shutdown_in_seconds: null,
    shutdown_grace_seconds: 30,
    attendance_supported: true,
    shop_status: null,
    error: null,
  };
  const config: ClientConfig = {
    server_url: "http://127.0.0.1:3210",
    pc_id: "pc-07",
    view_mode: "widget",
    launch_at_startup: true,
    disable_windows_sign_out: false,
  };

  function makeAccount(accountId: string, remainingSeconds: number): AccountAuth {
    return {
      token: "attendance-test-token",
      account: {
        id: accountId,
        username: accountId === "account-1" ? "alex" : "maria",
        remaining_seconds: remainingSeconds,
        points_balance_centi: 0,
        status: "active",
      },
      session: {
        id: `session-${accountId}`,
        account_id: accountId,
        username: accountId === "account-1" ? "alex" : "maria",
        pc_id: "pc-07",
        remaining_seconds: remainingSeconds,
        points_balance_centi: 0,
        claimable_guest_seconds: 0,
      },
    };
  }

  let accountAuth: AccountAuth | null = makeAccount("account-1", 3600);
  let accountId = "account-1";
  let offline = false;
  const attendanceRequestArgs: unknown[] = [];
  let heldAttendanceClaims = 0;
  let nextAttendanceClaimFailure: AttendanceClaimError | null = null;
  const pendingAttendanceClaimRejections: Array<() => void> = [];

  function monthSummary(
    earningMonth: string,
    state: "earning" | "claimable" | "claimed" | "expired" | "forfeited" | null = null,
    values: Partial<{
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
      latest_claim_on: string | null;
    }> = {},
  ) {
    return {
      earning_month: earningMonth,
      state,
      check_in_count: 0,
      earned_seconds: 0,
      claimed_seconds: 0,
      outstanding_seconds: 0,
      claimable_seconds: 0,
      expired_seconds: 0,
      forfeited_seconds: 0,
      claim_starts_at: null,
      deadline_at_exclusive: null,
      claim_starts_on: null,
      deadline_date_inclusive: null,
      claim_count: 0,
      latest_claim_at: null,
      latest_claim_on: null,
      ...values,
    };
  }

  const calendarMonths = Array.from({ length: 12 }, (_, index) => {
    const date = new Date(Date.UTC(2026, 8 - index, 1));
    const key = `${date.getUTCFullYear()}-${String(date.getUTCMonth() + 1).padStart(2, "0")}`;
    if (key === "2026-08") {
      return monthSummary(key, "claimable", {
        check_in_count: 2,
        earned_seconds: 5400,
        claimed_seconds: 1800,
        outstanding_seconds: 3600,
        claimable_seconds: 3600,
        claim_starts_at: "2026-09-01T00:00:00Z",
        deadline_at_exclusive: "2026-10-01T00:00:00Z",
        claim_starts_on: "2026-09-01",
        deadline_date_inclusive: "2026-09-30",
        claim_count: 1,
        latest_claim_at: "2026-09-02T18:00:00Z",
        latest_claim_on: "2026-09-03",
      });
    }
    if (key === "2026-07") {
      return monthSummary(key, "expired", {
        check_in_count: 1,
        earned_seconds: 3600,
        expired_seconds: 3600,
        claim_starts_at: "2026-08-01T00:00:00Z",
        deadline_at_exclusive: "2026-09-01T00:00:00Z",
        claim_starts_on: "2026-08-01",
        deadline_date_inclusive: "2026-08-31",
      });
    }
    if (key === "2026-09") {
      return monthSummary(key, "earning", {
        check_in_count: 2,
        earned_seconds: 7200,
        outstanding_seconds: 7200,
        claim_starts_at: "2026-10-01T00:00:00Z",
        deadline_at_exclusive: "2026-11-01T00:00:00Z",
        claim_starts_on: "2026-10-01",
        deadline_date_inclusive: "2026-10-31",
      });
    }
    return monthSummary(key);
  });

  const olderClaimableMonths = [
    monthSummary("2025-09", "claimable", {
      check_in_count: 1,
      earned_seconds: 1800,
      outstanding_seconds: 1800,
      claimable_seconds: 1800,
      claim_starts_at: "2025-10-01T00:00:00Z",
      deadline_at_exclusive: "2025-11-01T00:00:00Z",
      claim_starts_on: "2025-10-01",
      deadline_date_inclusive: "2025-10-31",
    }),
  ];

  let attendanceSummary: AttendanceSummary = {
    enabled: true,
    today: {
      earning_date: "2026-09-26",
      status: "in_progress" as const,
      capped_cents: 400,
      daily_threshold_cents: 1000,
      reward_seconds: 3600,
    },
    totals: {
      earned_seconds: 18000,
      claimed_seconds: 1800,
      outstanding_seconds: 12600,
      claimable_seconds: 5400,
      expired_seconds: 3600,
      forfeited_seconds: 0,
    },
    calendar_months: calendarMonths,
    older_claimable_months: olderClaimableMonths,
    as_of: "2026-09-26T12:00:00Z",
    admin_local_date: "2026-09-26",
    admin_local_month: "2026-09",
  };

  function makeMonth(earningMonth: string) {
    const [year, month] = earningMonth.split("-").map(Number);
    const count = new Date(Date.UTC(year, month, 0)).getUTCDate();
    const today = earningMonth === "2026-09" ? 26 : -1;
    const days = Array.from({ length: count }, (_, index) => {
      const day = index + 1;
      const date = `${earningMonth}-${String(day).padStart(2, "0")}`;
      if (date === "2026-09-01") {
        return { earning_date: date, status: "checked_in" as const, capped_cents: 1000, daily_threshold_cents: 1000, reward_seconds: 3600 };
      }
      if (date === "2026-09-02") {
        return { earning_date: date, status: "missed" as const, capped_cents: null, daily_threshold_cents: null, reward_seconds: null };
      }
      if (date === "2026-09-03") {
        return { earning_date: date, status: "ineligible" as const, capped_cents: null, daily_threshold_cents: null, reward_seconds: null };
      }
      if (day === today) {
        return { earning_date: date, status: "today_progress" as const, capped_cents: 400, daily_threshold_cents: 1000, reward_seconds: 3600 };
      }
      return day > 26 && earningMonth === "2026-09"
        ? { earning_date: date, status: "future" as const, capped_cents: null, daily_threshold_cents: null, reward_seconds: null }
        : { earning_date: date, status: "ineligible" as const, capped_cents: null, daily_threshold_cents: null, reward_seconds: null };
    });
    const summary = attendanceSummary.calendar_months.find((item) => item.earning_month === earningMonth)
      ?? olderClaimableMonths.find((item) => item.earning_month === earningMonth)
      ?? monthSummary(earningMonth);
    return {
      ...summary,
      days,
      as_of: attendanceSummary.as_of,
      admin_local_date: attendanceSummary.admin_local_date,
      admin_local_month: attendanceSummary.admin_local_month,
    };
  }

  function refreshSummaryAfterClaim(earningMonth: string, requestId: string) {
    const month = attendanceSummary.calendar_months.find((item) => item.earning_month === earningMonth)
      ?? olderClaimableMonths.find((item) => item.earning_month === earningMonth);
    if (!month || month.claimable_seconds <= 0) throw new Error("No claimable month in fixture");
    const claimedSeconds = month.claimable_seconds;
    const updatedMonth = {
      ...month,
      state: "claimed" as const,
      claimed_seconds: month.claimed_seconds + claimedSeconds,
      outstanding_seconds: month.outstanding_seconds - claimedSeconds,
      claimable_seconds: 0,
      claim_count: month.claim_count + 1,
      latest_claim_at: "2026-09-26T12:01:00Z",
      latest_claim_on: "2026-09-26",
    };
    const calendarIndex = attendanceSummary.calendar_months.findIndex((item) => item.earning_month === earningMonth);
    if (calendarIndex >= 0) attendanceSummary.calendar_months[calendarIndex] = updatedMonth;
    const olderIndex = olderClaimableMonths.findIndex((item) => item.earning_month === earningMonth);
    if (olderIndex >= 0) olderClaimableMonths.splice(olderIndex, 1);
    const remainingClaimable = attendanceSummary.totals.claimable_seconds - claimedSeconds;
    attendanceSummary = {
      ...attendanceSummary,
      totals: {
        ...attendanceSummary.totals,
        claimed_seconds: attendanceSummary.totals.claimed_seconds + claimedSeconds,
        outstanding_seconds: attendanceSummary.totals.outstanding_seconds - claimedSeconds,
        claimable_seconds: remainingClaimable,
      },
    };
    const remainingSeconds = (accountAuth?.account.remaining_seconds ?? 0) + claimedSeconds;
    if (accountId) accountAuth = makeAccount(accountId, remainingSeconds);
    timer = { ...timer, remaining_seconds: remainingSeconds, expired: remainingSeconds <= 0 };
    return {
      claim: {
        audit_event_id: "event-attendance-claim-1",
        request_id: requestId,
        earning_month: earningMonth,
        claimed_seconds: claimedSeconds,
        created_at: "2026-09-26T12:01:00Z",
      },
      remaining_seconds: remainingSeconds,
      totals: attendanceSummary.totals,
      month: updatedMonth,
      as_of: "2026-09-26T12:01:00Z",
      admin_local_date: "2026-09-26",
      admin_local_month: "2026-09",
    };
  }

  async function restoreClaimableAugust(): Promise<void> {
    const earningMonth = "2026-08";
    const previousMonth = attendanceSummary.calendar_months.find(
      (month) => month.earning_month === earningMonth,
    );
    if (!previousMonth) throw new Error("Missing August Daily Reward fixture");
    const month = monthSummary(earningMonth, "claimable", {
      check_in_count: 2,
      earned_seconds: 5400,
      claimed_seconds: 1800,
      outstanding_seconds: 3600,
      claimable_seconds: 3600,
      claim_starts_at: "2026-09-01T00:00:00Z",
      deadline_at_exclusive: "2026-10-01T00:00:00Z",
      claim_starts_on: "2026-09-01",
      deadline_date_inclusive: "2026-09-30",
      claim_count: 1,
      latest_claim_at: "2026-09-02T18:00:00Z",
      latest_claim_on: "2026-09-03",
    });
    attendanceSummary = {
      ...attendanceSummary,
      calendar_months: attendanceSummary.calendar_months.map((item) =>
        item.earning_month === earningMonth ? month : item,
      ),
      totals: {
        ...attendanceSummary.totals,
        outstanding_seconds:
          attendanceSummary.totals.outstanding_seconds +
          month.outstanding_seconds -
          previousMonth.outstanding_seconds,
        claimable_seconds:
          attendanceSummary.totals.claimable_seconds +
          month.claimable_seconds -
          previousMonth.claimable_seconds,
      },
    };
    await emit("coin-accepted", {
      pc_id: timer.pc_id,
      seconds_added: 60,
      coin_promo_bonus_seconds: 0,
    });
    await tick();
  }

  const eventInternals = window as unknown as {
    __TAURI_EVENT_PLUGIN_INTERNALS__?: unknown;
  };
  eventInternals.__TAURI_EVENT_PLUGIN_INTERNALS__ ??= {};
  mockWindows("main");
  mockIPC((command, args) => {
    calls.push({ command, args });
    if (command === "get_client_config") return config;
    if (command === "is_client_configured") return true;
    if (command === "is_windows_platform") return false;
    if (command === "get_account_auth") return accountAuth;
    if (command === "get_timer_state") return timer;
    if (command === "get_shop_name") return "Harness Shop";
    if (command === "get_attendance_summary") {
      if (offline) throw new Error("Admin server unavailable");
      return attendanceSummary;
    }
    if (command === "get_attendance_month") {
      if (offline) throw new Error("Admin server unavailable");
      if (args && typeof args === "object" && "earningMonth" in args) {
        attendanceRequestArgs.push(args);
        return makeMonth(String(args.earningMonth));
      }
      throw new Error("Missing earningMonth");
    }
    if (command === "claim_attendance") {
      if (offline) throw new Error("Admin server unavailable");
      if (args && typeof args === "object" && "requestId" in args && "earningMonth" in args) {
        attendanceRequestArgs.push(args);
        if (nextAttendanceClaimFailure) {
          const failure = nextAttendanceClaimFailure;
          nextAttendanceClaimFailure = null;
          throw failure;
        }
        if (heldAttendanceClaims > 0) {
          heldAttendanceClaims -= 1;
          return new Promise<never>((_resolve, reject) => {
            pendingAttendanceClaimRejections.push(
              () => reject(new Error("Admin server unavailable")),
            );
          });
        }
        return refreshSummaryAfterClaim(String(args.earningMonth), String(args.requestId));
      }
      throw new Error("Missing claim arguments");
    }
    if (command === "get_redemption_tiers") return { version: 1, tiers: [] };
    if (command === "get_wheel_enabled") return false;
    if (command === "plugin:app|version") return "browser";
    if (
      command === "prepare_startup_handoff" ||
      command === "frontend_ready" ||
      command === "enter_fullscreen_mode" ||
      command === "enter_widget_mode" ||
      command === "enter_mini_mode" ||
      command === "set_kiosk_lock_active" ||
      command === "pause_offline_shutdown" ||
      command === "resume_offline_shutdown" ||
      command === "logout_account"
    ) {
      if (command === "logout_account") {
        accountId = "";
        accountAuth = null;
      }
      return null;
    }
    throw new Error(`Unexpected attendance harness command: ${command}`);
  }, { shouldMockEvents: true });

  const controller: HarnessController = {
    get calls() {
      return calls;
    },
    get attendanceRequestArgs() {
      return attendanceRequestArgs;
    },
    async setTimer(remainingSeconds) {
      const accountSeconds = Math.max(0, remainingSeconds);
      timer = {
        ...timer,
        remaining_seconds: remainingSeconds,
        expired: remainingSeconds <= 0,
      };
      if (accountAuth) accountAuth = makeAccount(accountId, accountSeconds);
      await emit("timer-updated", timer);
      await tick();
    },
    async acceptCoin(cappedCents) {
      attendanceSummary = {
        ...attendanceSummary,
        today: { ...attendanceSummary.today, capped_cents: cappedCents },
      };
      await emit("coin-accepted", {
        pc_id: timer.pc_id,
        seconds_added: 60,
        coin_promo_bonus_seconds: 0,
      });
      await tick();
    },
    async setAttendanceToday(enabled, status, cappedCents) {
      attendanceSummary = {
        ...attendanceSummary,
        enabled,
        today: {
          ...attendanceSummary.today,
          status,
          capped_cents: cappedCents,
        },
      };
      await emit("coin-accepted", {
        pc_id: timer.pc_id,
        seconds_added: 60,
        coin_promo_bonus_seconds: 0,
      });
      await tick();
    },
    async heartbeat() {
      await emit("timer-updated", timer);
      await tick();
    },
    async setAttendanceSupported(supported) {
      timer = { ...timer, attendance_supported: supported };
      await emit("timer-updated", timer);
      await tick();
    },
    setOffline(value) {
      offline = value;
    },
    holdNextAttendanceClaims(count) {
      heldAttendanceClaims = count;
    },
    async rejectNextAttendanceClaim() {
      const reject = pendingAttendanceClaimRejections.shift();
      if (!reject) throw new Error("No held Daily Reward claim to reject");
      reject();
      await tick();
    },
    failNextAttendanceClaim(error) {
      nextAttendanceClaimFailure = error;
    },
    restoreClaimableAugust,
    async setAccount(nextAccountId) {
      accountId = nextAccountId ?? "";
      accountAuth = nextAccountId ? makeAccount(nextAccountId, timer.remaining_seconds) : null;
      if (nextAccountId === "account-2") {
        attendanceSummary = {
          ...attendanceSummary,
          today: { ...attendanceSummary.today, capped_cents: 0, status: "in_progress" },
          totals: { ...attendanceSummary.totals, claimable_seconds: 0 },
          older_claimable_months: [],
        };
      }
      await emit("timer-updated", timer);
      await tick();
    },
  };
  (window as typeof window & { attendanceHarness?: HarnessController }).attendanceHarness = controller;
</script>

<div data-harness-ready>
  <App />
</div>

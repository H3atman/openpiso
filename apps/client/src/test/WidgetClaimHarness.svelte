<script lang="ts">
  import { mockIPC } from "@tauri-apps/api/mocks";
  import Widget from "../views/Widget.svelte";
  import type { AccountAuth, ClientConfig, TimerState } from "$lib/types";

  interface Controller {
    readonly claimCalls: number;
    readonly authRefreshes: number;
    readonly timerRefreshes: number;
    resolveClaim: () => void;
    rejectClaim: (message: string) => void;
  }

  const params = new URLSearchParams(window.location.search);
  const claimableSeconds = params.get("claimable") === "0" ? 0 : 300;
  const hasAccountSession = params.get("account") !== "0";
  let claimCalls = 0;
  let authRefreshes = 0;
  let timerRefreshes = 0;
  let resolveClaim: (() => void) | null = null;
  let rejectClaim: ((reason: Error) => void) | null = null;

  const timer: TimerState = {
    pc_id: "pc-07",
    pc_name: "PC 07",
    pc_status: "online",
    remaining_seconds: 600,
    guest_remaining_seconds: claimableSeconds,
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
    error: null,
  };
  const config: ClientConfig = {
    server_url: "http://127.0.0.1:3210",
    pc_id: "pc-07",
    view_mode: "widget",
    launch_at_startup: true,
    disable_windows_sign_out: false,
  };
  const activeAccountAuth: AccountAuth = {
    token: "harness-token",
    account: {
      id: "account-1",
      username: "alex",
      remaining_seconds: 600,
      points_balance_centi: 1_200,
      status: "active",
    },
    session: {
      id: "session-1",
      account_id: "account-1",
      username: "alex",
      pc_id: "pc-07",
      remaining_seconds: 600,
      points_balance_centi: 1_200,
      claimable_guest_seconds: claimableSeconds,
    },
  };
  const accountAuth = hasAccountSession ? activeAccountAuth : null;

  mockIPC((command, args) => {
    if (command === "get_redemption_tiers") {
      return { version: 1, tiers: [{ id: 1, points: 10, seconds: 14_400 }] };
    }
    if (command !== "claim_guest_time") {
      throw new Error(`Unexpected command: ${command}`);
    }
    claimCalls += 1;
    return new Promise((resolve, reject) => {
      resolveClaim = () => resolve(activeAccountAuth);
      rejectClaim = reject;
    });
  });

  const controller: Controller = {
    get claimCalls() { return claimCalls; },
    get authRefreshes() { return authRefreshes; },
    get timerRefreshes() { return timerRefreshes; },
    resolveClaim() {
      resolveClaim?.();
      resolveClaim = null;
      rejectClaim = null;
    },
    rejectClaim(message) {
      rejectClaim?.(new Error(message));
      resolveClaim = null;
      rejectClaim = null;
    },
  };

  (window as typeof window & { widgetClaimHarness?: Controller }).widgetClaimHarness = controller;
  const noop = (): void => {};
</script>

<div data-harness-ready>
  <Widget
    {timer}
    {config}
    {accountAuth}
    attendanceSummary={null}
    attendanceMonths={{}}
    attendanceStale={false}
    attendanceSupported={false}
    attendanceMonthLoading={null}
    attendanceMonthError=""
    attendanceClaimingMonth={null}
    attendanceClaimError=""
    onLoadAttendanceMonth={noop}
    onClaimAttendanceMonth={noop}
    shopName="Harness Shop"
    busy={false}
    error=""
    persistWarning=""
    onAuthChanged={() => { authRefreshes += 1; }}
    onAuthLoggedOut={noop}
    onTimerChanged={() => { timerRefreshes += 1; }}
    onConfigChanged={noop}
    onPersistWarning={noop}
    onPersistViewMode={noop}
    onPersistLaunchAtStartup={noop}
    onPersistWindowsSignOut={noop}
    windowsPlatform={true}
    onRequestAuthentication={noop}
  />
</div>

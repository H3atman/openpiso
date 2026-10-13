<script lang="ts">
  import { mockIPC } from "@tauri-apps/api/mocks";
  import Widget from "../views/Widget.svelte";
  import type { AccountAuth, ClientConfig, TimerState } from "$lib/types";

  let balanceCenti = $state(700);
  const accountAuth = $derived<AccountAuth>({
    token: "harness-token",
    account: { id: "account-1", username: "alex", remaining_seconds: 600, points_balance_centi: balanceCenti, status: "active" },
    session: { id: "session-1", account_id: "account-1", username: "alex", pc_id: "pc-07", remaining_seconds: 600, points_balance_centi: balanceCenti, claimable_guest_seconds: 0 },
  });
  const timer: TimerState = { pc_id: "pc-07", pc_name: "PC 07", pc_status: "online", remaining_seconds: 600, guest_remaining_seconds: 0, timer_mode: "account", active_username: "alex", connected: true, connection_status: "connected", expired: false, overlay_locked: false, power_state: "active", shutdown_in_seconds: null, offline_shutdown_in_seconds: null, shutdown_grace_seconds: 30, error: null };
  const config: ClientConfig = { server_url: "http://127.0.0.1:3210", pc_id: "pc-07", view_mode: "widget", launch_at_startup: true, disable_windows_sign_out: false };

  mockIPC((command) => {
    if (command === "get_redemption_tiers") return { version: 3, tiers: [{ id: 2, points: 10, seconds: 14_400 }, { id: 1, points: 5, seconds: 3_600 }] };
    if (command === "redeem_points") return accountAuth;
    throw new Error(`Unexpected command: ${command}`);
  });
  (window as typeof window & { widgetTierMenuHarness?: { setBalance: (value: number) => void } }).widgetTierMenuHarness = {
    setBalance(value) { balanceCenti = value; },
  };
  const noop = (): void => {};
</script>

<div data-harness-ready>
  <Widget {timer} {config} {accountAuth} attendanceSummary={null} attendanceMonths={{}} attendanceStale={false} attendanceSupported={false} attendanceMonthLoading={null} attendanceMonthError="" attendanceClaimingMonth={null} attendanceClaimError="" onLoadAttendanceMonth={noop} onClaimAttendanceMonth={noop} shopName="Harness Shop" busy={false} error="" persistWarning="" onAuthChanged={noop} onAuthLoggedOut={noop} onTimerChanged={noop} onConfigChanged={noop} onPersistWarning={noop} onPersistViewMode={noop} onPersistLaunchAtStartup={noop} onPersistWindowsSignOut={noop} windowsPlatform={true} onRequestAuthentication={noop} />
</div>

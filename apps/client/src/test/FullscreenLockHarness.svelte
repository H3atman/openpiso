<script lang="ts">
  import { mockIPC } from "@tauri-apps/api/mocks";
  import FullscreenLock from "../views/FullscreenLock.svelte";
  import type { AccountAuth, ClientConfig, TimerState } from "$lib/types";

  let authenticationSucceeded = false;

  mockIPC((command, args) => {
    if (command === "plugin:app|version") return "browser";
    if (command === "login_account") {
      authenticationSucceeded = true;
      return accountFixture;
    }
    if (command === "get_redemption_tiers") {
      return { version: 1, tiers: [{ id: 1, points: 10, seconds: 14_400 }] };
    }
    if (command === "get_wheel_enabled") return true;
    if (command === "pause_offline_shutdown" || command === "resume_offline_shutdown") return null;
    if (command === "verify_owner_pin") {
      return args !== undefined && !Array.isArray(args) && "pin" in args && args.pin === "1234";
    }
    throw new Error(`Unexpected browser harness command: ${command}`);
  });

  type LockState =
    | "setup"
    | "insert-coin"
    | "guest-session-ended"
    | "shutdown"
    | "session-ended"
    | "connecting"
    | "disconnected"
    | "offline-shutdown"
    | "unregistered"
    | "shop-closed";
  type AccountState = "guest" | "account";
  type ConfigState = "configured" | "fresh";

  const baseTimer: TimerState = {
    pc_id: "pc-07",
    pc_name: "PC 07",
    pc_status: "active",
    remaining_seconds: 0,
    guest_remaining_seconds: 0,
    timer_mode: "guest",
    active_username: null,
    connected: true,
    connection_status: "connected",
    expired: true,
    overlay_locked: false,
    power_state: "active",
    shutdown_in_seconds: null,
    offline_shutdown_in_seconds: null,
    shutdown_grace_seconds: 30,
    error: null,
  };

  const accountFixture: AccountAuth = {
    token: "browser-harness-token",
    account: {
      id: "account-1",
      username: "alex",
      remaining_seconds: 0,
      points_balance_centi: 1_200,
      status: "active",
    },
    session: {
      id: "session-1",
      account_id: "account-1",
      username: "alex",
      pc_id: "pc-07",
      remaining_seconds: 0,
      points_balance_centi: 1_200,
      claimable_guest_seconds: 300,
    },
  };

  const configuredFixture: ClientConfig = {
    server_url: "http://127.0.0.1:3210",
    pc_id: "pc-07",
    view_mode: "widget",
    launch_at_startup: true,
    disable_windows_sign_out: false,
  };

  const params = new URLSearchParams(window.location.search);
  const lockState = (params.get("lock") as LockState | null) ?? "insert-coin";
  const accountState = (params.get("account") as AccountState | null) ?? "guest";
  const configState = (params.get("config") as ConfigState | null) ?? "configured";

  const timer = $derived.by<TimerState | null>(() => {
    switch (lockState) {
      case "shutdown":
        return { ...baseTimer, power_state: "shutdown", shutdown_in_seconds: Number(params.get("countdown") ?? 24) };
      case "setup":
        return null;
      case "session-ended":
        return { ...baseTimer, overlay_locked: true };
      case "guest-session-ended":
        return {
          ...baseTimer,
          remaining_seconds: 180,
          guest_remaining_seconds: 180,
          expired: false,
          overlay_locked: true,
        };
      case "connecting":
        return { ...baseTimer, connected: false, connection_status: "connecting" };
      case "disconnected":
        return {
          ...baseTimer,
          connected: false,
          connection_status: "disconnected",
          error: "Admin server unavailable",
        };
      case "offline-shutdown":
        return {
          ...baseTimer,
          connected: false,
          connection_status: "disconnected",
          offline_shutdown_in_seconds: Number(params.get("countdown") ?? 90),
          error: "Admin server unavailable",
        };
      case "unregistered":
        return { ...baseTimer, connected: false, connection_status: "unregistered" };
      case "shop-closed":
        return {
          ...baseTimer,
          shop_status: {
            open: false,
            next_change_at: params.get("override")
              ? "2030-06-21T18:00:00Z"
              : "2030-06-21T08:00:00Z",
            reason: params.get("override") ? "override" : "schedule",
            last_call_active: false,
          },
        };
      case "insert-coin":
        return baseTimer;
    }
  });

  let accountAuth = $state<AccountAuth | null>(
    accountState === "account" ? accountFixture : null,
  );
  let authenticationRequested = $state(params.get("auth-route") === "true");
  const config = $derived(configState === "configured" ? configuredFixture : null);
  const configured = $derived(configState === "configured");

  function refreshAuth(): void {
    if (authenticationSucceeded) {
      accountAuth = accountFixture;
      authenticationRequested = false;
    }
  }
  function noop(): void {}
</script>

<FullscreenLock
  {timer}
  {accountAuth}
  {config}
  attendanceSummary={null}
  attendanceSupported={false}
  attendanceStale={false}
  attendanceClaimingMonth={null}
  attendanceClaimError=""
  onClaimAttendanceMonth={noop}
  {configured}
  shopName="Cyberia Pisonet"
  busy={false}
  error=""
  persistWarning=""
  onAuthChanged={refreshAuth}
  onAuthLoggedOut={noop}
  onTimerChanged={noop}
  onConfigChanged={noop}
  onPersistWarning={noop}
  onPersistViewMode={noop}
  onPersistLaunchAtStartup={noop}
  onPersistWindowsSignOut={noop}
  onContinueGuestSession={noop}
  windowsPlatform={true}
  onQuit={noop}
  devBypassAvailable={true}
  onDevBypassScreenRestriction={noop}
  {authenticationRequested}
  onReturnToSession={() => (authenticationRequested = false)}
/>

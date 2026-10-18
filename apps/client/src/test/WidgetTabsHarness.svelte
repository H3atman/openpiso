<script lang="ts">
  import { mockIPC } from "@tauri-apps/api/mocks";
  import { emit } from "@tauri-apps/api/event";
  import { tick } from "svelte";
  import Widget from "../views/Widget.svelte";
  import type {
    AccountAuth,
    ClientConfig,
    CoinFeedback,
    TimerState,
    ViewMode,
    WheelSpinResult,
  } from "$lib/types";

  interface Controller {
    readonly viewModes: ViewMode[];
    readonly calls: Record<string, number>;
    readonly logoutCalls: number;
    destroyWidget: () => Promise<void>;
    mountWidget: () => Promise<void>;
    enterFullscreenLock: () => Promise<void>;
    returnFromFullscreenLock: () => Promise<void>;
    restart: () => Promise<void>;
    resetViewModes: () => void;
    setPinDelay: (milliseconds: number) => void;
    setFlavor: (flavor: "account" | "guest") => Promise<void>;
    setWheelDelay: (milliseconds: number) => void;
    setWheelResult: (result: WheelSpinResult) => void;
    /** `settings` is what `/api/wheel/settings` reports; `server` is the
     *  password the mock server really demands (null = not enforced). They
     *  differ to model stale client settings (ADR-0088). */
    setWheelPassword: (config: { settings: boolean; server: string | null }) => void;
    /** While set, every spin is rejected as throttled for this many seconds. */
    setWheelThrottle: (seconds: number | null) => void;
    /** Password argument of each `spin_wheel` call, in order. */
    readonly spinPasswords: (string | undefined)[];
    /** Worst-case Time tab: last-call banner + coin-feedback line together. */
    setLastCall: (active: boolean) => Promise<void>;
    emitCoinFeedback: () => Promise<void>;
    readonly authenticationRequests: number;
    readonly refreshCalls: number;
  }

  let showWidget = $state(true);
  let flavor = $state<"account" | "guest">("account");
  const viewModes: ViewMode[] = [];
  let logoutCalls = 0;
  const calls: Record<string, number> = {};
  let pinDelayMs = 0;
  let authenticationRequests = 0;
  let refreshCalls = 0;
  let wheelDelayMs = 0;
  let wheelSettingsPasswordRequired = false;
  let wheelServerPassword: string | null = null;
  let wheelThrottleSeconds: number | null = null;
  const spinPasswords: (string | undefined)[] = [];
  const wheelSegments = [
    { label: "5 min", seconds: 300, points_centi: 0, free_spin: false },
    { label: "10 min", seconds: 600, points_centi: 0, free_spin: false },
    { label: "15 min", seconds: 900, points_centi: 0, free_spin: false },
    { label: "Try again", seconds: 0, points_centi: 0, free_spin: false },
    { label: "Free spin", seconds: 0, points_centi: 0, free_spin: true },
  ];
  let wheelSpinResult: WheelSpinResult = {
    seconds_won: 300,
    points_won_centi: 0,
    free_spins_triggered: 0,
    segments: wheelSegments,
    steps: [{ segment_index: 0, label: "5 minutes", seconds: 300, points_centi: 0, free_spin: false }],
  };
  let timer = $state<TimerState>({
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
    shop_status: null,
    error: null,
  });
  let config = $state<ClientConfig>({
    server_url: "http://127.0.0.1:3210",
    pc_id: "pc-07",
    view_mode: "widget",
    launch_at_startup: true,
    disable_windows_sign_out: false,
  });
  const accountAuth: AccountAuth = {
    token: "token",
    account: {
      id: "account-1",
      username: "alex",
      remaining_seconds: 3600,
      points_balance_centi: 2_000,
      status: "active",
    },
    session: {
      id: "session-1",
      account_id: "account-1",
      username: "alex",
      pc_id: "pc-07",
      remaining_seconds: 3600,
      points_balance_centi: 2_000,
      claimable_guest_seconds: 300,
    },
  };

  // `mockIPC` assigns `unregisterListener` onto `__TAURI_EVENT_PLUGIN_INTERNALS__`
  // without creating it, so enabling event mocking outside a real Tauri window
  // throws unless the global exists first.
  // Widened to `unknown` on purpose: the ambient type declares
  // `unregisterListener` as required, but `mockIPC` is what installs it.
  const eventInternals = window as unknown as {
    __TAURI_EVENT_PLUGIN_INTERNALS__?: unknown;
  };
  eventInternals.__TAURI_EVENT_PLUGIN_INTERNALS__ ??= {};

  // `shouldMockEvents` routes the `plugin:event|*` commands through the mock's
  // own listener registry, so `listen("coin-accepted")` in the Widget resolves
  // and `emit` below reaches it. Without it those invokes fall through to the
  // catch-all throw and the coin-feedback path is untestable.
  mockIPC((command, args) => {
    if (command === "get_redemption_tiers") {
      return { version: 1, tiers: [{ id: 1, points: 10, seconds: 14_400 }] };
    }
    if (command === "get_coin_promo_menu") {
      return {
        enabled: true,
        tiers: [
          { amount_cents: 500, bonus_seconds: 180 },
          { amount_cents: 1_000, bonus_seconds: 600 },
          { amount_cents: 2_000, bonus_seconds: 1_200 },
          { amount_cents: 5_000, bonus_seconds: 3_600 },
        ],
      };
    }
    calls[command] = (calls[command] ?? 0) + 1;
    if (command === "logout_account") {
      logoutCalls += 1;
      return null;
    }
    if (command === "claim_guest_time" || command === "redeem_voucher" || command === "redeem_points" || command === "transfer_time") {
      return accountAuth;
    }
    if (command === "get_wheel_enabled") return true;
    if (command === "get_wheel_settings") {
      return {
        enabled: true,
        free_spin_cap: 3,
        spin_duration: 0.05,
        version: 1,
        segments: wheelSegments,
        expected_payout_seconds: 219,
        expected_payout_points_centi: 0,
        point_value_seconds: 1440,
        house_edge_basis_points: 8478,
        unsafe_house_edge: false,
        password_confirmation_required: wheelSettingsPasswordRequired,
      };
    }
    if (command === "spin_wheel") {
      const password = args && typeof args === "object" && "password" in args && typeof args.password === "string"
        ? args.password
        : undefined;
      spinPasswords.push(password);
      // The Rust command rejects with the server's body text.
      if (wheelThrottleSeconds !== null) {
        return Promise.reject(`too many attempts; try again in ${wheelThrottleSeconds}s`);
      }
      if (wheelServerPassword !== null && password === undefined) {
        return Promise.reject(JSON.stringify({ error: "password_required" }));
      }
      if (wheelServerPassword !== null && password !== wheelServerPassword) {
        return Promise.reject(JSON.stringify({ error: "invalid_password" }));
      }
      return wheelDelayMs > 0
        ? new Promise<WheelSpinResult>((resolve) => setTimeout(() => resolve(wheelSpinResult), wheelDelayMs))
        : wheelSpinResult;
    }
    if (command === "request_points_transfer") return { id: "request-1", code: "123456" };
    if (command === "change_account_password") return null;
    if (command === "pause_offline_shutdown" || command === "resume_offline_shutdown") return null;
    if (command === "verify_owner_pin") {
      const verified = Boolean(
        args && typeof args === "object" && "pin" in args && args.pin === "1234",
      );
      return pinDelayMs > 0
        ? new Promise<boolean>((resolve) => setTimeout(() => resolve(verified), pinDelayMs))
        : verified;
    }
    throw new Error(`Unexpected command: ${command}`);
  }, { shouldMockEvents: true });

  async function setWidgetVisible(visible: boolean): Promise<void> {
    showWidget = visible;
    await tick();
  }

  const controller: Controller = {
    get calls() {
      return calls;
    },
    get viewModes() {
      return viewModes;
    },
    get logoutCalls() {
      return logoutCalls;
    },
    destroyWidget: () => setWidgetVisible(false),
    mountWidget: () => setWidgetVisible(true),
    enterFullscreenLock: () => setWidgetVisible(false),
    returnFromFullscreenLock: () => setWidgetVisible(true),
    async restart() {
      await setWidgetVisible(false);
      await setWidgetVisible(true);
    },
    resetViewModes() {
      viewModes.length = 0;
    },
    setPinDelay(milliseconds) {
      pinDelayMs = milliseconds;
    },
    async setFlavor(nextFlavor) {
      flavor = nextFlavor;
      await tick();
    },
    setWheelDelay(milliseconds) {
      wheelDelayMs = milliseconds;
    },
    setWheelResult(result) {
      wheelSpinResult = result;
    },
    setWheelPassword(config) {
      wheelSettingsPasswordRequired = config.settings;
      wheelServerPassword = config.server;
    },
    setWheelThrottle(seconds) {
      wheelThrottleSeconds = seconds;
    },
    get spinPasswords() {
      return spinPasswords;
    },
    async setLastCall(active) {
      timer = {
        ...timer,
        shop_status: active
          ? {
              open: true,
              next_change_at: "2026-08-03T23:00:00Z",
              reason: "schedule",
              last_call_active: true,
            }
          : null,
      };
      await tick();
    },
    async emitCoinFeedback() {
      const payload: CoinFeedback = {
        pc_id: "pc-07",
        seconds_added: 600,
        coin_promo_bonus_seconds: 120,
      };
      await emit("coin-accepted", payload);
      await tick();
    },
    get authenticationRequests() {
      return authenticationRequests;
    },
    get refreshCalls() {
      return refreshCalls;
    },
  };
  (
    window as typeof window & { widgetTabsHarness?: Controller }
  ).widgetTabsHarness = controller;
  const noop = (): void => {};
</script>

<div data-harness-ready>
  {#if showWidget}
    <Widget
      {timer}
      {config}
      accountAuth={flavor === "account" ? accountAuth : null}
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
      error="Persistent test error"
      persistWarning=""
      onAuthChanged={() => { refreshCalls += 1; }}
      onAuthLoggedOut={noop}
      onTimerChanged={() => { refreshCalls += 1; }}
      onConfigChanged={noop}
      onPersistWarning={noop}
      onPersistViewMode={(mode) => {
        viewModes.push(mode);
      }}
      onPersistLaunchAtStartup={(enabled) => {
        config = { ...config, launch_at_startup: enabled };
      }}
      onPersistWindowsSignOut={(enabled) => {
        config = { ...config, disable_windows_sign_out: enabled };
      }}
      windowsPlatform={true}
      onRequestAuthentication={() => {
        authenticationRequests += 1;
      }}
    />
  {/if}
</div>

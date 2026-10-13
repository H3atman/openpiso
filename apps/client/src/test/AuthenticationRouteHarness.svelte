<script lang="ts">
  import { onMount } from "svelte";
  import { mockIPC } from "@tauri-apps/api/mocks";
  import {
    selectClientView,
    shouldClearAuthenticationRequest,
  } from "$lib/selectors";
  import type { AccountAuth, ClientConfig, TimerState } from "$lib/types";
  import FullscreenLock from "../views/FullscreenLock.svelte";
  import Widget from "../views/Widget.svelte";

  const config: ClientConfig = {
    server_url: "http://127.0.0.1:3210",
    pc_id: "pc-07",
    view_mode: "widget",
    launch_at_startup: true,
    disable_windows_sign_out: false,
  };
  const accountFixture: AccountAuth = {
    token: "browser-harness-token",
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
      claimable_guest_seconds: 300,
    },
  };
  let timer = $state<TimerState>({
    pc_id: "pc-07",
    pc_name: "PC 07",
    pc_status: "active",
    remaining_seconds: 600,
    guest_remaining_seconds: 600,
    timer_mode: "guest",
    active_username: null,
    connected: true,
    connection_status: "connected",
    expired: false,
    overlay_locked: false,
    power_state: "active",
    shutdown_in_seconds: null,
    offline_shutdown_in_seconds: null,
    shutdown_grace_seconds: 30,
    error: null,
  });
  let accountAuth = $state<AccountAuth | null>(null);
  let authenticationRequested = $state(false);

  mockIPC((command) => {
    if (command === "plugin:app|version") return "browser";
    if (command === "login_account") {
      accountAuth = accountFixture;
      return accountFixture;
    }
    throw new Error(`Unexpected authentication route command: ${command}`);
  });

  const view = $derived(
    selectClientView(timer, accountAuth, config.view_mode, authenticationRequested),
  );

  $effect(() => {
    if (
      authenticationRequested &&
      shouldClearAuthenticationRequest(timer, accountAuth)
    ) {
      authenticationRequested = false;
    }
  });

  function applyAuthoritativeLock(): void {
    timer = { ...timer, overlay_locked: true };
  }
  function noop(): void {}

  onMount(() => {
    window.addEventListener("authoritative-lock", applyAuthoritativeLock);
    return () => window.removeEventListener("authoritative-lock", applyAuthoritativeLock);
  });
</script>

{#if view === "widget"}
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
    onAuthChanged={noop}
    onAuthLoggedOut={noop}
    onTimerChanged={noop}
    onConfigChanged={noop}
    onPersistWarning={noop}
    onPersistViewMode={noop}
    onPersistLaunchAtStartup={noop}
    onPersistWindowsSignOut={noop}
    windowsPlatform={true}
    onRequestAuthentication={() => (authenticationRequested = true)}
  />
{:else}
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
    configured={true}
    shopName="Harness Shop"
    busy={false}
    error=""
    persistWarning=""
    onAuthChanged={noop}
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
    devBypassAvailable={false}
    onDevBypassScreenRestriction={noop}
    {authenticationRequested}
    onReturnToSession={() => (authenticationRequested = false)}
  />
{/if}

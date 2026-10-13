<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import {
    enterFullscreenMode,
    enterMiniMode,
    enterWidgetMode,
    continueGuestSession,
    frontendReady,
    getAccountAuth,
    getAttendanceMonth,
    getAttendanceSummary,
    claimAttendance,
    getClientConfig,
    isWindowsPlatform,
    prepareStartupHandoff,
    getShopName,
    getTimerState,
    isClientConfigured,
    setClientConfig,
    setKioskLockActive,
    setLaunchAtStartup,
    setWindowsSignOut,
    devBypassScreenRestriction,
    quitClient,
  } from "$lib/api";
  import type {
    AccountAuth,
    AttendanceClaimError,
    AttendanceMonth,
    AttendanceSummary,
    ClientConfig,
    CoinFeedback,
    ClientView,
    TimerState,
    ViewMode,
  } from "$lib/types";
  import {
    selectClientView,
    shouldClearAuthenticationRequest,
  } from "$lib/selectors";
  import FullscreenLock from "./views/FullscreenLock.svelte";
  import Widget from "./views/Widget.svelte";
  import MiniBar from "./views/MiniBar.svelte";

  let timer = $state<TimerState | null>(null);
  let accountAuth = $state<AccountAuth | null>(null);
  let attendanceSummary = $state<AttendanceSummary | null>(null);
  let attendanceAccountId = $state<string | null>(null);
  let attendanceStale = $state(false);
  let attendanceMonths = $state<Record<string, AttendanceMonth>>({});
  let attendanceMonthLoading = $state<string | null>(null);
  let attendanceMonthError = $state("");
  let attendanceMonthRequest = 0;
  let attendanceClaimingMonth = $state<string | null>(null);
  let attendanceClaimError = $state("");
  let attendanceClaimRequestIds = $state<Record<string, string>>({});
  let attendanceClaimAttempt = 0;
  let attendanceSummaryRequest = 0;
  let lastAttendanceBootstrapKey = "";
  let config = $state<ClientConfig | null>(null);
  let configured = $state(true);
  let windowsPlatform = $state(false);
  let shopName = $state("OpenPiso Shop");
  let busy = $state(false);
  let error = $state("");
  let persistWarning = $state("");
  let devLockBypassed = $state(false);
  let mountedView = $state<ClientView>("lock");
  let authenticationRequested = $state(false);
  let lastAppliedDevLockBypass = false;

  let timerRefreshInFlight = false;

  function nextPaint(): Promise<void> {
    return new Promise((resolve) => requestAnimationFrame(() => resolve()));
  }

  async function revealPaintedClient(): Promise<void> {
    // requestAnimationFrame may pause while WebView2 is hidden. Rust first
    // shows Tauri behind the still-topmost native placeholder. Two visible
    // frames then give the Lock time to reach the compositor before handoff.
    await prepareStartupHandoff();
    await nextPaint();
    await nextPaint();
    await frontendReady();
  }

  async function refreshTimer(): Promise<void> {
    if (timerRefreshInFlight) return;
    timerRefreshInFlight = true;
    try {
      timer = await getTimerState();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      timerRefreshInFlight = false;
    }
  }

  async function refreshAuth(): Promise<void> {
    try {
      accountAuth = await getAccountAuth();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function refreshAttendanceSummary(): Promise<void> {
    const accountId = accountAuth?.account.id;
    if (
      !accountId ||
      accountId !== attendanceAccountId ||
      timer?.attendance_supported !== true
    ) {
      return;
    }

    const request = ++attendanceSummaryRequest;
    try {
      const summary = await getAttendanceSummary();
      if (
        request !== attendanceSummaryRequest ||
        accountAuth?.account.id !== accountId ||
        attendanceAccountId !== accountId
      ) {
        return;
      }
      attendanceSummary = summary;
      attendanceStale = false;
    } catch {
      if (
        request === attendanceSummaryRequest &&
        accountAuth?.account.id === accountId &&
        attendanceAccountId === accountId
      ) {
        attendanceStale = attendanceSummary !== null;
      }
    }
  }

  async function loadAttendanceMonth(earningMonth: string): Promise<void> {
    const accountId = accountAuth?.account.id;
    if (
      !accountId ||
      accountId !== attendanceAccountId ||
      timer?.attendance_supported !== true ||
      !attendanceSummary?.calendar_months.some(
        (month) => month.earning_month === earningMonth,
      )
    ) {
      return;
    }

    const request = ++attendanceMonthRequest;
    attendanceMonthLoading = earningMonth;
    attendanceMonthError = "";
    try {
      const month = await getAttendanceMonth(earningMonth);
      if (
        request !== attendanceMonthRequest ||
        accountAuth?.account.id !== accountId ||
        attendanceAccountId !== accountId
      ) {
        return;
      }
      attendanceMonths = { ...attendanceMonths, [earningMonth]: month };
    } catch {
      if (
        request === attendanceMonthRequest &&
        accountAuth?.account.id === accountId &&
        attendanceAccountId === accountId
      ) {
        attendanceMonthError = "Could not refresh Daily Reward days. Showing saved information.";
        attendanceStale = attendanceSummary !== null;
      }
    } finally {
      if (request === attendanceMonthRequest) attendanceMonthLoading = null;
    }
  }

  async function claimAttendanceMonth(earningMonth: string): Promise<void> {
    const accountId = accountAuth?.account.id;
    if (
      !accountId ||
      accountId !== attendanceAccountId ||
      timer?.attendance_supported !== true ||
      attendanceClaimingMonth !== null
    ) {
      return;
    }

    const request = ++attendanceClaimAttempt;
    attendanceClaimingMonth = earningMonth;
    attendanceClaimError = "";
    const requestId =
      attendanceClaimRequestIds[earningMonth] ?? crypto.randomUUID();
    attendanceClaimRequestIds = {
      ...attendanceClaimRequestIds,
      [earningMonth]: requestId,
    };
    try {
      const result = await claimAttendance(requestId, earningMonth);
      if (accountAuth?.account.id !== accountId) return;
      const completedRequestIds = { ...attendanceClaimRequestIds };
      delete completedRequestIds[earningMonth];
      attendanceClaimRequestIds = completedRequestIds;

      if (attendanceSummary) {
        attendanceSummary = {
          ...attendanceSummary,
          totals: result.totals,
          calendar_months: attendanceSummary.calendar_months.map((month) =>
            month.earning_month === earningMonth ? result.month : month,
          ),
          older_claimable_months: attendanceSummary.older_claimable_months.filter(
            (month) => month.earning_month !== earningMonth,
          ),
          as_of: result.as_of,
          admin_local_date: result.admin_local_date,
          admin_local_month: result.admin_local_month,
        };
      }
      const cachedMonth = attendanceMonths[earningMonth];
      if (cachedMonth) {
        attendanceMonths = {
          ...attendanceMonths,
          [earningMonth]: {
            ...cachedMonth,
            ...result.month,
            as_of: result.as_of,
            admin_local_date: result.admin_local_date,
            admin_local_month: result.admin_local_month,
          },
        };
      }
      await refreshAuth();
      await refreshTimer();
      await refreshAttendanceSummary();
    } catch (caught) {
      const details =
        typeof caught === "object" && caught !== null
          ? (caught as Partial<AttendanceClaimError>)
          : {};
      if (details.status_code === 400 || details.status_code === 409) {
        const failedRequestIds = { ...attendanceClaimRequestIds };
        delete failedRequestIds[earningMonth];
        attendanceClaimRequestIds = failedRequestIds;
      }
      if (accountAuth?.account.id === accountId) {
        attendanceClaimError =
          typeof details.error === "string" && typeof details.message === "string"
            ? details.message
            : "Could not claim this Daily Reward. Refresh and try again.";
      }
    } finally {
      if (request === attendanceClaimAttempt) attendanceClaimingMonth = null;
    }
  }

  $effect(() => {
    const accountId = accountAuth?.account.id ?? null;
    const supported = timer?.attendance_supported === true;
    if (accountId !== attendanceAccountId) {
      ++attendanceSummaryRequest;
      attendanceAccountId = accountId;
      attendanceSummary = null;
      attendanceStale = false;
      ++attendanceMonthRequest;
      attendanceMonths = {};
      attendanceMonthLoading = null;
      attendanceMonthError = "";
      ++attendanceClaimAttempt;
      attendanceClaimingMonth = null;
      attendanceClaimError = "";
      attendanceClaimRequestIds = {};
    }

    const key = accountId && supported ? `${accountId}:supported` : "";
    if (key && key !== lastAttendanceBootstrapKey) {
      lastAttendanceBootstrapKey = key;
      void refreshAttendanceSummary();
    } else if (!key && lastAttendanceBootstrapKey) {
      lastAttendanceBootstrapKey = "";
    }
  });

  const currentAttendanceSummary = $derived(
    accountAuth?.account.id === attendanceAccountId ? attendanceSummary : null,
  );

  async function refreshShopName(): Promise<void> {
    try {
      shopName = await getShopName();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function refreshConfig(): Promise<void> {
    try {
      config = await getClientConfig();
      configured = await isClientConfigured();
      windowsPlatform = await isWindowsPlatform();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    }
  }

  const view = $derived<ClientView>(
    selectClientView(
      timer,
      accountAuth,
      config?.view_mode ?? "widget",
      authenticationRequested,
    ),
  );

  $effect(() => {
    if (
      authenticationRequested &&
      shouldClearAuthenticationRequest(timer, accountAuth)
    ) {
      authenticationRequested = false;
    }
  });

  $effect(() => {
    const nextView = view;
    if (nextView !== "lock" && devLockBypassed) {
      // The dev bypass is a one-shot escape from the *current* Lock; once
      // the client leaves the Lock it must re-arm, or every later Lock
      // entry silently skips the fullscreen transition (bug #11).
      devLockBypassed = false;
    }
    const bypassLock = devLockBypassed && nextView === "lock";
    if (mountedView === nextView && lastAppliedDevLockBypass === bypassLock) {
      return;
    }
    mountedView = nextView;
    lastAppliedDevLockBypass = bypassLock;
    void setKioskLockActive(nextView === "lock" && !bypassLock);
    if (nextView === "lock") {
      if (!bypassLock) {
        enterFullscreenMode().catch((caught) => {
          error = `Failed to enter fullscreen lock: ${caught instanceof Error ? caught.message : String(caught)}`;
        });
      }
    } else if (nextView === "widget") {
      void enterWidgetMode();
    } else {
      void enterMiniMode();
    }
  });

  async function saveConfig(
    serverUrl: string,
    nextViewMode?: ViewMode,
  ): Promise<void> {
    busy = true;
    try {
      const response = await setClientConfig(serverUrl, nextViewMode);
      config = response.config;
      configured = true;
      persistWarning = response.persist_warning ?? "";
      error = "";
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  function onAuthResolvedAsLoggedOut(): void {
    authenticationRequested = false;
    accountAuth = null;
  }

  async function persistViewModeChange(next: ViewMode): Promise<void> {
    if (!config) return;
    if (config.view_mode === next) return;
    await saveConfig(config.server_url, next);
  }

  async function persistWindowsSignOut(enabled: boolean): Promise<void> {
    busy = true;
    try {
      const response = await setWindowsSignOut(enabled);
      config = response.config;
      persistWarning = response.persist_warning ?? "";
      error = "";
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  async function resumeGuestSession(): Promise<void> {
    try {
      await continueGuestSession();
      await refreshTimer();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function persistLaunchAtStartup(enabled: boolean): Promise<void> {
    busy = true;
    try {
      const response = await setLaunchAtStartup(enabled);
      config = response.config;
      persistWarning = response.persist_warning ?? "";
      error = "";
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  let unlistenTimer: UnlistenFn | undefined;
  let unlistenCoinAccepted: UnlistenFn | undefined;
  let unlistenFocus: UnlistenFn | undefined;
  let mounted = true;

  function reassertLockOnBlur(focused: boolean): void {
    if (focused) return;
    if (view !== "lock") return;
    if (devLockBypassed) return;
    void enterFullscreenMode();
  }

  async function bypassDevScreenRestriction(): Promise<void> {
    try {
      await devBypassScreenRestriction();
      devLockBypassed = true;
      error = "";
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function suppressContextMenu(event: MouseEvent): void {
    event.preventDefault();
  }

  onMount(() => {
    void revealPaintedClient().catch((caught) => {
      error = `Failed to reveal Client window: ${caught instanceof Error ? caught.message : String(caught)}`;
    });

    void (async () => {
      try {
        await refreshConfig();
        await refreshAuth();
        await refreshShopName();
        await refreshTimer();
      } catch (caught) {
        error = caught instanceof Error ? caught.message : String(caught);
      }
    })();

    void listen<TimerState>("timer-updated", (event) => {
      timer = event.payload;
      error = "";
      void refreshAuth();
      void refreshShopName();
    }).then((cleanup) => {
      if (mounted) {
        unlistenTimer = cleanup;
      } else {
        cleanup();
      }
    });

    void listen<CoinFeedback>("coin-accepted", (event) => {
      if (!accountAuth || event.payload.pc_id !== accountAuth.session.pc_id) return;
      void refreshAttendanceSummary();
    }).then((cleanup) => {
      if (mounted) {
        unlistenCoinAccepted = cleanup;
      } else {
        cleanup();
      }
    });

    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => reassertLockOnBlur(focused))
      .then((cleanup) => {
        if (mounted) {
          unlistenFocus = cleanup;
        } else {
          cleanup();
        }
      });

    const timerPoll = window.setInterval(() => void refreshTimer(), 250);
    const attendancePoll = window.setInterval(
      () => void refreshAttendanceSummary(),
      30_000,
    );
    onDestroy(() => {
      window.clearInterval(timerPoll);
      window.clearInterval(attendancePoll);
    });
  });

  onDestroy(() => {
    mounted = false;
    unlistenTimer?.();
    unlistenCoinAccepted?.();
    unlistenFocus?.();
  });
</script>

<svelte:window oncontextmenu={suppressContextMenu} />

{#if view === "widget"}
  <Widget
    {timer}
    {config}
    {accountAuth}
    attendanceSummary={currentAttendanceSummary}
    {attendanceMonths}
    {attendanceStale}
    attendanceSupported={timer?.attendance_supported === true}
    {attendanceMonthLoading}
    {attendanceMonthError}
    {attendanceClaimingMonth}
    {attendanceClaimError}
    onLoadAttendanceMonth={loadAttendanceMonth}
    onClaimAttendanceMonth={claimAttendanceMonth}
    {shopName}
    {busy}
    {error}
    {persistWarning}
    onAuthChanged={() => refreshAuth()}
    onAuthLoggedOut={onAuthResolvedAsLoggedOut}
    onTimerChanged={() => void refreshTimer()}
    onConfigChanged={(next) => (config = next)}
    onPersistWarning={(warning) => (persistWarning = warning)}
    onPersistViewMode={persistViewModeChange}
    onPersistLaunchAtStartup={persistLaunchAtStartup}
    onPersistWindowsSignOut={persistWindowsSignOut}
    {windowsPlatform}
    onRequestAuthentication={() => (authenticationRequested = true)}
  />
{:else if view === "mini"}
  <MiniBar
    {timer}
    onExpand={() => void persistViewModeChange("widget")}
  />
{:else}
  <FullscreenLock
    {timer}
    {accountAuth}
    {config}
    attendanceSummary={currentAttendanceSummary}
    attendanceSupported={timer?.attendance_supported === true}
    {attendanceStale}
    {attendanceClaimingMonth}
    {attendanceClaimError}
    onClaimAttendanceMonth={claimAttendanceMonth}
    {configured}
    {shopName}
    {busy}
    {error}
    onAuthChanged={() => refreshAuth()}
    onAuthLoggedOut={onAuthResolvedAsLoggedOut}
    onTimerChanged={() => void refreshTimer()}
    onConfigChanged={(next) => {
      config = next;
      configured = true;
    }}
    persistWarning={persistWarning}
    onPersistWarning={(warning) => (persistWarning = warning)}
    onPersistViewMode={persistViewModeChange}
    onPersistLaunchAtStartup={persistLaunchAtStartup}
    onPersistWindowsSignOut={persistWindowsSignOut}
    onContinueGuestSession={resumeGuestSession}
    {windowsPlatform}
    onQuit={() => quitClient()}
    devBypassAvailable={import.meta.env.DEV}
    onDevBypassScreenRestriction={bypassDevScreenRestriction}
    {authenticationRequested}
    onReturnToSession={() => (authenticationRequested = false)}
  />
{/if}

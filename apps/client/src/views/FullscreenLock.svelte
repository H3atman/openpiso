<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { Button } from "$lib/components/ui/button";
  import type { AccountAuth, AttendanceSummary, ClientConfig, RedemptionTierMenu, ShopStatus, TimerState, ViewMode } from "$lib/types";
  import { formatShopClosed } from "$lib/format";
  import { selectLockSurface, type LockSurfaceLeft } from "$lib/selectors";
  import { accountLoginErrorMessage, claimGuestTime, connectToServer, disconnectFromServer, getWheelEnabled, loginAccount, logoutAccount, pauseOfflineShutdown, redeemPoints, redeemVoucher, resumeOfflineShutdown, setClientConfig, signUpAccount } from "$lib/api";
  import { isStaleMenuError, loadSortedTierMenu } from "$lib/redemption";
  import AccountPanel from "$lib/components/AccountPanel.svelte";
  import { createSpinMachine } from "$lib/machines/spin.svelte";
  import WheelDialog from "$lib/components/WheelDialog.svelte";
  import WheelEntry from "$lib/components/WheelEntry.svelte";
  import AuthForm from "$lib/components/AuthForm.svelte";
  import Banner from "$lib/components/Banner.svelte";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import OwnerPinGate from "$lib/components/OwnerPinGate.svelte";
  import SettingsForm from "$lib/components/SettingsForm.svelte";
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";

  interface Props {
    timer: TimerState | null; accountAuth: AccountAuth | null; config: ClientConfig | null;
    attendanceSummary: AttendanceSummary | null; attendanceSupported: boolean; attendanceStale: boolean;
    attendanceClaimingMonth: string | null; attendanceClaimError: string;
    onClaimAttendanceMonth: (earningMonth: string) => Promise<void> | void;
    configured: boolean; shopName: string; busy: boolean; error: string; persistWarning: string;
    onAuthChanged: () => Promise<void> | void; onAuthLoggedOut: () => void;
    onTimerChanged: () => Promise<void> | void; onConfigChanged: (next: ClientConfig) => void;
    onPersistWarning: (warning: string) => void; onPersistViewMode: (mode: ViewMode) => Promise<void> | void;
    onPersistLaunchAtStartup: (enabled: boolean) => Promise<void> | void;
    onPersistWindowsSignOut: (enabled: boolean) => Promise<void> | void;
    onContinueGuestSession: () => Promise<void> | void;
    onQuit: () => Promise<void> | void; windowsPlatform?: boolean; devBypassAvailable: boolean;
    onDevBypassScreenRestriction: () => Promise<void> | void;
    authenticationRequested: boolean;
    onReturnToSession: () => void;
  }
  let { timer, accountAuth, config, attendanceSummary, attendanceSupported, attendanceStale,
    attendanceClaimingMonth, attendanceClaimError, onClaimAttendanceMonth, configured, shopName, busy, error, persistWarning, onAuthChanged,
    onAuthLoggedOut, onTimerChanged, onConfigChanged, onPersistWarning, onPersistViewMode,
    onPersistLaunchAtStartup, onPersistWindowsSignOut, onContinueGuestSession, onQuit,
    windowsPlatform = false, devBypassAvailable, onDevBypassScreenRestriction, authenticationRequested,
    onReturnToSession }: Props = $props();

  // -- Wheel state -------------------------------------------------------
  let wheelEnabled = $state(false);
  // Shared with the Widget (ADR-0089).
  const spin = createSpinMachine({
    refresh: () => void refreshAccountAndTimer(),
    onEnabled: (enabled) => (wheelEnabled = enabled),
    onError: (message) => (actionError = message),
  });
  let redemptionTierMenu = $state<RedemptionTierMenu | null>(null);
  let redemptionTiersLoading = $state(false);
  let redemptionTiersError = $state("");
  let redemptionTiersSessionId = $state("");
  let authError = $state(""); let actionError = $state(""); let switchAccount = $state(false);
  let settingsOpen = $state(false); let ownerUnlocked = $state(false); let appVersion = $state("");
  let continuingGuestSession = $state(false);
  const surface = $derived(selectLockSurface(timer, configured, accountAuth));
  const shopStatus = $derived<ShopStatus | null>(timer?.shop_status ?? null);
  const shopClosed = $derived(Boolean(timer?.connected && shopStatus && !shopStatus.open));
  const closedGuidance = $derived(formatShopClosed(shopStatus));
  const pcName = $derived(timer?.pc_name || config?.pc_id || "This PC");
  const countdown = $derived(surface.left === "offline_shutdown"
    ? Math.max(0, timer?.offline_shutdown_in_seconds ?? 0)
    : Math.max(0, timer?.shutdown_in_seconds ?? 0));
  const guestSessionPaused = $derived(surface.left === "session_ended"
    && timer?.timer_mode === "guest" && timer.guest_remaining_seconds > 0);
  const unavailableReason = $derived(!configured ? "Authentication unavailable until this PC is configured."
    : surface.left === "connecting" ? "Authentication unavailable while connecting to the admin server."
    : surface.left === "disconnected" || surface.left === "offline_shutdown" ? "Authentication unavailable while the admin server is offline."
    : surface.left === "unregistered" ? "Authentication unavailable until the owner registers this PC."
    : "");
  const copy: Record<LockSurfaceLeft, { label: string; title: string; guidance: string }> = {
    setup: { label: "Setup required", title: "Set up this PC", guidance: "Enter the admin server URL to connect this PC." },
    unregistered: { label: "Unregistered", title: "This PC isn't registered", guidance: "Ask the owner to add it in web-admin (Settings → PCs). Once registered, the next heartbeat will bind this machine." },
    connecting: { label: "Connecting", title: "Reaching the admin server", guidance: "Please wait while this PC connects." },
    disconnected: { label: "Disconnected", title: "Can't reach server", guidance: "Check the admin server URL and that the server is running." },
    offline_shutdown: { label: "Offline shutdown", title: "Server unreachable — this PC will shut down after", guidance: "The admin server is unreachable. Open Settings below to fix the connection; reconnect before the countdown ends to resume this session." },
    shutdown: { label: "Shutting down", title: "This PC will shut down after", guidance: "Insert coins at the OpenPiso terminal to keep this PC on." },
    session_ended: { label: "Session ended", title: "Time expired", guidance: "Please insert coins at the OpenPiso terminal." },
    shop_closed: { label: "Shop Closed", title: "Shop Closed", guidance: "New logins, signups, vouchers, and coins are unavailable until the shop opens." },
    insert_coin: { label: "Awaiting payment", title: "Insert coin", guidance: "Insert coins at the OpenPiso terminal to add time to this PC." },
  };
  const currentCopy = $derived(surface.left === "session_ended" && guestSessionPaused
    ? { label: "Session ended", title: "Guest session paused", guidance: "Your Guest Time is still available on this PC." }
    : copy[surface.left]);

  async function resumeGuestSession(): Promise<void> {
    if (continuingGuestSession) return;
    continuingGuestSession = true;
    try {
      await onContinueGuestSession();
    } finally {
      continuingGuestSession = false;
    }
  }

  async function authenticate(mode: "login" | "signup", username: string, password: string): Promise<void> {
    if (unavailableReason) return; authError = "";
    try { await (mode === "signup" ? signUpAccount(username, password) : loginAccount(username, password)); await refreshAccountAndTimer(); switchAccount = false; }
    catch (caught) { authError = mode === "login" ? accountLoginErrorMessage(caught) : caught instanceof Error ? caught.message : String(caught); }
  }
  function presentActionError(caught: unknown): void { actionError = caught instanceof Error ? caught.message : String(caught); }
  async function refreshAccountAndTimer(): Promise<void> { await onAuthChanged(); await onTimerChanged(); }
  async function loadRedemptionTiers(): Promise<void> {
    redemptionTiersLoading = true;
    redemptionTiersError = "";
    try {
      redemptionTierMenu = await loadSortedTierMenu();
    } catch (caught) {
      redemptionTiersError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      redemptionTiersLoading = false;
    }
  }
  $effect(() => {
    const sessionId = accountAuth?.session.id ?? "";
    if (sessionId === redemptionTiersSessionId) return;
    redemptionTiersSessionId = sessionId;
    redemptionTierMenu = null;
    if (sessionId) void loadRedemptionTiers();
  });
  async function claim(): Promise<void> { actionError = ""; try { await claimGuestTime(); await refreshAccountAndTimer(); } catch (caught) { presentActionError(caught); } }
  async function voucher(code: string): Promise<boolean> { actionError = ""; try { await redeemVoucher(code); await refreshAccountAndTimer(); return true; } catch (caught) { presentActionError(caught); return false; } }
  async function points(tierId: number, version: number): Promise<void> { actionError = ""; try { await redeemPoints(tierId, version); await refreshAccountAndTimer(); } catch (caught) { if (isStaleMenuError(caught)) { await loadRedemptionTiers(); actionError = "Redemption Tiers changed. Menu refreshed; choose again."; } else { presentActionError(caught); } } }
  async function refreshWheelEnabled(): Promise<void> {
    try {
      wheelEnabled = await getWheelEnabled();
    } catch {
      wheelEnabled = false;
    }
  }

  // Re-fetch on identity changes and poll so owner promotion toggles propagate.
  $effect(() => {
    const _ = accountAuth?.session.id;
    void refreshWheelEnabled();
  });

  async function logout(): Promise<void> { actionError = ""; try { await logoutAccount(); onAuthLoggedOut(); await refreshAccountAndTimer(); } catch (caught) { presentActionError(caught); } }
  async function saveAndConnect(serverUrl: string): Promise<void> { const response = await setClientConfig(serverUrl, config?.view_mode ?? "widget"); onConfigChanged(response.config); onPersistWarning(response.persist_warning ?? ""); await connectToServer(); await onTimerChanged(); }
  async function disconnect(): Promise<void> { await disconnectFromServer(); await onTimerChanged(); }
  async function closeSettings(): Promise<void> {
    settingsOpen = false;
    ownerUnlocked = false;
    await resumeOfflineShutdown();
  }

  async function unlockSettings(): Promise<void> {
    await pauseOfflineShutdown();
    ownerUnlocked = true;
  }
  onMount(() => {
    void getVersion().then((value) => appVersion = value).catch(() => appVersion = "browser");
    const interval = setInterval(() => void refreshWheelEnabled(), 5_000);
    return () => clearInterval(interval);
  });
  onDestroy(() => { void resumeOfflineShutdown(); });
</script>

<svelte:window onkeydown={(event) => { if (event.key === "Escape" && settingsOpen) closeSettings(); }} />
<main class="lock-surface relative min-h-screen w-full overflow-x-hidden text-foreground lg:grid lg:grid-cols-[minmax(0,3fr)_minmax(22rem,2fr)]">
  <section class="flex min-h-[52vh] flex-col justify-between px-6 py-7 sm:px-10 lg:min-h-screen lg:px-12 lg:py-10">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div class="flex min-w-0 items-center gap-3">
        <BrandMark class="size-6 sm:size-8" />
        <h1 class="break-words text-4xl font-black tracking-tight sm:text-6xl">{shopName}</h1>
      </div>
      <p class="inline-flex rounded border border-border bg-muted/50 px-3 py-1.5 font-mono text-base sm:text-lg">{pcName}</p>
    </div>
    <div class="flex flex-col items-center py-10 text-center">
      <span class="rounded-md border border-primary/60 bg-primary/10 px-3 py-1.5 text-xs font-bold tracking-[.2em] text-primary uppercase dark:text-chart-2">● {currentCopy.label}</span>
      <p class="mt-5 text-xs font-bold tracking-[.3em] text-primary/80 uppercase dark:text-chart-2/80">{currentCopy.title}</p>
      {#if surface.left === "shutdown" || surface.left === "offline_shutdown"}
        <strong class:text-destructive={countdown <= 10} class:text-foreground={countdown > 10} class="mt-3 font-mono text-[clamp(7rem,24vw,15rem)] leading-none tabular-nums" role="timer" aria-live="assertive" aria-label={`${countdown} seconds until shutdown`}>{String(countdown).padStart(2, "0")}</strong>
      {:else}<h2 class="mt-4 max-w-2xl text-4xl font-black sm:text-5xl">{currentCopy.title}</h2>{/if}
      <p class="mt-5 max-w-xl text-base text-muted-foreground sm:text-lg">{surface.left === "shop_closed" ? closedGuidance : currentCopy.guidance}</p>
      {#if guestSessionPaused}
        <Button type="button" class="mt-6" disabled={continuingGuestSession || busy} onclick={resumeGuestSession}>Continue Guest session</Button>
      {/if}
    </div>
    <div></div>
  </section>

  <aside class="flex min-h-0 flex-col border-t border-primary/20 bg-primary/5 px-6 py-7 sm:px-10 lg:min-h-screen lg:border-t-0 lg:border-l dark:bg-primary/10">
    <div class="flex items-center justify-between text-xs font-bold uppercase">
      <span class={timer?.connected ? "text-emerald-600 dark:text-emerald-300" : "text-amber-600 dark:text-amber-300"}>● {timer?.connected ? "Connected" : "Offline"}</span>
      {#if devBypassAvailable}<Button type="button" variant="outline" size="sm" onclick={onDevBypassScreenRestriction}>Bypass lock</Button>{/if}
    </div>
    {#if error}<div class="mt-4"><Banner kind="error">{error}</Banner></div>{/if}
    <div class="flex flex-1 flex-col justify-start gap-6 py-8 lg:justify-center">
      {#if accountAuth}
        <AccountPanel {accountAuth} {busy} error={actionError} {shopClosed} {attendanceSummary} {attendanceSupported} {attendanceStale} {attendanceClaimingMonth} {attendanceClaimError} {onClaimAttendanceMonth} {redemptionTierMenu} {redemptionTiersLoading} {redemptionTiersError} {wheelEnabled} onClaim={claim} onRedeemVoucher={voucher} onRefreshRedemptionTiers={loadRedemptionTiers} onRedeemPoints={points} onOpenWheel={spin.open} onLogout={logout} onAuthenticate={authenticate} onSwitchMode={() => authError = ""} />
        {#if !shopClosed && switchAccount}<div class="rounded-lg border border-primary/30 p-4"><div class="mb-3 flex flex-wrap items-start justify-between gap-2"><div><h3 class="font-bold">Switch account</h3><p class="mt-1 text-sm text-amber-700 dark:text-amber-200">The current Active Session will be replaced when you log in or sign up.</p></div><Button type="button" variant="ghost" onclick={() => { switchAccount = false; authError = ""; }}>Cancel</Button></div><AuthForm {busy} error={authError} onAuthenticate={authenticate} onSwitchMode={() => authError = ""} /></div>
        {:else if !shopClosed}<Button type="button" variant="outline" onclick={() => switchAccount = true}>Switch account</Button>{/if}
      {:else if shopClosed}
        <div class="rounded-lg border border-primary/30 bg-primary/5 p-5 text-center">
          <h2 class="text-3xl font-bold">Shop Closed</h2>
          <p class="mt-3 text-muted-foreground">{closedGuidance}</p>
        </div>
      {:else}
        <div><h2 class="text-3xl font-bold">Sign in to start</h2><p class="mt-2 text-muted-foreground">Keep Remaining Time portable across PCs.</p></div>
        {#if authenticationRequested}
          <Button type="button" variant="outline" onclick={onReturnToSession}>
            Return to Guest session
          </Button>
        {/if}
        <AuthForm {busy} error={authError} disabled={Boolean(unavailableReason)} disabledReason={unavailableReason} onAuthenticate={authenticate} onSwitchMode={() => authError = ""} />
        <div class="flex items-center gap-3 text-xs font-bold tracking-widest text-muted-foreground/60 uppercase"><span class="h-px flex-1 bg-border"></span>or<span class="h-px flex-1 bg-border"></span></div>
        <p class="text-center text-sm text-muted-foreground">Select <span class="font-mono text-foreground">{pcName}</span>, insert coin to login as guest</p>
        <WheelEntry {accountAuth} {busy} {wheelEnabled} onOpen={spin.open} variant="card" />
      {/if}
    </div>
    <footer class="flex items-end justify-between gap-4 text-[.65rem] text-muted-foreground/70">
      <!-- Customer-facing: never print the admin server address here. The owner
           still reads it in SettingsPanel, behind the PIN gate. Connection state
           already has its own surface, so the version alone earns this slot. -->
      <span class="min-w-0 break-all">Client {appVersion}{configured ? "" : " · No server configured"}</span>
      <button type="button" onclick={() => settingsOpen = true} aria-label="Settings" class="shrink-0 rounded-lg border border-border p-2 text-lg">⚙</button>
    </footer>
  </aside>
</main>

{#if settingsOpen}
  <div class="fixed inset-0 z-50 overflow-y-auto bg-black/75 p-4" role="dialog" aria-modal="true" aria-label="PC settings">
    <div class="mx-auto my-4 w-full max-w-md rounded-xl bg-card p-6 text-card-foreground">
      <div class="mb-5 flex justify-between"><div><h2 class="text-2xl font-bold">PC settings</h2><p class="text-sm text-muted-foreground">Owner configuration</p></div><button type="button" aria-label="Close" onclick={closeSettings}>✕</button></div>
      {#if !configured}
        <SettingsForm serverUrl={config?.server_url ?? "http://127.0.0.1:3210"} connected={false} {busy} onSaveAndConnect={saveAndConnect} onDisconnect={disconnect} showDisconnect={false} />
      {:else if ownerUnlocked}
        <SettingsPanel serverUrl={config?.server_url ?? "http://127.0.0.1:3210"} pcId={config?.pc_id ?? null} connected={timer?.connected ?? false} {busy} viewMode={config?.view_mode ?? "widget"} launchAtStartup={config?.launch_at_startup ?? true} disableWindowsSignOut={config?.disable_windows_sign_out ?? false} {windowsPlatform} onSaveAndConnect={saveAndConnect} onDisconnect={disconnect} onSetViewMode={onPersistViewMode} onSetLaunchAtStartup={onPersistLaunchAtStartup} onSetWindowsSignOut={onPersistWindowsSignOut} {onQuit} />
      {:else}<OwnerPinGate onUnlock={unlockSettings} onCancel={closeSettings} />{/if}
      {#if persistWarning}<div class="mt-4"><Banner kind="warning">{persistWarning}</Banner></div>{/if}
    </div>
  </div>
{/if}

<WheelDialog
  spinState={spin.state}
  pointsBalanceCenti={accountAuth?.account.points_balance_centi ?? 0}
  onSpin={spin.spin}
  onSubmitPassword={spin.submitPassword}
  onNextFreeSpin={spin.nextFreeSpin}
  onClose={spin.close}
/>

<style>
  .lock-surface {
    background:
      radial-gradient(ellipse 60% 50% at 80% 90%, color-mix(in oklab, var(--primary) 14%, transparent), transparent 65%),
      radial-gradient(ellipse 50% 40% at 20% 10%, color-mix(in oklab, var(--primary) 9%, transparent), transparent 60%),
      var(--background);
  }
  :global(.dark) .lock-surface {
    background:
      radial-gradient(ellipse 60% 50% at 80% 90%, color-mix(in oklab, var(--primary) 42%, transparent), transparent 65%),
      radial-gradient(ellipse 50% 40% at 20% 10%, color-mix(in oklab, var(--primary) 28%, transparent), transparent 60%),
      var(--background);
  }
</style>

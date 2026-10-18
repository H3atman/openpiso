<!--
  Widget — the 320×480 floating Client view (ADR-0029 §4.1 / §8.2,
  size amended for bug #12 — dialogs must fit; was 200×300).

  Mounted by App.svelte's state machine when the customer has an active
  Account session or usable Guest Time. Two flavors:

  - **Guest flavor**: Time + Owner-PIN-gated Settings tabs, inline Central
    Terminal guidance, and persistent "Minimize" + "Log in" actions.
  - **Account flavor**: Time, Points, Account, and Owner-PIN-gated Settings
    tabs with persistent "Minimize" and "Log out" actions. Transactional
    self-service flows use the shared Widget overlay.
-->
<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Card, CardContent } from "$lib/components/ui/card";
  import { Separator } from "$lib/components/ui/separator";
  import {
    formatCoinCredit,
    formatCoinPromoTier,
    formatDuration,
    formatPcNumber,
    formatProseDuration,
    formatShopClosingTime,
    formatPoints,
  } from "$lib/format";
  import type {
    AccountAuth,
    AttendanceMonth,
    AttendanceSummary,
    ClientConfig,
    CoinFeedback,
    CoinPromoMenu,
    CoinPromoMenuTier,
    RedemptionTier,
    RedemptionTierMenu,
    TimerState,
    ViewMode,
  } from "$lib/types";
  import {
    cheapestTierShortfallCenti,
    isStaleMenuError,
    loadSortedTierMenu,
  } from "$lib/redemption";
  import {
    changeAccountPassword,
    claimGuestTime,
    connectToServer,
    disconnectFromServer,
    logoutAccount,
    pauseOfflineShutdown,
    quitClient,
    resumeOfflineShutdown,
    redeemPoints,
    redeemVoucher,
    requestPointsTransfer,
    getCoinPromoMenu,
    getWheelEnabled,
    setClientConfig,
    transferTime,
  } from "$lib/api";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import RedeemTiersDialog from "$lib/components/RedeemTiersDialog.svelte";
  import TransferTimeDialog from "$lib/components/TransferTimeDialog.svelte";
  import TransferPointsDialog from "$lib/components/TransferPointsDialog.svelte";
  import ChangePasswordDialog from "$lib/components/ChangePasswordDialog.svelte";

  import OwnerPinGate from "$lib/components/OwnerPinGate.svelte";
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";
  import { createSpinMachine } from "$lib/machines/spin.svelte";
  import WheelDialog from "$lib/components/WheelDialog.svelte";
  import WheelEntry from "$lib/components/WheelEntry.svelte";
  import WidgetOverlay from "$lib/components/WidgetOverlay.svelte";
  import DailyRewardStrip from "$lib/components/DailyRewardStrip.svelte";
  import DailyRewardDialog from "$lib/components/DailyRewardDialog.svelte";

  interface Props {
    timer: TimerState | null;
    config: ClientConfig | null;
    accountAuth: AccountAuth | null;
    attendanceSummary: AttendanceSummary | null;
    attendanceMonths: Record<string, AttendanceMonth>;
    attendanceStale: boolean;
    attendanceSupported: boolean;
    attendanceMonthLoading: string | null;
    attendanceMonthError: string;
    attendanceClaimingMonth: string | null;
    attendanceClaimError: string;
    onLoadAttendanceMonth: (earningMonth: string) => Promise<void> | void;
    onClaimAttendanceMonth: (earningMonth: string) => Promise<void> | void;
    shopName: string;
    busy: boolean;
    error: string;
    persistWarning: string;
    onAuthChanged: () => Promise<void> | void;
    onAuthLoggedOut: () => void;
    onTimerChanged: () => Promise<void> | void;
    onConfigChanged: (next: ClientConfig) => void;
    onPersistWarning: (warning: string) => void;
    onPersistViewMode: (mode: ViewMode) => Promise<void> | void;
    onPersistLaunchAtStartup: (enabled: boolean) => Promise<void> | void;
    onPersistWindowsSignOut: (enabled: boolean) => Promise<void> | void;
    windowsPlatform?: boolean;
    onRequestAuthentication: () => void;
  }

  let {
    timer,
    config,
    accountAuth,
    attendanceSummary,
    attendanceMonths,
    attendanceStale,
    attendanceSupported,
    attendanceMonthLoading,
    attendanceMonthError,
    attendanceClaimingMonth,
    attendanceClaimError,
    onLoadAttendanceMonth,
    onClaimAttendanceMonth,
    shopName,
    busy,
    error,
    persistWarning,
    onAuthChanged,
    onAuthLoggedOut,
    onTimerChanged,
    onConfigChanged,
    onPersistWarning,
    onPersistViewMode,
    onPersistLaunchAtStartup,
    onPersistWindowsSignOut,
    windowsPlatform = false,
    onRequestAuthentication,
  }: Props = $props();

  type CustomerTab = "time" | "points" | "account";
  type WidgetTab = CustomerTab | "settings";

  const accountTabs: { id: WidgetTab; label: string }[] = [
    { id: "time", label: "Time" },
    { id: "points", label: "Points" },
    { id: "account", label: "Account" },
    { id: "settings", label: "Settings" },
  ];
  const guestTabs: { id: WidgetTab; label: string }[] = [
    { id: "time", label: "Time" },
    { id: "settings", label: "Settings" },
  ];
  const activeTabs = $derived(accountAuth ? accountTabs : guestTabs);
  let selectedTab = $state<WidgetTab>("time");
  let previousCustomerTab = $state<CustomerTab>("time");
  let activeSessionId = $state("");
  let ownerUnlocked = $state(false);
  let ownerGateEpoch = $state(0);
  let unlockOwnerSettings = $state<() => Promise<void> | void>(() => {});

  // -- Wheel state -------------------------------------------------------
  let wheelEnabled = $state(false);
  // Shared with the Fullscreen Lock (ADR-0089).
  const spin = createSpinMachine({
    refresh: () => {
      void onAuthChanged();
      void onTimerChanged();
    },
    onEnabled: (enabled) => (wheelEnabled = enabled),
    onError: (message) => (localError = message),
  });

  let voucherCode = $state("");
  // The Coin Promo ladder is a collapsed disclosure so the Time tab fits the
  // Widget in one page in every state (ADR-0029 amendment 2026-08-03).
  let promoLadderOpen = $state(false);
  let coinFeedback = $state<CoinFeedback | null>(null);
  let coinFeedbackTimer: ReturnType<typeof setTimeout> | undefined;
  let coinPromoMenu = $state<CoinPromoMenu | null>(null);
  let tierMenu = $state<RedemptionTierMenu | null>(null);
  let tiersLoading = $state(false);
  let tiersError = $state("");
  let tiersSessionId = $state("");
  let redeemOpen = $state(false);
  let redemptionBusy = $state(false);
  let localError = $state("");
  let claimOpen = $state(false);
  let claimBusy = $state(false);
  let claimError = $state("");
  let claimSeconds = $state(0);
  let attendanceOpen = $state(false);
  let selectedAttendanceMonth = $state("");
  const selectedAttendanceMonthDetails = $derived(
    attendanceMonths[selectedAttendanceMonth] ?? null,
  );

  function openDailyReward(): void {
    if (!accountAuth || !attendanceSupported || !attendanceSummary) return;
    selectedAttendanceMonth = attendanceSummary.admin_local_month;
    attendanceOpen = true;
    void onLoadAttendanceMonth(selectedAttendanceMonth);
  }

  function selectDailyRewardMonth(earningMonth: string): void {
    if (
      !attendanceSummary?.calendar_months.some(
        (month) => month.earning_month === earningMonth,
      )
    ) {
      return;
    }
    selectedAttendanceMonth = earningMonth;
    void onLoadAttendanceMonth(earningMonth);
  }

  // Account self-service dialog state.
  let transferTimeOpen = $state(false);
  let transferTimeBusy = $state(false);
  let transferPointsOpen = $state(false);
  let transferPointsBusy = $state(false);
  let changePasswordOpen = $state(false);
  let changePasswordBusy = $state(false);

  const remaining = $derived(timer?.remaining_seconds ?? 0);
  const terminalPcNumber = $derived(formatPcNumber(timer?.pc_id));
  const shopShort = $derived(
    shopName.length > 24 ? `${shopName.slice(0, 22)}…` : shopName,
  );
  const pointsBalanceCenti = $derived(accountAuth?.account.points_balance_centi ?? 0);
  const cheapestShortfallCenti = $derived(
    cheapestTierShortfallCenti(tierMenu, pointsBalanceCenti),
  );
  // Headline for the collapsed ladder: the tier with the largest raw bonus,
  // always shown with its coin amount so the bonus is never quoted without
  // the coin that buys it. Deliberately *not* the glossary's "most generous"
  // row, which is defined by Effective Bonus %; on the escalating ladder
  // ADR-0052 argues for, the two coincide.
  const largestCoinPromoBonusTier = $derived(
    (coinPromoMenu?.tiers ?? []).reduce<CoinPromoMenuTier | null>(
      (best, tier) =>
        best === null || tier.bonus_seconds > best.bonus_seconds ? tier : best,
      null,
    ),
  );

  // Flavor/session transitions always start at Time. Tab state is transient,
  // and an Account logout with Guest Time can keep this component mounted.
  $effect(() => {
    const sessionId = accountAuth?.session.id ?? "";
    if (sessionId === activeSessionId) return;
    activeSessionId = sessionId;
    selectedTab = "time";
    previousCustomerTab = "time";
    ++ownerGateEpoch;
    ownerUnlocked = false;
    claimOpen = false;
    attendanceOpen = false;
    selectedAttendanceMonth = "";
    redeemOpen = false;
    promoLadderOpen = false;
    transferTimeOpen = false;
    transferPointsOpen = false;
    changePasswordOpen = false;
  });

  async function refreshWheelEnabled(): Promise<void> {
    try {
      wheelEnabled = await getWheelEnabled();
    } catch {
      wheelEnabled = false;
    }
  }

  async function refreshCoinPromoMenu(): Promise<void> {
    if (!accountAuth) {
      coinPromoMenu = null;
      return;
    }
    try {
      coinPromoMenu = await getCoinPromoMenu();
    } catch {
      coinPromoMenu = null;
    }
  }

  // Re-fetch on identity changes and poll so owner promotion toggles propagate.
  $effect(() => {
    const _ = accountAuth?.session.id;
    void refreshWheelEnabled();
    void refreshCoinPromoMenu();
  });
  onMount(() => {
    const interval = setInterval(() => {
      void refreshWheelEnabled();
      void refreshCoinPromoMenu();
    }, 5_000);
    let destroyed = false;
    let unlisten: (() => void) | undefined;
    void listen<CoinFeedback>("coin-accepted", (event) => {
      if (!accountAuth || event.payload.pc_id !== accountAuth.session.pc_id) return;
      coinFeedback = event.payload;
      if (coinFeedbackTimer) clearTimeout(coinFeedbackTimer);
      coinFeedbackTimer = setTimeout(() => (coinFeedback = null), 8_000);
    }).then((cleanup) => {
      if (destroyed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      destroyed = true;
      clearInterval(interval);
      if (coinFeedbackTimer) clearTimeout(coinFeedbackTimer);
      unlisten?.();
    };
  });

  async function loadRedemptionTiers(): Promise<void> {
    tiersLoading = true;
    tiersError = "";
    try {
      tierMenu = await loadSortedTierMenu();
    } catch (caught) {
      tiersError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      tiersLoading = false;
    }
  }

  $effect(() => {
    const sessionId = accountAuth?.session.id ?? "";
    if (sessionId === tiersSessionId) return;
    tiersSessionId = sessionId;
    tierMenu = null;
    if (sessionId) void loadRedemptionTiers();
  });

  function openClaimConfirmation(): void {
    if (!accountAuth || accountAuth.session.claimable_guest_seconds <= 0)
      return;
    claimSeconds = accountAuth.session.claimable_guest_seconds;
    claimError = "";
    claimOpen = true;
  }

  function closeClaimConfirmation(): void {
    if (claimBusy) return;
    claimOpen = false;
    claimError = "";
  }

  $effect(() => {
    if (!claimOpen || claimBusy) return;
    const currentClaimable = accountAuth?.session.claimable_guest_seconds ?? 0;
    if (currentClaimable <= 0) {
      claimOpen = false;
      return;
    }
    claimSeconds = currentClaimable;
  });

  async function claimTime(): Promise<void> {
    if (claimBusy) return;
    claimBusy = true;
    claimError = "";
    try {
      await claimGuestTime();
      localError = "";
      await onAuthChanged();
      await onTimerChanged();
      claimOpen = false;
    } catch (caught) {
      claimError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      claimBusy = false;
    }
  }

  async function submitVoucher(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (busy || !voucherCode) return;
    busy = true;
    try {
      await redeemVoucher(voucherCode);
      voucherCode = "";
      localError = "";
      await onAuthChanged();
      await onTimerChanged();
    } catch (caught) {
      localError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  async function handleRedeem(tier: RedemptionTier): Promise<void> {
    if (!tierMenu || redemptionBusy) return;
    redemptionBusy = true;
    try {
      await redeemPoints(tier.id, tierMenu.version);
      localError = "";
      redeemOpen = false;
      await onAuthChanged();
      await onTimerChanged();
    } catch (caught) {
      if (isStaleMenuError(caught)) {
        await loadRedemptionTiers();
        throw new Error("Redemption Tiers changed. Menu refreshed; choose again.");
      }
      throw caught;
    } finally {
      redemptionBusy = false;
    }
  }



  async function logout(): Promise<void> {
    if (busy) return;
    ++ownerGateEpoch;
    void resumeOfflineShutdown();
    ownerUnlocked = false;
    busy = true;
    try {
      await logoutAccount();
      localError = "";
      onAuthLoggedOut();
      await onAuthChanged();
      await onTimerChanged();
    } catch (caught) {
      localError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  function minimizeToMiniBar(): void {
    ++ownerGateEpoch;
    void resumeOfflineShutdown();
    ownerUnlocked = false;
    void onPersistViewMode("mini");
  }

  function selectTab(tab: WidgetTab): void {
    if (tab === "settings") {
      if (selectedTab !== "settings") {
        previousCustomerTab = selectedTab;
        const epoch = ++ownerGateEpoch;
        unlockOwnerSettings = async () => {
          await pauseOfflineShutdown();
          if (selectedTab === "settings" && ownerGateEpoch === epoch) {
            ownerUnlocked = true;
          } else {
            await resumeOfflineShutdown();
          }
        };
      }
    } else {
      ++ownerGateEpoch;
      void resumeOfflineShutdown();
      ownerUnlocked = false;
    }
    selectedTab = tab;
  }

  function cancelOwnerPin(): void {
    ++ownerGateEpoch;
    void resumeOfflineShutdown();
    ownerUnlocked = false;
    selectedTab = previousCustomerTab;
  }

  onDestroy(() => {
    void resumeOfflineShutdown();
  });

  function handleTabKeydown(event: KeyboardEvent): void {
    const currentIndex = activeTabs.findIndex((tab) => tab.id === selectedTab);
    const nextIndex =
      event.key === "ArrowRight"
        ? (currentIndex + 1) % activeTabs.length
        : event.key === "ArrowLeft"
          ? (currentIndex - 1 + activeTabs.length) % activeTabs.length
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? activeTabs.length - 1
              : null;
    if (nextIndex === null) return;
    event.preventDefault();
    selectTab(activeTabs[nextIndex].id);
    document.getElementById(`widget-${selectedTab}-tab`)?.focus();
  }

  async function saveAndConnect(serverUrl: string): Promise<void> {
    const response = await setClientConfig(
      serverUrl,
      config?.view_mode ?? "widget",
    );
    onConfigChanged(response.config);
    onPersistWarning(response.persist_warning ?? "");
    await connectToServer();
    await onTimerChanged();
  }

  async function disconnect(): Promise<void> {
    await disconnectFromServer();
    await onTimerChanged();
  }

  async function setDefaultViewMode(mode: ViewMode): Promise<void> {
    if (!config) return;
    const response = await setClientConfig(
      config?.server_url ?? "http://127.0.0.1:3210",
      mode,
    );
    onConfigChanged(response.config);
    onPersistWarning(response.persist_warning ?? "");
  }

  async function quit(): Promise<void> {
    await quitClient();
  }

  // Account self-service handlers.

  async function handleTransferTime(
    toUsername: string,
    seconds: number,
  ): Promise<void> {
    if (transferTimeBusy) return;
    transferTimeBusy = true;
    try {
      await transferTime(toUsername, seconds);
      localError = "";
      transferTimeOpen = false;
      await onAuthChanged();
      await onTimerChanged();
    } finally {
      transferTimeBusy = false;
    }
  }

  async function handleRequestPointsTransfer(
    toUsername: string,
    pointsCenti: number,
  ): Promise<string> {
    if (transferPointsBusy) {
      throw new Error("Points transfer already in progress.");
    }
    transferPointsBusy = true;
    try {
      const result = await requestPointsTransfer(toUsername, pointsCenti);
      localError = "";
      await onAuthChanged();
      return result.code;
    } finally {
      transferPointsBusy = false;
    }
  }

  async function handleChangePassword(
    oldPassword: string,
    newPassword: string,
  ): Promise<void> {
    changePasswordBusy = true;
    try {
      await changeAccountPassword(oldPassword, newPassword);
      localError = "";
    } finally {
      changePasswordBusy = false;
    }
  }
</script>

<section
  class="bg-card text-card-foreground flex h-[480px] w-[320px] flex-col overflow-hidden rounded-md border text-xs"
  aria-label="OpenPiso workstation widget"
>
  <header
    class="flex items-center justify-between gap-2 border-b px-3 py-2 text-[0.65rem] tracking-widest uppercase"
    data-tauri-drag-region
  >
    <div class="flex min-w-0 items-center gap-1.5" data-tauri-drag-region>
      <BrandMark class="size-5" />
      <span class="text-primary truncate" data-tauri-drag-region title={shopName}
        >{shopShort}</span
      >
    </div>
    <span
      class="bg-primary/10 text-primary rounded-full px-2 py-0.5 font-bold"
      data-tauri-drag-region>{timer?.pc_id ?? "—"}</span
    >
  </header>
  {#if timer?.shop_status?.last_call_active}
    <p
      class="border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300 mx-3 mt-2 rounded-md border px-2 py-1 text-center text-[0.65rem] font-semibold"
      role="status"
      aria-live="polite"
    >
      Closing at {formatShopClosingTime(timer.shop_status.next_change_at)} — last chance to insert coins.
    </p>
  {/if}
  <div class="min-h-5 px-3 pt-1">
    {#if localError || error || timer?.error}<p
        class="text-destructive text-[0.65rem]"
        role="alert"
        aria-live="assertive"
      >
        {localError || error || timer?.error}
      </p>{/if}
  </div>
  {#if accountAuth && coinFeedback}
    <p class="bg-primary/10 text-primary mx-3 my-2 rounded-md px-2 py-1 text-center text-xs font-semibold" data-coin-feedback aria-live="polite">
      {formatCoinCredit(coinFeedback.seconds_added, coinFeedback.coin_promo_bonus_seconds)}
    </p>
  {/if}
  <div
    class="grid border-b {accountAuth ? 'grid-cols-4' : 'grid-cols-2'}"
    aria-label={accountAuth ? "Account sections" : "Guest sections"}
    role="tablist"
  >
    {#each activeTabs as tab (tab.id)}
        <button
          type="button"
          role="tab"
          aria-selected={selectedTab === tab.id}
          tabindex={selectedTab === tab.id ? 0 : -1}
          aria-controls={selectedTab === tab.id
            ? `widget-${tab.id}-panel`
            : undefined}
          id={`widget-${tab.id}-tab`}
          class="py-2 text-[0.65rem] font-semibold tracking-wider uppercase {selectedTab ===
          tab.id
            ? 'text-primary border-primary border-b-2'
            : 'text-muted-foreground hover:text-foreground'}"
          onclick={() => selectTab(tab.id)}
          onkeydown={handleTabKeydown}>{tab.label}</button
        >
      {/each}
    </div>
    <div
      class="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden p-3"
      data-widget-tab-panel
      role="tabpanel"
      id={`widget-${selectedTab}-panel`}
      aria-labelledby={`widget-${selectedTab}-tab`}
    >
      {#if selectedTab === "time"}
        <!--
          The timer frame flexes instead of reserving a constant 200px, so
          the Time tab is one page in every state; it steps down to a smaller
          floor while the Coin Promo ladder is expanded, restoring on
          collapse (ADR-0029 amendment 2026-08-03).
        -->
        <div
          class="mx-auto flex w-full flex-1 flex-col items-center justify-center {promoLadderOpen
            ? 'min-h-[44px]'
            : 'min-h-[86px]'}"
          data-time-frame
        >
          <strong
            class="font-mono font-bold tabular-nums {promoLadderOpen
              ? 'text-3xl'
              : 'text-6xl'}">{formatDuration(remaining)}</strong
          ><span
            class="text-muted-foreground text-[0.6rem] tracking-widest uppercase"
            >{accountAuth ? "Remaining Time" : "Guest Time"}</span
          >
        </div>
        {#if accountAuth && attendanceSupported && attendanceSummary}
          <div class="mt-2 shrink-0">
            <DailyRewardStrip
              summary={attendanceSummary}
              stale={attendanceStale}
              onOpen={openDailyReward}
            />
          </div>
        {/if}
        {#if accountAuth}
          {#if coinPromoMenu?.enabled && coinPromoMenu.tiers.length > 0}
            <details
              class="border-primary/30 bg-primary/5 mt-2 flex min-h-0 flex-col rounded-md border p-2"
              data-coin-promo-ladder
              open={promoLadderOpen}
              ontoggle={(event) =>
                (promoLadderOpen = event.currentTarget.open)}
            >
              <summary
                class="text-primary cursor-pointer text-[0.65rem] font-bold tracking-widest uppercase"
              >
                Coin Promo{largestCoinPromoBonusTier
                  ? ` — ${formatCoinPromoTier(largestCoinPromoBonusTier)}`
                  : ""}
              </summary>
              <!--
                The ladder body is the scroll container, never the page: an
                owner may configure more tiers than the squeezed worst case
                can show.
              -->
              <div
                class="mt-1 max-h-[104px] min-h-0 flex-1 overflow-y-auto"
                data-coin-promo-body
              >
                <div class="grid grid-cols-2 gap-x-2 gap-y-0.5 text-[0.65rem]">
                  {#each coinPromoMenu.tiers as tier (tier.amount_cents)}
                    <span>{formatCoinPromoTier(tier)}</span>
                  {/each}
                </div>
              </div>
            </details>
          {/if}
          <div class="mt-3 flex shrink-0 flex-col gap-2">
            {#if accountAuth.session.claimable_guest_seconds > 0}<Button
                type="button"
                size="sm"
                disabled={busy || claimBusy}
                onclick={openClaimConfirmation}
                >Claim {formatProseDuration(
                  accountAuth.session.claimable_guest_seconds,
                )}</Button
              >{/if}
            <form
              class="flex flex-col gap-1"
              onsubmit={(event) => void submitVoucher(event)}
            >
              <Input
                aria-label="Voucher code"
                bind:value={voucherCode}
                maxlength={64}
                placeholder="Voucher code"
                required
              /><Button type="submit" size="sm" variant="outline" disabled={busy}
                >Redeem voucher</Button
              >
            </form>
          </div>
        {:else}
          <div class="mt-3 flex shrink-0 flex-col gap-1.5">
            <p class="text-muted-foreground text-[0.65rem]">
              Guest session — your time stays on this PC.
            </p>
            <p>Use the <strong>OpenPiso Kiosk</strong> to add time.</p>
            <p class="text-muted-foreground text-[0.65rem]">
              Enter <strong>{terminalPcNumber}</strong>, press Enter, then Insert Coin.
              Time is added instantly.
            </p>
            <WheelEntry {accountAuth} {busy} {wheelEnabled} onOpen={() => void spin.open()} />
          </div>
        {/if}
      {:else if selectedTab === "points" && accountAuth}
        <div class="flex flex-col items-center gap-0.5 py-2">
          <p
            class="text-muted-foreground text-[0.65rem] tracking-widest uppercase"
          >
            Points balance
          </p>
          <div class="flex items-baseline gap-1.5">
            <strong class="font-mono text-5xl tabular-nums"
              >{formatPoints(accountAuth.account.points_balance_centi)}</strong
            ><span class="text-muted-foreground">pts</span>
          </div>
          {#if cheapestShortfallCenti > 0}
            <p class="text-muted-foreground text-[0.65rem]" data-tier-nudge>
              {formatPoints(cheapestShortfallCenti)} more points to unlock the cheapest tier
            </p>
          {/if}
        </div>
        <div class="mt-2 flex flex-col gap-1">
          <Button
            type="button"
            size="sm"
            data-claim-time
            disabled={busy}
            onclick={() => (redeemOpen = true)}>Claim Time</Button
          >
        </div>
        <div class="mt-3 flex flex-col gap-1">
          <WheelEntry {accountAuth} {busy} {wheelEnabled} onOpen={() => void spin.open()} />
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy || accountAuth.account.points_balance_centi <= 0}
            onclick={() => (transferPointsOpen = true)}>Transfer Points</Button
          >
        </div>
      {:else if selectedTab === "account" && accountAuth}
        <div class="flex flex-col items-center gap-1 py-8 text-center">
          <p
            class="text-muted-foreground text-[0.65rem] tracking-widest uppercase"
          >
            Signed in as
          </p>
          <strong class="text-2xl tracking-wider uppercase"
            >{accountAuth.account.username}</strong
          ><span class="text-muted-foreground"
            >Remaining Time {formatProseDuration(
              accountAuth.account.remaining_seconds,
            )} ·
            {formatPoints(accountAuth.account.points_balance_centi)} pts</span
          >
        </div>
        <div class="flex flex-col gap-2">
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy || accountAuth.account.remaining_seconds <= 0}
            onclick={() => (transferTimeOpen = true)}>Transfer Time</Button
          ><Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy}
            onclick={() => (changePasswordOpen = true)}>Change Password</Button
          >
        </div>
      {:else}
        <div class="flex flex-col gap-4">
          <p
            class="text-muted-foreground text-[0.72rem] font-bold tracking-widest uppercase"
          >
            Owner settings
          </p>
          {#if ownerUnlocked}
            <SettingsPanel
              serverUrl={config?.server_url ?? "http://127.0.0.1:3210"}
              pcId={config?.pc_id ?? null}
              connected={timer?.connected ?? false}
              {busy}
              viewMode={config?.view_mode ?? "widget"}
              launchAtStartup={config?.launch_at_startup ?? true}
              disableWindowsSignOut={config?.disable_windows_sign_out ?? false}
              onSaveAndConnect={(url) => saveAndConnect(url)}
              onDisconnect={() => disconnect()}
              onSetViewMode={(mode) => setDefaultViewMode(mode)}
              onSetLaunchAtStartup={onPersistLaunchAtStartup}
              onSetWindowsSignOut={onPersistWindowsSignOut}
              {windowsPlatform}
              onQuit={() => quit()}
            />
          {:else}
            {#key ownerGateEpoch}
              <OwnerPinGate
                onUnlock={unlockOwnerSettings}
                onCancel={cancelOwnerPin}
              />
            {/key}
          {/if}
        </div>
      {/if}
    </div>
  <footer class="grid grid-cols-2 gap-2 border-t p-2">
    <Button
      type="button"
      size="sm"
      variant="ghost"
      disabled={busy}
      onclick={minimizeToMiniBar}>Minimize</Button
    >
    {#if accountAuth}
      <Button
        type="button"
        size="sm"
        variant="ghost"
        disabled={busy}
        class="text-destructive"
        onclick={() => void logout()}>Log out</Button
      >
    {:else}
      <Button type="button" size="sm" onclick={onRequestAuthentication}
        >Log in</Button
      >
    {/if}
  </footer>
</section>

<WidgetOverlay title="Confirm Claim" open={claimOpen}>
  <Card class="w-full max-w-md">
    <CardContent class="flex flex-col gap-4 py-6">
      <div class="flex flex-col gap-2">
        <p class="font-bold tracking-wider uppercase">Claim Guest Time?</p>
        <p>
          Move <strong>{formatProseDuration(claimSeconds)}</strong> of Guest Time to your
          Account? The time will become portable and this PC's Guest Time will drop
          to zero.
        </p>
      </div>
      {#if claimError}
        <p class="text-destructive" role="alert" aria-live="assertive">
          {claimError}
        </p>
      {/if}
      <div class="flex justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={claimBusy}
          onclick={closeClaimConfirmation}
        >
          Cancel
        </Button>
        <Button
          type="button"
          disabled={claimBusy}
          onclick={() => void claimTime()}
        >
          {claimBusy ? "Claiming…" : "Confirm Claim"}
        </Button>
      </div>
    </CardContent>
  </Card>
</WidgetOverlay>

{#if accountAuth}
  <RedeemTiersDialog
    open={redeemOpen}
    menu={tierMenu}
    loading={tiersLoading}
    loadError={tiersError}
    balanceCenti={pointsBalanceCenti}
    busy={redemptionBusy}
    onRetry={() => void loadRedemptionTiers()}
    onConfirm={handleRedeem}
    onClose={() => (redeemOpen = false)}
  />
  <TransferTimeDialog
    open={transferTimeOpen}
    currentSeconds={accountAuth.account.remaining_seconds}
    busy={transferTimeBusy}
    onSubmit={handleTransferTime}
    onClose={() => (transferTimeOpen = false)}
  />
  <TransferPointsDialog
    open={transferPointsOpen}
    currentPointsCenti={accountAuth.account.points_balance_centi}
    busy={transferPointsBusy}
    onSubmit={handleRequestPointsTransfer}
    onClose={() => (transferPointsOpen = false)}
  />
  <WheelDialog
    spinState={spin.state}
    pointsBalanceCenti={accountAuth.account.points_balance_centi}
    onSpin={spin.spin}
    onSubmitPassword={spin.submitPassword}
    onNextFreeSpin={spin.nextFreeSpin}
    onClose={spin.close}
  />
{/if}

{#if accountAuth && attendanceSupported && attendanceSummary}
  <DailyRewardDialog
    open={attendanceOpen}
    summary={attendanceSummary}
    selectedMonth={selectedAttendanceMonth}
    monthDetails={selectedAttendanceMonthDetails}
    monthLoading={attendanceMonthLoading === selectedAttendanceMonth}
    monthError={attendanceMonthError}
    stale={attendanceStale}
    claimingMonth={attendanceClaimingMonth}
    claimError={attendanceClaimError}
    onSelectMonth={selectDailyRewardMonth}
    onClose={() => (attendanceOpen = false)}
    onClaim={(earningMonth) => void onClaimAttendanceMonth(earningMonth)}
  />
{/if}

<ChangePasswordDialog
  open={changePasswordOpen}
  busy={changePasswordBusy}
  onSubmit={handleChangePassword}
  onClose={() => (changePasswordOpen = false)}
/>

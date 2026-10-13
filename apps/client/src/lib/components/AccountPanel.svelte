<script lang="ts">
  // Signed-in account panel: shows the username, portable time,
  // points, the claim-Guest-Time prompt when the active session has
  // a non-zero claimable balance, the voucher and points-redemption
  // forms, the Spin the Wheel control, and the Log out button.
  //
  // The server owns all of these mutations (claim, voucher, redeem,
  // spin, logout). The panel just collects input and surfaces the
  // server's refreshed AccountAuthState on success.

  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Separator } from "$lib/components/ui/separator";
  import {
    formatAttendanceMonth,
    formatPoints,
    formatProseDuration,
  } from "$lib/format";
  import type {
    AccountAuth,
    AttendanceSummary,
    RedemptionTierMenu,
  } from "$lib/types";
  import { cheapestTierShortfallCenti, isTierAffordable } from "$lib/redemption";
  import WheelEntry from "./WheelEntry.svelte";
  import AuthForm from "./AuthForm.svelte";

  interface Props {
    accountAuth: AccountAuth | null;
    attendanceSummary: AttendanceSummary | null;
    attendanceSupported: boolean;
    attendanceStale: boolean;
    attendanceClaimingMonth: string | null;
    attendanceClaimError: string;
    onClaimAttendanceMonth: (earningMonth: string) => Promise<void> | void;
    busy: boolean;
    error: string;
    shopClosed?: boolean;
    onClaim: () => Promise<void>;
    onRedeemVoucher: (code: string) => Promise<boolean>;
    redemptionTierMenu: RedemptionTierMenu | null;
    redemptionTiersLoading: boolean;
    redemptionTiersError: string;
    onRefreshRedemptionTiers: () => Promise<void>;
    onRedeemPoints: (tierId: number, version: number) => Promise<void>;
    /** Whether the wheel is enabled in the wheel settings. */
    wheelEnabled: boolean;
    /** Open the shared Spin the Wheel dialog. */
    onOpenWheel: () => void;
    onLogout: () => Promise<void>;
    onAuthenticate: (
      mode: "login" | "signup",
      username: string,
      password: string,
    ) => Promise<void>;
    onSwitchMode: (mode: "login" | "signup") => void;
  }

  let {
    accountAuth,
    attendanceSummary,
    attendanceSupported,
    attendanceStale,
    attendanceClaimingMonth,
    attendanceClaimError,
    onClaimAttendanceMonth,
    busy,
    error,
    shopClosed = false,
    onClaim,
    onRedeemVoucher,
    redemptionTierMenu,
    redemptionTiersLoading,
    redemptionTiersError,
    onRefreshRedemptionTiers,
    onRedeemPoints,
    wheelEnabled,
    onOpenWheel,
    onLogout,
    onAuthenticate,
    onSwitchMode,
  }: Props = $props();

  let voucherCode = $state("");
  const pointsBalanceCenti = $derived(accountAuth?.account.points_balance_centi ?? 0);
  const cheapestShortfallCenti = $derived(
    cheapestTierShortfallCenti(redemptionTierMenu, pointsBalanceCenti),
  );
  const dailyRewardClaimMonths = $derived(
    [...(attendanceSummary?.calendar_months ?? []), ...(attendanceSummary?.older_claimable_months ?? [])]
      .filter((month) => month.claimable_seconds > 0),
  );

  async function handleVoucherSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    // Match the pre-refactor behavior: clear the field only when the redeem
    // succeeds, so a failed code stays editable for a retry.
    if (await onRedeemVoucher(voucherCode)) {
      voucherCode = "";
    }
  }

</script>

{#if accountAuth}
  <div class="flex flex-col gap-4">
    <div class="flex flex-col items-center gap-1 text-center">
      <p class="text-muted-foreground text-xs font-bold tracking-widest uppercase">
        Logged In as
      </p>
      <h2 class="text-2xl font-semibold tracking-tight uppercase">
        {accountAuth.account.username}
      </h2>
      <p class="text-muted-foreground text-sm">
        {formatPoints(accountAuth.account.points_balance_centi)} points
      </p>
      <p class="text-amber-600 text-xs font-bold dark:text-amber-300">
        Log out to save your remaining time.
      </p>
      {#if accountAuth.account.remaining_seconds <= 0}
        <p
          class="mt-1 rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-xs text-amber-700 dark:text-amber-300"
          role="status"
          aria-live="polite"
        >
          {shopClosed
            ? "Account has no time remaining — log out to return later."
            : "Account has no time remaining — insert coin or redeem time."}
        </p>
      {/if}
    </div>

    {#if accountAuth.account.remaining_seconds <= 0 && attendanceSupported && attendanceSummary}
      <section
        class="flex flex-col gap-2 rounded-md border border-primary/40 bg-primary/5 p-3"
        aria-label="Daily Reward claim"
        data-daily-reward-lock-entry
      >
        <div class="flex items-center justify-between gap-2">
          <h3 class="text-sm font-bold">Daily Reward</h3>
          {#if attendanceStale}
            <span class="text-muted-foreground text-[0.65rem]">Saved information</span>
          {/if}
        </div>
        {#if attendanceClaimError}
          <p class="text-destructive text-xs" role="alert" aria-live="assertive">{attendanceClaimError}</p>
        {/if}
        {#if dailyRewardClaimMonths.length > 0}
          {#each dailyRewardClaimMonths as month (month.earning_month)}
            <div class="flex items-center justify-between gap-2 rounded-md border bg-card p-2">
              <span class="min-w-0">
                <span class="block text-xs font-semibold">{formatAttendanceMonth(month.earning_month)}</span>
                <span class="text-muted-foreground block text-xs">
                  {formatProseDuration(month.claimable_seconds)} ready to claim
                </span>
              </span>
              <Button
                type="button"
                size="sm"
                disabled={busy || attendanceClaimingMonth !== null}
                data-daily-reward-lock-claim={month.earning_month}
                aria-label="Claim {formatProseDuration(month.claimable_seconds)} from {formatAttendanceMonth(month.earning_month)}"
                onclick={() => void onClaimAttendanceMonth(month.earning_month)}
              >
                {attendanceClaimingMonth === month.earning_month ? "Claiming…" : "Claim"}
              </Button>
            </div>
          {/each}
        {:else}
          <p class="text-muted-foreground text-xs">No Daily Reward is ready to claim.</p>
        {/if}
      </section>
    {/if}

    {#if error}
      <p class="text-destructive text-sm" role="alert" aria-live="assertive">{error}</p>
    {/if}

    {#if accountAuth.session.claimable_guest_seconds > 0}
      <div
        class="flex flex-col gap-3 rounded-md border border-emerald-500/40 bg-emerald-500/10 p-3 text-sm"
      >
        <p class="text-emerald-700 dark:text-emerald-300">
          This PC has {formatProseDuration(accountAuth.session.claimable_guest_seconds)}
          of Guest Time. Claiming moves all of it to your portable account balance.
        </p>
        <div>
          <Button type="button" disabled={busy} onclick={() => void onClaim()}>
            Claim Guest Time
          </Button>
        </div>
      </div>
    {/if}

    {#if !shopClosed}
    <form class="grid grid-cols-1 items-end gap-2 sm:grid-cols-[1fr_auto]" onsubmit={handleVoucherSubmit}>
      <div class="flex flex-col gap-1.5">
        <label for="voucher-code" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
          Voucher code
        </label>
        <Input
          id="voucher-code"
          bind:value={voucherCode}
          maxlength={64}
          required
        />
      </div>
      <Button type="submit" disabled={busy}>Redeem voucher</Button>
    </form>
    {/if}

    <div class="flex flex-col gap-2">
      <p class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
        Redemption Tiers
      </p>
      {#if cheapestShortfallCenti > 0}
        <p class="text-muted-foreground text-xs">
          {formatPoints(cheapestShortfallCenti)} more points to unlock the cheapest tier
        </p>
      {/if}
      {#if redemptionTiersLoading}
        <p class="text-muted-foreground text-sm">Loading Redemption Tiers…</p>
      {:else if redemptionTiersError}
        <p class="text-destructive text-sm" role="alert">Could not load Redemption Tiers.</p>
        <Button type="button" variant="outline" onclick={() => void onRefreshRedemptionTiers()}>Try again</Button>
      {:else}
        <div class="grid gap-2 sm:grid-cols-2">
          {#each redemptionTierMenu?.tiers ?? [] as tier (tier.id)}
            <Button
              type="button"
              variant="outline"
              disabled={busy || !isTierAffordable(tier, accountAuth.account.points_balance_centi)}
              onclick={() => void onRedeemPoints(tier.id, redemptionTierMenu!.version)}
            >
              {tier.points} pts → {formatProseDuration(tier.seconds)}
            </Button>
          {/each}
        </div>
      {/if}
    </div>

    <WheelEntry {accountAuth} {busy} {wheelEnabled} onOpen={onOpenWheel} variant="card" />

    <Separator />

    <div class="flex flex-col">
      <Button
        type="button"
        variant="outline"
        disabled={busy}
        onclick={() => void onLogout()}
      >
        Log out and save time
      </Button>
    </div>
  </div>
{:else}
  <div class="flex flex-col gap-4">
    <div class="flex flex-col gap-1">
      <p class="text-muted-foreground text-xs font-bold tracking-widest uppercase">
        Customer account
      </p>
      <h2 class="text-2xl font-semibold tracking-tight">Log in</h2>
    </div>
    <AuthForm
      {busy}
      {error}
      {onAuthenticate}
      {onSwitchMode}
    />
  </div>
{/if}

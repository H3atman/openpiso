<script lang="ts">
  import { onMount } from "svelte";
  import { Button } from "./ui/button";
  import * as api from "../api";
  import { formatDuration, formatMoney, formatPoints } from "../format";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import type { CoinBatchRejection, RejectionCreditResult } from "../types";

  /**
   * Rejection Credit (ADR-0069): re-credit a coin the acceptor physically took
   * but never valued.
   *
   * The owner picks a denomination from the current Pulse Map — never a typed
   * peso amount, because a historical rejection has no automatic amount. The
   * seconds and points shown are computed by the server on a dry run, so this
   * dialog never recomputes pricing. The target is frozen at the rejection and
   * cannot be chosen here.
   */
  interface Props {
    rejection: CoinBatchRejection;
    oncredited: (credit: RejectionCreditResult) => void | Promise<void>;
    onclose: () => void;
  }

  let { rejection, oncredited, onclose }: Props = $props();

  let denominations = $state<number[]>([]);
  let denominationCents = $state<number | null>(null);
  let coins = $state(1);
  let preview = $state<RejectionCreditResult | null>(null);
  let loadError = $state("");
  let previewError = $state("");
  let busy = $state(false);
  let previewing = $state(false);

  // Web Admin is served over plain HTTP on the shop LAN, which is not a secure
  // context: `crypto.randomUUID` does not exist there. `$props.id()` is the
  // Svelte-provided per-instance id and needs no crypto.
  const instanceId = $props.id();
  const titleId = `rejection-credit-${instanceId}`;
  const maxCoins = $derived(Math.max(rejection.remaining_coins, 0));
  const target = $derived(
    rejection.account_username
      ? `Account ${rejection.account_username}`
      : `${rejection.pc_name ?? "this PC"} Guest Time`,
  );
  const ready = $derived(
    denominationCents !== null && coins >= 1 && coins <= maxCoins,
  );

  onMount(() => {
    void loadDenominations();
  });

  async function loadDenominations() {
    try {
      const result = await api.getRejectionDenominations();
      denominations = result.denominations_cents;
      denominationCents = null;
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  // The preview is a server dry run, so what the owner reads is exactly what
  // the commit will credit — including the Coin Promo bonus and the points
  // carry the browser has no way to compute.
  async function refreshPreview() {
    preview = null;
    previewError = "";
    if (!ready || denominationCents === null) return;
    previewing = true;
    try {
      preview = await api.creditRejection({
        rejectionAuditId: rejection.rejection_audit_id,
        denominationCents,
        coins,
        dryRun: true,
      });
    } catch (caught) {
      previewError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      previewing = false;
    }
  }

  async function commit() {
    if (!ready || busy || denominationCents === null) return;
    busy = true;
    try {
      const credit = await api.creditRejection({
        rejectionAuditId: rejection.rejection_audit_id,
        denominationCents,
        coins,
        dryRun: false,
      });
      reportActionSuccess("credit", `${formatMoney(credit.amount_cents)} to ${target}`);
      await oncredited(credit);
      onclose();
    } catch (caught) {
      reportActionFailure("credit", "rejected coin", caught);
    } finally {
      busy = false;
    }
  }

  function close() {
    if (busy) return;
    onclose();
  }
</script>

<svelte:window
  onkeydown={(event) => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    if (busy) return;
    close();
    // Claim a handled Escape so stacked overlays do not close together.
    event.preventDefault();
  }}
/>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
  onclick={close}
>
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="w-full max-w-md rounded-lg border bg-card p-6 shadow-lg"
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
  >
    <h3 class="text-lg font-semibold" id={titleId}>Credit rejected coin</h3>
    <p class="mt-2 text-sm text-muted-foreground max-md:break-words">
      {#if rejection.pulse_count !== null}
        {rejection.pc_name ?? rejection.pc_id} rejected a coin after
        {rejection.pulse_count} pulses.
      {:else}
        {rejection.pc_name ?? rejection.pc_id} suppressed
        {rejection.cap_coins} further rejected coins.
      {/if}
      Credit goes to <span class="font-medium text-foreground">{target}</span>.
    </p>

    {#if rejection.account_deleted}
      <p class="mt-4 rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive">
        The Account recorded on this rejection has been deleted, so this coin can
        no longer be credited. It is never redirected to the PC's Guest Time.
      </p>
    {:else if loadError}
      <p class="mt-4 text-sm text-destructive">
        Unable to load denominations: {loadError}
      </p>
    {:else}
      <div class="mt-4 space-y-4">
        <div>
          <span class="text-sm font-medium">Coin inserted</span>
          <p class="mb-2 text-xs text-muted-foreground">
            Pick what the customer actually put in. The observed pulse count has
            no known value — correct the Pulse Map to stop the next rejection.
          </p>
          <div class="flex flex-wrap gap-2">
            {#each denominations as cents (cents)}
              <Button
                type="button"
                size="sm"
                variant={denominationCents === cents ? "default" : "outline"}
                disabled={busy}
                onclick={() => {
                  denominationCents = cents;
                  void refreshPreview();
                }}
              >
                {formatMoney(cents)}
              </Button>
            {/each}
          </div>
        </div>

        {#if rejection.cap_coins > 1}
          <label class="block text-sm font-medium" for={`${titleId}-coins`}>
            How many coins
            <input
              id={`${titleId}-coins`}
              class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm"
              type="number"
              min="1"
              max={maxCoins}
              disabled={busy}
              bind:value={coins}
              onchange={() => void refreshPreview()}
            />
          </label>
          <p class="-mt-2 text-xs text-muted-foreground">
            {rejection.credited_coins} of {rejection.cap_coins} already credited;
            {maxCoins} left.
          </p>
        {/if}

        {#if previewing}
          <p class="text-sm text-muted-foreground">Calculating…</p>
        {:else if previewError}
          <p class="text-sm text-destructive">{previewError}</p>
        {:else if preview}
          <div class="rounded-md border bg-muted/30 p-3 text-sm">
            <p class="font-medium">
              {formatMoney(preview.amount_cents)} → {formatDuration(preview.seconds_added)}
            </p>
            <ul class="mt-1 space-y-0.5 text-xs text-muted-foreground">
              {#if preview.coin_promo_bonus_seconds > 0}
                <li>
                  Includes {formatDuration(preview.coin_promo_bonus_seconds)} Coin Promo bonus
                </li>
              {/if}
              <li>
                {preview.points_earned_centi > 0
                  ? `Earns ${formatPoints(preview.points_earned_centi)} points`
                  : "Earns no points (guest)"}
              </li>
              <li>Reported as Cash Revenue — the coin is in the coin box.</li>
            </ul>
          </div>
          <p class="text-xs text-muted-foreground">
            A Rejection Credit cannot be undone.
          </p>
        {/if}
      </div>
    {/if}

    <div class="mt-5 flex justify-end gap-2">
      <Button type="button" variant="outline" disabled={busy} onclick={close}>
        Cancel
      </Button>
      <Button
        type="button"
        disabled={busy || !preview || rejection.account_deleted}
        onclick={commit}
      >
        {busy ? "Crediting…" : "Credit"}
      </Button>
    </div>
  </div>
</div>

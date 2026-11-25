<script lang="ts">
  import { onMount } from "svelte";
  import { toast } from "svelte-sonner";
  import * as api from "../api";
  import { Button } from "$lib/components/ui/button";
  import { formatDate, formatMoney } from "../format";
  import { reportActionFailure } from "../notifications";
  import type { CashCollectionKind, CashOnHand } from "../types";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  /**
   * Cash on hand (ADR-0091): per kind, what should still be in the coin box or
   * at the counter. Independent of the report range. Collect is custody only
   * and never changes a sales figure; the Admin fixes the amount on confirm.
   */
  const CARDS: { kind: CashCollectionKind; label: string }[] = [
    { kind: "coin_box", label: "Coin Box" },
    { kind: "voucher_cash", label: "Voucher Cash" },
  ];

  let cashOnHand = $state<CashOnHand | null>(null);
  let loadError = $state("");
  let confirming = $state<CashCollectionKind | null>(null);
  let busy = $state(false);

  async function load(): Promise<void> {
    loadError = "";
    try {
      cashOnHand = await api.getCashOnHand();
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function collect(kind: CashCollectionKind, label: string): Promise<void> {
    busy = true;
    try {
      const recorded = await api.collectCash(kind);
      toast.success(`Collected ${formatMoney(recorded.amount_cents)} from ${label}`);
    } catch (caught) {
      reportActionFailure("collect", label, caught);
    } finally {
      busy = false;
      confirming = null;
    }
    await load();
  }

  onMount(() => void load());
</script>

<section aria-labelledby="cash-on-hand-heading">
  <h3 id="cash-on-hand-heading" class="mb-2 font-semibold">Cash on hand</h3>
  {#if loadError}
    <div class="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive" role="alert">
      Couldn't load cash on hand: {loadError}
    </div>
  {:else if cashOnHand}
    <div class="grid gap-4 md:grid-cols-2">
      {#each CARDS as card (card.kind)}
        {@const state = cashOnHand[card.kind]}
        <section class="rounded-lg border bg-card p-4" aria-label={card.label}>
          <p class="text-sm text-muted-foreground">{card.label} · Uncollected</p>
          <p class="mt-1 text-2xl font-bold tabular-nums">{formatMoney(state.uncollected_cents)}</p>
          <p class="mt-1 text-xs text-muted-foreground">
            Last collected:
            {#if state.last_collection}
              {formatDate(state.last_collection.collected_at)} · {formatMoney(state.last_collection.amount_cents)}
            {:else}
              Never
            {/if}
          </p>
          <Button
            class="mt-3"
            disabled={state.uncollected_cents <= 0}
            onclick={() => (confirming = card.kind)}
          >
            Collect
          </Button>
        </section>
      {/each}
    </div>
  {/if}
</section>

{#if confirming && cashOnHand}
  {@const card = CARDS.find((c) => c.kind === confirming)!}
  <ConfirmDialog
    title="Collect {formatMoney(cashOnHand[card.kind].uncollected_cents)} from {card.label}?"
    confirmLabel="Collect"
    busyLabel="Collecting…"
    {busy}
    onconfirm={() => collect(card.kind, card.label)}
    oncancel={() => (confirming = null)}
  >
    This records that you took the cash. It cannot be undone and does not change any sales figure. Cash that arrives
    before you confirm is included.
  </ConfirmDialog>
{/if}

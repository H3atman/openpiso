<script lang="ts">
  // RedeemTiersDialog — the "Claim Time" flow on the Points tab. Lists the
  // Redemption Tier menu (unaffordable tiers disabled), lets the customer
  // pick one, and confirms the redemption in the same dialog. The caller
  // owns menu loading (it also feeds the tab's shortfall nudge) and the
  // redeem mutation; a menu refresh (e.g. after a stale-menu conflict)
  // clears the selection so the customer chooses again.

  import { Button } from "$lib/components/ui/button";
  import { formatProseDuration } from "$lib/format";
  import { isTierAffordable } from "$lib/redemption";
  import WidgetOverlay from "$lib/components/WidgetOverlay.svelte";
  import type { RedemptionTier, RedemptionTierMenu } from "$lib/types";

  interface Props {
    open: boolean;
    menu: RedemptionTierMenu | null;
    loading: boolean;
    loadError: string;
    balanceCenti: number;
    busy: boolean;
    onRetry: () => void;
    onConfirm: (tier: RedemptionTier) => Promise<void>;
    onClose: () => void;
  }

  let {
    open,
    menu,
    loading,
    loadError,
    balanceCenti,
    busy,
    onRetry,
    onConfirm,
    onClose,
  }: Props = $props();

  let selectedId = $state<number | null>(null);
  let localError = $state("");

  const selectedTier = $derived(
    menu?.tiers.find((tier) => tier.id === selectedId) ?? null,
  );

  $effect(() => {
    if (open) {
      selectedId = null;
      localError = "";
    }
  });

  // A refreshed menu invalidates the previous choice.
  $effect(() => {
    menu;
    selectedId = null;
  });

  async function confirm(): Promise<void> {
    if (!selectedTier || busy) return;
    localError = "";
    try {
      await onConfirm(selectedTier);
    } catch (caught) {
      localError = caught instanceof Error ? caught.message : String(caught);
    }
  }
</script>

<WidgetOverlay title="Claim Time" open={open}>
  <div class="bg-card text-card-foreground w-full max-w-sm rounded-md border p-5">
    <p
      class="text-muted-foreground mb-3 text-[0.72rem] font-bold tracking-widest uppercase"
    >
      Claim Time
    </p>

    <div class="flex flex-col gap-1" data-tier-menu aria-live="polite">
      {#if loading}
        <p class="text-muted-foreground text-center text-xs">Loading Redemption Tiers…</p>
      {:else if loadError}
        <p class="text-destructive text-xs" role="alert">Could not load Redemption Tiers.</p>
        <Button type="button" size="sm" variant="outline" onclick={onRetry}>Try again</Button>
      {:else if !menu?.tiers.length}
        <p class="text-muted-foreground text-center text-xs">No Redemption Tiers available.</p>
      {:else}
        {#each menu.tiers as tier (tier.id)}
          <Button
            type="button"
            size="sm"
            variant={selectedId === tier.id ? "default" : "outline"}
            data-tier-id={tier.id}
            aria-pressed={selectedId === tier.id}
            disabled={busy || !isTierAffordable(tier, balanceCenti)}
            onclick={() => (selectedId = tier.id)}
          >
            {tier.points} pts → {formatProseDuration(tier.seconds)}
          </Button>
        {/each}
      {/if}
    </div>

    {#if selectedTier}
      <p class="mt-3 text-center">
        Redeem <strong>{selectedTier.points} points</strong> for
        <strong>{formatProseDuration(selectedTier.seconds)}</strong>?
      </p>
    {/if}

    {#if localError}
      <p class="text-destructive mt-2 text-[0.65rem]" role="alert" aria-live="assertive">
        {localError}
      </p>
    {/if}

    <div class="mt-4 flex justify-end gap-2">
      <Button type="button" variant="outline" disabled={busy} onclick={onClose}>
        Cancel
      </Button>
      <Button
        type="button"
        disabled={busy || !selectedTier}
        onclick={() => void confirm()}
      >
        {busy ? "Redeeming…" : "Confirm Redemption"}
      </Button>
    </div>
  </div>
</WidgetOverlay>

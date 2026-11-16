<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Switch } from "../../components/ui/switch";
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import ToggleRow from "../../components/settings/ToggleRow.svelte";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import Trash2Icon from "@lucide/svelte/icons/trash-2";
  import type { CoinPromoSettings, CoinPromoSettingsInput, CoinPromoTierInput } from "../../types";

  function formatBasisPoints(basisPoints: number): string {
    return `${(basisPoints / 100).toFixed(2)}%`;
  }

  // Coin Promo safety previews are returned by Rust; the browser only formats them.
  // Rows are authored in pesos; cents stay the wire/storage unit.
  type CoinPromoTierDraft = { amount_pesos: number; bonus_minutes: number };
  let coinPromoEnabled = $state(false);
  let coinPromoTiers = $state<CoinPromoTierDraft[]>([]);
  let coinPromoPreview = $state<CoinPromoSettings | null>(null);
  let coinPromoLoaded = $state(false);
  let coinPromoBusy = $state(false);
  let coinPromoLoadError = $state("");
  let coinPromoPreviewSequence = 0;

  function coinPromoTierCents(tier: CoinPromoTierDraft): number {
    return Math.round(tier.amount_pesos * 100);
  }

  function coinPromoInput(): CoinPromoSettingsInput {
    const tiers: CoinPromoTierInput[] = coinPromoTiers.map((tier) => ({
      amount_cents: coinPromoTierCents(tier),
      bonus_minutes: tier.bonus_minutes,
    }));
    return { enabled: coinPromoEnabled, tiers };
  }

  async function loadCoinPromo() {
    try {
      const settings = await api.getCoinPromo();
      coinPromoEnabled = settings.enabled;
      coinPromoTiers = settings.tiers.map((tier) => ({
        amount_pesos: tier.amount_cents / 100,
        bonus_minutes: tier.bonus_seconds / 60,
      }));
      coinPromoPreview = settings;
      coinPromoLoaded = true;
    } catch (caught) {
      coinPromoLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function addCoinPromoTier() {
    const nextAmount = coinPromoTiers.length === 0
      ? 1
      : Math.max(...coinPromoTiers.map((tier) => tier.amount_pesos)) + 1;
    coinPromoTiers = [...coinPromoTiers, { amount_pesos: nextAmount, bonus_minutes: 1 }];
  }

  function removeCoinPromoTier(tier: CoinPromoTierDraft) {
    coinPromoTiers = coinPromoTiers.filter((row) => row !== tier);
  }

  function coinPromoTierBasisPoints(tier: CoinPromoTierDraft): number | null {
    return coinPromoPreview?.tiers.find((computed) =>
      computed.amount_cents === coinPromoTierCents(tier) &&
      computed.bonus_seconds === tier.bonus_minutes * 60
    )?.effective_bonus_basis_points ?? null;
  }

  async function refreshCoinPromoPreview(input: CoinPromoSettingsInput, sequence: number) {
    try {
      const preview = await api.previewCoinPromo(input);
      if (sequence === coinPromoPreviewSequence) coinPromoPreview = preview;
    } catch {
      // Incomplete drafts have no computed preview; the authoritative save error uses a toast.
    }
  }

  async function saveCoinPromo() {
    ++coinPromoPreviewSequence;
    coinPromoBusy = true;
    try {
      const settings = await api.updateCoinPromo(coinPromoInput());
      coinPromoEnabled = settings.enabled;
      coinPromoTiers = settings.tiers.map((tier) => ({
        amount_pesos: tier.amount_cents / 100,
        bonus_minutes: tier.bonus_seconds / 60,
      }));
      coinPromoPreview = settings;
      reportActionSuccess("save", "Coin Promo");
    } catch (caught) {
      reportActionFailure("save", "Coin Promo", caught);
    } finally {
      coinPromoBusy = false;
    }
  }

  $effect(() => {
    const input = coinPromoInput();
    // Read nested draft fields now so Svelte reruns this effect for every edit.
    JSON.stringify(input);
    const sequence = ++coinPromoPreviewSequence;
    coinPromoPreview = null;
    if (!coinPromoLoaded || coinPromoBusy) return;
    const timer = setTimeout(() => void refreshCoinPromoPreview(input, sequence), 250);
    return () => clearTimeout(timer);
  });

  onMount(() => {
    void loadCoinPromo();
  });
</script>

<SettingsCard
  title="Bonus tiers"
  description="Give logged-in accounts bonus time for the coins they insert. Guests receive no bonus."
  flush
>
  {#snippet actions()}
    <Button variant="outline" size="sm" disabled={coinPromoBusy} onclick={addCoinPromoTier}>
      <PlusIcon aria-hidden="true" />
      Add tier
    </Button>
  {/snippet}

  <div class="space-y-4 border-b px-4 py-4 md:px-6">
    <ToggleRow
      id="coin-promo-enabled"
      label="Enable Coin Promo"
      description="When off, no Coin Promo bonus is given; your tiers stay saved."
      status={coinPromoEnabled ? "On" : "Off"}
    >
      <Switch id="coin-promo-enabled" aria-describedby="coin-promo-enabled-description" bind:checked={coinPromoEnabled} />
    </ToggleRow>

    <Notice>
      <p>
        Coins inserted together are credited as one batch and split across tiers largest-first, so ₱10 + ₱1 earns
        the ₱10 tier; an amount no tier covers earns nothing.
      </p>
      <p class="text-muted-foreground">Coin amounts are in pesos; bonuses are whole minutes.</p>
    </Notice>

    {#if coinPromoLoadError}
      <Notice tone="danger">{coinPromoLoadError}</Notice>
    {/if}
  </div>

  {#if coinPromoTiers.length === 0}
    <p class="px-4 py-10 text-center text-sm text-muted-foreground">
      No Coin Promo Tiers yet. Add one to give a bonus for a coin amount.
    </p>
  {:else}
    <div
      class="hidden grid-cols-[10rem_8rem_minmax(0,1fr)_2.75rem] items-center gap-4 border-b bg-muted/40 px-6 py-2 text-xs font-medium text-muted-foreground md:grid"
    >
      <span>Coin amount</span>
      <span>Bonus minutes</span>
      <span>Effective Bonus</span>
      <span class="sr-only">Actions</span>
    </div>
    <ul class="divide-y">
      <!-- Authoring order, not sorted order — see the Redemption Tiers list. -->
      {#each coinPromoTiers as tier (tier)}
        {@const basisPoints = coinPromoTierBasisPoints(tier)}
        <li
          class="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-end gap-x-3 gap-y-2 px-4 py-3 md:grid-cols-[10rem_8rem_minmax(0,1fr)_2.75rem] md:items-center md:gap-4 md:px-6"
        >
          <div class="grid gap-1">
            <span class="text-xs text-muted-foreground md:hidden" aria-hidden="true">Coin amount</span>
            <div class="relative">
              <span class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-sm text-muted-foreground" aria-hidden="true">₱</span>
              <Input
                aria-label="Coin amount in pesos"
                type="number"
                min="0.01"
                step="0.01"
                class="pl-6 tabular-nums"
                bind:value={tier.amount_pesos}
              />
            </div>
          </div>
          <div class="grid gap-1">
            <span class="text-xs text-muted-foreground md:hidden" aria-hidden="true">Bonus minutes</span>
            <Input
              aria-label="Coin Promo bonus minutes"
              type="number"
              min="1"
              step="1"
              class="tabular-nums"
              bind:value={tier.bonus_minutes}
            />
          </div>
          <p class="col-span-2 row-start-2 font-mono text-sm tabular-nums md:col-span-1 md:col-start-3 md:row-start-1">
            <span class="mr-2 font-sans text-muted-foreground md:hidden">Effective Bonus</span>
            {basisPoints === null ? "—" : formatBasisPoints(basisPoints)}
          </p>
          <Button
            variant="ghost"
            size="icon-sm"
            class="col-start-3 row-start-1 self-end text-muted-foreground hover:text-destructive md:col-start-4 md:self-center"
            aria-label="Remove ₱{tier.amount_pesos} tier"
            title="Remove tier"
            disabled={coinPromoBusy}
            onclick={() => removeCoinPromoTier(tier)}
          >
            <Trash2Icon aria-hidden="true" />
          </Button>
        </li>
      {/each}
    </ul>
  {/if}

  {#snippet footer()}
    <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">
      Effective Bonus is recalculated as you edit; nothing changes until you save.
    </p>
    <Button disabled={coinPromoBusy} onclick={saveCoinPromo}>
      {coinPromoBusy ? "Saving…" : "Save Coin Promo"}
    </Button>
  {/snippet}
</SettingsCard>

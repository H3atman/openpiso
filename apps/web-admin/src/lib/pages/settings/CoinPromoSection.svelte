<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Switch } from "../../components/ui/switch";
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

<div class="rounded-lg border bg-card p-6">
  <div class="mb-5 flex flex-wrap items-start justify-between gap-4">
    <div>
      <h3 class="text-lg font-semibold">Coin Promo</h3>
      <p class="text-sm text-muted-foreground">
        Give logged-in accounts bonus time for the coins they insert. Guests receive no bonus.
      </p>
    </div>
    <label class="flex items-center gap-2 text-sm font-medium">
      <Switch size="sm" bind:checked={coinPromoEnabled} />
      Enable Coin Promo
    </label>
  </div>

  <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
    <p class="text-xs text-muted-foreground">
      Coin amounts are pesos (stored as integer cents); bonuses are whole minutes. Percentages and the combined worst case are computed by Rust.
      Coins inserted together are credited as one batch and split across tiers largest-first, so ₱10 + ₱1 earns the ₱10 tier; an amount no tier covers earns nothing.
    </p>
    <button
      class="rounded-md border px-3 py-2 text-sm font-medium hover:bg-muted disabled:opacity-50"
      type="button"
      disabled={coinPromoBusy}
      onclick={addCoinPromoTier}
    >
      Add tier
    </button>
  </div>

  <div class="overflow-x-auto">
    <table class="w-full text-left text-sm">
      <thead class="border-b text-xs text-muted-foreground">
        <tr>
          <th class="px-2 py-2 font-medium">Coin amount</th>
          <th class="px-2 py-2 font-medium">Bonus minutes</th>
          <th class="px-2 py-2 font-medium">Effective Bonus</th>
          <th class="px-2 py-2"><span class="sr-only">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        <!-- Authoring order, not sorted order — see the Redemption Tiers table. -->
        {#each coinPromoTiers as tier (tier)}
          {@const basisPoints = coinPromoTierBasisPoints(tier)}
          <tr class="border-b last:border-0">
            <td class="px-2 py-2">
              <span class="mr-1 text-xs text-muted-foreground">₱</span>
              <input
                aria-label="Coin amount in pesos"
                type="number"
                min="0.01"
                step="0.01"
                class="w-36 rounded-md border bg-background px-3 py-2"
                bind:value={tier.amount_pesos}
              />
            </td>
            <td class="px-2 py-2">
              <input
                aria-label="Coin Promo bonus minutes"
                type="number"
                min="1"
                step="1"
                class="w-32 rounded-md border bg-background px-3 py-2"
                bind:value={tier.bonus_minutes}
              />
            </td>
            <td class="px-2 py-2 font-mono text-muted-foreground">
              {basisPoints === null ? "—" : formatBasisPoints(basisPoints)}
            </td>
            <td class="px-2 py-2 text-right">
              <button
                class="rounded-md px-3 py-2 text-sm text-destructive hover:bg-destructive/10 disabled:opacity-50"
                type="button"
                disabled={coinPromoBusy}
                onclick={() => removeCoinPromoTier(tier)}
              >
                Remove
              </button>
            </td>
          </tr>
        {/each}
        {#if coinPromoTiers.length === 0}
          <tr><td colspan="4" class="px-2 py-6 text-center text-muted-foreground">No Coin Promo Tiers authored.</td></tr>
        {/if}
      </tbody>
    </table>
  </div>

  {#if coinPromoLoadError}
    <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">{coinPromoLoadError}</div>
  {/if}

  <button
    class="mt-5 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
    type="button"
    disabled={coinPromoBusy}
    onclick={saveCoinPromo}
  >
    {coinPromoBusy ? "Saving…" : "Save Coin Promo"}
  </button>
</div>

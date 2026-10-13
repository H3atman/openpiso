<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { formatBonusPercent, formatDuration } from "../../format";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import * as Tooltip from "../../components/ui/tooltip";
  import type { EconomySettings, RedemptionTierInput } from "../../types";

  let {
    initialEconomy = null,
    onInitialEconomyConsumed,
  }: {
    initialEconomy?: EconomySettings | null;
    onInitialEconomyConsumed?: () => void;
  } = $props();
  let economy = $state<EconomySettings | null>(null);
  let earnPesos = $state(0);
  const earnCents = $derived(Math.round(earnPesos * 100));
  let economyBusy = $state(false);
  let economyLoadError = $state("");

  let redemptionTiers = $state<RedemptionTierInput[]>([]);
  let tiersBusy = $state(false);
  let tiersLoadError = $state("");
  let mostGenerousTier = $derived(
    redemptionTiers.reduce<RedemptionTierInput | null>((best, tier) => {
      if (tier.points <= 0 || tier.minutes <= 0) return best;
      if (!best || tier.minutes * best.points > best.minutes * tier.points) return tier;
      return best;
    }, null),
  );
  let headlineBonusBasisPoints = $derived(
    mostGenerousTier ? tierBonusBasisPoints(mostGenerousTier) : null,
  );
  let headlineUnsafe = $derived(
    headlineBonusBasisPoints !== null && economy !== null &&
      headlineBonusBasisPoints > economy.unsafe_bonus_threshold_basis_points,
  );

  function applyEconomy(settings: EconomySettings) {
    economy = settings;
    earnPesos = settings.earn_cents_per_point / 100;
  }

  async function loadEconomy() {
    try {
      applyEconomy(await api.getEconomySettings());
    } catch (caught) {
      economyLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function saveEconomy() {
    economyBusy = true;
    try {
      economy = await api.updateEconomySettings({
        earn_cents_per_point: earnCents,
      });
      earnPesos = economy.earn_cents_per_point / 100;
      reportActionSuccess("save", "Economy");
    } catch (caught) {
      reportActionFailure("save", "Economy", caught);
    } finally {
      economyBusy = false;
    }
  }

  async function loadRedemptionTiers() {
    try {
      const tiers = await api.getRedemptionTiers();
      redemptionTiers = tiers.map((tier) => ({
        points: tier.points,
        minutes: tier.seconds / 60,
      }));
    } catch (caught) {
      tiersLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function addRedemptionTier() {
    const nextPoints = redemptionTiers.length === 0
      ? 5
      : Math.max(...redemptionTiers.map((tier) => tier.points)) + 5;
    redemptionTiers = [...redemptionTiers, { points: nextPoints, minutes: 1 }];
  }

  function removeRedemptionTier(tier: RedemptionTierInput) {
    redemptionTiers = redemptionTiers.filter((row) => row !== tier);
  }

  function tierDuration(minutes: number): string {
    return Number.isFinite(minutes) ? formatDuration(minutes * 60) : "00:00:00";
  }

  // Live preview of openpiso-core's `effective_bonus_basis_points` — BigInt
  // so the truncating division can never drift from the server's rounding.
  function tierBonusBasisPoints(tier: RedemptionTierInput): number | null {
    const seconds = Math.round(tier.minutes * 60);
    if (
      !economy || earnCents <= 0 || seconds <= 0 ||
      !Number.isInteger(tier.points) || tier.points <= 0
    ) return null;
    const numerator = BigInt(seconds) * BigInt(economy.rate_amount_cents) * 10_000n;
    const denominator = BigInt(tier.points) * BigInt(earnCents) * BigInt(economy.rate_seconds);
    return denominator > 0n ? Number(numerator / denominator) : null;
  }

  async function saveRedemptionTiers() {
    if (redemptionTiers.some((tier) => !Number.isInteger(tier.minutes) || tier.minutes < 1)) {
      reportActionFailure(
        "save",
        "Redemption Tiers",
        new Error("Tier minutes must be whole numbers of at least 1 — adjust any migrated tier before saving"),
      );
      return;
    }
    tiersBusy = true;
    try {
      const tiers = await api.updateRedemptionTiers({ tiers: redemptionTiers });
      redemptionTiers = tiers.map((tier) => ({
        points: tier.points,
        minutes: tier.seconds / 60,
      }));
      reportActionSuccess("save", "Redemption Tiers");
    } catch (caught) {
      reportActionFailure("save", "Redemption Tiers", caught);
    } finally {
      tiersBusy = false;
    }
  }

  onMount(() => {
    if (initialEconomy) {
      applyEconomy(initialEconomy);
      onInitialEconomyConsumed?.();
    } else {
      void loadEconomy();
    }
    void loadRedemptionTiers();
  });
</script>

{#snippet infoTip(text: string)}
  <Tooltip.Root>
    <Tooltip.Trigger
      class="ml-1 inline-flex size-4 items-center justify-center rounded-full border text-[10px] font-semibold text-muted-foreground hover:bg-muted"
      aria-label="What is this?"
    >
      ?
    </Tooltip.Trigger>
    <Tooltip.Content>{text}</Tooltip.Content>
  </Tooltip.Root>
{/snippet}

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">Points Settings</h3>

  {#if economy}
    <div class="mb-4 grid gap-4 sm:grid-cols-3">
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">Effective Bonus:</span>
        {@render infoTip("The real extra value points hand out, computed from your most generous tier (the one granting the most time per point). Keep it near your target — if the worst case is safe, every tier is.")}
        <span class="ml-2 font-mono font-bold" class:text-primary={!headlineUnsafe} class:text-destructive={headlineUnsafe}>
          {headlineBonusBasisPoints === null ? "—" : formatBonusPercent(headlineBonusBasisPoints)}
        </span>
      </div>
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">Most generous tier:</span>
        {@render infoTip("The tier that grants the most time per point. The headline Effective Bonus and the Wheel's point value / house edge all use this worst case.")}
        <span class="ml-2 font-mono font-bold">
          {mostGenerousTier ? `${mostGenerousTier.points} pts = ${tierDuration(mostGenerousTier.minutes)}` : "—"}
        </span>
      </div>
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">Threshold:</span>
        {@render infoTip("When Effective Bonus rises above this percentage, it is flagged red as unsafe — points would be giving away more value than you intend.")}
        <span class="ml-2 font-mono font-bold">{(economy.unsafe_bonus_threshold_basis_points / 100).toFixed(2)}%</span>
      </div>
    </div>
  {/if}

  <section class="mb-6 rounded-md border p-4">
    <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
      <div>
        <h4 class="font-semibold">Redemption Tiers</h4>
        <p class="text-xs text-muted-foreground">
          1–20 guaranteed exchanges. Points must be unique multiples of 5. Safety warnings use the tier with the most time per point.
        </p>
      </div>
      <button
        class="rounded-md border px-3 py-2 text-sm font-medium hover:bg-muted disabled:opacity-50"
        type="button"
        disabled={redemptionTiers.length >= 20 || tiersBusy}
        onclick={addRedemptionTier}
      >
        Add tier
      </button>
    </div>

    <div class="overflow-x-auto">
      <table class="w-full text-left text-sm">
        <thead class="border-b text-xs text-muted-foreground">
          <tr>
            <th class="px-2 py-2 font-medium">Points {@render infoTip("Points a customer spends for this tier — whole numbers, multiples of 5, at least 5, unique across rows.")}</th>
            <th class="px-2 py-2 font-medium">Minutes {@render infoTip("Time this tier grants, in whole minutes. The Duration column shows the HH:MM:SS equivalent.")}</th>
            <th class="px-2 py-2 font-medium">Duration</th>
            <th class="px-2 py-2 font-medium">Effective Bonus {@render infoTip("What this row really costs you: the time it grants valued at the coin rate, versus the pesos spent earning its points.")}</th>
            <th class="px-2 py-2"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <!-- Authoring order, not sorted order: re-sorting on each keystroke
               moves the focused row's DOM node and drops the caret. The
               server returns tiers sorted, so the list settles on save. -->
          {#each redemptionTiers as tier (tier)}
            {@const bonus = tierBonusBasisPoints(tier)}
            <tr class="border-b last:border-0">
              <td class="px-2 py-2">
                <input
                  aria-label="Tier points"
                  type="number"
                  min="5"
                  step="5"
                  class="w-28 rounded-md border bg-background px-3 py-2"
                  bind:value={tier.points}
                />
              </td>
              <td class="px-2 py-2">
                <input
                  aria-label="Tier minutes"
                  type="number"
                  min="1"
                  step="1"
                  class="w-32 rounded-md border bg-background px-3 py-2"
                  bind:value={tier.minutes}
                />
              </td>
              <td class="px-2 py-2 font-mono text-muted-foreground">
                {tierDuration(tier.minutes)}
              </td>
              <td class="px-2 py-2 font-mono text-muted-foreground">
                {bonus === null ? "—" : formatBonusPercent(bonus)}
              </td>
              <td class="px-2 py-2 text-right">
                <button
                  class="rounded-md px-3 py-2 text-sm text-destructive hover:bg-destructive/10 disabled:opacity-50"
                  type="button"
                  disabled={tiersBusy || redemptionTiers.length <= 1}
                  onclick={() => removeRedemptionTier(tier)}
                >
                  Remove
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if tiersLoadError}
      <div class="mt-3 rounded-md bg-destructive/10 p-3 text-sm text-destructive">{tiersLoadError}</div>
    {/if}

    <button
      class="mt-4 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
      type="button"
      disabled={tiersBusy}
      onclick={saveRedemptionTiers}
    >
      {tiersBusy ? "Saving…" : "Save tiers"}
    </button>
  </section>

  <div class="space-y-4">
    <div>
      <label class="block text-sm font-medium" for="economy-earn-pesos">
        Credits per point
        {@render infoTip("Pesos a customer spends to earn 1 point (1 credit = ₱1). Raising it makes points harder to earn; lowering it makes them easier.")}
      </label>
      <input
        id="economy-earn-pesos"
        type="number"
        min="0.01"
        step="0.01"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        bind:value={earnPesos}
      />
      <p class="mt-1 text-xs text-muted-foreground">Spend this many pesos to earn 1 point. Default: 50</p>
    </div>
  </div>

  {#if economyLoadError}
    <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">{economyLoadError}</div>
  {/if}

  <button
    class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
    type="button"
    disabled={economyBusy}
    onclick={saveEconomy}
  >
    {economyBusy ? "Saving…" : "Save"}
  </button>
</div>

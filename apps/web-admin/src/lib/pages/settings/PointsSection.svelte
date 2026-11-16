<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { formatBonusPercent, formatDuration } from "../../format";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import * as Tooltip from "../../components/ui/tooltip";
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import SettingsField from "../../components/settings/SettingsField.svelte";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import Trash2Icon from "@lucide/svelte/icons/trash-2";
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

<!-- A compact "?" circle with a larger hit area: 24px on desktop and a
     44px square on phones (negative margins keep the line height), so the
     phone touch rule cannot stretch it into a capsule. -->
{#snippet infoTip(name: string, text: string)}
  <Tooltip.Root>
    <Tooltip.Trigger
      class="group -my-1 inline-flex size-6 items-center justify-center align-middle outline-none max-md:-my-3.5 max-md:size-11"
      aria-label={name}
    >
      <span
        class="flex size-4 items-center justify-center rounded-full border text-[10px] font-semibold text-muted-foreground group-hover:bg-muted group-focus-visible:ring-3 group-focus-visible:ring-ring/50"
        aria-hidden="true"
      >
        ?
      </span>
    </Tooltip.Trigger>
    <Tooltip.Content>{text}</Tooltip.Content>
  </Tooltip.Root>
{/snippet}

<!-- One KPI, in the Dashboard's tile style: a label/value row on phones,
     its own tile from 768px. -->
{#snippet stat(label: string, help: string, value: string, detail: string, danger = false)}
  <div
    class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 px-4 py-3 md:block md:rounded-lg md:border md:bg-card md:p-4"
  >
    <p class="text-sm text-muted-foreground">{label}{@render infoTip(`About ${label}`, help)}</p>
    <p
      class="col-start-2 row-span-2 row-start-1 font-mono text-xl font-bold tabular-nums md:mt-1 md:text-2xl"
      class:text-destructive={danger}
    >
      {value}
    </p>
    <p class="text-xs text-muted-foreground md:mt-0.5" class:text-destructive={danger}>{detail}</p>
  </div>
{/snippet}

<div class="space-y-6">
  {#if economy}
    <div
      class="grid divide-y rounded-lg border bg-card md:grid-cols-3 md:gap-4 md:divide-y-0 md:border-0 md:bg-transparent"
      aria-label="Points at a glance"
      role="group"
    >
      {@render stat(
        "Effective Bonus",
        "The real extra value points hand out, computed from your most generous tier (the one granting the most time per point). Keep it near your target — if the worst case is safe, every tier is.",
        headlineBonusBasisPoints === null ? "—" : formatBonusPercent(headlineBonusBasisPoints),
        headlineUnsafe ? "Above the threshold — unsafe" : "From the most generous tier",
        headlineUnsafe,
      )}
      {@render stat(
        "Most generous tier",
        "The tier that grants the most time per point. The headline Effective Bonus and the Wheel's point value / house edge all use this worst case.",
        mostGenerousTier ? `${mostGenerousTier.points} pts` : "—",
        mostGenerousTier ? `Grants ${tierDuration(mostGenerousTier.minutes)}` : "No valid tier yet",
      )}
      {@render stat(
        "Threshold",
        "When Effective Bonus rises above this percentage, it is flagged red as unsafe — points would be giving away more value than you intend.",
        `${(economy.unsafe_bonus_threshold_basis_points / 100).toFixed(2)}%`,
        "Bonus above this is flagged unsafe",
      )}
    </div>
  {/if}

  <SettingsCard
    title="Earning Points"
    description="How much a customer spends to earn one point. Raising it makes points harder to earn; lowering it makes them easier."
  >
    {#if economyLoadError}
      <Notice tone="danger" class="mb-4">{economyLoadError}</Notice>
    {/if}
    <SettingsField
      id="economy-earn-pesos"
      label="Credits per point"
      hint="Pesos spent to earn 1 point (1 credit = ₱1). Default: 50"
      class="max-w-xs"
    >
      <div class="relative">
        <span class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-sm text-muted-foreground" aria-hidden="true">₱</span>
        <Input
          id="economy-earn-pesos"
          type="number"
          min="0.01"
          step="0.01"
          class="pl-6 tabular-nums"
          aria-describedby="economy-earn-pesos-hint"
          bind:value={earnPesos}
        />
      </div>
    </SettingsField>

    {#snippet footer()}
      <Button disabled={economyBusy} onclick={saveEconomy}>
        {economyBusy ? "Saving…" : "Save earning rate"}
      </Button>
    {/snippet}
  </SettingsCard>

  <SettingsCard
    title="Redemption Tiers"
    description="1–20 guaranteed exchanges of points for time. Points must be unique multiples of 5."
    flush
  >
    {#snippet actions()}
      <Button
        variant="outline"
        size="sm"
        disabled={redemptionTiers.length >= 20 || tiersBusy}
        onclick={addRedemptionTier}
      >
        <PlusIcon aria-hidden="true" />
        Add tier
      </Button>
    {/snippet}

    {#if tiersLoadError}
      <div class="px-4 pt-4 md:px-6">
        <Notice tone="danger">{tiersLoadError}</Notice>
      </div>
    {/if}

    {#if redemptionTiers.length === 0}
      {#if !tiersLoadError}
        <p class="px-4 py-10 text-center text-sm text-muted-foreground">
          No Redemption Tiers yet. Add one so customers can exchange points for time.
        </p>
      {/if}
    {:else}
      <!-- Columns from 768px; below that each tier is a two-line row with
           its inputs side by side and the derived values underneath. -->
      <div
        class="hidden grid-cols-[8rem_8rem_minmax(0,1fr)_minmax(0,1fr)_2.75rem] items-center gap-4 border-b bg-muted/40 px-6 py-2 text-xs font-medium text-muted-foreground md:grid"
      >
        <span>Points{@render infoTip("About tier points", "Points a customer spends for this tier — whole numbers, multiples of 5, at least 5, unique across rows.")}</span>
        <span>Minutes{@render infoTip("About tier minutes", "Time this tier grants, in whole minutes. The Duration column shows the HH:MM:SS equivalent.")}</span>
        <span>Duration</span>
        <span>Effective Bonus{@render infoTip("About a tier's Effective Bonus", "What this row really costs you: the time it grants valued at the coin rate, versus the pesos spent earning its points.")}</span>
        <span class="sr-only">Actions</span>
      </div>
      <ul class="divide-y">
        <!-- Authoring order, not sorted order: re-sorting on each keystroke
             moves the focused row's DOM node and drops the caret. The
             server returns tiers sorted, so the list settles on save. -->
        {#each redemptionTiers as tier (tier)}
          {@const bonus = tierBonusBasisPoints(tier)}
          <li
            class="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-end gap-x-3 gap-y-2 px-4 py-3 md:grid-cols-[8rem_8rem_minmax(0,1fr)_minmax(0,1fr)_2.75rem] md:items-center md:gap-4 md:px-6"
          >
            <div class="grid gap-1">
              <span class="text-xs text-muted-foreground md:hidden" aria-hidden="true">Points</span>
              <Input aria-label="Tier points" type="number" min="5" step="5" class="tabular-nums" bind:value={tier.points} />
            </div>
            <div class="grid gap-1">
              <span class="text-xs text-muted-foreground md:hidden" aria-hidden="true">Minutes</span>
              <Input aria-label="Tier minutes" type="number" min="1" step="1" class="tabular-nums" bind:value={tier.minutes} />
            </div>
            <p class="col-span-2 row-start-2 flex flex-wrap gap-x-4 gap-y-1 font-mono text-sm text-muted-foreground tabular-nums md:contents">
              <span class="md:col-start-3 md:row-start-1">{tierDuration(tier.minutes)}</span>
              <span class="md:col-start-4 md:row-start-1">
                <span class="mr-1 font-sans whitespace-nowrap md:hidden">Effective Bonus</span>
                <span class="text-foreground">{bonus === null ? "—" : formatBonusPercent(bonus)}</span>
              </span>
            </p>
            <Button
              variant="ghost"
              size="icon-sm"
              class="col-start-3 row-start-1 self-end text-muted-foreground hover:text-destructive md:col-start-5 md:self-center"
              aria-label="Remove {tier.points}-point tier"
              title="Remove tier"
              disabled={tiersBusy || redemptionTiers.length <= 1}
              onclick={() => removeRedemptionTier(tier)}
            >
              <Trash2Icon aria-hidden="true" />
            </Button>
          </li>
        {/each}
      </ul>
    {/if}

    {#snippet footer()}
      <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">
        Safety warnings use the tier with the most time per point.
      </p>
      <Button disabled={tiersBusy} onclick={saveRedemptionTiers}>
        {tiersBusy ? "Saving…" : "Save tiers"}
      </Button>
    {/snippet}
  </SettingsCard>
</div>

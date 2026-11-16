<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { formatDate, formatFractionalSeconds, formatProseDuration } from "../../format";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Switch } from "../../components/ui/switch";
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import NativeSelect from "../../components/settings/NativeSelect.svelte";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import SettingsField from "../../components/settings/SettingsField.svelte";
  import ToggleRow from "../../components/settings/ToggleRow.svelte";
  import CopyIcon from "@lucide/svelte/icons/copy";
  import type {
    ShopHoursDayMode,
    ShopHoursSettingsInput,
    ShopHoursSettings,
    ShopOverrideInput,
    ShopRate,
    ShopStatus,
    PulseMapEntry,
  } from "../../types";

  let { onShopRateSaved }: { onShopRateSaved?: () => void | Promise<void> } = $props();

  // Shop
  let shopName = $state("");
  let shutdownGraceSeconds = $state(30);
  let offlineGraceSeconds = $state(120);
  let shopBusy = $state(false);
  let shopLoadError = $state("");
  let shopRateAmountPesos = $state(1);
  let shopRateSeconds = $state(300);
  let shopRateLoaded = $state(false);
  let shopRateBusy = $state(false);
  let shopRateLoadError = $state("");
  let coinPulseMap = $state<PulseMapEntry[]>([]);
  let coinPulseMapLoaded = $state(false);
  let coinPulseMapLoadError = $state("");
  type ShopDayDraft = {
    weekday: number;
    mode: ShopHoursDayMode;
    openTime: string;
    closeTime: string;
  };
  const weekdayLabels = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
  let shopHoursEnabled = $state(false);
  let lastCallLeadMinutes = $state(15);
  let shopDays = $state<ShopDayDraft[]>(defaultShopDays());
  let shopHoursOverride = $state<ShopHoursSettings["override"]>(null);
  let shopOverrideMode = $state<ShopOverrideInput["mode"]>("force_closed");
  let shopOverrideExpiresAt = $state("");
  let shopOverrideBusy = $state(false);
  let shopHoursStatus = $state<ShopStatus>({
    open: true,
    next_change_at: null,
    reason: "schedule",
    last_call_active: false,
  });
  let shopHoursLoaded = $state(false);
  let shopHoursBusy = $state(false);

  function minuteToTime(minute: number): string {
    const hours = Math.floor(minute / 60).toString().padStart(2, "0");
    const minutes = (minute % 60).toString().padStart(2, "0");
    return `${hours}:${minutes}`;
  }

  function timeToMinute(time: string): number {
    const [hours, minutes] = time.split(":").map(Number);
    return Number.isInteger(hours) && Number.isInteger(minutes)
      ? hours * 60 + minutes
      : -1;
  }

  function defaultShopDays(): ShopDayDraft[] {
    return weekdayLabels.map((_, weekday) => ({
      weekday,
      mode: "open_period",
      openTime: "08:00",
      closeTime: "22:00",
    }));
  }

  function toLocalDateTimeValue(value: string): string {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return "";
    const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
    return local.toISOString().slice(0, 16);
  }

  function defaultOverrideExpiry(status: ShopStatus): string {
    return toLocalDateTimeValue(
      status.next_change_at ?? new Date(Date.now() + 60 * 60_000).toISOString(),
    );
  }

  function applyShopRate(rate: ShopRate) {
    shopRateAmountPesos = rate.amount_cents / 100;
    shopRateSeconds = rate.seconds;
    shopRateLoaded = true;
  }

  function formatCoinDenomination(amountCents: number): string {
    const pesos = amountCents / 100;
    return Number.isInteger(pesos) ? `₱${pesos}` : `₱${pesos.toFixed(2)}`;
  }

  let coinDenominationPreviews = $derived.by(() => {
    if (
      !coinPulseMapLoaded ||
      !Number.isSafeInteger(shopRateAmountPesos) ||
      shopRateAmountPesos <= 0 ||
      !Number.isSafeInteger(shopRateSeconds) ||
      shopRateSeconds <= 0
    ) {
      return [];
    }
    const rateAmountCents = shopRateAmountPesos * 100;
    if (!Number.isSafeInteger(rateAmountCents)) return [];
    const seenAmounts = new Set<number>();
    return coinPulseMap.flatMap((entry) => {
      const amountCents = entry.pulse_count * 100;
      if (!Number.isSafeInteger(amountCents) || seenAmounts.has(amountCents)) return [];
      seenAmounts.add(amountCents);
      const numerator = amountCents * shopRateSeconds;
      return [{
        ...entry,
        amount_cents: amountCents,
        seconds: Math.floor(numerator / rateAmountCents),
        remainder: numerator % rateAmountCents,
        denominator: rateAmountCents,
      }];
    });
  });

  function overrideExpiryRfc3339(value: string): string {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      throw new Error("Choose a valid Shop Override expiry");
    }
    return date.toISOString();
  }

  function applyShopHours(settings: ShopHoursSettings) {
    shopHoursLoaded = true;
    shopHoursEnabled = settings.enabled;
    lastCallLeadMinutes = settings.last_call_lead_minutes;
    shopDays = settings.days.map((day) => ({
      weekday: day.weekday,
      mode: day.mode,
      openTime: minuteToTime(day.open_minute ?? 480),
      closeTime: minuteToTime(day.close_minute ?? 1320),
    }));
    shopHoursOverride = settings.override;
    shopHoursStatus = settings.status;
    const activeOverride = settings.status.reason === "override" ? settings.override : null;
    shopOverrideMode = activeOverride?.mode ?? "force_closed";
    shopOverrideExpiresAt = activeOverride
      ? toLocalDateTimeValue(activeOverride.expires_at)
      : defaultOverrideExpiry(settings.status);
  }

  async function loadShop() {
    shopLoadError = "";
    try {
      const settings = await api.getSettings();
      shopName = settings.shop_name;
      shutdownGraceSeconds = settings.shutdown_grace_seconds;
      offlineGraceSeconds = settings.offline_grace_seconds;
    } catch (caught) {
      shopLoadError = caught instanceof Error ? caught.message : String(caught);
    }
    try {
      applyShopHours(await api.getShopHours());
    } catch (caught) {
      shopLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function loadShopRate() {
    shopRateLoadError = "";
    try {
      applyShopRate(await api.getShopRate());
    } catch (caught) {
      shopRateLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function loadCoinPulseMap() {
    coinPulseMapLoadError = "";
    try {
      coinPulseMap = (await api.getCoinDeviceSettings()).pulse_map.map((entry) => ({ ...entry }));
      coinPulseMapLoaded = true;
    } catch (caught) {
      coinPulseMapLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function copyShopDayToAll(weekday: number) {
    const source = shopDays.find((day) => day.weekday === weekday);
    if (!source) return;
    shopDays = shopDays.map((day) => ({ ...source, weekday: day.weekday }));
  }

  function shopHoursInput(): ShopHoursSettingsInput {
    return {
      enabled: shopHoursEnabled,
      last_call_lead_minutes: lastCallLeadMinutes,
      days: shopDays.map((day) => ({
        weekday: day.weekday,
        mode: day.mode,
        open_minute: day.mode === "open_period" ? timeToMinute(day.openTime) : undefined,
        close_minute: day.mode === "open_period" ? timeToMinute(day.closeTime) : undefined,
      })),
    };
  }

  async function saveShopHours() {
    shopHoursBusy = true;
    try {
      applyShopHours(await api.updateShopHours(shopHoursInput()));
      reportActionSuccess("save", "Shop Hours");
    } catch (caught) {
      reportActionFailure("save", "Shop Hours", caught);
    } finally {
      shopHoursBusy = false;
    }
  }

  async function saveShopOverride() {
    shopOverrideBusy = true;
    try {
      const input: ShopOverrideInput = {
        mode: shopOverrideMode,
        expires_at: overrideExpiryRfc3339(shopOverrideExpiresAt),
      };
      applyShopHours(await api.setShopOverride(input));
      reportActionSuccess("save", "Shop Override");
    } catch (caught) {
      reportActionFailure("save", "Shop Override", caught);
    } finally {
      shopOverrideBusy = false;
    }
  }

  async function clearShopOverride() {
    shopOverrideBusy = true;
    try {
      applyShopHours(await api.clearShopOverride());
      reportActionSuccess("remove", "Shop Override");
    } catch (caught) {
      reportActionFailure("remove", "Shop Override", caught);
    } finally {
      shopOverrideBusy = false;
    }
  }

  async function saveShopRate() {
    if (
      !Number.isInteger(shopRateAmountPesos) ||
      shopRateAmountPesos < 1 ||
      shopRateAmountPesos > 1_000 ||
      !Number.isInteger(shopRateSeconds) ||
      shopRateSeconds < 1 ||
      shopRateSeconds > 86_400
    ) {
      reportActionFailure(
        "save",
        "Shop Rate",
        new Error("Amount must be 1-1,000 whole pesos and seconds must be 1-86,400"),
      );
      return;
    }
    shopRateBusy = true;
    try {
      applyShopRate(await api.updateShopRate({
        amount_cents: shopRateAmountPesos * 100,
        seconds: shopRateSeconds,
      }));
      await onShopRateSaved?.();
      reportActionSuccess("save", "Shop Rate");
    } catch (caught) {
      reportActionFailure("save", "Shop Rate", caught);
    } finally {
      shopRateBusy = false;
    }
  }

  async function saveShop() {
    shopBusy = true;
    try {
      const settings = await api.updateSettings({
        shop_name: shopName,
        shutdown_grace_seconds: shutdownGraceSeconds,
        offline_grace_seconds: offlineGraceSeconds,
      });
      shopName = settings.shop_name;
      shutdownGraceSeconds = settings.shutdown_grace_seconds;
      offlineGraceSeconds = settings.offline_grace_seconds;
      reportActionSuccess("save", "Shop Name");
    } catch (caught) {
      reportActionFailure("save", "Shop Name", caught);
    } finally {
      shopBusy = false;
    }
  }

  let shopRateValid = $derived(
    Number.isInteger(shopRateAmountPesos) && shopRateAmountPesos >= 1 && shopRateAmountPesos <= 1000 &&
      Number.isInteger(shopRateSeconds) && shopRateSeconds >= 1 && shopRateSeconds <= 86400,
  );

  onMount(() => {
    void loadShop();
    void loadShopRate();
    void loadCoinPulseMap();
  });
</script>

<div class="space-y-6">
  {#if shopLoadError}
    <Notice tone="danger">{shopLoadError}</Notice>
  {/if}

  <SettingsCard
    title="Shop Details"
    description="The shop's name, and how long a Client counts down before it powers off."
  >
    <div class="grid gap-6 md:grid-cols-2">
      <SettingsField
        id="shop-name"
        label="Shop name"
        hint="Shown on the owner dashboard and on every Client's Fullscreen Lock screen. Required, up to 80 characters."
        class="md:col-span-2 md:max-w-md"
      >
        <Input
          id="shop-name"
          placeholder="OpenPiso Shop"
          maxlength={80}
          aria-describedby="shop-name-hint"
          aria-invalid={!shopName.trim() || undefined}
          bind:value={shopName}
        />
      </SettingsField>
      <SettingsField
        id="shutdown-grace-seconds"
        label="Shutdown grace seconds"
        hint="Blocking countdown before the Client powers off after its time expires. 5–300 seconds. Default: 300."
      >
        <Input
          id="shutdown-grace-seconds"
          type="number"
          min="5"
          max="300"
          class="tabular-nums"
          aria-describedby="shutdown-grace-seconds-hint"
          aria-invalid={shutdownGraceSeconds < 5 || shutdownGraceSeconds > 300 || undefined}
          bind:value={shutdownGraceSeconds}
        />
      </SettingsField>
      <SettingsField
        id="offline-grace-seconds"
        label="Offline Grace seconds"
        hint="How long a Client waits before powering off when it can't reach the admin server. Whole seconds, at least 1. Default: 600."
      >
        <Input
          id="offline-grace-seconds"
          type="number"
          min="1"
          class="tabular-nums"
          aria-describedby="offline-grace-seconds-hint"
          aria-invalid={offlineGraceSeconds < 1 || undefined}
          bind:value={offlineGraceSeconds}
        />
      </SettingsField>
    </div>

    {#snippet footer()}
      <Button
        disabled={shopBusy || !shopName.trim() || shutdownGraceSeconds < 5 || shutdownGraceSeconds > 300 || offlineGraceSeconds < 1}
        onclick={saveShop}
      >
        {shopBusy ? "Saving…" : "Save Shop Details"}
      </Button>
    {/snippet}
  </SettingsCard>

  <SettingsCard
    title="Shop Rate"
    description="The paid time a coin buys. Changes apply to new Coin Events; the previous Shop Rate is kept for history."
  >
    {#if shopRateLoadError}
      <Notice tone="danger" class="mb-6">{shopRateLoadError}</Notice>
    {/if}

    <div class="grid max-w-xl gap-6 sm:grid-cols-2">
      <SettingsField id="shop-rate-amount" label="Amount (whole pesos)" hint="₱1–₱1,000.">
        <div class="relative">
          <span class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-sm text-muted-foreground" aria-hidden="true">₱</span>
          <Input
            id="shop-rate-amount"
            type="number"
            min="1"
            max="1000"
            step="1"
            class="pl-6 tabular-nums"
            aria-describedby="shop-rate-amount-hint"
            bind:value={shopRateAmountPesos}
          />
        </div>
      </SettingsField>
      <SettingsField
        id="shop-rate-seconds"
        label="Seconds"
        hint="1–86,400 seconds (= {formatProseDuration(shopRateSeconds)})."
      >
        <Input
          id="shop-rate-seconds"
          type="number"
          min="1"
          max="86400"
          step="1"
          class="tabular-nums"
          aria-describedby="shop-rate-seconds-hint"
          bind:value={shopRateSeconds}
        />
      </SettingsField>
    </div>

    {#if coinPulseMapLoadError}
      <Notice class="mt-6" role="status">
        <p>Coin denomination purchase previews unavailable: {coinPulseMapLoadError}</p>
      </Notice>
    {:else if coinDenominationPreviews.length > 0}
      <div class="mt-6 border-t pt-6" role="status">
        <p class="text-sm font-medium">Configured coin purchase preview</p>
        <ul class="mt-3 grid gap-3 text-sm sm:grid-cols-2 lg:grid-cols-3">
          {#each coinDenominationPreviews as denomination (denomination.amount_cents)}
            <li class="rounded-lg border bg-muted/40 px-3 py-2">
              <span class="font-semibold tabular-nums">{formatCoinDenomination(denomination.amount_cents)}</span>
              buys
              <span class="font-semibold">{formatProseDuration(denomination.seconds)}</span>.
              {#if denomination.remainder > 0}
                <span class="block text-xs text-amber-700 dark:text-amber-300">
                  Loses {formatFractionalSeconds(denomination.remainder, denomination.denominator)}
                  to integer division.
                </span>
              {/if}
            </li>
          {/each}
        </ul>
        {#if coinDenominationPreviews.some((denomination) => denomination.remainder > 0)}
          <p class="mt-3 text-xs text-muted-foreground">Truncation is advisory only; saving remains available.</p>
        {/if}
      </div>
    {/if}

    {#snippet footer()}
      <Button disabled={!shopRateLoaded || shopRateBusy || !shopRateValid} onclick={saveShopRate}>
        {shopRateBusy ? "Saving…" : "Save Shop Rate"}
      </Button>
    {/snippet}
  </SettingsCard>

  <SettingsCard title="Shop Hours" description="The weekly Open Periods, and the Last Call warning before a scheduled close.">
    <div class="space-y-6">
      <ToggleRow
        id="shop-hours-enabled"
        label="Enable Shop Hours"
        description="Off by default. Saved hours don't affect the shop until this is on."
        status={shopHoursEnabled ? "On" : "Off"}
      >
        <Switch id="shop-hours-enabled" aria-describedby="shop-hours-enabled-description" bind:checked={shopHoursEnabled} />
      </ToggleRow>

      <div class="flex flex-wrap items-center justify-between gap-x-6 gap-y-2 rounded-lg border bg-muted/40 px-4 py-3">
        <div class="flex items-center gap-3">
          <span
            class="size-2.5 shrink-0 rounded-full {shopHoursStatus.open ? 'bg-emerald-500' : 'bg-destructive'}"
            aria-hidden="true"
          ></span>
          <div>
            <p class="text-xs text-muted-foreground">Shop Status</p>
            <strong class="font-semibold">{shopHoursStatus.open ? "Shop Open" : "Shop Closed"}</strong>
          </div>
        </div>
        <p class="text-sm text-muted-foreground">
          {#if shopHoursStatus.next_change_at}
            Next change: {formatDate(shopHoursStatus.next_change_at)}
          {:else}
            No scheduled change.
          {/if}
          · {shopHoursStatus.reason === "override" ? "Shop Override" : "Schedule"}
        </p>
      </div>

      <SettingsField
        id="last-call-lead"
        label="Last Call lead minutes"
        hint="How long before a scheduled close the Last Call warning starts. Zero disables it."
        class="max-w-xs"
      >
        <Input
          id="last-call-lead"
          type="number"
          min="0"
          max="65535"
          class="tabular-nums"
          aria-describedby="last-call-lead-hint"
          aria-invalid={lastCallLeadMinutes < 0 || lastCallLeadMinutes > 65535 || undefined}
          bind:value={lastCallLeadMinutes}
        />
      </SettingsField>

      <div class="overflow-hidden rounded-lg border">
        <div
          class="hidden grid-cols-[7rem_minmax(0,12rem)_minmax(0,9rem)_minmax(0,9rem)_minmax(0,1fr)] items-center gap-4 border-b bg-muted/40 px-4 py-2 text-xs font-medium text-muted-foreground md:grid"
          aria-hidden="true"
        >
          <span>Day</span>
          <span>Mode</span>
          <span>Open Period start</span>
          <span>Open Period end</span>
          <span></span>
        </div>
        <ul class="divide-y" aria-label="Weekly Open Periods">
          {#each shopDays as day (day.weekday)}
            {@const dayLabel = weekdayLabels[day.weekday]}
            <li
              class="grid grid-cols-2 gap-x-3 gap-y-2 px-4 py-3 md:grid-cols-[7rem_minmax(0,12rem)_minmax(0,9rem)_minmax(0,9rem)_minmax(0,1fr)] md:items-center md:gap-4"
            >
              <p class="self-center font-medium">{dayLabel}</p>
              <div class="justify-self-end md:col-start-5 md:row-start-1">
                <Button variant="ghost" size="sm" disabled={shopHoursBusy} onclick={() => copyShopDayToAll(day.weekday)}>
                  <CopyIcon aria-hidden="true" />
                  Copy to all days
                </Button>
              </div>
              <NativeSelect
                class="col-span-2 md:col-span-1 md:col-start-2 md:row-start-1"
                bind:value={day.mode}
                aria-label={`${dayLabel} mode`}
              >
                <option value="open_period">Open Period</option>
                <option value="closed_all_day">Closed all day</option>
                <option value="open_24_hours">Open 24 hours</option>
              </NativeSelect>
              {#if day.mode === "open_period"}
                <label class="grid gap-1 md:col-start-3 md:row-start-1">
                  <span class="text-xs text-muted-foreground md:hidden">Start</span>
                  <Input type="time" class="tabular-nums" bind:value={day.openTime} aria-label={`${dayLabel} Open Period start time`} />
                </label>
                <label class="grid gap-1 md:col-start-4 md:row-start-1">
                  <span class="text-xs text-muted-foreground md:hidden">End</span>
                  <Input type="time" class="tabular-nums" bind:value={day.closeTime} aria-label={`${dayLabel} Open Period end time`} />
                </label>
              {:else}
                <p class="col-span-2 text-sm text-muted-foreground md:col-span-2 md:col-start-3 md:row-start-1">
                  No Open Period times to set.
                </p>
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    </div>

    {#snippet footer()}
      <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">Saves the switch, Last Call, and weekly hours.</p>
      <Button
        disabled={!shopHoursLoaded || shopHoursBusy || lastCallLeadMinutes < 0 || lastCallLeadMinutes > 65535}
        onclick={saveShopHours}
      >
        {shopHoursBusy ? "Saving…" : "Save Shop Hours"}
      </Button>
    {/snippet}
  </SettingsCard>

  <SettingsCard title="Shop Override" description="Force the shop open or closed until a set time, instead of following Shop Hours.">
    {#if shopHoursStatus.reason === "override" && shopHoursOverride}
      <div class="mb-6 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-muted/40 px-4 py-3 text-sm">
        <p>
          <span class="font-semibold">{shopHoursOverride.mode === "force_open" ? "Force-open" : "Force-closed"}</span>
          until {formatDate(shopHoursOverride.expires_at)}.
        </p>
        <Button variant="outline" size="sm" disabled={shopOverrideBusy} onclick={clearShopOverride}>
          {shopOverrideBusy ? "Clearing…" : "Clear Shop Override"}
        </Button>
      </div>
    {:else}
      <p class="mb-6 text-sm text-muted-foreground">No active Shop Override.</p>
    {/if}

    <div class="grid max-w-xl gap-6 sm:grid-cols-2">
      <SettingsField id="shop-override-mode" label="Mode">
        <NativeSelect id="shop-override-mode" bind:value={shopOverrideMode}>
          <option value="force_closed">Force-closed</option>
          <option value="force_open">Force-open</option>
        </NativeSelect>
      </SettingsField>
      <SettingsField id="shop-override-expires-at" label="Expires at" hint="Pre-filled with the next scheduled boundary.">
        <Input
          id="shop-override-expires-at"
          type="datetime-local"
          class="tabular-nums"
          aria-describedby="shop-override-expires-at-hint"
          bind:value={shopOverrideExpiresAt}
        />
      </SettingsField>
    </div>

    {#snippet footer()}
      <Button disabled={!shopHoursLoaded || shopOverrideBusy || !shopOverrideExpiresAt} onclick={saveShopOverride}>
        {shopOverrideBusy ? "Saving…" : "Set Shop Override"}
      </Button>
    {/snippet}
  </SettingsCard>
</div>

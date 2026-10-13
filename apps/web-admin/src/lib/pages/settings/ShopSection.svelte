<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { formatDate, formatFractionalSeconds, formatProseDuration } from "../../format";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Switch } from "../../components/ui/switch";
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

  onMount(() => {
    void loadShop();
    void loadShopRate();
    void loadCoinPulseMap();
  });
</script>

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">Shop</h3>
  <p class="mb-4 text-sm text-muted-foreground">
    The shop name is shown on the owner dashboard and on every Client's
    Fullscreen Lock screen. Shutdown Grace controls the blocking countdown
    after time expires; Offline Grace controls the countdown after a Client
    loses the admin server.
  </p>

  <div class="space-y-4">
    <div>
      <label class="block text-sm font-medium" for="shop-name">Shop name</label>
      <input
        id="shop-name"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        placeholder="OpenPiso Shop"
        bind:value={shopName}
        maxlength="80"
      />
      <p class="mt-1 text-xs text-muted-foreground">
        Non-empty, ≤ 80 characters. Persisted with a <code>shop_name_updated</code> audit event.
      </p>
    </div>
    <div>
      <label class="block text-sm font-medium" for="shutdown-grace-seconds">Shutdown grace seconds</label>
      <input
        id="shutdown-grace-seconds"
        type="number"
        min="5"
        max="300"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        bind:value={shutdownGraceSeconds}
      />
      <p class="mt-1 text-xs text-muted-foreground">
        Blocking countdown before the Client powers off after expiry. Default: 300 seconds. Valid range: 5-300 seconds.
      </p>
    </div>
    <div>
      <label class="block text-sm font-medium" for="offline-grace-seconds">Offline Grace seconds</label>
      <input
        id="offline-grace-seconds"
        type="number"
        min="1"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        bind:value={offlineGraceSeconds}
      />
      <p class="mt-1 text-xs text-muted-foreground">
        How long a Client waits before powering off when the admin server is unreachable. Default: 600 seconds. Positive integer seconds; distinct from Shutdown Grace.
      </p>
    </div>
  </div>

  <button
    class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
    type="button"
    disabled={shopBusy || !shopName.trim() || shutdownGraceSeconds < 5 || shutdownGraceSeconds > 300 || offlineGraceSeconds < 1}
    onclick={saveShop}
  >
    {shopBusy ? "Saving…" : "Save"}
  </button>
</div>

{#if shopLoadError}
  <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{shopLoadError}</div>
{/if}

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">Shop Rate</h3>
  <p class="mb-4 text-sm text-muted-foreground">
    The amount of paid time a coin buys. Changes apply to new Coin Events and preserve the previous Shop Rate for history.
  </p>

  <div class="grid gap-4 sm:grid-cols-2">
    <div>
      <label class="block text-sm font-medium" for="shop-rate-amount">Amount (whole pesos)</label>
      <input
        id="shop-rate-amount"
        type="number"
        min="1"
        max="1000"
        step="1"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        bind:value={shopRateAmountPesos}
      />
      <p class="mt-1 text-xs text-muted-foreground">Valid range: ₱1-₱1,000.</p>
    </div>
    <div>
      <label class="block text-sm font-medium" for="shop-rate-seconds">Seconds</label>
      <input
        id="shop-rate-seconds"
        type="number"
        min="1"
        max="86400"
        step="1"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        bind:value={shopRateSeconds}
      />
      <p class="mt-1 text-xs text-muted-foreground">Valid range: 1-86,400 seconds. = {formatProseDuration(shopRateSeconds)}</p>
    </div>
  </div>

  {#if coinPulseMapLoadError}
    <div class="mt-4 rounded-md bg-muted p-3 text-sm text-muted-foreground" role="status">
      Coin denomination purchase previews unavailable: {coinPulseMapLoadError}
    </div>
  {:else if coinDenominationPreviews.length > 0}
    <div class="mt-4 rounded-md border p-4 text-sm" role="status">
      <p class="font-medium">Configured coin purchase preview:</p>
      <ul class="mt-2 list-inside list-disc space-y-1">
        {#each coinDenominationPreviews as denomination (denomination.amount_cents)}
          <li>
            {formatCoinDenomination(denomination.amount_cents)} buys
            {formatProseDuration(denomination.seconds)}.
            {#if denomination.remainder > 0}
              <span class="text-amber-700 dark:text-amber-300">
                Loses {formatFractionalSeconds(denomination.remainder, denomination.denominator)}
                to integer division.
              </span>
            {/if}
          </li>
        {/each}
      </ul>
      {#if coinDenominationPreviews.some((denomination) => denomination.remainder > 0)}
        <p class="mt-2 text-muted-foreground">Truncation is advisory only; saving remains available.</p>
      {/if}
    </div>
  {/if}

  {#if shopRateLoadError}
    <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">{shopRateLoadError}</div>
  {/if}

  <button
    class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
    type="button"
    disabled={!shopRateLoaded || shopRateBusy || !Number.isInteger(shopRateAmountPesos) || shopRateAmountPesos < 1 || shopRateAmountPesos > 1000 || !Number.isInteger(shopRateSeconds) || shopRateSeconds < 1 || shopRateSeconds > 86400}
    onclick={saveShopRate}
  >
    {shopRateBusy ? "Saving…" : "Save Shop Rate"}
  </button>
</div>

<div class="rounded-lg border bg-card p-6">
  <div class="mb-4 flex flex-wrap items-start justify-between gap-4">
    <div>
      <h3 class="mb-2 text-lg font-semibold">Shop Hours</h3>
      <p class="text-sm text-muted-foreground">
        Configure the weekly Open Periods. The feature ships disabled, so
        these saved hours do not affect the shop until enabled.
      </p>
    </div>
    <label class="flex shrink-0 items-center gap-3 text-sm font-medium" for="shop-hours-enabled">
      Enable Shop Hours
      <Switch id="shop-hours-enabled" size="sm" bind:checked={shopHoursEnabled} />
    </label>
  </div>

  <div class="mb-5 rounded-md bg-muted p-4">
    <div class="flex flex-wrap items-baseline justify-between gap-2">
      <h4 class="font-semibold">Shop Status</h4>
      <strong class:text-primary={shopHoursStatus.open} class:text-destructive={!shopHoursStatus.open}>
        {shopHoursStatus.open ? "Shop Open" : "Shop Closed"}
      </strong>
    </div>
    <p class="mt-1 text-sm text-muted-foreground">
      {#if shopHoursStatus.next_change_at}
        Next change: {formatDate(shopHoursStatus.next_change_at)}
      {:else}
        No scheduled change.
      {/if}
      · {shopHoursStatus.reason === "override" ? "Shop Override" : "Schedule"}
    </p>
  </div>

  <div>
    <label class="block text-sm font-medium" for="last-call-lead">Last Call lead minutes</label>
    <input
      id="last-call-lead"
      type="number"
      min="0"
      max="65535"
      class="mt-1 block w-40 rounded-md border bg-background px-3 py-2 text-sm"
      bind:value={lastCallLeadMinutes}
    />
    <p class="mt-1 text-xs text-muted-foreground">Zero disables the scheduled-close warning.</p>
  </div>

  <div class="mt-6 overflow-x-auto">
    <table class="w-full text-left text-sm">
      <thead>
        <tr class="border-b text-muted-foreground">
          <th class="px-2 py-2 font-medium">Day</th>
          <th class="px-2 py-2 font-medium">Mode</th>
          <th class="px-2 py-2 font-medium">Open Period start</th>
          <th class="px-2 py-2 font-medium">Open Period end</th>
          <th class="px-2 py-2 font-medium"><span class="sr-only">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each shopDays as day (day.weekday)}
          <tr class="border-b last:border-0">
            <th class="whitespace-nowrap px-2 py-3 font-medium">{weekdayLabels[day.weekday]}</th>
            <td class="px-2 py-3">
              <select class="rounded-md border bg-background px-2 py-2" bind:value={day.mode} aria-label={`${weekdayLabels[day.weekday]} mode`}>
                <option value="open_period">Open Period</option>
                <option value="closed_all_day">Closed all day</option>
                <option value="open_24_hours">Open 24 hours</option>
              </select>
            </td>
            <td class="px-2 py-3">
              {#if day.mode === "open_period"}
                <input class="rounded-md border bg-background px-2 py-2" type="time" bind:value={day.openTime} aria-label={`${weekdayLabels[day.weekday]} Open Period start time`} />
              {:else}
                <span class="text-muted-foreground">—</span>
              {/if}
            </td>
            <td class="px-2 py-3">
              {#if day.mode === "open_period"}
                <input class="rounded-md border bg-background px-2 py-2" type="time" bind:value={day.closeTime} aria-label={`${weekdayLabels[day.weekday]} Open Period end time`} />
              {:else}
                <span class="text-muted-foreground">—</span>
              {/if}
            </td>
            <td class="px-2 py-3 text-right">
              <button
                class="whitespace-nowrap rounded-md border px-2 py-2 text-xs font-medium hover:bg-accent disabled:opacity-50"
                type="button"
                disabled={shopHoursBusy}
                onclick={() => copyShopDayToAll(day.weekday)}
              >
                Copy to all days
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  {#if shopHoursStatus.reason === "override" && shopHoursOverride}
    <p class="mt-4 flex flex-wrap items-center justify-between gap-3 rounded-md bg-muted p-3 text-sm">
      <span>
        {shopHoursOverride.mode === "force_open" ? "Force-open" : "Force-closed"}
        until {formatDate(shopHoursOverride.expires_at)}.
      </span>
      <button
        class="rounded-md border px-3 py-1.5 text-xs font-medium hover:bg-accent disabled:opacity-50"
        type="button"
        disabled={shopOverrideBusy}
        onclick={clearShopOverride}
      >
        {shopOverrideBusy ? "Clearing…" : "Clear Shop Override"}
      </button>
    </p>
  {:else}
    <p class="mt-4 text-sm text-muted-foreground">No active Shop Override.</p>
  {/if}

  <div class="mt-5 space-y-4 rounded-md border p-4">
    <h4 class="font-semibold">Shop Override</h4>
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="block text-sm font-medium" for="shop-override-mode">Mode</label>
        <select id="shop-override-mode" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={shopOverrideMode}>
          <option value="force_closed">Force-closed</option>
          <option value="force_open">Force-open</option>
        </select>
      </div>
      <div>
        <label class="block text-sm font-medium" for="shop-override-expires-at">Expires at</label>
        <input
          id="shop-override-expires-at"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          type="datetime-local"
          bind:value={shopOverrideExpiresAt}
        />
        <p class="mt-1 text-xs text-muted-foreground">Pre-filled with the next scheduled boundary.</p>
      </div>
    </div>
    <button
      class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
      type="button"
      disabled={!shopHoursLoaded || shopOverrideBusy || !shopOverrideExpiresAt}
      onclick={saveShopOverride}
    >
      {shopOverrideBusy ? "Saving…" : "Set Shop Override"}
    </button>
  </div>

  <button
    class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
    type="button"
    disabled={!shopHoursLoaded || shopHoursBusy || lastCallLeadMinutes < 0 || lastCallLeadMinutes > 65535}
    onclick={saveShopHours}
  >
    {shopHoursBusy ? "Saving…" : "Save Shop Hours"}
  </button>
</div>

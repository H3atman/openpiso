<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import {
    formatBonusPercent,
    formatCentsAsPesosInput,
    formatDate,
    formatMoney,
    formatMoneyCompact,
    formatProseDuration,
    parsePesosToCents,
  } from "../../format";
  import {
    reportActionFailure,
    reportAttendanceSettingsOutcome,
  } from "../../notifications";
  import { Badge } from "../../components/ui/badge";
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import { Switch } from "../../components/ui/switch";
  import type {
    AttendanceEconomicsAdvisory,
    AttendanceSettings,
    AttendanceSettingsDraft,
    AttendanceSettingsEffectiveChange,
    AttendanceSettingsPreview,
  } from "../../types";

  let settings = $state<AttendanceSettings | null>(null);
  let enabled = $state(false);
  let thresholdPesos = $state("");
  let rewardMinutes = $state("");
  let claimWindowMonths = $state("");
  let preview = $state<AttendanceSettingsPreview | null>(null);
  let loadError = $state("");
  let previewError = $state("");
  let busy = $state(false);
  let previewSequence = 0;

  function applySettings(next: AttendanceSettings) {
    settings = next;
    enabled = next.enabled;
    thresholdPesos = formatCentsAsPesosInput(next.daily_threshold_cents);
    rewardMinutes = (next.reward_seconds / 60).toString();
    claimWindowMonths = next.claim_window_months.toString();
    preview = null;
  }

  async function loadSettings(preserveDraft = false) {
    loadError = "";
    try {
      const next = await api.getAttendanceSettings();
      if (preserveDraft) settings = next;
      else applySettings(next);
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function parsedDraft(): { value: AttendanceSettingsDraft | null; error: string } {
    const dailyThresholdCents = parsePesosToCents(thresholdPesos);
    if (dailyThresholdCents === null) {
      return { value: null, error: "Enter a peso amount with at most two decimals" };
    }

    if (!/^\d+$/.test(rewardMinutes)) {
      return { value: null, error: "Enter reward minutes as a whole number" };
    }
    const minutes = Number(rewardMinutes);
    if (!Number.isFinite(minutes) || !Number.isInteger(minutes)) {
      return { value: null, error: "Enter reward minutes as a whole number" };
    }

    if (!/^\d+$/.test(claimWindowMonths)) {
      return { value: null, error: "Enter the claim window as a whole number of months" };
    }
    const months = Number(claimWindowMonths);
    if (!Number.isFinite(months) || !Number.isInteger(months)) {
      return { value: null, error: "Enter the claim window as a whole number of months" };
    }

    return {
      value: {
        enabled,
        daily_threshold_cents: dailyThresholdCents,
        reward_minutes: minutes,
        claim_window_months: months,
      },
      error: "",
    };
  }

  let draft = $derived(parsedDraft());
  let changed = $derived.by(() => {
    if (!settings || !draft.value) return false;
    return draft.value.enabled !== settings.enabled ||
      draft.value.daily_threshold_cents !== settings.daily_threshold_cents ||
      draft.value.reward_minutes * 60 !== settings.reward_seconds ||
      draft.value.claim_window_months !== settings.claim_window_months;
  });
  let canCommit = $derived(
    settings !== null && changed && !draft.error && preview !== null && !busy,
  );

  async function refreshPreview(input: AttendanceSettingsDraft, sequence: number) {
    previewError = "";
    try {
      const nextPreview = await api.previewAttendanceSettings(input);
      if (sequence === previewSequence) preview = nextPreview;
    } catch (caught) {
      if (sequence === previewSequence) {
        previewError = caught instanceof Error ? caught.message : String(caught);
      }
    }
  }

  $effect(() => {
    const input = parsedDraft();
    const current = settings;
    const requestSequence = ++previewSequence;
    preview = null;
    previewError = "";
    const draftValue = input.value;
    if (!current || !draftValue || !changed || busy) return;

    const timer = setTimeout(
      () => void refreshPreview(draftValue, requestSequence),
      200,
    );
    return () => clearTimeout(timer);
  });

  async function saveSettings() {
    if (!settings || !draft.value || !canCommit) return;
    busy = true;
    try {
      const updated = await api.updateAttendanceSettings({
        ...draft.value,
        expected_version: settings.version,
      });
      applySettings(updated);
      reportAttendanceSettingsOutcome(updated.outcome ?? "updated");
    } catch (caught) {
      if (caught instanceof api.AttendanceSettingsConflictError) {
        await loadSettings(true);
        reportActionFailure(
          "save",
          "Attendance",
          caught,
        );
      } else if (caught instanceof api.AttendanceSettingsValidationError) {
        reportActionFailure("save", "Attendance", caught);
      } else {
        reportActionFailure("save", "Attendance", caught);
      }
    } finally {
      busy = false;
    }
  }

  function bonusLabel(economics: AttendanceEconomicsAdvisory | null): string {
    if (!economics) return "Unavailable";
    if (economics.effective_bonus.kind === "infinite") return "Infinite";
    return formatBonusPercent(economics.effective_bonus.basis_points);
  }

  function boughtGivenSentence(economics: AttendanceEconomicsAdvisory | null): string {
    if (!economics) return "Shop Rate economics are unavailable.";
    const boughtSeconds = economics.effective_bonus.kind === "finite"
      ? economics.effective_bonus.base_seconds
      : 0;
    return `${formatMoneyCompact(economics.daily_threshold_cents)} buys ${formatProseDuration(boughtSeconds)}; Attendance Reward gives ${formatProseDuration(economics.reward_seconds)}.`;
  }

  function effectiveChangeLabel(change: AttendanceSettingsEffectiveChange): string {
    switch (change) {
      case "immediately_for_new_contributions":
        return "immediately for new contributions";
      case "next_unfrozen_daily_snapshot":
        return "at the next unfrozen daily snapshot";
      case "next_earning_month":
        return `next Earning Month (${settings?.next_earning_month ?? "pending"})`;
    }
  }

  onMount(() => {
    void loadSettings();
  });
</script>

<div class="grid items-start gap-6 xl:grid-cols-2">
  <section class="rounded-lg border bg-card p-6" aria-labelledby="attendance-exposure-heading">
    <div class="mb-4">
      <h3 id="attendance-exposure-heading" class="text-lg font-semibold">Current Attendance Exposure</h3>
      <p class="text-sm text-muted-foreground">
        Active settings and Outstanding Attendance Time are shop-wide, independent of report filters.
      </p>
    </div>

    {#if loadError}
      <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive" role="alert">{loadError}</div>
    {:else if settings}
      <div class="grid gap-3 sm:grid-cols-2">
        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Attendance Reward</p>
          <div class="mt-2"><Badge variant={settings.enabled ? "default" : "secondary"}>{settings.enabled ? "Enabled" : "Disabled"}</Badge></div>
        </div>
        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Daily threshold</p>
          <p class="mt-2 font-mono font-semibold">{formatMoney(settings.daily_threshold_cents)}</p>
        </div>
        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Reward</p>
          <p class="mt-2 font-semibold">{formatProseDuration(settings.reward_seconds)}</p>
        </div>
        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Claim window</p>
          <p class="mt-2 font-semibold">{settings.claim_window_months} {settings.claim_window_months === 1 ? "month" : "months"}</p>
        </div>
      </div>

      <div class="mt-4 grid gap-3 sm:grid-cols-2">
        <div class="rounded-md border p-3 text-sm">
          <p class="text-muted-foreground">Outstanding Attendance Time</p>
          <p class="mt-1 font-semibold">{formatProseDuration(settings.liability.outstanding_seconds)}</p>
        </div>
        <div class="rounded-md border p-3 text-sm">
          <p class="text-muted-foreground">Claimable Attendance Time</p>
          <p class="mt-1 font-semibold">{formatProseDuration(settings.liability.claimable_seconds)}</p>
        </div>
      </div>

      <div class="mt-4 rounded-md border p-4 text-sm">
        <p class="text-muted-foreground">Current Attendance Effective Bonus</p>
        <p class="mt-1 font-mono text-lg font-semibold">
          {bonusLabel(settings.economics_advisory)}
          {#if settings.economics_advisory?.unsafe_bonus}
            <Badge variant="destructive" class="ml-2">Unsafe</Badge>
          {/if}
        </p>
        <p class="mt-2 text-muted-foreground">{boughtGivenSentence(settings.economics_advisory)}</p>
      </div>
      <p class="mt-3 text-xs text-muted-foreground">
        Version {settings.version}, effective {formatDate(settings.effective_at)}. Claim window changes begin with the next Earning Month ({settings.next_earning_month}).
      </p>
    {/if}
  </section>

  {#if settings}
    <section class="rounded-lg border bg-card p-6" aria-labelledby="attendance-configure-heading">
      <div class="mb-5 flex flex-wrap items-start justify-between gap-4">
        <div>
          <h3 id="attendance-configure-heading" class="text-lg font-semibold">Configure Attendance Reward</h3>
          <p class="text-sm text-muted-foreground">
            Preview the draft with the active Shop Rate before committing.
          </p>
        </div>
        <div>
          <label class="flex items-center gap-2 text-sm font-medium" for="attendance-enabled">
            <Switch id="attendance-enabled" size="sm" bind:checked={enabled} />
            Enable Attendance Reward
          </label>
          <p class="mt-1 max-w-sm text-xs text-muted-foreground">
            Disabling stops new Attendance Check-Ins; earned Attendance Time remains claimable until its deadline.
          </p>
        </div>
      </div>

      <div class="grid gap-4 sm:grid-cols-3">
        <div>
          <label for="attendance-threshold" class="mb-1 block text-sm font-medium">Daily threshold (pesos)</label>
          <Input id="attendance-threshold" aria-label="Daily threshold (pesos)" type="text" inputmode="decimal" bind:value={thresholdPesos} />
          <p class="mt-1 text-xs text-muted-foreground">Default: ₱10</p>
        </div>
        <div>
          <label for="attendance-reward-minutes" class="mb-1 block text-sm font-medium">Reward minutes</label>
          <Input id="attendance-reward-minutes" aria-label="Reward minutes" type="number" step="1" bind:value={rewardMinutes} />
          <p class="mt-1 text-xs text-muted-foreground">Default: 60 minutes</p>
        </div>
        <div>
          <label for="attendance-claim-window" class="mb-1 block text-sm font-medium">Claim window (months)</label>
          <Input id="attendance-claim-window" aria-label="Claim window (months)" type="number" step="1" bind:value={claimWindowMonths} />
          <p class="mt-1 text-xs text-muted-foreground">Default: 1 month</p>
        </div>
      </div>

      {#if draft.error}
        <p class="mt-3 text-sm text-destructive" role="alert">{draft.error}</p>
      {/if}

      <div class="mt-4 rounded-md bg-muted p-4" aria-live="polite">
        <h4 class="font-semibold">Draft Exposure Preview</h4>
        {#if preview}
          <div class="mt-2 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
            <span>
              Attendance Effective Bonus:
              <del aria-label="Active Attendance Effective Bonus">{bonusLabel(settings.economics_advisory)}</del>
              <span aria-hidden="true"> → </span>
              <strong>{bonusLabel(preview.economics_advisory)}</strong>
            </span>
            <span>Threshold: <strong>{formatMoney(preview.daily_threshold_cents)}</strong></span>
            <span>Reward: <strong>{formatProseDuration(preview.reward_seconds)}</strong></span>
            {#if preview.economics_advisory?.unsafe_bonus}
              <Badge variant="destructive">Unsafe bonus</Badge>
            {/if}
          </div>
          <p class="mt-2 text-sm text-muted-foreground">{boughtGivenSentence(preview.economics_advisory)}</p>
        {:else}
          <p class="mt-2 text-sm text-muted-foreground">
            Active Attendance Effective Bonus:
            <del aria-label="Active Attendance Effective Bonus">{bonusLabel(settings.economics_advisory)}</del>
          </p>
          {#if previewError}
            <p class="mt-2 text-sm text-destructive" role="alert">{previewError}</p>
          {:else if changed && !draft.error}
            <p class="mt-2 text-sm text-muted-foreground" aria-live="polite">Calculating preview…</p>
          {:else}
            <p class="mt-2 text-sm text-muted-foreground">Edit a setting to preview its effective bonus before saving.</p>
          {/if}
        {/if}
      </div>

      <p class="mt-4 rounded-md border p-3 text-sm text-muted-foreground">
        Existing daily snapshots are unchanged; threshold and reward edits apply to the next unfrozen daily snapshot.
      </p>

      <div class="mt-4 rounded-md border p-4">
        <h4 class="font-semibold">When this commit takes effect</h4>
        <ul class="mt-2 grid gap-1 text-sm text-muted-foreground sm:grid-cols-2">
          <li>Enable or disable: {effectiveChangeLabel(settings.effective_changes.enabled)}</li>
          <li>Daily threshold: {effectiveChangeLabel(settings.effective_changes.daily_threshold_cents)}</li>
          <li>Reward minutes: {effectiveChangeLabel(settings.effective_changes.reward_minutes)}</li>
          <li>Claim window: {effectiveChangeLabel(settings.effective_changes.claim_window_months)}</li>
        </ul>
      </div>

      <Button class="mt-5" disabled={!canCommit} onclick={saveSettings}>
        {busy ? "Saving…" : `Commit v${settings.version + 1}`}
      </Button>
    </section>
  {/if}
</div>

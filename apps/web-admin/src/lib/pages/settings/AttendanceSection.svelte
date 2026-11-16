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
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import SettingsField from "../../components/settings/SettingsField.svelte";
  import StatePill from "../../components/settings/StatePill.svelte";
  import ToggleRow from "../../components/settings/ToggleRow.svelte";
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

  // Which fields the draft error refers to, so each can be marked invalid.
  // Number inputs bind numbers, so they are stringified before the check.
  let thresholdInvalid = $derived(parsePesosToCents(thresholdPesos) === null);
  let rewardInvalid = $derived(!/^\d+$/.test(String(rewardMinutes)));
  let claimWindowInvalid = $derived(!/^\d+$/.test(String(claimWindowMonths)));

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

{#snippet fact(label: string, value: string)}
  <div class="min-w-0">
    <p class="text-sm text-muted-foreground">{label}</p>
    <p class="mt-1 font-semibold tabular-nums">{value}</p>
  </div>
{/snippet}

<div class="space-y-6">
  <SettingsCard
    title="Current Attendance Exposure"
    description="Active settings and Outstanding Attendance Time are shop-wide, independent of report filters."
  >
    {#snippet actions()}
      {#if settings}
        <StatePill on={settings.enabled}>{settings.enabled ? "Enabled" : "Disabled"}</StatePill>
      {/if}
    {/snippet}

    {#if loadError}
      <Notice tone="danger" role="alert">{loadError}</Notice>
    {:else if settings}
      <div class="grid gap-x-6 gap-y-4 sm:grid-cols-3">
        {@render fact("Daily threshold", formatMoney(settings.daily_threshold_cents))}
        {@render fact("Reward", formatProseDuration(settings.reward_seconds))}
        {@render fact(
          "Claim window",
          `${settings.claim_window_months} ${settings.claim_window_months === 1 ? "month" : "months"}`,
        )}
      </div>

      <div class="mt-6 grid gap-x-6 gap-y-4 border-t pt-6 sm:grid-cols-3">
        {@render fact("Outstanding Attendance Time", formatProseDuration(settings.liability.outstanding_seconds))}
        {@render fact("Claimable Attendance Time", formatProseDuration(settings.liability.claimable_seconds))}
        <div class="min-w-0">
          <p class="text-sm text-muted-foreground">Current Attendance Effective Bonus</p>
          <p class="mt-1 flex flex-wrap items-center gap-2 font-mono text-lg font-semibold tabular-nums">
            {bonusLabel(settings.economics_advisory)}
            {#if settings.economics_advisory?.unsafe_bonus}
              <Badge variant="destructive">Unsafe</Badge>
            {/if}
          </p>
          <p class="mt-1 text-sm text-muted-foreground">{boughtGivenSentence(settings.economics_advisory)}</p>
        </div>
      </div>

      <p class="mt-6 text-xs text-muted-foreground">
        Version {settings.version}, effective {formatDate(settings.effective_at)}. Claim window changes begin with the next Earning Month ({settings.next_earning_month}).
      </p>
    {:else}
      <p class="py-6 text-center text-sm text-muted-foreground">Loading Attendance settings…</p>
    {/if}
  </SettingsCard>

  {#if settings}
    <SettingsCard
      title="Configure Attendance Reward"
      description="Preview the draft with the active Shop Rate before committing."
    >
      <div class="space-y-6">
        <ToggleRow
          id="attendance-enabled"
          label="Enable Attendance Reward"
          description="Disabling stops new Attendance Check-Ins; earned Attendance Time remains claimable until its deadline."
          status={enabled ? "On" : "Off"}
        >
          <Switch id="attendance-enabled" aria-describedby="attendance-enabled-description" bind:checked={enabled} />
        </ToggleRow>

        <div class="grid gap-4 border-t pt-6 sm:grid-cols-3">
          <SettingsField id="attendance-threshold" label="Daily threshold (pesos)" hint="Default: ₱10">
            <Input
              id="attendance-threshold"
              aria-label="Daily threshold (pesos)"
              aria-invalid={thresholdInvalid || undefined}
              aria-describedby="attendance-threshold-hint{thresholdInvalid && draft.error ? ' attendance-draft-error' : ''}"
              type="text"
              inputmode="decimal"
              class="tabular-nums"
              bind:value={thresholdPesos}
            />
          </SettingsField>
          <SettingsField id="attendance-reward-minutes" label="Reward minutes" hint="Default: 60 minutes">
            <Input
              id="attendance-reward-minutes"
              aria-label="Reward minutes"
              aria-invalid={rewardInvalid || undefined}
              aria-describedby="attendance-reward-minutes-hint{rewardInvalid && draft.error ? ' attendance-draft-error' : ''}"
              type="number"
              step="1"
              class="tabular-nums"
              bind:value={rewardMinutes}
            />
          </SettingsField>
          <SettingsField id="attendance-claim-window" label="Claim window (months)" hint="Default: 1 month">
            <Input
              id="attendance-claim-window"
              aria-label="Claim window (months)"
              aria-invalid={claimWindowInvalid || undefined}
              aria-describedby="attendance-claim-window-hint{claimWindowInvalid && draft.error ? ' attendance-draft-error' : ''}"
              type="number"
              step="1"
              class="tabular-nums"
              bind:value={claimWindowMonths}
            />
          </SettingsField>
          {#if draft.error}
            <p id="attendance-draft-error" class="text-sm text-destructive sm:col-span-3" role="alert">{draft.error}</p>
          {/if}
        </div>

        <div class="rounded-lg border bg-muted/40 p-4" aria-live="polite">
          <h4 class="text-sm font-semibold">Draft Exposure Preview</h4>
          {#if preview}
            <div class="mt-3 grid gap-4 text-sm sm:grid-cols-3">
              <div>
                <p class="text-muted-foreground">Attendance Effective Bonus</p>
                <p class="mt-1 flex flex-wrap items-center gap-x-2 font-mono tabular-nums">
                  <del class="text-muted-foreground" aria-label="Active Attendance Effective Bonus">{bonusLabel(settings.economics_advisory)}</del>
                  <span aria-hidden="true">→</span>
                  <strong class="text-base">{bonusLabel(preview.economics_advisory)}</strong>
                  {#if preview.economics_advisory?.unsafe_bonus}
                    <Badge variant="destructive">Unsafe bonus</Badge>
                  {/if}
                </p>
              </div>
              <div>
                <p class="text-muted-foreground">Threshold</p>
                <p class="mt-1"><strong class="tabular-nums">{formatMoney(preview.daily_threshold_cents)}</strong></p>
              </div>
              <div>
                <p class="text-muted-foreground">Reward</p>
                <p class="mt-1"><strong>{formatProseDuration(preview.reward_seconds)}</strong></p>
              </div>
            </div>
            <p class="mt-3 text-sm text-muted-foreground">{boughtGivenSentence(preview.economics_advisory)}</p>
          {:else}
            <p class="mt-2 text-sm text-muted-foreground">
              Active Attendance Effective Bonus:
              <span aria-label="Active Attendance Effective Bonus">{bonusLabel(settings.economics_advisory)}</span>
            </p>
            {#if previewError}
              <p class="mt-2 text-sm text-destructive" role="alert">{previewError}</p>
            {:else if changed && !draft.error}
              <p class="mt-2 text-sm text-muted-foreground">Calculating preview…</p>
            {:else}
              <p class="mt-2 text-sm text-muted-foreground">Edit a setting to preview its effective bonus before saving.</p>
            {/if}
          {/if}
        </div>

        <Notice>
          <p>Existing daily snapshots are unchanged; threshold and reward edits apply to the next unfrozen daily snapshot.</p>
        </Notice>

        <div>
          <h4 class="text-sm font-semibold">When this commit takes effect</h4>
          <ul class="mt-2 grid gap-x-6 gap-y-1.5 text-sm text-muted-foreground sm:grid-cols-2">
            <li>Enable or disable: {effectiveChangeLabel(settings.effective_changes.enabled)}</li>
            <li>Daily threshold: {effectiveChangeLabel(settings.effective_changes.daily_threshold_cents)}</li>
            <li>Reward minutes: {effectiveChangeLabel(settings.effective_changes.reward_minutes)}</li>
            <li>Claim window: {effectiveChangeLabel(settings.effective_changes.claim_window_months)}</li>
          </ul>
        </div>
      </div>

      {#snippet footer()}
        <!-- Re-narrows `settings`: snippet closures do not inherit the outer {#if}. -->
        {#if settings}
          <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">
            {changed ? `Commits these settings as version ${settings.version + 1}.` : "No changes to commit."}
          </p>
          <Button disabled={!canCommit} onclick={saveSettings}>
            {busy ? "Saving…" : `Commit v${settings.version + 1}`}
          </Button>
        {/if}
      {/snippet}
    </SettingsCard>
  {/if}
</div>

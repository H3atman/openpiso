<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import Notice from "$lib/components/settings/Notice.svelte";
  import SettingsCard from "$lib/components/settings/SettingsCard.svelte";
  import ToggleRow from "$lib/components/settings/ToggleRow.svelte";
  import { Switch } from "../../components/ui/switch";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import type { ClientRecoveryEscalation, PcMonitoringStatus } from "../../types";

  /** Server-side floor and ceiling, mirrored here only to fail early. */
  const MIN_DELAY_SECS = 30;
  const MAX_DELAY_SECS = 3600;

  let settings = $state<ClientRecoveryEscalation | null>(null);
  let monitoring = $state<PcMonitoringStatus | null>(null);
  let loadError = $state("");

  /** Draft delay, editable without committing anything. */
  let delayInput = $state(60);

  // The pending change, snapshotted when the PIN dialog opens; null means no
  // dialog is open. Nothing on screen changes until the server accepts it.
  let pending = $state<ClientRecoveryEscalation | null>(null);
  let pin = $state("");
  let pinError = $state("");
  let busy = $state(false);

  onMount(() => {
    void load();
  });

  async function load() {
    settings = null;
    monitoring = null;
    loadError = "";
    try {
      // Escalation rides on the detection above it: with monitoring off there
      // is no mismatch to escalate from, and the server refuses to enable it.
      monitoring = await api.getPcMonitoringStatus();
      settings = await api.getClientRecoveryEscalation();
      delayInput = settings.delay_secs;
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function ask(next: ClientRecoveryEscalation) {
    if (busy) return;
    pending = next;
    pin = "";
    pinError = "";
  }

  function askToggle(value: boolean) {
    ask({ enabled: value, delay_secs: delayInput });
  }

  function askDelay() {
    if (!settings || delayInput === settings.delay_secs) return;
    ask({ enabled: settings.enabled, delay_secs: delayInput });
  }

  function cancel() {
    if (busy) return;
    pending = null;
    pin = "";
    pinError = "";
    // Put the draft back to the saved value so a cancelled edit leaves
    // nothing misleading on screen.
    if (settings) delayInput = settings.delay_secs;
  }

  async function confirm() {
    if (busy || !pin || !pending) return;
    busy = true;
    pinError = "";
    // Act on the decision snapshotted when the dialog opened.
    const next = pending;
    try {
      settings = await api.setClientRecoveryEscalation(next.enabled, next.delay_secs, pin);
      delayInput = settings.delay_secs;
      reportActionSuccess("save", "Client Recovery Escalation");
      pending = null;
    } catch (caught) {
      pinError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("save", "Client Recovery Escalation", caught);
    } finally {
      busy = false;
    }
  }

  const delayValid = $derived(
    Number.isInteger(delayInput) &&
      delayInput >= MIN_DELAY_SECS &&
      delayInput <= MAX_DELAY_SECS,
  );
</script>

<SettingsCard
  title="Client Recovery Escalation"
  description="What Admin does when a PC's Client stays absent and its Guard cannot bring it back."
>
  {#if loadError}
    <Notice tone="danger">{loadError}</Notice>
  {/if}

  {#if monitoring && !(monitoring.enabled && monitoring.platform_supported)}
    <div class="rounded-lg border border-dashed px-4 py-8 text-center">
      <p class="text-sm font-medium">Available once PC Monitoring is on</p>
      <p class="mx-auto mt-1 max-w-prose text-sm text-muted-foreground">
        Escalation acts on a <strong class="font-medium text-foreground">Client Reachability Mismatch</strong>, so
        there is nothing for it to act on until that detection is running.
      </p>
    </div>
  {:else if settings}
    <div class="space-y-6">
      <p class="max-w-prose text-sm text-muted-foreground">
        The Client Guard relaunches an absent Client on its own. When a PC keeps
        answering on the LAN, its Client still does not report, and its Guard
        reports that it has tried and failed to relaunch, Admin
        <strong class="text-foreground">powers that PC off</strong>. The session ends and the customer
        keeps their balance, time, and Points — nothing is confiscated. At most
        one power off per PC per hour, and never while the PC is suppressed or
        the Guard is standing down for owner maintenance.
      </p>

      <!-- The Switch reflects the saved state; toggling it only opens the
           PIN confirmation. -->
      <ToggleRow
        id="escalation-enabled"
        label="Power off PCs that cannot recover"
        description="Changing this requires your current Web Admin PIN and is audited."
        status={settings.enabled ? "On" : "Off"}
        class="border-t pt-6"
      >
        <Switch
          id="escalation-enabled"
          aria-describedby="escalation-enabled-description"
          checked={settings.enabled}
          onCheckedChange={(value) => askToggle(value === true)}
          disabled={busy}
        />
      </ToggleRow>

      <div class="grid gap-1.5 border-t pt-6">
        <label class="text-sm font-medium" for="escalation-delay">Escalation Delay (seconds)</label>
        <div class="flex flex-wrap items-center gap-2">
          <Input
            id="escalation-delay"
            type="number"
            min={MIN_DELAY_SECS}
            max={MAX_DELAY_SECS}
            class="w-32 tabular-nums"
            aria-invalid={!delayValid || undefined}
            aria-describedby="escalation-delay-hint{delayValid ? '' : ' escalation-delay-error'}"
            bind:value={delayInput}
            disabled={busy}
          />
          <Button
            variant="outline"
            disabled={busy || !delayValid || delayInput === settings.delay_secs}
            onclick={askDelay}
          >
            Save delay
          </Button>
        </div>
        {#if !delayValid}
          <p id="escalation-delay-error" class="text-xs text-destructive">
            Enter a whole number between {MIN_DELAY_SECS} and {MAX_DELAY_SECS}.
          </p>
        {/if}
        <p id="escalation-delay-hint" class="max-w-prose text-xs text-muted-foreground">
          How long the Guard must be visibly failing to recover the Client
          before the PC is powered off. Minimum {MIN_DELAY_SECS} seconds. This
          is a second clock, separate from the 60-second mismatch grace that
          opens the Alert.
        </p>
      </div>

      {#if settings.enabled}
        <Notice tone="warning" role="status">
          <p>
            OpenPiso is authorised to power customer PCs off. A PC whose Guard is
            not running is never escalated against — that stays a
            <strong>Client Guard Coverage</strong> Alert for you to act on.
          </p>
        </Notice>
      {/if}
    </div>
  {:else if !loadError}
    <p class="text-sm text-muted-foreground">Loading escalation policy…</p>
  {/if}
</SettingsCard>

{#if settings && pending}
  <ConfirmDialog
    title={pending.enabled && !settings.enabled
      ? "Allow OpenPiso to power PCs off?"
      : !pending.enabled && settings.enabled
        ? "Disable Client Recovery Escalation?"
        : "Change the Escalation Delay?"}
    confirmLabel="Save"
    busyLabel="Saving…"
    {busy}
    destructive={pending.enabled}
    onconfirm={confirm}
    oncancel={cancel}
  >
    {#if pending.enabled && !settings.enabled}
      A mismatching PC whose Guard reports a failed relaunch will be powered off
      after {pending.delay_secs} seconds. Its session ends; balance, time, and
      Points are untouched.
    {:else if !pending.enabled}
      PCs will keep raising Alerts but will never be powered off. The Guard
      still relaunches absent Clients on its own.
    {:else}
      Escalation will wait {pending.delay_secs} seconds of continuous failed
      recovery before powering a PC off.
    {/if}
    <div class="mt-4 grid gap-1.5">
      <label class="text-sm font-medium text-foreground" for="escalation-pin">Current Web Admin PIN</label>
      <Input
        id="escalation-pin"
        type="password"
        autocomplete="current-password"
        placeholder="Enter current PIN"
        aria-invalid={pinError ? true : undefined}
        aria-describedby={pinError ? "escalation-pin-error" : undefined}
        bind:value={pin}
      />
      {#if pinError}
        <p id="escalation-pin-error" class="text-xs text-destructive">{pinError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}

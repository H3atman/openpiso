<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import { Input } from "$lib/components/ui/input";
  import Notice from "$lib/components/settings/Notice.svelte";
  import SettingsCard from "$lib/components/settings/SettingsCard.svelte";
  import ToggleRow from "$lib/components/settings/ToggleRow.svelte";
  import { Switch } from "../../components/ui/switch";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import type { PcMonitoringStatus } from "../../types";

  let status = $state<PcMonitoringStatus | null>(null);
  let loadError = $state("");

  // The pending toggle, snapshotted when the PIN dialog opens; null means no
  // dialog is open. The switch itself never changes until the server accepts
  // the PIN-gated write.
  let pendingEnabled = $state<boolean | null>(null);
  let pin = $state("");
  let pinError = $state("");
  let busy = $state(false);

  onMount(() => {
    void load();
  });

  async function load() {
    status = null;
    loadError = "";
    try {
      status = await api.getPcMonitoringStatus();
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function askToggle(value: boolean) {
    if (busy || !status?.platform_supported) return;
    pendingEnabled = value;
    pin = "";
    pinError = "";
  }

  function cancelToggle() {
    if (busy) return;
    pendingEnabled = null;
    pin = "";
    pinError = "";
  }

  async function confirmToggle() {
    if (busy || !pin || pendingEnabled === null) return;
    busy = true;
    pinError = "";
    // Act on the decision snapshotted when the dialog opened, never a value
    // re-derived from state that may have refreshed underneath it.
    const next = pendingEnabled === true;
    try {
      status = await api.setPcMonitoringEnabled(next, pin);
      reportActionSuccess("save", "PC Monitoring");
      pendingEnabled = null;
    } catch (caught) {
      // The switch stays on the machine's actual state; surface the refusal.
      pinError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("save", "PC Monitoring", caught);
    } finally {
      busy = false;
    }
  }
</script>

<SettingsCard
  title="Incident detection"
  description="One Alert per episode, with full evidence in the Audit Log. Monitoring is shop-wide and never locks, powers off, or bills anyone."
>
  {#if loadError}
    <Notice tone="danger">{loadError}</Notice>
  {/if}

  {#if status}
    <div class="space-y-6">
      <div>
        <h4 class="text-sm font-semibold">What it watches on every visible registered PC</h4>
        <ul class="mt-3 grid gap-3 text-sm text-muted-foreground lg:grid-cols-3">
          <li class="rounded-lg border px-4 py-3">
            <p>
              <strong class="block text-foreground">Client Reachability Mismatch</strong>
              A host that keeps answering on the LAN while its Client stops reporting.
            </p>
          </li>
          <li class="rounded-lg border px-4 py-3">
            <p>
              <strong class="block text-foreground">Host Probe Coverage</strong>
              A PC whose Client proves it is running while its reachability can no longer be tested. Missing
              coverage means that PC is unmonitored — not offline or tampered.
            </p>
          </li>
          <li class="rounded-lg border px-4 py-3">
            <p>
              <strong class="block text-foreground">Client Guard Coverage</strong>
              Release Windows Clients report whether they hold their Client Guard connection; two continuous
              minutes without it opens an incident. Advisory only — check the Guard service and Client
              installation on the PC, since Admin detects and never repairs services.
            </p>
          </li>
        </ul>
      </div>

      {#if !status.platform_supported}
        <Notice>
          <p>
            PC Monitoring requires a Windows Admin. This installation can send no probes and will never
            manufacture results.
          </p>
        </Notice>
      {:else}
        <!-- The Switch reflects the saved state; toggling it only opens the
             PIN confirmation. -->
        <ToggleRow
          id="pc-monitoring-enabled"
          label="Monitor PCs"
          description="Changing this requires your current Web Admin PIN and is audited."
          status={status.enabled ? "On" : "Off"}
          class="border-t pt-6"
        >
          <Switch
            id="pc-monitoring-enabled"
            aria-describedby="pc-monitoring-enabled-description"
            checked={status.enabled}
            onCheckedChange={(value) => askToggle(value === true)}
            disabled={busy}
          />
        </ToggleRow>

        {#if !status.enabled}
          <Notice tone="warning">
            <p>
              While monitoring is off, the dashboard shows a standing advisory and no incidents are detected.
              Disabling later closes active incidents as <strong>monitoring disabled</strong> — historical
              Alerts remain.
            </p>
          </Notice>
        {/if}
      {/if}
    </div>
  {:else if !loadError}
    <p class="text-sm text-muted-foreground">Loading PC Monitoring state…</p>
  {/if}
</SettingsCard>

{#if status && pendingEnabled !== null}
  <ConfirmDialog
    title={pendingEnabled ? "Enable PC Monitoring?" : "Disable PC Monitoring?"}
    confirmLabel={pendingEnabled ? "Enable" : "Disable"}
    busyLabel="Saving…"
    busy={busy}
    destructive={!pendingEnabled}
    onconfirm={confirmToggle}
    oncancel={cancelToggle}
  >
    {#if pendingEnabled}
      Admin will probe every visible registered PC every ten seconds and open an
      Alert after 60 seconds of continuous Client Reachability Mismatch.
    {:else}
      Active incidents will close as <strong>monitoring disabled</strong>, never
      as recovered. Historical Alerts remain in Notifications.
    {/if}
    <div class="mt-4 grid gap-1.5">
      <label class="text-sm font-medium text-foreground" for="pc-monitoring-pin">Current Web Admin PIN</label>
      <Input
        id="pc-monitoring-pin"
        type="password"
        autocomplete="current-password"
        placeholder="Enter current PIN"
        aria-invalid={pinError ? true : undefined}
        aria-describedby={pinError ? "pc-monitoring-pin-error" : undefined}
        bind:value={pin}
      />
      {#if pinError}
        <p id="pc-monitoring-pin-error" class="text-xs text-destructive">{pinError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}

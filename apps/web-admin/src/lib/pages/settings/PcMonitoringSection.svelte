<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import { Badge } from "$lib/components/ui/badge";
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

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">PC Monitoring</h3>

  {#if loadError}
    <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
      {loadError}
    </div>
  {/if}

  {#if status}
    <div class="space-y-4">
      <p class="text-sm text-muted-foreground">
        Watches every visible registered PC for a
        <strong>Client Reachability Mismatch</strong>: a host that keeps
        answering on the LAN while its Client stops reporting. It also watches
        <strong>Host Probe Coverage</strong>: a PC whose Client proves it is
        running while its reachability can no longer be tested. Missing
        coverage means that PC is unmonitored — not offline or tampered.
        Release Windows Clients also report whether they hold their
        <strong>Client Guard</strong> connection; two continuous minutes
        without it opens a <strong>Client Guard Coverage</strong> incident.
        This diagnostic is advisory only — check the Guard service and Client
        installation on the PC, since Admin detects and never repairs
        services. One Alert per episode, with full evidence in the Audit Log.
        Monitoring is shop-wide and never locks, powers off, or bills anyone.
      </p>

      <div class="flex items-center gap-3">
        <span class="text-sm font-medium">Monitoring:</span>
        <Badge variant={status.enabled ? "default" : "secondary"}>
          {status.enabled ? "On" : "Off"}
        </Badge>
      </div>

      {#if !status.platform_supported}
        <div class="rounded-md bg-muted p-3 text-sm text-muted-foreground">
          PC Monitoring requires a Windows Admin. This installation can send no
          probes and will never manufacture results.
        </div>
      {:else}
        <!-- The Switch reflects the saved state; toggling it only opens the
             PIN confirmation. -->
        <div class="flex items-center gap-3">
          <Switch
            checked={status.enabled}
            onCheckedChange={(value) => askToggle(value === true)}
            disabled={busy}
          />
          <span class="text-xs text-muted-foreground">
            Changing this requires your current Web Admin PIN and is audited.
          </span>
        </div>

        {#if !status.enabled}
          <div class="rounded-md bg-amber-50 p-3 text-sm text-amber-800 dark:bg-amber-950 dark:text-amber-200">
            While monitoring is off, the dashboard shows a standing advisory and
            no incidents are detected. Disabling later closes active incidents
            as <strong>monitoring disabled</strong> — historical Alerts remain.
          </div>
        {/if}
      {/if}
    </div>
  {:else if !loadError}
    <div class="text-sm text-muted-foreground">Loading PC Monitoring state…</div>
  {/if}
</div>

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
    <div class="mt-3">
      <label class="block text-sm font-medium" for="pc-monitoring-pin">
        Current Web Admin PIN
      </label>
      <input
        id="pc-monitoring-pin"
        type="password"
        autocomplete="current-password"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        placeholder="Enter current PIN"
        bind:value={pin}
      />
      {#if pinError}
        <p class="mt-1 text-xs text-destructive">{pinError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}

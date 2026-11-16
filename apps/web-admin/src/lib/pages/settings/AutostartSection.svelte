<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import StatePill from "../../components/settings/StatePill.svelte";
  import ToggleRow from "../../components/settings/ToggleRow.svelte";
  import { Switch } from "../../components/ui/switch";
  import type { AutostartResponse } from "../../types";

  let autostart = $state<AutostartResponse | null>(null);
  let loadError = $state("");
  let toggleBusy = $state(false);

  // Autostart may only be changed on a platform that implements it, from a
  // build allowed to write the host entry (release only, ADR-0055).
  const canEdit = $derived(
    autostart !== null && autostart.supported && autostart.editable,
  );

  onMount(() => {
    void load();
  });

  async function load() {
    autostart = null;
    loadError = "";
    try {
      autostart = await api.getAutostart();
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function toggle(value: boolean) {
    if (!autostart || toggleBusy || !canEdit) return;
    toggleBusy = true;
    const previousState = autostart.enabled;
    // Optimistic UI update.
    autostart = { ...autostart, enabled: value };
    try {
      autostart = await api.updateAutostart({ enabled: value });
      reportActionSuccess("save", "Autostart");
    } catch (caught) {
      // Revert to the machine's actual state on failure.
      try {
        autostart = await api.getAutostart();
      } catch {
        // If the re-read also fails, restore our previous best guess.
        autostart = { ...autostart, enabled: previousState };
      }
      reportActionFailure("save", "Autostart", caught);
    } finally {
      toggleBusy = false;
    }
  }
</script>

<SettingsCard
  title="Start at logon"
  description="Launches the Central Terminal when the Admin PC user logs in."
>
  {#snippet actions()}
    {#if autostart?.supported}
      <StatePill on={autostart.enabled}>
        <span class="text-muted-foreground">Enabled at logon:</span>
        <span>{autostart.enabled ? "Yes" : "No"}</span>
      </StatePill>
    {/if}
  {/snippet}

  {#if loadError}
    <Notice tone="danger"><p>{loadError}</p></Notice>
  {/if}

  {#if autostart}
    <div class="space-y-6">
      {#if !autostart.supported || !autostart.editable}
        <div class="space-y-3">
          {#if !autostart.supported}
            <Notice><p>Autostart is not supported on this platform.</p></Notice>
          {/if}
          {#if !autostart.editable}
            <Notice>
              <p>Autostart is read-only in this build. The toggle will be available in release builds.</p>
            </Notice>
          {/if}
        </div>
      {/if}

      <ToggleRow
        id="autostart-enabled"
        label="Start the Central Terminal at logon"
        description="Applies to this Admin PC only. It does not restart the Central Terminal after a crash."
        status={autostart.enabled ? "On" : "Off"}
      >
        <Switch
          id="autostart-enabled"
          aria-describedby="autostart-enabled-description"
          checked={autostart.enabled}
          onCheckedChange={(value) => toggle(value === true)}
          disabled={toggleBusy || !canEdit}
        />
      </ToggleRow>

      {#if autostart.supported}
        <div class="border-t pt-6">
          <p class="text-sm text-muted-foreground">Executable path</p>
          <code class="mt-1.5 block rounded-md border bg-muted/50 px-3 py-2 font-mono text-xs break-all">
            {autostart.exe_path}
          </code>
        </div>

        <Notice tone="warning" title="Logon scope — not boot scope">
          <p>
            Autostart launches the Central Terminal when the Admin PC user logs in.
            Unattended recovery after a power cycle also needs
            <strong>Windows autologon</strong>,
            which you provision separately &mdash; OpenPiso never configures it.
          </p>
        </Notice>

        <p class="max-w-prose text-sm text-muted-foreground">
          Autostart is not the <strong class="text-foreground">Watchdog</strong> or the
          <strong class="text-foreground">Client Guard</strong>. Those are Client-side and relaunch a
          killed process; Autostart only starts the Central Terminal at logon
          and does not restart it after a crash.
        </p>
      {/if}
    </div>
  {:else if !loadError}
    <p class="text-sm text-muted-foreground">Loading Autostart state…</p>
  {/if}
</SettingsCard>

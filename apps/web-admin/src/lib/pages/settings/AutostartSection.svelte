<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Badge } from "$lib/components/ui/badge";
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

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">Central Terminal Autostart</h3>

  {#if loadError}
    <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
      {loadError}
    </div>
  {/if}

  {#if autostart}
    <div class="space-y-4">
      {#if autostart.supported}
        <div class="flex items-center gap-3">
          <span class="text-sm font-medium">Enabled at logon:</span>
          <Badge variant={autostart.enabled ? "default" : "secondary"}>
            {autostart.enabled ? "Yes" : "No"}
          </Badge>
        </div>

        <div class="text-sm text-muted-foreground">
          <span class="font-medium">Executable path:</span>
          <code class="mt-1 block break-all rounded bg-muted px-2 py-1 font-mono text-xs">
            {autostart.exe_path}
          </code>
        </div>

        <div class="rounded-md bg-amber-50 p-3 text-sm text-amber-800 dark:bg-amber-950 dark:text-amber-200">
          <p class="font-medium">Logon scope &mdash; not boot scope</p>
          <p class="mt-1">
            Autostart launches the Central Terminal when the Admin PC user logs in.
            Unattended recovery after a power cycle also needs
            <strong>Windows autologon</strong>,
            which you provision separately &mdash; OpenPiso never configures it.
          </p>
        </div>

        <p class="text-sm text-muted-foreground">
          Autostart is not the <strong>Watchdog</strong> or the
          <strong>Client Guard</strong>. Those are Client-side and relaunch a
          killed process; Autostart only starts the Central Terminal at logon
          and does not restart it after a crash.
        </p>
      {/if}

      {#if !autostart.supported}
        <div class="rounded-md bg-muted p-3 text-sm text-muted-foreground">
          Autostart is not supported on this platform.
        </div>
      {/if}

      {#if !autostart.editable}
        <div class="rounded-md bg-muted p-3 text-sm text-muted-foreground">
          Autostart is read-only in this build. The toggle will be available in release builds.
        </div>
      {/if}

      <div class="flex items-center gap-3">
        <Switch
          checked={autostart.enabled}
          onCheckedChange={(value) => toggle(value === true)}
          disabled={toggleBusy || !canEdit}
        />
        <span class="text-sm text-muted-foreground">
          {autostart.enabled ? "On" : "Off"}
        </span>
      </div>
    </div>
  {:else if !loadError}
    <div class="text-sm text-muted-foreground">Loading Autostart state…</div>
  {/if}
</div>

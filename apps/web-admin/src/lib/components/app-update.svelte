<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import { mergeProps } from "bits-ui";
  import * as api from "../api";
  import type { AppUpdateState } from "../types";
  import * as Sidebar from "$lib/components/ui/sidebar/index.js";
  import * as Tooltip from "$lib/components/ui/tooltip/index.js";
  import * as Sheet from "$lib/components/ui/sheet/index.js";
  import { Button } from "$lib/components/ui/button";
  import DownloadIcon from "@lucide/svelte/icons/download";
  import { toast } from "svelte-sonner";
  import * as Collapsible from "$lib/components/ui/collapsible/index.js";
  import { Input } from "$lib/components/ui/input";
  import { Separator } from "$lib/components/ui/separator";
  import ReleaseNotes from "./release-notes.svelte";
  import { Switch } from "$lib/components/ui/switch";
  import { formatDate } from "../format";

  // Keep the controller and Sheet mounted outside the mobile sidebar's Sheet.
  // The caller places only this row in its footer.
  let { children }: { children?: Snippet<[Snippet]> } = $props();
  const sidebar = Sidebar.useSidebar();
  let update = $state<AppUpdateState | null>(null);
  let open = $state(false);
  let loadError = $state("");
  let ackError = $state("");
  let ackBusy = $state(false);
  let shownResult = "";
  let checking = $state(false);
  let toggleBusy = $state(false);
  let installing = $state(false);
  let awaitingRestart = $state(false);
  let confirmInstall = $state(false);
  let acknowledgeSchema = $state(false);
  let rollbackVersion = $state("");
  let confirmVersion = $state("");
  const busy = $derived(
    installing ||
      awaitingRestart ||
      checking ||
      toggleBusy ||
      ackBusy ||
      !!update?.download ||
      !!update?.lastResult,
  );
  const canRollback = $derived(
    !busy &&
      !!rollbackVersion &&
      confirmVersion === rollbackVersion &&
      !!update?.rollbacks.some(
        (release) => release.version === rollbackVersion,
      ),
  );
  const canInstall = $derived(
    !!update?.available &&
      !busy &&
      (!update.available.raisesSchema || acknowledgeSchema),
  );
  const label = $derived(
    update?.available
      ? `Update available · v${update.available.version}`
      : update
        ? `App Update · v${update.currentVersion}`
        : "App Update",
  );
  const summary = $derived(
    update?.available?.notes
      .split("\n")
      .filter(Boolean)
      .slice(0, 3)
      .join("\n") || "View the installed version and check for App Updates.",
  );

  let reading = false;
  let generation = 0;
  onMount(() => {
    void load();
    return () => {
      generation += 1;
    };
  });

  // GET never checks the Release Feed. Keep download progress live while the
  // install POST waits; outside the panel, use the shell's 15-second cadence.
  $effect(() => {
    const interval =
      open || installing || awaitingRestart || update?.download
        ? 1_000
        : 15_000;
    const timer = setInterval(() => void load(), interval);
    return () => clearInterval(timer);
  });

  function publish(next: AppUpdateState) {
    generation += 1;
    if (
      next.lastResult ||
      (update && update.currentVersion !== next.currentVersion)
    )
      awaitingRestart = false;
    if (update?.available?.version !== next.available?.version) {
      confirmInstall = false;
      acknowledgeSchema = false;
    }
    if (
      !next.rollbacks.some((release) => release.version === rollbackVersion)
    ) {
      rollbackVersion = "";
      confirmVersion = "";
    }
    update = next;
  }

  $effect(() => {
    if (!update?.lastResult) return;
    const result = update.lastResult;
    const key = JSON.stringify(result);
    if (key === shownResult) return;
    shownResult = key;
    if (result.ok) toast.success(`Updated to v${result.to}`);
    else toast.error(`Update failed, still on v${update.currentVersion}`);
    void acknowledge();
  });

  async function acknowledge() {
    if (ackBusy) return;
    ackBusy = true;
    generation += 1;
    try {
      publish(await api.acknowledgeAppUpdate());
      ackError = "";
    } catch (error) {
      ackError = error instanceof Error ? error.message : String(error);
      toast.error(`Could not acknowledge App Update: ${ackError}`);
    } finally {
      ackBusy = false;
    }
  }

  async function load() {
    if (reading || checking || toggleBusy || ackBusy) return;
    reading = true;
    const requestGeneration = ++generation;
    try {
      const next = await api.getAppUpdate();
      if (requestGeneration !== generation) return;
      publish(next);
      loadError = "";
    } catch (error) {
      if (requestGeneration !== generation) return;
      loadError = error instanceof Error ? error.message : String(error);
    } finally {
      reading = false;
    }
  }

  async function toggleAutoCheck(enabled: boolean) {
    if (toggleBusy) return;
    toggleBusy = true;
    generation += 1;
    try {
      publish(await api.setAppUpdateAutoCheck({ enabled }));
      toast.success(
        `Automatic App Update checks ${enabled ? "enabled" : "disabled"}.`,
      );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error));
    } finally {
      toggleBusy = false;
    }
  }

  async function installOrRollback(rollback: boolean) {
    const version = rollback ? rollbackVersion : update?.available?.version;
    if (!version || (rollback ? !canRollback : !canInstall)) return;
    installing = true;
    try {
      const next = rollback
        ? await api.rollbackAppUpdate({ version, confirmVersion })
        : await api.installAppUpdate({
            version,
            acknowledgeSchemaChange: acknowledgeSchema,
          });
      publish(next);
      if (rollback) {
        rollbackVersion = "";
        confirmVersion = "";
      } else {
        confirmInstall = false;
      }
      awaitingRestart = !next.lastResult;
      toast.success(
        `${rollback ? "Compatible Rollback" : "App Update"} prepared. The Admin is restarting.`,
      );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error));
    } finally {
      installing = false;
    }
  }

  async function check() {
    if (busy) return;
    checking = true;
    generation += 1;
    try {
      const next = await api.checkAppUpdate();
      publish(next);
      loadError = "";
      if (next.lastError) toast.error(next.lastError);
      else
        toast.success(
          next.available ? "App Update available." : "The Admin is up to date.",
        );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error));
    } finally {
      checking = false;
    }
  }
</script>

{#snippet row()}
  <Sidebar.Menu>
    <Sidebar.MenuItem>
      <!-- Pointer-driven focus must not open the tooltip: closing the Sheet
           returns focus to this row, which would reopen it and leave it
           showing until blur. Keyboard focus still opens it. -->
      <Tooltip.Root ignoreNonKeyboardFocus disableHoverableContent>
        <Tooltip.Trigger>
          {#snippet child({ props })}
            <Sidebar.MenuButton
              {...mergeProps(props, {
                onclick: () => {
                  sidebar.setOpenMobile(false);
                  open = true;
                },
              })}
              aria-label={label}
              data-testid="app-update-row"
            >
              <DownloadIcon />
              <span>{label}</span>
            </Sidebar.MenuButton>
          {/snippet}
        </Tooltip.Trigger>
        <Tooltip.Content side="right"
          ><p class="max-w-xs whitespace-pre-line">
            {summary}
          </p></Tooltip.Content
        >
      </Tooltip.Root>
    </Sidebar.MenuItem>
  </Sidebar.Menu>
{/snippet}

{#if children}
  {@render children(row)}
{:else}
  {@render row()}
{/if}

<Sheet.Root bind:open>
  <Sheet.Content class="overflow-y-auto" data-testid="app-update-panel">
    <Sheet.Header>
      <Sheet.Title>App Update</Sheet.Title>
      <Sheet.Description
        >Update the Admin app from this device.</Sheet.Description
      >
    </Sheet.Header>
    <div class="flex flex-col gap-4 px-4 pb-4">
      {#if loadError || update?.lastError}
        <p role="alert" class="text-destructive">
          {loadError || update?.lastError}
        </p>
      {/if}
      {#if ackError}
        <p role="alert" class="text-destructive">
          Could not acknowledge App Update: {ackError}
        </p>
        <Button onclick={acknowledge} disabled={ackBusy}
          >Retry acknowledgement</Button
        >
      {/if}
      {#if update}
        <p>
          Installed: v{update.currentVersion} · Schema Version {update.currentSchemaVersion}
        </p>
        <p class="text-muted-foreground">
          Last checked: {update.lastCheckedAt
            ? formatDate(update.lastCheckedAt)
            : "Never"}
        </p>
        <div class="flex items-center justify-between gap-3">
          <label for="app-update-auto-check">Automatic checks</label>
          <Switch
            id="app-update-auto-check"
            checked={update.autoCheck}
            onCheckedChange={toggleAutoCheck}
            disabled={toggleBusy || checking || busy}
          />
        </div>
        <p class="text-muted-foreground">
          On startup and every 6 hours. Turning this off prevents automatic
          outbound checks; Check now still works.
        </p>
      {/if}
      <Button onclick={check} disabled={checking || busy}
        >{checking ? "Checking…" : "Check now"}</Button
      >
      {#if update?.download}
        <p aria-live="polite">
          Downloading v{update.download.targetVersion}: {update.download
            .percent}%
        </p>
        <progress
          max="100"
          value={update.download.percent}
          aria-label={`Downloading v${update.download.targetVersion}`}
          aria-valuenow={update.download.percent}
          class="w-full"
        ></progress>
      {:else if installing}
        <p role="status">
          Preparing the signed installer. The Admin will restart after
          verification.
        </p>
      {/if}
      {#if update && update.runningSessionCount > 0}
        <p role="note">
          {update.runningSessionCount} PCs have running time. Remaining time is preserved.
          During the restart, Clients return to the Fullscreen Lock under the Offline
          Shutdown Countdown and resume on reconnect.
        </p>
      {/if}
      {#if awaitingRestart && !update?.download}
        <p role="status">
          Waiting for the Admin to restart. Sign in again after it reconnects to
          see the result.
        </p>
      {/if}
      {#if update?.available}
        <h3>v{update.available.version}</h3>
        <ReleaseNotes notes={update.available.notes} />
        {#if confirmInstall}
          <p>Install v{update.available.version} and restart the Admin?</p>
          {#if update.available.raisesSchema}
            <p class="text-destructive">
              This App Update raises the Schema Version. You cannot roll back
              below this version afterwards without restoring a Pre-Migration
              Backup.
            </p>
            <label class="flex items-center gap-2">
              <input
                type="checkbox"
                bind:checked={acknowledgeSchema}
                disabled={busy}
              />
              I understand
            </label>
          {/if}
          <Button onclick={() => installOrRollback(false)} disabled={!canInstall}
            >Install and restart</Button
          >
          <Button
            variant="outline"
            onclick={() => (confirmInstall = false)}
            disabled={busy}>Cancel</Button
          >
        {:else}
          <Button
            onclick={() => {
              acknowledgeSchema = false;
              confirmInstall = true;
            }}
            disabled={busy}>Install</Button
          >
        {/if}
      {/if}
      {#if update}
        <Separator />
        <Collapsible.Root>
          <Collapsible.Trigger>
            {#snippet child({ props })}
              <Button {...props} variant="outline">Compatible Rollback</Button>
            {/snippet}
          </Collapsible.Trigger>
          <Collapsible.Content class="flex flex-col gap-4 pt-4">
            <p class="text-muted-foreground">
              Only older stable releases with Schema Version {update.currentSchemaVersion}
              are offered.
            </p>
            {#each update.rollbacks as release (release.version)}
              <section class="flex flex-col gap-3">
                <h3>v{release.version}</h3>
                <ReleaseNotes notes={release.notes} />
                {#if rollbackVersion === release.version}
                  <p role="alert" class="text-destructive">
                    Compatible Rollback replaces the Admin with an older release
                    and restarts the shop server. Features and fixes from newer
                    releases will be unavailable.
                  </p>
                  <label for="app-update-confirm-version"
                    >Type {release.version} to confirm</label
                  >
                  <Input
                    id="app-update-confirm-version"
                    bind:value={confirmVersion}
                    disabled={busy}
                    autocomplete="off"
                  />
                  <Button
                    variant="destructive"
                    onclick={() => installOrRollback(true)}
                    disabled={!canRollback}>Roll back and restart</Button
                  >
                  <Button
                    variant="outline"
                    onclick={() => (rollbackVersion = "")}
                    disabled={busy}>Cancel rollback</Button
                  >
                {:else}
                  <Button
                    variant="outline"
                    disabled={busy}
                    onclick={() => {
                      rollbackVersion = release.version;
                      confirmVersion = "";
                    }}>Roll back to v{release.version}</Button
                  >
                {/if}
              </section>
            {:else}
              <p>No Compatible Rollback releases available.</p>
            {/each}
          </Collapsible.Content>
        </Collapsible.Root>
      {/if}
    </div>
  </Sheet.Content>
</Sheet.Root>

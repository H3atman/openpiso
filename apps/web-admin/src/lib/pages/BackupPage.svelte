<script lang="ts">
  import { onMount } from "svelte";
  import { toast } from "svelte-sonner";
  import * as Sheet from "$lib/components/ui/sheet/index.js";
  import { Badge } from "$lib/components/ui/badge";
  import { IsMobile } from "$lib/hooks/is-mobile.svelte";
  import * as api from "../api";
  import { formatDate } from "../format";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import { Switch } from "../components/ui/switch";
  import type {
    BackupCatalogEntry,
    BackupScheduleFrequency,
    BackupScheduleSettings,
    BackupScheduleSettingsInput,
    PendingRestore,
  } from "../types";

  const backupSets = [
    "Accounts",
    "Configuration",
    "Ledger",
    "Vouchers",
    "Terminals",
    "Runtime State",
    "Credentials",
  ];

  let selectedSets = $state<string[]>([...backupSets]);
  let scheduleSelectedSets = $state<string[]>([...backupSets]);
  let catalog = $state<BackupCatalogEntry[]>([]);
  let loading = $state(true);
  let creating = $state(false);
  let error = $state("");
  let deletingFilename = $state("");
  let scheduleLoaded = $state(false);
  let scheduleBusy = $state(false);
  let scheduleError = $state("");
  let scheduleEnabled = $state(true);
  let scheduleFrequency = $state<BackupScheduleFrequency>("daily");
  let scheduleTime = $state("04:00");
  let scheduleWeekday = $state(0);
  let scheduleDayOfMonth = $state(1);
  let scheduleRetentionCount = $state(14);
  let scheduleMirrorDirectory = $state("");
  let scheduleIntervalMinutes = $state(1);
  let scheduleNextDueAt = $state<string | null>(null);
  let scheduleMirrorError = $state<string | null>(null);
  let pendingRestore = $state<PendingRestore | null>(null);
  let restoreCandidate = $state<BackupCatalogEntry | null>(null);
  let restorePathCandidate = $state("");
  let restoreUploadCandidate = $state<File | null>(null);
  let restorePath = $state("");
  let restoreUpload = $state<File | null>(null);
  let restoreBusy = $state(false);
  let restoreError = $state("");
  let blockingPcs = $state<string[]>([]);
  const weekdays = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
  const debugIntervalMode = import.meta.env.DEV;

  function minuteToTime(minute: number): string {
    return `${String(Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}`;
  }

  function timeToMinute(value: string): number {
    const [hours, minutes] = value.split(":").map(Number);
    return hours * 60 + minutes;
  }

  function applySchedule(settings: BackupScheduleSettings) {
    scheduleLoaded = true;
    scheduleEnabled = settings.enabled;
    scheduleFrequency = settings.frequency;
    scheduleTime = minuteToTime(settings.minute_of_day);
    scheduleWeekday = settings.weekday;
    scheduleDayOfMonth = settings.day_of_month;
    scheduleSelectedSets = [...settings.backup_sets];
    scheduleRetentionCount = settings.retention_count;
    scheduleMirrorDirectory = settings.mirror_directory ?? "";
    scheduleMirrorError = settings.mirror_error;
    scheduleIntervalMinutes = Math.max(1, Math.round((settings.interval_seconds ?? 60) / 60));
    scheduleNextDueAt = settings.next_due_at;
  }

  function scheduleInput(): BackupScheduleSettingsInput {
    const debugInterval = debugIntervalMode
      ? { interval_seconds: Math.max(60, scheduleIntervalMinutes * 60) }
      : {};
    return {
      enabled: scheduleEnabled,
      frequency: scheduleFrequency,
      minute_of_day: timeToMinute(scheduleTime),
      weekday: scheduleWeekday,
      day_of_month: scheduleDayOfMonth,
      backup_sets: scheduleSelectedSets,
      retention_count: scheduleRetentionCount,
      mirror_directory: scheduleMirrorDirectory.trim() || null,
      ...debugInterval,
    };
  }

  async function loadSchedule() {
    scheduleError = "";
    try {
      applySchedule(await api.getBackupSchedule());
    } catch (caught) {
      scheduleError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function saveSchedule() {
    scheduleBusy = true;
    try {
      applySchedule(await api.updateBackupSchedule(scheduleInput()));
      reportActionSuccess("save", "Backup Schedule");
    } catch (caught) {
      reportActionFailure("save", "Backup Schedule", caught);
    } finally {
      scheduleBusy = false;
    }
  }

  function toggleSet(name: string) {
    selectedSets = selectedSets.includes(name)
      ? selectedSets.filter((set) => set !== name)
      : [...selectedSets, name];
  }

  function toggleScheduleSet(name: string) {
    scheduleSelectedSets = scheduleSelectedSets.includes(name)
      ? scheduleSelectedSets.filter((set) => set !== name)
      : [...scheduleSelectedSets, name];
  }

  function formatBytes(value: number): string {
    if (value < 1_024) return `${value} B`;
    if (value < 1_024 * 1_024) return `${(value / 1_024).toFixed(1)} KB`;
    return `${(value / (1_024 * 1_024)).toFixed(1)} MB`;
  }

  async function loadCatalog() {
    loading = true;
    error = "";
    try {
      catalog = await api.listBackups();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      loading = false;
    }
  }

  async function createBackup() {
    if (selectedSets.length === 0) return;
    creating = true;
    try {
      const created = await api.createBackup(selectedSets);
      toast.success("Backup created", {
        description: `${created.filename} (${formatBytes(created.size_bytes)})`,
      });
      await loadCatalog();
    } catch (caught) {
      reportActionFailure("create", "Backup", caught);
    } finally {
      creating = false;
    }
  }

  async function deleteBackup(entry: BackupCatalogEntry) {
    if (!window.confirm(`Delete ${entry.filename}?`)) return;
    deletingFilename = entry.filename;
    try {
      await api.deleteBackup(entry.filename);
      reportActionSuccess("delete", "Backup");
      catalog = catalog.filter(({ filename }) => filename !== entry.filename);
    } catch (caught) {
      reportActionFailure("delete", "Backup", caught);
    } finally {
      deletingFilename = "";
    }
  }

  function openRestoreDialog(entry: BackupCatalogEntry) {
    restoreCandidate = entry;
    restorePathCandidate = "";
    restoreUploadCandidate = null;
    restoreError = "";
    blockingPcs = [];
  }

  function openRestorePathDialog() {
    const path = restorePath.trim();
    if (!path) return;
    restoreCandidate = null;
    restorePathCandidate = path;
    restoreUploadCandidate = null;
    restoreError = "";
    blockingPcs = [];
  }

  function selectRestoreUpload(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    restoreUpload = input.files?.[0] ?? null;
  }

  function openRestoreUploadDialog() {
    if (!restoreUpload) return;
    restoreCandidate = null;
    restorePathCandidate = "";
    restoreUploadCandidate = restoreUpload;
    restoreError = "";
    blockingPcs = [];
  }

  function closeRestoreConfirm() {
    restoreCandidate = null;
    restorePathCandidate = "";
    restoreUploadCandidate = null;
  }

  const isMobile = new IsMobile();

  async function confirmRestore() {
    if (!restoreCandidate && !restorePathCandidate && !restoreUploadCandidate) return;
    restoreBusy = true;
    restoreError = "";
    blockingPcs = [];
    try {
      pendingRestore = restoreUploadCandidate
        ? await api.uploadBackup(restoreUploadCandidate)
        : await api.stageRestore(
            restoreCandidate
              ? { filename: restoreCandidate.filename }
              : { path: restorePathCandidate },
          );
      restoreCandidate = null;
      restorePathCandidate = "";
      restoreUploadCandidate = null;
      restorePath = "";
      restoreUpload = null;
      toast.success("Restore staged", {
        description: "It will take effect on the next launch.",
      });
    } catch (caught) {
      restoreError = caught instanceof Error ? caught.message : String(caught);
      if (caught instanceof api.RestoreRejectedError) {
        blockingPcs = caught.blockingPcs;
      }
    } finally {
      restoreBusy = false;
    }
  }

  async function cancelRestore() {
    try {
      await api.cancelPendingRestore();
      pendingRestore = null;
      reportActionSuccess("remove", "Pending Restore");
    } catch (caught) {
      reportActionFailure("remove", "Pending Restore", caught);
    }
  }

  async function loadPendingRestore() {
    try {
      pendingRestore = await api.getPendingRestore();
    } catch (caught) {
      restoreError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  onMount(() => {
    void loadCatalog();
    void loadSchedule();
    void loadPendingRestore();
  });
</script>

<div class="space-y-6">
  <div>
    <h2 class="text-2xl font-bold tracking-tight">Backups</h2>
    <p class="mt-1 text-sm text-muted-foreground">
      Create readable SQL Backups. Backup files contain secrets when Credentials or Terminals are included.
    </p>
  </div>

  <section class="rounded-lg border bg-card p-6" aria-labelledby="backup-schedule-heading">
    <div class="mb-4 flex flex-wrap items-start justify-between gap-4">
      <div>
        <h3 id="backup-schedule-heading" class="mb-1 text-lg font-semibold">Backup Schedule</h3>
        <p class="text-sm text-muted-foreground">
          Automatic Backups use the Admin host's local time. A separate worker checks the schedule every 30 seconds and makes one catch-up Backup after downtime.
        </p>
      </div>
      <label class="flex items-center gap-2 text-sm font-medium">
        <Switch size="sm" bind:checked={scheduleEnabled} />
        Enable automatic Backups
      </label>
    </div>
    {#if scheduleError}
      <div class="mb-4 rounded-md border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive" role="alert">
        Could not load Backup Schedule: {scheduleError}
      </div>
    {/if}
    <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      <div>
        <label class="block text-sm font-medium" for="backup-frequency">Frequency</label>
        <select id="backup-frequency" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={scheduleFrequency}>
          <option value="daily">Daily</option>
          <option value="weekly">Weekly</option>
          <option value="monthly">Monthly</option>
          {#if debugIntervalMode}
            <option value="interval">Every N minutes (debug)</option>
          {/if}
        </select>
      </div>
      {#if scheduleFrequency === "weekly"}
        <div>
          <label class="block text-sm font-medium" for="backup-weekday">Day of week</label>
          <select id="backup-weekday" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={scheduleWeekday}>
            {#each weekdays as weekday, index}
              <option value={index}>{weekday}</option>
            {/each}
          </select>
        </div>
      {:else if scheduleFrequency === "monthly"}
        <div>
          <label class="block text-sm font-medium" for="backup-day-of-month">Day of month</label>
          <input id="backup-day-of-month" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" type="number" min="1" max="31" bind:value={scheduleDayOfMonth} />
        </div>
      {:else if scheduleFrequency === "interval" && debugIntervalMode}
        <div>
          <label class="block text-sm font-medium" for="backup-interval-minutes">Every N minutes</label>
          <input id="backup-interval-minutes" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" type="number" min="1" max="43200" bind:value={scheduleIntervalMinutes} />
        </div>
      {/if}
      {#if scheduleFrequency !== "interval" || !debugIntervalMode}
        <div>
          <label class="block text-sm font-medium" for="backup-time">Time of day</label>
          <input id="backup-time" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" type="time" bind:value={scheduleTime} />
        </div>
      {/if}
      <div>
        <label class="block text-sm font-medium" for="backup-retention">Backups to retain</label>
        <input id="backup-retention" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" type="number" min="1" max="365" bind:value={scheduleRetentionCount} />
      </div>
      <div class="sm:col-span-2">
        <label class="block text-sm font-medium" for="backup-mirror-directory">Mirror Directory (optional)</label>
        <input id="backup-mirror-directory" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" type="text" placeholder="D:\\Backups" bind:value={scheduleMirrorDirectory} />
      </div>
    </div>
    <fieldset class="mt-4">
      <legend class="mb-2 text-sm font-medium">Backup Sets for automatic Backups</legend>
      <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {#each backupSets as set}
          <label class="flex items-center gap-2 rounded-md border px-3 py-2 text-sm">
            <input
              type="checkbox"
              aria-label={`Scheduled ${set}`}
              checked={scheduleSelectedSets.includes(set)}
              onchange={() => toggleScheduleSet(set)}
            />
            <span>{set}</span>
          </label>
        {/each}
      </div>
    </fieldset>
    {#if scheduleMirrorError}
      <div class="mt-4 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-sm" role="status">
        Mirror Directory copy failed: {scheduleMirrorError}
      </div>
    {/if}
    <div class="mt-4 rounded-md bg-muted p-3 text-sm">
      <strong>Next automatic Backup:</strong>
      {scheduleNextDueAt ? formatDate(scheduleNextDueAt) : "Not scheduled"}
    </div>
    <button
      type="button"
      class="mt-5 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50"
      disabled={!scheduleLoaded || scheduleBusy || scheduleSelectedSets.length === 0}
      onclick={saveSchedule}
    >
      {scheduleBusy ? "Saving…" : "Save Backup Schedule"}
    </button>
  </section>

  {#if pendingRestore}
    <section class="rounded-lg border border-amber-500/50 bg-amber-500/10 p-4" aria-labelledby="pending-restore-heading">
      <h3 id="pending-restore-heading" class="font-semibold">Pending Restore</h3>
      <p class="mt-1 text-sm max-md:break-words">
        {pendingRestore.filename} (schema v{pendingRestore.schema_version}) is verified and will replace the live database on the next launch.
      </p>
      <button type="button" class="mt-3 rounded-md border px-3 py-2 text-sm" onclick={() => void cancelRestore()}>Cancel Pending Restore</button>
    </section>
  {/if}

  <section class="rounded-lg border bg-card p-6" aria-labelledby="restore-path-heading">
    <h3 id="restore-path-heading" class="mb-1 text-lg font-semibold">Restore from an absolute path</h3>
    <p class="mb-4 text-sm text-muted-foreground">Use a `.sql` Backup on a USB stick or share mounted on the Admin machine.</p>
    <div class="flex flex-col gap-3 sm:flex-row">
      <label class="sr-only" for="restore-path">Absolute Backup path</label>
      <input id="restore-path" class="min-w-0 flex-1 rounded-md border bg-background px-3 py-2 text-sm" type="text" placeholder="C:\\Backups\\openpiso-manual-...sql" bind:value={restorePath} />
      <button type="button" class="rounded-md border px-4 py-2 text-sm disabled:opacity-50" onclick={openRestorePathDialog} disabled={!restorePath.trim() || restoreBusy}>Review Path Restore</button>
    </div>
  </section>

  <section class="rounded-lg border bg-card p-6" aria-labelledby="restore-upload-heading">
    <h3 id="restore-upload-heading" class="mb-1 text-lg font-semibold">Restore from a browser upload</h3>
    <p class="mb-4 text-sm text-muted-foreground">Upload a `.sql` Backup from this laptop. Files larger than 256 MB are refused.</p>
    <div class="flex flex-col gap-3 sm:flex-row sm:items-center">
      <label class="sr-only" for="restore-upload">SQL Backup file</label>
      <input id="restore-upload" class="min-w-0 flex-1 rounded-md border bg-background px-3 py-2 text-sm" type="file" accept=".sql,application/sql" onchange={selectRestoreUpload} disabled={restoreBusy} />
      <button type="button" class="rounded-md border px-4 py-2 text-sm disabled:opacity-50" onclick={openRestoreUploadDialog} disabled={!restoreUpload || restoreBusy}>Review Upload Restore</button>
    </div>
    {#if restoreUpload}
      <p class="mt-2 text-xs text-muted-foreground max-md:break-words">Selected: {restoreUpload.name}</p>
    {/if}
  </section>

  {#if restoreError}
    <div class="rounded-md border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive max-md:break-words" role="alert">
      Restore refused: {restoreError}
      {#if blockingPcs.length > 0}
        <div class="mt-2 font-medium">Blocking PCs: {blockingPcs.join(", ")}</div>
      {/if}
    </div>
  {/if}

  <section class="rounded-lg border bg-card p-6" aria-labelledby="backup-create-heading">
    <h3 id="backup-create-heading" class="mb-1 text-lg font-semibold">Create a Backup</h3>
    <p class="mb-4 text-sm text-muted-foreground">Choose the Backup Sets to include. All seven are selected by default.</p>
    <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {#each backupSets as set}
        <label class="flex items-center gap-2 rounded-md border px-3 py-2 text-sm">
          <input
            type="checkbox"
            aria-label={set}
            checked={selectedSets.includes(set)}
            onchange={() => toggleSet(set)}
          />
          <span>{set}</span>
        </label>
      {/each}
    </div>
    <button
      type="button"
      class="mt-5 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50"
      disabled={creating || selectedSets.length === 0}
      onclick={createBackup}
    >
      {creating ? "Creating…" : "Create Backup"}
    </button>
    {#if selectedSets.length === 0}
      <p class="mt-2 text-sm text-destructive">Select at least one Backup Set.</p>
    {/if}
  </section>

  <section class="rounded-lg border bg-card p-6" aria-labelledby="backup-catalog-heading">
    <div class="mb-4 flex items-center justify-between">
      <div>
        <h3 id="backup-catalog-heading" class="text-lg font-semibold">Backup Catalog</h3>
        <p class="text-sm text-muted-foreground">Backups are read from the app data directory, newest first.</p>
      </div>
      <button type="button" class="rounded-md border px-3 py-2 text-sm" onclick={() => void loadCatalog()} disabled={loading}>Refresh</button>
    </div>

    {#if error}
      <div class="rounded-md border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive" role="alert">
        Could not load Backup Catalog: {error}
      </div>
    {:else if loading}
      <p class="text-sm text-muted-foreground">Loading Backups…</p>
    {:else if catalog.length === 0}
      <p class="text-sm text-muted-foreground">No Backups found.</p>
    {:else}
      <div class="overflow-x-auto">
        <table class="w-full text-left text-sm">
          <thead class="border-b text-xs uppercase text-muted-foreground">
            <tr>
              <th class="px-3 py-3">Kind</th>
              <th class="px-3 py-3">Created</th>
              <th class="px-3 py-3">Schema</th>
              <th class="px-3 py-3">Size</th>
              <th class="px-3 py-3">Backup Sets</th>
              <th class="px-3 py-3"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody class="divide-y">
            {#each catalog as entry}
              <tr>
                <td class="px-3 py-3 font-medium">
                  <div>
                    {entry.kind}
                    {#if entry.catch_up}
                      <Badge variant="secondary" class="ml-2">catch-up</Badge>
                    {/if}
                  </div>
                  <div class="mt-1 text-xs font-normal text-muted-foreground">{entry.filename}</div>
                </td>
                <td class="whitespace-nowrap px-3 py-3">{formatDate(entry.created_at)}</td>
                <td class="px-3 py-3">v{entry.schema_version}</td>
                <td class="px-3 py-3">{formatBytes(entry.size_bytes)}</td>
                <td class="max-w-sm px-3 py-3">{entry.backup_sets.join(", ")}</td>
                <td class="whitespace-nowrap px-3 py-3 text-right">
                  <a
                    class="mr-3 text-primary hover:underline"
                    href={api.downloadBackupUrl(entry.filename)}
                    download={entry.filename}
                  >Download</a>
                  <button
                    type="button"
                    class="mr-3 text-primary hover:underline disabled:opacity-50"
                    disabled={pendingRestore !== null || restoreBusy}
                    onclick={() => openRestoreDialog(entry)}
                  >Restore</button>
                  <button
                    type="button"
                    class="text-destructive hover:underline disabled:opacity-50"
                    disabled={deletingFilename === entry.filename}
                    onclick={() => void deleteBackup(entry)}
                  >
                    {deletingFilename === entry.filename ? "Deleting…" : "Delete"}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

{#snippet restoreConfirmContent()}
  <h2 id="restore-confirm-heading" class="text-lg font-semibold">Confirm Restore</h2>
  <p class="mt-3 text-sm max-md:break-words">
    This will replace the live database with <strong>{restoreCandidate?.filename ?? (restorePathCandidate || restoreUploadCandidate?.name)}</strong> on the next launch.
    The current database will first be saved as a Pre-Restore Backup. No customer balances or PC time can be restored while a PC is in use or has Remaining Time.
  </p>
  <div class="mt-5 flex justify-end gap-3">
    <button type="button" class="rounded-md border px-4 py-2 text-sm" onclick={closeRestoreConfirm} disabled={restoreBusy}>Cancel</button>
    <button type="button" class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50" onclick={() => void confirmRestore()} disabled={restoreBusy}>
      {restoreBusy ? "Verifying…" : "Confirm Restore"}
    </button>
  </div>
{/snippet}

{#if restoreCandidate || restorePathCandidate || restoreUploadCandidate}
  {#if isMobile.current}
    <Sheet.Root open onOpenChange={(open) => { if (!open && !restoreBusy) closeRestoreConfirm(); }}>
      <Sheet.Content side="bottom" showCloseButton={false} aria-labelledby="restore-confirm-heading" class="max-h-none gap-0 overflow-y-auto rounded-none p-4 data-[side=bottom]:h-dvh">
        {@render restoreConfirmContent()}
      </Sheet.Content>
    </Sheet.Root>
  {:else}
    <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4" role="presentation">
      <div class="w-full max-w-lg rounded-lg border bg-card p-6 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="restore-confirm-heading">
        {@render restoreConfirmContent()}
      </div>
    </div>
  {/if}
{/if}

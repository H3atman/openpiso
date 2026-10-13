<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../api";
  import { formatDuration, formatDate } from "../format";
  import type { ManualAdjustment, Pc } from "../types";
  import { reportActionFailure, reportActionSuccess } from "../notifications";

  let pcs = $state<Pc[]>([]);
  let adjustments = $state<ManualAdjustment[]>([]);
  let error = $state("");
  let busy = $state(false);

  let pcId = $state("");
  let secondsDelta = $state(300);
  let reason = $state("");
  let createBusy = $state(false);

  async function load() {
    busy = true;
    error = "";
    try {
      const dash = await api.getDashboard();
      pcs = dash.pcs;
      adjustments = []; // Manual adjustments endpoint not paginated; for now show empty
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  async function createAdjustment() {
    if (!pcId) return;
    createBusy = true;
    try {
      await api.createManualAdjustment({
        pc_id: pcId,
        seconds_delta: secondsDelta,
        reason: reason || null,
      });
      await load();
      reportActionSuccess("create", "Manual Adjustment");
    } catch (caught) {
      reportActionFailure("create", "Manual Adjustment", caught);
    } finally {
      createBusy = false;
    }
  }

  onMount(load);
</script>

<div class="space-y-6">
  <h2 class="text-2xl font-bold tracking-tight">Manual Adjustments</h2>

  {#if busy}
    <div class="p-4 text-center text-sm text-muted-foreground">Loading…</div>
  {:else if error}
    <div class="rounded-md bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
  {:else}
  <div class="rounded-lg border bg-card p-6">
    <h3 class="mb-4 text-lg font-semibold">Adjust Guest Time</h3>
    <div class="space-y-4">
      <div>
        <label class="block text-sm font-medium" for="adjust-pc">PC</label>
        <select id="adjust-pc" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={pcId}>
          <option value="">Select PC…</option>
          {#each pcs as pc (pc.id)}
            <option value={pc.id}>{pc.name} ({formatDuration(pc.remaining_seconds)} remaining)</option>
          {/each}
        </select>
      </div>
      <div>
        <label class="block text-sm font-medium" for="adjust-seconds">Seconds Delta</label>
        <input id="adjust-seconds" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={secondsDelta} />
        <p class="mt-1 text-xs text-muted-foreground">
          Positive = add time. Negative = remove time (clamped to zero).
          Result: {formatDuration(secondsDelta)}
        </p>
      </div>
      <div>
        <label class="block text-sm font-medium" for="adjust-reason">Reason (optional)</label>
        <input id="adjust-reason" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" placeholder="e.g. Compensation for outage" bind:value={reason} />
      </div>
    </div>

    <button
      class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
      type="button"
      disabled={createBusy || !pcId}
      onclick={createAdjustment}
    >
      {createBusy ? "Applying…" : "Apply Adjustment"}
    </button>
  </div>
  {/if}
</div>

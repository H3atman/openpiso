<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../api";
  import { formatDuration } from "../format";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import {
    initialAdjustmentState,
    reduceAdjustment,
    type AdjustmentState,
  } from "../machines/adjustment";

  let current = $state.raw<AdjustmentState>(initialAdjustmentState);

  let pcId = $state("");
  let secondsDelta = $state(300);
  let reason = $state("");

  async function load() {
    try {
      const dash = await api.getDashboard();
      current = reduceAdjustment(current, { type: "loaded", pcs: dash.pcs });
    } catch (caught) {
      current = reduceAdjustment(current, {
        type: "load-failed",
        error: caught instanceof Error ? caught.message : String(caught),
      });
    }
  }

  async function createAdjustment() {
    if (!pcId) return;
    const next = reduceAdjustment(current, { type: "submit" });
    if (next === current) return;
    current = next;
    try {
      await api.createManualAdjustment({
        pc_id: pcId,
        seconds_delta: secondsDelta,
        reason: reason || null,
      });
      reportActionSuccess("create", "Manual Adjustment");
      await load();
    } catch (caught) {
      reportActionFailure("create", "Manual Adjustment", caught);
    } finally {
      current = reduceAdjustment(current, { type: "submit-finished" });
    }
  }

  onMount(load);
</script>

<div class="space-y-6">
  <h2 class="text-2xl font-bold tracking-tight">Manual Adjustments</h2>

  {#if current.phase === "loading"}
    <div class="p-4 text-center text-sm text-muted-foreground">Loading…</div>
  {:else if current.phase === "load-failed"}
    <div class="rounded-md bg-destructive/10 p-4 text-sm text-destructive">{current.error}</div>
  {:else}
  {#if current.error}
    <div class="rounded-md bg-destructive/10 p-4 text-sm text-destructive">{current.error}</div>
  {/if}
  <div class="rounded-lg border bg-card p-6">
    <h3 class="mb-4 text-lg font-semibold">Adjust Guest Time</h3>
    <div class="space-y-4">
      <div>
        <label class="block text-sm font-medium" for="adjust-pc">PC</label>
        <select id="adjust-pc" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={pcId}>
          <option value="">Select PC…</option>
          {#each current.pcs as pc (pc.id)}
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
      disabled={current.submitting || !pcId}
      onclick={createAdjustment}
    >
      {current.submitting ? "Applying…" : "Apply Adjustment"}
    </button>
  </div>
  {/if}
</div>

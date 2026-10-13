<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import * as api from "../api";
  import { formatDate, formatDateShort, formatPoints } from "../format";
  import type { PendingPointsTransfer } from "../types";
  import { reportActionFailure, reportActionSuccess } from "../notifications";

  let requests = $state<PendingPointsTransfer[]>([]);
  let error = $state("");
  let busy = $state(false);
  let refreshTimer: ReturnType<typeof setInterval> | undefined;

  // Per-row busy state prevents duplicate action while a request is in flight.
  let rowBusy = $state<Record<string, boolean>>({});

  async function load() {
    busy = true;
    error = "";
    try {
      requests = await api.listTransferRequests();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  async function approve(request: PendingPointsTransfer) {
    rowBusy[request.id] = true;
    try {
      await api.confirmTransferRequest({ code: request.code });
      await load();
      reportActionSuccess("approve", "Points Transfer Request");
    } catch (caught) {
      reportActionFailure("approve", "Points Transfer Request", caught);
    } finally {
      rowBusy[request.id] = false;
    }
  }

  async function reject(request: PendingPointsTransfer) {
    rowBusy[request.id] = true;
    try {
      await api.rejectTransferRequest({ code: request.code });
      await load();
      reportActionSuccess("reject", "Points Transfer Request");
    } catch (caught) {
      reportActionFailure("reject", "Points Transfer Request", caught);
    } finally {
      rowBusy[request.id] = false;
    }
  }

  function ageLabel(createdAt: string): string {
    const normalized = createdAt.includes("T")
      ? createdAt
      : `${createdAt.replace(" ", "T")}Z`;
    const created = new Date(normalized).getTime();
    if (Number.isNaN(created)) return createdAt;
    const seconds = Math.max(0, Math.floor((Date.now() - created) / 1000));
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    return `${hours}h ${minutes % 60}m ago`;
  }

  onMount(() => {
    load();
    // Pending requests time-box out at ~10 min; refresh on a short interval
    // so a stale request is swept to "expired" server-side and disappears
    // from the list.
    refreshTimer = setInterval(load, 15_000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <div>
      <h2 class="text-2xl font-bold tracking-tight">Points Transfer Approvals</h2>
      <p class="text-sm text-muted-foreground">
        Customers initiate a points transfer by reading a 6-digit code off
        their screen. Enter the code from the customer to confirm or reject.
      </p>
    </div>
    <button
      class="rounded-md border bg-card px-3 py-1.5 text-sm hover:bg-accent"
      type="button"
      onclick={load}
    >
      Refresh
    </button>
  </div>

  {#if error}
    <div class="rounded-md bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
  {:else}
  <div class="rounded-lg border bg-card">
    <div class="border-b px-4 py-3">
      <h3 class="font-semibold">Pending requests</h3>
    </div>
    {#if busy && requests.length === 0}
      <p class="p-4 text-sm text-muted-foreground">Loading…</p>
    {:else if requests.length === 0}
      <p class="p-4 text-sm text-muted-foreground">No pending requests.</p>
    {:else}
      <div class="divide-y">
        {#each requests as request (request.id)}
          <div class="space-y-2 p-4">
            <div class="flex flex-wrap items-center justify-between gap-3">
              <div class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 font-mono text-sm">
                <span class="font-semibold">{request.from_username}</span>
                <span class="text-muted-foreground">→</span>
                <span class="font-semibold">{request.to_username}</span>
                <span class="ml-2 text-muted-foreground">·</span>
                <span class="font-mono font-bold">{formatPoints(request.points_centi)} pts</span>
              </div>
              <div class="flex items-center gap-3">
                <div class="text-right">
                  <p class="font-mono text-2xl font-bold tracking-widest">{request.code}</p>
                </div>
              </div>
            </div>
            <div class="flex flex-wrap items-center justify-between gap-3">
              <p class="text-xs text-muted-foreground">
                Created {formatDate(request.created_at)} · {ageLabel(request.created_at)}
              </p>
              <div class="flex gap-2">
                <button
                  class="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                  type="button"
                  disabled={rowBusy[request.id]}
                  onclick={() => approve(request)}
                >
                  {rowBusy[request.id] ? "…" : "Approve"}
                </button>
                <button
                  class="rounded-md border bg-card px-3 py-1.5 text-sm font-medium hover:bg-accent disabled:opacity-50"
                  type="button"
                  disabled={rowBusy[request.id]}
                  onclick={() => reject(request)}
                >
                  Reject
                </button>
              </div>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
  {/if}

  <p class="text-xs text-muted-foreground">
    Last refreshed {formatDateShort(new Date().toISOString())}. Pending
    requests time-box out after ~10 minutes; an expired request disappears
    from this list and is audited as <code>points_transfer_expired</code>.
  </p>
</div>

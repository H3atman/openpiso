<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import * as api from "../api";
  import { formatDate, formatDateShort, formatPoints } from "../format";
  import type { PendingPointsTransfer } from "../types";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import {
    initialTransfersState,
    reduceTransfers,
    visibleRequests,
    type TransferAction,
    type TransfersEvent,
    type TransfersState,
  } from "../machines/transfers";

  let transfers = $state.raw<TransfersState>(initialTransfersState);
  let refreshTimer: ReturnType<typeof setInterval> | undefined;

  const visible = $derived(visibleRequests(transfers));

  function dispatch(event: TransfersEvent) {
    transfers = reduceTransfers(transfers, event);
  }

  // Each load carries the id the reducer assigned, so an answer older than one
  // already applied is dropped instead of overwriting newer state.
  async function load() {
    dispatch({ type: "load-started" });
    const loadId = transfers.loadId;
    try {
      const requests = await api.listTransferRequests();
      dispatch({ type: "load-succeeded", loadId, requests });
    } catch (caught) {
      const error = caught instanceof Error ? caught.message : String(caught);
      dispatch({ type: "load-failed", loadId, error });
    }
  }

  // Rows act concurrently; success hides the row and reloads the list.
  async function act(request: PendingPointsTransfer, action: TransferAction) {
    dispatch({ type: "act-started", id: request.id, action });
    try {
      const resolve =
        action === "approve" ? api.confirmTransferRequest : api.rejectTransferRequest;
      await resolve({ code: request.code });
      dispatch({ type: "act-succeeded", id: request.id });
      await load();
      reportActionSuccess(action, "Points Transfer Request");
    } catch (caught) {
      dispatch({ type: "act-failed", id: request.id });
      reportActionFailure(action, "Points Transfer Request", caught);
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

  {#if transfers.phase === "failed"}
    <div class="rounded-md bg-destructive/10 p-4 text-sm text-destructive">{transfers.error}</div>
  {:else}
  <div class="rounded-lg border bg-card">
    <div class="border-b px-4 py-3">
      <h3 class="font-semibold">Pending requests</h3>
    </div>
    {#if transfers.phase === "loading" && visible.length === 0}
      <p class="p-4 text-sm text-muted-foreground">Loading…</p>
    {:else if visible.length === 0}
      <p class="p-4 text-sm text-muted-foreground">No pending requests.</p>
    {:else}
      <div class="divide-y">
        {#each visible as request (request.id)}
          {@const row = transfers.rows[request.id]}
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
                  disabled={row?.phase === "acting"}
                  onclick={() => act(request, "approve")}
                >
                  {row?.phase === "acting" && row.action === "approve" ? "…" : "Approve"}
                </button>
                <button
                  class="rounded-md border bg-card px-3 py-1.5 text-sm font-medium hover:bg-accent disabled:opacity-50"
                  type="button"
                  disabled={row?.phase === "acting"}
                  onclick={() => act(request, "reject")}
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

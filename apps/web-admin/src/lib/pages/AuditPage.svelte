<script lang="ts">
  import * as api from "../api";
  import {
    formatAuditSummary,
    formatMoney,
    formatSignedDuration,
    formatSignedProseDuration,
    formatDate,
  } from "../format";
  import { opendatepicker } from "../utils";
  import type { AuditFilter, AuditRow } from "../types";
  import { reportActionFailure } from "../notifications";
  import { Badge } from "$lib/components/ui/badge";

  const PAGE_SIZE = 50;

  let page = $state(0);
  let total = $state(0);
  let filterFromDate = $state("");
  let filterToDate = $state("");
  let filterCategory = $state("");
  let filterActor = $state<string[]>([]);
  let filterEventType = $state<string[]>([]);
  let filterSearch = $state("");
  let results = $state<AuditRow[]>([]);
  let busy = $state(false);
  let error = $state("");
  let exportUrl = $state("");

  let actors = $state<string[]>([]);
  let eventTypes = $state<string[]>([]);
  let actorsBusy = $state(true);
  let eventTypesBusy = $state(true);

  let filterActorSearch = $state("");
  let filterEventTypeSearch = $state("");

  let filteredActors = $derived(
    filterActorSearch
      ? actors.filter((a) => a.toLowerCase().includes(filterActorSearch.toLowerCase()))
      : actors,
  );
  let filteredEventTypes = $derived(
    filterEventTypeSearch
      ? eventTypes.filter((e) => e.toLowerCase().includes(filterEventTypeSearch.toLowerCase()))
      : eventTypes,
  );

  let actorOpen = $state(false);
  let eventTypeOpen = $state(false);

  /** The filters alone, without the page window. */
  function buildFilter(): AuditFilter {
    return {
      from_date: filterFromDate || undefined,
      to_date: filterToDate || undefined,
      category: filterCategory || undefined,
      actor: filterActor.length > 0 ? filterActor : undefined,
      event_type: filterEventType.length > 0 ? filterEventType : undefined,
      search: filterSearch || undefined,
    };
  }

  let pageCount = $derived(Math.max(1, Math.ceil(total / PAGE_SIZE)));
  let rangeStart = $derived(total === 0 ? 0 : page * PAGE_SIZE + 1);
  let rangeEnd = $derived(Math.min(total, page * PAGE_SIZE + results.length));

  async function loadOptions() {
    actorsBusy = true;
    eventTypesBusy = true;
    try {
      const [a, et] = await Promise.all([
        api.fetchAuditActors(),
        api.fetchAuditEventTypes(),
      ]);
      actors = a;
      eventTypes = et;
    } catch {
      // silently fail; filters still usable with text etc.
    } finally {
      actorsBusy = false;
      eventTypesBusy = false;
    }
  }

  // kick off on mount
  $effect(() => { loadOptions(); });

  /** Loads one page. `search` resets to the first page; the pager does not. */
  async function loadPage() {
    busy = true;
    error = "";
    exportUrl = "";
    try {
      const result = await api.searchAuditLog({
        ...buildFilter(),
        limit: PAGE_SIZE,
        offset: page * PAGE_SIZE,
      });
      results = result.rows;
      total = result.total;
    } catch (caught) {
      results = [];
      total = 0;
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  async function search() {
    page = 0;
    await loadPage();
  }

  async function goToPage(next: number) {
    const clamped = Math.min(Math.max(next, 0), pageCount - 1);
    if (clamped === page) return;
    page = clamped;
    await loadPage();
  }

  // The export covers the whole filtered set, not the page on screen, so it
  // deliberately omits limit/offset.
  async function exportCsv() {
    busy = true;
    try {
      const csv = await api.exportAuditLogCsv(buildFilter());
      const blob = new Blob([csv], { type: "text/csv" });
      exportUrl = URL.createObjectURL(blob);
    } catch (caught) {
      reportActionFailure("export", "Audit Log", caught);
    } finally {
      busy = false;
    }
  }

  function toggleActor(value: string) {
    if (filterActor.includes(value)) {
      filterActor = filterActor.filter((v) => v !== value);
    } else {
      filterActor = [...filterActor, value];
    }
  }

  function toggleEventType(value: string) {
    if (filterEventType.includes(value)) {
      filterEventType = filterEventType.filter((v) => v !== value);
    } else {
      filterEventType = [...filterEventType, value];
    }
  }

  function closeDropdowns(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (!target.closest('[data-dropdown]')) {
      actorOpen = false;
      eventTypeOpen = false;
      filterActorSearch = "";
      filterEventTypeSearch = "";
    }
  }
</script>

<svelte:window onclick={closeDropdowns} />

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <h2 class="text-2xl font-bold tracking-tight">Audit Log</h2>
  </div>

  <div class="rounded-lg border bg-card p-6">
    <h3 class="mb-4 text-sm font-semibold">Filters</h3>
    <div class="grid gap-4 sm:grid-cols-3">
      <div>
        <label for="af-from" class="block text-xs font-medium text-muted-foreground">From</label>
        <input id="af-from" type="date" class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={filterFromDate} use:opendatepicker />
      </div>
      <div>
        <label for="af-to" class="block text-xs font-medium text-muted-foreground">To</label>
        <input id="af-to" type="date" class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={filterToDate} use:opendatepicker />
      </div>
      <div>
        <label for="af-category" class="block text-xs font-medium text-muted-foreground">Category</label>
        <select id="af-category" class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={filterCategory}>
          <option value="">All</option>
          <option value="auth">Auth</option>
          <option value="money">Money</option>
          <option value="time">Time</option>
          <option value="points">Points</option>
          <option value="admin">Admin</option>
          <option value="system">System</option>
        </select>
      </div>

      <!-- Actor multi-select -->
      <div class="relative" data-dropdown>
        <label for="af-actor-btn" class="block text-xs font-medium text-muted-foreground">Actor</label>
        <button
          id="af-actor-btn"
          type="button"
          class="mt-1 flex w-full items-center justify-between rounded-md border bg-background px-3 py-2 text-sm"
          onclick={() => { actorOpen = !actorOpen; eventTypeOpen = false; filterActorSearch = ""; }}
        >
          <span class={filterActor.length === 0 ? "text-muted-foreground" : ""}>
            {filterActor.length === 0 ? "All" : `${filterActor.length} selected`}
          </span>
          <svg class="size-4 text-muted-foreground" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/></svg>
        </button>
        {#if actorOpen}
          <div class="absolute z-10 mt-1 max-h-60 w-full overflow-auto rounded-md border bg-popover p-1 shadow-md">
            <div class="sticky top-0 bg-popover px-1 pb-1">
              <!-- svelte-ignore a11y_autofocus -->
              <input
                type="text"
                class="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
                placeholder="Search actors…"
                bind:value={filterActorSearch}
                onkeydown={(e) => { if (e.key === "Escape") { actorOpen = false; filterActorSearch = ""; } }}
                autofocus
              />
            </div>
            {#if actorsBusy}
              <div class="px-2 py-3 text-center text-xs text-muted-foreground">Loading…</div>
            {:else if filteredActors.length === 0 && filterActorSearch}
              <div class="px-2 py-3 text-center text-xs text-muted-foreground">No matching actors</div>
            {:else if actors.length === 0}
              <div class="px-2 py-3 text-center text-xs text-muted-foreground">No actors found</div>
            {:else}
              {#each filteredActors as actor}
                <label class="flex cursor-pointer items-center gap-2 rounded-sm px-2 py-1.5 text-sm hover:bg-accent">
                  <input type="checkbox" class="size-4 accent-primary" checked={filterActor.includes(actor)} onchange={() => toggleActor(actor)} />
                  {actor}
                </label>
              {/each}
            {/if}
          </div>
        {/if}
      </div>

      <!-- Event Type multi-select -->
      <div class="relative" data-dropdown>
        <label for="af-event-btn" class="block text-xs font-medium text-muted-foreground">Event Type</label>
        <button
          id="af-event-btn"
          type="button"
          class="mt-1 flex w-full items-center justify-between rounded-md border bg-background px-3 py-2 text-sm"
          onclick={() => { eventTypeOpen = !eventTypeOpen; actorOpen = false; filterEventTypeSearch = ""; }}
        >
          <span class={filterEventType.length === 0 ? "text-muted-foreground" : ""}>
            {filterEventType.length === 0 ? "All" : `${filterEventType.length} selected`}
          </span>
          <svg class="size-4 text-muted-foreground" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/></svg>
        </button>
        {#if eventTypeOpen}
          <div class="absolute z-10 mt-1 max-h-60 w-full overflow-auto rounded-md border bg-popover p-1 shadow-md">
            <div class="sticky top-0 bg-popover px-1 pb-1">
              <!-- svelte-ignore a11y_autofocus -->
              <input
                type="text"
                class="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
                placeholder="Search event types…"
                bind:value={filterEventTypeSearch}
                onkeydown={(e) => { if (e.key === "Escape") { eventTypeOpen = false; filterEventTypeSearch = ""; } }}
                autofocus
              />
            </div>
            {#if eventTypesBusy}
              <div class="px-2 py-3 text-center text-xs text-muted-foreground">Loading…</div>
            {:else if filteredEventTypes.length === 0 && filterEventTypeSearch}
              <div class="px-2 py-3 text-center text-xs text-muted-foreground">No matching event types</div>
            {:else if eventTypes.length === 0}
              <div class="px-2 py-3 text-center text-xs text-muted-foreground">No event types found</div>
            {:else}
              {#each filteredEventTypes as et}
                <label class="flex cursor-pointer items-center gap-2 rounded-sm px-2 py-1.5 text-sm hover:bg-accent">
                  <input type="checkbox" class="size-4 accent-primary" checked={filterEventType.includes(et)} onchange={() => toggleEventType(et)} />
                  {et}
                </label>
              {/each}
            {/if}
          </div>
        {/if}
      </div>

      <div>
        <label for="af-search" class="block text-xs font-medium text-muted-foreground">Search</label>
        <input id="af-search" class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm" placeholder="Search text…" bind:value={filterSearch} />
      </div>
    </div>

    <div class="mt-4 flex gap-3">
      <button
        class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
        type="button"
        disabled={busy}
        onclick={search}
      >
        {busy ? "Searching…" : "Search"}
      </button>
      <button
        class="rounded-md border bg-card px-4 py-2 text-sm font-medium hover:bg-accent disabled:opacity-50"
        type="button"
        disabled={busy}
        onclick={exportCsv}
      >
        Export CSV
      </button>
    </div>

    {#if exportUrl}
      <div class="mt-4">
        <a
          href={exportUrl}
          download="audit-log.csv"
          class="text-sm text-primary hover:underline"
          onclick={() => setTimeout(() => URL.revokeObjectURL(exportUrl!), 1000)}
        >
          Download CSV
        </a>
      </div>
    {/if}

  </div>

  <div class="rounded-lg border bg-card">
    {#if error}
      <div class="p-4 text-sm text-destructive">{error}</div>
    {:else if busy}
      <div class="p-4 text-center text-sm text-muted-foreground">Loading…</div>
    {:else if results.length === 0}
      <div class="p-4 text-center text-sm text-muted-foreground">No results. Apply filters and search.</div>
    {:else}
      <div class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead>
            <tr class="border-b text-left text-muted-foreground">
              <th class="px-4 py-3 font-medium">Time</th>
              <th class="px-4 py-3 font-medium">Event</th>
              <th class="px-4 py-3 font-medium">Outcome</th>
              <th class="px-4 py-3 font-medium">Actor</th>
              <th class="px-4 py-3 font-medium">PC</th>
              <th class="px-4 py-3 font-medium">Amount</th>
              <th class="px-4 py-3 font-medium">Duration</th>
              <th class="px-4 py-3 font-medium">Attendance Time</th>
              <th class="px-4 py-3 font-medium">Summary</th>
            </tr>
          </thead>
          <tbody class="divide-y">
            {#each results as row (row.id)}
              <tr class="hover:bg-muted/50">
                <td class="whitespace-nowrap px-4 py-3 text-xs text-muted-foreground">
                  {formatDate(row.created_at)}
                </td>
                <td class="px-4 py-3 font-mono text-xs">{row.event_type}</td>
                <td class="px-4 py-3">
                  <Badge
                    variant={row.outcome === "denied"
                      ? "destructive"
                      : row.outcome === "success"
                        ? "default"
                        : "secondary"}
                  >
                    {row.outcome}
                  </Badge>
                </td>
                <td class="px-4 py-3 text-xs">{row.actor}</td>
                <td class="px-4 py-3 text-xs text-muted-foreground">{row.pc_name ?? "—"}</td>
                <td class="px-4 py-3 font-mono text-xs">{row.amount_cents != null ? formatMoney(row.amount_cents) : "—"}</td>
                <td class="px-4 py-3 font-mono text-xs">{row.seconds_delta != null ? formatSignedDuration(row.seconds_delta) : "—"}</td>
                <td class="px-4 py-3 font-mono text-xs">{row.attendance_seconds_delta != null ? formatSignedProseDuration(row.attendance_seconds_delta) : "—"}</td>
                <td class="max-w-xs truncate px-4 py-3 text-xs text-muted-foreground">
                  {formatAuditSummary(row)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      <div class="flex flex-wrap items-center justify-between gap-3 border-t px-4 py-3">
        <p class="text-xs text-muted-foreground">
          Showing {rangeStart.toLocaleString()}–{rangeEnd.toLocaleString()} of {total.toLocaleString()}
        </p>
        <div class="flex items-center gap-2">
          <button
            class="rounded-md border bg-card px-3 py-1.5 text-sm font-medium hover:bg-accent disabled:opacity-50"
            type="button"
            disabled={busy || page === 0}
            onclick={() => goToPage(page - 1)}
          >
            Previous
          </button>
          <span class="text-xs text-muted-foreground">
            Page {(page + 1).toLocaleString()} of {pageCount.toLocaleString()}
          </span>
          <button
            class="rounded-md border bg-card px-3 py-1.5 text-sm font-medium hover:bg-accent disabled:opacity-50"
            type="button"
            disabled={busy || page >= pageCount - 1}
            onclick={() => goToPage(page + 1)}
          >
            Next
          </button>
        </div>
      </div>
    {/if}
  </div>
</div>

<script lang="ts">
  import { onMount } from "svelte";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import * as DropdownMenu from "$lib/components/ui/dropdown-menu/index.js";
  import MoreHorizontalIcon from "@lucide/svelte/icons/more-horizontal";
  import * as Sheet from "$lib/components/ui/sheet/index.js";
  import { IsMobile } from "$lib/hooks/is-mobile.svelte";
  import * as api from "../api";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import ResponsiveRows from "../components/ResponsiveRows.svelte";
  import RejectionCreditDialog from "../components/RejectionCreditDialog.svelte";

  import { formatDuration, formatDate, formatPoints, parsePointsToCenti } from "../format";
  import type {
    Account,
    AccountImportReport,
    CoinBatchRejection,
  } from "../types";
  import { reportActionFailure, reportActionSuccess } from "../notifications";

  const isMobile = new IsMobile();
  const instanceId = $props.id();
  const createDialogTitleId = `create-account-dialog-title-${instanceId}`;
  const adjustDialogTitleId = `adjust-account-dialog-title-${instanceId}`;
  const creditChoicesDialogTitleId = `credit-choices-dialog-title-${instanceId}`;

  // Password every reset lands on; the customer changes it from the client.
  const DEFAULT_PASSWORD = "1234567890";

  let accounts = $state<Account[]>([]);
  let error = $state("");
  let busy = $state(false);
  let search = $state("");
  // The table shows one Account status at a time. Deleted Accounts are
  // read-only history — zeroed balances, no owner action applies — so they get
  // their own trimmed column set instead of sitting disabled among live rows.
  type StatusFilter = Account["status"];
  let statusFilter = $state<StatusFilter>("active");
  // Each view offers its own subset of these; the switch resets the sort so a
  // column can never stay selected in a view that does not render it.
  type SortColumn =
    | "username"
    | "remaining_seconds"
    | "points_balance_centi"
    | "created_at"
    | "last_login_at"
    | "deleted_at";
  type SortDirection = "asc" | "desc";
  let sortColumn = $state<SortColumn>("username");
  let sortDirection = $state<SortDirection>("asc");
  const PAGE_SIZE = 10;
  let shownCount = $state(PAGE_SIZE);
  let focusAccountId = $state<string | null>(null);

  // Account Import preview
  let importInput: HTMLInputElement;
  let showImport = $state(false);
  let importBusy = $state(false);
  let importError = $state("");
  let importPayload = $state<unknown | null>(null);
  let importCommitted = $state(false);
  let importReport = $state<AccountImportReport | null>(null);

  function compareText(left: string, right: string): number {
    return left.localeCompare(right, undefined, { sensitivity: "base" });
  }

  // A missing timestamp sorts last whichever way the column points, so an
  // unknown value never outranks a real one. Returns null when both sides are
  // present and the ordinary comparison applies.
  function pinnedNullOrder(left: string | null, right: string | null): number | null {
    if (left === null && right !== null) return 1;
    if (left !== null && right === null) return -1;
    return null;
  }

  function compareAccounts(left: Account, right: Account): number {
    let comparison = 0;
    if (sortColumn === "username") {
      comparison = compareText(left.username, right.username);
    } else if (sortColumn === "remaining_seconds") {
      comparison = left.remaining_seconds - right.remaining_seconds;
    } else if (sortColumn === "points_balance_centi") {
      comparison = left.points_balance_centi - right.points_balance_centi;
    } else if (sortColumn === "created_at") {
      comparison = compareText(left.created_at, right.created_at);
    } else {
      const [leftValue, rightValue] =
        sortColumn === "deleted_at"
          ? [left.deleted_at, right.deleted_at]
          : [left.last_login_at, right.last_login_at];
      const pinned = pinnedNullOrder(leftValue, rightValue);
      if (pinned !== null) return pinned;
      if (leftValue !== null && rightValue !== null) {
        comparison = compareText(leftValue, rightValue);
      }
    }

    if (comparison !== 0) return comparison * (sortDirection === "asc" ? 1 : -1);
    comparison = compareText(left.username, right.username);
    return comparison !== 0 ? comparison : compareText(left.id, right.id);
  }

  function resetPaging() {
    shownCount = PAGE_SIZE;
    focusAccountId = null;
  }

  function sortBy(column: SortColumn) {
    if (sortColumn === column) {
      sortDirection = sortDirection === "asc" ? "desc" : "asc";
    } else {
      sortColumn = column;
      sortDirection = "asc";
    }
    resetPaging();
    selectionAnchorId = null;
  }

  function sortIndicator(column: SortColumn): string {
    return sortColumn === column ? (sortDirection === "asc" ? "↑" : "↓") : "↕";
  }

  function ariaSort(column: SortColumn): "none" | "ascending" | "descending" {
    if (sortColumn !== column) return "none";
    return sortDirection === "asc" ? "ascending" : "descending";
  }

  const searchNeedle = $derived(search.trim().toLowerCase().replace(/\s+/g, "_"));

  const filtered = $derived.by(() => {
    const visible = accounts.filter((a) => a.status === statusFilter);
    const matching = searchNeedle
      ? visible.filter((a) => a.username.toLowerCase().includes(searchNeedle))
      : visible;
    return [...matching].sort(compareAccounts);
  });

  // Cross-mode nudge: an owner hunting a customer among active Accounts should
  // learn the Account is deleted rather than conclude it never existed.
  const deletedSearchMatches = $derived(
    statusFilter === "active" && searchNeedle
      ? accounts.filter(
          (a) => a.status === "deleted" && a.username.toLowerCase().includes(searchNeedle),
        ).length
      : 0,
  );
  const pagedAccounts = $derived(filtered.slice(0, shownCount));
  const displayedCount = $derived(Math.min(shownCount, filtered.length));
  const hasMoreAccounts = $derived(displayedCount < filtered.length);

  function loadMore() {
    if (!hasMoreAccounts) return;
    focusAccountId = filtered[displayedCount]?.id ?? null;
    shownCount += PAGE_SIZE;
  }

  $effect(() => {
    if (focusAccountId === null) return;
    const target = document.getElementById(`account-row-${focusAccountId}`);
    if (target instanceof HTMLElement) {
      target.focus();
      focusAccountId = null;
    }
  });

  let selectedIds = $state<Set<string>>(new Set());
  let selectionAnchorId = $state<string | null>(null);
  let headerCheckbox = $state<HTMLInputElement | undefined>(undefined);
  const selectedCount = $derived(selectedIds.size);
  // Only the Active view renders selection controls, so anything visible there
  // is selectable and the Deleted view offers nothing to select.
  const visibleActiveIds = $derived(
    statusFilter === "active" ? pagedAccounts.map((account) => account.id) : [],
  );
  const allVisibleActiveSelected = $derived(
    visibleActiveIds.length > 0 && visibleActiveIds.every((id) => selectedIds.has(id)),
  );
  const someVisibleActiveSelected = $derived(
    visibleActiveIds.some((id) => selectedIds.has(id)) && !allVisibleActiveSelected,
  );

  $effect(() => {
    if (headerCheckbox) headerCheckbox.indeterminate = someVisibleActiveSelected;
  });

  function clearSelectionAnchor() {
    selectionAnchorId = null;
  }

  function updateSelection(ids: string[], selected: boolean) {
    const next = new Set(selectedIds);
    for (const id of ids) {
      if (selected) next.add(id);
      else next.delete(id);
    }
    selectedIds = next;
  }

  function toggleAccountSelection(account: Account, event: MouseEvent) {
    // Never cancel the click. The browser toggles `checked` before dispatch and
    // restores it after, which lands *after* Svelte has already flushed the new
    // `checked` — so a canceled click silently reverts the check mark while the
    // selection state stays set. Every branch below flips the clicked row, so
    // the native toggle already agrees with what Svelte renders.
    const currentSelected = selectedIds.has(account.id);
    if (event.shiftKey && selectionAnchorId !== null) {
      const anchorIndex = filtered.findIndex((item) => item.id === selectionAnchorId);
      const targetIndex = filtered.findIndex((item) => item.id === account.id);
      if (anchorIndex >= 0 && targetIndex >= 0) {
        const start = Math.min(anchorIndex, targetIndex);
        const end = Math.max(anchorIndex, targetIndex);
        const range = filtered.slice(start, end + 1).map((item) => item.id);
        updateSelection(range, !currentSelected);
        return;
      }
    }
    updateSelection([account.id], !currentSelected);
    selectionAnchorId = account.id;
  }

  function toggleVisibleSelection() {
    updateSelection(visibleActiveIds, !allVisibleActiveSelected);
    selectionAnchorId = null;
  }

  function clearSelection() {
    selectedIds = new Set();
    selectionAnchorId = null;
  }

  // Switching views resets sort and selection so neither carries over into a
  // table where it no longer applies. The search term survives: it is a
  // question about a customer, and the other view is where the answer may be.
  function setStatusFilter(next: StatusFilter) {
    if (next === statusFilter) return;
    statusFilter = next;
    sortColumn = next === "active" ? "username" : "deleted_at";
    sortDirection = next === "active" ? "asc" : "desc";
    resetPaging();
    clearSelection();
  }

  function handleSearchInput() {
    resetPaging();
    clearSelectionAnchor();
  }

  // Create account
  let showCreate = $state(false);
  let newUsername = $state("");
  let newPassword = $state("");
  let createMode = $state<"custom" | "default" | null>(null);

  // Reset password
  let resetTarget = $state<Account | null>(null);
  let resetBusy = $state(false);

  // Delete
  let deleteIds = $state<string[]>([]);
  let deleteBusy = $state(false);

  // Adjust time / points
  let adjustTarget = $state<Account | null>(null);
  let adjustKind = $state<"time" | "points">("time");
  let adjustAmount = $state("");
  let adjustBusy = $state(false);

  // Rejection Credits (ADR-0069). Loaded once for every account, so a row can
  // say whether it has a rejected coin without a per-row request. The list is
  // keyed on the Account **recorded on the rejection**, never on who occupies
  // the PC now.
  let creditableByAccount = $state(new Map<string, CoinBatchRejection[]>());
  let creditChoices = $state<CoinBatchRejection[] | null>(null);
  let creditTarget = $state<CoinBatchRejection | null>(null);

  function closeCreateDialog() {
    if (createMode !== null) return;
    showCreate = false;
  }

  function closeResetDialog() {
    if (resetBusy) return;
    resetTarget = null;
  }

  function closeAdjustDialog() {
    if (adjustBusy) return;
    adjustTarget = null;
  }

  function closeDeleteDialog() {
    if (deleteBusy) return;
    deleteIds = [];
  }

  function closeCreditChoicesDialog() {
    creditChoices = null;
  }

  async function loadCreditableRejections() {
    try {
      const result = await api.listCoinBatchRejections({ creditableOnly: true });
      const grouped = new Map<string, CoinBatchRejection[]>();
      for (const item of result.items) {
        if (!item.account_id || item.account_deleted) continue;
        const group = grouped.get(item.account_id) ?? [];
        group.push(item);
        grouped.set(item.account_id, group);
      }
      creditableByAccount = grouped;
    } catch {
      // Never blocks the Accounts list; the action just reads as empty.
      creditableByAccount = new Map();
    }
  }

  function openCredit(account: Account) {
    const choices = creditableByAccount.get(account.id) ?? [];
    if (choices.length === 0) return;
    if (choices.length === 1) {
      creditTarget = choices[0];
    } else {
      creditChoices = choices;
    }
  }

  async function load() {
    busy = true;
    error = "";
    clearSelection();
    try {
      accounts = await api.listAccounts();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
    await loadCreditableRejections();
  }

  function openImport() {
    showImport = true;
    importError = "";
    importPayload = null;
    importCommitted = false;
    importReport = null;
    importInput?.click();
  }

  function closeImport() {
    if (importBusy) return;
    showImport = false;
    importError = "";
    importPayload = null;
    importCommitted = false;
    importReport = null;
    if (importInput) importInput.value = "";
  }

  async function handleImportFile(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;

    importBusy = true;
    importError = "";
    importReport = null;
    try {
      const text = await file.text();
      const payload: unknown = JSON.parse(text);
      importPayload = payload;
      importCommitted = false;
      importReport = await api.importAccounts(payload, true);
    } catch (caught) {
      importError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("import", "Accounts", caught);
    } finally {
      importBusy = false;
    }
  }

  function skipReasonLabel(reason: string): string {
    return reason.replaceAll("_", " ");
  }

  async function confirmImport() {
    if (!importReport || importPayload === null || importCommitted) return;
    importBusy = true;
    importError = "";
    try {
      importReport = await api.importAccounts(importPayload, false);
      importCommitted = true;
      await load();
      reportActionSuccess("import", "Accounts");
    } catch (caught) {
      importError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("import", "Accounts", caught);
    } finally {
      importBusy = false;
    }
  }

  async function createAccount(password: string, mode: "custom" | "default") {
    if (!newUsername || !password) return;
    createMode = mode;
    try {
      await api.createAccount(newUsername, password);
      newUsername = "";
      newPassword = "";
      showCreate = false;
      await load();
      reportActionSuccess(
        "create",
        "Account",
        mode === "default" ? "default-password" : undefined,
      );
    } catch (caught) {
      reportActionFailure("create", "Account", caught);
    } finally {
      createMode = null;
    }
  }

  async function reset() {
    if (!resetTarget) return;
    resetBusy = true;
    try {
      await api.resetAccountPassword(resetTarget.id, DEFAULT_PASSWORD);
      resetTarget = null;
      await load();
      reportActionSuccess("complete", "Password Reset");
    } catch (caught) {
      reportActionFailure("complete", "Password Reset", caught);
    } finally {
      resetBusy = false;
    }
  }

  function openDelete(ids: string[]) {
    deleteIds = [...ids];
  }

  async function deleteSelected() {
    if (deleteIds.length === 0) return;
    deleteBusy = true;
    try {
      const result = await api.deleteAccounts(deleteIds);
      deleteIds = [];
      clearSelection();
      await load();
      reportActionSuccess(
        "delete",
        `${result.deleted_count} Account${result.deleted_count === 1 ? "" : "s"}`,
      );
    } catch (caught) {
      reportActionFailure("delete", "Accounts", caught);
    } finally {
      deleteBusy = false;
    }
  }

  function openAdjust(account: Account, kind: "time" | "points") {
    adjustTarget = account;
    adjustKind = kind;
    adjustAmount = "";
  }

  // `sign` is +1 (add) or -1 (remove); the server clamps the result at zero.
  async function submitAdjust(sign: 1 | -1) {
    if (!adjustTarget) return;
    adjustBusy = true;
    try {
      if (adjustKind === "time") {
        const magnitude = Number(adjustAmount);
        if (!Number.isFinite(magnitude) || magnitude <= 0) {
          reportActionFailure("adjust", "Remaining Time", new Error("Enter a positive amount"));
          return;
        }
        // Owner enters minutes; the API delta is signed seconds.
        await api.adjustAccountTime(adjustTarget.id, sign * Math.round(magnitude) * 60);
      } else {
        // Owner enters points with up to two decimals; the API delta is
        // signed integer centipoints (ADR-0038).
        const centi = parsePointsToCenti(String(adjustAmount));
        if (centi === null || centi <= 0) {
          reportActionFailure(
            "adjust",
            "Points",
            new Error("Enter a positive amount with at most two decimals"),
          );
          return;
        }
        await api.adjustAccountPoints(adjustTarget.id, sign * centi);
      }
      const subject = adjustKind === "points" ? "Points" : "Remaining Time";
      adjustTarget = null;
      await load();
      reportActionSuccess("adjust", subject);
    } catch (caught) {
      reportActionFailure(
        "adjust",
        adjustKind === "points" ? "Points" : "Remaining Time",
        caught,
      );
    } finally {
      adjustBusy = false;
    }
  }

  onMount(load);
</script>

<svelte:window
  onkeydown={(event) => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    // Claim a handled Escape so stacked overlays do not close together.
    // Reset and Delete are ConfirmDialogs; each owns its own Escape.
    if (creditChoices) {
      closeCreditChoicesDialog();
      event.preventDefault();
      return;
    }
    if (adjustTarget) {
      if (adjustBusy) return;
      closeAdjustDialog();
      event.preventDefault();
      return;
    }
    if (showCreate && createMode === null) {
      closeCreateDialog();
      event.preventDefault();
    }
  }}
/>

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <h2 class="text-2xl font-bold tracking-tight">Accounts</h2>
    <div class="flex items-center gap-2">
      <!-- On phones the secondary actions collapse into the overflow menu so
           Create Account stays a single-line primary action. -->
      <button class="rounded-md border bg-card px-3 py-1.5 text-sm hover:bg-accent max-md:hidden" type="button" onclick={load}>
        Refresh
      </button>
      <button class="rounded-md border bg-card px-3 py-1.5 text-sm hover:bg-accent max-md:hidden" type="button" onclick={openImport}>
        Import Accounts
      </button>
      <button
        class="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 max-md:whitespace-nowrap max-md:px-2.5 max-md:py-1 max-md:text-xs"
        type="button"
        onclick={() => { showCreate = true; }}
      >
        Create Account
      </button>
      <DropdownMenu.Root>
        <DropdownMenu.Trigger>
          {#snippet child({ props })}
            <button
              {...props}
              class="flex size-9 items-center justify-center rounded-md border bg-card hover:bg-accent md:hidden"
              type="button"
              aria-label="Accounts actions"
            >
              <MoreHorizontalIcon class="size-4" />
            </button>
          {/snippet}
        </DropdownMenu.Trigger>
        <DropdownMenu.Content align="end" class="w-44">
          <DropdownMenu.Item onSelect={load}>Refresh</DropdownMenu.Item>
          <DropdownMenu.Item onSelect={openImport}>Import Accounts</DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Root>
    </div>
  </div>

  <div class="rounded-lg border bg-card">
    <div class="flex items-center justify-between gap-4 border-b px-4 py-3 max-md:flex-col max-md:items-stretch max-md:gap-3">
      <div class="flex flex-wrap items-center gap-4">
        <h3 class="font-semibold">Accounts</h3>
        <label class="flex items-center gap-2 text-sm text-muted-foreground max-md:ml-auto" for="account-status">
          Status
          <select
            id="account-status"
            class="rounded-md border bg-background px-3 py-2 text-sm"
            value={statusFilter}
            onchange={(event) =>
              setStatusFilter((event.currentTarget as HTMLSelectElement).value as StatusFilter)}
          >
            <option value="active">Active</option>
            <option value="deleted">Deleted</option>
          </select>
        </label>
        {#if selectedCount > 0}
          <button
            class="rounded-md border border-destructive/50 px-2.5 py-1 text-xs text-destructive hover:bg-destructive/10"
            type="button"
            onclick={() => openDelete([...selectedIds])}
          >
            Delete Selected ({selectedCount})
          </button>
          <button
            class="rounded-md border bg-card px-2.5 py-1 text-xs hover:bg-accent"
            type="button"
            onclick={clearSelection}
          >
            Clear Selection
          </button>
        {/if}
      </div>
      <input
        class="w-full rounded-md border bg-background px-3 py-1.5 text-sm md:max-w-xs"
        type="search"
        placeholder="Search username…"
        bind:value={search}
        oninput={handleSearchInput}
      />
    </div>
    {#if busy}
      <div class="p-4 text-center text-sm text-muted-foreground">Loading…</div>
    {:else if error}
      <div class="p-4 text-sm text-destructive">{error}</div>
    {:else if filtered.length === 0}
      <div class="p-4 text-center text-sm text-muted-foreground">
        {#if statusFilter === "deleted"}
          {searchNeedle ? "No deleted accounts match your search." : "No deleted accounts."}
        {:else if searchNeedle}
          No accounts match your search.
          {#if deletedSearchMatches > 0}
            <div class="mt-1">
              {deletedSearchMatches} deleted account{deletedSearchMatches === 1 ? "" : "s"}
              match{deletedSearchMatches === 1 ? "es" : ""} your search.
            </div>
          {/if}
        {:else if accounts.length === 0}
          No accounts yet.
        {:else}
          No active accounts.
        {/if}
      </div>
    {:else}
      <div class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead>
            <tr class="border-b text-left text-muted-foreground max-md:hidden">
              <th class="px-4 py-3 font-medium" aria-sort={ariaSort("username")}>
                <div class="flex items-center gap-2">
                  {#if statusFilter === "active"}
                    <input
                      bind:this={headerCheckbox}
                      type="checkbox"
                      aria-label="Select visible active Accounts"
                      checked={allVisibleActiveSelected}
                      disabled={visibleActiveIds.length === 0}
                      onclick={toggleVisibleSelection}
                    />
                  {/if}
                  <button type="button" onclick={() => sortBy("username")}>Username <span aria-hidden="true">{sortIndicator("username")}</span></button>
                </div>
              </th>
              {#if statusFilter === "active"}
                <th class="px-4 py-3 font-medium" aria-sort={ariaSort("remaining_seconds")}>
                  <button type="button" onclick={() => sortBy("remaining_seconds")}>Remaining Time <span aria-hidden="true">{sortIndicator("remaining_seconds")}</span></button>
                </th>
                <th class="px-4 py-3 font-medium" aria-sort={ariaSort("points_balance_centi")}>
                  <button type="button" onclick={() => sortBy("points_balance_centi")}>Points <span aria-hidden="true">{sortIndicator("points_balance_centi")}</span></button>
                </th>
              {/if}
              <th class="px-4 py-3 font-medium" aria-sort={ariaSort("created_at")}>
                <button type="button" onclick={() => sortBy("created_at")}>Account Created <span aria-hidden="true">{sortIndicator("created_at")}</span></button>
              </th>
              <th class="px-4 py-3 font-medium" aria-sort={ariaSort("last_login_at")}>
                <button type="button" onclick={() => sortBy("last_login_at")}>Last Login <span aria-hidden="true">{sortIndicator("last_login_at")}</span></button>
              </th>
              {#if statusFilter === "active"}
                <th class="px-4 py-3 text-right font-medium">Actions</th>
              {:else}
                <th class="px-4 py-3 font-medium" aria-sort={ariaSort("deleted_at")}>
                  <button type="button" onclick={() => sortBy("deleted_at")}>Deleted <span aria-hidden="true">{sortIndicator("deleted_at")}</span></button>
                </th>
              {/if}
            </tr>
          </thead>
          <tbody class="divide-y">
            <ResponsiveRows rows={pagedAccounts}>
              {#snippet row(a)}
                <tr id={`account-row-${a.id}`} tabindex="-1" class="hover:bg-muted/50">
                  <td class="px-4 py-3 font-medium">
                    {#if statusFilter === "active"}
                      <input
                        type="checkbox"
                        aria-label={`Select Account ${a.username}`}
                        checked={selectedIds.has(a.id)}
                        onclick={(event) => toggleAccountSelection(a, event)}
                      />
                    {/if}
                    <div>{a.username}</div>
                    {#if a.imported_from}
                      <div class="mt-1 flex flex-wrap gap-1">
                        <Badge variant="secondary">Legacy</Badge>
                        {#if a.legacy_not_logged_in}
                          <Badge variant="secondary" class="bg-amber-100 text-amber-800 dark:bg-amber-950 dark:text-amber-200">Not yet logged in</Badge>
                        {/if}
                      </div>
                    {/if}
                  </td>
                  {#if statusFilter === "active"}
                    <td class="px-4 py-3 font-mono">{formatDuration(a.remaining_seconds)}</td>
                    <td class="px-4 py-3 font-mono">{formatPoints(a.points_balance_centi)}</td>
                  {/if}
                  <td class="px-4 py-3 text-muted-foreground">{formatDate(a.created_at)}</td>
                  <td class="px-4 py-3 text-muted-foreground">
                    {a.last_login_at === null ? "Never" : formatDate(a.last_login_at)}
                  </td>
                  {#if statusFilter === "deleted"}
                    <td class="px-4 py-3 text-muted-foreground">
                      {a.deleted_at === null ? "Unknown" : formatDate(a.deleted_at)}
                    </td>
                  {:else}
                  <td class="px-4 py-3">
                    <div class="flex items-center justify-end gap-2">
                      <button
                        class="rounded-md border bg-card px-2.5 py-1 text-xs hover:bg-accent"
                        type="button"
                        onclick={() => openAdjust(a, "time")}
                      >
                        Add/Remove Time
                      </button>
                      <DropdownMenu.Root>
                        <DropdownMenu.Trigger>
                          {#snippet child({ props })}
                            <button
                              {...props}
                              class="rounded-md border bg-card p-1.5 hover:bg-accent"
                              type="button"
                              aria-label="Account actions"
                            >
                              <MoreHorizontalIcon class="size-4" />
                            </button>
                          {/snippet}
                        </DropdownMenu.Trigger>
                        <DropdownMenu.Content align="end" class="w-44">
                          <DropdownMenu.Item onSelect={() => openAdjust(a, "points")}>
                            Add/Remove Points
                          </DropdownMenu.Item>
                          <DropdownMenu.Item
                            disabled={(creditableByAccount.get(a.id) ?? []).length === 0}
                            onSelect={() => openCredit(a)}
                          >
                            {(creditableByAccount.get(a.id) ?? []).length === 0
                              ? "No rejected coins"
                              : "Credit rejected coin"}
                          </DropdownMenu.Item>
                          <DropdownMenu.Item onSelect={() => { resetTarget = a; }}>
                            Reset Password
                          </DropdownMenu.Item>
                          <DropdownMenu.Separator />
                          <DropdownMenu.Item
                            class="text-destructive data-highlighted:text-destructive"
                            onSelect={() => openDelete([a.id])}
                          >
                            Delete Account
                          </DropdownMenu.Item>
                        </DropdownMenu.Content>
                      </DropdownMenu.Root>
                    </div>
                  </td>
                  {/if}
                </tr>
              {/snippet}
              {#snippet card(a)}
                <tr>
                  <td colspan={statusFilter === "active" ? 6 : 4} class="p-2">
                    <article
                      id={`account-row-${a.id}`}
                      tabindex="-1"
                      class="rounded-md border bg-muted/30 p-4"
                      aria-label={a.username}
                    >
                      <div class="flex flex-col gap-3">
                        <div class="flex min-w-0 items-start gap-2">
                          {#if statusFilter === "active"}
                            <label class="flex size-11 shrink-0 items-start justify-center">
                              <input
                                class="mt-1 size-5"
                                type="checkbox"
                                aria-label={`Select Account ${a.username}`}
                                checked={selectedIds.has(a.id)}
                                onclick={(event) => toggleAccountSelection(a, event)}
                              />
                            </label>
                          {/if}
                          <div class="min-w-0">
                            <h4 class="break-all font-semibold">{a.username}</h4>
                            {#if a.imported_from}
                              <div class="mt-1 flex flex-wrap gap-1">
                                <Badge variant="secondary">Legacy</Badge>
                                {#if a.legacy_not_logged_in}
                                  <Badge variant="secondary" class="bg-amber-100 text-amber-800 dark:bg-amber-950 dark:text-amber-200">Not yet logged in</Badge>
                                {/if}
                              </div>
                            {/if}
                          </div>
                        </div>
                        {#if statusFilter === "active"}
                          <div class="flex w-full items-center gap-2">
                            <button
                              class="min-h-11 flex-1 rounded-md border bg-card px-2.5 py-1 text-xs hover:bg-accent"
                              type="button"
                              onclick={() => openAdjust(a, "time")}
                            >
                              Add/Remove Time
                            </button>
                            <DropdownMenu.Root>
                              <DropdownMenu.Trigger>
                                {#snippet child({ props })}
                                  <button
                                    {...props}
                                    class="flex size-11 items-center justify-center rounded-md border bg-card hover:bg-accent"
                                    type="button"
                                    aria-label="Account actions"
                                  >
                                    <MoreHorizontalIcon class="size-4" />
                                  </button>
                                {/snippet}
                              </DropdownMenu.Trigger>
                              <DropdownMenu.Content align="end" class="w-44">
                                <DropdownMenu.Item onSelect={() => openAdjust(a, "points")}>
                                  Add/Remove Points
                                </DropdownMenu.Item>
                                <DropdownMenu.Item
                                  disabled={(creditableByAccount.get(a.id) ?? []).length === 0}
                                  onSelect={() => openCredit(a)}
                                >
                                  {(creditableByAccount.get(a.id) ?? []).length === 0
                                    ? "No rejected coins"
                                    : "Credit rejected coin"}
                                </DropdownMenu.Item>
                                <DropdownMenu.Item onSelect={() => { resetTarget = a; }}>
                                  Reset Password
                                </DropdownMenu.Item>
                                <DropdownMenu.Separator />
                                <DropdownMenu.Item
                                  class="text-destructive data-highlighted:text-destructive"
                                  onSelect={() => openDelete([a.id])}
                                >
                                  Delete Account
                                </DropdownMenu.Item>
                              </DropdownMenu.Content>
                            </DropdownMenu.Root>
                          </div>
                        {/if}
                      </div>
                      <dl class="mt-4 space-y-3">
                        {#if statusFilter === "active"}
                          <div class="flex items-start justify-between gap-4">
                            <dt class="shrink-0 text-xs text-muted-foreground">Remaining Time</dt>
                            <dd class="min-w-0 flex-1 break-words text-right font-mono font-medium">{formatDuration(a.remaining_seconds)}</dd>
                          </div>
                          <div class="flex items-start justify-between gap-4">
                            <dt class="shrink-0 text-xs text-muted-foreground">Points</dt>
                            <dd class="min-w-0 flex-1 break-words text-right font-mono font-medium">{formatPoints(a.points_balance_centi)}</dd>
                          </div>
                        {/if}
                        <div class="flex items-start justify-between gap-4">
                          <dt class="shrink-0 text-xs text-muted-foreground">Account Created</dt>
                          <dd class="min-w-0 flex-1 break-words text-right text-muted-foreground">{formatDate(a.created_at)}</dd>
                        </div>
                        <div class="flex items-start justify-between gap-4">
                          <dt class="shrink-0 text-xs text-muted-foreground">Last Login</dt>
                          <dd class="min-w-0 flex-1 break-words text-right text-muted-foreground">
                            {a.last_login_at === null ? "Never" : formatDate(a.last_login_at)}
                          </dd>
                        </div>
                        {#if statusFilter === "deleted"}
                          <div class="flex items-start justify-between gap-4">
                            <dt class="shrink-0 text-xs text-muted-foreground">Deleted</dt>
                            <dd class="min-w-0 flex-1 break-words text-right text-muted-foreground">
                              {a.deleted_at === null ? "Unknown" : formatDate(a.deleted_at)}
                            </dd>
                          </div>
                        {/if}
                      </dl>
                    </article>
                  </td>
                </tr>
              {/snippet}
            </ResponsiveRows>
          </tbody>
        </table>
      </div>
      {#if hasMoreAccounts}
        <div class="border-t p-2">
          <button
            class="min-h-11 w-full rounded-md border bg-card px-4 py-2 text-sm font-medium hover:bg-accent"
            type="button"
            onclick={loadMore}
          >
            Load More ({displayedCount} of {filtered.length})
          </button>
        </div>
      {/if}
    {/if}
  </div>
</div>

<input
  bind:this={importInput}
  class="hidden"
  type="file"
  accept="application/json,.json"
  aria-label="Martec account export"
  onchange={handleImportFile}
/>

<!-- Account Import preview dialog -->
{#snippet importDialogContent()}
      <div class="flex items-start justify-between gap-4">
        <div>
          <h3 id="account-import-title" class="text-lg font-semibold">Import Accounts</h3>
          <p class="mt-1 text-sm text-muted-foreground">
            {#if importCommitted}
              Import completed. The report below is the committed result.
            {:else}
              Preview only. Nothing is written until you confirm the import.
            {/if}
          </p>
        </div>
        <button
          class="rounded-md border bg-card px-3 py-1.5 text-sm hover:bg-accent disabled:opacity-50"
          type="button"
          disabled={importBusy}
          onclick={closeImport}
        >
          Cancel
        </button>
      </div>

      {#if importBusy}
        <div class="mt-6 rounded-md border p-4 text-center text-sm text-muted-foreground">
          {importReport ? "Importing…" : "Reading export…"}
        </div>
      {:else if importError}
        <div class="mt-6 rounded-md bg-destructive/10 p-4 text-sm text-destructive">{importError}</div>
      {:else if !importReport}
        <div class="mt-6 rounded-md border p-4 text-sm text-muted-foreground">
          Choose a Martec <span class="font-mono">AccountsData.json</span> export to preview it.
        </div>
      {:else}
        <div class="mt-6 grid gap-3 sm:grid-cols-4">
          <div class="rounded-md border p-3">
            <div class="text-xs text-muted-foreground">{importCommitted ? "Created" : "Would create"}</div>
            <div class="mt-1 text-xl font-semibold">{importReport.totals.created}</div>
          </div>
          <div class="rounded-md border p-3">
            <div class="text-xs text-muted-foreground">Skipped</div>
            <div class="mt-1 text-xl font-semibold">{importReport.totals.skipped}</div>
          </div>
          <div class="rounded-md border p-3">
            <div class="text-xs text-muted-foreground">Remaining Time</div>
            <div class="mt-1 font-mono text-xl font-semibold">{formatDuration(importReport.totals.total_seconds)}</div>
          </div>
          <div class="rounded-md border p-3">
            <div class="text-xs text-muted-foreground">Points</div>
            <div class="mt-1 font-mono text-xl font-semibold">{formatPoints(importReport.totals.total_points_centi)}</div>
          </div>
        </div>

        <div class="mt-6 space-y-6">
          <section>
            <h4 class="mb-2 font-semibold">{importCommitted ? "Created" : "Would create"}</h4>
            <div class="overflow-x-auto rounded-md border">
              <table class="w-full text-sm">
                <thead>
                  <tr class="border-b text-left text-muted-foreground max-md:hidden">
                    <th class="px-3 py-2 font-medium">Username</th>
                    <th class="px-3 py-2 font-medium">Remaining Time</th>
                    <th class="px-3 py-2 font-medium">Points</th>
                  </tr>
                </thead>
                <tbody class="divide-y">
                  {#if importReport.rows.some((row) => row.outcome === "created")}
                    <ResponsiveRows rows={importReport.rows.filter((row) => row.outcome === "created")}>
                      {#snippet row(item)}
                        <tr>
                          <td class="px-3 py-2">
                            {#if item.final_username}
                              <div class="font-medium">{item.final_username}</div>
                              <div class="text-xs text-amber-700">Renamed from {item.original_username}</div>
                            {:else}
                              <span class="font-medium">{item.original_username}</span>
                            {/if}
                          </td>
                          <td class="px-3 py-2 font-mono">{formatDuration(item.remaining_seconds)}</td>
                          <td class="px-3 py-2 font-mono">{formatPoints(item.points_balance_centi)}</td>
                        </tr>
                      {/snippet}
                      {#snippet card(item)}
                        <tr>
                          <td colspan="3" class="p-2">
                            <article class="rounded-md border bg-muted/30 p-4" aria-label={item.final_username ?? item.original_username}>
                              {#if item.final_username}
                                <h4 class="font-semibold">{item.final_username}</h4>
                                <p class="text-xs text-amber-700">Renamed from {item.original_username}</p>
                              {:else}
                                <h4 class="font-semibold">{item.original_username}</h4>
                              {/if}
                              <dl class="mt-4 grid gap-3 sm:grid-cols-2">
                                <div>
                                  <dt class="text-xs text-muted-foreground">Remaining Time</dt>
                                  <dd class="mt-1 font-mono font-medium">{formatDuration(item.remaining_seconds)}</dd>
                                </div>
                                <div>
                                  <dt class="text-xs text-muted-foreground">Points</dt>
                                  <dd class="mt-1 font-mono font-medium">{formatPoints(item.points_balance_centi)}</dd>
                                </div>
                              </dl>
                            </article>
                          </td>
                        </tr>
                      {/snippet}
                    </ResponsiveRows>
                  {:else}
                    <tr><td class="px-3 py-3 text-muted-foreground" colspan="3">{importCommitted ? "No accounts were created." : "No accounts would be created."}</td></tr>
                  {/if}
                </tbody>
              </table>
            </div>
          </section>

          <section>
            <h4 class="mb-2 font-semibold">Skipped</h4>
            <div class="overflow-x-auto rounded-md border">
              <table class="w-full text-sm">
                <thead>
                  <tr class="border-b text-left text-muted-foreground max-md:hidden">
                    <th class="px-3 py-2 font-medium">Username</th>
                    <th class="px-3 py-2 font-medium">Reason</th>
                    <th class="px-3 py-2 font-medium">Converted balances</th>
                  </tr>
                </thead>
                <tbody class="divide-y">
                  {#if importReport.rows.some((row) => row.outcome === "skipped")}
                    <ResponsiveRows rows={importReport.rows.filter((row) => row.outcome === "skipped")}>
                      {#snippet row(item)}
                        <tr>
                          <td class="px-3 py-2">
                            {#if item.final_username}
                              <div class="font-medium">{item.final_username}</div>
                              <div class="text-xs text-amber-700">Renamed from {item.original_username}</div>
                            {:else}
                              <span class="font-medium">{item.original_username}</span>
                            {/if}
                          </td>
                          <td class="px-3 py-2">
                            <code class="rounded bg-muted px-1.5 py-0.5 text-xs">{item.skip_reason}</code>
                            <div class="mt-1 text-xs text-muted-foreground">{skipReasonLabel(item.skip_reason ?? "unknown")}</div>
                          </td>
                          <td class="px-3 py-2 font-mono text-xs">
                            {formatDuration(item.remaining_seconds)} · {formatPoints(item.points_balance_centi)} pts
                          </td>
                        </tr>
                      {/snippet}
                      {#snippet card(item)}
                        <tr>
                          <td colspan="3" class="p-2">
                            <article class="rounded-md border bg-muted/30 p-4" aria-label={item.final_username ?? item.original_username}>
                              {#if item.final_username}
                                <h4 class="font-semibold">{item.final_username}</h4>
                                <p class="text-xs text-amber-700">Renamed from {item.original_username}</p>
                              {:else}
                                <h4 class="font-semibold">{item.original_username}</h4>
                              {/if}
                              <dl class="mt-4 grid gap-3 sm:grid-cols-2">
                                <div>
                                  <dt class="text-xs text-muted-foreground">Reason</dt>
                                  <dd class="mt-1">
                                    <code class="rounded bg-muted px-1.5 py-0.5 text-xs">{item.skip_reason}</code>
                                    <div class="mt-1 text-xs text-muted-foreground">{skipReasonLabel(item.skip_reason ?? "unknown")}</div>
                                  </dd>
                                </div>
                                <div>
                                  <dt class="text-xs text-muted-foreground">Converted balances</dt>
                                  <dd class="mt-1 font-mono text-xs">
                                    {formatDuration(item.remaining_seconds)} · {formatPoints(item.points_balance_centi)} pts
                                  </dd>
                                </div>
                              </dl>
                            </article>
                          </td>
                        </tr>
                      {/snippet}
                    </ResponsiveRows>
                  {:else}
                    <tr><td class="px-3 py-3 text-muted-foreground" colspan="3">No rows would be skipped.</td></tr>
                  {/if}
                </tbody>
              </table>
            </div>
          </section>
        </div>

        {#if !importCommitted}
          <div class="mt-6 flex justify-end">
            <button
              class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              type="button"
              disabled={importBusy}
              onclick={confirmImport}
            >
              {importBusy ? "Importing…" : "Confirm Import"}
            </button>
          </div>
        {/if}
      {/if}
{/snippet}

{#if showImport}
  {#if isMobile.current}
    <Sheet.Root open onOpenChange={(open) => { if (!open && !importBusy) closeImport(); }}>
      <Sheet.Content side="bottom" showCloseButton={false} aria-labelledby="account-import-title" class="max-h-none gap-0 overflow-y-auto rounded-none p-4 data-[side=bottom]:h-dvh">
        {@render importDialogContent()}
      </Sheet.Content>
    </Sheet.Root>
  {:else}
    <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
      <div class="max-h-[90vh] w-full max-w-5xl overflow-y-auto rounded-lg border bg-card p-6 shadow-lg" role="dialog" aria-modal="true" aria-labelledby="account-import-title">
        {@render importDialogContent()}
      </div>
    </div>
  {/if}
{/if}

<!-- Create Account dialog -->
{#if showCreate}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
    <div
      class="w-full max-w-sm rounded-lg border bg-card p-6 shadow-lg"
      role="dialog"
      aria-modal="true"
      aria-labelledby={createDialogTitleId}
      tabindex="-1"
    >
      <h3 class="mb-4 text-sm font-semibold" id={createDialogTitleId}>Create Account</h3>
      <div class="space-y-3">
        <input
          class="w-full rounded-md border bg-background px-3 py-2 text-sm"
          placeholder="Username"
          bind:value={newUsername}
        />
        <input
          class="w-full rounded-md border bg-background px-3 py-2 text-sm"
          type="password"
          placeholder="Password"
          bind:value={newPassword}
        />
      </div>
      <Button
        class="mt-4 w-full"
        type="button"
        variant="outline"
        disabled={createMode !== null || !newUsername}
        onclick={() => createAccount(DEFAULT_PASSWORD, "default")}
      >
        {createMode === "default" ? "Creating…" : "Create with Default Password"}
      </Button>
      <p class="mt-2 text-xs text-muted-foreground">
        Default password: {DEFAULT_PASSWORD}. Customer should change it after login.
      </p>
      <div class="mt-4 flex gap-2">
        <Button
          class="flex-1"
          type="button"
          variant="outline"
          disabled={createMode !== null}
          onclick={closeCreateDialog}
        >
          Cancel
        </Button>
        <Button
          class="flex-1"
          type="button"
          disabled={createMode !== null || !newUsername || !newPassword}
          onclick={() => createAccount(newPassword, "custom")}
        >
          {createMode === "custom" ? "Creating…" : "Create"}
        </Button>
      </div>
    </div>
  </div>
{/if}

<!-- Reset Password dialog -->
{#if resetTarget}
  <ConfirmDialog
    title={`Reset Password — ${resetTarget.username}`}
    confirmLabel="Confirm Reset"
    busyLabel="Resetting…"
    busy={resetBusy}
    onconfirm={reset}
    oncancel={closeResetDialog}
  >
    <p class="text-sm">
      This sets {resetTarget.username}'s password to <span class="font-mono">{DEFAULT_PASSWORD}</span>.
    </p>
    <p class="mt-2 text-xs text-muted-foreground">
      Tell the customer to change it after logging in.
    </p>
  </ConfirmDialog>
{/if}

<!-- Add/Remove Time or Points dialog -->
{#if adjustTarget}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
    <div
      class="w-full max-w-sm rounded-lg border bg-card p-6 shadow-lg"
      role="dialog"
      aria-modal="true"
      aria-labelledby={adjustDialogTitleId}
      tabindex="-1"
    >
      <h3 class="mb-4 text-sm font-semibold" id={adjustDialogTitleId}>
        {adjustKind === "time" ? "Add/Remove Time" : "Add/Remove Points"} — {adjustTarget.username}
      </h3>
      <input
        class="w-full rounded-md border bg-background px-3 py-2 text-sm"
        type={adjustKind === "time" ? "number" : "text"}
        inputmode="decimal"
        placeholder={adjustKind === "time" ? "Minutes" : "Points (e.g. 1.50)"}
        bind:value={adjustAmount}
      />
      <p class="mt-2 text-xs text-muted-foreground">
        {adjustKind === "time" ? "Amount in minutes." : "Points, up to two decimals."} Balance never goes below zero.
      </p>
      <div class="mt-4 flex gap-2">
        <Button
          class="flex-1"
          type="button"
          variant="outline"
          disabled={adjustBusy}
          onclick={closeAdjustDialog}
        >
          Cancel
        </Button>
        <Button
          class="flex-1"
          type="button"
          disabled={adjustBusy}
          onclick={() => submitAdjust(1)}
        >
          Add
        </Button>
        <Button
          class="flex-1"
          type="button"
          variant="destructive"
          disabled={adjustBusy}
          onclick={() => submitAdjust(-1)}
        >
          Remove
        </Button>
      </div>
    </div>
  </div>
{/if}

<!-- Delete Account dialog -->
{#if deleteIds.length > 0}
  <ConfirmDialog
    title={`Delete ${deleteIds.length} Account${deleteIds.length === 1 ? "" : "s"}?`}
    confirmLabel={`Delete ${deleteIds.length} Account${deleteIds.length === 1 ? "" : "s"}`}
    busyLabel="Deleting…"
    busy={deleteBusy}
    destructive
    onconfirm={deleteSelected}
    oncancel={closeDeleteDialog}
  >
    <div class="rounded-md bg-destructive/10 p-3 text-xs text-destructive">
      This soft-deletes the selected Accounts, forfeits Remaining Time and Points, ends Active Sessions, revokes Account Sessions, preserves history, and makes each Username reusable only by Account Import. No usernames are listed.
    </div>
  </ConfirmDialog>
{/if}

<!-- Rejection Credit: pick which rejected coin, when the Account has several -->
{#if creditChoices}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
    onclick={closeCreditChoicesDialog}
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="w-full max-w-md rounded-lg border bg-card p-6 shadow-lg"
      role="dialog"
      aria-modal="true"
      aria-labelledby={creditChoicesDialogTitleId}
      tabindex="-1"
      onclick={(event) => event.stopPropagation()}
    >
      <h3 class="text-sm font-semibold" id={creditChoicesDialogTitleId}>Which rejected coin?</h3>
      <p class="mt-1 text-xs text-muted-foreground">
        Coin Batch Rejections recorded for this Account in the last 30 days.
      </p>
      <ul class="mt-4 max-h-72 space-y-2 overflow-y-auto">
        {#each creditChoices as choice (choice.rejection_audit_id)}
          <li>
            <Button
              class="h-auto w-full justify-start whitespace-normal px-3 py-2 text-left text-sm"
              type="button"
              variant="outline"
              onclick={() => {
                creditTarget = choice;
                creditChoices = null;
              }}
            >
              <span class="block w-full">
                <span class="font-medium">{formatDate(choice.created_at)}</span>
                <span class="block text-xs text-muted-foreground max-md:break-words">
                  {choice.pc_name ?? choice.pc_id}
                  {#if choice.pulse_count !== null}
                    · {choice.pulse_count} pulses
                  {:else}
                    · {choice.remaining_coins} of {choice.cap_coins} suppressed coins left
                  {/if}
                </span>
              </span>
            </Button>
          </li>
        {/each}
      </ul>
      <div class="mt-4 flex justify-end">
        <Button
          type="button"
          variant="outline"
          onclick={closeCreditChoicesDialog}
        >
          Cancel
        </Button>
      </div>
    </div>
  </div>
{/if}

{#if creditTarget}
  <RejectionCreditDialog
    rejection={creditTarget}
    oncredited={loadCreditableRejections}
    onclose={() => (creditTarget = null)}
  />
{/if}

<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { toast } from "svelte-sonner";
  import * as api from "../api";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import { Badge } from "$lib/components/ui/badge";
  import { reportActionFailure, reportActionSuccess, type NotificationAction } from "../notifications";
  import {
    formatClockDifference,
    formatMoney,
    formatDuration,
    statusLabel,
    formatPoints,
    formatDate,
    hostReachabilityLabel,
    clientReachabilityLabel,
    guardCoverageLabel,
    incidentKindHint,
    incidentKindLabel,
  } from "../format";
  import type {
    DashboardSnapshot,
    Pc,
    PcMonitoringCheck,
  } from "../types";

  let dashboard = $state<DashboardSnapshot | null>(null);
  let error = $state("");
  let refreshTimer: ReturnType<typeof setInterval> | undefined;

  // --- PC Monitoring Check now (ADR-0076): read-only facts per PC row. ---
  let checkResults = $state<Record<string, PcMonitoringCheck>>({});
  let checkingPcId = $state<string | null>(null);
  let checkingAll = $state(false);

  // --- Per-PC Monitoring Suppression (ADR-0076 / #342). The pending target
  // is snapshotted when the PIN dialog opens; the dashboard's 5s refresh may
  // move underneath, but the mutation always applies to the clicked PC. ---
  const SUPPRESSION_DURATIONS = [15, 30, 45, 60];
  let suppressionTarget = $state<{ pcId: string; pcName: string } | null>(null);
  let cancellingSuppression = $state<{ pcId: string; pcName: string } | null>(null);
  let suppressionDuration = $state(30);
  let suppressionPin = $state("");
  let suppressionError = $state("");
  let suppressionBusy = $state(false);

  function askSuppress(pc: Pc) {
    if (suppressionBusy) return;
    suppressionTarget = { pcId: pc.id, pcName: pc.name };
    suppressionDuration = 30;
    suppressionPin = "";
    suppressionError = "";
  }

  function askCancelSuppression(pc: Pc) {
    if (suppressionBusy || !dashboard?.pc_monitoring.suppressions[pc.id]) return;
    cancellingSuppression = { pcId: pc.id, pcName: pc.name };
    suppressionPin = "";
    suppressionError = "";
  }

  function closeSuppressionDialogs() {
    if (suppressionBusy) return;
    suppressionTarget = null;
    cancellingSuppression = null;
  }

  async function confirmSuppress() {
    const target = suppressionTarget;
    if (!target || suppressionBusy || !suppressionPin) return;
    await runSuppressionMutation(
      "suppress",
      target.pcName,
      () => (suppressionTarget = null),
      () => api.setPcMonitoringSuppression(target.pcId, suppressionDuration, suppressionPin),
    );
  }

  async function confirmCancelSuppression() {
    const target = cancellingSuppression;
    if (!target || suppressionBusy || !suppressionPin) return;
    await runSuppressionMutation(
      "cancel-suppression",
      target.pcName,
      () => (cancellingSuppression = null),
      () => api.cancelPcMonitoringSuppression(target.pcId, suppressionPin),
    );
  }

  /** Shared submit shape for both suppression mutations: busy-guard, PIN
   *  error surfacing, toast outcome, dialog close, dashboard refresh. */
  async function runSuppressionMutation(
    action: NotificationAction,
    subject: string,
    closeDialog: () => void,
    call: () => Promise<unknown>,
  ) {
    suppressionBusy = true;
    suppressionError = "";
    try {
      await call();
      reportActionSuccess(action, subject);
      closeDialog();
      await load();
    } catch (caught) {
      suppressionError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure(action, subject, caught);
    } finally {
      suppressionBusy = false;
    }
  }

  async function runCheck(pc: Pc) {
    if (checkingPcId || checkingAll) return;
    checkingPcId = pc.id;
    try {
      const result = await api.checkPcMonitoring(pc.id);
      checkResults[pc.id] = result;
      reportActionSuccess("check", pc.name);
    } catch (caught) {
      reportActionFailure("check", pc.name, caught);
    } finally {
      checkingPcId = null;
    }
  }

  async function runCheckAll() {
    if (checkingAll || checkingPcId) return;
    checkingAll = true;
    try {
      const results = await api.checkAllPcMonitoring();
      const next: Record<string, PcMonitoringCheck> = {};
      for (const result of results) next[result.pc_id] = result;
      checkResults = next;
      reportActionSuccess(
        "check",
        `${results.length} eligible ${results.length === 1 ? "PC" : "PCs"}`,
      );
    } catch (caught) {
      reportActionFailure("check", "all PCs", caught);
    } finally {
      checkingAll = false;
    }
  }

  /** The Owner Sign-Out target, snapshotted when the dialog opens. The 5s
   *  refresh keeps running underneath it; the snapshot's `sessionId` is what
   *  the server checks, so a row that changed meanwhile degrades to an
   *  "already ended" no-op instead of ejecting the wrong customer. */
  type SignOutTarget = {
    pcId: string;
    pcName: string;
    username: string;
    sessionId: string;
  };
  let signOutTarget = $state<SignOutTarget | null>(null);
  let signOutBusy = $state(false);

  function askSignOut(pc: Pc) {
    if (!pc.active_session) return;
    signOutTarget = {
      pcId: pc.id,
      pcName: pc.name,
      username: pc.active_session.username,
      sessionId: pc.active_session.id,
    };
  }

  function closeSignOut() {
    if (signOutBusy) return;
    signOutTarget = null;
  }

  async function confirmSignOut() {
    if (!signOutTarget || signOutBusy) return;
    const target = signOutTarget;
    signOutBusy = true;
    try {
      const result = await api.signOutPcSession(target.pcId, target.sessionId);
      const subject = `${result.username ?? target.username} from ${target.pcName}`;
      if (result.signed_out) {
        reportActionSuccess("sign-out", subject);
      } else {
        // Neither a success nor a failure: the session ended on its own
        // between the dashboard snapshot and the click.
        toast.info("That session already ended");
      }
      signOutTarget = null;
      await load();
    } catch (caught) {
      reportActionFailure(
        "sign-out",
        `${target.username} from ${target.pcName}`,
        caught,
      );
    } finally {
      signOutBusy = false;
    }
  }

  async function load() {
    try {
      dashboard = await api.getDashboard();
      error = "";
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function pcStatusColor(pc: Pc): string {
    switch (pc.status) {
      case "in_use": return "text-primary";
      case "online": return "text-green-400";
      case "offline": return "text-muted-foreground";
      case "locked": return "text-destructive";
      default: return "text-muted-foreground";
    }
  }

  function pcIndicatorColor(pc: Pc): string {
    switch (pc.status) {
      case "in_use": return "bg-primary";
      case "online": return "bg-green-400";
      case "offline": return "bg-muted-foreground";
      case "locked": return "bg-destructive";
      default: return "bg-muted-foreground";
    }
  }

  onMount(() => {
    load();
    refreshTimer = setInterval(load, 5000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <div class="max-md:min-w-0">
      <h2 class="text-2xl font-bold tracking-tight">Dashboard</h2>
      <p class="text-sm text-muted-foreground max-md:break-words">Shop {dashboard?.shop_name ?? ""}</p>
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
  {/if}

  {#if dashboard && !dashboard.pc_monitoring.enabled}
    <!-- Standing advisory (ADR-0076): monitoring is off, so nothing is being
         watched. Inline, not a toast — it stays until the owner acts. -->
    <div
      class="rounded-md bg-amber-50 p-3 text-sm text-amber-800 dark:bg-amber-950 dark:text-amber-200"
      role="status"
      data-testid="monitoring-advisory"
    >
      PC Monitoring is disabled — no PC is being watched for Client
      Reachability Mismatches. Enable it in Settings → PC Monitoring.
    </div>
  {/if}

  {#if dashboard?.pc_monitoring.escalation_enabled}
    <!-- Standing advisory (ADR-0077), the inverse of the one above: this one
         stands while the feature is ON, because powering customer PCs off is
         not something the owner should discover from the Audit Log. -->
    <div
      class="rounded-md bg-amber-50 p-3 text-sm text-amber-800 dark:bg-amber-950 dark:text-amber-200"
      role="status"
      data-testid="escalation-advisory"
    >
      Client Recovery Escalation is on — a PC whose Client cannot be relaunched
      will be powered off and its session ended. Balance, time, and Points are
      never affected. Change this in Settings → PC Monitoring.
    </div>
  {/if}

  {#if dashboard}
    <div class="grid gap-4 sm:grid-cols-3">
      <div class="rounded-lg border bg-card p-4">
        <p class="text-sm text-muted-foreground">Today's Sales</p>
        <p class="mt-1 text-2xl font-bold">{formatMoney(dashboard.today.sales_cents)}</p>
        <p class="text-xs text-muted-foreground">{dashboard.today.transaction_count} transactions</p>
      </div>
      <div class="rounded-lg border bg-card p-4">
        <p class="text-sm text-muted-foreground">Time Sold Today</p>
        <p class="mt-1 text-2xl font-bold">{formatDuration(dashboard.today.seconds_sold)}</p>
      </div>
      <div class="rounded-lg border bg-card p-4">
        <p class="text-sm text-muted-foreground">PCs</p>
        <p class="mt-1 text-2xl font-bold">{dashboard.pcs.length}</p>
        <p class="text-xs text-muted-foreground">
          {dashboard.pcs.filter((p) => p.status === "in_use").length} in use
        </p>
      </div>
    </div>

    <div class="rounded-lg border bg-card">
      <div class="flex items-center justify-between border-b px-4 py-3">
        <h3 class="font-semibold">PCs</h3>
        <button
          class="rounded-md border px-2 py-1 text-xs hover:bg-accent disabled:opacity-50"
          type="button"
          disabled={checkingAll || checkingPcId !== null}
          onclick={runCheckAll}
        >
          {checkingAll ? "Checking…" : "Check All PCs"}
        </button>
      </div>
      <div class="divide-y">
        {#each dashboard.pcs as pc (pc.id)}
          <div id={`pc-${pc.id}`} class="scroll-mt-4 px-4 py-3" data-pc-id={pc.id}>
            <div
              class="flex items-center justify-between gap-3 max-md:flex-wrap max-md:items-start max-md:gap-y-2"
            >
              <div class="flex min-w-0 flex-1 items-center gap-3">
                <span class="h-2 w-2 shrink-0 rounded-full {pcIndicatorColor(pc)}"></span>
                <div class="min-w-0">
                  <p class="truncate font-medium">{pc.name}</p>
                  <p class="text-xs text-muted-foreground">{pc.active_session?.username ?? "Guest"}</p>
                  {#if pc.clock_difference_ms != null && Math.abs(pc.clock_difference_ms) > 60_000}
                    <p class="text-xs text-amber-700 dark:text-amber-300" role="status">
                      Client clock is {formatClockDifference(pc.clock_difference_ms)}.
                    </p>
                  {/if}
                </div>
              </div>
              <div class="flex shrink-0 items-center gap-3 max-md:max-w-[60%] max-md:flex-col max-md:items-end max-md:gap-1">
                {#if dashboard.pc_monitoring.active_incidents[pc.id]?.length}
                  <!-- Active incidents stay visible regardless of Alert read
                       state; acknowledging records review, never repair. -->
                  <div
                    class="flex shrink-0 flex-col items-end gap-1"
                    data-testid="incident-badges-{pc.id}"
                  >
                    {#each dashboard.pc_monitoring.active_incidents[pc.id] as kind (kind)}
                      <Badge
                        class="bg-destructive text-white dark:bg-destructive max-md:h-auto max-md:max-w-full max-md:whitespace-normal max-md:text-right"
                        variant="destructive"
                        title={incidentKindHint(kind)}
                      >
                        {incidentKindLabel(kind)}
                      </Badge>
                    {/each}
                  </div>
                {/if}
                <div class="text-right">
                  <p class="font-mono text-sm font-bold {pcStatusColor(pc)}">
                    {statusLabel(pc.status)}
                  </p>
                  <p class="font-mono text-xs text-muted-foreground">
                    {formatDuration(pc.remaining_seconds)}
                  </p>
                </div>
                {#if dashboard.pc_monitoring.suppressions[pc.id]}
                  <!-- Monitoring Suppression (ADR-0076): absolute expiry as
                       Admin computed it, never a countdown. -->
                  <Badge
                    class="shrink-0 bg-sky-600 text-white dark:bg-sky-400 dark:text-sky-950 max-md:h-auto max-md:max-w-full max-md:whitespace-normal max-md:text-right"
                    data-testid="suppression-badge-{pc.id}"
                  >
                    Suppressed until {formatDate(dashboard.pc_monitoring.suppressions[pc.id])}
                  </Badge>
                {/if}
              </div>
              <div class="flex items-center gap-2 max-md:w-full max-md:flex-wrap max-md:justify-end">
                <button
                  class="rounded-md border px-2 py-1 text-xs hover:bg-accent disabled:opacity-50 md:shrink-0 max-md:min-h-11 max-md:min-w-11 max-md:flex-1"
                  type="button"
                  disabled={checkingAll || checkingPcId !== null}
                  onclick={() => runCheck(pc)}
                >
                  {checkingPcId === pc.id ? "Checking…" : "Check"}
                </button>
                {#if dashboard.pc_monitoring.suppressions[pc.id]}
                  <button
                    class="rounded-md border px-2 py-1 text-xs hover:bg-accent md:shrink-0 max-md:min-h-11 max-md:min-w-11 max-md:flex-1"
                    type="button"
                    onclick={() => askCancelSuppression(pc)}
                  >
                    Cancel Suppression
                  </button>
                {:else}
                  <button
                    class="rounded-md border px-2 py-1 text-xs hover:bg-accent md:shrink-0 max-md:min-h-11 max-md:min-w-11 max-md:flex-1"
                    type="button"
                    onclick={() => askSuppress(pc)}
                  >
                    Suppress
                  </button>
                {/if}
                {#if pc.active_session}
                  <button
                    class="rounded-md border px-2 py-1 text-xs hover:bg-accent md:shrink-0 max-md:min-h-11 max-md:min-w-11 max-md:flex-1"
                    type="button"
                    onclick={() => askSignOut(pc)}
                  >
                    Sign Out
                  </button>
                {/if}
              </div>
            </div>
            {#if checkResults[pc.id]}
              <!-- Standing facts stay inline; only action outcomes use toasts. -->
              <p
                class="mt-2 font-mono text-xs text-muted-foreground"
                data-testid="monitoring-check"
              >
                {hostReachabilityLabel(checkResults[pc.id].host_reachability)}
                {" · "}
                {clientReachabilityLabel(checkResults[pc.id].client_reachability)}
                {" · "}
                {guardCoverageLabel(checkResults[pc.id].guard_coverage)}
              </p>
            {/if}
          </div>
        {/each}
      </div>
    </div>

    <div class="rounded-lg border bg-card">
      <div class="border-b px-4 py-3">
        <h3 class="font-semibold">Recent Transactions</h3>
      </div>
      <div class="divide-y">
        {#each dashboard.recent_transactions as tx (tx.id)}
          <div class="flex items-center justify-between px-4 py-3">
            <div class="max-md:min-w-0">
              <p class="text-sm font-medium max-md:break-words">{tx.pc_name}</p>
              <p class="text-xs text-muted-foreground max-md:break-words">
                {formatDuration(tx.seconds_added)} · {tx.target_kind === "account" ? tx.account_username ?? "Account" : "Guest"}
                {#if tx.points_earned_centi > 0}
                  · +{formatPoints(tx.points_earned_centi)} pts
                {/if}
              </p>
            </div>
            <div class="text-right max-md:shrink-0">
              <p class="font-mono text-sm font-bold">{formatMoney(tx.amount_cents)}</p>
            </div>
          </div>
        {/each}
      </div>
    </div>
  {:else if !error}
    <p class="text-center text-muted-foreground">Loading dashboard…</p>
  {/if}
</div>

{#if signOutTarget}
  <ConfirmDialog
    title="Sign out {signOutTarget.username} from {signOutTarget.pcName}?"
    confirmLabel="Sign Out"
    busyLabel="Signing out…"
    busy={signOutBusy}
    onconfirm={confirmSignOut}
    oncancel={closeSignOut}
  >
    Their remaining time stays on the account. They can log in again on any PC.
  </ConfirmDialog>
{/if}

{#if suppressionTarget || cancellingSuppression}
  <!-- Transactional dialog: may occlude the widget; PIN step-up is required
       server-side either way (ADR-0076). -->
  <ConfirmDialog
    title={cancellingSuppression
      ? `Cancel monitoring suppression for ${cancellingSuppression.pcName}?`
      : `Suppress monitoring for ${suppressionTarget?.pcName ?? "this PC"}?`}
    confirmLabel={cancellingSuppression ? "Cancel Suppression" : "Suppress"}
    busyLabel="Saving…"
    busy={suppressionBusy}
    onconfirm={cancellingSuppression ? confirmCancelSuppression : confirmSuppress}
    oncancel={closeSuppressionDialogs}
  >
    {#if cancellingSuppression}
      New incidents for this PC will be detected again after a full grace
      period — expiry and cancellation never open an immediate Alert.
    {:else}
      While suppressed, no new PC Monitoring incidents open for this PC.
      Incidents already open stay open, and suppression expires automatically
      at most 60 minutes after it starts.
      <div class="mt-3">
        <label class="block text-sm font-medium" for="suppression-duration">
          Duration
        </label>
        <select
          id="suppression-duration"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          bind:value={suppressionDuration}
          disabled={suppressionBusy}
        >
          {#each SUPPRESSION_DURATIONS as minutes (minutes)}
            <option value={minutes}>{minutes} minutes</option>
          {/each}
        </select>
      </div>
    {/if}
    <div class="mt-3">
      <label class="block text-sm font-medium" for="suppression-pin">
        Current Web Admin PIN
      </label>
      <input
        id="suppression-pin"
        type="password"
        autocomplete="current-password"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        placeholder="Enter current PIN"
        bind:value={suppressionPin}
      />
      {#if suppressionError}
        <p class="mt-1 text-xs text-destructive">{suppressionError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}

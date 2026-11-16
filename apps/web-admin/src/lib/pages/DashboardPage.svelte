<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { toast } from "svelte-sonner";
  import * as api from "../api";
  import BellOffIcon from "@lucide/svelte/icons/bell-off";
  import ClockAlertIcon from "@lucide/svelte/icons/clock-alert";
  import PowerIcon from "@lucide/svelte/icons/power";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import TriangleAlertIcon from "@lucide/svelte/icons/triangle-alert";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { reportActionFailure, reportActionSuccess, type NotificationAction } from "../notifications";
  import {
    formatClockDifference,
    formatMoney,
    formatDuration,
    statusLabel,
    formatPoints,
    formatDate,
    formatTransactionTime,
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
    PcStatus,
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

  /** An empty PIN sends nothing, so there is no outcome to toast: say so next
   *  to the field (its aria-describedby error) and put focus there. */
  function requirePin(): boolean {
    if (suppressionPin) return true;
    suppressionError = "Enter your current Web Admin PIN.";
    document.getElementById("suppression-pin")?.focus();
    return false;
  }

  async function confirmSuppress() {
    const target = suppressionTarget;
    if (!target || suppressionBusy || !requirePin()) return;
    await runSuppressionMutation(
      "suppress",
      target.pcName,
      () => (suppressionTarget = null),
      () => api.setPcMonitoringSuppression(target.pcId, suppressionDuration, suppressionPin),
    );
  }

  async function confirmCancelSuppression() {
    const target = cancellingSuppression;
    if (!target || suppressionBusy || !requirePin()) return;
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

  /** Status pill per PC status. The label always carries the meaning; colour
   *  only reinforces it. In Use is the one filled pill so occupied PCs stand
   *  out; the rest are outlined with a coloured dot. */
  const STATUS_STYLE: Record<PcStatus, { pill: string; dot: string }> = {
    in_use: { pill: "border-transparent bg-primary text-primary-foreground", dot: "bg-primary-foreground" },
    online: { pill: "text-foreground", dot: "bg-emerald-500" },
    offline: { pill: "border-dashed text-muted-foreground", dot: "bg-muted-foreground/60" },
    locked: { pill: "text-foreground", dot: "bg-destructive" },
  };

  const statusCounts = $derived.by(() => {
    const counts: Record<PcStatus, number> = { in_use: 0, online: 0, offline: 0, locked: 0 };
    for (const pc of dashboard?.pcs ?? []) counts[pc.status] += 1;
    return counts;
  });

  /** "2 online · 1 offline · 1 locked": the PCs not in use, zero counts left out. */
  const idleBreakdown = $derived(
    (["online", "offline", "locked"] as const)
      .filter((status) => statusCounts[status] > 0)
      .map((status) => `${statusCounts[status]} ${statusLabel(status).toLowerCase()}`)
      .join(" · "),
  );

  onMount(() => {
    load();
    refreshTimer = setInterval(load, 5000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<!-- One KPI. Below 768px it is a compact label/value row inside a shared card
     (label and detail left, value right); from 768px it is its own tile. -->
{#snippet stat(label: string, value: string, detail: string)}
  <div
    class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 px-4 py-3 md:block md:rounded-lg md:border md:bg-card md:p-4"
  >
    <p class="text-sm text-muted-foreground">{label}</p>
    <p class="col-start-2 row-span-2 row-start-1 text-xl font-bold tabular-nums md:mt-1 md:text-2xl">
      {value}
    </p>
    {#if detail}
      <p class="text-xs text-muted-foreground md:mt-0.5">{detail}</p>
    {/if}
  </div>
{/snippet}

<div class="space-y-6">
  <div class="flex items-start justify-between gap-4">
    <div class="min-w-0">
      <h2 class="text-2xl font-bold tracking-tight">Dashboard</h2>
      {#if dashboard?.shop_name}
        <p class="text-sm break-words text-muted-foreground">{dashboard.shop_name}</p>
      {/if}
    </div>
    <Button variant="outline" onclick={load}>
      <RefreshCwIcon aria-hidden="true" />
      Refresh
    </Button>
  </div>

  {#if error}
    <div
      class="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
      role="alert"
    >
      Couldn't load the dashboard: {error}
    </div>
  {/if}

  {#if dashboard}
    <section
      class="grid divide-y rounded-lg border bg-card md:grid-cols-3 md:gap-4 md:divide-y-0 md:border-0 md:bg-transparent"
      aria-label="Today at a glance"
    >
      {@render stat(
        "Today's Sales",
        formatMoney(dashboard.today.sales_cents),
        `${dashboard.today.transaction_count} ${dashboard.today.transaction_count === 1 ? "transaction" : "transactions"}`,
      )}
      {@render stat("Time Sold Today", formatDuration(dashboard.today.seconds_sold), "")}
      {@render stat(
        "PCs In Use",
        `${statusCounts.in_use} / ${dashboard.pcs.length}`,
        dashboard.pcs.length ? idleBreakdown : "No PCs yet",
      )}
    </section>

    {#if !dashboard.pc_monitoring.enabled || dashboard.pc_monitoring.escalation_enabled}
      <div
        class="divide-y divide-amber-200 rounded-lg border border-amber-200 bg-amber-50 text-sm text-amber-900 dark:divide-amber-900 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-100"
      >
        {#if !dashboard.pc_monitoring.enabled}
          <!-- Standing advisory (ADR-0076): monitoring is off, so nothing is
               being watched. Inline, not a toast — it stays until the owner acts. -->
          <div class="flex gap-3 px-4 py-3" role="status" data-testid="monitoring-advisory">
            <TriangleAlertIcon class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
            <p class="min-w-0">
              <span class="font-semibold">PC Monitoring is disabled.</span>
              No PC is being watched for Client Reachability Mismatches. Enable it in
              <a class="font-medium underline underline-offset-2 hover:no-underline" href="/admin/settings/pc-monitoring">Settings → PC Monitoring</a>.
            </p>
          </div>
        {/if}
        {#if dashboard.pc_monitoring.escalation_enabled}
          <!-- Standing advisory (ADR-0077), the inverse of the one above: this
               one stands while the feature is ON, because powering customer PCs
               off is not something the owner should discover from the Audit Log. -->
          <div class="flex gap-3 px-4 py-3" role="status" data-testid="escalation-advisory">
            <PowerIcon class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
            <p class="min-w-0">
              <span class="font-semibold">Client Recovery Escalation is on.</span>
              A PC whose Client cannot be relaunched will be powered off and its session
              ended. Balance, time, and Points are never affected. Change this in
              <a class="font-medium underline underline-offset-2 hover:no-underline" href="/admin/settings/pc-monitoring">Settings → PC Monitoring</a>.
            </p>
          </div>
        {/if}
      </div>
    {/if}

    <!-- Wide screens put Recent Transactions beside the PCs instead of below. -->
    <div class="grid gap-6 2xl:grid-cols-[minmax(0,1fr)_26rem] 2xl:items-start">
      <section class="min-w-0 rounded-lg border bg-card" aria-labelledby="dashboard-pcs-heading">
        <div class="flex items-center justify-between gap-3 border-b px-4 py-3">
          <h3 id="dashboard-pcs-heading" class="font-semibold">PCs</h3>
          {#if dashboard.pcs.length}
            <Button
              variant="outline"
              size="sm"
              disabled={checkingAll || checkingPcId !== null}
              onclick={runCheckAll}
            >
              {checkingAll ? "Checking…" : "Check All PCs"}
            </Button>
          {/if}
        </div>

        {#if dashboard.pcs.length === 0}
          <p class="px-4 py-10 text-center text-sm text-muted-foreground">
            No PCs yet. Add one in
            <a class="font-medium text-foreground underline underline-offset-2 hover:no-underline" href="/admin/settings/pcs">Settings → PCs</a>.
          </p>
        {:else}
          <!-- One column grid shared by every row (subgrid) so status, time,
               and actions line up from 768px; below that each row is a card.
               Keep narrow desktop overflow inside the grid, away from the shell. -->
          <div class="md:overflow-x-auto">
            <div class="md:grid md:min-w-[48rem] md:grid-cols-[minmax(10rem,1fr)_auto_auto_auto] md:gap-x-6">
              <div
                class="hidden border-b px-4 py-2 text-xs font-medium text-muted-foreground md:col-span-full md:grid md:grid-cols-subgrid"
                aria-hidden="true"
              >
                <span>PC</span>
                <span>Status</span>
                <span class="text-right">Remaining Time</span>
                <span class="text-right">Actions</span>
              </div>
              <ul class="divide-y md:col-span-full md:grid md:grid-cols-subgrid">
                {#each dashboard.pcs as pc (pc.id)}
                  {@const incidents = dashboard.pc_monitoring.active_incidents[pc.id] ?? []}
                  {@const suppressedUntil = dashboard.pc_monitoring.suppressions[pc.id]}
                  {@const clockDrift = pc.clock_difference_ms != null && Math.abs(pc.clock_difference_ms) > 60_000 ? pc.clock_difference_ms : null}
                  {@const user = pc.active_session?.username ?? (pc.status === "in_use" ? "Guest" : null)}
                  {@const style = STATUS_STYLE[pc.status]}
                  {@const checkLabel = checkingPcId === pc.id ? "Checking…" : "Check"}
                  <li
                    id={`pc-${pc.id}`}
                    class="grid scroll-mt-4 grid-cols-[minmax(0,1fr)_auto] items-start gap-x-3 px-4 py-3 md:col-span-full md:grid-cols-subgrid md:items-center md:gap-x-6"
                    data-pc-id={pc.id}
                  >
                    <div class="col-start-1 row-span-2 row-start-1 min-w-0 md:row-span-1">
                      <p class="truncate font-medium">{pc.name}</p>
                      {#if user}
                        <p class="truncate text-sm text-muted-foreground">{user}</p>
                      {/if}
                    </div>

                    <div class="col-start-2 row-start-1 justify-self-end md:row-span-2 md:justify-self-start">
                      <Badge variant="outline" class="gap-1.5 {style.pill}">
                        <span class="size-1.5 rounded-full {style.dot}" aria-hidden="true"></span>
                        {statusLabel(pc.status)}
                      </Badge>
                    </div>

                    <div
                      class="col-start-2 row-start-2 justify-self-end font-mono text-sm tabular-nums md:col-start-3 md:row-span-2 md:row-start-1"
                    >
                      {#if pc.status === "in_use" || pc.remaining_seconds > 0}
                        <span class="sr-only">Remaining Time</span>
                        {formatDuration(pc.remaining_seconds)}
                      {:else}
                        <span class="text-muted-foreground max-md:hidden" aria-hidden="true">—</span>
                        <span class="sr-only">No Remaining Time</span>
                      {/if}
                    </div>

                    {#if incidents.length || suppressedUntil || clockDrift !== null}
                      <div
                        class="col-span-2 row-start-3 mt-2 flex flex-wrap items-center gap-1.5 md:col-span-1 md:col-start-1 md:row-start-2 md:mt-1.5"
                      >
                        {#if incidents.length}
                          <!-- Active incidents stay visible regardless of Alert
                               read state; acknowledging records review, never repair. -->
                          <div class="contents" data-testid="incident-badges-{pc.id}">
                            {#each incidents as kind (kind)}
                              <Badge
                                variant="destructive"
                                class="border-red-200 bg-red-50 text-red-800 dark:border-red-900 dark:bg-red-950 dark:text-red-200"
                                title={incidentKindHint(kind)}
                              >
                                <TriangleAlertIcon aria-hidden="true" />
                                {incidentKindLabel(kind)}
                                <span class="sr-only">: {incidentKindHint(kind)}</span>
                              </Badge>
                            {/each}
                          </div>
                        {/if}
                        {#if suppressedUntil}
                          <!-- Monitoring Suppression (ADR-0076): absolute expiry
                               as Admin computed it, never a countdown. -->
                          <Badge
                            class="max-w-full bg-sky-100 text-sky-900 dark:bg-sky-950 dark:text-sky-100"
                            data-testid="suppression-badge-{pc.id}"
                          >
                            <BellOffIcon aria-hidden="true" />
                            Suppressed until {formatDate(suppressedUntil)}
                          </Badge>
                        {/if}
                        {#if clockDrift !== null}
                          <div class="flex basis-full items-center gap-1.5 text-xs text-amber-700 dark:text-amber-300">
                            <ClockAlertIcon class="size-3.5 shrink-0" aria-hidden="true" />
                            <p role="status">Client clock is {formatClockDifference(clockDrift)}.</p>
                          </div>
                        {/if}
                      </div>
                    {/if}

                    <!-- Accessible names are the visible label plus the PC name
                         ("Check PC 01"), so a screen reader's button list can tell
                         rows apart while the label still starts the name. -->
                    <div
                      class="col-span-2 row-start-4 mt-3 flex flex-wrap gap-2 md:col-span-1 md:col-start-4 md:row-span-2 md:row-start-1 md:mt-0 md:justify-end"
                    >
                      <Button
                        variant="outline"
                        size="sm"
                        class="max-md:flex-auto"
                        aria-label="{checkLabel} {pc.name}"
                        disabled={checkingAll || checkingPcId !== null}
                        onclick={() => runCheck(pc)}
                      >
                        {checkLabel}
                      </Button>
                      {#if suppressedUntil}
                        <Button variant="outline" size="sm" class="max-md:flex-auto" aria-label="Cancel Suppression {pc.name}" onclick={() => askCancelSuppression(pc)}>
                          Cancel Suppression
                        </Button>
                      {:else}
                        <Button variant="outline" size="sm" class="max-md:flex-auto" aria-label="Suppress {pc.name}" onclick={() => askSuppress(pc)}>
                          Suppress
                        </Button>
                      {/if}
                      {#if pc.active_session}
                        <Button variant="outline" size="sm" class="max-md:flex-auto" aria-label="Sign Out {pc.name}" onclick={() => askSignOut(pc)}>
                          Sign Out
                        </Button>
                      {/if}
                    </div>

                    {#if checkResults[pc.id]}
                      <!-- Standing facts stay inline; only action outcomes use toasts. -->
                      <p
                        class="col-span-2 row-start-5 mt-2 flex flex-wrap gap-x-4 gap-y-1 rounded-md border px-3 py-2 text-sm text-muted-foreground md:col-span-full md:row-start-3"
                        data-testid="monitoring-check"
                      >
                        <span>{hostReachabilityLabel(checkResults[pc.id].host_reachability)}</span>
                        <span>{clientReachabilityLabel(checkResults[pc.id].client_reachability)}</span>
                        <span>{guardCoverageLabel(checkResults[pc.id].guard_coverage)}</span>
                      </p>
                    {/if}
                  </li>
                {/each}
              </ul>
            </div>
          </div>
        {/if}
      </section>

      <section class="min-w-0 rounded-lg border bg-card" aria-labelledby="dashboard-transactions-heading">
        <div class="border-b px-4 py-3">
          <h3 id="dashboard-transactions-heading" class="font-semibold">Recent Transactions</h3>
        </div>
        {#if dashboard.recent_transactions.length === 0}
          <p class="px-4 py-10 text-center text-sm text-muted-foreground">No transactions yet.</p>
        {:else}
          <ul class="divide-y">
            {#each dashboard.recent_transactions as tx (tx.id)}
              <li class="flex items-center justify-between gap-4 px-4 py-2.5">
                <div class="min-w-0">
                  <p class="truncate text-sm">
                    <span class="font-medium">{tx.pc_name}</span>
                    <span class="text-muted-foreground">
                      · {tx.target_kind === "account" ? tx.account_username ?? "Account" : "Guest"}
                    </span>
                  </p>
                  <p class="text-xs text-muted-foreground tabular-nums">
                    <time datetime={tx.created_at}>{formatTransactionTime(tx.created_at)}</time>
                    · +{formatDuration(tx.seconds_added)}
                    {#if tx.points_earned_centi > 0}
                      · +{formatPoints(tx.points_earned_centi)} pts
                    {/if}
                  </p>
                </div>
                <p class="shrink-0 text-sm font-semibold tabular-nums">{formatMoney(tx.amount_cents)}</p>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    </div>
  {:else if !error}
    <p class="py-12 text-center text-sm text-muted-foreground" role="status">Loading dashboard…</p>
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
      <div class="mt-4">
        <label class="block text-sm font-medium text-foreground" for="suppression-duration">
          Duration
        </label>
        <select
          id="suppression-duration"
          class="mt-1.5 block min-h-11 w-full rounded-lg border border-input bg-background px-2.5 text-base text-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none md:h-9 md:min-h-0 md:text-sm dark:bg-input/30"
          bind:value={suppressionDuration}
          disabled={suppressionBusy}
        >
          {#each SUPPRESSION_DURATIONS as minutes (minutes)}
            <option value={minutes}>{minutes} minutes</option>
          {/each}
        </select>
      </div>
    {/if}
    <div class="mt-4">
      <label class="block text-sm font-medium text-foreground" for="suppression-pin">
        Current Web Admin PIN
      </label>
      <Input
        id="suppression-pin"
        type="password"
        autocomplete="current-password"
        class="mt-1.5 text-foreground md:h-9"
        placeholder="Enter current PIN"
        aria-invalid={suppressionError ? true : undefined}
        aria-describedby={suppressionError ? "suppression-pin-error" : undefined}
        bind:value={suppressionPin}
        oninput={() => (suppressionError = "")}
        onkeydown={(event: KeyboardEvent) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          if (cancellingSuppression) confirmCancelSuppression();
          else confirmSuppress();
        }}
      />
      {#if suppressionError}
        <p id="suppression-pin-error" class="mt-1.5 text-xs text-destructive">{suppressionError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}

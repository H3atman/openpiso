<script lang="ts">
  import { onMount } from "svelte";
  import CheckCheckIcon from "@lucide/svelte/icons/check-check";
  import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
  import InfoIcon from "@lucide/svelte/icons/info";
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import RejectionCreditDialog from "$lib/components/RejectionCreditDialog.svelte";
  import SupportBundlePreview from "$lib/components/SupportBundlePreview.svelte";
  import * as api from "../api";
  import {
    formatAuditSummary,
    formatDate,
    formatDay,
    formatEventType,
  } from "../format";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import type {
    CoinBatchRejection,
    IncidentKind,
    NotificationItem,
  } from "../types";

  const PAGE_SIZE = 50;

  let {
    onNotificationsChanged = () => undefined,
  }: {
    onNotificationsChanged?: () => void | Promise<void>;
  } = $props();

  let activeAlerts = $state<NotificationItem[]>([]);
  let activeAlertsHasMore = $state(false);
  let notices = $state<NotificationItem[]>([]);
  let noticesHasMore = $state(false);
  let noticesBusy = $state(false);
  let acknowledgedAlerts = $state<NotificationItem[]>([]);
  let acknowledgedHasMore = $state(false);
  let loading = $state(true);
  let error = $state("");
  let historyError = $state("");
  let showHistory = $state(false);
  let historyLoaded = $state(false);
  let alertsBusy = $state(false);
  let historyBusy = $state(false);
  let busyAlertId = $state<string | null>(null);
  let bulkBusy = $state(false);
  let showBulkConfirm = $state(false);

  function closeBulkConfirm() {
    if (bulkBusy) return;
    showBulkConfirm = false;
  }

  const noticeGroups = $derived.by(() => {
    const groups = new Map<string, NotificationItem[]>();
    for (const notice of notices) {
      const day = notice.created_at.slice(0, 10);
      const group = groups.get(day) ?? [];
      group.push(notice);
      groups.set(day, group);
    }
    return [...groups.entries()];
  });

  const quiet = $derived(activeAlerts.length === 0 && notices.length === 0);

  // Coin Batch Rejections in the same 30-day window, keyed by their audit id.
  // This is the only door a **guest** rejection has: there is no Account to
  // reach it from (ADR-0069).
  let rejections = $state(new Map<string, CoinBatchRejection>());
  let creditTarget = $state<CoinBatchRejection | null>(null);

  // Incident-anchored Support Bundles (#363), keyed by the Alert that asked
  // for them. The window covers the incident, not the present.
  let anchoredBundles = $state<Record<string, string>>({});
  let bundleBusyId = $state<string | null>(null);

  async function requestAnchoredBundle(alert: NotificationItem) {
    if (bundleBusyId || !alert.pc_id) return;
    bundleBusyId = alert.id;
    try {
      const markdown = await api.getSupportBundle(alert.pc_id, undefined, alert.created_at);
      if (!markdown.trim()) {
        throw new Error("the server returned an empty bundle");
      }
      anchoredBundles = { ...anchoredBundles, [alert.id]: markdown };
      reportActionSuccess("request", "Support Bundle");
    } catch (caught) {
      reportActionFailure("request", "Support Bundle", caught);
    } finally {
      bundleBusyId = null;
    }
  }

  async function loadRejections() {
    try {
      const result = await api.listCoinBatchRejections();
      rejections = new Map(
        result.items.map((item) => [item.rejection_audit_id, item]),
      );
    } catch {
      // A rejection-list failure must never hide the Notices themselves; the
      // Credit action simply does not render.
      rejections = new Map();
    }
  }

  function mergeUnique(
    current: NotificationItem[],
    incoming: NotificationItem[],
  ): NotificationItem[] {
    const seen = new Set(current.map((item) => item.id));
    return [...current, ...incoming.filter((item) => !seen.has(item.id))];
  }

  async function load() {
    loading = true;
    error = "";
    historyError = "";
    try {
      // Notices are read unfiltered: the rolling window stays browsable after
      // the watermark advances, and the per-item `read` flag marks what is new.
      const [alertsResult, noticesResult] = await Promise.allSettled([
        api.listNotifications({
          limit: PAGE_SIZE,
          offset: 0,
          severity: "alert",
          unread: true,
        }),
        api.listNotifications({
          limit: PAGE_SIZE,
          offset: 0,
          severity: "notice",
        }),
      ]);

      if (alertsResult.status === "fulfilled") {
        activeAlerts = alertsResult.value.items;
        activeAlertsHasMore = alertsResult.value.has_more;
      } else {
        error = messageFor(alertsResult.reason);
      }

      if (noticesResult.status === "fulfilled") {
        notices = noticesResult.value.items;
        noticesHasMore = noticesResult.value.has_more;
        // The watermark moves only after the visible Notices were loaded, so
        // this visit keeps its "new" markers.
        try {
          await api.markNotificationsSeen();
          await onNotificationsChanged();
        } catch (caught) {
          error = messageFor(caught);
        }
      } else {
        error = error || messageFor(noticesResult.reason);
      }
    } finally {
      loading = false;
    }
  }

  async function loadMoreNotices() {
    if (noticesBusy || !noticesHasMore) return;
    noticesBusy = true;
    error = "";
    try {
      const page = await api.listNotifications({
        limit: PAGE_SIZE,
        offset: notices.length,
        severity: "notice",
      });
      notices = mergeUnique(notices, page.items);
      noticesHasMore = page.has_more;
    } catch (caught) {
      error = messageFor(caught);
    } finally {
      noticesBusy = false;
    }
  }

  function messageFor(caught: unknown): string {
    return caught instanceof Error ? caught.message : String(caught);
  }

  function monitoringEvidence(kind: IncidentKind): string {
    switch (kind) {
      case "client_reachability_mismatch":
        return "Observed: the host answered probes while the Client remained unreachable for 60 seconds.";
      case "host_probe_coverage":
        return "Observed: the Client was reachable while every host probe failed for two minutes.";
      case "client_guard_coverage":
        return "Observed: a reachable release Windows Client reported no Client Guard connection for two minutes.";
    }
  }

  function monitoringTroubleshooting(kind: IncidentKind): string {
    switch (kind) {
      case "client_reachability_mismatch":
        return "If Remote Control is configured for this PC, use it to check that the Client and OpenPisoClientGuard service are running.";
      case "host_probe_coverage":
        return "Check the PC's registered address and Windows ICMP or firewall setup. A failed probe is missing coverage, not proof that the host is offline.";
      case "client_guard_coverage":
        return "Check the OpenPisoClientGuard service and Client logs. If the service is missing, rerun the Client installer as Administrator.";
    }
  }

  function alertPage(offset: number) {
    return api.listNotifications({
      limit: PAGE_SIZE,
      offset,
      severity: "alert",
      unread: true,
    });
  }

  async function loadMoreAlerts() {
    if (alertsBusy || !activeAlertsHasMore) return;
    alertsBusy = true;
    error = "";
    try {
      const page = await alertPage(activeAlerts.length);
      activeAlerts = mergeUnique(activeAlerts, page.items);
      activeAlertsHasMore = page.has_more;
    } catch (caught) {
      error = messageFor(caught);
    } finally {
      alertsBusy = false;
    }
  }

  async function loadHistory() {
    if (historyLoaded || historyBusy) return;
    historyBusy = true;
    historyError = "";
    try {
      const page = await api.listNotifications({
        limit: PAGE_SIZE,
        offset: 0,
        severity: "alert",
        unread: false,
      });
      acknowledgedAlerts = page.items;
      acknowledgedHasMore = page.has_more;
      historyLoaded = true;
    } catch (caught) {
      historyError = messageFor(caught);
    } finally {
      historyBusy = false;
    }
  }

  async function loadMoreHistory() {
    if (historyBusy || !acknowledgedHasMore) return;
    historyBusy = true;
    historyError = "";
    try {
      const page = await api.listNotifications({
        limit: PAGE_SIZE,
        offset: acknowledgedAlerts.length,
        severity: "alert",
        unread: false,
      });
      acknowledgedAlerts = mergeUnique(acknowledgedAlerts, page.items);
      acknowledgedHasMore = page.has_more;
    } catch (caught) {
      historyError = messageFor(caught);
    } finally {
      historyBusy = false;
    }
  }

  async function toggleHistory() {
    showHistory = !showHistory;
    if (showHistory) await loadHistory();
  }

  async function acknowledge(alert: NotificationItem) {
    if (busyAlertId) return;
    busyAlertId = alert.id;
    try {
      await api.acknowledgeNotification(alert.id);
      activeAlerts = activeAlerts.filter((item) => item.id !== alert.id);
      reportActionSuccess("acknowledge", "Alert");
      await onNotificationsChanged();
    } catch (caught) {
      reportActionFailure("acknowledge", "Alert", caught);
    } finally {
      busyAlertId = null;
    }
  }

  // The next page of Alerts slides in when more were waiting behind the
  // acknowledged ones, so the queue is refetched rather than filtered locally.
  async function refreshAlerts() {
    try {
      const page = await alertPage(0);
      activeAlerts = page.items;
      activeAlertsHasMore = page.has_more;
    } catch (caught) {
      error = messageFor(caught);
    }
  }

  function invalidateHistory() {
    acknowledgedAlerts = [];
    acknowledgedHasMore = false;
    historyLoaded = false;
  }

  // Acknowledges only the rendered Alerts (ADR-0058): the server never sweeps
  // Alerts the owner has not seen, so unloaded pages stay in attention.
  async function acknowledgeAll() {
    if (bulkBusy || alertsBusy || busyAlertId) return;
    const auditIds = activeAlerts.map((alert) => alert.id);
    if (auditIds.length === 0) return;
    bulkBusy = true;
    try {
      const result = await api.acknowledgeNotifications(auditIds);
      showBulkConfirm = false;
      reportActionSuccess(
        "acknowledge",
        `${result.acknowledged_count} ${result.acknowledged_count === 1 ? "Alert" : "Alerts"}`,
      );
    } catch (caught) {
      reportActionFailure("acknowledge", "Alerts", caught);
    } finally {
      // A long list is acknowledged in batches, so even a failed run can have
      // acknowledged some Alerts: reread the queue either way.
      await refreshAlerts();
      invalidateHistory();
      if (showHistory) await loadHistory();
      await onNotificationsChanged();
      bulkBusy = false;
    }
  }

  onMount(() => {
    void load();
    void loadRejections();
  });
</script>

<div class="mx-auto max-w-6xl space-y-6">
  <header class="flex flex-wrap items-end justify-between gap-3">
    <div>
      <h2 class="text-2xl font-bold tracking-tight">Notifications</h2>
      <p class="mt-1 text-sm text-muted-foreground">
        Alerts stay until acknowledged. Notices are grouped by day.
      </p>
    </div>
    <Button variant="outline" onclick={toggleHistory}>
      {showHistory ? "Hide" : "Show"} acknowledged{acknowledgedAlerts.length > 0 ? ` (${acknowledgedAlerts.length})` : ""}
      <ChevronDownIcon class={showHistory ? "size-4 rotate-180" : "size-4"} />
    </Button>
  </header>

  {#if error}
    <div class="rounded-lg border border-destructive/40 bg-destructive/5 p-4 text-sm text-destructive" role="alert">
      Unable to load Notifications: {error}
    </div>
  {/if}

  {#if loading}
    <div class="rounded-lg border bg-card p-8 text-center text-sm text-muted-foreground">Loading Notifications…</div>
  {:else if quiet && !showHistory && !error}
    <section class="rounded-xl border border-dashed bg-card p-12 text-center">
      <CheckCheckIcon class="mx-auto size-9 text-primary" />
      <h3 class="mt-3 text-lg font-semibold">Shop is in good shape</h3>
      <p class="mx-auto mt-1 max-w-md text-sm text-muted-foreground">
        No active Alerts and no Notices in the last 30 days. Older activity remains available in Audit.
      </p>
    </section>
  {:else}
    <div class="grid gap-6 xl:grid-cols-[minmax(0,1.1fr)_minmax(18rem,0.9fr)]">
      <section aria-labelledby="attention-heading">
        <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
          <div>
            <h3 id="attention-heading" class="font-semibold">Needs attention</h3>
            <p class="text-xs text-muted-foreground">Alerts stay until explicitly acknowledged.</p>
          </div>
          <div class="flex items-center gap-2">
            {#if activeAlerts.length > 0}
              <Button
                size="sm"
                variant="outline"
                disabled={bulkBusy || alertsBusy || busyAlertId !== null}
                onclick={() => (showBulkConfirm = true)}
              >
                {bulkBusy ? "Acknowledging…" : `Acknowledge all (${activeAlerts.length})`}
              </Button>
            {/if}
            <Badge variant="destructive">
              {activeAlerts.length}{activeAlertsHasMore ? "+" : ""}
            </Badge>
          </div>
        </div>
        {#if activeAlerts.length === 0}
          <div class="rounded-lg border border-dashed bg-card p-6 text-sm text-muted-foreground">No active Alerts.</div>
        {:else}
          <div class="space-y-3">
            {#each activeAlerts as alert (alert.id)}
              <article class="rounded-lg border-l-4 border-l-destructive bg-card p-4 shadow-sm">
                <div class="flex items-start justify-between gap-3">
                  <div class="min-w-0">
                    <div class="flex flex-wrap items-center gap-2">
                      <Badge variant="destructive" class="font-semibold uppercase">Alert</Badge>
                      <span class="font-mono text-xs text-muted-foreground">{formatEventType(alert.event_type)}</span>
                    </div>
                    <p class="mt-2 text-sm font-medium leading-6">
                      {formatAuditSummary(alert)}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    disabled={busyAlertId !== null || bulkBusy}
                    onclick={() => acknowledge(alert)}
                  >
                    {busyAlertId === alert.id ? "Acknowledging…" : "Acknowledge"}
                  </Button>
                </div>
                <div class="mt-3 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
                  <span>{formatDate(alert.created_at)}</span>
                  {#if alert.pc_id}
                    <a
                      class="font-medium text-primary hover:underline"
                      href={`/admin/dashboard#pc-${encodeURIComponent(alert.pc_id)}`}
                    >
                      View {alert.pc_name ?? `PC ${alert.pc_id}`}
                    </a>
                  {:else}
                    <span>Shop</span>
                  {/if}
                  {#if alert.account_username}
                    <span>Account {alert.account_username}</span>
                  {:else if alert.account_id}
                    <span>Account {alert.account_id}</span>
                  {/if}
                  <span>By {alert.actor}</span>
                </div>
                {#if alert.event_type === "pc_monitoring_incident_opened" && alert.incident_kind}
                  <div
                    class="mt-3 space-y-2 rounded-md border bg-muted/30 p-3 text-xs leading-5"
                    data-testid="monitoring-alert-guidance"
                  >
                    <p class="font-medium">{monitoringEvidence(alert.incident_kind)}</p>
                    <p>{monitoringTroubleshooting(alert.incident_kind)}</p>
                    <p class="text-muted-foreground">
                      For planned maintenance, open the PC on Dashboard and start a temporary
                      Monitoring Suppression before stopping the Client. Suppression prevents new
                      incidents but keeps this incident open.
                    </p>
                    <a class="font-medium text-primary hover:underline" href="/admin/settings/pc-monitoring">
                      Review PC Monitoring and installation guidance
                    </a>
                    {#if alert.pc_id}
                      <div>
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={bundleBusyId !== null}
                          onclick={() => requestAnchoredBundle(alert)}
                        >
                          {bundleBusyId === alert.id
                            ? "Collecting…"
                            : anchoredBundles[alert.id]
                              ? "Refresh Support Bundle"
                              : "Get Support Bundle"}
                        </Button>
                      </div>
                      {#if anchoredBundles[alert.id]}
                        <p class="text-muted-foreground">
                          Evidence covers one hour before to fifteen minutes after this incident —
                          the Alert's own moment, not the present.
                        </p>
                        <SupportBundlePreview
                          bundle={anchoredBundles[alert.id]}
                          filename={`support-bundle-${alert.pc_id}.md`}
                          name="Support Bundle"
                        />
                      {/if}
                    {/if}
                  </div>
                {/if}
              </article>
            {/each}
          </div>
          {#if activeAlertsHasMore}
            <Button class="mt-3 w-full" variant="outline" disabled={alertsBusy} onclick={loadMoreAlerts}>
              {alertsBusy ? "Loading…" : "Load more Alerts"}
            </Button>
          {/if}
        {/if}

        {#if showHistory}
          <div class="mt-4 space-y-2 border-t pt-4">
            <p class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Acknowledged Alert history</p>
            {#if historyError}
              <p class="text-sm text-destructive">Unable to load history: {historyError}</p>
            {:else if historyBusy && !historyLoaded}
              <p class="text-sm text-muted-foreground">Loading history…</p>
            {:else if acknowledgedAlerts.length === 0}
              <p class="text-sm text-muted-foreground">No acknowledged Alerts.</p>
            {:else}
              {#each acknowledgedAlerts as alert (alert.id)}
                <div class="rounded-md border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
                  <span class="font-medium">
                    {formatAuditSummary(alert)}
                  </span>
                  · {formatDate(alert.created_at)}
                </div>
              {/each}
              {#if acknowledgedHasMore}
                <Button size="sm" variant="outline" disabled={historyBusy} onclick={loadMoreHistory}>
                  {historyBusy ? "Loading…" : "Load more history"}
                </Button>
              {/if}
            {/if}
          </div>
        {/if}
      </section>

      <section aria-labelledby="notices-heading">
        <div class="mb-3 flex items-center justify-between">
          <div>
            <h3 id="notices-heading" class="font-semibold">Notices</h3>
            <p class="text-xs text-muted-foreground">The last 30 days, newest first.</p>
          </div>
          <InfoIcon class="size-4 text-muted-foreground" />
        </div>
        {#if noticeGroups.length === 0}
          <div class="rounded-lg border border-dashed bg-card p-6 text-sm text-muted-foreground">No Notices in the last 30 days.</div>
        {:else}
          <div class="space-y-5">
            {#each noticeGroups as [day, group] (day)}
              <div>
                <h4 class="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">{formatDay(day)}</h4>
                <div class="divide-y rounded-lg border bg-card">
                  {#each group as notice (notice.id)}
                    <article class="p-4">
                      <div class="flex gap-3">
                        <InfoIcon class="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                        <div class="min-w-0">
                          <div class="flex flex-wrap items-center gap-2">
                            <span class="font-mono text-[11px] text-muted-foreground">{formatEventType(notice.event_type)}</span>
                            {#if !notice.read}
                              <Badge variant="secondary" class="bg-primary/10 text-primary text-[10px] font-semibold uppercase">New</Badge>
                            {/if}
                          </div>
                          <p class="mt-1 text-sm leading-5">
                            {formatAuditSummary(notice)}
                          </p>
                          <div class="mt-2 text-xs text-muted-foreground">
                            {formatDate(notice.created_at)} · {notice.pc_name ?? "Shop"}
                            {#if notice.account_username} · Account {notice.account_username}{/if}
                          </div>
                          {#if rejections.get(notice.id)}
                            {@const rejection = rejections.get(notice.id)!}
                            <!-- A credited rejection keeps its row and gains a
                                 marker: the list is append-only evidence, so it
                                 is never filtered out (ADR-0069). -->
                            <div class="mt-2 flex flex-wrap items-center gap-2">
                              {#if rejection.credited_coins > 0}
                                <Badge variant="secondary" class="bg-primary/10 text-primary text-[10px] font-semibold uppercase">
                                  Credited
                                </Badge>
                              {/if}
                              {#if rejection.remaining_coins > 0}
                                <Button
                                  size="sm"
                                  variant="outline"
                                  onclick={() => (creditTarget = rejection)}
                                >
                                  Credit
                                </Button>
                              {/if}
                            </div>
                          {/if}
                        </div>
                      </div>
                    </article>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
          {#if noticesHasMore}
            <Button class="mt-3 w-full" variant="outline" disabled={noticesBusy} onclick={loadMoreNotices}>
              {noticesBusy ? "Loading…" : "Load more Notices"}
            </Button>
          {/if}
        {/if}
      </section>
    </div>
  {/if}

  {#if showHistory && historyLoaded && acknowledgedAlerts.length === 0 && !historyError && !quiet}
    <p class="text-xs text-muted-foreground">Acknowledged Alert history is empty.</p>
  {/if}
</div>

{#if creditTarget}
  <RejectionCreditDialog
    rejection={creditTarget}
    oncredited={async () => {
      await loadRejections();
      await onNotificationsChanged();
    }}
    onclose={() => (creditTarget = null)}
  />
{/if}

<!-- Acknowledge all confirmation -->
{#if showBulkConfirm}
  <ConfirmDialog
    title={`Acknowledge ${activeAlerts.length} ${activeAlerts.length === 1 ? "Alert" : "Alerts"}?`}
    confirmLabel="Acknowledge all"
    busyLabel="Acknowledging…"
    busy={bulkBusy}
    onconfirm={acknowledgeAll}
    oncancel={closeBulkConfirm}
  >
    <p class="text-sm">
      This marks the {activeAlerts.length} listed
      {activeAlerts.length === 1 ? "Alert" : "Alerts"} as reviewed. It cannot be undone.
    </p>
    <p class="mt-2 text-xs text-muted-foreground">
      Acknowledging changes no balance, points, or PC — it only clears them from attention.
      {#if activeAlertsHasMore}
        Alerts not loaded yet stay in attention.
      {/if}
    </p>
  </ConfirmDialog>
{/if}

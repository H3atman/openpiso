<script lang="ts">
  import * as api from "../api";
  import { copyTextToClipboard } from "../clipboard";
  import { exitReasonLabel, formatDate, formatTransactionTime } from "../format";
  import SupportBundlePreview from "../components/SupportBundlePreview.svelte";
  import LoadErrorNotice from "../components/LoadErrorNotice.svelte";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import {
    initialSupportBundleState,
    reduceSupportBundle,
    type SupportBundleEvent,
  } from "../machines/support-bundle";
  import {
    initialCoinWindowState,
    reduceCoinWindow,
    type CoinWindowEvent,
  } from "../machines/coin-window";
  import type { HostDiagnostics, PcSettings } from "../types";

  // The page is revealed only while Diagnostics Mode (ADR-0081) is on; with
  // the flag off the route still resolves and renders the stub below, and
  // while the flag is not yet known the page asserts neither. Tabs are local
  // state, never sub-routes, so browser Back does not walk tab history
  // (ADR-0051).
  let {
    enabled,
    loadError = "",
    onRetry,
  }: {
    enabled: boolean | null;
    // The root shell's most recent Diagnostics Mode read failure. With no
    // value yet, the page names the failure rather than claiming either side.
    loadError?: string;
    // Required: with no value and a failed read, the retry is the only way
    // back to a known state without reloading the whole console.
    onRetry: () => void;
  } = $props();

  const tabs = [
    { id: "client", label: "Client" },
    { id: "coin", label: "Coin" },
    { id: "server", label: "Server" },
  ] as const;

  type Tab = (typeof tabs)[number]["id"];

  let tab = $state<Tab>("client");

  // The server default (6 h) and its hard cap (24 h), plus one narrower
  // option for a fresh symptom (ADR-0078).
  const windowOptions = [
    { seconds: 3_600, label: "1 hour" },
    { seconds: 21_600, label: "6 hours" },
    { seconds: 86_400, label: "24 hours" },
  ];

  let pcs = $state<PcSettings[] | null>(null);
  let pcsError = $state("");
  let pcId = $state("");
  let windowSeconds = $state(21_600);
  // A ready bundle stays under the PC it was collected for (its filename names
  // that PC), whatever the picker shows now, and survives a later request.
  let bundleState = $state.raw(initialSupportBundleState);
  let diagnosticsBusy = $state(false);
  let coinBundle = $state<string | null>(null);
  let coinBundleBusy = $state(false);
  let coinCopyBusy = $state(false);
  // Reads and resets are keyed so a read already in flight when a reset lands
  // drops its own answer instead of restoring an error over a known-fresh
  // start (ADR-0078).
  let coinWindow = $state.raw(initialCoinWindowState);
  let coinWindowLoaded = false;
  let host = $state<HostDiagnostics | null>(null);
  let hostError = $state("");
  let hostLoaded = false;

  // Load the PC list only once the flag reveals the page, so the stub never
  // touches the API; a flag flip while mounted re-loads it.
  $effect(() => {
    if (enabled) void loadPcs();
  });

  // The Coin tab reads the window when first shown, so a page opened for the
  // Client tab alone does not touch the coin API; a flag flip re-reads it.
  $effect(() => {
    if (!enabled) {
      coinWindowLoaded = false;
      return;
    }
    if (tab === "coin" && !coinWindowLoaded) {
      coinWindowLoaded = true;
      void loadCoinWindow();
    }
  });

  // The Server tab reads the host diagnostics endpoint (ADR-0082) when first
  // shown, so a page opened for the Client tab alone does not touch it; a
  // flag flip re-reads it. It stays a report: one read, no live tail.
  $effect(() => {
    if (!enabled) {
      hostLoaded = false;
      return;
    }
    if (tab === "server" && !hostLoaded) {
      hostLoaded = true;
      void loadHostDiagnostics();
    }
  });

  const bundleBusy = $derived(bundleState.phase === "requesting");

  function dispatchBundle(event: SupportBundleEvent) {
    bundleState = reduceSupportBundle(bundleState, event);
  }

  function dispatchCoinWindow(event: CoinWindowEvent) {
    coinWindow = reduceCoinWindow(coinWindow, event);
  }

  async function loadPcs() {
    pcs = null;
    pcsError = "";
    try {
      pcs = await api.listPcSettings();
    } catch (caught) {
      pcsError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function requestBundle() {
    if (bundleBusy || !pcId) return;
    const requestedPcId = pcId;
    dispatchBundle({ type: "requested", pcId: requestedPcId });
    try {
      const markdown = await api.getSupportBundle(requestedPcId, windowSeconds);
      if (!markdown.trim()) {
        throw new Error("the server returned an empty bundle");
      }
      dispatchBundle({ type: "succeeded", pcId: requestedPcId, bundle: markdown });
      reportActionSuccess("request", "Support Bundle");
    } catch (caught) {
      dispatchBundle({ type: "failed", pcId: requestedPcId });
      reportActionFailure("request", "Support Bundle", caught);
    }
  }

  async function requestDiagnostics() {
    if (diagnosticsBusy || !pcId) return;
    diagnosticsBusy = true;
    try {
      await api.requestDiagnostics(pcId);
      reportActionSuccess("place", "Diagnostic Request");
    } catch (caught) {
      reportActionFailure("place", "Diagnostic Request", caught);
    } finally {
      diagnosticsBusy = false;
    }
  }

  async function loadCoinWindow() {
    dispatchCoinWindow({ type: "read-started" });
    const requestId = coinWindow.requestId;
    try {
      const { since } = await api.getCoinDebugWindow();
      dispatchCoinWindow({ type: "read-succeeded", requestId, since });
    } catch (caught) {
      // Name the failure inline: an absent coverage start must not read as an
      // empty window. The reset control still works.
      const error = caught instanceof Error ? caught.message : String(caught);
      dispatchCoinWindow({ type: "read-failed", requestId, error });
    }
  }

  async function loadHostDiagnostics() {
    host = null;
    hostError = "";
    try {
      host = await api.getHostDiagnostics();
    } catch (caught) {
      hostError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function fetchCoinBundle(): Promise<string> {
    const markdown = await api.getCoinDebugBundle();
    if (!markdown.trim()) {
      throw new Error("the server returned an empty bundle");
    }
    return markdown;
  }

  async function requestCoinBundle() {
    if (coinBundleBusy) return;
    coinBundleBusy = true;
    try {
      coinBundle = await fetchCoinBundle();
      reportActionSuccess("request", "Coin Debug Bundle");
    } catch (caught) {
      reportActionFailure("request", "Coin Debug Bundle", caught);
    } finally {
      coinBundleBusy = false;
    }
  }

  async function copyCoinBundle() {
    if (coinCopyBusy) return;
    coinCopyBusy = true;
    try {
      await copyTextToClipboard(await fetchCoinBundle());
      reportActionSuccess("copy", "Coin Debug Bundle");
    } catch (caught) {
      reportActionFailure("copy", "Coin Debug Bundle", caught);
    } finally {
      coinCopyBusy = false;
    }
  }

  async function resetCoinWindow() {
    if (coinWindow.resetting) return;
    dispatchCoinWindow({ type: "reset-started" });
    try {
      const { since } = await api.resetCoinDebugWindow();
      dispatchCoinWindow({ type: "reset-succeeded", since });
      reportActionSuccess("reset", "Coin Debug Window");
    } catch (caught) {
      dispatchCoinWindow({ type: "reset-failed" });
      reportActionFailure("reset", "Coin Debug Window", caught);
    }
  }
</script>

<div class="space-y-6">
  <h2 class="text-2xl font-bold tracking-tight">Diagnostics</h2>

  <!-- A read failure is named whether or not a last-known value survives it:
       with no value the notice stands alone, and with one the surface below
       stays put but may be stale, so the retry stays in reach (ADR-0081). -->
  {#if loadError}
    <LoadErrorNotice
      message="Diagnostics Mode could not be read"
      detail={loadError}
      {onRetry}
    />
  {/if}

  {#if enabled === null}
    {#if !loadError}
      <div class="text-sm text-muted-foreground">Loading Diagnostics Mode…</div>
    {/if}
  {:else if !enabled}
    <div class="rounded-lg border bg-card p-6">
      <p class="text-sm text-muted-foreground">
        Diagnostics Mode is off, so this page stays hidden. Enable it in
        <strong>Settings → Security</strong> to reveal the Support Bundle, the
        Diagnostic Request, and the coin and server evidence. The link you
        followed still works once it is on.
      </p>
    </div>
  {:else}
    <div
      class="rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm"
      role="status"
    >
      <strong>Diagnostics Mode is on</strong> — this page is revealed on every
      device signed in to this console. Turn it off in
      <strong>Settings → Security</strong> when you are done.
    </div>

    <div class="flex gap-1 border-b" role="tablist" aria-label="Diagnostics">
      {#each tabs as item (item.id)}
        <button
          type="button"
          role="tab"
          aria-selected={tab === item.id}
          class="border-b-2 px-4 py-2 text-sm font-medium {tab === item.id
            ? 'border-primary text-foreground'
            : 'border-transparent text-muted-foreground'}"
          onclick={() => (tab = item.id)}
        >
          {item.label}
        </button>
      {/each}
    </div>

    {#if tab === "client"}
      <div class="rounded-lg border bg-card p-6">
        <h3 class="mb-4 text-lg font-semibold">Client</h3>

        {#if pcsError}
          <div class="mb-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            {pcsError}
          </div>
        {/if}

        <section>
          <h4 class="text-sm font-semibold">Support Bundle</h4>
          <p class="text-xs text-muted-foreground">
            PC health — one PC's Client, Client Guard, and Admin evidence.
          </p>
          <p class="mt-2 mb-4 text-sm text-muted-foreground">
            Collects one PC's <strong>Client</strong>, <strong>Client Guard</strong>,
            and <strong>Admin</strong> evidence — forwarded diagnostics, PC Monitoring
            Incidents, and Audit Log events — into one redacted Markdown bundle with a
            plain-language Verdict. The preview is read-only; requesting a bundle never
            changes a PC, a session, or money. Your Owner Session is enough — no PIN.
            <strong>Request Diagnostics</strong> asks a live Client for more: on its
            next Heartbeat it uploads one small, recent slice of its own log, which
            the next bundle includes. A PC that is offline or unreachable simply never
            answers — no error, no alert.
          </p>

          {#if pcs}
            <div class="flex flex-wrap items-end gap-3">
              <div class="max-md:min-w-0 max-md:w-full">
                <label class="block text-sm font-medium" for="support-pc">PC</label>
                <select
                  id="support-pc"
                  bind:value={pcId}
                  class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm max-md:w-full"
                >
                  <option value="" disabled>Choose a PC…</option>
                  {#each pcs as pc (pc.id)}
                    <option value={pc.id}>{pc.name}</option>
                  {/each}
                </select>
              </div>
              <div>
                <label class="block text-sm font-medium" for="support-window">Window</label>
                <select
                  id="support-window"
                  bind:value={windowSeconds}
                  class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm"
                >
                  {#each windowOptions as option (option.seconds)}
                    <option value={option.seconds}>{option.label}</option>
                  {/each}
                </select>
              </div>
              <button
                type="button"
                class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                disabled={bundleBusy || !pcId}
                onclick={requestBundle}
              >
                {bundleBusy ? "Requesting…" : "Request Bundle"}
              </button>
              <button
                type="button"
                class="rounded-md border px-4 py-2 text-sm font-medium hover:bg-muted disabled:opacity-50"
                disabled={diagnosticsBusy || !pcId}
                onclick={requestDiagnostics}
              >
                {diagnosticsBusy ? "Requesting…" : "Request Diagnostics"}
              </button>
            </div>

            {#if bundleState.shown}
              <div class="mt-4">
                <SupportBundlePreview
                  bundle={bundleState.shown.bundle}
                  filename={`support-bundle-${bundleState.shown.pcId}.md`}
                  name="Support Bundle"
                />
              </div>
            {/if}
          {:else if !pcsError}
            <div class="text-sm text-muted-foreground">Loading PCs…</div>
          {/if}
        </section>
      </div>
    {:else if tab === "coin"}
      <div class="rounded-lg border bg-card p-6">
        <h3 class="mb-4 text-lg font-semibold">Coin</h3>

        <section>
          <h4 class="text-sm font-semibold">Coin Debug Bundle</h4>
          <p class="text-xs text-muted-foreground">
            Coin path — Admin's own serial acceptor line and coin pipeline.
          </p>
          <p class="mt-2 mb-4 text-sm text-muted-foreground">
            Covers the coin path over the current app run and the Coin Debug
            Window (reset below) in one bounded Markdown bundle with a
            plain-language Verdict. Same read-only stance: Owner Session only,
            no PIN, and requesting it never changes money. When the trouble is
            coins rather than a PC, fetch this one.
          </p>

          <div class="flex flex-wrap gap-3">
            <button
              type="button"
              class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              disabled={coinBundleBusy}
              onclick={requestCoinBundle}
            >
              {coinBundleBusy ? "Requesting…" : "Request Coin Debug Bundle"}
            </button>
            <button
              type="button"
              class="rounded-md border bg-card px-4 py-2 text-sm font-medium hover:bg-accent disabled:opacity-50"
              disabled={coinCopyBusy}
              onclick={copyCoinBundle}
            >
              {coinCopyBusy ? "Copying…" : "Copy Coin Debug Bundle"}
            </button>
          </div>

          {#if coinBundle}
            <div class="mt-4">
              <SupportBundlePreview
                bundle={coinBundle}
                filename="coin-debug-bundle.md"
                name="Coin Debug Bundle"
              />
            </div>
          {/if}
        </section>

        <section class="mt-6 border-t pt-6">
          <h4 class="text-sm font-semibold">Coin Debug Window</h4>
          <p class="mt-1 text-xs text-muted-foreground">
            Coverage start for the Coin Debug Bundle. Reset it before
            reproducing a coin problem so the next bundle holds a clean
            capture.
          </p>
          {#if coinWindow.error}
            <div class="mt-3">
              <LoadErrorNotice
                message="The Coin Debug Window could not be read"
                detail={coinWindow.error}
                busy={coinWindow.resetting}
                onRetry={loadCoinWindow}
              />
            </div>
          {:else if coinWindow.since}
            <p class="mt-3 text-sm">
              Coin Debug Window start {formatTransactionTime(coinWindow.since)}
            </p>
          {/if}
          <button
            type="button"
            class="mt-3 rounded-md border bg-card px-4 py-2 text-sm font-medium hover:bg-accent disabled:opacity-50"
            disabled={coinWindow.resetting}
            onclick={resetCoinWindow}
          >
            {coinWindow.resetting ? "Resetting…" : "Reset Coin Debug Window"}
          </button>
        </section>
      </div>
    {:else}
      <div class="rounded-lg border bg-card p-6">
        <h3 class="mb-4 text-lg font-semibold">Server</h3>
        <p class="text-xs text-muted-foreground">
          Admin's own health — when it last started, why past runs ended, and
          any panic evidence. A report, not a live console.
        </p>

        {#if hostError}
          <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            {hostError}
          </div>
        {:else if host}
          {#if host.runs.length === 0}
            <p class="mt-4 text-sm text-muted-foreground">
              No Run Records yet — Admin writes one at each startup, so
              nothing is recorded here.
            </p>
          {:else}
            <ul class="mt-4 space-y-3">
              {#each host.runs as run (run.started_at)}
                <li class="rounded-md border p-3">
                  <p class="text-sm font-medium">
                    {formatDate(run.started_at)} — Admin version
                    {run.version}, PID {run.pid}
                    {#if run.ended_at === null && !run.suspected_hard_exit}
                      <span
                        class="ml-1 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground"
                        >this run</span
                      >
                    {/if}
                  </p>
                  {#if run.suspected_hard_exit}
                    <p class="mt-1 text-sm font-medium text-amber-600">
                      Suspected hard exit
                    </p>
                    <p class="text-xs text-muted-foreground">
                      The process went without saying why: no shutdown or
                      panic was recorded. That may mean a WebView2 crash, an
                      external kill, or power loss — the record does not say
                      which.
                    </p>
                  {:else if run.ended_at}
                    <p class="mt-1 text-sm">
                      Ended {formatDate(run.ended_at)} —
                      {run.exit_reason
                        ? exitReasonLabel(run.exit_reason)
                        : "reason not recorded"}
                    </p>
                  {:else}
                    <p class="mt-1 text-sm text-muted-foreground">Still running</p>
                  {/if}
                </li>
              {/each}
            </ul>
            <p class="mt-3 text-xs text-muted-foreground">
              Newest first. Several runs stacked up in a short window is the
              restart pattern to look for.
            </p>
          {/if}

          <section class="mt-6 border-t pt-6">
            <h4 class="text-sm font-semibold">Panic file</h4>
            {#if host.panic_file.exists}
              <p class="mt-2 text-sm">
                Present — last written
                {host.panic_file.last_modified_at
                  ? formatDate(host.panic_file.last_modified_at)
                  : "at an unknown time"}.
              </p>
              {#if host.panic_file.newest_8kb !== null}
                <pre
                  class="mt-3 max-h-96 overflow-auto rounded-md border bg-muted/40 p-4 font-mono text-xs whitespace-pre-wrap">{host.panic_file.newest_8kb}</pre
                >
              {:else}
                <p class="text-xs text-muted-foreground">
                  Its bytes could not be read.
                </p>
              {/if}
            {:else}
              <p class="mt-2 text-sm text-muted-foreground">
                No panic file — Admin writes one only when a Rust panic lands.
              </p>
            {/if}
          </section>
        {:else}
          <div class="mt-4 text-sm text-muted-foreground">Loading…</div>
        {/if}
      </div>
    {/if}
  {/if}
</div>

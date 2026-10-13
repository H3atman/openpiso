<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { Badge } from "$lib/components/ui/badge";
  import { formatMoney, formatTransactionTime } from "../../format";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import type {
    CoinDeviceSettings,
    AcceptorHealth,
    CalibrationStatus,
    DetectedSerialPort,
    PulseMapEntry,
  } from "../../types";

  let coinDevice = $state<CoinDeviceSettings | null>(null);
  let coinPorts = $state<DetectedSerialPort[]>([]);
  let coinPortsBusy = $state(false);
  let coinPortsError = $state("");
  let comPort = $state("");
  let coinPortUserChanged = $state(false);
  let displayedCoinPorts = $derived.by(() => {
    if (
      !comPort ||
      coinPorts.some((port) => port.name.toLowerCase() === comPort.toLowerCase())
    ) {
      return coinPorts;
    }
    return [
      ...coinPorts,
      { name: comPort, label: "Saved port", detected: false },
    ];
  });
  let selectedCoinPort = $derived(
    displayedCoinPorts.find(
      (port) => port.name.toLowerCase() === comPort.toLowerCase(),
    ),
  );
  let baudRate = $state(9600);
  let setPinLine = $state("dtr");
  let coinPinLine = $state("cts");
  let coinPinPolarity = $state("normally_open");
  let interCoinGapMs = $state(200);
  let inactivityTimeoutSeconds = $state(15);
  let hardCapSeconds = $state(180);
  let pulseMap = $state<PulseMapEntry[]>([]);
  let calibrationRequired = $state(false);
  let coinBusy = $state(false);
  let coinLoadError = $state("");
  let testResult = $state("");
  let acceptorHealth = $state<AcceptorHealth | null>(null);
  let acceptorHealthBusy = $state(false);
  let acceptorHealthLoadError = $state("");
  let calibration = $state<CalibrationStatus | null>(null);
  let calibrationTargetCents = $state(2000);
  let calibrationBusy = $state(false);
  let calibrationError = $state("");
  let calibrationPollTimer: ReturnType<typeof setInterval> | undefined;
  let calibrationPollGeneration = 0;
  let calibrationPollRequestSequence = 0;
  let calibrationCapturing = $derived(calibration?.phase === "capturing");
  let calibrationLocksSettings = $derived(
    calibration?.phase === "capturing" ||
      calibration?.phase === "completed",
  );

  // Shop-wide Diagnostics Mode (ADR-0081): while on, this section points to
  // the Diagnostics Coin tab that now holds the Coin Debug Bundle and the
  // Coin Debug Window controls. Unknown or off, the pointer stays absent, so
  // the page never advertises a surface the owner cannot open. The value is
  // the root shell's, so turning the mode off anywhere in this browser
  // removes the pointer at once instead of on the next mount.
  let {
    diagnosticsMode = null,
  }: {
    diagnosticsMode?: boolean | null;
  } = $props();
  let hasTwentyMapping = $derived(
    pulseMap.some(
      (entry) => entry.pulse_count === 20 && entry.amount_cents === 2000,
    ),
  );
  let duplicatePulseCounts = $derived.by(() => {
    const counts = new Map<number, number>();
    for (const entry of pulseMap) {
      counts.set(entry.pulse_count, (counts.get(entry.pulse_count) ?? 0) + 1);
    }
    return [...counts.entries()]
      .filter(([, count]) => count > 1)
      .map(([pulseCount]) => pulseCount);
  });

  function addPulseMapEntry() {
    const nextPulseCount = pulseMap.reduce(
      (highest, entry) => Math.max(highest, entry.pulse_count),
      0,
    ) + 1;
    pulseMap = [...pulseMap, { pulse_count: nextPulseCount, amount_cents: 100 }];
  }

  function removePulseMapEntry(i: number) {
    pulseMap = pulseMap.filter((_, idx) => idx !== i);
  }

  function calibrationSubject(amountCents: number): string {
    return amountCents === 2000 ? "₱20 calibration" : "Coin calibration";
  }

  async function loadCoinDevice() {
    try {
      coinDevice = await api.getCoinDeviceSettings();
      if (!coinPortUserChanged) comPort = coinDevice.com_port ?? "";
      baudRate = coinDevice.baud_rate;
      setPinLine = coinDevice.set_pin_line;
      coinPinLine = coinDevice.coin_pin_line;
      coinPinPolarity = coinDevice.coin_pin_polarity;
      interCoinGapMs = coinDevice.inter_coin_gap_ms;
      inactivityTimeoutSeconds = coinDevice.inactivity_timeout_seconds;
      hardCapSeconds = coinDevice.hard_cap_seconds;
      pulseMap = coinDevice.pulse_map.map((p) => ({ ...p }));
      calibrationRequired = coinDevice.calibration_required;
    } catch (caught) {
      coinLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function loadAcceptorHealth() {
    acceptorHealthBusy = true;
    acceptorHealthLoadError = "";
    try {
      acceptorHealth = await api.getAcceptorHealth();
    } catch (caught) {
      acceptorHealthLoadError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      acceptorHealthBusy = false;
    }
  }

  async function loadCoinPorts() {
    coinPortsBusy = true;
    coinPortsError = "";
    try {
      const ports = await api.getCoinDevicePorts();
      coinPorts = ports;
      const selected = ports.find(
        (port) => port.name.toLowerCase() === comPort.toLowerCase(),
      );
      if (selected) comPort = selected.name;
    } catch (caught) {
      coinPortsError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      coinPortsBusy = false;
    }
  }

  async function saveCoinDevice() {
    coinBusy = true;
    try {
      coinDevice = await api.updateCoinDeviceSettings({
        com_port: comPort || null,
        baud_rate: baudRate,
        set_pin_line: setPinLine,
        coin_pin_line: coinPinLine,
        coin_pin_polarity: coinPinPolarity,
        inter_coin_gap_ms: interCoinGapMs,
        inactivity_timeout_seconds: inactivityTimeoutSeconds,
        hard_cap_seconds: hardCapSeconds,
        pulse_map: pulseMap,
      });
      // A Coin Device save advances the debug window server-side; the Acceptor
      // Health refresh below reflects it.
      void loadAcceptorHealth();
      reportActionSuccess("save", "Coin Device", "coin-debug-window-reset");
    } catch (caught) {
      reportActionFailure("save", "Coin Device", caught);
    } finally {
      coinBusy = false;
    }
  }

  async function testAcceptor() {
    testResult = "";
    try {
      const result = await api.testCoinDevice();
      testResult = result.message;
      reportActionSuccess("complete", "Coin acceptor");
    } catch (caught) {
      testResult = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("complete", "Coin acceptor", caught);
    }
  }

  function stopCalibrationPolling() {
    calibrationPollGeneration += 1;
    if (calibrationPollTimer !== undefined) {
      clearInterval(calibrationPollTimer);
      calibrationPollTimer = undefined;
    }
  }

  function startCalibrationPolling() {
    stopCalibrationPolling();
    calibrationPollTimer = setInterval(() => void refreshCalibration(), 1000);
  }

  async function loadCurrentCalibration() {
    try {
      const status = await api.getCurrentCoinCalibration();
      if (!status) return;
      calibration = status;
      calibrationTargetCents = status.target_amount_cents;
      if (status.phase === "capturing") startCalibrationPolling();
    } catch (caught) {
      calibrationError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function refreshCalibration() {
    const capture = calibration;
    if (!capture) return;
    const captureId = capture.capture_id;
    const generation = calibrationPollGeneration;
    const requestSequence = ++calibrationPollRequestSequence;
    try {
      const status = await api.getCoinCalibration(captureId);
      if (
        requestSequence !== calibrationPollRequestSequence ||
        generation !== calibrationPollGeneration ||
        calibration?.capture_id !== captureId
      ) return;
      calibration = status;
      if (status.phase !== "capturing") stopCalibrationPolling();
    } catch (caught) {
      if (
        requestSequence !== calibrationPollRequestSequence ||
        generation !== calibrationPollGeneration ||
        calibration?.capture_id !== captureId
      ) return;
      calibrationError = caught instanceof Error ? caught.message : String(caught);
      // Keep polling: a transient LAN failure must not leave the owner with a
      // stale capturing view that cannot be refreshed or restarted.
    }
  }

  async function startCalibration() {
    calibrationBusy = true;
    calibrationError = "";
    stopCalibrationPolling();
    try {
      calibration = await api.startCoinCalibration(calibrationTargetCents);
      reportActionSuccess("create", calibrationSubject(calibrationTargetCents));
      startCalibrationPolling();
    } catch (caught) {
      calibrationError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("create", calibrationSubject(calibrationTargetCents), caught);
    } finally {
      calibrationBusy = false;
    }
  }

  async function recordCalibrationSample() {
    if (!calibration) return;
    calibrationPollRequestSequence += 1;
    calibrationBusy = true;
    calibrationError = "";
    try {
      calibration = await api.recordCoinCalibrationSample(calibration.capture_id);
      if (calibration.phase === "failed") {
        const reason = calibration.failure_reason ?? "calibration capture failed";
        calibrationError = reason;
        reportActionFailure("complete", "Coin calibration sample", new Error(reason));
        stopCalibrationPolling();
      } else {
        reportActionSuccess("complete", `${calibrationSubject(calibration.target_amount_cents)} sample`);
        if (calibration.phase !== "capturing") stopCalibrationPolling();
      }
    } catch (caught) {
      calibrationError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("complete", "Coin calibration sample", caught);
    } finally {
      calibrationBusy = false;
    }
  }

  async function armCalibrationSample() {
    if (!calibration) return;
    calibrationPollRequestSequence += 1;
    calibrationBusy = true;
    calibrationError = "";
    try {
      calibration = await api.armCoinCalibrationSample(calibration.capture_id);
    } catch (caught) {
      calibrationError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("complete", "next calibration sample", caught);
    } finally {
      calibrationBusy = false;
    }
  }

  async function cancelCalibration() {
    if (!calibration) return;
    calibrationPollRequestSequence += 1;
    calibrationBusy = true;
    calibrationError = "";
    try {
      calibration = await api.cancelCoinCalibration(calibration.capture_id);
      reportActionSuccess("remove", calibrationSubject(calibration.target_amount_cents));
      stopCalibrationPolling();
    } catch (caught) {
      calibrationError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("remove", "Coin calibration", caught);
    } finally {
      calibrationBusy = false;
    }
  }

  async function applyCalibration() {
    const capture = calibration;
    if (!capture) return;
    calibrationPollRequestSequence += 1;
    calibrationBusy = true;
    calibrationError = "";
    try {
      coinDevice = await api.applyCoinCalibration(capture.capture_id);
      pulseMap = coinDevice.pulse_map.map((entry) => ({ ...entry }));
      calibrationRequired = coinDevice.calibration_required;
      reportActionSuccess("save", calibrationSubject(capture.target_amount_cents), "coin-debug-window-reset");
      stopCalibrationPolling();
      calibration = null;
      void loadAcceptorHealth();
    } catch (caught) {
      calibrationError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("save", "Coin calibration", caught);
    } finally {
      calibrationBusy = false;
    }
  }

  onMount(() => {
    void loadCoinDevice();
    void loadCoinPorts();
    void loadAcceptorHealth();
    void loadCurrentCalibration();
    return stopCalibrationPolling;
  });
</script>

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">Coin Device Settings</h3>

  {#if diagnosticsMode === true}
    <p class="-mt-2 mb-4 text-sm text-muted-foreground">
      The coin evidence tools — the Coin Debug Bundle and the Coin Debug
      Window — moved to
      <a
        class="font-medium text-primary hover:underline"
        href="/admin/diagnostics">Diagnostics</a
      >.
    </p>
  {/if}

  <fieldset class="contents" disabled={calibrationLocksSettings}>
    <div class="space-y-4">
    <div>
      <label class="block text-sm font-medium" for="coin-com-port">COM Port</label>
      <div class="mt-1 flex gap-2">
        <select
          id="coin-com-port"
          class="block min-w-0 flex-1 rounded-md border bg-background px-3 py-2 text-sm font-mono"
          value={comPort}
          onchange={(event) => {
            const target = event.currentTarget as HTMLSelectElement;
            comPort = target.value;
            coinPortUserChanged = true;
          }}
        >
          <option value="">Simulator (no port)</option>
          {#each displayedCoinPorts as port (port.name)}
            <option value={port.name}>
              {port.name} — {port.detected ? port.label : "saved, not detected"}
            </option>
          {/each}
        </select>
        <button
          class="rounded-md border bg-card px-3 py-2 text-sm font-medium hover:bg-accent disabled:opacity-50"
          type="button"
          disabled={coinPortsBusy}
          onclick={() => void loadCoinPorts()}
        >
          {coinPortsBusy ? "Refreshing…" : "Refresh"}
        </button>
      </div>
      {#if selectedCoinPort && !selectedCoinPort.detected}
        <p class="mt-2 text-sm text-destructive" role="alert">
          Saved port {selectedCoinPort.name} was not detected. It remains selectable.
        </p>
      {/if}
      {#if coinPortsError}
        <div class="mt-2 flex items-center gap-3 text-sm text-destructive" role="alert">
          <span>{coinPortsError}</span>
          <button
            class="font-medium underline disabled:opacity-50"
            type="button"
            disabled={coinPortsBusy}
            onclick={() => void loadCoinPorts()}
          >
            Retry
          </button>
        </div>
      {/if}
    </div>
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="block text-sm font-medium" for="coin-baud-rate">Baud Rate</label>
        <input id="coin-baud-rate" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={baudRate} />
      </div>
      <div>
        <label class="block text-sm font-medium" for="coin-gap-ms">Inter-coin Gap (ms)</label>
        <input id="coin-gap-ms" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={interCoinGapMs} />
      </div>
    </div>
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="block text-sm font-medium" for="coin-inactivity">Inactivity Timeout (s)</label>
        <input id="coin-inactivity" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={inactivityTimeoutSeconds} />
        <p class="mt-1 text-xs text-muted-foreground">Selection window closes after this idle gap. Default: 15</p>
      </div>
      <div>
        <label class="block text-sm font-medium" for="coin-hard-cap">Hard Cap (s)</label>
        <input id="coin-hard-cap" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={hardCapSeconds} />
        <p class="mt-1 text-xs text-muted-foreground">Maximum selection window duration. Default: 180</p>
      </div>
    </div>
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="block text-sm font-medium" for="coin-set-pin">Set Pin (control)</label>
        <select id="coin-set-pin" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={setPinLine}>
          <option value="dtr">DTR</option>
          <option value="rts">RTS</option>
        </select>
      </div>
      <div>
        <label class="block text-sm font-medium" for="coin-coin-pin">Coin Pin (status)</label>
        <select id="coin-coin-pin" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={coinPinLine}>
          <option value="cts">CTS</option>
          <option value="dsr">DSR</option>
          <option value="ri">RI</option>
          <option value="cd">DCD</option>
        </select>
      </div>
      <div>
        <label class="block text-sm font-medium" for="coin-pin-polarity">Coin Pin Polarity</label>
        <select id="coin-pin-polarity" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={coinPinPolarity}>
          <option value="normally_open">Normally open (pulse high)</option>
          <option value="normally_closed">Normally closed (pulse low)</option>
        </select>
        <p class="mt-1 text-xs text-muted-foreground">Choose Normally closed for acceptors idle high and pulsing low.</p>
      </div>
    </div>
  </div>

  <fieldset class="mt-6" disabled={calibrationRequired}>
    <div class="mb-2 flex items-center justify-between">
      <h4 class="text-sm font-semibold">Pulse Map (historical Rejection Credit denominations)</h4>
      <button class="text-xs text-primary hover:underline" type="button" onclick={addPulseMapEntry}>+ Add</button>
    </div>
    <p class="mb-2 text-xs text-muted-foreground">Automatic Coin Batch authority is always 100 cents per pulse. This persisted map is retained for physical calibration and historical Rejection Credit denomination choices.</p>
    {#if duplicatePulseCounts.length > 0}
      <p class="mb-2 text-sm text-destructive" role="alert">Duplicate pulse counts: {duplicatePulseCounts.join(", ")}. Each pulse count must be unique.</p>
    {/if}
    <div class="space-y-2">
      {#each pulseMap as entry, i}
        <div class="flex flex-wrap items-center gap-2 rounded-md border bg-muted/50 p-2">
          <label class="text-xs">Pulses <input type="number" class="w-20 rounded border bg-background px-2 py-1 text-xs" bind:value={entry.pulse_count} /></label>
          <span class="text-xs">→</span>
          <label class="text-xs">Cents <input type="number" class="w-24 rounded border bg-background px-2 py-1 text-xs" bind:value={entry.amount_cents} /></label>
          <span class="text-xs text-muted-foreground">= {formatMoney(entry.amount_cents)}</span>
          <button class="text-xs text-destructive hover:underline" type="button" onclick={() => removePulseMapEntry(i)}>Remove</button>
        </div>
      {/each}
    </div>
    {#if calibrationRequired}
      <p class="mt-2 text-xs text-muted-foreground">Candidate map locked until physical calibration Apply.</p>
    {/if}
  </fieldset>
  </fieldset>

  {#if coinLoadError}
    <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">{coinLoadError}</div>
  {/if}

  {#if calibrationRequired}
    <div class="mt-6 rounded-md border border-amber-500/40 bg-amber-500/10 p-4 text-sm">
      {#if hasTwentyMapping}
        <p class="font-medium">₱20 mapping is a calibration candidate</p>
        <p class="mt-1 text-muted-foreground">A configured 20-pulse row is not proof of this acceptor's physical output. Complete five genuine ₱20 samples before serving customers.</p>
      {:else}
        <p class="font-medium">₱20 mapping needs physical calibration</p>
        <p class="mt-1 text-muted-foreground">Calibrate with five genuine ₱20 coins before serving customers.</p>
      {/if}
      <p class="mt-3 text-xs text-muted-foreground">Use the Coin Calibration panel below to choose the target amount and begin the exclusive capture.</p>
    </div>
  {/if}

  <section class="mt-6 rounded-md border p-4" aria-labelledby="coin-calibration-heading">
    <div class="flex items-start justify-between gap-3">
      <div>
        <h4 id="coin-calibration-heading" class="text-sm font-semibold">Coin Calibration</h4>
        <p class="mt-1 text-xs text-muted-foreground">Owner-only, serial hardware only. Captured coins are diagnostic and never credit a PC or customer.</p>
      </div>
      {#if calibration}
        <Badge variant="secondary" class="capitalize">{calibration.phase}</Badge>
      {/if}
    </div>

    {#if !calibration || calibration.phase !== "capturing" && calibration.phase !== "completed"}
      {#if calibration?.phase === "failed" && calibration.failure_reason}
        <p class="mt-3 text-sm text-destructive" role="alert">{calibration.failure_reason}</p>
      {/if}
      <p class="mt-3 text-sm text-muted-foreground">Save the serial settings first, then record five individual genuine coins. The source stays exclusive and inhibited between samples.</p>
      <label class="mt-3 block text-sm" for="calibration-target-cents">Target amount (integer cents; ₱20 default)</label>
      <input id="calibration-target-cents" class="mt-1 w-32 rounded border bg-background px-2 py-1 text-sm" type="number" min="1" bind:value={calibrationTargetCents} />
      <button
        class="mt-3 rounded-md border bg-card px-3 py-2 text-sm font-medium hover:bg-accent disabled:opacity-50"
        type="button"
        disabled={calibrationBusy || coinBusy || !comPort}
        onclick={() => void startCalibration()}
      >
        {calibrationBusy ? "Starting…" : calibrationTargetCents === 2000 ? "Start ₱20 Calibration" : "Start Calibration"}
      </button>
    {:else if calibration.phase === "capturing"}
      <p class="mt-3 text-sm">Sample {calibration.sample_counts.length + 1} of {calibration.required_samples}. Pulses captured: <strong>{calibration.current_pulse_count}</strong></p>
      {#if calibration.sample_armed}
        <p class="mt-1 text-xs text-muted-foreground">Insert exactly one genuine {formatMoney(calibration.target_amount_cents)} coin, wait for the train to finish, then record the sample.</p>
      {:else}
        <p class="mt-1 text-xs text-muted-foreground">Set Pin is inhibited between samples. Arm the next sample before inserting the next coin.</p>
      {/if}
      <div class="mt-3 flex flex-wrap gap-2">
        {#if calibration.sample_armed}
          <button class="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50" type="button" disabled={calibrationBusy} onclick={() => void recordCalibrationSample()}>
            {calibrationBusy ? "Recording…" : "Record Sample"}
          </button>
        {:else}
          <button class="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50" type="button" disabled={calibrationBusy} onclick={() => void armCalibrationSample()}>
            {calibrationBusy ? "Arming…" : "Arm Next Sample"}
          </button>
        {/if}
        <button class="rounded-md border bg-card px-3 py-2 text-sm hover:bg-accent disabled:opacity-50" type="button" disabled={calibrationBusy} onclick={() => void cancelCalibration()}>Cancel</button>
      </div>
    {:else}
      <p class="mt-3 text-sm">Samples: <strong>{calibration.sample_counts.join(", ")}</strong> pulses</p>
      <p class="mt-1 text-xs text-muted-foreground">Apply is available only when all five samples agree. Target amount: {formatMoney(calibration.target_amount_cents)} ({calibration.target_amount_cents} cents).</p>
      <div class="mt-3 flex flex-wrap gap-2">
        <button class="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50" type="button" disabled={calibrationBusy || calibration.phase !== "completed"} onclick={() => void applyCalibration()}>Apply Mapping</button>
        <button class="rounded-md border bg-card px-3 py-2 text-sm hover:bg-accent disabled:opacity-50" type="button" disabled={calibrationBusy} onclick={() => void cancelCalibration()}>Discard</button>
      </div>
    {/if}
    {#if calibrationError}
      <p class="mt-3 text-sm text-destructive" role="alert">{calibrationError}</p>
    {/if}
  </section>

  <section class="mt-6 rounded-md border p-4" aria-labelledby="acceptor-health-heading">
    <div class="flex items-center justify-between gap-3">
      <div>
        <h4 id="acceptor-health-heading" class="text-sm font-semibold">Acceptor Health</h4>
        <p class="mt-1 text-xs text-muted-foreground">Current diagnostic coverage and timing instrumentation status.</p>
      </div>
      <button
        class="rounded-md border bg-card px-3 py-1.5 text-xs hover:bg-accent disabled:opacity-50"
        type="button"
        aria-label="Refresh Acceptor Health"
        disabled={acceptorHealthBusy}
        onclick={() => void loadAcceptorHealth()}
      >
        {acceptorHealthBusy ? "Refreshing…" : "Refresh"}
      </button>
    </div>
    {#if acceptorHealthLoadError}
      <p class="mt-3 text-sm text-destructive" role="alert">Unable to load Acceptor Health: {acceptorHealthLoadError}</p>
    {:else if acceptorHealth === null}
      <p class="mt-3 text-sm text-muted-foreground">Loading Acceptor Health…</p>
    {:else}
      <dl class="mt-3 grid gap-3 text-sm sm:grid-cols-3">
        <div>
          <dt class="text-xs text-muted-foreground">Coin Debug Window start</dt>
          <dd class="mt-1 font-medium">{formatTransactionTime(acceptorHealth.debug_window_start)}</dd>
        </div>
        <div>
          <dt class="text-xs text-muted-foreground">Inter-pulse timing</dt>
          <dd class="mt-1 font-medium">Unavailable until filtered timing (S2)</dd>
          <dd class="mt-1 font-mono text-xs text-muted-foreground">{acceptorHealth.timing_status}</dd>
        </div>
        <div>
          <dt class="text-xs text-muted-foreground">Implausible Timing rate</dt>
          <dd class="mt-1 font-medium">{acceptorHealth.implausible_timing_rate === null ? "Not reported in S1" : `${acceptorHealth.implausible_timing_rate}%`}</dd>
        </div>
      </dl>
    {/if}
  </section>

  <div class="mt-6 flex gap-3">
    <button
      class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
      type="button"
      disabled={coinBusy || calibrationLocksSettings || duplicatePulseCounts.length > 0}
      onclick={saveCoinDevice}
    >
      {coinBusy ? "Saving…" : "Save"}
    </button>
    <button
      class="rounded-md border bg-card px-4 py-2 text-sm font-medium hover:bg-accent"
      type="button"
      disabled={calibrationCapturing}
      onclick={testAcceptor}
    >
      Test Acceptor
    </button>
  </div>

  {#if testResult}
    <div class="mt-4 rounded-md bg-muted p-3 text-sm">{testResult}</div>
  {/if}
</div>

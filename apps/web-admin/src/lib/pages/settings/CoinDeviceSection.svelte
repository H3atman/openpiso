<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import NativeSelect from "$lib/components/settings/NativeSelect.svelte";
  import Notice from "$lib/components/settings/Notice.svelte";
  import SettingsCard from "$lib/components/settings/SettingsCard.svelte";
  import SettingsField from "$lib/components/settings/SettingsField.svelte";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import Trash2Icon from "@lucide/svelte/icons/trash-2";
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

<div class="space-y-6">
  {#if diagnosticsMode === true}
    <Notice>
      <p>
        The coin evidence tools — the Coin Debug Bundle and the Coin Debug
        Window — moved to
        <a
          class="font-medium text-primary underline-offset-4 hover:underline"
          href="/admin/diagnostics">Diagnostics</a
        >.
      </p>
    </Notice>
  {/if}

  {#if coinLoadError}
    <Notice tone="danger">{coinLoadError}</Notice>
  {/if}

  {#if calibrationRequired}
    <Notice
      tone="warning"
      title={hasTwentyMapping ? "₱20 mapping is a calibration candidate" : "₱20 mapping needs physical calibration"}
    >
      {#if hasTwentyMapping}
        <p>A configured 20-pulse row is not proof of this acceptor's physical output. Complete five genuine ₱20 samples before serving customers.</p>
      {:else}
        <p>Calibrate with five genuine ₱20 coins before serving customers.</p>
      {/if}
      <p>Use Coin Calibration below to choose the target amount and begin the exclusive capture.</p>
    </Notice>
  {/if}

  <SettingsCard
    title="Serial settings"
    description="How the Admin PC talks to the coin acceptor. The Pulse Map saves with these settings."
  >
    <fieldset class="contents" disabled={calibrationLocksSettings}>
      <div class="space-y-8">
        <div class="space-y-4">
          <h4 class="text-sm font-semibold">Connection</h4>
          <div class="grid gap-4 md:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
            <div class="grid content-start gap-1.5">
              <label class="text-sm font-medium" for="coin-com-port">COM Port</label>
              <div class="flex gap-2 max-sm:flex-col">
                <NativeSelect
                  id="coin-com-port"
                  class="flex-1"
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
                </NativeSelect>
                <Button variant="outline" disabled={coinPortsBusy} onclick={() => void loadCoinPorts()}>
                  <RefreshCwIcon class={coinPortsBusy ? "animate-spin" : ""} aria-hidden="true" />
                  {coinPortsBusy ? "Refreshing…" : "Refresh"}
                </Button>
              </div>
              {#if selectedCoinPort && !selectedCoinPort.detected}
                <p class="text-sm text-destructive" role="alert">
                  Saved port {selectedCoinPort.name} was not detected. It remains selectable.
                </p>
              {/if}
              {#if coinPortsError}
                <div class="flex flex-wrap items-center gap-x-3 text-sm text-destructive" role="alert">
                  <span>{coinPortsError}</span>
                  <Button variant="link" class="h-auto px-0 text-destructive" disabled={coinPortsBusy} onclick={() => void loadCoinPorts()}>
                    Retry
                  </Button>
                </div>
              {/if}
            </div>
            <SettingsField id="coin-baud-rate" label="Baud Rate">
              <Input id="coin-baud-rate" type="number" class="tabular-nums" bind:value={baudRate} />
            </SettingsField>
          </div>
        </div>

        <div class="space-y-4 border-t pt-6">
          <h4 class="text-sm font-semibold">Signal pins</h4>
          <div class="grid gap-4 md:grid-cols-3">
            <SettingsField id="coin-set-pin" label="Set Pin (control)">
              <NativeSelect id="coin-set-pin" bind:value={setPinLine}>
                <option value="dtr">DTR</option>
                <option value="rts">RTS</option>
              </NativeSelect>
            </SettingsField>
            <SettingsField id="coin-coin-pin" label="Coin Pin (status)">
              <NativeSelect id="coin-coin-pin" bind:value={coinPinLine}>
                <option value="cts">CTS</option>
                <option value="dsr">DSR</option>
                <option value="ri">RI</option>
                <option value="cd">DCD</option>
              </NativeSelect>
            </SettingsField>
            <SettingsField
              id="coin-pin-polarity"
              label="Coin Pin Polarity"
              hint="Choose Normally closed for acceptors that idle high and pulse low."
            >
              <NativeSelect id="coin-pin-polarity" aria-describedby="coin-pin-polarity-hint" bind:value={coinPinPolarity}>
                <option value="normally_open">Normally open (pulse high)</option>
                <option value="normally_closed">Normally closed (pulse low)</option>
              </NativeSelect>
            </SettingsField>
          </div>
        </div>

        <div class="space-y-4 border-t pt-6">
          <h4 class="text-sm font-semibold">Timing</h4>
          <div class="grid gap-4 md:grid-cols-3">
            <SettingsField id="coin-gap-ms" label="Inter-coin Gap (ms)">
              <Input id="coin-gap-ms" type="number" class="tabular-nums" bind:value={interCoinGapMs} />
            </SettingsField>
            <SettingsField
              id="coin-inactivity"
              label="Inactivity Timeout (s)"
              hint="The selection window closes after this idle gap. Default: 15"
            >
              <Input id="coin-inactivity" type="number" class="tabular-nums" aria-describedby="coin-inactivity-hint" bind:value={inactivityTimeoutSeconds} />
            </SettingsField>
            <SettingsField id="coin-hard-cap" label="Hard Cap (s)" hint="The longest a selection window can stay open. Default: 180">
              <Input id="coin-hard-cap" type="number" class="tabular-nums" aria-describedby="coin-hard-cap-hint" bind:value={hardCapSeconds} />
            </SettingsField>
          </div>
        </div>

        <!-- The border lives on a wrapper: a fieldset's top border would be
             interrupted by its visible legend. -->
        <div class="border-t pt-6">
        <fieldset class="min-w-0 space-y-4" disabled={calibrationRequired}>
          <legend class="text-sm font-semibold">Pulse Map</legend>
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div class="min-w-0 flex-1 basis-60">
              <p class="max-w-prose text-sm text-muted-foreground">
                Coin Batches are always credited at 100 cents per pulse. This map is kept for physical calibration
                and for historical Rejection Credit denomination choices.
              </p>
            </div>
            <Button variant="outline" size="sm" onclick={addPulseMapEntry}>
              <PlusIcon aria-hidden="true" />
              Add row
            </Button>
          </div>
          {#if duplicatePulseCounts.length > 0}
            <p id="pulse-map-duplicate-error" class="text-sm text-destructive" role="alert">Duplicate pulse counts: {duplicatePulseCounts.join(", ")}. Each pulse count must be unique.</p>
          {/if}
          {#if pulseMap.length === 0}
            <p class="rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">No Pulse Map rows.</p>
          {:else}
            <ul class="divide-y rounded-lg border">
              {#each pulseMap as entry, i}
                <li class="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-end gap-3 px-3 py-2 sm:grid-cols-[8rem_10rem_minmax(0,1fr)_auto] sm:items-center">
                  <label class="grid gap-1 text-xs text-muted-foreground">
                    Pulses
                    <Input
                      type="number"
                      class="text-foreground tabular-nums"
                      aria-label={`Pulses, row ${i + 1}`}
                      aria-invalid={duplicatePulseCounts.includes(entry.pulse_count) || undefined}
                      aria-describedby={duplicatePulseCounts.includes(entry.pulse_count) ? "pulse-map-duplicate-error" : undefined}
                      bind:value={entry.pulse_count}
                    />
                  </label>
                  <label class="grid gap-1 text-xs text-muted-foreground">
                    Cents
                    <Input type="number" class="text-foreground tabular-nums" aria-label={`Cents, row ${i + 1}`} bind:value={entry.amount_cents} />
                  </label>
                  <span class="col-span-2 row-start-2 text-sm tabular-nums text-muted-foreground sm:col-span-1 sm:col-start-3 sm:row-start-1 sm:self-end sm:pb-1.5">
                    = {formatMoney(entry.amount_cents)}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    class="col-start-3 row-start-1 text-muted-foreground hover:text-destructive sm:col-start-4 sm:self-end"
                    aria-label={`Remove Pulse Map row ${i + 1}`}
                    title="Remove row"
                    onclick={() => removePulseMapEntry(i)}
                  >
                    <Trash2Icon aria-hidden="true" />
                  </Button>
                </li>
              {/each}
            </ul>
          {/if}
          {#if calibrationRequired}
            <p class="text-xs text-muted-foreground">The candidate map is locked until a physical calibration is applied.</p>
          {/if}
        </fieldset>
        </div>
      </div>
    </fieldset>

    {#if testResult}
      <Notice class="mt-6" role="status" title="Acceptor test result">
        <p>{testResult}</p>
      </Notice>
    {/if}

    {#snippet footer()}
      <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">
        {calibrationLocksSettings
          ? "Locked while a calibration is in progress."
          : "Saving starts a new Coin Debug Window."}
      </p>
      <Button variant="outline" disabled={calibrationCapturing} onclick={testAcceptor}>Test Acceptor</Button>
      <Button disabled={coinBusy || calibrationLocksSettings || duplicatePulseCounts.length > 0} onclick={saveCoinDevice}>
        {coinBusy ? "Saving…" : "Save Coin Device"}
      </Button>
    {/snippet}
  </SettingsCard>

  <SettingsCard
    title="Coin Calibration"
    description="Owner-only, serial hardware only. Captured coins are diagnostic and never credit a PC or customer."
  >
    {#snippet actions()}
      {#if calibration}
        <Badge variant="secondary" class="capitalize">{calibration.phase}</Badge>
      {/if}
    {/snippet}

    {#if !calibration || calibration.phase !== "capturing" && calibration.phase !== "completed"}
      <div class="space-y-4">
        {#if calibration?.phase === "failed" && calibration.failure_reason}
          <Notice tone="danger" role="alert">{calibration.failure_reason}</Notice>
        {/if}
        <p class="max-w-prose text-sm text-muted-foreground">
          Save the serial settings first, then record five individual genuine coins. The source stays exclusive and
          inhibited between samples.
        </p>
        <div class="flex flex-wrap items-end gap-3">
          <SettingsField id="calibration-target-cents" label="Target amount (cents)" class="w-44">
            <Input
              id="calibration-target-cents"
              type="number"
              min="1"
              class="tabular-nums"
              aria-describedby="calibration-target-cents-hint"
              bind:value={calibrationTargetCents}
            />
          </SettingsField>
          <Button
            variant="outline"
            disabled={calibrationBusy || coinBusy || !comPort}
            onclick={() => void startCalibration()}
          >
            {calibrationBusy ? "Starting…" : calibrationTargetCents === 2000 ? "Start ₱20 Calibration" : "Start Calibration"}
          </Button>
        </div>
        <p id="calibration-target-cents-hint" class="text-xs text-muted-foreground">
          Whole cents. Default: 2000 (₱20).{!comPort ? " Choose a COM Port first; calibration needs serial hardware." : ""}
        </p>
      </div>
    {:else if calibration.phase === "capturing"}
      {@const recorded = calibration.sample_counts.length}
      <div class="space-y-3">
        <div class="flex flex-wrap items-baseline justify-between gap-2 text-sm">
          <p class="font-medium">Sample {recorded + 1} of {calibration.required_samples}</p>
          <p>Pulses captured: <strong class="tabular-nums">{calibration.current_pulse_count}</strong></p>
        </div>
        <div
          class="h-1.5 overflow-hidden rounded-full bg-muted"
          role="progressbar"
          aria-label="Samples recorded"
          aria-valuemin={0}
          aria-valuemax={calibration.required_samples}
          aria-valuenow={recorded}
        >
          <div class="h-full bg-primary transition-all" style="width: {(recorded / Math.max(calibration.required_samples, 1)) * 100}%"></div>
        </div>
        {#if calibration.sample_armed}
          <p class="text-sm text-muted-foreground">Insert exactly one genuine {formatMoney(calibration.target_amount_cents)} coin, wait for the train to finish, then record the sample.</p>
        {:else}
          <p class="text-sm text-muted-foreground">Set Pin is inhibited between samples. Arm the next sample before inserting the next coin.</p>
        {/if}
        <div class="flex flex-wrap gap-2 max-md:*:flex-1">
          {#if calibration.sample_armed}
            <Button disabled={calibrationBusy} onclick={() => void recordCalibrationSample()}>
              {calibrationBusy ? "Recording…" : "Record Sample"}
            </Button>
          {:else}
            <Button disabled={calibrationBusy} onclick={() => void armCalibrationSample()}>
              {calibrationBusy ? "Arming…" : "Arm Next Sample"}
            </Button>
          {/if}
          <Button variant="outline" disabled={calibrationBusy} onclick={() => void cancelCalibration()}>Cancel</Button>
        </div>
      </div>
    {:else}
      <div class="space-y-3">
        <p class="text-sm">Samples: <strong class="tabular-nums">{calibration.sample_counts.join(", ")}</strong> pulses</p>
        <p class="text-sm text-muted-foreground">Apply is available only when all five samples agree. Target amount: {formatMoney(calibration.target_amount_cents)} ({calibration.target_amount_cents} cents).</p>
        <div class="flex flex-wrap gap-2 max-md:*:flex-1">
          <Button disabled={calibrationBusy || calibration.phase !== "completed"} onclick={() => void applyCalibration()}>Apply Mapping</Button>
          <Button variant="outline" disabled={calibrationBusy} onclick={() => void cancelCalibration()}>Discard</Button>
        </div>
      </div>
    {/if}
    {#if calibrationError}
      <p class="mt-4 text-sm text-destructive" role="alert">{calibrationError}</p>
    {/if}
  </SettingsCard>

  <SettingsCard title="Acceptor Health" description="Current diagnostic coverage and timing instrumentation status.">
    {#snippet actions()}
      <Button
        variant="outline"
        size="sm"
        aria-label="Refresh Acceptor Health"
        disabled={acceptorHealthBusy}
        onclick={() => void loadAcceptorHealth()}
      >
        <RefreshCwIcon class={acceptorHealthBusy ? "animate-spin" : ""} aria-hidden="true" />
        {acceptorHealthBusy ? "Refreshing…" : "Refresh"}
      </Button>
    {/snippet}

    {#if acceptorHealthLoadError}
      <Notice tone="danger" role="alert">
        <p>Unable to load Acceptor Health: {acceptorHealthLoadError}</p>
      </Notice>
    {:else if acceptorHealth === null}
      <p class="text-sm text-muted-foreground">Loading Acceptor Health…</p>
    {:else}
      <dl class="grid gap-4 text-sm sm:grid-cols-3">
        <div>
          <dt class="text-muted-foreground">Coin Debug Window start</dt>
          <dd class="mt-1 font-medium tabular-nums">{formatTransactionTime(acceptorHealth.debug_window_start)}</dd>
        </div>
        <div>
          <dt class="text-muted-foreground">Inter-pulse timing</dt>
          <dd class="mt-1 font-medium">Unavailable until filtered timing (S2)</dd>
          <dd class="mt-0.5 font-mono text-xs text-muted-foreground">{acceptorHealth.timing_status}</dd>
        </div>
        <div>
          <dt class="text-muted-foreground">Implausible Timing rate</dt>
          <dd class="mt-1 font-medium tabular-nums">{acceptorHealth.implausible_timing_rate === null ? "Not reported in S1" : `${acceptorHealth.implausible_timing_rate}%`}</dd>
        </div>
      </dl>
    {/if}
  </SettingsCard>
</div>

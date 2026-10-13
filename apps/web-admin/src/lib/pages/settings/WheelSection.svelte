<script lang="ts">
  import { onMount } from "svelte";
  import {
    chartCssColor,
    chartCssTextColor,
    computeExpectations,
    computeSliceArcs,
    formatPrizeLine,
    labelAnchor,
    naturalBokyaRateBasisPoints,
    rotationForSlice,
    svgArcPath,
  } from "@openpiso/ui/wheel";
  import * as api from "../../api";
  import { formatDuration, formatPoints, parsePointsToCenti } from "../../format";
  import type { WheelSettings, WheelSegmentRow } from "../../types";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Switch } from "../../components/ui/switch";

  let wheel = $state<WheelSettings | null>(null);
  let wheelEnabled = $state(false);
  let bokyaRateInput = $state("");
  let freeSpinCap = $state(3);
  let spinDuration = $state(5);
  let segments = $state<WheelSegmentRow[]>([]);
  let pointInputs = $state<string[]>([]);
  let wheelBusy = $state(false);
  let loadError = $state("");

  // -- live preview state -----------------------------------------------

  // Equal slices, so the geometry follows from the count alone (ADR-0047).
  const arcs = $derived(
    computeSliceArcs(segments.length, bokyaRateInput === "" ? null : Number(bokyaRateInput)),
  );

  const previewSegments = $derived(
    segments.map((segment, index) => ({
      ...segment,
      points_centi: parsePointsToCenti(pointInputs[index] ?? "") ?? 0,
    })),
  );
  const bokyaRateBasisPoints = $derived(
    bokyaRateInput === "" ? null : Number(bokyaRateInput),
  );
  const naturalBokyaRate = $derived(naturalBokyaRateBasisPoints(previewSegments));
  const expectations = $derived(
    computeExpectations(previewSegments, freeSpinCap, bokyaRateBasisPoints),
  );
  const liveHouseEdgeBasisPoints = $derived(
    wheel && wheel.point_value_seconds > 0
      ? Math.round(
          ((wheel.point_value_seconds - expectations.expectedSecondsPerSpin) /
            wheel.point_value_seconds) *
            10_000,
        )
      : 0,
  );
  const liveUnsafeHouseEdge = $derived(liveHouseEdgeBasisPoints <= 0);
  const liveUnsafeWheel = $derived(
    liveUnsafeHouseEdge || (wheel?.unsafe_shop_rate_house_edge ?? false),
  );

  // ----------------------------------------------------------------------

  async function loadWheel() {
    try {
      wheel = await api.getWheelSettings();
      wheelEnabled = wheel.enabled;
      bokyaRateInput = wheel.bokya_rate_basis_points?.toString() ?? "";
      freeSpinCap = wheel.free_spin_cap;
      spinDuration = wheel.spin_duration;
      segments = wheel.segments.map((s) => ({ ...s }));
      pointInputs = wheel.segments.map((s) => formatPoints(s.points_centi));
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function saveWheel() {
    wheelBusy = true;
    try {
      const parsedPoints = pointInputs.map(parsePointsToCenti);
      if (parsedPoints.some((value) => value === null)) {
        reportActionFailure(
          "save",
          "Spin the Wheel",
          new Error("Points must be non-negative numbers with at most two decimals"),
        );
        return;
      }
      wheel = await api.updateWheelSettings({
        enabled: wheelEnabled,
        bokya_rate_basis_points: bokyaRateBasisPoints,
        free_spin_cap: freeSpinCap,
        spin_duration: spinDuration,
        segments: segments.map((segment, index) => ({
          ...segment,
          points_centi: parsedPoints[index] ?? 0,
        })),
      });
      bokyaRateInput = wheel.bokya_rate_basis_points?.toString() ?? "";
      reportActionSuccess("save", "Spin the Wheel");
    } catch (caught) {
      reportActionFailure("save", "Spin the Wheel", caught);
    } finally {
      wheelBusy = false;
    }
  }

  function addSegment() {
    segments = [...segments, { label: "", seconds: 300, points_centi: 0, free_spin: false }];
    pointInputs = [...pointInputs, "0.00"];
  }

  /** Repeating a prize is the only way to make it likelier now that segments
   * carry no weight (ADR-0047), so it gets a first-class action. */
  function duplicateSegment(index: number) {
    const source = segments[index];
    if (!source) return;
    segments = [...segments.slice(0, index + 1), { ...source }, ...segments.slice(index + 1)];
    pointInputs = [
      ...pointInputs.slice(0, index + 1),
      pointInputs[index] ?? "0.00",
      ...pointInputs.slice(index + 1),
    ];
  }

  function removeSegment(i: number) {
    segments = segments.filter((_, idx) => idx !== i);
    pointInputs = pointInputs.filter((_, idx) => idx !== i);
  }

  function moveSegment(index: number, offset: -1 | 1) {
    const target = index + offset;
    if (target < 0 || target >= segments.length) return;
    [segments[index], segments[target]] = [segments[target], segments[index]];
    [pointInputs[index], pointInputs[target]] = [pointInputs[target], pointInputs[index]];
    segments = [...segments];
    pointInputs = [...pointInputs];
  }

  onMount(() => {
    void loadWheel();
  });
</script>

<div class="rounded-lg border bg-card p-6">
  <div class="mb-4 flex flex-wrap items-center justify-between gap-4">
    <h3 class="text-lg font-semibold">Spin the Wheel Settings</h3>
    <label class="flex items-center gap-2 text-sm font-medium">
      <Switch size="sm" bind:checked={wheelEnabled} />
      Wheel enabled
    </label>
  </div>

  <!--
    Warn on load, not only on save. An active Shop Rate can lower the paid-time
    value of an earned point enough to make an existing wheel unsafe without
    the owner touching the wheel. The existing tier guardrail remains a
    separate save/spin refusal; this rate-based warning is advisory only.
  -->
  {#if wheel && liveUnsafeWheel}
    <div class="mb-4 rounded-md border border-destructive bg-destructive/10 p-3 text-sm text-destructive">
      {#if liveUnsafeHouseEdge}
        <p class="font-semibold">This wheel pays out more than a point is worth.</p>
        <p class="mt-1">
          A spin is expected to return {formatDuration(Math.round(expectations.expectedSecondsPerSpin))},
          but a point redeems for {formatDuration(wheel.point_value_seconds)}. Customers would spin
          instead of redeem, so spins are refused until this is fixed. Lower the prizes, or repeat
          “Try again” across more segments.
        </p>
      {:else}
        <p class="font-semibold">This wheel is unsafe under the active Shop Rate.</p>
        <p class="mt-1">
          A spin is expected to return {formatDuration(Math.round(expectations.expectedSecondsPerSpin))},
          but an earned point currently buys only {formatDuration(wheel.shop_rate_point_value_seconds)}.
          Retune the wheel by lowering prizes, or repeat “Try again” across more segments.
        </p>
      {/if}
    </div>
  {/if}

  {#if wheel}
    <div class="mb-4 grid gap-4 sm:grid-cols-4">
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">House Edge:</span>
        <span class="ml-2 font-mono font-bold" class:text-primary={!liveUnsafeHouseEdge} class:text-destructive={liveUnsafeHouseEdge}>
          {(liveHouseEdgeBasisPoints / 100).toFixed(2)}%
        </span>
      </div>
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">Point Value:</span>
        <span class="ml-2 font-mono font-bold">{formatDuration(wheel.point_value_seconds)}</span>
      </div>
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">Exp. Time/spin:</span>
        <span class="ml-2 font-mono font-bold">{formatDuration(Math.round(expectations.expectedSecondsPerSpin))}</span>
      </div>
      <div class="rounded-md bg-muted p-3 text-sm">
        <span class="text-muted-foreground">Exp. Points/spin:</span>
        <span class="ml-2 font-mono font-bold">{formatPoints(Math.round(expectations.expectedPointsPerSpin))}</span>
      </div>
    </div>
  {/if}

  <!-- wheel preview -->
  <div class="mb-6 flex flex-col items-center gap-4 sm:flex-row">
    <div class="shrink-0">
      <svg
        viewBox="0 0 240 240"
        class="h-56 w-56"
        role="img"
        aria-label="Wheel preview"
      >
        <!-- pointer -->
        <polygon points="120,5 110,25 130,25" fill="var(--foreground)" />

        <!-- wheel body rotated to land preview slice 0 at the top -->
        <g
          transform="translate(120,120) rotate({((rotationForSlice(0, arcs) * 180) / Math.PI)})"
        >
          {#each arcs as arc, i}
            <path
              d={svgArcPath(arc, 100)}
              style="fill: {chartCssColor(i)}"
              stroke="var(--background)"
              stroke-width="2"
            />
            {@const anchor = labelAnchor(arc)}
            {#if arc.fraction > 0}
              <!-- Two lines: the owner's label, and beneath it the prize the
                   slice actually pays, derived from its own seconds/points. -->
              <text
                x={anchor.x * 100}
                y={anchor.y * 100}
                transform="rotate({anchor.angleDeg}, {anchor.x * 100}, {anchor.y * 100})"
                text-anchor="middle"
                dominant-baseline="central"
                fill={chartCssTextColor(i)}
                font-size="10"
                font-weight="500"
              >
                <tspan x={anchor.x * 100} dy="-0.5em">{segments[i]?.label || ""}</tspan>
                <tspan x={anchor.x * 100} dy="1.1em" font-size="8" font-weight="400" opacity="0.85">
                  {previewSegments[i] ? formatPrizeLine(previewSegments[i]) : ""}
                </tspan>
              </text>
            {/if}
          {:else}
            <circle cx="0" cy="0" r="100" fill="none" stroke="var(--border)" stroke-dasharray="6 4" />
            <text
              x="0" y="0"
              text-anchor="middle" dominant-baseline="central"
              fill="var(--muted-foreground)"
              font-size="13"
            >
              No segments
            </text>
          {/each}

          <!-- centre dot -->
          <circle cx="0" cy="0" r="6" fill="var(--foreground)" />
        </g>
      </svg>
    </div>
  </div>

  <div class="space-y-4">
    <div class="grid gap-4 sm:grid-cols-3">
      <div>
        <label class="block text-sm font-medium" for="wheel-free-spin-cap">Free spin cap</label>
        <input
          id="wheel-free-spin-cap"
          type="number"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          bind:value={freeSpinCap}
        />
      </div>
      <div>
        <label class="block text-sm font-medium" for="wheel-bokya-rate">Bokya Rate (%)</label>
        <input
          id="wheel-bokya-rate"
          type="number"
          min="0"
          max="100"
          step="0.01"
          placeholder="Unset"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          value={bokyaRateInput === "" ? "" : Number(bokyaRateInput) / 100}
          oninput={(event) => {
            const value = event.currentTarget.value;
            bokyaRateInput = value === "" ? "" : String(Math.round(Number(value) * 100));
          }}
        />
        <p class="mt-1 text-xs text-muted-foreground">
          Natural: {(naturalBokyaRate / 100).toFixed(2)}%. Clear to use natural uniform odds.
        </p>
      </div>
      <div>
        <label class="block text-sm font-medium" for="wheel-spin-duration">Spin duration (seconds)</label>
        <input
          id="wheel-spin-duration"
          type="number"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          bind:value={spinDuration}
        />
      </div>
    </div>
  </div>

  <div class="mt-6">
    <div class="mb-2 flex items-center justify-between">
      <h4 class="text-sm font-semibold">Segments</h4>
      <button class="text-xs text-primary hover:underline" type="button" onclick={addSegment}>+ Add</button>
    </div>

    <p class="mb-3 text-xs text-muted-foreground">
      Slices stay equal in size. Percentages below are the true landing odds under the current
      unsaved Bokya Rate.
    </p>

    <div class="space-y-2">
      {#each segments as seg, i}
        <div class="flex items-center gap-2 rounded-md border bg-muted/50 p-2 max-md:flex-wrap">
          <span class="w-6 shrink-0 text-center text-xs text-muted-foreground">{i + 1}</span>
          <input class="w-24 rounded border bg-background px-2 py-1 text-xs" aria-label={`Label for segment ${i + 1}`} placeholder="Label" bind:value={seg.label} />
          <input type="number" class="w-20 rounded border bg-background px-2 py-1 text-xs" aria-label={`Seconds for ${seg.label || `segment ${i + 1}`}`} placeholder="Seconds" bind:value={seg.seconds} />
          <input class="w-20 rounded border bg-background px-2 py-1 text-xs" inputmode="decimal" aria-label={`Points for ${seg.label || `segment ${i + 1}`}`} placeholder="Points" bind:value={pointInputs[i]} />
          <label class="flex items-center gap-1 text-xs">
            <input type="checkbox" bind:checked={seg.free_spin} class="rounded" />
            Free
          </label>
          <!-- What this slice actually pays, echoed back from the same
               derivation the customer sees on the wheel. -->
          <span class="min-w-24 flex-1 truncate text-xs font-medium" title="Prize paid by this slice">
            {previewSegments[i] ? formatPrizeLine(previewSegments[i]) : ""}
          </span>
          <span class="w-16 text-right font-mono text-xs" title="True landing probability">
            {(expectations.probabilityPercent[i] ?? 0).toFixed(2)}%
          </span>
          <button class="text-xs hover:underline disabled:opacity-40" type="button" aria-label={`Move segment ${i + 1} up`} disabled={i === 0} onclick={() => moveSegment(i, -1)}>↑</button>
          <button class="text-xs hover:underline disabled:opacity-40" type="button" aria-label={`Move segment ${i + 1} down`} disabled={i === segments.length - 1} onclick={() => moveSegment(i, 1)}>↓</button>
          <button class="text-xs text-primary hover:underline" type="button" onclick={() => duplicateSegment(i)}>Copy</button>
          <button class="text-xs text-destructive hover:underline" type="button" onclick={() => removeSegment(i)}>Remove</button>
        </div>
      {/each}
    </div>
  </div>

  {#if loadError}
    <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">{loadError}</div>
  {/if}

  <button
    class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
    type="button"
    disabled={wheelBusy}
    onclick={saveWheel}
  >
    {wheelBusy ? "Saving…" : "Save"}
  </button>
</div>

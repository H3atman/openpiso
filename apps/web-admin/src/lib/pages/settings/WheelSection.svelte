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
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import SettingsField from "../../components/settings/SettingsField.svelte";
  import ToggleRow from "../../components/settings/ToggleRow.svelte";
  import ArrowDownIcon from "@lucide/svelte/icons/arrow-down";
  import ArrowUpIcon from "@lucide/svelte/icons/arrow-up";
  import CopyIcon from "@lucide/svelte/icons/copy";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import Trash2Icon from "@lucide/svelte/icons/trash-2";

  let wheel = $state<WheelSettings | null>(null);
  let wheelEnabled = $state(false);
  let bokyaRateInput = $state("");
  let freeSpinCap = $state(3);
  let spinDuration = $state(5);
  let passwordConfirmationRequired = $state(false);
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
      passwordConfirmationRequired = wheel.password_confirmation_required;
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
        password_confirmation_required: passwordConfirmationRequired,
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

{#snippet figure(label: string, value: string, danger = false)}
  <div class="min-w-0">
    <p class="text-xs text-muted-foreground">{label}</p>
    <p class="mt-0.5 font-mono text-lg font-semibold tabular-nums" class:text-destructive={danger}>{value}</p>
  </div>
{/snippet}

<div class="space-y-6">
  <!--
    Warn on load, not only on save. An active Shop Rate can lower the paid-time
    value of an earned point enough to make an existing wheel unsafe without
    the owner touching the wheel. The existing tier guardrail remains a
    separate save/spin refusal; this rate-based warning is advisory only.
  -->
  {#if wheel && liveUnsafeWheel}
    {#if liveUnsafeHouseEdge}
      <Notice tone="danger" title="This wheel pays out more than a point is worth.">
        <p>
          A spin is expected to return {formatDuration(Math.round(expectations.expectedSecondsPerSpin))},
          but a point redeems for {formatDuration(wheel.point_value_seconds)}. Customers would spin
          instead of redeem, so spins are refused until this is fixed. Lower the prizes, or repeat
          “Try again” across more segments.
        </p>
      </Notice>
    {:else}
      <Notice tone="danger" title="This wheel is unsafe under the active Shop Rate.">
        <p>
          A spin is expected to return {formatDuration(Math.round(expectations.expectedSecondsPerSpin))},
          but an earned point currently buys only {formatDuration(wheel.shop_rate_point_value_seconds)}.
          Retune the wheel by lowering prizes, or repeat “Try again” across more segments.
        </p>
      </Notice>
    {/if}
  {/if}

  {#if loadError}
    <Notice tone="danger">{loadError}</Notice>
  {/if}

  <div class="grid items-start gap-6 xl:grid-cols-[minmax(0,22rem)_minmax(0,1fr)]">
    <SettingsCard title="Preview" description="Updates as you edit, before you save.">
      <svg
        viewBox="0 0 240 240"
        class="mx-auto size-56"
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

          <!-- centre dot; an empty wheel has no hub, only its "No segments" label -->
          {#if arcs.length > 0}
            <circle cx="0" cy="0" r="6" fill="var(--foreground)" />
          {/if}
        </g>
      </svg>

      {#if wheel}
        <div class="mt-6 grid grid-cols-2 gap-4 border-t pt-4">
          {@render figure("House Edge", `${(liveHouseEdgeBasisPoints / 100).toFixed(2)}%`, liveUnsafeHouseEdge)}
          {@render figure("Point Value", formatDuration(wheel.point_value_seconds))}
          {@render figure("Expected time per spin", formatDuration(Math.round(expectations.expectedSecondsPerSpin)))}
          {@render figure("Expected points per spin", formatPoints(Math.round(expectations.expectedPointsPerSpin)))}
        </div>
      {/if}
    </SettingsCard>

    <SettingsCard title="Rules" description="Who can spin, how often a free spin can chain, and how the odds are set.">
      <div class="space-y-4">
        <ToggleRow id="wheel-enabled" label="Wheel enabled" status={wheelEnabled ? "On" : "Off"}>
          <Switch id="wheel-enabled" bind:checked={wheelEnabled} />
        </ToggleRow>
        <ToggleRow
          id="wheel-password-required"
          label="Require password to spin"
          status={passwordConfirmationRequired ? "On" : "Off"}
        >
          <Switch id="wheel-password-required" bind:checked={passwordConfirmationRequired} />
        </ToggleRow>
      </div>

      <div class="mt-6 grid gap-4 border-t pt-6 sm:grid-cols-3">
        <SettingsField id="wheel-free-spin-cap" label="Free spin cap">
          <Input id="wheel-free-spin-cap" type="number" class="tabular-nums" bind:value={freeSpinCap} />
        </SettingsField>
        <SettingsField
          id="wheel-bokya-rate"
          label="Bokya Rate (%)"
          hint="Natural: {(naturalBokyaRate / 100).toFixed(2)}%. Clear to use natural uniform odds."
        >
          <Input
            id="wheel-bokya-rate"
            type="number"
            min="0"
            max="100"
            step="0.01"
            placeholder="Unset"
            class="tabular-nums"
            aria-describedby="wheel-bokya-rate-hint"
            value={bokyaRateInput === "" ? "" : Number(bokyaRateInput) / 100}
            oninput={(event) => {
              const value = event.currentTarget.value;
              bokyaRateInput = value === "" ? "" : String(Math.round(Number(value) * 100));
            }}
          />
        </SettingsField>
        <SettingsField id="wheel-spin-duration" label="Spin duration (seconds)">
          <Input id="wheel-spin-duration" type="number" class="tabular-nums" bind:value={spinDuration} />
        </SettingsField>
      </div>
    </SettingsCard>
  </div>

  <SettingsCard
    title="Segments"
    description="Slices stay equal in size, so repeat a prize to make it likelier. Odds are the true landing odds under the current unsaved Bokya Rate."
    flush
  >
    {#snippet actions()}
      <Button variant="outline" size="sm" onclick={addSegment}>
        <PlusIcon aria-hidden="true" />
        Add segment
      </Button>
    {/snippet}

    {#if segments.length === 0}
      <p class="px-4 py-10 text-center text-sm text-muted-foreground">No segments yet. Add one to build the wheel.</p>
    {:else}
      <div
        class="hidden grid-cols-[2rem_minmax(0,1fr)_6rem_6rem_5rem_minmax(0,1fr)_4.5rem_7rem] items-center gap-x-3 border-b bg-muted/40 px-6 py-2 text-xs font-medium text-muted-foreground xl:grid"
      >
        <span>#</span>
        <span>Label</span>
        <span>Seconds</span>
        <span>Points</span>
        <span>Free spin</span>
        <span>Prize paid</span>
        <span class="text-right">Odds</span>
        <span class="sr-only">Actions</span>
      </div>
      <ol class="divide-y">
        <!-- Below 1280px each segment is a small form; from 1280px it is one
             row under the column headers. -->
        {#each segments as seg, i}
          <li
            class="grid grid-cols-2 items-end gap-x-3 gap-y-3 px-4 py-3 sm:grid-cols-4 xl:grid-cols-[2rem_minmax(0,1fr)_6rem_6rem_5rem_minmax(0,1fr)_4.5rem_7rem] xl:items-center xl:gap-y-0 xl:px-6"
          >
            <div class="col-span-full flex items-center justify-between gap-2 xl:contents">
              <span class="text-sm font-medium text-muted-foreground tabular-nums xl:col-start-1 xl:row-start-1">
                <span class="xl:hidden">{"Segment "}</span>{i + 1}
              </span>
              <div class="flex items-center xl:col-start-8 xl:row-start-1">
                <Button variant="ghost" size="icon-sm" aria-label={`Move segment ${i + 1} up`} title="Move up" disabled={i === 0} onclick={() => moveSegment(i, -1)}>
                  <ArrowUpIcon aria-hidden="true" />
                </Button>
                <Button variant="ghost" size="icon-sm" aria-label={`Move segment ${i + 1} down`} title="Move down" disabled={i === segments.length - 1} onclick={() => moveSegment(i, 1)}>
                  <ArrowDownIcon aria-hidden="true" />
                </Button>
                <Button variant="ghost" size="icon-sm" aria-label={`Copy segment ${i + 1}`} title="Copy (makes this prize likelier)" onclick={() => duplicateSegment(i)}>
                  <CopyIcon aria-hidden="true" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  class="text-muted-foreground hover:text-destructive"
                  aria-label={`Remove segment ${i + 1}`}
                  title="Remove"
                  onclick={() => removeSegment(i)}
                >
                  <Trash2Icon aria-hidden="true" />
                </Button>
              </div>
            </div>
            <div class="col-span-2 grid gap-1 xl:col-span-1 xl:col-start-2 xl:row-start-1">
              <span class="text-xs text-muted-foreground xl:hidden" aria-hidden="true">Label</span>
              <Input aria-label={`Label for segment ${i + 1}`} placeholder="Label" bind:value={seg.label} />
            </div>
            <div class="grid gap-1 xl:col-start-3 xl:row-start-1">
              <span class="text-xs text-muted-foreground xl:hidden" aria-hidden="true">Seconds</span>
              <Input type="number" class="tabular-nums" aria-label={`Seconds for ${seg.label || `segment ${i + 1}`}`} placeholder="Seconds" bind:value={seg.seconds} />
            </div>
            <div class="grid gap-1 xl:col-start-4 xl:row-start-1">
              <span class="text-xs text-muted-foreground xl:hidden" aria-hidden="true">Points</span>
              <Input inputmode="decimal" class="tabular-nums" aria-label={`Points for ${seg.label || `segment ${i + 1}`}`} placeholder="Points" bind:value={pointInputs[i]} />
            </div>
            <label class="flex min-h-11 items-center gap-2 text-sm md:min-h-8 xl:col-start-5 xl:row-start-1">
              <input type="checkbox" class="size-4 accent-primary" bind:checked={seg.free_spin} />
              <span class="xl:sr-only">Free spin</span>
            </label>
            <!-- True landing probability, before the prize so phones pair it
                 with the Free spin box and give the prize its own line. -->
            <p class="self-center text-right text-sm tabular-nums xl:col-start-7 xl:row-start-1">
              <span class="mr-1 text-xs text-muted-foreground xl:hidden">Odds</span>
              <span class="font-mono">{(expectations.probabilityPercent[i] ?? 0).toFixed(2)}%</span>
            </p>
            <!-- What this slice actually pays, echoed back from the same
                 derivation the customer sees on the wheel. -->
            <p class="col-span-2 min-w-0 truncate text-sm font-medium xl:col-span-1 xl:col-start-6 xl:row-start-1" title="Prize paid by this slice">
              <span class="mr-1 text-xs font-normal text-muted-foreground xl:hidden">Pays</span>
              {previewSegments[i] ? formatPrizeLine(previewSegments[i]) : ""}
            </p>
          </li>
        {/each}
      </ol>
    {/if}

    {#snippet footer()}
      <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">Saves the rules and segments together.</p>
      <Button disabled={wheelBusy} onclick={saveWheel}>
        {wheelBusy ? "Saving…" : "Save wheel"}
      </Button>
    {/snippet}
  </SettingsCard>
</div>

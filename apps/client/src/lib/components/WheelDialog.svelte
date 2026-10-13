<script lang="ts">
  // WheelDialog — animated Spin the Wheel dialog.
  //
  // One shared dialog for both the Widget and the Fullscreen Lock.
  // The caller fetches the wheel settings and passes segments + spin_duration;
  // this component draws the wheel, animates the spin, and walks the
  // pre-resolved free-spin chain one landing at a time.
  //
  // The server resolves the entire chain atomically before any animation;
  // the "spin again" button issues no request — it merely reveals the
  // next step the server already chose (see the comment at its call site).

  import { untrack } from "svelte";
  import { Button } from "$lib/components/ui/button";
  import { Card, CardContent } from "$lib/components/ui/card";
  import { formatPoints, formatProseDuration } from "$lib/format";
  import { chartCssColor, chartCssTextColor, computeSliceArcs, formatPrizeLine, labelAnchor, rotationForSlice, svgArcPath } from "@openpiso/ui/wheel";
  import type { WheelSegmentRow, WheelSpinResult } from "$lib/types";
  import WidgetOverlay from "./WidgetOverlay.svelte";

  interface Props {
    open: boolean;
    /** Wheel segment definitions (the full prize table, not just results). */
    segments: WheelSegmentRow[];
    /** Duration of the spin animation in seconds (owner-configured). */
    spinDuration: number;
    /** Latest spin outcome from the server, or null before first spin. */
    spinResult: WheelSpinResult | null;
    /** True while the server request is in-flight. */
    spinning: boolean;
    /** Error message from a failed spin. */
    error: string;
    /** Customer's current points balance in centi-points (refreshed after
     *  each spin) — decides whether another spin is affordable. */
    pointsBalanceCenti: number;
    /** Called when the customer presses Spin. */
    onSpin: () => Promise<void>;
    /** Called when the dialog is dismissed by the customer. */
    onClose: () => void;
  }

  let {
    open,
    segments,
    spinDuration,
    spinResult,
    spinning,
    error,
    pointsBalanceCenti,
    onSpin,
    onClose,
  }: Props = $props();

  /** Cost of one spin, mirrored from the server (ADR-0047). */
  const SPIN_COST_CENTI = 100;

  const canAffordSpin = $derived(pointsBalanceCenti >= SPIN_COST_CENTI);

  // -- wheel geometry ---------------------------------------------------

  // Equal slices, so the geometry follows from the count alone (ADR-0047).
  const arcs = $derived(computeSliceArcs(segments.length));

  // -- spin state machine -----------------------------------------------

  type SpinPhase = "idle" | "animating" | "reveal" | "done";

  // How long to wait after the wheel stops before showing the result text.
  const WAIT_BEFORE_REVEAL_MS = 400;

  let phase = $state<SpinPhase>("idle");
  /** Which step of `spinResult.steps` is currently revealed (0-based). */
  let currentStep = $state(0);
  let accumulatedSeconds = $state(0);
  let accumulatedPointsCenti = $state(0);

  /** Running total of CSS rotation degrees so the wheel always continues
   *  from where it last stopped. */
  let totalRotationDeg = $state(0);

  // Track the animation duration so the CSS transition matches.
  let animDurationSec = $state(5);

  // Reset state when the dialog opens.
  $effect(() => {
    if (open) {
      phase = "idle";
      currentStep = 0;
      accumulatedSeconds = 0;
      accumulatedPointsCenti = 0;
    }
  });

  // Apply spin_duration prop when available.
  $effect(() => {
    if (spinDuration > 0) {
      animDurationSec = spinDuration;
    }
  });

  /** Rotate wheel so the given segment index lands at the pointer.
   *  Adds 3–5 extra full turns for visual drama. */
  function rotateToSegment(segmentIndex: number): void {
    const targetRad = rotationForSlice(segmentIndex, arcs);
    const targetDeg = (targetRad * 180) / Math.PI;
    const normalizedTarget = ((targetDeg % 360) + 360) % 360;
    const currentMod = ((totalRotationDeg % 360) + 360) % 360;
    // How far from current angle to the target (shortest forward path).
    const diff = (normalizedTarget - currentMod + 720) % 360;
    // Add 3–5 extra full turns for visual drama.
    const extraTurns = 3 + Math.floor(Math.random() * 3);
    totalRotationDeg += diff + extraTurns * 360;
  }

  /** True while a server request is in flight, so the animation effect does
   *  not replay the PREVIOUS spin's result while waiting for the new one. */
  let awaitingResult = $state(false);

  /** Kick off a spin: request the server outcome, then animate. */
  async function handleSpin(): Promise<void> {
    if (phase === "animating" || awaitingResult) return;
    awaitingResult = true;
    phase = "animating";
    currentStep = 0;
    accumulatedSeconds = 0;
    accumulatedPointsCenti = 0;
    try {
      await onSpin();
    } finally {
      awaitingResult = false;
    }
    // A failed spin leaves the previous result in place; fall back to idle
    // instead of re-animating an outcome that was already paid out.
    if (error) {
      phase = "idle";
      return;
    }
    // spinResult signal updates via the effect below.
  }

  /** Advance to the next free-spin landing WITHOUT a server request.
   *
   *  The "spin again" control issues NO request — the outcome was already
   *  chosen and credited by the server in the single `spin_wheel` call.
   *  The client only animates a result the server already chose, consistent
   *  with the rule that the client is untrusted (ADR-0005). This looks like
   *  a bug to a reviewer, hence this comment. */
  function advanceStep(): void {
    if (!spinResult) return;
    const next = currentStep + 1;
    if (next >= spinResult.steps.length) {
      phase = "done";
      return;
    }
    const nextStep = spinResult.steps[next];
    currentStep = next;
    accumulatedSeconds += nextStep.seconds;
    accumulatedPointsCenti += nextStep.points_centi;
    phase = "animating";
  }

  // When spinResult arrives and we are in animating phase, start the
  // wheel rotation animation.  After the animation duration elapses,
  // transition to "reveal" so the customer sees the result.
  $effect(() => {
    if (awaitingResult || phase !== "animating" || !spinResult || spinResult.steps.length === 0) {
      return;
    }
    const step = spinResult.steps[currentStep];
    if (!step) return;
    untrack(() => rotateToSegment(step.segment_index));
    if (currentStep === 0) {
      accumulatedSeconds = step.seconds;
      accumulatedPointsCenti = step.points_centi;
    }
    const ms = animDurationSec * 1000 + WAIT_BEFORE_REVEAL_MS;
    const timer = setTimeout(() => {
      phase = "reveal";
    }, ms);
    return () => clearTimeout(timer);
  });

  // Detect a spin that failed (spinning false but spinResult still null).
  let wasSpinning = $state(false);
  $effect(() => {
    if (spinning && !wasSpinning) {
      wasSpinning = true;
    } else if (!spinning && wasSpinning) {
      wasSpinning = false;
      if (phase === "animating" && !spinResult) {
        // The server request finished but returned no result — error case.
        phase = "idle";
      }
    }
  });

  // -- derived display helpers ------------------------------------------

  const totalSteps = $derived(spinResult?.steps.length ?? 1);

  function closeAndRefresh(): void {
    // Balances refresh on dialog close, not on animation end (consistent
    // with the rule that the server already credited the outcome). A customer
    // who closes mid-chain has already been credited in full, so the next
    // balance they see is correct and never stale.
    onClose();
  }
</script>

<!-- End-of-round actions: another paid spin when the customer can still
     afford one, otherwise Close plus the shortfall reason. -->
{#snippet endActions()}
  {#if canAffordSpin}
    <Button type="button" disabled={spinning} onclick={() => void handleSpin()}>
      {spinning ? "Spinning…" : "Spin Again"}
    </Button>
  {:else}
    <Button type="button" onclick={closeAndRefresh}>Close</Button>
    <p class="text-destructive text-xs font-bold">Not enough points</p>
  {/if}
{/snippet}

<WidgetOverlay title="Spin the Wheel" {open}>
  <Card class="w-full max-w-xs">
    <CardContent class="flex flex-col items-center gap-3 px-4 py-5">
      <!-- Wheel SVG -->
      <div class="relative flex items-center justify-center">
        <svg
          viewBox="0 0 240 240"
          class="h-52 w-52"
          role="img"
          aria-label="Wheel"
        >
          <!-- Pointer at 12 o'clock -->
          <polygon points="120,5 110,22 130,22" fill="var(--foreground)" />

          <!-- Centered wheel body with CSS-based rotation animation.
               The outer <g> centres the wheel; the inner <g> spins. -->
          <g transform="translate(120,120)">
            <g
              style="transform: rotate({totalRotationDeg}deg);
                     transition: transform {animDurationSec}s cubic-bezier(0.17, 0.67, 0.12, 0.99);"
            >
              {#each arcs as arc, i}
                <path
                  d={svgArcPath(arc, 100)}
                  style="fill: {chartCssColor(i)}"
                  stroke="var(--background)"
                  stroke-width="2"
                />
                {@const anchor = labelAnchor(arc)}
                {#if arc.fraction > 0.03}
                  <!-- Two lines: the shop's label, and beneath it the prize
                       this slice actually pays. The derived line comes from
                       the segment's own seconds/points, so a label can never
                       misdescribe the payout (ADR-0047). -->
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
                    {#if segments[i]}
                      <tspan x={anchor.x * 100} dy="1.1em" font-size="8" font-weight="400" opacity="0.85">
                        {formatPrizeLine(segments[i])}
                      </tspan>
                    {/if}
                  </text>
                {/if}
              {:else}
                <circle
                  cx="0" cy="0" r="100"
                  fill="none" stroke="var(--border)" stroke-dasharray="6 4"
                />
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
          </g>
        </svg>
      </div>

      <div class="flex max-h-16 w-full flex-wrap justify-center gap-x-2 gap-y-1 overflow-y-auto text-[0.6rem]" aria-label="Wheel prizes">
        {#each segments as segment, i}
          <span class="inline-flex items-center gap-1">
            <span class="h-2 w-2 rounded-full" style="background: {chartCssColor(i)}"></span>
            {segment.label}
            <span class="opacity-70">{formatPrizeLine(segment)}</span>
          </span>
        {/each}
      </div>

      <!-- Error message -->
      {#if error}
        <p class="text-destructive text-center text-xs" role="alert" aria-live="assertive">
          {error}
        </p>
      {/if}

      <!-- Phase: idle — show Spin button -->
      {#if phase === "idle"}
        <Button
          type="button"
          disabled={spinning}
          onclick={() => void handleSpin()}
        >
          {spinning ? "Spinning…" : "Spin for 1 point"}
        </Button>
        <p class="text-muted-foreground text-center text-[0.6rem]">
          Bet 1 point for a chance at bonus time.
        </p>

      <!-- Phase: animating — wheel is turning -->
      {:else if phase === "animating"}
        <p class="text-muted-foreground animate-pulse text-center text-xs">
          Spinning…
        </p>

      <!-- Phase: reveal — show current landing -->
      {:else if phase === "reveal"}
        {#if spinResult && spinResult.steps[currentStep]}
          {@const step = spinResult.steps[currentStep]}
          {@const hasNextSpin = step.free_spin && currentStep < totalSteps - 1}
          <div class="flex flex-col items-center gap-2 text-center" role="status" aria-live="polite">
            {#if step.seconds > 0 || step.points_centi > 0}
              <strong class="text-foreground text-sm">
                {#if step.seconds > 0}
                  Won {formatProseDuration(step.seconds)}!
                {/if}
                {#if step.points_centi > 0}
                  {step.seconds > 0 ? " and " : "Won "}
                  {formatPoints(step.points_centi)} pts!
                {/if}
              </strong>
            {:else if !step.free_spin}
              <strong class="text-foreground text-sm">No win this spin.</strong>
            {/if}
            {#if step.free_spin}
              <strong class="text-primary text-sm">{hasNextSpin ? "Free Spin!" : "Free Spin cap reached."}</strong>
            {/if}

            <!-- Accumulated winnings tracker (visible during chain) -->
            {#if totalSteps > 1}
              <div class="bg-muted rounded-md px-3 py-1.5">
                <p class="text-muted-foreground text-[0.6rem] tracking-wider uppercase">
                  Accumulated ({currentStep + 1}/{totalSteps})
                </p>
                <p class="text-xs font-bold">
                  {#if accumulatedSeconds > 0}
                    {formatProseDuration(accumulatedSeconds)}
                  {/if}
                  {#if accumulatedPointsCenti > 0}
                    {accumulatedSeconds > 0 ? " + " : ""}
                    {formatPoints(accumulatedPointsCenti)} pts
                  {/if}
                </p>
              </div>
            {/if}

            <!-- Free spin offers "Spin again" (no server request) -->
            {#if hasNextSpin}
              <Button
                type="button"
                variant="outline"
                onclick={advanceStep}
              >
                Spin again
              </Button>
              <!-- "Spin again" issues no server request — the outcome was
                   already resolved by the server. See advanceStep(). -->
            {:else if !step.free_spin || currentStep >= totalSteps - 1}
              {@render endActions()}
            {/if}
          </div>
        {/if}

      <!-- Phase: done — all steps revealed -->
      {:else if phase === "done"}
        <div class="flex flex-col items-center gap-2 text-center" role="status" aria-live="polite">
          <strong class="text-foreground text-sm">
            {#if accumulatedSeconds > 0 || accumulatedPointsCenti > 0}
              Total won:
              {#if accumulatedSeconds > 0}
                {formatProseDuration(accumulatedSeconds)}
              {/if}
              {#if accumulatedPointsCenti > 0}
                {accumulatedSeconds > 0 ? " + " : ""}
                {formatPoints(accumulatedPointsCenti)} pts
              {/if}
            {:else}
              No winnings this round.
            {/if}
          </strong>
          {@render endActions()}
        </div>
      {/if}

      <!-- Close button always available — spec req: customer can close at
           any point, including mid-animation, mid-chain, and after the
           round ends (where the primary control may be "Spin Again"). -->
      <div class="mt-1">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onclick={closeAndRefresh}
        >
          Close
        </Button>
      </div>
    </CardContent>
  </Card>
</WidgetOverlay>

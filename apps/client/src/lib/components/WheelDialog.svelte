<script lang="ts">
  // WheelDialog — animated Spin the Wheel dialog.
  //
  // One shared dialog for both the Widget and the Fullscreen Lock. It is a
  // view of the spin machine state (`$lib/machines/spin`) plus callbacks that
  // dispatch to it: the machine owns the request, the Spin Duration timer and
  // the free-spin chain; this component draws the wheel, turns it toward the
  // current step, and shows the password prompt (ADR-0088). Its only flow
  // state is the typed password.
  //
  // The server resolves the entire chain atomically before any animation;
  // the "spin again" button issues no request — it merely reveals the
  // next step the server already chose (see the comment at its call site).

  import { untrack } from "svelte";
  import { Button } from "$lib/components/ui/button";
  import { Card, CardContent } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { formatPoints, formatProseDuration } from "$lib/format";
  import { chartCssColor, chartCssTextColor, computeSliceArcs, formatPrizeLine, labelAnchor, rotationForSlice, svgArcPath } from "@openpiso/ui/wheel";
  import type { SpinState } from "$lib/machines/spin";
  import WidgetOverlay from "./WidgetOverlay.svelte";

  interface Props {
    /** Spin machine state; the dialog is open unless it is `closed`. */
    spinState: SpinState;
    /** Customer's current points balance in centi-points (refreshed after
     *  each spin) — decides whether another spin is affordable. */
    pointsBalanceCenti: number;
    /** Called when the customer presses Spin. */
    onSpin: () => void;
    /** Called with the typed password when the customer confirms a spin. */
    onSubmitPassword: (password: string) => void;
    /** Called when the customer presses "Spin again" in a free-spin chain. */
    onNextFreeSpin: () => void;
    /** Called when the dialog is dismissed by the customer. */
    onClose: () => void;
  }

  let { spinState, pointsBalanceCenti, onSpin, onSubmitPassword, onNextFreeSpin, onClose }: Props = $props();

  const open = $derived(spinState.kind !== "closed");
  const segments = $derived(spinState.kind === "closed" ? [] : spinState.wheel.segments);
  const animDurationSec = $derived(spinState.kind === "closed" ? 5 : spinState.wheel.spinDuration || 5);
  const prompting = $derived(spinState.kind === "confirming");
  // The spin is charged once requested; Close stays hidden until it is revealed.
  const inFlight = $derived(spinState.kind === "requesting" || spinState.kind === "animating");
  const error = $derived(spinState.kind === "idle" || spinState.kind === "confirming" ? spinState.error : "");

  // The landing on screen, and the winnings of the chain up to and including it.
  const landing = $derived(
    spinState.kind === "animating" || spinState.kind === "revealed" ? spinState : null,
  );
  const step = $derived(landing?.result.steps[landing.step]);
  const totalSteps = $derived(landing?.result.steps.length ?? 1);
  const accumulated = $derived.by(() => {
    const steps = landing?.result.steps.slice(0, landing.step + 1) ?? [];
    return {
      seconds: steps.reduce((sum, each) => sum + each.seconds, 0),
      pointsCenti: steps.reduce((sum, each) => sum + each.points_centi, 0),
    };
  });

  /** Cost of one spin, mirrored from the server (ADR-0047). */
  const SPIN_COST_CENTI = 100;

  const canAffordSpin = $derived(pointsBalanceCenti >= SPIN_COST_CENTI);

  // -- wheel geometry ---------------------------------------------------

  // Equal slices, so the geometry follows from the count alone (ADR-0047).
  const arcs = $derived(computeSliceArcs(segments.length));

  // -- wheel animation ---------------------------------------------------

  /** Running total of CSS rotation degrees so the wheel always continues
   *  from where it last stopped. */
  let totalRotationDeg = $state(0);

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

  // Turn the wheel toward each step as the machine enters `animating`.
  $effect(() => {
    if (spinState.kind === "animating") {
      const target = spinState.result.steps[spinState.step];
      if (target) untrack(() => rotateToSegment(target.segment_index));
    }
  });

  // The password never outlives the prompt: clear it whenever the machine
  // leaves `confirming` (submit, close, ...).
  let password = $state("");
  $effect(() => {
    if (!prompting) password = "";
  });

  function submitPassword(event: SubmitEvent): void {
    event.preventDefault();
    onSubmitPassword(password);
  }
</script>

<!-- End-of-round actions: another paid spin when the customer can still
     afford one, otherwise Close plus the shortfall reason. -->
{#snippet endActions()}
  {#if canAffordSpin}
    <Button type="button" onclick={onSpin}>Spin Again</Button>
  {:else}
    <Button type="button" onclick={onClose}>Close</Button>
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

      <!-- Password confirmation (ADR-0088) — replaces the spin controls
           until the customer submits. -->
      {#if prompting}
        <form class="flex w-full flex-col gap-2" onsubmit={submitPassword}>
          <label for="wheel-password" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
            Confirm password to spin
          </label>
          <Input
            id="wheel-password"
            bind:value={password}
            type="password"
            autocomplete="current-password"
            maxlength={128}
            required
          />
          <Button type="submit" disabled={!password}>Spin for 1 point</Button>
        </form>

      <!-- State: idle — show Spin button -->
      {:else if spinState.kind === "idle"}
        <Button type="button" onclick={onSpin}>Spin for 1 point</Button>
        <p class="text-muted-foreground text-center text-[0.6rem]">
          Bet 1 point for a chance at bonus time.
        </p>

      <!-- State: requesting / animating — wheel is turning -->
      {:else if inFlight}
        <p class="text-muted-foreground animate-pulse text-center text-xs">
          Spinning…
        </p>

      <!-- State: revealed — show current landing -->
      {:else if spinState.kind === "revealed" && step}
        {@const hasNextSpin = step.free_spin && spinState.step < totalSteps - 1}
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
                  Accumulated ({spinState.step + 1}/{totalSteps})
                </p>
                <p class="text-xs font-bold">
                  {#if accumulated.seconds > 0}
                    {formatProseDuration(accumulated.seconds)}
                  {/if}
                  {#if accumulated.pointsCenti > 0}
                    {accumulated.seconds > 0 ? " + " : ""}
                    {formatPoints(accumulated.pointsCenti)} pts
                  {/if}
                </p>
              </div>
            {/if}

            <!-- Free spin offers "Spin again" (no server request) -->
            {#if hasNextSpin}
              <Button
                type="button"
                variant="outline"
                onclick={onNextFreeSpin}
              >
                Spin again
              </Button>
              <!-- "Spin again" issues no server request — the outcome was
                   already resolved by the server. See nextFreeSpin in the spin machine. -->
            {:else}
              {@render endActions()}
            {/if}
          </div>

      {/if}

      <!-- Close is hidden while the spin is in flight: it is already charged,
           and closing would lose the prize. -->
      {#if !inFlight}
        <div class="mt-1">
          <Button type="button" variant="ghost" size="sm" onclick={onClose}>
            Close
          </Button>
        </div>
      {/if}
    </CardContent>
  </Card>
</WidgetOverlay>

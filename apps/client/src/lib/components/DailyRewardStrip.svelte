<script lang="ts">
  import {
    attendanceProgressPercent,
    formatPeso,
    formatProseDuration,
  } from "$lib/format";
  import type { AttendanceSummary } from "$lib/types";

  interface Props {
    summary: AttendanceSummary;
    stale?: boolean;
    onOpen: () => void;
  }

  let { summary, stale = false, onOpen }: Props = $props();

  const progressPercent = $derived(
    summary.today.status === "checked_in"
      ? 100
      : summary.enabled && summary.today.status === "in_progress"
        ? attendanceProgressPercent(
            summary.today.capped_cents,
            summary.today.daily_threshold_cents,
          )
        : 0,
  );
  const primaryCopy = $derived.by(() => {
    if (!summary.enabled || summary.today.status === "ineligible") {
      return "Daily Reward paused";
    }
    if (summary.today.status === "checked_in") {
      return `Today earned ${formatProseDuration(summary.today.reward_seconds)}`;
    }
    if (summary.today.status === "in_progress") {
      const remainingCents = Math.max(
        0,
        summary.today.daily_threshold_cents - summary.today.capped_cents,
      );
      return remainingCents > 0
        ? `${formatPeso(remainingCents)} more today`
        : `Today earned ${formatProseDuration(summary.today.reward_seconds)}`;
    }
    return "Daily Reward paused";
  });
  const secondaryCopy = $derived(
    summary.totals.claimable_seconds > 0
      ? `${formatProseDuration(summary.totals.claimable_seconds)} waiting to claim`
      : summary.enabled
        ? "Earn coins during Shop Open"
        : "Daily Reward is turned off",
  );
</script>

<button
  type="button"
  class="flex w-full items-center gap-3 rounded-md border p-2 text-left transition-colors hover:bg-muted/70 {summary.totals.outstanding_seconds >
  0
    ? 'border-primary/50 bg-primary/10'
    : 'border-border bg-muted/30'}"
  data-daily-reward-strip
  aria-label="Daily Reward: {primaryCopy}. {secondaryCopy}. Open details."
  onclick={onOpen}
>
  <span
    class="relative size-11 shrink-0 rounded-full p-[3px]"
    style="background: conic-gradient(var(--primary) {progressPercent}%, var(--border) 0)"
    data-daily-reward-progress
    aria-label="{summary.today.status === 'checked_in'
      ? 'Daily Reward complete'
      : `${progressPercent}% of today's Daily Reward`}"
  >
    <span
      class="bg-card text-foreground flex size-full items-center justify-center rounded-full text-[0.55rem] font-bold tabular-nums"
      aria-hidden="true"
      >{summary.today.status === "checked_in" ? "✓" : `${progressPercent}%`}</span
    >
  </span>
  <span class="min-w-0 flex-1">
    <span class="text-muted-foreground block text-[0.55rem] font-bold tracking-widest uppercase"
      >Daily Reward</span
    >
    <span class="block truncate font-semibold" data-daily-reward-primary
      >{primaryCopy}</span
    >
    <span class="text-muted-foreground block truncate text-[0.65rem]"
      >{secondaryCopy}</span
    >
    {#if stale}
      <span class="text-muted-foreground block text-[0.6rem]" data-daily-reward-stale
        >Showing saved information</span
      >
    {/if}
  </span>
  <span class="text-muted-foreground text-lg" aria-hidden="true">›</span>
</button>

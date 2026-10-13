<script lang="ts">
  import { Button } from "$lib/components/ui/button";
  import {
    attendanceProgressPercent,
    formatAttendanceDate,
    formatAttendanceMonth,
    formatProseDuration,
  } from "$lib/format";
  import type {
    AttendanceDay,
    AttendanceMonth,
    AttendanceMonthSummary,
    AttendanceSummary,
  } from "$lib/types";
  import WidgetOverlay from "./WidgetOverlay.svelte";

  interface Props {
    open: boolean;
    summary: AttendanceSummary;
    selectedMonth: string;
    monthDetails: AttendanceMonth | null;
    monthLoading: boolean;
    monthError: string;
    stale: boolean;
    claimingMonth: string | null;
    claimError: string;
    onSelectMonth: (earningMonth: string) => void;
    onClose: () => void;
    onClaim: (earningMonth: string) => void;
  }

  let {
    open,
    summary,
    selectedMonth,
    monthDetails,
    monthLoading,
    monthError,
    stale,
    claimingMonth,
    claimError,
    onSelectMonth,
    onClose,
    onClaim,
  }: Props = $props();

  const selectedIndex = $derived(
    summary.calendar_months.findIndex(
      (month) => month.earning_month === selectedMonth,
    ),
  );
  const selectedSummary = $derived(
    summary.calendar_months.find(
      (month) => month.earning_month === selectedMonth,
    ) ?? null,
  );
  const olderClaimableMonths = $derived(
    summary.older_claimable_months.filter((month) => month.claimable_seconds > 0),
  );
  const firstWeekday = $derived(getFirstWeekday(selectedMonth));
  const calendarCells = $derived([
    ...Array.from({ length: firstWeekday }, () => null),
    ...(monthDetails?.days ?? []),
  ]);

  function getFirstWeekday(earningMonth: string): number {
    const [year, month] = earningMonth.split("-").map(Number);
    if (!year || !month) return 0;
    return new Date(Date.UTC(year, month - 1, 1)).getUTCDay();
  }

  function dayDescription(day: AttendanceDay): string {
    switch (day.status) {
      case "checked_in":
        return "Daily Reward earned";
      case "missed":
        return "No Daily Reward earned";
      case "ineligible":
        return "Not eligible";
      case "future":
        return "Future date";
      case "today_progress":
        return "Today's progress";
    }
  }

  function dayMark(day: AttendanceDay): string {
    if (day.status === "checked_in") return "✓";
    if (day.status === "missed") return "×";
    if (day.status === "ineligible") return "—";
    if (day.status === "today_progress") {
      return `${attendanceProgressPercent(
        day.capped_cents ?? 0,
        day.daily_threshold_cents ?? 0,
      )}%`;
    }
    return "";
  }

  function claimTiming(month: AttendanceMonthSummary): string {
    const timing: string[] = [];
    if (month.latest_claim_on) {
      timing.push(`Claimed ${formatAttendanceDate(month.latest_claim_on, summary.admin_local_date)}`);
    }
    if (month.state === "claimable" && month.deadline_date_inclusive) {
      timing.push(`Claim by ${formatAttendanceDate(month.deadline_date_inclusive, summary.admin_local_date)}`);
    } else if (month.state === "expired" && month.deadline_date_inclusive) {
      timing.push(`Expired ${formatAttendanceDate(month.deadline_date_inclusive, summary.admin_local_date)}`);
    } else if (month.state === "earning" && month.claim_starts_on) {
      timing.push(`Claimable from ${formatAttendanceDate(month.claim_starts_on, summary.admin_local_date)}`);
    }
    return timing.join(" · ");
  }

  function claimDisabledReason(month: AttendanceMonthSummary): string {
    if (month.claimable_seconds > 0) return "";
    if (month.state === "earning") {
      return month.claim_starts_on
        ? `Claiming starts ${formatAttendanceDate(month.claim_starts_on, summary.admin_local_date)}`
        : "Claiming opens next month";
    }
    if (month.state === "expired" && month.deadline_date_inclusive) {
      return `Expired ${formatAttendanceDate(month.deadline_date_inclusive, summary.admin_local_date)}`;
    }
    if (month.state === "claimed") return "All Daily Reward time has been claimed";
    if (month.state === "claimable") return "No Daily Reward time is ready to claim";
    if (month.state === "forfeited") return "Daily Reward is unavailable";
    return "No Daily Reward was earned this month";
  }
</script>

<WidgetOverlay title="Daily Reward" {open}>
  <section
    class="bg-card text-card-foreground my-auto flex max-h-[calc(100dvh-2rem)] w-full max-w-md flex-col overflow-hidden rounded-lg border shadow-xl"
    data-daily-reward-sheet
  >
    <header class="flex items-center justify-between border-b px-4 py-3">
      <div>
        <h2 class="font-bold">Daily Reward</h2>
        <p class="text-muted-foreground text-xs">{summary.admin_local_date}</p>
      </div>
    </header>

    <div class="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
      {#if stale}
        <p class="rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-xs" role="status" data-daily-reward-stale>
          Showing saved information. Reconnect to refresh.
        </p>
      {/if}
      {#if monthError}
        <p class="text-muted-foreground text-xs" role="status">{monthError}</p>
      {/if}
      {#if claimError}
        <p class="text-destructive text-xs" role="alert" aria-live="assertive">{claimError}</p>
      {/if}

      <section aria-label="Daily Reward calendar">
        <div class="mb-2 flex items-center justify-between gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label="Previous month"
            disabled={selectedIndex < 0 || selectedIndex >= summary.calendar_months.length - 1}
            onclick={() => onSelectMonth(summary.calendar_months[selectedIndex + 1].earning_month)}
            data-previous-month
          >
            ‹
          </Button>
          <h3 class="font-semibold" data-selected-month={selectedMonth}>
            {formatAttendanceMonth(selectedMonth)}
          </h3>
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label="Next month"
            disabled={selectedIndex <= 0}
            onclick={() => onSelectMonth(summary.calendar_months[selectedIndex - 1].earning_month)}
            data-next-month
          >
            ›
          </Button>
        </div>

        {#if selectedSummary}
          <div class="text-muted-foreground mb-2 space-y-1 text-center text-xs" data-daily-reward-month-summary>
            <p>
              {selectedSummary.check_in_count} Daily Reward day{selectedSummary.check_in_count === 1 ? "" : "s"} earned
              · {formatProseDuration(selectedSummary.earned_seconds)} earned
            </p>
            {#if selectedSummary.claimable_seconds > 0}
              <p>{formatProseDuration(selectedSummary.claimable_seconds)} ready to claim</p>
            {/if}
            {#if claimTiming(selectedSummary)}
              <p>{claimTiming(selectedSummary)}</p>
            {/if}
            {#if selectedSummary.claim_count > 0 && selectedSummary.claimable_seconds > 0}
              <p role="note">More Daily Reward time is ready after an earlier claim.</p>
            {/if}
          </div>
        {/if}

        {#if monthLoading}
          <p class="text-muted-foreground py-8 text-center text-sm" role="status">Loading Daily Reward days…</p>
        {:else if monthDetails}
          <div class="grid grid-cols-7 gap-1 text-center" data-daily-reward-calendar>
            {#each ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"] as weekday}
              <span class="text-muted-foreground py-1 text-[0.65rem] font-semibold" aria-hidden="true">{weekday}</span>
            {/each}
            {#each calendarCells as day, index (`${day?.earning_date ?? 'blank'}-${index}`)}
              {#if day}
                <div
                  class="flex min-h-10 flex-col items-center justify-center rounded-md border text-[0.65rem] {day.status ===
                  'checked_in'
                    ? 'border-emerald-500/40 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300'
                    : day.status === 'missed'
                      ? 'border-muted bg-muted/50 text-muted-foreground'
                      : day.status === 'ineligible'
                        ? 'border-muted/30 text-muted-foreground'
                        : day.status === 'today_progress'
                          ? 'border-primary/50 bg-primary/10 text-primary'
                          : 'border-transparent text-muted-foreground'}"
                  data-daily-reward-day={day.earning_date}
                  data-day-status={day.status}
                  aria-label="{formatAttendanceDate(day.earning_date, summary.admin_local_date)}, {dayDescription(day)}"
                  title="{formatAttendanceDate(day.earning_date, summary.admin_local_date)} · {dayDescription(day)}"
                >
                  <span>{day.earning_date.slice(-2)}</span>
                  {#if dayMark(day)}
                    <span class="font-bold leading-none" aria-hidden="true">{dayMark(day)}</span>
                  {/if}
                </div>
              {:else}
                <span aria-hidden="true"></span>
              {/if}
            {/each}
          </div>
        {:else}
          <p class="text-muted-foreground py-8 text-center text-sm" role="status">
            {monthError ? "Saved Daily Reward days are not available for this month." : "Daily Reward days unavailable."}
          </p>
        {/if}
        {#if monthDetails && !monthLoading}
          <div class="text-muted-foreground mt-3 grid grid-cols-2 gap-1 text-[0.6rem]" role="group" aria-label="Daily Reward calendar legend" data-calendar-legend>
            <span>✓ Daily Reward earned</span>
            <span>× No Daily Reward</span>
            <span>— Not eligible</span>
            <span>% Today's progress</span>
            <span>Blank = future date</span>
          </div>
        {/if}
      </section>

      <section class="border-t pt-3" aria-label="Older Daily Reward claims">
        <h3 class="mb-2 text-xs font-bold tracking-wider uppercase">Older Daily Reward</h3>
        {#if olderClaimableMonths.length > 0}
          <div class="flex flex-col gap-2">
            {#each olderClaimableMonths as month (month.earning_month)}
              <div class="flex items-center justify-between gap-2 rounded-md border p-2" data-older-claim={month.earning_month}>
                <span class="min-w-0">
                  <span class="block text-xs font-semibold">{formatAttendanceMonth(month.earning_month)}</span>
                  <span class="text-muted-foreground block text-[0.65rem]">
                    {formatProseDuration(month.claimable_seconds)} ready to claim
                    {#if claimTiming(month)} · {claimTiming(month)}{/if}
                  </span>
                </span>
                <Button
                  type="button"
                  size="sm"
                  disabled={claimingMonth !== null}
                  data-claim-month={month.earning_month}
                  aria-label="Claim {formatProseDuration(month.claimable_seconds)} from {formatAttendanceMonth(month.earning_month)}"
                  onclick={() => onClaim(month.earning_month)}
                >
                  {claimingMonth === month.earning_month ? "Claiming…" : "Claim"}
                </Button>
              </div>
            {/each}
          </div>
        {:else}
          <p class="text-muted-foreground text-xs">No older Daily Reward is ready to claim.</p>
        {/if}
      </section>
    </div>
    <footer class="flex items-center justify-between gap-3 border-t p-3">
      <Button type="button" variant="outline" aria-label="Close Daily Reward" onclick={onClose}
        >Close</Button
      >
      <div class="flex min-w-0 flex-col items-end gap-1">
        {#if selectedSummary && selectedSummary.claimable_seconds <= 0}
          <span class="text-muted-foreground text-[0.6rem]" id="claim-disabled-reason" data-claim-disabled-reason role="status">
            {claimDisabledReason(selectedSummary)}
          </span>
        {/if}
        <Button
          type="button"
          size="sm"
          disabled={!selectedSummary || selectedSummary.claimable_seconds <= 0 || claimingMonth !== null}
          data-claim-selected-month={selectedMonth}
          aria-describedby={selectedSummary && selectedSummary.claimable_seconds <= 0 ? "claim-disabled-reason" : undefined}
          aria-label="Claim all {formatProseDuration(selectedSummary?.outstanding_seconds ?? 0)} from {formatAttendanceMonth(selectedMonth)}"
          onclick={() => onClaim(selectedMonth)}
        >
          {claimingMonth === selectedMonth
            ? "Claiming…"
            : `Claim all ${formatProseDuration(selectedSummary?.outstanding_seconds ?? 0)}`}
        </Button>
      </div>
    </footer>
  </section>
</WidgetOverlay>

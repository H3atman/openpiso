<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../api";
  import DailySalesChart from "../components/DailySalesChart.svelte";
  import ResponsiveRows from "../components/ResponsiveRows.svelte";
  import AttendancePager from "../components/AttendancePager.svelte";
  import NativeSelect from "../components/settings/NativeSelect.svelte";
  import MoveRightIcon from "@lucide/svelte/icons/move-right";
  import TriangleAlertIcon from "@lucide/svelte/icons/triangle-alert";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Skeleton } from "$lib/components/ui/skeleton";
  import {
    MONTH_NAMES,
    formatAttendanceDeadline,
    formatMoney,
    formatProseDuration,
    formatSalesDateRange,
    isoDate,
    lastDayOfMonth,
    monthDateRange,
    statusLabel,
  } from "../format";
  import type {
    AttendanceReport,
    AttendanceReportInput,
    AttendanceEarningMonthRow,
    AttendanceReportPeriod,
    DailySalesPoint,
    ReportBucket,
    SalesReportOverview,
  } from "../types";

  let salesReport = $state<SalesReportOverview | null>(null);
  let busy = $state(false);
  let error = $state("");
  let attendanceReport = $state<AttendanceReport | null>(null);
  let attendanceBusy = $state(false);
  let attendanceError = $state("");
  let attendancePeriod = $state<AttendanceReportPeriod>("month");
  let attendanceFromDate = $state("");
  let attendanceToDate = $state("");
  let attendanceRequest = 0;

  const today = new Date();
  const YEAR_OPTIONS = Array.from(
    { length: 5 },
    (_, index) => today.getFullYear() - 4 + index,
  );

  let chartYear = $state(today.getFullYear());
  let chartMonth = $state(today.getMonth() + 1);
  let chartPoints = $state<DailySalesPoint[]>([]);
  let chartBusy = $state(false);
  let chartError = $state("");
  let chartRequest = 0;

  /** Expands the sparse server buckets into one point per calendar day. */
  function dailySeries(
    year: number,
    month: number,
    buckets: readonly ReportBucket[],
  ): DailySalesPoint[] {
    const byDate = new Map(buckets.map((bucket) => [bucket.period_label, bucket]));
    return Array.from({ length: lastDayOfMonth(year, month) }, (_, index) => {
      const date = isoDate(year, month, index + 1);
      const bucket = byDate.get(date);
      return {
        date,
        coin_cents: bucket?.coin_sales_cents ?? 0,
        voucher_cents: bucket?.voucher_sales_cents ?? 0,
      };
    });
  }

  async function loadDailySales(year: number, month: number): Promise<void> {
    const request = ++chartRequest;
    chartBusy = true;
    chartError = "";
    const [fromDate, toDate] = monthDateRange(year, month);
    try {
      const summary = await api.getReportSummary("day", fromDate, toDate);
      if (request !== chartRequest) return;
      chartPoints = dailySeries(year, month, summary.buckets);
    } catch (caught) {
      if (request !== chartRequest) return;
      chartPoints = [];
      chartError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      if (request === chartRequest) chartBusy = false;
    }
  }

  let chartCaption = $derived(formatSalesDateRange(...monthDateRange(chartYear, chartMonth)));

  /**
   * The admin host owns the business date, so prefer the one the overview
   * reports and fall back to the browser clock only until it loads. Days after
   * it are unwritten, not zero-revenue, so they are left off the plot.
   */
  let businessToday = $derived(
    salesReport?.daily.to_date ??
      isoDate(today.getFullYear(), today.getMonth() + 1, today.getDate()),
  );
  let chartSeries = $derived(chartPoints.filter((point) => point.date <= businessToday));

  $effect(() => {
    void loadDailySales(chartYear, chartMonth);
  });

  function salesPeriodRange(period: SalesReportOverview["daily"] | undefined): string {
    return period ? formatSalesDateRange(period.from_date, period.to_date) : "";
  }

  /** Earning Month state pill colours; the label text always carries the state. */
  const STATE_STYLE: Record<AttendanceEarningMonthRow["state"], string> = {
    earning: "bg-secondary text-secondary-foreground",
    claimable: "bg-emerald-100 text-emerald-900 dark:bg-emerald-950 dark:text-emerald-100",
    claimed: "bg-sky-100 text-sky-900 dark:bg-sky-950 dark:text-sky-100",
    expired: "bg-amber-100 text-amber-900 dark:bg-amber-950 dark:text-amber-100",
    forfeited: "bg-red-100 text-red-900 dark:bg-red-950 dark:text-red-100",
  };

  /**
   * First column of the sideways-scrolling Attendance tables: pinned with an
   * opaque background so the row's identity stays visible while scrolling, and
   * a hairline on phones where the tables always overflow.
   */
  const STICKY_CELL =
    "sticky left-0 z-10 bg-card group-hover:bg-[color-mix(in_oklab,var(--muted)_50%,var(--card))] max-md:shadow-[inset_-1px_0_0_var(--border)]";

  const PERIOD_LABEL: Record<AttendanceReportPeriod, string> = {
    day: "Day",
    week: "Week",
    month: "Month",
  };

  /** Fade the contents as well as the background, and clear it at the end. */
  function scrollCue(node: HTMLDivElement) {
    function updateFade() {
      const remaining = Math.max(0, node.scrollWidth - node.clientWidth - node.scrollLeft);
      node.style.setProperty("--scroll-fade", `${Math.min(24, remaining)}px`);
    }

    const observer = new ResizeObserver(updateFade);
    observer.observe(node);
    if (node.firstElementChild) observer.observe(node.firstElementChild);
    node.addEventListener("scroll", updateFade, { passive: true });
    updateFade();

    return {
      destroy() {
        observer.disconnect();
        node.removeEventListener("scroll", updateFade);
      },
    };
  }

  function attendanceInput(
    overrides: Partial<AttendanceReportInput> = {},
  ): AttendanceReportInput {
    return {
      period: attendancePeriod,
      ...(attendanceFromDate ? { from_date: attendanceFromDate } : {}),
      ...(attendanceToDate ? { to_date: attendanceToDate } : {}),
      ...overrides,
    };
  }

  async function loadAttendanceReport(
    pagination: Partial<AttendanceReportInput> = {},
  ): Promise<void> {
    const request = ++attendanceRequest;
    attendanceBusy = true;
    attendanceError = "";
    try {
      const report = await api.getAttendanceReport(attendanceInput(pagination));
      if (request !== attendanceRequest) return;
      attendanceReport = report;
    } catch (caught) {
      if (request !== attendanceRequest) return;
      attendanceError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      if (request === attendanceRequest) attendanceBusy = false;
    }
  }

  function selectAttendancePeriod(value: string) {
    if (value !== "day" && value !== "week" && value !== "month") return;
    attendancePeriod = value;
    void loadAttendanceReport();
  }

  function nextAttendanceMonthPage(direction: -1 | 1) {
    if (!attendanceReport) return;
    const { month_page: months, account_page: accounts } = attendanceReport;
    const offset = Math.max(0, months.offset + direction * months.limit);
    void loadAttendanceReport({
      month_limit: months.limit,
      month_offset: offset,
      account_limit: accounts.limit,
      account_offset: accounts.offset,
    });
  }

  function nextAttendanceAccountPage(direction: -1 | 1) {
    if (!attendanceReport) return;
    const { month_page: months, account_page: accounts } = attendanceReport;
    const offset = Math.max(0, accounts.offset + direction * accounts.limit);
    void loadAttendanceReport({
      month_limit: months.limit,
      month_offset: months.offset,
      account_limit: accounts.limit,
      account_offset: offset,
    });
  }

  async function loadSalesReport(): Promise<void> {
    busy = true;
    error = "";
    salesReport = null;
    try {
      salesReport = await api.getSalesReportOverview();
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void loadSalesReport();
    void loadAttendanceReport();
  });
</script>

<!-- One sales KPI. Below 768px it is a compact label/value row inside a shared
     card (label and period left, value right); from 768px it is its own tile.
     Mirrors the Dashboard's "Today at a glance" stat. -->
{#snippet stat(label: string, value: string, detail: string)}
  <div
    class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 px-4 py-3 md:block md:rounded-lg md:border md:bg-card md:p-4"
  >
    <p class="text-sm text-muted-foreground">{label}</p>
    <p class="col-start-2 row-span-2 row-start-1 text-xl font-bold tabular-nums md:mt-1 md:text-2xl">
      {value}
    </p>
    <p class="text-xs text-muted-foreground md:mt-0.5">{detail}</p>
  </div>
{/snippet}

<!-- One Attendance figure inside a card. -->
{#snippet tile(label: string, value: string, extraClass = "")}
  <div class="rounded-md bg-muted/60 px-3 py-2.5 {extraClass}">
    <p class="text-xs text-muted-foreground">{label}</p>
    <p class="mt-0.5 text-lg font-semibold tabular-nums">{value}</p>
  </div>
{/snippet}

<!-- A right-aligned duration cell; zero values recede so non-zero ones scan. -->
{#snippet durationCell(seconds: number)}
  <td class="px-4 py-2.5 text-right tabular-nums {seconds === 0 ? 'text-muted-foreground' : ''}">
    {formatProseDuration(seconds)}
  </td>
{/snippet}

<!-- Phone-only: the Attendance tables are always wider than a phone. The
     edge fade (.scroll-cue) also covers wider screens when they overflow. -->
{#snippet scrollHint()}
  <p class="flex items-center gap-1.5 px-4 py-2 text-xs text-muted-foreground md:hidden" aria-hidden="true">
    <MoveRightIcon class="size-3.5 shrink-0" />
    Swipe sideways for more columns
  </p>
{/snippet}

{#snippet countCell(count: number)}
  <td class="px-4 py-2.5 text-right tabular-nums {count === 0 ? 'text-muted-foreground' : ''}">
    {count.toLocaleString()}
  </td>
{/snippet}

<div class="mx-auto w-full max-w-7xl space-y-6">
  <div>
    <h2 class="text-2xl font-bold tracking-tight">Sales Report</h2>
    <p class="text-sm text-muted-foreground">Cash sales per period and PC, then Attendance Time.</p>
  </div>

  {#if error}
    <div
      class="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
      role="alert"
    >
      Couldn't load the sales report: {error}
    </div>
  {/if}

  <section aria-labelledby="sales-summary-heading">
    <h3 id="sales-summary-heading" class="sr-only">Summary</h3>
    {#if salesReport}
      <div class="grid divide-y rounded-lg border bg-card md:grid-cols-2 md:gap-4 md:divide-y-0 md:border-0 md:bg-transparent xl:grid-cols-4">
        {@render stat("Daily Sales", formatMoney(salesReport.daily.cash_revenue_cents), salesPeriodRange(salesReport.daily))}
        {@render stat("Weekly Sales", formatMoney(salesReport.weekly.cash_revenue_cents), salesPeriodRange(salesReport.weekly))}
        {@render stat("Monthly Sales", formatMoney(salesReport.monthly.cash_revenue_cents), salesPeriodRange(salesReport.monthly))}
        {@render stat("Total Sales", formatMoney(salesReport.total.cash_revenue_cents), salesPeriodRange(salesReport.total))}
      </div>
    {:else if busy}
      <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4" role="status">
        <span class="sr-only">Loading sales report…</span>
        {#each [0, 1, 2, 3] as tileIndex (tileIndex)}
          <Skeleton class="h-[5.5rem] rounded-lg" />
        {/each}
      </div>
    {/if}
  </section>

  <section class="rounded-lg border bg-card" aria-labelledby="daily-trend-heading">
    <div class="flex flex-wrap items-center justify-between gap-3 border-b px-4 py-3">
      <div class="min-w-0">
        <h3 id="daily-trend-heading" class="font-semibold">Daily Sales Trend</h3>
        <p class="text-xs text-muted-foreground">{chartCaption}</p>
      </div>
      <div class="grid w-full grid-cols-2 gap-2 md:flex md:w-auto">
        <label for="sales-chart-month" class="sr-only">Month</label>
        <NativeSelect id="sales-chart-month" class="md:w-36" bind:value={chartMonth}>
          {#each MONTH_NAMES as name, index}
            <option value={index + 1}>{name}</option>
          {/each}
        </NativeSelect>
        <label for="sales-chart-year" class="sr-only">Year</label>
        <NativeSelect id="sales-chart-year" class="md:w-24" bind:value={chartYear}>
          {#each YEAR_OPTIONS as year}
            <option value={year}>{year}</option>
          {/each}
        </NativeSelect>
      </div>
    </div>

    <div class="p-4">
      {#if chartError}
        <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive" role="alert">
          Couldn't load daily sales: {chartError}
        </div>
      {:else if chartBusy && chartPoints.length === 0}
        <div role="status">
          <span class="sr-only">Loading daily sales…</span>
          <Skeleton class="h-[12.5rem] md:h-[15rem]" />
        </div>
      {:else}
        <DailySalesChart points={chartSeries} caption={chartCaption} />
      {/if}
    </div>
  </section>

  {#if salesReport}
    <section class="rounded-lg border bg-card" aria-labelledby="pc-breakdown-heading">
      <div class="flex items-baseline justify-between gap-3 border-b px-4 py-3">
        <h3 id="pc-breakdown-heading" class="font-semibold">PC Breakdown</h3>
        {#if salesReport.pcs.length > 0}
          <p class="text-xs text-muted-foreground">
            {salesReport.pcs.length} {salesReport.pcs.length === 1 ? "PC" : "PCs"}
          </p>
        {/if}
      </div>

      {#if salesReport.pcs.length > 0}
        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <caption class="sr-only">Sales breakdown by PC</caption>
            <thead>
              <tr class="border-b text-left text-xs text-muted-foreground max-md:hidden">
                <th scope="col" class="px-4 py-2 font-medium">PC</th>
                <th scope="col" class="px-4 py-2 text-right font-medium">
                  <span class="block">Daily Sales</span>
                  <span class="block font-normal">{salesPeriodRange(salesReport.daily)}</span>
                </th>
                <th scope="col" class="px-4 py-2 text-right font-medium">
                  <span class="block">Weekly Sales</span>
                  <span class="block font-normal">{salesPeriodRange(salesReport.weekly)}</span>
                </th>
                <th scope="col" class="px-4 py-2 text-right font-medium">
                  <span class="block">Monthly Sales</span>
                  <span class="block font-normal">{salesPeriodRange(salesReport.monthly)}</span>
                </th>
                <th scope="col" class="px-4 py-2 text-right font-medium">
                  <span class="block">Total Sales</span>
                  <span class="block font-normal">{salesPeriodRange(salesReport.total)}</span>
                </th>
              </tr>
            </thead>
            <tbody class="divide-y">
              <ResponsiveRows rows={salesReport.pcs}>
                {#snippet row(pc)}
                  <tr class="hover:bg-muted/50">
                    <th scope="row" class="px-4 py-2.5 text-left font-medium">{pc.name}</th>
                    {#each [pc.daily_sales_cents, pc.weekly_sales_cents, pc.monthly_sales_cents] as cents}
                      <td class="px-4 py-2.5 text-right tabular-nums {cents === 0 ? 'text-muted-foreground' : ''}">
                        {formatMoney(cents)}
                      </td>
                    {/each}
                    <td
                      class="px-4 py-2.5 text-right font-semibold tabular-nums {pc.total_sales_cents === 0 ? 'text-muted-foreground' : ''}"
                    >
                      {formatMoney(pc.total_sales_cents)}
                    </td>
                  </tr>
                {/snippet}
                {#snippet card(pc)}
                  {@const figures = [
                    { label: "Daily Sales", cents: pc.daily_sales_cents },
                    { label: "Weekly Sales", cents: pc.weekly_sales_cents },
                    { label: "Monthly Sales", cents: pc.monthly_sales_cents },
                    { label: "Total Sales", cents: pc.total_sales_cents },
                  ]}
                  <tr>
                    <td colspan="5" class="px-4 py-3">
                      <article aria-label={pc.name}>
                        <!-- `wrap-anywhere`, not `break-words`: this card is a
                             single colspan cell in an auto-layout table, and
                             only `overflow-wrap: anywhere` lowers the cell's
                             min-content width enough for the table to stop
                             widening past the phone viewport. -->
                        <h4 class="font-semibold max-md:wrap-anywhere">{pc.name}</h4>
                        <dl class="mt-2 space-y-1.5">
                          {#each figures as figure}
                            <div class="flex items-baseline justify-between gap-3">
                              <dt class="min-w-0 text-sm text-muted-foreground">
                                {figure.label}
                              </dt>
                              <dd
                                class="tabular-nums {figure.label === 'Total Sales' ? 'font-semibold' : 'font-medium'} {figure.cents === 0 ? 'text-muted-foreground' : ''}"
                              >
                                {formatMoney(figure.cents)}
                              </dd>
                            </div>
                          {/each}
                        </dl>
                      </article>
                    </td>
                  </tr>
                {/snippet}
              </ResponsiveRows>
            </tbody>
          </table>
        </div>
      {:else}
        <p class="px-4 py-10 text-center text-sm text-muted-foreground">
          No PCs registered. Add one in
          <a class="font-medium text-foreground underline underline-offset-2 hover:no-underline" href="/admin/settings/pcs">Settings → PCs</a>.
        </p>
      {/if}
    </section>
  {/if}

  <section class="space-y-4 border-t pt-6" aria-labelledby="attendance-report-heading">
    <div class="flex flex-wrap items-end justify-between gap-x-4 gap-y-2">
      <div class="min-w-0">
        <h3 id="attendance-report-heading" class="text-xl font-bold tracking-tight">Attendance Report</h3>
        <p class="text-sm text-muted-foreground">
          {#if attendanceReport}
            Business date {attendanceReport.admin_local_date} · Earning Month {attendanceReport.admin_local_month}
          {:else}
            Dates follow the Admin host's business date.
          {/if}
        </p>
      </div>
      {#if attendanceReport?.economics_advisory}
        {@const bonus = attendanceReport.economics_advisory.effective_bonus}
        <p
          class="rounded-md border px-2.5 py-1 text-xs {bonus.kind === 'infinite'
            ? 'border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-100'
            : 'text-muted-foreground'}"
        >
          Current Effective Bonus:
          <span class="font-semibold text-foreground tabular-nums">
            {#if bonus.kind === "infinite"}
              Infinite (unsafe)
            {:else}
              {(bonus.basis_points / 100).toFixed(2)}%
            {/if}
          </span>
          {#if bonus.kind !== "infinite"}(informational){/if}
        </p>
      {/if}
    </div>

    {#if attendanceReport?.clock_regression_advisory}
      <div
        class="flex gap-3 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-900 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-100"
        role="status"
      >
        <TriangleAlertIcon class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        <p class="min-w-0">
          <span class="font-semibold">Attendance clock regression detected.</span>
          The durable clock floor remains authoritative.
        </p>
      </div>
    {/if}

    {#if attendanceError}
      <div
        class="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
        role="alert"
      >
        Couldn't load the Attendance report: {attendanceError}
      </div>
    {/if}

    {#if attendanceBusy && !attendanceReport}
      <div class="space-y-4" role="status">
        <span class="sr-only">Loading Attendance report…</span>
        <Skeleton class="h-32 rounded-lg" />
        <Skeleton class="h-64 rounded-lg" />
      </div>
    {/if}

    {#if attendanceReport}
      <!-- Range-independent: the current liability, before the range filters. -->
      <section class="rounded-lg border bg-card" aria-labelledby="attendance-liability-heading">
        <div class="gap-6 p-4 md:flex md:items-center md:justify-between">
          <div class="min-w-0">
            <h4 id="attendance-liability-heading" class="font-semibold">Outstanding Attendance Time</h4>
            <p class="text-xs text-muted-foreground">Current balance across all Earning Months. The flow range does not change it.</p>
          </div>
          <div class="mt-3 grid shrink-0 grid-cols-2 gap-3 md:mt-0 md:w-[26rem]">
          {@render tile("Outstanding", formatProseDuration(attendanceReport.liability.outstanding_seconds))}
          {@render tile("Claimable", formatProseDuration(attendanceReport.liability.claimable_seconds))}
          </div>
        </div>
      </section>

      <section class="rounded-lg border bg-card" aria-labelledby="attendance-flow-heading">
        <div class="border-b px-4 py-3">
          <h4 id="attendance-flow-heading" class="font-semibold">Attendance flow</h4>
          <p class="text-xs text-muted-foreground">
            {formatSalesDateRange(attendanceReport.from_date, attendanceReport.to_date)} · by {PERIOD_LABEL[attendanceReport.period].toLowerCase()}
          </p>
        </div>

        <div class="space-y-4 p-4">
          <div
            class="grid grid-cols-2 gap-3 md:flex md:flex-wrap md:items-end"
            role="group"
            aria-label="Attendance filters"
          >
            <div class="col-span-2 md:w-36">
              <label for="attendance-period" class="mb-1.5 block text-xs font-medium text-muted-foreground">Attendance period</label>
              <NativeSelect
                id="attendance-period"
                value={attendancePeriod}
                disabled={attendanceBusy}
                onchange={(event) => selectAttendancePeriod(event.currentTarget.value)}
              >
                <option value="day">Day</option>
                <option value="week">Week</option>
                <option value="month">Month</option>
              </NativeSelect>
            </div>
            <div class="min-w-0 md:w-40">
              <label for="attendance-from-date" class="mb-1.5 block text-xs font-medium text-muted-foreground">From</label>
              <Input id="attendance-from-date" type="date" class="dark:scheme-dark" max={attendanceToDate || undefined} bind:value={attendanceFromDate} />
            </div>
            <div class="min-w-0 md:w-40">
              <label for="attendance-to-date" class="mb-1.5 block text-xs font-medium text-muted-foreground">To</label>
              <Input id="attendance-to-date" type="date" class="dark:scheme-dark" min={attendanceFromDate || undefined} bind:value={attendanceToDate} />
            </div>
            <Button class="col-span-2" disabled={attendanceBusy} onclick={() => loadAttendanceReport()}>
              {attendanceBusy ? "Applying…" : "Apply Attendance filters"}
            </Button>
          </div>

          <div class="grid grid-cols-2 gap-3 lg:grid-cols-5">
            {@render tile("Attendance Check-Ins", attendanceReport.flow_totals.check_ins.toLocaleString(), "col-span-2 lg:col-span-1")}
            {@render tile("Earned", formatProseDuration(attendanceReport.flow_totals.earned_seconds))}
            {@render tile("Claimed", formatProseDuration(attendanceReport.flow_totals.claimed_seconds))}
            {@render tile("Expired", formatProseDuration(attendanceReport.flow_totals.expired_seconds))}
            {@render tile("Forfeited", formatProseDuration(attendanceReport.flow_totals.forfeited_seconds))}
          </div>
        </div>

        <h5 class="border-t px-4 pt-3 pb-1 text-sm font-medium">Flow breakdown</h5>
        {#if attendanceReport.flow_buckets.length > 0}
          {@render scrollHint()}
          <!-- Focusable so keyboard users can scroll it (axe scrollable-region-focusable). -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div
            use:scrollCue
            class="scroll-cue overflow-x-auto focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
            role="region"
            aria-label="Attendance flow breakdown table"
            tabindex="0"
          >
            <table class="attendance-flow-table w-full text-left text-sm whitespace-nowrap">
              <caption class="sr-only">Attendance flow breakdown by {attendanceReport.period}</caption>
              <colgroup>
                <col class="attendance-identity" />
                <col class="attendance-check-ins" />
                <col span="4" />
              </colgroup>
              <thead class="border-b text-xs text-muted-foreground">
                <tr>
                  <th scope="col" class="px-4 py-2 font-medium {STICKY_CELL}">{attendanceReport.period === "week" ? "Week of" : PERIOD_LABEL[attendanceReport.period]}</th>
                  <th scope="col" class="check-ins-heading px-4 py-2 text-right font-medium">Attendance Check-Ins</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Earned</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Claimed</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Expired</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Forfeited</th>
                </tr>
              </thead>
              <tbody class="divide-y">
                {#each attendanceReport.flow_buckets as bucket (bucket.period_label)}
                  <tr class="group hover:bg-muted/50">
                    <th scope="row" class="px-4 py-2.5 font-medium tabular-nums {STICKY_CELL}">{bucket.period_label}</th>
                    {@render countCell(bucket.check_ins)}
                    {@render durationCell(bucket.earned_seconds)}
                    {@render durationCell(bucket.claimed_seconds)}
                    {@render durationCell(bucket.expired_seconds)}
                    {@render durationCell(bucket.forfeited_seconds)}
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {:else}
          <p class="px-4 py-8 text-center text-sm text-muted-foreground">No Attendance flows in this range.</p>
        {/if}
      </section>

      <section class="rounded-lg border bg-card" aria-labelledby="attendance-month-heading">
        <div class="border-b px-4 py-3">
          <h4 id="attendance-month-heading" class="font-semibold">Earning Month exposure</h4>
          <p class="text-xs text-muted-foreground">Attendance Time per Account and Earning Month. The flow range does not change it.</p>
        </div>
        {#if attendanceReport.month_page.rows.length > 0}
          {@render scrollHint()}
          <!-- Focusable so keyboard users can scroll it (axe scrollable-region-focusable). -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div
            use:scrollCue
            class="scroll-cue overflow-x-auto focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
            role="region"
            aria-label="Earning Month exposure table"
            tabindex="0"
          >
            <table class="attendance-month-table w-full text-left text-sm whitespace-nowrap">
              <caption class="sr-only">Attendance exposure by Earning Month</caption>
              <colgroup>
                <col class="month-account" />
                <col class="month-period" />
                <col class="month-state" />
                <col span="5" />
                <col class="month-deadline" />
              </colgroup>
              <thead class="border-b text-xs text-muted-foreground">
                <tr>
                  <th scope="col" class="px-4 py-2 font-medium {STICKY_CELL}">Account</th>
                  <th scope="col" class="px-4 py-2 font-medium">Earning Month</th>
                  <th scope="col" class="px-4 py-2 font-medium">State</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Earned</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Claimed</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Outstanding</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Expired</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Forfeited</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Claim deadline (inclusive)</th>
                </tr>
              </thead>
              <tbody class="divide-y">
                {#each attendanceReport.month_page.rows as row (`${row.account_id}-${row.earning_month}`)}
                  <tr class="group hover:bg-muted/50">
                    <th scope="row" class="px-4 py-2.5 font-medium {STICKY_CELL}">{row.username}</th>
                    <td class="px-4 py-2.5 tabular-nums">{row.earning_month}</td>
                    <td class="px-4 py-2.5">
                      <Badge class={STATE_STYLE[row.state]}>{statusLabel(row.state)}</Badge>
                    </td>
                    {@render durationCell(row.earned_seconds)}
                    {@render durationCell(row.claimed_seconds)}
                    {@render durationCell(row.outstanding_seconds)}
                    {@render durationCell(row.expired_seconds)}
                    {@render durationCell(row.forfeited_seconds)}
                    <td class="px-4 py-2.5 text-right">{formatAttendanceDeadline(row.claim_deadline_local_date)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {:else}
          <p class="px-4 py-8 text-center text-sm text-muted-foreground">No Earning Month rows.</p>
        {/if}
        <AttendancePager
          page={attendanceReport.month_page}
          pageName="Earning Month"
          busy={attendanceBusy}
          onPrevious={() => nextAttendanceMonthPage(-1)}
          onNext={() => nextAttendanceMonthPage(1)}
        />
      </section>

      <section class="rounded-lg border bg-card" aria-labelledby="attendance-account-heading">
        <div class="border-b px-4 py-3">
          <h4 id="attendance-account-heading" class="font-semibold">Account exposure</h4>
          <p class="text-xs text-muted-foreground">Attendance Time per Account across all Earning Months. The flow range does not change it.</p>
        </div>
        {#if attendanceReport.account_page.rows.length > 0}
          {@render scrollHint()}
          <!-- Focusable so keyboard users can scroll it (axe scrollable-region-focusable). -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div
            use:scrollCue
            class="scroll-cue overflow-x-auto focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
            role="region"
            aria-label="Account exposure table"
            tabindex="0"
          >
            <table class="attendance-account-table w-full text-left text-sm whitespace-nowrap">
              <caption class="sr-only">Attendance exposure by Account</caption>
              <colgroup>
                <col class="attendance-identity" />
                <col class="attendance-check-ins" />
                <col span="5" />
              </colgroup>
              <thead class="border-b text-xs text-muted-foreground">
                <tr>
                  <th scope="col" class="px-4 py-2 font-medium {STICKY_CELL}">Account</th>
                  <th scope="col" class="check-ins-heading px-4 py-2 text-right font-medium">Attendance Check-Ins</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Earned</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Claimed</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Outstanding</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Claimable</th>
                  <th scope="col" class="px-4 py-2 text-right font-medium">Expired</th>
                </tr>
              </thead>
              <tbody class="divide-y">
                {#each attendanceReport.account_page.rows as row (row.account_id)}
                  <tr class="group hover:bg-muted/50">
                    <th scope="row" class="px-4 py-2.5 font-medium {STICKY_CELL}">{row.username}</th>
                    {@render countCell(row.check_ins)}
                    {@render durationCell(row.earned_seconds)}
                    {@render durationCell(row.claimed_seconds)}
                    {@render durationCell(row.outstanding_seconds)}
                    {@render durationCell(row.claimable_seconds)}
                    {@render durationCell(row.expired_seconds)}
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {:else}
          <p class="px-4 py-8 text-center text-sm text-muted-foreground">No Account rows.</p>
        {/if}
        <AttendancePager
          page={attendanceReport.account_page}
          pageName="Account"
          busy={attendanceBusy}
          onPrevious={() => nextAttendanceAccountPage(-1)}
          onNext={() => nextAttendanceAccountPage(1)}
        />
      </section>
    {/if}
  </section>
</div>

<style>
  .scroll-cue {
    container-type: inline-size;
    background: linear-gradient(to left, color-mix(in oklab, var(--foreground) 18%, var(--card)), transparent)
      right / var(--scroll-fade, 0px) 100% no-repeat;
    mask-image: linear-gradient(to right, black calc(100% - var(--scroll-fade, 0px)), transparent);
  }

  @media (width < 768px) {
    /* Whole columns fill the initial viewport, including at the 360px floor.
       Later columns keep enough room for prose durations when scrolled. */
    .attendance-flow-table,
    .attendance-month-table,
    .attendance-account-table {
      table-layout: fixed;
    }

    .attendance-flow-table {
      width: calc(100cqw + 36rem);
    }

    .attendance-account-table {
      width: calc(100cqw + 50rem);
    }

    .attendance-identity { width: 40cqw; }
    .attendance-check-ins { width: 60cqw; }

    .attendance-month-table {
      width: calc(100cqw + 58rem);
    }

    .month-account { width: 38cqw; }
    .month-period { width: 32cqw; }
    .month-state { width: 30cqw; }
    .month-deadline { width: 13rem; }

    .attendance-flow-table th,
    .attendance-flow-table td,
    .attendance-month-table th,
    .attendance-month-table td,
    .attendance-account-table th,
    .attendance-account-table td {
      padding-inline: 0.75rem;
      white-space: normal;
      overflow-wrap: anywhere;
    }

    .attendance-flow-table .check-ins-heading,
    .attendance-account-table .check-ins-heading {
      white-space: nowrap;
    }

    /* Keep the resting view's last column clear of the edge fade. */
    .attendance-flow-table th:nth-child(2),
    .attendance-flow-table td:nth-child(2),
    .attendance-account-table th:nth-child(2),
    .attendance-account-table td:nth-child(2) {
      padding-right: 1.75rem;
    }
  }
</style>

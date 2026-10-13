<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../api";
  import DailySalesChart from "../components/DailySalesChart.svelte";
  import ResponsiveRows from "../components/ResponsiveRows.svelte";
  import AttendancePager from "../components/AttendancePager.svelte";
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
    const byDate = new Map(
      buckets.map((bucket) => [bucket.period_label, bucket.cash_revenue_cents]),
    );
    return Array.from({ length: lastDayOfMonth(year, month) }, (_, index) => {
      const date = isoDate(year, month, index + 1);
      return { date, cents: byDate.get(date) ?? 0 };
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

<div class="space-y-6">
  <h2 class="text-2xl font-bold tracking-tight">Sales Report</h2>

  {#if error}
    <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive" role="alert">
      {error}
    </div>
  {/if}

  {#if busy && !salesReport}
    <p class="text-sm text-muted-foreground" aria-live="polite">Loading sales report…</p>
  {/if}

  <section class="rounded-lg border bg-card p-6" aria-labelledby="daily-trend-heading">
    <div class="mb-4 flex flex-wrap items-end justify-between gap-4">
      <div>
        <h3 id="daily-trend-heading" class="text-lg font-semibold">Daily Sales Trend</h3>
        <p class="mt-1 text-xs text-muted-foreground">{chartCaption}</p>
      </div>

      <div class="flex flex-wrap items-end gap-3">
        <div>
          <label for="sales-chart-month" class="text-xs text-muted-foreground">Month</label>
          <select
            id="sales-chart-month"
            class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm"
            bind:value={chartMonth}
          >
            {#each MONTH_NAMES as name, index}
              <option value={index + 1}>{name}</option>
            {/each}
          </select>
        </div>

        <div>
          <label for="sales-chart-year" class="text-xs text-muted-foreground">Year</label>
          <select
            id="sales-chart-year"
            class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm"
            bind:value={chartYear}
          >
            {#each YEAR_OPTIONS as year}
              <option value={year}>{year}</option>
            {/each}
          </select>
        </div>
      </div>
    </div>

    {#if chartError}
      <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive" role="alert">
        {chartError}
      </div>
    {:else if chartBusy && chartPoints.length === 0}
      <p class="text-sm text-muted-foreground" aria-live="polite">Loading daily sales…</p>
    {:else}
      <DailySalesChart points={chartSeries} caption={chartCaption} />
    {/if}
  </section>

  {#if salesReport}
    <section class="rounded-lg border bg-card p-6" aria-labelledby="sales-summary-heading">
      <h3 id="sales-summary-heading" class="mb-4 text-lg font-semibold">Summary</h3>

      <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Daily Sales</p>
          <p class="mt-1 text-xs text-muted-foreground">
            {formatSalesDateRange(salesReport.daily.from_date, salesReport.daily.to_date)}
          </p>
          <p class="mt-2 text-xl font-bold">{formatMoney(salesReport.daily.cash_revenue_cents)}</p>
        </div>

        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Weekly Sales</p>
          <p class="mt-1 text-xs text-muted-foreground">
            {formatSalesDateRange(salesReport.weekly.from_date, salesReport.weekly.to_date)}
          </p>
          <p class="mt-2 text-xl font-bold">{formatMoney(salesReport.weekly.cash_revenue_cents)}</p>
        </div>

        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Monthly Sales</p>
          <p class="mt-1 text-xs text-muted-foreground">
            {formatSalesDateRange(salesReport.monthly.from_date, salesReport.monthly.to_date)}
          </p>
          <p class="mt-2 text-xl font-bold">{formatMoney(salesReport.monthly.cash_revenue_cents)}</p>
        </div>

        <div class="rounded-md bg-muted p-4">
          <p class="text-xs text-muted-foreground">Total Sales</p>
          <p class="mt-1 text-xs text-muted-foreground">
            {formatSalesDateRange(salesReport.total.from_date, salesReport.total.to_date)}
          </p>
          <p class="mt-2 text-xl font-bold">{formatMoney(salesReport.total.cash_revenue_cents)}</p>
        </div>
      </div>
    </section>

    <section class="rounded-lg border bg-card p-6" aria-labelledby="pc-breakdown-heading">
      <h3 id="pc-breakdown-heading" class="mb-4 text-lg font-semibold">PC Breakdown</h3>

      {#if salesReport.pcs.length > 0}
        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <caption class="sr-only">Sales breakdown by PC</caption>
            <thead>
              <tr class="border-b text-left text-muted-foreground max-md:hidden">
                <th scope="col" class="px-4 py-3 font-medium">PC #</th>
                <th scope="col" class="px-4 py-3 font-medium">
                  <span class="block">Daily Sales</span>
                  <span class="mt-1 block text-xs font-normal">
                    {formatSalesDateRange(salesReport.daily.from_date, salesReport.daily.to_date)}
                  </span>
                </th>
                <th scope="col" class="px-4 py-3 font-medium">
                  <span class="block">Weekly Sales</span>
                  <span class="mt-1 block text-xs font-normal">
                    {formatSalesDateRange(salesReport.weekly.from_date, salesReport.weekly.to_date)}
                  </span>
                </th>
                <th scope="col" class="px-4 py-3 font-medium">
                  <span class="block">Monthly Sales</span>
                  <span class="mt-1 block text-xs font-normal">
                    {formatSalesDateRange(salesReport.monthly.from_date, salesReport.monthly.to_date)}
                  </span>
                </th>
                <th scope="col" class="px-4 py-3 font-medium">
                  <span class="block">Total Sales</span>
                  <span class="mt-1 block text-xs font-normal">
                    {formatSalesDateRange(salesReport.total.from_date, salesReport.total.to_date)}
                  </span>
                </th>
              </tr>
            </thead>
            <tbody class="divide-y">
              <ResponsiveRows rows={salesReport.pcs}>
                {#snippet row(pc)}
                  <tr class="hover:bg-muted/50">
                    <td class="px-4 py-3 font-medium">{pc.name}</td>
                    <td class="px-4 py-3 font-mono">{formatMoney(pc.daily_sales_cents)}</td>
                    <td class="px-4 py-3 font-mono">{formatMoney(pc.weekly_sales_cents)}</td>
                    <td class="px-4 py-3 font-mono">{formatMoney(pc.monthly_sales_cents)}</td>
                    <td class="px-4 py-3 font-mono">{formatMoney(pc.total_sales_cents)}</td>
                  </tr>
                {/snippet}
                {#snippet card(pc)}
                  <tr>
                    <td colspan="5" class="p-2">
                      <article class="rounded-md border bg-muted/30 p-4" aria-label={pc.name}>
                        <!-- `wrap-anywhere`, not `break-words`: this card is a
                             single colspan cell in an auto-layout table, and
                             only `overflow-wrap: anywhere` lowers the cell's
                             min-content width enough for the table to stop
                             widening past the phone viewport. -->
                        <h4 class="font-semibold max-md:wrap-anywhere">{pc.name}</h4>
                        <dl class="mt-4 grid gap-3 sm:grid-cols-2">
                          <div>
                            <dt class="text-xs text-muted-foreground">Daily Sales</dt>
                            <dd class="mt-1 flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
                              <span class="text-xs text-muted-foreground">
                                {salesPeriodRange(salesReport?.daily)}
                              </span>
                              <span class="font-mono font-medium">{formatMoney(pc.daily_sales_cents)}</span>
                            </dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground">Weekly Sales</dt>
                            <dd class="mt-1 flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
                              <span class="text-xs text-muted-foreground">
                                {salesPeriodRange(salesReport?.weekly)}
                              </span>
                              <span class="font-mono font-medium">{formatMoney(pc.weekly_sales_cents)}</span>
                            </dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground">Monthly Sales</dt>
                            <dd class="mt-1 flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
                              <span class="text-xs text-muted-foreground">
                                {salesPeriodRange(salesReport?.monthly)}
                              </span>
                              <span class="font-mono font-medium">{formatMoney(pc.monthly_sales_cents)}</span>
                            </dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground">Total Sales</dt>
                            <dd class="mt-1 flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
                              <span class="text-xs text-muted-foreground">
                                {salesPeriodRange(salesReport?.total)}
                              </span>
                              <span class="font-mono font-medium">{formatMoney(pc.total_sales_cents)}</span>
                            </dd>
                          </div>
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
        <p class="text-sm text-muted-foreground">No PCs registered.</p>
      {/if}
    </section>
  {/if}

  <section class="space-y-4" aria-labelledby="attendance-report-heading">
    <div class="rounded-lg border bg-card p-6">
      <div class="mb-4">
        <h2 id="attendance-report-heading" class="text-xl font-semibold">Attendance Report</h2>
        <p class="mt-1 text-sm text-muted-foreground">
          Attendance flow uses the Admin host's business date. Outstanding Attendance Time is shown separately from the selected range.
        </p>
      </div>

      <div class="flex flex-wrap items-end gap-3">
        <div>
          <label for="attendance-period" class="block text-xs text-muted-foreground">Attendance period</label>
          <select
            id="attendance-period"
            class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm"
            value={attendancePeriod}
            disabled={attendanceBusy}
            onchange={(event) => selectAttendancePeriod(event.currentTarget.value)}
          >
            <option value="day">Day</option>
            <option value="week">Week</option>
            <option value="month">Month</option>
          </select>
        </div>
        <div>
          <label for="attendance-from-date" class="block text-xs text-muted-foreground">Attendance from</label>
          <input
            id="attendance-from-date"
            type="date"
            class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm"
            bind:value={attendanceFromDate}
          />
        </div>
        <div>
          <label for="attendance-to-date" class="block text-xs text-muted-foreground">Attendance to</label>
          <input
            id="attendance-to-date"
            type="date"
            class="mt-1 block rounded-md border bg-background px-3 py-2 text-sm"
            bind:value={attendanceToDate}
          />
        </div>
        <button
          class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          type="button"
          disabled={attendanceBusy}
          onclick={() => loadAttendanceReport()}
        >
          {attendanceBusy ? "Loading…" : "Apply Attendance filters"}
        </button>
      </div>
      {#if attendanceError}
        <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive" role="alert">
          {attendanceError}
        </div>
      {/if}
    </div>

    {#if attendanceBusy && !attendanceReport}
      <p class="text-sm text-muted-foreground" aria-live="polite">Loading Attendance report…</p>
    {/if}

    {#if attendanceReport}
      {#if attendanceReport.clock_regression_advisory}
        <div class="rounded-md bg-muted p-3 text-sm" role="status">
          Attendance clock regression detected. The durable clock floor remains authoritative.
        </div>
      {/if}

      {#if attendanceReport.economics_advisory}
        <p class="text-sm text-muted-foreground">
          Current Effective Bonus:
          {#if attendanceReport.economics_advisory.effective_bonus.kind === "infinite"}
            Infinite (unsafe)
          {:else}
            {(attendanceReport.economics_advisory.effective_bonus.basis_points / 100).toFixed(2)}% (informational)
          {/if}
        </p>
      {/if}

      <section class="rounded-lg border bg-card p-6" aria-labelledby="attendance-liability-heading">
        <h3 id="attendance-liability-heading" class="text-lg font-semibold">Outstanding Attendance Time (ignores selected date range)</h3>
        <div class="mt-3 grid gap-3 sm:grid-cols-2">
          <p class="rounded-md bg-muted p-3 text-sm">Outstanding: {formatProseDuration(attendanceReport.liability.outstanding_seconds)}</p>
          <p class="rounded-md bg-muted p-3 text-sm">Claimable: {formatProseDuration(attendanceReport.liability.claimable_seconds)}</p>
        </div>
        <p class="mt-2 text-xs text-muted-foreground">
          Selected flow range: {formatSalesDateRange(attendanceReport.from_date, attendanceReport.to_date)}
        </p>
      </section>

      <section class="rounded-lg border bg-card p-6" aria-labelledby="attendance-flow-heading">
        <div class="mb-4">
          <h3 id="attendance-flow-heading" class="text-lg font-semibold">Flow totals</h3>
          <p class="mt-1 text-xs text-muted-foreground">
            {attendanceReport.admin_local_date} Admin business date · {attendanceReport.admin_local_month} Earning Month
          </p>
        </div>
        <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
          <p class="rounded-md bg-muted p-3 text-sm">Attendance Check-Ins: {attendanceReport.flow_totals.check_ins.toLocaleString()}</p>
          <p class="rounded-md bg-muted p-3 text-sm">Earned: {formatProseDuration(attendanceReport.flow_totals.earned_seconds)}</p>
          <p class="rounded-md bg-muted p-3 text-sm">Claimed: {formatProseDuration(attendanceReport.flow_totals.claimed_seconds)}</p>
          <p class="rounded-md bg-muted p-3 text-sm">Expired: {formatProseDuration(attendanceReport.flow_totals.expired_seconds)}</p>
          <p class="rounded-md bg-muted p-3 text-sm">Forfeited: {formatProseDuration(attendanceReport.flow_totals.forfeited_seconds)}</p>
        </div>

        <div class="mt-5">
          <h4 class="mb-2 font-semibold">Flow breakdown</h4>
          {#if attendanceReport.flow_buckets.length > 0}
            <div class="overflow-x-auto">
              <table class="w-full text-left text-sm">
                <caption class="sr-only">Attendance flow breakdown by {attendanceReport.period}</caption>
                <thead class="border-b text-xs text-muted-foreground">
                  <tr>
                    <th scope="col" class="px-3 py-2 font-medium">{attendanceReport.period} period</th>
                    <th scope="col" class="px-3 py-2 text-right font-medium">Attendance Check-Ins</th>
                    <th scope="col" class="px-3 py-2 text-right font-medium">Earned</th>
                    <th scope="col" class="px-3 py-2 text-right font-medium">Claimed</th>
                    <th scope="col" class="px-3 py-2 text-right font-medium">Expired</th>
                    <th scope="col" class="px-3 py-2 text-right font-medium">Forfeited</th>
                  </tr>
                </thead>
                <tbody class="divide-y">
                  {#each attendanceReport.flow_buckets as bucket (bucket.period_label)}
                    <tr>
                      <td class="px-3 py-2">{bucket.period_label}</td>
                      <td class="px-3 py-2 text-right tabular-nums">{bucket.check_ins.toLocaleString()}</td>
                      <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(bucket.earned_seconds)}</td>
                      <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(bucket.claimed_seconds)}</td>
                      <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(bucket.expired_seconds)}</td>
                      <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(bucket.forfeited_seconds)}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {:else}
            <p class="text-sm text-muted-foreground">No Attendance flows in this range.</p>
          {/if}
        </div>
      </section>

      <section class="rounded-lg border bg-card p-6" aria-labelledby="attendance-month-heading">
        <h3 id="attendance-month-heading" class="mb-3 text-lg font-semibold">Earning Month exposure</h3>
        {#if attendanceReport.month_page.rows.length > 0}
          <div class="overflow-x-auto">
            <table class="w-full text-left text-sm">
              <caption class="sr-only">Attendance exposure by Earning Month</caption>
              <thead class="border-b text-xs text-muted-foreground">
                <tr>
                  <th scope="col" class="px-3 py-2 font-medium">Account</th>
                  <th scope="col" class="px-3 py-2 font-medium">Earning Month</th>
                  <th scope="col" class="px-3 py-2 font-medium">State</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Earned</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Claimed</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Outstanding</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Expired</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Forfeited</th>
                  <th scope="col" class="px-3 py-2 font-medium">Claim deadline (inclusive)</th>
                </tr>
              </thead>
              <tbody class="divide-y">
                {#each attendanceReport.month_page.rows as row (`${row.account_id}-${row.earning_month}`)}
                  <tr>
                    <td class="px-3 py-2 font-medium">{row.username}</td>
                    <td class="px-3 py-2">{row.earning_month}</td>
                    <td class="px-3 py-2">{statusLabel(row.state)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.earned_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.claimed_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.outstanding_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.expired_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.forfeited_seconds)}</td>
                    <td class="px-3 py-2">{formatAttendanceDeadline(row.claim_deadline_local_date)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {:else}
          <p class="text-sm text-muted-foreground">No Earning Month rows.</p>
        {/if}
        <AttendancePager
          page={attendanceReport.month_page}
          pageName="Earning Month"
          busy={attendanceBusy}
          onPrevious={() => nextAttendanceMonthPage(-1)}
          onNext={() => nextAttendanceMonthPage(1)}
        />
      </section>

      <section class="rounded-lg border bg-card p-6" aria-labelledby="attendance-account-heading">
        <h3 id="attendance-account-heading" class="mb-3 text-lg font-semibold">Account exposure</h3>
        {#if attendanceReport.account_page.rows.length > 0}
          <div class="overflow-x-auto">
            <table class="w-full text-left text-sm">
              <caption class="sr-only">Attendance exposure by Account</caption>
              <thead class="border-b text-xs text-muted-foreground">
                <tr>
                  <th scope="col" class="px-3 py-2 font-medium">Account</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Attendance Check-Ins</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Earned</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Claimed</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Outstanding</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Claimable</th>
                  <th scope="col" class="px-3 py-2 text-right font-medium">Expired</th>
                </tr>
              </thead>
              <tbody class="divide-y">
                {#each attendanceReport.account_page.rows as row (row.account_id)}
                  <tr>
                    <td class="px-3 py-2 font-medium">{row.username}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{row.check_ins.toLocaleString()}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.earned_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.claimed_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.outstanding_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.claimable_seconds)}</td>
                    <td class="px-3 py-2 text-right tabular-nums">{formatProseDuration(row.expired_seconds)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {:else}
          <p class="text-sm text-muted-foreground">No Account rows.</p>
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

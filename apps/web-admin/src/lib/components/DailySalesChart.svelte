<script lang="ts">
  import { scaleUtc } from "d3-scale";
  import { curveLinear } from "d3-shape";
  import { AreaChart } from "layerchart";
  import * as Chart from "$lib/components/ui/chart/index.js";
  import { axisLabel, formatMoney, formatMoneyCompact, formatReportDate } from "../format";
  import type { DailySalesPoint, ReportPeriod } from "../types";

  interface Props {
    points: readonly DailySalesPoint[];
    caption: string;
    /** What one bar covers; day bars are labelled by date, others by their start. */
    bucket?: ReportPeriod;
  }

  interface ChartPoint {
    date: Date;
    coin_cents: number;
    voucher_cents: number;
  }

  let { points, caption, bucket = "day" }: Props = $props();

  const BUCKET_WORD = { day: "Daily", week: "Weekly", month: "Monthly" } as const;
  const BUCKET_NOUN = { day: "day", week: "week", month: "month" } as const;

  /** Coin Sales and Voucher Sales, stacked so the top edge is Cash Revenue. */
  const chartConfig = {
    coin_cents: {
      label: "Coin Sales",
      theme: {
        light: "var(--chart-5)",
        dark: "var(--chart-2)",
      },
    },
    voucher_cents: {
      label: "Voucher Sales",
      theme: {
        light: "var(--chart-2)",
        dark: "var(--chart-4)",
      },
    },
  } satisfies Chart.ChartConfig;

  function totalCents(point: DailySalesPoint): number {
    return point.coin_cents + point.voucher_cents;
  }

  function niceMax(maxCents: number): number {
    if (maxCents <= 100) return 100;
    const magnitude = 10 ** Math.floor(Math.log10(maxCents));
    for (const step of [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) {
      const candidate = step * magnitude;
      if (candidate >= maxCents) return candidate;
    }
    return magnitude * 10;
  }

  function toUtcDate(date: string): Date {
    return new Date(`${date}T00:00:00Z`);
  }

  function toReportDate(value: unknown): string {
    if (!(value instanceof Date)) return "";
    return formatReportDate(value.toISOString().slice(0, 10));
  }

  function toMoney(value: unknown): string {
    return typeof value === "number" ? formatMoney(value) : "";
  }

  let hasSales = $derived(points.some((point) => totalCents(point) > 0));
  let chartData = $derived<ChartPoint[]>(
    points.map((point) => ({
      date: toUtcDate(point.date),
      coin_cents: point.coin_cents,
      voucher_cents: point.voucher_cents,
    })),
  );
  let axisMax = $derived(niceMax(Math.max(0, ...points.map(totalCents))));
  // Day bars tick every fifth date; week and month bars tick at most ~12 times.
  let xTicks = $derived(
    bucket === "day"
      ? chartData.filter((point) => point.date.getUTCDate() % 5 === 1).map((point) => point.date)
      : chartData
          .filter((_, index) => index % Math.ceil(chartData.length / 12) === 0)
          .map((point) => point.date),
  );
  let peak = $derived.by(() => {
    let best = points[0] ?? null;
    for (const point of points) {
      if (best && totalCents(point) > totalCents(best)) best = point;
    }
    return best;
  });
</script>

{#snippet tooltipRow(label: string, value: unknown, strong = false)}
  <div class="flex w-full min-w-28 items-center justify-between gap-3">
    <span class={strong ? "font-medium" : "text-muted-foreground"}>{label}</span>
    <span class="font-mono font-medium tabular-nums">{toMoney(value)}</span>
  </div>
{/snippet}

<!-- One row per series; the last row also carries the bar's total. -->
{#snippet tooltipValue({
  value,
  name,
  index,
  payload,
}: {
  value: unknown;
  name: string;
  index: number;
  payload: readonly { value?: unknown }[];
})}
  {@render tooltipRow(name, value)}
  {#if index === payload.length - 1}
    {@render tooltipRow(
      "Total",
      payload.reduce(
        (sum, item) => sum + (typeof item.value === "number" ? item.value : 0),
        0,
      ),
      true,
    )}
  {/if}
{/snippet}

{#if points.length === 0}
  <!-- Same footprint as the plot, so switching ranges does not jump the page. -->
  <div
    class="text-muted-foreground flex h-[12.5rem] items-center justify-center rounded-md border border-dashed text-sm md:h-[15rem]"
  >
    <p>No sales recorded for this range.</p>
  </div>
{:else}
  <figure class="m-0">
    {#if !hasSales}
      <p class="text-muted-foreground mb-3 text-sm">No sales recorded for this range.</p>
    {/if}
    {#if peak}
      <div
        data-testid="sales-peak"
        class="mb-3 flex min-h-5 flex-wrap items-baseline gap-x-2 text-sm"
      >
        <span class="text-muted-foreground"
          >Best {BUCKET_NOUN[bucket]} · {formatReportDate(peak.date)}</span
        >
        <span class="font-semibold tabular-nums">{formatMoney(totalCents(peak))}</span>
      </div>
    {/if}

    <Chart.Container
      config={chartConfig}
      class="aspect-auto h-[10.75rem] w-full md:h-[13.75rem]"
      role="img"
      aria-label="{BUCKET_WORD[bucket]} sales for {caption}"
    >
      <AreaChart
        data={chartData}
        x="date"
        xScale={scaleUtc()}
        yDomain={[0, axisMax]}
        series={[
          {
            key: "coin_cents",
            label: "Coin Sales",
            color: "var(--color-coin_cents)",
          },
          {
            key: "voucher_cents",
            label: "Voucher Sales",
            color: "var(--color-voucher_cents)",
          },
        ]}
        seriesLayout="stack"
        points={chartData.length === 1}
        tooltipContext={{ mode: "bisect-x" }}
        props={{
          area: {
            curve: curveLinear,
            fillOpacity: 0.25,
            line: { class: "stroke-2" },
            motion: "tween",
          },
          xAxis: { format: (value: Date) => axisLabel(bucket, value), ticks: xTicks },
          yAxis: { format: formatMoneyCompact, ticks: 4 },
        }}
      >
        {#snippet tooltip()}
          <Chart.Tooltip
            data-testid="sales-tooltip"
            labelFormatter={toReportDate}
            formatter={tooltipValue}
          />
        {/snippet}
      </AreaChart>
    </Chart.Container>

    <div class="sr-only">
      <table>
        <caption>{BUCKET_WORD[bucket]} sales for {caption}</caption>
        <thead>
          <tr>
            <th scope="col">Date</th>
            <th scope="col">Coin Sales</th>
            <th scope="col">Voucher Sales</th>
            <th scope="col">Total</th>
          </tr>
        </thead>
        <tbody>
          {#each points as point}
            <tr>
              <th scope="row">{formatReportDate(point.date)}</th>
              <td>{formatMoney(point.coin_cents)}</td>
              <td>{formatMoney(point.voucher_cents)}</td>
              <td>{formatMoney(totalCents(point))}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </figure>
{/if}

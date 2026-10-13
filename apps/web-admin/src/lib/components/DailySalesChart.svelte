<script lang="ts">
  import { scaleUtc } from "d3-scale";
  import { curveLinear } from "d3-shape";
  import { AreaChart } from "layerchart";
  import * as Chart from "$lib/components/ui/chart/index.js";
  import { formatMoney, formatMoneyCompact, formatReportDate } from "../format";
  import type { DailySalesPoint } from "../types";

  interface Props {
    points: readonly DailySalesPoint[];
    caption: string;
  }

  interface ChartPoint {
    date: Date;
    cents: number;
  }

  let { points, caption }: Props = $props();

  const chartConfig = {
    cents: {
      label: "Sales",
      theme: {
        light: "var(--chart-4)",
        dark: "var(--chart-2)",
      },
    },
  } satisfies Chart.ChartConfig;

  function niceMax(maxCents: number): number {
    if (maxCents <= 100) return 100;
    const magnitude = 10 ** Math.floor(Math.log10(maxCents));
    for (const step of [1, 2, 5, 10]) {
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

  function dayLabel(value: Date): string {
    return String(value.getUTCDate());
  }

  let hasSales = $derived(points.some((point) => point.cents > 0));
  let chartData = $derived<ChartPoint[]>(
    points.map((point) => ({
      date: toUtcDate(point.date),
      cents: point.cents,
    })),
  );
  let axisMax = $derived(
    niceMax(Math.max(0, ...points.map((point) => point.cents))),
  );
  let xTicks = $derived(
    chartData
      .filter((point) => point.date.getUTCDate() % 5 === 1)
      .map((point) => point.date),
  );
  let peak = $derived.by(() => {
    let best = points[0] ?? null;
    for (const point of points) {
      if (best && point.cents > best.cents) best = point;
    }
    return best;
  });
</script>

{#snippet tooltipValue({ value }: { value: unknown })}
  <div class="flex w-full min-w-28 items-center justify-between gap-3">
    <span class="text-muted-foreground">Sales</span>
    <span class="font-mono font-medium tabular-nums">{toMoney(value)}</span>
  </div>
{/snippet}

{#if !hasSales}
  <p class="text-muted-foreground text-sm">No sales recorded for this month.</p>
{:else}
  <figure class="m-0">
    {#if peak}
      <div
        data-testid="sales-peak"
        class="mb-2 flex min-h-5 flex-wrap items-baseline gap-x-2 text-xs"
      >
        <span class="text-muted-foreground"
          >Best day · {formatReportDate(peak.date)}</span
        >
        <span class="font-mono font-medium">{formatMoney(peak.cents)}</span>
      </div>
    {/if}

    <Chart.Container
      config={chartConfig}
      class="aspect-auto h-[100px] w-full md:h-[120px]"
      role="img"
      aria-label="Daily sales for {caption}"
    >
      <AreaChart
        data={chartData}
        x="date"
        xScale={scaleUtc()}
        yDomain={[0, axisMax]}
        series={[
          {
            key: "cents",
            label: "Sales",
            color: "var(--color-cents)",
          },
        ]}
        seriesLayout="stack"
        points={chartData.length === 1}
        tooltipContext={{ mode: "bisect-x" }}
        props={{
          area: {
            curve: curveLinear,
            fillOpacity: 0.4,
            line: { class: "stroke-1" },
            motion: "tween",
          },
          xAxis: { format: dayLabel, ticks: xTicks },
          yAxis: { format: formatMoneyCompact, ticks: 3 },
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
        <caption>Daily sales for {caption}</caption>
        <thead>
          <tr>
            <th scope="col">Date</th>
            <th scope="col">Sales</th>
          </tr>
        </thead>
        <tbody>
          {#each points as point}
            <tr>
              <th scope="row">{formatReportDate(point.date)}</th>
              <td>{formatMoney(point.cents)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </figure>
{/if}

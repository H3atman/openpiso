// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, type RenderResult } from "@testing-library/svelte";
import DailySalesChart from "./DailySalesChart.svelte";
import { formatMoney, formatMoneyCompact } from "../format";
import type { DailySalesPoint } from "../types";

const points: DailySalesPoint[] = [
  { date: "2026-08-01", cents: 0 },
  { date: "2026-08-02", cents: 1_500 },
  { date: "2026-08-03", cents: 500 },
];

function renderChart(
  overrides: Partial<{ points: DailySalesPoint[]; caption: string }> = {},
): RenderResult<typeof DailySalesChart> {
  return render(DailySalesChart, {
    props: { points, caption: "Aug 1–3, 2026", ...overrides },
  });
}

afterEach(cleanup);

describe("DailySalesChart", () => {
  it("renders one accessible row per day", () => {
    const view = renderChart();
    const rows = view.getByRole("table").querySelectorAll("tbody tr");

    expect(rows).toHaveLength(3);
  });

  it("exposes the same values as an accessible table", () => {
    const view = renderChart();

    const table = view.getByRole("table");
    expect(table.textContent).toContain(formatMoney(1_500));
    expect(table.textContent).toContain(formatMoney(500));
  });

  it("hides the table's caption from the page, not just the table", () => {
    const view = renderChart();
    const table = view.getByRole("table");

    // A caption escapes `sr-only` applied to its own table, so the wrapper
    // has to carry it or the caption paints on screen.
    expect(table.classList.contains("sr-only")).toBe(false);
    expect(table.parentElement?.classList.contains("sr-only")).toBe(true);
  });

  it("reports an empty state instead of an empty plot", () => {
    const view = renderChart({ points: [] });

    expect(view.getByText("No sales recorded for this month.")).toBeTruthy();
    expect(view.queryByRole("img")).toBeNull();
  });

  it("reports no sales rather than plotting a flat line on a zero month", () => {
    const view = renderChart({
      points: [
        { date: "2026-08-01", cents: 0 },
        { date: "2026-08-02", cents: 0 },
      ],
    });

    expect(view.getByText("No sales recorded for this month.")).toBeTruthy();
    expect(view.queryByRole("img")).toBeNull();
  });

  it("keeps every gridline label on whole cents", () => {
    const view = renderChart({ points: [{ date: "2026-08-01", cents: 3 }] });
    const labels = [...view.container.querySelectorAll("text")].map(
      (node) => node.textContent ?? "",
    );

    expect(labels).toContain(formatMoneyCompact(100));
    expect(labels.every((label) => !/\.\d{3}/.test(label))).toBe(true);
  });

  it("keeps the first and every fifth day on the x-axis", () => {
    const view = renderChart({
      points: Array.from({ length: 11 }, (_, index) => ({
        date: `2026-08-${String(index + 1).padStart(2, "0")}`,
        cents: 100,
      })),
    });
    const labels = [
      ...view.container.querySelectorAll(
        ".lc-axis.placement-bottom .lc-axis-tick-label",
      ),
    ].map((node) => node.textContent);

    expect(labels).toEqual(["1", "6", "11"]);
  });

  it("marks a single sales day", async () => {
    const view = renderChart({
      points: [{ date: "2026-08-01", cents: 1_500 }],
    });

    await vi.waitFor(() => {
      expect(view.container.querySelector(".lc-point")).not.toBeNull();
    });
  });

  it("keeps the best day as a standing advisory", () => {
    const view = renderChart();

    const peak = view.getByTestId("sales-peak").textContent ?? "";
    expect(peak).toContain("Aug 2, 2026");
    expect(peak).toContain(formatMoney(1_500));
  });

  it("shows the cursor date and formatted sales value", async () => {
    const view = renderChart();
    const target = view.container.querySelector<SVGElement>(
      ".lc-tooltip-context",
    );
    const root =
      view.container.querySelector<HTMLElement>(".lc-root-container");
    if (!target) throw new Error("chart tooltip target missing");
    if (!root) throw new Error("chart root missing");

    const bounds = {
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 720,
      bottom: 120,
      width: 720,
      height: 120,
      toJSON: () => ({}),
    } as DOMRect;
    target.getBoundingClientRect = () => bounds;
    root.getBoundingClientRect = () => bounds;
    Object.defineProperties(target, {
      offsetLeft: { configurable: true, value: 20 },
      offsetTop: { configurable: true, value: 4 },
      offsetWidth: { configurable: true, value: 696 },
      offsetHeight: { configurable: true, value: 96 },
    });
    const event = { bubbles: true, clientX: 360, clientY: 60 };

    await vi.waitFor(() => {
      target.dispatchEvent(new PointerEvent("pointerenter", event));
      target.dispatchEvent(new PointerEvent("pointermove", event));

      const tooltip = view.getByTestId("sales-tooltip").textContent ?? "";
      expect(tooltip).toContain("Aug 2, 2026");
      expect(tooltip).toContain(formatMoney(1_500));
    });
  });
});

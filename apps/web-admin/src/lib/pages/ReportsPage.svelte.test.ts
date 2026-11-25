// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  getSalesReportOverview: vi.fn(),
  getReportSummary: vi.fn(),
  getPcSales: vi.fn(),
  getAttendanceReport: vi.fn(),
  getCashOnHand: vi.fn().mockResolvedValue({
    coin_box: { uncollected_cents: 0, last_collection: null },
    voucher_cash: { uncollected_cents: 0, last_collection: null },
  }),
  collectCash: vi.fn(),
}));

import { fireEvent, render, waitFor, within, type RenderResult } from "@testing-library/svelte";
import * as api from "../api";
import ReportsPage from "./ReportsPage.svelte";
import { formatMoney } from "../format";

const overview = {
  daily: {
    from_date: "2026-08-06",
    to_date: "2026-08-06",
    cash_revenue_cents: 500,
  },
  weekly: {
    from_date: "2026-08-03",
    to_date: "2026-08-09",
    cash_revenue_cents: 1_000,
  },
  monthly: {
    from_date: "2026-08-01",
    to_date: "2026-08-06",
    cash_revenue_cents: 2_000,
  },
  total: {
    from_date: null,
    to_date: null,
    cash_revenue_cents: 5_000,
  },
  pcs: [
    {
      id: "pc-01",
      name: "PC 01",
      daily_sales_cents: 500,
      weekly_sales_cents: 1_000,
      monthly_sales_cents: 2_000,
      total_sales_cents: 5_000,
    },
    {
      id: "pc-02",
      name: "PC 02",
      daily_sales_cents: 0,
      weekly_sales_cents: 0,
      monthly_sales_cents: 0,
      total_sales_cents: 0,
    },
  ],
};

const pcSales = {
  from_date: "2026-08-01",
  to_date: "2026-08-31",
  pcs: [
    { id: "pc-01", name: "PC 01", coin_sales_cents: 200, voucher_sales_cents: 0 },
    { id: "pc-02", name: "PC 02", coin_sales_cents: 500, voucher_sales_cents: 500 },
  ],
  unassigned: { coin_sales_cents: 0, voucher_sales_cents: 2_000 },
};

const dailySummary = {
  period: "day" as const,
  from_date: "2026-08-01",
  to_date: "2026-08-31",
  coin_sales_cents: 1_700,
  voucher_sales_cents: 300,
  cash_revenue_cents: 2_000,
  free_time_dispensed_seconds: 0,
  points_minted_by_wheel_centi: 0,
  points_liability_centi: 0,
  buckets: [
    {
      period_label: "2026-08-02",
      coin_sales_cents: 1_200,
      voucher_sales_cents: 300,
      cash_revenue_cents: 1_500,
      free_time_dispensed_seconds: 0,
      points_minted_by_wheel_centi: 0,
    },
    {
      period_label: "2026-08-03",
      coin_sales_cents: 500,
      voucher_sales_cents: 0,
      cash_revenue_cents: 500,
      free_time_dispensed_seconds: 0,
      points_minted_by_wheel_centi: 0,
    },
  ],
};

const attendanceReport = {
  period: "month" as const,
  from_date: null,
  to_date: null,
  as_of: "2026-09-26T00:00:00Z",
  admin_local_date: "2026-09-26",
  admin_local_month: "2026-09",
  flow_totals: {
    check_ins: 2,
    earned_seconds: 5_400,
    claimed_seconds: 1_800,
    expired_seconds: 0,
    forfeited_seconds: 0,
  },
  flow_buckets: [],
  lifetime_totals: {
    earned_seconds: 5_400,
    claimed_seconds: 1_800,
    outstanding_seconds: 3_600,
    expired_seconds: 0,
    forfeited_seconds: 0,
  },
  liability: { outstanding_seconds: 3_600, claimable_seconds: 1_800 },
  month_page: {
    rows: [
      {
        account_id: "account-1",
        username: "jose",
        earning_month: "2026-08",
        state: "claimable" as const,
        claim_starts_at: "2026-09-01T00:00:00Z",
        deadline_at_exclusive: "2026-10-01T00:00:00Z",
        claim_deadline_local_date: "2026-09-30",
        earned_seconds: 5_400,
        claimed_seconds: 1_800,
        outstanding_seconds: 3_600,
        expired_seconds: 0,
        forfeited_seconds: 0,
      },
    ],
    total: 1,
    limit: 50,
    offset: 0,
    has_more: false,
  },
  account_page: {
    rows: [
      {
        account_id: "account-1",
        username: "jose",
        check_ins: 2,
        earned_seconds: 5_400,
        claimed_seconds: 1_800,
        outstanding_seconds: 3_600,
        claimable_seconds: 1_800,
        expired_seconds: 0,
      },
    ],
    total: 1,
    limit: 50,
    offset: 0,
    has_more: false,
  },
  economics_advisory: null,
  clock_regression_advisory: false,
};


type View = RenderResult<typeof ReportsPage>;

describe("Sales Report", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getSalesReportOverview).mockResolvedValue(overview);
    vi.mocked(api.getReportSummary).mockResolvedValue(dailySummary);
    vi.mocked(api.getPcSales).mockResolvedValue(pcSales);
    vi.mocked(api.getAttendanceReport).mockResolvedValue(attendanceReport);
  });

  it("renders the four fixed cards and the ranged per-PC table", async () => {
    const view = render(ReportsPage);

    await waitFor(() => expect(view.getByText("PC Breakdown")).toBeTruthy());
    expect(api.getSalesReportOverview).toHaveBeenCalledOnce();
    expect(view.getByText("Summary")).toBeTruthy();
    for (const label of ["Daily Sales", "Weekly Sales", "Monthly Sales", "Total Sales"]) {
      expect(view.getAllByText(label).length).toBe(1);
    }
    expect(view.getByRole("table", { name: "Sales breakdown by PC" })).toBeTruthy();
    expect(view.queryByText("Free Time Dispensed")).toBeNull();
    expect(view.queryByText("Points Minted by Wheel")).toBeNull();
    expect(view.queryByText("Points Liability")).toBeNull();
  });

  it("shows each PC's Coin and Voucher Sales plus the unassigned row, following the range", async () => {
    const view = render(ReportsPage);
    await waitFor(() =>
      expect(api.getPcSales).toHaveBeenCalledWith("2026-08-01", "2026-08-31"),
    );

    const rows = await chartRows(view, /Sales breakdown by PC/);
    const bodyRows = rows.filter((row) => row.querySelector("td"));
    const line = (row: HTMLElement) => [
      row.querySelector("th")?.textContent?.trim(),
      ...cells(row).map((cell) => cell?.trim()),
    ];
    expect(bodyRows.map(line)).toEqual([
      ["PC 01", formatMoney(200), formatMoney(0), formatMoney(200)],
      ["PC 02", formatMoney(500), formatMoney(500), formatMoney(1_000)],
      ["Unassigned", formatMoney(0), formatMoney(2_000), formatMoney(2_000)],
    ]);

    vi.mocked(api.getPcSales).mockResolvedValueOnce({
      ...pcSales,
      pcs: [
        { id: "pc-01", name: "PC 01", coin_sales_cents: 100, voucher_sales_cents: 0 },
        { id: "pc-02", name: "PC 02", coin_sales_cents: 0, voucher_sales_cents: 0 },
      ],
      unassigned: { coin_sales_cents: 0, voucher_sales_cents: 0 },
    });
    await fireEvent.click(view.getByRole("button", { name: "Today" }));
    await waitFor(() =>
      expect(api.getPcSales).toHaveBeenLastCalledWith("2026-08-06", "2026-08-06"),
    );
    await waitFor(() => {
      const next = within(view.getByRole("table", { name: /Sales breakdown by PC/ }))
        .getAllByRole("row")
        .filter((row) => row.querySelector("td"));
      expect(line(next[0])[3]).toBe(formatMoney(100));
    });
  });

  it("keeps the caption, chart and totals on one period until the new range arrives", async () => {
    const view = render(ReportsPage);
    await waitFor(() => expect(view.getAllByText("Aug 1–31, 2026").length).toBeGreaterThan(0));

    let release: (value: typeof dailySummary) => void = () => {};
    vi.mocked(api.getReportSummary).mockReturnValueOnce(
      new Promise((resolve) => (release = resolve)),
    );
    await fireEvent.click(view.getByRole("button", { name: "Today" }));

    // Still loading: everything on screen is still the month.
    expect(view.getAllByText("Aug 1–31, 2026").length).toBeGreaterThan(0);
    expect(view.getByRole("table", { name: /^Daily sales for Aug 1–31, 2026/ })).toBeTruthy();

    release({ ...dailySummary, coin_sales_cents: 0, voucher_sales_cents: 0, cash_revenue_cents: 0, buckets: [] });
    await waitFor(() =>
      expect(view.getByRole("table", { name: /^Daily sales for Aug 6, 2026/ })).toBeTruthy(),
    );
  });

  it("does not fall back to the browser date when the overview fails", async () => {
    vi.mocked(api.getSalesReportOverview).mockRejectedValue(new Error("offline"));
    const view = render(ReportsPage);

    await waitFor(() => expect(view.getByText(/Couldn't load the sales report: offline/)).toBeTruthy());
    expect(api.getReportSummary).not.toHaveBeenCalled();
    expect(api.getPcSales).not.toHaveBeenCalled();
    expect(view.getByText(/business date is unavailable/i)).toBeTruthy();
  });

  it("opens on This month, anchored to the server's business date", async () => {
    const view = render(ReportsPage);

    await waitFor(() =>
      expect(api.getReportSummary).toHaveBeenCalledWith("day", "2026-08-01", "2026-08-31"),
    );
    expect(view.getByText("Sales Trend")).toBeTruthy();
    expect(
      view.getByRole("button", { name: "This month" }).getAttribute("aria-pressed"),
    ).toBe("true");
  });

  it("requests the right inclusive range for each preset", async () => {
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalled());

    for (const [name, from, to] of [
      ["Today", "2026-08-06", "2026-08-06"],
      ["This week", "2026-08-03", "2026-08-09"],
      ["Last 30 days", "2026-07-08", "2026-08-06"],
      ["This month", "2026-08-01", "2026-08-31"],
    ] as const) {
      await fireEvent.click(view.getByRole("button", { name }));
      await waitFor(() =>
        expect(api.getReportSummary).toHaveBeenLastCalledWith("day", from, to),
      );
    }
  });

  it("summarises Coin Sales, Voucher Sales and Cash Revenue for the range", async () => {
    const view = render(ReportsPage);

    const summary = await waitFor(() =>
      view.getByRole("region", { name: "Selected range summary" }),
    );
    expect(within(summary).getByText("Coin Sales").nextElementSibling?.textContent).toContain(
      formatMoney(1_700),
    );
    expect(within(summary).getByText("Voucher Sales").nextElementSibling?.textContent).toContain(
      formatMoney(300),
    );
    expect(within(summary).getByText("Cash Revenue").nextElementSibling?.textContent).toContain(
      formatMoney(2_000),
    );
  });

  async function applyCustom(
    view: View,
    from: string,
    to: string,
  ): Promise<void> {
    await fireEvent.click(view.getByRole("button", { name: "Custom" }));
    await fireEvent.input(view.getByLabelText("Sales from"), { target: { value: from } });
    await fireEvent.input(view.getByLabelText("Sales to"), { target: { value: to } });
    await fireEvent.click(view.getByRole("button", { name: "Apply range" }));
  }

  it("refuses a custom range that is reversed or incomplete instead of loading", async () => {
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalledOnce());

    await applyCustom(view, "2026-08-10", "2026-08-01");
    expect((await view.findByRole("alert")).textContent).toContain(
      "From date must not be after To date",
    );

    await applyCustom(view, "", "2026-08-01");
    expect((await view.findByRole("alert")).textContent).toContain("Choose both dates");
    expect(api.getReportSummary).toHaveBeenCalledOnce();
  });

  it("loads a valid custom range and shows a server refusal as a message", async () => {
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalledOnce());

    await applyCustom(view, "2026-07-01", "2026-07-05");
    await waitFor(() =>
      expect(api.getReportSummary).toHaveBeenLastCalledWith("day", "2026-07-01", "2026-07-05"),
    );

    vi.mocked(api.getReportSummary).mockRejectedValueOnce(new Error("date must be YYYY-MM-DD"));
    await applyCustom(view, "2026-07-01", "2026-07-06");
    expect((await view.findByText(/date must be YYYY-MM-DD/)).textContent).toContain(
      "Couldn't load sales",
    );
  });

  it("switches bucket size at 92 days and again beyond 366 days", async () => {
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalledOnce());

    for (const [from, to, bucket] of [
      ["2026-01-01", "2026-04-02", "day"], // 92 days
      ["2026-01-01", "2026-04-03", "week"], // 93 days
      ["2025-01-01", "2026-01-01", "week"], // 366 days
      ["2025-01-01", "2026-01-02", "month"], // 367 days
    ] as const) {
      await applyCustom(view, from, to);
      await waitFor(() =>
        expect(api.getReportSummary).toHaveBeenLastCalledWith(bucket, from, to),
      );
    }
  });

  function chartRows(view: View, name: RegExp) {
    return waitFor(() => within(view.getByRole("table", { name })).getAllByRole("row"));
  }

  function cells(row: HTMLElement | undefined): (string | null)[] {
    return [...(row?.querySelectorAll("td") ?? [])].map((cell) => cell.textContent);
  }

  it("leaves days after the admin host's business date off the plot", async () => {
    const view = render(ReportsPage);

    // Header plus Aug 1-6 only: the month runs to the 31st but today is the 6th.
    const rows = await chartRows(view, /^Daily sales for/);
    expect(rows).toHaveLength(7);
    // The 2nd plots the server's Coin/Voucher split and its total.
    expect(cells(rows[2])).toEqual([formatMoney(1_200), formatMoney(300), formatMoney(1_500)]);
    // Days without sales are zero, not skipped.
    expect(cells(rows[1])).toEqual([formatMoney(0), formatMoney(0), formatMoney(0)]);
  });

  it("zero-fills every Monday week in a weekly range", async () => {
    vi.mocked(api.getReportSummary).mockResolvedValue({
      ...dailySummary,
      period: "week",
      buckets: [{ ...dailySummary.buckets[0]!, period_label: "2026-05-04" }],
    });
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalledOnce());

    await applyCustom(view, "2026-04-01", "2026-08-06");
    // Weeks of Mar 30 .. Aug 3 are 19; the week of Aug 10 is after today.
    const rows = await chartRows(view, /^Weekly sales for/);
    expect(rows).toHaveLength(20);
    expect(within(rows[6]!).getByRole("rowheader").textContent).toBe("May 4, 2026");
    expect(cells(rows[6])).toEqual([formatMoney(1_200), formatMoney(300), formatMoney(1_500)]);
    expect(cells(rows[7])).toEqual([formatMoney(0), formatMoney(0), formatMoney(0)]);
  });

  it("zero-fills every month in a monthly range", async () => {
    vi.mocked(api.getReportSummary).mockResolvedValue({
      ...dailySummary,
      period: "month",
      buckets: [{ ...dailySummary.buckets[0]!, period_label: "2025-03" }],
    });
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalledOnce());

    await applyCustom(view, "2025-01-15", "2026-02-20");
    // Jan 2025 .. Feb 2026 is 14 months.
    const rows = await chartRows(view, /^Monthly sales for/);
    expect(rows).toHaveLength(15);
    expect(cells(rows[3])).toEqual([formatMoney(1_200), formatMoney(300), formatMoney(1_500)]);
  });

  it("reports Attendance flow and range-independent liability in prose durations", async () => {
    const view = render(ReportsPage);

    await waitFor(() => expect(api.getAttendanceReport).toHaveBeenCalledWith({
      period: "month",
    }));
    const liability = await waitFor(() =>
      view.getByRole("region", { name: "Outstanding Attendance Time" }),
    );
    expect(view.getByText("Attendance Report")).toBeTruthy();
    expect(within(liability).getByText("Outstanding")).toBeTruthy();
    expect(within(liability).getByText("1 hour")).toBeTruthy();
    expect(within(liability).getByText("Claimable")).toBeTruthy();
    expect(within(liability).getByText("30 min")).toBeTruthy();
    const flow = view.getByRole("region", { name: "Attendance flow" });
    expect(within(flow).getByText("Earned")).toBeTruthy();
    expect(within(flow).getByText("1 hour 30 min")).toBeTruthy();
    expect(within(flow).getByText("Attendance Check-Ins")).toBeTruthy();
    expect(within(flow).getByText("2")).toBeTruthy();
    expect(view.getByText("2026-08")).toBeTruthy();
    expect(view.getByText("Sep 30, 2026")).toBeTruthy();
    const earningMonths = view.getByRole("table", { name: "Attendance exposure by Earning Month" });
    expect(within(earningMonths).getByRole("columnheader", { name: "Expired" })).toBeTruthy();
    expect(within(earningMonths).getByRole("columnheader", { name: "Forfeited" })).toBeTruthy();
    expect(within(earningMonths).getByRole("columnheader", { name: "Earned" }).classList.contains("text-right")).toBe(true);
    expect(view.getAllByText("jose")).toHaveLength(2);

    await fireEvent.change(view.getByLabelText("Attendance period"), {
      target: { value: "week" },
    });
    await waitFor(() => expect(api.getAttendanceReport).toHaveBeenLastCalledWith({
      period: "week",
    }));
  });
});

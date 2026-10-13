// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  getSalesReportOverview: vi.fn(),
  getReportSummary: vi.fn(),
  getAttendanceReport: vi.fn(),
}));

import { fireEvent, render, waitFor, within } from "@testing-library/svelte";
import * as api from "../api";
import ReportsPage from "./ReportsPage.svelte";

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

const dailySummary = {
  period: "day" as const,
  from_date: "2026-08-01",
  to_date: "2026-08-31",
  cash_revenue_cents: 2_000,
  free_time_dispensed_seconds: 0,
  points_minted_by_wheel_centi: 0,
  points_liability_centi: 0,
  buckets: [
    {
      period_label: "2026-08-02",
      cash_revenue_cents: 1_500,
      free_time_dispensed_seconds: 0,
      points_minted_by_wheel_centi: 0,
    },
    {
      period_label: "2026-08-03",
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

function pad2(value: number): string {
  return value.toString().padStart(2, "0");
}

describe("Sales Report", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getSalesReportOverview).mockResolvedValue(overview);
    vi.mocked(api.getReportSummary).mockResolvedValue(dailySummary);
    vi.mocked(api.getAttendanceReport).mockResolvedValue(attendanceReport);
  });

  it("renders sales-only summary cards and the all-PC breakdown table", async () => {
    const view = render(ReportsPage);

    await waitFor(() => expect(view.getByText("Summary")).toBeTruthy());
    expect(api.getSalesReportOverview).toHaveBeenCalledOnce();
    expect(view.getByText("PC Breakdown")).toBeTruthy();
    expect(view.getAllByText("Daily Sales").length).toBe(2);
    expect(view.getAllByText("Weekly Sales").length).toBe(2);
    expect(view.getAllByText("Monthly Sales").length).toBe(2);
    expect(view.getAllByText("Total Sales").length).toBe(2);
    expect(view.getAllByText("Aug 3–9, 2026").length).toBe(2);
    expect(view.getByText("PC 01")).toBeTruthy();
    expect(view.getByText("PC 02")).toBeTruthy();
    expect(
      view.getByRole("table", { name: "Sales breakdown by PC" }),
    ).toBeTruthy();
    expect(view.queryByText("Cash Revenue")).toBeNull();
    expect(view.queryByText("Free Time Dispensed")).toBeNull();
    expect(view.queryByText("Points Minted by Wheel")).toBeNull();
    expect(view.queryByText("Points Liability")).toBeNull();
  });

  it("loads the daily sales chart for the current month on mount", async () => {
    const now = new Date();
    const year = now.getFullYear();
    const month = now.getMonth() + 1;
    const lastDay = new Date(year, month, 0).getDate();

    const view = render(ReportsPage);

    await waitFor(() =>
      expect(api.getReportSummary).toHaveBeenCalledWith(
        "day",
        `${year}-${pad2(month)}-01`,
        `${year}-${pad2(month)}-${pad2(lastDay)}`,
      ),
    );
    expect(view.getByText("Daily Sales Trend")).toBeTruthy();
  });

  it("reloads the chart for the selected month and year", async () => {
    const view = render(ReportsPage);
    await waitFor(() => expect(api.getReportSummary).toHaveBeenCalled());

    await fireEvent.change(view.getByLabelText("Year"), {
      target: { value: "2025" },
    });
    await fireEvent.change(view.getByLabelText("Month"), {
      target: { value: "2" },
    });

    await waitFor(() =>
      expect(api.getReportSummary).toHaveBeenLastCalledWith(
        "day",
        "2025-02-01",
        "2025-02-28",
      ),
    );
  });

  it("reports Attendance flow and range-independent liability in prose durations", async () => {
    const view = render(ReportsPage);

    await waitFor(() => expect(api.getAttendanceReport).toHaveBeenCalledWith({
      period: "month",
    }));
    await waitFor(() => expect(view.getByText("Outstanding Attendance Time (ignores selected date range)")).toBeTruthy());
    expect(view.getByText("Attendance Report")).toBeTruthy();
    expect(view.getByText("Outstanding Attendance Time (ignores selected date range)")).toBeTruthy();
    expect(view.getByText("Outstanding: 1 hour")).toBeTruthy();
    expect(view.getByText("Claimable: 30 min")).toBeTruthy();
    expect(view.getByText("Earned: 1 hour 30 min")).toBeTruthy();
    expect(view.getByText("Attendance Check-Ins: 2")).toBeTruthy();
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

  it("leaves days after the admin host's business date off the plot", async () => {
    const now = new Date();
    const year = now.getFullYear();
    const month = now.getMonth() + 1;
    const businessToday = `${year}-${pad2(month)}-03`;
    vi.mocked(api.getSalesReportOverview).mockResolvedValue({
      ...overview,
      daily: {
        from_date: businessToday,
        to_date: businessToday,
        cash_revenue_cents: 500,
      },
    });
    vi.mocked(api.getReportSummary).mockResolvedValue({
      ...dailySummary,
      buckets: [
        {
          period_label: `${year}-${pad2(month)}-02`,
          cash_revenue_cents: 1_500,
          free_time_dispensed_seconds: 0,
          points_minted_by_wheel_centi: 0,
        },
      ],
    });

    const view = render(ReportsPage);

    const chart = await waitFor(() =>
      view.getByRole("table", { name: /^Daily sales for/ }),
    );
    // Header row plus the 1st, 2nd, and 3rd only.
    expect(within(chart).getAllByRole("row")).toHaveLength(4);
  });
});

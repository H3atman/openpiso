// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  getDashboard: vi.fn(),
  signOutPcSession: vi.fn(),
  checkPcMonitoring: vi.fn(),
  checkAllPcMonitoring: vi.fn(),
  setPcMonitoringSuppression: vi.fn(),
  cancelPcMonitoringSuppression: vi.fn(),
}));

vi.mock("svelte-sonner", () => ({
  toast: {
    success: vi.fn(),
    info: vi.fn(),
    error: vi.fn(),
  },
}));

import { fireEvent, render, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { toast } from "svelte-sonner";
import * as api from "../api";
import DashboardPage from "./DashboardPage.svelte";
import type { DashboardSnapshot, PcMonitoringCheck } from "../types";

const dashboard: DashboardSnapshot = {
  shop_name: "Test Shop",
  pcs: [
    {
      id: "pc-01",
      name: "PC 01",
      status: "online",
      remaining_seconds: 0,
      guest_remaining_seconds: 0,
      timer_mode: "guest",
      power_state: "active",
      shutdown_grace_seconds: 30,
      active_session: null,
      last_seen_at: "2026-08-10 12:00:00",
      clock_difference_ms: 61_000,
    },
    {
      id: "pc-02",
      name: "PC 02",
      status: "online",
      remaining_seconds: 0,
      guest_remaining_seconds: 0,
      timer_mode: "guest",
      power_state: "active",
      shutdown_grace_seconds: 30,
      active_session: null,
      last_seen_at: "2026-08-10 12:00:00",
      clock_difference_ms: 60_000,
    },
  ],
  recent_transactions: [],
  today: {
    sales_cents: 0,
    transaction_count: 0,
    seconds_sold: 0,
  },
  pc_monitoring: {
    enabled: true,
    active_incidents: {},
    suppressions: {},
    escalation_enabled: false,
  },
};

describe("Dashboard PC clock advisory", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getDashboard).mockResolvedValue(dashboard);
  });

  it("shows an inline advisory only beyond the 60-second threshold", async () => {
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());
    expect(
      view.getByText("Client clock is 1 min 1 sec behind the Admin clock."),
    ).toBeTruthy();
    expect(
      view.queryByText("Client clock is 1 min behind the Admin clock."),
    ).toBeNull();
  });
});

describe("Owner Sign-Out", () => {
  const inUse: DashboardSnapshot = {
    ...dashboard,
    pcs: [
      dashboard.pcs[0],
      {
        ...dashboard.pcs[1],
        status: "in_use",
        timer_mode: "account",
        remaining_seconds: 600,
        active_session: {
          id: "session-9",
          account_id: "account-9",
          username: "juan",
          pc_id: "pc-02",
          remaining_seconds: 600,
          points_balance_centi: 0,
          claimable_guest_seconds: 0,
        },
      },
    ],
  };

  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getDashboard).mockResolvedValue(inUse);
  });

  it("offers Sign Out only for a PC with an Active Session", async () => {
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 02")).toBeTruthy());

    // Scope to each PC's own row, so this cannot pass by rendering the button
    // on the wrong one.
    const row = (pcId: string) =>
      view.container.querySelector(`[data-pc-id="${pcId}"]`) as HTMLElement;
    expect(
      within(row("pc-01")).queryByRole("button", { name: "Sign Out" }),
    ).toBeNull();
    expect(
      within(row("pc-02")).getByRole("button", { name: "Sign Out" }),
    ).toBeTruthy();
  });

  it("names the user and PC in the confirmation", async () => {
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 02")).toBeTruthy());
    await fireEvent.click(view.getByRole("button", { name: "Sign Out" }));

    expect(view.getByText("Sign out juan from PC 02?")).toBeTruthy();
  });

  it("signs out on the row's session id and reloads the dashboard", async () => {
    vi.mocked(api.signOutPcSession).mockResolvedValue({
      signed_out: true,
      username: "juan",
    });
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 02")).toBeTruthy());
    await fireEvent.click(view.getByRole("button", { name: "Sign Out" }));
    const confirm = view
      .getAllByRole("button", { name: "Sign Out" })
      .at(-1) as HTMLElement;
    await fireEvent.click(confirm);

    await waitFor(() =>
      expect(api.signOutPcSession).toHaveBeenCalledWith("pc-02", "session-9"),
    );
    // Initial load plus the post-action refresh.
    await waitFor(() => expect(api.getDashboard).toHaveBeenCalledTimes(2));
  });
});

describe("PC Monitoring Check now", () => {
  const reachable: PcMonitoringCheck = {
    pc_id: "pc-01",
    pc_name: "PC 01",
    host_reachability: { state: "reachable" },
    client_reachability: "reachable",
    guard_coverage: "covered",
  };

  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getDashboard).mockResolvedValue(dashboard);
  });

  it("checks one PC from its row and shows the facts inline", async () => {
    vi.mocked(api.checkPcMonitoring).mockResolvedValue(reachable);
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());
    const row = () =>
      view.container.querySelector('[data-pc-id="pc-01"]') as HTMLElement;
    await fireEvent.click(within(row()).getByRole("button", { name: "Check" }));

    await waitFor(() =>
      expect(api.checkPcMonitoring).toHaveBeenCalledWith("pc-01"),
    );
    await waitFor(() =>
      expect(within(row()).getByText(/Host: reachable/)).toBeTruthy(),
    );
    expect(within(row()).getByText(/Client: reachable/)).toBeTruthy();
    expect(within(row()).getByText(/Guard: covered/)).toBeTruthy();
    expect(toast.success).toHaveBeenCalled();
  });

  it("shows unavailable reasons inline instead of a healthy claim", async () => {
    vi.mocked(api.checkPcMonitoring).mockResolvedValue({
      ...reachable,
      host_reachability: {
        state: "unavailable",
        reason: "registered_ip_missing",
      },
      client_reachability: "unreachable",
    });
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());
    await fireEvent.click(
      within(
        view.container.querySelector('[data-pc-id="pc-01"]') as HTMLElement,
      ).getByRole("button", { name: "Check" }),
    );

    await waitFor(() =>
      expect(
        view.getByText(/Host: unavailable \(no registered IP\)/),
      ).toBeTruthy(),
    );
    expect(view.getByText(/Client: no heartbeat/)).toBeTruthy();
  });

  it("checks all PCs and renders one result per returned PC", async () => {
    vi.mocked(api.checkAllPcMonitoring).mockResolvedValue([
      reachable,
      { ...reachable, pc_id: "pc-02", pc_name: "PC 02" },
    ]);
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());
    await fireEvent.click(view.getByRole("button", { name: "Check All PCs" }));

    await waitFor(() =>
      expect(api.checkAllPcMonitoring).toHaveBeenCalledTimes(1),
    );
    await waitFor(() =>
      expect(view.getAllByText(/Host: reachable/)).toHaveLength(2),
    );
    expect(toast.success).toHaveBeenCalled();
  });

  it("reports check failures through a toast, not an inline result", async () => {
    vi.mocked(api.checkPcMonitoring).mockRejectedValue(new Error("boom"));
    const view = render(DashboardPage);

    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());
    await fireEvent.click(
      within(
        view.container.querySelector('[data-pc-id="pc-01"]') as HTMLElement,
      ).getByRole("button", { name: "Check" }),
    );

    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(view.queryByText(/Host:/)).toBeNull();
  });
});

describe("PC Monitoring standing advisory and incident badges", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getDashboard).mockResolvedValue(dashboard);
  });

  it("shows the standing advisory only while monitoring is disabled", async () => {
    const disabled = {
      ...dashboard,
      pc_monitoring: {
        enabled: false,
        active_incidents: {},
        suppressions: {},
        escalation_enabled: false,
      },
    };
    vi.mocked(api.getDashboard).mockResolvedValue(disabled);
    const view = render(DashboardPage);

    await waitFor(() =>
      expect(view.getByTestId("monitoring-advisory").textContent).toContain(
        "PC Monitoring is disabled",
      ),
    );

    // Re-enabled: the advisory disappears.
    vi.mocked(api.getDashboard).mockResolvedValue(dashboard);
    await fireEvent.click(view.getByRole("button", { name: "Refresh" }));
    await waitFor(() =>
      expect(view.queryByTestId("monitoring-advisory")).toBeNull(),
    );
  });

  it("shows an active incident badge per PC independent of acknowledgement", async () => {
    const withIncident: DashboardSnapshot = {
      ...dashboard,
      pc_monitoring: {
        enabled: true,
        active_incidents: {
          "pc-02": ["client_reachability_mismatch"],
        },
        suppressions: {},
        escalation_enabled: false,
      },
    };
    vi.mocked(api.getDashboard).mockResolvedValue(withIncident);
    const view = render(DashboardPage);
    await waitFor(() => expect(view.getByText("PC 02")).toBeTruthy());

    const row = within(
      view.container.querySelector('[data-pc-id="pc-02"]') as HTMLElement,
    );
    await waitFor(() =>
      expect(row.getByText("Client Reachability Mismatch")).toBeTruthy(),
    );
    // The clean PC has no badge.
    expect(view.queryByTestId("incident-badges-pc-01")).toBeNull();
  });

  it("explains a Host Probe Coverage badge as unmonitored, not offline", async () => {
    const withCoverageLoss: DashboardSnapshot = {
      ...dashboard,
      pc_monitoring: {
        enabled: true,
        active_incidents: {
          "pc-01": ["host_probe_coverage"],
        },
        suppressions: {},
        escalation_enabled: false,
      },
    };
    vi.mocked(api.getDashboard).mockResolvedValue(withCoverageLoss);
    const view = render(DashboardPage);
    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());

    const row = within(
      view.container.querySelector('[data-pc-id="pc-01"]') as HTMLElement,
    );
    const badge = row.getByText("Host Probe Coverage");
    // The hint states exactly what missing coverage does and does not mean.
    expect(badge.getAttribute("title")).toContain("unmonitored");
    expect(badge.getAttribute("title")).toContain("not offline or tampered");
  });
});

describe("Monitoring Suppression", () => {
  /** 2026-08-26T04:30:00Z, formatted like formatDate renders it locally. */
  const EXPIRY = "2026-08-26T04:30:00Z";

  const withSuppression = (pcId: string): DashboardSnapshot => ({
    ...dashboard,
    pc_monitoring: {
      enabled: true,
      active_incidents: {},
      suppressions: { [pcId]: EXPIRY },
      escalation_enabled: false,
    },
  });

  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getDashboard).mockResolvedValue(dashboard);
  });

  it("shows the absolute expiry badge and Cancel button only on suppressed PCs", async () => {
    vi.mocked(api.getDashboard).mockResolvedValue(withSuppression("pc-01"));
    const view = render(DashboardPage);
    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());

    const row = (pcId: string) =>
      view.container.querySelector(`[data-pc-id="${pcId}"]`) as HTMLElement;
    const suppressedRow = within(row("pc-01"));
    expect(
      suppressedRow.getByTestId("suppression-badge-pc-01").textContent,
    ).toContain("Suppressed until");
    expect(
      suppressedRow.getByRole("button", { name: "Cancel Suppression" }),
    ).toBeTruthy();

    // The unsuppressed PC offers suppression instead.
    const cleanRow = within(row("pc-02"));
    expect(cleanRow.queryByTestId("suppression-badge-pc-02")).toBeNull();
    expect(cleanRow.getByRole("button", { name: "Suppress" })).toBeTruthy();
    expect(
      cleanRow.queryByRole("button", { name: "Cancel Suppression" }),
    ).toBeNull();
  });

  it("creates a suppression with duration and PIN, then reloads", async () => {
    const user = userEvent.setup();
    vi.mocked(api.setPcMonitoringSuppression).mockResolvedValue({
      pc_id: "pc-01",
      pc_name: "PC 01",
      expires_at: EXPIRY,
    });
    const view = render(DashboardPage);
    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());

    await user.click(
      within(
        view.container.querySelector('[data-pc-id="pc-01"]') as HTMLElement,
      ).getByRole("button", { name: "Suppress" }),
    );
    expect(view.getByText("Suppress monitoring for PC 01?")).toBeTruthy();

    await user.selectOptions(view.getByLabelText("Duration"), "60");
    await user.type(view.getByLabelText("Current Web Admin PIN"), "owner-pin");
    // The dialog's confirm shares its label with the row button; take the
    // last match, which is always the dialog's.
    await user.click(
      view.getAllByRole("button", { name: "Suppress" }).at(-1) as HTMLElement,
    );

    await waitFor(() =>
      expect(api.setPcMonitoringSuppression).toHaveBeenCalledWith(
        "pc-01",
        60,
        "owner-pin",
      ),
    );
    expect(toast.success).toHaveBeenCalled();
    // Initial load plus the post-action refresh.
    await waitFor(() => expect(api.getDashboard).toHaveBeenCalledTimes(2));
  });

  it("reports a refused PIN through toast and keeps the dialog open", async () => {
    const user = userEvent.setup();
    vi.mocked(api.setPcMonitoringSuppression).mockRejectedValue(
      new Error("invalid owner PIN"),
    );
    const view = render(DashboardPage);
    await waitFor(() => expect(view.getByText("PC 01")).toBeTruthy());

    await user.click(
      within(
        view.container.querySelector('[data-pc-id="pc-01"]') as HTMLElement,
      ).getByRole("button", { name: "Suppress" }),
    );
    await user.type(view.getByLabelText("Current Web Admin PIN"), "wrong");
    await user.click(
      view.getAllByRole("button", { name: "Suppress" }).at(-1) as HTMLElement,
    );

    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(view.getByText(/invalid owner PIN/)).toBeTruthy();
    expect(api.getDashboard).toHaveBeenCalledTimes(1);
  });

  it("cancels a suppression with PIN step-up and reloads", async () => {
    const user = userEvent.setup();
    vi.mocked(api.getDashboard).mockResolvedValue(withSuppression("pc-02"));
    vi.mocked(api.cancelPcMonitoringSuppression).mockResolvedValue({
      cancelled: true,
    });
    const view = render(DashboardPage);
    await waitFor(() => expect(view.getByText("PC 02")).toBeTruthy());

    await user.click(view.getByRole("button", { name: "Cancel Suppression" }));
    expect(
      view.getByText("Cancel monitoring suppression for PC 02?"),
    ).toBeTruthy();
    await user.type(view.getByLabelText("Current Web Admin PIN"), "owner-pin");
    await user.click(
      view
        .getAllByRole("button", { name: "Cancel Suppression" })
        .at(-1) as HTMLElement,
    );

    await waitFor(() =>
      expect(api.cancelPcMonitoringSuppression).toHaveBeenCalledWith(
        "pc-02",
        "owner-pin",
      ),
    );
    expect(toast.success).toHaveBeenCalled();
    await waitFor(() => expect(api.getDashboard).toHaveBeenCalledTimes(2));
  });
});

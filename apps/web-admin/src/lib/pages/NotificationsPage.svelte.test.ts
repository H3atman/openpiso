// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  listNotifications: vi.fn(),
  markNotificationsSeen: vi.fn(),
  acknowledgeNotification: vi.fn(),
  getSupportBundle: vi.fn(),
}));

vi.mock("../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../api";
import { reportActionFailure, reportActionSuccess } from "../notifications";
import type { NotificationItem } from "../types";
import NotificationsPage from "./NotificationsPage.svelte";

// Section order matters to the Verdict extraction, mirroring the builder's
// output: Environment before Verdict, `\n\n---\n\n` separators, later `## `
// headings ending the extracted body.
const BUNDLE = [
  "# Support Bundle",
  "",
  "## Environment",
  "",
  "- Registered IP: 192.168.1.20",
  "",
  "---",
  "",
  "## Verdict",
  "",
  "Client Guard Coverage reads `unavailable` while the Guard reports the Client connected — the coverage contradiction.",
  "",
  "---",
  "",
  "## Timeline",
  "",
  "## Anomalies",
  "",
  "## Raw Appendix",
].join("\n");

function item(
  id: string,
  severity: NotificationItem["severity"],
): NotificationItem {
  const isAlert = severity === "alert";
  return {
    id,
    severity,
    event_type: isAlert ? "tamper_kill" : "backup_created",
    outcome: "success",
    actor: "system",
    actor_account_id: null,
    actor_account_username: null,
    account_id: null,
    account_username: null,
    pc_id: isAlert ? "pc-01" : null,
    pc_name: isAlert ? "PC 01" : null,
    amount_cents: null,
    seconds_delta: null,
    points_delta_centi: null,
    summary: `${severity} ${id}`,
    created_at: "2026-08-03 10:00:00",
    read: false,
    acknowledged_at: null,
  };
}

const alert = item("alert-1", "alert");
const noticeOne = item("notice-1", "notice");
const noticeTwo = item("notice-2", "notice");

beforeEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
  vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
    if (options.severity === "alert" && options.unread === true) {
      return { items: [alert], has_more: false };
    }
    if (options.severity === "alert" && options.unread === false) {
      return { items: [], has_more: false };
    }
    if (options.offset === 0) {
      return { items: [noticeOne], has_more: true };
    }
    return { items: [noticeTwo], has_more: false };
  });
  noticeTwo.read = true;
  vi.mocked(api.markNotificationsSeen).mockResolvedValue({
    last_seen_at: "2026-08-03 10:01:00",
  });
  vi.mocked(api.acknowledgeNotification).mockResolvedValue({
    acknowledged_at: "2026-08-03 10:02:00",
  });
  vi.mocked(api.getSupportBundle).mockResolvedValue(BUNDLE);
});

describe("NotificationsPage", () => {
  it("lists read and unread Notices, then advances the watermark", async () => {
    const user = userEvent.setup();
    const refreshed = vi.fn();
    const view = render(NotificationsPage, {
      props: { onNotificationsChanged: refreshed },
    });

    await waitFor(() =>
      expect(api.markNotificationsSeen).toHaveBeenCalledOnce(),
    );
    // No unread filter: an already-seen Notice stays browsable in the window.
    expect(api.listNotifications).toHaveBeenCalledWith({
      limit: 50,
      offset: 0,
      severity: "notice",
    });
    const seenCall = vi.mocked(api.markNotificationsSeen).mock.invocationCallOrder[0];
    const noticeCalls = vi.mocked(api.listNotifications).mock.invocationCallOrder;
    expect(seenCall).toBeGreaterThan(noticeCalls[1]);
    expect(refreshed).toHaveBeenCalledOnce();
    expect(view.getByText("notice notice-1")).toBeTruthy();

    await user.click(view.getByRole("button", { name: "Load more Notices" }));

    await waitFor(() => expect(view.getByText("notice notice-2")).toBeTruthy());
    expect(api.listNotifications).toHaveBeenCalledWith({
      limit: 50,
      offset: 1,
      severity: "notice",
    });
    // Only the unread Notice carries the "New" marker.
    expect(view.getAllByText("New")).toHaveLength(1);
  });

  it("renders Actor Account attribution in Alert and Notice summaries", async () => {
    const accountAlert = {
      ...alert,
      actor: "account",
      actor_account_id: "actor-account",
      actor_account_username: "juan",
      account_id: "target-account",
      account_username: "maria",
      summary: "Sent 1min 0sec to maria",
    };
    const accountNotice = {
      ...noticeOne,
      actor: "account",
      actor_account_id: "actor-account",
      actor_account_username: "juan",
      account_id: "target-account",
      account_username: "maria",
      summary: "Updated maria's points",
    };
    vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
      if (options.severity === "alert") {
        return { items: [accountAlert], has_more: false };
      }
      return { items: [accountNotice], has_more: false };
    });

    const view = render(NotificationsPage);

    await waitFor(() =>
      expect(view.getByText("Sent 1min 0sec to maria — by juan")).toBeTruthy(),
    );
    expect(view.getByText("Updated maria's points — by juan")).toBeTruthy();
    expect(view.getByText("By account")).toBeTruthy();
  });

  it("distinguishes all Tamper Kill Subjects and renders Target Account identity separately", async () => {
    const accountTamperKill = {
      ...alert,
      id: "tamper-account",
      account_id: "target-account",
      account_username: "maria",
      summary: "Tamper Kill on PC 01 during an Account session",
    };
    const guestTamperKill = {
      ...alert,
      id: "tamper-guest",
      account_id: null,
      account_username: null,
      summary: "Tamper Kill on PC 01 during a Guest session",
    };
    const noneTamperKill = {
      ...alert,
      id: "tamper-none",
      account_id: null,
      account_username: null,
      summary: "Tamper Kill on PC 01 with no active session",
    };
    const unknownTamperKill = {
      ...alert,
      id: "tamper-unknown",
      account_id: null,
      account_username: null,
      summary:
        "Tamper Kill on PC 01: Client did not report a Kill Report Age; it may be an older Client",
    };
    vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
      if (options.severity === "alert") {
        return {
          items: [accountTamperKill, guestTamperKill, noneTamperKill, unknownTamperKill],
          has_more: false,
        };
      }
      return { items: [], has_more: false };
    });

    const view = render(NotificationsPage);

    await waitFor(() =>
      expect(view.getByText("Tamper Kill on PC 01 during an Account session")).toBeTruthy(),
    );
    expect(view.getByText("Tamper Kill on PC 01 during a Guest session")).toBeTruthy();
    expect(view.getByText("Tamper Kill on PC 01 with no active session")).toBeTruthy();
    expect(
      view.getByText(
        "Tamper Kill on PC 01: Client did not report a Kill Report Age; it may be an older Client",
      ),
    ).toBeTruthy();
    expect(view.getByText("Account maria")).toBeTruthy();
    expect(view.queryByText(/session.*maria/i)).toBeNull();
  });

  it("renders an account summary without attribution when Actor Account is absent", async () => {
    const noActor = {
      ...alert,
      actor: "account",
      actor_account_id: null,
      actor_account_username: null,
      summary: "Account login denied",
    };
    vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
      if (options.severity === "alert") return { items: [noActor], has_more: false };
      return { items: [], has_more: false };
    });

    const view = render(NotificationsPage);

    await waitFor(() => expect(view.getByText("Account login denied")).toBeTruthy());
    expect(view.queryByText(/unknown account/)).toBeNull();
    expect(view.queryByText("Account login denied — by")).toBeNull();
  });

  it("shows PC Monitoring evidence and investigation guidance", async () => {
    const monitoringAlert: NotificationItem = {
      ...alert,
      event_type: "pc_monitoring_incident_opened",
      incident_kind: "client_reachability_mismatch",
      summary: "Client Reachability Mismatch detected on PC 01",
    };
    vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
      if (options.severity === "alert") {
        return { items: [monitoringAlert], has_more: false };
      }
      return { items: [], has_more: false };
    });

    const view = render(NotificationsPage);

    await waitFor(() =>
      expect(
        view.getByText(
          "Observed: the host answered probes while the Client remained unreachable for 60 seconds.",
        ),
      ).toBeTruthy(),
    );
    expect(view.getByRole("link", { name: "View PC 01" }).getAttribute("href")).toBe(
      "/admin/dashboard#pc-pc-01",
    );
    expect(view.getByText(/Remote Control is configured/)).toBeTruthy();
    expect(view.getByText(/temporary Monitoring Suppression/)).toBeTruthy();
    expect(view.getByText(/OpenPisoClientGuard service/)).toBeTruthy();
    expect(
      view.getByRole("link", { name: "Review PC Monitoring and installation guidance" }),
    ).toBeTruthy();
  });

  it("requests an incident-anchored Support Bundle from the Alert and previews it", async () => {
    const user = userEvent.setup();
    const monitoringAlert: NotificationItem = {
      ...alert,
      event_type: "pc_monitoring_incident_opened",
      incident_kind: "client_guard_coverage",
      summary: "Client Guard Coverage detected on PC 01",
    };
    vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
      if (options.severity === "alert") {
        return { items: [monitoringAlert], has_more: false };
      }
      return { items: [], has_more: false };
    });

    const view = render(NotificationsPage);
    await waitFor(() =>
      expect(view.getByRole("button", { name: "Get Support Bundle" })).toBeTruthy(),
    );

    await user.click(view.getByRole("button", { name: "Get Support Bundle" }));

    // The anchor is the incident's own moment, so the bundle covers that
    // hour-before-to-fifteen-after slice, not backwards from now.
    await waitFor(() =>
      expect(api.getSupportBundle).toHaveBeenCalledWith(
        "pc-01",
        undefined,
        "2026-08-03 10:00:00",
      ),
    );
    await waitFor(() =>
      expect(
        view.getByRole("region", { name: "Support Bundle preview" }),
      ).toBeTruthy(),
    );
    expect(view.getByText(/one hour before to fifteen minutes after/)).toBeTruthy();
    const verdictBlock = view.getByText("Verdict", { exact: true }).parentElement!;
    expect(verdictBlock.textContent).toContain("coverage contradiction");
    expect(
      view.getByRole("button", { name: "Download Support Bundle" }),
    ).toBeTruthy();
    expect(reportActionSuccess).toHaveBeenCalledWith("request", "Support Bundle");
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("reports a failed anchored bundle request and keeps the Alert unchanged", async () => {
    const user = userEvent.setup();
    vi.mocked(api.getSupportBundle).mockRejectedValue(new Error("PC not found"));
    const monitoringAlert: NotificationItem = {
      ...alert,
      event_type: "pc_monitoring_incident_opened",
      incident_kind: "client_guard_coverage",
      summary: "Client Guard Coverage detected on PC 01",
    };
    vi.mocked(api.listNotifications).mockImplementation(async (options = {}) => {
      if (options.severity === "alert") {
        return { items: [monitoringAlert], has_more: false };
      }
      return { items: [], has_more: false };
    });

    const view = render(NotificationsPage);
    await waitFor(() =>
      expect(view.getByRole("button", { name: "Get Support Bundle" })).toBeTruthy(),
    );

    await user.click(view.getByRole("button", { name: "Get Support Bundle" }));

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "request",
        "Support Bundle",
        expect.anything(),
      ),
    );
    expect(
      view.queryByRole("region", { name: "Support Bundle preview" }),
    ).toBeNull();
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });

  it("removes an acknowledged Alert and reports the action", async () => {
    const user = userEvent.setup();
    const refreshed = vi.fn();
    const view = render(NotificationsPage, {
      props: { onNotificationsChanged: refreshed },
    });
    await waitFor(() => expect(view.getByText("alert alert-1")).toBeTruthy());

    await user.click(view.getByRole("button", { name: "Acknowledge" }));

    await waitFor(() => expect(view.queryByText("alert alert-1")).toBeNull());
    expect(api.acknowledgeNotification).toHaveBeenCalledWith("alert-1");
    expect(reportActionSuccess).toHaveBeenCalledWith("acknowledge", "Alert");
    expect(refreshed).toHaveBeenCalled();
  });

  it("leaves an Alert active and reports acknowledgement failures", async () => {
    vi.mocked(api.acknowledgeNotification).mockRejectedValueOnce(
      new Error("offline"),
    );
    const user = userEvent.setup();
    const view = render(NotificationsPage);
    await waitFor(() => expect(view.getByText("alert alert-1")).toBeTruthy());

    await user.click(view.getByRole("button", { name: "Acknowledge" }));

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "acknowledge",
        "Alert",
        expect.any(Error),
      ),
    );
    expect(view.getByText("alert alert-1")).toBeTruthy();
  });

  it("loads acknowledged history on demand with the read filter", async () => {
    const user = userEvent.setup();
    const acknowledged = {
      ...alert,
      id: "ack-1",
      summary: "acknowledged alert",
      read: true,
      acknowledged_at: "2026-08-03 10:03:00",
    };
    vi.mocked(api.listNotifications).mockImplementation(
      async (options = {}) => {
        if (options.unread === false)
          return { items: [acknowledged], has_more: false };
        if (options.severity === "alert")
          return { items: [alert], has_more: false };
        return { items: [], has_more: false };
      },
    );
    const view = render(NotificationsPage);
    await waitFor(() => expect(view.getByText("alert alert-1")).toBeTruthy());

    await user.click(view.getByRole("button", { name: /Show acknowledged/ }));

    await waitFor(() =>
      expect(view.getByText("acknowledged alert")).toBeTruthy(),
    );
    expect(api.listNotifications).toHaveBeenCalledWith({
      limit: 50,
      offset: 0,
      severity: "alert",
      unread: false,
    });
  });
});

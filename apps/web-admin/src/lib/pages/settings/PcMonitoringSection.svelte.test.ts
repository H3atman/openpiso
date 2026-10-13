// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getPcMonitoringStatus: vi.fn(),
  setPcMonitoringEnabled: vi.fn(),
  // The PC Monitoring tab also renders Client Recovery Escalation (ADR-0077).
  getClientRecoveryEscalation: vi.fn(),
  setClientRecoveryEscalation: vi.fn(),
}));

vi.mock("../../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../../api";
import { reportActionSuccess } from "../../notifications";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

function renderSection() {
  return render(SettingsPageHarness, { props: { section: "pc-monitoring" } });
}

describe("PC Monitoring Settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getPcMonitoringStatus).mockResolvedValue({
      enabled: false,
      platform_supported: true,
    });
    vi.mocked(api.setPcMonitoringEnabled).mockResolvedValue({
      enabled: true,
      platform_supported: true,
    });
    vi.mocked(api.getClientRecoveryEscalation).mockResolvedValue({
      enabled: false,
      delay_secs: 60,
    });
  });

  it("renders the disabled state with the standing-advisory hint", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(
        view.getByText(/standing advisory/i, { exact: false }),
      ).toBeTruthy(),
    );
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });

  it("explains that missing Host Probe Coverage means unmonitored", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByText("Host Probe Coverage", { exact: false })).toBeTruthy(),
    );

    const explainer = view.getByText(/Host Probe Coverage/i).closest("p")!;
    expect(explainer.textContent).toContain("unmonitored");
    expect(explainer.textContent).toContain("not offline or tampered");
  });

  it("enabling requires the current PIN and reports success", async () => {
    const view = renderSection();
    await waitFor(() => expect(view.getByRole("switch")).toBeTruthy());

    await userEvent.click(view.getByRole("switch"));

    // The PIN dialog gates the write; nothing is sent before it.
    expect(api.setPcMonitoringEnabled).not.toHaveBeenCalled();

    await userEvent.type(
      view.getByLabelText("Current Web Admin PIN"),
      "1234",
    );
    await userEvent.click(view.getByRole("button", { name: "Enable" }));

    await waitFor(() =>
      expect(api.setPcMonitoringEnabled).toHaveBeenCalledWith(true, "1234"),
    );
    await waitFor(() => expect(reportActionSuccess).toHaveBeenCalled());
  });

  it("renders the unsupported-platform advisory without offering a toggle write", async () => {
    vi.mocked(api.getPcMonitoringStatus).mockResolvedValue({
      enabled: false,
      platform_supported: false,
    });
    const view = renderSection();

    await waitFor(() =>
      expect(
        view.getByText(/requires a Windows Admin/i),
      ).toBeTruthy(),
    );
    expect(view.queryByRole("switch")).toBeNull();
  });

  it("shows no PIN dialog on mount when monitoring is already enabled", async () => {
    vi.mocked(api.getPcMonitoringStatus).mockResolvedValue({
      enabled: true,
      platform_supported: true,
    });
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByText("On", { exact: true })).toBeTruthy(),
    );
    expect(view.queryByText("Disable PC Monitoring?")).toBeNull();
    expect(view.queryByLabelText("Current Web Admin PIN")).toBeNull();
  });

  it("renders the enabled state", async () => {
    vi.mocked(api.getPcMonitoringStatus).mockResolvedValue({
      enabled: true,
      platform_supported: true,
    });
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByText("On", { exact: true })).toBeTruthy(),
    );
  });
});

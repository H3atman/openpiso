// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getPcMonitoringStatus: vi.fn(),
  setPcMonitoringEnabled: vi.fn(),
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

describe("Client Recovery Escalation Settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getPcMonitoringStatus).mockResolvedValue({
      enabled: true,
      platform_supported: true,
    });
    vi.mocked(api.getClientRecoveryEscalation).mockResolvedValue({
      enabled: false,
      delay_secs: 60,
    });
    vi.mocked(api.setClientRecoveryEscalation).mockResolvedValue({
      enabled: true,
      delay_secs: 60,
    });
  });

  it("says plainly that the PC is powered off and nothing is confiscated", async () => {
    const view = renderSection();
    // The card is only complete once its delay control is on screen; the
    // heading also renders in the not-yet-available state.
    await waitFor(() => expect(view.getByLabelText(/escalation delay/i)).toBeTruthy());

    const explainer = view.getByText(/relaunches an absent Client/i).closest("p")!;
    expect(explainer.textContent).toContain("powers that PC off");
    expect(explainer.textContent).toContain("balance, time, and Points");
    expect(explainer.textContent).toContain("one power off per PC per hour");
  });

  it("enabling requires the current PIN and reports success", async () => {
    const user = userEvent.setup();
    const view = renderSection();
    await waitFor(() => expect(view.getByLabelText(/escalation delay/i)).toBeTruthy());

    const switches = view.getAllByRole("switch");
    await user.click(switches[switches.length - 1]);

    const pin = await view.findByLabelText(/current web admin pin/i);
    await user.type(pin, "1234");
    await user.click(view.getByRole("button", { name: /^save$/i }));

    await waitFor(() =>
      expect(api.setClientRecoveryEscalation).toHaveBeenCalledWith(
        true,
        60,
        "1234",
      ),
    );
    expect(reportActionSuccess).toHaveBeenCalledWith(
      "save",
      "Client Recovery Escalation",
    );
  });

  it("refuses a delay below the floor without asking for a PIN", async () => {
    const user = userEvent.setup();
    const view = renderSection();
    await waitFor(() => expect(view.getByLabelText(/escalation delay/i)).toBeTruthy());

    const delay = view.getByLabelText(/escalation delay/i);
    await user.clear(delay);
    await user.type(delay, "5");

    const save = view.getByRole("button", { name: /save delay/i });
    expect(save.hasAttribute("disabled")).toBe(true);
    expect(api.setClientRecoveryEscalation).not.toHaveBeenCalled();
  });

  it("shows the standing advisory only while escalation is on", async () => {
    vi.mocked(api.getClientRecoveryEscalation).mockResolvedValue({
      enabled: true,
      delay_secs: 90,
    });
    const view = renderSection();
    await waitFor(() =>
      expect(
        view.getByText(/authorised to power customer PCs off/i),
      ).toBeTruthy(),
    );
  });
});

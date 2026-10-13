// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getAutostart: vi.fn(),
  updateAutostart: vi.fn(),
}));

vi.mock("../../theme", () => ({ applyThemeRecord: vi.fn() }));

vi.mock("../../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../../api";
import { reportActionSuccess, reportActionFailure } from "../../notifications";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

function renderSection() {
  return render(SettingsPageHarness, { props: { section: "autostart" } });
}

describe("Autostart Settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getAutostart).mockResolvedValue({
      enabled: false,
      supported: true,
      editable: true,
      exe_path: "C:\\openpiso-admin.exe",
    });
    vi.mocked(api.updateAutostart).mockResolvedValue({
      enabled: true,
      supported: true,
      editable: true,
      exe_path: "C:\\openpiso-admin.exe",
    });
  });

  it("renders the enabled state and executable path", async () => {
    vi.mocked(api.getAutostart).mockResolvedValue({
      enabled: true,
      supported: true,
      editable: true,
      exe_path: "C:\\openpiso-admin.exe",
    });
    const view = renderSection();
    await waitFor(() => expect(view.getByText("Yes")).toBeTruthy());
    expect(view.getByText("C:\\openpiso-admin.exe")).toBeTruthy();
  });

  it("renders the disabled state", async () => {
    const view = renderSection();
    await waitFor(() => expect(view.getByText("No")).toBeTruthy());
  });

  it("renders the not-editable inline advisory", async () => {
    vi.mocked(api.getAutostart).mockResolvedValue({
      enabled: false,
      supported: true,
      editable: false,
      exe_path: "C:\\openpiso-admin.exe",
    });
    const view = renderSection();
    await waitFor(() =>
      expect(
        view.getByText("read-only in this build", { exact: false }),
      ).toBeTruthy(),
    );
    // The toggle is rendered but inert.
    await userEvent.click(view.getByRole("switch"));
    expect(api.updateAutostart).not.toHaveBeenCalled();
  });

  it("renders the not-supported advisory when platform lacks autostart", async () => {
    vi.mocked(api.getAutostart).mockResolvedValue({
      enabled: false,
      supported: false,
      editable: false,
      exe_path: "C:\\openpiso-admin.exe",
    });
    const view = renderSection();
    await waitFor(() =>
      expect(
        view.getByText("not supported on this platform", { exact: false }),
      ).toBeTruthy(),
    );
    // Both reasons render when both are false — neither suppresses the other.
    expect(
      view.getByText("read-only in this build", { exact: false }),
    ).toBeTruthy();
    // The Windows-specific panel stays hidden on an unsupported platform.
    expect(view.queryByText("Windows autologon", { exact: false })).toBeNull();
    expect(view.queryByText("C:\\openpiso-admin.exe")).toBeNull();

    await userEvent.click(view.getByRole("switch"));
    expect(api.updateAutostart).not.toHaveBeenCalled();
  });

  it("distinguishes Autostart from the Watchdog and the Client Guard", async () => {
    const view = renderSection();
    await waitFor(() => expect(view.getByText("Watchdog")).toBeTruthy());
    expect(view.getByText("Client Guard")).toBeTruthy();
  });

  it("renders the logon-vs-boot advisory", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(
        view.getByText("Logon scope", { exact: false }),
      ).toBeTruthy(),
    );
    expect(
      view.getByText("Windows autologon", { exact: false }),
    ).toBeTruthy();
  });

  it("reports a load failure inline, not as a toast", async () => {
    vi.mocked(api.getAutostart).mockRejectedValue(new Error("Network error"));
    const view = renderSection();
    await waitFor(() => expect(view.getByText("Network error")).toBeTruthy());
  });

  it("calls updateAutostart and reports success on toggle", async () => {
    vi.mocked(api.getAutostart).mockResolvedValue({
      enabled: false,
      supported: true,
      editable: true,
      exe_path: "C:\\openpiso-admin.exe",
    });
    vi.mocked(api.updateAutostart).mockResolvedValue({
      enabled: true,
      supported: true,
      editable: true,
      exe_path: "C:\\openpiso-admin.exe",
    });
    const view = renderSection();
    // Wait for the component to load so the switch appears.
    await waitFor(() => expect(view.getByText("Off")).toBeTruthy());

    const switchControl = view.getByRole("switch");
    await userEvent.click(switchControl);

    await waitFor(() => {
      expect(api.updateAutostart).toHaveBeenCalledWith({ enabled: true });
      expect(reportActionSuccess).toHaveBeenCalledWith("save", "Autostart");
    });
  });

  it("reports failure and reverts toggle on a rejected update", async () => {
    vi.mocked(api.getAutostart).mockResolvedValue({
      enabled: false,
      supported: true,
      editable: true,
      exe_path: "C:\\openpiso-admin.exe",
    });
    vi.mocked(api.updateAutostart).mockRejectedValue(
      new Error("Autostart is not editable in this build"),
    );
    const view = renderSection();
    await waitFor(() => expect(view.getByText("Off")).toBeTruthy());

    const switchControl = view.getByRole("switch");
    await userEvent.click(switchControl);

    await waitFor(() => {
      expect(api.updateAutostart).toHaveBeenCalledWith({ enabled: true });
      expect(reportActionFailure).toHaveBeenCalledWith(
        "save",
        "Autostart",
        expect.any(Error),
      );
    });

    // After the failure, getAutostart is called again to re-read the
    // machine state. The mock still returns disabled, so the toggle
    // reverts to showing "Off".
    await waitFor(() => expect(view.getByText("Off")).toBeTruthy());
  });
});

// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getDiagnosticsMode: vi.fn(),
  setDiagnosticsMode: vi.fn(),
  setOwnerPin: vi.fn(),
  rotateWebAdminPin: vi.fn(),
}));

vi.mock("../../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../../api";
import { reportActionFailure, reportActionSuccess } from "../../notifications";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

function renderSection() {
  return render(SettingsPageHarness, { props: { section: "security" } });
}

describe("Security Settings — Diagnostics Mode", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getDiagnosticsMode).mockResolvedValue({ enabled: false });
    vi.mocked(api.setDiagnosticsMode).mockResolvedValue({ enabled: true });
  });

  it("reflects the stored value, toggles it, and reports success", async () => {
    const view = renderSection();
    const control = await waitFor(() => view.getByRole("switch"));
    expect(control.getAttribute("aria-checked")).toBe("false");

    await userEvent.click(control);

    await waitFor(() =>
      expect(api.setDiagnosticsMode).toHaveBeenCalledWith(true),
    );
    await waitFor(() => expect(reportActionSuccess).toHaveBeenCalled());
    // The switch reflects the server's stored state, not the request.
    await waitFor(() =>
      expect(view.getByRole("switch").getAttribute("aria-checked")).toBe("true"),
    );
  });

  it("reflects the enabled value the server reports after a reload", async () => {
    vi.mocked(api.getDiagnosticsMode).mockResolvedValue({ enabled: true });
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByRole("switch").getAttribute("aria-checked")).toBe("true"),
    );
  });

  it("reports failure and keeps the stored state when the write is refused", async () => {
    vi.mocked(api.setDiagnosticsMode).mockRejectedValue(new Error("refused"));
    const view = renderSection();
    const control = await waitFor(() => view.getByRole("switch"));

    await userEvent.click(control);

    await waitFor(() => expect(reportActionFailure).toHaveBeenCalled());
    expect(view.getByRole("switch").getAttribute("aria-checked")).toBe("false");
  });

  it("names a failed read inline and recovers on retry", async () => {
    vi.mocked(api.getDiagnosticsMode).mockRejectedValueOnce(
      new Error("host unreachable"),
    );
    const view = renderSection();

    const message = await waitFor(() => view.getByText(/host unreachable/));
    expect(message).toBeTruthy();
    expect(view.queryByRole("switch")).toBeNull();

    vi.mocked(api.getDiagnosticsMode).mockResolvedValue({ enabled: true });
    await userEvent.click(view.getByRole("button", { name: "Retry" }));

    await waitFor(() =>
      expect(view.getByRole("switch").getAttribute("aria-checked")).toBe("true"),
    );
    expect(view.queryByText(/host unreachable/)).toBeNull();
  });
});

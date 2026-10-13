// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api")>()),
  getAttendanceSettings: vi.fn(),
  previewAttendanceSettings: vi.fn(),
  updateAttendanceSettings: vi.fn(),
}));
vi.mock("../../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportAttendanceSettingsOutcome: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { fireEvent, render, waitFor } from "@testing-library/svelte";
import * as api from "../../api";
import {
  reportActionFailure,
  reportAttendanceSettingsOutcome,
} from "../../notifications";
import AttendanceSection from "./AttendanceSection.svelte";

const settings = {
  version: 3,
  enabled: true,
  daily_threshold_cents: 1_000,
  reward_seconds: 3_600,
  claim_window_months: 1,
  effective_at: "2026-09-01T00:00:00Z",
  economics_advisory: {
    daily_threshold_cents: 1_000,
    reward_seconds: 3_600,
    rate_amount_cents: 1_000,
    rate_seconds: 3_000,
    effective_bonus: {
      kind: "finite" as const,
      base_seconds: 3_000,
      basis_points: 12_000,
    },
    unsafe_bonus: false,
  },
  liability: { outstanding_seconds: 90_000, claimable_seconds: 45_000 },
  next_earning_month: "2026-10",
  effective_changes: {
    enabled: "immediately_for_new_contributions" as const,
    daily_threshold_cents: "next_unfrozen_daily_snapshot" as const,
    reward_minutes: "next_unfrozen_daily_snapshot" as const,
    claim_window_months: "next_earning_month" as const,
  },
};

const preview = {
  enabled: true,
  daily_threshold_cents: 2_000,
  reward_seconds: 1_800,
  claim_window_months: 2,
  economics_advisory: {
    daily_threshold_cents: 2_000,
    reward_seconds: 1_800,
    rate_amount_cents: 1_000,
    rate_seconds: 3_000,
    effective_bonus: {
      kind: "finite" as const,
      base_seconds: 6_000,
      basis_points: 3_000,
    },
    unsafe_bonus: false,
  },
};

describe("Attendance settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getAttendanceSettings).mockResolvedValue(settings);
    vi.mocked(api.previewAttendanceSettings).mockResolvedValue(preview);
    vi.mocked(api.updateAttendanceSettings).mockResolvedValue({
      ...settings,
      version: 4,
      daily_threshold_cents: 2_000,
      reward_seconds: 1_800,
      claim_window_months: 2,
      outcome: "updated",
    });
  });

  it("shows current exposure and previews a two-pane draft before committing", async () => {
    const view = render(AttendanceSection);

    await waitFor(() => expect(api.getAttendanceSettings).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByLabelText("Daily threshold (pesos)")).toBeTruthy());
    await waitFor(() => expect(view.getByText("Configure Attendance Reward")).toBeTruthy());
    expect(view.getByText("Outstanding Attendance Time")).toBeTruthy();
    expect(view.getByText("25 hours")).toBeTruthy();
    expect(view.getByText("120.00%", { selector: "p" })).toBeTruthy();
    expect(view.getByLabelText("Active Attendance Effective Bonus").tagName).toBe("DEL");
    expect(view.getByText(/₱10 buys 50 min; Attendance Reward gives 1 hour/)).toBeTruthy();
    expect(view.getByText("Default: ₱10")).toBeTruthy();
    expect(view.getByText(/Existing daily snapshots are unchanged/)).toBeTruthy();
    expect(view.getByText(/Claim window: next Earning Month \(2026-10\)/)).toBeTruthy();
    const effectiveChanges = [...view.getByRole("list").querySelectorAll("li")].map((item) =>
      item.textContent?.trim(),
    );
    expect(effectiveChanges).toEqual([
      "Enable or disable: immediately for new contributions",
      "Daily threshold: at the next unfrozen daily snapshot",
      "Reward minutes: at the next unfrozen daily snapshot",
      "Claim window: next Earning Month (2026-10)",
    ]);

    const commit = view.getByRole("button", { name: "Commit v4" });
    expect((commit as HTMLButtonElement).disabled).toBe(true);

    await fireEvent.input(view.getByLabelText("Daily threshold (pesos)"), {
      target: { value: "20" },
    });
    await fireEvent.input(view.getByLabelText("Reward minutes"), {
      target: { value: "30" },
    });
    await fireEvent.input(view.getByLabelText("Claim window (months)"), {
      target: { value: "2" },
    });

    await waitFor(() =>
      expect(api.previewAttendanceSettings).toHaveBeenLastCalledWith({
        enabled: true,
        daily_threshold_cents: 2_000,
        reward_minutes: 30,
        claim_window_months: 2,
      }),
    );
    expect(view.getByText("30.00%", { exact: true })).toBeTruthy();
    expect((commit as HTMLButtonElement).disabled).toBe(false);

    await fireEvent.click(commit);
    await waitFor(() =>
      expect(api.updateAttendanceSettings).toHaveBeenCalledWith({
        enabled: true,
        daily_threshold_cents: 2_000,
        reward_minutes: 30,
        claim_window_months: 2,
        expected_version: 3,
      }),
    );
    expect(reportAttendanceSettingsOutcome).toHaveBeenCalledWith("updated");
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("sends numeric drafts to preview and leaves server validation inline", async () => {
    vi.mocked(api.previewAttendanceSettings).mockRejectedValueOnce(
      new Error("invalid input: daily threshold must be positive"),
    );
    const view = render(AttendanceSection);
    await waitFor(() => expect(api.getAttendanceSettings).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByLabelText("Daily threshold (pesos)")).toBeTruthy());

    await fireEvent.input(view.getByLabelText("Daily threshold (pesos)"), {
      target: { value: "0" },
    });
    await waitFor(() =>
      expect(api.previewAttendanceSettings).toHaveBeenCalledWith({
        enabled: true,
        daily_threshold_cents: 0,
        reward_minutes: 60,
        claim_window_months: 1,
      }),
    );
    expect(view.getByText("invalid input: daily threshold must be positive")).toBeTruthy();
    expect(view.getByRole("button", { name: "Commit v4" }).hasAttribute("disabled")).toBe(true);
    expect(api.updateAttendanceSettings).not.toHaveBeenCalled();
  });

  it("preserves a draft on a stale commit and reports the refreshed version", async () => {
    vi.mocked(api.updateAttendanceSettings).mockRejectedValue(
      new api.AttendanceSettingsConflictError(4),
    );
    vi.mocked(api.getAttendanceSettings)
      .mockResolvedValueOnce(settings)
      .mockResolvedValueOnce({ ...settings, version: 4, daily_threshold_cents: 1_500 });
    const view = render(AttendanceSection);
    await waitFor(() => expect(api.getAttendanceSettings).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByLabelText("Daily threshold (pesos)")).toBeTruthy());

    await fireEvent.input(view.getByLabelText("Daily threshold (pesos)"), {
      target: { value: "20" },
    });
    await waitFor(() => expect(api.previewAttendanceSettings).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        (view.getByRole("button", { name: "Commit v4" }) as HTMLButtonElement).disabled,
      ).toBe(false),
    );
    await fireEvent.click(view.getByRole("button", { name: "Commit v4" }));

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "save",
        "Attendance",
        expect.any(api.AttendanceSettingsConflictError),
      ),
    );
    await waitFor(() => expect(view.getByRole("button", { name: "Commit v5" })).toBeTruthy());
    expect(api.getAttendanceSettings).toHaveBeenCalledTimes(2);
    expect((view.getByLabelText("Daily threshold (pesos)") as HTMLInputElement).value).toBe("20");
  });

  it("reports unchanged saves distinctly from updated saves", async () => {
    vi.mocked(api.updateAttendanceSettings).mockResolvedValueOnce({
      ...settings,
      outcome: "unchanged",
    });
    const view = render(AttendanceSection);
    await waitFor(() => expect(api.getAttendanceSettings).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByLabelText("Daily threshold (pesos)")).toBeTruthy());
    await fireEvent.input(view.getByLabelText("Daily threshold (pesos)"), {
      target: { value: "20" },
    });
    await waitFor(() => expect(api.previewAttendanceSettings).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        (view.getByRole("button", { name: "Commit v4" }) as HTMLButtonElement).disabled,
      ).toBe(false),
    );
    await fireEvent.click(view.getByRole("button", { name: "Commit v4" }));

    await waitFor(() => expect(reportAttendanceSettingsOutcome).toHaveBeenCalledWith("unchanged"));
  });

  it("reports a server-invalid commit as a distinct failure", async () => {
    vi.mocked(api.updateAttendanceSettings).mockRejectedValueOnce(
      new api.AttendanceSettingsValidationError("invalid input: threshold overflow"),
    );
    const view = render(AttendanceSection);
    await waitFor(() => expect(api.getAttendanceSettings).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByLabelText("Daily threshold (pesos)")).toBeTruthy());
    await fireEvent.input(view.getByLabelText("Daily threshold (pesos)"), {
      target: { value: "20" },
    });
    await waitFor(() => expect(api.previewAttendanceSettings).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        (view.getByRole("button", { name: "Commit v4" }) as HTMLButtonElement).disabled,
      ).toBe(false),
    );
    await fireEvent.click(view.getByRole("button", { name: "Commit v4" }));

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "save",
        "Attendance",
        expect.any(api.AttendanceSettingsValidationError),
      ),
    );
  });
});

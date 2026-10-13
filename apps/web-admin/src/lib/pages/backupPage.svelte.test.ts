// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";

vi.mock("../api", () => ({
  listBackups: vi.fn(async () => [
    {
      filename: "openpiso-manual-20260730T120000Z-v54.sql",
      kind: "manual",
      created_at: "2026-07-30T12:00:00Z",
      schema_version: 54,
      size_bytes: 2048,
      backup_sets: ["Accounts", "Configuration"],
      catch_up: false,
    },
  ]),
  createBackup: vi.fn(async (backup_sets: string[]) => ({
    filename: "openpiso-manual-20260730T120001Z-v54.sql",
    size_bytes: backup_sets.length * 100,
  })),
  downloadBackupUrl: vi.fn((filename: string) => `/web-admin/backups/${filename}`),
  deleteBackup: vi.fn(async () => ({ ok: true })),
  getBackupSchedule: vi.fn(async () => ({
    enabled: true,
    frequency: "daily",
    minute_of_day: 240,
    weekday: 0,
    day_of_month: 1,
    backup_sets: ["Accounts", "Configuration", "Ledger", "Vouchers", "Terminals", "Runtime State", "Credentials"],
    retention_count: 14,
    mirror_directory: null,
    mirror_error: null,
    next_due_at: "2026-08-01T04:00:00+08:00",
  })),
  updateBackupSchedule: vi.fn(async (input) => ({
    ...input,
    next_due_at: "2026-08-01T04:00:00+08:00",
  })),
  getPendingRestore: vi.fn(async () => null),
  stageRestore: vi.fn(async () => ({
    filename: "openpiso-manual-20260730T120000Z-v54.sql",
    schema_version: 54,
    requested_at: "2026-07-30T12:00:00Z",
    mirror_error: null,
  })),
  uploadBackup: vi.fn(async (file: File) => ({
    filename: file.name,
    schema_version: 54,
    requested_at: "2026-07-30T12:00:00Z",
    mirror_error: null,
  })),
  cancelPendingRestore: vi.fn(async () => ({ cancelled: true })),
  RestoreRejectedError: class MockRestoreRejectedError extends Error {
    gate: string;
    blockingPcs: string[];
    constructor(gate: string, message: string, blockingPcs: string[]) {
      super(message);
      this.gate = gate;
      this.blockingPcs = blockingPcs;
    }
  },
}));

import * as api from "../api";
import BackupPageHarness from "./BackupPageHarness.svelte";

describe("BackupPage", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
  });

  it("selects all seven Backup Sets by default and allows changing selection", async () => {
    const user = userEvent.setup();
    const view = render(BackupPageHarness);
    await waitFor(() => expect(view.getByText("openpiso-manual-20260730T120000Z-v54.sql")).toBeTruthy());

    const accounts = view.getByRole("checkbox", { name: "Accounts" }) as HTMLInputElement;
    expect(accounts.checked).toBe(true);
    expect(view.getAllByRole("checkbox")).toHaveLength(14);

    await user.click(accounts);
    expect(accounts.checked).toBe(false);
    await user.click(view.getByRole("checkbox", { name: "Accounts" }));
    await user.click(view.getByRole("button", { name: "Create Backup" }));
    await waitFor(() => expect(api.createBackup).toHaveBeenCalledWith([
      "Configuration",
      "Ledger",
      "Vouchers",
      "Terminals",
      "Runtime State",
      "Credentials",
      "Accounts",
    ]));
  });

  it("reveals frequency-dependent fields and submits the schedule payload", async () => {
    const user = userEvent.setup();
    const view = render(BackupPageHarness);
    await waitFor(() => expect(view.getByLabelText("Frequency")).toBeTruthy());

    await user.selectOptions(view.getByLabelText("Frequency"), "weekly");
    expect(view.getByLabelText("Day of week")).toBeTruthy();
    expect(view.queryByLabelText("Day of month")).toBeNull();
    await user.selectOptions(view.getByLabelText("Frequency"), "monthly");
    expect(view.getByLabelText("Day of month")).toBeTruthy();
    expect(view.queryByLabelText("Day of week")).toBeNull();

    await user.click(view.getByRole("button", { name: "Save Backup Schedule" }));
    await waitFor(() => expect(api.updateBackupSchedule).toHaveBeenCalledWith(expect.objectContaining({
      frequency: "monthly",
      day_of_month: 1,
      minute_of_day: 240,
      retention_count: 14,
      backup_sets: expect.arrayContaining(["Accounts", "Credentials"]),
    })));
  });

  it("renders a mirror failure as an inline advisory", async () => {
    vi.mocked(api.getBackupSchedule).mockResolvedValueOnce({
      enabled: true,
      frequency: "daily",
      minute_of_day: 240,
      weekday: 0,
      day_of_month: 1,
      backup_sets: ["Accounts"],
      retention_count: 14,
      mirror_directory: "D:/backups",
      mirror_error: "filesystem error: Permission denied",
      next_due_at: "2026-08-01T04:00:00+08:00",
    });

    const view = render(BackupPageHarness);
    const advisory = await waitFor(() => view.getByRole("status"));

    expect(advisory.textContent).toContain("Mirror Directory copy failed: filesystem error: Permission denied");
    expect(advisory.closest("section")?.getAttribute("aria-labelledby")).toBe("backup-schedule-heading");
    expect(view.queryByRole("alert")).toBeNull();
  });

  it("requires confirmation before staging a restore", async () => {
    const user = userEvent.setup();
    const view = render(BackupPageHarness);
    await waitFor(() => expect(view.getByText("openpiso-manual-20260730T120000Z-v54.sql")).toBeTruthy());

    await user.click(view.getByRole("button", { name: "Restore" }));
    expect(view.getByRole("dialog").textContent).toContain("replace the live database");
    expect(api.stageRestore).not.toHaveBeenCalled();
    await user.click(view.getByRole("button", { name: "Confirm Restore" }));
    await waitFor(() => expect(api.stageRestore).toHaveBeenCalledWith({
      filename: "openpiso-manual-20260730T120000Z-v54.sql",
    }));
  });

  it("requires confirmation before staging a browser upload", async () => {
    const user = userEvent.setup();
    const view = render(BackupPageHarness);
    await waitFor(() => expect(view.getByLabelText("SQL Backup file")).toBeTruthy());

    const file = new File(["not a Backup"], "owner-backup.sql", { type: "application/sql" });
    await user.upload(view.getByLabelText("SQL Backup file"), file);
    await user.click(view.getByRole("button", { name: "Review Upload Restore" }));
    expect(view.getByRole("dialog").textContent).toContain("owner-backup.sql");
    expect(api.uploadBackup).not.toHaveBeenCalled();
    await user.click(view.getByRole("button", { name: "Confirm Restore" }));
    await waitFor(() => expect(api.uploadBackup).toHaveBeenCalledWith(file));
  });

  it("renders blocking PCs returned by the restore refusal", async () => {
    const user = userEvent.setup();
    vi.mocked(api.stageRestore).mockRejectedValueOnce(
      new api.RestoreRejectedError("precondition", "Restore is blocked", ["PC 01", "PC 02"]),
    );
    const view = render(BackupPageHarness);
    await waitFor(() => expect(view.getByText("openpiso-manual-20260730T120000Z-v54.sql")).toBeTruthy());
    await user.click(view.getByRole("button", { name: "Restore" }));
    await user.click(view.getByRole("button", { name: "Confirm Restore" }));
    expect(await view.findByText("Blocking PCs: PC 01, PC 02")).toBeTruthy();
  });

  it("renders the filesystem catalog with metadata and actions", async () => {
    const view = render(BackupPageHarness);
    await waitFor(() => expect(view.getByText("openpiso-manual-20260730T120000Z-v54.sql")).toBeTruthy());

    expect(view.getByText("manual")).toBeTruthy();
    expect(view.getByText("v54")).toBeTruthy();
    expect(view.getByText("2.0 KB")).toBeTruthy();
    expect(view.getByText("Accounts, Configuration")).toBeTruthy();
    expect(view.getByRole("link", { name: "Download" }).getAttribute("href")).toBe(
      "/web-admin/backups/openpiso-manual-20260730T120000Z-v54.sql",
    );
  });
});

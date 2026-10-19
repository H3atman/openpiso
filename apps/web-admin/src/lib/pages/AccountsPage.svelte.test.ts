// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  listAccounts: vi.fn(async () => []),
  deleteAccounts: vi.fn(async () => ({ deleted_count: 1 })),
  createAccount: vi.fn(async () => ({
    id: "account-1",
    username: "Customer",
    status: "active",
    remaining_seconds: 0,
    points_balance_centi: 0,
    created_at: "2099-01-01T00:00:00Z",
    last_login_at: null,
  })),
  importAccounts: vi.fn(async () => ({
    rows: [],
    totals: { created: 0, skipped: 0, total_seconds: 0, total_points_centi: 0 },
  })),
}));

vi.mock("../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { render, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../api";
import { formatDate } from "../format";
import { reportActionFailure, reportActionSuccess } from "../notifications";
import type { Account } from "../types";
import { createMatchMedia } from "../../test/match-media";
import AccountsPage from "./AccountsPage.svelte";

afterEach(() => {
  vi.unstubAllGlobals();
});

function pagingAccount(id: string, status: Account["status"] = "active"): Account {
  return {
    id,
    username: id,
    status,
    remaining_seconds: 60,
    points_balance_centi: 100,
    created_at: "2025-01-01T00:00:00Z",
    deleted_at: status === "deleted" ? "2025-02-01T00:00:00Z" : null,
    last_login_at: "2025-01-02T03:04:00Z",
    imported_from: null,
    imported_at: null,
    legacy_not_logged_in: false,
  };
}

function mockAccounts() {
  vi.mocked(api.listAccounts).mockResolvedValue([
    {
      id: "1",
      username: "juan_smith",
      status: "active" as const,
      remaining_seconds: 0,
      points_balance_centi: 0,
      created_at: "2025-01-01T00:00:00Z",
      deleted_at: null,
      last_login_at: null,
      imported_from: null,
      imported_at: null,
      legacy_not_logged_in: false,
    },
    {
      id: "2",
      username: "maria_garcia",
      status: "active" as const,
      remaining_seconds: 120,
      points_balance_centi: 50,
      created_at: "2025-01-02T00:00:00Z",
      deleted_at: null,
      last_login_at: null,
      imported_from: null,
      imported_at: null,
      legacy_not_logged_in: false,
    },
    {
      id: "3",
      username: "carlos",
      status: "active" as const,
      remaining_seconds: 300,
      points_balance_centi: 0,
      created_at: "2025-01-03T00:00:00Z",
      deleted_at: null,
      last_login_at: null,
      imported_from: null,
      imported_at: null,
      legacy_not_logged_in: false,
    },
  ]);
}

describe("AccountsPage search leniency", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    mockAccounts();
  });

  it("matches underscored username when searching with a space", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    const searchInput = view.getByPlaceholderText(
      "Search username…",
    ) as HTMLInputElement;
    const user = userEvent.setup();
    await user.type(searchInput, "juan smith");
    await waitFor(() => {
      expect(view.getByText("juan_smith")).toBeTruthy();
    });
    expect(() => view.getByText("maria_garcia")).toThrow();
  });

  it("matches underscored username when searching with underscore directly", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    const searchInput = view.getByPlaceholderText(
      "Search username…",
    ) as HTMLInputElement;
    const user = userEvent.setup();
    await user.type(searchInput, "maria_garcia");
    await waitFor(() => {
      expect(view.getByText("maria_garcia")).toBeTruthy();
    });
  });

  it("collapses multiple spaces to single underscore in search", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    const searchInput = view.getByPlaceholderText(
      "Search username…",
    ) as HTMLInputElement;
    const user = userEvent.setup();
    await user.type(searchInput, "maria   garcia");
    await waitFor(() => {
      expect(view.getByText("maria_garcia")).toBeTruthy();
    });
  });

  it("shows no-match message when search does not match", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    const searchInput = view.getByPlaceholderText(
      "Search username…",
    ) as HTMLInputElement;
    const user = userEvent.setup();
    await user.type(searchInput, "zzz_nonexistent");
    await waitFor(() => {
      expect(view.getByText("No accounts match your search.")).toBeTruthy();
    });
  });
});

describe("AccountsPage visibility and sorting", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.listAccounts).mockResolvedValue([
      {
        id: "zulu",
        username: "zulu",
        status: "active",
        remaining_seconds: 30,
        points_balance_centi: 200,
        created_at: "2025-03-01T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
      {
        id: "alpha",
        username: "alpha",
        status: "active",
        remaining_seconds: 10,
        points_balance_centi: 100,
        created_at: "2025-01-01T00:00:00Z",
        deleted_at: null,
        last_login_at: "2025-02-01T00:00:00Z",
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
      {
        id: "bravo",
        username: "bravo",
        status: "deleted",
        remaining_seconds: 20,
        points_balance_centi: 0,
        created_at: "2025-02-01T00:00:00Z",
        deleted_at: "2025-02-02T00:00:00Z",
        last_login_at: "2025-03-01T00:00:00Z",
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
      {
        id: "charlie",
        username: "charlie",
        status: "active",
        remaining_seconds: 20,
        points_balance_centi: 100,
        created_at: "2025-02-02T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
    ]);
  });

  function rowNames(container: HTMLElement): string[] {
    return Array.from(container.querySelectorAll("tbody tr")).map(
      (row) => row.querySelector("td")?.textContent?.trim() ?? "",
    );
  }

  it("shows one Account status at a time and trims the deleted view", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());

    await waitFor(() =>
      expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]),
    );
    expect(view.getAllByText("Never").length).toBe(2);
    expect(
      view.getByRole("columnheader", { name: /Account Created/ }),
    ).toBeTruthy();
    expect(view.getByRole("columnheader", { name: /Last Login/ })).toBeTruthy();

    await userEvent.setup().selectOptions(view.getByLabelText("Status"), "deleted");

    expect(rowNames(view.container)).toEqual(["bravo"]);
    const deletedRow = view.getByText("bravo").closest("tr") as HTMLElement;
    expect(within(deletedRow).getByText(/Mar 1, 2025/)).toBeTruthy();
    expect(within(deletedRow).getByText(/Feb 2, 2025/)).toBeTruthy();

    // Deleted Accounts are read-only history: no balances, no selection, no actions.
    expect(view.getByRole("columnheader", { name: /^Deleted/ })).toBeTruthy();
    expect(view.queryByRole("columnheader", { name: /Remaining Time/ })).toBeNull();
    expect(view.queryByRole("columnheader", { name: /^Points/ })).toBeNull();
    expect(view.queryByRole("columnheader", { name: /Actions/ })).toBeNull();
    expect(view.queryAllByRole("checkbox")).toEqual([]);
    expect(view.queryByRole("button", { name: "Add/Remove Time" })).toBeNull();
  });

  it("pages matching Accounts and focuses the first newly revealed Account", async () => {
    vi.mocked(api.listAccounts).mockResolvedValue(
      Array.from({ length: 21 }, (_, index) => pagingAccount(`account_${index + 1}`)),
    );
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(rowNames(view.container)).toHaveLength(10));

    expect(view.getByRole("button", { name: "Load More (10 of 21)" })).toBeTruthy();
    const header = view.getByRole("checkbox", {
      name: "Select visible active Accounts",
    }) as HTMLInputElement;
    await user.click(header);
    expect(view.getByRole("button", { name: "Delete Selected (10)" })).toBeTruthy();

    await user.click(view.getByRole("button", { name: "Load More (10 of 21)" }));

    await waitFor(() => {
      expect(rowNames(view.container)).toHaveLength(20);
      expect(document.activeElement?.id).toBe("account-row-account_19");
      expect(view.getByRole("button", { name: "Delete Selected (10)" })).toBeTruthy();
      expect(header.checked).toBe(false);
    });
    await user.click(view.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledTimes(2));
    expect(rowNames(view.container)).toHaveLength(20);

    await user.click(view.getByRole("button", { name: "Load More (20 of 21)" }));

    expect(rowNames(view.container)).toHaveLength(21);
    expect(view.queryByRole("button", { name: /Load More/ })).toBeNull();
  });

  it("resets paging for search, sort, and status changes", async () => {
    vi.mocked(api.listAccounts).mockResolvedValue([
      ...Array.from({ length: 25 }, (_, index) => pagingAccount(`active_${index + 1}`)),
      ...Array.from({ length: 12 }, (_, index) => pagingAccount(`deleted_${index + 1}`, "deleted")),
    ]);
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(rowNames(view.container)).toHaveLength(10));

    await user.click(view.getByRole("button", { name: "Load More (10 of 25)" }));
    expect(rowNames(view.container)).toHaveLength(20);

    const search = view.getByPlaceholderText("Search username…");
    await user.type(search, "active");
    expect(view.getByRole("button", { name: "Load More (10 of 25)" })).toBeTruthy();

    await user.click(view.getByRole("button", { name: "Load More (10 of 25)" }));
    await user.click(view.getByRole("button", { name: /^Username/ }));
    expect(view.getByRole("button", { name: "Load More (10 of 25)" })).toBeTruthy();

    await user.clear(search);
    await user.selectOptions(view.getByLabelText("Status"), "deleted");
    expect(view.getByRole("button", { name: "Load More (10 of 12)" })).toBeTruthy();
    expect(rowNames(view.container)).toHaveLength(10);
  });

  it("renders complete Account details on mobile cards", async () => {
    vi.stubGlobal("matchMedia", createMatchMedia(true));
    vi.mocked(api.listAccounts).mockResolvedValue([pagingAccount("mobile_account")]);
    const view = render(AccountsPage);
    await waitFor(() => expect(view.getByText("mobile_account")).toBeTruthy());

    const card = within(view.getByRole("article", { name: "mobile_account" }));
    expect(card.getByText("Remaining Time")).toBeTruthy();
    expect(card.getByText("00:01:00")).toBeTruthy();
    expect(card.getByText("Points")).toBeTruthy();
    expect(card.getByText("1.00")).toBeTruthy();
    expect(card.getByText("Account Created")).toBeTruthy();
    expect(card.getByText(formatDate("2025-01-01T00:00:00Z"))).toBeTruthy();
    expect(card.getByText("Last Login")).toBeTruthy();
    expect(card.getByText(formatDate("2025-01-02T03:04:00Z"))).toBeTruthy();
  });

  it("sorts every active column and keeps null Last Login values last", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]),
    );

    await user.click(view.getByRole("button", { name: /^Remaining Time/ }));
    expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]);
    await user.click(view.getByRole("button", { name: /^Remaining Time/ }));
    expect(rowNames(view.container)).toEqual(["zulu", "charlie", "alpha"]);

    await user.click(view.getByRole("button", { name: /^Points/ }));
    expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]);
    await user.click(view.getByRole("button", { name: /^Points/ }));
    expect(rowNames(view.container)).toEqual(["zulu", "alpha", "charlie"]);

    await user.click(view.getByRole("button", { name: /^Account Created/ }));
    expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]);
    await user.click(view.getByRole("button", { name: /^Account Created/ }));
    expect(rowNames(view.container)).toEqual(["zulu", "charlie", "alpha"]);

    // Accounts that never logged in stay last in both directions.
    await user.click(view.getByRole("button", { name: /^Last Login/ }));
    expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]);
    await user.click(view.getByRole("button", { name: /^Last Login/ }));
    expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]);
  });

  it("keeps the search across views but resets sort and selection", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    await user.click(view.getByRole("button", { name: /^Account Created/ }));
    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    expect(view.getByRole("button", { name: "Delete Selected (1)" })).toBeTruthy();

    const search = view.getByPlaceholderText("Search username…");
    await user.type(search, "bravo");
    const emptyState = view.getByText(/No accounts match your search\./);
    expect(emptyState.textContent).toMatch(/1 deleted account\s+matches your search\./);

    await user.selectOptions(view.getByLabelText("Status"), "deleted");
    // The search survives the switch; the selection does not.
    expect((search as HTMLInputElement).value).toBe("bravo");
    expect(rowNames(view.container)).toEqual(["bravo"]);
    expect(view.queryByRole("button", { name: /Delete Selected/ })).toBeNull();

    await user.clear(search);
    await user.selectOptions(view.getByLabelText("Status"), "active");
    // Sort is back to its Username ascending default, not Account Created.
    expect(rowNames(view.container)).toEqual(["alpha", "charlie", "zulu"]);
    expect(view.queryByRole("button", { name: /Delete Selected/ })).toBeNull();
  });

  it("names the empty deleted view without borrowing the active wording", async () => {
    const user = userEvent.setup();
    vi.mocked(api.listAccounts).mockResolvedValue([]);
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("No accounts yet.")).toBeTruthy());

    await user.selectOptions(view.getByLabelText("Status"), "deleted");
    expect(view.getByText("No deleted accounts.")).toBeTruthy();

    await user.type(view.getByPlaceholderText("Search username…"), "zzz");
    expect(view.getByText("No deleted accounts match your search.")).toBeTruthy();
  });

  it("does not claim the shop has no accounts when every account is deleted", async () => {
    vi.mocked(api.listAccounts).mockResolvedValue([
      {
        id: "bravo",
        username: "bravo",
        status: "deleted",
        remaining_seconds: 0,
        points_balance_centi: 0,
        created_at: "2025-02-01T00:00:00Z",
        deleted_at: "2025-02-02T00:00:00Z",
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
    ]);
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("No active accounts.")).toBeTruthy());
  });
});

describe("AccountsPage bulk selection and deletion", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.listAccounts).mockResolvedValue([
      {
        id: "alpha",
        username: "alpha",
        status: "active",
        remaining_seconds: 10,
        points_balance_centi: 0,
        created_at: "2025-01-01T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
      {
        id: "bravo",
        username: "bravo",
        status: "deleted",
        remaining_seconds: 0,
        points_balance_centi: 0,
        created_at: "2025-01-02T00:00:00Z",
        deleted_at: "2025-01-03T00:00:00Z",
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
      {
        id: "charlie",
        username: "charlie",
        status: "active",
        remaining_seconds: 20,
        points_balance_centi: 0,
        created_at: "2025-01-04T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
      {
        id: "zulu",
        username: "zulu",
        status: "active",
        remaining_seconds: 30,
        points_balance_centi: 0,
        created_at: "2025-01-05T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
    ]);
    vi.mocked(api.deleteAccounts).mockResolvedValue({ deleted_count: 1 });
  });

  it("selects and deselects inclusive ranges of the visible rows", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await user.keyboard("{Shift>}");
    await user.click(view.getByRole("checkbox", { name: "Select Account zulu" }));
    await user.keyboard("{/Shift}");
    expect(view.getByRole("button", { name: "Delete Selected (3)" })).toBeTruthy();

    await user.keyboard("{Shift>}");
    await user.click(view.getByRole("checkbox", { name: "Select Account charlie" }));
    await user.keyboard("{/Shift}");
    expect(view.getByRole("button", { name: "Delete Selected (1)" })).toBeTruthy();
  });

  it("keeps hidden selections, limits select-all to visible active rows, and clears on refresh", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    const search = view.getByPlaceholderText("Search username…");
    await user.type(search, "zulu");
    await user.click(view.getByRole("checkbox", { name: "Select visible active Accounts" }));
    expect(view.getByRole("button", { name: "Delete Selected (2)" })).toBeTruthy();

    await user.clear(search);
    await user.click(view.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledTimes(2));
    expect(view.queryByRole("button", { name: /Delete Selected/ })).toBeNull();
  });

  it("submits the explicit selection and reports the deleted count", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());
    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await user.click(view.getByRole("button", { name: "Delete Selected (1)" }));
    const warning = view.getByText(/forfeits Remaining Time and Points/);
    expect(warning.textContent).toContain("Username reusable only by Account Import");
    expect(warning.textContent).not.toContain("alpha");
    await user.click(view.getByRole("button", { name: "Delete 1 Account" }));

    await waitFor(() => {
      expect(api.deleteAccounts).toHaveBeenCalledWith(["alpha"]);
      expect(api.listAccounts).toHaveBeenCalledTimes(2);
      expect(reportActionSuccess).toHaveBeenCalledWith("delete", "1 Account");
      expect(view.queryByRole("button", { name: /Delete Selected/ })).toBeNull();
    });
  });

  it("keeps the selection and reports an all-or-nothing failure", async () => {
    const user = userEvent.setup();
    vi.mocked(api.deleteAccounts).mockRejectedValueOnce(new Error("active account"));
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());
    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await user.click(view.getByRole("button", { name: "Delete Selected (1)" }));
    await user.click(view.getByRole("button", { name: "Delete 1 Account" }));

    await waitFor(() => {
      expect(reportActionFailure).toHaveBeenCalledWith(
        "delete",
        "Accounts",
        expect.any(Error),
      );
      expect(view.getByRole("button", { name: "Delete Selected (1)" })).toBeTruthy();
    });
  });

  it("shows header partial state and clears selection", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    const header = view.getByRole("checkbox", {
      name: "Select visible active Accounts",
    }) as HTMLInputElement;
    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await waitFor(() => expect(header.indeterminate).toBe(true));
    await user.click(header);
    await waitFor(() => {
      expect(header.indeterminate).toBe(false);
      expect(view.getByRole("button", { name: "Delete Selected (3)" })).toBeTruthy();
    });
    await user.click(header);
    expect(view.queryByRole("button", { name: /Delete Selected/ })).toBeNull();

    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await user.click(view.getByRole("button", { name: "Clear Selection" }));
    expect(view.queryByRole("button", { name: /Delete Selected/ })).toBeNull();
  });

  it("adds and removes ranges without losing selections outside them", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await user.click(view.getByRole("checkbox", { name: "Select Account zulu" }));
    await user.keyboard("{Shift>}");
    await user.click(view.getByRole("checkbox", { name: "Select Account charlie" }));
    await user.keyboard("{/Shift}");
    expect(view.getByRole("button", { name: "Delete Selected (3)" })).toBeTruthy();

    await user.keyboard("{Shift>}");
    await user.click(view.getByRole("checkbox", { name: "Select Account charlie" }));
    await user.keyboard("{/Shift}");
    expect(view.getByRole("button", { name: "Delete Selected (1)" })).toBeTruthy();
  });

  // Regression: the handlers used to call `event.preventDefault()`. A canceled
  // click makes the browser restore `checked` *after* Svelte has flushed the new
  // value, so the row stayed selected while its check mark disappeared. jsdom
  // cannot reproduce the ordering, so assert the property that causes it.
  it("never cancels a selection click, so the browser keeps the check mark", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    const row = view.getByRole("checkbox", {
      name: "Select Account alpha",
    }) as HTMLInputElement;
    const header = view.getByRole("checkbox", {
      name: "Select visible active Accounts",
    }) as HTMLInputElement;

    for (const box of [row, header]) {
      const event = new MouseEvent("click", { bubbles: true, cancelable: true });
      box.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(false);
    }

    await waitFor(() =>
      expect(view.getByRole("button", { name: /Delete Selected/ })).toBeTruthy(),
    );
  });

  it("clears the range anchor when sorting changes the visible view", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.getByText("alpha")).toBeTruthy());

    await user.click(view.getByRole("checkbox", { name: "Select Account alpha" }));
    await user.click(view.getByRole("button", { name: /^Username/ }));
    await user.keyboard("{Shift>}");
    await user.click(view.getByRole("checkbox", { name: "Select Account zulu" }));
    await user.keyboard("{/Shift}");
    expect(view.getByRole("button", { name: "Delete Selected (2)" })).toBeTruthy();
  });
});

describe("AccountsPage account import preview", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.listAccounts).mockResolvedValue([]);
    vi.mocked(api.importAccounts).mockResolvedValue({
      rows: [
        {
          outcome: "created",
          original_username: "CleanUser",
          remaining_seconds: 90,
          points_balance_centi: 125,
        },
        {
          outcome: "created",
          original_username: "Spaced User",
          final_username: "Spaced_User",
          remaining_seconds: 0,
          points_balance_centi: 0,
        },
        {
          outcome: "skipped",
          original_username: "Disabled",
          remaining_seconds: 60,
          points_balance_centi: 0,
          skip_reason: "disabled_at_source",
        },
      ],
      totals: {
        created: 2,
        skipped: 1,
        total_seconds: 90,
        total_points_centi: 125,
      },
    });
  });

  it("uploads the selected export as a dry-run and renders grouped rows", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());

    await user.click(view.getByRole("button", { name: "Import Accounts" }));
    const payload = {
      CleanUser: {
        id: "1",
        password:
          "$2b$12$.....................................................",
        credits: 1.5,
        points: 1.25,
        enabled: true,
        last: "2026-01-01",
      },
    };
    const file = new File([JSON.stringify(payload)], "AccountsData.json", {
      type: "application/json",
    });
    await user.upload(view.getByLabelText("Martec account export"), file);

    await waitFor(() => {
      expect(api.importAccounts).toHaveBeenCalledWith(payload, true);
      expect(view.getAllByText("Would create").length).toBe(2);
      expect(view.getByText("Spaced_User")).toBeTruthy();
      expect(view.getByText("Renamed from Spaced User")).toBeTruthy();
      expect(view.getByText("disabled_at_source")).toBeTruthy();
    });

    await user.click(view.getByRole("button", { name: "Confirm Import" }));
    await waitFor(() => {
      expect(api.importAccounts).toHaveBeenLastCalledWith(payload, false);
      expect(reportActionSuccess).toHaveBeenCalledWith("import", "Accounts");
    });
  });

  it("cancels without making an import request or reloading the list", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    const listCalls = vi.mocked(api.listAccounts).mock.calls.length;

    await user.click(view.getByRole("button", { name: "Import Accounts" }));
    await user.click(view.getByRole("button", { name: "Cancel" }));

    expect(api.importAccounts).not.toHaveBeenCalled();
    expect(api.listAccounts).toHaveBeenCalledTimes(listCalls);
  });

  it("reports failed previews through the existing failure path", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());
    vi.mocked(api.importAccounts).mockRejectedValueOnce(
      new Error("not a Martec export"),
    );

    await user.click(view.getByRole("button", { name: "Import Accounts" }));
    const file = new File(["{}"], "wrong.json", { type: "application/json" });
    await user.upload(view.getByLabelText("Martec account export"), file);

    await waitFor(() => {
      expect(reportActionFailure).toHaveBeenCalledWith(
        "import",
        "Accounts",
        expect.any(Error),
      );
      expect(view.getByText("not a Martec export")).toBeTruthy();
    });
  });
});

describe("AccountsPage account provenance", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.listAccounts).mockResolvedValue([
      {
        id: "legacy",
        username: "legacy_user",
        status: "active",
        remaining_seconds: 60,
        points_balance_centi: 100,
        created_at: "2025-01-01T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: "martec",
        imported_at: "2025-01-01T00:00:00Z",
        legacy_not_logged_in: true,
      },
      {
        id: "migrated",
        username: "migrated_user",
        status: "active",
        remaining_seconds: 60,
        points_balance_centi: 100,
        created_at: "2025-01-01T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: "martec",
        imported_at: "2025-01-01T00:00:00Z",
        legacy_not_logged_in: false,
      },
      {
        id: "native",
        username: "native_user",
        status: "active",
        remaining_seconds: 0,
        points_balance_centi: 0,
        created_at: "2025-01-01T00:00:00Z",
        deleted_at: null,
        last_login_at: null,
        imported_from: null,
        imported_at: null,
        legacy_not_logged_in: false,
      },
    ]);
  });

  it("shows permanent legacy and pending-login badges only for imported accounts", async () => {
    const view = render(AccountsPage);
    await waitFor(() => expect(view.getAllByText("Legacy")).toHaveLength(2));

    const legacyRow = view.getByText("legacy_user").closest("tr");
    const migratedRow = view.getByText("migrated_user").closest("tr");
    const nativeRow = view.getByText("native_user").closest("tr");
    expect(legacyRow).not.toBeNull();
    expect(migratedRow).not.toBeNull();
    expect(nativeRow).not.toBeNull();

    expect(within(legacyRow as HTMLElement).getByText("Legacy")).toBeTruthy();
    expect(
      within(legacyRow as HTMLElement).getByText("Not yet logged in"),
    ).toBeTruthy();
    expect(within(migratedRow as HTMLElement).getByText("Legacy")).toBeTruthy();
    expect(
      within(migratedRow as HTMLElement).queryByText("Not yet logged in"),
    ).toBeNull();
    expect(within(nativeRow as HTMLElement).queryByText("Legacy")).toBeNull();
    expect(
      within(nativeRow as HTMLElement).queryByText("Not yet logged in"),
    ).toBeNull();
  });
});

describe("AccountsPage account creation", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
  });

  it("creates an account with the default password without requiring a custom password", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());

    await user.click(view.getByRole("button", { name: "Create Account" }));
    await user.type(view.getByPlaceholderText("Username"), "Customer");
    await user.click(
      view.getByRole("button", { name: "Create with Default Password" }),
    );

    await waitFor(() => {
      expect(api.createAccount).toHaveBeenCalledWith("Customer", "1234567890");
    });
  });

  it("creates an account with the default password while ignoring custom password text", async () => {
    const user = userEvent.setup();
    const view = render(AccountsPage);
    await waitFor(() => expect(api.listAccounts).toHaveBeenCalledOnce());

    await user.click(view.getByRole("button", { name: "Create Account" }));
    await user.type(view.getByPlaceholderText("Username"), "Customer");
    await user.type(view.getByPlaceholderText("Password"), "custom-password");

    expect(
      view.getByText(
        "Default password: 1234567890. Customer should change it after login.",
      ),
    ).toBeTruthy();
    await user.click(
      view.getByRole("button", { name: "Create with Default Password" }),
    );

    await waitFor(() => {
      expect(api.createAccount).toHaveBeenCalledWith("Customer", "1234567890");
      expect(reportActionSuccess).toHaveBeenCalledWith(
        "create",
        "Account",
        "default-password",
      );
    });
  });
});

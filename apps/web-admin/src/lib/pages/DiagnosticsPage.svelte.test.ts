// @vitest-environment jsdom
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  listPcSettings: vi.fn(),
  getSupportBundle: vi.fn(),
  requestDiagnostics: vi.fn(),
  getCoinDebugBundle: vi.fn(),
  getCoinDebugWindow: vi.fn(),
  resetCoinDebugWindow: vi.fn(),
  getHostDiagnostics: vi.fn(),
}));

vi.mock("../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../api";
import { formatTransactionTime } from "../format";
import { reportActionFailure, reportActionSuccess } from "../notifications";
import type { HostDiagnostics } from "../types";
import DiagnosticsPage from "./DiagnosticsPage.svelte";

// Section order matters to the Verdict extraction: Environment before
// Verdict, with the builder's `\n\n---\n\n` separators and later `## `
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
  "| Time | Source | Line |",
  "",
  "## Anomalies",
  "",
  "## Raw Appendix",
].join("\n");

// Same heading/separator shape as the real Coin Debug Bundle builder
// (Environment before Verdict, `\n\n---\n\n` separators).
const COIN_BUNDLE = [
  "# Coin Debug Bundle",
  "",
  "## Environment",
  "",
  "- Admin version: 0.9.10",
  "",
  "---",
  "",
  "## Verdict",
  "",
  "No known coin-path failure shape matched the evidence.",
  "",
  "---",
  "",
  "## Timeline",
  "",
  "| Time | Coin Trace Id | Amount |",
  "",
  "## Anomalies",
  "",
  "## Raw Appendix",
].join("\n");

// Run Records newest first with the suspected-hard-exit classification the
// endpoint applies, plus a panic file with an inline slice (ADR-0082).
const HOST: HostDiagnostics = {
  runs: [
    {
      version: "0.9.10",
      pid: 4242,
      started_at: "2026-09-09T01:15:00Z",
      ended_at: null,
      exit_reason: null,
      suspected_hard_exit: false,
    },
    {
      version: "0.9.10",
      pid: 4241,
      started_at: "2026-09-09T00:05:00Z",
      ended_at: null,
      exit_reason: null,
      suspected_hard_exit: true,
    },
    {
      version: "0.9.9",
      pid: 4240,
      started_at: "2026-09-08T22:00:00Z",
      ended_at: "2026-09-08T23:59:00Z",
      exit_reason: "window_closed",
      suspected_hard_exit: false,
    },
  ],
  panic_file: {
    exists: true,
    size_bytes: 40,
    last_modified_at: "2026-09-08T23:00:00Z",
    newest_8kb: "panic: index out of bounds\n",
  },
};

const HOST_EMPTY: HostDiagnostics = {
  runs: [],
  panic_file: {
    exists: false,
    size_bytes: 0,
    last_modified_at: null,
    newest_8kb: null,
  },
};

function renderPage(props: {
  enabled: boolean | null;
  loadError?: string;
  onRetry?: () => void;
}) {
  return render(DiagnosticsPage, {
    props: { onRetry: () => {}, ...props },
  });
}

/** Stubs the object-URL + anchor download path the preview uses, since the
 *  jsdom origin has neither object URLs wired to a disk nor a clipboard. */
function stubBundleDownload() {
  const createObjectURL = vi.fn((_blob: Blob) => "blob:support-bundle");
  const revokeObjectURL = vi.fn();
  const original = URL.createObjectURL;
  URL.createObjectURL = createObjectURL as typeof URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  URL.revokeObjectURL = revokeObjectURL as typeof URL.revokeObjectURL;
  let downloaded: HTMLAnchorElement | null = null;
  const click = HTMLAnchorElement.prototype.click;
  HTMLAnchorElement.prototype.click = function clickSpy(
    this: HTMLAnchorElement,
  ) {
    downloaded = this;
  };
  return {
    createObjectURL,
    get downloaded() {
      return downloaded;
    },
    restore() {
      URL.createObjectURL = original;
      URL.revokeObjectURL = originalRevoke;
      HTMLAnchorElement.prototype.click = click;
    },
  };
}

async function requestBundleForChosenPc() {
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PC" }),
    "pc-01",
  );
  await userEvent.click(screen.getByRole("button", { name: "Request Bundle" }));
  await waitFor(() => expect(api.getSupportBundle).toHaveBeenCalled());
}

describe("Diagnostics page (ADR-0081)", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    window.history.replaceState(null, "", "/admin/diagnostics");
    vi.clearAllMocks();
    vi.mocked(api.listPcSettings).mockResolvedValue([
      {
        id: "pc-01",
        name: "PC 01",
        visible: true,
        registered_ip: "192.168.1.20",
        mac_address: "AA:BB:CC:DD:EE:01",
      },
    ]);
    vi.mocked(api.getSupportBundle).mockResolvedValue(BUNDLE);
    vi.mocked(api.requestDiagnostics).mockResolvedValue({ ok: true });
    vi.mocked(api.getCoinDebugBundle).mockResolvedValue(COIN_BUNDLE);
    vi.mocked(api.getCoinDebugWindow).mockResolvedValue({
      since: "2026-08-12T10:00:00Z",
    });
    vi.mocked(api.resetCoinDebugWindow).mockResolvedValue({
      since: "2026-08-12T12:00:00Z",
    });
    vi.mocked(api.getHostDiagnostics).mockResolvedValue(HOST);
  });

  afterEach(() => {
    cleanup();
  });

  it("renders the Client, Coin, and Server tabs in that order", () => {
    renderPage({ enabled: true });

    expect(
      screen.getAllByRole("tab").map((tab) => tab.textContent?.trim()),
    ).toEqual(["Client", "Coin", "Server"]);
  });

  it("carries a standing advisory naming Diagnostics Mode while on", () => {
    const view = renderPage({ enabled: true });

    expect(view.getByRole("status").textContent).toContain(
      "Diagnostics Mode is on",
    );
    expect(view.getByRole("status").textContent).toContain(
      "Settings → Security",
    );
  });

  it("keeps the page on one route while switching tabs", async () => {
    renderPage({ enabled: true });

    await userEvent.click(screen.getByRole("tab", { name: "Server" }));
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));

    expect(window.location.pathname).toBe("/admin/diagnostics");
  });

  it("keeps each tab's working surface intact while switching", async () => {
    renderPage({ enabled: true });

    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    expect(
      screen.getByRole("button", { name: "Request Coin Debug Bundle" }),
    ).toBeTruthy();
    await userEvent.click(screen.getByRole("tab", { name: "Server" }));
    await waitFor(() => expect(api.getHostDiagnostics).toHaveBeenCalled());
    expect(screen.getByRole("heading", { name: "Panic file" })).toBeTruthy();
    // Back on Client, the working surface is intact.
    await userEvent.click(screen.getByRole("tab", { name: "Client" }));
    expect(
      screen.getByRole("heading", { name: "Support Bundle" }),
    ).toBeTruthy();
  });

  it("requests the Coin Debug Bundle and previews it inline", async () => {
    renderPage({ enabled: true });

    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    await userEvent.click(
      screen.getByRole("button", { name: "Request Coin Debug Bundle" }),
    );

    await waitFor(() => expect(api.getCoinDebugBundle).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        screen.getByRole("region", { name: "Coin Debug Bundle preview" }),
      ).toBeTruthy(),
    );
    // The Support Bundle stays absent — the two artifacts never blur.
    expect(
      screen.queryByRole("region", { name: "Support Bundle preview" }),
    ).toBeNull();
    expect(reportActionSuccess).toHaveBeenCalledWith(
      "request",
      "Coin Debug Bundle",
    );
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("downloads the Coin Debug Bundle as its own .md file", async () => {
    const stub = stubBundleDownload();

    try {
      renderPage({ enabled: true });
      await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
      await userEvent.click(
        screen.getByRole("button", { name: "Request Coin Debug Bundle" }),
      );
      await screen.findByRole("region", { name: "Coin Debug Bundle preview" });

      await userEvent.click(
        screen.getByRole("button", { name: "Download Coin Debug Bundle" }),
      );

      await waitFor(() => expect(stub.createObjectURL).toHaveBeenCalled());
      expect(stub.downloaded!.download).toBe("coin-debug-bundle.md");
      const blob = stub.createObjectURL.mock.calls[0][0];
      expect(blob.type).toBe("text/markdown");
      expect(await blob.text()).toBe(COIN_BUNDLE);
      expect(reportActionSuccess).toHaveBeenCalledWith(
        "download",
        "Coin Debug Bundle",
      );
      // Owner Session only, like every Support surface.
      expect(screen.queryByLabelText(/pin/i)).toBeNull();
    } finally {
      stub.restore();
    }
  });

  it("copies the Coin Debug Bundle to the clipboard without rendering a preview", async () => {
    // jsdom has no Clipboard API and no execCommand — the same shape as the
    // plain-HTTP LAN origin the fallback branch exists for.
    const doc = document as Document & {
      execCommand?: (commandId: string) => boolean;
    };
    const originalExecCommand = doc.execCommand;
    const execCommand = vi.fn(() => true);
    doc.execCommand = execCommand;

    try {
      renderPage({ enabled: true });
      await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
      await userEvent.click(
        screen.getByRole("button", { name: "Copy Coin Debug Bundle" }),
      );

      await waitFor(() => expect(api.getCoinDebugBundle).toHaveBeenCalled());
      expect(execCommand).toHaveBeenCalledWith("copy");
      await waitFor(() =>
        expect(reportActionSuccess).toHaveBeenCalledWith(
          "copy",
          "Coin Debug Bundle",
        ),
      );
      expect(reportActionFailure).not.toHaveBeenCalled();
      expect(
        screen.queryByRole("region", { name: "Coin Debug Bundle preview" }),
      ).toBeNull();
    } finally {
      if (originalExecCommand) doc.execCommand = originalExecCommand;
      else delete (doc as { execCommand?: unknown }).execCommand;
    }
  });

  it("reports a failed Coin Debug Bundle request via toast and leaves the preview absent", async () => {
    vi.mocked(api.getCoinDebugBundle).mockRejectedValue(
      new Error("Owner Session expired"),
    );
    renderPage({ enabled: true });
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    await userEvent.click(
      screen.getByRole("button", { name: "Request Coin Debug Bundle" }),
    );

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "request",
        "Coin Debug Bundle",
        expect.anything(),
      ),
    );
    expect(
      screen.queryByRole("region", { name: "Coin Debug Bundle preview" }),
    ).toBeNull();
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });

  it("reads the Coin Debug Window when the Coin tab first opens, not before", async () => {
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    expect(api.getCoinDebugWindow).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    await waitFor(() => expect(api.getCoinDebugWindow).toHaveBeenCalled());
    expect(
      screen.getByText(
        `Coin Debug Window start ${formatTransactionTime("2026-08-12T10:00:00Z")}`,
      ),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Reset Coin Debug Window" }),
    ).toBeTruthy();
  });

  it("resets the Coin Debug Window from the Coin tab", async () => {
    renderPage({ enabled: true });
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    await waitFor(() => expect(api.getCoinDebugWindow).toHaveBeenCalled());

    await userEvent.click(
      screen.getByRole("button", { name: "Reset Coin Debug Window" }),
    );

    await waitFor(() => expect(api.resetCoinDebugWindow).toHaveBeenCalled());
    expect(
      screen.getByText(
        `Coin Debug Window start ${formatTransactionTime("2026-08-12T12:00:00Z")}`,
      ),
    ).toBeTruthy();
    expect(reportActionSuccess).toHaveBeenCalledWith(
      "reset",
      "Coin Debug Window",
    );
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("reports a failed window reset via toast and keeps the previous start", async () => {
    vi.mocked(api.resetCoinDebugWindow).mockRejectedValue(
      new Error("Owner Session expired"),
    );
    renderPage({ enabled: true });
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Reset Coin Debug Window" }),
      ).toBeTruthy(),
    );

    await userEvent.click(
      screen.getByRole("button", { name: "Reset Coin Debug Window" }),
    );

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "reset",
        "Coin Debug Window",
        expect.anything(),
      ),
    );
    expect(
      screen.getByText(
        `Coin Debug Window start ${formatTransactionTime("2026-08-12T10:00:00Z")}`,
      ),
    ).toBeTruthy();
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });

  it("names a failed Coin Debug Window read inline and recovers on retry", async () => {
    vi.mocked(api.getCoinDebugWindow).mockRejectedValueOnce(
      new Error("Owner Session expired"),
    );
    renderPage({ enabled: true });
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));

    // An unreadable coverage start is named, never shown as an empty window.
    await waitFor(() =>
      expect(
        screen.getByText(/The Coin Debug Window could not be read/),
      ).toBeTruthy(),
    );
    expect(screen.queryByText(/Coin Debug Window start/)).toBeNull();
    expect(reportActionFailure).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() =>
      expect(
        screen.getByText(
          `Coin Debug Window start ${formatTransactionTime("2026-08-12T10:00:00Z")}`,
        ),
      ).toBeTruthy(),
    );
    expect(
      screen.queryByText(/The Coin Debug Window could not be read/),
    ).toBeNull();
  });

  it("clears a Coin Debug Window read failure once a reset succeeds", async () => {
    vi.mocked(api.getCoinDebugWindow).mockRejectedValue(
      new Error("Owner Session expired"),
    );
    renderPage({ enabled: true });
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));
    await waitFor(() =>
      expect(
        screen.getByText(/The Coin Debug Window could not be read/),
      ).toBeTruthy(),
    );

    await userEvent.click(
      screen.getByRole("button", { name: "Reset Coin Debug Window" }),
    );

    await waitFor(() =>
      expect(
        screen.getByText(
          `Coin Debug Window start ${formatTransactionTime("2026-08-12T12:00:00Z")}`,
        ),
      ).toBeTruthy(),
    );
    expect(
      screen.queryByText(/The Coin Debug Window could not be read/),
    ).toBeNull();
  });

  it("keeps a successful reset when an older Coin Debug Window read fails after it", async () => {
    let failRead: (error: Error) => void = () => {};
    vi.mocked(api.getCoinDebugWindow).mockReturnValueOnce(
      new Promise((_resolve, reject) => {
        failRead = reject;
      }),
    );
    renderPage({ enabled: true });
    await userEvent.click(screen.getByRole("tab", { name: "Coin" }));

    await userEvent.click(
      screen.getByRole("button", { name: "Reset Coin Debug Window" }),
    );
    const fresh = `Coin Debug Window start ${formatTransactionTime("2026-08-12T12:00:00Z")}`;
    await waitFor(() => expect(screen.getByText(fresh)).toBeTruthy());

    // The read was already in flight when the reset succeeded, so its verdict
    // is stale: it must not restore an error over a known-fresh start (#382).
    failRead(new Error("Owner Session expired"));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(
      screen.queryByText(/The Coin Debug Window could not be read/),
    ).toBeNull();
    expect(screen.getByText(fresh)).toBeTruthy();
  });

  it("opens on the Client tab and requests the bundle for the chosen PC and window", async () => {
    const view = renderPage({ enabled: true });

    await waitFor(() =>
      expect(view.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    await userEvent.selectOptions(
      view.getByRole("combobox", { name: "PC" }),
      "pc-01",
    );
    await userEvent.selectOptions(
      view.getByRole("combobox", { name: "Window" }),
      "24 hours",
    );
    await userEvent.click(view.getByRole("button", { name: "Request Bundle" }));

    await waitFor(() =>
      expect(api.getSupportBundle).toHaveBeenCalledWith("pc-01", 86_400),
    );
    await waitFor(() =>
      expect(
        view.getByRole("region", { name: "Support Bundle preview" }),
      ).toBeTruthy(),
    );
    expect(reportActionSuccess).toHaveBeenCalledWith(
      "request",
      "Support Bundle",
    );
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("keeps the Verdict readable without downloading anything", async () => {
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    await requestBundleForChosenPc();

    await waitFor(() =>
      expect(screen.getByText("Verdict", { exact: true })).toBeTruthy(),
    );
    const verdictBlock = screen.getByText("Verdict", {
      exact: true,
    }).parentElement!;
    expect(verdictBlock.textContent).toContain("coverage contradiction");
    expect(verdictBlock.textContent).toContain(
      "the Guard reports the Client connected",
    );
    // The callout is the verdict alone — no section separator leaks in.
    expect(verdictBlock.textContent).not.toContain("---");
  });

  it("saves the bundle as a .md file without a clipboard control", async () => {
    const stub = stubBundleDownload();

    try {
      renderPage({ enabled: true });
      await waitFor(() =>
        expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
      );
      await requestBundleForChosenPc();

      await userEvent.click(
        screen.getByRole("button", { name: "Download Support Bundle" }),
      );

      await waitFor(() => expect(stub.createObjectURL).toHaveBeenCalled());
      expect(stub.downloaded!.download).toBe("support-bundle-pc-01.md");
      const blob = stub.createObjectURL.mock.calls[0][0];
      expect(blob.type).toBe("text/markdown");
      expect(await blob.text()).toBe(BUNDLE);
      expect(reportActionSuccess).toHaveBeenCalledWith(
        "download",
        "Support Bundle",
      );

      // The plain-HTTP LAN origin is an insecure context where the clipboard
      // API is unavailable, so the surface offers no copy control at all.
      expect(screen.queryByRole("button", { name: /copy/i })).toBeNull();
    } finally {
      stub.restore();
    }
  });

  it.each(["during", "after"])(
    "keeps the download PC when selection changes %s the request",
    async (timing) => {
      vi.mocked(api.listPcSettings).mockResolvedValue([
        {
          id: "pc-01",
          name: "PC 01",
          visible: true,
          registered_ip: null,
          mac_address: null,
        },
        {
          id: "pc-02",
          name: "PC 02",
          visible: true,
          registered_ip: null,
          mac_address: null,
        },
      ]);
      let resolveBundle!: (markdown: string) => void;
      vi.mocked(api.getSupportBundle).mockReturnValue(
        new Promise<string>((resolve) => {
          resolveBundle = resolve;
        }),
      );
      const stub = stubBundleDownload();
      try {
        renderPage({ enabled: true });
        const picker = await screen.findByRole("combobox", { name: "PC" });
        await requestBundleForChosenPc();
        if (timing === "during") await userEvent.selectOptions(picker, "pc-02");
        resolveBundle(BUNDLE);
        await screen.findByRole("region", { name: "Support Bundle preview" });
        if (timing === "after") await userEvent.selectOptions(picker, "pc-02");
        await userEvent.click(
          screen.getByRole("button", { name: "Download Support Bundle" }),
        );
        expect(stub.downloaded?.download).toBe("support-bundle-pc-01.md");
        expect(await stub.createObjectURL.mock.calls[0][0].text()).toBe(BUNDLE);
      } finally {
        stub.restore();
      }
    },
  );

  it("reports a failed bundle request via toast and leaves the preview absent", async () => {
    vi.mocked(api.getSupportBundle).mockRejectedValue(
      new Error("PC not found"),
    );
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    await requestBundleForChosenPc();

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "request",
        "Support Bundle",
        expect.anything(),
      ),
    );
    expect(
      screen.queryByRole("region", { name: "Support Bundle preview" }),
    ).toBeNull();
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });

  it("places the Diagnostic Request for the chosen PC with no PIN", async () => {
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    await userEvent.selectOptions(
      screen.getByRole("combobox", { name: "PC" }),
      "pc-01",
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Request Diagnostics" }),
    );

    await waitFor(() =>
      expect(api.requestDiagnostics).toHaveBeenCalledWith("pc-01"),
    );
    await waitFor(() =>
      expect(reportActionSuccess).toHaveBeenCalledWith(
        "place",
        "Diagnostic Request",
      ),
    );
    expect(reportActionFailure).not.toHaveBeenCalled();
    // Owner Session only, like every Support surface.
    expect(screen.queryByLabelText(/pin/i)).toBeNull();
  });

  it("reports a refused Diagnostic Request via toast and stays silent when placed", async () => {
    vi.mocked(api.requestDiagnostics).mockRejectedValue(
      new Error("PC not found"),
    );
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    await userEvent.selectOptions(
      screen.getByRole("combobox", { name: "PC" }),
      "pc-01",
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Request Diagnostics" }),
    );

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "place",
        "Diagnostic Request",
        expect.anything(),
      ),
    );
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });

  it("reports a PC-list load failure inline as a standing advisory", async () => {
    vi.mocked(api.listPcSettings).mockRejectedValue(
      new Error("Owner Session expired"),
    );
    const view = renderPage({ enabled: true });

    await waitFor(() =>
      expect(view.getByText("Owner Session expired")).toBeTruthy(),
    );
    expect(view.queryByRole("button", { name: "Request Bundle" })).toBeNull();
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("needs no PIN step-up anywhere in the surface", async () => {
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    await requestBundleForChosenPc();

    await waitFor(() =>
      expect(screen.getByText("Verdict", { exact: true })).toBeTruthy(),
    );
    expect(screen.queryByLabelText(/pin/i)).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("Diagnostics page Server tab (ADR-0082)", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    window.history.replaceState(null, "", "/admin/diagnostics");
    vi.clearAllMocks();
    vi.mocked(api.getHostDiagnostics).mockResolvedValue(HOST);
  });

  afterEach(() => {
    cleanup();
  });

  function openServerTab() {
    return userEvent.click(screen.getByRole("tab", { name: "Server" }));
  }

  it("reads the host diagnostics endpoint only when the Server tab opens", async () => {
    renderPage({ enabled: true });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "PC" })).toBeTruthy(),
    );
    expect(api.getHostDiagnostics).not.toHaveBeenCalled();

    await openServerTab();
    await waitFor(() => expect(api.getHostDiagnostics).toHaveBeenCalled());
  });

  it("shows the current run's start, version, and PID, and lists recent runs with their Exit Reasons", async () => {
    renderPage({ enabled: true });
    await openServerTab();

    await waitFor(() =>
      expect(screen.getByText(/Admin version 0\.9\.10, PID 4242/)).toBeTruthy(),
    );
    // The live run is named as such, never suspected of having exited.
    expect(screen.getByText("Still running")).toBeTruthy();
    // A normal exit is named for what it was, never dressed up as a crash.
    expect(screen.getByText(/Ended .* — Window closed/)).toBeTruthy();
  });

  it("reads a past start with no end as a suspected hard exit, worded as an absence of evidence", async () => {
    renderPage({ enabled: true });
    await openServerTab();

    await waitFor(() =>
      expect(screen.getByText("Suspected hard exit")).toBeTruthy(),
    );
    const item = screen.getByText("Suspected hard exit").closest("li")!;
    // Absence of evidence, not a verdict on the cause. Element boundaries
    // introduce whitespace, so the wording is matched normalized.
    const wording = item.textContent?.replace(/\s+/g, " ");
    expect(wording).toContain("went without saying why");
    expect(wording).toContain("no shutdown or panic was recorded");
    expect(wording).toContain("may mean a WebView2 crash");
    expect(wording).toContain("does not say which");
  });

  it("shows the panic file with its last-written time and the newest slice inline", async () => {
    renderPage({ enabled: true });
    await openServerTab();

    await waitFor(() =>
      expect(screen.getByText(/Present — last written/)).toBeTruthy(),
    );
    expect(screen.getByText("panic: index out of bounds")).toBeTruthy();
  });

  it("states absent panic evidence and an empty run history as plain absences", async () => {
    vi.mocked(api.getHostDiagnostics).mockResolvedValue(HOST_EMPTY);
    renderPage({ enabled: true });
    await openServerTab();

    await waitFor(() => expect(screen.getByText(/No Run Records yet/)).toBeTruthy());
    expect(screen.getByText(/No panic file — Admin writes one only/)).toBeTruthy();
    // An absence is a stated fact, not a failure.
    expect(screen.queryByText(/failed/i)).toBeNull();
    expect(reportActionFailure).not.toHaveBeenCalled();
  });

  it("reports a load failure inline, never by toast", async () => {
    vi.mocked(api.getHostDiagnostics).mockRejectedValue(
      new Error("Owner Session expired"),
    );
    renderPage({ enabled: true });
    await openServerTab();

    await waitFor(() =>
      expect(screen.getByText("Owner Session expired")).toBeTruthy(),
    );
    expect(reportActionFailure).not.toHaveBeenCalled();
    expect(reportActionSuccess).not.toHaveBeenCalled();
  });
});

describe("Diagnostics page with Diagnostics Mode off (ADR-0081)", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    window.history.replaceState(null, "", "/admin/diagnostics");
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders a stub naming Settings → Security instead of the surface", () => {
    renderPage({ enabled: false });

    // No tabs, no working surface, no API traffic.
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByRole("tab")).toBeNull();
    expect(screen.queryByRole("combobox", { name: "PC" })).toBeNull();
    expect(api.listPcSettings).not.toHaveBeenCalled();
    expect(screen.getByText(/Diagnostics Mode is off/)).toBeTruthy();
    expect(screen.getByText(/Settings → Security/)).toBeTruthy();
  });

  it("asserts neither stub nor surface while the flag is not yet known", () => {
    renderPage({ enabled: null });

    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByText(/Diagnostics Mode is off/)).toBeNull();
    expect(api.listPcSettings).not.toHaveBeenCalled();
    expect(screen.getByText(/Loading Diagnostics Mode/)).toBeTruthy();
  });

  it("explains a failed flag read inline instead of loading forever", async () => {
    const onRetry = vi.fn();
    renderPage({ enabled: null, loadError: "host unreachable", onRetry });

    // No claim about either side, and no loading text to mistake for slowness.
    expect(screen.queryByText(/Loading Diagnostics Mode/)).toBeNull();
    expect(screen.queryByText(/Diagnostics Mode is off/)).toBeNull();
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.getByText(/host unreachable/)).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalled();
  });

  it("keeps the last known flag and still names a later read failure", async () => {
    const onRetry = vi.fn();
    renderPage({ enabled: true, loadError: "host unreachable", onRetry });

    // A transient poll failure must not flicker a known surface away, but it
    // must not hide either: the value on screen may be stale, so the failure
    // is named where it happened and the retry stays in reach (#382).
    expect(screen.getByRole("tablist")).toBeTruthy();
    expect(
      screen.getByText(/Diagnostics Mode could not be read: host unreachable/),
    ).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalled();
  });
});

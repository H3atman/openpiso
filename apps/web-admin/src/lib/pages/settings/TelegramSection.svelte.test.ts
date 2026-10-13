// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api", () => ({
  getTelegramStatus: vi.fn(),
  submitTelegramToken: vi.fn(),
  detectTelegramChat: vi.fn(),
  connectTelegram: vi.fn(),
  disconnectTelegram: vi.fn(),
}));

vi.mock("../../notifications", () => ({
  reportActionFailure: vi.fn(),
  reportActionSuccess: vi.fn(),
}));

import { cleanup, render, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import * as api from "../../api";
import {
  reportActionFailure,
  reportActionSuccess,
} from "../../notifications";
import SettingsPageHarness from "../SettingsPageHarness.svelte";

const disconnected = {
  platform_supported: true,
  connected: false,
};

function renderSection() {
  return render(SettingsPageHarness, { props: { section: "telegram" } });
}

describe("Telegram Settings", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    vi.clearAllMocks();
    vi.mocked(api.getTelegramStatus).mockResolvedValue(disconnected);
    vi.mocked(api.submitTelegramToken).mockResolvedValue({
      bot_username: "shop_alerts_bot",
    });
    vi.mocked(api.detectTelegramChat).mockResolvedValue([
      { chat_id: "424242", title: "Owner" },
    ]);
    vi.mocked(api.connectTelegram).mockResolvedValue({
      platform_supported: true,
      connected: true,
      destination: {
        bot_username: "shop_alerts_bot",
        chat_title: "Owner",
        chat_id: "424242",
        connected_at: "2026-08-26T00:00:00+00:00",
      },
      last_test: { verified_at: "2026-08-26T00:00:00+00:00" },
    });
  });

  afterEach(() => {
    // Unmount every root so stale components cannot answer later assertions
    // with a previous test's state.
    cleanup();
  });

  it("guides the owner through BotFather setup while disconnected", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByText(/@BotFather/i, { exact: false })).toBeTruthy(),
    );
    // The token field is a password input and no status shows any secret.
    expect(view.getByPlaceholderText(/BotFather token/i)).toBeTruthy();
  });

  it("reports non-Windows Admin as unavailable with no setup path", async () => {
    vi.mocked(api.getTelegramStatus).mockResolvedValue({
      platform_supported: false,
      connected: false,
    });
    const view = renderSection();
    await waitFor(() =>
      expect(
        view.getByText(/requires a Windows Admin/i, { exact: false }),
      ).toBeTruthy(),
    );
    expect(view.queryByPlaceholderText(/BotFather token/i)).toBeNull();
  });

  it("shows the connected identity and last test without any secret", async () => {
    vi.mocked(api.getTelegramStatus).mockResolvedValue({
      platform_supported: true,
      connected: true,
      destination: {
        bot_username: "shop_alerts_bot",
        chat_title: "Owner",
        chat_id: "424242",
        connected_at: "2026-08-26T00:00:00+00:00",
      },
      last_test: { verified_at: "2026-08-26T01:02:03+00:00" },
    });
    const view = renderSection();
    await waitFor(() => expect(view.getByText(/shop_alerts_bot/)).toBeTruthy());
    expect(view.getByText(/chat 424242/)).toBeTruthy();
    expect(view.getByText(/Last successful test:/)).toBeTruthy();
    expect(document.body.textContent).not.toContain("secret-token");
  });

  it("toasts token verification failures", async () => {
    vi.mocked(api.submitTelegramToken).mockRejectedValueOnce(new Error("invalid token"));
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByPlaceholderText(/BotFather token/i)).toBeTruthy(),
    );

    await userEvent.type(view.getByPlaceholderText(/BotFather token/i), "bad-token");
    await userEvent.click(view.getByRole("button", { name: /verify token/i }));

    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "verify",
        "Telegram bot token",
        expect.any(Error),
      ),
    );
  });

  it("toasts chat detection success and failure", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByPlaceholderText(/BotFather token/i)).toBeTruthy(),
    );
    await userEvent.type(view.getByPlaceholderText(/BotFather token/i), "token");
    await userEvent.click(view.getByRole("button", { name: /verify token/i }));
    await waitFor(() => expect(view.getByText(/@shop_alerts_bot/)).toBeTruthy());

    await userEvent.click(view.getByRole("button", { name: /look for my message/i }));
    await waitFor(() =>
      expect(reportActionSuccess).toHaveBeenCalledWith("detect", "1 Telegram chat"),
    );

    vi.mocked(api.detectTelegramChat).mockRejectedValueOnce(new Error("offline"));
    await userEvent.click(view.getByRole("button", { name: /look for my message/i }));
    await waitFor(() =>
      expect(reportActionFailure).toHaveBeenCalledWith(
        "detect",
        "Telegram chat",
        expect.any(Error),
      ),
    );
  });

  it("connects through verify → detect → test with the current PIN", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByPlaceholderText(/BotFather token/i)).toBeTruthy(),
    );

    await userEvent.type(
      view.getByPlaceholderText(/BotFather token/i),
      "123456:secret-token-value",
    );
    await userEvent.click(view.getByRole("button", { name: /verify token/i }));

    await waitFor(() =>
      expect(api.submitTelegramToken).toHaveBeenCalledWith(
        "123456:secret-token-value",
      ),
    );
    await waitFor(() =>
      expect(view.getByText(/@shop_alerts_bot/)).toBeTruthy(),
    );

    await userEvent.click(
      view.getByRole("button", { name: /look for my message/i }),
    );
    await waitFor(() => expect(api.detectTelegramChat).toHaveBeenCalled());
    await waitFor(() => expect(view.getByText(/Owner/)).toBeTruthy());

    const connectButton = view.getByRole("button", {
      name: /send test & connect/i,
    });
    await userEvent.click(connectButton);

    // The PIN dialog gates the actual connect call.
    expect(api.connectTelegram).not.toHaveBeenCalled();
    await userEvent.type(
      document.getElementById("telegram-connect-pin") as HTMLInputElement,
      "owner-pin",
    );
    await userEvent.click(
      view.getByRole("button", { name: /^Test & connect$/i }),
    );

    await waitFor(() =>
      expect(api.connectTelegram).toHaveBeenCalledWith(undefined, "owner-pin"),
    );
    await waitFor(() => expect(reportActionSuccess).toHaveBeenCalled());
  });

  it("connects the manual fallback chat id after its own test", async () => {
    const view = renderSection();
    await waitFor(() =>
      expect(view.getByPlaceholderText(/BotFather token/i)).toBeTruthy(),
    );

    await userEvent.type(
      view.getByPlaceholderText(/BotFather token/i),
      "123456:secret-token-value",
    );
    await userEvent.click(view.getByRole("button", { name: /verify token/i }));
    await waitFor(() =>
      expect(view.getByText(/@shop_alerts_bot/)).toBeTruthy(),
    );

    // Skip detection entirely: manual entry is the fallback path.
    await userEvent.click(view.getByText(/Enter chat ID manually/i));
    await userEvent.type(
      view.getByPlaceholderText(/Numeric private chat ID/i),
      "987654321",
    );
    await userEvent.click(
      view.getByRole("button", { name: /send test & connect/i }),
    );
    await userEvent.type(
      document.getElementById("telegram-connect-pin") as HTMLInputElement,
      "owner-pin",
    );
    await userEvent.click(
      view.getByRole("button", { name: /^Test & connect$/i }),
    );

    await waitFor(() =>
      expect(api.connectTelegram).toHaveBeenCalledWith(
        "987654321",
        "owner-pin",
      ),
    );
  });

  it("shows delivery health inline while connected", async () => {
    vi.mocked(api.getTelegramStatus).mockResolvedValue({
      platform_supported: true,
      connected: true,
      destination: {
        bot_username: "shop_alerts_bot",
        chat_title: "Owner",
        chat_id: "424242",
        connected_at: "2026-08-26T00:00:00+00:00",
      },
      last_test: { verified_at: "2026-08-26T01:02:03+00:00" },
      delivery: {
        last_sent_at: "2026-08-26T02:00:00+00:00",
        pending_count: 2,
        queued_count: 0,
        retrying_count: 2,
        expired_count: 0,
        last_error: "could not reach Telegram",
        last_error_at: "2026-08-26T03:00:00+00:00",
      },
    });
    const view = renderSection();
    await waitFor(() => expect(view.getByText(/shop_alerts_bot/)).toBeTruthy());
    expect(view.container.textContent).toContain("Last message delivered:");
    expect(view.container.textContent).toContain(
      "2 retrying after failed attempts",
    );
    expect(view.container.textContent).toContain("could not reach Telegram");
  });

  it("names each waiting queue part and never a message secret", async () => {
    vi.mocked(api.getTelegramStatus).mockResolvedValue({
      platform_supported: true,
      connected: true,
      destination: {
        bot_username: "shop_alerts_bot",
        chat_title: "Owner",
        chat_id: "424242",
        connected_at: "2026-08-26T00:00:00+00:00",
      },
      delivery: {
        pending_count: 3,
        queued_count: 2,
        retrying_count: 1,
        expired_count: 4,
      },
    });
    const view = renderSection();
    await waitFor(() =>
      expect(view.container.textContent).toContain(
        "2 queued, 1 retrying after failed attempts; 4 expired unsent after 24 hours of retries",
      ),
    );
    // Health counts only: no redacted line or token ever appears.
    expect(document.body.textContent).not.toContain("detected on");
    expect(document.body.textContent).not.toContain("secret-token");
  });

  it("reports a healthy empty queue and says nothing was delivered yet without inventing an error", async () => {
    vi.mocked(api.getTelegramStatus).mockResolvedValue({
      platform_supported: true,
      connected: true,
      destination: {
        bot_username: "shop_alerts_bot",
        chat_title: "Owner",
        chat_id: "424242",
        connected_at: "2026-08-26T00:00:00+00:00",
      },
      delivery: {
        pending_count: 0,
        queued_count: 0,
        retrying_count: 0,
        expired_count: 0,
      },
    });
    const view = renderSection();
    await waitFor(() =>
      expect(view.container.textContent).toContain(
        "Delivery is healthy: nothing waiting.",
      ),
    );
    expect(view.container.textContent).toContain("No message delivered yet.");
    expect(view.container.textContent).not.toContain("Last error");
  });
});

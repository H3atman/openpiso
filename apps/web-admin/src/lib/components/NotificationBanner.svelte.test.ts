// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { render } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import type { NotificationItem } from "$lib/types";
import NotificationBanner from "./NotificationBanner.svelte";

function alert(overrides: Partial<NotificationItem> = {}): NotificationItem {
  return {
    id: "alert-1",
    severity: "alert",
    event_type: "account_transfer_out",
    outcome: "success",
    actor: "account",
    actor_account_id: "actor-account",
    actor_account_username: "juan",
    account_id: "target-account",
    account_username: "maria",
    pc_id: null,
    pc_name: null,
    amount_cents: null,
    seconds_delta: 60,
    points_delta_centi: null,
    summary: "Sent 1min 0sec to maria",
    created_at: "2026-08-03 10:00:00",
    read: false,
    acknowledged_at: null,
    ...overrides,
  };
}

describe("NotificationBanner", () => {
  it("renders the Actor Account in the compact notification row", async () => {
    const user = userEvent.setup();
    const view = render(NotificationBanner, {
      props: { alerts: [alert()], onNavigate: () => undefined },
    });

    await user.click(view.getByRole("button", { name: "Preview" }));

    expect(view.getByText("Sent 1min 0sec to maria — by juan")).toBeTruthy();
  });

  it("does not append an attribution suffix without an Actor Account", async () => {
    const user = userEvent.setup();
    const view = render(NotificationBanner, {
      props: {
        alerts: [
          alert({
            actor_account_id: null,
            actor_account_username: null,
            summary: "Account login denied",
          }),
        ],
        onNavigate: () => undefined,
      },
    });

    await user.click(view.getByRole("button", { name: "Preview" }));

    expect(view.getByText("Account login denied")).toBeTruthy();
    expect(view.queryByText(/unknown account/)).toBeNull();
  });
});

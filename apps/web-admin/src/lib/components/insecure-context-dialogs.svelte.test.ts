// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import ConfirmDialog from "./ConfirmDialog.svelte";
import RejectionCreditDialog from "./RejectionCreditDialog.svelte";
import type { CoinBatchRejection } from "$lib/types";

// Web Admin is served over plain HTTP on the shop LAN (http://<lan-ip>:8080),
// which is **not** a secure context: `crypto.randomUUID` is undefined there.
// Anything a dialog needs at construction time must not depend on it.
describe("dialogs in an insecure context", () => {
  let restore: (() => void) | null = null;

  beforeEach(() => {
    // `randomUUID` lives on Crypto.prototype, so an own-property delete would
    // leave it reachable — patch the prototype to mirror the real HTTP origin.
    const proto = Object.getPrototypeOf(crypto) as Record<string, unknown>;
    const descriptor = Object.getOwnPropertyDescriptor(proto, "randomUUID");
    delete proto.randomUUID;
    restore = () => {
      if (descriptor) Object.defineProperty(proto, "randomUUID", descriptor);
    };
    expect(crypto.randomUUID).toBeUndefined();
  });

  afterEach(() => {
    restore?.();
    restore = null;
    vi.restoreAllMocks();
  });

  it("renders ConfirmDialog", () => {
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Sign out juan from PC 02?",
        children: createRawSnippet(() => ({
          render: () => "<p>This ends the session.</p>",
        })),
        confirmLabel: "Sign out",
        busyLabel: "Signing out…",
        onconfirm: () => undefined,
        oncancel: () => undefined,
      },
    });
    expect(getByText("Sign out juan from PC 02?")).toBeTruthy();
  });

  it("renders RejectionCreditDialog", () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ denominations_cents: [100, 500] }), {
        headers: { "content-type": "application/json" },
      }),
    );
    const rejection: CoinBatchRejection = {
      rejection_audit_id: "audit-1",
      pc_id: "pc-1",
      pc_name: "PC 02",
      account_id: null,
      account_username: null,
      account_deleted: false,
      target_kind: "guest",
      cap_coins: 3,
      credited_coins: 0,
      remaining_coins: 3,
      pulse_count: null,
      created_at: "2026-08-03 10:00:00",
    };
    const { getByText } = render(RejectionCreditDialog, {
      props: {
        rejection,
        oncredited: () => undefined,
        onclose: () => undefined,
      },
    });
    expect(getByText("Credit rejected coin")).toBeTruthy();
  });
});

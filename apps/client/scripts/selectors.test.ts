import { describe, expect, test } from "bun:test";
import {
  selectClientView,
  selectLockSurface,
  shouldClearAuthenticationRequest,
} from "../src/lib/selectors";
import type { AccountAuth, TimerState } from "../src/lib/types";

function timer(overrides: Partial<TimerState> = {}): TimerState {
  return {
    pc_id: null,
    pc_name: "",
    pc_status: "",
    remaining_seconds: 0,
    guest_remaining_seconds: 0,
    timer_mode: "guest",
    active_username: null,
    connected: true,
    connection_status: "connected",
    expired: false,
    overlay_locked: false,
    power_state: "active",
    shutdown_in_seconds: null,
    offline_shutdown_in_seconds: null,
    shutdown_grace_seconds: 60,
    error: null,
    ...overrides,
  };
}

const mockAccountAuth: AccountAuth = {
  token: "tok",
  account: {
    id: "1",
    username: "test",
    remaining_seconds: 600,
    points_balance_centi: 5_000,
    status: "active",
  },
  session: {
    id: "s1",
    account_id: "1",
    username: "test",
    pc_id: "pc1",
    remaining_seconds: 600,
    points_balance_centi: 5_000,
    claimable_guest_seconds: 0,
  },
};

describe("selectClientView", () => {
  test("returns lock when timer is null", () => {
    expect(selectClientView(null, null, "widget")).toBe("lock");
  });

  test("returns lock when power_state is shutdown", () => {
    expect(
      selectClientView(timer({ power_state: "shutdown", remaining_seconds: 300 }), null, "widget"),
    ).toBe("lock");
  });

  test("returns lock when overlay_locked", () => {
    expect(
      selectClientView(timer({ overlay_locked: true, remaining_seconds: 300 }), null, "widget"),
    ).toBe("lock");
  });

  test("returns lock when remaining_seconds is 0", () => {
    expect(selectClientView(timer({ remaining_seconds: 0 }), null, "widget")).toBe("lock");
  });

  test("returns lock when expired", () => {
    expect(
      selectClientView(timer({ remaining_seconds: 300, expired: true }), null, "widget"),
    ).toBe("lock");
  });

  test("returns lock when remaining_seconds < 0", () => {
    expect(selectClientView(timer({ remaining_seconds: -10 }), null, "widget")).toBe("lock");
  });

  test("guest with usable time auto-enters widget by default", () => {
    expect(
      selectClientView(timer({ remaining_seconds: 300 }), null, "widget"),
    ).toBe("widget");
  });

  test("disconnected time is never routable to Widget or Mini Bar", () => {
    expect(
      selectClientView(
        timer({ remaining_seconds: 300, connection_status: "disconnected", connected: false }),
        null,
        "widget",
      ),
    ).toBe("lock");
    expect(
      selectClientView(
        timer({ remaining_seconds: 300, connection_status: "disconnected", connected: false }),
        null,
        "mini",
      ),
    ).toBe("lock");
  });

  test("guest authentication request routes usable Guest Time to lock", () => {
    expect(
      selectClientView(timer({ remaining_seconds: 300 }), null, "widget", true),
    ).toBe("lock");
  });

  test("guest with usable time auto-enters mini when view_mode is mini", () => {
    expect(
      selectClientView(timer({ remaining_seconds: 300 }), null, "mini"),
    ).toBe("mini");
  });

  test("successful authentication overrides the transient request", () => {
    expect(
      selectClientView(
        timer({ remaining_seconds: 300 }),
        mockAccountAuth,
        "widget",
        true,
      ),
    ).toBe("widget");
  });

  test("authoritative lock conditions override the transient request", () => {
    expect(
      selectClientView(
        timer({ remaining_seconds: 300, overlay_locked: true }),
        null,
        "widget",
        true,
      ),
    ).toBe("lock");
    expect(
      selectClientView(
        timer({ remaining_seconds: 300, power_state: "shutdown" }),
        null,
        "widget",
        true,
      ),
    ).toBe("lock");
    expect(
      selectClientView(timer({ remaining_seconds: 0 }), null, "widget", true),
    ).toBe("lock");
  });

  test("account session auto-enters widget", () => {
    expect(
      selectClientView(timer({ remaining_seconds: 300 }), mockAccountAuth, "widget"),
    ).toBe("widget");
  });

  test("account session auto-enters mini when view_mode is mini", () => {
    expect(
      selectClientView(timer({ remaining_seconds: 300 }), mockAccountAuth, "mini"),
    ).toBe("mini");
  });
});

describe("shouldClearAuthenticationRequest", () => {
  test("keeps only a usable Guest authentication request", () => {
    expect(
      shouldClearAuthenticationRequest(timer({ remaining_seconds: 300 }), null),
    ).toBe(false);
  });

  test("clears after login and authoritative Lock conditions", () => {
    expect(
      shouldClearAuthenticationRequest(
        timer({ remaining_seconds: 300 }),
        mockAccountAuth,
      ),
    ).toBe(true);
    expect(
      shouldClearAuthenticationRequest(
        timer({ remaining_seconds: 300, overlay_locked: true }),
        null,
      ),
    ).toBe(true);
    expect(
      shouldClearAuthenticationRequest(
        timer({ remaining_seconds: 300, power_state: "shutdown" }),
        null,
      ),
    ).toBe(true);
    expect(shouldClearAuthenticationRequest(timer(), null)).toBe(true);
  });
});

describe("selectLockSurface", () => {
  test("not configured: setup + settings", () => {
    const s = selectLockSurface(null, false, null);
    expect(s.left).toBe("setup");
    expect(s.right).toBe("settings");
  });

  test("configured before the first timer snapshot: connecting + empty", () => {
    const s = selectLockSurface(null, true, null);
    expect(s.left).toBe("connecting");
    expect(s.right).toBe("empty");
  });

  test("unregistered: left unregistered, right empty", () => {
    const s = selectLockSurface(
      timer({ connection_status: "unregistered" }),
      true,
      null,
    );
    expect(s.left).toBe("unregistered");
    expect(s.right).toBe("empty");
  });

  test("connecting: left connecting, right empty", () => {
    const s = selectLockSurface(
      timer({ connection_status: "connecting" }),
      true,
      null,
    );
    expect(s.left).toBe("connecting");
    expect(s.right).toBe("empty");
  });

  test("disconnected: left disconnected, right settings", () => {
    const s = selectLockSurface(
      timer({ connection_status: "disconnected" }),
      true,
      null,
    );
    expect(s.left).toBe("disconnected");
    expect(s.right).toBe("settings");
  });

  test("previously-connected disconnect: offline shutdown, right settings", () => {
    const s = selectLockSurface(
      timer({ connection_status: "disconnected", offline_shutdown_in_seconds: 120 }),
      true,
      null,
    );
    expect(s.left).toBe("offline_shutdown");
    expect(s.right).toBe("settings");
  });

  test("disconnected with error: left disconnected, right settings", () => {
    const s = selectLockSurface(
      timer({ connection_status: "connected", error: "fail" }),
      true,
      null,
    );
    expect(s.left).toBe("disconnected");
    expect(s.right).toBe("settings");
  });

  test("shutdown without account: left shutdown, right auth", () => {
    const s = selectLockSurface(
      timer({ power_state: "shutdown" }),
      true,
      null,
    );
    expect(s.left).toBe("shutdown");
    expect(s.right).toBe("auth");
  });

  test("shutdown with account: left shutdown, right account", () => {
    const s = selectLockSurface(
      timer({ power_state: "shutdown" }),
      true,
      mockAccountAuth,
    );
    expect(s.left).toBe("shutdown");
    expect(s.right).toBe("account");
  });

  test("overlay_locked without account: left session_ended, right auth", () => {
    const s = selectLockSurface(
      timer({ overlay_locked: true }),
      true,
      null,
    );
    expect(s.left).toBe("session_ended");
    expect(s.right).toBe("auth");
  });

  test("overlay_locked with account: left session_ended, right account", () => {
    const s = selectLockSurface(
      timer({ overlay_locked: true }),
      true,
      mockAccountAuth,
    );
    expect(s.left).toBe("session_ended");
    expect(s.right).toBe("account");
  });

  test("connected, out of time: insert_coin, right auth", () => {
    const s = selectLockSurface(
      timer({ remaining_seconds: 0 }),
      true,
      null,
    );
    expect(s.left).toBe("insert_coin");
    expect(s.right).toBe("auth");
  });

  test("connected, out of time with account: insert_coin, right account", () => {
    const s = selectLockSurface(
      timer({ remaining_seconds: 0 }),
      true,
      mockAccountAuth,
    );
    expect(s.left).toBe("insert_coin");
    expect(s.right).toBe("account");
  });

  test("shutdown + disconnected: disconnected wins (precedence)", () => {
    const s = selectLockSurface(
      timer({
        power_state: "shutdown",
        connection_status: "disconnected",
      }),
      true,
      null,
    );
    expect(s.left).toBe("disconnected");
  });

  test("overlay_locked + disconnected: disconnected wins (precedence)", () => {
    const s = selectLockSurface(
      timer({
        overlay_locked: true,
        connection_status: "disconnected",
      }),
      true,
      null,
    );
    expect(s.left).toBe("disconnected");
  });
});

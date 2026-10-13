import type { AccountAuth, ClientView, TimerState, ViewMode } from "./types";

export type LockSurfaceLeft =
  | "setup"
  | "unregistered"
  | "connecting"
  | "disconnected"
  | "offline_shutdown"
  | "shutdown"
  | "session_ended"
  | "shop_closed"
  | "insert_coin";

export type LockSurfaceRight =
  | "settings"
  | "empty"
  | "auth"
  | "account";

export interface LockSurface {
  left: LockSurfaceLeft;
  right: LockSurfaceRight;
}

export function canRouteToSession(
  timer: TimerState | null,
): timer is TimerState {
  return (
    timer !== null &&
    timer.connection_status === "connected" &&
    timer.offline_shutdown_in_seconds === null &&
    timer.power_state !== "shutdown" &&
    !timer.overlay_locked &&
    timer.remaining_seconds > 0 &&
    !timer.expired
  );
}

export function shouldClearAuthenticationRequest(
  timer: TimerState | null,
  accountAuth: AccountAuth | null,
): boolean {
  return accountAuth !== null || !canRouteToSession(timer);
}

export function selectClientView(
  timer: TimerState | null,
  accountAuth: AccountAuth | null,
  viewMode: ViewMode,
  authenticationRequested = false,
): ClientView {
  if (!canRouteToSession(timer)) return "lock";
  if (authenticationRequested && !accountAuth) return "lock";
  return viewMode === "mini" ? "mini" : "widget";
}

export function selectLockSurface(
  timer: TimerState | null,
  configured: boolean,
  accountAuth: AccountAuth | null,
): LockSurface {
  if (!configured) {
    return { left: "setup", right: "settings" };
  }
  if (timer === null) {
    return { left: "connecting", right: "empty" };
  }
  const status = timer.connection_status;
  if (status === "unregistered") {
    return { left: "unregistered", right: "empty" };
  }
  if (status === "connecting") {
    if (timer.tamper_kill_pending) {
      return { left: "connecting", right: "empty" };
    }
    if (timer.offline_shutdown_in_seconds !== null) {
      return { left: "offline_shutdown", right: "settings" };
    }
    return { left: "connecting", right: "empty" };
  }
  if (status === "disconnected") {
    if (timer?.offline_shutdown_in_seconds !== null) {
      return { left: "offline_shutdown", right: "settings" };
    }
    return { left: "disconnected", right: "settings" };
  }
  if (timer?.error) {
    return { left: "disconnected", right: "settings" };
  }
  if (timer?.power_state === "shutdown") {
    return {
      left: "shutdown",
      right: accountAuth ? "account" : "auth",
    };
  }
  if (timer?.overlay_locked) {
    return {
      left: "session_ended",
      right: accountAuth ? "account" : "auth",
    };
  }
  if (timer?.shop_status && !timer.shop_status.open) {
    return {
      left: "shop_closed",
      right: accountAuth ? "account" : "auth",
    };
  }
  return {
    left: "insert_coin",
    right: accountAuth ? "account" : "auth",
  };
}

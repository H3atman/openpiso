export type LoginMode = "sign-in" | "setup";
export type PinConfigured = boolean | "unknown";

/** Login lifecycle only; PINs stay in the form, never in machine state. */
export type LoginState =
  | { phase: "checking-session" }
  | {
      phase: "ready";
      mode: LoginMode;
      configured: PinConfigured;
      error: string | null;
    }
  | { phase: "submitting"; mode: LoginMode; configured: PinConfigured }
  | {
      phase: "locked-out";
      mode: LoginMode;
      configured: PinConfigured;
      secondsLeft: number;
    }
  | { phase: "authenticated" };

export type LoginEvent =
  | { type: "session-loaded"; authenticated: boolean; configured: boolean }
  | { type: "session-check-failed" }
  | { type: "choose-setup" }
  | { type: "submit"; pinsMatch: boolean }
  | { type: "submit-succeeded" }
  | { type: "submit-failed"; message: string }
  | { type: "setup-conflict"; message: string }
  | { type: "throttled"; retryAfterSeconds: number }
  | { type: "lockout-tick" };

export const initialLoginState: LoginState = { phase: "checking-session" };

/** Apply a login event without performing requests or starting timers. */
export function reduceLogin(state: LoginState, event: LoginEvent): LoginState {
  switch (event.type) {
    case "session-loaded":
      if (state.phase !== "checking-session") return state;
      return event.authenticated
        ? { phase: "authenticated" }
        : {
            phase: "ready",
            mode: event.configured ? "sign-in" : "setup",
            configured: event.configured,
            error: null,
          };
    case "session-check-failed":
      return state.phase === "checking-session"
        ? {
            phase: "ready",
            mode: "sign-in",
            configured: "unknown",
            error: null,
          }
        : state;
    case "choose-setup":
      return state.phase === "ready" &&
        state.mode === "sign-in" &&
        state.configured !== true
        ? { ...state, mode: "setup", error: null }
        : state;
    case "submit":
      if (state.phase !== "ready") return state;
      if (state.mode === "setup" && !event.pinsMatch)
        return { ...state, error: "PINs do not match." };
      return {
        phase: "submitting",
        mode: state.mode,
        configured: state.configured,
      };
    case "submit-succeeded":
      return state.phase === "submitting" ? { phase: "authenticated" } : state;
    case "submit-failed":
      return state.phase === "submitting"
        ? {
            phase: "ready",
            mode: state.mode,
            configured: state.configured,
            error: event.message,
          }
        : state;
    case "setup-conflict":
      return state.phase === "submitting" && state.mode === "setup"
        ? {
            phase: "ready",
            mode: "sign-in",
            configured: true,
            error: event.message,
          }
        : state;
    case "throttled":
      return state.phase === "submitting"
        ? {
            phase: "locked-out",
            mode: state.mode,
            configured: state.configured,
            secondsLeft: event.retryAfterSeconds,
          }
        : state;
    case "lockout-tick":
      if (state.phase !== "locked-out") return state;
      return state.secondsLeft <= 1
        ? {
            phase: "ready",
            mode: state.mode,
            configured: state.configured,
            error: null,
          }
        : { ...state, secondsLeft: state.secondsLeft - 1 };
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

/** Submission requires a ready form and a nonempty PIN. */
export function canSubmit(state: LoginState, pinEntered: boolean): boolean {
  return state.phase === "ready" && pinEntered;
}

/** Explain setup when no configured PIN is known. */
export function showFirstRunBanner(state: LoginState): boolean {
  return "mode" in state && state.mode === "setup" && state.configured !== true;
}

/** Offer recovery guidance only for an already-configured PIN. */
export function showForgotHint(state: LoginState): boolean {
  return (
    "mode" in state && state.mode === "sign-in" && state.configured === true
  );
}

/** Hide setup on a box known to have a PIN already. */
export function showSetupLink(state: LoginState): boolean {
  return (
    "mode" in state && state.mode === "sign-in" && state.configured !== true
  );
}

/** Keep the form action label consistent across request and lockout phases. */
export function buttonLabel(state: LoginState): string {
  return state.phase === "submitting"
    ? "Please wait…"
    : "mode" in state && state.mode === "setup"
      ? "Set PIN"
      : "Sign in";
}

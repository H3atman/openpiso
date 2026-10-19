import { describe, expect, it } from "vitest";
import {
  buttonLabel,
  showSetupLink,
  showForgotHint,
  showFirstRunBanner,
  canSubmit,
  reduceLogin,
  type LoginEvent,
  type LoginState,
} from "./login";

describe("reduceLogin", () => {
  it("restores an authenticated Owner Session", () => {
    expect(
      reduceLogin(
        { phase: "checking-session" },
        {
          type: "session-loaded",
          authenticated: true,
          configured: true,
        },
      ),
    ).toEqual({ phase: "authenticated" });
  });
  it("chooses sign-in or first-run setup from the session response", () => {
    expect(
      reduceLogin(
        { phase: "checking-session" },
        {
          type: "session-loaded",
          authenticated: false,
          configured: true,
        },
      ),
    ).toEqual({
      phase: "ready",
      mode: "sign-in",
      configured: true,
      error: null,
    });
    expect(
      reduceLogin(
        { phase: "checking-session" },
        {
          type: "session-loaded",
          authenticated: false,
          configured: false,
        },
      ),
    ).toEqual({
      phase: "ready",
      mode: "setup",
      configured: false,
      error: null,
    });
  });
  it("allows sign-in with unknown configuration after a session check failure", () => {
    expect(
      reduceLogin(
        { phase: "checking-session" },
        { type: "session-check-failed" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "sign-in",
      configured: "unknown",
      error: null,
    });
  });
  it("offers setup only to an unconfigured or unknown sign-in form", () => {
    expect(
      reduceLogin(
        {
          phase: "ready",
          mode: "sign-in",
          configured: "unknown",
          error: "Offline",
        },
        { type: "choose-setup" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "setup",
      configured: "unknown",
      error: null,
    });
    expect(
      reduceLogin(
        { phase: "ready", mode: "sign-in", configured: false, error: null },
        { type: "choose-setup" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "setup",
      configured: false,
      error: null,
    });
    const configured: LoginState = {
      phase: "ready",
      mode: "sign-in",
      configured: true,
      error: null,
    };
    expect(reduceLogin(configured, { type: "choose-setup" })).toBe(configured);
  });
  it("rejects mismatched setup PINs without submitting", () => {
    expect(
      reduceLogin(
        { phase: "ready", mode: "setup", configured: false, error: null },
        { type: "submit", pinsMatch: false },
      ),
    ).toEqual({
      phase: "ready",
      mode: "setup",
      configured: false,
      error: "PINs do not match.",
    });
  });
  it("submits sign-in and matching setup while clearing old errors", () => {
    expect(
      reduceLogin(
        {
          phase: "ready",
          mode: "sign-in",
          configured: true,
          error: "Wrong PIN",
        },
        { type: "submit", pinsMatch: false },
      ),
    ).toEqual({ phase: "submitting", mode: "sign-in", configured: true });
    expect(
      reduceLogin(
        { phase: "ready", mode: "setup", configured: "unknown", error: null },
        { type: "submit", pinsMatch: true },
      ),
    ).toEqual({ phase: "submitting", mode: "setup", configured: "unknown" });
  });
  it("authenticates after a successful submission", () => {
    expect(
      reduceLogin(
        { phase: "submitting", mode: "setup", configured: false },
        { type: "submit-succeeded" },
      ),
    ).toEqual({ phase: "authenticated" });
  });
  it("returns a rejected submission to its form with the server message", () => {
    expect(
      reduceLogin(
        { phase: "submitting", mode: "sign-in", configured: true },
        { type: "submit-failed", message: "Wrong PIN" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "sign-in",
      configured: true,
      error: "Wrong PIN",
    });
  });
  it("recovers an already-configured setup conflict by switching to sign-in", () => {
    expect(
      reduceLogin(
        { phase: "submitting", mode: "setup", configured: "unknown" },
        { type: "setup-conflict", message: "PIN already configured" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "sign-in",
      configured: true,
      error: "PIN already configured",
    });
    const signingIn: LoginState = {
      phase: "submitting",
      mode: "sign-in",
      configured: true,
    };
    expect(
      reduceLogin(signingIn, { type: "setup-conflict", message: "Conflict" }),
    ).toBe(signingIn);
  });
  it("locks out a throttled submission for the server retry window", () => {
    expect(
      reduceLogin(
        { phase: "submitting", mode: "sign-in", configured: true },
        {
          type: "throttled",
          retryAfterSeconds: 3,
        },
      ),
    ).toStrictEqual({
      phase: "locked-out",
      mode: "sign-in",
      configured: true,
      secondsLeft: 3,
    });
  });
  it("counts down lockout and returns to a ready form at zero", () => {
    expect(
      reduceLogin(
        {
          phase: "locked-out",
          mode: "setup",
          configured: false,
          secondsLeft: 2,
        },
        { type: "lockout-tick" },
      ),
    ).toEqual({
      phase: "locked-out",
      mode: "setup",
      configured: false,
      secondsLeft: 1,
    });
    expect(
      reduceLogin(
        {
          phase: "locked-out",
          mode: "setup",
          configured: false,
          secondsLeft: 1,
        },
        { type: "lockout-tick" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "setup",
      configured: false,
      error: null,
    });
    expect(
      reduceLogin(
        {
          phase: "locked-out",
          mode: "sign-in",
          configured: true,
          secondsLeft: 0,
        },
        { type: "lockout-tick" },
      ),
    ).toEqual({
      phase: "ready",
      mode: "sign-in",
      configured: true,
      error: null,
    });
  });
  it("preserves state identity for ignored lifecycle events", () => {
    const checking: LoginState = { phase: "checking-session" };
    const ready: LoginState = {
      phase: "ready",
      mode: "sign-in",
      configured: true,
      error: null,
    };
    const submitting: LoginState = {
      phase: "submitting",
      mode: "sign-in",
      configured: true,
    };
    const lockedOut: LoginState = {
      phase: "locked-out",
      mode: "sign-in",
      configured: true,
      secondsLeft: 3,
    };
    const authenticated: LoginState = { phase: "authenticated" };
    const cases: { state: LoginState; event: LoginEvent }[] = [
      { state: checking, event: { type: "submit", pinsMatch: true } },
      { state: submitting, event: { type: "submit", pinsMatch: true } },
      { state: lockedOut, event: { type: "submit", pinsMatch: true } },
      { state: ready, event: { type: "submit-succeeded" } },
      {
        state: ready,
        event: {
          type: "session-loaded",
          authenticated: true,
          configured: true,
        },
      },
      { state: checking, event: { type: "lockout-tick" } },
      { state: ready, event: { type: "lockout-tick" } },
      { state: submitting, event: { type: "lockout-tick" } },
      { state: authenticated, event: { type: "lockout-tick" } },
    ];
    for (const { state, event } of cases) {
      expect(reduceLogin(state, event)).toBe(state);
    }
  });
});

describe("login selectors", () => {
  it("enables submit only on a ready form with a PIN entered", () => {
    expect(canSubmit({ phase: "checking-session" }, true)).toBe(false);
    expect(
      canSubmit(
        { phase: "ready", mode: "sign-in", configured: true, error: null },
        false,
      ),
    ).toBe(false);
    expect(
      canSubmit(
        { phase: "ready", mode: "setup", configured: false, error: null },
        true,
      ),
    ).toBe(true);
    expect(
      canSubmit(
        { phase: "submitting", mode: "sign-in", configured: true },
        true,
      ),
    ).toBe(false);
    expect(
      canSubmit(
        {
          phase: "locked-out",
          mode: "sign-in",
          configured: true,
          secondsLeft: 1,
        },
        true,
      ),
    ).toBe(false);
    expect(canSubmit({ phase: "authenticated" }, true)).toBe(false);
  });
  it("shows the first-run banner only during unconfigured setup", () => {
    expect(
      showFirstRunBanner({
        phase: "ready",
        mode: "setup",
        configured: false,
        error: null,
      }),
    ).toBe(true);
    expect(
      showFirstRunBanner({
        phase: "submitting",
        mode: "setup",
        configured: "unknown",
      }),
    ).toBe(true);
    expect(
      showFirstRunBanner({
        phase: "ready",
        mode: "sign-in",
        configured: true,
        error: null,
      }),
    ).toBe(false);
    expect(showFirstRunBanner({ phase: "checking-session" })).toBe(false);
  });
  it("shows the forgot-PIN hint only for configured sign-in", () => {
    expect(
      showForgotHint({
        phase: "ready",
        mode: "sign-in",
        configured: true,
        error: null,
      }),
    ).toBe(true);
    expect(
      showForgotHint({
        phase: "locked-out",
        mode: "sign-in",
        configured: true,
        secondsLeft: 2,
      }),
    ).toBe(true);
    expect(
      showForgotHint({
        phase: "ready",
        mode: "sign-in",
        configured: "unknown",
        error: null,
      }),
    ).toBe(false);
    expect(
      showForgotHint({
        phase: "ready",
        mode: "setup",
        configured: false,
        error: null,
      }),
    ).toBe(false);
    expect(showForgotHint({ phase: "checking-session" })).toBe(false);
  });
  it("offers the setup link only when sign-in configuration is not known true", () => {
    expect(
      showSetupLink({
        phase: "ready",
        mode: "sign-in",
        configured: "unknown",
        error: null,
      }),
    ).toBe(true);
    expect(
      showSetupLink({
        phase: "ready",
        mode: "sign-in",
        configured: false,
        error: null,
      }),
    ).toBe(true);
    expect(
      showSetupLink({
        phase: "ready",
        mode: "sign-in",
        configured: true,
        error: null,
      }),
    ).toBe(false);
    expect(
      showSetupLink({
        phase: "ready",
        mode: "setup",
        configured: false,
        error: null,
      }),
    ).toBe(false);
    expect(showSetupLink({ phase: "checking-session" })).toBe(false);
  });
  it("labels the active action and pending submission", () => {
    expect(
      buttonLabel({
        phase: "ready",
        mode: "setup",
        configured: false,
        error: null,
      }),
    ).toBe("Set PIN");
    expect(
      buttonLabel({
        phase: "ready",
        mode: "sign-in",
        configured: true,
        error: null,
      }),
    ).toBe("Sign in");
    expect(
      buttonLabel({ phase: "submitting", mode: "setup", configured: false }),
    ).toBe("Please wait…");
    expect(
      buttonLabel({
        phase: "locked-out",
        mode: "setup",
        configured: false,
        secondsLeft: 3,
      }),
    ).toBe("Set PIN");
  });
});

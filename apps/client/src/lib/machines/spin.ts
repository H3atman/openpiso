// Spin the Wheel flow (ADR-0089, ADR-0088). Pure state machine shared by the
// Widget and the Fullscreen Lock; no I/O. The runner in `spin.svelte.ts` owns
// the state, calls the server on entering `requesting`, runs the Spin Duration
// timer on entering `animating`, and dispatches the result events.
//
//   closed --opened--> idle --spin--> requesting            (setting off)
//                          \--spin--> confirming --submit--> requesting
//   requesting --succeeded--> animating{0} --animationFinished--> revealed{0}
//   revealed{n} --nextFreeSpin--> animating{n+1}            (free-spin chain)
//   revealed{last} --spin--> confirming | requesting        (another paid spin)
//   requesting --rejected--> confirming (password problem) | idle (other error)
//
// `close` is refused while `requesting` or `animating`: the spin is already
// charged (and credited by the server), so leaving would lose the prize.
//
// The password only travels on the `submit` event; it is never held in state.
// Free-spin chain steps are pre-resolved by the server and only revealed here:
// `nextFreeSpin` never reaches the server.

import { INVALID_SPIN_PASSWORD_MESSAGE, formatSpinThrottled } from "../format";
import type { WheelSegmentRow, WheelSpinResult } from "../types";

/** What the dialog needs from `/api/wheel/settings`. */
export interface SpinWheelConfig {
  segments: WheelSegmentRow[];
  spinDuration: number;
  /** Owner setting (ADR-0088); also flipped on by a server `password_required`. */
  passwordRequired: boolean;
}

export type SpinFailure =
  | { kind: "password_required" }
  | { kind: "invalid_password" }
  | { kind: "throttled"; seconds: number }
  | { kind: "error"; message: string };

export type SpinState =
  | { kind: "closed" }
  | { kind: "idle"; wheel: SpinWheelConfig; error: string }
  | { kind: "confirming"; wheel: SpinWheelConfig; error: string }
  | { kind: "requesting"; wheel: SpinWheelConfig }
  /** `step` indexes `result.steps`; the wheel is turning toward it. */
  | { kind: "animating"; wheel: SpinWheelConfig; result: WheelSpinResult; step: number }
  /** The wheel has landed on `step`; its prize is shown. */
  | { kind: "revealed"; wheel: SpinWheelConfig; result: WheelSpinResult; step: number };

export type SpinEvent =
  | { type: "opened"; wheel: SpinWheelConfig }
  | { type: "spin" }
  | { type: "submit"; password: string }
  | { type: "succeeded"; result: WheelSpinResult }
  | { type: "rejected"; failure: SpinFailure }
  | { type: "animationFinished" }
  | { type: "nextFreeSpin" }
  | { type: "close" };

export const initialSpinState: SpinState = { kind: "closed" };

/**
 * Classify the raw server error code/message from a rejected spin request.
 * Throttle text is `too many attempts; try again in <n>s`.
 */
export function classifySpinError(code: string): SpinFailure {
  if (code === "password_required") return { kind: "password_required" };
  if (code === "invalid_password") return { kind: "invalid_password" };
  if (code.startsWith("too many attempts")) {
    return { kind: "throttled", seconds: Number(/(\d+)s/.exec(code)?.[1] ?? 0) };
  }
  return { kind: "error", message: code };
}

/** True when `state` shows the last landing of its result. */
function atChainEnd(state: Extract<SpinState, { kind: "revealed" }>): boolean {
  return state.step >= state.result.steps.length - 1;
}

/** A paid spin asks for the password first when the setting is on. */
function startSpin(wheel: SpinWheelConfig): SpinState {
  return wheel.passwordRequired ? { kind: "confirming", wheel, error: "" } : { kind: "requesting", wheel };
}

/** A password rejection sends the customer back to the prompt with a reason. */
function spinRejected(wheel: SpinWheelConfig, failure: SpinFailure): SpinState {
  switch (failure.kind) {
    case "password_required":
      return { kind: "confirming", wheel: { ...wheel, passwordRequired: true }, error: "" };
    case "invalid_password":
      return { kind: "confirming", wheel, error: INVALID_SPIN_PASSWORD_MESSAGE };
    case "throttled":
      return { kind: "confirming", wheel, error: formatSpinThrottled(failure.seconds) };
    case "error":
      return { kind: "idle", wheel, error: failure.message };
    default: {
      const unreachable: never = failure;
      return unreachable;
    }
  }
}

export function reduceSpin(state: SpinState, event: SpinEvent): SpinState {
  switch (event.type) {
    case "opened":
      return state.kind === "closed" ? { kind: "idle", wheel: event.wheel, error: "" } : state;
    case "spin":
      if (state.kind === "idle" || (state.kind === "revealed" && atChainEnd(state))) {
        return startSpin(state.wheel);
      }
      return state;
    case "submit":
      return state.kind === "confirming" ? { kind: "requesting", wheel: state.wheel } : state;
    case "succeeded":
      return state.kind === "requesting"
        ? {
            kind: "animating",
            wheel: { ...state.wheel, segments: event.result.segments },
            result: event.result,
            step: 0,
          }
        : state;
    case "rejected":
      return state.kind === "requesting" ? spinRejected(state.wheel, event.failure) : state;
    case "animationFinished":
      return state.kind === "animating" ? { ...state, kind: "revealed" } : state;
    case "nextFreeSpin":
      return state.kind === "revealed" && !atChainEnd(state)
        ? { ...state, kind: "animating", step: state.step + 1 }
        : state;
    case "close":
      return state.kind === "requesting" || state.kind === "animating" ? state : initialSpinState;
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

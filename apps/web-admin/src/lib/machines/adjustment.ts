import type { Pc } from "../types";

/** Manual Adjustment lifecycle; editable form values stay in the component. */
export type AdjustmentState =
  | { phase: "loading" }
  | { phase: "load-failed"; error: string }
  | { phase: "ready"; pcs: Pc[]; submitting: boolean; error: string | null };

export type AdjustmentEvent =
  | { type: "loaded"; pcs: Pc[] }
  | { type: "load-failed"; error: string }
  | { type: "submit" }
  | { type: "submit-finished" };

export const initialAdjustmentState: AdjustmentState = { phase: "loading" };

/** Apply load and submission results without hiding the form during refresh. */
export function reduceAdjustment(
  state: AdjustmentState,
  event: AdjustmentEvent,
): AdjustmentState {
  switch (event.type) {
    case "loaded":
      return {
        phase: "ready",
        pcs: event.pcs,
        submitting: state.phase === "ready" && state.submitting,
        error: null,
      };
    case "load-failed":
      return state.phase === "ready"
        ? { ...state, error: event.error }
        : { phase: "load-failed", error: event.error };
    case "submit":
      return state.phase === "ready" && !state.submitting
        ? { ...state, submitting: true }
        : state;
    case "submit-finished":
      return state.phase === "ready" && state.submitting
        ? { ...state, submitting: false }
        : state;
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

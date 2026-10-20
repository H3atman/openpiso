/** A collected bundle, kept under the PC it was requested for. */
export type ShownBundle = { pcId: string; bundle: string };

/**
 * Support Bundle request. The last ready bundle stays in `shown` while a new
 * request is in flight and after it fails; only a success replaces it.
 */
export type SupportBundleState =
  | { phase: "idle"; shown: null }
  | { phase: "requesting"; pcId: string; shown: ShownBundle | null }
  | { phase: "ready"; shown: ShownBundle };

export type SupportBundleEvent =
  | { type: "requested"; pcId: string }
  | { type: "succeeded"; pcId: string; bundle: string }
  | { type: "failed"; pcId: string };

export const initialSupportBundleState: SupportBundleState = {
  phase: "idle",
  shown: null,
};

/** One request at a time; a result applies only to the request in flight. */
export function reduceSupportBundle(
  state: SupportBundleState,
  event: SupportBundleEvent,
): SupportBundleState {
  switch (event.type) {
    case "requested":
      return state.phase === "requesting"
        ? state
        : { phase: "requesting", pcId: event.pcId, shown: state.shown };
    case "succeeded":
      return state.phase === "requesting" && state.pcId === event.pcId
        ? {
            phase: "ready",
            shown: { pcId: event.pcId, bundle: event.bundle },
          }
        : state;
    case "failed":
      if (state.phase !== "requesting" || state.pcId !== event.pcId)
        return state;
      return state.shown
        ? { phase: "ready", shown: state.shown }
        : initialSupportBundleState;
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

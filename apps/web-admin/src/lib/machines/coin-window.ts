/**
 * Coin Debug Window read/reset (ADR-0078). The window is shop-wide, not
 * PC-scoped, so reads are keyed by `requestId`: a newer read or a successful
 * reset bumps it and a result carrying an older id is dropped. `since` and
 * `error` persist across a read in flight; `error` wins when both are set.
 */
export type CoinWindowState = {
  requestId: number;
  resetting: boolean;
  since: string;
  error: string;
};

export type CoinWindowEvent =
  | { type: "read-started" }
  | { type: "read-succeeded"; requestId: number; since: string }
  | { type: "read-failed"; requestId: number; error: string }
  | { type: "reset-started" }
  | { type: "reset-succeeded"; since: string }
  | { type: "reset-failed" };

export const initialCoinWindowState: CoinWindowState = {
  requestId: 0,
  resetting: false,
  since: "",
  error: "",
};

/** Apply read and reset events without performing requests. */
export function reduceCoinWindow(
  state: CoinWindowState,
  event: CoinWindowEvent,
): CoinWindowState {
  switch (event.type) {
    case "read-started":
      return { ...state, requestId: state.requestId + 1 };
    case "read-succeeded":
      return event.requestId === state.requestId
        ? { ...state, since: event.since, error: "" }
        : state;
    case "read-failed":
      return event.requestId === state.requestId
        ? { ...state, error: event.error }
        : state;
    case "reset-started":
      return state.resetting ? state : { ...state, resetting: true };
    case "reset-succeeded":
      // A fresh start supersedes an earlier read failure and any read in flight.
      return state.resetting
        ? {
            requestId: state.requestId + 1,
            resetting: false,
            since: event.since,
            error: "",
          }
        : state;
    case "reset-failed":
      return state.resetting ? { ...state, resetting: false } : state;
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

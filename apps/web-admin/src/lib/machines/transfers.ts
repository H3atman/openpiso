import type { PendingPointsTransfer } from "../types";

export type TransferAction = "approve" | "reject";

/**
 * One row's approve/reject lifecycle, separate from the list. `afterLoad` is
 * the load id current when the action succeeded: the row stays hidden until a
 * load with a greater id (started after success) lands. A failed action has
 * no row entry; the failure is toasted and the row is enabled again.
 */
export type TransferRow =
  | { phase: "acting"; action: TransferAction }
  | { phase: "succeeded"; action: TransferAction; afterLoad: number };

/**
 * Points Transfer approvals: list load lifecycle (`phase`) plus per-row
 * lifecycles. Requests stay in state while refreshing and after a failure.
 * `loadId` is the newest load started; `appliedId` the newest result applied,
 * so a slow poll still lands unless a newer result already did.
 */
export type TransfersState = {
  requests: PendingPointsTransfer[];
  rows: Record<string, TransferRow>;
  loadId: number;
  appliedId: number;
} & (
  | { phase: "loading" }
  | { phase: "ready" }
  | { phase: "failed"; error: string }
);

export type TransfersEvent =
  | { type: "load-started" }
  | {
      type: "load-succeeded";
      loadId: number;
      requests: PendingPointsTransfer[];
    }
  | { type: "load-failed"; loadId: number; error: string }
  | { type: "act-started"; id: string; action: TransferAction }
  | { type: "act-succeeded"; id: string }
  | { type: "act-failed"; id: string };

export const initialTransfersState: TransfersState = {
  phase: "loading",
  requests: [],
  rows: {},
  loadId: 0,
  appliedId: 0,
};

/** Requests to render: those whose action has succeeded stay hidden until a fresh load confirms. */
export function visibleRequests(
  state: TransfersState,
): PendingPointsTransfer[] {
  return state.requests.filter((r) => state.rows[r.id]?.phase !== "succeeded");
}

/** Apply list and row events without performing requests or timers. */
export function reduceTransfers(
  state: TransfersState,
  event: TransfersEvent,
): TransfersState {
  switch (event.type) {
    case "load-started":
      return { ...state, phase: "loading", loadId: state.loadId + 1 };
    case "load-succeeded": {
      if (event.loadId <= state.appliedId) return state;
      // Keep acting rows, and succeeded rows still listed by a load that
      // started before their success; drop entries for ids no longer listed.
      const listed = new Set(event.requests.map((r) => r.id));
      const rows = Object.fromEntries(
        Object.entries(state.rows).filter(([id, row]) =>
          row.phase === "acting"
            ? true
            : listed.has(id) && row.afterLoad >= event.loadId,
        ),
      );
      return {
        phase: event.loadId === state.loadId ? "ready" : "loading",
        requests: event.requests,
        rows,
        loadId: state.loadId,
        appliedId: event.loadId,
      };
    }
    case "load-failed":
      if (event.loadId <= state.appliedId) return state;
      return {
        ...state,
        ...(event.loadId === state.loadId
          ? { phase: "failed", error: event.error }
          : { phase: "loading" }),
        appliedId: event.loadId,
      };
    case "act-started": {
      const row = state.rows[event.id];
      if (row?.phase === "acting" || row?.phase === "succeeded") return state;
      return {
        ...state,
        rows: {
          ...state.rows,
          [event.id]: { phase: "acting", action: event.action },
        },
      };
    }
    case "act-succeeded": {
      const row = state.rows[event.id];
      if (row?.phase !== "acting") return state;
      return {
        ...state,
        rows: {
          ...state.rows,
          [event.id]: {
            phase: "succeeded",
            action: row.action,
            afterLoad: state.loadId,
          },
        },
      };
    }
    case "act-failed": {
      if (state.rows[event.id]?.phase !== "acting") return state;
      const { [event.id]: _failed, ...rows } = state.rows;
      return { ...state, rows };
    }
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

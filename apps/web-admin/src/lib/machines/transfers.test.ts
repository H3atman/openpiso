import { describe, expect, it } from "vitest";
import {
  initialTransfersState,
  reduceTransfers,
  visibleRequests,
  type TransfersEvent,
  type TransfersState,
} from "./transfers";
import type { PendingPointsTransfer } from "../types";

function request(id: string): PendingPointsTransfer {
  return {
    id,
    from_account_id: "a-1",
    from_username: "ana",
    to_account_id: "a-2",
    to_username: "ben",
    points_centi: 500,
    code: "123456",
    created_at: "2026-10-20T00:00:00Z",
  };
}

const r1 = request("r1");
const r2 = request("r2");

function run(state: TransfersState, ...events: TransfersEvent[]) {
  return events.reduce(reduceTransfers, state);
}

describe("reduceTransfers list", () => {
  it("loads, then keeps requests visible while refreshing and after a failure", () => {
    const loading = run(initialTransfersState, { type: "load-started" });
    expect(loading).toEqual({
      phase: "loading",
      requests: [],
      rows: {},
      loadId: 1,
      appliedId: 0,
    });
    const ready = run(loading, {
      type: "load-succeeded",
      loadId: 1,
      requests: [r1],
    });
    expect(ready).toEqual({
      phase: "ready",
      requests: [r1],
      rows: {},
      loadId: 1,
      appliedId: 1,
    });
    const refreshing = run(ready, { type: "load-started" });
    expect(refreshing).toEqual({
      phase: "loading",
      requests: [r1],
      rows: {},
      loadId: 2,
      appliedId: 1,
    });
    expect(
      run(refreshing, { type: "load-failed", loadId: 2, error: "Offline" }),
    ).toEqual({
      phase: "failed",
      error: "Offline",
      requests: [r1],
      rows: {},
      loadId: 2,
      appliedId: 2,
    });
  });

  it("applies an older load that lands before the newer one does", () => {
    const second = run(
      initialTransfersState,
      { type: "load-started" },
      { type: "load-started" },
    );
    // Slow polls must not starve the list: load 1 lands while 2 is in flight.
    expect(
      run(second, { type: "load-succeeded", loadId: 1, requests: [r1] }),
    ).toEqual({
      phase: "loading",
      requests: [r1],
      rows: {},
      loadId: 2,
      appliedId: 1,
    });
  });

  it("ignores a load older than one already applied", () => {
    const applied = run(
      initialTransfersState,
      { type: "load-started" },
      { type: "load-started" },
      { type: "load-succeeded", loadId: 2, requests: [r2] },
    );
    expect(applied.requests).toEqual([r2]);
    const stale: TransfersEvent[] = [
      { type: "load-succeeded", loadId: 1, requests: [r1] },
      { type: "load-failed", loadId: 1, error: "late" },
    ];
    for (const event of stale) {
      expect(reduceTransfers(applied, event)).toBe(applied);
    }
  });
});

describe("reduceTransfers rows", () => {
  const ready = run(
    initialTransfersState,
    { type: "load-started" },
    { type: "load-succeeded", loadId: 1, requests: [r1, r2] },
  );

  it("acts on rows independently and concurrently", () => {
    const acting = run(
      ready,
      { type: "act-started", id: "r1", action: "approve" },
      { type: "act-started", id: "r2", action: "reject" },
    );
    expect(acting.rows).toEqual({
      r1: { phase: "acting", action: "approve" },
      r2: { phase: "acting", action: "reject" },
    });
  });

  it("fails one row without touching the other and allows a retry", () => {
    const failed = run(
      ready,
      { type: "act-started", id: "r1", action: "approve" },
      { type: "act-started", id: "r2", action: "approve" },
      { type: "act-failed", id: "r1" },
    );
    expect(failed.rows).toEqual({ r2: { phase: "acting", action: "approve" } });
    expect(
      run(failed, { type: "act-started", id: "r1", action: "reject" }).rows.r1,
    ).toEqual({ phase: "acting", action: "reject" });
  });

  it("ignores events illegal for the row's phase", () => {
    const acting = run(ready, {
      type: "act-started",
      id: "r1",
      action: "approve",
    });
    const succeeded = run(acting, { type: "act-succeeded", id: "r1" });
    const ignored: [TransfersState, TransfersEvent][] = [
      [ready, { type: "act-succeeded", id: "r1" }],
      [ready, { type: "act-failed", id: "r1" }],
      [acting, { type: "act-started", id: "r1", action: "reject" }],
      [succeeded, { type: "act-started", id: "r1", action: "reject" }],
      [succeeded, { type: "act-succeeded", id: "r1" }],
      [succeeded, { type: "act-failed", id: "r1" }],
    ];
    for (const [state, event] of ignored) {
      expect(reduceTransfers(state, event)).toBe(state);
    }
  });
});

describe("reduceTransfers poll and action interleaving", () => {
  const acting = run(
    initialTransfersState,
    { type: "load-started" },
    { type: "load-succeeded", loadId: 1, requests: [r1, r2] },
    { type: "act-started", id: "r1", action: "approve" },
  );

  it("keeps the acting row disabled when a poll lands mid-action", () => {
    const polled = run(
      acting,
      { type: "load-started" },
      { type: "load-succeeded", loadId: 2, requests: [r1, r2] },
    );
    expect(polled.rows).toEqual({ r1: { phase: "acting", action: "approve" } });
    expect(polled.requests).toEqual([r1, r2]);
  });

  it("does not resurrect a succeeded row from a poll started before success", () => {
    const polling = run(acting, { type: "load-started" }); // loadId 2 in flight
    const succeeded = run(polling, { type: "act-succeeded", id: "r1" });
    expect(succeeded.rows.r1).toEqual({
      phase: "succeeded",
      action: "approve",
      afterLoad: 2,
    });
    // The pre-success poll lands with the row still listed.
    const stale = run(succeeded, {
      type: "load-succeeded",
      loadId: 2,
      requests: [r1, r2],
    });
    expect(stale.rows.r1).toEqual({
      phase: "succeeded",
      action: "approve",
      afterLoad: 2,
    });
    expect(visibleRequests(stale)).toEqual([r2]);
  });

  it("drops the succeeded row once a load started after success lands", () => {
    const succeeded = run(acting, { type: "act-succeeded", id: "r1" });
    const reloading = run(succeeded, { type: "load-started" });
    // The reload may still list the row until the server confirms.
    expect(visibleRequests(reloading)).toEqual([r2]);
    const fresh = run(reloading, {
      type: "load-succeeded",
      loadId: 2,
      requests: [r2],
    });
    expect(fresh).toEqual({
      phase: "ready",
      requests: [r2],
      rows: {},
      loadId: 2,
      appliedId: 2,
    });
  });

  it("keeps a succeeded row hidden when a pre-success load lands after a post-success load started", () => {
    const polling = run(acting, { type: "load-started" }); // id 2
    const reloading = run(
      polling,
      { type: "act-succeeded", id: "r1" },
      { type: "load-started" }, // id 3
    );
    const landed = run(reloading, {
      type: "load-succeeded",
      loadId: 2,
      requests: [r1, r2],
    });
    expect(visibleRequests(landed)).toEqual([r2]);
    // The post-success load then confirms and releases the row entry.
    expect(
      run(landed, { type: "load-succeeded", loadId: 3, requests: [r2] }).rows,
    ).toEqual({});
  });

  it("drops row entries for ids absent from a freshly applied list, except acting ones", () => {
    const rows = run(
      acting,
      { type: "act-started", id: "r2", action: "reject" },
      { type: "load-started" },
      { type: "load-succeeded", loadId: 2, requests: [] },
    ).rows;
    expect(rows).toEqual({
      r1: { phase: "acting", action: "approve" },
      r2: { phase: "acting", action: "reject" },
    });
    const gone = run(
      acting,
      { type: "act-succeeded", id: "r1" },
      { type: "load-started" },
      { type: "load-succeeded", loadId: 2, requests: [r2] },
    );
    expect(gone.rows).toEqual({});
  });
});

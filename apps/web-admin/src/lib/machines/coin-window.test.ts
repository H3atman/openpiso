import { describe, expect, it } from "vitest";
import {
  initialCoinWindowState,
  reduceCoinWindow,
  type CoinWindowEvent,
  type CoinWindowState,
} from "./coin-window";

function run(state: CoinWindowState, ...events: CoinWindowEvent[]) {
  return events.reduce(reduceCoinWindow, state);
}

describe("reduceCoinWindow reads", () => {
  it("reads the window start", () => {
    const reading = run(initialCoinWindowState, { type: "read-started" });
    expect(reading).toEqual({
      requestId: 1,
      resetting: false,
      since: "",
      error: "",
    });
    expect(
      run(reading, { type: "read-succeeded", requestId: 1, since: "T1" }),
    ).toEqual({ requestId: 1, resetting: false, since: "T1", error: "" });
  });

  it("keeps the previous start and error while a read is in flight", () => {
    const ready = run(
      initialCoinWindowState,
      { type: "read-started" },
      { type: "read-succeeded", requestId: 1, since: "T1" },
    );
    expect(run(ready, { type: "read-started" })).toMatchObject({ since: "T1" });
    const failed = run(
      ready,
      { type: "read-started" },
      { type: "read-failed", requestId: 2, error: "Offline" },
    );
    expect(failed).toEqual({
      requestId: 2,
      resetting: false,
      since: "T1",
      error: "Offline",
    });
    expect(run(failed, { type: "read-started" })).toMatchObject({
      since: "T1",
      error: "Offline",
    });
  });

  it("clears the error on a successful retry", () => {
    const failed = run(
      initialCoinWindowState,
      { type: "read-started" },
      { type: "read-failed", requestId: 1, error: "Offline" },
    );
    expect(
      run(
        failed,
        { type: "read-started" },
        { type: "read-succeeded", requestId: 2, since: "T1" },
      ),
    ).toEqual({ requestId: 2, resetting: false, since: "T1", error: "" });
  });

  it("ignores results of a superseded read", () => {
    const second = run(
      initialCoinWindowState,
      { type: "read-started" },
      { type: "read-started" },
    );
    const stale: CoinWindowEvent[] = [
      { type: "read-succeeded", requestId: 1, since: "old" },
      { type: "read-failed", requestId: 1, error: "late" },
    ];
    for (const event of stale) {
      expect(reduceCoinWindow(second, event)).toBe(second);
    }
  });
});

describe("reduceCoinWindow reset", () => {
  it("a reset supersedes a read in flight and clears a read failure", () => {
    const reading = run(
      initialCoinWindowState,
      { type: "read-started" },
      { type: "reset-started" },
    );
    expect(reading.resetting).toBe(true);
    const reset = run(reading, { type: "reset-succeeded", since: "T2" });
    expect(reset).toEqual({
      requestId: 2,
      resetting: false,
      since: "T2",
      error: "",
    });
    // The older read answers after the reset, success or failure.
    for (const event of [
      { type: "read-succeeded", requestId: 1, since: "T1" },
      { type: "read-failed", requestId: 1, error: "late" },
    ] satisfies CoinWindowEvent[]) {
      expect(reduceCoinWindow(reset, event)).toBe(reset);
    }
  });

  it("a failed reset keeps the displayed window and re-enables reset", () => {
    const ready = run(
      initialCoinWindowState,
      { type: "read-started" },
      { type: "read-succeeded", requestId: 1, since: "T1" },
      { type: "reset-started" },
    );
    expect(run(ready, { type: "reset-failed" })).toEqual({
      requestId: 1,
      resetting: false,
      since: "T1",
      error: "",
    });
  });

  it("ignores a reset while one is in flight and stray reset results", () => {
    const resetting = run(initialCoinWindowState, { type: "reset-started" });
    expect(reduceCoinWindow(resetting, { type: "reset-started" })).toBe(
      resetting,
    );
    const idle = initialCoinWindowState;
    expect(reduceCoinWindow(idle, { type: "reset-failed" })).toBe(idle);
    expect(
      reduceCoinWindow(idle, { type: "reset-succeeded", since: "T" }),
    ).toBe(idle);
  });
});

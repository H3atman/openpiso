import { describe, expect, it } from "vitest";
import { reduceAdjustment, type AdjustmentState } from "./adjustment";
import type { Pc } from "../types";

const pc: Pc = {
  id: "pc-1",
  name: "PC 1",
  status: "online",
  remaining_seconds: 300,
  guest_remaining_seconds: 300,
  timer_mode: "guest",
  power_state: "active",
  shutdown_grace_seconds: 0,
  active_session: null,
  last_seen_at: null,
};

describe("reduceAdjustment", () => {
  it("opens the form after PCs load", () => {
    expect(
      reduceAdjustment({ phase: "loading" }, { type: "loaded", pcs: [pc] }),
    ).toEqual({
      phase: "ready",
      pcs: [
        {
          id: "pc-1",
          name: "PC 1",
          status: "online",
          remaining_seconds: 300,
          guest_remaining_seconds: 300,
          timer_mode: "guest",
          power_state: "active",
          shutdown_grace_seconds: 0,
          active_session: null,
          last_seen_at: null,
        },
      ],
      submitting: false,
      error: null,
    });
  });
  it("shows load failures inline while keeping an already-visible form", () => {
    expect(
      reduceAdjustment(
        { phase: "loading" },
        { type: "load-failed", error: "Server unavailable" },
      ),
    ).toEqual({ phase: "load-failed", error: "Server unavailable" });
    expect(
      reduceAdjustment(
        { phase: "ready", pcs: [], submitting: true, error: null },
        { type: "load-failed", error: "Refresh failed" },
      ),
    ).toEqual({
      phase: "ready",
      pcs: [],
      submitting: true,
      error: "Refresh failed",
    });
    expect(
      reduceAdjustment(
        { phase: "ready", pcs: [], submitting: false, error: null },
        { type: "load-failed", error: "Refresh failed" },
      ),
    ).toEqual({
      phase: "ready",
      pcs: [],
      submitting: false,
      error: "Refresh failed",
    });
  });
  it("starts one submission only from a ready form", () => {
    expect(
      reduceAdjustment(
        { phase: "ready", pcs: [], submitting: false, error: null },
        { type: "submit" },
      ),
    ).toEqual({ phase: "ready", pcs: [], submitting: true, error: null });
    const submitting: AdjustmentState = {
      phase: "ready",
      pcs: [],
      submitting: true,
      error: null,
    };
    expect(reduceAdjustment(submitting, { type: "submit" })).toBe(submitting);
    const loading: AdjustmentState = { phase: "loading" };
    expect(reduceAdjustment(loading, { type: "submit" })).toBe(loading);
    const failed: AdjustmentState = { phase: "load-failed", error: "Offline" };
    expect(reduceAdjustment(failed, { type: "submit" })).toBe(failed);
  });
  it("refreshes PCs and clears load errors without hiding or enabling the form", () => {
    expect(
      reduceAdjustment(
        {
          phase: "ready",
          pcs: [pc],
          submitting: true,
          error: "Refresh failed",
        },
        { type: "loaded", pcs: [] },
      ),
    ).toEqual({ phase: "ready", pcs: [], submitting: true, error: null });
  });
  it("finishes success or failure without losing PCs and ignores stray completions", () => {
    expect(
      reduceAdjustment(
        { phase: "ready", pcs: [pc], submitting: true, error: null },
        { type: "submit-finished" },
      ),
    ).toEqual({
      phase: "ready",
      pcs: [
        {
          id: "pc-1",
          name: "PC 1",
          status: "online",
          remaining_seconds: 300,
          guest_remaining_seconds: 300,
          timer_mode: "guest",
          power_state: "active",
          shutdown_grace_seconds: 0,
          active_session: null,
          last_seen_at: null,
        },
      ],
      submitting: false,
      error: null,
    });
    const ready: AdjustmentState = {
      phase: "ready",
      pcs: [pc],
      submitting: false,
      error: null,
    };
    expect(reduceAdjustment(ready, { type: "submit-finished" })).toBe(ready);
    const loading: AdjustmentState = { phase: "loading" };
    expect(reduceAdjustment(loading, { type: "submit-finished" })).toBe(
      loading,
    );
    const failed: AdjustmentState = { phase: "load-failed", error: "Offline" };
    expect(reduceAdjustment(failed, { type: "submit-finished" })).toBe(failed);
  });
});

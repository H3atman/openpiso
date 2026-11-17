import { describe, expect, test } from "bun:test";
import { initialSetupState, reduceSetup, type SetupEvent, type SetupState } from "../src/lib/machines/setup";
import type { DiscoveredServer } from "../src/lib/types";

const shopA: DiscoveredServer = { url: "http://192.168.1.10:3210", shop_name: "Shop A" };
const shopB: DiscoveredServer = { url: "http://192.168.1.11:3210", shop_name: "Shop B" };

function run(state: SetupState, ...events: SetupEvent[]): SetupState {
  return events.reduce(reduceSetup, state);
}

describe("setup machine", () => {
  test("starts by searching", () => {
    expect(initialSetupState).toEqual({ kind: "searching" });
  });

  test("exactly one server connects without asking", () => {
    expect(run(initialSetupState, { type: "found", servers: [shopA] })).toEqual({ kind: "saving", url: shopA.url });
  });

  test("several servers → pick → save", () => {
    const choosing = run(initialSetupState, { type: "found", servers: [shopA, shopB] });
    expect(choosing).toEqual({ kind: "choosing", servers: [shopA, shopB] });
    expect(reduceSetup(choosing, { type: "pick", url: shopB.url })).toEqual({ kind: "saving", url: shopB.url });
  });

  test("picking a server that was not offered is ignored", () => {
    const choosing = run(initialSetupState, { type: "found", servers: [shopA, shopB] });
    expect(reduceSetup(choosing, { type: "pick", url: "http://10.0.0.1:3210" })).toBe(choosing);
  });

  test("none found → wait → search again", () => {
    const waiting = run(initialSetupState, { type: "found", servers: [] });
    expect(waiting).toEqual({ kind: "waiting" });
    expect(reduceSetup(waiting, { type: "retry" })).toEqual({ kind: "searching" });
  });

  test("a stale discovery result after switching to manual is ignored", () => {
    const manual = run(initialSetupState, { type: "enterManually" });
    expect(manual).toEqual({ kind: "manual", address: "", error: "" });
    expect(reduceSetup(manual, { type: "found", servers: [shopA] })).toBe(manual);
  });

  test("a stale retry timer after switching to manual is ignored", () => {
    const manual = run(initialSetupState, { type: "found", servers: [] }, { type: "enterManually" });
    expect(reduceSetup(manual, { type: "retry" })).toBe(manual);
  });

  test("a discovery result while saving is ignored", () => {
    const saving = run(initialSetupState, { type: "found", servers: [shopA] });
    expect(reduceSetup(saving, { type: "found", servers: [shopB] })).toBe(saving);
  });

  test("manual entry is reachable from searching, waiting, and choosing", () => {
    const manual = { kind: "manual", address: "", error: "" };
    expect(run(initialSetupState, { type: "enterManually" })).toEqual(manual);
    expect(run(initialSetupState, { type: "found", servers: [] }, { type: "enterManually" })).toEqual(manual);
    expect(run(initialSetupState, { type: "found", servers: [shopA, shopB] }, { type: "enterManually" })).toEqual(manual);
  });

  test("manual submit saves the typed address", () => {
    expect(run(initialSetupState, { type: "enterManually" }, { type: "submit", address: "192.168.1.10" })).toEqual({
      kind: "saving",
      url: "192.168.1.10",
    });
  });

  test("a save failure shows the error with the address kept for editing", () => {
    const failed = run(initialSetupState, { type: "found", servers: [shopA] }, { type: "saveFailed", error: "boom" });
    expect(failed).toEqual({ kind: "manual", address: shopA.url, error: "boom" });
  });

  test("a saved config ends the flow", () => {
    const saved = run(initialSetupState, { type: "found", servers: [shopA] }, { type: "saved" });
    expect(saved).toEqual({ kind: "saved" });
    for (const event of [
      { type: "found", servers: [shopB] },
      { type: "retry" },
      { type: "enterManually" },
      { type: "search" },
      { type: "saveFailed", error: "late" },
    ] satisfies SetupEvent[]) {
      expect(reduceSetup(saved, event)).toBe(saved);
    }
  });

  test("search again from manual or choosing", () => {
    expect(run(initialSetupState, { type: "enterManually" }, { type: "search" })).toEqual({ kind: "searching" });
    expect(run(initialSetupState, { type: "found", servers: [shopA, shopB] }, { type: "search" })).toEqual({
      kind: "searching",
    });
  });

  test("events invalid in the current state leave it unchanged", () => {
    const states: SetupState[] = [
      { kind: "searching" },
      { kind: "waiting" },
      { kind: "choosing", servers: [shopA, shopB] },
      { kind: "manual", address: "", error: "" },
      { kind: "saving", url: shopA.url },
      { kind: "saved" },
    ];
    const invalid: Record<SetupState["kind"], SetupEvent[]> = {
      searching: [{ type: "retry" }, { type: "pick", url: shopA.url }, { type: "submit", address: "x" }, { type: "saved" }, { type: "saveFailed", error: "x" }, { type: "search" }],
      waiting: [{ type: "found", servers: [shopA] }, { type: "pick", url: shopA.url }, { type: "submit", address: "x" }, { type: "saved" }, { type: "search" }],
      choosing: [{ type: "found", servers: [shopA] }, { type: "retry" }, { type: "submit", address: "x" }, { type: "saved" }, { type: "saveFailed", error: "x" }],
      manual: [{ type: "found", servers: [shopA] }, { type: "retry" }, { type: "pick", url: shopA.url }, { type: "enterManually" }, { type: "saved" }, { type: "saveFailed", error: "x" }],
      saving: [{ type: "found", servers: [shopA] }, { type: "retry" }, { type: "pick", url: shopA.url }, { type: "enterManually" }, { type: "submit", address: "x" }, { type: "search" }],
      saved: [],
    };
    for (const state of states) {
      for (const event of invalid[state.kind]) {
        expect(reduceSetup(state, event)).toBe(state);
      }
    }
  });
});

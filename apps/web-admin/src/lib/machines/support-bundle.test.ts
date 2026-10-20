import { describe, expect, it } from "vitest";
import {
  initialSupportBundleState,
  reduceSupportBundle,
  type SupportBundleState,
} from "./support-bundle";

const shown = { pcId: "pc-1", bundle: "# Bundle" };

describe("reduceSupportBundle", () => {
  it("requests a bundle for a PC and keeps it under that PC", () => {
    const requesting = reduceSupportBundle(initialSupportBundleState, {
      type: "requested",
      pcId: "pc-1",
    });
    expect(requesting).toEqual({
      phase: "requesting",
      pcId: "pc-1",
      shown: null,
    });
    expect(
      reduceSupportBundle(requesting, {
        type: "succeeded",
        pcId: "pc-1",
        bundle: "# Bundle",
      }),
    ).toEqual({ phase: "ready", shown });
  });

  it("keeps the last ready bundle while a new request is in flight", () => {
    expect(
      reduceSupportBundle(
        { phase: "ready", shown },
        { type: "requested", pcId: "pc-2" },
      ),
    ).toEqual({ phase: "requesting", pcId: "pc-2", shown });
  });

  it("keeps the last ready bundle after a failed re-request", () => {
    expect(
      reduceSupportBundle(
        { phase: "requesting", pcId: "pc-2", shown },
        { type: "failed", pcId: "pc-2" },
      ),
    ).toEqual({ phase: "ready", shown });
  });

  it("returns to idle when the first request fails and allows a new one", () => {
    const idle = reduceSupportBundle(
      { phase: "requesting", pcId: "pc-1", shown: null },
      { type: "failed", pcId: "pc-1" },
    );
    expect(idle).toEqual({ phase: "idle", shown: null });
    expect(
      reduceSupportBundle(idle, { type: "requested", pcId: "pc-2" }),
    ).toEqual({ phase: "requesting", pcId: "pc-2", shown: null });
  });

  it("replaces the shown bundle only on success", () => {
    expect(
      reduceSupportBundle(
        { phase: "requesting", pcId: "pc-2", shown },
        { type: "succeeded", pcId: "pc-2", bundle: "# New" },
      ),
    ).toEqual({ phase: "ready", shown: { pcId: "pc-2", bundle: "# New" } });
  });

  it("ignores a second request while one is in flight", () => {
    const requesting: SupportBundleState = {
      phase: "requesting",
      pcId: "pc-1",
      shown: null,
    };
    expect(
      reduceSupportBundle(requesting, { type: "requested", pcId: "pc-2" }),
    ).toBe(requesting);
  });

  it("ignores results that do not match the request in flight", () => {
    const requesting: SupportBundleState = {
      phase: "requesting",
      pcId: "pc-2",
      shown: null,
    };
    expect(
      reduceSupportBundle(requesting, {
        type: "succeeded",
        pcId: "pc-1",
        bundle: "# Stale",
      }),
    ).toBe(requesting);
    expect(
      reduceSupportBundle(requesting, { type: "failed", pcId: "pc-1" }),
    ).toBe(requesting);
    const idle = initialSupportBundleState;
    expect(
      reduceSupportBundle(idle, {
        type: "succeeded",
        pcId: "pc-1",
        bundle: "x",
      }),
    ).toBe(idle);
    const ready: SupportBundleState = { phase: "ready", shown };
    expect(reduceSupportBundle(ready, { type: "failed", pcId: "pc-1" })).toBe(
      ready,
    );
  });
});

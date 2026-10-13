// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BrowserRouter } from "./router.svelte";

const LAST_ROUTE_KEY = "openpiso.last-route";

// A matchMedia stub that records listeners so a test can simulate the
// viewport crossing the 768px switch (ADR-0065).
function createFireableMatchMedia(initialMatches: boolean) {
  let matches = initialMatches;
  const listeners = new Set<EventListener>();
  return {
    get matches() {
      return matches;
    },
    media: "(max-width: 767px)",
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: (_type: string, listener: EventListener) => {
      listeners.add(listener);
    },
    removeEventListener: (_type: string, listener: EventListener) => {
      listeners.delete(listener);
    },
    dispatchEvent: () => false,
    setMatches(next: boolean) {
      matches = next;
      listeners.forEach((listener) => listener(new Event("change")));
    },
  };
}

function setMatchMedia(matches: boolean): void {
  vi.stubGlobal("matchMedia", () => createFireableMatchMedia(matches));
}

beforeEach(() => {
  window.history.replaceState(null, "", "/admin/settings");
  localStorage.removeItem(LAST_ROUTE_KEY);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("BrowserRouter bare Settings path (ADR-0065)", () => {
  it("keeps the bare URL and flags the section index below 768px", () => {
    setMatchMedia(true);
    const router = new BrowserRouter();
    router.start();

    expect(router.bareSettings).toBe(true);
    expect(window.location.pathname).toBe("/admin/settings");

    router.stop();
  });

  it("resolves the bare path to Points and replaces the entry at 768px and above", () => {
    setMatchMedia(false);
    const router = new BrowserRouter();
    router.start();

    expect(window.location.pathname).toBe("/admin/settings/points");

    router.stop();
  });

  it("records no last route for a section the owner never chose", () => {
    setMatchMedia(true);
    const router = new BrowserRouter();
    router.start();

    expect(localStorage.getItem(LAST_ROUTE_KEY)).toBeNull();

    router.stop();
  });

  it("stops showing the index at a named-route URL after the viewport shrinks", () => {
    const mql = createFireableMatchMedia(false);
    vi.stubGlobal("matchMedia", () => mql);
    const router = new BrowserRouter();
    router.start();
    expect(window.location.pathname).toBe("/admin/settings/points");

    mql.setMatches(true);

    expect(router.bareSettings).toBe(false);
    expect(router.route).toEqual({ page: "settings", section: "points" });

    router.stop();
  });

  it("replaces the bare URL when the viewport widens past the index", () => {
    const mql = createFireableMatchMedia(true);
    vi.stubGlobal("matchMedia", () => mql);
    const router = new BrowserRouter();
    router.start();
    expect(router.bareSettings).toBe(true);
    expect(window.location.pathname).toBe("/admin/settings");

    mql.setMatches(false);

    expect(window.location.pathname).toBe("/admin/settings/points");

    router.stop();
  });
});

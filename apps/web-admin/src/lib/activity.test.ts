// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { trackHumanActivity } from "./activity";

// jsdom makes `isTrusted` unforgeable, so a trusted event is handed straight
// to the listener the tracker registered on `window`; an untrusted one is a
// real script-dispatched event.
const listeners = new Map<string, EventListener>();

function userInput(type: string, isTrusted: boolean): void {
  if (isTrusted) {
    listeners.get(type)?.({ isTrusted } as Event);
  } else {
    window.dispatchEvent(new Event(type));
  }
}

describe("human activity reporting", () => {
  const report = vi.fn<() => void>();
  let stop: () => void;

  beforeEach(() => {
    vi.useFakeTimers();
    report.mockReset();
    listeners.clear();
    const add = window.addEventListener.bind(window);
    vi.spyOn(window, "addEventListener").mockImplementation(
      (type: string, listener: EventListenerOrEventListenerObject, options?: unknown) => {
        listeners.set(type, listener as EventListener);
        add(type, listener, options as AddEventListenerOptions);
      },
    );
    stop = trackHumanActivity(report);
  });

  afterEach(() => {
    stop();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("reports trusted pointer, keyboard, and touch input", () => {
    for (const type of ["pointerdown", "pointermove", "keydown", "touchstart"]) {
      report.mockClear();
      userInput(type, true);
      expect(report, type).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(60_000);
    }
  });

  it("ignores script-dispatched events", () => {
    userInput("pointerdown", false);
    userInput("keydown", false);

    expect(report).not.toHaveBeenCalled();
  });

  it("does not listen for focus or visibility changes", () => {
    expect([...listeners.keys()]).not.toContain("focus");
    expect([...listeners.keys()]).not.toContain("visibilitychange");
  });

  it("throttles bursts to one report per window and resumes afterwards", () => {
    userInput("keydown", true);
    userInput("keydown", true);
    userInput("pointermove", true);
    vi.advanceTimersByTime(59_000);
    userInput("keydown", true);
    expect(report).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(1_000);
    userInput("keydown", true);
    expect(report).toHaveBeenCalledTimes(2);
  });
});

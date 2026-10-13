/** Human input events that count as Owner Session activity (ADR-0074). */
const ACTIVITY_EVENTS = ["pointerdown", "pointermove", "keydown", "touchstart"] as const;

/** Monotonic (immune to wall-clock jumps). Well under the server's 30m idle limit. */
const REPORT_INTERVAL_MS = 60_000;

/**
 * Call `report` when the user touches, clicks, or types, at most once per
 * `REPORT_INTERVAL_MS`. Script-dispatched events (`isTrusted` false) and
 * background work such as polling never count. Returns a disposer.
 */
export function trackHumanActivity(report: () => void): () => void {
  let lastReportAt = -Infinity;
  const onInput = (event: Event) => {
    if (!event.isTrusted) return;
    const now = performance.now();
    if (now - lastReportAt < REPORT_INTERVAL_MS) return;
    lastReportAt = now;
    report();
  };
  for (const type of ACTIVITY_EVENTS) {
    window.addEventListener(type, onInput, { capture: true, passive: true });
  }
  return () => {
    for (const type of ACTIVITY_EVENTS) {
      window.removeEventListener(type, onInput, { capture: true });
    }
  };
}

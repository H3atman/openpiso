// Renders the Sales Report harness (reports-harness.html) in headless Chrome
// across a fixed state matrix and writes one PNG per state, via
// scripts/screenshot-runner.ts. Fetch is fixture-stubbed in
// src/test/reports-harness-main.ts.
// Usage: bun run scripts/reports-screenshots.ts --out <dir>
import { runScreenshots, type ShotState } from "./screenshot-runner";

interface State extends ShotState {
  scenario: "busy" | "empty";
  theme: "light" | "dark";
}

const busy = { scenario: "busy", fullPage: true } as const;
// The Attendance flow table re-renders for the picked period once it loads.
const weekPeriod = {
  fill: { scope: "main", name: "Attendance period", text: "week" },
  until: {
    expr: `[...document.querySelectorAll("main caption")].some((el) => el.textContent?.trim() === "Attendance flow breakdown by week")`,
    what: "Attendance flow breakdown did not re-render for the week period",
  },
} as const;

const states: State[] = [
  { ...busy, file: "reports-desktop-light.png", theme: "light", device: "desktop" },
  { ...busy, file: "reports-desktop-dark.png", theme: "dark", device: "desktop" },
  { ...busy, file: "reports-wide-light.png", theme: "light", device: "wide" },
  { ...busy, file: "reports-wide-dark.png", theme: "dark", device: "wide" },
  { ...busy, file: "reports-phone-light.png", theme: "light", device: "phone" },
  { ...busy, file: "reports-phone-dark.png", theme: "dark", device: "phone" },
  { file: "reports-empty-desktop-light.png", scenario: "empty", theme: "light", device: "desktop", fullPage: true },
  { file: "reports-empty-phone-dark.png", scenario: "empty", theme: "dark", device: "phone", fullPage: true },
  {
    ...busy,
    file: "reports-focus-desktop-light.png",
    theme: "light",
    device: "desktop",
    fullPage: false,
    tabTo: { scope: "main", name: "Apply Attendance filters" },
  },
  { ...busy, ...weekPeriod, file: "reports-attendance-range-desktop-light.png", theme: "light", device: "desktop" },
  { ...busy, ...weekPeriod, file: "reports-attendance-range-phone-dark.png", theme: "dark", device: "phone" },
];

// Ready once the overview GET was served and no section still shows a
// "Loading…" placeholder (sales, daily chart, or Attendance).
await runScreenshots({
  harness: "reports-harness.html",
  states,
  query: (state) => `scenario=${state.scenario}&theme=${state.theme}`,
  pageReady: `document.documentElement.dataset.harnessServed === "true" &&
    !!document.querySelector("main #attendance-liability-heading") &&
    !/Loading/.test(document.querySelector("main")?.textContent ?? "")`,
});

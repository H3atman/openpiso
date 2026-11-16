// Renders the Dashboard harness (dashboard-harness.html) in headless Chrome
// across a fixed state matrix and writes one PNG per state, via
// scripts/screenshot-runner.ts. Fetch is fixture-stubbed in
// src/test/dashboard-harness-main.ts.
// Usage: bun run scripts/dashboard-screenshots.ts --out <dir>
import { runScreenshots, type ShotState } from "./screenshot-runner";

interface State extends ShotState {
  scenario: "busy" | "quiet" | "empty";
  theme: "light" | "dark";
}

const pc01 = '[data-pc-id="pc-01"]';
const dialogOpen = { expr: "!!document.querySelector('[role=dialog]')", what: "no [role=dialog] opened" };
const busy = { scenario: "busy", fullPage: true } as const;

const states: State[] = [
  { ...busy, file: "dashboard-desktop-light.png", theme: "light", device: "desktop" },
  { ...busy, file: "dashboard-desktop-dark.png", theme: "dark", device: "desktop" },
  { ...busy, file: "dashboard-wide-light.png", theme: "light", device: "wide" },
  { ...busy, file: "dashboard-wide-dark.png", theme: "dark", device: "wide" },
  { ...busy, file: "dashboard-phone-light.png", theme: "light", device: "phone" },
  { ...busy, file: "dashboard-phone-dark.png", theme: "dark", device: "phone" },
  { file: "dashboard-quiet-desktop-light.png", scenario: "quiet", theme: "light", device: "desktop", fullPage: true },
  { file: "dashboard-quiet-phone-dark.png", scenario: "quiet", theme: "dark", device: "phone", fullPage: true },
  { file: "dashboard-empty-desktop-light.png", scenario: "empty", theme: "light", device: "desktop", fullPage: true },
  { file: "dashboard-empty-phone-light.png", scenario: "empty", theme: "light", device: "phone", fullPage: true },
  {
    ...busy,
    file: "dashboard-suppress-dialog-desktop-light.png",
    theme: "light",
    device: "desktop",
    fullPage: false,
    click: { scope: pc01, name: "Suppress" },
    until: dialogOpen,
  },
  {
    ...busy,
    file: "dashboard-suppress-dialog-phone-dark.png",
    theme: "dark",
    device: "phone",
    fullPage: false,
    click: { scope: pc01, name: "Suppress" },
    until: dialogOpen,
  },
  {
    ...busy,
    file: "dashboard-signout-dialog-desktop-dark.png",
    theme: "dark",
    device: "desktop",
    fullPage: false,
    click: { scope: pc01, name: "Sign Out" },
    until: dialogOpen,
  },
  {
    ...busy,
    file: "dashboard-check-results-desktop-light.png",
    theme: "light",
    device: "desktop",
    click: { scope: "main", name: "Check All PCs" },
    until: {
      expr: "document.querySelectorAll('[data-testid=monitoring-check]').length === Number(document.documentElement.dataset.harnessPcs)",
      what: "per-PC [data-testid=monitoring-check] results did not appear for every PC row",
    },
  },
  {
    ...busy,
    file: "dashboard-focus-desktop-light.png",
    theme: "light",
    device: "desktop",
    fullPage: false,
    tabTo: { scope: "[data-pc-id]", name: "Check" },
  },
];

// Ready once the dashboard GET was served and every fixture PC row rendered.
await runScreenshots({
  harness: "dashboard-harness.html",
  states,
  query: (state) => `scenario=${state.scenario}&theme=${state.theme}`,
  pageReady: `document.documentElement.dataset.harnessServed === "true" &&
    document.querySelectorAll("[data-pc-id]").length === Number(document.documentElement.dataset.harnessPcs)`,
});

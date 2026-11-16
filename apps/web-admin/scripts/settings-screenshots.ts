// Renders the Settings harness (settings-harness.html) in headless Chrome
// across a fixed state matrix and writes one PNG per state, via
// scripts/screenshot-runner.ts. Fetch is fixture-stubbed in
// src/test/settings-harness-main.ts.
// Usage: bun run scripts/settings-screenshots.ts --out <dir>
import { settingsSections, type SettingsSection } from "../src/lib/routing";
import { runScreenshots, type ShotState } from "./screenshot-runner";

interface State extends ShotState {
  section: SettingsSection | "index";
  scenario: "default" | "empty";
  theme: "light" | "dark";
}

const dialogOpen = { expr: "!!document.querySelector('[role=dialog]')", what: "no [role=dialog] opened" };
const emptySections: SettingsSection[] = ["pcs", "points", "wheel", "telegram", "coin-device", "coin-promo"];

const states: State[] = [
  ...settingsSections.flatMap(({ id: section }): State[] => {
    const base = { section, scenario: "default", fullPage: true } as const;
    return [
      { ...base, file: `settings-${section}-desktop-light.png`, theme: "light", device: "desktop" },
      { ...base, file: `settings-${section}-wide-dark.png`, theme: "dark", device: "wide" },
      { ...base, file: `settings-${section}-phone-light.png`, theme: "light", device: "phone" },
      { ...base, file: `settings-${section}-phone-dark.png`, theme: "dark", device: "phone" },
    ];
  }),
  {
    file: "settings-index-phone-light.png",
    section: "index",
    scenario: "default",
    theme: "light",
    device: "phone",
    fullPage: true,
  },
  ...emptySections.flatMap((section): State[] => {
    const base = { section, scenario: "empty", fullPage: true } as const;
    return [
      { ...base, file: `settings-${section}-empty-desktop-light.png`, theme: "light", device: "desktop" },
      { ...base, file: `settings-${section}-empty-phone-dark.png`, theme: "dark", device: "phone" },
    ];
  }),
  {
    file: "settings-pcs-remove-dialog-desktop-light.png",
    section: "pcs",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: false,
    click: { scope: "main", name: "Remove" },
    until: dialogOpen,
  },
  {
    file: "settings-pcs-remove-dialog-phone-dark.png",
    section: "pcs",
    scenario: "default",
    theme: "dark",
    device: "phone",
    fullPage: false,
    click: { scope: "main", name: "Remove" },
    until: dialogOpen,
  },
  {
    file: "settings-telegram-disconnect-dialog-desktop-light.png",
    section: "telegram",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: false,
    click: { scope: "main", name: "Disconnect" },
    until: dialogOpen,
  },
  {
    file: "settings-points-focus-desktop-light.png",
    section: "points",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: false,
    tabTo: { scope: "main", name: "Save tiers" },
  },
  {
    file: "settings-attendance-invalid-desktop-light.png",
    section: "attendance",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: true,
    fill: { scope: "main", name: "Daily threshold (pesos)", text: "12.345" },
    until: {
      expr: "!!document.querySelector('main [role=alert]')",
      what: "no inline [role=alert] validation error appeared",
    },
  },
];

// Ready once the harness has answered every GET the section issues on mount
// (data-harness-served) and no section still shows a "Loading…" placeholder.
await runScreenshots({
  harness: "settings-harness.html",
  states,
  query: (state) => `section=${state.section}&scenario=${state.scenario}&theme=${state.theme}`,
  pageReady: `document.documentElement.dataset.harnessServed === "true" &&
    !!document.querySelector("main h2") &&
    !/Loading/.test(document.querySelector("main")?.textContent ?? "")`,
});

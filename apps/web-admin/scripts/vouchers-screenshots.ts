// Renders the Vouchers harness (vouchers-harness.html) in headless Chrome
// across a fixed state matrix and writes one PNG per state, via
// scripts/screenshot-runner.ts. Fetch is fixture-stubbed in
// src/test/vouchers-harness-main.ts.
// Usage: bun run scripts/vouchers-screenshots.ts --out <dir>
//
// Not covered: the void confirmations. The page asks through the browser's
// native window.confirm, which is not part of the DOM and blocks the page
// until answered, so neither a screenshot nor the runner's click can see it.
import { runScreenshots, type ShotState } from "./screenshot-runner";

type Tab = "vouchers" | "batches" | "template";

interface State extends ShotState {
  tab: Tab;
  editor?: "html" | "css" | "script";
  scenario: "default" | "empty";
  theme: "light" | "dark";
}

const tabs: Tab[] = ["vouchers", "batches", "template"];
// The sandboxed preview frame has rendered; the default script has been applied.
const previewReady = "document.querySelector('[data-voucher-render=preview]')?.dataset.renderStatus === 'ready'";
const scriptApplied = `${previewReady} && document.querySelector("iframe[title='Voucher template live preview']").srcdoc.includes("dataset.tier")`;

const states: State[] = [
  ...tabs.flatMap((tab): State[] => {
    const base = { tab, scenario: "default", fullPage: true } as const;
    return [
      { ...base, file: `vouchers-${tab}-desktop-light.png`, theme: "light", device: "desktop" },
      { ...base, file: `vouchers-${tab}-desktop-dark.png`, theme: "dark", device: "desktop" },
      { ...base, file: `vouchers-${tab}-wide-light.png`, theme: "light", device: "wide" },
      { ...base, file: `vouchers-${tab}-phone-light.png`, theme: "light", device: "phone" },
      { ...base, file: `vouchers-${tab}-phone-dark.png`, theme: "dark", device: "phone" },
    ];
  }),
  {
    file: "vouchers-batches-detail-desktop-light.png",
    tab: "batches",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: true,
    click: { scope: "main", name: "Regular ₱50" },
    until: { expr: "!!document.querySelector('main table')", what: "batch detail did not open" },
  },
  {
    file: "vouchers-batches-detail-phone-dark.png",
    tab: "batches",
    scenario: "default",
    theme: "dark",
    device: "phone",
    fullPage: true,
    click: { scope: "main", name: "Regular ₱50" },
    until: { expr: "!!document.querySelector('main table')", what: "batch detail did not open" },
  },
  {
    file: "vouchers-template-script-run-desktop-light.png",
    tab: "template",
    editor: "script",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: true,
    click: { scope: "main", name: "Run" },
    until: { expr: scriptApplied, what: "preview did not re-render with the Template Script" },
  },
  {
    file: "vouchers-vouchers-focus-desktop-light.png",
    tab: "vouchers",
    scenario: "default",
    theme: "light",
    device: "desktop",
    fullPage: false,
    tabTo: { scope: "main", name: "Create Voucher" },
  },
  ...tabs.slice(0, 2).flatMap((tab): State[] => {
    const base = { tab, scenario: "empty", fullPage: true } as const;
    return [
      { ...base, file: `vouchers-${tab}-empty-desktop-light.png`, theme: "light", device: "desktop" },
      { ...base, file: `vouchers-${tab}-empty-phone-dark.png`, theme: "dark", device: "phone" },
    ];
  }),
];

// Ready once the harness has answered the page's mount requests and shown the
// requested tab (data-harness-served), the list has stopped loading, and, on
// the Template tab, the sandboxed preview has reported ready.
await runScreenshots({
  harness: "vouchers-harness.html",
  states,
  query: (state) =>
    `tab=${state.tab}&scenario=${state.scenario}&theme=${state.theme}${state.editor ? `&editor=${state.editor}` : ""}`,
  pageReady: `document.documentElement.dataset.harnessServed === "true" &&
    !!document.querySelector("main h2") &&
    !/Loading/.test(document.querySelector("main")?.textContent ?? "") &&
    (!document.querySelector("[data-voucher-render=preview]") || ${previewReady})`,
});

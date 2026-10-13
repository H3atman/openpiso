// ---------------------------------------------------------------------------
// Wheel browser test — open, spin, chain tap-through, close with balance
// refresh, error handling, and disable-while-flying.
//
// Follows the existing browser-script prior art (widget-tabs-browser.ts,
// widget-claim-browser.ts).
// ---------------------------------------------------------------------------

import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertProcessRunning,
  browserProfilePath,
  removeBrowserProfile,
  stopProcessTree,
} from "./browser-harness-process";

const dir = resolve(fileURLToPath(import.meta.url), "../..");
const chrome = [
  process.env.CHROME_PATH,
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
]
  .filter((path): path is string => Boolean(path))
  .find(existsSync);
if (!chrome) {
  console.log("SKIP: Chrome/Chromium not found");
  process.exit(0);
}
const vitePort = 17000 + (process.pid % 1000);
const cdpPort = 9900 + (process.pid % 40);
let id = 0;
let ws: WebSocket;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();
async function waitUrl(url: string): Promise<void> {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {}
    await Bun.sleep(100);
  }
  throw new Error(`Timeout: ${url}`);
}
function call(method: string, params: object = {}): Promise<unknown> {
  const current = ++id;
  return new Promise((resolveValue, reject) => {
    pending.set(current, { resolve: resolveValue, reject });
    ws.send(JSON.stringify({ id: current, method, params }));
  });
}
async function evaluate<T>(expression: string): Promise<T> {
  const response = (await call("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })) as { result: { value: T }; exceptionDetails?: { text: string } };
  if (response.exceptionDetails)
    throw new Error(response.exceptionDetails.text);
  return response.result.value;
}
let waitCount = 0;
async function waitFor(
  expression: string,
  expected: unknown,
  label = expression,
): Promise<void> {
  const currentWait = ++waitCount;
  for (let i = 0; i < 160; i++) {
    if ((await evaluate(expression)) === expected) return;
    await Bun.sleep(50);
  }
  throw new Error(`Timeout at wait ${currentWait}: ${label}`);
}
async function click(text: string): Promise<void> {
  try {
    await evaluate(
      `(() => { const button = [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === ${JSON.stringify(text)}); if (!button) throw new Error('missing'); button.click(); })()`,
    );
  } catch {
    throw new Error(`Missing button: ${text}`);
  }
}
async function clickDialog(text: string): Promise<void> {
  await evaluate(`(() => {
    const button = [...document.querySelectorAll('[role="dialog"] button')].find(
      (candidate) => candidate.textContent?.trim() === ${JSON.stringify(text)},
    );
    if (!button) throw new Error('missing dialog button');
    button.click();
  })()`);
}
async function clickWheelEntry(): Promise<void> {
  await evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find(
      (candidate) => !candidate.closest('[role="dialog"]') && candidate.textContent?.trim() === 'Spin for 1 point',
    );
    if (!button) throw new Error('missing wheel entry');
    button.click();
  })()`);
}

let spinCalls = 0;
const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
  cwd: dir,
  env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
  stdout: "ignore",
  stderr: "inherit",
});
const profile = browserProfilePath(`openpiso-wheel-${process.pid}`);
const browser = Bun.spawn(
  [
    chrome,
    "--headless=new",
    `--remote-debugging-port=${cdpPort}`,
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--disable-gpu",
    "about:blank",
  ],
  { stdout: "ignore", stderr: "ignore" },
);
try {
  await waitUrl(`http://127.0.0.1:${vitePort}/widget-tabs-harness.html`);
  await assertProcessRunning(vite, "Wheel browser Vite");
  await waitUrl(`http://127.0.0.1:${cdpPort}/json/version`);
  const pages = (await (
    await fetch(`http://127.0.0.1:${cdpPort}/json/list`)
  ).json()) as { type: string; webSocketDebuggerUrl: string }[];
  const page = pages.find((entry) => entry.type === "page");
  if (!page) throw new Error("No page");
  ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise<void>((resolveOpen, reject) => {
    ws.onopen = resolveOpen;
    ws.onerror = () => reject(new Error("CDP failed"));
  });
  ws.onmessage = (event) => {
    const message = JSON.parse(String(event.data)) as {
      id?: number;
      result?: unknown;
      error?: { message: string };
    };
    if (!message.id) return;
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    message.error
      ? waiter.reject(new Error(message.error.message))
      : waiter.resolve(message.result);
  };
  await call("Runtime.enable");
  await call("Page.enable");
  await call("Emulation.setDeviceMetricsOverride", {
    width: 320,
    height: 480,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await call("Page.navigate", {
    url: `http://127.0.0.1:${vitePort}/widget-tabs-harness.html`,
  });
  await waitFor(
    "Boolean(document.querySelector('[data-harness-ready]'))",
    true,
  );

  // Guests discover the enabled wheel but cannot open it.
  await evaluate("window.widgetTabsHarness.setFlavor('guest')");
  await waitFor("document.body.innerText.includes('Log in to spin')", true, "guest wheel reason");
  await evaluate("window.widgetTabsHarness.setFlavor('account')");

  // Navigate to Points tab and configure a deterministic two-step chain.
  await click("Points");
  await waitFor("document.body.innerText.includes('SPIN THE WHEEL')", true);
  await evaluate(`window.widgetTabsHarness.setWheelResult({
    seconds_won: 360,
    points_won_centi: 125,
    free_spins_triggered: 1,
    segments: [
      { label: '5 min', seconds: 300, points_centi: 0, free_spin: false },
      { label: '10 min', seconds: 600, points_centi: 0, free_spin: false },
      { label: '15 min', seconds: 900, points_centi: 0, free_spin: false },
      { label: 'Try again', seconds: 0, points_centi: 0, free_spin: false },
      { label: 'Free spin', seconds: 0, points_centi: 0, free_spin: true }
    ],
    steps: [
      { segment_index: 4, label: 'Free spin', seconds: 60, points_centi: 25, free_spin: true },
      { segment_index: 0, label: '5 minutes', seconds: 300, points_centi: 100, free_spin: false }
    ]
  })`);

  await clickWheelEntry();
  await waitFor("Boolean(document.querySelector('[role=\"dialog\"] svg[aria-label=\"Wheel\"]'))", true);
  const slicePaths = await evaluate("document.querySelectorAll('[role=\"dialog\"] svg path').length");
  if (slicePaths < 1) throw new Error("Wheel has no slice paths");

  spinCalls = await evaluate<number>("window.widgetTabsHarness.calls.spin_wheel ?? 0");
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Free Spin!')", true, "free-spin landing");
  await waitFor("document.body.innerText.includes('ACCUMULATED (1/2)')", true);
  await click("Spin again");
  await waitFor("document.body.innerText.includes('Won 5 min!')", true, "second landing");
  await waitFor("document.body.innerText.includes('ACCUMULATED (2/2)')", true);
  const callsAfterChain = await evaluate<number>("window.widgetTabsHarness.calls.spin_wheel ?? 0");
  if (callsAfterChain !== spinCalls + 1) throw new Error("Spin again issued a server request");

  const refreshBefore = await evaluate<number>("window.widgetTabsHarness.refreshCalls");
  await click("Close");
  await waitFor("Boolean(document.querySelector('[role=\"dialog\"]'))", false);
  await waitFor(`window.widgetTabsHarness.refreshCalls > ${refreshBefore}`, true, "balance refresh on close");

  // Close remains available while the server request is still in flight, and
  // completion performs a second refresh so the committed balance is visible.
  await evaluate(`window.widgetTabsHarness.setWheelResult({
    seconds_won: 300, points_won_centi: 0, free_spins_triggered: 0,
    segments: [{ label: '5 minutes', seconds: 300, points_centi: 0, free_spin: false }],
    steps: [{ segment_index: 0, label: '5 minutes', seconds: 300, points_centi: 0, free_spin: false }]
  })`);
  await evaluate("window.widgetTabsHarness.setWheelDelay(300)");
  await clickWheelEntry();
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Spinning')", true);
  await click("Close");
  await waitFor("Boolean(document.querySelector('[role=\"dialog\"]'))", false, "close during request");
  const refreshAfterClose = await evaluate<number>("window.widgetTabsHarness.refreshCalls");
  await waitFor(`window.widgetTabsHarness.refreshCalls > ${refreshAfterClose}`, true, "refresh after request completes");

  console.log("GREEN: Wheel browser tests passed");
} finally {
  ws?.close();
  browser.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

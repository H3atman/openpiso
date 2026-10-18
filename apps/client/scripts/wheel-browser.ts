// ---------------------------------------------------------------------------
// Wheel browser test — open, spin, chain tap-through, close with balance
// refresh, error handling, close-unavailable-while-flying, and the password
// confirmation flow (ADR-0088). Screenshots land in target/wheel-*.png.
//
// Follows the existing browser-script prior art (widget-tabs-browser.ts,
// widget-claim-browser.ts).
// ---------------------------------------------------------------------------

import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
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

/** Type into the wheel's password field the way a customer would. */
async function typePassword(value: string): Promise<void> {
  await evaluate(`(() => {
    const input = document.querySelector('#wheel-password');
    if (!input) throw new Error('missing password field');
    input.value = ${JSON.stringify(value)};
    input.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
}
const hasPasswordField = "Boolean(document.querySelector('#wheel-password'))";
const dialogText = "document.querySelector('[role=\"dialog\"]')?.innerText ?? ''";
const spinPasswords = "window.widgetTabsHarness.spinPasswords";
async function openWheelDialog(): Promise<void> {
  await clickWheelEntry();
  await waitFor("Boolean(document.querySelector('[role=\"dialog\"] svg[aria-label=\"Wheel\"]'))", true, "wheel dialog open");
}
async function closeWheelDialog(): Promise<void> {
  await clickDialog("Close");
  await waitFor("Boolean(document.querySelector('[role=\"dialog\"]'))", false, "wheel dialog closed");
}
async function saveScreenshot(name: string): Promise<void> {
  const shot = (await call("Page.captureScreenshot", { format: "png" })) as { data: string };
  const artifact = resolve(dir, "../../target", name);
  await mkdir(resolve(artifact, ".."), { recursive: true });
  await Bun.write(artifact, Buffer.from(shot.data, "base64"));
  console.log(`ARTIFACT: ${artifact}`);
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

  // Close is unavailable while the spin is in flight (requesting, then the
  // wheel turning): the spin is already charged, so closing would lose the
  // prize. It returns once the landing is revealed, and closing refreshes the
  // balance again.
  await evaluate(`window.widgetTabsHarness.setWheelResult({
    seconds_won: 300, points_won_centi: 0, free_spins_triggered: 0,
    segments: [{ label: '5 minutes', seconds: 300, points_centi: 0, free_spin: false }],
    steps: [{ segment_index: 0, label: '5 minutes', seconds: 300, points_centi: 0, free_spin: false }]
  })`);
  await evaluate("window.widgetTabsHarness.setWheelDelay(300)");
  const dialogHasClose = `[...document.querySelectorAll('[role="dialog"] button')].some((b) => b.textContent?.trim() === 'Close')`;
  await clickWheelEntry();
  await waitFor(dialogHasClose, true, "Close available before spinning");
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Spinning')", true);
  await waitFor(dialogHasClose, false, "Close hidden while the request is in flight");
  await waitFor("document.body.innerText.includes('Won 5 min')", true, "reveal after request");
  await waitFor(dialogHasClose, true, "Close returns once revealed");
  const refreshAfterSettle = await evaluate<number>("window.widgetTabsHarness.refreshCalls");
  await click("Close");
  await waitFor("Boolean(document.querySelector('[role=\"dialog\"]'))", false, "close after reveal");
  await waitFor(`window.widgetTabsHarness.refreshCalls > ${refreshAfterSettle}`, true, "refresh on close");

  // -- Password confirmation (ADR-0088). Failure paths first. ---------------
  await evaluate("window.widgetTabsHarness.setWheelDelay(0)");
  const singleStep = `{
    seconds_won: 300, points_won_centi: 0, free_spins_triggered: 0,
    segments: [{ label: '5 min', seconds: 300, points_centi: 0, free_spin: false }],
    steps: [{ segment_index: 0, label: '5 min', seconds: 300, points_centi: 0, free_spin: false }]
  }`;
  const freeSpinChain = `{
    seconds_won: 300, points_won_centi: 0, free_spins_triggered: 1,
    segments: [
      { label: '5 min', seconds: 300, points_centi: 0, free_spin: false },
      { label: 'Free spin', seconds: 0, points_centi: 0, free_spin: true }
    ],
    steps: [
      { segment_index: 1, label: 'Free spin', seconds: 0, points_centi: 0, free_spin: true },
      { segment_index: 0, label: '5 min', seconds: 300, points_centi: 0, free_spin: false }
    ]
  }`;
  await evaluate(`window.widgetTabsHarness.setWheelResult(${singleStep})`);

  // Throttled: the server's lockout message is shown, the prompt stays, and the
  // field is cleared.
  await evaluate("window.widgetTabsHarness.setWheelPassword({ settings: true, server: 'hunter2' })");
  await evaluate("window.widgetTabsHarness.setWheelThrottle(30)");
  await openWheelDialog();
  await clickDialog("Spin for 1 point");
  await waitFor(hasPasswordField, true, "prompt before throttle");
  await typePassword("hunter2");
  await clickDialog("Spin for 1 point");
  await waitFor(`(${dialogText}).includes('Please wait 30 seconds')`, true, "throttle message");
  await waitFor(hasPasswordField, true, "prompt stays while throttled");
  await waitFor("document.querySelector('#wheel-password').value", "", "field cleared after throttle");
  await evaluate("window.widgetTabsHarness.setWheelThrottle(null)");
  await closeWheelDialog();

  // Stale settings: the client believes the setting is off, the server says
  // otherwise. The first request goes out without a password and the
  // password_required answer opens the prompt without an error.
  await evaluate("window.widgetTabsHarness.setWheelPassword({ settings: false, server: 'hunter2' })");
  await openWheelDialog();
  let attempts = await evaluate<number>(`${spinPasswords}.length`);
  await clickDialog("Spin for 1 point");
  await waitFor(hasPasswordField, true, "prompt after password_required");
  if ((await evaluate<number>(`${spinPasswords}.length`)) !== attempts + 1) throw new Error("Stale settings: expected exactly one passwordless request");
  if ((await evaluate<string | undefined>(`${spinPasswords}.at(-1)`)) !== undefined) throw new Error("Stale settings: first request carried a password");
  if ((await evaluate<string>(dialogText)).includes("Incorrect")) throw new Error("password_required must not show a wrong-password error");
  await typePassword("hunter2");
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Won 5 min!')", true, "win after stale-settings prompt");
  await closeWheelDialog();

  // Wrong password: error shown, field cleared, retry allowed, no result.
  await evaluate("window.widgetTabsHarness.setWheelPassword({ settings: true, server: 'hunter2' })");
  await openWheelDialog();
  attempts = await evaluate<number>(`${spinPasswords}.length`);
  await clickDialog("Spin for 1 point");
  await waitFor(hasPasswordField, true, "prompt when setting is on");
  if ((await evaluate<number>(`${spinPasswords}.length`)) !== attempts) throw new Error("Prompt must appear before any request is sent");
  await typePassword("nope");
  await clickDialog("Spin for 1 point");
  await waitFor(`(${dialogText}).includes('Incorrect password')`, true, "wrong-password error");
  await waitFor("document.querySelector('#wheel-password').value", "", "field cleared after wrong password");
  if ((await evaluate<string>(dialogText)).includes("Won")) throw new Error("Wrong password must not produce a result");
  await saveScreenshot("wheel-password-error.png");

  // Retry with the correct password: the wheel spins and the result shows.
  await typePassword("hunter2");
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Won 5 min!')", true, "win after correct password");
  await waitFor(hasPasswordField, false, "prompt closes after success");
  if ((await evaluate<string[]>(`${spinPasswords}.slice(-2)`)).join() !== "nope,hunter2") throw new Error("Unexpected password attempts");
  await saveScreenshot("wheel-password-success.png");

  // A second paid spin asks again.
  await clickDialog("Spin Again");
  await waitFor(hasPasswordField, true, "prompt for the next paid spin");
  await closeWheelDialog();

  // Setting off: Spin goes straight to the request, no prompt, no password.
  await evaluate("window.widgetTabsHarness.setWheelPassword({ settings: false, server: null })");
  await openWheelDialog();
  attempts = await evaluate<number>(`${spinPasswords}.length`);
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Won 5 min!')", true, "win with confirmation off");
  if (await evaluate<boolean>(hasPasswordField)) throw new Error("Prompt appeared with the setting off");
  if ((await evaluate<string | undefined>(`${spinPasswords}.at(-1)`)) !== undefined || (await evaluate<number>(`${spinPasswords}.length`)) !== attempts + 1) {
    throw new Error("Setting off: expected one request without a password");
  }
  await closeWheelDialog();

  // Free-spin chain: one prompt for the paid spin, "Spin again" never asks.
  await evaluate("window.widgetTabsHarness.setWheelPassword({ settings: true, server: 'hunter2' })");
  await evaluate(`window.widgetTabsHarness.setWheelResult(${freeSpinChain})`);
  await openWheelDialog();
  attempts = await evaluate<number>(`${spinPasswords}.length`);
  await clickDialog("Spin for 1 point");
  await waitFor(hasPasswordField, true, "prompt for the chain's paid spin");
  await typePassword("hunter2");
  await clickDialog("Spin for 1 point");
  await waitFor("document.body.innerText.includes('Free Spin!')", true, "free-spin landing with password");
  await click("Spin again");
  await waitFor("document.body.innerText.includes('Won 5 min!')", true, "chain's second landing");
  if (await evaluate<boolean>(hasPasswordField)) throw new Error("Free-spin chain prompted for a password");
  if ((await evaluate<number>(`${spinPasswords}.length`)) !== attempts + 1) throw new Error("Free-spin chain sent an extra request");
  await closeWheelDialog();

  console.log("GREEN: Wheel browser tests passed");
} finally {
  ws?.close();
  browser.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

// Browser regression matrix for the real production Fullscreen Lock.
// Requires Chrome. Usage: bun run scripts/fullscreen-lock-browser.ts
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
const chromePath = [
  process.env.CHROME_PATH,
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
]
  .filter((path): path is string => Boolean(path))
  .find(existsSync);
if (!chromePath) {
  console.log("SKIP: Chrome/Chromium not found");
  process.exit(0);
}
const vitePort = 14_000 + (process.pid % 1_000);
const cdpPort = 9_400 + (process.pid % 400);
const base = `http://127.0.0.1:${vitePort}/fullscreen-lock-harness.html`;
let id = 0;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();

async function wait(url: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* starting */
    }
    await Bun.sleep(150);
  }
  throw new Error(`timeout: ${url}`);
}
function call(
  ws: WebSocket,
  method: string,
  params: object = {},
): Promise<unknown> {
  const messageId = ++id;
  return new Promise((resolveValue, reject) => {
    pending.set(messageId, { resolve: resolveValue, reject });
    ws.send(JSON.stringify({ id: messageId, method, params }));
  });
}
async function evaluate(ws: WebSocket, expression: string): Promise<unknown> {
  const response = (await call(ws, "Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })) as { result: { value: unknown } };
  return response.result.value;
}
async function waitFor(
  ws: WebSocket,
  condition: string,
  timeoutMs = 8_000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await evaluate(ws, condition)) return true;
    await Bun.sleep(100);
  }
  return false;
}

const scenarios = [
  ["setup", "config=fresh&lock=setup", "Set up this PC"],
  ["unregistered", "lock=unregistered", "This PC isn't registered"],
  ["connecting", "lock=connecting", "Reaching the admin server"],
  ["disconnected", "lock=disconnected", "Can't reach server"],
  ["offline shutdown", "lock=offline-shutdown&countdown=90", "OFFLINE SHUTDOWN"],
  ["session ended", "lock=session-ended", "SESSION ENDED"],
  ["guest session paused", "lock=guest-session-ended", "Continue Guest session"],
  ["insert coin", "lock=insert-coin", "Insert coin"],
  ["shop closed", "lock=shop-closed", "Shop Closed"],
  ["shutdown 24", "lock=shutdown&countdown=24", "24"],
  ["shutdown 10", "lock=shutdown&countdown=10", "10"],
  ["shutdown zero", "lock=shutdown&countdown=0", "00"],
] as const;
const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
  cwd: dir,
  env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
  stdout: "ignore",
  stderr: "inherit",
});
const profile = browserProfilePath(`openpiso-lock-${process.pid}`);
const chrome = Bun.spawn(
  [
    chromePath,
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
  await wait(base);
  await assertProcessRunning(vite, "Fullscreen Lock harness Vite");
  await wait(`http://127.0.0.1:${cdpPort}/json/version`);
  const pages = (await (
    await fetch(`http://127.0.0.1:${cdpPort}/json/list`)
  ).json()) as { type: string; webSocketDebuggerUrl: string }[];
  const page = pages.find((item) => item.type === "page");
  if (!page) throw new Error("no Chrome page");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise<void>((resolveOpen, reject) => {
    ws.onopen = () => resolveOpen();
    ws.onerror = () => reject(new Error("CDP connection failed"));
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
  await call(ws, "Runtime.enable");
  await call(ws, "Page.enable");
  for (const [name, query, expected] of scenarios) {
    await call(ws, "Page.navigate", { url: `${base}?${query}` });
    let body = "";
    for (let attempt = 0; attempt < 80; attempt++) {
      body = String(await evaluate(ws, "document.body.innerText"));
      if (body.includes(expected)) break;
      await Bun.sleep(100);
    }
    if (!body.includes(expected)) {
      throw new Error(`${name}: missing ${expected}; rendered ${body}`);
    }
    console.log(`[GREEN] ${name}`);
  }
  await call(ws, "Page.navigate", { url: `${base}?lock=shop-closed` });
  await Bun.sleep(250);
  const closedSurface = (await evaluate(
    ws,
    `(() => { const text = document.body.innerText; return text.includes('Closed — opens') && !document.querySelector('main form') && !text.includes('insert coin to login as guest'); })()`,
  )) as boolean;
  if (!closedSurface)
    throw new Error("Shop Closed Lock did not hide customer entry affordances");
  console.log("[GREEN] Shop Closed Lock hides entry affordances");

  await call(ws, "Page.navigate", { url: `${base}?lock=shop-closed&override=true` });
  await Bun.sleep(250);
  const overrideSurface = String(await evaluate(ws, "document.body.innerText"));
  if (!overrideSurface.includes("Closed until"))
    throw new Error("Shop Override Lock did not render its expiry copy");
  console.log("[GREEN] Shop Override Lock renders override expiry");

  // ADR-0048 (amended 2026-08-02): a Tamper Kill is logged, never announced,
  // so there is no tamper surface left for this harness to render. This mounts
  // Lock directly, never App; installed-kiosk checks cover the Guard response.

  for (const [width, height] of [
    [1280, 800],
    [360, 480],
    [800, 420],
  ]) {
    await call(ws, "Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await call(ws, "Page.navigate", { url: `${base}?lock=insert-coin` });
    await Bun.sleep(300);
    const result = (await evaluate(
      ws,
      `({ overflow: document.documentElement.scrollWidth > innerWidth, reachable: document.documentElement.scrollHeight >= innerHeight, buttons: [...document.querySelectorAll('button')].every(b => b.getBoundingClientRect().right <= innerWidth + .5) })`,
    )) as { overflow: boolean; reachable: boolean; buttons: boolean };
    if (result.overflow || !result.reachable || !result.buttons)
      throw new Error(`${width}x${height}: layout unreachable/overflowing`);
    console.log(`[GREEN] viewport ${width}x${height}`);
  }
  await call(ws, "Emulation.setDeviceMetricsOverride", {
    width: 1280,
    height: 800,
    deviceScaleFactor: 1,
    mobile: false,
  });
  for (const query of [
    "config=fresh&lock=setup",
    "lock=connecting",
    "lock=disconnected",
    "lock=unregistered",
  ]) {
    await call(ws, "Page.navigate", { url: `${base}?${query}` });
    await Bun.sleep(250);
    const authDisabled = await evaluate(
      ws,
      "[...document.querySelectorAll('main form input, main form button')].every(el => el.disabled)",
    );
    if (!authDisabled)
      throw new Error(`${query}: authentication controls enabled`);
  }
  console.log("[GREEN] unavailable authentication states");

  await call(ws, "Page.navigate", { url: `${base}?lock=insert-coin` });
  await Bun.sleep(250);
  const modeChanged = await evaluate(
    ws,
    `(async () => { const button = [...document.querySelectorAll('main button')].find(b => b.textContent.includes('Create account')); button?.click(); await new Promise(r => requestAnimationFrame(r)); return [...document.querySelectorAll('main button')].some(b => b.textContent.trim() === 'Sign up'); })()`,
  );
  if (!modeChanged) throw new Error("Create account did not switch mode");
  console.log("[GREEN] explicit authentication modes");

  await call(ws, "Page.navigate", {
    url: `${base}?lock=insert-coin&auth-route=true`,
  });
  await Bun.sleep(250);
  const returnedToGuest = await evaluate(
    ws,
    `(async () => { const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Return to Guest session'); if (!button) return false; button.click(); await new Promise(r => requestAnimationFrame(r)); return ![...document.querySelectorAll('button')].some(b => b.textContent.trim() === 'Return to Guest session'); })()`,
  );
  if (!returnedToGuest)
    throw new Error("authentication route did not return to Guest session");
  console.log("[GREEN] authentication route returns to Guest session");

  await call(ws, "Page.navigate", {
    url: `${base}?lock=insert-coin&auth-route=true`,
  });
  await Bun.sleep(250);
  const authenticatedRoute = await evaluate(
    ws,
    `(async () => { const username = document.querySelector('#auth-username'); const password = document.querySelector('#auth-password'); if (!(username instanceof HTMLInputElement) || !(password instanceof HTMLInputElement)) return false; username.value = 'alex'; username.dispatchEvent(new Event('input', { bubbles: true })); password.value = 'password'; password.dispatchEvent(new Event('input', { bubbles: true })); username.form?.requestSubmit(); await new Promise(r => setTimeout(r, 100)); const text = document.body.innerText; return text.toLowerCase().includes('alex') && !text.includes('Return to Guest session'); })()`,
  );
  if (!authenticatedRoute)
    throw new Error("successful authentication did not clear route intent");
  console.log("[GREEN] successful authentication clears route intent");

  await call(ws, "Page.navigate", { url: `${base}?route-harness` });
  await Bun.sleep(250);
  const routedBackToWidget = await evaluate(
    ws,
    `(async () => { const login = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Log in'); login?.click(); await new Promise(r => requestAnimationFrame(r)); const back = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Return to Guest session'); if (!back) return false; back.click(); await new Promise(r => requestAnimationFrame(r)); return document.body.innerText.includes('Guest session') && document.body.innerText.includes('00:10:00'); })()`,
  );
  if (!routedBackToWidget)
    throw new Error("Widget login route did not return with Guest Time intact");

  await call(ws, "Page.navigate", { url: `${base}?route-harness` });
  await Bun.sleep(250);
  const routedLoginSucceeded = await evaluate(
    ws,
    `(async () => { [...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Log in')?.click(); await new Promise(r => requestAnimationFrame(r)); const username = document.querySelector('#auth-username'); const password = document.querySelector('#auth-password'); if (!(username instanceof HTMLInputElement) || !(password instanceof HTMLInputElement)) return false; username.value = 'alex'; username.dispatchEvent(new Event('input', { bubbles: true })); password.value = 'password'; password.dispatchEvent(new Event('input', { bubbles: true })); username.form?.requestSubmit(); await new Promise(r => setTimeout(r, 100)); return Boolean(document.querySelector('[role=tab][aria-selected=true]')?.textContent.includes('Time')) && document.body.innerText.includes('Log out') && !document.body.innerText.includes('Return to Guest session'); })()`,
  );
  if (!routedLoginSucceeded)
    throw new Error("Widget login route did not follow Account view selection");

  await call(ws, "Page.navigate", { url: `${base}?route-harness` });
  await Bun.sleep(250);
  const loginControlRendered = await waitFor(
    ws,
    `[...document.querySelectorAll('button')].some(b => b.textContent.trim() === 'Log in')`,
  );
  if (!loginControlRendered) {
    const text = String(await evaluate(ws, "document.body.innerText"));
    throw new Error(`authentication harness did not render Log in: ${text}`);
  }
  await evaluate(
    ws,
    `[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Log in')?.click()`,
  );
  const authenticationRouteRendered = await waitFor(
    ws,
    `document.body.innerText.includes('Return to Guest session')`,
  );
  if (!authenticationRouteRendered) {
    const text = String(await evaluate(ws, "document.body.innerText"));
    throw new Error(`authentication route did not render: ${text}`);
  }
  await evaluate(ws, `window.dispatchEvent(new Event('authoritative-lock'))`);
  const lockStateSettled = await waitFor(
    ws,
    `document.body.innerText.includes('SESSION ENDED') && !document.body.innerText.includes('Return to Guest session')`,
  );
  if (!lockStateSettled) {
    const text = String(await evaluate(ws, "document.body.innerText"));
    throw new Error(`authoritative lock state did not settle: ${text}`);
  }
  console.log("[GREEN] Widget authentication route lifecycle");

  await call(ws, "Page.navigate", {
    url: `${base}?account=account&lock=insert-coin`,
  });
  await Bun.sleep(250);
  const collapsedRecovery = await evaluate(
    ws,
    `(() => { const text = document.body.innerText; return text.toLowerCase().includes('alex') && text.includes('12.00 points') && text.includes('Log out to save your remaining time') && text.includes('Claim Guest Time') && text.includes('Redeem voucher') && text.toLowerCase().includes('redemption tiers') && text.includes('Spin for 1 point') && text.includes('Switch account') && !document.querySelector('#auth-username'); })()`,
  );
  if (!collapsedRecovery)
    throw new Error("zero-time recovery or collapsed switch missing");
  const disclosedSwitch = await evaluate(
    ws,
    `(async () => { const button = [...document.querySelectorAll('main button')].find(b => b.textContent.trim() === 'Switch account'); button?.click(); await new Promise(r => requestAnimationFrame(r)); const text = document.body.innerText; const create = [...document.querySelectorAll('main button')].find(b => b.textContent.trim() === 'Create account'); create?.click(); await new Promise(r => requestAnimationFrame(r)); return text.includes('current Active Session will be replaced') && !!document.querySelector('#auth-username') && [...document.querySelectorAll('main button')].some(b => b.textContent.trim() === 'Sign up'); })()`,
  );
  if (!disclosedSwitch)
    throw new Error("switch disclosure or explicit modes missing");
  const actionError = await evaluate(
    ws,
    `(async () => { const input = document.querySelector('#voucher-code'); if (!(input instanceof HTMLInputElement)) return false; input.value = 'BAD'; input.dispatchEvent(new Event('input', { bubbles: true })); input.form?.requestSubmit(); await new Promise(r => setTimeout(r, 100)); return !!document.querySelector('[role="alert"]'); })()`,
  );
  if (!actionError) throw new Error("account operation error not presented");
  console.log("[GREEN] zero-time recovery, switch disclosure, and errors");

  for (const [width, height] of [
    [360, 480],
    [800, 420],
  ]) {
    await call(ws, "Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await call(ws, "Page.navigate", {
      url: `${base}?account=account&lock=insert-coin`,
    });
    await Bun.sleep(250);
    const overflow = await evaluate(
      ws,
      "document.documentElement.scrollWidth > innerWidth",
    );
    if (overflow)
      throw new Error(
        `${width}x${height}: account controls overflow horizontally`,
      );
  }
  console.log("[GREEN] account recovery viewports");

  await call(ws, "Page.navigate", { url: `${base}?lock=insert-coin` });
  await Bun.sleep(250);
  const closedByDefault = await evaluate(
    ws,
    "!document.querySelector('[role=dialog]') && document.querySelectorAll('button[aria-label=Settings]').length === 1",
  );
  if (!closedByDefault)
    throw new Error("settings auto-opened or gained another opener");
  const ownerGate = await evaluate(
    ws,
    `(async () => { document.querySelector('button[aria-label="Settings"]')?.click(); await new Promise(r => setTimeout(r, 50)); const text = document.querySelector('[role=dialog]')?.textContent ?? ''; return !!document.querySelector('#owner-pin') && !document.querySelector('#server-url') && !text.includes('PC id') && !text.includes('Default view') && !text.includes('Quit client'); })()`,
  );
  if (!ownerGate)
    throw new Error("configured settings exposed controls before Owner PIN");
  const unlocked = await evaluate(
    ws,
    `(async () => { const input = document.querySelector('#owner-pin'); if (!(input instanceof HTMLInputElement)) return false; input.value = '1234'; input.dispatchEvent(new Event('input', { bubbles: true })); input.form?.requestSubmit(); await new Promise(r => setTimeout(r, 100)); const text = document.querySelector('[role=dialog]')?.textContent ?? ''; return !!document.querySelector('#server-url') && text.includes('Disconnect') && text.includes('Default view') && text.includes('PC id') && text.includes('Quit client'); })()`,
  );
  if (!unlocked)
    throw new Error("verified owner did not receive complete settings");
  const resetAfterEscape = await evaluate(
    ws,
    `(async () => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); await new Promise(r => requestAnimationFrame(r)); document.querySelector('button[aria-label="Settings"]')?.click(); await new Promise(r => requestAnimationFrame(r)); return !!document.querySelector('#owner-pin') && !document.querySelector('#server-url'); })()`,
  );
  if (!resetAfterEscape)
    throw new Error("Escape did not close and reset owner unlock");
  const resetAfterClose = await evaluate(
    ws,
    `(async () => { const close = document.querySelector('button[aria-label="Close"]'); close?.click(); await new Promise(r => requestAnimationFrame(r)); document.querySelector('button[aria-label="Settings"]')?.click(); await new Promise(r => requestAnimationFrame(r)); return !!document.querySelector('#owner-pin') && !document.querySelector('#server-url'); })()`,
  );
  if (!resetAfterClose)
    throw new Error("explicit close did not reset owner unlock");
  console.log("[GREEN] configured owner gate, unlock, and reset");

  // The Lock footer is customer-facing: it must never print the admin server
  // address. The address stays reachable to the owner behind the PIN gate
  // (SettingsPanel) and in the local log — never on screen.
  await call(ws, "Page.navigate", { url: `${base}?lock=insert-coin` });
  await Bun.sleep(250);
  const footerHidesAddress = await evaluate(
    ws,
    `(() => { const text = document.body.innerText; return text.includes('Client ') && !text.includes('127.0.0.1') && !text.includes(':3210'); })()`,
  );
  if (!footerHidesAddress)
    throw new Error("configured Lock footer exposed the admin server address");
  console.log("[GREEN] Lock footer hides the admin server address");

  await call(ws, "Page.navigate", { url: `${base}?config=fresh&lock=setup` });
  await Bun.sleep(250);
  const footerSetupHint = await evaluate(
    ws,
    `document.body.innerText.includes('No server configured')`,
  );
  if (!footerSetupHint)
    throw new Error("unconfigured Lock footer lost its setup hint");
  const bootstrap = await evaluate(
    ws,
    `(async () => { document.querySelector('button[aria-label="Settings"]')?.click(); await new Promise(r => setTimeout(r, 50)); const text = document.querySelector('[role=dialog]')?.textContent ?? ''; return !!document.querySelector('#server-url') && !document.querySelector('#owner-pin') && !text.includes('Disconnect') && !text.includes('Quit client') && !text.includes('Default view') && !text.includes('PC id'); })()`,
  );
  if (!bootstrap)
    throw new Error(
      "first configuration exposed controls beyond server connection",
    );
  for (const [width, height] of [
    [360, 480],
    [800, 420],
  ]) {
    await call(ws, "Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });
    const reachable = await evaluate(
      ws,
      `(() => { const dialog = document.querySelector('[role=dialog]'); const input = document.querySelector('#server-url'); if (!dialog || !input) return false; input.scrollIntoView(); const box = input.getBoundingClientRect(); return document.documentElement.scrollWidth <= innerWidth && box.top >= 0 && box.bottom <= innerHeight; })()`,
    );
    if (!reachable)
      throw new Error(`${width}x${height}: first-setup controls unreachable`);
  }
  console.log("[GREEN] narrow first setup and responsive reachability");
} finally {
  chrome.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

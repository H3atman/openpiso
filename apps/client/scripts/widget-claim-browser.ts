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
  .filter((p): p is string => Boolean(p))
  .find(existsSync);
if (!chrome) {
  console.log("SKIP: Chrome/Chromium not found");
  process.exit(0);
}
const vitePort = 16000 + (process.pid % 1000);
const cdpPort = 9950 + (process.pid % 40);
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
async function waitFor(expression: string, expected: unknown): Promise<void> {
  for (let i = 0; i < 80; i++) {
    if ((await evaluate(expression)) === expected) return;
    await Bun.sleep(50);
  }
  throw new Error(`Timeout: ${expression}`);
}
async function load(claimable = true, accountSession = true): Promise<void> {
  await call("Page.navigate", {
    url: `http://127.0.0.1:${vitePort}/widget-claim-harness.html?claimable=${claimable ? 300 : 0}&account=${accountSession ? 1 : 0}`,
  });
  await waitFor(
    "Boolean(document.querySelector('[data-harness-ready]'))",
    true,
  );
}
async function click(text: string): Promise<void> {
  await evaluate(
    `(() => { const button = [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === ${JSON.stringify(text)}); if (!button) throw new Error('Missing button: ' + ${JSON.stringify(text)}); button.click(); })()`,
  );
}

const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
  cwd: dir,
  env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
  stdout: "ignore",
  stderr: "inherit",
});
const profile = browserProfilePath(`openpiso-claim-${process.pid}`);
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
  await waitUrl(`http://127.0.0.1:${vitePort}/widget-claim-harness.html`);
  await assertProcessRunning(vite, "Claim harness Vite");
  await waitUrl(`http://127.0.0.1:${cdpPort}/json/version`);
  const pages = (await (
    await fetch(`http://127.0.0.1:${cdpPort}/json/list`)
  ).json()) as { type: string; webSocketDebuggerUrl: string }[];
  const page = pages.find((p) => p.type === "page");
  if (!page) throw new Error("No page");
  ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise<void>((resolveOpen, reject) => {
    ws.onopen = () => resolveOpen();
    ws.onerror = () => reject(new Error("CDP failed"));
  });
  ws.onmessage = (event) => {
    const msg = JSON.parse(String(event.data)) as {
      id?: number;
      result?: unknown;
      error?: { message: string };
    };
    if (!msg.id) return;
    const waiter = pending.get(msg.id);
    if (!waiter) return;
    pending.delete(msg.id);
    msg.error
      ? waiter.reject(new Error(msg.error.message))
      : waiter.resolve(msg.result);
  };
  await call("Runtime.enable");
  await call("Page.enable");
  await call("Emulation.setDeviceMetricsOverride", {
    width: 320,
    height: 480,
    deviceScaleFactor: 1,
    mobile: false,
  });

  await load(false);
  if (await evaluate("document.body.innerText.includes('Claim')"))
    throw new Error("Claim visible without claimable Guest Time");
  await load(true, false);
  if (await evaluate("document.body.innerText.includes('Claim')"))
    throw new Error("Claim visible without an Account Session");
  await load();
  await click("Claim 5 min");
  await waitFor(
    "document.querySelector('[role=dialog]')?.getAttribute('aria-label')",
    "Confirm Claim",
  );
  if (
    !(await evaluate(
      "document.querySelector('[role=dialog]')?.textContent?.includes('5 min')",
    ))
  )
    throw new Error("Amount missing");
  await waitFor("document.activeElement?.textContent?.trim()", "Cancel");
  const fit = await evaluate(
    `(async () => {
      const dialog = document.querySelector('[role=dialog]');
      if (!dialog) return false;
      const card = dialog.firstElementChild;
      if (!card) return false;
      const targets = [card, ...dialog.querySelectorAll('button')];
      for (const target of targets) {
        target.scrollIntoView({ block: 'nearest' });
        await new Promise(resolve => requestAnimationFrame(resolve));
        const rect = target.getBoundingClientRect();
        if (rect.left < 0 || rect.right > innerWidth || rect.top < 0 || rect.bottom > innerHeight) return false;
      }
      return dialog.scrollWidth <= dialog.clientWidth;
    })()`,
  );
  if (!fit) throw new Error("Confirmation geometry failed");
  const focusTrapped = await evaluate(
    `(() => {
      const buttons = [...document.querySelectorAll('[role=dialog] button')];
      buttons.at(-1)?.focus();
      buttons.at(-1)?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }));
      return document.activeElement === buttons[0];
    })()`,
  );
  if (!focusTrapped)
    throw new Error("Confirmation did not trap keyboard focus");
  await click("Cancel");
  if (await evaluate("Boolean(document.querySelector('[role=dialog]'))"))
    throw new Error("Cancel did not close");
  if ((await evaluate("window.widgetClaimHarness.claimCalls")) !== 0)
    throw new Error("Cancel invoked mutation");

  await click("Claim 5 min");
  await click("Confirm Claim");
  await evaluate("document.querySelector('button')");
  await click("Claiming…").catch(() => {});
  if ((await evaluate("window.widgetClaimHarness.claimCalls")) !== 1)
    throw new Error("Duplicate Claim invocation");
  const locked = await evaluate(
    `(() => { const d = document.querySelector('[role=dialog]'); return [...d.querySelectorAll('button')].every(b => b.disabled); })()`,
  );
  if (!locked) throw new Error("Actions not disabled while busy");
  await evaluate("window.widgetClaimHarness.resolveClaim()");
  await waitFor("Boolean(document.querySelector('[role=dialog]'))", false);
  if (
    (await evaluate(
      "window.widgetClaimHarness.authRefreshes + ':' + window.widgetClaimHarness.timerRefreshes",
    )) !== "1:1"
  )
    throw new Error("State refreshes missing");

  await load();
  await click("Claim 5 min");
  await click("Confirm Claim");
  await evaluate(
    "window.widgetClaimHarness.rejectClaim('Server unavailable. Try again.')",
  );
  await waitFor(
    "document.querySelector('[role=alert]')?.textContent?.trim()",
    "Server unavailable. Try again.",
  );
  if (!(await evaluate("Boolean(document.querySelector('[role=dialog]'))")))
    throw new Error("Failure closed confirmation");
  if (
    !(await evaluate(
      "[...document.querySelectorAll('[role=dialog] button')].every(b => !b.disabled)",
    ))
  )
    throw new Error("Failure did not restore actions");
  console.log("GREEN: Widget Claim confirmation browser tests passed");
} finally {
  ws?.close();
  browser.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

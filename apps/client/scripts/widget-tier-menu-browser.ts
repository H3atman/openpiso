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
const vitePort = 16000 + (process.pid % 1000);
const cdpPort = 9950 + (process.pid % 40);
let id = 0;
let ws: WebSocket;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();
const sleep = (ms: number): Promise<void> => Bun.sleep(ms);
async function waitUrl(url: string): Promise<void> {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {}
    await sleep(100);
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
  let actual: unknown;
  for (let i = 0; i < 100; i++) {
    actual = await evaluate(expression);
    if (actual === expected) return;
    await sleep(50);
  }
  throw new Error(
    `Timeout: ${expression}; expected ${String(expected)}, got ${String(actual)}`,
  );
}
const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
  cwd: dir,
  env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
  stdout: "ignore",
  stderr: "inherit",
});
const profile = browserProfilePath(`openpiso-tier-${process.pid}`);
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
  await waitUrl(
    `http://127.0.0.1:${vitePort}/widget-tier-menu-harness.html`,
  );
  await assertProcessRunning(vite, "Tier harness Vite");
  await waitUrl(`http://127.0.0.1:${cdpPort}/json/version`);
  const pages = (await (
    await fetch(`http://127.0.0.1:${cdpPort}/json/list`)
  ).json()) as { type: string; webSocketDebuggerUrl: string }[];
  const page = pages.find((candidate) => candidate.type === "page");
  if (!page) throw new Error("No page");
  ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise<void>((resolveOpen, reject) => {
    ws.onopen = () => resolveOpen();
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
  await call("Page.navigate", {
    url: `http://127.0.0.1:${vitePort}/widget-tier-menu-harness.html`,
  });
  await waitFor(
    "Boolean(document.querySelector('[data-harness-ready]'))",
    true,
  );
  await evaluate(`([...document.querySelectorAll('button')].find(button => button.textContent?.trim() === 'Points')).click()`);
  await waitFor("Boolean(document.querySelector('[data-claim-time]'))", true);
  if (await evaluate("Boolean(document.querySelector('[data-tier-id]'))")) throw new Error('tier menu rendered inline before Claim Time dialog opened');
  await evaluate("document.querySelector('[data-claim-time]').click()");
  await waitFor("document.querySelectorAll('[data-tier-id]').length", 2);
  const labels = await evaluate<string[]>("[...document.querySelectorAll('[data-tier-id]')].map(element => element.textContent.trim())");
  if (labels.join('|') !== '5 pts → 1 hour|10 pts → 4 hours') throw new Error(`tiers not sorted/rendered: ${labels}`);
  const disabled = await evaluate<boolean[]>("[...document.querySelectorAll('[data-tier-id]')].map(element => element.disabled)");
  if (disabled.join('|') !== 'false|true') throw new Error(`tier affordability wrong: ${disabled}`);
  if (await evaluate("Boolean(document.querySelector('[data-tier-nudge]'))")) throw new Error('nudge shown while a tier is affordable');
  await evaluate("window.widgetTierMenuHarness.setBalance(300)");
  await waitFor("document.body.innerText.includes('2.00 more points to unlock the cheapest tier')", true);
  const allDisabled = await evaluate<boolean>("[...document.querySelectorAll('[data-tier-id]')].every(element => element.disabled)");
  if (!allDisabled) throw new Error('unaffordable tier enabled');
  console.log("GREEN: Widget Redemption Tier menu browser tests passed");
} finally {
  ws?.close();
  browser.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

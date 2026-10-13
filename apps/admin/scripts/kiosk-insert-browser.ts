// Headless-Chrome regression loop for the Central Terminal insert screen.
//
// Symptom under test: a coin is accepted and credited server-side, but the
// insert screen's "Inserted" / "Added" strip stays at ₱0.00 / +0s. The
// harness drives the real `App.svelte` with a stubbed Tauri bridge, opens a
// Selection Window, pushes the same `pc-updated` + `selection-state` events
// the Rust `EventHub` emits on an accepted coin, and asserts the rendered
// totals.
//
//   bun run apps/admin/scripts/kiosk-insert-browser.ts

import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertProcessRunning,
  removeBrowserProfile,
  stopProcessTree,
} from "../../client/scripts/browser-harness-process";

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

const vitePort = 18000 + (process.pid % 1000);
const cdpPort = 9800 + (process.pid % 40);
const harnessUrl = `http://127.0.0.1:${vitePort}/kiosk-insert-harness.html`;
let id = 0;
let ws: WebSocket;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();

async function waitUrl(url: string): Promise<void> {
  for (let i = 0; i < 600; i++) {
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
    throw new Error(`${response.exceptionDetails.text} :: ${expression}`);
  return response.result.value;
}

async function waitFor(
  expression: string,
  expected: unknown,
  label = expression,
): Promise<void> {
  let last: unknown;
  for (let i = 0; i < 80; i++) {
    last = await evaluate(expression);
    if (last === expected) return;
    await Bun.sleep(50);
  }
  const body = await evaluate<string>("document.body.innerText");
  throw new Error(
    `Timeout: ${label} — expected ${expected}, saw ${last}\n--- page ---\n${body}`,
  );
}

/** Text of the value under an uppercase label in the insert-screen strip. */
const strip = (label: string) =>
  `(() => { const box = [...document.querySelectorAll('p')].find(p => p.textContent?.trim() === ${JSON.stringify(label)})?.parentElement; return box?.querySelector('p:last-child')?.textContent?.trim() ?? '<missing>'; })()`;

const vite = Bun.spawn(
  [
    "bun",
    "x",
    "vite",
    "--strictPort",
    "--host",
    "127.0.0.1",
    "--port",
    String(vitePort),
  ],
  { cwd: dir, stdout: "ignore", stderr: "inherit" },
);
const profile = resolve(
  process.env.TEMP ?? dir,
  `openpiso-kiosk-insert-${process.pid}`,
);
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
  await waitUrl(harnessUrl);
  await assertProcessRunning(vite, "Kiosk insert Vite");
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
    width: 1280,
    height: 800,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await call("Page.navigate", { url: harnessUrl });

  // Grid → type "01" → Enter opens the Selection Window (insert screen).
  await waitFor("Boolean(window.__harness)", true, "harness bridge ready");
  await waitFor(
    "document.body.innerText.includes('ALL PCS')",
    true,
    "kiosk grid rendered",
  );
  for (const key of ["0", "1", "Enter"]) {
    await evaluate(
      `window.dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(key)}, bubbles: true })), true`,
    );
  }
  await waitFor(
    "Boolean([...document.querySelectorAll('p')].find(p => p.textContent?.trim() === 'For PC 01'))",
    true,
    "insert screen open",
  );

  // One accepted ₱5 coin → 1500 seconds credited by the server.
  await evaluate("window.__harness.insertCoin(500, 1500), true");
  await waitFor(strip("Inserted"), "₱5.00", "Inserted total after one coin");
  await waitFor(strip("Added"), "+00:25:00", "Added total after one coin");
  await waitFor(
    strip("Equivalent time"),
    "+00:25:00",
    "initial equivalent-time preview",
  );

  // An owner save pushes the new rate while the Selection Window is open.
  // Previously committed money keeps its original quote; only later coins use
  // the new rate.
  await evaluate(
    "window.__harness.emit('shop-rate-updated', { rate_amount_cents: 300, rate_seconds: 10 }), true",
  );
  await waitFor(
    strip("Equivalent time"),
    "+00:25:00",
    "committed equivalent time stays unchanged after Shop Rate push",
  );
  await waitFor(strip("Added"), "+00:25:00", "committed time stays unchanged");

  // Three ₱1 coins each truncate to 3 seconds under ₱3 = 10 sec. They must
  // remain three purchases (9 sec), not be aggregated and requoted as 10 sec.
  for (let coin = 0; coin < 3; coin += 1) {
    await evaluate("window.__harness.insertCoin(100, 3), true");
  }
  await waitFor(strip("Inserted"), "₱8.00", "Inserted total after four coins");
  await waitFor(strip("Added"), "+00:25:09", "Added total after four coins");
  await waitFor(
    strip("Equivalent time"),
    "+00:25:09",
    "equivalent-time preview preserves per-coin truncation",
  );

  console.log("PASS: insert screen preserves committed quotes across Shop Rate changes");
} finally {
  stopProcessTree(vite);
  stopProcessTree(browser);
  removeBrowserProfile(profile);
}

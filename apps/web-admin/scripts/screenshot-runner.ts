// Shared runner for the fixture-fed screenshot harnesses: starts Vite and
// headless Chrome, loads `<harness>?<query>` for each state, performs the
// state's action, and writes one PNG per state to `--out <dir>`. No Admin
// server needed: each harness entry stubs `window.fetch` with fixtures.
// Used by scripts/dashboard-screenshots.ts and scripts/settings-screenshots.ts.
//
// Elements are found by accessible name (aria-label, label, or text) inside
// stable hooks (`data-*`, `[role=dialog]`, `main`), never by class or
// nesting, so state lists survive a redesign that keeps labels and hooks.
import { existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export type Device = "desktop" | "wide" | "phone";
/** An element found by its accessible name inside `scope`. */
export interface Target {
  scope: string;
  name: string;
}
export interface ShotState {
  file: string;
  device: Device;
  /** Grow the viewport to the document height; false for overlay states. */
  fullPage: boolean;
  click?: Target;
  /** Replace the value of the input named `name` with `text`, as typed; for a select, pick option value `text`. */
  fill?: Target & { text: string };
  /** Press Tab until this element holds keyboard focus. */
  tabTo?: Target;
  /** Page condition the action must produce, and what it means when it doesn't. */
  until?: { expr: string; what: string };
}
export interface RunOptions<S extends ShotState> {
  /** Harness page path relative to the app root, e.g. "dashboard-harness.html". */
  harness: string;
  states: S[];
  /** Query string (without `?`) that loads `state` in the harness. */
  query: (state: S) => string;
  /** Page expression that is truthy once the state's data has rendered. */
  pageReady: string;
}

const viewports: Record<Device, { width: number; height: number; scale: number; mobile: boolean }> = {
  desktop: { width: 1280, height: 800, scale: 1, mobile: false },
  wide: { width: 1920, height: 1080, scale: 1, mobile: false },
  phone: { width: 390, height: 844, scale: 2, mobile: true },
};

const dir = resolve(fileURLToPath(import.meta.url), "../..");

function findChrome(): string | undefined {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const cache = join(homedir(), ".cache/ms-playwright");
  const cached = existsSync(cache)
    ? readdirSync(cache)
        .filter((name) => name.startsWith("chromium-"))
        .map((name) => join(cache, name, "chrome-linux64/chrome"))
    : [];
  return [
    ...cached,
    "C:/Program Files/Google/Chrome/Application/chrome.exe",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
  ].find(existsSync);
}

let id = 0;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();

async function wait(target: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt++) {
    try {
      if ((await fetch(target)).ok) return;
    } catch {
      /* starting */
    }
    await Bun.sleep(150);
  }
  throw new Error(`timeout waiting for ${target}`);
}
function call(ws: WebSocket, method: string, params: object = {}): Promise<unknown> {
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
async function waitFor(ws: WebSocket, condition: string, timeoutMs = 15_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await evaluate(ws, condition)) return true;
    await Bun.sleep(100);
  }
  return false;
}
async function setViewport(ws: WebSocket, device: Device, height?: number): Promise<void> {
  const viewport = viewports[device];
  await call(ws, "Emulation.setDeviceMetricsOverride", {
    width: viewport.width,
    height: height ?? viewport.height,
    deviceScaleFactor: viewport.scale,
    mobile: viewport.mobile,
  });
}
/** Page expression evaluating to the target button, or undefined. */
function find(target: Target): string {
  return `[...document.querySelectorAll(${JSON.stringify(target.scope)})]
    .flatMap((scope) => [...scope.querySelectorAll("button, [role=button], a[href]")])
    .find((el) => {
      // Per-PC actions may carry the PC name after the label ("Check PC 01").
      const name = (el.getAttribute("aria-label") ?? el.textContent ?? "").trim();
      return name === ${JSON.stringify(target.name)} || name.startsWith(${JSON.stringify(`${target.name} `)});
    })`;
}
/** Page expression evaluating to the input named by aria-label or <label>, or undefined. */
function findInput(target: Target): string {
  return `[...document.querySelectorAll(${JSON.stringify(target.scope)})]
    .flatMap((scope) => [...scope.querySelectorAll("input, textarea, select")])
    .find((el) => (el.getAttribute("aria-label") ?? el.labels?.[0]?.textContent ?? "").trim() === ${JSON.stringify(target.name)})`;
}
function describe(target: Target, kind = "button"): string {
  return `${kind} "${target.name}" inside ${target.scope}`;
}

async function capture<S extends ShotState>(
  ws: WebSocket,
  options: RunOptions<S>,
  harnessUrl: string,
  outDir: string,
  state: S,
): Promise<string> {
  await setViewport(ws, state.device);
  // Drop the previous page's ready marker so the wait below sees only the new load.
  await evaluate(ws, "delete document.documentElement.dataset.harnessServed");
  await call(ws, "Page.navigate", { url: `${harnessUrl}?${options.query(state)}` });
  // Generous: the first load waits on a cold Vite transform and dependency optimise.
  if (!(await waitFor(ws, options.pageReady, 60_000)))
    throw new Error(`page never became ready: ${options.pageReady.replace(/\s+/g, " ")}`);
  await evaluate(ws, "document.fonts.ready.then(() => true)");

  if (state.click) {
    if (!(await evaluate(ws, `!!(${find(state.click)})`)))
      throw new Error(`could not find ${describe(state.click)}`);
    await evaluate(ws, `(${find(state.click)}).click()`);
  }
  if (state.fill) {
    if (!(await evaluate(ws, `!!(${findInput(state.fill)})`)))
      throw new Error(`could not find ${describe(state.fill, "input")}`);
    // A <select> takes `text` as the option value and fires `change`, as a pick would.
    const picked = await evaluate(ws, `(() => {
      const el = ${findInput(state.fill)};
      el.focus();
      if (!(el instanceof HTMLSelectElement)) return el.select(), false;
      el.value = ${JSON.stringify(state.fill.text)};
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
    if (!picked) await call(ws, "Input.insertText", { text: state.fill.text });
  }
  if (state.tabTo) {
    if (!(await evaluate(ws, `!!(${find(state.tabTo)})`)))
      throw new Error(`could not find ${describe(state.tabTo)}`);
    let focused = false;
    for (let press = 0; press < 80 && !focused; press++) {
      for (const type of ["keyDown", "keyUp"])
        await call(ws, "Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
      focused = Boolean(await evaluate(ws, `document.activeElement === (${find(state.tabTo)})`));
    }
    if (!focused) throw new Error(`Tab never reached ${describe(state.tabTo)}`);
  }
  if (state.until && !(await waitFor(ws, state.until.expr)))
    throw new Error(state.until.what);

  if (state.fullPage) {
    const height = Number(await evaluate(ws, "document.documentElement.scrollHeight"));
    await setViewport(ws, state.device, height);
  }
  // Two frames plus a beat for layout after any resize; motion is reduced.
  await evaluate(ws, "new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(true))))");
  await Bun.sleep(300);
  const shot = (await call(ws, "Page.captureScreenshot", { format: "png" })) as { data: string };
  const png = Buffer.from(shot.data, "base64");
  const file = join(outDir, state.file);
  await Bun.write(file, png);
  return `${file} ${png.readUInt32BE(16)}x${png.readUInt32BE(20)}`;
}

/** Captures every state, then exits non-zero if any state failed. */
export async function runScreenshots<S extends ShotState>(options: RunOptions<S>): Promise<void> {
  const outIndex = process.argv.indexOf("--out");
  const outArg = outIndex === -1 ? undefined : process.argv[outIndex + 1];
  if (!outArg) {
    console.error("FAIL: missing --out <dir>");
    process.exit(1);
  }
  const outDir = resolve(outArg);
  mkdirSync(outDir, { recursive: true });

  const chromePath = findChrome();
  if (!chromePath) {
    console.error("FAIL: Chrome/Chromium not found (set CHROME_PATH)");
    process.exit(1);
  }

  const vitePort = 15_000 + (process.pid % 1_000);
  const cdpPort = 9_800 + (process.pid % 190);
  const harnessUrl = `http://127.0.0.1:${vitePort}/admin/${options.harness}`;
  const vite = Bun.spawn(
    ["bun", "x", "vite", "--host", "127.0.0.1", "--port", String(vitePort), "--strictPort"],
    { cwd: dir, stdout: "ignore", stderr: "inherit" },
  );
  const profile = resolve(tmpdir(), `openpiso-screenshots-${process.pid}`);
  const chrome = Bun.spawn(
    [
      chromePath,
      "--headless=new",
      `--remote-debugging-port=${cdpPort}`,
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--disable-gpu",
      "--hide-scrollbars",
      "about:blank",
    ],
    { stdout: "ignore", stderr: "ignore" },
  );

  const failures: string[] = [];
  try {
    await wait(harnessUrl);
    if (vite.exitCode !== null) throw new Error("harness Vite exited");
    await wait(`http://127.0.0.1:${cdpPort}/json/version`);
    const pages = (await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json()) as {
      type: string;
      webSocketDebuggerUrl: string;
    }[];
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
        method?: string;
        params?: { type?: string; args?: { value?: unknown }[] };
        result?: unknown;
        error?: { message: string };
      };
      // Surface the page's warnings and errors (e.g. unstubbed requests).
      if (message.method === "Runtime.consoleAPICalled") {
        const type = message.params?.type;
        if (type === "warning" || type === "error") {
          console.error(`[page ${type}]`, ...(message.params?.args ?? []).map((arg) => arg.value));
        }
        return;
      }
      if (!message.id) return;
      const waiter = pending.get(message.id);
      if (!waiter) return;
      pending.delete(message.id);
      message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);
    };
    await call(ws, "Runtime.enable");
    await call(ws, "Page.enable");
    // Determinism: fixture timestamps render in a fixed zone, and toasts and
    // transitions settle immediately instead of mid-animation.
    await call(ws, "Emulation.setTimezoneOverride", { timezoneId: "Asia/Manila" });
    await call(ws, "Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-reduced-motion", value: "reduce" }],
    });

    for (const state of options.states) {
      try {
        console.log(`wrote ${await capture(ws, options, harnessUrl, outDir, state)}`);
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error);
        failures.push(`${state.file}: ${reason}`);
        console.error(`FAIL ${state.file}: ${reason}`);
      }
    }
    ws.close();
  } catch (error) {
    failures.push(error instanceof Error ? error.message : String(error));
    console.error(`FAIL: ${failures.at(-1)}`);
  } finally {
    chrome.kill();
    vite.kill();
    try {
      rmSync(profile, { recursive: true, force: true });
    } catch {
      // Chrome can briefly hold profile files while exiting.
    }
  }
  if (failures.length > 0) {
    console.error(`${failures.length} state(s) failed`);
    process.exit(1);
  }
}

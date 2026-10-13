// Live Owner Session E2E (ADR-0074): real Web Admin + running Admin over
// headless Chromium CDP, no mocks.
//
//   bun run apps/web-admin/scripts/owner-session-browser.ts
//
// Env: OPENPISO_OWNER_URL (default http://127.0.0.1:8080/admin), OPENPISO_OWNER_PIN
// (else random PIN, fresh DB setup only), OPENPISO_E2E_OUT, CHROME_PATH,
// OPENPISO_E2E_ACTIVITY_TIMEOUT_MS (default 75000, > one throttle window).
// Run only against a throwaway Admin data dir (XDG_DATA_HOME): it sets up or
// logs into the Owner and renames the shop. Idle/absolute expiry is covered by
// the Rust route tests.

import { verifyAppUpdate, type UpdateFixture } from "./owner-session-app-update";
import type { AppUpdateState } from "../src/lib/types";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  assertProcessRunning,
  removeBrowserProfile,
  stopProcessTree,
} from "../../client/scripts/browser-harness-process";

const ownerUrl = new URL(process.env.OPENPISO_OWNER_URL ?? "http://127.0.0.1:8080/admin");
const origin = ownerUrl.origin;
const appPath = ownerUrl.pathname.replace(/\/$/, "");
const activityTimeoutMs = Number(process.env.OPENPISO_E2E_ACTIVITY_TIMEOUT_MS ?? 75000);
const outDir = resolve(
  process.env.OPENPISO_E2E_OUT ?? join(tmpdir(), `openpiso-owner-session-${Date.now()}`),
);
mkdirSync(outDir, { recursive: true });

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
const chrome = findChrome();
if (!chrome) {
  console.error("FAIL: Chrome/Chromium not found (set CHROME_PATH)");
  process.exit(1);
}

// Secrets live only in memory; the artifact is scanned for them before write.
const secrets = new Set<string>();
const pin =
  process.env.OPENPISO_OWNER_PIN ??
  String(crypto.getRandomValues(new Uint32Array(1))[0] % 100_000_000).padStart(8, "0");
secrets.add(pin);

const checks: { name: string; ok: boolean; detail?: string }[] = [];
function check(name: string, ok: boolean, detail?: string): void {
  checks.push({ name, ok, ...(detail ? { detail } : {}) });
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail && !ok ? ` — ${detail}` : ""}`);
  if (!ok) throw new Error(`Check failed: ${name}${detail ? ` (${detail})` : ""}`);
}

// ---------------------------------------------------------------- CDP plumbing

const cdpPort = 9840 + (process.pid % 40);
let id = 0;
let ws: WebSocket | undefined;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();

function call(method: string, params: object = {}): Promise<unknown> {
  const socket = ws;
  if (!socket) return Promise.reject(new Error("CDP not connected"));
  const current = ++id;
  return new Promise((resolveValue, reject) => {
    pending.set(current, { resolve: resolveValue, reject });
    socket.send(JSON.stringify({ id: current, method, params }));
  });
}

async function evaluate<T>(expression: string): Promise<T> {
  const response = (await call("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })) as { result: { value: T }; exceptionDetails?: { text: string } };
  if (response.exceptionDetails) throw new Error(`${response.exceptionDetails.text} :: ${expression}`);
  return response.result.value;
}

async function waitFor(expression: string, label: string, tries = 100): Promise<void> {
  for (let i = 0; i < tries; i++) {
    if (await evaluate<boolean>(expression)) return;
    await Bun.sleep(100);
  }
  const body = await evaluate<string>("document.body.innerText.slice(0, 400)");
  throw new Error(`Timeout: ${label}\n--- page (truncated) ---\n${body}`);
}

// Network log. Header NAMES and presence only; cookie/CSRF values are never stored.
interface NetRecord {
  seq: number;
  method: string;
  path: string;
  origin?: string;
  hasCookie?: boolean;
  hasCsrf?: boolean;
  status?: number;
  setCookie?: string[]; // raw Set-Cookie lines, kept in memory only
  finished: boolean;
}
const net = new Map<string, NetRecord>();
let seq = 0;

/** The subset of CDP Network event payloads this harness reads. */
interface CdpNetworkParams {
  requestId: string;
  request?: { url: string; method: string };
  response?: { status: number };
  headers?: Record<string, string>;
  statusCode?: number;
}

const earlyRequestHeaders = new Map<string, Record<string, string>>();

/** Record header presence only; the CSRF value is kept solely as a redaction secret. */
function applyRequestHeaders(rec: NetRecord, headers: Record<string, string>): void {
  const names = Object.fromEntries(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), v]));
  rec.origin = names["origin"];
  rec.hasCookie = "cookie" in names;
  rec.hasCsrf = "x-csrf-token" in names;
  if (rec.hasCsrf) secrets.add(names["x-csrf-token"]);
}

function onEvent(method: string, p: CdpNetworkParams): void {
  if (method === "Network.requestWillBeSent" && p.request) {
    const url = new URL(p.request.url);
    if (url.origin !== origin) return;
    const rec: NetRecord = { seq: ++seq, method: p.request.method, path: url.pathname, finished: false };
    net.set(p.requestId, rec);
    const early = earlyRequestHeaders.get(p.requestId);
    if (early) applyRequestHeaders(rec, early);
  } else if (method === "Network.requestWillBeSentExtraInfo") {
    // Chromium may deliver this before requestWillBeSent; buffer until the record exists.
    const rec = net.get(p.requestId);
    if (rec) applyRequestHeaders(rec, p.headers ?? {});
    else earlyRequestHeaders.set(p.requestId, p.headers ?? {});
  } else if (method === "Network.responseReceived") {
    const rec = net.get(p.requestId);
    if (rec && p.response) rec.status = p.response.status;
  } else if (method === "Network.responseReceivedExtraInfo") {
    const rec = net.get(p.requestId);
    if (!rec) return;
    rec.status = p.statusCode;
    const raw = Object.entries(p.headers ?? {}).find(
      ([k]) => k.toLowerCase() === "set-cookie",
    )?.[1];
    if (raw) rec.setCookie = raw.split("\n");
  } else if (method === "Network.loadingFinished" || method === "Network.loadingFailed") {
    const rec = net.get(p.requestId);
    if (rec) rec.finished = true;
  }
}

const requestIdOf = (rec: NetRecord) => [...net.entries()].find(([, r]) => r === rec)?.[0];

/** Wait for a finished request after `since`; returns the record. */
async function waitRequest(
  method: string,
  path: string,
  since: number,
  timeoutMs = 10000,
): Promise<NetRecord> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const rec = [...net.values()].find(
      (r) => r.seq > since && r.method === method && r.path === path && r.finished && r.status,
    );
    if (rec) return rec;
    await Bun.sleep(50);
  }
  throw new Error(`Timeout waiting for ${method} ${path}`);
}

async function body(rec: NetRecord): Promise<string> {
  const response = (await call("Network.getResponseBody", { requestId: requestIdOf(rec) })) as {
    body: string;
  };
  return response.body;
}

const count = (method: string, path: string, since: number) =>
  [...net.values()].filter((r) => r.seq > since && r.method === method && r.path === path).length;

// Trusted (isTrusted === true) browser input via the CDP Input domain.
async function center(selector: string): Promise<{ x: number; y: number }> {
  const point = await evaluate<{ x: number; y: number } | null>(
    `(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return null; el.scrollIntoView({block:'center'}); const r = el.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`,
  );
  if (!point) throw new Error(`Missing element: ${selector}`);
  return point;
}
async function click(selector: string): Promise<void> {
  const { x, y } = await center(selector);
  await call("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
  await call("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await call("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
}
async function typeInto(selector: string, text: string): Promise<void> {
  await click(selector);
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).select(), true`);
  await call("Input.insertText", { text });
}
async function screenshot(name: string): Promise<void> {
  const shot = (await call("Page.captureScreenshot", { format: "png" })) as { data: string };
  writeFileSync(join(outDir, name), Buffer.from(shot.data, "base64"));
}

const hasSidebar = `Boolean(document.querySelector('[data-sidebar="footer"]'))`;
const hasPinField = `Boolean(document.querySelector('#pin'))`;

/** Parse a Set-Cookie line into attributes; the value is returned separately. */
function parseSetCookie(line: string) {
  const [pair, ...attrs] = line.split(";").map((part) => part.trim());
  const eq = pair.indexOf("=");
  const lower = attrs.map((a) => a.toLowerCase());
  return {
    name: pair.slice(0, eq),
    value: pair.slice(eq + 1),
    httpOnly: lower.includes("httponly"),
    secure: lower.includes("secure"),
    sameSite: attrs.find((a) => a.toLowerCase().startsWith("samesite="))?.slice(9),
    path: attrs.find((a) => a.toLowerCase().startsWith("path="))?.slice(5),
    maxAge: attrs.find((a) => a.toLowerCase().startsWith("max-age="))?.slice(8),
  };
}

/** Assert hardened cookie + body shape of a setup/login/session-bootstrap response. */
async function assertAuthResponse(rec: NetRecord, label: string, expectCookie: boolean) {
  check(`${label}: 200`, rec.status === 200, String(rec.status));
  const text = await body(rec);
  const json = JSON.parse(text) as Record<string, unknown>;
  const csrf = json.csrf_token;
  check(`${label}: csrf_token present`, typeof csrf === "string" && csrf.length > 0);
  secrets.add(csrf as string);
  check(`${label}: no token field`, !("token" in json));
  let tokenValue: string | undefined;
  if (expectCookie) {
    const line = rec.setCookie?.find((l) => l.startsWith("op_owner="));
    check(`${label}: Set-Cookie op_owner`, Boolean(line));
    const cookie = parseSetCookie(line as string);
    tokenValue = cookie.value;
    secrets.add(tokenValue);
    check(`${label}: cookie attrs`, cookie.httpOnly && !cookie.secure && cookie.sameSite === "Strict" && cookie.path === "/" && cookie.maxAge === "43200", `httpOnly=${cookie.httpOnly} secure=${cookie.secure} sameSite=${cookie.sameSite} path=${cookie.path} maxAge=${cookie.maxAge}`);
  }
  if (tokenValue) check(`${label}: token not in body`, !text.includes(tokenValue));
  return { csrf: csrf as string, tokenValue };
}

// ------------------------------------------------------------------- run

const profile = mkdtempSync(join(tmpdir(), "openpiso-owner-session-profile-"));
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

let failure: unknown;
try {
  await assertProcessRunning(browser, "Chromium");
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${cdpPort}/json/version`)).ok) break;
    } catch {}
    if (i > 100) throw new Error("CDP not reachable");
    await Bun.sleep(100);
  }

  // Preflight from Bun: the real Admin must be up; anonymous state is clean.
  let anon: { configured: boolean; authenticated: boolean; csrf_token?: string };
  try {
    anon = (await (await fetch(`${origin}/web-admin/session`)).json()) as typeof anon;
  } catch (e) {
    throw new Error(`Admin not reachable at ${origin} (start the real Admin first): ${e}`);
  }
  check("preflight: anonymous session has no csrf", anon.authenticated === false && !("csrf_token" in anon));
  const configured = anon.configured;
  if (configured && !process.env.OPENPISO_OWNER_PIN) {
    throw new Error("Admin already has an Owner PIN; set OPENPISO_OWNER_PIN or use a fresh temp DB");
  }

  const pages = (await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json()) as {
    type: string;
    webSocketDebuggerUrl: string;
  }[];
  const page = pages.find((entry) => entry.type === "page");
  if (!page) throw new Error("No page");
  const socket = new WebSocket(page.webSocketDebuggerUrl);
  ws = socket;
  await new Promise<void>((open, reject) => {
    socket.onopen = () => open();
    socket.onerror = () => reject(new Error("CDP failed"));
  });
  ws.onmessage = (event) => {
    const message = JSON.parse(String(event.data)) as {
      id?: number;
      method?: string;
      params?: CdpNetworkParams;
      result?: unknown;
      error?: { message: string };
    };
    if (message.method) return onEvent(message.method, message.params ?? { requestId: "" });
    const waiter = message.id ? pending.get(message.id) : undefined;
    if (!waiter) return;
    pending.delete(message.id as number);
    message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);
  };
  await call("Runtime.enable");
  await call("Page.enable");
  await call("Network.enable");
  await call("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });

  // 1. Setup (fresh DB) or login (provided PIN) through the real form.
  await call("Page.navigate", { url: `${origin}${appPath}` });
  await waitFor(hasPinField, "login form");
  let mark = seq;
  await typeInto("#pin", pin);
  if (!configured) {
    await waitFor(`Boolean(document.querySelector('#confirm-pin'))`, "setup form");
    await typeInto("#confirm-pin", pin);
  }
  await click('form button[type="submit"]');
  const authPath = configured ? "/web-admin/login" : "/web-admin/setup";
  const authRec = await waitRequest("POST", authPath, mark);
  const { csrf: csrf0, tokenValue } = await assertAuthResponse(authRec, configured ? "login" : "setup", true);
  const token = tokenValue as string;
  await waitFor(hasSidebar, "dashboard after auth");
  await screenshot("1-dashboard-after-login.png");
  if (process.env.OPENPISO_E2E_APP_UPDATE) {
    const fixture = JSON.parse(process.env.OPENPISO_E2E_APP_UPDATE) as UpdateFixture;
    await verifyAppUpdate({
      call, evaluate, waitFor, click, typeInto, screenshot, check,
      state: async () => {
        const response = await fetch(`${origin}/web-admin/update`, { headers: { Cookie: `op_owner=${token}` } });
        if (!response.ok) throw new Error(`App Update state: HTTP ${response.status}`);
        return await response.json() as AppUpdateState;
      },
    }, fixture);
    check("App Update: no install/rollback request reached Admin", count("POST", "/web-admin/update/install", 0) === 0 && count("POST", "/web-admin/update/rollback", 0) === 0);
  }

  // Cookie visible to CDP only; JS cannot see it, and CSRF is not in storage.
  const cookies = ((await call("Network.getCookies", { urls: [origin] })) as {
    cookies: { name: string; httpOnly: boolean; secure: boolean; sameSite?: string; path: string }[];
  }).cookies;
  const owner = cookies.find((c) => c.name === "op_owner");
  check("browser cookie jar: HttpOnly, Strict, not Secure, Path=/", Boolean(owner?.httpOnly && !owner.secure && owner.sameSite === "Strict" && owner.path === "/"));
  const jsView = await evaluate<string>(
    `JSON.stringify({ cookie: document.cookie, local: JSON.stringify(localStorage), session: JSON.stringify(sessionStorage) })`,
  );
  check("token/csrf not readable from JS or storage", ![token, csrf0].some((s) => jsView.includes(s)) && !/op_owner/.test(jsView));

  // 2. Normal authenticated mutation through the UI (Shop settings).
  const shopUrl = `${appPath}/settings/shop`;
  const gotoShop = async () => {
    await evaluate(`history.pushState(null, '', ${JSON.stringify(shopUrl)}); window.dispatchEvent(new PopStateEvent('popstate')); true`);
    await waitFor(`Boolean(document.querySelector('#shop-name')) && document.querySelector('#shop-name').value.length > 0`, "shop settings loaded");
  };
  const saveShopName = async (name: string) => {
    mark = seq;
    await typeInto("#shop-name", name);
    const saveButton = `Array.from(document.querySelectorAll('button')).find(b => b.textContent.trim() === 'Save')`;
    await evaluate(`(${saveButton}).setAttribute('data-e2e-save', ''), true`);
    await click("[data-e2e-save]");
    await evaluate(`document.querySelectorAll('[data-e2e-save]').forEach(b => b.removeAttribute('data-e2e-save')), true`);
    const rec = await waitRequest("PUT", "/web-admin/settings", mark);
    check(`PUT settings "${name}": 200`, rec.status === 200, String(rec.status));
    check(`PUT settings "${name}": automatic Origin + CSRF header + cookie`, rec.origin === origin && rec.hasCsrf === true && rec.hasCookie === true, `origin=${rec.origin} csrf=${rec.hasCsrf} cookie=${rec.hasCookie}`);
  };
  const suffix = crypto.randomUUID().slice(0, 8);
  await gotoShop();
  await saveShopName(`E2E Shop ${suffix} a`);
  await screenshot("2-settings-after-first-save.png");

  // 3. Reload: session persists, bootstrap restores CSRF (no token), mutation works.
  mark = seq;
  await call("Page.reload");
  await waitFor(hasSidebar, "dashboard after reload");
  check("reload: no login form", !(await evaluate<boolean>(hasPinField)));
  const bootstrap = await waitRequest("GET", "/web-admin/session", mark);
  const boot = await assertAuthResponse(bootstrap, "bootstrap", false);
  check("bootstrap: no Set-Cookie rotation leak of token", !(bootstrap.setCookie?.length) || !(await body(bootstrap)).includes(token));
  await gotoShop();
  await saveShopName(`E2E Shop ${suffix} b`);
  await screenshot("3-settings-after-reload-save.png");

  // 4. Server-side denials with the live session (from Bun; real HTTP). State must not change.
  const cookieHeader = { Cookie: `op_owner=${token}` };
  const put = (headers: Record<string, string>) =>
    fetch(`${origin}/web-admin/settings`, {
      method: "PUT",
      headers: { "Content-Type": "application/json", ...cookieHeader, ...headers },
      body: JSON.stringify({ shop_name: `E2E denied ${suffix}`, shutdown_grace_seconds: 30, offline_grace_seconds: 30 }),
    });
  const currentName = async () =>
    ((await (await fetch(`${origin}/web-admin/settings`, { headers: cookieHeader })).json()) as { shop_name: string }).shop_name;
  check("safe read needs no CSRF/Origin", (await fetch(`${origin}/web-admin/settings`, { headers: cookieHeader })).status === 200);
  for (const [label, headers] of [
    ["no origin, no csrf", {}],
    ["origin without csrf", { Origin: origin }],
    ["csrf without origin", { "X-CSRF-Token": boot.csrf }],
    ["wrong csrf", { Origin: origin, "X-CSRF-Token": "wrong" }],
    ["foreign origin + valid csrf", { Origin: "http://evil.example", "X-CSRF-Token": boot.csrf }],
    ["opaque origin + valid csrf", { Origin: "null", "X-CSRF-Token": boot.csrf }],
  ] as const) {
    const response = await put({ ...headers });
    check(`denied: ${label} -> 403`, response.status === 403, String(response.status));
  }
  check("denied mutations changed nothing", (await currentName()) === `E2E Shop ${suffix} b`);

  // 5. Activity: idle polling and synthetic events report nothing; trusted input reports, throttled.
  await Bun.sleep(1500); // let post-save UI settle
  mark = seq;
  await Bun.sleep(4000);
  check("idle (polling only) reports no activity", count("POST", "/web-admin/activity", mark) === 0);
  mark = seq;
  await evaluate(
    `for (const ev of [new MouseEvent('mousemove', {bubbles:true}), new PointerEvent('pointerdown', {bubbles:true}), new KeyboardEvent('keydown', {key:'a', bubbles:true}), new TouchEvent('touchstart', {bubbles:true})]) { document.dispatchEvent(ev); window.dispatchEvent(ev); document.body.dispatchEvent(ev); } true`,
  );
  await Bun.sleep(2500);
  check("synthetic (untrusted) events report no activity", count("POST", "/web-admin/activity", mark) === 0);
  mark = seq;
  // The page throttles reports (leading edge, no trailing report), and earlier
  // trusted input may have just used the window. Keep emitting bounded trusted
  // input every 2s until the next report lands or the timeout (> one window).
  const activityDeadline = Date.now() + activityTimeoutMs;
  let activity: NetRecord | undefined;
  for (let n = 0; !activity && Date.now() < activityDeadline; n++) {
    await call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 400 + (n % 2), y: 300 });
    await call("Input.dispatchKeyEvent", { type: "keyDown", key: "Shift", code: "ShiftLeft" });
    await call("Input.dispatchKeyEvent", { type: "keyUp", key: "Shift", code: "ShiftLeft" });
    activity = await waitRequest("POST", "/web-admin/activity", mark, 2000).catch(() => undefined);
  }
  if (!activity) throw new Error(`No activity report within ${activityTimeoutMs}ms of trusted input`);
  check("trusted input reports activity (2xx, Origin + CSRF)", (activity.status ?? 0) >= 200 && (activity.status ?? 0) < 300 && activity.origin === origin && activity.hasCsrf === true, `status=${activity.status} origin=${activity.origin} csrf=${activity.hasCsrf}`);
  mark = seq;
  for (let i = 0; i < 40; i++) {
    await call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 200 + i * 5, y: 250 });
  }
  await Bun.sleep(2000);
  check("burst of trusted input is throttled (<=1 report)", count("POST", "/web-admin/activity", mark) <= 1, String(count("POST", "/web-admin/activity", mark)));

  // 6. Logout via the UI, then anonymous state and server-side revocation.
  mark = seq;
  await click('[data-sidebar="footer"] button[aria-haspopup="menu"]');
  await waitFor(`Array.from(document.querySelectorAll('[role="menuitem"]')).some(i => i.textContent.trim() === 'Sign out')`, "user menu");
  await evaluate(`Array.from(document.querySelectorAll('[role="menuitem"]')).find(i => i.textContent.trim() === 'Sign out').setAttribute('data-e2e-signout', ''), true`);
  await click("[data-e2e-signout]");
  const logout = await waitRequest("POST", "/web-admin/logout", mark);
  check("logout: 200 with automatic Origin + CSRF", logout.status === 200 && logout.origin === origin && logout.hasCsrf === true, `status=${logout.status} origin=${logout.origin} csrf=${logout.hasCsrf}`);
  await waitFor(hasPinField, "login form after logout");
  const after = await evaluate<{ authenticated: boolean; csrf: boolean }>(
    `fetch('/web-admin/session', { credentials: 'include' }).then(r => r.json()).then(j => ({ authenticated: j.authenticated, csrf: 'csrf_token' in j }))`,
  );
  check("after logout: anonymous, no csrf", after.authenticated === false && after.csrf === false);
  const afterCookies = ((await call("Network.getCookies", { urls: [origin] })) as { cookies: { name: string }[] }).cookies;
  check("after logout: browser cookie cleared", !afterCookies.some((c) => c.name === "op_owner"));
  const replay = await fetch(`${origin}/web-admin/session`, { headers: cookieHeader });
  const replayJson = (await replay.json()) as { authenticated: boolean };
  check("old cookie token revoked server-side", replayJson.authenticated === false);
  const replayRead = await fetch(`${origin}/web-admin/settings`, { headers: cookieHeader });
  check("old cookie token cannot read protected data (401)", replayRead.status === 401, String(replayRead.status));

  // 7. Sanitized artifact. Refuse to write anything containing a secret.
  const artifact = JSON.stringify(
    {
      origin,
      path: appPath,
      path_taken: configured ? "login" : "setup",
      checks,
      requests: [...net.values()]
        .filter((r) => r.path.startsWith("/web-admin/"))
        .map(({ seq: s, method, path, origin: o, hasCookie, hasCsrf, status }) => ({ seq: s, method, path, origin: o, hasCookie, hasCsrf, status })),
    },
    null,
    2,
  );
  for (const secret of secrets) {
    if (secret.length >= 4 && artifact.includes(secret)) throw new Error("Artifact would leak a secret; not written");
  }
  writeFileSync(join(outDir, "owner-session-browser.json"), artifact);
  console.log(`PASS: Owner Session live browser verification. Artifact: ${outDir}`);
} catch (error) {
  failure = error;
  if (ws?.readyState === WebSocket.OPEN) await screenshot("failure.png").catch(() => {});
} finally {
  stopProcessTree(browser);
  removeBrowserProfile(profile);
}
if (failure) {
  // Failure output must not echo secrets either.
  let message = failure instanceof Error ? `${failure.message}` : String(failure);
  for (const secret of secrets) if (secret.length >= 4) message = message.split(secret).join("<redacted>");
  console.error(message);
  process.exit(1);
}

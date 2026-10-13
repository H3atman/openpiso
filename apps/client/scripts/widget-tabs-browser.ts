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
// A harness that fails to mount reports only "timeout waiting for
// data-harness-ready", which says nothing about why. Capture the page's own
// exceptions so the timeout can name the cause.
let lastPageException = "";
let waitCount = 0;
async function waitFor(
  expression: string,
  expected: unknown,
  label = expression,
  timeoutMs = 4_000,
): Promise<void> {
  const currentWait = ++waitCount;
  for (let i = 0; i < timeoutMs / 50; i++) {
    if ((await evaluate(expression)) === expected) return;
    await Bun.sleep(50);
  }
  throw new Error(
    `Timeout at wait ${currentWait}: ${label}${
      lastPageException ? `\n  Page exception: ${lastPageException}` : ""
    }`,
  );
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
/** Click a button by label, disambiguated by whether it sits in a dialog. */
async function clickScoped(
  text: string,
  scope: "in-dialog" | "outside-dialog",
): Promise<void> {
  const predicate =
    scope === "in-dialog"
      ? "Boolean(button.closest('[role=\"dialog\"]'))"
      : "!button.closest('[role=\"dialog\"]')";
  try {
    await evaluate(
      `(() => { const button = [...document.querySelectorAll('button')].find(button => button.textContent?.trim() === ${JSON.stringify(text)} && ${predicate}); if (!button) throw new Error('missing'); button.click(); })()`,
    );
  } catch {
    throw new Error(`Missing button (${scope}): ${text}`);
  }
}
async function setInput(selector: string, value: string): Promise<void> {
  await evaluate(
    `(() => { const input = document.querySelector(${JSON.stringify(selector)}); if (!(input instanceof HTMLInputElement)) throw new Error('Missing input: ${selector}'); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`,
  );
}
async function submitForm(selector: string): Promise<void> {
  await evaluate(
    `(() => { const input = document.querySelector(${JSON.stringify(selector)}); if (!(input instanceof HTMLInputElement)) throw new Error('Missing input: ${selector}'); input.form?.requestSubmit(); })()`,
  );
}
async function assertDialogReachable(): Promise<void> {
  const reachable = await evaluate(
    `(async () => { const dialog = document.querySelector('[role="dialog"]'); if (!dialog) return false; const controls = [...dialog.querySelectorAll('button,input,select,textarea')]; for (const control of controls) { control.scrollIntoView({ block: 'nearest' }); await new Promise(resolve => requestAnimationFrame(resolve)); const rect = control.getBoundingClientRect(); if (rect.left < 0 || rect.right > innerWidth || rect.top < 0 || rect.bottom > innerHeight) return false; } return dialog.scrollWidth <= dialog.clientWidth && controls.every(control => { const rect = control.getBoundingClientRect(); return rect.left >= 0 && rect.right <= innerWidth; }); })()`,
  );
  if (!reachable)
    throw new Error("Dialog overflows or has unreachable controls");
}
async function submitOwnerPin(pin: string): Promise<void> {
  await setInput("#owner-pin", pin);
  await submitForm("#owner-pin");
}
async function assertStableHeader(): Promise<void> {
  const stable = await evaluate(
    `(() => { const header = document.querySelector('[aria-label="OpenPiso workstation widget"] > header'); return Boolean(header?.textContent?.includes('Harness Shop') && header.textContent.includes('pc-07') && header.hasAttribute('data-tauri-drag-region')); })()`,
  );
  if (!stable) throw new Error("Stable draggable Widget header missing");
}
async function assertPanelReachable(): Promise<void> {
  const reachable = await evaluate(
    `(async () => { const root = document.querySelector('[aria-label="OpenPiso workstation widget"]'); const scroller = document.querySelector('[data-widget-tab-panel]'); const lastControl = [...scroller.querySelectorAll('button, input')].at(-1); lastControl?.scrollIntoView({ block: 'nearest' }); await new Promise(resolve => requestAnimationFrame(resolve)); const rect = (lastControl ?? scroller)?.getBoundingClientRect(); const controls = [...root.querySelectorAll('button, input')]; const horizontallyReachable = controls.every(control => { const bounds = control.getBoundingClientRect(); return bounds.left >= 0 && bounds.right <= innerWidth; }); return root.scrollWidth <= root.clientWidth && scroller.scrollWidth <= scroller.clientWidth && horizontallyReachable && rect && rect.left >= 0 && rect.right <= innerWidth && rect.top >= 0 && rect.bottom <= innerHeight; })()`,
  );
  if (!reachable)
    throw new Error("Widget panel overflows or has unreachable controls");
}
// Mirrors the Time tab's layout constants in Widget.svelte: the timer frame's
// `min-h-[86px]` / `min-h-[44px]` floors and the ladder body's `max-h-[104px]`
// cap. Changing a class without changing these here silently weakens the
// assertions below.
const COLLAPSED_TIMER_FLOOR = 86;
const EXPANDED_TIMER_FLOOR = 44;
const LADDER_BODY_CAP = 104;

/**
 * The Time tab must render as one page in *every* state — Claim available,
 * last-call banner, coin-feedback line, Coin Promo ladder expanded — in both
 * flavors (ADR-0029 amendment 2026-08-03). The panel keeps `overflow-y-auto`
 * deliberately, so the guarantee lives here rather than in CSS: a regression
 * must fail loudly in CI and merely degrade to a scrollbar in production.
 *
 * This replaces the former fixed `[data-time-frame]` height === 200 pin; the
 * frame now flexes, so only its floor is pinned.
 */
async function assertTimeTabFitsOnePage(
  flavor: "account" | "guest",
): Promise<void> {
  const panelFits = `(() => { const panel = document.querySelector('[data-widget-tab-panel]'); return panel.scrollHeight - panel.clientHeight <= 1; })()`;
  const frameHeight =
    "document.querySelector('[data-time-frame]')?.getBoundingClientRect().height ?? 0";

  await evaluate("window.widgetTabsHarness.setLastCall(true)");
  if (flavor === "account") {
    await evaluate("window.widgetTabsHarness.emitCoinFeedback()");
    await waitFor(
      "Boolean(document.querySelector('[data-coin-feedback]'))",
      true,
      "coin-feedback line rendered",
    );
  }

  if (!(await evaluate(panelFits)))
    throw new Error(`Time tab (${flavor}) scrolls with the ladder collapsed`);
  if ((await evaluate<number>(frameHeight)) < COLLAPSED_TIMER_FLOOR)
    throw new Error(`Time frame (${flavor}) is below its collapsed floor`);

  if (flavor === "account") {
    // The collapsed ladder must still name an offer: a customer who never
    // expands it sees only this line, and the bonus must carry its coin.
    if (
      !(await evaluate(
        "/coin promo\\s+—\\s+₱\\d/i.test(document.querySelector('[data-coin-promo-ladder] summary')?.innerText ?? '')",
      ))
    )
      throw new Error(
        "Coin Promo summary does not lead with a tier and its coin amount",
      );
    await evaluate(
      "document.querySelector('[data-coin-promo-ladder] summary').click()",
    );
    await evaluate("new Promise(r => requestAnimationFrame(r))");
    if (
      !(await evaluate(
        "document.querySelector('[data-coin-promo-ladder]')?.open === true",
      ))
    )
      throw new Error("Coin Promo ladder did not expand");
    if (!(await evaluate(panelFits)))
      throw new Error("Time tab scrolls with the Coin Promo ladder expanded");
    if ((await evaluate<number>(frameHeight)) < EXPANDED_TIMER_FLOOR)
      throw new Error("Time frame is below its expanded floor");
    // The ladder body, not the page, absorbs an overlong tier list.
    if (
      !(await evaluate(
        `(() => { const body = document.querySelector('[data-coin-promo-body]'); return Boolean(body) && getComputedStyle(body).overflowY === 'auto' && body.clientHeight <= ${LADDER_BODY_CAP}; })()`,
      ))
    )
      throw new Error("Coin Promo ladder body is not a capped scroll container");
    await evaluate(
      "document.querySelector('[data-coin-promo-ladder] summary').click()",
    );
    await evaluate("new Promise(r => requestAnimationFrame(r))");
  }

  await evaluate("window.widgetTabsHarness.setLastCall(false)");
}

const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
  cwd: dir,
  env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
  stdout: "ignore",
  stderr: "inherit",
});
const profile = browserProfilePath(`openpiso-tabs-${process.pid}`);
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
  await assertProcessRunning(vite, "Widget matrix Vite");
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
      method?: string;
      params?: { exceptionDetails?: { text: string; exception?: { description?: string } } };
      result?: unknown;
      error?: { message: string };
    };
    if (message.method === "Runtime.exceptionThrown") {
      const details = message.params?.exceptionDetails;
      lastPageException =
        details?.exception?.description ?? details?.text ?? "unknown";
    }
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
    "Widget harness mount",
    15_000,
  );
  const tabs = await evaluate(
    "[...document.querySelectorAll('[role=tab]')].map(t => [t.textContent.trim(), t.getAttribute('aria-selected')])",
  );
  if (
    JSON.stringify(tabs) !==
    JSON.stringify([
      ["Time", "true"],
      ["Points", "false"],
      ["Account", "false"],
      ["Settings", "false"],
    ])
  )
    throw new Error(`Bad default tabs: ${JSON.stringify(tabs)}`);
  await evaluate(
    "document.querySelector('[role=tab][aria-selected=true]').dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }))",
  );
  await waitFor(
    "document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim()",
    "Points",
  );
  await click("Time");
  await assertStableHeader();
  if (
    !(await evaluate(
      "document.body.innerText.includes('Redeem voucher') && document.body.innerText.includes('Confirm Claim') === false",
    ))
  )
    throw new Error("Time actions missing");
  await assertTimeTabFitsOnePage("account");
  await assertPanelReachable();
  const claimCalls = await evaluate<number>(
    "window.widgetTabsHarness.calls.claim_guest_time ?? 0",
  );
  await click("Claim 5 min");
  await assertDialogReachable();
  await click("Cancel");
  if (
    (await evaluate("window.widgetTabsHarness.calls.claim_guest_time ?? 0")) !==
    claimCalls
  )
    throw new Error("Claim Cancel mutated Guest Time");
  await click("Claim 5 min");
  await click("Confirm Claim");
  await waitFor(
    "window.widgetTabsHarness.calls.claim_guest_time",
    claimCalls + 1,
  );
  await setInput('input[aria-label="Voucher code"]', "OP-TEST");
  await submitForm('input[aria-label="Voucher code"]');
  await waitFor("window.widgetTabsHarness.calls.redeem_voucher", 1);

  await click("Points");
  await waitFor(
    "document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim()",
    "Points",
  );
  await assertStableHeader();
  await waitFor("document.body.textContent.includes('Points balance')", true);
  if (
    !(await evaluate(
      // Case-insensitive: these labels are uppercased by CSS and `innerText`
      // reflects `text-transform`, so a case-sensitive match can never hit.
      "['claim time', 'spin the wheel', 'transfer points'].every(label => document.body.innerText.toLowerCase().includes(label))",
    ))
  )
    throw new Error("Points actions missing");
  await assertPanelReachable();
  await click("Claim Time");
  await waitFor("document.body.innerText.includes('10 pts → 4 hours')", true);
  await click("10 pts → 4 hours");
  await click("Confirm Redemption");
  await waitFor("window.widgetTabsHarness.calls.redeem_points", 1);
  await waitFor(
    "document.querySelectorAll('[data-tier-id]').length",
    0,
    "Claim Time dialog closed after redemption",
  );
  // "Spin the Wheel" is WheelEntry's heading, not a control; the opener is
  // "Spin for 1 point", which the dialog then repeats — so both clicks are
  // scoped by dialog membership the same way wheel-browser.ts does it.
  await clickScoped("Spin for 1 point", "outside-dialog");
  await waitFor(
    "document.body.innerText.includes('Bet 1 point')",
    true,
    "Wheel dialog opened",
  );
  await clickScoped("Spin for 1 point", "in-dialog");
  await waitFor(
    "document.body.innerText.includes('Won 5 min!')",
    true,
    "Spin result in dialog",
  );
  await click("Close");
  await click("Transfer Points");
  await assertDialogReachable();
  await setInput("#pt-transfer-to", "maria");
  await setInput("#pt-transfer-points", "5");
  await submitForm("#pt-transfer-points");
  await waitFor(
    "document.body.innerText.includes('123456')",
    true,
    "Points Transfer code",
  );
  await assertDialogReachable();
  await click("Done");
  await click("Transfer Points");
  await assertDialogReachable();
  if (await evaluate("document.body.innerText.includes('123456')"))
    throw new Error("Reopened Points Transfer dialog shows stale code");
  if (
    !(await evaluate(
      "document.querySelector('#pt-transfer-to') instanceof HTMLInputElement && document.querySelector('#pt-transfer-to').value === ''",
    ))
  )
    throw new Error("Reopened Points Transfer dialog did not reset its form");
  await click("Cancel");

  await click("Account");
  await assertStableHeader();
  await waitFor("document.body.textContent.includes('Signed in as')", true);
  if (
    !(await evaluate(
      "document.body.textContent.includes('alex') && document.body.innerText.includes('Transfer Time') && document.body.innerText.includes('Change Password') && !document.body.innerText.includes('portable')",
    ))
  )
    throw new Error("Account content invalid");
  if (
    (await evaluate(
      "[...document.querySelectorAll('button')].filter(b => b.textContent?.trim() === 'Log out').length",
    )) !== 1
  )
    throw new Error("Log out duplicated");
  if (
    !(await evaluate(
      "document.querySelector('[role=alert]')?.textContent.includes('Persistent test error')",
    ))
  )
    throw new Error("Persistent error missing");
  await click("Transfer Time");
  await assertDialogReachable();
  await setInput("#transfer-to", "maria");
  await setInput("#transfer-seconds", "60");
  await submitForm("#transfer-seconds");
  await waitFor("window.widgetTabsHarness.calls.transfer_time", 1);
  await click("Change Password");
  await assertDialogReachable();
  await setInput("#old-password", "old-password");
  await setInput("#new-password", "new-password");
  await setInput("#confirm-password", "new-password");
  await submitForm("#confirm-password");
  await waitFor("window.widgetTabsHarness.calls.change_account_password", 1);
  await waitFor(
    "document.body.innerText.includes('Password changed successfully')",
    true,
  );
  await assertDialogReachable();
  await click("Close");
  await click("Settings");
  await assertStableHeader();
  if (
    !(await evaluate(
      "Boolean(document.querySelector('#owner-pin')) && !document.querySelector('#server-url') && !document.body.innerText.includes('Quit client') && document.body.innerText.toLowerCase().includes('harness shop') && document.body.innerText.toLowerCase().includes('pc-07')",
    ))
  )
    throw new Error("Settings PIN gate leaked controls or hid Widget identity");
  await submitOwnerPin("9999");
  await waitFor(
    "[...document.querySelectorAll('[role=alert]')].some(element => element.textContent.includes('Incorrect owner PIN.'))",
    true,
  );
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Incorrect PIN unlocked Settings");
  await click("Cancel");
  await waitFor(
    "document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim()",
    "Account",
  );

  await click("Settings");
  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  if (await evaluate("Boolean(document.querySelector('#owner-pin'))"))
    throw new Error("Successful PIN did not replace the gate");
  await assertPanelReachable();
  await waitFor(
    "document.querySelector('[role=switch]')?.getAttribute('aria-checked')",
    "true",
    "Launch at startup defaults on",
  );
  await evaluate("document.querySelector('[role=switch]')?.click()");
  await waitFor(
    "document.querySelector('[role=switch]')?.getAttribute('aria-checked')",
    "false",
    "Launch at startup can be disabled",
  );
  await evaluate("document.querySelector('[role=switch]')?.click()");
  await waitFor(
    "document.querySelector('[role=switch]')?.getAttribute('aria-checked')",
    "true",
    "Launch at startup can be enabled",
  );
  await waitFor(
    "document.querySelector('[data-testid=\"settings-disable-windows-sign-out\"]')?.getAttribute('aria-checked')",
    "false",
    "Windows sign-out protection defaults off",
  );
  await evaluate("document.querySelector('[data-testid=\"settings-disable-windows-sign-out\"]')?.click()");
  await waitFor(
    "document.querySelector('[data-testid=\"settings-disable-windows-sign-out\"]')?.getAttribute('aria-checked')",
    "true",
    "Windows sign-out protection can be enabled",
  );
  await evaluate("document.querySelector('[data-testid=\"settings-disable-windows-sign-out\"]')?.click()");
  await waitFor(
    "document.querySelector('[data-testid=\"settings-disable-windows-sign-out\"]')?.getAttribute('aria-checked')",
    "false",
    "Windows sign-out protection round-trips",
  );

  await click("Points");
  await click("Settings");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Selecting a customer tab did not relock Settings");

  await evaluate("window.widgetTabsHarness.setPinDelay(100)");
  await submitOwnerPin("1234");
  await click("Account");
  await click("Settings");
  await Bun.sleep(150);
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Late PIN verification preserved an invalidated unlock");
  await evaluate("window.widgetTabsHarness.setPinDelay(0)");

  await evaluate("window.widgetTabsHarness.resetViewModes()");
  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  await click("Minimize");
  if ((await evaluate("window.widgetTabsHarness.viewModes.at(-1)")) !== "mini")
    throw new Error("Minimize did not set Mini Bar View Mode");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Minimize did not immediately relock Settings");
  await evaluate("window.widgetTabsHarness.restart()");
  await click("Settings");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Minimize did not relock Settings");

  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  await click("Log out");
  await waitFor("window.widgetTabsHarness.logoutCalls", 1);
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Log out did not relock Settings");

  await evaluate("window.widgetTabsHarness.restart()");
  await click("Settings");
  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  await evaluate("window.widgetTabsHarness.enterFullscreenLock()");
  if (
    await evaluate(
      "Boolean(document.querySelector('[aria-label=\"OpenPiso workstation widget\"]'))",
    )
  )
    throw new Error("Fullscreen Lock entry did not destroy the Widget");
  await evaluate("window.widgetTabsHarness.returnFromFullscreenLock()");
  await click("Settings");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Fullscreen Lock entry preserved Settings unlock");

  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  await evaluate("window.widgetTabsHarness.restart()");
  await click("Settings");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Restart preserved Settings unlock");

  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  await evaluate("window.widgetTabsHarness.destroyWidget()");
  if (
    await evaluate(
      "Boolean(document.querySelector('[aria-label=\"OpenPiso workstation widget\"]'))",
    )
  )
    throw new Error("Widget destruction did not unmount the Widget");
  await evaluate("window.widgetTabsHarness.mountWidget()");
  await click("Settings");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Widget destruction preserved Settings unlock");
  await click("Points");
  await evaluate("window.widgetTabsHarness.restart()");
  if (
    (await evaluate(
      "document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim()",
    )) !== "Time"
  )
    throw new Error("Account Widget re-entry did not reset to Time");

  await evaluate("window.widgetTabsHarness.setFlavor('guest')");
  const guestTabs = await evaluate(
    "[...document.querySelectorAll('[role=tab]')].map(t => [t.textContent.trim(), t.getAttribute('aria-selected')])",
  );
  if (
    JSON.stringify(guestTabs) !==
    JSON.stringify([
      ["Time", "true"],
      ["Settings", "false"],
    ])
  )
    throw new Error(`Bad Guest tabs: ${JSON.stringify(guestTabs)}`);
  await assertStableHeader();
  if (
    !(await evaluate(
      "document.body.innerText.toLowerCase().includes('guest time') && document.body.innerText.includes('OpenPiso Kiosk') && document.body.innerText.includes('Insert Coin')",
    ))
  )
    throw new Error("Guest Time guidance missing");
  if (
    await evaluate(
      "['Voucher', 'Claim', 'Points', 'Account', 'Transfer', 'Change Password', 'Log out'].some(label => document.body.innerText.includes(label))",
    )
  )
    throw new Error("Guest Widget exposed Account-only controls");
  if (
    !(await evaluate(
      "document.body.innerText.toLowerCase().includes('harness shop') && document.body.innerText.toLowerCase().includes('pc-07') && document.body.innerText.includes('Minimize') && document.body.innerText.includes('Log in')",
    ))
  )
    throw new Error("Guest shell identity or footer actions missing");
  if (
    !(await evaluate(
      "document.querySelector('[role=alert]')?.textContent.includes('Persistent test error')",
    ))
  )
    throw new Error("Persistent error missing from Guest Time");
  await assertTimeTabFitsOnePage("guest");
  await assertPanelReachable();
  await click("Settings");
  if (
    !(await evaluate(
      "Boolean(document.querySelector('#owner-pin')) && document.body.innerText.toLowerCase().includes('harness shop') && document.body.innerText.toLowerCase().includes('pc-07') && document.querySelector('[role=alert]')?.textContent.includes('Persistent test error')",
    ))
  )
    throw new Error("Guest Settings gate, identity, or error missing");
  await click("Cancel");
  await waitFor(
    "document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim()",
    "Time",
  );
  await click("Settings");
  await submitOwnerPin("1234");
  await waitFor("Boolean(document.querySelector('#server-url'))", true);
  await assertPanelReachable();
  await click("Time");
  await click("Settings");
  if (!(await evaluate("Boolean(document.querySelector('#owner-pin'))")))
    throw new Error("Guest Settings did not relock after tab change");
  await click("Time");
  await evaluate("window.widgetTabsHarness.resetViewModes()");
  await click("Minimize");
  if ((await evaluate("window.widgetTabsHarness.viewModes.at(-1)")) !== "mini")
    throw new Error("Guest Minimize did not set Mini Bar View Mode");
  const authRequests = await evaluate<number>(
    "window.widgetTabsHarness.authenticationRequests",
  );
  await click("Log in");
  await waitFor(
    "window.widgetTabsHarness.authenticationRequests",
    authRequests + 1,
    "Guest Log in authentication route",
  );
  await click("Settings");
  await evaluate("window.widgetTabsHarness.restart()");
  if (
    (await evaluate(
      "document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim()",
    )) !== "Time"
  )
    throw new Error("Guest Widget re-entry did not reset to Time");
  console.log("GREEN: Account and Guest Widget tab browser tests passed");
} finally {
  ws?.close();
  browser.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

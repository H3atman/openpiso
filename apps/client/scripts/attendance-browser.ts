import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertProcessRunning,
  browserProfilePath,
  removeBrowserProfile,
  stopProcessTree,
} from "./browser-harness-process.ts";
import { formatProseDuration } from "../src/lib/format.ts";

for (const [seconds, expected] of [
  [90_000, "25 hours"],
  [5_400, "1 hour 30 min"],
  [2_700, "45 min"],
  [30, "30 sec"],
  [0, "0 sec"],
]) {
  if (formatProseDuration(seconds) !== expected) {
    throw new Error(`Expected ${expected} for ${seconds} seconds`);
  }
}

const clientDir = resolve(fileURLToPath(import.meta.url), "../..");
const artifactPath = resolve(clientDir, "../../target/attendance-calendar.png");
const chrome = [
  process.env.CHROME_PATH,
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
].find((candidate) => candidate && existsSync(candidate));
if (!chrome) {
  console.log("SKIP: Chrome/Chromium not found");
  process.exit(0);
}

const vitePort = 17_000 + (process.pid % 1_000);
const cdpPort = 9_900 + (process.pid % 40);
const profile = browserProfilePath(`openpiso-attendance-${process.pid}`);
const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
  cwd: clientDir,
  env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
  stdout: "ignore",
  stderr: "inherit",
});
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

let socket;
let nextId = 0;
let lastPageException = "";
let lastPageConsoleError = "";
let lastPageNetworkError = "";
const networkErrors: string[] = [];
const networkRequests = new Map<string, string>();
const pending = new Map();
const documentRequests: string[] = [];

async function waitForUrl(url) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // Vite is still starting.
    }
    await Bun.sleep(100);
  }
  throw new Error(`Timeout waiting for ${url}`);
}

function call(method, params = {}) {
  const id = ++nextId;
  return new Promise((resolveValue, reject) => {
    pending.set(id, { resolve: resolveValue, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

async function evaluate(expression) {
  const response = await call("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (response.exceptionDetails) {
    throw new Error(response.exceptionDetails.text);
  }
  return response.result.value;
}

async function waitFor(expression, expected, label = expression) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if ((await evaluate(expression)) === expected) return;
    await Bun.sleep(50);
  }
  const pageState = await evaluate(`JSON.stringify({
    url: location.href,
    title: document.title,
    readyState: document.readyState,
    text: document.body.innerText.slice(0, 1000),
    scripts: [...document.scripts].map((script) => script.src),
  })`).catch(() => "unavailable");
  throw new Error(
    `Timeout waiting for ${label}${lastPageException ? `\nPage exception: ${lastPageException}` : ""}${lastPageConsoleError ? `\nConsole error: ${lastPageConsoleError}` : ""}${lastPageNetworkError ? `\nNetwork error: ${lastPageNetworkError}` : ""}\nDocument requests: ${JSON.stringify(documentRequests)}\nNetwork failures: ${JSON.stringify(networkErrors.slice(-10))}\nPage: ${pageState}`,
  );
}

try {
  await waitForUrl(`http://127.0.0.1:${vitePort}/attendance-harness.html`);
  await assertProcessRunning(vite, "Daily Reward harness Vite");
  await waitForUrl(`http://127.0.0.1:${cdpPort}/json/version`);
  const pages = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
  const page = pages.find((entry) => entry.type === "page");
  if (!page) throw new Error("No Chrome page");
  socket = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolveOpen, reject) => {
    socket.onopen = resolveOpen;
    socket.onerror = () => reject(new Error("Chrome DevTools connection failed"));
  });
  socket.onmessage = (event) => {
    const message = JSON.parse(String(event.data));
    if (message.method === "Runtime.exceptionThrown") {
      lastPageException = message.params?.exceptionDetails?.text ?? "unknown page exception";
    }
    if (message.method === "Runtime.consoleAPICalled" && message.params?.type === "error") {
      lastPageConsoleError = message.params.args
        ?.map((arg) => arg.description ?? arg.value ?? "")
        .join(" ") ?? "unknown console error";
    }
    if (message.method === "Network.requestWillBeSent") {
      networkRequests.set(message.params.requestId, message.params.request.url);
      if (message.params.type === "Document") {
        documentRequests.push(message.params.request.url);
        if (documentRequests.length > 10) documentRequests.shift();
      }
      if (networkRequests.size > 500) {
        const oldestRequest = networkRequests.keys().next().value;
        if (oldestRequest) networkRequests.delete(oldestRequest);
      }
    }
    if (message.method === "Network.loadingFailed") {
      lastPageNetworkError = `${networkRequests.get(message.params.requestId) ?? "unknown request"}: ${message.params.errorText}`;
      networkErrors.push(lastPageNetworkError);
      if (networkErrors.length > 20) networkErrors.shift();
    }
    if (message.method === "Network.responseReceived" && message.params.response.status >= 400) {
      lastPageNetworkError = `${message.params.response.status} ${message.params.response.url}`;
      networkErrors.push(lastPageNetworkError);
      if (networkErrors.length > 20) networkErrors.shift();
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
  await call("Network.enable");
  await call("Emulation.setDeviceMetricsOverride", {
    width: 320,
    height: 480,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await call("Page.navigate", {
    url: `http://127.0.0.1:${vitePort}/attendance-harness.html`,
  });
  await waitFor("Boolean(document.querySelector('[data-harness-ready]'))", true, "harness mount");
  await waitFor(
    "Boolean(document.querySelector('[aria-label=\"OpenPiso workstation widget\"]'))",
    true,
    "signed-in Widget",
  );
  const strip = await evaluate(
    "document.querySelector('[data-daily-reward-strip]')?.innerText ?? ''",
  );
  if (!strip) throw new Error("Daily Reward strip missing for supported signed-in Account");
  if (!strip.includes("₱6 more today")) {
    throw new Error(`Daily Reward progress is incorrect: ${strip}`);
  }
  if (!strip.includes("1 hour 30 min waiting to claim")) {
    throw new Error(`Claimable Daily Reward is not prose-formatted: ${strip}`);
  }
  if (/attendance|check-in/i.test(strip)) {
    throw new Error(`Internal Attendance wording leaked into customer copy: ${strip}`);
  }
  const progressLabel = await evaluate(
    "document.querySelector('[data-daily-reward-progress]')?.getAttribute('aria-label') ?? ''",
  );
  if (progressLabel !== "40% of today's Daily Reward") {
    throw new Error(`Daily Reward progress ring is incorrect: ${progressLabel}`);
  }
  const summaryCallsBeforeCoin = await evaluate(`window.attendanceHarness?.calls.filter((call) => call.command === 'get_attendance_summary').length ?? 0`);
  await evaluate("window.attendanceHarness?.acceptCoin(600)");
  await waitFor(
    "document.querySelector('[data-daily-reward-primary]')?.textContent?.trim() === '₱4 more today'",
    true,
    "Daily Reward progress refresh after accepted coin",
  );
  const summaryCallsAfterCoin = await evaluate(`window.attendanceHarness?.calls.filter((call) => call.command === 'get_attendance_summary').length ?? 0`);
  if (summaryCallsAfterCoin <= summaryCallsBeforeCoin) {
    throw new Error("Coin progress did not promptly refresh the Daily Reward summary");
  }
  await evaluate("window.attendanceHarness?.heartbeat()");
  await Bun.sleep(100);
  const summaryCallsAfterHeartbeat = await evaluate(`window.attendanceHarness?.calls.filter((call) => call.command === 'get_attendance_summary').length ?? 0`);
  if (summaryCallsAfterHeartbeat !== summaryCallsAfterCoin) {
    throw new Error("Routine timer heartbeat unnecessarily refreshed Daily Reward summary");
  }
  await evaluate("window.attendanceHarness?.setAttendanceToday(false, 'checked_in', 1000)");
  await waitFor(
    "document.querySelector('[data-daily-reward-primary]')?.textContent?.trim() === 'Daily Reward paused' && document.querySelector('[data-daily-reward-progress]')?.getAttribute('aria-label') === 'Daily Reward complete'",
    true,
    "earned Daily Reward remains marked after pausing",
  );
  await evaluate("window.attendanceHarness?.setAttendanceToday(true, 'ineligible', 0)");
  await waitFor(
    "document.querySelector('[data-daily-reward-primary]')?.textContent?.trim() === 'Daily Reward paused'",
    true,
    "ineligible Daily Reward copy",
  );
  await evaluate("window.attendanceHarness?.setAttendanceToday(true, 'in_progress', 600)");
  await waitFor(
    "document.querySelector('[data-daily-reward-primary]')?.textContent?.trim() === '₱4 more today'",
    true,
    "Daily Reward resume after paused reward fixture",
  );
  await evaluate("document.querySelector('[data-daily-reward-strip]')?.click()");
  await waitFor(
    "Boolean(document.querySelector('[role=dialog][aria-label=\"Daily Reward\"]'))",
    true,
    "Daily Reward month sheet",
  );
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-day=\"2026-09-01\"][data-day-status=\"checked_in\"]'))",
    true,
    "Admin-local Daily Reward calendar day",
  );
  const statusMarks = await evaluate(`[
    ['2026-09-01', 'checked_in'],
    ['2026-09-02', 'missed'],
    ['2026-09-03', 'ineligible'],
    ['2026-09-26', 'today_progress'],
    ['2026-09-27', 'future'],
  ].every(([date, status]) => document.querySelector(
    '[data-daily-reward-day="' + date + '"]',
  )?.getAttribute('data-day-status') === status)`);
  if (!statusMarks) throw new Error("Daily Reward calendar marks are incorrect");
  const marks = JSON.parse(await evaluate(`JSON.stringify(Object.fromEntries(
    ['2026-09-01', '2026-09-02', '2026-09-03', '2026-09-26', '2026-09-27'].map((date) => {
      const cell = document.querySelector('[data-daily-reward-day="' + date + '"]');
      return [date, cell?.querySelectorAll('span')[1]?.textContent?.trim() ?? ''];
    }),
  ))`));
  const expectedMarks = {
    "2026-09-01": "✓",
    "2026-09-02": "×",
    "2026-09-03": "—",
    "2026-09-26": "40%",
    "2026-09-27": "",
  };
  if (JSON.stringify(marks) !== JSON.stringify(expectedMarks)) {
    throw new Error(`Daily Reward calendar marks are incorrect: ${JSON.stringify(marks)}`);
  }
  const legend = await evaluate(
    "document.querySelector('[data-calendar-legend]')?.innerText ?? ''",
  );
  if (!["✓ Daily Reward earned", "× No Daily Reward", "— Not eligible", "% Today's progress", "Blank = future date"].every((label) => legend.includes(label))) {
    throw new Error(`Daily Reward calendar legend is incomplete: ${legend}`);
  }
  const selectedMonthCopy = await evaluate(
    "document.querySelector('[data-daily-reward-month-summary]')?.innerText ?? ''",
  );
  if (!selectedMonthCopy.includes("2 Daily Reward days earned") || !selectedMonthCopy.includes("2 hours earned") || !selectedMonthCopy.includes("Claimable from Oct 1")) {
    throw new Error(`Daily Reward month summary is incomplete: ${selectedMonthCopy}`);
  }
  const selectedClaim = await evaluate(`(() => {
    const button = document.querySelector('[data-claim-selected-month]');
    return { text: button?.innerText?.trim(), disabled: button?.disabled, reason: document.querySelector('[data-claim-disabled-reason]')?.innerText?.trim() };
  })()`);
  if (selectedClaim?.text !== "Claim all 2 hours" || !selectedClaim.disabled || selectedClaim.reason !== "Claiming starts Oct 1") {
    throw new Error(`Non-claimable Daily Reward month reason is incorrect: ${JSON.stringify(selectedClaim)}`);
  }
  const adminDate = await evaluate(
    "document.querySelector('[data-daily-reward-sheet] header p')?.textContent?.trim() ?? ''",
  );
  if (adminDate !== "2026-09-26") {
    throw new Error(`Daily Reward date was not kept in Admin local time: ${adminDate}`);
  }
  const screenshot = await call("Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: false,
  });
  await mkdir(resolve(artifactPath, ".."), { recursive: true });
  await Bun.write(artifactPath, Buffer.from(screenshot.data, "base64"));
  console.log(`ARTIFACT: ${artifactPath}`);

  await evaluate("document.querySelector('[data-previous-month]')?.click()");
  await waitFor(
    "document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '2026-08'",
    true,
    "claimable August Daily Reward calendar",
  );
  const augustCopy = await evaluate(
    "document.querySelector('[data-daily-reward-month-summary]')?.innerText ?? ''",
  );
  if (!augustCopy.includes("2 Daily Reward days earned") || !augustCopy.includes("Claimed Sep 3") || !augustCopy.includes("Claim by Sep 30") || !augustCopy.includes("More Daily Reward time is ready after an earlier claim")) {
    throw new Error(`Supplemental Daily Reward claim details are incomplete: ${augustCopy}`);
  }
  const augustClaim = await evaluate(`(() => {
    const button = document.querySelector('[data-claim-selected-month]');
    return { text: button?.innerText?.trim(), disabled: button?.disabled };
  })()`);
  if (augustClaim?.text !== "Claim all 1 hour" || augustClaim.disabled) {
    throw new Error(`Claimable month button is incorrect: ${JSON.stringify(augustClaim)}`);
  }
  await evaluate("document.querySelector('[data-previous-month]')?.click()");
  await waitFor(
    "document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '2026-07'",
    true,
    "expired July Daily Reward calendar",
  );
  const julyCopy = await evaluate(
    "document.querySelector('[data-daily-reward-month-summary]')?.innerText ?? ''",
  );
  if (!julyCopy.includes("Expired Aug 31")) {
    throw new Error(`Expired Daily Reward date is incorrect: ${julyCopy}`);
  }
  await evaluate("document.querySelector('[data-next-month]')?.click()");
  await waitFor(
    "document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '2026-08'",
    true,
    "return to August Daily Reward calendar",
  );
  await evaluate("document.querySelector('[data-next-month]')?.click()");
  await waitFor(
    "document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '2026-09'",
    true,
    "return to current Daily Reward calendar",
  );
  const olderClaimCopy = await evaluate(
    "document.querySelector('[data-older-claim=\\\"2025-09\\\"]')?.innerText ?? ''",
  );
  if (!olderClaimCopy.includes("Claim by Oct 31, 2025")) {
    throw new Error(`Older Daily Reward date omitted its year: ${olderClaimCopy}`);
  }

  const calendarMonths = [
    "2026-08", "2026-07", "2026-06", "2026-05", "2026-04", "2026-03",
    "2026-02", "2026-01", "2025-12", "2025-11", "2025-10",
  ];
  for (const month of calendarMonths) {
    await evaluate("document.querySelector('[data-previous-month]')?.click()");
    await waitFor(
      `document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '${month}'`,
      true,
      `calendar month ${month}`,
    );
  }
  const previousDisabled = await evaluate(
    "document.querySelector('[data-previous-month]')?.disabled === true",
  );
  if (!previousDisabled) throw new Error("Calendar navigated before its 12-month range");
  for (const month of [
    "2025-11", "2025-12", "2026-01", "2026-02", "2026-03", "2026-04",
    "2026-05", "2026-06", "2026-07", "2026-08", "2026-09",
  ]) {
    await evaluate("document.querySelector('[data-next-month]')?.click()");
    await waitFor(
      `document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '${month}'`,
      true,
      `calendar month ${month}`,
    );
  }
  const nextDisabled = await evaluate(
    "document.querySelector('[data-next-month]')?.disabled === true",
  );
  if (!nextDisabled) throw new Error("Calendar navigated beyond the Admin-local month");

  await evaluate("document.querySelector('[data-claim-month=\"2025-09\"]')?.click()");
  await waitFor(
    "!document.querySelector('[data-claim-month=\"2025-09\"]')",
    true,
    "one-tap Daily Reward claim refresh",
  );
  const claimCalls = JSON.parse(await evaluate(`JSON.stringify(
    window.attendanceHarness?.calls.filter((call) => call.command === 'claim_attendance') ?? [],
  )`));
  if (claimCalls.length !== 1) {
    throw new Error(`Expected one one-tap claim call, received ${claimCalls.length}`);
  }
  const claimArgs = claimCalls[0]?.args;
  if (
    claimArgs?.earningMonth !== "2025-09" ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(claimArgs?.requestId ?? "")
  ) {
    throw new Error(`Claim request arguments are incorrect: ${JSON.stringify(claimArgs)}`);
  }
  const refreshedStrip = await evaluate(
    "document.querySelector('[data-daily-reward-strip]')?.innerText ?? ''",
  );
  if (!refreshedStrip.includes("1 hour waiting to claim")) {
    throw new Error(`Daily Reward summary was not refreshed after claiming: ${refreshedStrip}`);
  }

  await evaluate("window.attendanceHarness?.restoreClaimableAugust()");
  await evaluate("document.querySelector('[data-previous-month]')?.click()");
  await waitFor(
    "document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '2026-08'",
    true,
    "August Daily Reward claim error checks",
  );
  await evaluate(`window.attendanceHarness?.failNextAttendanceClaim({
    error: 'attendance_claim_denied',
    reason: 'expired',
    status_code: 409,
    message: 'This Daily Reward has expired.',
  })`);
  await evaluate("document.querySelector('[data-claim-selected-month]')?.click()");
  await waitFor(
    "document.querySelector('[role=alert]')?.textContent?.trim() === 'This Daily Reward has expired.'",
    true,
    "structured Daily Reward denial copy",
  );
  const deniedRequestId = await evaluate(`window.attendanceHarness?.attendanceRequestArgs.at(-1)?.requestId`);
  await evaluate(`window.attendanceHarness?.failNextAttendanceClaim({
    error: 'invalid_input',
    status_code: 400,
    message: 'The Daily Reward request was invalid.',
  })`);
  await evaluate("document.querySelector('[data-claim-selected-month]')?.click()");
  await waitFor(
    "document.querySelector('[role=alert]')?.textContent?.trim() === 'The Daily Reward request was invalid.'",
    true,
    "definitive Daily Reward request error copy",
  );
  const invalidRequestId = await evaluate(`window.attendanceHarness?.attendanceRequestArgs.at(-1)?.requestId`);
  if (!deniedRequestId || invalidRequestId === deniedRequestId) {
    throw new Error("Daily Reward request ID was not cleared after a definitive denial");
  }

  await evaluate("window.attendanceHarness?.setOffline(true)");
  await evaluate("document.querySelector('[data-previous-month]')?.click()");
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-stale]'))",
    true,
    "saved Daily Reward state after offline month request",
  );
  await evaluate("window.attendanceHarness?.setTimer(0)");
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-lock-entry]'))",
    true,
    "zero-time Lock Daily Reward entry",
  );
  const lockEntry = await evaluate(
    "document.querySelector('[data-daily-reward-lock-entry]')?.innerText ?? ''",
  );
  if (!lockEntry.includes("August 2026") || !lockEntry.includes("1 hour")) {
    throw new Error(`Saved Daily Reward state was not retained on the Lock: ${lockEntry}`);
  }

  await evaluate("document.querySelector('[data-daily-reward-lock-claim=\"2026-08\"]')?.click()");
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-lock-entry] [role=alert]'))",
    true,
    "offline Daily Reward claim error",
  );
  const callsBeforeRetry = JSON.parse(await evaluate(`JSON.stringify(
    window.attendanceHarness?.calls.filter((call) => call.command === 'claim_attendance') ?? [],
  )`));
  const offlineRequestId = callsBeforeRetry.at(-1)?.args?.requestId;
  if (!offlineRequestId || offlineRequestId === invalidRequestId) {
    throw new Error("Daily Reward request ID was not cleared after a bad request");
  }
  await evaluate("window.attendanceHarness?.setOffline(false)");
  await evaluate("document.querySelector('[data-daily-reward-lock-claim=\"2026-08\"]')?.click()");
  await waitFor(
    "Boolean(document.querySelector('[aria-label=\"OpenPiso workstation widget\"]'))",
    true,
    "successful retry returns to Widget",
  );
  const callsAfterRetry = JSON.parse(await evaluate(`JSON.stringify(
    window.attendanceHarness?.calls.filter((call) => call.command === 'claim_attendance') ?? [],
  )`));
  if (
    callsAfterRetry.length !== callsBeforeRetry.length + 1 ||
    callsAfterRetry.at(-1)?.args?.requestId !== offlineRequestId
  ) {
    throw new Error("Daily Reward retry did not reuse its request ID");
  }
  await evaluate("window.attendanceHarness?.setOffline(true)");
  await evaluate("window.attendanceHarness?.setTimer(0)");
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-lock-entry]'))",
    true,
    "zero-time Account Lock after Daily Reward retry",
  );

  const claimCallsBeforeAccountSwitch = await evaluate(`window.attendanceHarness?.calls.filter((call) => call.command === 'claim_attendance').length ?? 0`);
  await evaluate("window.attendanceHarness?.setOffline(false)");
  await evaluate("window.attendanceHarness?.restoreClaimableAugust()");
  await evaluate("window.attendanceHarness?.holdNextAttendanceClaims(2)");
  await evaluate("document.querySelector('[data-daily-reward-lock-claim=\"2026-08\"]')?.click()");
  await waitFor(
    `window.attendanceHarness?.calls.filter((call) => call.command === 'claim_attendance').length === ${claimCallsBeforeAccountSwitch + 1}`,
    true,
    "old Account Daily Reward claim request",
  );
  await evaluate("window.attendanceHarness?.setAccount('account-2')");
  await evaluate("window.attendanceHarness?.setOffline(false)");
  await evaluate("window.attendanceHarness?.setTimer(3600)");
  await waitFor(
    "Boolean(document.querySelector('[aria-label=\"OpenPiso workstation widget\"]'))",
    true,
    "Widget after Lock",
  );
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-strip]'))",
    true,
    "new Account Daily Reward summary",
  );
  await evaluate("document.querySelector('[data-daily-reward-strip]')?.click()");
  await waitFor(
    "Boolean(document.querySelector('[role=dialog][aria-label=\"Daily Reward\"]'))",
    true,
    "new Account Daily Reward dialog",
  );
  await evaluate("document.querySelector('[data-previous-month]')?.click()");
  await waitFor(
    "document.querySelector('[data-selected-month]')?.getAttribute('data-selected-month') === '2026-08'",
    true,
    "new Account claimable Daily Reward month",
  );
  await evaluate("document.querySelector('[data-claim-selected-month]')?.click()");
  await waitFor(
    `window.attendanceHarness?.calls.filter((call) => call.command === 'claim_attendance').length === ${claimCallsBeforeAccountSwitch + 2}`,
    true,
    "new Account Daily Reward claim request",
  );
  await evaluate("window.attendanceHarness?.rejectNextAttendanceClaim()");
  await Bun.sleep(100);
  const pendingClaim = await evaluate(`(() => {
    const button = document.querySelector('[data-claim-selected-month]');
    return { text: button?.innerText?.trim(), disabled: button?.disabled };
  })()`);
  if (pendingClaim?.text !== "Claiming…" || !pendingClaim.disabled) {
    throw new Error(`Old Account claim cleared the new Account's busy state: ${JSON.stringify(pendingClaim)}`);
  }
  await evaluate("window.attendanceHarness?.rejectNextAttendanceClaim()");
  await waitFor(
    "document.querySelector('[data-claim-selected-month]')?.disabled === false",
    true,
    "new Account Daily Reward claim finishes",
  );
  await evaluate("document.querySelector('[role=dialog][aria-label=\"Daily Reward\"] button[aria-label=\"Close Daily Reward\"]')?.click()");
  await evaluate("window.attendanceHarness?.setAttendanceSupported(false)");
  await waitFor(
    "!document.querySelector('[data-daily-reward-strip]')",
    true,
    "pre-attendance Admin feature flag hides strip",
  );
  await evaluate("window.attendanceHarness?.setAttendanceSupported(true)");
  await waitFor(
    "Boolean(document.querySelector('[data-daily-reward-strip]'))",
    true,
    "supported Admin refreshes Daily Reward strip",
  );
  const secondAccountStrip = await evaluate(
    "document.querySelector('[data-daily-reward-strip]')?.innerText ?? ''",
  );
  if (!secondAccountStrip.includes("₱10 more today") || secondAccountStrip.includes("₱6 more today")) {
    throw new Error(`Daily Reward data leaked across Accounts: ${secondAccountStrip}`);
  }
  await evaluate("document.querySelector('[data-daily-reward-strip]')?.click()");
  await waitFor(
    "Boolean(document.querySelector('[data-claim-month=\"2025-09\"]'))",
    false,
    "old Account's claimable month is cleared",
  );
  await evaluate("document.querySelector('[role=dialog][aria-label=\"Daily Reward\"] button[aria-label=\"Close Daily Reward\"]')?.click()");
  await evaluate(`Array.from(document.querySelectorAll('button'))
    .find((button) => button.innerText.trim() === 'Log out')?.click()`);
  await waitFor(
    "!document.querySelector('[data-daily-reward-strip]')",
    true,
    "logout clears the Daily Reward strip",
  );
  const guestWidget = await evaluate(
    "document.querySelector('[aria-label=\"OpenPiso workstation widget\"]')?.innerText ?? ''",
  );
  if (!guestWidget.toLowerCase().includes("guest time")) {
    throw new Error(`Expected a Guest Widget after logout: ${guestWidget}`);
  }
  console.log("GREEN: Daily Reward calendar, claims, and account-scoped Lock retention work");
} finally {
  socket?.close();
  browser.kill();
  stopProcessTree(vite);
  removeBrowserProfile(profile);
}

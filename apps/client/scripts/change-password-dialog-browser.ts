// Browser regression harness for the real production ChangePasswordDialog.
// Requires Chrome. Usage: bun run scripts/change-password-dialog-browser.ts
//
// Covers: duplicate-submit prevention, busy/locked state, success message,
// wrong-password/validation/409/500 messages, interaction restoration after failure.
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
const chromeCandidates = [
  process.env.CHROME_PATH,
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
].filter((path): path is string => Boolean(path));
const chromePath = chromeCandidates.find(existsSync);

if (!chromePath) {
  console.log(
    "SKIP: Chrome/Chromium not found. Set CHROME_PATH or install Chrome to run this harness.",
  );
  process.exit(0);
}

const vitePort = 15_000 + (process.pid % 1_000);
const cdpPort = 9_800 + (process.pid % 150);
const pageUrl = `http://127.0.0.1:${vitePort}/change-password-dialog-harness.html`;
let id = 0;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();
let ws: WebSocket | undefined;

async function waitForUrl(url: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // Process still starting.
    }
    await Bun.sleep(100);
  }
  throw new Error(`Timed out waiting for ${url}`);
}

function call(method: string, params: object = {}): Promise<unknown> {
  if (!ws) throw new Error("CDP is not connected");
  const messageId = ++id;
  return new Promise((resolveValue, reject) => {
    pending.set(messageId, { resolve: resolveValue, reject });
    ws!.send(JSON.stringify({ id: messageId, method, params }));
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

async function waitFor<T>(expression: string, expected: T): Promise<void> {
  for (let attempt = 0; attempt < 80; attempt += 1) {
    if ((await evaluate<T>(expression)) === expected) return;
    await Bun.sleep(50);
  }
  throw new Error(`Timed out waiting for: ${expression}`);
}

async function loadDialog(): Promise<void> {
  await call("Page.navigate", { url: pageUrl });
  await waitFor("Boolean(document.querySelector('#old-password'))", true);
}

async function fillAndSubmitTwice(): Promise<{
  calls: number;
  cancelDisabled: boolean;
  submitDisabled: boolean;
  saving: boolean;
}> {
  return evaluate(`(async () => {
    for (const [selector, value] of [
      ['#old-password', 'old-password'],
      ['#new-password', 'new-password'],
      ['#confirm-password', 'new-password'],
    ]) {
      const input = document.querySelector(selector);
      if (!(input instanceof HTMLInputElement)) throw new Error('missing ' + selector);
      input.value = value;
      input.dispatchEvent(new Event('input', { bubbles: true }));
    }
    const form = document.querySelector('form');
    if (!(form instanceof HTMLFormElement)) throw new Error('missing form');
    form.requestSubmit();
    form.requestSubmit();
    await new Promise(resolve => requestAnimationFrame(() => resolve()));
    const cancel = form.querySelector('button[type=button]');
    const submit = form.querySelector('button[type=submit]');
    return {
      calls: window.passwordChangeHarness.calls,
      cancelDisabled: cancel?.disabled === true,
      submitDisabled: submit?.disabled === true,
      saving: submit?.textContent?.includes('Saving…') === true,
    };
  })()`);
}

async function main(): Promise<number> {
  const vite = Bun.spawn(["bun", "x", "vite", "--strictPort"], {
    cwd: dir,
    env: { ...process.env, OPENPISO_CLIENT_DEV_PORT: String(vitePort) },
    stdout: "ignore",
    stderr: "inherit",
  });
  const profile = browserProfilePath(`openpiso-password-${process.pid}`);
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
    await waitForUrl(pageUrl);
    await assertProcessRunning(vite, "Password harness Vite");
    await waitForUrl(`http://127.0.0.1:${cdpPort}/json/version`);
    const pages = (await (
      await fetch(`http://127.0.0.1:${cdpPort}/json/list`)
    ).json()) as { type: string; webSocketDebuggerUrl: string }[];
    const page = pages.find((candidate) => candidate.type === "page");
    if (!page) throw new Error("Chrome page target not found");
    ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise<void>((resolveOpen, reject) => {
      ws!.onopen = () => resolveOpen();
      ws!.onerror = () => reject(new Error("CDP connection failed"));
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

    let red = false;

    // Scenario 1: duplicate-submit prevention
    console.log(
      "[TEST] locks immediately, invokes once, preserves success behavior",
    );
    await loadDialog();
    const result = await fillAndSubmitTwice();
    const expected = {
      calls: 1,
      cancelDisabled: true,
      submitDisabled: true,
      saving: true,
    };
    if (JSON.stringify(result) !== JSON.stringify(expected)) {
      console.log(
        `[RED ] expected ${JSON.stringify(expected)}, got ${JSON.stringify(result)}`,
      );
      red = true;
    } else {
      console.log("[GREEN] immediate lock and single invocation");
    }

    await evaluate("window.passwordChangeHarness.resolve()");
    await waitFor(
      "document.body.innerText.includes('Password changed successfully. Your session is still active.')",
      true,
    );
    const callsAfterSuccess = await evaluate<number>(
      "window.passwordChangeHarness.calls",
    );
    if (callsAfterSuccess !== 1) {
      console.log(
        `[RED ] expected 1 call after success, got ${callsAfterSuccess}`,
      );
      red = true;
    } else {
      console.log("[GREEN] success message and call count preserved");
    }

    // Scenario 2: preserved response copy plus 409/unexpected failure copy
    const failures = [
      {
        error: {
          code: "customer_message",
          message: "invalid username or password",
        },
        message: "invalid username or password",
      },
      {
        error: {
          code: "customer_message",
          message: "password must be between 4 and 128 characters",
        },
        message: "password must be between 4 and 128 characters",
      },
      {
        error: { code: "conflict" },
        message:
          "Password could not be changed. Re-enter your current password and try again.",
      },
      {
        error: { code: "internal_error" },
        message: "Password could not be changed. Try again.",
      },
    ];
    for (const { error, message } of failures) {
      console.log(
        `[TEST] renders failure and restores interaction: ${message}`,
      );
      await loadDialog();
      const r = await fillAndSubmitTwice();
      if (r.calls !== 1) {
        console.log(`[RED ] expected 1 call, got ${r.calls}`);
        red = true;
        continue;
      }
      await evaluate(
        `window.passwordChangeHarness.reject(${JSON.stringify(error)})`,
      );
      await waitFor(
        "document.querySelector('[role=alert]')?.textContent?.trim()",
        message,
      );

      const restored = await evaluate(`(() => {
        const form = document.querySelector('form');
        const buttons = [...document.querySelectorAll('button')];
        return Boolean(form) && buttons.every(button => !button.disabled) &&
          buttons.some(button => button.textContent?.includes('Change password'));
      })()`);
      if (!restored) {
        console.log("[RED ] interaction not restored after failure");
        red = true;
        continue;
      }

      await evaluate("document.querySelector('form')?.requestSubmit()");
      await waitFor("window.passwordChangeHarness.calls", 2);
      const lockedAgain = await evaluate(
        "document.querySelector('button[type=submit]')?.disabled === true",
      );
      if (!lockedAgain) {
        console.log("[RED ] submit not disabled after re-submit");
        red = true;
        continue;
      }
      await evaluate("window.passwordChangeHarness.resolve()");
      console.log(`[GREEN] ${message}`);
    }

    if (red) {
      console.log("\nRESULT: RED — some checks failed");
      return 1;
    }
    console.log("\nRESULT: GREEN — all checks passed");
    return 0;
  } finally {
    ws?.close();
    chrome.kill();
    stopProcessTree(vite);
    removeBrowserProfile(profile);
  }
}

process.exit(await main());

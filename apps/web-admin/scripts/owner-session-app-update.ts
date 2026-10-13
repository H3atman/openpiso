import type { AppUpdateState } from "../src/lib/types";

export interface UpdateFixture {
  current: string;
  newer: string;
  compatible: string;
  incompatible: string;
  schema: number;
}

interface Browser {
  call(method: string, params?: object): Promise<unknown>;
  evaluate<T>(expression: string): Promise<T>;
  waitFor(expression: string, label: string): Promise<void>;
  click(selector: string): Promise<void>;
  typeInto(selector: string, text: string): Promise<void>;
  screenshot(name: string): Promise<void>;
  check(name: string, ok: boolean, detail?: string): void;
  state(): Promise<AppUpdateState>;
}

/** Real Owner UI and HTTP, stopping at the browser's installer request boundary. */
export async function verifyAppUpdate(
  browser: Browser,
  fixture: UpdateFixture,
): Promise<void> {
  const { call, evaluate, waitFor, click, typeInto, screenshot, check, state } =
    browser;
  const panel = '[data-testid="app-update-panel"]';
  const row = '[data-testid="app-update-row"]';
  const button = (label: string) =>
    `Array.from(document.querySelectorAll('${panel} button')).find(b => b.textContent.trim() === ${JSON.stringify(label)})`;
  const press = async (label: string) => {
    await waitFor(`Boolean(${button(label)})`, label);
    await evaluate(
      `(${button(label)}).setAttribute('data-e2e-update-action', ''), true`,
    );
    await click("[data-e2e-update-action]");
    await evaluate(
      `document.querySelectorAll('[data-e2e-update-action]').forEach(b => b.removeAttribute('data-e2e-update-action')), true`,
    );
  };
  const enabled = (label: string) =>
    `Boolean(${button(label)}) && !(${button(label)}).disabled`;

  await waitFor(
    `document.body.innerText.includes(${JSON.stringify(`Updated to v${fixture.current}`)})`,
    "simulated restart success toast",
  );
  check("App Update: simulated restart success toast", true);
  await screenshot("update-1-restart-toast.png");
  await waitFor(
    `document.querySelector(${JSON.stringify(row)})?.textContent.includes(${JSON.stringify(`Update available · v${fixture.newer}`)})`,
    "update sidebar row",
  );
  check("App Update: sidebar offers newer release", true);
  await waitFor(
    `!document.querySelector(${JSON.stringify(panel)})`,
    "panel initially closed",
  );
  const point = await evaluate<{ x: number; y: number }>(
    `(() => { const r = document.querySelector(${JSON.stringify(row)}).getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2}; })()`,
  );
  await call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
  await waitFor(
    `Array.from(document.querySelectorAll('[data-slot="tooltip-content"]')).some(t => t.textContent.includes('Fixture update summary'))`,
    "hover release summary",
  );
  check("App Update: hover summary", true);
  await screenshot("update-2-hover-summary.png");
  await click(row);
  await waitFor(
    `document.querySelector(${JSON.stringify(panel)})?.getAttribute('role') === 'dialog'`,
    "App Update Sheet",
  );
  check(
    "App Update: Sheet accessible name",
    await evaluate<boolean>(
      `document.getElementById(document.querySelector(${JSON.stringify(panel)}).getAttribute('aria-labelledby'))?.textContent.trim() === 'App Update'`,
    ),
  );
  await waitFor(enabled("Check now"), "restart acknowledgement complete");
  const text = await evaluate<string>(
    `document.querySelector(${JSON.stringify(panel)}).innerText`,
  );
  check(
    "App Update: panel installed version and schema",
    text.includes(
      `Installed: v${fixture.current} · Schema Version ${fixture.schema}`,
    ),
  );
  check(
    "App Update: full sanitized Markdown notes",
    await evaluate<boolean>(
      `document.querySelector('[data-testid="app-update-notes"] strong')?.textContent === 'Full fixture notes' && !document.querySelector('[data-testid="app-update-notes"] script')`,
    ),
  );
  check("App Update: restart result acknowledged", !(await state()).lastResult);
  check("App Update: automatic checks default on", (await state()).autoCheck);
  await click("#app-update-auto-check");
  await waitFor(
    `document.querySelector('#app-update-auto-check')?.getAttribute('aria-checked') === 'false'`,
    "automatic checks disabled",
  );
  check(
    "App Update: automatic checks persisted off",
    !(await state()).autoCheck,
  );
  await press("Check now");
  await waitFor(enabled("Check now"), "manual check complete");
  check(
    "App Update: Check now works with automatic checks off",
    Boolean((await state()).lastCheckedAt) && !(await state()).lastError,
  );
  await screenshot("update-3-panel.png");

  await press("Install");
  check(
    "App Update: schema acknowledgement blocks install",
    !(await evaluate<boolean>(enabled("Install and restart"))),
  );
  check(
    "App Update: schema warning shown",
    await evaluate<boolean>(
      `document.querySelector(${JSON.stringify(panel)}).innerText.includes('cannot roll back')`,
    ),
  );
  await waitFor(
    `!document.querySelector('[data-sonner-toast]')`,
    "transient toasts dismissed for gate screenshots",
  );
  await screenshot("update-4-schema-gate.png");
  await click(`${panel} input[type="checkbox"]`);
  await waitFor(
    enabled("Install and restart"),
    "schema acknowledgement enables install",
  );
  check("App Update: schema acknowledgement enables install", true);
  // Never submit the final action: production installation is outside this E2E.
  await press("Cancel");
  await press("Compatible Rollback");
  await waitFor(
    `Boolean(${button(`Roll back to v${fixture.compatible}`)})`,
    "compatible rollback list",
  );
  const rollbackState = await state();
  check(
    "App Update: only same-schema older release offered",
    rollbackState.rollbacks.length === 1 &&
      rollbackState.rollbacks[0].version === fixture.compatible,
  );
  check(
    "App Update: lower-schema and newer releases hidden from rollback UI",
    !(await evaluate<boolean>(
      `Boolean(${button(`Roll back to v${fixture.incompatible}`)}) || Boolean(${button(`Roll back to v${fixture.newer}`)})`,
    )),
  );
  await press(`Roll back to v${fixture.compatible}`);
  check(
    "App Update: empty typed confirmation blocks rollback",
    !(await evaluate<boolean>(enabled("Roll back and restart"))),
  );
  await typeInto("#app-update-confirm-version", `v${fixture.compatible}`);
  check(
    "App Update: mismatched typed confirmation blocks rollback",
    !(await evaluate<boolean>(enabled("Roll back and restart"))),
  );
  await evaluate(
    `(${button("Roll back and restart")}).scrollIntoView({block:'center'}), true`,
  );
  await screenshot("update-5-rollback-gate.png");
  await typeInto("#app-update-confirm-version", fixture.compatible);
  await waitFor(
    enabled("Roll back and restart"),
    "exact typed confirmation enables rollback",
  );
  check("App Update: exact typed confirmation enables rollback", true);
  await evaluate(
    `(${button("Roll back and restart")}).scrollIntoView({block:'center'}), true`,
  );
  await screenshot("update-6-rollback-confirmed.png");
  await press("Cancel rollback");
  await call("Input.dispatchKeyEvent", {
    type: "keyDown",
    key: "Escape",
    code: "Escape",
  });
  await call("Input.dispatchKeyEvent", {
    type: "keyUp",
    key: "Escape",
    code: "Escape",
  });
  await waitFor(
    `!document.querySelector(${JSON.stringify(panel)})`,
    "App Update Sheet closed",
  );
}

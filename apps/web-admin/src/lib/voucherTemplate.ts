export const VOUCHER_TOKENS = [
  "code",
  "time",
  "points",
  "price",
  "shop_name",
  "expires_at",
  "prefix",
] as const;

export type VoucherToken = (typeof VOUCHER_TOKENS)[number];
export type VoucherTokenValues = Partial<Record<VoucherToken, string>>;

export interface VoucherTemplateContent {
  html: string;
  css: string;
}

const TOKEN_PATTERN =
  /{{(code|time|points|price|shop_name|expires_at|prefix)}}/g;

/** How long a render may go without reporting `ready` or `error`. */
export const RENDER_TIMEOUT_MS = 3000;
/** Wait after `document.fonts.ready` so late async errors are still caught. */
const SETTLE_MS = 500;
/**
 * Elements removed from every card; none can be allowed to run, load or hide
 * markup. `link` goes too: CSP blocks every link load, but a full Chrome still
 * honours `rel=preconnect`, which leaks the voucher code through the host name.
 */
const FORBIDDEN_CARD_ELEMENTS = "script, meta, base, link, noscript, template";

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/** Pure, escaped substitution for the Voucher Template's supported tokens. */
export function substituteVoucherTokens(
  template: string,
  values: VoucherTokenValues,
): string {
  return template.replace(TOKEN_PATTERN, (_token, key: VoucherToken) =>
    escapeHtml(values[key] ?? ""),
  );
}

/**
 * Parses one card into an inert `<template>` (no script runs, nothing loads),
 * removes every forbidden element and serialises it back. The serialised form
 * is balanced, so a card can no longer swallow the rest of the document. Needs
 * a real browser parser; the E2E corpus covers it, jsdom would pass every bypass.
 */
function cleanCard(cardHtml: string): string {
  const inert = document.createElement("template");
  inert.innerHTML = cardHtml;
  inert.content.querySelectorAll(FORBIDDEN_CARD_ELEMENTS).forEach((element) => {
    element.remove();
  });
  return inert.innerHTML;
}

function randomNonce(): string {
  return btoa(
    String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))),
  );
}

/** Records the first error or unhandled rejection for the tail to report. */
const BOOTSTRAP = `window.__voucherError = null;
const report = (message) => { window.__voucherError ??= String(message); };
addEventListener("error", (event) => report(event.message || "Script error"));
addEventListener("unhandledrejection", (event) => report("Unhandled rejection: " + (event.reason?.message ?? event.reason)));`;

function tail(print: boolean): string {
  return `document.fonts.ready.then(() => setTimeout(() => {
  if (window.__voucherError) {
    parent.postMessage({ type: "error", message: window.__voucherError }, "*");
    return;
  }
  parent.postMessage({ type: "ready", height: Math.ceil(document.documentElement.getBoundingClientRect().height) }, "*");
  ${
    print
      ? `addEventListener("afterprint", () => parent.postMessage({ type: "afterprint" }, "*"));
  print();`
      : ""
  }
}, ${SETTLE_MS}));`;
}

export interface BuildOptions {
  /** The frame calls its own `print()` once ready. */
  print?: boolean;
  /** Fixed nonce for tests; defaults to a fresh random one per render. */
  nonce?: string;
}

/** Thrown when a card cannot be rendered safely; shown to the Owner as-is. */
export class VoucherRenderError extends Error {}

/**
 * Creates the owner-only preview/print document for the sandboxed render frame.
 * Cleaning applies to the cards only; the document's own scripts are nonced.
 * Throws {@link VoucherRenderError} if cleaning removed a card's voucher code.
 */
export function buildVoucherDocument(
  template: VoucherTemplateContent,
  cards: VoucherTokenValues[],
  { print = false, nonce = randomNonce() }: BuildOptions = {},
): string {
  const css = template.css.replace(/<\/style/gi, "<\\/style");
  const fragments = cards.map((values) => {
    const filled = substituteVoucherTokens(template.html, values);
    const cleaned = cleanCard(filled);
    const code = escapeHtml(values.code ?? "");
    if (code && filled.includes(code) && !cleaned.includes(code))
      throw new VoucherRenderError(
        `A card lost its voucher code ${values.code} while the template was cleaned. Check the Card HTML.`,
      );
    return cleaned;
  });
  return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; script-src 'nonce-${nonce}'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'">
<script nonce="${nonce}">${BOOTSTRAP}</script>
<style>${css}</style>
</head>
<body><main class="voucher-grid">${fragments.join("\n")}</main>
<script nonce="${nonce}">${tail(print)}</script>
</body>
</html>`;
}

export type RenderResult =
  | { type: "ready"; height: number }
  | { type: "error"; message: string }
  | { type: "timeout"; message: string };

interface FrameMessage {
  type?: unknown;
  height?: unknown;
  message?: unknown;
}

/**
 * Loads `srcdoc` into a sandboxed frame and reports the first `ready` or
 * `error` (final), or a timeout. Only messages from the frame's own window are
 * accepted: its origin is "null", so `event.source` is the only identity check.
 * Returns a disposer that stops listening.
 */
export function runVoucherFrame(
  frame: HTMLIFrameElement,
  srcdoc: string,
  handlers: {
    onResult: (result: RenderResult) => void;
    onAfterPrint?: () => void;
  },
): () => void {
  let settled = false;
  const stop = () => {
    window.removeEventListener("message", onMessage);
    window.clearTimeout(timer);
  };
  const settle = (result: RenderResult) => {
    if (settled) return;
    settled = true;
    handlers.onResult(result);
  };
  const onMessage = (event: MessageEvent<FrameMessage | null>) => {
    if (event.source !== frame.contentWindow) return;
    const data = event.data;
    if (data?.type === "ready" && typeof data.height === "number")
      settle({ type: "ready", height: data.height });
    else if (data?.type === "error")
      settle({ type: "error", message: String(data.message) });
    else if (data?.type === "afterprint" && settled) handlers.onAfterPrint?.();
  };
  const timer = window.setTimeout(
    () =>
      settle({
        type: "timeout",
        message: `Render timed out after ${RENDER_TIMEOUT_MS / 1000} seconds: the template did not finish rendering. Check the Card HTML for unclosed or broken markup.`,
      }),
    RENDER_TIMEOUT_MS,
  );
  window.addEventListener("message", onMessage);
  frame.srcdoc = srcdoc;
  return stop;
}

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

function stripScripts(value: string): string {
  return value
    .replace(/<script\b[^>]*>[\s\S]*?<\/script\s*>/gi, "")
    .replace(/<script\b[^>]*\/?\s*>/gi, "")
    .replace(/<\/script\s*>/gi, "");
}

/**
 * Creates the owner-only preview/print document. The caller also sandboxes the
 * iframe; CSP and script stripping provide defense in depth for no-JS v1.
 */
export function buildVoucherDocument(
  template: VoucherTemplateContent,
  cards: VoucherTokenValues[],
): string {
  const css = template.css.replace(/<\/style/gi, "<\\/style");
  const fragments = cards
    .map((values) =>
      stripScripts(substituteVoucherTokens(template.html, values)),
    )
    .join("\n");
  return stripScripts(`<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; script-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'">
<style>${css}</style>
</head>
<body><main class="voucher-grid">${fragments}</main></body>
</html>`);
}

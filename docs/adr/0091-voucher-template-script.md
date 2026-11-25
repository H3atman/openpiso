# ADR-0091: Voucher Template Script

- Status: Accepted
- Date: 2026-11-26
- Deciders: shop owner (product)
- Supersedes: ADR-0041's "JavaScript does not execute" decision, in part. The
  rest of ADR-0041 (one shop-wide template, token substitution, owner-only
  rendering, reset-to-default) stands.
- Relates to: ADR-0024 (web-admin sole owner surface)

## Context

Some card layouts need per-card logic CSS cannot express (for example, colour by
price). ADR-0041 refused JavaScript to keep the browser security surface small.
The Voucher Template is already trusted Owner-authored content rendered only in
the Owner's browser, so the question is how much a script can reach, not whether
the Owner is trusted.

## Decision

The **Voucher Template** gains an optional third part, the **Template Script**
(JavaScript text), stored as `settings` key `voucher_template_script`, written
in the same SQLite transaction as the HTML and CSS. The owner endpoints carry
`html`, `css`, `script` and `default_html`, `default_css`, `default_script`
(the price-tier default script, see below). An empty script is valid; blank HTML or CSS is still rejected.
Each part is limited to 100,000 bytes.

**Render.** The script text is escaped (`</script` → `<\/script`,
`<!--` → `<\!--`) and inserted as a nonced `<script>` after every card and before
the bootstrap tail. It runs once per document, in preview and in print. Tokens
are never substituted into it; it reads card data from the cards
(`data-price="{{price}}"`). The preview runs it only when the Owner clicks **Run**
(so a half-typed loop cannot hang the tab); HTML/CSS edits still update live,
using the script as of the last Run. Print always runs the saved script.

**Isolation model.** Unchanged from ADR-0041's frame: a sandboxed iframe
(`allow-scripts allow-modals`, no `allow-same-origin`) with a nonce-only CSP
(`default-src 'none'`, `script-src 'nonce-…'`). The script gets an opaque origin,
so it cannot reach the Owner Session, cookies, storage or the owner API.

**Trusted-Owner model.** The Template Script is trusted Owner code. Only the
Owner (Web Admin PIN holder) can edit it, and the editor says plainly that a
Template Script can send voucher codes off this PC.

**Errors.** Sync errors, unhandled rejections (collected for a settle window
after `document.fonts.ready`) and a render that never finishes (parent-side
timeout) are shown under the preview and block print.

## Accepted risks

- A script can navigate its own frame and send voucher codes to any host. This
  is inherent in running Owner code at all; the warning is the mitigation.
- The parent cannot detect frame self-navigation: a navigated frame could post a
  fake `ready`. Accepted under the trusted-Owner model.
- An async error that fires after the settle window (about 500 ms past
  `fonts.ready`) is not reported and does not block print.
- Escaping `<!--` breaks a `/u` regex that contains it (reported as a script
  error, so print is blocked rather than silently wrong).
- Card HTML can still stop the script silently (for example a dangling
  `<script`, or an unclosed foreign-content element) while `ready` posts.

## Residual deny-list risk

Card HTML is cleaned by removing a fixed list of elements (`script`, `meta`,
`base`, `link`, `noscript`, `template`). A deny-list can miss a parser quirk;
cards cannot be allowed to run, load or hide markup. Because Template Script
makes frame-level exfiltration available to the Owner anyway, a bypass does not
widen what the Owner could already do, but it matters for the no-script HTML/CSS
corpus the harness still runs. New quirks are added to the list and the corpus.

## Consequences

- ADR-0041 is superseded in part; its "no JavaScript" statements no longer hold.
- The default script tiers each card by price (`data-tier`); it parses the price
  strictly, so a malformed price such as `₱50..20` gets no tier.
- Verified by the Voucher render frame harness (save/reload/reset, Run, each
  error kind blocking print).

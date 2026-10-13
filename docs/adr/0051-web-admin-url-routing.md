# ADR-0051: Web Admin URL routing and last-route restore

- Status: Accepted
- Date: 2026-07-29
- Deciders: shop owner (product)
- Relates to: [ADR-0024](0024-web-admin-sole-owner-surface.md) (Web Admin as
  the sole owner surface), [ADR-0050](0050-web-admin-pin-rotation.md) (Web
  Admin PIN rotation)

## Context

ADR-0024 made the Web Admin the sole owner surface, but every screen still
shared `/admin`: component state, rather than the address bar, selected the
page. Refresh lost the owner's place, Back and Forward could not navigate
inside the app, and screens could not be bookmarked, shared, or opened in a new
tab.

Two internal Settings identifiers had also drifted from owner-facing language:
`economy` labelled a **Points** section, and `owner-pin` labelled a Security
section that governs both the **Owner PIN** and the **Web Admin PIN**, whose
rotation is governed by ADR-0050.

## Decision

### The URL is authority

The URL names the screen and the app renders what the URL says. All screens
live beneath the app's `/admin/` base:

| Path | Screen |
| --- | --- |
| `/admin/dashboard` | Dashboard |
| `/admin/vouchers` | Vouchers |
| `/admin/adjustments` | Adjustments |
| `/admin/transfers` | Transfers |
| `/admin/accounts` | Accounts |
| `/admin/reports` | Reports |
| `/admin/audit` | Audit |
| `/admin/settings/points` | Settings → Points |
| `/admin/settings/wheel` | Settings → Wheel |
| `/admin/settings/coin-device` | Settings → Coin Device |
| `/admin/settings/shop` | Settings → Shop |
| `/admin/settings/pcs` | Settings → PCs |
| `/admin/settings/security` | Settings → Security |
| `/admin/settings/appearance` | Settings → Appearance |

The app uses real History API paths, not hash fragments. The Admin server
already serves the app shell for extensionless paths beneath `/admin`, making
deep links safe without a server change. This creates an ongoing dependency on
that fallback behaviour, so a server test pins nested extensionless paths to
the app shell while missing concrete assets remain real 404s.

Routing is hand-written. The route table is flat and closed: it has no dynamic
parameters, nested layouts, or code-splitting need in a LAN-local app. A small
owned router is preferable to adding a production dependency while the Svelte
5 path-router ecosystem is immature.

### Settings identifiers match their meaning before URLs ship

Rename `economy` to `points`, conforming to the glossary's **Points**, and
`owner-pin` to `security`, reflecting that the section governs both the
**Owner PIN** and the **Web Admin PIN**. These names land before the URL scheme
ships because bookmarkable URLs become permanent identifiers. No legacy aliases
are added: neither old slug has shipped as a URL, so there is no compatibility
contract to preserve.

### Resolution preserves useful navigation

Bare `/admin/settings` resolves to the default Points section and replaces the
current history entry rather than pushing one, so Back does not bounce through
the redirect. An unknown Settings section resolves the same way: within
Settings, the useful response is a valid default alongside the submenu that
shows every section that does exist.

An unknown top-level path instead renders a Not-found view inside the sidebar
shell. The owner is already authenticated and must retain navigation rather
than being silently moved or stranded outside the owner surface.

### Login is a gate, not a route

Login remains a conditional gate over the whole app and has no URL of its own.
While unauthenticated, the URL is untouched. A deep link therefore survives
login and renders immediately after authentication without a redirect,
round-trip, or `next` parameter. This applies equally when the credential is
the rotatable **Web Admin PIN** governed by ADR-0050.

### Last-route restore is only a default for an empty URL

The app stores the last screen path with a timestamp and reads it only when the
URL names no screen: the bare app root. A URL naming any screen always wins.
The stored route expires after 24 hours, a shop-day boundary that supports
resuming work within a shift without carrying yesterday's context into a new
one. Missing, malformed, expired, or no-longer-valid values fall back to
Dashboard.

Logout does not clear the value. It is a screen name rather than shop data, and
clearing it would defeat the resume flow. The accepted consequence is that a
borrowed machine retains that screen name, but no shop data, until the 24-hour
expiry.

## Considered and rejected

- **Component state as screen authority:** refresh, bookmarks, shared links,
  new tabs, and browser history remain broken. Rejected in favour of the URL.
- **Hash-fragment routing:** avoids dependence on server fallback behaviour but
  produces second-class URLs despite the Admin server already supporting real
  paths. Rejected; the accepted server dependency is pinned by a test.
- **A routing library:** unnecessary for a flat, closed table with no dynamic
  parameters, nested layouts, or code splitting, and adds dependency risk in
  an immature Svelte 5 router ecosystem. Rejected in favour of a small
  hand-written router.
- **Shipping `economy` and `owner-pin`, or preserving them as aliases:** would
  make drifted internal names permanent despite no released URLs requiring
  compatibility. Rejected in favour of `points` and `security` before launch.
- **Pushing bare-Settings resolution into history:** Back would bounce through
  a route that names no usable section. Rejected in favour of replacement.
- **Treating every unknown route alike:** silently defaulting unknown top-level
  paths hides mistakes, while a Not-found page for an unknown Settings section
  is less useful than the existing section menu. Rejected in favour of distinct
  top-level and Settings resolution rules.
- **A dedicated login route with redirect state or a `next` parameter:** adds a
  round-trip and risks discarding deep-link intent. Rejected in favour of an
  URL-preserving conditional gate.
- **Letting stored state win over the URL:** would silently override every
  bookmark and shared link. Rejected; storage is consulted only at the bare
  app root.
- **Clearing the stored route on logout:** prevents the motivating resume flow.
  Rejected; a bounded screen-name trace is accepted.
- **Permanent last-route storage:** leaves stale context on shared machines.
  Rejected in favour of the 24-hour shop-day expiry.

## Consequences

- Refresh, Back and Forward, bookmarks, shared links, and native new-tab
  gestures address the same screen consistently.
- URL slugs use the existing domain vocabulary, including **Points**, and keep
  the **Owner PIN** distinct from the **Web Admin PIN**.
- The Web Admin depends on the Admin server continuing to serve its shell for
  nested extensionless `/admin/` paths; a test makes regressions explicit.
- Unknown top-level URLs retain the authenticated sidebar, while invalid or
  bare Settings URLs resolve to Points without polluting browser history.
- Last-route restore improves bare-root landing without weakening URL
  authority; a borrowed machine can retain only a screen name for at most 24
  hours.

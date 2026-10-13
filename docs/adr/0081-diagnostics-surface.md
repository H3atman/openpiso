# ADR-0081: One gated Diagnostics page holds every piece of evidence about the running system

- Status: Accepted
- Date: 2026-09-09
- Deciders: shop owner (product)
- Amends: [ADR-0078](0078-support-bundle.md) — the bundle's owner surface moves
  from a Support section under Settings to the top-level Diagnostics page; the
  bundle itself, its transport, its redaction stance, and its no-step-up rule
  are unchanged.
- Relates to: [ADR-0016](0016-diagnostic-logging.md) (the rotating diagnostic
  files and ~7-day retention this page reports on and never stores),
  [ADR-0024](0024-web-admin-sole-owner-surface.md) (the Web Admin as the sole
  owner surface this page belongs to),
  [ADR-0043](0043-coin-ingestion-sampling-and-observability.md) (the Coin Debug
  Bundle and Coin Debug Window the Coin tab holds),
  [ADR-0051](0051-web-admin-url-routing.md) (the authoritative URLs, the
  redirect rule, and the 24-hour last-route restore this page rides),
  [ADR-0065](0065-phone-designed-vs-phone-tolerable-owner-screens.md) and
  [ADR-0067](0067-paged-phone-lists-and-mobile-settings-navigation.md) (the
  phone-tolerable screen class and the mobile Settings navigation the retired
  Support section leaves), and
  [ADR-0082](0082-admin-run-records.md) (the Run Records, Exit Reasons, and
  panic evidence the Server tab displays).

## Context

Before this decision, the evidence an owner needed during a problem was
scattered across Settings and partly nonexistent:

- The Support Bundle and the Diagnostic Request lived in Settings → Support.
- The Coin Debug Bundle lived in that same Support section while the Coin
  Debug Window controls lived in Settings → Coin Device — one investigation,
  two pages.
- Admin's own health — its Run Records, Exit Reasons, and panic file — had no
  owner-facing display anywhere, though ADR-0082 (decided in the same session)
  now produces that evidence.

Settings was the wrong home for all of it. Settings is where the owner goes to
change how the shop works; gathering evidence about what the system did is a
read-only task of a different shape. And a surface needed a handful of times a
year should not sit in the daily furniture competing for attention.

The open question was whether the new page should be always visible. A
diagnostics surface read once every few weeks is invisible on an unused day and
one click too deep on the day it matters, and hiding evidence looks — to a
future reader — like a security decision, which it must not be. That tension
produced the flag this ADR is primarily about.

## Decision

### One top-level page, three tabs

The Web Admin gains **Diagnostics**, a new top-level page beside dashboard,
reports, audit, backups, and notifications — rendered in the sidebar's System
group directly after Settings. It holds, as tabs in this order:

- **Client** — the Support Bundle (PC and window picker, inline preview,
  download) and the Diagnostic Request. Client first because it is today's
  most-used path.
- **Coin** — the Coin Debug Bundle and the Coin Debug Window controls,
  consolidating the split placement above into one tab.
- **Server** — Admin's Run Records, their Exit Reasons, and panic evidence,
  the display ADR-0082's records needed.

It is one route — `/admin/diagnostics` — with tabs, never sub-routes: the URL
names the page, not the tab, so browser Back does not walk the owner through
tab history. It holds no configuration — Coin Device settings, the Pulse Map,
Coin Calibration, and Acceptor Health stay in Settings → Coin Device. The page
reports; Settings configures, and the split is deliberate.

### Diagnostics Mode is a visibility flag and nothing else

The page is hidden until the owner enables **Diagnostics Mode**:

- It is a shop-wide, SQLite-persisted setting — a new single-row table,
  following this repo's convention of one table per setting — defaulting to
  off. The toggle lives in Settings → Security, is read and written through
  the Web Admin HTTP API under an Owner Session, and every change is audited,
  so months later it is answerable who revealed the surface and when.
- While on, the Diagnostics page carries a standing inline advisory naming it
  as on, with the toggle as the way off.

The load-bearing rule, stated so nobody later turns the flag into something
more: **Diagnostics Mode changes visibility only.** Admin and the Client
capture identical evidence whether it is on or off — the rotating files, the
Forwarded Diagnostics, the Support Bundle ingredients, and Run Records are all
produced unconditionally. No capture, verbosity, or retention behavior anywhere
branches on the flag. An incident is therefore explainable after the fact
without anyone having reproduced it with a switch flipped; had the flag gated
capture, yesterday's evidence would depend on yesterday's toggle state, which
is exactly the failure this design refuses.

It is also not a security boundary. The diagnostics endpoints — the Support
Bundle, the Diagnostic Request, the Coin Debug Bundle and Window, and the host
diagnostics — serve any authenticated Owner Session whether the flag is on or
off, with no PIN step-up: the surface is redacted and read-only (ADR-0078), and
step-up is reserved for authority actions. Hiding a page is not protecting one,
and the flag is not a permission.

### The Support section retires

The Settings **Support** section is retired — its section entry, its
component, and its slot in the mobile Settings section index go with it
(ADR-0067's collapsible mobile Settings navigation itself is untouched). It is
retired rather than kept alongside because two surfaces for the same evidence
drift: the first move costs a redirect, the second costs a divergence. Nothing
in the Support section is lost — its contents move whole to the Client tab.

The retirement is complete at the routing layer (ADR-0051):

- `/admin/settings/support` redirects unconditionally to the Diagnostics
  route, independent of the flag, handled before Settings' unknown-section
  fallback so a stale link does not land somewhere unrelated.
- The Diagnostics URL always resolves: with the flag off the page renders a
  stub naming Settings → Security as the place to enable it, so a link sent by
  support instructs instead of failing.
- Settings → Coin Device gains a pointer to Diagnostics rendered only while
  the flag is on — the coin evidence tools that used to live beside it moved,
  and an off flag must not advertise a page the owner cannot open.

The page itself is phone-tolerable, not phone-designed (ADR-0065): copying a
bundle must work on a phone, because that is when support is on the other end;
detailed readouts may stay desktop-comfortable.

### The accepted cost: every support call starts with a toggle

With the surface fully gated, every support call now begins with "Settings →
Security, enable Diagnostics." That step is accepted deliberately: it buys a
quiet everyday Web Admin at the price of one extra step a few times a year. No
escape hatch is provided — no always-visible bundle button outside the gated
page — because a second, less-capable path to the same evidence would drift
from the first, the same reason the Support section could not stay.

## Considered options

- **Keep the Support section alongside the new page.** Rejected: two surfaces
  for the same evidence drift apart; the redirect is cheaper than the
  divergence.
- **Let Diagnostics Mode gate capture or verbosity.** Rejected: it would make
  after-the-fact explainability depend on a toggle having been on at the time
  — the precise problem this surface exists to solve.
- **Treat the flag as a security boundary and gate the endpoints on it.**
  Rejected: the endpoints are read-only and redacted and already require an
  Owner Session; gating them on a convenience flag would add a boundary that
  protects nothing and quietly changes evidence availability.
- **A per-browser preference instead of a shop-wide setting.** Rejected: the
  owner must be able to reveal the surface from the phone while standing at
  the shop PC, and storage in SQLite is what makes one flip mean everywhere.
- **An auto-off timer for the flag.** Rejected in favor of manual control plus
  the standing advisory; the flag's whole job is to be a deliberate act.
- **Sub-routes per tab.** Rejected: Back would walk tab history, and the tab
  is a view over one page, not a place.

## Consequences

- Every support workflow gains one fixed first step: enable Diagnostics Mode.
  Documentation for support procedures must state it, and the disabled stub is
  designed to state it too.
- [ADR-0078](0078-support-bundle.md)'s "a Support section under Settings"
  placement moves to the top-level Diagnostics page; its bundle behavior does
  not. The host-diagnostics endpoint and the Diagnostics Mode get/set pair
  reach `docs/api.md` with the tickets that implement them.
- One new migration and one new single-row settings table exist; the flag
  touches no money, time, Points, or session data and grants no authority.
- The four glossary entries — Diagnostics, Diagnostics Mode, Run Record, Exit
  Reason — are the vocabulary for this area. The surface is **Diagnostics**
  everywhere — page, URL, code, and logs; "debugging" is not used.
- The Coin tab consolidation means a coin investigation no longer spans two
  Settings pages, and the Coin Device pointer only exists while the page it
  points to is openable.

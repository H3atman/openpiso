# ADR-0026: Central Terminal kiosk grid UI

- Status: Accepted
- Date: 2026-06-28
- Deciders: shop owner (product)
- Refines: ADR-0023 (serial coin input and the terminal selection lock), ADR-0025 (selection-commit flow).

## Context

ADR-0023 established the Central Terminal Selection Window as the server-owned
customer surface. ADR-0025 refined the numpad-driven selection-commit flow and
moved the kiosk onto a `grid → insert → success` flow with a typed number on
the physical numpad. The on-screen rendering of the grid itself was
underspecified: the original UI showed a flat card grid that read the PC list
back from the server and gave every PC equal visual weight.

The grid is what the customer looks at while deciding which PC to pick. The
shop's two audiences — walk-up guests picking a free PC, and account customers
walking back to a specific seat — have different information needs, and the
flat card grid satisfied neither:

- A guest scanning for an available PC had to read the time on the clock, the
  status dot, and the timer mode (guest vs account) from three different parts
  of each card. Hard to scan at distance.
- An account customer looking for their seat (or the seat of a friend) had no
  way to see who was logged in at which PC without walking up to the machine.
- A customer who mistyped a PC number saw a generic "Pick a PC number first."
  toast and a stale number that stayed on screen until they manually cleared
  it.

We also prototyped the question "what should the grid look like?" through
six UI variants on a single route, switchable with `?variant=`. The variants
spanned the design space from a flat card mosaic (closest to the original)
to a flight-departure-board wall. The product decision was to commit to a
**terminal-display** layout (the "departures board" family) as the final
kiosk UI.

## Decision

### 1. Layout — terminal-display departures board

The grid screen has three regions, top to bottom:

1. A small **header** strip: the kiosk title on the left, and a pill on the
   right showing `N/M available` and the coin-acceptor health (green/rose
   dot).
2. A large **hero** panel (the bulk of the vertical space) that mirrors the
   customer's typed PC number. When nothing is typed, the hero shows a
   giant em-dash. When a number is typed and matches a PC, the hero shows
   the PC's status dot, the PC number at 12rem, and either a "Press
   Enter to open" affordance (for an available PC) or a destructive pill
   ("Terminal in use", "PC offline", "PC locked") for an unavailable one.
   Below the number: a "Time on clock" reading and, if the PC is in
   account mode, the account username.
3. A **5-column grid of every PC** in the roster. Each tile carries:
   - The PC number, the time on the clock, and the account username (or
     the mode label) — three lines of mono tabular numerals.
   - A **status-colored emphasized 2 px border** so the customer can
     scan the room from a distance: emerald for available, amber for
     in-use, rose for locked, zinc for offline.
   - The matched PC (the one the customer is currently typing) scales up
     by 4% and gets a primary-color ring.

The fixed 5-column grid was the product decision after evaluating the
layout at 4, 8, and 20 PCs. It scans cleanly in a single glance, fills
the kiosk's landscape aspect ratio without leaving awkward gaps, and is
dense enough to feel like a departures board without becoming noisy.

### 2. Auto-delete on unavailable PC (frontend behavior change)

The customer types a PC number on the numpad and presses Enter. If the
number is empty, references a non-existent PC, or references a PC whose
`status` is not `online`, the typed number is **auto-deleted**: the hero
flashes red, the screen shakes for ~180 ms, the typed number is cleared,
and a brief transient reason is shown ("PC 02 is offline", "Terminal
in use", "No PC 99 on this terminal."). The customer can start over
immediately — no stale number, no dismissable toast.

The availability check is `status === "online"` only. `remaining_seconds
=== 0` is the **normal** state for a heartbeating PC that's ready to
accept coins; it is not an unavailability signal (the kiosk's job is
to *add* time, not to sell existing time). `db.rs` derives `in_use` from
`remaining_seconds > 0` server-side, so a PC that already has time is
correctly surfaced as unavailable through the `status` field, not the
time field.

This replaces the prior behavior ("Pick a PC number first." toast,
typed number left on screen until the customer manually cleared it).
The auto-delete keeps the kiosk in a clean state for the next
customer, which matters when 20 strangers use the same screen in an
evening.

### 3. No on-screen Backspace / Clear buttons

The kiosk is a physical-numpad surface (ADR-0025). The numpad's
Backspace key and Escape key are the only commit/clear affordances.
On-screen Backspace and Clear buttons were removed because they
duplicated the physical keys, were never aimed at, and added a layer
of "is the screen a touch surface?" ambiguity in a kiosk that isn't
one.

## Considered options

- **Mosaic auto-fit grid (variant A).** Keeps the original card-grid
  structure but uses CSS `repeat(auto-fit, minmax(180px, 1fr))` so
  tiles grow to fill the screen. Rejected: the auto-fit density
  varies wildly between 4 and 20 PCs, and the per-PC information
  density is still too low (status dot + time + mode each on its
  own line, no border emphasis).
- **Numpad spotlight (variant B).** Big calculator-style display on
  the left, compact roster on the right. Rejected: the customer has
  to mentally map a single PC number to one cell in a 2-column list,
  which is slower than the departures-board approach.
- **Concourse / Boarding pass / Wall (variants D, E, F).** Each
  solved a different aspect of the problem (horizontal density,
  split layout, equal cards) but all lost either the hero
  affordance for the typed number or the kiosk feel of the 5-column
  grid. Kept as throwaway during the prototype, deleted at wrap-up.
- **Keep the original flat card grid.** Rejected for the reasons in
  context: the information hierarchy doesn't support a customer
  scanning the room for an available PC or a specific account.

## Consequences

- The frontend is now in `apps/admin/src/lib/components/kiosk/`
  (`grid.svelte` + `pc-tile.svelte`) and `apps/admin/src/lib/kiosk.ts`
  for the shared `isAvailablePc`, `pcDisplayNumber`, `statusTone`,
  and `unavailabilityReason` helpers. The throwaway `prototype/`
  directory that held the six variants is gone.
- The `?variant=` URL parameter and the floating prototype switcher
  are gone. The kiosk now ships one design.
- A future reader must not reintroduce the on-screen Backspace /
  Clear buttons — the kiosk is a numpad surface and the physical
  keys are the only commit/clear affordances (ADR-0025).
- A future reader must not "fix" the auto-delete back into a
  dismissable toast — the auto-delete is deliberate. It keeps the
  kiosk in a clean state and removes one action from the
  customer-facing flow.
- The hero panel is the canonical signal for what the customer is
  about to commit. Any new visual on the grid screen (a promo
  banner, a queue number, etc.) belongs in the header strip, not
  the hero, so the customer's commit decision is never crowded.

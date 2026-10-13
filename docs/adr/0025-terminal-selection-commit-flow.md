# ADR-0025: Central Terminal selection-commit flow

- Status: Accepted
- Date: 2026-06-28 (refined 2026-06-28: success screen removed, real-time Current-time clock)
- Deciders: shop owner (product)
- Refines: ADR-0023 (serial coin input and the terminal selection lock).
- Relates to: ADR-0008 (coin routing via a central terminal),
  ADR-0024 (web-admin sole owner surface).

## Context

ADR-0023 established the server-owned Terminal Selection and named `Enter` as
one of the ways a Selection Window closes, but left the on-screen interaction
underspecified. The original kiosk UI implemented it as a four-screen flow
(`grid → confirm → insert → success`) with an on-screen 12-key keypad, and its
insert-screen "Cancel" button only cleared the typed number locally — it did
**not** close the Selection Window, so cancelling left the terminal selection
open (and the Set Pin asserted) until the inactivity timeout. We also fixed the
input model: the Central Terminal is driven by a **physical numpad**, not a
touchscreen.

(Note: "claim/claiming" here is avoided deliberately — **Claim** is a reserved
glossary term for absorbing Guest Time into an account. The terminal verb is
**open**/**close** a Selection Window.)

## Decision

- **Open on Enter; no confirm screen.** The customer types a PC number on the
  numpad (a display shows the in-progress number — required once a shop has
  PC ≥ 10) and presses **Enter** to open a Selection Window for that PC. The
  separate confirm screen is removed; the flow is `grid → insert`.
- **The insert-screen Enter and Escape both close the Selection Window and
  return to the grid.** There is no success screen: per ADR-0023 each coin
  commits immediately, so by the time the customer presses Enter the time is
  already credited and visible on the grid's PC tile and on the insert-screen
  "Current time" display. The on-screen Done/Cancel button label is
  context-sensitive (Done if money has landed, Cancel otherwise) so the
  customer sees an honest "I changed my mind" affordance when nothing has
  been inserted, but both branches invoke the same `close` call.
- **Cancel must close the Selection Window.** Both the zero-coin Enter and
  `Escape` call `close` to free the terminal immediately, instead of clearing
  local state and waiting out the inactivity timeout. This fixes the prior bug
  where a cancelled selection kept the single physical acceptor busy (Set Pin
  asserted) for up to 15 s.
- **The "Current time" display on the insert screen is a real-time clock.** The
  server broadcasts `pc-updated` once per second for any active PC; the kiosk
  mirrors that value into a local display state and decrements it on a
  1-second local tick between broadcasts, so the on-screen clock reads as a
  smooth countdown (a coin insert that bumps the time snaps the clock back
  up before the tick resumes). The server remains the single source of truth
  for committed time — the local tick is purely cosmetic and never triggers
  any state change.
- **The Central Terminal is numpad/physical-keyboard driven.** The on-screen
  clickable keypad is removed; number entry, the typed-number display, and
  Backspace editing are kept. PC tiles become read-only status cards, not the
  selection mechanism.

## Considered options

- **Keep the confirm screen (re-skinned).** Preserves an explicit commit gate
  against misfires, but adds a step to every selection. Rejected: numpad number
  entry plus an explicit Enter is already a deliberate commit, and the
  close-on-zero-coin path bounds the cost of a wrong selection to one keystroke.
- **Keep a success screen summarizing the credited time.** Helpful for
  confirmation, but redundant: per ADR-0023 each coin commits immediately, so
  the running "Current time" on the insert screen and the PC tile on the grid
  both already show the credited time the moment the coin lands. The
  auto-returning success screen only added a brief interlude before the same
  information. Rejected.
- **Separate Cancel and Done buttons on the insert screen.** Clearer at a
  glance, but the terminal is a single-Enter numpad surface; one context-sensitive
  key matches the hardware and avoids a button the customer must aim at.

## Consequences

- This is a **behavior change**, not pure presentation: the selection-commit
  flow and the cancel/close semantics differ from the original UI. The Rust IPC
  surface, money/time authority, and Tauri capabilities are unchanged — the
  change is entirely in the frontend over the existing `open_selection` /
  `close_selection` commands.
- A future reader must not "fix" the missing success screen back in — the
  per-coin commit (ADR-0023) and the real-time "Current time" mirror make the
  success screen redundant, and its auto-return timer was a source of subtle
  race conditions with the `selection-state` listener.
- ADR-0023's "the window closes on `Enter`" is refined: `Enter` always closes
  the Selection Window (same as `Escape`) and the kiosk returns to the grid.
  The Done/Cancel label distinction is a frontend-only affordance that tells
  the customer an honest story about whether money has been inserted.

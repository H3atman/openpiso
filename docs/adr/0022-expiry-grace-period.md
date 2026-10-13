# ADR-0022: Expiry warning and shutdown grace period

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Resolves: the open "grace period / warning before power-off" item left in
  [ADR-0005](0005-tamper-response-and-expiry-powraoff.md)
- Superseded in part by [ADR-0034](0034-connected-zero-time-shutdown-and-channel-consistency.md):
  the "command shutdown only on an observed `> 0 → 0` transition" rule (see the
  *Grace phase* and *Authority and safety* sections below) is replaced — **any**
  connected PC at zero is commanded to shut down, `power_state` is a pure function
  of status + Remaining Time, and the command is delivered consistently across the
  heartbeat response and the SSE stream. The two-phase warning/grace UX itself
  stands.

## Context

ADR-0005 decided that a diskless PC **powers off** when paid time hits zero, so
a reboot wipes the previous customer's sessions for privacy. It explicitly left
the warning and grace-period user experience open: an immediate power-off at the
exact zero second would discard unsaved work and surprise customers, while an
unbounded grace would let a customer keep using a PC after their time ran out.

The money is already safe server-side (ADR-0001), so this decision is purely
about user experience and who is authoritative for the *decision* to power off.

## Decision

Two phases, both driven by server-authoritative state streamed on the PC's
`pc.updated` event as a `power_state` field (`active`, `warning`, `shutdown`):

### Warning phase (non-blocking)

While `0 < Remaining Time ≤ 120 seconds` the server sets `power_state` to
`warning`. The client shows a non-blocking low-time banner urging the customer
to top up at the terminal. Nothing is locked; the customer keeps working.

### Grace phase (blocking)

When a **connected** PC's authoritative time transitions from `> 0` to `0`, the
server commands `power_state = shutdown`. The client shows a blocking full-screen
countdown overlay using the owner-configured grace window, defaulting to
**300 seconds** ("Time's up — shutting down in N seconds. Insert coins to
continue."). If a top-up raises Remaining Time above zero during the grace
window, the server flips `power_state` back to `active` (or `warning`) and the
client cancels the shutdown. If the grace window elapses while the server still
commands shutdown, the **client process** performs a real Windows OS shutdown.

### Authority and safety

- The **server** owns the decision: it commands shutdown only on the observed
  `> 0 → 0` transition for a connected PC and cancels it the moment a top-up
  raises the balance. A freshly booted diskless PC that reports zero time (no
  prior positive observation in this connection) is **not** commanded to shut
  down, which prevents a reboot loop.
- The **client Rust process** performs the OS shutdown via the operating system
  shutdown command. Shutdown authority is never exposed to the Svelte frontend;
  the frontend only renders `power_state` and the grace countdown.
- **The power-off is forced (`/f`).** A graceful request lets Windows show the
  "these apps are preventing shutdown" screen, and a customer who picks
  *Cancel* keeps a running PC on free time. Same rationale as the Guard's
  Escalated Power Off (ADR-0077); the grace countdown, not Windows, is the
  customer's warning.
- **A power-off that did not take effect is issued again.** `shutdown.exe`
  reports only that it was spawned, so the Client records *when* it issued the
  power-off rather than *that* it did, and re-issues it once the machine is
  still running `SHUTDOWN_RETRY_SECONDS` later and the server still commands
  shutdown. A top-up still cancels the deadline and clears the record.
- **Release builds** perform the real power-off. **Debug builds** log the intent
  and forward a diagnostic instead of shutting down, so development machines are
  never powered off by a dev admin server.
- Shutdown command failures are forwarded to the admin diagnostic endpoint so a
  PC that fails to power off is visible to the owner.

## Accepted trade-offs

- The 120-second warning remains fixed. The shutdown grace defaults to 300
  seconds and is owner-configurable within a bounded range; this changes UX
  timing only and does not move shutdown authority into the frontend.
- A client that is briefly disconnected at the exact expiry second may reconnect
  showing the generic login screen without having received the shutdown command.
  This is acceptable: the owner sees the PC did not power off and the next login
  still burns server-side time. The arms race ends at the power button
  (ADR-0005).

## Consequences

- The PC `power_state` becomes part of the `pc.updated` contract.
- The client gains an in-process grace timer and an OS-shutdown path gated on
  server state and build profile.
- No schema migration is required; `power_state` is derived in-memory.

## Amendment (2026-09-23)

Onboarding defaults are now 300 s Shutdown Grace and 600 s Offline Grace.
Migration 0054 bumps only untouched old defaults; customized values remain.

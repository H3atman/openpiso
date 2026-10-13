# ADR-0023: Serial coin input and the terminal selection lock

- Status: Accepted
- Date: 2026-06-27
- Deciders: shop owner (product)
- Refines: ADR-0008 (coin routing via a central terminal).
- Relates to: ADR-0024 (`:8080/admin` owner surface).

## Context

ADR-0008 fixed *who* a coin credits (the PC's login state decides account vs
guest) but assumed the entry mechanism was an ESP32 posting `POST
/api/coin-events` with a `pc_id` in the payload. For v1 the hardware is
different and simpler: the coin acceptor wires directly into the **server
PC's COM port**. The acceptor emits pulses on one line (the **coin pin**) and
accepts an enable/inhibit signal on another (the **set pin**); it has no idea
which PC the customer picked. Something in the admin process must bridge "a
physical coin pulse" to "the PC the customer selected on screen," and a
physical acceptor swallows the coin *before* software can object — so an
unrouted coin must be impossible by construction, not handled after the fact.

## Decision

- **The admin Rust process reads the acceptor over the COM port directly.**
  No ESP32, no device HTTP path, no device-supplied `pc_id` in v1. The existing
  `POST /api/coin-events` becomes the simulator/legacy path only.
- **Coin entry is gated by a single, server-owned `TerminalSelection`.** It is
  a global singleton (`Option<TerminalSelection>`) because there is exactly one
  physical acceptor. Opening is allowed only when none is active; a second open
  is rejected ("terminal in use").
- **Selection-first lock via the Set Pin.** The Set Pin is **fail-closed**:
  inhibited at startup before serial init, whenever no window is open, on
  crash/restart, and on shutdown; asserted only while a Selection Window is
  open. This makes orphan coins impossible in normal operation. A train that
  started while asserted may straddle inhibition: its later edges are admitted
  while a pulse is already buffered and the full Coin Batch credits the
  just-selected PC during close drain. An edge with an empty buffer is refused
  as an Inhibited Pulse.
- **The Selection Window is UX only; each coin commits immediately** through the
  existing authoritative `record_coin_event` path. There is no pending/uncommitted
  value, so crash behaviour is safe and reporting is unchanged. The window closes
  on inactivity (default 15 s, reset per coin), `Enter`, or a hard cap (default
  180 s); all owner-editable.
- **Pulses → pesos by gap-batched mapping.** Pulses are grouped into a Coin
  Batch closed by a quiet inter-coin gap; a configurable pulse-count → cents
  map yields one coin's value → one coin transaction. The map, inter-coin gap,
  COM port, and which control/status line is the set/coin pin are owner-editable
  settings persisted in SQLite (calibrated on-site).
- **One `CoinSource` abstraction** with serial and simulator implementations,
  both feeding the same selection path; the simulator credits the active
  selection's PC, never an arbitrary `pc_id`.
- **Any configured, visible PC is selectable, even offline.** Coins to an
  offline PC preload its Guest Time (which already survives offline). Account
  routing is impossible without a live session, so offline always means Guest.

## Consequences

- Routing semantics from ADR-0008 are unchanged; only the entry mechanism and
  the new selection/lock state are added. ADR-0008's "the ESP32 payload carries
  `pc_id`" no longer holds for v1.
- `hardware.md` is rewritten for the COM-port wiring; the ESP32 notes move to a
  deferred-hardware appendix.
- The single-acceptor reality closes the door on concurrent terminals in v1.
- Server-side serial control (drive set pin, read coin-pin edges) is a new
  platform dependency on Windows COM APIs; the abstraction keeps it testable.

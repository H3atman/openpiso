# ADR-0008: Coin routing via a single central terminal

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0001 (time ownership), ADR-0003 (points).

## Context

OpenPiso is a *centralized* coinslot. In the MVP, the terminal flow was "pick a
PC, insert a coin, credit that PC" because time lived on the PC. Accounts
(ADR-0001) and points (ADR-0003) mean a coin can now land in two places, and
points exist only on accounts. The terminal must decide who it is crediting.

## Decision

- **One central terminal is the only place coins enter the shop.** There are no
  per-PC coin slots. Every peso flows through the single terminal (the admin
  "Insert Coin" interface, backed by the ESP32 coin device).
- **The customer selects a PC** at the terminal; they do **not** authenticate
  at the terminal.
- **The server routes the coin by that PC's current session state:**
  - PC has an **active account session** → credit that **account**: time **and**
    points (ADR-0003 earn rate).
  - PC is in **guest** state (no session) → credit the **PC** as guest time,
    with **zero points** (ADR-0003).
- The same physical action (pick PC-03, insert ₱20) therefore credits an
  account-with-points or a PC-without-points purely based on PC-03's login state
  at that instant. This is intended.

## Consequences / still open

- `coin_transactions` must record the resolved target: a nullable `account_id`
  alongside `pc_id`, plus `points_earned`. Routing is resolved server-side in
  the coin-event handler, never trusted from the device.
- **An account can only be credited while it is logged in at a PC.** There is
  no terminal path to pre-load an account that isn't seated, or to top up
  someone else's account. **Vouchers** are the expected mechanism for
  off-PC / gifted top-ups (see voucher ADR, not yet written).
- The ESP32 coin-event payload still carries `pc_id` + amount; the server does
  all account/points resolution.

> **Amended by [ADR-0023](0023-serial-coin-input-and-terminal-selection.md):**
> in v1 the acceptor is read directly over the server's COM port and the device
> supplies only pulses — the **terminal selection** supplies the `pc_id`, not
> the device payload. The routing rule above (server resolves account vs guest
> from the PC's live login state) is unchanged.

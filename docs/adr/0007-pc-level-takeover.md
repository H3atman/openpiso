# ADR-0007: PC-level takeover (two accounts, one PC)

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0002 (single active session), ADR-0006 (force re-login).

## Context

ADR-0006 forces a generic login screen when a crashed client restarts, even
though the prior account's session may still be live and burning on that PC.
This is the mirror of ADR-0002: two accounts contending for one PC.

## Decision

- The login screen after a crash is **generic** — any account can log in.
- A new account logging in at a PC **takes over** that PC: the prior account's
  session on that PC ends, and the **prior account keeps its full remaining
  balance** (time and points are portable, so it loses nothing but the seat).
  The new account's session then begins.
- Chosen trade-off: **seat liberation over seat protection.** A customer who
  walks away without logging out can lose the machine to the next person — but
  never loses time or points. The alternative (locking an abandoned PC to an
  absent customer until it powers off at 0) was rejected as worse for a busy
  shop.

## The complete displacement model

A login always wins both contests, and any displaced account keeps its portable
balance:

- **By account (ADR-0002):** logging in at a new PC kicks the account off its
  previous PC. (One account cannot occupy two PCs.)
- **By PC (this ADR):** logging in at a PC ends whatever account was on that
  PC. (One PC cannot host two accounts.)

The binding (account ↔ PC) is therefore unique on both axes, enforced at login.

## Consequences

- Login is the single mutation that resolves all session contention; it must
  run server-side in a transaction that atomically ends any prior session on
  both the account and the PC before establishing the new binding.

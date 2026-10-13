# ADR-0001: Time ownership — account vs PC

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)

## Context

The MVP (the original implementation specification) models all paid time as
`pcs.remaining_seconds` — time belongs to the PC, and the authoritative server
timer decrements the PC every second.

the original expansion plan introduces customer **accounts** with personal time. This raises the
core question: when a customer buys time, who owns it — the PC, or the person?
The answer determines what the authoritative timer actually decrements.

## Decision

Time ownership depends on login state:

1. **Guest (not logged in):** time is bound to the **PC**
   (`pcs.remaining_seconds`). It is not portable. The next person at that PC
   consumes whatever is left. This preserves the MVP model.

2. **Logged in:** time is bound to the **account**. The account balance is the
   source of truth and is what the authoritative timer decrements each second.
   The PC is only a viewport into the logged-in account's balance.

3. **Claim (PC → account):** when a customer logs in at a PC that has Guest
   Time, an explicit prompt ("Do you want to claim this time?") offers to
   absorb the PC's guest balance into the account. On confirm, the balance
   moves to the account and the PC's guest balance drops to 0. Absorption is
   never silent. A customer with a zero account balance can still claim: the
   server keeps a zero-balance session bound while its PC holds claimable Guest
   Time, so the per-second timer does not end the session before the customer
   presses the claim button.

4. **Logout (time leaves with the customer):** on logout (or session end), the
   account keeps its remaining balance and the PC drops to 0. Time is portable
   and follows the customer, not the seat.

Consequence: there are **two timer modes**. A PC-bound timer for guest
sessions, and an account-bound timer for logged-in sessions. The authoritative
server must know which account (if any) is bound to which PC to tick the right
balance.

## Accepted trade-offs

- A guest can convert PC-bound leftover time into portable personal time by
  logging in and claiming, then logging out. The owner has accepted this:
  "no one stops them" — guest mode is for frictionless walk-up use, and the
  claim convenience outweighs the leakage.
- Whoever logs in at a PC can claim leftover guest time that a *previous*
  guest paid for. Also accepted for the same reason.

## Consequences / still open

- The MVP timer loop (decrements PCs) is insufficient for logged-in sessions
  and must be generalized.
- A new concept is needed to bind an **account ↔ PC** for the duration of a
  logged-in session (see future ADR). Single-session constraints (can one
  account be active at two PCs at once?) are not yet decided.
- Crash/power-loss durability of the account balance (which now ticks every
  second) is not yet decided.

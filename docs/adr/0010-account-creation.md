# ADR-0010: Account creation & "guest accounts" clarification

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0004 (auth threat model).

## Context

the original expansion plan mentions account "Sign-up," an admin "Create/Delete Account," and a
separate line "Implement Guest Accounts." Need to pin who creates accounts and
whether "guest accounts" is a distinct entity.

## Decision

- **Accounts are created two ways:**
  1. **Self-service sign-up** — a guest at a PC clicks "Sign up," chooses a
     unique username + password, and is immediately logged in.
  2. **Admin create** — the owner creates an account from the dashboard.
- **No verification, junk accounts accepted.** There is no email/phone to
  verify (ADR-0004), so sign-up is just username + password. Mass-creating
  empty accounts is **not defended against** — empty accounts hold no value.
- **"Guest Accounts" is NOT an account entity.** It is exactly the anonymous
  **guest mode** already modeled (ADR-0001): insert coin, play, no login, time
  bound to the PC, zero points. No `accounts` row is created for a guest. Do
  not build a separate guest-account table or identity.

## Consequences

- `accounts` needs a unique username constraint.
- Admin **delete** account semantics (what happens to a deleted account's
  balance and its historical transactions/audit rows) are not yet decided —
  see next ADR. History must not be destroyed.

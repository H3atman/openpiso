# ADR-0004: Account authentication & threat model

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)

## Context

ADR-0001/0003 turn an account into a wallet holding portable time and points,
both convertible to free computer use — i.e. money. The MVP had no auth because
nothing was stealable (time lived on the PC). Portable value makes account
theft worth doing. Constraints: local LAN only, **no cloud, no SMS, no email**,
so all identity/recovery happens physically in the shop.

## Decision

- **Credential: username + password.** Verified by the admin server against
  local SQLite. No remote auth.
- **Threat model: weak security is acceptable.** This is a neighborhood shop.
  If customer C knows customer B's password, C can drain B's time and points,
  and the system does not try to prevent this (no second factor, no out-of-band
  verification — there is no channel for one). Accepted.
- **Recovery is an owner action.** There is no self-service "forgot password."
  The admin **resets** a customer's password manually (per the original expansion plan). This
  also serves customers (e.g. kids) who will forget passwords.

## Non-negotiable guardrails (cheap even under "weak security")

1. **Store password hashes, never plaintext.** "Weak security acceptable"
   refers to the *theft-by-known-password* threat, not to storing readable
   passwords. The admin **resets** (sets a new) password; it never **retrieves**
   the old one. A leaked SQLite file must not expose customers' reused
   passwords.
2. **Audit sensitive account actions** (login, claim, redemption, wheel spins,
   manual admin adjustments) so the owner can investigate disputes like
   "someone took my time."

## Amendment: legacy bcrypt credentials

ADR-0057 permits Account Import to store an existing Martec bcrypt hash
without re-hashing. The login verifier accepts `$2a$`, `$2b$`, and `$2y$`
bcrypt credentials only for this transitional path; after successful
verification it immediately replaces the credential with Argon2 using
compare-and-swap semantics. New, changed, and reset passwords remain Argon2.

## Consequences / still open

- Accounts table needs username (unique) + password hash.
- Sign-up flow (who can create accounts — self-service at a PC, or owner-only?)
  is not yet decided.
- Guest sessions remain fully anonymous, no credential.

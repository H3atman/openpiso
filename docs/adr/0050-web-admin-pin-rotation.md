# ADR-0050: Web Admin PIN rotation preserves the caller session

- Status: Accepted
- Date: 2026-07-29
- Deciders: shop owner (product)
- Supersedes: [ADR-0024](0024-web-admin-sole-owner-surface.md) (the absence of a PIN rotation route)

## Context

The Web Admin PIN was set only during first-run setup. Changing a known PIN
required wiping the credential and repeating setup, which made routine secret
rotation unnecessarily disruptive. Wipe-based recovery remains necessary when
the current PIN is forgotten.

## Decision

- Add an owner-session-authenticated Web Admin PIN rotation route.
- Require the current PIN and verify it against `owner_credentials` before
  replacing the Argon2 hash. The new PIN uses the shared 4–128 character
  password validator.
- Perform the credential replacement and success audit in one SQLite
  transaction. Audit wrong-current-PIN denials separately.
- Revoke every previously issued owner session after success except the token
  that called the route. The caller remains authenticated; all other browsers
  must log in with the new PIN.
- Do not add rate limiting: the route already requires both a valid owner
  session and proof of the current PIN.

## Consequences

Routine rotation no longer requires destructive recovery. A stolen session
alone cannot change the credential, and sessions held elsewhere stop working
after a successful change. Forgotten-PIN recovery remains credential wipe plus
first-run setup.

## Amendment (2026-08-25): Remote Control termination

[ADR-0074](0074-owner-session-hardening.md) retains the caller's Owner Session
but ends every Remote Control Session after Web Admin PIN rotation, including
one owned by the caller.

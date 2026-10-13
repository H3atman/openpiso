# ADR-0017: Case-insensitive account usernames

- Status: Accepted
- Date: 2026-06-21
- Deciders: implementation team
- Refines: ADR-0004 and ADR-0010

## Context

M1 requires unique local usernames. Treating `Juan` and `juan` as different
accounts would create avoidable login mistakes and confusing audit history.
Unicode case folding also varies across libraries and SQLite builds.

## Decision

- Usernames are trimmed and limited to 3-32 ASCII characters.
- Allowed characters are letters, digits, `.`, `_`, and `-`.
- The entered spelling is preserved as `username`.
- Identity and uniqueness use `username_normalized`, produced by ASCII
  lowercasing the validated username.
- Login applies the same normalization.
- Soft-deleted usernames remain reserved permanently.

## Consequences

`accounts.username_normalized` has a unique constraint. A later change to
Unicode usernames or username reuse requires an explicit migration and ADR.

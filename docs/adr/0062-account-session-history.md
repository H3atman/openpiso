# ADR-0062: Account Session history and restart closure

- Status: Accepted
- Date: 2026-08-07
- Deciders: shop owner (product)
- Refines: [ADR-0013](0013-balance-and-session-durability.md)
- Relates to: [ADR-0048](0048-client-guard-tamper-kill.md)

## Context

The live Account Session binding is held only in Admin memory, so the Audit
Log alone cannot answer which Account held a PC at a past instant. Tamper Kill
attribution uses that history to identify the Account at the server-derived
kill instant rather than relying on the delayed report instant.

The repository already has ADR-0061 for Account Login Throttle. This decision
therefore uses the next available ADR number rather than replacing an accepted
ADR.

## Decision

Admin stores an append-only Account Session history in
`account_session_history`. Each row records the session id, Account, PC,
`started_at`, `ended_at`, and the matching session end reason. A new row is
inserted in the same SQLite transaction as the existing `login` Audit Log
event. Its end is written in the same transaction as the corresponding
`logout`, `auto_kick`, `taken_over`, `expired`, or `tamper_kick` Audit Log event.

An Account Session that logs in again on the same PC resumes its existing live
binding and history row; it is not a new session.

On Admin startup, one async recovery operation closes every history row without
an end time with `server_restart`. The end time is that PC's last known
heartbeat. If no heartbeat exists, the operation uses the current database
time so startup still succeeds. Recovery writes no session-end Audit Log
event and is idempotent: already-closed rows are untouched.

## Boundary with ADR-0013

This table is **history, not live state**. The in-memory binding remains the
authority for who is currently on a PC. Recovery never reconstructs bindings or
Account tokens. An Admin restart still logs everyone out and customers still
return to the login screen; only the historical record becomes durable.
Balances, Points, and Guest Time remain unchanged.

The history is the durable source for new kill-instant attribution. A Tamper
Kill report derives its instant as Admin report receipt minus **Kill Report
Age**, then selects the historical Account Session covering that instant. A
missing or over-24-hour age records `unknown`; a session already ended before
report receipt still identifies its Account. `server_restart` closure is
infrastructure history, not evidence that the Account was absent. This does
not restore a live binding or change the customer-facing Tamper Kick behavior.

## Consequences

- Session history and the Audit Log remain transactionally aligned for normal
  Account Session starts and ends.
- Restart closure records infrastructure recovery without manufacturing a
  customer-facing session-end event.
- Account Session history is retained with the Accounts Backup Set and is not
  exposed as a new owner screen in this change.

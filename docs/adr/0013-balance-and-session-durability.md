# ADR-0013: Balance & session durability

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0001 (account is source of truth), ADR-0002 (binding).

## Context

The account balance is the source of truth (ADR-0001) and ticks every second on
the admin server (disk-backed, unlike the diskless clients). Need to decide how
durable the tick is, which way crash errors fall, and what a server restart
does to live sessions.

## Decision

- **Write every second.** Each active session's balance is written to SQLite
  once per second. At this scale (a handful of PCs) the I/O is negligible and
  "the database is always the truth" eliminates a class of stale-state bugs. No
  in-memory batching of the authoritative balance.
- **Crash errors favor the customer.** If the server dies mid-second, the
  in-flight (unsaved) second is **not** charged — the customer keeps it. The
  shop absorbs the trivial loss; customers never feel cheated by a crash.
- **Session bindings are in-memory only.** A server restart (crash, update,
  power blip) **logs everyone out** — all clients drop to the login screen.
  Balances are safe because they were written every second. Customers re-login
  and resume against their intact balance. The durable
  [Account Session history](0062-account-session-history.md) is only a record:
  it does not restore bindings or tokens, and startup closes open records as
  `server_restart` without manufacturing session-end Audit Log events.

## Emergent (and acceptable) behavior

- While the server is down it is **not ticking**, so customers **ride free**
  through the downtime and then must re-login — both customer-favorable, and
  consistent with ADR-0013's "errors favor the customer."
- The coin terminal is the *same* admin server, so **no time can be sold during
  downtime** anyway. Existing customers riding free until reconnect is bounded
  by how fast the server comes back.

## Consequences / still open

- Mitigation for the above is operational: keep the local server reliable
  (it's a LAN box, restarts are rare and fast).
- The `sessions`/binding state is deliberately ephemeral; only balances,
  transactions, and audit rows are durable. Account Session history is a
  durable historical record, not binding state; see
  [ADR-0062](0062-account-session-history.md).

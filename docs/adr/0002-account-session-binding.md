# ADR-0002: Account ↔ PC session binding (single active session)

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)

## Context

[ADR-0001](0001-time-ownership.md) makes the **account** the source of truth
for time while logged in, and the authoritative server timer decrements that
account every second. For the server to tick the right balance, it must know
which account is bound to which PC. This binding — an **active session** — is a
new core entity not present in the MVP schema. Its hardest constraint is what
happens when one account tries to be in two places at once.

## Decision

- An account has **at most one active session** at any time.
- Logging in at a new PC **auto-kicks** (force-logs-out) the account's prior
  PC. The newest login wins; time follows the customer to the new PC. The old
  PC drops to 0 and shows the expired overlay (consistent with ADR-0001
  logout: time leaves with the customer).
- The **server is authoritative** over the binding. The kicked PC does not
  need to be reachable at kick time: when it next heartbeats, the server tells
  it "you are no longer the active session" and it logs out locally. A network
  blip on the old PC cannot create two live sessions for one account, because
  only the server-side binding counts.

## Consequences / still open

- A session/binding record is needed (account_id ↔ pc_id ↔ live status). Its
  exact shape (reuse `sessions` table vs new table) is an implementation
  decision deferred to schema design.
- Durability: the account balance now ticks every second. Whether every tick
  is a DB write, or ticks are batched in memory and flushed periodically (and
  what happens to the balance on a crash/power loss mid-session), is not yet
  decided.
- Guest sessions still bind to the PC only (no account), unchanged from the
  MVP.

# ADR-0068: Owner Sign-Out records a Logout in history and an owner act in the Audit Log

- Status: Accepted
- Date: 2026-08-11
- Deciders: shop owner (product)
- Refines: [ADR-0001](0001-time-ownership.md), [ADR-0002](0002-account-session-binding.md),
  [ADR-0006](0006-session-lifecycle.md)
- Relates to: [ADR-0015](0015-audit-log.md), [ADR-0062](0062-account-session-history.md)

## Context

ADR-0006 fixes the only events that end a logged-in session: the customer's
own **logout**, **auto-kick**, **taken over**, and **time = 0**. Every one of
them is triggered by a customer action or by the clock. The owner had no way
to free a seat — a customer who walks away without logging out holds a PC
until their balance drains, and an owner facing a queue, a shift change, or a
tamper-locked PC could only wait.

Adding the owner as a fifth trigger is uncontroversial as *lifecycle*: nothing
new has to be decided about what happens to the money, because ADR-0001
already answers it. What has to be decided is **what the two durable records
say about it**, and those two records want different answers.

- **Account Session history** (ADR-0062) feeds reporting and Tamper Kill
  attribution. Both care about *when a session ended and for what lifecycle
  reason*. For those consumers an Owner Sign-Out is a logout in every respect
  that matters.
- The **Audit Log** (ADR-0015) answers *who did this*. It is the record a shop
  reaches for when a customer disputes a charge or claims they were thrown off
  a machine.

A single shared record cannot serve both. `sessions.rs` derives the audit
actor from the end reason — `SessionEndReason::Logout` implies
`AuditActor::Account` plus the customer's `actor_account_id` and the message
"Customer logged out and preserved account time". Reusing `Logout` unchanged
would therefore have produced an audit row asserting the customer left
voluntarily. In a dispute that row is evidence, and it would have been false.

The mirror option — a new `SessionEndReason::OwnerSignOut` — keeps the audit
honest but forks session history and every report and attribution query
reading `end_reason`, to record a distinction none of them need.

## Decision

**Owner Sign-Out** ends the Active Session on a PC from the Web Admin
dashboard. Its lifecycle effect is exactly a logout: the balance stays on the
account, the binding is dropped, the PC falls back to its own Guest Time, and
no money or timer changes beyond the binding ending.

The two records deliberately diverge:

- **Account Session history records `end_reason = 'logout'`.** Reporting and
  attribution do not fork over who clicked.
- **The Audit Log records a distinct `owner_sign_out` event with the `admin`
  actor**, carrying `account_id`, `pc_id`, and the `session_id`, and no
  `actor_account_id`. The customer is the *subject*, never the actor.

A history row reading `logout` beside an audit row reading `owner_sign_out` is
the intended state, not drift.

Two further consequences of the existing architecture, restated because they
are what make the feature safe rather than incidental:

- **The request is keyed on `session_id`, not on the PC.** The owner clicks
  from a dashboard snapshot that refreshes every few seconds. A stale click
  resolves to an idempotent no-op — never "end whatever is on this PC now",
  which would eject whoever took the seat since.
- **The PC need not be reachable.** ADR-0002 already makes the server
  authoritative over the binding; the Client drops a vanished session on its
  next heartbeat. An offline or tamper-locked PC is precisely when an owner
  needs the seat back, so PC state does not gate the action.

`owner_sign_out` is left out of both notification severity lists, so
ADR-0058's classifier defaults it to **Silent**: the owner performed the
action themselves and is already told by a toast.

## Consequences

- Reports and Tamper Kill attribution need no change; they continue to read a
  fixed set of `end_reason` values.
- Anyone auditing session ends must join the two records to distinguish an
  owner-ended session from a customer-ended one. This is documented in the
  glossary term and is the deliberate cost of not forking `end_reason`.
- Ending a **Guest** session remains undecided and unimplemented. A guest's
  time lives on the PC row, so ejecting a guest is a money decision (strand it,
  or destroy it) rather than a binding decision, and it needs its own ADR.
- There is no Accounts-page entry point. The owner's model here is "free up
  this PC", not "eject this account"; an account-centric sign-out would be a
  separate decision about a different question.

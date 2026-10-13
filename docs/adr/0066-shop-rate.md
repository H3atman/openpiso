# ADR-0066: Shop Rate — append-only paid-time pricing

- Status: Accepted
- Date: 2026-08-09
- Deciders: shop owner (product)
- Relates to: [ADR-0003](0003-points-economy.md) (points earn rate and
  Effective Bonus %), [ADR-0052](0052-coin-promo.md) (Coin Promo is measured
  against the paid-time rate)

## Context

The shop needs one unambiguous term for the owner-configured exchange that
turns inserted money into paid time. Calling this merely a "rate" is too vague:
the points earn rate turns money into loyalty points, while the **Bokya Rate**
controls the probability of a Spin the Wheel outcome. None of those settings
changes the amount of paid time a coin buys.

A rate change also needs durable meaning. Editing one row in place would make
old purchases appear to have been priced under today's setting and would erase
which exchange was in force when a customer paid. Economy warnings must help
the owner understand that choice without silently making the shop rate
unchangeable.

## Decision

### Shop Rate is the paid-time exchange

A **Shop Rate** is owner-configurable and consists of an integer amount in cents
and the whole number of seconds that amount buys. It is shop-wide: there is one
active Shop Rate for new paid-time purchases.

For an inserted amount of `inserted_cents`, the server computes paid time as:

```text
seconds_added = inserted_cents * rate_seconds / rate_amount_cents
```

The division is integer division. Any fractional second is truncated; it is
not rounded up. The server remains authoritative for the resulting time.

### Changes append and retire

A Shop Rate change appends a new rate and retires the previously active rate.
The previous rate is never edited in place, and there is never more than one
active rate. A purchase keeps the Shop Rate used when it was committed; changing
the active rate affects future purchases only.

### Economy guardrails advise, never veto

Economy guardrails, including the **Effective Bonus %** warning, may warn about
a Shop Rate change but never block it. The owner may deliberately choose a rate
that triggers a warning. Ordinary validation and database integrity rules are
separate from these economy warnings.

### No scheduled rates

Shop Rates do not vary by schedule, date range, or time of day. An owner makes
an explicit change, and the newly active rate applies from then on.

## Consequences

- Integer division can discard a fractional second. For example, with a
  `3-cent` Shop Rate that buys `10` seconds, `1` cent buys `3` seconds, not
  `3⅓` seconds. Web Admin reads the configured Coin Device Pulse Map and
  shows an inline advisory for each affected denomination; the advisory never
  blocks saving the Shop Rate.
- Committed time is never retroactively recomputed when the Shop Rate changes;
  historical purchases retain the seconds the server committed under their
  then-active rate.
- Append-only history preserves which paid-time exchange was active for each
  change and keeps the active rate distinct from retired rates.
- Effective Bonus % and the Wheel's paid-time safety advisory remain useful
  owner warnings, but a warning cannot prevent an owner-authored Shop Rate
  change. The Wheel's own Redemption Tier save/spin guardrail remains intact.
- There is no automatic happy-hour or time-of-day repricing. Any future
  scheduling policy would require a separate decision.

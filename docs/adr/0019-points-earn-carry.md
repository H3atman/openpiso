# ADR-0019: Points earn carry for logged-in purchases

- Status: Superseded by [ADR-0038](0038-fractional-points-centipoints.md)
- Date: 2026-06-21
- Deciders: implementation, constrained by ADR-0003 and ADR-0008

## Context

ADR-0003 defines whole-number points with a configurable earn rate, default
PHP 50 = 1 point, but left open what happens to purchases below the threshold.
OpenPiso accepts small coin events, so dropping every sub-threshold purchase
would make two separate PHP 25 purchases worse than one PHP 50 purchase for a
logged-in customer.

## Decision

- Points remain integer only.
- Logged-in account coin purchases carry sub-threshold paid cents on the
  account as `point_earn_carry_cents`.
- On each logged-in coin event, the server adds the event amount to the
  account carry, awards `total / earn_cents_per_point` whole points, and stores
  `total % earn_cents_per_point` as the new carry.
- Guest coin events earn zero points and store no carry.
- Claiming Guest Time never grants points or carry retroactively.

## Consequences

- The account is the only place where earn carry can live.
- The coin transaction and audit event record the whole points awarded at the
  accepted coin event.
- Changing the earn rate affects future carried cents under the new rate; it
  does not re-price historical coin events.

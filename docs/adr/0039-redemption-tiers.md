# ADR-0039: Redemption Tiers

- Status: Accepted
- Date: 2026-07-20
- Deciders: shop owner (product)
- Supersedes: [ADR-0038](0038-fractional-points-centipoints.md) whole-batch redemption language
- Amends: [ADR-0003](0003-points-economy.md) guaranteed redemption model

## Context

The points economy currently exposes one guaranteed exchange pair. A customer
can redeem only repeated whole batches of that pair. Shops need several clear
points-for-time choices without making the customer type an amount or deriving
time on the client.

The existing pair may predate the tier authoring rules. An upgrade must not
silently alter that promise.

## Decision

Guaranteed redemption is configured as **Redemption Tiers**: an ordered set of
(points, time) options. Points are whole points and time is stored as integer
seconds. Owner writes must contain 1–20 tiers; each points value must be unique,
at least 5, and a multiple of 5; each duration must be a whole number of minutes
and at least one minute. Reads are always sorted by points ascending.

Migration seeds exactly one tier from the current `economy_settings.redeem_points`
and `redeem_seconds` values. That migrated row is grandfathered exactly as-is,
even when it violates the new authoring rules. Validation applies when the owner
next writes the tier set, at which point the complete replacement must be valid.

Redemption Tiers are authoritative for customer redemption and all points safety
math. Effective Bonus % is computed per tier; the headline value uses the most
generous tier (highest seconds per point). Wheel point value and house edge use
that same worst-case tier. The former redeem pair is absent from settings
contracts and owner UI; its released SQLite columns remain dormant.

## Consequences

- SQLite is the source of truth for the tier set; replacement is atomic.
- Rust validates the complete owner-supplied set before any rows change.
- The owner UI enters whole minutes and shows the resulting `HH:MM:SS` live.
- A grandfathered invalid row can be read and displayed but cannot be saved
  unchanged through the validated write endpoint.
- ADR-0038's centipoint storage remains unchanged. Tier prices are intentionally
  whole points.
- Editing tiers immediately changes Effective Bonus % and wheel safety math.
- The dormant `economy_settings.redeem_points` and `redeem_seconds` columns are
  retained only because released migrations are immutable.

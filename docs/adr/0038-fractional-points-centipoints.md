# ADR-0038: Fractional points (centipoints)

- Status: Accepted
- Date: 2026-07-19
- Deciders: shop owner (product)
- Supersedes: [ADR-0019](0019-points-earn-carry.md)
- Amends: [ADR-0003](0003-points-economy.md) (points are no longer whole-number only)

## Context

ADR-0003 defined whole-number points and ADR-0019 handled sub-threshold
purchases by carrying paid cents on the account. At the default earn rate
(₱50 = 1 point) a ₱25 purchase visibly earns nothing — the customer's progress
is invisible until a later purchase crosses the threshold. The owner wants
every purchase to visibly move the points balance, displayed with two
decimals.

Floating point remains banned for money-equivalent values, so fractional
points need an integer sub-unit.

## Decision

### Unit

- The canonical points unit becomes the **centipoint** = 1/100 point, stored
  and transmitted as integer `i64` everywhere (DB, core math, API, Tauri
  commands). No floats at any boundary.
- UI displays points with **exactly two decimals** (`12.00`, `9.98`) via a
  single `formatPoints()` per frontend; division by 100 happens only there.

### Earning (replaces ADR-0019 carry)

- Logged-in coin purchases earn **fractional points directly**: ₱25 at
  ₱50/point earns 0.50 points on the spot.
- Carry survives only for remainders **smaller than one centipoint**. Because
  `earn_cents_per_point` need not be divisible by 100, exact carry is stored
  in **centicents** (1/100 cent) as `point_earn_carry_centicents`.
- Guests still earn zero points and no carry; claiming Guest Time still never
  grants retroactive points.

### Whole-unit exceptions

All point amounts are centipoint-precision **except**:

- **Wheel spin** — costs exactly 100 centipoints (1.00 point); fractional
  bets are not allowed (ADR-0012 unchanged).
- **Redemption** — uses whole-point options; ADR-0039 supersedes the original
  whole-batch rule with owner-authored Redemption Tiers.

Fractional amounts are allowed for: transfers (ADR-0030), manual admin
adjustments (ADR-0021), and voucher point values (ADR-0009), all at 0.01
precision.

### Wire format

- API and Tauri payloads carry integer centipoints in fields renamed with a
  `_centi` suffix (e.g. `points_balance` → `points_balance_centi`). The
  rename — not a silent reinterpretation — forces every consumer to update.

### Migration

- One migration renames every points column (accounts, transactions,
  transfers, wheel, audit deltas) to its `_centi` name and backfills ×100.
- Existing `point_earn_carry_cents` is **paid out** into fractional points at
  the earn rate in effect at migration time; the sub-centipoint remainder
  becomes the new `point_earn_carry_centicents`. This is a one-time payout of
  accrued progress at the going rate, not a re-pricing of history.

## Consequences

- ADR-0019 is superseded; its guest/claim rules live on here.
- The Effective Bonus % invariant (ADR-0003) is unchanged. ADR-0039 replaces
  the former redeem pair with whole-point Redemption Tiers; only awarding
  granularity changed here.
- Customers can strand value below 1.00 point for the wheel and below the
  cheapest Redemption Tier; transfers let accounts pool fractional Points.
- Changing the earn rate still only affects future earning; the migration
  payout is the single exception, applied once.

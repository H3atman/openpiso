# ADR-0003: Points economy (earn, redeem, wheel)

- Status: Accepted (amended by [ADR-0038](0038-fractional-points-centipoints.md)
  for centipoints and [ADR-0039](0039-redemption-tiers.md) for guaranteed
  redemption)
- Date: 2026-06-21
- Deciders: shop owner (product)

## Context

the original expansion plan adds **Points** as a second currency: earned per inserted coin,
redeemable for time, and bettable on a "Spin the Wheel." None of it had
numbers. The danger: earn and guaranteed-redemption values can look plausible
independently while their combined **Effective Bonus %** loses the shop time.
ADR-0039 now represents guaranteed options as Redemption Tiers and uses the
most generous tier as the worst-case safety benchmark.

## Decision

### Earning
- Points are a **loyalty bonus layered on top** of the normal time purchase.
  Buying time still yields the full time (e.g. ₱1 = 5 min) **plus** points.
  Points never reduce the time bought.
- Earn rate is configurable. **Default: ₱50 = 1 point.**
- **Guests earn zero points.** Points require an account to live on. This is a
  permanent, intentional incentive to log in before paying. Guest coins that
  are later claimed (ADR-0001) do **not** retroactively grant points.

### Redeeming (guaranteed)
- Points redeem for time through owner-authored **Redemption Tiers**
  (ADR-0039). The seeded default option is **10 points = 4 hours (240 min)**.
  Each tier is a guaranteed, non-gambling exchange.

### Spin the Wheel (gamble)
- Separate from redemption. The customer bets points for a chance at a time
  prize and **can lose the bet entirely** (zero time). The wheel has a
  **configurable house edge** — it is a game the shop wins on average, not a
  giveaway.

### The governing invariant
- Each Redemption Tier has an **Effective Bonus %** based on its seconds per
  point, the earn rate, and the paid-time rate.
- **Target: ~10%** loyalty kicker. At the defaults (₱50=1pt, 10pts=4hr,
  ₱1=5min) this works out to ≈ 9.6%.
- The admin settings screen **must display the live Effective Bonus %** per tier.
  Its headline warning uses the most generous tier (highest seconds per point),
  so it always reflects the owner's worst case.

## Consequences / still open

- An account needs a points balance alongside its time balance.
- Wheel prize table / odds representation and how the house edge is expressed
  in settings are not yet modeled.
- Voucher top-ups (which can grant points) must respect that points are
  account-only.

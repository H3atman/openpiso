# ADR-0014: Reporting model (cash vs giveaway, liability, dimensions)

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0003 (points), ADR-0009 (vouchers), ADR-0012 (wheel).

## Context

The MVP report was one number: sum of coins = today's sales. The expansion
moves value through non-cash channels (promo vouchers, points redemptions,
wheel winnings, manual add-time) and creates a future obligation (earned
points). A single "sales" number would silently blend money made with time
given away.

## Decision

### Headline split — cash vs giveaway
- **Top-line "sales" = cash in only:** coins through the terminal **+ `sold`
  vouchers** (cash collected at the counter).
- **Free time dispensed is a separate cost line:** `promo` vouchers + points
  **redemptions** + **wheel** winnings + **manual** admin add-time.
- The two are **never blended**. The owner always sees "took in ₱X cash, gave
  away Y minutes free" side by side.

### Points liability
- The dashboard tracks **total outstanding (unredeemed) points across all
  accounts** — the future free-time already promised. This is the early-warning
  number for a ballooning giveaway obligation.

### Dimensions (all required now)
- **Historical** by period: day / week / month (not just "today").
- **Per-account** breakdowns.
- **Per-PC** breakdowns.

## Data-model mandate (the real cost of this ADR)

Every value-moving event must be recorded **consistently categorized from day
one**, even for cuts not yet displayed:
- a **type** (coin / sold-voucher / promo-voucher / redemption / wheel-bet /
  wheel-win / manual-adjustment),
- a **cash vs non-cash** flag,
- nullable **account_id** and **pc_id**,
- **amount_cents** (cash), **seconds delta**, **points delta**, **timestamp**.

This implies a **points ledger** (earn/redeem/wheel entries), not just a cached
balance, so per-account history and the liability total are derivable. Reports
should read from one consistently-typed source (a unified ledger or a reporting
view across the typed tables), not ad-hoc unions.

## Consequences / still open

- Exact schema (single ledger table vs typed tables + reporting view) is an
  implementation choice, but the categorization above is mandatory.
- Retention/rollup strategy for long histories is not yet decided.

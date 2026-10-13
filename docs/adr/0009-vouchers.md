# ADR-0009: Vouchers (minting, single-use redemption, kinds)

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0003 (points), ADR-0008 (off-PC top-up).

## Context

Vouchers are the only way to put time/points into an account without a coin
passing through the central terminal (ADR-0008). They create value outside the
coin economy, so the ~10% bonus invariant (ADR-0003) does not govern them. That
makes them useful (prepaid cards, promos) and dangerous (a reusable or leaked
code is a money printer).

## Decision

### Minting
- **Owner/admin only.** There is no customer-facing voucher generation. A
  voucher carries an owner-set **time** value and **points** value.

### Redemption
- **Single-use.** A voucher is consumed on first successful redemption and is
  then dead.
- **First-to-redeem wins.** A shared code redeemed by a friend first is simply
  spent; the original holder gets nothing. Accepted.
- **Atomic, exactly-once.** Redemption runs in a server-side DB transaction
  that flips the voucher's status (e.g. `active → redeemed`) under a
  row-level guarantee. The same code typed at two PCs at once can pay out **at
  most once** — never twice.
- **Account-only.** Redemption credits an account's time and points, so the
  customer must be **logged in**. Guests cannot redeem (no account to hold the
  points). Redeemed at a PC by the logged-in account.

### Kinds (for reporting)
A voucher has a **kind**, because it represents two different financial events:
- **`sold`** — a prepaid card the owner sold for cash at the counter. Its
  redemption (or its sale) counts as **revenue** in reports.
- **`promo`** — a giveaway / marketing freebie. Its redemption counts as a
  **cost / discount**, not revenue.
Reports must separate coin revenue, sold-voucher revenue, and promo cost.

### Audit
- Every voucher **mint** and **redemption** is written to the audit log (code,
  kind, values, who, when) so giveaways and prepaid sales are traceable.

## Consequences / still open

- A `vouchers` table: code (unique), kind, time value, points value, status,
  created_by/at, redeemed_by_account/at.
- **Code format** should be long/random enough to resist guessing
  (implementation guardrail).
- **Expiry** (do vouchers expire? different for sold vs promo?) is not yet
  decided.
- Reporting model must gain voucher-aware revenue/cost lines.

# ADR-0020: Voucher cash recognition and expiry representation

- Status: Accepted
- Date: 2026-06-21
- Deciders: implementation decision for M4
- Refines: ADR-0009 (vouchers), ADR-0014 (reporting model).

## Context

M4 requires vouchers to be report-ready from day one. Two details were still
open: when a `sold` voucher's cash is recognized, and how voucher expiry is
represented.

## Decision

- A single-mint voucher infers its kind from its server-validated cash amount:
  `sold_amount_cents > 0` is `sold`; zero is `promo`. The create request does
  not select kind.
- A `sold` voucher records the explicit cash amount collected at the counter
  when the owner mints/records the sale. That mint/sale operation writes the
  cash audit event with `cash_flag = true`, `amount_cents`, and the server
  timestamp. Later redemption credits prepaid value but does not create a
  second cash-revenue event.
- A `promo` voucher has zero cash amount. Its redemption is recorded as
  non-cash free value for reporting.
- Voucher expiry is represented as nullable `expires_at`. M4 creates vouchers
  without expiry by default; a non-null value can be introduced by later UI/API
  work without another table redesign.

## Consequences

- A positive `sold_amount_cents` creates a sold Voucher; zero creates a promo
  Voucher.
- Redemption rejects expired active vouchers without changing account balances
  or voucher status and records a denial event.
- Reports can count sold-voucher cash from `voucher_sold` events and free time
  from `promo_voucher_redeemed` and points-redemption events.

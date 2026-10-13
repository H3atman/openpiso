# ADR-0042: Single Voucher cash inference and individual void

- Status: Accepted
- Date: 2026-07-21
- Deciders: shop owner (product)
- Refines: ADR-0020, ADR-0040

## Context

The owner records one single Voucher by its cash amount, not a separately
selected kind. They also need to make one mistakenly minted Voucher
unredeemable without erasing its history or a recorded sale.

## Decision

Single-mint Voucher kind is inferred server-side from `sold_amount_cents`:
positive is `sold`; zero is `promo`. The create request has no `kind` field.
A sold Voucher still recognises its cash at mint, as ADR-0020 specifies.

An owner may void one active Voucher. The server persists its `voided` status
and records one non-cash `voucher_voided` Audit Log event. A voided Voucher
cannot redeem; redeemed Vouchers cannot be voided. Void never deletes the
Voucher or reverses a single-mint sold Voucher's already-recorded revenue.

## Consequences

- Voucher list clients can default to active cards while retaining redeemed and
  voided history through status filtering and pagination.
- The vouchers schema permits `voided` single-mint Vouchers and links an
  individual void to its Audit Log event.
- Refunds remain a future compensating-transaction feature, not a void side
  effect.

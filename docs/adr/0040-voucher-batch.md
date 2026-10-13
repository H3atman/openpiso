# ADR-0040: Voucher Batch (generate, kind inference, void, audit)

- Status: Accepted
- Date: 2026-07-20
- Deciders: shop owner (product)
- Refines: ADR-0009 (vouchers), ADR-0020 (voucher cash recognition)

## Context

The owner needs to generate 1–1000 vouchers in one action — prepaid cards sold
at the counter, or promo giveaways — sharing a prefix, uniform time/points/cash
values, and optional expiry. Unlike single-mint sold vouchers (ADR-0020), a
batch-sold card's cash is recognised **at redemption**, never at mint, because
the batch simulates a physical card sold to a walk-in customer who pays at the
counter; the cash is collected physically, and the API generates the code to
print. The mint action itself moves no money.

A batch is reopenable for re-print and CSV re-export, and can be **voided** —
all still-active cards become unredeemable in one audited action, without
touching already-redeemed cards.

## Decision

### Voucher Batch as a first-class entity

A `voucher_batches` table records one row per batch: immutable `name`, `prefix`, `suffix_length`
(6–16, default 8), `count` (1–1000), uniform `seconds_value` and
`points_value_centi`, optional `cash_amount_cents` (> 0 for batch-sold cards,
0 for promo), optional `expires_at`, `created_at`, and an admin-only
`voided_at` / `voided_audit_event_id`. Kind is inferred from
`cash_amount_cents`: > 0 → `sold`, 0 → `promo`. A batch with zero time and zero
points is rejected.

Each generated voucher carries a nullable `batch_id` foreign key back to its
batch. The voucher's `kind` is computed from the batch's cash amount.

### Cash-at-redemption for batch-sold cards (amending ADR-0020)

For **batch-sold** vouchers (`cash_amount_cents > 0`), the `sold_at` column is
`NULL` at mint — cash is recognised only when the voucher is redeemed. The
`sold_voucher_redeemed` event carries both the credited value and cash facts.
The cash-revenue view counts that event only when `cash_flag = true`; this does
not affect single-mint sold vouchers, which still recognise cash at mint per
ADR-0020 and redeem with `cash_flag = false`.

When `cash_amount_cents > 0`:
- The voucher is created with `kind = 'sold'`, `sold_amount_cents = <cash>`,
  `sold_at = NULL`.
- Redemption writes one `sold_voucher_redeemed` event in the same transaction
  as the balance and voucher-status changes.
- That event carries `cash_flag = true`, `amount_cents`, credited time/points,
  and the server timestamp; `report_cash_revenue` counts it.

### Batch-level void

An owner can void a batch. In one SQLite transaction, the server sets
`voided_at` and `voided_audit_event_id` on the batch row, changes every still-
`active` voucher in that batch to the persisted `voided` status, and writes one
non-cash `voucher_batch_voided` audit event with the batch id, prefix, and count
of cards changed. Redeemed cards remain untouched. Persisting the status keeps
SQLite authoritative while retaining batch-level audit granularity.

### Batch-level audit granularity

Minting audits once per batch: a single `voucher_batch_minted` event with
metadata containing the batch id, prefix, count, values, and expiry. No
per-card events. The cash flag is `false` and there is no cash amount at mint
(batch-sold cards recognise cash at redemption per the amended ADR-0020 rule
above).

### Code generation

Batch codes follow the format `PREFIX-XXXX-XXXX-...` where:
- `PREFIX` is 2–8 uppercase A–Z / 0–9, required, stored uppercase.
- The suffix is N random chars (6–16, default 8) from the 32-char unambiguous
  set: A–Z (minus O and I) and 2–9.
- Dash-grouped every 4 characters.
- Globally unique: the server retries on collision (UNIQUE constraint).

A prefix is reusable across batches — uniqueness is enforced by the full code,
not the prefix.

### Batch names

The server assigns every batch an immutable, globally unique five-character
case-sensitive name from Base62 (`A-Z`, `a-z`, `0-9`) and retries collisions.
The name identifies the batch in owner surfaces; the prefix remains the
owner-chosen component of voucher codes. Existing batches receive unique names
on migration.

## Consequences

- Voucher batches are unreachable from the `POST /web-admin/vouchers` single-
  mint endpoint; existing single-voucher paths are unchanged.
- The `vouchers` table gains nullable `batch_id` and `cash_amount_cents`
  columns (the latter already exists; `batch_id` is new).
- `redeem_voucher` rejects the voucher's persisted `voided` status with a
  distinct denial reason.
- Batch-sold voucher redemption writes one cash-bearing
  `sold_voucher_redeemed` event. Reporting counts this event alongside
  `voucher_sold`, whose single-mint recognition remains at mint.
- Migration 0019 adds `voided` to the voucher status constraint and backfills
  active cards belonging to batches that were already logically voided.

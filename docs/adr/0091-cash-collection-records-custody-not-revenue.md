# ADR-0091: Cash Collection records custody, not revenue

- Status: Accepted
- Date: 2026-11-26
- Deciders: shop owner (product)
- Relates to: [ADR-0014](0014-reporting-model.md), [ADR-0015](0015-audit-log.md), [ADR-0020](0020-voucher-recognition-and-expiry.md), [ADR-0040](0040-voucher-batch.md), [ADR-0054](0054-backup-and-restore.md), [ADR-0069](0069-rejection-credit.md)

## Context

The owner needs to know how much cash should be in the coin box and at the counter, and when and how much they last took. Cash Revenue answers a different question (what the shop earned) and must not move when the owner empties the box.

## Decision

A **Cash Collection** records that the owner took the cash of one kind: a **Coin Box Collection** (covers **Coin Sales**) or a **Voucher Cash Collection** (covers **Voucher Sales**).

- **The system computes the amount.** The owner supplies only the kind. There is no owner count, variance or shortage tracking, and a client-supplied amount is never read.
- **One collection covers everything not yet collected**, up to the moment it commits. Partial collections do not exist.
- **There is no undo.** The Audit Log is the history; there is no history list in the UI.
- **Storage** is a dedicated append-only `cash_collections` table (kind, positive integer `amount_cents`, server `collected_at`) in the Ledger Backup Set, plus a non-cash `cash_collected` Audit Log event under the `custody` Audit Category.
- **Uncollected(kind) = all-time sales of that kind minus the sum of that kind's collections.** There is no timestamp cut-off. Audit timestamps have one-second precision and two separate requests were measured landing in the same second, so "sales after the last collection time" is ambiguous. The all-time difference is exact because Cash Revenue rows are append-only and never reversed (voiding a voucher leaves revenue unchanged).
- **Collect runs in one `BEGIN IMMEDIATE` transaction** that computes Uncollected, refuses at 0 with a conflict, inserts the row and appends the audit event. Concurrent Collect calls therefore record the cash once.
- **Collecting never changes** Coin Sales, Voucher Sales, Cash Revenue or any report figure.

## Consequences

- Batch-sold voucher cash enters Voucher Cash Uncollected only at redemption (ADR-0040), not when the owner took it at the counter.
- Backups refuse an unmapped table, so `cash_collections` is mapped to Ledger. Restore needs all seven Backup Sets, so collections and the sales rows they offset always restore together.
- Coin simulator coins count as Coin Sales (ADR-0014 is unchanged), so they appear in Coin Box Uncollected.

### Rejected

- **Timestamp cut-off** (sales after the last collection): ambiguous within one second.
- **Owner-entered amount or count:** invites variance tracking the shop does not want, and trusts a client number for money.
- **Reversible or editable collections:** the audit trail would no longer be a plain history.

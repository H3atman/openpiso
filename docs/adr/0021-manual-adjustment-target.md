# ADR-0021: Manual adjustments target PC Guest Time only

- Status: Superseded by [ADR-0069](0069-rejection-credit.md)
- Date: 2026-06-21
- Deciders: implementation team, constrained by ADR-0011 and ADR-0014
- Refines: ADR-0014 (reporting model), ADR-0018 (canonical audit compatibility)

> **Superseded 2026-08-11 by [ADR-0069](0069-rejection-credit.md).** The durable
> rule below — Manual Adjustment targets PC **Guest Time** only and reports as
> Free Time Dispensed — survives and now lives in ADR-0069. Two claims here are
> retired. First, "there is no owner command that directly increments or
> decreases an account's `remaining_seconds` or `points_balance`" was already
> false: `account_time_adjusted` and `account_points_adjusted` are shipped owner
> commands with their own typed events. Second, the workaround of minting a
> `promo` Voucher to correct an account is no longer the answer for a rejected
> coin; a **Rejection Credit** is. Read ADR-0069 for the current decision.

## Context

The expansion plan left open whether owner manual adjustments can target
account Remaining Time, guest PC balances, or both. The decision was deferred
to M6 because M6 is the first milestone that must make manual adjustments
report-ready (ADR-0014 lists "manual admin add-time" as a Free Time Dispensed
source).

The existing `manual_adjustments` table from the MVP is PC-bound: it has a
required `pc_id` foreign key to `pcs`, no `account_id` column, and the M1
migration backfilled canonical `manual_adjustment` audit events with only
`pc_id` and `seconds_delta`. M1 also added triggers that reject any new
`manual_adjustments` row without a linked canonical audit event.

Account balance manipulation already has several deliberately typed paths in
the expansion: coin events credit logged-in accounts (with points carry),
vouchers credit account time and points at redemption, guaranteed points
redemption converts points to account time, Spin the Wheel credits account
time, and admin soft deletion forfeits account balances. None of those is
labelled a "manual adjustment," and each carries its own audit event type and
reporting categorization.

## Decision

- Manual adjustments remain **PC-bound (Guest Time) only**.
- The owner's manual adjustment command targets a PC and an integer
  `seconds_delta`, applies to `pcs.remaining_seconds` clamped at zero, writes
  the canonical `manual_adjustment` audit event with `actor = admin`,
  `pc_id`, `seconds_delta`, `cash_flag = 0`, and a human-readable reason, and
  inserts the linked `manual_adjustments` detail row in the same SQLite
  transaction.
- A negative `seconds_delta` subtracts Guest Time but never drops the PC below
  zero. Such a subtraction is a **compensating event**, not an audit-history
  edit (ADR-0015).
- There is no owner command that directly increments or decreayses an
  account's `remaining_seconds` or `points_balance` as a "manual adjustment."
  Account-level corrections continue to flow through the typed mutation paths
  (coin events, vouchers, points redemption, wheel, soft deletion) so the
  audit categorization stays unambiguous.
- M6 reports classify `manual_adjustment` events with `seconds_delta > 0` as
  Free Time Dispensed and never as Cash Revenue.

## Consequences

- The `manual_adjustments` schema stays as M1 left it; no `account_id` column
  is added and no foreign key to `accounts` is introduced.
- Account-level disputes or corrections require the owner to use the existing
  typed paths (for example, mint a `promo` voucher and redeem it on the
  customer's account to grant free time, or soft-delete the account to forfeit
  balances).
- If a future milestone needs direct owner manipulation of account balances,
  it must add an explicit ADR, a new mutation path with its own audit event
  type, and a new report categorization — it cannot relabel
  `manual_adjustment` events as account-targeted after the fact.
- The Free Time Dispensed report total reconciles exactly with canonical
  `manual_adjustment` audit rows whose `pc_id` is set and `seconds_delta > 0`.

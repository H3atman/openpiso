# ADR-0069: Rejection Credit — owner re-credit of a rejected coin

> **Boundary amendment:** [ADR-0070](0070-credit-the-pulse-sum.md) removes
> the live mismatch producer. This ADR's repository, API, frozen attribution,
> Pulse Map denomination source, and economics continue to apply to historical
> `coin_batch_rejected` rows.

- Status: Accepted
- Date: 2026-08-11
- Deciders: shop owner (product)
- Supersedes: [ADR-0021](0021-manual-adjustment-target.md) (manual adjustment
  target)
- Amends: [ADR-0052](0052-coin-promo.md) (the coin-events-only list)
- Relates to: [ADR-0003](0003-points-economy.md) (earn is account-only),
  [ADR-0008](0008-coin-routing-central-terminal.md) (routing decides the
  target), [ADR-0014](0014-reporting-model.md) (cash vs giveaway split),
  [ADR-0015](0015-audit-log.md) (append-only Audit Log),
  [ADR-0018](0018-canonical-audit-compatibility.md) (detail tables hang off the
  canonical event), [ADR-0038](0038-fractional-points-centipoints.md)
  (centipoints and the centicent carry),
  [ADR-0058](0058-audit-derived-web-admin-notifications.md) (Notifications),
  [ADR-0064](0064-owner-visible-coin-loss-notices.md) (Coin Batch Rejection
  Notices), [ADR-0066](0066-shop-rate.md) (Shop Rate)

## Context

A **Coin Batch Rejection** is a closed **Coin Batch** whose observed pulse count
matches no **Pulse Map** entry. The acceptor has physically taken the coin — the
money is in the coin box — but the system credited nothing, because it could not
say what the coin was worth. ADR-0064 made that loss owner-visible as a
`coin_batch_rejected` **Notice** carrying the PC, the observed pulse count, and
any logged-in **Account**, and explicitly said the owner "can use the PC and
Account subjects to investigate a refund." It barred only an *automatic* refund.

Until now the owner had no honest way to complete that investigation. The
available paths all lie about what happened: minting a `promo` **Voucher**
records a giveaway when cash was actually taken; a **Manual Adjustment** is
PC-bound and reports as **Free Time Dispensed**; and neither can restore
**Points** or a **Coin Promo** bonus the customer would have received.

ADR-0021 stated there was "no owner command that directly increments or
decreases an account's `remaining_seconds` or `points_balance`." That statement
is already false in the shipped product: `account_time_adjusted` and
`account_points_adjusted` are owner commands on the Accounts screen, each with
its own typed event and classification. ADR-0021's *durable* rule — that
**Manual Adjustment** itself stays PC-bound and that account balance changes
need typed paths of their own — survives here; its false claim and its stale
"use a promo voucher" workaround do not.

The customer's word for this is "refund." That word is taken: ADR-0053 and
ADR-0054 use "no refund path" to mean the shop cannot hand cash back or rewind
paid value, and both statements remain true. **Credit** is also taken — the
glossary defines it as display-only vocabulary for ₱1 of inserted money.

## Decision

### The feature is a Rejection Credit, and nothing else triggers it

A **Rejection Credit** is an owner-initiated, evidence-linked re-credit of a
coin recorded as a **Coin Batch Rejection**. It is not a cash refund, not a
goodwill gesture, and not a service-failure remedy. Every Rejection Credit is
**required** to link to a specific `coin_batch_rejected` Audit Log row.

The link is the whole design. It is what turns a typed number into a claim the
owner can defend six months later, it is what supplies the attribution, and it
is what makes double-crediting detectable. A free-standing "credit this account
₱20" command was rejected: it is indistinguishable from theft in the Audit Log
and it already exists in weaker form as the account time and points adjustments.

For the same reason there is **no free-text reason field**. The linked rejection
is the reason; a prose box invites a second, unverifiable story beside it.

### Attribution is frozen at the rejection

The credit targets the subjects recorded on the rejection row, never the current
state of the world:

- Rejection recorded an **Account** → that Account.
- Rejection recorded no Account → that **PC**'s **Guest Time**.

The PC may have a different customer on it by the time the owner clicks, and the
rejection may be days old. Reading live occupancy would pay one customer's money
to another. This is the same reasoning ADR-0068 applies by keying Owner Sign-Out
on `session_id`. There is no owner override of the target.

If the recorded Account has since been deleted, the credit is **refused** with an
explanatory error. Silently falling back to the PC's Guest Time would hand a
deleted customer's money to whoever sits down next; ADR-0011 already forfeits a
deleted account's balance deliberately.

### Economics are exactly those of an accepted coin

The credit reuses `compute_coin_outcome`. For an Account target it credits:

- **Shop Rate** seconds for the amount (integer division, ADR-0066);
- **Points**, including the `point_earn_carry_centicents` carry (ADR-0038); and
- the matching **Coin Promo** bonus, if a tier matches the amount exactly
  (ADR-0052).

For a Guest target it credits **base Shop Rate seconds only** — no Points, no
Coin Promo — because both are account-only at insertion (ADR-0003, ADR-0008,
ADR-0052). This is an accepted consequence, not an oversight: a guest whose coin
was rejected gets exactly what a guest whose coin was accepted would have got.

Recomputing rather than replaying makes the credit sensitive to a Shop Rate
change between the rejection and the credit. That is accepted: the alternative
is storing a shadow price on a row that, by definition, never had one.

This amends ADR-0052's "coin events only" list. Coin Promo now also applies to a
Rejection Credit, because a Rejection Credit *is* a coin event that the machine
failed to read — the customer inserted the coin the tier was written for.
Vouchers, Manual Adjustments, points redemptions, and wheel winnings remain
excluded.

### The amount comes from the Pulse Map, never free text

The owner picks a denomination from the distinct cents values in the **current
Pulse Map**, and a coin count. The amount can never be prefilled: an unmapped
pulse count has no known value, which is the entire reason the rejection exists.

The Pulse Map is the right source even though it is the thing that just failed.
What was wrong was a *count*; the *denominations* the shop accepts are right.
An owner who recalibrates before crediting is behaving exactly as intended.

### Suppression summaries are creditable, partially

ADR-0064 bounds rejection Notices per **Selection Window** and emits a closing
summary of N suppressed batches. A jamming acceptor therefore produces exactly
the case where individual rows do not exist — the worst case to leave
uncreditable. A summary row is creditable up to its recorded count, in one or
more credits.

### One credit per action, guarded by a uniform count cap

Each action credits one linked rejection. Multi-select was considered and
rejected: it makes the confirmation preview a total the owner cannot check
against any single piece of evidence.

Double-crediting is prevented inside the credit transaction by a count cap:

```
SUM(coins_credited for this rejection) + requested <= cap
```

where `cap` is 1 for a per-batch rejection and the recorded suppressed count for
a summary. A unique index on the rejection id was rejected because it is
incompatible with partially crediting a summary. One rule, one code path.

A Rejection Credit is creditable for **30 days**, matching the Notification
view's window — the owner cannot act on evidence that is no longer visible.

### There is no reversal

A Rejection Credit cannot be undone. The Audit Log is append-only (ADR-0015) and
a reversal would need its own confiscation semantics against a balance the
customer may already have spent. The mitigation is prevention: the confirmation
dialog previews the exact seconds and points before commit.

### Persistence: a canonical audit event plus a detail table

The credit writes, in one immediate SQLite transaction:

- the canonical `rejection_credit` Audit Log event; and
- a linked `rejection_credits` detail row,

mirroring how `manual_adjustments` hangs off its canonical event (ADR-0018).

It deliberately writes **no** `coin_transactions` row. That table means "a coin
was accepted and valued by the coin path." A Rejection Credit is an owner
assertion about a coin the coin path rejected, and folding it in would corrupt
every coin-pipeline metric that reads the table as machine truth.

The audit event records `actor = admin`, `account_id` = the frozen target (null
for a guest credit), `pc_id` always set from the rejection, `amount_cents` =
denomination × coin count, `seconds_delta` = the total credited including any
promo bonus, `points_delta_centi` = points earned (0 for a guest),
`cash_flag = 1`, and `outcome = 'success'`. Metadata carries the linked rejection
audit id, the observed pulse count, the denomination, the coin count, and the
promo seconds.

A **Coin Trace Id** is deliberately absent: a trace id is minted when a Coin
Batch closes into a *valued* batch, and the `coin_batch_rejected` row records
only `pulse_count` and `suppressed_count`. Copying a trace id the rejection
never recorded would invent evidence; the linked rejection audit id is the
durable trail.

### Reporting: Cash Revenue, with the promo split preserved

A Rejection Credit is **Cash Revenue**, not Free Time Dispensed. The money is
physically in the coin box; the shop took it. Reporting it as a giveaway would
understate revenue and overstate promo cost from one keystroke.

Both views need the migration. `report_cash_revenue` keeps an explicit
event-type allowlist alongside `cash_flag = 1`, so `rejection_credit` is added
to it and nothing else about the view changes. `report_free_time_dispensed`
joins its coin-promo component off `coin_transactions`, so the migration adds a
parallel `UNION ALL` over `rejection_credits.coin_promo_bonus_seconds`. A
credit's promo component therefore hits the cost line exactly as a real coin's
does, and the two reports stay reconcilable.

### Classification: Alert

`rejection_credit` is an **Alert**, consistent with the existing
`account_time_adjusted` and `account_points_adjusted` Alerts, and stronger than
either: it is the only owner action that writes Cash Revenue from a typed
number.

### Owner surfaces

- **Notifications** (`NotificationsPage.svelte`) — a Credit action on the
  `coin_batch_rejected` Notice row. This is the canonical door and the **only**
  door for a guest rejection, which has no Account to browse from.
- **Accounts** (`AccountsPage.svelte`) — a per-row action opening that account's
  uncredited rejections from the last 30 days, disabled with "No rejected coins"
  when there are none. This is the door the owner reaches when the customer
  complains by name.

After a rejection is credited, its Notice renders a **Credited** marker and loses
its Credit action. It is never filtered out of history: the Notification list is
append-only evidence, and a row that vanishes on being acted upon destroys the
owner's ability to see what they did.

## Rejected alternatives

- **Call it a refund:** rejected. ADR-0053 and ADR-0054 use "no refund path" to
  mean cash out and rewinding paid value; both remain true. This feature moves
  value *in*.
- **Call it a Credit or Coin Credit:** rejected. **Credit** is a decided
  glossary term for display-only ₱1 vocabulary.
- **Call it an Account Credit:** rejected once guest-PC targeting was settled —
  the name would be false for every guest credit.
- **Extend Manual Adjustment to accounts:** rejected. It reports as Free Time
  Dispensed, carries a free-text reason, and cannot restore Points or a Coin
  Promo. ADR-0021's PC-bound rule survives for exactly this reason.
- **Allow any amount as free text:** rejected. It converts an evidence-linked
  correction into an unbounded owner payout with no defensible source.
- **Prefill the amount from the pulse count:** impossible by definition — an
  unmapped count has no known value.
- **Let the owner retarget the credit:** rejected. Frozen attribution is the
  control against paying one customer's money to another.
- **Write a `coin_transactions` row:** rejected. It would let an owner assertion
  masquerade as a machine reading in every coin-pipeline metric.
- **Report it as Free Time Dispensed:** rejected. The cash is real.
- **A unique index per rejection:** rejected. Incompatible with partial
  crediting of a suppression summary.
- **Reversal or void:** rejected. Append-only history plus a possibly-spent
  balance; the confirmation preview is the control instead.
- **A dedicated "Rejected Coins" screen:** rejected. The rejections already
  render in Notifications; a third surface would fork the evidence.

## Consequences

- ADR-0021 is **superseded**. Manual Adjustment remains PC-bound and reports as
  Free Time Dispensed; that rule moves here. ADR-0021's claim that no owner
  command touches account balances was already false and is retired, as is its
  "mint a promo voucher" workaround for account corrections.
- ADR-0052 gains one applies-to entry: Coin Promo applies to a Rejection Credit.
  Vouchers, adjustments, redemptions, and the wheel stay excluded.
- ADR-0053 and ADR-0054's no-refund statements are clarified to say **no cash
  refund path**; neither decision changes.
- ADR-0064's bar on an *automatic* refund is unchanged. Every Rejection Credit
  is an explicit owner act on a specific piece of evidence.
- Cash Revenue can now be increased by an owner keystroke. The linked rejection,
  the Pulse Map-constrained amount, the count cap, the 30-day window, and the
  Alert classification are the compensating controls.
- A guest Rejection Credit is deliberately worth less than an account one, in
  exactly the way an accepted guest coin is.
- Recomputing under the current Shop Rate means a rate change between rejection
  and credit changes the seconds credited.
- Correcting the Pulse Map remains the real fix. A Rejection Credit repairs one
  customer's loss; it does not stop the next one.

## Verification

Tests cover: outcome parity with an accepted coin (seconds, points, carry, promo)
through the shared pricing path; the guest path crediting base seconds with zero
points and zero promo; refusal when the recorded Account was deleted; the count
cap rejecting a second credit on a per-batch rejection and rejecting an overrun
on a partially credited summary; the 30-day window; and report reconciliation —
the credit's `amount_cents` appearing in Cash Revenue and only its promo
component appearing in Free Time Dispensed.

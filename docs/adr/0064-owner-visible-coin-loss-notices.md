# ADR-0064: Owner-visible coin-loss Notices

> **Historical boundary:** ADR-0070 removes the live Pulse Map mismatch path.
> Existing `coin_batch_rejected` rows remain immutable, readable, and eligible
> for Rejection Credit; current Coin Batch acceptance creates no new rows.
> The sections below preserve the historical decision and rationale; they are
> not an S1 producer or owner-list contract.

- Status: Accepted
- Date: 2026-08-08
- Deciders: shop owner (product)
- Refines: [ADR-0043](0043-coin-ingestion-sampling-and-observability.md) and
  [ADR-0058](0058-audit-derived-web-admin-notifications.md).
- Relates to: [ADR-0015](0015-audit-log.md) (Audit Log),
  [ADR-0023](0023-serial-coin-input-and-terminal-selection.md) (serial coin
  input), and [ADR-0063](0063-unmapped-pulse-count-typed-batch-read-outcome.md)
  (typed batch-read outcome).

## Context

Two coin-loss paths are now settled and must be understandable from the Web
Admin Notification list. They are not one anomaly with two messages:

- An **Inhibited Pulse** is a rising edge observed while the Set Pin is not
  asserted and the shared pulse buffer is empty. It is refused before a
  **Coin Batch** exists and can only be reported as a rate-limited aggregate.
  A rising edge while inhibited with a non-empty buffer is the admitted tail of
  a train that began before close and remains part of that Coin Batch.
- A **Coin Batch Rejection** is a closed Coin Batch whose observed pulse count
  matches no **Pulse Map** entry. The selection layer attributes it to the PC
  held by the Selection Window, including residual batches drained as that
  window closes, and to any logged-in Account resolved for that PC. It is
  reported per batch, subject to the Selection Window bound.

Before the two implementation tickets, the existing `coin_sampling_anomaly`
Notice presented an undifferentiated anomaly to the owner, while an unmapped
Coin Batch appeared only in the Coin Debug Bundle and Diagnostic Log. The owner
could not connect an inhibited pulse to Coin Source availability, investigate
idle insertion versus inhibit wiring with field evidence, or discover a
mis-calibrated denomination while performing the recent physical ₱20 coin
calibration work.

## Decision

### Two owner-visible Notices

`coin_sampling_anomaly` remains the Notice for aggregate Inhibited Pulses. Its
cause-keyed `reason` and summary distinguish two observable states:

1. the Coin Source was unavailable or recovering; or
2. the sampler was healthy while no Selection Window was open and the Set Pin
   was inhibited.

The second state does not claim whether the owner saw an idle insertion or a
wiring, polarity, or acceptor fault. Both produce the same rising edge while
the only available Set Pin state is the commanded inhibited value. The summary
states both possibilities and directs the owner to copy the Coin Debug Bundle.
The bundle's Anomalies section carries a readable cause and timestamp, and its
bounded raw appendix preserves the source evidence the owner can return for
field feedback. No new Web Admin component is needed: the existing **Copy Coin
Debug Bundle** action already copies the generated bundle.

`coin_batch_rejected` is a separate Notice for an unmapped Coin Batch. Its
per-batch summary states the PC, observed pulse count, and Account when one is
logged in. A closing suppression summary reports the number of suppressed
batches and includes an Account only when the suppressed rows have a uniform
Account attribution. It is the owner-visible surface for a likely Pulse Map or
denomination calibration problem; the Coin Debug Bundle remains the deeper
diagnostic surface.

Both events remain Notices rather than Alerts. A repeating hardware fault can
produce many rows, but an individual row has no owner action that an Alert
acknowledgement would perform. Keeping these rows out of the Alert
acknowledgement queue prevents a chattering signal from turning into a queue of
unactionable work.

### Why healthy Inhibited Pulses do not claim a definitive fault

The serial API can command RTS or DTR for the Set Pin and retain that commanded
state, but it cannot read the electrical line or receive an inhibit
acknowledgement from the acceptor. A healthy idle insertion and an acceptor
that ignores inhibit therefore have identical software evidence: a rising Coin
Pin edge while available, healthy, and inhibited with no Selection Window.
Pulse order is not causal evidence. Treating the first pulse as customer error
and later pulses as a Set Pin fault was rejected because a normal multi-pulse
denomination would trigger both diagnoses from one insertion.

Definitive separation requires new hardware evidence such as Set Pin voltage
feedback, inhibit acknowledgement, or coin-return sensing. Until field
feedback justifies that scope, the Notice reports only observable facts and the
Coin Debug Bundle carries the diagnostic evidence.

### Why unmapped rejections are Notices but commit rejections are not

An unmapped pulse count is a domain outcome, not a transport or database
failure. It is the exact failure mode produced by a mis-calibrated
**denomination**, and it was invisible during the physical ₱20 calibration
work. The owner can act on the observed count by correcting the Pulse Map and
can use the PC and Account subjects to investigate a refund — since
[ADR-0069](0069-rejection-credit.md), by issuing a **Rejection Credit** against
this Notice. It therefore
belongs in the owner-visible Notification projection.

A commit rejection is different: it is a rare failure after a mapped Coin
Batch has reached the authoritative commit path. It is already recorded in
Diagnostic Logs, and making it a Notification would extend the Audit Log
writer surface without a matching per-row owner action. Commit rejections
therefore remain Diagnostic Log evidence and do not join the Notice allowlist.
This boundary preserves both the actionable owner signal and the existing
fail-closed behavior.

### Why two event types instead of one reason discriminator

A single event type with a reason discriminator was initially agreed and then
rejected after the two cases were compared at their actual boundaries. They
have different producers (the Pulse Sampler versus the selection loop),
different cardinality (a rate-limited aggregate versus one event per rejected
Coin Batch), different attribution (none versus a PC and possible Account),
and different aggregation rules.

Folding them together would make the `count` field mean pulses for one row and
rejected batches for another. It would also make `pc_id` appear on only some
rows and make the glossary's promise that the Coin Sampling Anomaly is a
rate-limited aggregate false for Coin Batch Rejections. Separate event types
keep each row's shape and owner meaning honest.

### Attribution and count semantics

A refused Inhibited Pulse names no PC and no Account. The empty pulse buffer,
not transient in-memory selection state, provides no truthful PC or Account
attribution while a Selection Window is being torn down or a Coin Source is
recovering. An admitted tail is different: it remains part of the already
attributed Coin Batch and is credited to the just-selected PC during close
drain. Attributing a refused edge to the last-selected PC was considered and
rejected: an owner seeing a PC and Username beside a coin loss would blame a
station or customer on the strength of a guess.

For a Coin Batch Rejection, the PC and any logged-in Account are written to the
Audit Log subject columns. The actor is **System** and `actor_account_id`
remains empty. The Account did not perform an action; it is the Target Account
whose selected Coin Batch the system rejected. Putting it in the actor column
would render the customer as having caused a calibration fault.

Refused Inhibited Pulse counts are reported as pulses, never coins or pesos.
Those refused pulses never pass through gap logic, so the system has no
reliable coin boundary; a coin or peso figure would invent authority. An
admitted straddling edge is not an Inhibited Pulse and remains in its Coin
Batch. A Coin Batch Rejection reports its observed pulse count so the owner can
compare it with the Pulse Map, not a guessed monetary value.

### Existing history

Both events remain append-only Audit Log history. Pre-upgrade
`coin_sampling_anomaly` rows are not backfilled or rewritten: their sub-cause
was never recorded and cannot be recovered. Rewriting a summary to imply a
diagnosis the system never made is worse than allowing a vague row to age out
of the 30-day Notification view.

## Rejected alternatives

- **Leave unmapped rejections only in the Coin Debug Bundle:** rejected because
  the rejection path is precisely what a mis-calibrated denomination triggers.
  Keeping it diagnostic-only leaves the most likely silent coin-loss complaint
  invisible unless the owner knows to generate and inspect a debug export. The
  Coin Debug Bundle remains useful for deep investigation, but it is not the
  owner-visible first signal.
- **Make commit rejections Notifications too:** rejected because they are rare,
  already logged, and add no matching owner action; they remain Diagnostic Log
  evidence.
- **Use one `coin_loss` event with a reason:** rejected for the producer,
  cardinality, attribution, aggregation, and count-shape reasons above.
- **Promote either event to an Alert:** rejected because repeated hardware
  faults would fill the Alert acknowledgement queue with rows carrying no
  per-row action.
- **Infer a Set Pin fault from repeated healthy pulses:** rejected because
  pulse order is not causal evidence and one multi-pulse denomination could
  receive contradictory diagnoses.
- **Add Set Pin electrical feedback now:** deferred because it requires hardware
  and installation changes beyond this Notification boundary. Field evidence
  from copied Coin Debug Bundles will determine whether that work is needed.
- **Backfill old rows with inferred causes:** rejected because the missing
  sub-cause is not recoverable and Audit Log history is immutable.

## Consequences

- Owners can distinguish Coin Source availability from a healthy Inhibited
  Pulse and from Pulse Map calibration failures in the Web Admin Notification
  list. The healthy Notice truthfully names idle insertion and inhibit wiring
  as possibilities; copied Coin Debug Bundles carry field evidence for deeper
  feedback.
- The Audit Log remains canonical; no notification table, schema migration,
  automatic refund, or automatic Pulse Map correction is introduced. ADR-0069
  later adds an explicit, owner-initiated **Rejection Credit** against a
  `coin_batch_rejected` row; the bar on an *automatic* refund stands.
- The fail-closed guarantee remains unchanged: an unmapped Coin Batch credits
  no money, time, or points, and a commit failure remains retryable diagnostic
  evidence.
- ADR-0043 remains the authority for Pulse Sampler mechanics and Inhibited
  Pulse rate limiting, while ADR-0058 remains the authority for the derived
  Notification view and classifier. This ADR refines their owner-visible
  coin-loss boundary and the reasons for it.

## Verification

The shipped implementation verifies both observable Inhibited Pulse causes,
the healthy-state aggregate pulse count, and mixed-cause aggregation at the
sampler seam. Coin Debug Bundle tests verify that inhibited-pulse reasons become
readable anomaly evidence. Selection-layer tests verify attributed, bounded Coin
Batch Rejections, unchanged balances, and continued draining. The shared
classifier covers both event types as Notices.

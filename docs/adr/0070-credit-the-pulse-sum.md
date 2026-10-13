# ADR-0070: Credit the pulse sum, not a Pulse Map lookup

- Status: Accepted
- Date: 2026-08-13
- Deciders: shop owner (product)
- Supersedes: [ADR-0063](0063-unmapped-pulse-count-typed-batch-read-outcome.md)
  (the typed boundary survives without the `Unmapped` outcome),
  [ADR-0043](0043-coin-ingestion-sampling-and-observability.md) (observability
  of the coin path)
- Relates to: [ADR-0023](0023-serial-coin-input-and-terminal-selection.md)
  (the batch close and in-flight drain),
  [ADR-0033](0033-typed-coin-source-trust-and-coin-outcome.md) (typed trust
  boundary), [ADR-0060](0060-coin-20-calibration-capture.md) (what calibration
  writes), [ADR-0064](0064-owner-visible-coin-loss-notices.md) (the rejection
  Notice this decision mostly empties),
  [ADR-0069](0069-rejection-credit.md) (the manual repair this decision makes
  rare), [ADR-0071](0071-event-driven-coin-reader.md),
  [ADR-0072](0072-credit-the-straddling-train.md),
  [ADR-0073](0073-owner-coin-surface-is-a-report.md)
- Evidence: [`coin-acceptor-characterization.md`](../coin-acceptor-characterization.md)
  §3, §6

## Context

The acceptor emits **exactly one pulse per ₱1**, confirmed at volume: 705
pulses across 65 individual coins and four denominations, every trial exact
(characterization §3). The pulse count already *is* the peso value.

The **Coin Batch** close does not add the pulses up. It splits on the quiet gap,
looks the batch's *count* up in the **Pulse Map** (`{1, 5, 10, 20}`), and
credits nothing for a count that is not there. ADR-0063 made that zero-credit
deliberate and named it fail-closed.

Simulated end-to-end over the bench archive — ₱1,825 inserted, 116 valid trials:

| Path | Credited | Recovery |
|---|---|---|
| Shipped: 30 ms poll, no filter, Pulse Map lookup | ₱722 | **39.6 %** |
| A *perfect* filter → shipped Pulse Map lookup | ₱925 | **50.7 %** |
| **Unfiltered incumbent poll → sum crediting** | ₱1,854 | **101.6 %** |
| Filter → sum crediting | ₱1,825 | **100.0 %** |

The lookup throws away roughly 60 % of inserted value, and **the filter is not
what recovers it.** Fixing the signal and keeping the lookup reaches 50.7 %;
keeping the bad signal and summing reaches 101.6 %.

The mechanism is that the lookup converts a **proportional** error into a
**total** one. A ₱10 coin read as 9 or 11 pulses is a ₱1 error; under the lookup
neither count is a key, so the credit is ₱0 — the whole coin. The same
asymmetry destroys every legitimate merged batch: two coins inserted close
together concatenate into one train with **no seam at all** (characterization
N2), so ₱20+₱10 counts 30 pulses *perfectly* and credits **₱0**. Thirty pulses
is ₱30 no matter which coins made it.

The 30 ms polling period and 600 ms quiet gap are conservative defaults
for typical pulse coin acceptors, not calibration results. The quiet gap is
operator-adjustable. The measurements below, rather than the defaults alone,
motivate pulse-sum crediting.

## Decision

### The governing principle

> **The customer's coin is the ground truth. The machine's uncertainty is the
> machine's problem, not the customer's.**

The two failure modes are not symmetric in kind, only in sign. A **lost coin**
leaves the customer standing at the machine having paid for nothing — visible,
loud, trust-destroying, and under the shipped code it is the *modal* outcome on
a ₱10 coin. An **over-credit** costs the owner ₱1–3, quietly, and is bounded.
Fail-closed weighs these equally and picks the owner's side; the archive prices
that posture at ~60 % of gross.

### Credit is the sum

A closed positive Coin Batch credits `pulse_count × 100 cents`, unconditionally.
The fixed cents-per-pulse value is Rust authority, not a setting.

The scalar carries a live invariant: **sum crediting is valid only while
`pesos_per_pulse` is 1**, which is why the acceptor stays at one pulse per peso
(characterization §9.2). The invariant attaches to the scalar, never to the
Pulse Map table.

### The Pulse Map is demoted to reporting metadata, structurally

Nothing in the automatic Coin Batch crediting path reads it. It is kept — "this
hour: 40 batches, mostly ₱10-shaped" is genuinely useful — but it must live
where it **cannot reach automatic crediting**. A comment saying "do not credit
from this" is not sufficient: the entire failure was a reporting concept
holding the credit hostage. Rejection Credit remains a separate,
owner-authorized repair path and can read Pulse Map denomination metadata under
ADR-0069.

`CoinSourceConfig` does not contain the Pulse Map. `CoinBatch` contains pulse
observations, not a stored amount. Its amount is derived with checked arithmetic
from `pulse_count × 100 cents`. `CoinBatchRead` has `None` and `Batch` outcomes;
Pulse Map mismatch is not an outcome. The error channel remains reserved for
transport and availability failures.

If checked amount calculation overflows, the batch does not credit. The existing
commit-failure path retains it for investigation, inhibits the Set Pin, and
returns an error. This is an arithmetic failure, not a plausibility decision.

Owner-facing consequences of the demotion (read-only table, written only by
Verification) are ADR-0073's.

### The plausibility gate keeps its job and loses its power

An implausible count is real evidence of a fault, and crediting blind means a
hardware failure that emits spurious pulses hands out free money with nothing
in the way. So the gate stays — **it reports, it never refuses**.

That frees the predicate from having to be safe (a false positive now costs a
log line, not a coin), and it changes which predicate is right:

- **Adopted — inter-pulse timing.** The low period is fixed at ~205.7 ms and is
  independent of the acceptor's width switch (characterization §3). An acceptor
  emitting spurious pulses violates that period; three ₱20 coins in a burst do
  not. This is the only candidate that separates *a fault* from *a lot of
  coins*, and it needs no new measurement — `interval_us` is already logged per
  pulse at `coin_source.rs:423`.
- **Adopted as companion — pulse width.** The drift alarm that catches a unit
  left in the acceptor's fast mode (30.09 ms against 100.3 ms). Its input comes
  from the filter's own LOW span under ADR-0071, never from `high_since`.

### Vocabulary

`Unmapped` retires as a *crediting outcome*. Its replacement names an
observation about the **signal**, never about a batch's value:
`AnomalyKind::UnmappedRejection` retires, and `getUnmappedCoinPulses()` becomes
`getAcceptorHealth()`. S1 removes the per-batch event list and reports timing as
unavailable until S2 supplies filtered timing. S2 adds
`AnomalyKind::ImplausibleTiming` and its rate; S1 does not give old Pulse Map
mismatch evidence a new name.

S1 serves `GET /web-admin/coin-device/acceptor-health` with the current Coin
Debug Window start, `timing_status = unavailable_until_filtered`, and a null
implausible-timing rate. The old `/unmapped` route has no compatibility alias.

### This ships alone, ahead of the filter

Sum crediting scores 99.6–103.2 % across 60 phase offsets on the **unfiltered
incumbent poll**. It needs no filter, no acceptor change and no bench work.

The honest cost, stated here as **known, quantified and temporary** so it is not
later read as a regression: the unfiltered poll splits ~7–9 % of pulses, so this
over-credits by **1.1–2.1 %** until the filter lands. At the ratified filter the
over-credit is **0.0 %**. Trading a −60 % error for a +2 % one is not a close
call, and gating the cheap fix behind the expensive one costs real money in the
interim.

## Rejected alternatives

- **Keep the lookup, force separation at the slot.** Depends on an operator
  behaving, which is exactly what fails; and no spacing rule exists to give them
  (characterization N3).
- **Sum with a count ceiling.** No magnitude threshold separates a fault from a
  legitimate merged train — three ₱20 coins is 60 pulses. This moves the
  incumbent's cliff further out rather than removing it.
- **Sum gated on decomposability into known denominations.** Coin
  *identification* wearing a fault-detector's costume — the exact confusion this
  ADR dissolves. It also fails on its own terms: it would have caught **none** of
  the merged trials, because 30 decomposes as 20+10 (characterization N4).
- **Identify the merged coins instead.** Impossible, not merely hard. ₱20→₱10
  and ₱10→₱20 are indistinguishable over 30 trials (characterization N2). Sum
  crediting never asks the question.
- **Delete the Pulse Map.** It answers a real reporting question. Severance is
  the requirement, not removal.
- **Wait for the filter, then change the batcher.** The archive says the filter
  buys 1.1–2.1 % and the batcher buys ~60 %.

## Consequences

- **ADR-0063 is superseded.** Its typed `CoinBatchRead` boundary, its
  reservation of the error channel, and its drain behaviour survive and are
  load-bearing for ADR-0072. The `Unmapped` variant and its fail-closed crediting
  consequence retire; every closed batch credits its pulse sum.
- ADR-0064's `coin_batch_rejected` Notice becomes rare rather than routine:
  the batches it fired on now credit. S1 removes the live in-memory producer,
  counters, suppression, and queue because no current input creates this event.
  Historical Audit Log rows and Rejection Credit remain readable and usable.
- ADR-0069's **Rejection Credit** stays, and stays necessary — an acceptor that
  physically takes a coin and emits nothing at all is still possible — but the
  volume it exists to repair largely disappears.
- Over-credit of 1.1–2.1 % is accepted between stage S1 and stage S2 of the
  remediation plan, and is expected to reach 0.0 % at S2.
- The coin path now has exactly **two** fault detectors: the inter-pulse timing
  predicate and the pulse-width drift alarm. ADR-0071 §"taint" exists because
  both are silently falsifiable by a stall.
- `pesos_per_pulse` becomes a live constraint on any future acceptor
  reprogramming: changing pulses-per-coin invalidates this decision
  (characterization §9.2).

## Verification

Tests construct two settings values with different Pulse Maps and assert that
both produce the same `CoinSourceConfig`. With an adversarial map that contains
only `1 pulse → 777 cents`, a batch of 9 pulses credits ₱9 through standing
acceptance and a batch of 30 pulses credits ₱30 through the close drain. Exactly
two coin transactions result. The health surface returns an unavailable timing
status and no per-batch rows until S2. A checked-amount overflow returns an error,
retains the batch, and creates no transaction. S2 tests that an implausible
inter-pulse interval raises `ImplausibleTiming` **and** still credits.

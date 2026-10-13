# ADR-0047: Spin the Wheel — equal-slice segments, no probability weights

- Status: Accepted (truthful-geometry guarantee superseded in part by ADR-0049)
- Date: 2026-07-26
- Deciders: shop owner (product)
- Supersedes: [ADR-0012](0012-spin-the-wheel.md) (in part: per-segment
  probability weights and the "segments + probabilities" editor)
- Refines: [ADR-0046](0046-spin-the-wheel-points-prize.md) (segment shape)

> **Superseded in part by [ADR-0049](0049-bokya-rate.md):** equal slice
> geometry, duplication as the way to distribute prizes within a draw group,
> the derived prize line, and the load-time house-edge warning remain
> authoritative. When Bokya Rate is set, however, segments are not all `1/n`
> likely and the drawn geometry is no longer a complete or truthful statement
> of the odds. An unset Bokya Rate preserves the uniform draw described below.

## Context

Until now a Wheel Segment carried a positive integer `weight`. The weight drove
two things at once: the segment's probability of being drawn, and the angular
size of its slice. Because both derived from the same number they agreed — a
20-weight slice really was twice the arc of a 10-weight slice.

They agreed, and the wheel still looked rigged. A customer sees a wheel whose
slices are visibly unequal and reasonably concludes the shop has tuned the big
"Try again" wedge in its own favour — which is exactly what the owner had done.
The mechanic's credibility, not its mathematics, was the problem: a gamble that
looks fixed does not get played, and an unplayed wheel earns nothing.

The owner asked for a wheel that reads as fair at a glance while remaining
economically safe.

## Decision

### Segments are equal in probability and in size

A Wheel Segment has **no weight**. Every segment on a wheel of `n` segments has
probability exactly `1/n` and occupies exactly `1/n` of the circle. There is no
owner-facing or server-side knob that makes one slice likelier than another.

The displayed geometry is therefore a complete and truthful statement of the
odds. This is the whole point of the decision: what the customer sees is what
the RNG does.

### Rarity is expressed by duplication

The owner still controls the odds — by **repeating a prize across several
segments**. Three "Try again" slices on a twelve-slice wheel is a 25% chance of
losing, and it is legible as such from across the room. This is how a physical
carnival wheel works, and it is deliberately the *only* mechanism available.

The consequence is a floor on granularity. With `MAX_SEGMENTS = 24` the finest
expressible probability is `1/24` ≈ 4.2%; a 1% jackpot can no longer be
configured. This is accepted, not overlooked. A sub-1% prize is precisely the
kind of invisible sliver that made the wheel feel dishonest, and the honest
alternative — a smaller prize on a visible slice — is available.

### The house-edge guardrail survives, unchanged in kind

The time-axis guardrail from ADR-0012 is untouched: a spin's expected time
payout, including free-spin chaining up to the cap, must stay strictly below the
redemption value of the one point it costs, measured against the most generous
Redemption Tier. The points axis remains deliberately unguarded
(ADR-0046). Only the probability distribution feeding the expectation changed,
from weighted to uniform.

### The guardrail is checked on load, not only on save

The **spin path already refuses** a negative-edge configuration — it denies with
`wheel_spin_denied` / `negative_house_edge`, charges nothing, and mints nothing.
That behaviour predates this ADR and is unchanged; it is restated here because
this ADR makes it load-bearing in a way it was not before.

It was previously a defence against a configuration that no reachable path could
produce, since every saved wheel had passed `ensure_positive_house_edge`.
Flattening weights on migration breaks that assumption: an existing wheel can
now cross into negative edge without anyone pressing Save, so the spin-path
check becomes the thing that actually fires.

What this ADR adds is the owner-facing half: the **Wheel tab warns on load**,
not only on save, naming the problem. Without it a shop would discover the
migration cost them their edge only by noticing customers can no longer spin.

A wheel that goes dark until the owner retunes it is the correct failure
direction for a money bug. The alternative — quietly paying out more than a
point is worth — is the money printer ADR-0012 exists to prevent.

### Slices state their own prize

Each slice renders two lines: the owner's free-text `label`, and beneath it a
**derived prize line** computed from the segment's `seconds` and `points_centi`
— `5 min`, `2.00 pts`, `10 min + 2.00 pts`, `Free spin`, or `—` for nothing.

The label is kept, and stays free-text flavour (`JACKPOT`, `Swerte!`). It can
disagree with the prize; the derived line cannot, and sits next to it. The
dishonesty being removed here is *hidden odds*, not owner voice.

### Migration

Weights are dropped from the schema. Existing segments become equal slices in
place, which changes the odds of every configured wheel. Two cases:

- A wheel still matching the seeded segment ids from migration 0025 is
  **untouched by its owner** and carries no configuration worth preserving. It
  is replaced with the new equal-slice default.
- Any edited wheel is **flattened as-is** and left alone. The owner retunes it,
  warned by the load-time banner if the flattening cost them their edge.

The new default is twelve slices — Try again ×5, 5 min ×3, 10 min ×2, 15 min
×1, Free spin ×1 — expecting 270.8s per spin against a 1,440s point value, an
81.19% house edge.

### Duplicate slices are interleaved, not grouped

Segment order is now a visual decision, because duplication is the odds
mechanism. Five adjacent "Try again" slices would draw one contiguous 150°
losing wedge — visually indistinguishable from the weighted wheel this ADR
removes, and just as discouraging. The seeded default therefore **alternates**
losing and winning slices around the circle.

This is a property of the seeded default only; nothing forbids an owner from
grouping duplicates. It is recorded because the reason for the ordering is not
recoverable from the data.

## Consequences

- `WheelSegment.weight` is removed from the core type, the `wheel_segments`
  table, and the client/owner API payloads. `WheelError::InvalidWeight` and
  `WheelConfig::total_weight` disappear; `free_spin_weight` becomes a count.
- Segment selection is a uniform draw over `0..n` rather than a cumulative walk.
- This is a breaking API and schema change to a shipped feature. Admin and
  client version-bump together, but the Rust deserializers ignore unknown fields
  so a mid-upgrade LAN running an older client degrades rather than errors.
- Owners upgrading with a customised wheel will see their odds change and must
  retune. The load-time banner and the spin-path refusal make an unsafe outcome
  loud rather than silent.
- ADR-0012's "still open" note about the segment/probability editor is closed by
  removal: there are no probabilities left to edit.
- The glossary entry for **Wheel Segment** drops "a probability weight".

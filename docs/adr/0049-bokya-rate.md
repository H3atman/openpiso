# ADR-0049: Bokya Rate — hidden server-side landing bias

- Status: Accepted
- Date: 2026-07-28
- Deciders: shop owner (product)
- Supersedes: [ADR-0047](0047-equal-slice-wheel-segments.md) (in part: the
  guarantee that the drawn geometry is a complete and truthful statement of
  the odds)
- Relates to: [ADR-0012](0012-spin-the-wheel.md) (time-axis house-edge
  invariant), [ADR-0046](0046-spin-the-wheel-points-prize.md) (unguarded points
  axis)

## Context

ADR-0047 is only two days old (2026-07-26). Its central promise was explicit:
the wheel's equal geometry would be a complete and truthful statement of the
odds. The owner has now chosen a hidden draw bias instead: the wheel must keep
looking even while a server-side setting fixes how often a landing is Bokya.

ADR-0047's stated cause is preserved intact: **a wheel that looks rigged does
not get played, and an unplayed wheel earns nothing**. The equal slices still
make the wheel look even. Its stated principle is not preserved. The drawn
geometry is no longer necessarily the odds.

This is an honesty trade, not a technical refinement. The cause is preserved;
the principle is inverted.

## Decision

### Bokya is derived from the prize

A **Bokya** landing awards no time, no points, and no Free Spin. Bokya is
derived from a Wheel Segment's prize pair and free-spin flag and is never
stored as a separate segment property. A segment awarding only a Free Spin is a
win, not Bokya.

### The owner may set a hidden Bokya Rate

**Bokya Rate** is the optional owner-set integer probability, from 0 through
10,000 basis points inclusive, that any single landing is Bokya. When it is
unset, the server draws uniformly across all Wheel Segments, preserving
ADR-0047's truthful-geometry behavior.

When it is set, the server first resolves whether the landing is Bokya using
the configured probability, then draws uniformly among the segments in the
selected group. A positive rate requires at least one Bokya segment; a rate
below 10,000 requires at least one non-Bokya segment. Invalid configurations
are refused by the same save and spin-path boundaries that protect the wheel's
economics. The RNG and outcome remain server-authoritative; the client only
animates the already-resolved segment.

The setting is not represented in slice size. Every slice remains exactly
`1/n` of the circle. Therefore, with a Bokya Rate set, the displayed wheel no
longer guarantees a complete or truthful statement of the landing odds, even
though a particular configuration can coincide with its visible proportions.
This inversion is intentional and must not be softened into language about
merely tuning or presentation.

### What survives from ADR-0047

ADR-0047 remains authoritative for:

- **Equal slice geometry:** every Wheel Segment occupies exactly `1/n` of the
  circle.
- **Duplication within a group:** repeated segments remain the mechanism for
  distributing probability among prizes inside the Bokya or non-Bokya group.
- **Derived prize line:** every slice still renders the prize derived from its
  seconds, centipoints, and free-spin flag; its label cannot conceal the prize.
- **Load-time house-edge warning:** an unsafe configuration is still made loud
  before the owner attempts a spin or save.

ADR-0047 no longer governs the complete landing distribution when Bokya Rate is
set. Its guarantees that every segment is `1/n` likely and that geometry tells
the whole truth are superseded.

### Existing economic guardrails remain

ADR-0012's time-axis house-edge invariant is unchanged **in kind**: expected
time payout, including Free Spin chaining, must remain strictly below the value
of the point spent, measured against the most generous Redemption Tier. Its
calculation must use the effective draw distribution, including Bokya Rate when
set. Save-time enforcement, spin-path enforcement, and ADR-0047's load-time
warning remain.

ADR-0046's points axis remains unguarded. Bokya Rate does not introduce a cap,
warning, or validation based on expected points payout; **Points Minted by
Wheel** remains the compensating control.

## Rejected alternative

### Target percentage translated into duplicated Bokya slices

An editor could accept a target percentage and automatically duplicate Bokya
segments until the equal-slice wheel approximated it. This would keep the wheel
perfectly honest: geometry and draw odds would still agree.

It lost for two reasons. With ADR-0047's 24-segment cap, probability has a
roughly `1/24` or **4.2% granularity floor**, so many requested rates cannot be
represented closely. Reaching a substantial house advantage also visibly
crowds the wheel with repeated "Try again" slices, recreating the appearance
problem ADR-0047 was written to solve. The owner chose exact hidden control and
an even-looking wheel over truthful geometry.

## Consequences

- Schema, core, API, and UI use the terms **Bokya** and **Bokya Rate** exactly as
  defined in the glossary.
- Bokya classification is derived, never persisted; changing a segment's prize
  can move it between draw groups.
- Unset Bokya Rate preserves a uniform draw and ADR-0047's original odds.
- Set Bokya Rate deliberately removes the guarantee that equal visual geometry
  matches actual landing probability.
- Wheel safety math and warnings must use the effective biased distribution,
  while the points axis remains deliberately unguarded.

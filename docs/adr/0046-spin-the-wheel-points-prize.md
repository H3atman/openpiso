# ADR-0046: Spin the Wheel — points become a legal prize

- Status: Accepted
- Date: 2026-07-25
- Deciders: shop owner (product)
- Supersedes: [ADR-0012](0012-spin-the-wheel.md) (in part: the "never points" prize rule)
- Amends: [ADR-0003](0003-points-economy.md) (wheel prizes now include points)

## Context

ADR-0012 (§Mechanics) explicitly forbids points as a wheel prize: *"Prizes:
time amounts, 'win nothing,' and 'extra free spin.' Never points. The wheel
cannot feed itself with points."* That rule was a deliberate guardrail during
the initial points rollout — a conservative stance while the economy was
untested.

The shop owner has now reversed that rule knowingly. Points are a legal prize
on the wheel. The wheel *can* feed itself with points. This ADR records the
reversal, the deliberate asymmetry between the points axis and the time axis,
and the compensating control that replaces the removed gate.

The time-axis guardrail (the house edge measured against the most generous
Redemption Tier) survives unchanged — it was always the stronger constraint
and is independently necessary.

## Decision

### Points are a legal prize

A Wheel Segment may award **points** (alongside time and free spins). The
wheel can feed itself with points: a points prize is deposited directly into
the account's points balance, and those points may be spent on further spins.

Four prize types now exist:
- **Time** — added to the account's Remaining Time (integer seconds).
- **Points** — added to the account's centipoint balance.
- **Nothing** — the spin is lost; no prize awarded.
- **Free Spin** — a spin that does not deduct from the account's point
  balance; chains up to the Free Spin Chain cap.

There is no validation, no warning, and **no cap** on how much of a spin's
expected value comes from points. An all-points wheel that returns >1.00
expected points per spin is legal if the house-edge invariant still passes.

### Asymmetric guardrails — deliberate

The **time axis** has a guardrail (house edge against the most generous
Redemption Tier), and the **points axis** has none. This asymmetry is
deliberate:

- The time guardrail exists because time is the shop's core product and its
  redemption rate is the economic floor. A wheel that prints time undercuts
  every time-coin purchase and every Redemption Tier, making those paths
  irrational.
- Points won on the wheel are **not** folded into the time guardrail's
  computation. Doing so would make the guardrail block points-heavy
  configurations — exactly what the owner rejected. Points are a loyalty
  currency; their creation by the wheel is a marketing cost, not a threat to
  the shop's time-pricing floor.

The compensating control is **reporting**: the **Points Minted by Wheel**
report line (see [ADR-0014](0014-reporting-model.md)) tracks how many centipoints
the wheel has created. The owner monitors this as an early-warning signal. No
automatic cap or gate is imposed.

### Surviving decisions (unchanged)

The following ADR-0012 decisions remain authoritative and are restated here so
they are not re-litigated:

- **Server-authoritative outcome.** One spin = one server-side transaction:
  deduct the point, roll the RNG on the server, resolve the entire (capped)
  free-spin chain, credit the won time and/or points to the account, write the
  audit row. The client only animates a result the server already chose.
- **Fixed 1.00 point cost.** One spin costs exactly 100 centipoints
  (1.00 point). No variable betting.
- **Capped Free Spin Chain.** Free spins chain but are capped at a configurable
  maximum number of consecutive free spins, then forced stop. The cap bounds
  both the EV math and a lucky customer's runaway streak.
- **Master on/off.** The owner can enable or disable the wheel at any time from
  settings.
- **House edge against the most generous Redemption Tier.** The invariant is
  measured against the most generous Redemption Tier's value of a point.
  A spin's expected time payout, including free-spin chaining up to the cap,
  must be less than the redemption value of its cost. The settings screen
  computes the live house-edge % and warns/refuses when it goes negative.
- **Benchmark derived from the current tier set.** Editing Redemption Tiers can
  change the wheel's house edge and the owner is warned that editing tiers
  affects the wheel.
- **Every spin is audited** — account, point spent, outcome type(s), time won,
  points won, free spins triggered — feeding reporting (wheel outcomes are a
  time/points giveaway = cost line).

### Compensating control

The **Points Minted by Wheel** report line (ADR-0014) is the compensating
control for the removed "never points" gate. It tracks total centipoints
awarded by the wheel across all spins in a given period. The owner monitors it
alongside **Points Liability** (total unredeemed points) and **Free Time
Dispensed** as an early-warning signal for runaway point creation.

## Consequences

- Wheel audit events carry one net `points_delta_centi` (winnings minus the fixed cost); the linked `wheel_spins` detail records gross `points_won_centi` for reporting.
- The "Points Minted by Wheel" report line is added to the reporting model
  (ADR-0014).
- ADR-0012's "never points" rule is superseded; the rest of ADR-0012 remains
  authoritative.
- The glossary is updated: "Spin the Wheel" no longer says "never points";
  "Wheel Segment," "Points Minted by Wheel," "Spin Duration," and "Free Spin
  Chain" are new entries.
- The "Still open" entry for the wheel segment/probability editor UI is closed
  — the editor is now an implementation detail of the surviving ADR-0012
  economics.

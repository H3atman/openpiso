# ADR-0012: Spin the Wheel — mechanics & economics

- Status: Accepted (prize rule superseded in part by ADR-0046; segment weights
  superseded by ADR-0047; equal-likelihood guarantee superseded by ADR-0049)
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0003 (points economy), ADR-0005 (client is untrusted).

> **Superseded in part by [ADR-0046](0046-spin-the-wheel-points-prize.md):**
> the "never points" prize rule below is reversed — points are now a legal
> prize. The time-axis guardrail (house edge against the most generous
> Redemption Tier), server-authoritative outcome, fixed 1.00 point cost,
> capped Free Spin Chain, and master on/off are **unchanged** and remain
> authoritative.
>
> **Superseded in part by [ADR-0047](0047-equal-slice-wheel-segments.md):**
> segments no longer carry probability weights and every segment occupies `1/n`
> of the circle. Every segment is also `1/n` likely only while Bokya Rate is
> unset; [ADR-0049](0049-bokya-rate.md) supersedes that guarantee when the rate
> is set. References below to "segments + probabilities" and to the
> segment/probability editor as a still-open question are closed by removal. The
> house-edge invariant itself is unchanged, but is now enforced on the spin path
> as well as on save.

## Context

ADR-0003 established the wheel as a points→time gamble with a configurable
house edge. This ADR pins the mechanics and the economics. The danger mirrors
ADR-0003: a plausible-looking prize wheel can have a *negative* house edge and
become a money printer, especially because a "free spin" prize chains.

## Decision

### Mechanics
- **Cost: fixed 1 point = 1 spin.** No variable betting.
- **Prizes: time amounts, "win nothing," and "extra free spin." Never points.**
  The wheel cannot feed itself with points.
- **Free spins chain** (a free spin can land on another free spin) but the
  chain is **capped** at a configurable maximum number of consecutive free
  spins, then forced stop. The cap bounds both the EV math and a lucky
  customer's runaway streak.
- **Server-authoritative.** One spin = one server-side transaction: deduct the
  point, roll the RNG on the server, resolve the entire (capped) free-spin
  chain, credit the won time to the account, write the audit row. The client
  **only animates** a result the server already chose. Reason: the client runs
  on a customer-controlled machine (ADR-0005); client-side randomness is
  riggable.
- **Master on/off.** The owner can **enable or disable the wheel at any time**
  from settings.

### Economics (the governing invariant)
- The house edge is measured **against the most generous Redemption Tier's
  value of a point**: at the seeded default tier (10 pts = 4 hr) a point is
  worth **24 min guaranteed**.
- A spin's **expected time payout, including free-spin chaining up to the cap,
  must be LESS than the redemption value** of its cost. Otherwise customers
  spin instead of redeem and the wheel re-opens the money printer.
- The settings screen computes the **live house-edge %** from segments +
  probabilities + free-spin slot + chain cap, and **warns/refuses** when it goes
  negative — the same class of guardrail as ADR-0003's effective-bonus %.

### Coupling (must be honored)
- The benchmark is **derived from the current most generous Redemption Tier**.
  Editing tiers can change the wheel's house edge. The guardrail must recompute
  from the live tier set, never a hard-coded value, and the owner is warned that
  editing Redemption Tiers affects the wheel.

## Consequences / still open

- Every spin is audited (account, point spent, outcome, time won, free spins
  triggered) — feeds reporting (wheel is a time giveaway = cost).
- Segment/probability editor UI and the exact RNG source are implementation
  details.

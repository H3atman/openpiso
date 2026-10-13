# ADR-0060: Physical ₱20 calibration capture

- Status: Accepted
- Date: 2026-08-06
- Deciders: shop owner (product)
- Relates to: ADR-0023 (serial coin input), ADR-0043 (sampling and
  observability), ADR-0024 (Web Admin owner surface)

## Context

A Philippine ₱20 coin was inserted but was not recognized. The serial
acceptor emits device-programmed pulses; denomination cannot safely be inferred
from the coin's face value. A fresh installation may therefore offer `20 →
2000` cents as a candidate, but an existing shop must not have its calibrated
map changed silently.

Inspecting a copied debug bundle is too error-prone for calibration. Opening a
second serial handle is unsafe because Windows COM access is exclusive and it
would create a second Set Pin authority.

## Decision

- Fresh installs receive the candidate `20 pulses → 2000 cents` mapping through
  migration 0038. The migration runner captures whether history was empty
  before applying migrations. Existing installations, including custom maps,
  are untouched.
- Web Admin provides an owner-authenticated, local-serial guided capture.
  It shares the live Pulse Sampler and Terminal Selection mutex; it never
  opens a second COM handle and rejects the Simulator, remote devices, active
  Selection Windows, pending batches, and buffered production pulses.
- A capture has a target amount in integer cents, defaults in the UI to ₱20,
  and collects five individually operator-ended non-empty samples. The Set
  Pin is inhibited before each sample is taken and between samples; the owner
  explicitly arms each next sample. Only five equal pulse counts produce an
  Apply proposal.
- Capture is diagnostic-only. It creates no Coin Transaction, Guest Time,
  Account balance, Points, Coin Promo, or Coin Trace Id. Apply is an explicit
  owner action that changes only the pulse map and writes an audit event.
- A capture expires after 60 seconds of inactivity or five minutes absolute.
  Source loss, process restart, cancellation, and expiry are fail-closed and
  discard incomplete pulses. Settings reconfiguration is rejected while
  recording. A completed result remains in memory until Apply, discard, or a
  new explicit attempt.
- Existing batch semantics remain unchanged: a rapid pair of ₱10 coins may
  merge into one 20-pulse batch. The owner must validate the production quiet
  gap from bench evidence; it is not changed automatically.
- The owner surface shows **Acceptor Health** for the current Coin Debug
  Window. S1 reports timing as `unavailable_until_filtered` with a null
  **Implausible Timing** rate; it does not show per-batch unmapped rows.

## Consequences

- A customer can use the normal authoritative coin path after the owner has
  physically calibrated and applied the mapping. Pricing, seconds, Points,
  and exact Coin Promo matching need no ₱20-specific branch.
- Existing installations require an explicit owner calibration and save. The
  one-time incident remedy remains a non-cash Manual Adjustment with reason
  `Unread ₱20 coin`; it does not fabricate revenue, Points, or Promo.
- Automated tests prove migration, locking, map validation, lifecycle, and
  fail-closed boundaries. Production release additionally requires physical
  evidence: five consistent ₱20 trials and documented ₱10 behavior.

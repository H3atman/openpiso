# ADR-0071: Event-driven coin reader with a heartbeat, and a filter that lives in it

- Status: Accepted
- Date: 2026-08-13
- Deciders: shop owner (product)
- Refines: [ADR-0043](0043-coin-ingestion-sampling-and-observability.md) (the
  30 ms Pulse Sampler and its observability), [ADR-0023](0023-serial-coin-input-and-terminal-selection.md)
  (the serial coin input)
- Relates to: [ADR-0016](0016-diagnostic-logging.md) (where the stall record
  lands), [ADR-0033](0033-typed-coin-source-trust-and-coin-outcome.md),
  [ADR-0070](0070-credit-the-pulse-sum.md) (the two fault detectors a stall
  falsifies), [ADR-0072](0072-credit-the-straddling-train.md) (the Set Pin
  write this reader must not block),
  [ADR-0073](0073-owner-coin-surface-is-a-report.md) (the refusal and health
  surfaces)
- Evidence: [`coin-acceptor-characterization.md`](../coin-acceptor-characterization.md)
  §4, §5

## Context

The shipped Pulse Sampler is `thread::sleep(PULSE_SAMPLER_POLL_MS)` at
`coin_source.rs:1043` — a **fixed sleep with no deadline correction**, so the
real period is 30 ms plus the loop's work plus any stall, and it drifts.

That makes the sampler's problem structural rather than occasional. It is not a
continuous reader that occasionally goes blind: **it is blind ~30 ms out of
every ~30 ms, permanently, by design.** The characterization's 45 ms safe-blind
budget was measured against a bench tool watching the line at 63.7–68.2 kHz, so
that budget **never applied to this reader at all**.

The poll was not merely coarse. It was coarse *and silent about being coarse*,
and the silence is the half that made it undetectable for a year: the reader
cannot distinguish "the line was high" from "I was not looking", so it takes the
hold-the-last-level policy by omission — precisely the policy that cannot see
its own failure. Its code comment justifies the coarse period as letting
"narrow floating-line noise spikes (a few ms)" fall between samples; the spikes
are 0.08 ms and they ride *inside* real pulses, so the poll aliases them into
over-counts during a genuine coin.

OpenPiso also runs **no filter at all** today: `PulseObservation::observe`
(`coin_source.rs:400`) counts raw rising edges of the normalized level. The
two-sided filter is something the characterization designed and this codebase
has never had.

## Decision

### The principle

> **A reader must represent "I was not looking" explicitly, and must never let
> a consumer mistake it for "the line was idle."**

### 1. The reader is event-driven, on a directly-opened overlapped handle

Normally an ADR states a property and leaves the mechanism to the implementer.
Here that would send the work into a wall, so the mechanism is named and the
obstacle is costed:

- `serialport` 4.9 has **no comm-event support whatsoever** — no `SetCommMask`,
  no `WaitCommEvent`, no `EV_DSR`.
- It does implement `AsRawHandle` (`windows/com.rs:189`), so Win32 is reachable.
- But it opens the port **synchronously** — `CreateFileW(..., FILE_ATTRIBUTE_NORMAL, ...)`
  at `windows/com.rs:65`, with no `FILE_FLAG_OVERLAPPED`.

**Adopted: open the coin port directly with `FILE_FLAG_OVERLAPPED` and stop
using the crate for this port.**

**Rejected: FFI on the crate's existing raw handle.** It looks like the cheap
route and it is a trap. A synchronous handle gives `WaitCommEvent` **no
timeout**, which decision 3 requires; and the wait **blocks every other
operation on the handle**, including the **Set Pin** write — which ADR-0072
requires to work *during* a pulse train. Cancellation would also depend on
`SetCommMask(h, 0)` from a second thread, a shutdown path nobody wants to own.

**A risk considered and discounted: the chatter storm.** Chatter runs at ~1 kHz
*inside* a pulse, so an event reader looks like it should drown. It does not.
`WaitCommEvent` **coalesces** — it reports "DSR changed since the last wait",
not each change — so a chatter burst costs one wake, not a thousand. Missing
chatter edges is harmless by construction: `min_high` exists to delete them.
Real edges are ~100 ms apart, and the bench measured peak 1570 edges/s against a
loop sustaining 68 kHz.

**The payoff:** the ratified filter parameters ship **unchanged**, and the 45 ms
erasure bound becomes a real margin against a rare stall instead of a number
describing the reader's permanent condition.

### 2. No fallback — refuse the coin path loudly

When comm events are unavailable (a USB-serial adapter that does not report
`EV_DSR`), OpenPiso **refuses to run the coin path and says why**. It does not
quietly degrade to a poll.

The reason is not purity. A silent fallback means shipping **two** filter
parameter sets forever, and a machine running unratified values with nobody
aware it happened — the same class of failure as the incumbent: a reader in a
worse mode than anyone believes, saying nothing. The owner-facing wording,
placement, and its distinguishability from "acceptor unplugged" are ADR-0073's.

### 3. A heartbeat, at N = 20 ms

An event reader cannot detect its own stall the way a poll can. A poll compares
sample timestamps; an event reader has no regular samples, and **silence is the
normal state of an idle line** — so it cannot tell "nothing happened" from "I
was asleep", which is the exact confusion the principle bans.

So: wake at least every N ms with no event pending, and flag a wake that arrives
late. The wait timeout is **the next filter deadline, or N, whichever is
sooner**. This is the second independent reason decision 1 rejects the
synchronous handle.

**N = 20 ms**, and the reasoning matters more than the number, because N's job is
narrower than it looks. During a **pulse train** the filter's own deadlines fire
every 28–45 ms anyway, so the reader is densely covered and a stall there is
caught regardless of N. **N governs the idle line only.** An idle stall erases
nothing and costs no money — but this reporting surface is the *only*
measurement of the runtime's scheduler that will ever exist, so it must not
sample dishonestly. At 20 ms a 45 ms stall always misses at least two heartbeats
and is unambiguous. At 100 ms a 45 ms idle stall hides entirely inside one
interval, and the reported rate would count only stalls that landed during
coins — the sample that makes a sick machine look healthy.

Cost at idle: 50 wakes/s, against the 33/s of the 30 ms poll it replaces.

### 4. The two-sided filter moves into the reader, before `observe()`

Parameters are `min_high` **45 ms** / `min_low` **28 ms** (characterization §4):
absorb any HIGH shorter than `min_high`, then reject any surviving LOW shorter
than `min_low`. `observe()` then receives clean pulses and its edge counting is
left alone.

**Rejected: filtering downstream in the batcher.** The batcher sees counts. By
the time a batch exists, the **pulse widths** the drift alarms need are gone.

Two properties are stated outright, or they will later be read as faults:

- **The filter is a state machine with timers, not a per-sample test.**
  Rejecting a LOW shorter than `min_low` means *waiting* 28 ms before declaring
  a pulse; despiking means holding a HIGH for 45 ms before believing it. This is
  what supplies decision 3's deadline wakeups.
- **Every pulse is reported up to ~45 ms after it truly ended.** Against a
  600 ms quiet gap and a 4008 ms ₱20 train that is negligible — but it is
  designed behaviour with a stated budget, not latency to be optimized away.

### 5. The width the drift alarm reads is the filter's own LOW span

`high_since` (`coin_source.rs:373`) and its falling-edge log are **deleted**, not
fixed. Today it measures the 105.4 ms inter-pulse *gap* as the "pulse width".
ADR-0073's polarity redefinition would, by itself, *fix* it — which is the
problem: the codebase would then hold **two** correct width measurements, one at
sampling resolution and one from the filter that already validated the span
against `min_low`/`min_high`. Two copies of a measurement drift apart. The
filter's is authoritative, because it is the span the parameters were chosen
against and the span decision 6's taint rule attaches to.

### 6. A stall taints the measurement, never the credit

**Logging the stall is not enough.** A logged stall never joined to the
measurement it corrupted leaves a wrong width sitting in the alarm's input, with
the alarm unable to know. That is a decorative log.

**When a blind window touches a batch, that batch's pulse-width and
inter-pulse-timing samples are excluded from the drift-alarm statistics. Credit
is untouched** (per ADR-0070: the gate reports, it never refuses).

**Boundary rule: taint when the blind window intersects the batch span widened
by `min_high` at each end** — `first_pulse_start − 45 ms` to
`last_pulse_end + 45 ms`. The obvious rule ("the window overlaps the batch") is
subtly wrong: the filter's verdict on the first and last pulse depends on the
line for up to `min_high` *outside* the batch's own span, so a stall landing
30 ms ahead of the first pulse still changes that pulse's measured width. The
margin is a constant the filter already carries, so it adds **no tuning
freedom**. The bench archive confirms the widened span is the right one: the
one width outlier's corrupting blind sat 6 ms outside the filtered pulse
boundary.

Why this matters at all after ADR-0070: an erased pulse used to cost the whole
coin (10 → 9 → unmapped → ₱0) and now costs ₱1. But ADR-0070 leaves exactly
**two** fault detectors, and a stall silently falsifies the input to both. An
alarm that cannot be believed is worse than no alarm, because it is trusted.

### 7. Report a rate **and** a maximum

Surface stalls per hour, the **worst blind window**, and the count of tainted
batches, alongside ADR-0070's `ImplausibleTiming` rate.

The maximum carries as much weight as the rate, for a reason specific to this
failure: erasure is a **threshold** effect, not a gradual degradation. A machine
at a 44 ms worst case is fine and a machine at 46 ms is losing coins, and no
rate distinguishes them.

Note the threshold's honest shape, from the characterization §5: 45 ms is a
**worst case over blind position and UNKNOWN policy, not a trigger that fires
when crossed**. The bench saw a 51.5 ms blind land *inside* a pulse and the
pulse survived. The number belongs in the report as a bound to watch, never
quoted bare as "coins are lost past 45 ms".

## Rejected alternatives

- **Do nothing; the filter with margin is the whole fix.** The budget the margin
  was measured against never governed this reader.
- **Keep the polled shape and ratify polled filter values.** The cheap route —
  one const change — and it was overruled deliberately. It would ship a second
  parameter set forever and leave the reader permanently inside the blind
  condition the margin was supposed to cover. **No polled parameter set has ever
  been ratified**, and none should be.
- **Detect and refuse — treat a stalled batch as unmapped.** That is the current
  failure outcome made deliberate, and it is exactly the direction ADR-0070
  reverses.
- **Heartbeat at 100 ms** to save wakes. It hides a 45 ms idle stall entirely and
  biases the reported rate toward healthy.
- **Bench-measure the poll loop's stall tail first.** It measures a PowerShell
  loop on the bench box, a proxy for a program nobody ships; this runtime is Rust
  in Tauri with no GC. Decisions 3 and 7 make OpenPiso measure its own tail in
  production, which is strictly better evidence (characterization §9.3).

## Consequences

- The coin port is no longer opened through `serialport`; that dependency stays
  for any other port and the coin path owns raw Win32 open/close, overlapped I/O,
  and cancellation.
- ADR-0043's 30 ms sampling constant is retired. Its observability intent
  survives and is strengthened: the reader now reports its own blindness.
- `high_since` and the falling-edge width log are removed.
- Pulse reporting gains ~45 ms of designed latency.
- The coin path can now be **unavailable on hardware where it used to appear to
  work**. That is intended, and ADR-0073 owns making it legible.
- This is the largest of the three remediation stages and the last (S3). The
  plan states in its own voice that S2 — the filter and the owner surface — must
  not be deferred merely because it buys little money, or S3 arrives with no
  detectors to feed.

## Verification

Tests cover: a simulated 45 ms blind window during a train marking the batch
tainted while crediting it unchanged; the taint boundary firing for a blind
window 30 ms *before* the first pulse; a chatter burst producing one wake and
zero counted pulses; a late heartbeat raising a stall record with its measured
duration; comm events unavailable refusing the coin path with the
distinguishable cause; and the filter's ~45 ms reporting delay asserted as a
bound rather than incidentally.

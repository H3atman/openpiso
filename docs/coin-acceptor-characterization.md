# Coin Acceptor Characterization

> **This document is measurement, not decision.** Every number here was
> measured on the bench against a CH-926-family acceptor. Raw traces are not
> distributed with this repository (§10). The decisions these numbers drove live in
> ADR-0070 through ADR-0073. Numbers
> do not carry a `Status:` and are not up for revision — only re-measurement.

- Measured: 2026-07-25 (first probe) and 2026-08-12 (full protocol, 117 trials)
- Unit: CH-926 family coin acceptor, 12 V, on a native motherboard UART
- Evidence: 5 capture sessions, 1464 s of line at 0.0222 % blind, 705 pulses,
  65 individual coins, ₱1,825 of coin inserted

---

## 1. Scope, and how to read this

### What is measured vs what is decided

| | Lives here | Lives in an ADR |
|---|---|---|
| Pulse width, period, quiet gap, train length | ✔ | |
| Filter model and its parameters (45 / 28) | ✔ | |
| Erasure threshold and its dependencies | ✔ | |
| Negative results | ✔ | |
| Credit the pulse sum instead of a `pulse_map` lookup | | ADR-0070 |
| Event-driven reader, no polled fallback | | ADR-0071 |
| Credit a train that straddles inhibit | | ADR-0072 |
| The owner coin surface is a report, not a control panel | | ADR-0073 |

If you are implementing, read §2 → §3 → §4 → §5 in order, then the ADR for the
component you are touching. If you are about to propose tuning something, read
§6 first — five things this document rules out are things people reliably
propose.

### Defaults are not calibration

`PULSE_SAMPLER_POLL_MS = 30` and `inter_coin_gap_ms = 600` are conservative
defaults chosen for typical pulse coin acceptors. The quiet gap is
operator-adjustable; the polling period is a code constant. These measurements
do not substantiate bench validation of either default. Characterize the
installed acceptor before customer use.

The measured noise spikes are **0.08 ms** and occur inside real pulses, not
only at idle. A coarse poll is not a validated noise filter; see the measured
filter model and polling losses below.

---

## 2. Preconditions

Every number below is conditional on this topology. State it, and detect
departure from it — the risk this document guards against is not a wrong
decision, it is **drift**: a slide switch on the side of the unit and values in
the acceptor's memory, both changeable without touching software.

### Documented (belongs in the wiring diagram)

- **Coin Pin = DSR**, read by the server.
- **Set Pin = RTS**, driven by the server. **RTS asserted enables** the
  acceptor; deasserted inhibits. "Active low" is folklore.
- **DTR is the pull-up.** It is tied to the same node as DSR and the acceptor's
  COIN wire, and it is what puts a valid +10 V on the idle line. **DTR must be
  asserted for the Coin Pin to read coins at all** — with DTR low the line
  floats in the RS-232 undefined band, chatters continuously, and real coin
  pulses never register. `assert_pullup_dtr` (`coin_source.rs:1510`) already
  does this at every open, and is correctly skipped when DTR is itself the Set
  Pin.
- **Common ground** across PC, acceptor, and the 12 V supply.
- **Acceptor slide switches: N.O. and Slow (100 ms).**
- **1 pulse per peso** taught into the acceptor.

### Detectable at runtime

| Precondition | Detector | Signature of a fault |
|---|---|---|
| Speed switch in Slow | Acceptor Verification's width band (ADR-0073) | measured width ~30 ms instead of ~100 ms |
| DTR pull-up present | idle edge rate on a no-coin window | **any** nonzero edge count at idle |
| Native UART, not a USB-serial adapter | host port enumeration: `ACPI\PNP0501` vs `usbser` / `ch341` / `ftdi` / `silabser` | adapter present |

An adapter **taints, it does not refuse** — see §9.

---

## 3. The measured envelope

The Coin Pin **idles HIGH (10.03 V measured) and a coin pulse pulls it LOW.**
All figures below describe the LOW span as the pulse.

### Per denomination — 116 valid trials, 2026-08-12

| Denom | Coins | Pulses | Width min / **med** / max (ms) | sd | Period min / **med** / max (ms) | sd |
|---|---|---|---|---|---|---|
| ₱20 | 25 | 500 | 94.284 / **100.313** / 101.064 | 0.293 | 199.710 / **205.650** / 206.348 | 0.295 |
| ₱10 | 15 | 150 | 99.336 / **100.317** / 100.934 | 0.131 | 204.764 / **205.682** / 206.609 | 0.158 |
| ₱5 | 9 | 45 | 100.070 / **100.303** / 100.329 | 0.041 | 205.469 / **205.594** / 205.802 | 0.087 |
| ₱1 | 10 | 10 | 100.285 / **100.304** / 100.682 | 0.114 | — | — |

**Headline figures:**

| Quantity | Value |
|---|---|
| Pulse width (LOW) | **100.31 ms**, sd 0.02 across denominations |
| Inter-pulse period | **205.7 ms** |
| Intra-train quiet gap (HIGH) | **105.4 ms** |
| ₱20 train duration | **4008 ms** (20 pulses) |
| Pulses per peso | **1**, confirmed at volume |
| Fast-mode width, for contrast | **30.09 ms** — gap stays 105.40 ms |

**The pulse width is a hardware constant, not a distribution.** Medians span
100.303–100.317 ms across four denominations, 705 pulses, and 65 individual
coins — 0.014 ms between denominations. Coin-to-coin spread within ₱20 is the
same story (25 coin medians spanning 100.302–100.325 ms), and repeat
insertions of the same coin land inside the spread of the singles. **There is
no coin-dependent width.** This is what makes a width-based drift alarm viable:
a fast-mode unit reads 30.09 ms against a population that never leaves
100.31 ± 0.02.

**Every width outlier in the archive is a capture artefact, not a coin.** The
94.284 ms minimum is trial `A01` pulse 1, with a 6.024 ms blind window sitting
on its leading edge; the ₱10 minimum of 99.336 ms is the same mechanism. See
§5.

### Pulses per peso, confirmed at volume

| Block | Trials | Expected pulses | Result |
|---|---|---|---|
| ₱20 single | 25 | 20 | 20 on all 25 |
| ₱10 single | 15 | 10 | 10 on all 15 |
| ₱5 single | 9 | 5 | 5 on all 9 |
| ₱1 single | 10 | 1 | 1 on all 10 |
| ₱20→₱10 | 15 | 30 | 30 on all 15 |
| ₱10→₱20 | 15 | 30 | 30 on all 15 |
| ₱10+₱10 | 10 | 20 | 20 on all 10 |
| ₱1+₱1 | 10 | 2 | 2 on all 10 |
| foreign object | 5 | 0 | 0 on all 5 |
| no-coin baseline | 2 | 0 | 0 on both |

The acceptor rejects an untaught coin **silently** — no partial train, no
chatter, nothing.

### Fast mode, and why it is not used

The acceptor's pulse width is switchable (20 / 50 / 100 ms; the bench unit's
fast position measures **30.09 ms**) but the **inter-pulse gap is fixed at
105.4 ms regardless** — identical to 0.1 ms in both modes. Fast mode is
therefore a **1.52× speedup**, not a 5× one: a ₱20 train goes from 4008 ms to
~2600 ms, still 4.3× over the 600 ms quiet gap. It buys nothing structural and
costs the entire stall margin (§5). See §9.

### The chatter

While the line is held LOW it emits ~0.08 ms HIGH / ~1.0 ms LOW chatter at
~1 kHz. This is **not acceptor noise** — the LOW level sits at ~0 V, inside the
RS-232 undefined band (−3 V to +3 V), so the receiver oscillates at its own
threshold. The HIGH level is a valid 10.03 V, so the idle line is clean and
silent. Chatter exists **only inside a pulse**, never at idle. On a correctly
configured line the idle phantom rate is **exactly zero**.

The ₱1 pulse additionally carries a **0.26 ms residual LOW, 59.4 ms ahead of
the real pulse** — a chatter burst on the leading edge. This single artefact is
why the filter must be two-sided (§4).

---

## 4. The filter

### The model

**Two-sided, applied to the raw level stream:**

1. **Despike** — absorb any HIGH shorter than `min_high`.
2. **Reject** — discard any surviving LOW shorter than `min_low`.

What survives is a pulse.

```
min_high = 45 ms
min_low  = 28 ms
```

Result over the full archive: **116 of 116 valid trials counted exactly**,
including all 25 ₱20 traces. Over every trace ever captured, `two_sided` scores
**100.00 % exact**.

### Why it must be two-sided

A despike alone is not sufficient. It merges the ₱1 precursor's 0.26 ms
residual LOW into the pulse's neighbourhood and yields **2** pulses for a ₱1
coin. The `min_low` half exists specifically to delete that residual. One
artefact, on one denomination, sets the shape of the filter.

### Why 45 / 28, and why the choice was not made on accuracy

**Three filter models tie at 100 % exact** — two-sided, matched-filter, and (on
narrow parameter diagonals) majority-vote. Accuracy does not discriminate, so
**plateau width chose the filter**:

| Model | Grid points exact | Shape |
|---|---|---|
| two_sided | **239 of 285** | one contiguous block, `min_high` 2–100 ms × `min_low` 0.3–100 ms |
| majority_vote | 11 of 80 | narrow diagonal |
| stable_for_n | 11 of 80 | narrow diagonal |

A parameter correct at exactly 12 ms and wrong at 11 and 13 is not a
recommendation. The two-sided filter's bounds are set by the **signal** — a
100.3 ms pulse sitting three orders of magnitude away from 0.08 ms chatter —
rather than by tuning, which is also why it is expected to generalize across
the 616/926 family.

**The matched filter loses despite scoring 100 %.** Its accuracy is bounded by
a peak-separation prior that collapses outside 60–190 ms — i.e. it is bounded
by *the period itself*, so a reprogrammed acceptor breaks it. It also cannot
measure a width, and it is non-causal.

### Why these particular values inside the plateau

Counting accuracy is flat across the plateau; the boundaries below are what
pick the point:

| Bound | Value | Set by |
|---|---|---|
| `min_high` upper limit for **boundary accuracy** | < 59.4 ms | above this, the despike absorbs the ₱1 precursor and shifts that pulse's timestamp 59.4 ms early |
| `min_high` chosen | **45 ms** | maximizes the safe blind window (§5) while staying under 59.4 |
| `min_low` band valid in **both** speed modes | [28, 29] ms | fast mode's pulse is 30.09 ms; at `min_low` 30 a fast ₱10 credits ₱0 |
| `min_low` chosen | **28 ms** | byte-identical to 30 on all 14 slow-mode trials, and correct on both fast-mode trials where 30 loses the coin — strictly dominant, not a trade |

`min_low` 28 is the **drift-tolerant** choice, not a licence to run fast mode.
Slow mode remains the supported configuration.

### Where the filter sits, and what it costs

The filter belongs **between the reader and edge counting**, not downstream in
the batcher — the batcher sees counts, and by the time a batch exists the pulse
widths the drift alarms need are gone. Two properties follow and must not be
mistaken for faults later:

- **It is a state machine with timers, not a per-sample test.** Rejecting a LOW
  shorter than `min_low` means *waiting* 28 ms before declaring a pulse;
  despiking means holding a HIGH for 45 ms before believing it.
- **Every pulse is reported up to ~45 ms after it truly ended.** Against a
  600 ms quiet gap and a 4008 ms ₱20 train this is negligible, but it is
  designed behaviour with a stated budget, not latency to be optimized away.

### Where the measured width comes from

**The width the drift alarms read is the filter's own LOW span.** The filter
already produces the exact interval it validated against `min_low` and
`min_high`; nothing downstream needs to re-derive it.

This **retires `high_since`** (`coin_source.rs:373`). That field exists to
measure "how long the line stayed asserted", and under the shipped
`NormallyOpen` pass-through it times the **105.4 ms quiet gap** instead of the
100.3 ms pulse — the wrong interval, which is why the width distribution the
code logs specifically to help owners size a debounce has never measured a
pulse. Note that the polarity redefinition (ADR-0073) would *by itself* make
`high_since` time the correct interval; that is not a reason to keep it. It
would still measure at sampling resolution, downstream of the component that
already knows the answer, and duplicating the measurement is how the two copies
drift apart. **Delete `high_since` and its falling-edge log; take the width
from the filter.**

The taint rule of §5 attaches to those same filter spans.

---

## 5. Blindness and erasure

### The principle

> **A reader must represent "I was not looking" explicitly, and must never let
> a consumer mistake it for "the line was idle."**

The shipped sampler is `thread::sleep(30 ms)` with no deadline correction
(`coin_source.rs:1043`). It is not a continuous reader that occasionally goes
blind — it is **blind ~30 ms out of every ~30 ms, permanently, by design**, and
silent about it. That silence is the half that made the defect undetectable for
as long as it was. The bench capture tool takes the opposite approach: every
gap ≥ 1 ms is written into the trace as a `BLIND` record with its duration and
last-observed level, so an analyzer must assume the level held *deliberately*
rather than by omission.

### The erasure threshold, and why 45 ms must never be quoted bare

A blind window that begins during chatter can split one pulse into two; a wide
enough blind window instead **erases** the pulse, because both remnants fall
under `min_low` and are rejected. At `min_high` 45 / `min_low` 28, splitting is
geometrically impossible and the measured safe blind window is **45 ms**.

**45 ms is a worst case over blind position and UNKNOWN-resolution policy — not
a threshold that fires whenever it is crossed.** Trial `A04` carried a
**51.509 ms** blind window that opened 24.5 ms *inside* pulse 1, and the pulse
survived:

| UNKNOWN policy | Pulses counted | |
|---|---|---|
| `hold` (last observed level carries) | **20** | correct |
| `assert` | **20** | correct |
| `idle` | 19 | **pulse lost** |

Which side of the threshold you land on depends on where the window falls
relative to the pulse and on how unknown time is resolved. State both, or the
number is misleading.

### The measured stall tail (bench capture tool, 117 trials, live signal)

| Statistic | Value |
|---|---|
| max | **51.509 ms** (trial `A04`) |
| p99.9 | 21.868 ms |
| p99 | 18.232 ms |
| median | 1.050 ms |
| trials over the 40 ms discard bar | **1 of 117** (0.85 %) |
| first trial of session (warm-up) | 18.232 ms |
| blind fraction over the session | 0.0222 % |

The shape is **warm-up, plus a quiet steady state, plus a rare heavy tail that
arrives mid-session without warning**. An earlier 23-trace sample suggested
"first trial worst, ≤ 10.6 ms thereafter"; the full session broke that — the
51.5 ms outlier was trial *five*. Do not design against a steady state that has
a 0.85 % tail three times its width.

These are figures for a PowerShell capture tool on the bench box, and they are
**not** a prediction for the Tauri runtime. OpenPiso measures its own tail in
production instead (ADR-0071: 20 ms heartbeat, reported rate + maximum +
tainted-batch count). The maximum carries as much weight as the rate, because
erasure is a threshold effect: a machine at a 44 ms worst case is fine and a
machine at 46 ms is losing coins, and no rate distinguishes them.

### A stall taints the measurement, never the credit

Logging a stall is not enough. A logged stall that is never joined to the
measurement it corrupted leaves a wrong width sitting in the alarm's input,
with the alarm unable to know.

**When a blind window intersects a batch's span widened by `min_high` at each
end — `first_pulse_start − 45 ms` to `last_pulse_end + 45 ms` — that batch's
width and inter-pulse-timing samples are excluded from the drift-alarm
statistics. Credit is untouched.**

The widening is not a safety margin someone chose; the filter's verdict on the
first and last pulse genuinely depends on the line for up to `min_high` outside
the batch's own span. It reuses a constant the filter already carries, so it
adds no tuning freedom.

This is observed, not predicted: `A01`'s corrupting 6.024 ms blind sat 6 ms
*outside* the filtered pulse boundary and still shaved the measured width to
94.284 ms. The naive rule ("the window overlaps the batch") would have missed
it.

---

## 6. Negative results

Each of these is also stated in place above; the index exists so that nobody
re-opens one in six months. Read in isolation a negative result looks like an
omission — read in place it looks like a caveat. It needs both.

| # | Negative result | Stated in | Evidence |
|---|---|---|---|
| N1 | **No defensible poll rate exists.** Polling faster is worse, and the best value found (50 ms) is still wrong on 1 coin in 20. The lever is the filter, not the rate. | §4, §5 | 1–150 ms sweep; incumbent 30 ms credits the correct value on **0 %** of insertions |
| N2 | **Merged coins are unidentifiable, not merely hard to identify.** ₱20→₱10 and ₱10→₱20 cannot be told apart from the line. | §7 note below | 30 trials of 30: train span 6064.0 vs 6064.3 ms, each block's own range wider than the difference; no seam, no width signature, no period disturbance |
| N3 | **No minimum inter-coin spacing rule exists to state.** The merge boundary is "did the second coin arrive before the first train ended", not a millisecond threshold. | below | bracketed to (110.3, 200.6] ms of quiet gap, but separated seams come back **quantized to the acceptor's emission slot** — 200.6 / 205.7 / 220.7 / 230.7 / 290.9 / 300.9 / 301.0 / 642.0 ms |
| N4 | **`pulse_map` decomposability is not a fault detector.** "Can this count be decomposed into known denominations?" is coin *identification* wearing a fault-detector's costume. | §8 | it would have caught **none** of the merged trials — 30 decomposes as 20+10 |
| N5 | **A passive pull-down cannot clean the signal.** The acceptor's COIN output is an open-collector NPN whose emitter sits at system ground; asserted, it is a hard short to 0 V and no resistor to any negative rail can drag the node below that. | §9 | the cheap hardware fix people propose does not exist |

**On N2 and N3 together.** A queued coin is emitted on the acceptor's own
clock, concatenated onto the running train with **no seam at all** — the
205.6 / 205.8 ms ripple runs unbroken across the join. Timing-based segmentation
of a merged train is therefore not a hard problem but an absent one, and
information about the insertion instant is destroyed rather than obscured. Two
consequences worth naming:

- `inter_coin_gap_ms` **cannot** buy back a merged pair at any value. It is the
  one knob owners were actively told to tune (see §8).
- Merged pairs sometimes credit correctly **by accident**: 9 of 10 ₱10+₱10
  pairs merge to 20 pulses, and 20 is a `pulse_map` key, so they credit ₱20.
  The lookup's successes are not evidence that it works.

### What the shipped code does to this archive

Included here because it is measurement, and because it is the single number
that reorders everything. ₱1,825 inserted across 116 valid trials, credit
simulated end-to-end:

| Path | Credited | Recovery |
|---|---|---|
| Shipped: 30 ms poll, no filter, `pulse_map` lookup | ₱722 | **39.6 %** |
| Perfect filter → shipped `pulse_map` lookup | ₱925 | **50.7 %** |
| **Unfiltered incumbent → sum crediting** | ₱1,854 | **101.6 %** |
| Filter → sum crediting | ₱1,825 | **100.0 %** |

The shipped machine takes 39.6 % of the money inserted into it, and it fails by
**over-count, not phantom**: ~7–9 % of real pulses split in two, an over-counted
batch matches no `pulse_map` key, and an unmapped batch credits **nothing**. A
real pulse is essentially never missed (0.03 %).

**The filter was never the binding constraint on the money.** Every ₱20→₱10 and
₱10→₱20 trial is counted *perfectly* at 30 pulses and credited **₱0**, because
30 is not a key. Sum crediting needs no filter, no acceptor change and no bench
work, and its over-credit is 1.1–2.1 % — falling to 0.0 % once the filter is in
place. That asymmetry is what ADR-0070 and the staging in the remediation plan
are built on.

---

## 7. Glossary delta

Four entries in [`glossary.md`](glossary.md) were marked **(decided)** and were
**true descriptions of shipped behaviour** — which is exactly what made them
dangerous. An implementer reading the glossary mid-task got a confident,
correct-about-that-version model with no marker that it was under revision. The
delta below records their pre-remediation sense and replacement sense.

### Pulse Sampler

- **Currently:** "The serial-only worker that polls the **Coin Pin** at
  approximately 30 ms and performs rising-edge detection into the shared pulse
  buffer."
- **Why it is now wrong:** both halves go. It does not poll (event-driven, no
  fallback — ADR-0071), and it does not hand raw rising edges downstream (the
  two-sided filter sits ahead of edge counting — §4).
- **Replacement sense:** the serial-only worker that watches the Coin Pin for
  line-state events with a 20 ms heartbeat, applies the two-sided filter, and
  emits *filtered pulses with their measured widths* into the shared buffer. It
  represents its own blind time explicitly and refuses the coin path loudly
  when events are unavailable.

### Coin Batch

- **Previously:** "The group of pulses for one physical coin," valued by a
  configurable pulse-count → cents map.
- **Why it is now wrong:** multiple coins can concatenate into one train with no
  observable seam, and ADR-0070 removes Pulse Map valuation.
- **Replacement sense:** one pulse train, closed by a quiet gap. It can contain
  one or multiple physically inseparable coins and credits its pulse sum.

### Coin Batch Rejection

- **Currently:** "A closed **Coin Batch** whose observed pulse count matches no
  **Pulse Map** entry. It is not credited with money, time, or points…"
- **Why it is now wrong:** the concept survives; its power to withhold credit
  does not. A batch is credited by pulse sum regardless (ADR-0070), so a
  no-match is a *signal-quality report*, not a crediting outcome.
- **Replacement sense:** renamed **`ImplausibleTiming`** — a closed batch whose
  **inter-pulse timing** departs from the measured 205.7 ms period. It reports,
  it never refuses. Reported as a **rate**, not per batch. **Rejection Credit**
  keeps its historical machinery but loses its routine cause.

### Inhibited Pulse

- **Currently:** "A rising edge observed while the **Set Pin** is not
  asserted… never enters a **Coin Batch** and can never credit a customer."
- **Why it is now wrong:** ADR-0072 makes exactly that pulse creditable when it
  belongs to a train that began while asserted. Inhibit gates *acceptance*, not
  *output* — RTS dropped mid-train does not stop the train (measured: all 20
  pulses ran to completion after the pin dropped at 2000 ms).
- **Replacement sense:** a pulse observed while the Set Pin is not asserted. If
  the reader holds buffered pulses it is part of a **straddling train** and is
  credited in full with the rest of that train; only a pulse arriving with an
  empty buffer is the anomaly the old definition described.

### Pulse Map — added

- **Previously:** used as a defined term but described only inside **Coin
  Batch** as "a configurable pulse-count → cents map [that] turns a closed batch
  into one coin's value" — i.e. the money path.
- **Why it is now wrong:** ADR-0070 demotes it to **denomination metadata**,
  structurally severed from crediting. Crediting reads a single pesos-per-pulse
  scalar (1, by the invariant in §2). The map's absence from the glossary while
  it is named in two other entries is itself a defect.
- **Replacement sense:** add an entry. A read-only table of pulse count →
  denomination, written only by Acceptor Verification, used for **reporting and
  display**. It cannot withhold or alter credit.

---

## 8. Required corrections to shipped documentation

Listed by file and line. **Not applied here** — applying them is the
implementing effort's work, tracked in the remediation plan.

| Location | What it says | Why it is wrong |
|---|---|---|
| [`hardware.md:182-185`](hardware.md) | "Derive the production inter-coin gap from observed intra-coin gaps with margin." | Tells owners to tune `inter_coin_gap_ms`. No value of it can buy back a merged pair (N3). ADR-0073 demotes the setting to read-only. |
| [`hardware.md:18-22`](hardware.md) | The Pulse Sampler "polls the Coin Pin at approximately 30 ms… the coarse period undersamples narrow floating-line noise so sub-30 ms spikes are not counted as pulses" | The spikes are 0.08 ms and ride *inside* real pulses; the poll aliases them into over-counts during a genuine coin. Same retrofitted rationale as §1. |
| `apps/web-admin/src/lib/pages/settings/CoinDeviceSection.svelte:494-497` | Option reads `Normally open (pulse high)`; the helper directly below reads "Choose Normally closed for acceptors idle high and pulsing low." | The UI instructs the owner to pick the option the code does not implement — `NormallyOpen` normalizes as a **pass-through**, so on this idle-high unit the shipped `normally_open` value already behaves as idle-high/pulse-low. ADR-0073 redefines the setting and migrates **by value** (`normally_open` → `normally_closed`) so observed behaviour is preserved exactly. |
| `apps/admin/src-tauri/src/terminal_selection.rs:896-899` | "The set pin is already inhibited at this point so no new pulses can appear; only the residual buffer can produce a coin." | False premise, stated in a doc comment. Inhibit does not stop a running train, so the drain drains a *buffer* against a 4008 ms *train* on a 700 ms deadline — a ₱20 cut at 2 s credits ₱10 and looks, from every log, like a correctly accepted smaller coin. **ADR-0072 requires this comment corrected by name.** |

---

## 9. Declined options, with the measurements that reverse them

Stated as thresholds rather than prose: a declined option with no reversal
condition comes back as folklore.

### 9.1 The wiring fix (forcing a valid RS-232 low)

**Declined** — not fleet-wide, not as an option.

The signal-quality argument targets a component that is not where the money
leaks: a *perfect* filter on the shipped batcher recovers 50.7 %, against
101.6 % for sum crediting on the *unfiltered* incumbent (§6). The stall-margin
argument was already spent by going event-driven (ADR-0071), which fixes a
defect no wiring change touches — a reader blind ~30 ms out of every ~30 ms
permanently. Note also N5: the cheap passive fix does not exist.

Open-sourcing was weighed and *strengthens* the decline, for a non-obvious
reason: a reference circuit would harden the parameter that was never fragile.
The 45/28 band's bounds come from three orders of magnitude between pulse and
chatter, which generalizes across the family; what does **not** generalize is
**width and mode**, and wiring does nothing for width. The OSS-correct response
is to state the envelope and detect departure from it (§2).

**Reversal conditions — two, distinct, either sufficient:**

| # | Condition | Today's margin |
|---|---|---|
| R1 | Any capture forces `min_high` above **~59.4 ms** (the ₱1-precursor ceiling). Above it the two-sided filter has no valid setting left. | longest chatter HIGH is ~0.08 ms — five orders of magnitude under |
| R2 | **Any nonzero edge count on a no-coin baseline.** The DTR reference is gone and the line is floating; no filter can save that. | 4 of 4 and 2 of 2 baselines at zero edges; live form ships as the idle edge rate in Acceptor Health |

### 9.2 Reprogramming the acceptor

**Declined — change nothing on the acceptor.**

Pulses-per-coin is freely programmable (1–50), and 4 pulses + fast mode would
cut a ₱20 train to ~0.48 s. Declined on measured accuracy, not operational
cost:

- **Fast mode has no stall margin at all.** Any blind window over **2.09 ms**
  (30.09 − 28) erases a fast pulse, and a **5 ms** stall already loses a coin.
  Slow mode absorbs a **40 ms** stall at every position. Against measured
  stalls of 17.6–29.9 ms, slow has 1.5–2.6× to spare and fast is 8–14× under
  water.
- **Compression is ruled out** because sum crediting is legal only at
  1 pulse per peso, and it is the cheapest fix available. Shorter trains also
  shrink the merge *window* without ever removing the merge.

**Live invariant: pesos-per-pulse must remain 1.** ADR-0070 depends on it.

**Reversal condition:** none stated — reversing it would invalidate ADR-0070's
premise, so it is a re-decision rather than a threshold.

**Drift defences that replace the change** (as they stand after ADR-0073):

| | Defence | Status |
|---|---|---|
| d1 | Warn on measured pulse width outside the band — a ~30 ms low means the speed switch moved. Warn as a **real fault**; do not refuse credit, do not auto-widen the filter. | **kept.** Needs §4's width source; band is a spec constant, deliberately loose at **70–130 ms**, because its job is mode discrimination (100.3 vs 30.09), not precision |
| d2 | Warn when `pulse_map` is not the identity `n → n × ₱1` | **kept, but unreachable by owner action** — ADR-0073 makes the map read-only, so the only route to non-identity is a re-teach, which Verification catches |
| d3 | Make `min_high` / `min_low` / poll owner-editable behind an advanced gate | **declined by ADR-0073.** They are not owner settings; they are physics. Displayed read-only for phone support |
| d4 | Record 1-pulse-per-peso as a live constraint | **kept** — see the invariant above |

### 9.3 Bench measurement of the poll loop's stall tail

**Declined as evidence.** It measures a PowerShell loop on the bench box, which
is a proxy for a program nobody ships (OpenPiso is Rust in Tauri with no GC).
ADR-0071 has OpenPiso measure its own tail in production instead — direct
evidence about the real runtime, which no bench run can supply. The bench
tool's own tail is reported in §5 as an instrument property, not as a
prediction.

**Reversal condition:** none. Superseded by a strictly better measurement.

### 9.4 The COUNTER line as a second signal path

**Out of scope.** The 4-pin harness is 12 V / COIN / GND / **COUNTER**, and
COUNTER on this family typically pulses once per *physical coin accepted*,
independent of denomination — a per-coin boundary marker that would settle the
merged-train identification N2 declares absent. It is nonetheless a second
signal path, an architecture change, and sum crediting already removed the
money that would justify it. **It buys reporting fidelity, not pesos.**

**Reversal condition:** a decision that per-coin identification inside a merged
train is required for something other than reporting.

### 9.5 A USB-serial adapter — taint, do not refuse

Not a declined option so much as a declined *refusal*. ADR-0071 refuses the
coin path when comm events are unavailable; folding the adapter into that rule
is the tempting move and the wrong one.

The reader **functions** on an adapter: `WaitCommEvent` is implemented by
`usbser` / CH340 / FTDI drivers, and a 1–16 ms driver polling interval sits well
under `min_low` 28 ms, so the filter survives comfortably. This is **not** a
reproduction of the incumbent-poll disaster, which was caused by running no
filter at all rather than by the poll period.

What does break is narrower: the driver polls the modem status lines on its own
interval, and **that blind time is invisible from user space** — uncountable,
unreportable, unbounded. So an adapter-backed port **permanently taints the
drift alarms** (width, inter-pulse timing) via §5's existing taint machinery,
applied at the *port* level instead of per batch; Acceptor Health shows the
reason and **crediting is untouched**. Refusing would lock out most downstream
builders — motherboard COM headers are largely gone from current consumer
boards — over a measurement concern.

### 9.6 The ESP32 coinslot firmware alternative

Out of scope for characterization: replacing the RS-232 path is a different
architecture decision, not a characterization of this one.

---

## 10. Measurement availability

The measurements summarize CH-926 bench sessions from July–August 2026:
initial polarity and pull-up checks; slow/fast-mode comparisons; inhibit
checks; and the 117-trial characterization (1464 seconds, 705 pulses,
65 individual coins). Raw captures and analysis tools are not distributed
with this repository. The tables preserve the reported measurements and
limitations, not a claim that readers can reproduce them from this tree.
Repeat characterization on the installed hardware before relying on the
reported envelope or selecting configurable quiet-gap values.

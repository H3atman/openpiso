# ADR-0072: Credit the straddling train

- Status: Accepted
- Date: 2026-08-13
- Deciders: shop owner (product)
- Refines: [ADR-0023](0023-serial-coin-input-and-terminal-selection.md) (the
  in-flight drain keeps its intent and loses its stated premise)
- Relates to: [ADR-0063](0063-unmapped-pulse-count-typed-batch-read-outcome.md)
  (the typed outcomes the drain reports through),
  [ADR-0064](0064-owner-visible-coin-loss-notices.md) (Inhibited Pulse Notices),
  [ADR-0070](0070-credit-the-pulse-sum.md) (what makes the recovered batch
  credit correctly), [ADR-0071](0071-event-driven-coin-reader.md) (the reader
  this gate sits in), [ADR-0073](0073-owner-coin-surface-is-a-report.md) (the
  draining state and the cap-trip count)
- Evidence: [`coin-acceptor-characterization.md`](../coin-acceptor-characterization.md)
  §3, §8

## Context

Two bench facts, measured, that the code contradicts:

1. **Inhibit cannot stop a train already running.** The Set Pin was dropped
   2000 ms into a ₱20 insertion and all 20 pulses ran to completion 2.7 s later.
2. **Inhibit reliably stops a train from starting.** Sixteen inhibited
   insertions across four denominations produced zero pulses and zero retained
   coins — every coin returned. The cut is the *start of the train*, not the
   insertion.

Together those bound the live case exactly. It is not a coin accepted while
inhibited — this acceptor does not do that. It is the **straddling train**: a
coin accepted while the Selection Window was open, whose pulses are still coming
out after it closed. That is guaranteed by the hardware on a correctly wired
machine, not rare — every window that closes mid-train produces one — and ₱20 is
the worst case at a **4008 ms** train.

**The cost today is a silent halving, not a loss.** `close`
(`terminal_selection.rs:532`) inhibits the Set Pin, then drains until
`inter_coin_gap + IN_FLIGHT_DRAIN_HEADROOM` = **700 ms**. Its doc comment
(`:896-899`) states the governing premise outright:

> *"The set pin is already inhibited at this point so no new pulses can appear;
> only the residual buffer can produce a coin."*

Fact 1 disproves that sentence. **It drains a buffer, not a train.** A ₱20 cut
at 2 s leaves 10 pulses in the buffer, credited through `pulse_map[10]` as
**₱10**, and the remaining 10 die at the reader's gate
(`coin_source.rs:428` buffers a pulse only `if set_pin_asserted`). The machine
bills half a coin and looks, from every log and every screen, like it correctly
accepted a smaller one. That is worse than an outright loss in the way that
matters: a visible zero gets reported by the owner; a plausible ₱10 never does.

700 ms cannot reach a 4008 ms train, so this fires on **every** mid-train close.
Two realistic triggers: the 15 s inactivity timer (`last_activity_at` updates
only on an *accepted batch*, so a 4 s train logs no activity and the timer runs
straight through it), and an operator closing the window as the customer drops
one last coin. Meanwhile the anomaly it logs tells the owner to *"check the Coin
Debug Bundle for an idle insertion or an inhibit wiring problem"*
(`coin_source.rs:259`) — for an event guaranteed by the hardware on a correctly
wired machine.

## Decision

### 1. Credit it, in full, to the just-selected PC

Not refuse, not hold. ADR-0070's principle is at maximum strength here: the
machine agreed to take the coin while it was saying yes, and there is no
uncertainty about the coin at all. Holding invents a reconciliation surface and
a human step for something that happens by hardware design several times a day.

### 2. Inhibit immediately on close; extend the *drain*, never the *window*

The tempting cheap fix — do not drop the Set Pin while a train is in flight —
is **rejected, and the reason inverts it.** Inhibit's real power is that it
prevents a train from *starting*. Holding the pin asserted through the tail
keeps the slot **live**: the customer feeds another coin into a window the
operator already closed, that coin is legitimately accepted, and the tail
chains — unbounded, on a window nobody has open.

Immediate inhibit is precisely what makes the tail **provably bounded by one
longest train (4.11 s) and never chained**. `begin_close`'s fail-closed inhibit
stays exactly as written.

### 3. The in-flight predicate is the buffer itself

The discriminator must live in the reader, because `terminal_selection` can only
credit what `observe` buffered. It needs no new state:

```rust
if set_pin_asserted || self.has_buffered_pulses() { self.pulses.push(now); }
```

The buffer is non-empty only if a pulse was admitted, and the quiet gap has not
yet elapsed; inductively, a pulse is only ever admitted if the train began while
the pin was asserted. So that one term means exactly *"this pulse belongs to a
batch that started inside an open window"*. It self-terminates at the first
silence of `inter_coin_gap`, and it is the same predicate `open_at` (`:487`) and
`finish_close_if_drained` (`:617`) already use to mean "the terminal is not
free" — so all three agree by construction.

Its second effect is the one that kills the halving: the buffer never empties
mid-train, so **the straddle arrives as one batch of 20**, not 10 + 10.

### 4. An absolute cap at 6 s, as a fault trip and never a knob

The gap rule self-terminates on real hardware, but a stuck line would hold the
terminal locked indefinitely. Ceiling: **6 s from the close instant** (₱20's
4.11 s train + the 600 ms gap + margin), sized so **nothing legitimate can reach
it**. Crossing it means the line is faulty. The wording matters: this is a fault
trip, not a tuning parameter, and it must never acquire a settings row.

A pulse-*count* cap is rejected for ADR-0070's reason: no count threshold
separates a fault from a large coin.

### 5. The drain loop becomes state-driven

Replace the up-front `drain_deadline` clock with
`while has_buffered_pulses() || pending_batch.is_some()`, under decision 4's
absolute cap — the same predicate as decision 3, `open_at`, and
`finish_close_if_drained`.

A rolling `last_pulse + inter_coin_gap` deadline was rejected because it
reimplements the reader's batch-close rule in a second place. A fixed 6 s
deadline was rejected because it makes *every* close take six seconds.

### 6. Draining is published as a state

`open_at` (`:478-487`) already refuses a new window while closing or while
pulses are buffered. That lock lasts 700 ms today and nobody notices; under this
decision it lasts up to ~4.8 s, and the next customer's operator sees
`Conflict("terminal in use")` on a machine that visibly just finished. The bound
is the honest price of not losing the coin — but `begin_close` currently calls
`publish(None)`, so the UI is told the window is simply gone. The
closing/draining phase must be **published as a state** ("finishing coin…"). A
silent 4.8 s refusal reads as a hang.

### 7. Named inhibit variants, so a forfeit is deliberate

`refresh_calibration_locked` (`:1552-1561`) happens to be safe under the
permeable gate — it inhibits and *then* calls `clear_buffered_pulses()`, sealing
the gate so later tail pulses find an empty buffer. But that safety is an
**ordering accident inside one function**, and there are several other
non-transactional inhibits: calibration cancel/fail (`:1411`, `:1435`, `:1446`),
`reconfigure_after` (`:1631`), and source drop (`coin_source.rs:1469-1500`).
Under decision 3 every one of them now silently decides whether a customer's
in-flight coin survives.

Split the trait method into **`inhibit_and_discard`** and
**`inhibit_and_drain`**. Calibration and shutdown discard (there is no PC to
credit); only `close` drains. Encoding "this inhibit forfeits a coin" as a side
effect of a call that reads like housekeeping is the same silence ADR-0071 is
unwinding, at a smaller scale — and the next person to add an inhibit path
would not know they had to choose.

### 8. The gate keeps its remaining job, and its existing copy becomes true

With the straddle admitted one layer down, `set_pin_asserted == false` plus a
pulse now means exactly one thing: **a genuine idle insertion or an inhibit
wiring problem** — which is what `InhibitedPulseCause::NoSelectionWindow`'s
existing copy already claims. Its text becomes true for the first time; leave it
alone. The gate stops being a blunt discard without acquiring a discriminator of
its own.

The event that has no name yet is decision 4's cap trip — a coin the machine took
and then truncated. Add one cause for it.

### 9. Owner visibility: the cap trip only

**Only the cap trip is owner-visible**, through ADR-0073's Acceptor Health, as a
*count* next to the stall rate. A straddle is normal and fully paid, so
surfacing it trains the owner to ignore the channel; a truncated coin is real
money withheld and is the only retained-but-uncredited case that survives
decision 1. This is ADR-0070's report-don't-refuse with an asymmetry ADR-0070 did
not need: report the fault, stay silent on the routine.

### 10. Fix the port-open phantom at its source

`PulseObservation` starts `prev_coin_pin = false` against a line that idles
HIGH, so the first sample after every port open books a rising edge — and only
the `set_pin_asserted` gate stops it becoming a phantom ₱1 on a mid-session
reconnect inside an open window. Decision 3 thins that net.

**The reader stores its previous level as optional state.** The first actual
sample seeds that state and cannot create a pulse. Recovery resets it to
unseeded. The gate was never the right thing standing between an idle line and a
free ₱1 — that is a defect wearing a workaround. ADR-0071's rewrite touches this
same initialization, so it is stated here rather than left to be rediscovered.

The observation result distinguishes an observed edge from an admitted pulse.
Only an edge that is observed and refused raises an **Inhibited Pulse** anomaly.
A pulse admitted through the non-empty-buffer term is part of the straddling
train and does not raise that anomaly.

## Rejected alternatives

- **Refuse the straddle** (the shipped behaviour, made deliberate). The machine
  took the coin while it was saying yes.
- **Hold it** as a pending credit for the owner to resolve. A human step for a
  hardware-guaranteed daily event.
- **Keep the Set Pin asserted until the train ends.** Rejected in decision 2 —
  it converts a bounded tail into a chainable one.
- **Track trains explicitly** (last-edge instant plus a began-while-asserted
  flag). The same predicate as decision 3, with state that can disagree with the
  buffer.
- **Pass a `closing` flag down from the selection layer.** The tempting one and
  the worst: it makes the reader's correctness depend on the selection layer's
  phase, in exactly the direction ADR-0071 is unwinding.
- **A pulse-count cap** instead of a time cap. No count separates a fault from a
  large coin.

## Consequences

- **ADR-0023's in-flight rule keeps its intent and loses its premise.**
  Inhibiting stops new pulses only for a train not yet started. The doc comment
  at `terminal_selection.rs:896-899` must be **corrected by name** as part of
  implementing this ADR; it is currently the load-bearing false statement in the
  close path.
- The terminal lock after a close grows from 700 ms to up to ~4.8 s, bounded at
  6 s by the fault trip. Decision 6 is what makes that acceptable to an operator.
- Two one-line changes — the permeable gate term and the previous-level seed —
  ship in stage S1 of the remediation plan alongside ADR-0070, with no filter and
  no bench work. Decisions 4–7 follow with the rest of the close path.
- ADR-0064's Inhibited Pulse Notices become meaningful rather than routine: with
  the straddle credited, an inhibited pulse really does indicate an idle
  insertion or a wiring fault.
- Under ADR-0071 the filter's ~45 ms delay rides inside the tail and the gate
  sits behind the filter. The predicate is unchanged either way, and the 6 s cap
  absorbs the delay.

## Verification

Tests cover: a ₱20 train cut at 2 s crediting ₱20 as **one** batch of 20 rather
than ₱10; a coin inserted after the close finding the slot inhibited and never
entering the buffer; the drain terminating on the quiet gap rather than on a
clock; the 6 s cap tripping on a stuck line and raising its own cause; a
calibration inhibit discarding while a `close` inhibit drains; and a port open
against an idle-high line booking zero pulses.

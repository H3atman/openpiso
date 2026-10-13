# ADR-0073: The owner's coin surface is a report, not a control panel

- Status: Accepted
- Date: 2026-08-13
- Deciders: shop owner (product)
- Refines: [ADR-0060](0060-coin-20-calibration-capture.md) (calibration becomes
  Acceptor Verification), [ADR-0043](0043-coin-ingestion-sampling-and-observability.md)
  (what the coin path reports to the owner)
- Relates to: [ADR-0064](0064-owner-visible-coin-loss-notices.md) (the event
  rows this replaces with rates), [ADR-0065](0065-phone-designed-vs-phone-tolerable-owner-screens.md)
  (the Settings screen this section lives in),
  [ADR-0070](0070-credit-the-pulse-sum.md) (the demotion this surface
  implements), [ADR-0071](0071-event-driven-coin-reader.md) (the refusal and
  the health metrics), [ADR-0072](0072-credit-the-straddling-train.md) (the
  draining state and the cap-trip count)
- Evidence: [`coin-acceptor-characterization.md`](../coin-acceptor-characterization.md)
  §3, §7, §8

## Context

ADR-0070 severed crediting from the **Pulse Map**: credit is
`pulse_count × pesos_per_pulse` from a single scalar, and the Pulse Map survives
only as reporting metadata. That leaves the owner-facing coin configuration
incoherent — a calibration flow whose output no longer credits anything, and
five homeless things wanting a home: the crediting scalar, the Pulse Map as
metadata, the filter parameters, the implausible-timing rate, and ADR-0071's
reader-unavailable refusal.

What makes the answer affordable is that the bench finally supplied the numbers
the knobs were standing in for. Pulse width is a **hardware constant** —
100.303–100.317 ms across 705 pulses, 65 coins and four denominations, 0.014 ms
of spread — and the ratified filter is exact on 116 of 116 valid trials. There
is nothing left for an owner to tune.

Four premises were corrected by reading the shipped surface:

- **"Calibration is append-only" is half right.** Apply appends, but the manual
  editor is full-table and the save path is `DELETE FROM coin_device_pulse_map`
  + re-insert (`db/settings.rs:296`). The danger was never that rows are stuck —
  it is that anyone can write anything.
- **The gating mechanism this needed already exists.** `calibration_required`
  both locks the editor and filters the candidate row out of the live config
  (`settings.rs:120`). A re-aim, not new machinery.
- **The polarity control is actively wrong on screen today.** The dropdown reads
  `Normally open (pulse high)` while the helper directly below reads *"Choose
  Normally closed for acceptors idle high and pulsing low"*, against a measured
  idle-high / pulse-low unit whose shipped value is `normally_open`. **The UI
  instructs the owner to pick the option the code does not implement.**
- **A precedent for silently rewriting a live coin setting already shipped**:
  `0022_inter_coin_gap_default.sql`, a blanket `UPDATE ... WHERE id = 1`. It is
  the pattern decision 11 deliberately does not follow.

## Decision

### The governing principle

> **A value set by the acceptor's physics is not a site setting, and shipping it
> as one taught everyone the wrong model of the machine.**

The surface stops being a control panel and becomes a report. Of the five
homeless things, exactly one stays editable; two settings that *are* editable
today are demoted with the rest.

### The section, top to bottom

Four blocks, ordered so the ability to break things decreases as the owner reads
down:

1. **Connection** *(editable — the only Save button)*: COM port, baud, Set Pin,
   Coin Pin, Coin Pin Polarity. **Check Connection** lives here.
2. **Acceptor Verification** *(action)*: Verify button, last-verified timestamp
   and result, the read-only pesos-per-pulse statement, the read-only
   denomination labels.
3. **Acceptor Health** *(read-only)*: implausible-timing rate, measured vs
   expected width, stall rate + worst blind window, cap-trip count, with Debug
   Bundle / Reset Window as escalation.
4. **Advanced (reference)** *(read-only, plain collapsed disclosure, no gate)*:
   `min_high`, `min_low`, heartbeat, `inter_coin_gap_ms`, each with a "set by
   the acceptor, not by this site" note.

### 1. Calibration becomes Acceptor Verification, and keeps its teeth

Not deleted, not reduced to a labelling ceremony. It still takes five genuine
coins under an exclusive lock, but its pass condition changes from "five samples
agree" to **count == denomination ÷ ₱1 AND measured width in band**. That makes
it the on-demand form of the width drift alarm, and the only place the
one-pulse-per-peso invariant is *proved* rather than asserted. Writing the
metadata row is a side effect, not the purpose.

Deleting the flow would throw away the one ceremony that puts a real coin
through the whole path — which the bench work showed is not substitutable by
reasoning.

### 2. Pesos-per-pulse is displayed, never edited

*"This acceptor emits 1 pulse per ₱1. Credit = pulses × ₱1."* No input. Editable
would undo ADR-0070's severance with one text field; invisible would leave the
owner unable to explain why 30 pulses credited ₱30.

This makes a "warn when it is not 1" check **unreachable by owner action**,
which is the correct outcome: the only route to non-1 is a hardware re-teach,
and Verification catches that.

### 3. The Pulse Map is read-only, written only by Verification

ADR-0070 demanded the demotion be structural, and a hand-editable table in the
coin settings panel is precisely the affordance that taught everyone it was the
money path. Read-only also makes the stale-row trap self-healing: rows can only
arrive via a verified coin.

### 4. The filter parameters are not owner settings at all

`min_high` / `min_low` / the reader's timing ship as **read-only diagnostics** in
block 4. An advanced gate with warn-don't-reject bounds was asked for; that is
protection for *editable* values, and these should not be editable. The filter
sits on a ~70× parameter plateau and no poll rate is defensible
(characterization §4, N1), so an owner can only move these the wrong way. A
support engineer who genuinely needs a change edits the database.

### 5. `inter_coin_gap_ms` is demoted with them

The one coin knob owners were actively *told* to tune (`hardware.md:182`). The
merge boundary is bracketed to (110.3, 200.6] ms against a 4008 ms ₱20 train and
no millisecond rule exists (characterization N3); ADR-0070 removed the money
stake. Leaving it editable keeps alive the belief that merges are a tuning
problem. The spec says it plainly: **this was a knob because we did not know the
number; now we do.**

### 6. Reader-unavailable reuses the fail-closed surface, with a distinguishable cause

Customer-facing outcome is unchanged — no Selection Window, terminal
out-of-order — so no new customer concept. In admin it names cause and fix:
*"This COM port does not support hardware event notification. The coin path is
disabled."* It must be **distinguishable from "acceptor unplugged"**, because the
remedies are opposite (change port or driver vs check wiring). Its own audit
event alongside `coin_device_unavailable`.

### 7. One Acceptor Health panel of rates; the event list dies

"Recent unmapped pulses" is an artifact of the era when each row was a lost
coin — after ADR-0070 none of them are. Show implausible-timing rate, measured
vs expected width, stall rate + worst blind window, cap-trip count. Raw rows
survive in the Debug Bundle, off the owner's screen.

Renames: `getUnmappedCoinPulses` → `getAcceptorHealth`,
`AnomalyKind::UnmappedRejection` → `AnomalyKind::ImplausibleTiming`.

### 8. Polarity: fix the meaning, not the label

`normally_open` is **redefined** to mean what it says (idle-low / pulse-high),
and this unit becomes `normally_closed`. Relabelling alone preserves a lie in
the data model forever; removing the control is tempting, but the field has more
than one acceptor model in its future and a control whose semantics are true
costs nothing.

### 9. Expected width is a spec constant with a deliberately loose band

100.3 ms measured, band **70–130 ms**. There is nothing to learn from the field:
the spread is 0.014 ms. The band's job is **mode discrimination** — 100.3 ms
against the acceptor's fast mode at 30.09 ms — not precision, so it is loose on
purpose and the spec says so. A *learned* reference is the worst option: it would
drift to accept a field-re-taught unit, the exact event the alarm exists to
catch.

### 10. Upgrade is announced, automatic, non-blocking, and actively repairs

The mechanism migrates silently — rows keep meaning, as labels — with a
dismissible admin notice: *"Coin credit no longer comes from this table."* Then
**delete any row violating the identity invariant**, each deletion recorded in
the audit event. A stale `5→₱5` row is harmless for *money* after ADR-0070, but
the table is still read for *reports*, and a stale row makes reports lie.

No confirmation gate: a dialog on upgrade is an outage for anyone who does not
read it.

### 11. The polarity flip migrates by value, not by blanket update

Rewrite `normally_open` → `normally_closed` **per install**, so observed
behaviour is preserved exactly — the definition changes, the wire does not. This
is safe precisely because the shipped `NormallyOpen` normalization is a
pass-through (`coin_source.rs:105`), so today's value already encodes idle-high /
pulse-low regardless of its name. Verification then serves as the confirmation:
a hand-mis-set install fails loudly instead of quietly eating coins.

### 12. ADR-0072's two artifacts split by audience

The **draining state** is customer-facing on the terminal ("finishing your
coin"), because the lock grows from 700 ms to ~4.8 s and a bare
`TERMINAL_IN_USE` reads as a hang. The **cap trip** is a *count* in Acceptor
Health next to the stall rate — same class of thing, the reader saw something it
cannot explain. Neither gets an editable value.

### 13. "Test Acceptor" becomes "Check Connection" and absorbs the capability check

It is already a **no-coin connectivity probe** (`api.testCoinDevice()` returns a
message), structurally distinct from Verification — which makes it the only
thing that can report decision 6's event-notification unavailability *before* a
customer inserts a coin. It stays in block 1. Two buttons that both sound like
"check the coin slot" is how an owner runs the wrong one; the new names make the
distinction legible — one needs a coin, one does not.

### 14. Verification is advisory, except on failure

The current hard gate (`calibration_required` filtering the 20-pulse row out of
the live config, `settings.rs:120`) existed because an unverified ₱20 row could
mis-credit. ADR-0070 removed that risk structurally, and blocking a shop from
opening over a labelling ceremony is disproportionate — a persistent unverified
banner instead.

**The exception: stay hard when Verification has *failed*.** A wrong count or an
out-of-band width is a live acceptor fault, not an incomplete ceremony, and it
is the one state where crediting is genuinely untrustworthy.

## Rejected alternatives

- **Delete the calibration flow.** It is the only ceremony that puts a real coin
  through the whole path.
- **Keep the Pulse Map editable as metadata.** The editable table is the
  affordance that created the belief being dismantled.
- **An advanced gate over the filter parameters.** Protection for editable values
  that should not be editable.
- **A gate over the read-only Advanced block.** Theatre on values nobody can
  change, and it makes phone support harder.
- **Learn the expected width from the field.** It would drift to accept the fault
  it exists to catch.
- **Relabel the polarity option without redefining it.** Preserves a false data
  model permanently.
- **A blanket `UPDATE` for the polarity migration**, following the
  `0022_inter_coin_gap_default.sql` precedent. Migrating by value is what
  preserves observed behaviour on every install.
- **A confirmation dialog on upgrade.** An outage for anyone who does not read it.
- **Keep the per-event "recent unmapped pulses" list.** Every row in it now
  credits.

## Consequences

- ADR-0060's calibration keeps its capture mechanism and loses its purpose as a
  crediting input; its pass condition is strengthened and its hard gate is
  narrowed to genuine failure.
- ADR-0064's rejection Notices lose most of their volume to ADR-0070 and their
  owner-facing home to decision 7's rates. The Debug Bundle keeps the raw rows.
- Two migrations ship together: the Pulse Map invariant repair (decision 10) and
  the polarity redefinition by value (decision 11).
- `inter_coin_gap_ms` stops being owner-editable. `hardware.md:182-185` must be
  corrected in the same change (characterization §8).
- The coin settings screen loses most of its inputs. That is the deliverable, not
  a side effect.
- This lands in stage S2 of the remediation plan, with the filter. S2 buys
  1.1–2.1 % of money and it must not be judged on that: it is what makes width
  and timing measurable, so stage S3's drift alarms have an input.

## Verification

Tests cover: the Pulse Map rejecting any write that does not come from
Verification; a failed Verification blocking crediting while an unverified one
does not; the polarity migration leaving observed behaviour identical on an
install carrying `normally_open`; the invariant repair deleting a stale `5→₱5`
row with an audit entry; and the reader-unavailable message being distinct from
the acceptor-unplugged message in both text and audit event type.

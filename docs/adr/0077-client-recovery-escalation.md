# ADR-0077: Client Recovery Escalation powers off an unrecoverable PC

- Status: Accepted
- Date: 2026-09-02
- Deciders: shop owner (product)
- Narrows: [ADR-0076](0076-client-reachability-mismatch-monitoring.md) — its
  "The first phase detects and reports only. It never changes PC state,
  sessions, balances, time, or Points" is narrowed to allow exactly two
  changes, PC power state and the session bound to that PC, and reaffirms that
  balances, time, and Points remain untouched.
- Relates to: [ADR-0048](0048-client-guard-tamper-kill.md) (the Client Guard,
  its pipe, its relaunch, and its deliberate absence of a network client),
  [ADR-0045](0045-offline-shutdown-countdown.md) (a Client that can still run
  but cannot reach Admin powers the PC off by itself),
  [ADR-0015](0015-audit-log.md) (distinct session-end event types),
  [ADR-0031](0031-client-config-persistence-and-pc-identity.md) (MAC-keyed PC
  identity), [ADR-0005](0005-tamper-response-and-expiry-powraoff.md) (the arms
  race ends at the power button)

## Context

ADR-0076 detects a **Client Reachability Mismatch**: a host that answers ICMP
while its Client does not report. It deliberately stops at reporting. The owner
gets an Alert and must walk to the PC.

Three mechanisms already cover most of the ways a Client can stop reporting,
and understanding which ones they cover is what makes this ADR narrow:

- A Client that **dies** drops the Client Guard pipe, and the Guard relaunches
  it immediately (ADR-0048). No mismatch survives long enough to alert.
- A Client that **runs but cannot reach Admin** arms the Offline Shutdown
  Countdown and powers the PC off itself after Offline Grace (ADR-0045).
- A Client the owner **quit deliberately** stands the Guard down for that boot
  and is the owner's own maintenance state.

What is left is the case none of them covers: the Guard is running but
**edge-blind**. Its liveness model is edge-triggered on pipe events, and
ADR-0048 states the consequence plainly — "a Client that never connects is
never guarded and never noticed: no connection means no disconnect, so no
relaunch and no signal." A PC that boots to a desktop whose Client never
started, or whose Client died in a window where the Guard could not observe a
disconnect, sits there earning nothing, showing no lock, and exposing whatever
the previous customer left on screen. The Guard is present and able to fix it.
Nothing tells it to.

The Admin can see that PC. It has Host Reachability and no Client Reachability.
It cannot act, because ADR-0048 gave the Guard no way to be told anything: "The
Guard does not talk to the admin server." The pipe cannot carry a message the
other way either — its DACL grants the interactive user pipe write and
explicitly excludes pipe read.

## Decision

### The Guard notices an absent Client on its own

The Guard's relaunch trigger becomes **level-triggered as well as
edge-triggered**. It periodically evaluates whether a verified Client currently
holds the pipe, and if none does, it relaunches on the existing bounded
backoff. The edge trigger stays exactly as it is, because immediate relaunch on
a dropped pipe is what makes a Tamper Kill feel instant; the level check is the
floor underneath it.

This alone closes the edge-blind case without any server involvement, and it is
the load-bearing half of this ADR. Everything below exists for the PC where
relaunch is tried and does not work.

**Relaunch never terminates a live Client.** The check is relaunch-if-absent.
Giving the Guard a kill capability would make it manufacture the exact process
death ADR-0048 classifies as a Tamper Kill, producing false accusations against
the seated customer, and there is no case where the Guard is a better judge of
a running Client than ADR-0045's own offline countdown.

### The Guard polls the Admin, outbound only

The Guard gains an outbound HTTP client and polls `POST /api/guard/state` on
the existing `:3210` client/device surface. This reverses ADR-0048's "no second
network client" on one narrow ground: that decision weighed a second network
client against **no gain**, and the gain here is a channel that did not exist
and cannot be built any other way.

The poll is outbound only. The Guard opens no listener and accepts no inbound
connection. A listening socket on the least trusted machine in the shop, run by
a SYSTEM service, is not worth a lower command latency.

It identifies itself by MAC header exactly as the Client heartbeat does, with
no device secret. That matches the trust level of the surrounding identity
endpoints, and the only authority it carries is "this PC should power itself
off" — an attacker who can spoof it on the LAN has denied service to one PC
they could equally have unplugged. This endpoint must not be extended into a
general command channel without first gaining a provisioned secret.

The Guard learns both the Admin base URL **and the MAC** from the Client over
the existing pipe, and caches them under `%ProgramData%\OpenPiso`. The MAC
comes from the Client rather than being resolved again by the Guard because a
Guard that picked a different adapter would poll as a different PC, or as none
at all; the Client reports the pair only after a heartbeat the Admin accepted,
so what is cached is what actually works. A re-pointed Admin therefore
propagates without touching the Guard's installation, and a Guard that has
never seen a Client connect has no endpoint until the cache exists. That
ordering is accepted: the level-triggered relaunch does not require the poll,
so the case that needs no server is exactly the case that needs no endpoint.

### The poll carries the facts the Admin was missing

Each poll reports the Guard's own liveness, whether a verified Client currently
holds the pipe, whether the Guard is disarmed for this boot, and how many
relaunch attempts the current absence has cost.

This is the evidence the whole feature stands on. Before it, the Admin could
not distinguish a PC whose Guard is dead from a PC whose Guard is alive and
losing, because ADR-0076 learns Guard presence only from the Client heartbeat —
unavailable precisely when a mismatch is open. As a consequence, Client Guard
Coverage gains a direct source, but the two signals stay separate: coverage
remains a diagnostic incident about protection, and escalation remains an
action about one PC's power state.

### Escalation is one decision, gated on failed local recovery

The Admin opens a **Client Recovery Escalation** clock for a PC only while all
of these hold continuously:

1. Host Reachability,
2. no Client Reachability,
3. the Guard is polling,
4. the Guard reports no verified Client connected,
5. the Guard reports at least one relaunch attempt for this absence,
6. the PC is not under Monitoring Suppression, and
7. the Guard is not disarmed.

Any one going false clears the clock to zero. When it runs continuously for the
**Escalation Delay**, the Admin marks that PC for an **Escalated Power Off**
and the next poll carries the instruction.

There is no server-commanded relaunch stage. The Guard already relaunches
continuously and on its own; a second, slower, remotely-issued relaunch would
add a delivery-acknowledgement problem and a command a disarmed Guard would
ignore, in exchange for repeating something already happening. Condition 5 is
what preserves the intent of relaunching first: the Admin refuses to power off
a PC whose Guard has not tried.

Escalation Delay defaults to 60 seconds, is owner-settable, and has a hard
floor of 30 seconds. ADR-0076's mismatch grace stays at its fixed 60 seconds:
this is a second, independent clock, and lowering the first one would change
alerting for every shop that only wants alerts.

The clock lives in memory alongside the existing monitoring grace state. An
Admin restart restarts it from zero. Powering off a customer's PC on a clock
that was running before the process died fails in the wrong direction; a
restart should cost a delay.

### The instruction is desired state, not a queue

The poll response states whether that PC should be powered off, when the Admin
decided it, and a nonce. The Guard acts once per nonce and records the nonce it
acted on.

A queue would need delivery acknowledgement, deduplication, and expiry, and a
missed poll would lose a message. Desired state is idempotent: a Guard that
misses a poll simply reads the same state on the next one, and one that acts
twice on the same nonce does not.

### The power off is forced, and the session ends

On a release Windows build the Guard invokes the same Windows shutdown path the
Client already uses for its own power-offs, forced. Debug and non-Windows builds
log the intent only, matching the existing gating of ADR-0045's countdown and
ADR-0048's own release-Windows behaviour. A failed shutdown records its error
and is retried on the next poll rather than escalating to anything else.

The Guard records the nonce it is acting on, and persists that record, *before*
powering off. A successful power off is the last thing the process does, so a
nonce written afterwards would never be written at all, and the same order
would be acted on again after the reboot.

The shutdown is forced and immediate. A graceful request can be refused by a
hung application, and this PC is by definition already failing to run the one
application it exists to run.

The Admin ends the session bound to that PC under a new session-end type,
`escalated_power_off`. Leaving it open would burn a paying customer's time
against a PC that no longer has power, which is the one outcome with no
defence. Reusing `tamper_kick` was rejected on ADR-0015's own rule that a log
naming the wrong cause is a log that lies: this is a monitoring action with no
attribution and no accusation.

**Balance, Points, and time are untouched.** Account time is account-bound and
survives the logout; Guest Time is PC-bound and stays on the PC. This is the
half of ADR-0076's guarantee that this ADR reaffirms rather than narrows.

### Escalation is off by default and PIN-gated

**Client Recovery Escalation** is a shop-wide setting nested under PC
Monitoring, disabled after upgrade, enabled only with the current Owner PIN,
and audited. While it is enabled the Web Admin shows a standing advisory — the
inverse of the monitoring advisory, which stands while monitoring is off —
because the owner should never be unaware that OpenPiso is authorised to power
off customer PCs.

Escalation cannot be enabled while PC Monitoring itself is off: it acts on a
mismatch, so without the detection there is nothing for it to act on, and a
setting that silently does nothing is worse than one that refuses.

Monitoring Suppression already suppresses mismatch detection for one PC for at
most 60 minutes; it suppresses escalation with it. A Guard disarmed by a
PIN-gated owner quit is likewise never escalated against, since that state is
the owner's own maintenance and powering off the PC the owner is working on is
indefensible. The Guard reports its disarm in the poll so the Web Admin can
show why that PC is mismatching instead of only alerting about it.

### The record says what we did

An Escalated Power Off writes a `pc_escalated_power_off` Audit event,
classified as an Alert, carrying the PC, the mismatch incident it resolved, the
Guard's relaunch attempt count, and the nonce.

The open mismatch incident closes with the reason `escalated_power_off`. Once
the PC is off it stops answering ICMP, and ADR-0076 would otherwise close it as
a host that stopped responding with Client recovery unconfirmed — true of an
unplugged PC and a lie about one we powered off ourselves.

One Telegram delivery item says the PC was powered off after repeated failed
Client relaunches, naming it by display name only, carrying no IP, Account,
balance, or customer data, exactly as ADR-0076 requires of every other message.

A per-PC **Escalation Cooldown** prevents a second Escalated Power Off for that
PC within one hour. A diskless PC that reboots into the same broken state would
otherwise mismatch, escalate, and power off on a roughly one-minute cycle all
night. The cooldown converts a cycling machine into one Alert and a PC the
owner finds off in the morning.

## Considered options

- **Server-commanded relaunch as a first stage, then power off as a second.**
  The original shape of the request. Rejected once the Guard could notice an
  absent Client by itself: the first stage becomes a slower duplicate of
  something already running continuously, and it can only reach Guards that
  were already fixing the problem.
- **A Guard listener the Admin pushes to.** Lower latency, and an inbound
  socket on a customer PC run by a SYSTEM service. Rejected.
- **Reading the Client's config to find the Admin URL.** The Client's config
  lives in its per-user `%APPDATA%`; a SYSTEM service has no clean read of it,
  and the pipe already carries a connection the Client owns.
- **Reusing `tamper_kick` for the session end.** No migration, no new
  enumeration entries, and a log that accuses a customer of tampering because
  their PC's Client failed to start.
- **Leaving the session running through the power off.** Simplest, and it bills
  a customer for a PC we turned off.

## Consequences

- The Client Guard gains its first network dependency and its first outbound
  connection. `apps/client-guard` was previously `openpiso-core` plus tracing,
  using raw Win32 FFI for everything.
- `escalated_power_off` must be added everywhere session-end types are
  enumerated, including the report views that filter on `event_type IN (...)`.
- **A dead or never-installed Guard cannot be escalated against.** It has no
  poll, so it has no channel, and it is exactly the PC a determined tamperer
  will produce. That stays a Client Guard Coverage problem for the owner to
  see, not a second enforcement path. As ADR-0005 and ADR-0048 both concluded,
  the arms race ends at the power button; this is good enough, not absolute.
- The level-triggered relaunch, the poll against a real Admin, the OS shutdown,
  and the disarm interaction are only exercisable on an installed kiosk. The
  escalation predicate, the clock, the nonce handling, the cooldown, the
  incident closure, the session end, and the audit metadata are pure and
  unit-tested everywhere, matching ADR-0048's split.

# ADR-0078: The Support Bundle forwards diagnostics always and pulls on demand

- Status: Accepted
- Date: 2026-09-04
- Deciders: shop owner (product)
- Amended: 2026-09-09 — the owner surface below moves from a Support section
  under Settings (the Coin Debug Bundle beside it moves too) to the top-level
  Diagnostics page ([ADR-0081](0081-diagnostics-surface.md)); the bundle
  itself, its transport, its redaction stance, and its no-step-up rule are
  unchanged.
- Closes: [ADR-0016](0016-diagnostic-logging.md)'s open consequence — "a
  lightweight client→server diagnostic-forwarding path is needed".
- Supersedes: [ADR-0016](0016-diagnostic-logging.md)'s proposed interleaved
  developer-log viewer on a Logs screen. The Support Bundle is the
  developer-log surface, and the Audit page already covers the audit half; the
  viewer was never built.
- Relates to: [ADR-0005](0005-tamper-response-and-expiry-powraoff.md) (diskless
  clients erase local evidence on reboot),
  [ADR-0043](0043-coin-ingestion-sampling-and-observability.md) (the Coin Debug
  Bundle, whose redaction stance this mirrors),
  [ADR-0045](0045-offline-shutdown-countdown.md) (the Offline Shutdown
  Countdown the Verdict must name),
  [ADR-0048](0048-client-guard-tamper-kill.md) (the Client Guard and its pipe),
  [ADR-0074](0074-owner-session-hardening.md) (the Owner Session that gates the
  bundle),
  [ADR-0076](0076-client-reachability-mismatch-monitoring.md) (the PC
  Monitoring Incidents this evidence explains), and
  [ADR-0077](0077-client-recovery-escalation.md) (the Guard's poll, its cached
  endpoint, and the Escalated Power Off whose cause it records).

## Context

The Web Admin tells the owner that something is wrong on a PC — a Client Guard
Coverage warning, a Client Reachability Mismatch, a PC Monitoring Incident —
but never why. The evidence is scattered across three stores the owner cannot
reach from the Web Admin: the Client's per-user rotating logs on that PC, the
Client Guard's machine-wide rotating logs on that PC, and Admin's own rotating
logs plus the SQLite rows that recorded the symptom.

Diskless PCs (ADR-0005) make this worse than "go read the files": a reboot
erases the Client's and Guard's local logs, so by the time the owner notices
the Alert, the evidence may already be gone. And the PC that most needs
diagnosing is frequently the one that is powered off, unreachable, or whose
Client never started — exactly the PC no on-demand fetch can reach.

The concrete case that motivated this: a Client Guard Coverage warning read
`unavailable` for a PC, and three different root causes produce that identical
report — the Guard rejecting the Client's image as foreign, a reconnect gap
after a legitimate pipe drop, and a Guard that is not running. Distinguishing
them required reading Guard logs and Client logs side by side, and the owner
had neither.

[ADR-0016](0016-diagnostic-logging.md) anticipated this and left it open: "a
lightweight client→server diagnostic-forwarding path is needed (which client
events count as 'meaningful' is a tuning detail)." A minimal answer already
shipped: the Client posts individual error entries to a rate-limited
`POST /api/diagnostics`, and Admin writes them into its own rotating logs. It
covers a handful of one-off Client events only — no allowlist buffer, no
batching, no ack-gated retention, no source discriminator, and no Guard
participation — so it cannot carry the evidence load this ADR describes. This
ADR closes ADR-0016's open consequence and replaces that minimal path.

## Decision

### Diagnostic Forwarding runs always; the Diagnostic Request tops it up

**Diagnostic Forwarding** is always on, with no switch to forget. The Client
and the Client Guard push their meaningful log lines to Admin as they are
produced, and Admin stores them in its own rotating diagnostic files, tagged
by PC and source. The evidence therefore survives the PC rebooting, the Client
dying, and the PC being powered off — it is already on the Admin host before
anyone notices the symptom.

The **Diagnostic Request** is the top-up. When the PC is still alive and the
forwarded allowlist was not enough, Admin places a single-use nonce on the
Heartbeat response; the Client uploads one windowed, byte-capped slice of its
full log, which the next bundle includes. The Client acts once per nonce, so a
repeated Heartbeat response cannot cause repeated uploads, and truncation of
the slice is stated in the resulting bundle.

Rejected: **pull only**. The PC being debugged is frequently powered off,
unreachable, or freshly rebooted, so a pull reaches exactly the evidence it
cannot reach; the evidence must already sit on the Admin host before anyone
asks. Rejected also: **forwarding only**. An allowlist cannot anticipate every
bug, and the unusual failure lives in lines nobody predicted; a live PC must
be able to hand over a windowed slice the allowlist did not forward.

### Forwarded Diagnostics go to Admin's rotating files, never SQLite

Forwarded Diagnostics are appended to a dedicated daily-rotating log file
alongside Admin's own diagnostics, sharing its rotation and seven-day
retention. Every line carries the PC identity and a source discriminator
(`client` or `guard`), so the bundle builder filters without guessing.
Nothing is written to SQLite, and no migration is created.

Rejected: **SQLite rows**. Forwarded Diagnostics are high-volume, disposable
records whose value collapses once the bug is fixed — ADR-0016's own reason
for keeping diagnostics out of durable stores — and the database is the source
of truth for money, time, and sessions. Making high-volume diagnostics compete
with it buys nothing the rotating files do not already provide.

### Asymmetric transport: the Client dials its own endpoint; the Guard rides its poll

The Client gains a device-authenticated endpoint on the existing `:3210`
client/device surface that accepts a batch of Forwarded Diagnostics,
replacing the existing single-entry `/api/diagnostics` path (no batching, no
failure retention, no Guard participation). It is
deliberately not the Heartbeat: the Heartbeat carries authority values and
must stay small and prompt, and a failed diagnostic send must never affect
session correctness. Sends run asynchronously with respect to the Heartbeat
loop.

The Guard's forwarding rides the poll request it already makes (ADR-0077): the
poll body is widened to carry a strictly bounded batch, with the remainder
draining across later polls, and the response continues to carry its existing
order. No second network call is added — ADR-0077 chose a hand-written
HTTP/1.1 request precisely to keep the SYSTEM service small, and a second one
would erode that. The Guard uses its cached Admin endpoint, so a Guard whose
Client never connected this boot can still forward its own lines; an
unprovisioned PC with no cached endpoint forwards nothing rather than failing
loudly.

Rejected: **widening the Heartbeat for the Client too**. One transport would
be simpler, but it couples a diagnostic payload to the authority path — a slow
or failing send could delay or fail timer synchronisation, the one failure
diagnostics must never cause. Rejected also: **a second Guard network call**,
for the same reason ADR-0077 kept the Guard to one request.

### Release-only, with an explicit development opt-in

Forwarding is gated to release builds by default, mirroring the stance the
Client already takes for its Client Guard Coverage report. The environment
variable `OPENPISO_FORWARD_DIAGNOSTICS=1` turns it on deliberately in a
development build. The gate applies only to the device-side senders — the
Admin sink, the Support Bundle builder, and the Web Admin surface are
unconditional, so nearly everything is testable without a release install.
The Diagnostic Request sender (#364) is a third device-side sender and is
gated the same way: a development Client that is not explicitly opted in
ignores the nonce and uploads no slice.

Rejected: **on in development builds too** — a development build pointed at a
shop's Admin would fill the forwarded log with local noise. Rejected also:
**off in development with no override** — the real path could then only be
watched after shipping.

### One shared forwarding buffer

A single pure type in `openpiso-core` owns the forwarding logic both senders
drive, so the two sides cannot drift in what they consider meaningful: the
allowlist predicate (WARN and ERROR unconditionally, plus a named set of INFO
events — Guard pipe connect and disconnect, Client Guard Coverage transitions,
Heartbeat failure, Client startup milestones, Offline Shutdown Countdown arm
and disarm, Tamper Kill — under a rate cap above the allowlist), a bounded
ring, drop-oldest overflow that emits an explicit "N lines dropped" marker so
gaps are visible rather than silent, batch extraction, and ack-gated
retention: lines stay until Admin returns a 2xx for the batch carrying them,
then are dropped, and a failed send retains them for the next attempt. The
type has no Windows, tokio, or network dependency.

### Process-lifetime context convention

Every meaningful Client lifecycle line — startup milestones, Guard pipe
connect and disconnect, Client Guard Coverage transitions, and the sibling
startup-arbitration evidence (#366) — carries a compact lifetime context as
ordinary structured tracing fields: `process_id`, the OS process ID, and
`launch`, a per-launch discriminator derived once per process from the launch
wall-clock and the PID (issue #368). The shared renderers fold the fields
into the message (`(process_id=4242 launch=1a2b3c4d)`), so the context rides
the unchanged Forwarded Diagnostic envelope, redaction, named INFO
qualification, and rate limits — no new payload, schema, or wire field — and
the Diagnostic Request slice re-renders it identically. A reused PID must
never imply one continuous process, so the discriminator exists; lines from
before the convention lack the fields and still render. The context is
diagnostic only: it carries no identity, authentication, token, raw command
line, or routing authority.

### The Support Bundle builder is a plain function

A free function taking the log directories, the database pool, a window or an
incident anchor, and options, and returning a Markdown string — testable
without HTTP or UI, mirroring the Coin Debug Bundle builder in shape and in
its security stance. Sections render in order — Environment, Verdict,
Timeline, Anomalies, raw appendix — so findings precede data and survive
truncation. Defaults are a six-hour window and roughly a 250 KB cap, wider
than the Coin Debug Bundle's two hours, because a coverage problem accumulates
over a grace period and is noticed hours later, whereas a coin problem is a
burst; an incident-anchored bundle windows roughly one hour before to fifteen
minutes after the incident rather than backwards from now.

Redaction is by construction: device secrets are structurally excluded (no
query or parse path selects them), account identities are pseudonymised to
stable per-bundle labels, and the raw appendix is re-serialised from a field
whitelist rather than passed through.

The **Verdict** names the failure shape in plain language, or says plainly
that no known shape matched — silence is never mistaken for "nothing wrong".
Six rules ship: foreign Client image rejected by the Guard (expected and
actual paths, rejection count), Guard not running or not polling, Client
reconnect flap (reported as a duty cycle over the window), Offline Shutdown
Countdown armed, clock skew beyond threshold, and the coverage contradiction
where Client Guard Coverage reports `unavailable` while the Guard reports a
connected Client. Rules 1, 3, and 6 exist to separate the three root causes
that produce an identical `unavailable` report. Amended (2026-09-08, issue
#368): the coverage-contradiction finding states both observations — the
stored `unavailable` report and the recent connected Guard Poll — and
explicitly declines to assert a cause; contradictory observations sampled at
different times do not prove a reporting defect, a duplicate Client, or any
other source.

### The Web Admin surface is read-only and unstepped

The bundle is reachable from the PC Monitoring Incident that alerted the owner
(anchored to that incident) and from a Support section under Settings (a PC
and window picker), alongside the existing Coin Debug Bundle. It requires only
the Owner Session — no PIN step-up — because the bundle is redacted and
read-only, and step-up is reserved for authority actions. It offers inline
preview and a Markdown file download, and deliberately offers no clipboard
copy: the Web Admin is served over a plain-HTTP LAN origin, an insecure
context where the clipboard API is unavailable.

## Considered options

- **Building the bundle first against Admin's existing logs.** It ships a
  button that is empty for exactly the PC being debugged. Forwarding lands
  first.
- **ADR-0016's interleaved Logs screen.** One screen showing the Audit Log
  with a "show developer logs" toggle interleaving diagnostic entries.
  Superseded by this ADR before it was ever built: the Support Bundle is the
  developer-log surface, and the Audit page already covers the audit half.
- **Live log streaming or tailing a PC in real time.** A standing connection
  per PC for a question asked occasionally; the bundle answers after the fact
  at a fraction of the cost.
- **Backfilling logs produced before this feature ships.** They do not exist
  on diskless PCs and cannot be gathered from PCs that are already off.

## Consequences

- [ADR-0016](0016-diagnostic-logging.md)'s open forwarding consequence is
  closed, and its interleaved Logs-screen viewer is superseded. ADR-0016
  retains the local diagnostic-log store, its rotation and retention, and the
  rule that routine client noise stays local and evaporates.
- A new device-authenticated endpoint, a widened Guard poll body, and new
  Heartbeat response fields reach `docs/api.md` with the implementation
  tickets that add them; the existing single-entry `POST /api/diagnostics`
  section is superseded then.
- No schema change: Forwarded Diagnostics never touch SQLite, so there is no
  migration and no new table to back up.
- The forwarded log inherits Admin's existing rotation and seven-day
  retention, so the feature's storage cost is bounded by construction.
- Money, time, points, sessions, and routing are untouched: the whole feature
  is diagnostic and grants no authority.

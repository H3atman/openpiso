# ADR-0043: Coin ingestion sampling and observability

> **Amendment:** [ADR-0070](0070-credit-the-pulse-sum.md) keeps Pulse
> Sampler, quiet-gap, trace, transport, and retry diagnostics, but removes
> Pulse Map mismatch diagnostics and live Coin Batch Rejection production.
> The owner-facing S1 **Acceptor Health** read reports the current **Coin
> Debug Window** start, `timing_status: unavailable_until_filtered`, and a
> null `implausible_timing_rate`. It has no per-batch unmapped-event list and
> does not relabel historical mismatch evidence as **Implausible Timing**;
> valid raw evidence remains available to the **Coin Debug Bundle**.

- Status: Accepted
- Date: 2026-07-23
- Deciders: shop owner (product)
- Refines: ADR-0023 (serial coin input and terminal selection).
- Refined by: [ADR-0064](0064-owner-visible-coin-loss-notices.md) for the
  owner-visible coin-loss boundary and Notice treatment.
- Relates to: ADR-0015 (audit log), ADR-0016 (diagnostic logging), ADR-0024
  (web-admin sole owner surface), and ADR-0033 (typed coin-source trust).

## Context

The original coin path read the **Coin Pin** once per 500 ms selection-loop
iteration. A typical acceptor pulse is 20–100 ms wide, so most pulses had
already ended before the next sample. The inter-coin gap was 200 ms, shorter
than that sample period. A pulse that happened to be seen was therefore
followed by the next loop's silence and every **Coin Batch** closed at one
pulse; a five-pulse ₱5 coin could not assemble. The existing tests did not
catch this: they supplied injected instants directly to batch assembly and
never exercised production sampling cadence.

The same path was difficult to investigate. At the time of this decision,
pulses that never formed a batch left no evidence, and an unmapped Coin Batch
had no owner-visible event. There was no identifier joining a physical coin's
log records to its stored transaction. An owner also had no bounded, redacted
export to hand to someone helping diagnose a bench failure. The later boundary
between unmapped Coin Batch Rejections and commit failures is refined by
ADR-0064.

These problems are coupled. A trace id is useful only when a sampler produces
real multi-pulse batches, and a debug bundle is useful only when it can join
those batches to their commit and database records.

## Decision

### Pulse Sampler

A serial **Pulse Sampler** runs on its own thread and polls the Coin Pin every
approximately 30 ms. Its observation state starts unseeded at initial open and
after reset or port recovery. The first normalized Coin Pin sample establishes
previous-level state and reports no edge or pulse; only a later low-to-high
normalized transition can be a rising edge. This rule applies to both supported
polarity values and uses the same sampler path for initial open and recovery.
The sampler performs rising-edge detection only and writes pulse timestamps to
the shared observation buffer. It does not apply the gap, pulse map, pricing,
selection, or commit policy. The serial port is shared between
the sampler and the selection path behind a standard mutex; that port mutex is
distinct from the async Terminal Selection mutex.

The 500 ms selection loop retains selection timeouts, batch closing, commits,
event publication, and UI snapshots. It drains the sampler's buffer instead of
reading the pin. The loop is deliberately not sped up: running database
commits and event publication 100 times more often while holding the global
selection lock has no useful sampling benefit. The accepted consequence is up
to approximately 500 ms of latency between the quiet gap and crediting.

The sampler is serial-only; the simulator injects pulses and starts no
thread. It stops on source drop/shutdown and the source inhibits the Set Pin
at construction, close, failure, and drop. The sampler free-runs so it can
observe edges outside a Selection Window.
It admits a rising edge while the Set Pin is asserted, and it may admit a later
edge while the shared buffer is non-empty so a train can straddle inhibition.
Only an edge observed with an empty buffer while inhibited is refused and
cannot become money.

The default inter-coin gap is 600 ms, through migration
`0022_inter_coin_gap_default.sql`: a conservative default for typical pulse
coin acceptors. It remains owner-editable and requires characterization against
the installed acceptor; the default is not a calibration result.

### Inhibited Pulse

An **Inhibited Pulse** is a rising edge observed while the Set Pin is not
asserted and the shared pulse buffer is empty. The reader exposes both facts —
edge observed and pulse admitted — so this edge is refused, recorded as an
anomaly, and never enters the pulse buffer used for a Coin Batch. A rising edge
while inhibited with a non-empty buffer is admitted as the tail of the already
started train; it is not an Inhibited Pulse and remains part of that Coin Batch.
The aggregate Audit Log Notice carries an observable cause in its existing
`reason` metadata: the Coin Source was unavailable or recovering, or the
sampler was healthy while no Selection Window was open and the Set Pin was
inhibited. The healthy state cannot distinguish one idle insertion from an
inhibit wiring, polarity, or acceptor fault because the serial API exposes the
commanded Set Pin state but no electrical feedback. Its summary states both
possibilities and directs the owner to the Coin Debug Bundle, whose anomaly
table and raw appendix retain the observable reason and timestamp for field
feedback. Each observable cause keeps its own bounded aggregate, so a mixed
period emits separate truthful summaries. Summaries report refused pulses,
never coins or pesos, and carry no PC or Account: an empty buffer provides no
truthful Selection Window attribution. Inhibited-pulse logging is rate-limited
(five entries per ten-second window) to prevent a noisy line from flooding
diagnostic files.

Coin-path edges and batch details are INFO records; raw per-sample pin state
is DEBUG. Commit rejection branches — failures after a mapped Coin Batch
reaches the authoritative commit path — also log their reason at warning
level. Historical Pulse Map mismatch evidence remains available to the Coin
Debug Bundle, but S1 has no live mismatch detector or Coin Batch Rejection
producer. The diagnostic target is module-scoped so a bench run can increase coin-path verbosity
without enabling unrelated SQL logging. The levelling choice is an
operational setting, not a separate architecture decision.

### Coin Trace Id

A **Coin Trace Id** is minted when a Coin Batch closes and is carried by the
batch value itself. Every commit attempt for that batch uses the same id and
an incrementing attempt number. A retry therefore remains one physical coin,
and a new id is never minted merely because a commit attempt failed.

The id is stored in the existing Audit Log event's metadata JSON. The coin
transaction already links to that audit event, so this joins diagnostic log
records and the database without a schema migration. JSON extraction is an
accepted, unindexed lookup trade-off at shop volumes; a trace-id column and
index are deferred until evidence shows they are needed. The audit metadata
contains no device secret.

Minting a fresh id per commit attempt was rejected because it models the
transport rather than the physical coin, makes retries look like multiple
coins, and forces consumers to carry two identities. Minting at commit time
was also rejected because it cannot identify the ingestion records and cannot
survive a retry.

### Coin Debug Bundle

The owner-authenticated `GET /web-admin/coin-device/debug-bundle` endpoint
builds a **Coin Debug Bundle**. The web-admin coin-device settings page puts
its "Copy Coin Debug Bundle" button beside "Test acceptor". The handler is a
thin wrapper; the builder joins the rotating diagnostic log directory and
SQLite using the Coin Trace Id.

The bounded Markdown document contains, in order:

1. Environment — app version, timestamp, OS, resolved coin configuration,
   simulator state, and active rate;
2. Verdict — computed inhibited-pulse, missing-database-row, amount-mismatch,
   and duplicate-trace findings; historical mismatch log fields remain
   whitelisted raw evidence only;
3. Timeline — one row per joined coin;
4. Anomalies; and
5. a JSONL raw appendix.

The bundle receives the current admin app-start boundary from the owner-server
state. Its represented window always ends at generation time and begins at the
later of app start or two hours before generation. Logs and database
transactions are filtered to that window before labels, verdict, timeline,
anomalies, or appendix are computed. The timeline is capped at the 50 most
recent physical coins and the complete document has an approximately 80 KiB
hard ceiling. Timeline rows originate from successful `Coin Batch closed`
evidence, then join all commit attempts and persisted transactions by Coin
Trace Id. Thus a retry enriches the same row, which reports the total attempt
count and final database outcome; a closed batch with no persisted transaction
remains a `missing` row/anomaly. Duplicate persisted rows remain a `duplicate`
outcome/anomaly. Size reduction removes appendix content first, then oldest
timeline rows; Environment and Verdict are retained. The coverage header
reports the exact represented window and separately identifies time-window,
timeline, appendix, and byte truncation.

Device secrets are structurally excluded: bundle queries do not select the
secret column at all. This is safer than selecting broadly and applying a
blacklist, which fails open when a column or wildcard is added. Account
usernames are also excluded and replaced with stable per-bundle account
labels. The appendix is re-serialized from a field whitelist rather than
passing through raw log text, so future coin-path fields cannot silently
become exported data.

A command-line exporter was rejected in favor of the owner-authenticated
in-app surface (with the accepted limitation that the bundle is unavailable
when the admin app is stopped or wedged). Separate ADRs for the sampler,
trace id, and bundle were rejected because each decision depends on the other
two.

## Consequences

- Field-observed 20–100 ms pulse trains can be captured by the approximately
  30 ms sampler and can assemble into a multi-pulse Coin Batch; this is not a
  guarantee for pulses narrower than the polling period. The existing
  injected-clock batch and routing logic remains in place.
- A refused Inhibited Pulse is observable but inert; an admitted tail of a
  buffered train remains creditable as part of that Coin Batch. The fail-closed
  Set Pin invariant and server-authoritative pricing/routing are unchanged.
- Logs, Audit Log metadata, and coin transactions can be correlated without a
  migration, at the cost of an unindexed JSON lookup.
- Owners can export useful findings without exposing device credentials or
  customer usernames. The export is truthful about current-run coverage and
  every applied bound.
- The 600 ms gap is a field-informed starting point, not a claim that the
  actual acceptor has been calibrated. Bench results must revisit the gap and
  pulse map.
- The sampler/trace/bundle behavior shipped across #93–#98; log levelling and
  Markdown formatting remain deliberately reversible implementation details.

## Rejected alternatives

- **Speed up the whole selection loop:** rejected because it would run
  timeouts, commits, database work, and event publication unnecessarily often
  under the global selection lock; it would still be the wrong ownership
  boundary for pulse sampling.
- **Mint a trace id per commit attempt:** rejected because retries would look
  like separate coins; the physical coin, not its transport attempt, owns the
  identity.
- **Store a trace-id column immediately:** rejected to avoid a migration; the
  existing audit-event foreign key plus JSON metadata is sufficient at shop
  volumes.
- **Blacklist secrets from a broad bundle query:** rejected because blacklist
  filtering fails open; structural column exclusion is safer.
- **Use a separate CLI bundle exporter:** rejected to keep the owner on the
  sole web-admin surface; the app-stopped limitation is accepted.

## Verification

The shipped implementation includes deterministic tests for edge detection,
realistic-width multi-pulse trains, straddling-train admission, the 600 ms
ten-pulse gap, refused inhibited-pulse inertness and rate limiting, trace-id
retry/metadata behavior, rejection logging and claim redaction, bundle anomaly
detection, joining, bounding, and redaction, plus an owner-gated endpoint test.
The repository checks remain the verification gate for the complete change.

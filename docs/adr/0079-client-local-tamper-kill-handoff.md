# ADR-0079: A durable local handoff carries Tamper Kill reports to the running Client

- Status: Accepted
- Date: 2026-09-08
- Deciders: shop owner (product)
- Relates to: [ADR-0048](0048-client-guard-tamper-kill.md) (the Tamper Kill
  policy this handoff serves: server-owned attribution, receipt-anchored kill
  instant, offline retry),
  [ADR-0045](0045-offline-shutdown-countdown.md) (offline behaviour the retry
  path lives inside),
  [ADR-0031](0031-client-config-persistence-and-pc-identity.md) (the Client's
  per-user config directory this inbox lives beside),
  [ADR-0078](0078-support-bundle.md) (the Diagnostic Forwarding allowlist the
  handoff's INFO events ride), and
  [ADR-0029](0029-client-window-model.md) (the startup reveal fallback the
  harness seam skips).
- Implemented for: openpiso#367 (this slice). The dependent startup gate
  (openpiso#369, parent openpiso#366) consumes the boundary described here.
  openpiso#371 corrected two mechanisms described below — the delivery
  ledger and interprocess capacity admission — without changing the
  boundary's contract.

## Context

ADR-0048's Guard relaunches the Client with two integers —
`--openpiso-tamper-kill <kill_timestamp_unix_ms> <attempt_count>` — and the
relaunched Client files the report on its first Heartbeat, retries through
Admin outages, and clears it only on the server's `recorded` acknowledgement.

The Client startup work (openpiso#366) will make a second ordinary launch
inert before it can create a window or start a second Heartbeat stream. That
gate creates a new obligation: a **losing** launch may be the one carrying the
Guard's report arguments. An early return that ignores them silently destroys
the only evidence of a real death, and a losing process must never open a
second Heartbeat stream to say so. Before duplicate rejection can become
inert, an already-running Client must be able to *accept* a validated report
from another local launch and deliver it through the existing authoritative
path.

The mechanism must survive delivery retries, an Admin outage, and the death
of either party at any point, without letting a sender overwrite the live
Client's pending-report state, without inflating attempt counts when one
delivery is retried, and without a second window, Guard connection, network
listener, service, or production dependency.

## Decision

A **file inbox beside the authoritative report** is the local handoff
boundary, owned by the Client's startup coordinator
(`apps/client/src-tauri/src/startup_coordinator.rs`):

- **Sender.** A launch that will not become the runtime validates its
  arguments at the Rust boundary (`parse_report_args`: positive integer
  timestamp and attempt count; anything else, including ordinary launches and
  report-free Guard level checks, can never manufacture a report), atomically
  writes one small JSON file into `tamper-handoff-inbox/` inside the Client's
  config directory, prints one receipt line, and exits. The file name is the
  report identity — `report-<timestamp>-<attempt_count>.json` — so retrying
  one delivery rewrites the same file. Exit codes tell the truth: `0` durable
  accept, `2` invalid arguments, `3` unpersistable (retry later). The sender
  never touches the authoritative `tamper-kill-report.json`.
- **Receiver.** The running Client drains the inbox at startup (before the
  first heartbeat) and on a one-second runtime loop, merges each file into
  the pending report under the existing coalescing rules, **persists the
  merged report before removing any inbox file**, and bumps the
  `tamper_kill_generation` barrier so an active SSE stream cannot apply stale
  pre-report state. Ingestion and server-acknowledgement clearing serialize
  on the connection-transition mutex, so a delayed acknowledgement for an
  older report can never erase a newer handed-off one. Clearing still
  requires the existing server acknowledgement; a local accept means
  *durably recoverable*, never *Admin acknowledged*.
- **Delivery identity is distinct from Tamper Kill coalescing.** The
  identity of a delivery is the pair the Guard handed over, not the coalesced
  aggregate the Client holds. Comparing a delivery against the aggregate is
  not enough: once two distinct reports coalesce, neither constituent equals
  the aggregate any more, so a retry of either would read as a new report and
  inflate the attempt count. The authoritative file therefore carries a
  **bounded delivery ledger** — every delivery identity already folded in —
  and a delivery whose identity is in the ledger is dropped as a retry.
  Genuinely distinct deliveries keep the existing oldest-timestamp-wins,
  attempt-coalescing semantics. The ledger covers deliveries through the
  inbox and through the Guard's relaunch arguments alike (openpiso#371).
- **Ledger lifetime.** The report and its ledger are one atomic write, so no
  crash can record a delivery without its report or a report without the
  identity that recognizes its retry. The ledger **survives the server
  acknowledgement**: the acknowledged report is cleared from the file while
  the identities stay, so a delayed retry of an already-acknowledged delivery
  has no new consequence. It holds at most 64 identities, dropping the oldest
  kills first, because a retry always chases a recent delivery.
- **Durable input validation.** Inbox files satisfy the same positive-integer
  contract as the command-line boundary: a zero or negative kill timestamp,
  like a non-positive attempt count, is rejected at ingestion and never
  becomes a pending report (openpiso#371).
- **Bounds.** The inbox holds at most 32 files of at most 4 KiB each; an
  inbox-file rejection is fail-closed (the sender exits `3` and evidence is
  never dropped). Corrupt or oversized inbox files are removed at drain time
  and never become reports. Because capacity admission is a check followed by
  a publish, it runs under one **interprocess admission lock** — an
  atomically created directory inside the inbox — so concurrent senders
  cannot each observe the same spare slot and collectively exceed the cap. A
  sender that cannot enumerate the inbox, or cannot claim the lock within its
  bounded wait, fails truthfully (exit `3`) rather than admitting a report it
  cannot show there is room for; re-delivering an already-published report is
  admitted even at capacity. The lock is a single record, reclaimed by the
  next sender once it outlives any plausible holder, so a crashed sender
  cannot wedge or grow the inbox. Draining is deliberately not serialized
  with senders — a receiver must never wait on one to start — so it removes a
  sender staging file only once that file has outlived every bounded sender
  path, and a concurrent drain cannot fail a live sender's publish
  (openpiso#371).
- **Diagnostics.** Receipt and failure emit concise events on the
  `openpiso_client::tamper` forwarding target (ADR-0078's "Tamper Kill"
  allowlist), so the independent diagnostic slice can retain them without
  assuming that slice has landed. The sender prints one stdout/stderr line
  per outcome.

### Why a file inbox

It is the smallest local-only mechanism that meets the contract. Atomic
persistence comes from the existing tmp-file-plus-rename writer shared with
`config_store`; bounded data comes from the two caps; retry identity comes
from the deterministic file name; receiver lifetime changes come from the
filesystem itself — inbox files survive any process death and are drained by
the next admitted owner. A named pipe, local socket, or shared-memory
protocol would need a receiver that is always listening, hand-rolled framing,
and its own crash protocol, with no additional guarantee.

### Trust boundary

The inbox lives in the same per-user directory as the Client config —
writable by exactly the standard user who can already write that directory
and launch the Client. Local launch arguments and inbox files are **hints,
never authority**: they carry only a kill timestamp (the stable report name)
and an attempt count; they are validated at the Rust boundary; attribution,
the kill instant, and every consequence remain the server's (ADR-0048). This
boundary claims no strength beyond today's Guard launch arguments. The Guard
pipe stays write-only; there is no LAN endpoint, no general command bus, no
new service, and no new production dependency (`axum` joins the Client crate
as a dev-dependency only, for the isolated test Admin).

`OPENPISO_CLIENT_CONFIG_DIR` redirects every Client-local file — config,
report, inbox, and logs — for tests and the process harness. Production never
sets it; a process running under it is definitionally not a kiosk install, so
it also skips the startup reveal fallback (ADR-0029). A harness receiver
therefore cannot paint a fullscreen window over the developer's desktop, and
harness persistence never touches a real install.

### How the startup gate consumes this (openpiso#369)

The duplicate gate arbitrates ownership before visible startup. Its losing
branch parses the report arguments through the same
`parse_report_args` boundary, calls the same `submit_report`, and may only
report a handled-success outcome after exit code `0`; an unpersistable
submission is a coordination failure, not a benign duplicate. The gate never
reads transport state, never touches the authoritative report file, and
inherits crash recovery: if the winner dies before draining, the successor's
startup recovery picks the inbox up.

## Consequences

- At most one Heartbeat stream ever reports a kill: the sender's whole
  runtime contribution is one inbox file and one exit code.
- Crash windows are recoverable by construction: the authoritative file
  (report plus ledger, one atomic write) first, inbox delete second; a crash
  between the two leaves the inbox file, whose re-ingest is a no-op by
  delivery identity; a failed persist leaves every inbox file for the next
  drain.
- Retry state is bounded and documented: at most 32 inbox files, at most 64
  delivery identities, one admission lock record, and the single
  authoritative report file; nothing grows with retries.
- The authoritative report file gained two shapes it did not have before: it
  may carry a delivery ledger alongside the report, and after an
  acknowledgement it may exist carrying only the ledger. A historical file
  holding just the two report fields still loads, still coalesces, and gains
  a ledger in place; a file with neither a report nor identities is removed.
- The Admin outage story is unchanged from ADR-0048: reports persist and
  retry through the existing Heartbeat path; only the server's
  acknowledgement clears them.
- The process-level Windows harness
  (`apps/client/src-tauri/tests/handoff_harness.rs`) is the shared seam for
  the startup tickets: real sender/receiver child processes of the production
  binary, one isolated config directory and test Admin per scenario, observed
  through persistence and recorded Heartbeat requests — never private flags.
  The focused regression command and its outcome are recorded in
  [development.md](../development.md).
- Receiver death *during* the microsecond-wide persist-then-delete window is
  covered by the unit-level crash-window regression rather than a timed
  child-process kill; every other death point (before receipt, after persist
  before server acknowledgement, after local acceptance) is covered at the
  process level.

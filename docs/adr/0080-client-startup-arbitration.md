# ADR-0080: Startup arbitration admits exactly one Client runtime

- Status: Accepted
- Date: 2026-09-08
- Deciders: shop owner (product)
- Relates to: [ADR-0079](0079-client-local-tamper-kill-handoff.md) (the durable
  local handoff a report-bearing loser must use before it may exit as a
  handled duplicate),
  [ADR-0048](0048-client-guard-tamper-kill.md) (the Client Guard and Tamper
  Kill policy whose classification, disarm/re-arm, and never-give-up relaunch
  behavior this gate must not disturb),
  [ADR-0029](0029-client-window-model.md) (the one-window model the losing
  launch must not disturb),
  [ADR-0028](0028-client-kiosk-os-hardening.md) (the keyboard hook that must not
  be installed twice), and
  [ADR-0078](0078-support-bundle.md) (the diagnostic conventions the
  arbitration evidence follows).
- Implemented for: openpiso#369 (parent openpiso#366). Revisits the closed
  proposal openpiso#224 and supplements the shipped exact-child liveness gate
  of openpiso#233.

## Context

The installed Windows Client had no single-instance gate before its startup
placeholder, keyboard hooks, Guard connection, Tauri/WebView initialization,
state writes, or networking. Ordinary competing launches — Windows Autostart
versus a shortcut, a double-click versus a Guard relaunch — could each start a
full runtime. Investigation (openpiso#366) reproduced the resulting
Client Guard Coverage contradiction: one Client connection holds the Guard
pipe while a second independent connection fails with pipe-busy and reports
`unavailable`, alongside competing window surfaces.

Two prior decisions bound this work:

- **openpiso#233 (shipped)**: the Client Guard tracks the exact child process
  it launched — retained process handle plus PID — and suppresses further
  relaunches while that child lives. This is valuable but only serializes
  *Guard-launched* children; it cannot see independent launches from
  Autostart, a shortcut, or a manual double-click.
- **openpiso#224 (closed NOT_PLANNED)**: an earlier proposal for a named-mutex
  single-instance gate. It was rejected when the only available design was
  "mutex plus unconditional early exit": such a gate silently drops a losing
  launch's Guard-supplied Tamper Kill report, and its exit is
  indistinguishable from a real clean exit.

The missing piece that made rejection correct then is the durable local
handoff (ADR-0079): a report-bearing launch can now hand its validated report
to the running Client without ever becoming a runtime.

## Decision

**One startup coordinator decides admission at the top of the Client
launch boundary, before any externally visible startup.** The launch either
owns a lifetime-scoped OS object or it does not.

- **Mechanism.** A machine-wide (`Global\`) Win32 mutex with the fixed name
  `Global\OpenPiso-Client-Runtime`. `CreateMutexW` followed by a
  zero-timeout `WaitForSingleObject` is the atomic check-and-own step — the
  kernel serializes competing processes, so a check-then-start race cannot
  admit two runtimes. The name carries no working directory, shortcut,
  executable path, or version component, so none of those can admit a second
  participating production runtime.
- **Scope.** `Global\` makes the object machine-wide: a production Client in
  another Windows session competes for the same object instead of racing for
  the machine-wide Guard pipe under a session-local name. A
  conflicting-session loser exits inertly; there is no takeover, no
  migration, and no process termination. The supported deployment remains one
  active interactive shop session. The coordination object itself cannot
  identify *which* session owns the runtime — doing so would need session
  enumeration, which this design deliberately avoids — so a
  conflicting-session loser records the same one-line duplicate-suppression
  evidence as any other loser, in its own process's local log, and the
  conflict is diagnosed from that evidence rather than acted on.
- **Non-inheritance.** The ownership handle is created with
  `bInheritHandle = FALSE`, so WebView helper processes, spawned children, or
  test fixtures cannot keep Client ownership alive after the owner exits.
- **Lifetime.** Ownership is retained until process teardown completes: the
  handle lives in a process static and is never closed; the OS releases it at
  process exit. Clean exit, startup crash, and runtime crash all free
  ownership for the next eligible launch with no stale-file cleanup. A
  `WAIT_ABANDONED` acquisition is the safe re-evaluation of ownership after
  owner death — a PID alone is never an ownership token and no PID-reuse
  decision exists anywhere in the gate.
- **Three outcomes, never collapsed.** *Admitted owner* — continues into
  normal startup unchanged. *Handled duplicate* — exits promptly and inertly:
  no placeholder, window, hook, Guard pipe, Heartbeat/SSE/Diagnostic
  Forwarding task, and no unrelated configuration/report/countdown writes. If
  the launch carries validated Guard report arguments, the report is durably
  handed off through the ADR-0079 inbox before the handled-success exit; an
  unpersistable handoff exits `3` (truthful failure), never a benign
  duplicate. Malformed arguments can manufacture no report.
  *Coordination failure* — denied access (exit `4`), an incompatible object
  under the coordination name (exit `4`), or an uncertain wait outcome: the
  launch never admits, never poses as a benign duplicate, and preserves the
  Win32 operation/error code in its evidence.
- **Platform gating.** Enforced in release Windows builds. Debug and
  non-Windows builds stay inert so `tauri dev`, multiple dev instances, and
  the `not_applicable` Client Guard Coverage behavior are untouched. Two
  test-only environment overrides (`OPENPISO_CLIENT_ARBITRATION_ENFORCE` to
  activate the production gate in a debug build, `OPENPISO_CLIENT_ARBITRATION_NAME`
  to isolate the object) exist solely for the process harness; production
  never sets them.
- **Relationship to the Guard.** The gate supplements, never replaces, the
  #233 exact-child handle/PID liveness gate, PID-matched connection
  recognition, status-error behavior, bounded backoff, and the never-give-up
  policy. A losing launch never opens the Guard pipe, so it cannot disarm the
  Guard, fabricate a Tamper Kill, or send a clean-exit message on the
  surviving Client's behalf. Owner Quit and a later legitimate start keep
  their existing disarm/re-arm semantics.
- **Diagnostics.** Every outcome emits one compact evidence line with
  process-lifetime context (PID plus the per-launch token of ADR-0078's
  convention) on the `openpiso_client::startup` target and one
  `openpiso-arbitration:` console line. Rejected processes are rate-bounded
  by construction — exactly one line per process — and never start their own
  network forwarder. The mutex mechanism permits no owner-side aggregate
  count; suppressed launches are visible through their own local evidence
  only.
- **Rollout gate.** Coordination works only between participating binaries.
  A legacy Client already running from before an upgrade never acquires the
  object, so this gate cannot evict or coordinate it. Rollout validation
  requires a clean restart into the updated installed Client; live coexistence
  with older copies is explicitly not promised.

### Revisiting openpiso#224

The earlier named-mutex proposal was closed `NOT_PLANNED` for two reasons,
both now addressed deliberately:

1. *A losing launch's Tamper Kill report would be silently dropped.* The
   losing branch now consumes ADR-0079's handoff boundary and may only report
   a handled-success outcome after the report is durably recoverable; an
   unpersistable submission is a truthful failure. The same boundary serves
   the sender-only mode, the startup recovery path, and this gate.
2. *A mutex plus unconditional early exit is indistinguishable from a clean
   exit and hides real failures.* The gate now distinguishes admitted owner,
   handled duplicate, and coordination failure; failure outcomes carry the
   Win32 error and exit nonzero; the loser never opens the Guard pipe, so it
   cannot be mistaken by the Guard for anything at all.

What remains of #224 is exactly the kernel-object idea — chosen again because
a lifetime-scoped mutex has no stale files to clean up, no PID-reuse
ambiguity, and no cleanup process to write. What is new is everything around
it: the report handoff, the outcome taxonomy, the platform gating, the
evidence, and the process-level test seam.

## Consequences

- At most one participating production Client per shop PC owns a runtime,
  regardless of launch source, timing, or startup-stage delays of the owner.
- A duplicate launch costs one process lifetime of microseconds: no second
  window, no second Heartbeat stream, no second Guard connection, no
  geometry or focus fights.
- The admitted owner's startup evidence and the duplicate-suppression and
  handoff-outcome evidence land even if the independent diagnostic slice has
  not landed; they ride the existing local log and console conventions.
- The process-level Windows harness
  (`apps/client/src-tauri/tests/handoff_harness.rs`) remains the single
  shared seam (no second ownership simulator): real contender processes of
  the production binary run the production entrypoint ordering with a
  barrier-start event, real native objects, and owner-death injection. The
  inert pre-fix behavior is captured by the same seam with the gate
  deactivated (a debug-build scenario: the child binary of a release test
  run enforces natively, so release runs exercise the gate without the
  override). Of 100 synchronized stress rounds with two to four contenders,
  the barrier and staggered rounds admit exactly one runtime with every
  other contender a handled duplicate — every contender decides while the
  owner is alive, so that assertion is airtight — and the owner-death
  rounds wait for a contender to actually reach admission, kill it while the
  others are still deciding, and then assert non-overlap directly: each
  admitted process records the instant it was admitted, the kernel supplies
  the instant it exited, and no two of those lifetimes may overlap. A
  contender still starting at the injection may observe the abandoned
  ownership and succeed it; that is the designed owner-death recovery and it
  is a later, non-overlapping lifetime, so the assertion permits succession
  while still rejecting two owners alive at one instant (openpiso#371). That
  the check can fail is demonstrated at the same seam with the gate
  deactivated, where two admitted lifetimes really do overlap.
  Focused scenarios cover denied and
  incompatible coordination objects, non-inheritable ownership handles,
  report handoff from a losing launch to a live owner, inert plain
  duplicates, and ownership recovery after a full runtime crash.
- The gate cannot help a non-participating legacy process (see the rollout
  gate above), cannot coordinate a conflicting Windows session away from its
  owner, and does not change PC Monitoring thresholds, escalation
  predicates, Guard classification, or any monetary/session authority.

# ADR-0082: Admin records its own runs and names why each one ended

- Status: Accepted
- Date: 2026-09-09
- Deciders: shop owner (product)
- Relates to: [ADR-0016](0016-diagnostic-logging.md) (the rotating diagnostic
  files and the ~7-day retention this file deliberately sits outside of),
  [ADR-0078](0078-support-bundle.md) (the Support Bundle that carries these
  facts to whoever reads the evidence), and
  [ADR-0081](0081-diagnostics-surface.md) (the Diagnostics surface that
  displays them).

## Context

An owner reported that the Kiosk had crashed and supplied Admin's
`diagnostics.<date>.log` for the day. The log showed continuous normal coin
traffic and then simply stopped. It could not distinguish a crash from Alt+F4,
from a clean shutdown, or from the log ending because the day rolled over. No
`panic.log` existed either, which was the only real signal available: the panic
hook at `apps/admin/src-tauri/src/diagnostics.rs` writes synchronously and
flushes, so its absence rules out a Rust panic and leaves a silent exit, a
WebView2 crash, an external kill, or OOM — with no way to tell which.

Three specific gaps produced that blindness, all in `apps/admin`:

- **Nothing is written at startup or shutdown.** The Client records a startup
  milestone; Admin records neither, so a restart and an uninterrupted run look
  identical in the log.
- **Buffered log lines die with the process.** `init_tracing` builds a
  non-blocking appender and then calls `std::mem::forget(guard)`, so the
  `WorkerGuard` destructor never runs and the final buffered lines are never
  flushed. Admin also omits `.lossy(false)`, which the Client sets, so lines
  can be dropped silently under pressure. The last lines before an exit are
  exactly the ones worth having.
- **There is no lifecycle handling at all.** No `on_window_event`,
  `CloseRequested`, or `RunEvent` handler exists, so Tauri's default applies
  and closing the kiosk window exits without a word.

The evidence that would answer "did it restart overnight, and why" has to
survive the process that produced it, and has to survive log rotation, since
the question is usually asked days later.

## Decision

### Admin keeps Run Records outside the rotating logs

Admin writes a **Run Record** at startup — app version, PID, start time, no
Exit Reason — and completes it with an end time and an **Exit Reason** when the
process exits through its own shutdown path. Records are stored as a rolling
set of the newest ten in a single small JSON file in the Admin data directory,
**not** in `logs/`, so ADR-0016's retention sweep never removes them and the
history outlives the logs it annotates.

It is a fixed-size file, not a table. Ten records answer "how often has this
restarted lately" without a migration, a query path, or a growth story.

Each save is written to a sibling file and renamed over the record file, so a
failed write leaves the previous history in place. Losing history costs history
rather than correctness, but only a completed replacement should ever cost it.

### A start without an end is a suspected hard exit

Writing at startup is what makes silence legible. When the newest record has a
start and no end, the previous run died without reaching its own shutdown path,
and that is reported as **suspected hard exit** — a WebView2 crash, an external
kill, or power loss. It is stated as an absence of evidence, never as a claim
about the cause.

### The appender is flushed on the way out

The `WorkerGuard` is held in application state for the life of the process
instead of being forgotten, the appender is built with `.lossy(false)`, and a
`RunEvent::ExitRequested` / `WindowEvent::CloseRequested` handler logs the
reason and completes the Run Record before the process goes. Startup logs a
line naming version and PID.

### The Client gets a panic hook, not Run Records

The Client currently installs no panic hook at all, so a Client-side Rust panic
leaves nothing behind. It gains one mirroring Admin's. It does **not** gain Run
Records: reporting them to Admin would be a protocol change, and the Client
Guard already reports client-side coverage gaps (ADR-0077). Revisit if Client
crashes prove to be the recurring problem.

### Run Records reach the owner two ways

The same builder serves the Diagnostics page live and the Support Bundle
offline. `panic.log` is carried as its newest 8 KB in both, so what the owner
sees matches what support reads, and the bundle's size cap stays honest; the
full file remains on disk.

## Consequences

- A restart is visible as a restart, and a silent death is visible as a
  suspected hard exit rather than as a log that stops.
- Holding the guard means shutdown ordering now matters: the guard must outlive
  the exit handler that writes the final lines.
- Ten records is a deliberate ceiling. A machine restarting in a loop overwrites
  its own early history within minutes; the rotating logs remain the record for
  anything longer.
- Run Records describe the Admin process only. A question about a Client PC's
  restarts is still answered by the Guard and the Support Bundle.
- The JSON file is new state in the data directory. It is not authority: it
  holds no money, time, session, or Points data, and losing it costs history,
  not correctness.

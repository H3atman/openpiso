# ADR-0016: Diagnostic logging (rotating files, diskless forwarding)

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0005 (diskless clients), ADR-0015 (audit log).
- Amended: 2026-09-04 — the open diagnostic-forwarding consequence below is
  **closed** by [ADR-0078](0078-support-bundle.md), and the proposed
  "show developer logs" interleaved viewer is **superseded**: the Support
  Bundle is the developer-log surface, and the Audit page covers the audit
  half. The separate store, the rotating files, and the local-retention rules
  below stand.

## Context

Developer/diagnostic logging is in scope alongside the audit log, but it is a
fundamentally different artifact: high-volume, disposable, developer-facing.
The diskless setup (ADR-0005) complicates it: anything written to a client's
local disk evaporates on reboot.

## Decision

### Separate store, unified viewer
- Diagnostic logs do **not** go in the `audit_log` table. They use `tracing`
  to **rotating files** (the audit table stays clean, durable, append-only).
- ~~The admin **Logs screen** shows the audit log by default and has a **"show
  developer logs"** toggle that interleaves diagnostic entries by timestamp —
  one screen, two stores.~~ Superseded by
  [ADR-0078](0078-support-bundle.md): the Support Bundle is the developer-log
  surface, and the Audit page covers the audit half.

### Server diagnostics
- `tracing` → rotating files in the app data dir, **daily rotation, ~7-day
  cap** (ADR-0015 retention). Default level INFO/WARN/ERROR; **DEBUG/TRACE
  opt-in** via setting/env when chasing a bug. Structured fields (`pc_id`,
  `account_id`) so entries align with audit rows.

### Client diagnostics under diskless
- Audit events are **already safe** — they happen *through the server* (login,
  coin, redeem, spin all hit the server API), so they are centrally recorded
  regardless of client reboots. No change needed.
- The Client writes daily JSON files under its per-user app data directory, with
  about seven days of bounded retention. This makes an installed build
  diagnosable during the current boot even though it has no console; the files
  remain disposable and evaporate when the diskless image resets.
- **Meaningful client diagnostics** (errors, crashes, disconnect reasons) are
  **forwarded to the server** and stored in the server's rotating diagnostic
  files, so the one viewer can show them.
- **Routine client noise** (heartbeat OK, normal reconnects) stays local and is
  allowed to evaporate on reboot.

## Rationale

The two stores have opposite value curves: an audit row's value grows with age;
a diagnostic line's value collapses once the bug is fixed. Mixing them would
bloat the audit table and break its never-purge guarantee. And on diskless
clients, "log to local disk" means "discard on reboot" — so only server-side
storage keeps client problems visible the next day.

## Consequences / still open

- ~~A lightweight client→server diagnostic-forwarding path is needed (which
  client events count as "meaningful" is a tuning detail).~~ Closed by
  [ADR-0078](0078-support-bundle.md).
- DEBUG/TRACE toggle should be reachable without a rebuild (setting or env).

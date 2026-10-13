# ADR-0044: Immediate Write Transactions & Clean PC Removal

**Status:** Accepted

## Context

Owners saving settings — most visibly in Settings → PCs on rapid consecutive
saves — intermittently hit `database is locked` (`SQLITE_BUSY`, code 5). The
save failed and had to be retried by hand.

Root cause: the admin's 1-second timer tick is a constant SQLite writer, and
every settings write opened a **deferred** transaction (`pool.begin()` →
`BEGIN`) that read a row before writing it. In WAL mode a deferred transaction
acquires the writer lazily, on its first write. If another connection (the timer
tick) commits between that transaction's first read and its lock upgrade, SQLite
fails the upgrade **immediately** with `SQLITE_BUSY` and never consults
`busy_timeout`. Fast UI saves merely raised the collision odds; any settings
save could hit it. (This is the write-path analogue of the snapshot problem
ADR-0037 fixed for Password Change.)

Separately, there was no way to remove a PC row. A typo'd or mistakenly
pre-registered PC lingered forever; the only recourse was hiding it, which still
cluttered Settings.

## Decision

### Immediate write transactions (global)

A shared `db::begin_immediate(pool)` helper begins every production
read-then-write transaction with `BEGIN IMMEDIATE` (sqlx 0.8 `begin_with`),
reserving the writer up front. Contending writers then block on `busy_timeout`
and queue instead of erroring. The connect options set an explicit 5-second
`busy_timeout`. Read-only paths are untouched; migrations and first-run seeding
(startup, single-threaded) keep the plain deferred `begin`.

This is the rule: every read-then-write transaction begins immediate. New code
that reaches for `pool.begin()` on a write path silently reintroduces the lock
error, so it must use the helper.

### Clean PC removal

Removing a PC is a **hard delete of a clean row only**, done inside one
immediate transaction. The row must have:

- no referencing rows in `coin_transactions`, `manual_adjustments`,
  `wheel_spins`, or `audit_log`;
- `remaining_seconds = 0`;
- status neither `online` nor `in_use`.

A bound-but-offline MAC does not block removal (it is not history). Any violation
returns a conflict whose message names the reason (has history / is online / has
remaining time); an unknown id is 404. There is no schema change — the pcs
foreign keys keep protecting history as a backstop. Rows with history stay and
are hidden via the existing `visible` flag, so historical transactions of
decommissioned hardware are preserved and revenue reports stay accurate.

Removal appends a `pc_removed` audit event. Because `audit_log.pc_id` is a
foreign key into `pcs` and `audit_log` is append-only (no update/delete), a set
`pc_id` on any event referencing the row would permanently pin the row and make
the delete impossible. So the `pc_removed` event's FK `pc_id` is **null** and
the removed id, name, registered IP, and MAC go in the event metadata. For the
same reason the pre-existing `pc_pre_registered` and `pc_settings_updated`
lifecycle events also move their id into metadata and leave the FK `pc_id` null
— otherwise a PC an owner pre-registered by mistake could never be removed. Only
material events (coins, adjustments, wheel spins) keep the FK `pc_id` and rightly
block removal.

The DELETE endpoint (`DELETE /web-admin/pcs/{id}`) is owner-authed like its
sibling PC settings routes. The PCs tab gains a per-row destructive Remove button
guarded by a transactional confirmation dialog; the server is the sole authority
(no client-side pre-disabling), its refusal message surfaces through the Web
Admin's shared action-outcome toast, and success reloads the list.

## Consequences

The whole class of `database is locked` errors on settings saves is fixed at the
root, on every settings surface, not just the PCs tab — a save that contends with
the timer takes slightly longer instead of failing. A regression test proves an
immediate write transaction queues behind a held writer and succeeds rather than
erroring with code 5.

Owners can clean up mistaken PC rows while financial history is structurally
impossible to destroy: the clean-row checks refuse, and the foreign keys abort
the delete even if a check is ever missed. Audit filtering by `pc_id` no longer
returns the cosmetic lifecycle events (they carry the id in metadata instead),
which is acceptable — those are not material to a PC report.

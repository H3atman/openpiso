# ADR-0085: Admin refuses to start on an unknown or gapped migration history

- Status: Accepted
- Date: 2026-09-24
- Deciders: shop owner (product)
- Relates to: [ADR-0054](0054-backup-and-restore.md) (Pre-Migration Backups,
  the supported way back to an older Admin). Precursor to Attendance Rewards
  (ADR-0084), decided in "Define Attendance migration, compatibility, and
  recovery behavior".

## Context

The migration runner applied any version it knew and missed, and ignored
versions it did not know. An older Admin binary could therefore start against
a database already migrated by a newer Admin, reading and writing tables whose
meaning it does not understand. Features that add authority state (Attendance
first) cannot be rolled back safely under that behavior.

## Decision

Before the fresh-install marker, the Pre-Migration Backup, any migration, or
seeding, the runner reads the applied history and refuses to continue when:

- any positive version is unknown to, or newer than, this binary; or
- the known `1..N` history has a gap (a missing version below an applied one).

Version 0 keeps its meaning as the fresh-install marker. Fresh databases and
contiguous histories (including a run interrupted after some version) still
migrate normally. Refusal is a dedicated `UnsupportedMigrationHistory` error
that stops startup and names the offending version. The same runner guards
staged Restores.

Direct database downgrade is unsupported. To return to an older Admin, restore
its matching Pre-Migration Backup.

## Consequences

- An older Admin never runs against a newer live database; it fails closed.
- A mid-history gap is treated as corruption; recovery is a known-good Backup.
- Only binaries that carry this guard get this protection, so it must ship in
  a release before any release whose migrations it is meant to protect.

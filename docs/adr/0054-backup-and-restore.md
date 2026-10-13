# ADR-0054: Backup and Restore — pre-migration protection and filesystem catalog

- Status: Accepted
- Date: 2026-07-30
- Deciders: shop owner (product)
- Relates to: [ADR-0011](0011-admin-authority.md) (admin authority),
  [ADR-0015](0015-audit-log.md) (append-only Audit Log),
  [ADR-0024](0024-web-admin-sole-owner-surface.md) (sole owner surface),
  [ADR-0044](0044-immediate-write-transactions-and-pc-removal.md) (immediate
  write transactions), [ADR-0053](0053-shop-hours.md) (no-cash-refund-path
  reasoning)

## Context

SQLite is the source of truth for the shop's money, time, points, accounts, and
history. The load-bearing threat is a bad application update or migration: a
scheduled Backup taken after a bad migration would preserve the damage, while a
Backup taken immediately before it provides a rollback point. The owner also
needs a copy that remains readable and recoverable with stock `sqlite3`, even if
OpenPiso itself will not start.

The owner surface needs to distinguish deliberate copies, unattended copies,
and recovery copies. Restore must not partially replace a live database or
rewind paid value while a customer is using a PC. A database table cannot be
the source of truth for files that may outlive the database being restored.

## Decision

### The Pre-Migration Backup is the safety boundary

Before applying any pending migration, the Admin writes a **Backup** covering
every **Backup Set** and a byte-exact database companion. The companion is not a
second Backup: it is a direct rollback artifact for the Pre-Migration operation.
If either artifact cannot be written, migration is refused and the app does not
start. This is fail-closed and has no bypass:
a full disk, permission error, or other write failure must be fixed before the
update can proceed.

The **Pre-Migration Backup**, not the **Backup Schedule**, is the load-bearing
protection. The schedule is useful routine bookkeeping, but cannot protect
against the bad-update threat when its run happens after the damage.

### Canonical Backup artifact

A Backup is a readable, self-describing `.sql` dump shaped like `sqlite3
.dump`. Its header identifies the UTC creation time, Backup Kind, app and
schema versions, included Backup Sets, per-table row counts, checksum, and an
explicit warning that it contains secrets. The body contains schema DDL, data,
and migration history, with trigger DDL ordered after inserts so the dump can be
replayed by stock `sqlite3`.

The `.sql` file is the canonical Backup artifact and the only artifact listed
by the **Backup Catalog** or accepted as an owner-selected restore input. The
byte-exact database companion is not cataloged and does not have a Backup Kind;
it exists for direct last-resort rollback. Device secrets and the Web Admin PIN
hash are included verbatim when their **Backup Sets** are selected: a restorable
copy must be able to restore a working shop. This is deliberately different from
a shareable diagnostic export, which redacts secrets. A Backup with a restricted
Backup Set selection is useful as a readable export but is not valid restore
input.

### Backup Sets and kinds

A **Backup Set** is a named group of tables. The current **Backup Sets** are
Accounts, Configuration, Ledger, Vouchers, Terminals, Runtime State, and
Credentials. Table membership is explicit, while tables are discovered from
SQLite at dump time so a full Backup cannot silently omit a table added by a
future migration. Migration history is always included.

A **Backup Kind** is exactly one of `manual`, `scheduled`, `pre_migration`, or
`pre_restore`. An owner-triggered copy is `manual`; there is no separate
"save" concept. The Pre-Migration Backup always includes every **Backup Set**,
regardless of the schedule's selection.

**Backup Set** selection shapes the dump only. It never applies to restore:
restore requires coverage of every **Backup Set** and replaces the whole
database. Selective restore is out of scope.

### Schedule execution and catch-up

The Backup Schedule is owned by a separate Admin background task with a
30-second tick; it is never part of the one-second money/time timer. The task
persists `next_due_at`, writes at most one `scheduled` Backup when it is due,
and advances the instant past every missed occurrence. A due instant found on
startup is marked `catch_up` in the Backup header and Catalog, so downtime does
not flood the filesystem. Scheduled Backups are not appended to the Audit Log.

The persisted due time was added after the original schedule settings migration
so existing databases initialize it on their first scheduler tick. After each
successful write, retention is applied independently by Backup Kind: scheduled
files keep the configured count, Pre-Migration and Pre-Restore files keep the
newest 10 each, and manual files are never pruned automatically. The optional
`mirror_error` is a persistent owner-visible advisory; mirror failures never
invalidate the primary Backup.

### Retention

Retention is count-based rather than age-based, so downtime cannot empty the
catalog. Each Backup Kind has its own retention domain; scheduled files cannot
evict either recovery kind, and manual files are never removed automatically.

### Filesystem catalog and mirror

The **Backup Catalog** is derived by scanning the primary Backup directory and
parsing each file's header. It is not a SQLite table. Files are the source of
truth, so the catalog survives a database restore and does not require its own
migration or become stale when a file is removed outside the app.

The primary directory is inside the Admin app data directory. An optional
**Mirror Directory** receives a copy after the primary write succeeds. A failed
mirror copy never invalidates the primary Backup or the Pre-Migration safety
boundary; the owner receives a persistent advisory naming the filesystem error.

### Staged Restore and Pending Restore

A **Staged Restore** is a candidate database made by validating the header and
checksum, requiring all Backup Sets, replaying the SQL into a temporary file,
verifying every table's row count against the header while the replay is still an
exact reproduction of the file, then forward-migrating it to the running schema
and verifying foreign-key integrity. Row counts are checked before migrating
rather than after, because migrations legitimately insert and rebuild rows and an
older header cannot describe a migrated database. The live database is untouched
while this happens.

A **Pending Restore** is the durable marker for a verified **Staged Restore**.
It is consumed on the next launch: the outgoing live database is first dumped as
a canonical `.sql` Backup of Kind `pre_restore`, then the staged database
replaces it, and the marker is cleared. The owner can cancel a Pending Restore
before launch. Restore is refused while any PC is in use, has Remaining Time,
or has an **Active Session**;
there is no force-restore bypass because the shop has no cash refund path for
rewinding paid value.

### Owner surface and history

Backup and restore actions exist only on the authenticated web-admin owner
surface, never on the client/device API. Manual creation, deletion, schedule
changes, and restore transitions follow the **Audit Log** conventions in
ADR-0015; routine scheduled and Pre-Migration files are machine bookkeeping and
are not appended as noisy audit events. Restore writes its applied marker after
the swap so it belongs to the restored **Audit Log**.

## Considered options

### Rely on the Backup Schedule

Rejected. A scheduled copy that runs after a bad update captures the already
corrupted state. Only the fail-closed Pre-Migration Backup is guaranteed to
exist before migration.

### Store a Backup Catalog in SQLite

Rejected. The catalog would be reverted with the database, require schema
migration, and could be lost with a corrupt database. The filesystem is the
source of truth.

### Make a binary database file the canonical Backup

Rejected. A readable, self-describing SQL artifact works without OpenPiso and
with stock `sqlite3`. A byte-exact database companion remains valuable for the
Pre-Migration rollback path, but is not the owner-facing canonical artifact.

### Apply Backup Set selection during restore

Rejected. A partial database cannot safely replace a complete one. Backup Sets
are an export/dump boundary only; restore requires every Backup Set and is
whole-database.

### Permit a bypass for a failed Pre-Migration write

Rejected. Proceeding without a rollback point is the exact failure this ADR
exists to prevent. Availability yields to protection of the shop's books.

### Redact secrets from restorable Backups

Rejected. Redaction would make a restored shop incomplete and require re-pairing
or credential recovery. Secrets are included verbatim, and Credentials and
Terminals can be excluded when an owner deliberately creates a shareable export.

## Consequences

- The Pre-Migration Backup is a mandatory, fail-closed migration precondition;
  an update can stop the app until its write failure is fixed.
- `.sql` is readable and portable, while its checksum gate prevents accidental
  or silent hand edits from being treated as restore input.
- The Backup Catalog has no table or migration and reflects the files that
  actually exist.
- Backup Set selection produces useful partial exports but never selective
  restores; every restore is a verified whole-database replacement.
- Restores are deliberately staged and take effect only on the next launch,
  with a `pre_restore` recovery copy of the outgoing database.
- Backup files contain secrets when the relevant **Backup Sets** are present and
  must be protected as sensitive filesystem artifacts.
- The web admin remains the sole owner control surface, and the **Audit Log**
  keeps deliberate owner actions traceable without flooding it with routine files.

# ADR-0015: Unified append-only audit log

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0014 (reporting), and absorbs the "write to the audit log"
  requirements scattered through ADR-0004, 0009, 0011, 0012.

## Context

The owner wants to "always double-check" who logged in/out, inserted coins,
etc. The MVP has an `audit_logs` table, and several ADRs already say "audit
this," but there was no comprehensive, reviewable design. The audit trail also
overlaps heavily with the ADR-0014 reporting ledger (a coin insert is both an
audit event and a financial entry).

## Decision

### One stream, typed columns
- A **single append-only event log** (`audit_log`, absorbing the MVP
  `audit_logs`) is the **single source of truth** for who-did-what **and** for
  financial reporting. Each value-moving event is written **once**; the ADR-0014
  reports are a **typed view** over this table (`WHERE event_type IN (...)`),
  not a parallel table.
- Columns are **strongly typed**, not a JSON blob: `event_type`, `actor`,
  nullable `account_id` / `pc_id`, nullable `amount_cents` / `seconds_delta` /
  `points_delta`, `cash_flag`, `created_at`, a human-readable `summary`, plus
  `metadata_json` for event-specific extras.
- Rationale: two parallel tables drift; an audit trail that disagrees with the
  sales report is worse than useless.

### Actor & honest session-end reasons
- Every row records its **actor**: a customer **account**, the **admin**, the
  **coin device**, or **system** (timer/auto-kick/takeover/expiry).
- Session ends are **distinct event types**, never a generic "logout":
  `logout` (account), `auto_kick` (ADR-0002, system), `taken_over` (ADR-0007,
  system), `expired` (ADR-0005, system). A log that says "logout" when the
  customer was actually kicked is a log that lies.

### Failures and denials are logged
- `login_failed` (bad password vs unknown user), `voucher_rejected` (invalid /
  already-spent), `redeem_denied` / `spin_denied` (insufficient points). These
  are the highest-value rows — fraud and code-guessing show up here first.

### Append-only, no app-side mutation
- The app exposes **no edit or delete** for audit rows. Corrections are made by
  writing a new **compensating event**, never by altering history.
- **No cryptographic hash-chaining now:** per ADR-0011 the owner is the only
  admin, so there is no insider to defend against. Revisit tamper-evidence
  **if/when staff accounts are added** (then insiders exist). This cannot stop
  direct edits to the SQLite file — it makes the *app* honest, not the disk.

### Retention & review
- **Never auto-purged** (it is tiny and its value grows with age). Provide
  **CSV export** for archiving/accounting.
- **Review screen:** reverse-chronological feed of plain-language summaries;
  filters for date range, PC, account, event type (Auth/Money/Time/Points/
  Admin/System), and actor; free-text search; a "show developer logs" toggle
  (ADR-0016); CSV export. Filter dimensions deliberately match ADR-0014.

## Consequences / still open

- The MVP `audit_logs` schema is replaced/extended by this typed `audit_log`;
  a migration is required.
- Diagnostic logging is a separate store — see ADR-0016.

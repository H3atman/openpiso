# ADR-0057: Account Import from Legacy Systems

- Status: Accepted
- Date: 2026-07-31
- Refines: ADR-0004, ADR-0010, ADR-0015, ADR-0017

## Decision

OpenPiso supports an owner-authenticated **Account Import** from the Martec
`AccountsData.json` shape. The browser reads the selected file and posts its
parsed JSON; the admin server never opens a caller-supplied path.

`dry_run: true` plans and reports the import without writes. `dry_run: false`
uses the same planning code and commits all valid rows, skipped rows included
in the report, in one `BEGIN IMMEDIATE` SQLite transaction. An unexpected
failure rolls the transaction back. An existing normalized Username is never
overwritten: an Account imported from Martec is reported as
`already_imported`, while any other existing Account is reported as an
`existing_account_conflict`.

Imported Accounts store `imported_from = 'martec'` and the import timestamp.
The Martec account id and last-seen value are audit provenance only. The
OpenPiso `created_at` remains the import date. The bcrypt hash is stored
without re-hashing; the existing transitional login path verifies it and
upgrades it to Argon2 after a successful login. New, changed, or reset
passwords remain Argon2. This bcrypt branch is transitional but has **no
scheduled removal**: it costs nothing once no bcrypt rows remain, while
removing it early would lock out customers who have not yet returned to log in.

Each imported Account has one non-cash `account_created` audit event carrying
its imported time and Points deltas. The run also has one
`account_import_completed` summary event with counts and totals. Both events
set `cash_flag = false` because imported balances are inherited liability, not
cash that entered the shop; counting them as revenue would fabricate sales in
reports.

The owner Accounts page confirms by posting the identical parsed payload with
only `dry_run` changed to `false`. Imported rows retain the existing per-Account
actions. The Accounts list shows a permanent Legacy badge; its Not yet logged
in badge is derived from the imported bcrypt credential and disappears after a
successful login, Password Change, or Password Reset. The owner-facing label is
Legacy; `imported_from` remains a neutral source-system value so future import
sources do not leak into the UI vocabulary.

Login uses a lenient Username normalizer: it trims input and converts each run
of whitespace to `_` before the existing case-insensitive lookup. This lets a
customer type a legacy Username such as `Juan Smith` as `Juan_Smith` without
changing the stored identity. Signup, owner account creation, and transfer
recipient lookup remain strict, so no stored Username contains whitespace.

## Consequences

- `accounts.imported_from` and `accounts.imported_at` are nullable, so existing
  Accounts are untouched by the migration and no index is needed.
- Martec `credits` are minutes converted to rounded integer seconds, and
  `points` are converted to rounded integer centipoints. Negative values are
  floored at zero; floating-point dust becomes zero.
- Disabled, invalid, renamed, duplicate, and conflicting rows remain visible
  in the report with machine-readable reasons.
- A future source can reuse the neutral provenance columns, but this ADR does
  not define a generic import framework.

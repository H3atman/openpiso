# ADR-0037: Contention-safe Password Change

**Status:** Accepted

## Context

A customer Password Change previously opened a SQLite read transaction before
Argon2 verification and hashing. In WAL mode, an unrelated commit could then
make that read snapshot impossible to promote to a writer
(`SQLITE_BUSY_SNAPSHOT`, extended code 517). Concurrent Password Changes could
also overwrite each other.

Password Change is distinct from owner Password Reset. It must preserve the
Account Session, Active Session, Remaining Time, and Points.

## Decision

Password Change uses three phases:

1. Read the active Account's exact password hash without opening an explicit
   transaction.
2. Synchronously verify the current password and compute the new Argon2 hash,
   holding no SQLite connection or transaction.
3. In a short write transaction, replace the credential only when the Account
   is still active and its hash exactly equals the verified hash. Append one
   `account_password_changed` event in the same transaction and commit.

The conditional update gives concurrent valid changes first-writer-wins
semantics. A zero-row update means the Account became inactive/removed or the
credential became stale. It does not modify or reactivate the Account. An
inactive or removed Account returns the same generic HTTP 401 used for bad
credentials; only a superseded credential returns HTTP 409 with `password
change conflict`. Both outcomes best-effort append an
`account_password_change_failed` event after the failed transaction is closed.

Wrong-current-password behavior remains HTTP 401 with the existing generic
message and best-effort denial audit. A missing Account follows those same
semantics and writes a best-effort denial audit without a dangling Account
foreign key. Validation and same-password behavior are
unchanged. Password Change never revokes or replaces session state.

Unexpected Password Change failures return HTTP 500 with `password change
failed`; database details are written only to the Diagnostic Log.

## Consequences

Argon2 work cannot reserve SQLite's writer. Routine timer-equivalent writes on
other runtime workers can commit while password computation runs. The success
audit and credential replacement remain atomic. A stale-failure audit is
best-effort because it occurs after the losing transaction and must not mask the
customer-safe conflict response.

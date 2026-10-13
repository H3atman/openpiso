# ADR-0011: Admin authority — owner password & soft account deletion

- Status: Accepted (owner-login portion superseded by ADR-0024)
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0004 (auth), ADR-0010 (account creation).

> **Superseded in part by [ADR-0024](0024-web-admin-sole-owner-surface.md):**
> the single owner *password* below is replaced by a simple **PIN** once the
> desktop console is retired and `:8080/admin` becomes the sole owner surface.
> The soft-delete decision and the no-staff-roles / single-operator audit
> limitation are **unchanged** and remain authoritative.

## Context

the original expansion plan adds "Admin Login" and "Delete Account." The MVP had no admin auth
and explicitly no staff roles. Need to scope admin authority and define safe
deletion given financial history.

## Decision

### Admin login
- A **single owner password** gates the dashboard/terminal — a lock on the
  door to keep customers out of admin functions.
- **No staff accounts, no roles** (deferred). Consequence: the audit log cannot
  attribute an action to an individual staff member; it records *that* an admin
  action happened, not *which* person. Acceptable while the shop is
  single-operator. Revisit if staff are hired (multi-user audit is the reason
  to revisit).

### Account deletion (soft delete)
- Deleting a customer account is a **soft delete**: the account is marked
  deleted/disabled and can no longer log in, but **all historical rows**
  (`coin_transactions`, `manual_adjustments`, `audit_logs`, redeemed vouchers)
  are **preserved** — they are the shop's financial records.
- On deletion: the account's remaining **time and points are forfeited**, and
  if it currently has an **active session, that session ends immediately**.
- The deletion itself is written to the audit log.
- Recommended: **retire the username** (do not allow reuse) so old history
  can't be confused with a new person taking the same name.

## Consequences

- `accounts` needs a soft-delete marker (e.g. `status` / `deleted_at`), and all
  account lookups for login must exclude deleted accounts.
- Foreign keys from history tables to `accounts` must not cascade-delete.

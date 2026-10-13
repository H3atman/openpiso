# ADR-0018: Canonical audit log with linked financial details

- Status: Accepted
- Date: 2026-06-21
- Deciders: implementation team
- Refines: ADR-0015

## Context

The MVP reads `coin_transactions` directly for the dashboard and reports.
M1 must make `audit_log` canonical without duplicating historical financial
events or prematurely rebuilding the M3/M6 query surfaces.

## Decision

- `audit_log` is the canonical event and reporting stream.
- `coin_transactions` and `manual_adjustments` remain compatibility/detail
  tables with a required one-to-one `audit_event_id` link.
- New value-moving writes insert the canonical event and its detail row in the
  same database transaction.
- Migration backfills exactly one canonical event per legacy financial row.
- Matching MVP `audit_logs` rows are absorbed rather than copied a second time.
- Other legacy audit rows are retained as typed `legacy_event` rows with their
  original fields in metadata.

## Consequences

Existing dashboard queries remain stable. M3 and M6 may add views over
`audit_log`, but must not create a second financial event for a linked detail
row.

-- Migration 0009: account-to-account transfers (ADR-0030).
--
-- Time transfers are **immediate**, atomic, and server-authoritative; the
-- detail row below mirrors the canonical `audit_log` `account_transfer_out`
-- / `account_transfer_in` events the Rust path writes, the same way
-- `manual_adjustments` links to its `manual_adjustment` event.
--
-- Points transfers are **owner-confirmed**. The customer creates a pending
-- request with a single-use, time-boxed 6-digit code (default TTL ~10 min);
-- the owner enters that code in web-admin to confirm. The request row is
-- the durable state — an admin restart does not drop pending requests, and
-- the audit trail records the full lifecycle.

PRAGMA foreign_keys = ON;

CREATE TABLE account_transfers (
  id TEXT PRIMARY KEY,
  from_account_id TEXT NOT NULL,
  to_account_id TEXT NOT NULL,
  seconds INTEGER NOT NULL CHECK (seconds > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  FOREIGN KEY (from_account_id) REFERENCES accounts(id),
  FOREIGN KEY (to_account_id) REFERENCES accounts(id)
);

CREATE INDEX idx_account_transfers_from
  ON account_transfers(from_account_id, created_at);
CREATE INDEX idx_account_transfers_to
  ON account_transfers(to_account_id, created_at);
CREATE UNIQUE INDEX idx_account_transfers_audit_event_id
  ON account_transfers(audit_event_id);

-- The `audit_event_id` column is `NOT NULL REFERENCES audit_log(id)`, so the
-- FK plus NOT NULL already guarantee every detail row links to a canonical
-- audit event; no extra trigger is needed.

CREATE TABLE points_transfer_requests (
  id TEXT PRIMARY KEY,
  from_account_id TEXT NOT NULL,
  to_account_id TEXT NOT NULL,
  points INTEGER NOT NULL CHECK (points > 0),
  code TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'confirmed', 'rejected', 'expired')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  resolved_at TEXT,
  audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  FOREIGN KEY (from_account_id) REFERENCES accounts(id),
  FOREIGN KEY (to_account_id) REFERENCES accounts(id)
);

CREATE INDEX idx_points_transfer_requests_from
  ON points_transfer_requests(from_account_id, created_at);
CREATE INDEX idx_points_transfer_requests_to
  ON points_transfer_requests(to_account_id, created_at);
CREATE INDEX idx_points_transfer_requests_code_status
  ON points_transfer_requests(code, status);
-- A code is single-use only while the request is live. Scoping uniqueness to
-- `pending` rows lets resolved (confirmed/rejected/expired) codes recur
-- without an unbounded global collision space, while still guaranteeing no two
-- open requests share a code (the confirm/reject lookup is by code).
CREATE UNIQUE INDEX idx_points_transfer_requests_pending_code
  ON points_transfer_requests(code) WHERE status = 'pending';
CREATE UNIQUE INDEX idx_points_transfer_requests_audit_event_id
  ON points_transfer_requests(audit_event_id);

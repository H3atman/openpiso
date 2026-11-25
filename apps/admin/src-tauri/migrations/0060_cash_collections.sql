-- Cash Collection (ADR-0091): the owner records emptying the coin box or
-- taking the counter's voucher cash. Custody only; it never changes a sales or
-- revenue figure. The amount is always computed by the server inside the same
-- transaction that inserts the row, so a row is the single record of that cash.
CREATE TABLE cash_collections (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('coin_box', 'voucher_cash')),
  amount_cents INTEGER NOT NULL CHECK (amount_cents > 0),
  collected_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Serves Uncollected (SUM per kind) and the last-collection lookup.
CREATE INDEX idx_cash_collections_kind
  ON cash_collections (kind, collected_at);

-- Append-only like audit_log: there is no undo.
CREATE TRIGGER cash_collections_no_update
BEFORE UPDATE ON cash_collections
BEGIN
  SELECT RAISE(ABORT, 'cash_collections is append-only');
END;

CREATE TRIGGER cash_collections_no_delete
BEFORE DELETE ON cash_collections
BEGIN
  SELECT RAISE(ABORT, 'cash_collections is append-only');
END;

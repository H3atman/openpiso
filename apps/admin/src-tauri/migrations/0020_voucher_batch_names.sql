PRAGMA foreign_keys = ON;

ALTER TABLE voucher_batches ADD COLUMN name TEXT;

-- Deterministically assign every existing batch a unique five-character Base62
-- name. New batches use server-side random Base62 generation.
WITH numbered AS (
  SELECT id, row_number() OVER (ORDER BY created_at, id) - 1 AS n
  FROM voucher_batches
), names AS (
  SELECT
    id,
    substr('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789', ((n / 14776336) % 62) + 1, 1) ||
    substr('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789', ((n / 238328) % 62) + 1, 1) ||
    substr('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789', ((n / 3844) % 62) + 1, 1) ||
    substr('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789', ((n / 62) % 62) + 1, 1) ||
    substr('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789', (n % 62) + 1, 1) AS name
  FROM numbered
)
UPDATE voucher_batches
SET name = (SELECT names.name FROM names WHERE names.id = voucher_batches.id);

CREATE UNIQUE INDEX idx_voucher_batches_name
  ON voucher_batches(name COLLATE BINARY);

CREATE TRIGGER voucher_batches_name_insert_guard
BEFORE INSERT ON voucher_batches
WHEN NEW.name IS NULL
  OR length(NEW.name) != 5
  OR NEW.name GLOB '*[^A-Za-z0-9]*'
BEGIN
  SELECT RAISE(ABORT, 'voucher batch name must be five Base62 characters');
END;

CREATE TRIGGER voucher_batches_name_immutable
BEFORE UPDATE OF name ON voucher_batches
WHEN NEW.name IS NOT OLD.name
BEGIN
  SELECT RAISE(ABORT, 'voucher batch name is immutable');
END;

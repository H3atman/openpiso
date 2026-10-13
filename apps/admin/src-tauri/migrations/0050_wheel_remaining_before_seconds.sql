PRAGMA foreign_keys = ON;

-- Remaining Time on the account immediately before the spin resolved
-- (ADR-0012 server-authoritative spin; folklore-fairness evidence).
--
-- Nullable like bokya_rate_basis_points (migration 0028): null marks rows
-- written before this migration. Treating unknown history as zero time would
-- corrupt a pre-spin-balance breakdown, so no backfill is attempted.
ALTER TABLE wheel_spins
  ADD COLUMN remaining_before_seconds INTEGER;

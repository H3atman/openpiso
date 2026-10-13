PRAGMA foreign_keys = ON;

ALTER TABLE wheel_settings
  ADD COLUMN bokya_rate_basis_points INTEGER
  CHECK (bokya_rate_basis_points BETWEEN 0 AND 10000);

ALTER TABLE wheel_spins
  ADD COLUMN bokya_rate_basis_points INTEGER
  CHECK (bokya_rate_basis_points BETWEEN 0 AND 10000);

-- Store the latest measured Admin-minus-Client wall-clock difference.
ALTER TABLE pcs ADD COLUMN clock_difference_ms INTEGER;

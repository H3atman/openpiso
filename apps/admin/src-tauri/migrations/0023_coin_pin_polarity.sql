-- Owner-configurable Coin Pin wiring polarity.
-- normally_open preserves the original low -> high pulse behavior.
ALTER TABLE coin_device_settings
  ADD COLUMN coin_pin_polarity TEXT NOT NULL DEFAULT 'normally_open'
  CHECK (coin_pin_polarity IN ('normally_open', 'normally_closed'));

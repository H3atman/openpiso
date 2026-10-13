-- M4 (ADR-0023 / ADR-0024): owner-editable coin-device settings.
--
-- The coin acceptor is wired directly to the server PC's COM port. These
-- settings are calibrated on-site and read by the coin pipeline (M1/M2):
--   * com_port        which serial port the acceptor is wired to. NULL means
--                     "no hardware configured" -> the simulator source is used.
--   * baud_rate       serial baud rate for the port.
--   * set_pin_line    the control line the server DRIVES to enable/inhibit the
--                     acceptor (the "set pin"). Owner-editable because boards
--                     differ (RTS vs DTR).
--   * coin_pin_line   the status line the server READS for pulse edges (the
--                     "coin pin"): CTS / DSR / RI / CD.
--   * inter_coin_gap_ms  the quiet gap that closes a Coin Batch.
--
-- Money stays integer cents and the pulse->cents map is the only place a
-- pulse count is turned into value (ADR-0023). A single-row table keeps the
-- settings authoritative and easy to read.
CREATE TABLE coin_device_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  com_port TEXT,
  baud_rate INTEGER NOT NULL DEFAULT 9600 CHECK (baud_rate > 0),
  set_pin_line TEXT NOT NULL DEFAULT 'rts'
    CHECK (set_pin_line IN ('rts', 'dtr')),
  coin_pin_line TEXT NOT NULL DEFAULT 'cd'
    CHECK (coin_pin_line IN ('cts', 'dsr', 'ri', 'cd')),
  inter_coin_gap_ms INTEGER NOT NULL DEFAULT 200
    CHECK (inter_coin_gap_ms >= 0),
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Defaults mirror the M1 constants (200 ms gap; ₱1/₱5/₱10 pulse map) so a
-- fresh install behaves identically to the hard-coded M1/M2 wiring until the
-- owner calibrates on-site.
INSERT INTO coin_device_settings (id) VALUES (1);

CREATE TABLE coin_device_pulse_map (
  pulse_count INTEGER PRIMARY KEY CHECK (pulse_count > 0),
  amount_cents INTEGER NOT NULL CHECK (amount_cents > 0)
);

INSERT INTO coin_device_pulse_map (pulse_count, amount_cents) VALUES
  (1, 100),
  (5, 500),
  (10, 1000);

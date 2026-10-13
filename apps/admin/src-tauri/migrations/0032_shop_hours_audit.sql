CREATE TABLE shop_hours_runtime_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  observed_open INTEGER CHECK (observed_open IS NULL OR observed_open IN (0, 1)),
  observed_reason TEXT CHECK (
    observed_reason IS NULL OR observed_reason IN ('schedule', 'override')
  )
);

INSERT INTO shop_hours_runtime_state (id) VALUES (1);

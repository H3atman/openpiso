-- Spin Password Confirmation: when enabled, POST /api/wheel/spin requires the
-- signed-in Account's password. Off by default for new and existing shops.
ALTER TABLE wheel_settings ADD COLUMN password_confirmation_required INTEGER NOT NULL DEFAULT 0
  CHECK (password_confirmation_required IN (0, 1));

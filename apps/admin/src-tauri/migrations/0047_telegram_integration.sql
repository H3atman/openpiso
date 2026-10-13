-- Optional Telegram integration (ADR-0076 / GitHub #345): one private owner
-- chat that receives outbound PC Monitoring notifications. The whole
-- integration is deliberately excluded from Backups (ADR-0076), so this table
-- is listed in `backup::BACKUP_EXCLUDED_TABLES` and never exported: a restored
-- Admin starts disconnected until the owner completes setup again.
--
-- Single-row by construction (`id = 1`): Telegram v1 supports exactly one bot
-- and one destination chat. Disconnecting deletes the row, which destroys the
-- protected token and the destination together.
--
-- `token_protected` holds the BotFather token encrypted with Windows DPAPI
-- under the Admin Windows identity (hex-encoded ciphertext). It is never
-- returned by any API, Audit event, diagnostic, or error; only the Admin Rust
-- HTTPS client decrypts it in memory to deliver messages.
CREATE TABLE telegram_config (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  bot_username TEXT NOT NULL,
  chat_id TEXT NOT NULL,
  chat_title TEXT NOT NULL,
  token_protected TEXT NOT NULL,
  connected_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  -- When the stored configuration last passed a real test send. A failed
  -- replacement attempt never touches this row: the working configuration
  -- stays untouched until a new one passes its test.
  verified_at TEXT NOT NULL,
  open_audit_id TEXT NOT NULL REFERENCES audit_log(id)
);

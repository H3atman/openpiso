PRAGMA foreign_keys = ON;

CREATE TABLE account_session_history (
  session_id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL,
  pc_id TEXT NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  end_reason TEXT CHECK (
    end_reason IS NULL OR end_reason IN (
      'logout', 'auto_kick', 'taken_over', 'expired', 'tamper_kick',
      'server_restart'
    )
  ),
  FOREIGN KEY (account_id) REFERENCES accounts(id),
  FOREIGN KEY (pc_id) REFERENCES pcs(id)
);

CREATE INDEX idx_account_session_history_pc_started
  ON account_session_history(pc_id, started_at);
CREATE INDEX idx_account_session_history_account_started
  ON account_session_history(account_id, started_at);
CREATE INDEX idx_account_session_history_open
  ON account_session_history(pc_id, ended_at)
  WHERE ended_at IS NULL;

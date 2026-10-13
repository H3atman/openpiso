use openpiso_core::session::SessionEndReason;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::{errors::AppResult, models::ActiveSession};

/// Persist the beginning of a new Account Session in the caller's mutation
/// transaction. A same-PC re-login resumes an existing session and must not
/// call this function.
pub async fn start(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &ActiveSession,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO account_session_history
           (session_id, account_id, pc_id, started_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&session.id)
    .bind(&session.account_id)
    .bind(&session.pc_id)
    .bind(&session.started_at)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Close an Account Session history row in the caller's mutation transaction.
/// The guarded update keeps an already-closed row untouched and makes a
/// missing history row fail the enclosing mutation rather than letting the
/// Audit Log and session history drift apart.
pub async fn end(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: &str,
    reason: SessionEndReason,
) -> AppResult<()> {
    let result = sqlx::query(
        "UPDATE account_session_history
         SET ended_at = CURRENT_TIMESTAMP, end_reason = ?
         WHERE session_id = ? AND ended_at IS NULL",
    )
    .bind(reason.event_type())
    .bind(session_id)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() != 1 {
        return Err(crate::errors::AppError::Conflict(format!(
            "Account Session history row '{session_id}' is not open"
        )));
    }
    Ok(())
}

/// Close rows left open by the previous Admin process. This is deliberately
/// not an audit-producing operation: ADR-0013 already accounts for the
/// restart logout, while the history row needs a finite interval for later
/// reporting and attribution.
pub async fn recover_open(pool: &SqlitePool) -> AppResult<()> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    sqlx::query(
        "UPDATE account_session_history
         SET ended_at = COALESCE(
               (SELECT last_seen_at FROM pcs WHERE pcs.id = account_session_history.pc_id),
               CURRENT_TIMESTAMP
             ),
             end_reason = 'server_restart'
         WHERE ended_at IS NULL",
    )
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

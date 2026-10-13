use serde_json::json;
use sqlx::{SqlitePool, Transaction};

use crate::{
    audit::{self, NewAuditEvent},
    errors::AppResult,
};

/// Read the shop-wide Diagnostics Mode flag (ADR-0081). It is visibility
/// only: nothing in Admin or the Client captures, reports, or retains
/// differently because of it, and it is not a security boundary.
pub async fn enabled(pool: &SqlitePool) -> AppResult<bool> {
    let (enabled,): (i64,) =
        sqlx::query_as("SELECT enabled FROM diagnostics_mode_settings WHERE id = 1")
            .fetch_one(pool)
            .await?;
    Ok(enabled != 0)
}

/// Flip the shop-wide Diagnostics Mode flag and audit the change in one
/// immediate transaction (ADR-0081). Returns the stored value, so callers
/// echo machine state rather than the request's claim.
pub async fn set_enabled(pool: &SqlitePool, next: bool) -> AppResult<bool> {
    let mut transaction = super::begin_immediate(pool).await?;
    set_enabled_in_transaction(&mut transaction, next).await?;
    transaction.commit().await?;
    enabled(pool).await
}

async fn set_enabled_in_transaction(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    next: bool,
) -> AppResult<()> {
    sqlx::query("UPDATE diagnostics_mode_settings SET enabled = ? WHERE id = 1")
        .bind(next)
        .execute(&mut **transaction)
        .await?;

    let mut event = NewAuditEvent::simple(
        "diagnostics_mode_updated",
        openpiso_core::audit::AuditOutcome::Success,
        openpiso_core::audit::AuditActor::Admin,
        if next {
            "Enabled Diagnostics Mode"
        } else {
            "Disabled Diagnostics Mode"
        },
    );
    event.metadata = json!({ "enabled": next });
    audit::append(transaction, event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mem_test_pool;

    #[tokio::test]
    async fn defaults_off_and_round_trips_with_one_audit_event_per_change() -> AppResult<()> {
        let pool = mem_test_pool().await;
        assert!(!enabled(&pool).await?);

        assert!(set_enabled(&pool, true).await?);
        assert!(enabled(&pool).await?);
        assert!(!set_enabled(&pool, false).await?);
        assert!(!enabled(&pool).await?);

        let states: Vec<(i64,)> = sqlx::query_as(
            "SELECT json_extract(metadata_json, '$.enabled') FROM audit_log
             WHERE event_type = 'diagnostics_mode_updated'",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(states, vec![(1,), (0,)]);
        Ok(())
    }
}

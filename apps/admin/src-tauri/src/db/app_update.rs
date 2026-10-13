//! App Update's database facts, shop preference and restart marker (ADR-0086).

use openpiso_core::audit::{AuditActor, AuditOutcome};
use sqlx::SqlitePool;

use crate::{
    audit::{self, NewAuditEvent},
    errors::AppResult,
};

/// SQLite persists the marker beside the existing App Update preference.
pub async fn restart_marker(
    pool: &SqlitePool,
) -> AppResult<Option<crate::app_update::RestartMarker>> {
    super::settings::get_setting(pool, "app_update_restart_marker")
        .await?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                crate::errors::AppError::Internal(format!(
                    "invalid App Update restart marker: {error}"
                ))
            })
        })
        .transpose()
}

/// Persist the restart marker and request audit before handing off to NSIS.
pub async fn begin_install(
    pool: &SqlitePool,
    marker: &crate::app_update::RestartMarker,
    rollback: bool,
) -> AppResult<()> {
    let value = serde_json::to_string(marker).map_err(|error| {
        crate::errors::AppError::Internal(format!(
            "could not encode App Update restart marker: {error}"
        ))
    })?;
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('app_update_restart_marker', ?, CURRENT_TIMESTAMP)")
        .bind(value).execute(&mut *transaction).await?;
    let mut event = NewAuditEvent::simple(
        if rollback {
            "app_update_rollback"
        } else {
            "app_update_install"
        },
        AuditOutcome::Success,
        AuditActor::Admin,
        "App Update installer prepared; restart pending",
    );
    event.metadata = serde_json::json!({
        "fromVersion": marker.from_version,
        "version": marker.target_version,
        "startedAt": marker.started_at,
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn last_result(pool: &SqlitePool) -> AppResult<Option<crate::app_update::UpdateResult>> {
    super::settings::get_setting(pool, "app_update_last_result")
        .await?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                crate::errors::AppError::Internal(format!("invalid App Update result: {error}"))
            })
        })
        .transpose()
}

/// Resolve an outstanding marker once. The result survives later restarts until ack.
pub async fn resolve_restart(
    pool: &SqlitePool,
) -> AppResult<Option<crate::app_update::UpdateResult>> {
    let mut transaction = super::begin_immediate(pool).await?;
    let previous: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'app_update_last_result'")
            .fetch_optional(&mut *transaction)
            .await?;
    if previous.is_some() {
        transaction.commit().await?;
        return last_result(pool).await;
    }
    let marker: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'app_update_restart_marker'")
            .fetch_optional(&mut *transaction)
            .await?;
    let Some(marker) = marker else {
        transaction.commit().await?;
        return Ok(None);
    };
    let marker: crate::app_update::RestartMarker =
        serde_json::from_str(&marker).map_err(|error| {
            crate::errors::AppError::Internal(format!("invalid App Update restart marker: {error}"))
        })?;
    let result = crate::app_update::UpdateResult {
        from: marker.from_version,
        ok: marker.target_version == env!("CARGO_PKG_VERSION"),
        to: marker.target_version,
    };
    let value = serde_json::to_string(&result).map_err(|error| {
        crate::errors::AppError::Internal(format!("could not encode App Update result: {error}"))
    })?;
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('app_update_last_result', ?, CURRENT_TIMESTAMP)")
        .bind(value).execute(&mut *transaction).await?;
    let mut event = NewAuditEvent::simple(
        "app_update_result",
        if result.ok {
            AuditOutcome::Success
        } else {
            AuditOutcome::Failure
        },
        AuditActor::System,
        "App Update restart result",
    );
    event.metadata = serde_json::json!({ "result": result, "startedAt": marker.started_at, "runningVersion": env!("CARGO_PKG_VERSION") });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(Some(result))
}

pub async fn acknowledge_result(pool: &SqlitePool) -> AppResult<()> {
    let mut transaction = super::begin_immediate(pool).await?;
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key = 'app_update_restart_marker'")
            .fetch_one(&mut *transaction)
            .await?;
    let resolved: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM settings WHERE key = 'app_update_last_result'")
            .fetch_one(&mut *transaction)
            .await?;
    if pending > 0 && resolved == 0 {
        return Err(crate::errors::AppError::Conflict(
            "App Update still awaiting restart".to_owned(),
        ));
    }
    sqlx::query(
        "DELETE FROM settings WHERE key IN ('app_update_restart_marker', 'app_update_last_result')",
    )
    .execute(&mut *transaction)
    .await?;
    audit::append(
        &mut transaction,
        NewAuditEvent::simple(
            "app_update_result_acknowledged",
            AuditOutcome::Success,
            AuditActor::Admin,
            "App Update result acknowledged",
        ),
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn schema_version(pool: &SqlitePool) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _openpiso_migrations")
            .fetch_one(pool)
            .await?,
    )
}

pub async fn auto_check(pool: &SqlitePool) -> AppResult<bool> {
    Ok(super::settings::get_setting(pool, "app_update_auto_check")
        .await?
        .is_none_or(|value| value == "true"))
}

/// Commit the preference and its audit event together, in existing shop settings.
pub async fn set_auto_check(pool: &SqlitePool, enabled: bool) -> AppResult<()> {
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES ('app_update_auto_check', ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(if enabled { "true" } else { "false" })
    .execute(&mut *transaction)
    .await?;
    let mut event = NewAuditEvent::simple(
        "app_update_auto_check_changed",
        AuditOutcome::Success,
        AuditActor::Admin,
        "App Update automatic checks changed",
    );
    event.metadata = serde_json::json!({ "enabled": enabled });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(())
}

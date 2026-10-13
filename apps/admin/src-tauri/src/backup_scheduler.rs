use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local};
use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::SqlitePool;
use tracing::{error, warn};

use crate::audit::{self, NewAuditEvent};

use crate::{
    backup::{self, BackupSet},
    db,
    errors::{AppError, AppResult},
};

const TICK: Duration = Duration::from_secs(30);

/// Run the unattended Backup Schedule independently from the one-second money
/// and time timer. Backup file I/O must never delay balance writes.
pub async fn run(pool: SqlitePool, backup_directory: PathBuf) {
    let mut interval = tokio::time::interval(TICK);
    let mut starting = true;
    loop {
        interval.tick().await;
        if let Err(error) = run_once(
            &pool,
            &backup_directory,
            Local::now().fixed_offset(),
            starting,
        )
        .await
        {
            error!(
                target: "openpiso_admin_lib::backup_scheduler",
                "scheduled Backup tick failed: {error}"
            );
        }
        starting = false;
    }
}

async fn run_once(
    pool: &SqlitePool,
    backup_directory: &Path,
    now: DateTime<FixedOffset>,
    starting: bool,
) -> AppResult<bool> {
    let settings = db::backup_schedule_settings(pool).await?;
    if !settings.enabled {
        return Ok(false);
    }

    let schedule = db::backup_schedule::core_schedule_from_settings(&settings)?;
    let stored_due = db::backup_schedule::stored_next_due_at(pool).await?;
    let stored_due = match stored_due {
        Some(value) => value,
        None => {
            #[cfg(debug_assertions)]
            let initial_due = if settings.frequency
                == openpiso_core::backup_schedule::BackupFrequency::Interval
            {
                openpiso_core::backup_schedule::next_due(schedule, now)
            } else {
                openpiso_core::backup_schedule::latest_due(schedule, now)
            };
            #[cfg(not(debug_assertions))]
            let initial_due = openpiso_core::backup_schedule::latest_due(schedule, now);
            let initial_due = initial_due.ok_or_else(|| {
                AppError::InvalidInput("enabled Backup Schedule has no due time".to_owned())
            })?;
            let value = initial_due.to_rfc3339();
            db::backup_schedule::set_next_due_at(pool, Some(&value)).await?;
            value
        }
    };

    let due = DateTime::parse_from_rfc3339(&stored_due).map_err(|error| {
        AppError::InvalidInput(format!(
            "stored Backup Schedule due time is invalid: {error}"
        ))
    })?;
    if due > now {
        return Ok(false);
    }

    let backup_sets = match settings
        .backup_sets
        .iter()
        .map(|name| {
            BackupSet::parse_name(name).ok_or_else(|| {
                AppError::InvalidInput(format!("unknown Backup Set in schedule: {name}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(backup_sets) => backup_sets,
        Err(error) => {
            append_schedule_failure(pool, &stored_due, &error).await;
            return Err(error);
        }
    };
    // Only a due instant found on scheduler startup represents downtime. A
    // normal poll is expected to arrive slightly after its due instant.
    let catch_up = starting && due < now;
    let artifact = match backup::write_scheduled_backup(
        pool,
        backup_directory,
        &backup_sets,
        catch_up,
    )
    .await
    {
        Ok(artifact) => artifact,
        Err(error) => {
            append_schedule_failure(pool, &stored_due, &error).await;
            return Err(error);
        }
    };

    let next_due =
        match openpiso_core::backup_schedule::collapse_missed_occurrences(schedule, due, now) {
            Some(next_due) => next_due,
            None => {
                let error = AppError::InvalidInput(
                    "enabled Backup Schedule has no next occurrence".to_owned(),
                );
                append_schedule_failure(pool, &stored_due, &error).await;
                return Err(error);
            }
        };
    // The advance and its catch-up Notice commit together: a crash between the
    // two must not leave the missed run silently unreported.
    let advanced = match advance_and_note_catch_up(
        pool,
        &stored_due,
        &next_due.to_rfc3339(),
        catch_up.then(|| catch_up_event(&stored_due, artifact.path.as_path())),
    )
    .await
    {
        Ok(advanced) => advanced,
        Err(error) => {
            append_schedule_failure(pool, &stored_due, &error).await;
            return Err(error);
        }
    };
    if !advanced {
        warn!(
            target: "openpiso_admin_lib::backup_scheduler",
            due = %stored_due,
            "Backup Schedule due time changed before it could advance"
        );
    }
    Ok(true)
}

fn catch_up_event<'a>(stored_due: &str, path: &Path) -> NewAuditEvent<'a> {
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scheduled Backup".to_owned());
    let mut event = NewAuditEvent::simple(
        "backup_schedule_caught_up",
        AuditOutcome::Success,
        AuditActor::System,
        "Completed a missed scheduled Backup",
    );
    event.metadata = json!({
        "missed_due_time": stored_due,
        "filename": filename,
    });
    event
}

async fn advance_and_note_catch_up(
    pool: &SqlitePool,
    stored_due: &str,
    next_due_at: &str,
    catch_up_event: Option<NewAuditEvent<'_>>,
) -> AppResult<bool> {
    let mut transaction = db::begin_immediate(pool).await?;
    let advanced = db::backup_schedule::advance_next_due_at_in_transaction(
        &mut transaction,
        stored_due,
        next_due_at,
    )
    .await?;
    if let Some(event) = catch_up_event {
        audit::append(&mut transaction, event).await?;
    }
    transaction.commit().await?;
    Ok(advanced)
}

fn owner_safe_failure_reason(error: &AppError) -> &'static str {
    match error {
        AppError::Io(_) => "backup_filesystem_error",
        AppError::Database(_) => "backup_database_error",
        AppError::InvalidInput(_) => "backup_schedule_configuration_error",
        AppError::Conflict(_) => "backup_schedule_conflict",
        _ => "backup_attempt_failed",
    }
}

async fn append_schedule_failure(pool: &SqlitePool, due: &str, error: &AppError) {
    let mut event = NewAuditEvent::simple(
        "backup_schedule_failed",
        AuditOutcome::Failure,
        AuditActor::System,
        "A scheduled Backup failed",
    );
    event.metadata = json!({
        "due_time": due,
        "reason": owner_safe_failure_reason(error),
    });
    if let Err(audit_error) = audit::append_standalone(pool, event).await {
        warn!(
            target: "openpiso_admin_lib::backup_scheduler",
            "Backup failure audit append failed: {audit_error}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mem_test_pool;
    use chrono::DateTime;

    fn fixed_now(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).expect("valid test timestamp")
    }

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "openpiso-scheduled-backup-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("scheduled Backup directory");
        path
    }

    #[tokio::test]
    async fn due_schedule_writes_one_catch_up_backup_and_advances_due_time() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let directory = temp_dir();
        sqlx::query(
            "UPDATE backup_schedule_settings
             SET frequency = 'daily', next_due_at = '2026-01-01T04:00:00+08:00',
                 backup_sets = '[\"Accounts\"]', retention_count = 2",
        )
        .execute(&pool)
        .await?;

        let now = fixed_now("2026-08-03T05:00:00+08:00");
        assert!(run_once(&pool, &directory, now, true).await?);
        let entries = backup::list_backups(&directory).await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "scheduled");
        assert!(entries[0].catch_up);

        sqlx::query(
            "UPDATE backup_schedule_settings
             SET next_due_at = '2026-08-03T04:00:00+08:00', retention_count = 1",
        )
        .execute(&pool)
        .await?;
        assert!(run_once(&pool, &directory, now, false).await?);
        assert_eq!(backup::list_backups(&directory).await?.len(), 1);

        let next_due: String =
            sqlx::query_scalar("SELECT next_due_at FROM backup_schedule_settings WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        assert_eq!(next_due, "2026-08-04T04:00:00+08:00");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'backup_created'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 0);
        let catch_up_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'backup_schedule_caught_up'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(catch_up_count, 1);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test]
    async fn failed_due_attempt_emits_one_failure_notice() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let directory = temp_dir();
        let blocked = directory.join("not-a-directory");
        std::fs::write(&blocked, "blocked")?;
        sqlx::query(
            "UPDATE backup_schedule_settings
             SET next_due_at = '2026-01-01T04:00:00+08:00', backup_sets = '[\"Accounts\"]'",
        )
        .execute(&pool)
        .await?;

        let result = run_once(
            &pool,
            &blocked,
            fixed_now("2026-08-03T05:00:00+08:00"),
            false,
        )
        .await;
        assert!(result.is_err());
        let failure_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'backup_schedule_failed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(failure_count, 1);
        let metadata: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'backup_schedule_failed'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(metadata.len() < 500);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test]
    async fn disabled_schedule_does_not_write() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let directory = temp_dir();
        sqlx::query(
            "UPDATE backup_schedule_settings
             SET enabled = 0, next_due_at = '2020-01-01T00:00:00Z'",
        )
        .execute(&pool)
        .await?;
        assert!(
            !run_once(
                &pool,
                &directory,
                fixed_now("2026-08-03T05:00:00+08:00"),
                true,
            )
            .await?
        );
        assert_eq!(backup::list_backups(&directory).await?.len(), 0);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }
}

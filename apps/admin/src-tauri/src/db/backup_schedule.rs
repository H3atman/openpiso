use chrono::Local;
use openpiso_core::backup_schedule::{self, BackupFrequency, BackupSchedule};
use serde_json::json;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

type StoredScheduleRow = (
    i64,
    String,
    i64,
    i64,
    i64,
    String,
    i64,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
);

use crate::{
    audit::{self, NewAuditEvent},
    backup::BackupSet,
    errors::{AppError, AppResult},
    models::{BackupScheduleSettings, BackupScheduleSettingsInput},
};

fn frequency_name(frequency: BackupFrequency) -> &'static str {
    match frequency {
        BackupFrequency::Daily => "daily",
        BackupFrequency::Weekly => "weekly",
        BackupFrequency::Monthly => "monthly",
        #[cfg(debug_assertions)]
        BackupFrequency::Interval => "interval",
    }
}

fn frequency_from_stored(value: &str) -> AppResult<BackupFrequency> {
    match value {
        "daily" => Ok(BackupFrequency::Daily),
        "weekly" => Ok(BackupFrequency::Weekly),
        "monthly" => Ok(BackupFrequency::Monthly),
        "interval" => {
            #[cfg(debug_assertions)]
            {
                Ok(BackupFrequency::Interval)
            }
            #[cfg(not(debug_assertions))]
            {
                // A development database must remain usable by a release build.
                Ok(BackupFrequency::Daily)
            }
        }
        other => Err(AppError::InvalidInput(format!(
            "unknown stored Backup Schedule frequency: {other}"
        ))),
    }
}

fn parse_backup_sets(value: &str) -> AppResult<Vec<String>> {
    let sets: Vec<String> = serde_json::from_str(value).map_err(|error| {
        AppError::InvalidInput(format!("stored Backup Schedule sets are invalid: {error}"))
    })?;
    if sets.is_empty() {
        return Err(AppError::InvalidInput(
            "Backup Schedule requires at least one Backup Set".to_owned(),
        ));
    }
    let mut parsed = Vec::with_capacity(sets.len());
    for set in sets {
        if BackupSet::parse_name(&set).is_none() || parsed.contains(&set) {
            return Err(AppError::InvalidInput(format!(
                "unknown or duplicate Backup Set: {set}"
            )));
        }
        parsed.push(set);
    }
    Ok(parsed)
}

fn input_backup_sets(input: &BackupScheduleSettingsInput) -> AppResult<Vec<String>> {
    if input.backup_sets.is_empty() {
        return Err(AppError::InvalidInput(
            "Backup Schedule requires at least one Backup Set".to_owned(),
        ));
    }
    let encoded = serde_json::to_string(&input.backup_sets).map_err(|error| {
        AppError::InvalidInput(format!("Backup Schedule sets are invalid: {error}"))
    })?;
    parse_backup_sets(&encoded)
}

fn core_schedule(input: &BackupScheduleSettingsInput) -> AppResult<BackupSchedule> {
    #[cfg(debug_assertions)]
    let interval_seconds = Some(input.interval_seconds);
    #[cfg(not(debug_assertions))]
    let interval_seconds = None;

    backup_schedule::validate_fields(
        Some(input.frequency),
        input.weekday,
        input.day_of_month,
        input.minute_of_day,
        input.retention_count,
        interval_seconds,
    )
    .map_err(|error| AppError::InvalidInput(error.to_string()))?;

    Ok(BackupSchedule {
        enabled: input.enabled,
        frequency: input.frequency,
        minute_of_day: u16::try_from(input.minute_of_day)
            .map_err(|_| AppError::InvalidInput("Backup minute of day is invalid".to_owned()))?,
        weekday: u8::try_from(input.weekday)
            .map_err(|_| AppError::InvalidInput("Backup weekday is invalid".to_owned()))?,
        day_of_month: u8::try_from(input.day_of_month)
            .map_err(|_| AppError::InvalidInput("Backup day of month is invalid".to_owned()))?,
        #[cfg(debug_assertions)]
        interval_seconds: u32::try_from(
            interval_seconds
                .ok_or_else(|| AppError::InvalidInput("Backup interval is invalid".to_owned()))?,
        )
        .map_err(|_| AppError::InvalidInput("Backup interval is invalid".to_owned()))?,
    })
}

pub(crate) fn core_schedule_from_settings(
    settings: &BackupScheduleSettings,
) -> AppResult<BackupSchedule> {
    let input = BackupScheduleSettingsInput {
        enabled: settings.enabled,
        frequency: settings.frequency,
        minute_of_day: settings.minute_of_day,
        weekday: settings.weekday,
        day_of_month: settings.day_of_month,
        backup_sets: settings.backup_sets.clone(),
        retention_count: settings.retention_count,
        mirror_directory: settings.mirror_directory.clone(),
        #[cfg(debug_assertions)]
        interval_seconds: settings.interval_seconds,
    };
    core_schedule(&input)
}

fn computed_next_due_at(input: &BackupScheduleSettingsInput) -> AppResult<Option<String>> {
    Ok(
        backup_schedule::next_due(core_schedule(input)?, Local::now().fixed_offset())
            .map(|value| value.to_rfc3339()),
    )
}

async fn settings_from_connection(
    connection: &mut SqliteConnection,
) -> AppResult<BackupScheduleSettings> {
    let (
        enabled,
        frequency,
        minute_of_day,
        weekday,
        day_of_month,
        backup_sets,
        retention_count,
        mirror_directory,
        mirror_error,
        interval_seconds,
        stored_next_due_at,
    ): StoredScheduleRow = sqlx::query_as(
        "SELECT enabled, frequency, minute_of_day, weekday, day_of_month,
                backup_sets, retention_count, mirror_directory, mirror_error,
                interval_seconds, next_due_at
         FROM backup_schedule_settings WHERE id = 1",
    )
    .fetch_one(&mut *connection)
    .await?;

    let frequency = frequency_from_stored(&frequency)?;
    #[cfg(not(debug_assertions))]
    let _ = interval_seconds;
    let backup_sets = parse_backup_sets(&backup_sets)?;
    let input = BackupScheduleSettingsInput {
        enabled: enabled != 0,
        frequency,
        minute_of_day,
        weekday,
        day_of_month,
        backup_sets: backup_sets.clone(),
        retention_count,
        mirror_directory,
        #[cfg(debug_assertions)]
        interval_seconds,
    };
    let next_due_at = if input.enabled {
        stored_next_due_at.or(computed_next_due_at(&input)?)
    } else {
        None
    };

    Ok(BackupScheduleSettings {
        enabled: input.enabled,
        frequency,
        minute_of_day,
        weekday,
        day_of_month,
        backup_sets,
        retention_count,
        mirror_directory: input.mirror_directory,
        mirror_error,
        next_due_at,
        #[cfg(debug_assertions)]
        interval_seconds,
    })
}

pub async fn backup_schedule_settings(pool: &SqlitePool) -> AppResult<BackupScheduleSettings> {
    let mut connection = pool.acquire().await?;
    settings_from_connection(&mut connection).await
}

pub(crate) async fn stored_next_due_at(pool: &SqlitePool) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT next_due_at FROM backup_schedule_settings WHERE id = 1")
            .fetch_one(pool)
            .await?,
    )
}

pub(crate) async fn configured_mirror_directory(pool: &SqlitePool) -> AppResult<Option<String>> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master
           WHERE type = 'table' AND name = 'backup_schedule_settings'
         )",
    )
    .fetch_one(pool)
    .await?;
    if exists == 0 {
        return Ok(None);
    }

    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT mirror_directory FROM backup_schedule_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?
    .and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }))
}

pub(crate) async fn set_mirror_error(
    pool: &SqlitePool,
    mirror_error: Option<&str>,
) -> AppResult<()> {
    sqlx::query("UPDATE backup_schedule_settings SET mirror_error = ? WHERE id = 1")
        .bind(mirror_error)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn set_next_due_at(pool: &SqlitePool, next_due_at: Option<&str>) -> AppResult<()> {
    sqlx::query("UPDATE backup_schedule_settings SET next_due_at = ? WHERE id = 1")
        .bind(next_due_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Advances the schedule watermark inside the caller's transaction so the
/// catch-up Notice commits with the advance, never after it.
pub(crate) async fn advance_next_due_at_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    expected: &str,
    next_due_at: &str,
) -> AppResult<bool> {
    let result = sqlx::query(
        "UPDATE backup_schedule_settings
         SET next_due_at = ?
         WHERE id = 1 AND enabled = 1 AND next_due_at = ?",
    )
    .bind(next_due_at)
    .bind(expected)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn update_backup_schedule(
    pool: &SqlitePool,
    input: &BackupScheduleSettingsInput,
) -> AppResult<BackupScheduleSettings> {
    let backup_sets = input_backup_sets(input)?;
    // Validate before opening the transaction so malformed owner input cannot
    // partially change the singleton record.
    core_schedule(input)?;

    let mut transaction = super::begin_immediate(pool).await?;
    update_in_transaction(&mut transaction, input, &backup_sets).await?;
    transaction.commit().await?;
    backup_schedule_settings(pool).await
}

async fn update_in_transaction(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    input: &BackupScheduleSettingsInput,
    backup_sets: &[String],
) -> AppResult<()> {
    let mirror_directory = input
        .mirror_directory
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    sqlx::query(
        "UPDATE backup_schedule_settings
         SET enabled = ?, frequency = ?, minute_of_day = ?, weekday = ?,
             day_of_month = ?, backup_sets = ?, retention_count = ?,
             mirror_directory = ?, mirror_error = NULL, next_due_at = ?, interval_seconds = ?
         WHERE id = 1",
    )
    .bind(input.enabled)
    .bind(frequency_name(input.frequency))
    .bind(input.minute_of_day)
    .bind(input.weekday)
    .bind(input.day_of_month)
    .bind(serde_json::to_string(backup_sets).map_err(|error| {
        AppError::InvalidInput(format!("Backup Schedule sets are invalid: {error}"))
    })?)
    .bind(input.retention_count)
    .bind(mirror_directory)
    .bind(computed_next_due_at(input)?)
    .bind({
        #[cfg(debug_assertions)]
        {
            input.interval_seconds
        }
        #[cfg(not(debug_assertions))]
        {
            60_i64
        }
    })
    .execute(&mut **transaction)
    .await?;

    let mut event = NewAuditEvent::simple(
        "backup_settings_changed",
        openpiso_core::audit::AuditOutcome::Success,
        openpiso_core::audit::AuditActor::Admin,
        "Updated Backup Schedule",
    );
    event.metadata = json!({
        "enabled": input.enabled,
        "frequency": frequency_name(input.frequency),
        "minute_of_day": input.minute_of_day,
        "weekday": input.weekday,
        "day_of_month": input.day_of_month,
        "backup_sets": backup_sets,
        "retention_count": input.retention_count,
        "mirror_directory": mirror_directory,
    });
    audit::append(transaction, event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mem_test_pool;

    fn input() -> BackupScheduleSettingsInput {
        BackupScheduleSettingsInput {
            enabled: true,
            frequency: BackupFrequency::Daily,
            minute_of_day: 240,
            weekday: 0,
            day_of_month: 1,
            backup_sets: BackupSet::all()
                .into_iter()
                .map(|set| set.as_str().to_owned())
                .collect(),
            retention_count: 14,
            mirror_directory: None,
            #[cfg(debug_assertions)]
            interval_seconds: 60,
        }
    }

    #[tokio::test]
    async fn defaults_round_trip_and_update_audit() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let defaults = backup_schedule_settings(&pool).await?;
        assert!(defaults.enabled);
        assert_eq!(defaults.frequency, BackupFrequency::Daily);
        assert_eq!(defaults.minute_of_day, 240);
        assert_eq!(defaults.retention_count, 14);
        assert_eq!(defaults.backup_sets.len(), 7);

        let mut changed = input();
        changed.frequency = BackupFrequency::Monthly;
        changed.day_of_month = 31;
        changed.mirror_directory = Some("D:/mirrors".to_owned());
        let saved = update_backup_schedule(&pool, &changed).await?;
        assert_eq!(saved.frequency, BackupFrequency::Monthly);
        assert_eq!(saved.day_of_month, 31);
        assert_eq!(saved.mirror_directory.as_deref(), Some("D:/mirrors"));
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'backup_settings_changed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(count, 1);
        Ok(())
    }
}

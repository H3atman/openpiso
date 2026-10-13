use std::collections::HashSet;

use chrono::{DateTime, Local, Utc};
use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    shop_hours::{self, DaySchedule, ShopHoursSchedule, ShopOverride, ShopOverrideMode},
};
use serde_json::json;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{
        ShopHoursDay, ShopHoursDayInput, ShopHoursDayMode, ShopHoursOverride, ShopHoursSettings,
        ShopHoursSettingsInput, ShopOverrideInput, ShopStatus,
    },
};

const MINUTES_PER_DAY: i64 = 1_440;
const MAX_LAST_CALL_LEAD_MINUTES: i64 = 65_535;
type ValidatedDay = (i64, ShopHoursDayMode, Option<i64>, Option<i64>);

fn mode_name(mode: ShopHoursDayMode) -> &'static str {
    match mode {
        ShopHoursDayMode::OpenPeriod => "open_period",
        ShopHoursDayMode::ClosedAllDay => "closed_all_day",
        ShopHoursDayMode::Open24Hours => "open_24_hours",
    }
}

fn day_mode_from_storage(mode: &str) -> AppResult<ShopHoursDayMode> {
    match mode {
        "open_period" => Ok(ShopHoursDayMode::OpenPeriod),
        "closed_all_day" => Ok(ShopHoursDayMode::ClosedAllDay),
        "open_24_hours" => Ok(ShopHoursDayMode::Open24Hours),
        other => Err(AppError::InvalidInput(format!(
            "unknown stored Shop Hours mode: {other}"
        ))),
    }
}

pub(crate) fn day_schedule_from_storage(
    mode: &str,
    open_minute: Option<i64>,
    close_minute: Option<i64>,
) -> AppResult<DaySchedule> {
    match day_mode_from_storage(mode)? {
        ShopHoursDayMode::OpenPeriod => {
            let open = u16::try_from(
                open_minute
                    .ok_or_else(|| AppError::InvalidInput("missing open minute".to_owned()))?,
            )
            .map_err(|_| AppError::InvalidInput("invalid open minute".to_owned()))?;
            let close = u16::try_from(
                close_minute
                    .ok_or_else(|| AppError::InvalidInput("missing close minute".to_owned()))?,
            )
            .map_err(|_| AppError::InvalidInput("invalid close minute".to_owned()))?;
            DaySchedule::open_period(open, close)
                .map_err(|error| AppError::InvalidInput(error.to_string()))
        }
        ShopHoursDayMode::ClosedAllDay => Ok(DaySchedule::closed_all_day()),
        ShopHoursDayMode::Open24Hours => Ok(DaySchedule::open_24_hours()),
    }
}

pub(crate) fn override_mode_from_storage(mode: &str) -> AppResult<ShopOverrideMode> {
    match mode {
        "force_open" => Ok(ShopOverrideMode::ForceOpen),
        "force_closed" => Ok(ShopOverrideMode::ForceClosed),
        other => Err(AppError::InvalidInput(format!(
            "unknown stored Shop Override mode: {other}"
        ))),
    }
}

fn validate_days(input: &[ShopHoursDayInput]) -> AppResult<Vec<ValidatedDay>> {
    if input.len() != 7 {
        return Err(AppError::InvalidInput(
            "Shop Hours requires exactly seven day rows".to_owned(),
        ));
    }

    let mut weekdays = HashSet::with_capacity(input.len());
    let mut rows = Vec::with_capacity(input.len());

    for day in input {
        if !(0..=6).contains(&day.weekday) || !weekdays.insert(day.weekday) {
            return Err(AppError::InvalidInput(
                "Shop Hours weekdays must be Monday through Sunday exactly once".to_owned(),
            ));
        }

        match day.mode {
            ShopHoursDayMode::OpenPeriod => {
                let open_minute = day.open_minute.ok_or_else(|| {
                    AppError::InvalidInput("Open Period requires an open minute".to_owned())
                })?;
                let close_minute = day.close_minute.ok_or_else(|| {
                    AppError::InvalidInput("Open Period requires a close minute".to_owned())
                })?;
                if !(0..=1_439).contains(&open_minute) || !(0..=1_439).contains(&close_minute) {
                    return Err(AppError::InvalidInput(
                        "Open and close minutes must be between 0 and 1439".to_owned(),
                    ));
                }
                rows.push((day.weekday, day.mode, Some(open_minute), Some(close_minute)));
            }
            ShopHoursDayMode::ClosedAllDay | ShopHoursDayMode::Open24Hours => {
                if day.open_minute.is_some() || day.close_minute.is_some() {
                    return Err(AppError::InvalidInput(
                        "Closed and Open 24 Hours rows cannot carry times".to_owned(),
                    ));
                }
                rows.push((day.weekday, day.mode, None, None));
            }
        }
    }

    rows.sort_unstable_by_key(|row| row.0);
    Ok(rows)
}

fn validate_input(input: &ShopHoursSettingsInput) -> AppResult<Vec<ValidatedDay>> {
    if input.last_call_lead_minutes < 0 {
        return Err(AppError::InvalidInput(
            "Last Call lead minutes cannot be negative".to_owned(),
        ));
    }
    if input.last_call_lead_minutes > MAX_LAST_CALL_LEAD_MINUTES {
        return Err(AppError::InvalidInput(
            "Last Call lead minutes is too large".to_owned(),
        ));
    }

    let rows = validate_days(&input.days)?;
    if let Some(shortest_open_period) = rows
        .iter()
        .filter_map(|(_, mode, open, close)| {
            if !matches!(mode, ShopHoursDayMode::OpenPeriod) {
                return None;
            }
            let open = (*open)?;
            let close = (*close)?;
            Some(if close > open {
                close - open
            } else {
                MINUTES_PER_DAY - open + close
            })
        })
        .min()
    {
        if input.last_call_lead_minutes > shortest_open_period {
            return Err(AppError::InvalidInput(
                "Last Call lead cannot exceed the shortest Open Period".to_owned(),
            ));
        }
    }
    Ok(rows)
}

async fn shop_hours_settings_from_connection(
    connection: &mut SqliteConnection,
) -> AppResult<ShopHoursSettings> {
    let (enabled, last_call_lead_minutes): (i64, i64) = sqlx::query_as(
        "SELECT enabled, last_call_lead_minutes
         FROM shop_hours_settings WHERE id = 1",
    )
    .fetch_one(&mut *connection)
    .await?;

    let days = sqlx::query_as::<_, (i64, String, Option<i64>, Option<i64>)>(
        "SELECT weekday, mode, open_minute, close_minute
         FROM shop_hours_schedule ORDER BY weekday",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(weekday, mode, open_minute, close_minute)| {
        let mode = day_mode_from_storage(&mode)?;
        Ok(ShopHoursDay {
            weekday,
            mode,
            open_minute,
            close_minute,
        })
    })
    .collect::<AppResult<Vec<_>>>()?;

    let shop_override = sqlx::query_as::<_, (String, String)>(
        "SELECT mode, expires_at FROM shop_override WHERE id = 1",
    )
    .fetch_optional(&mut *connection)
    .await?
    .map(|(mode, expires_at)| -> AppResult<ShopHoursOverride> {
        Ok(ShopHoursOverride {
            mode: override_mode_from_storage(&mode)?,
            expires_at,
        })
    })
    .transpose()?;

    Ok(ShopHoursSettings {
        enabled: enabled != 0,
        last_call_lead_minutes,
        days,
        shop_override,
    })
}

pub async fn shop_hours_settings(pool: &SqlitePool) -> AppResult<ShopHoursSettings> {
    let mut connection = pool.acquire().await?;
    shop_hours_settings_from_connection(&mut connection).await
}

fn core_schedule(settings: &ShopHoursSettings) -> AppResult<ShopHoursSchedule> {
    if settings.days.len() != 7 {
        return Err(AppError::InvalidInput(
            "stored Shop Hours schedule must contain seven days".to_owned(),
        ));
    }

    let mut days = [None; 7];
    for day in &settings.days {
        let weekday = usize::try_from(day.weekday).map_err(|_| {
            AppError::InvalidInput("stored Shop Hours weekday is invalid".to_owned())
        })?;
        if weekday >= days.len() || days[weekday].is_some() {
            return Err(AppError::InvalidInput(
                "stored Shop Hours weekdays are invalid".to_owned(),
            ));
        }
        let schedule = match day.mode {
            ShopHoursDayMode::OpenPeriod => {
                let open = u16::try_from(day.open_minute.ok_or_else(|| {
                    AppError::InvalidInput("stored Open Period has no open minute".to_owned())
                })?)
                .map_err(|_| AppError::InvalidInput("stored open minute is invalid".to_owned()))?;
                let close = u16::try_from(day.close_minute.ok_or_else(|| {
                    AppError::InvalidInput("stored Open Period has no close minute".to_owned())
                })?)
                .map_err(|_| AppError::InvalidInput("stored close minute is invalid".to_owned()))?;
                DaySchedule::open_period(open, close).map_err(|error| {
                    AppError::InvalidInput(format!("stored Open Period is invalid: {error}"))
                })?
            }
            ShopHoursDayMode::ClosedAllDay => DaySchedule::closed_all_day(),
            ShopHoursDayMode::Open24Hours => DaySchedule::open_24_hours(),
        };
        days[weekday] = Some(schedule);
    }

    let days = days
        .into_iter()
        .map(|day| {
            day.ok_or_else(|| AppError::InvalidInput("stored Shop Hours day is missing".to_owned()))
        })
        .collect::<AppResult<Vec<_>>>()?
        .try_into()
        .map_err(|_| {
            AppError::InvalidInput("stored Shop Hours schedule is incomplete".to_owned())
        })?;

    Ok(ShopHoursSchedule {
        enabled: settings.enabled,
        last_call_lead_minutes: u16::try_from(settings.last_call_lead_minutes).map_err(|_| {
            AppError::InvalidInput("stored Last Call lead minutes are invalid".to_owned())
        })?,
        days,
    })
}

fn core_override(settings: &ShopHoursSettings) -> AppResult<Option<ShopOverride>> {
    settings
        .shop_override
        .as_ref()
        .map(|value| {
            let expires_at = DateTime::parse_from_rfc3339(&value.expires_at).map_err(|error| {
                AppError::InvalidInput(format!("stored Shop Override expiry is invalid: {error}"))
            })?;
            Ok(ShopOverride {
                mode: value.mode,
                expires_at,
            })
        })
        .transpose()
}

fn resolved_shop_status(
    settings: &ShopHoursSettings,
    now: DateTime<chrono::FixedOffset>,
) -> AppResult<openpiso_core::shop_hours::ShopStatus> {
    let schedule = core_schedule(settings)?;
    let shop_override = core_override(settings)?;
    Ok(shop_hours::resolve(&schedule, shop_override.as_ref(), now))
}

fn status_model(resolved: openpiso_core::shop_hours::ShopStatus) -> ShopStatus {
    ShopStatus {
        open: resolved.open,
        next_change_at: resolved.next_change_at.map(|value| value.to_rfc3339()),
        reason: resolved.reason,
        last_call_active: resolved.last_call_active,
    }
}

/// Resolve the current Shop Status from persisted Shop Hours and the Admin
/// host's local clock. This read does not mutate the transition ledger; the
/// background observer owns transition detection.
pub async fn shop_status(pool: &SqlitePool) -> AppResult<ShopStatus> {
    let settings = shop_hours_settings(pool).await?;
    Ok(status_model(resolved_shop_status(
        &settings,
        Local::now().fixed_offset(),
    )?))
}

#[derive(Debug, Clone, Copy)]
enum ShopTransitionCause {
    Schedule,
    Override,
    MasterToggle,
}

impl ShopTransitionCause {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Override => "override",
            Self::MasterToggle => "master_toggle",
        }
    }
}

fn reason_name(reason: openpiso_core::shop_hours::ShopStatusReason) -> &'static str {
    match reason {
        openpiso_core::shop_hours::ShopStatusReason::Schedule => "schedule",
        openpiso_core::shop_hours::ShopStatusReason::Override => "override",
    }
}

async fn observe_shop_status_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    now: DateTime<chrono::FixedOffset>,
    cause: Option<ShopTransitionCause>,
) -> AppResult<ShopStatus> {
    let settings = shop_hours_settings_from_connection(&mut *transaction).await?;
    let resolved = resolved_shop_status(&settings, now)?;
    let (previous_open, previous_reason): (Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT observed_open, observed_reason
         FROM shop_hours_runtime_state WHERE id = 1",
    )
    .fetch_one(&mut **transaction)
    .await?;

    // Disabled Shop Hours is inert for audit purposes. Resetting the ledger to
    // Shop Open means enabling it later can record the resulting transition,
    // while flipping it off never manufactures a historical event.
    let ledger_open = if settings.enabled {
        i64::from(resolved.open)
    } else {
        1
    };
    let ledger_reason = if settings.enabled {
        reason_name(resolved.reason)
    } else {
        "schedule"
    };
    let transitioned = settings.enabled && previous_open.is_some_and(|open| open != ledger_open);

    if transitioned {
        let transition_cause = cause.unwrap_or_else(|| {
            if previous_reason.as_deref() == Some("override")
                || resolved.reason == openpiso_core::shop_hours::ShopStatusReason::Override
            {
                ShopTransitionCause::Override
            } else {
                ShopTransitionCause::Schedule
            }
        });
        let opened = resolved.open;
        let mut event = NewAuditEvent::simple(
            if opened { "shop_opened" } else { "shop_closed" },
            AuditOutcome::Success,
            AuditActor::System,
            if opened { "Shop Opened" } else { "Shop Closed" },
        );
        event.metadata = json!({
            "cause": transition_cause.as_str(),
            "reason": reason_name(resolved.reason),
            "from_open": previous_open == Some(1),
            "to_open": opened,
            "next_change_at": resolved.next_change_at.map(|value| value.to_rfc3339()),
        });
        audit::append(transaction, event).await?;
    }

    sqlx::query(
        "UPDATE shop_hours_runtime_state
         SET observed_open = ?, observed_reason = ?
         WHERE id = 1",
    )
    .bind(ledger_open)
    .bind(ledger_reason)
    .execute(&mut **transaction)
    .await?;

    Ok(status_model(resolved))
}

pub async fn observe_shop_status(pool: &SqlitePool) -> AppResult<ShopStatus> {
    observe_shop_status_at(pool, Local::now().fixed_offset()).await
}

async fn observe_shop_status_at(
    pool: &SqlitePool,
    now: DateTime<chrono::FixedOffset>,
) -> AppResult<ShopStatus> {
    let mut transaction = super::begin_immediate(pool).await?;
    let status = observe_shop_status_in_transaction(&mut transaction, now, None).await?;
    transaction.commit().await?;
    Ok(status)
}

fn override_mode_name(mode: openpiso_core::shop_hours::ShopOverrideMode) -> &'static str {
    match mode {
        openpiso_core::shop_hours::ShopOverrideMode::ForceOpen => "force_open",
        openpiso_core::shop_hours::ShopOverrideMode::ForceClosed => "force_closed",
    }
}

fn parse_future_override(input: &ShopOverrideInput) -> AppResult<DateTime<chrono::FixedOffset>> {
    let expires_at = DateTime::parse_from_rfc3339(&input.expires_at).map_err(|error| {
        AppError::InvalidInput(format!("Shop Override expiry must be RFC 3339: {error}"))
    })?;
    if expires_at <= Utc::now().with_timezone(expires_at.offset()) {
        return Err(AppError::InvalidInput(
            "Shop Override expiry must be in the future".to_owned(),
        ));
    }
    Ok(expires_at)
}

pub async fn set_shop_override(
    pool: &SqlitePool,
    input: &ShopOverrideInput,
) -> AppResult<ShopHoursSettings> {
    let expires_at = parse_future_override(input)?;
    let mut transaction = super::begin_immediate(pool).await?;
    if expires_at <= Utc::now().with_timezone(expires_at.offset()) {
        return Err(AppError::InvalidInput(
            "Shop Override expiry must be in the future".to_owned(),
        ));
    }
    let previous = shop_hours_settings_from_connection(&mut transaction).await?;
    let changed = !previous.shop_override.as_ref().is_some_and(|value| {
        value.mode == input.mode
            && DateTime::parse_from_rfc3339(&value.expires_at)
                .is_ok_and(|stored| stored == expires_at)
    });
    observe_shop_status_in_transaction(&mut transaction, Local::now().fixed_offset(), None).await?;

    sqlx::query(
        "INSERT INTO shop_override (id, mode, expires_at)
         VALUES (1, ?, ?)
         ON CONFLICT(id) DO UPDATE SET mode = excluded.mode, expires_at = excluded.expires_at",
    )
    .bind(override_mode_name(input.mode))
    .bind(expires_at.to_rfc3339())
    .execute(&mut *transaction)
    .await?;
    observe_shop_status_in_transaction(
        &mut transaction,
        Local::now().fixed_offset(),
        Some(ShopTransitionCause::Override),
    )
    .await?;

    let mut event = NewAuditEvent::simple(
        "shop_override_set",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Set Shop Override",
    );
    event.metadata = json!({
        "mode": override_mode_name(input.mode),
        "expires_at": expires_at.to_rfc3339(),
    });
    let audit_id = audit::append(&mut transaction, event).await?;
    if changed {
        sqlx::query("INSERT INTO shop_override_history (mode, effective_at, expires_at, audit_event_id) VALUES (?, ?, ?, ?)")
            .bind(override_mode_name(input.mode))
            .bind(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            .bind(
                expires_at
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            )
            .bind(audit_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;

    shop_hours_settings(pool).await
}

pub async fn clear_shop_override(pool: &SqlitePool) -> AppResult<ShopHoursSettings> {
    let mut transaction = super::begin_immediate(pool).await?;
    let previous = shop_hours_settings_from_connection(&mut transaction).await?;
    let changed = previous.shop_override.is_some();
    observe_shop_status_in_transaction(&mut transaction, Local::now().fixed_offset(), None).await?;
    sqlx::query("DELETE FROM shop_override WHERE id = 1")
        .execute(&mut *transaction)
        .await?;
    observe_shop_status_in_transaction(
        &mut transaction,
        Local::now().fixed_offset(),
        Some(ShopTransitionCause::Override),
    )
    .await?;

    let event = NewAuditEvent::simple(
        "shop_override_cleared",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Cleared Shop Override",
    );
    let audit_id = audit::append(&mut transaction, event).await?;
    if changed {
        sqlx::query("INSERT INTO shop_override_history (mode, effective_at, expires_at, audit_event_id) VALUES (NULL, ?, NULL, ?)")
            .bind(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            .bind(audit_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;

    shop_hours_settings(pool).await
}

pub async fn update_shop_hours(
    pool: &SqlitePool,
    input: &ShopHoursSettingsInput,
) -> AppResult<ShopHoursSettings> {
    let rows = validate_input(input)?;
    let mut transaction = super::begin_immediate(pool).await?;
    let previous_settings = shop_hours_settings_from_connection(&mut transaction).await?;
    let changed =
        previous_settings.enabled != input.enabled
            || previous_settings.last_call_lead_minutes != input.last_call_lead_minutes
            || !previous_settings.days.iter().zip(&rows).all(|(day, row)| {
                (day.weekday, day.mode, day.open_minute, day.close_minute) == *row
            });
    observe_shop_status_in_transaction(&mut transaction, Local::now().fixed_offset(), None).await?;
    let transition_cause = if previous_settings.enabled != input.enabled {
        ShopTransitionCause::MasterToggle
    } else {
        ShopTransitionCause::Schedule
    };

    sqlx::query("DELETE FROM shop_hours_schedule")
        .execute(&mut *transaction)
        .await?;
    for (weekday, mode, open_minute, close_minute) in &rows {
        sqlx::query(
            "INSERT INTO shop_hours_schedule
               (weekday, mode, open_minute, close_minute)
             VALUES (?, ?, ?, ?)",
        )
        .bind(weekday)
        .bind(mode_name(*mode))
        .bind(open_minute)
        .bind(close_minute)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        "UPDATE shop_hours_settings
         SET enabled = ?, last_call_lead_minutes = ?
         WHERE id = 1",
    )
    .bind(input.enabled)
    .bind(input.last_call_lead_minutes)
    .execute(&mut *transaction)
    .await?;

    let mut event = NewAuditEvent::simple(
        "shop_hours_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Shop Hours settings",
    );
    event.metadata = json!({
        "enabled": input.enabled,
        "last_call_lead_minutes": input.last_call_lead_minutes,
        "days": rows.iter().map(|(weekday, mode, open_minute, close_minute)| json!({
            "weekday": weekday,
            "mode": mode_name(*mode),
            "open_minute": open_minute,
            "close_minute": close_minute,
        })).collect::<Vec<_>>(),
    });
    let audit_id = audit::append(&mut transaction, event).await?;
    if changed {
        let version_id = sqlx::query("INSERT INTO shop_hours_versions (enabled, last_call_lead_minutes, effective_at, audit_event_id) VALUES (?, ?, ?, ?)")
            .bind(input.enabled)
            .bind(input.last_call_lead_minutes)
            .bind(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            .bind(audit_id)
            .execute(&mut *transaction)
            .await?
            .last_insert_rowid();
        for (weekday, mode, open_minute, close_minute) in &rows {
            sqlx::query("INSERT INTO shop_hours_version_days (shop_hours_version_id, weekday, mode, open_minute, close_minute) VALUES (?, ?, ?, ?, ?)")
                .bind(version_id)
                .bind(weekday)
                .bind(mode_name(*mode))
                .bind(open_minute)
                .bind(close_minute)
                .execute(&mut *transaction)
                .await?;
        }
    }
    observe_shop_status_in_transaction(
        &mut transaction,
        Local::now().fixed_offset(),
        Some(transition_cause),
    )
    .await?;
    transaction.commit().await?;

    shop_hours_settings(pool).await
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        super::super::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await?;
        super::super::seed(&pool).await?;
        Ok(pool)
    }

    fn open_days(open_minute: i64, close_minute: i64) -> Vec<ShopHoursDayInput> {
        (0..7)
            .map(|weekday| ShopHoursDayInput {
                weekday,
                mode: ShopHoursDayMode::OpenPeriod,
                open_minute: Some(open_minute),
                close_minute: Some(close_minute),
            })
            .collect()
    }

    fn input(days: Vec<ShopHoursDayInput>) -> ShopHoursSettingsInput {
        ShopHoursSettingsInput {
            enabled: true,
            last_call_lead_minutes: 15,
            days,
        }
    }

    fn mode_days(mode: ShopHoursDayMode) -> Vec<ShopHoursDayInput> {
        (0..7)
            .map(|weekday| ShopHoursDayInput {
                weekday,
                mode,
                open_minute: None,
                close_minute: None,
            })
            .collect()
    }

    /// A `closed_all_day` or `open_24_hours` row carrying times, which
    /// `validate_days` rejects before any SQL runs because those modes have no
    /// meaningful Open Period. Either time alone is enough to reject.
    fn timed_mode_days(
        mode: ShopHoursDayMode,
        weekday: usize,
        open_minute: Option<i64>,
        close_minute: Option<i64>,
    ) -> Vec<ShopHoursDayInput> {
        let mut days = mode_days(mode);
        days[weekday].open_minute = open_minute;
        days[weekday].close_minute = close_minute;
        days
    }

    /// An `open_period` row missing one of the times it requires.
    fn untimed_open_period_days(open_minute: Option<i64>) -> Vec<ShopHoursDayInput> {
        let mut days = open_days(480, 1_320);
        days[0].open_minute = open_minute;
        days[0].close_minute = None;
        days
    }

    async fn transition_events(pool: &SqlitePool) -> AppResult<Vec<(String, String, String)>> {
        Ok(sqlx::query_as(
            "SELECT event_type, actor, metadata_json
             FROM audit_log
             WHERE event_type IN ('shop_opened', 'shop_closed')
             ORDER BY created_at, id",
        )
        .fetch_all(pool)
        .await?)
    }

    #[tokio::test]
    async fn scheduled_transition_is_recorded_once_and_survives_repeated_observation(
    ) -> AppResult<()> {
        let pool = test_pool().await?;
        update_shop_hours(
            &pool,
            &ShopHoursSettingsInput {
                enabled: true,
                last_call_lead_minutes: 15,
                days: mode_days(ShopHoursDayMode::ClosedAllDay),
            },
        )
        .await?;
        update_shop_hours(
            &pool,
            &ShopHoursSettingsInput {
                enabled: true,
                last_call_lead_minutes: 15,
                days: mode_days(ShopHoursDayMode::Open24Hours),
            },
        )
        .await?;

        let now = Local::now().fixed_offset();
        observe_shop_status_at(&pool, now).await?;
        observe_shop_status_at(&pool, now).await?;

        let events = transition_events(&pool).await?;
        assert_eq!(events.len(), 2);
        let scheduled = events
            .iter()
            .find(|(_, _, metadata)| {
                serde_json::from_str::<serde_json::Value>(metadata).unwrap()["cause"] == "schedule"
            })
            .expect("schedule transition event");
        assert_eq!(scheduled.0, "shop_opened");
        assert_eq!(scheduled.1, "system");
        Ok(())
    }

    #[tokio::test]
    async fn override_set_and_expiry_record_override_driven_transitions() -> AppResult<()> {
        let pool = test_pool().await?;
        update_shop_hours(
            &pool,
            &ShopHoursSettingsInput {
                enabled: true,
                last_call_lead_minutes: 15,
                days: mode_days(ShopHoursDayMode::ClosedAllDay),
            },
        )
        .await?;
        let expiry = Utc::now() + Duration::hours(1);
        set_shop_override(
            &pool,
            &ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceOpen,
                expires_at: expiry.to_rfc3339(),
            },
        )
        .await?;
        observe_shop_status_at(&pool, expiry.fixed_offset()).await?;

        let events = transition_events(&pool).await?;
        assert_eq!(events.len(), 3);
        let override_events: Vec<_> = events
            .iter()
            .filter(|(_, _, metadata_json)| {
                serde_json::from_str::<serde_json::Value>(metadata_json).unwrap()["cause"]
                    == "override"
            })
            .collect();
        assert_eq!(override_events.len(), 2);
        assert!(override_events.iter().any(|event| event.0 == "shop_opened"));
        assert!(override_events.iter().any(|event| event.0 == "shop_closed"));
        for (_, actor, _) in override_events {
            assert_eq!(*actor, "system");
        }
        Ok(())
    }

    #[tokio::test]
    async fn disabled_shop_hours_emits_no_transition_until_enabled() -> AppResult<()> {
        let pool = test_pool().await?;
        let disabled = ShopHoursSettingsInput {
            enabled: false,
            last_call_lead_minutes: 15,
            days: mode_days(ShopHoursDayMode::ClosedAllDay),
        };
        update_shop_hours(&pool, &disabled).await?;
        observe_shop_status_at(&pool, Local::now().fixed_offset()).await?;
        assert!(transition_events(&pool).await?.is_empty());

        update_shop_hours(
            &pool,
            &ShopHoursSettingsInput {
                enabled: true,
                ..disabled
            },
        )
        .await?;
        let events = transition_events(&pool).await?;
        assert_eq!(events.len(), 1);
        let metadata: serde_json::Value = serde_json::from_str(&events[0].2).unwrap();
        assert_eq!(metadata["cause"], "master_toggle");
        Ok(())
    }

    #[tokio::test]
    async fn seeded_defaults_are_inert_and_have_no_override() -> AppResult<()> {
        let settings = shop_hours_settings(&test_pool().await?).await?;
        assert!(!settings.enabled);
        assert_eq!(settings.last_call_lead_minutes, 15);
        assert_eq!(settings.days.len(), 7);
        assert!(settings.days.iter().all(|day| {
            day.mode == ShopHoursDayMode::OpenPeriod
                && day.open_minute == Some(480)
                && day.close_minute == Some(1_320)
        }));
        assert!(settings.shop_override.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unchanged_shop_hours_and_override_do_not_append_evidence() -> AppResult<()> {
        let pool = test_pool().await?;
        let settings = shop_hours_settings(&pool).await?;
        let unchanged = ShopHoursSettingsInput {
            enabled: settings.enabled,
            last_call_lead_minutes: settings.last_call_lead_minutes,
            days: settings
                .days
                .iter()
                .map(|day| ShopHoursDayInput {
                    weekday: day.weekday,
                    mode: day.mode,
                    open_minute: day.open_minute,
                    close_minute: day.close_minute,
                })
                .collect(),
        };
        update_shop_hours(&pool, &unchanged).await?;
        let observed: (Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT observed_open, observed_reason FROM shop_hours_runtime_state WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(observed, (Some(1), Some("schedule".to_owned())));
        sqlx::query(
            "UPDATE shop_hours_runtime_state SET observed_open = NULL, observed_reason = NULL WHERE id = 1",
        )
        .execute(&pool)
        .await?;
        clear_shop_override(&pool).await?;
        let observed: (Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT observed_open, observed_reason FROM shop_hours_runtime_state WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(observed, (Some(1), Some("schedule".to_owned())));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shop_hours_versions")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shop_override_history")
                .fetch_one(&pool)
                .await?,
            0
        );
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE event_type IN ('shop_hours_settings_updated', 'shop_override_cleared')").fetch_one(&pool).await?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn changed_shop_hours_and_override_link_audit_evidence() -> AppResult<()> {
        let pool = test_pool().await?;
        let changed = input(open_days(1_200, 120));
        update_shop_hours(&pool, &changed).await?;
        sqlx::query(
            "UPDATE shop_hours_runtime_state SET observed_open = NULL, observed_reason = NULL WHERE id = 1",
        )
        .execute(&pool)
        .await?;
        update_shop_hours(&pool, &changed).await?;
        let observed: (Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT observed_open, observed_reason FROM shop_hours_runtime_state WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert!(observed.0.is_some());
        let expiry = (Utc::now() + Duration::hours(1)).to_rfc3339();
        let override_input = ShopOverrideInput {
            mode: openpiso_core::shop_hours::ShopOverrideMode::ForceOpen,
            expires_at: expiry,
        };
        set_shop_override(&pool, &override_input).await?;
        sqlx::query(
            "UPDATE shop_hours_runtime_state SET observed_open = NULL, observed_reason = NULL WHERE id = 1",
        )
        .execute(&pool)
        .await?;
        set_shop_override(&pool, &override_input).await?;
        let observed: (Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT observed_open, observed_reason FROM shop_hours_runtime_state WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert!(observed.0.is_some());
        clear_shop_override(&pool).await?;
        let hours: (i64, i64) = sqlx::query_as("SELECT COUNT(*), COUNT(a.id) FROM shop_hours_versions v LEFT JOIN audit_log a ON a.id = v.audit_event_id").fetch_one(&pool).await?;
        assert_eq!(hours, (2, 1));
        let overrides: (i64, i64) = sqlx::query_as("SELECT COUNT(*), COUNT(a.id) FROM shop_override_history h JOIN audit_log a ON a.id = h.audit_event_id").fetch_one(&pool).await?;
        assert_eq!(overrides, (2, 2));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shop_hours_version_days")
                .fetch_one(&pool)
                .await?,
            14
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_hours_settings_updated'",
            )
            .fetch_one(&pool)
            .await?,
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_override_set'",
            )
            .fetch_one(&pool)
            .await?,
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn replacement_reads_back_and_appends_one_admin_audit_event() -> AppResult<()> {
        let pool = test_pool().await?;
        let mut days = open_days(1_200, 120);
        days[5] = ShopHoursDayInput {
            weekday: 5,
            mode: ShopHoursDayMode::ClosedAllDay,
            open_minute: None,
            close_minute: None,
        };
        let saved = update_shop_hours(
            &pool,
            &ShopHoursSettingsInput {
                enabled: true,
                last_call_lead_minutes: 30,
                days,
            },
        )
        .await?;
        assert!(saved.enabled);
        assert_eq!(saved.last_call_lead_minutes, 30);
        assert_eq!(saved.days[5].mode, ShopHoursDayMode::ClosedAllDay);
        assert_eq!(saved.days[0].open_minute, Some(1_200));
        assert_eq!(saved.days[0].close_minute, Some(120));

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_hours_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 1);
        let actor: String = sqlx::query_scalar(
            "SELECT actor FROM audit_log WHERE event_type = 'shop_hours_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(actor, "admin");
        Ok(())
    }

    #[tokio::test]
    async fn setting_and_clearing_override_round_trips_and_audits() -> AppResult<()> {
        let pool = test_pool().await?;
        let expires_at = (Utc::now() + Duration::hours(1)).to_rfc3339();
        let saved = set_shop_override(
            &pool,
            &ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceClosed,
                expires_at: expires_at.clone(),
            },
        )
        .await?;
        assert_eq!(
            saved
                .shop_override
                .as_ref()
                .map(|value| value.expires_at.as_str()),
            Some(expires_at.as_str())
        );
        assert_eq!(
            saved.shop_override.as_ref().map(|value| value.mode),
            Some(openpiso_core::shop_hours::ShopOverrideMode::ForceClosed)
        );

        let cleared = clear_shop_override(&pool).await?;
        assert!(cleared.shop_override.is_none());
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type IN ('shop_override_set', 'shop_override_cleared')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 2);
        let actors: Vec<String> = sqlx::query_scalar(
            "SELECT actor FROM audit_log WHERE event_type IN ('shop_override_set', 'shop_override_cleared') ORDER BY created_at",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(actors, vec!["admin", "admin"]);
        Ok(())
    }

    #[tokio::test]
    async fn past_override_expiry_is_rejected_without_replacing_prior_state() -> AppResult<()> {
        let pool = test_pool().await?;
        let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
        set_shop_override(
            &pool,
            &ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceOpen,
                expires_at: future,
            },
        )
        .await?;
        let result = set_shop_override(
            &pool,
            &ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceClosed,
                expires_at: (Utc::now() - Duration::seconds(1)).to_rfc3339(),
            },
        )
        .await;
        assert!(
            matches!(result, Err(AppError::InvalidInput(message)) if message.contains("future"))
        );
        let current = shop_hours_settings(&pool).await?;
        assert_eq!(
            current.shop_override.as_ref().map(|value| value.mode),
            Some(openpiso_core::shop_hours::ShopOverrideMode::ForceOpen)
        );
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_override_set'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn every_validation_failure_leaves_the_previous_configuration_intact() -> AppResult<()> {
        let pool = test_pool().await?;
        let original = update_shop_hours(&pool, &input(open_days(480, 1_320))).await?;

        let mut duplicate = open_days(480, 1_320);
        duplicate[6].weekday = 5;
        let mut too_few = open_days(480, 1_320);
        too_few.pop();
        let mut out_of_range_weekday = open_days(480, 1_320);
        out_of_range_weekday[6].weekday = 7;
        // Each rule gets its own case, matched on its own message so a case can
        // never pass by tripping a different rule.
        let invalids = vec![
            (
                ShopHoursSettingsInput {
                    enabled: false,
                    last_call_lead_minutes: 15,
                    days: too_few,
                },
                "exactly seven day rows",
            ),
            (
                ShopHoursSettingsInput {
                    enabled: false,
                    last_call_lead_minutes: 15,
                    days: duplicate,
                },
                "Monday through Sunday exactly once",
            ),
            (
                ShopHoursSettingsInput {
                    enabled: false,
                    last_call_lead_minutes: 15,
                    days: out_of_range_weekday,
                },
                "Monday through Sunday exactly once",
            ),
            (input(open_days(1_440, 0)), "between 0 and 1439"),
            (input(open_days(0, 1_440)), "between 0 and 1439"),
            (
                input(untimed_open_period_days(Some(480))),
                "requires a close minute",
            ),
            (
                input(untimed_open_period_days(None)),
                "requires an open minute",
            ),
            (
                ShopHoursSettingsInput {
                    enabled: false,
                    last_call_lead_minutes: -1,
                    days: open_days(480, 1_320),
                },
                "cannot be negative",
            ),
            (
                ShopHoursSettingsInput {
                    enabled: false,
                    last_call_lead_minutes: MAX_LAST_CALL_LEAD_MINUTES + 1,
                    days: open_days(480, 1_320),
                },
                "too large",
            ),
            (
                ShopHoursSettingsInput {
                    enabled: false,
                    last_call_lead_minutes: 61,
                    days: open_days(480, 540),
                },
                "shortest Open Period",
            ),
            (
                input(timed_mode_days(
                    ShopHoursDayMode::ClosedAllDay,
                    0,
                    Some(480),
                    Some(1_320),
                )),
                "cannot carry times",
            ),
            (
                input(timed_mode_days(
                    ShopHoursDayMode::Open24Hours,
                    3,
                    None,
                    Some(1_320),
                )),
                "cannot carry times",
            ),
        ];

        for (invalid, expected) in invalids {
            let result = update_shop_hours(&pool, &invalid).await;
            assert!(
                matches!(&result, Err(AppError::InvalidInput(message)) if message.contains(expected)),
                "expected a rejection mentioning {expected:?}, got {result:?}"
            );
            assert_eq!(shop_hours_settings(&pool).await?, original);
        }
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_hours_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 1);
        Ok(())
    }
}

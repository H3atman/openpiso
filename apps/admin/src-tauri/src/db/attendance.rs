use super::{begin_immediate, shop_hours};
use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
};
use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, Offset, SecondsFormat, Utc,
};
use openpiso_core::{
    attendance::{
        self, AttendanceEffectiveBonus, AttendanceSettingsDraft, OverrideHistory, ScheduleVersion,
        ValidatedAttendanceSettings,
    },
    audit::{AuditActor, AuditOutcome},
    shop_hours::{DaySchedule, ShopHoursSchedule},
};
use serde::Serialize;
use serde_json::json;
use sqlx::{Sqlite, SqlitePool, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwardSnapshot {
    pub configuration_version: i64,
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
}

#[derive(Debug, Clone, Copy)]
struct SettingsSnapshot {
    enabled: bool,
    award: AwardSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceSettingsVersion {
    pub version: i64,
    pub enabled: bool,
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
    pub claim_window_months: i64,
    pub effective_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceSettingsEconomics {
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
    pub rate_amount_cents: i64,
    pub rate_seconds: i64,
    pub effective_bonus: AttendanceEffectiveBonus,
    pub unsafe_bonus: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceSettingsPreview {
    pub enabled: bool,
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
    pub claim_window_months: u32,
    pub economics_advisory: Option<AttendanceSettingsEconomics>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttendanceSettingsUpdateOutcome {
    Updated,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct AttendanceSettingsUpdate {
    pub outcome: AttendanceSettingsUpdateOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    AcceptedCoin,
    RejectionCredit,
}

impl SourceKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AcceptedCoin => "coin_accepted",
            Self::RejectionCredit => "rejection_credit",
        }
    }
}

pub fn host_offset() -> FixedOffset {
    chrono::Local::now().offset().fix()
}

#[derive(Debug, Clone, Copy)]
pub struct AttendanceClock {
    pub utc_floor: DateTime<Utc>,
    pub open_local_date: NaiveDate,
}

fn instant(value: &str) -> AppResult<DateTime<Utc>> {
    parse_stored_instant(value)
}

pub(super) fn validate_month_key(value: &str) -> AppResult<NaiveDate> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 7
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..].iter().all(u8::is_ascii_digit);
    valid_shape
        .then(|| NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d").ok())
        .flatten()
        .filter(|date| date.year() > 0 && date.format("%Y-%m").to_string() == value)
        .ok_or_else(|| AppError::InvalidInput("earning_month must be canonical YYYY-MM".to_owned()))
}

pub(super) fn validate_date(value: &str, field: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|date| date.format("%Y-%m-%d").to_string() == value)
        .ok_or_else(|| AppError::InvalidInput(format!("{field} must be a valid YYYY-MM-DD date")))
}

pub(super) fn parse_stored_date(value: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|date| date.to_string() == value)
        .ok_or_else(|| AppError::Internal("invalid stored Attendance date".to_owned()))
}

pub(super) fn parse_stored_instant(value: &str) -> AppResult<DateTime<Utc>> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(value) {
        return Ok(instant.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .map(|instant| instant.and_utc())
        .map_err(|_| AppError::Internal("invalid stored Attendance timestamp".to_owned()))
}

/// Avoid a write transaction on the one-second timer unless a local date crossed.
pub async fn finalize_calendar_dates(
    pool: &SqlitePool,
    host_utc: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<AttendanceClock> {
    let (stored_floor, stored_open): (String, String) = sqlx::query_as(
        "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    let floor = instant(&stored_floor)?;
    let old_date = NaiveDate::parse_from_str(&stored_open, "%Y-%m-%d")
        .map_err(|e| AppError::Internal(format!("invalid stored Attendance date: {e}")))?;
    let utc_floor = attendance::authoritative_instant(host_utc, floor);
    let open_local_date = attendance::authoritative_local_date(utc_floor, offset, old_date);
    if open_local_date == old_date {
        return Ok(AttendanceClock {
            utc_floor: floor,
            open_local_date,
        });
    }

    let mut tx = begin_immediate(pool).await?;
    let clock = finalize_in_transaction(&mut tx, host_utc, offset).await?;
    tx.commit().await?;
    Ok(clock)
}

/// Finalize inside a caller-owned transaction. The coin and Rejection Credit
/// paths call this with the transaction they already hold so the earning
/// instant, the frozen date and every Attendance write stay in one commit
/// (ADR-0084). Cheap when the host clock has not crossed a new local date.
pub async fn finalize_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    host_utc: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<AttendanceClock> {
    let (stored_floor, stored_open): (String, String) = sqlx::query_as(
        "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
    )
    .fetch_one(&mut **tx)
    .await?;
    let floor = instant(&stored_floor)?;
    let old_date = NaiveDate::parse_from_str(&stored_open, "%Y-%m-%d")
        .map_err(|e| AppError::Internal(format!("invalid stored Attendance date: {e}")))?;
    let utc_floor = attendance::authoritative_instant(host_utc, floor);
    let open_local_date = attendance::authoritative_local_date(utc_floor, offset, old_date);

    if open_local_date > old_date {
        let date_jump_days = (open_local_date - old_date).num_days();
        if date_jump_days > 1 {
            tracing::warn!(
                previous_open_local_date = %old_date,
                new_open_local_date = %open_local_date,
                date_jump_days,
                "Attendance clock detected a forward discontinuity"
            );
        }
        let (versions, overrides) = shop_evidence(tx).await?;
        let mut date = old_date;
        while date < open_local_date {
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT earning_date FROM attendance_calendar_dates WHERE earning_date = ?",
            )
            .bind(date.to_string())
            .fetch_optional(&mut **tx)
            .await?;
            if existing.is_none() {
                sqlx::query("INSERT INTO attendance_calendar_dates (earning_date, finalized_at, restart_observed_utc_offset) VALUES (?, ?, ?)")
                    .bind(date.to_string()).bind(utc_floor.to_rfc3339_opts(SecondsFormat::Millis, true)).bind(offset.local_minus_utc())
                    .execute(&mut **tx).await?;
                for interval in attendance::eligible_intervals(date, offset, &versions, &overrides)
                {
                    sqlx::query("INSERT INTO attendance_eligible_intervals (earning_date, starts_at, ends_at) VALUES (?, ?, ?)")
                        .bind(date.to_string()).bind(interval.start.to_rfc3339_opts(SecondsFormat::Millis, true)).bind(interval.end.to_rfc3339_opts(SecondsFormat::Millis, true))
                        .execute(&mut **tx).await?;
                }
            }
            date = date
                .succ_opt()
                .ok_or_else(|| AppError::Internal("Attendance date overflow".to_owned()))?;
        }
    }

    sqlx::query(
        "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = ? WHERE id = 1",
    )
    .bind(utc_floor.to_rfc3339_opts(SecondsFormat::Millis, true))
    .bind(open_local_date.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(AttendanceClock {
        utc_floor,
        open_local_date,
    })
}

/// What one Attendance source admits into a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceQualification {
    /// Local earning date the server fixes for this source (calendar seam).
    pub earning_date: NaiveDate,
    pub award: AwardSnapshot,
}

/// Resolve whether an accepted coin earns: Attendance must be enabled, the
/// Account live at commit (the caller holds the transaction that wrote the
/// source event), and the server-fixed local date must overlap effective Shop
/// Open. The coin itself qualifies at commit time even while the shop is
/// currently closed; a clock regression maps it onto the last open date.
/// Returns `None` when Attendance is disabled or the date is ineligible.
pub async fn qualify_source(
    tx: &mut Transaction<'_, Sqlite>,
    source_at: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<Option<SourceQualification>> {
    let settings = current_settings_in_transaction(tx).await?;
    if !settings.enabled {
        return Ok(None);
    }
    let clock = finalize_in_transaction(tx, source_at, offset).await?;
    let earning_date =
        attendance::authoritative_local_date(clock.utc_floor, offset, clock.open_local_date);
    if !shop_open_overlaps_date(tx, earning_date, offset).await? {
        return Ok(None);
    }
    Ok(Some(SourceQualification {
        earning_date,
        award: settings.award,
    }))
}

/// Resolve an Account Rejection Credit against its original rejection instant.
/// The returned snapshot and local date are shared with the earning writer so
/// callers do not repeat configuration or calendar resolution.
pub async fn qualify_rejection(
    tx: &mut Transaction<'_, Sqlite>,
    original_rejection_at: DateTime<Utc>,
    host_utc: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<Option<SourceQualification>> {
    let Some(settings) = settings_at(tx, original_rejection_at).await? else {
        return Ok(None);
    };
    if !settings.enabled {
        return Ok(None);
    }
    finalize_in_transaction(tx, host_utc, offset).await?;
    let earning_date = original_rejection_at.with_timezone(&offset).date_naive();
    if !shop_open_at(tx, earning_date, original_rejection_at, offset).await? {
        return Ok(None);
    }
    Ok(Some(SourceQualification {
        earning_date,
        award: settings.award,
    }))
}

/// Whether `date` overlaps any effective Shop Open interval. A live date uses
/// the same immutable evidence the finalizer will freeze.
async fn shop_open_overlaps_date(
    tx: &mut Transaction<'_, Sqlite>,
    date: NaiveDate,
    offset: FixedOffset,
) -> AppResult<bool> {
    let date_key = date.to_string();
    let finalized: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM attendance_calendar_dates WHERE earning_date = ?")
            .bind(&date_key)
            .fetch_optional(&mut **tx)
            .await?;
    if finalized.is_some() {
        let open: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM attendance_eligible_intervals WHERE earning_date = ? LIMIT 1",
        )
        .bind(&date_key)
        .fetch_optional(&mut **tx)
        .await?;
        return Ok(open.is_some());
    }
    let (versions, overrides) = shop_evidence(tx).await?;
    Ok(!attendance::eligible_intervals(date, offset, &versions, &overrides).is_empty())
}

/// Whether the instant falls inside effective Shop Open on `date`. Completed
/// dates answer from their frozen intervals; the live (not yet finalized)
/// date resolves from the same immutable evidence the finalization will
/// freeze (ADR-0084).
async fn shop_open_at(
    tx: &mut Transaction<'_, Sqlite>,
    date: NaiveDate,
    at: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<bool> {
    let date_key = date.to_string();
    let at_key = at.to_rfc3339_opts(SecondsFormat::Millis, true);
    let finalized: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM attendance_calendar_dates WHERE earning_date = ?")
            .bind(&date_key)
            .fetch_optional(&mut **tx)
            .await?;
    if finalized.is_some() {
        let open: Option<i64> = sqlx::query_scalar(
            "SELECT 1
             FROM attendance_eligible_intervals
             WHERE earning_date = ?
               AND starts_at <= ? AND ends_at > ?",
        )
        .bind(&date_key)
        .bind(&at_key)
        .bind(&at_key)
        .fetch_optional(&mut **tx)
        .await?;
        return Ok(open.is_some());
    }
    // Not finalized yet: resolve live. The evidence is immutable, so this is
    // exactly the interval set the date's finalization will freeze.
    let (versions, overrides) = shop_evidence(tx).await?;
    Ok(
        attendance::eligible_intervals(date, offset, &versions, &overrides)
            .iter()
            .any(|interval| interval.start <= at && interval.end > at),
    )
}

/// The immutable Shop Hours and Override evidence in effective-time order, for
/// resolving eligibility at any instant.
async fn shop_evidence(
    tx: &mut Transaction<'_, Sqlite>,
) -> AppResult<(Vec<ScheduleVersion>, Vec<OverrideHistory>)> {
    let raw: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "SELECT id, enabled, last_call_lead_minutes, effective_at
         FROM shop_hours_versions ORDER BY effective_at, id",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut versions = Vec::new();
    for (id, enabled, lead, effective_at) in raw {
        let rows: Vec<(i64, String, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT weekday, mode, open_minute, close_minute
             FROM shop_hours_version_days
             WHERE shop_hours_version_id = ? ORDER BY weekday",
        )
        .bind(id)
        .fetch_all(&mut **tx)
        .await?;
        if rows.len() != 7 {
            return Err(AppError::InvalidInput(
                "incomplete Shop Hours version".to_owned(),
            ));
        }
        let mut days = [DaySchedule::ClosedAllDay; 7];
        for (weekday, mode, start, end) in rows {
            let index = usize::try_from(weekday)
                .map_err(|_| AppError::InvalidInput("invalid weekday".to_owned()))?;
            if index >= days.len() {
                return Err(AppError::InvalidInput("invalid weekday".to_owned()));
            }
            days[index] = shop_hours::day_schedule_from_storage(&mode, start, end)?;
        }
        versions.push(ScheduleVersion {
            effective_at: instant(&effective_at)?,
            schedule: ShopHoursSchedule {
                enabled: enabled == 1,
                last_call_lead_minutes: u16::try_from(lead)
                    .map_err(|_| AppError::InvalidInput("invalid lead".to_owned()))?,
                days,
            },
        });
    }
    let history: Vec<(Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT mode, effective_at, expires_at
         FROM shop_override_history ORDER BY effective_at, id",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut overrides: Vec<OverrideHistory> = Vec::new();
    for (mode, effective, expiry) in history {
        let effective_at = instant(&effective)?;
        if let Some(previous) = overrides.last_mut() {
            previous.expires_at = previous
                .expires_at
                .map(|expires_at| expires_at.min(effective_at));
        }
        let mode = mode
            .as_deref()
            .map(shop_hours::override_mode_from_storage)
            .transpose()?;
        let expires_at = expiry.as_deref().map(instant).transpose()?;
        if mode.is_some() != expires_at.is_some() {
            return Err(AppError::InvalidInput(
                "stored Shop Override history is incomplete".to_owned(),
            ));
        }
        overrides.push(OverrideHistory {
            effective_at,
            expires_at,
            mode,
        });
    }
    Ok((versions, overrides))
}

pub async fn preview_settings(
    pool: &SqlitePool,
    draft: AttendanceSettingsDraft,
) -> AppResult<AttendanceSettingsPreview> {
    let validated = validate_settings_draft(draft)?;
    let economics_advisory = economics_advisory_for_values(
        pool,
        validated.daily_threshold_cents,
        validated.reward_seconds,
    )
    .await?;
    Ok(AttendanceSettingsPreview {
        enabled: validated.enabled,
        daily_threshold_cents: validated.daily_threshold_cents,
        reward_seconds: validated.reward_seconds,
        claim_window_months: validated.claim_window_months,
        economics_advisory,
    })
}

pub async fn update_settings(
    pool: &SqlitePool,
    draft: AttendanceSettingsDraft,
    expected_version: i64,
) -> AppResult<AttendanceSettingsUpdate> {
    let validated = validate_settings_draft(draft)?;
    if expected_version < 1 {
        return Err(AppError::InvalidInput(
            "expected_version must be positive".to_owned(),
        ));
    }

    let mut tx = begin_immediate(pool).await?;
    let current = current_settings_version_in_transaction(&mut tx).await?;
    if settings_match(&current, &validated) {
        tx.commit().await?;
        return Ok(AttendanceSettingsUpdate {
            outcome: AttendanceSettingsUpdateOutcome::Unchanged,
        });
    }
    if expected_version != current.version {
        return Err(AppError::AttendanceSettingsConflict {
            current_version: current.version,
        });
    }

    let version = current
        .version
        .checked_add(1)
        .ok_or_else(|| AppError::Conflict("Attendance settings version overflow".to_owned()))?;
    let effective_at = Utc::now();
    let mut event = NewAuditEvent::simple(
        "attendance_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Attendance settings",
    );
    event.metadata = json!({
        "before": {
            "version": current.version,
            "enabled": current.enabled,
            "daily_threshold_cents": current.daily_threshold_cents,
            "reward_seconds": current.reward_seconds,
            "claim_window_months": current.claim_window_months,
        },
        "after": {
            "version": version,
            "enabled": validated.enabled,
            "daily_threshold_cents": validated.daily_threshold_cents,
            "reward_seconds": validated.reward_seconds,
            "claim_window_months": validated.claim_window_months,
        },
    });
    let audit_event_id = audit::append(&mut tx, event).await?;
    sqlx::query(
        "INSERT INTO attendance_settings_versions
           (version, enabled, daily_threshold_cents, reward_seconds,
            claim_window_months, effective_at, audit_event_id)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(version)
    .bind(i64::from(validated.enabled))
    .bind(validated.daily_threshold_cents)
    .bind(validated.reward_seconds)
    .bind(i64::from(validated.claim_window_months))
    .bind(effective_at.to_rfc3339_opts(SecondsFormat::Millis, true))
    .bind(audit_event_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(AttendanceSettingsUpdate {
        outcome: AttendanceSettingsUpdateOutcome::Updated,
    })
}

pub async fn economics_advisory_for_values(
    pool: &SqlitePool,
    daily_threshold_cents: i64,
    reward_seconds: i64,
) -> AppResult<Option<AttendanceSettingsEconomics>> {
    let (rate_amount_cents, rate_seconds) = match super::rates::active_rate(pool).await {
        Ok(rate) => rate,
        Err(AppError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let effective_bonus = attendance::attendance_effective_bonus(
        daily_threshold_cents,
        reward_seconds,
        rate_amount_cents,
        rate_seconds,
    )
    .map_err(AppError::Pricing)?;
    Ok(Some(AttendanceSettingsEconomics {
        daily_threshold_cents,
        reward_seconds,
        rate_amount_cents,
        rate_seconds,
        unsafe_bonus: effective_bonus.is_unsafe(),
        effective_bonus,
    }))
}

fn validate_settings_draft(
    draft: AttendanceSettingsDraft,
) -> AppResult<ValidatedAttendanceSettings> {
    attendance::validate_attendance_settings(draft)
        .map_err(|error| AppError::InvalidInput(error.to_string()))
}

fn settings_match(current: &AttendanceSettingsVersion, next: &ValidatedAttendanceSettings) -> bool {
    current.enabled == next.enabled
        && current.daily_threshold_cents == next.daily_threshold_cents
        && current.reward_seconds == next.reward_seconds
        && current.claim_window_months == i64::from(next.claim_window_months)
}

pub(super) async fn current_settings_version_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
) -> AppResult<AttendanceSettingsVersion> {
    let raw: (i64, i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT version, enabled, daily_threshold_cents, reward_seconds,
                claim_window_months, effective_at
         FROM attendance_settings_versions ORDER BY version DESC LIMIT 1",
    )
    .fetch_one(&mut **tx)
    .await?;
    settings_version(raw)
}

fn settings_version(
    (version, enabled, daily_threshold_cents, reward_seconds, claim_window_months, effective_at): (
        i64,
        i64,
        i64,
        i64,
        i64,
        String,
    ),
) -> AppResult<AttendanceSettingsVersion> {
    Ok(AttendanceSettingsVersion {
        version,
        enabled: enabled == 1,
        daily_threshold_cents,
        reward_seconds,
        claim_window_months,
        effective_at: instant(&effective_at)?,
    })
}

async fn current_settings_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
) -> AppResult<SettingsSnapshot> {
    let (version, enabled, daily_threshold_cents, reward_seconds): (i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT version, enabled, daily_threshold_cents, reward_seconds
             FROM attendance_settings_versions ORDER BY version DESC LIMIT 1",
        )
        .fetch_one(&mut **tx)
        .await?;
    Ok(SettingsSnapshot {
        enabled: enabled == 1,
        award: AwardSnapshot {
            configuration_version: version,
            daily_threshold_cents,
            reward_seconds,
        },
    })
}

/// Whether the latest committed Attendance settings enable earning.
pub async fn enabled_in_transaction(tx: &mut Transaction<'_, Sqlite>) -> AppResult<bool> {
    Ok(current_settings_in_transaction(tx).await?.enabled)
}

/// Settings whose `effective_at` covers `at`, newest version first.
async fn settings_at(
    tx: &mut Transaction<'_, Sqlite>,
    at: DateTime<Utc>,
) -> AppResult<Option<SettingsSnapshot>> {
    let settings: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT version, enabled, daily_threshold_cents, reward_seconds
         FROM attendance_settings_versions
         WHERE effective_at <= ? ORDER BY version DESC LIMIT 1",
    )
    .bind(at.to_rfc3339_opts(SecondsFormat::Millis, true))
    .fetch_optional(&mut **tx)
    .await?;
    Ok(
        settings.map(|(version, enabled, threshold, reward)| SettingsSnapshot {
            enabled: enabled == 1,
            award: AwardSnapshot {
                configuration_version: version,
                daily_threshold_cents: threshold,
                reward_seconds: reward,
            },
        }),
    )
}

/// The settings version whose `effective_at` covers `at`, newest first.
pub(super) async fn settings_version_at(
    tx: &mut Transaction<'_, Sqlite>,
    at: DateTime<Utc>,
) -> AppResult<Option<i64>> {
    Ok(settings_at(tx, at)
        .await?
        .map(|settings| settings.award.configuration_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    #[tokio::test]
    async fn backward_clock_and_offset_change_never_reopen_frozen_dates() -> AppResult<()> {
        let (pool, _) = crate::test_utils::wal_test_pool("attendance-backward").await;
        let start = Utc.with_ymd_and_hms(2026, 1, 10, 23, 0, 0).unwrap();
        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = '2026-01-11'",
        )
        .bind(start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .execute(&pool)
        .await?;
        let east = FixedOffset::east_opt(3600).unwrap();
        let first = finalize_calendar_dates(&pool, start + Duration::hours(25), east).await?;
        assert_eq!(first.open_local_date.to_string(), "2026-01-12");
        let before = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_calendar_dates")
            .fetch_one(&pool)
            .await?;
        let west = FixedOffset::west_opt(12 * 3600).unwrap();
        let regressed = finalize_calendar_dates(&pool, start - Duration::days(2), west).await?;
        assert_eq!(regressed.open_local_date, first.open_local_date);
        assert_eq!(regressed.utc_floor, first.utc_floor);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_calendar_dates")
                .fetch_one(&pool)
                .await?,
            before
        );
        finalize_calendar_dates(&pool, start + Duration::days(4), west).await?;
        let offsets: Vec<i64> = sqlx::query_scalar(
            "SELECT restart_observed_utc_offset FROM attendance_calendar_dates ORDER BY earning_date",
        ).fetch_all(&pool).await?;
        assert_eq!(offsets, vec![3600, -12 * 3600, -12 * 3600]);
        Ok(())
    }

    #[tokio::test]
    async fn timezone_change_does_not_recalculate_frozen_eligibility() -> AppResult<()> {
        let (pool, _) = crate::test_utils::wal_test_pool("attendance-frozen-timezone").await;
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 11).unwrap();
        let next_date = date.succ_opt().unwrap();
        let east = FixedOffset::east_opt(8 * 3600).unwrap();
        let west = FixedOffset::west_opt(12 * 3600).unwrap();
        let local_midnight = |date: chrono::NaiveDate| {
            east.from_local_datetime(&date.and_time(chrono::NaiveTime::MIN))
                .single()
                .unwrap()
                .to_utc()
        };
        let start = local_midnight(date);
        let next_start = local_midnight(next_date);
        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .bind(date.to_string())
        .execute(&pool)
        .await?;
        let version_id: i64 = sqlx::query_scalar(
            "INSERT INTO shop_hours_versions (enabled, last_call_lead_minutes, effective_at)
             VALUES (1, 0, ?) RETURNING id",
        )
        .bind(start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .fetch_one(&pool)
        .await?;
        for weekday in 0..7 {
            sqlx::query(
                "INSERT INTO shop_hours_version_days
                   (shop_hours_version_id, weekday, mode, open_minute, close_minute)
                 VALUES (?, ?, 'open_24_hours', NULL, NULL)",
            )
            .bind(version_id)
            .bind(weekday)
            .execute(&pool)
            .await?;
        }

        finalize_calendar_dates(&pool, next_start, east).await?;
        let frozen_before: Vec<(String, String)> = sqlx::query_as(
            "SELECT starts_at, ends_at FROM attendance_eligible_intervals
             WHERE earning_date = ? ORDER BY starts_at",
        )
        .bind(date.to_string())
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            frozen_before,
            vec![(
                start.to_rfc3339_opts(SecondsFormat::Millis, true),
                next_start.to_rfc3339_opts(SecondsFormat::Millis, true)
            )]
        );

        finalize_calendar_dates(&pool, next_start - Duration::days(2), west).await?;
        finalize_calendar_dates(&pool, next_start + Duration::days(2), west).await?;
        let frozen_after: Vec<(String, String)> = sqlx::query_as(
            "SELECT starts_at, ends_at FROM attendance_eligible_intervals
             WHERE earning_date = ? ORDER BY starts_at",
        )
        .bind(date.to_string())
        .fetch_all(&pool)
        .await?;
        let observed_offset: i64 = sqlx::query_scalar(
            "SELECT restart_observed_utc_offset FROM attendance_calendar_dates
             WHERE earning_date = ?",
        )
        .bind(date.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(frozen_after, frozen_before);
        assert_eq!(observed_offset, east.local_minus_utc() as i64);
        Ok(())
    }

    #[tokio::test]
    async fn forward_jump_finalizes_skipped_dates_once() -> AppResult<()> {
        let (pool, _) = crate::test_utils::wal_test_pool("attendance-forward").await;
        let start = Utc.with_ymd_and_hms(2026, 1, 10, 12, 0, 0).unwrap();
        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = '2026-01-10'",
        )
        .bind(start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .execute(&pool)
        .await?;
        let offset = FixedOffset::east_opt(0).unwrap();
        let forward = start + Duration::days(4);
        finalize_calendar_dates(&pool, forward, offset).await?;
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_calendar_dates WHERE earning_date BETWEEN '2026-01-10' AND '2026-01-13'")
            .fetch_one(&pool).await?, 4);
        finalize_calendar_dates(&pool, forward, offset).await?;
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_calendar_dates WHERE earning_date BETWEEN '2026-01-10' AND '2026-01-13'")
            .fetch_one(&pool).await?, 4);
        Ok(())
    }

    #[tokio::test]
    async fn finalization_freezes_shop_hours_and_override_history() -> AppResult<()> {
        let pool = crate::test_utils::mem_test_pool().await;
        let stamp = |day, hour| {
            Utc.with_ymd_and_hms(2026, 1, day, hour, 0, 0)
                .unwrap()
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        };
        async fn append_monday_schedule(
            pool: &SqlitePool,
            effective_at: String,
            monday: (&str, Option<i64>, Option<i64>),
        ) -> AppResult<()> {
            let version_id: i64 = sqlx::query_scalar(
                "INSERT INTO shop_hours_versions (enabled, last_call_lead_minutes, effective_at)
                 VALUES (1, 0, ?) RETURNING id",
            )
            .bind(effective_at)
            .fetch_one(pool)
            .await?;
            for weekday in 0..7 {
                let day = if weekday == 0 {
                    monday
                } else {
                    ("closed_all_day", None, None)
                };
                sqlx::query(
                    "INSERT INTO shop_hours_version_days
                       (shop_hours_version_id, weekday, mode, open_minute, close_minute)
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(version_id)
                .bind(weekday)
                .bind(day.0)
                .bind(day.1)
                .bind(day.2)
                .execute(pool)
                .await?;
            }
            Ok(())
        }

        append_monday_schedule(&pool, stamp(5, 0), ("open_period", Some(480), Some(1_080))).await?;
        append_monday_schedule(&pool, stamp(5, 12), ("closed_all_day", None, None)).await?;
        append_monday_schedule(&pool, stamp(5, 18), ("open_period", Some(1_320), Some(120)))
            .await?;
        for (mode, effective_at, expires_at) in [
            (Some("force_closed"), stamp(5, 10), Some(stamp(5, 12))),
            (None, stamp(5, 11), None),
            (Some("force_open"), stamp(5, 20), Some(stamp(5, 21))),
        ] {
            sqlx::query(
                "INSERT INTO shop_override_history (mode, effective_at, expires_at)
                 VALUES (?, ?, ?)",
            )
            .bind(mode)
            .bind(effective_at)
            .bind(expires_at)
            .execute(&pool)
            .await?;
        }

        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = '2026-01-04' WHERE id = 1",
        )
        .bind(stamp(4, 0))
        .execute(&pool)
        .await?;
        let finalized_at = Utc.with_ymd_and_hms(2026, 1, 9, 12, 0, 0).unwrap();
        finalize_calendar_dates(&pool, finalized_at, FixedOffset::east_opt(0).unwrap()).await?;

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT earning_date, starts_at, ends_at FROM attendance_eligible_intervals
             WHERE earning_date IN ('2026-01-05', '2026-01-06')
             ORDER BY earning_date, starts_at",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            rows,
            vec![
                ("2026-01-05".to_owned(), stamp(5, 8), stamp(5, 10),),
                ("2026-01-05".to_owned(), stamp(5, 11), stamp(5, 12),),
                ("2026-01-05".to_owned(), stamp(5, 20), stamp(5, 21),),
                ("2026-01-05".to_owned(), stamp(5, 22), stamp(6, 0),),
                ("2026-01-06".to_owned(), stamp(6, 0), stamp(6, 2),),
            ]
        );
        Ok(())
    }
}

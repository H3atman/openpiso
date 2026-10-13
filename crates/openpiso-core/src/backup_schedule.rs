use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MINUTES_PER_DAY: i64 = 1_440;
pub const MIN_INTERVAL_SECONDS: i64 = 60;
pub const MAX_INTERVAL_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const MAX_RETENTION_COUNT: i64 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupFrequency {
    Daily,
    Weekly,
    Monthly,
    #[cfg(debug_assertions)]
    Interval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ScheduleValidationError {
    #[error("Backup frequency is invalid")]
    Frequency,
    #[error("Backup weekday must be between 0 and 6")]
    Weekday,
    #[error("Backup day of month must be between 1 and 31")]
    DayOfMonth,
    #[error("Backup minute of day must be between 0 and 1439")]
    MinuteOfDay,
    #[error("Backup retention count must be between 1 and 365")]
    RetentionCount,
    #[error("Backup interval must be between 60 seconds and 30 days")]
    Interval,
}

/// Validate the scalar fields at the pure domain boundary. Each invalid field
/// has a distinct error so the owner receives an actionable message.
pub fn validate_fields(
    frequency: Option<BackupFrequency>,
    weekday: i64,
    day_of_month: i64,
    minute_of_day: i64,
    retention_count: i64,
    interval_seconds: Option<i64>,
) -> Result<(), ScheduleValidationError> {
    #[cfg(not(debug_assertions))]
    let _ = interval_seconds;

    if frequency.is_none() {
        return Err(ScheduleValidationError::Frequency);
    }
    if !(0..=6).contains(&weekday) {
        return Err(ScheduleValidationError::Weekday);
    }
    if !(1..=31).contains(&day_of_month) {
        return Err(ScheduleValidationError::DayOfMonth);
    }
    if !(0..MINUTES_PER_DAY).contains(&minute_of_day) {
        return Err(ScheduleValidationError::MinuteOfDay);
    }
    if !(1..=MAX_RETENTION_COUNT).contains(&retention_count) {
        return Err(ScheduleValidationError::RetentionCount);
    }
    #[cfg(debug_assertions)]
    if frequency == Some(BackupFrequency::Interval)
        && !interval_seconds
            .is_some_and(|seconds| (MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&seconds))
    {
        return Err(ScheduleValidationError::Interval);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupSchedule {
    pub enabled: bool,
    pub frequency: BackupFrequency,
    pub minute_of_day: u16,
    /// Monday = 0 through Sunday = 6.
    pub weekday: u8,
    pub day_of_month: u8,
    #[cfg(debug_assertions)]
    pub interval_seconds: u32,
}

fn local_datetime(
    now: DateTime<FixedOffset>,
    date: NaiveDate,
    minute: u16,
) -> DateTime<FixedOffset> {
    let naive = NaiveDateTime::new(date, NaiveTime::MIN) + Duration::minutes(i64::from(minute));
    now.offset()
        .from_local_datetime(&naive)
        .single()
        .unwrap_or(now)
}

fn date_for_weekday(date: NaiveDate, weekday: u8) -> NaiveDate {
    let current = date.weekday().num_days_from_monday() as i64;
    date + Duration::days(i64::from(weekday) - current)
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|date| date.pred_opt())
        .map_or(28, |date| date.day())
}

fn monthly_date(year: i32, month: u32, day: u8) -> NaiveDate {
    let clamped = u32::from(day).min(last_day_of_month(year, month));
    NaiveDate::from_ymd_opt(year, month, clamped)
        .or_else(|| NaiveDate::from_ymd_opt(year, month, 1))
        .unwrap_or(NaiveDate::MIN)
}

/// Return the first scheduled occurrence strictly after `reference`.
///
/// Using a reference instant rather than an ambient clock makes this suitable
/// for both the scheduler and deterministic tests. A disabled schedule has no
/// due time.
pub fn next_due(
    schedule: BackupSchedule,
    reference: DateTime<FixedOffset>,
) -> Option<DateTime<FixedOffset>> {
    if !schedule.enabled {
        return None;
    }

    #[cfg(debug_assertions)]
    if schedule.frequency == BackupFrequency::Interval {
        return Some(reference + Duration::seconds(i64::from(schedule.interval_seconds)));
    }

    let date = reference.date_naive();
    let candidate = match schedule.frequency {
        BackupFrequency::Daily => local_datetime(reference, date, schedule.minute_of_day),
        BackupFrequency::Weekly => {
            let target = date_for_weekday(date, schedule.weekday);
            local_datetime(reference, target, schedule.minute_of_day)
        }
        BackupFrequency::Monthly => local_datetime(
            reference,
            monthly_date(date.year(), date.month(), schedule.day_of_month),
            schedule.minute_of_day,
        ),
        #[cfg(debug_assertions)]
        BackupFrequency::Interval => unreachable!(),
    };

    if candidate > reference {
        return Some(candidate);
    }

    match schedule.frequency {
        BackupFrequency::Daily => Some(local_datetime(
            reference,
            date + Duration::days(1),
            schedule.minute_of_day,
        )),
        BackupFrequency::Weekly => Some(local_datetime(
            reference,
            date_for_weekday(date, schedule.weekday) + Duration::days(7),
            schedule.minute_of_day,
        )),
        BackupFrequency::Monthly => {
            let (year, month) = if date.month() == 12 {
                (date.year() + 1, 1)
            } else {
                (date.year(), date.month() + 1)
            };
            Some(local_datetime(
                reference,
                monthly_date(year, month, schedule.day_of_month),
                schedule.minute_of_day,
            ))
        }
        #[cfg(debug_assertions)]
        BackupFrequency::Interval => unreachable!(),
    }
}

/// Return the most recent scheduled occurrence at or before `reference`.
///
/// This is used only to initialize a newly-added persisted due-time column;
/// normal scheduling starts from the stored instant.
pub fn latest_due(
    schedule: BackupSchedule,
    reference: DateTime<FixedOffset>,
) -> Option<DateTime<FixedOffset>> {
    if !schedule.enabled {
        return None;
    }

    #[cfg(debug_assertions)]
    if schedule.frequency == BackupFrequency::Interval {
        return Some(reference);
    }

    let date = reference.date_naive();
    let candidate = match schedule.frequency {
        BackupFrequency::Daily => local_datetime(reference, date, schedule.minute_of_day),
        BackupFrequency::Weekly => local_datetime(
            reference,
            date_for_weekday(date, schedule.weekday),
            schedule.minute_of_day,
        ),
        BackupFrequency::Monthly => local_datetime(
            reference,
            monthly_date(date.year(), date.month(), schedule.day_of_month),
            schedule.minute_of_day,
        ),
        #[cfg(debug_assertions)]
        BackupFrequency::Interval => unreachable!(),
    };
    if candidate <= reference {
        return Some(candidate);
    }

    match schedule.frequency {
        BackupFrequency::Daily => Some(local_datetime(
            reference,
            date - Duration::days(1),
            schedule.minute_of_day,
        )),
        BackupFrequency::Weekly => Some(local_datetime(
            reference,
            date_for_weekday(date, schedule.weekday) - Duration::days(7),
            schedule.minute_of_day,
        )),
        BackupFrequency::Monthly => {
            let (year, month) = if date.month() == 1 {
                (date.year() - 1, 12)
            } else {
                (date.year(), date.month() - 1)
            };
            Some(local_datetime(
                reference,
                monthly_date(year, month, schedule.day_of_month),
                schedule.minute_of_day,
            ))
        }
        #[cfg(debug_assertions)]
        BackupFrequency::Interval => unreachable!(),
    }
}

/// Collapse any number of missed occurrences to the next future occurrence.
/// A future candidate is preserved; an occurrence at or before `reference` is
/// never replayed one-by-one.
pub fn collapse_missed_occurrences(
    schedule: BackupSchedule,
    candidate: DateTime<FixedOffset>,
    reference: DateTime<FixedOffset>,
) -> Option<DateTime<FixedOffset>> {
    if !schedule.enabled {
        return None;
    }
    if candidate > reference {
        Some(candidate)
    } else {
        next_due(schedule, reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(&format!("{value}+08:00")).unwrap()
    }

    fn schedule(frequency: BackupFrequency) -> BackupSchedule {
        BackupSchedule {
            enabled: true,
            frequency,
            minute_of_day: 4 * 60,
            weekday: 0,
            day_of_month: 29,
            #[cfg(debug_assertions)]
            interval_seconds: 60,
        }
    }

    #[test]
    fn next_due_resolves_daily_weekly_and_monthly() {
        assert_eq!(
            next_due(schedule(BackupFrequency::Daily), now("2026-08-03T05:00:00")),
            Some(now("2026-08-04T04:00:00"))
        );
        assert_eq!(
            next_due(
                schedule(BackupFrequency::Weekly),
                now("2026-08-03T05:00:00")
            ),
            Some(now("2026-08-10T04:00:00"))
        );
        assert_eq!(
            next_due(
                schedule(BackupFrequency::Monthly),
                now("2026-08-03T05:00:00")
            ),
            Some(now("2026-08-29T04:00:00"))
        );
    }

    #[test]
    fn an_uninitialized_schedule_can_find_the_latest_occurrence() {
        let schedule = schedule(BackupFrequency::Daily);
        assert_eq!(
            latest_due(schedule, now("2026-08-03T05:00:00")),
            Some(now("2026-08-03T04:00:00"))
        );
        assert_eq!(
            latest_due(schedule, now("2026-08-03T03:00:00")),
            Some(now("2026-08-02T04:00:00"))
        );
    }

    #[test]
    fn missed_occurrences_collapse_across_arbitrary_downtime() {
        let schedule = schedule(BackupFrequency::Daily);
        let old_due = now("2026-01-01T04:00:00");
        let reference = now("2026-08-03T05:00:00");
        assert_eq!(
            collapse_missed_occurrences(schedule, old_due, reference),
            Some(now("2026-08-04T04:00:00"))
        );
    }

    #[test]
    fn monthly_days_clamp_in_leap_and_non_leap_february() {
        for day in [29, 30, 31] {
            let mut schedule = schedule(BackupFrequency::Monthly);
            schedule.day_of_month = day;
            let leap = next_due(schedule, now("2028-02-01T00:00:00")).unwrap();
            let non_leap = next_due(schedule, now("2027-02-01T00:00:00")).unwrap();
            assert_eq!(leap.date_naive().day(), 29);
            assert_eq!(non_leap.date_naive().day(), 28);
        }
    }

    #[test]
    fn weekly_and_monthly_roll_over_boundaries() {
        let mut weekly = schedule(BackupFrequency::Weekly);
        weekly.weekday = 6;
        assert_eq!(
            next_due(weekly, now("2026-08-08T05:00:00")),
            Some(now("2026-08-09T04:00:00"))
        );

        let monthly = schedule(BackupFrequency::Monthly);
        assert_eq!(
            next_due(monthly, now("2026-12-30T05:00:00")),
            Some(now("2027-01-29T04:00:00"))
        );
    }

    #[test]
    fn each_scalar_validation_rule_has_a_distinct_error() {
        assert_eq!(
            validate_fields(None, 0, 1, 0, 1, Some(60)),
            Err(ScheduleValidationError::Frequency)
        );
        assert_eq!(
            validate_fields(Some(BackupFrequency::Daily), 7, 1, 0, 1, Some(60)),
            Err(ScheduleValidationError::Weekday)
        );
        assert_eq!(
            validate_fields(Some(BackupFrequency::Daily), 0, 0, 0, 1, Some(60)),
            Err(ScheduleValidationError::DayOfMonth)
        );
        assert_eq!(
            validate_fields(Some(BackupFrequency::Daily), 0, 1, 1_440, 1, Some(60)),
            Err(ScheduleValidationError::MinuteOfDay)
        );
        assert_eq!(
            validate_fields(Some(BackupFrequency::Daily), 0, 1, 0, 0, Some(60)),
            Err(ScheduleValidationError::RetentionCount)
        );
    }
}

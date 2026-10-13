use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use openpiso_core::attendance::{self as attendance_domain, EarningMonthBalance, Interval};
use serde::Deserialize;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::errors::{AppError, AppResult};

pub use openpiso_core::attendance_wire::{
    AttendanceClaimFact, AttendanceClaimResponse, AttendanceDay, AttendanceDayStatus,
    AttendanceMonth, AttendanceMonthSummary, AttendanceSummary, AttendanceToday,
    AttendanceTodayStatus, AttendanceTotals,
};

pub struct AttendanceAccountSnapshot {
    pub summary: AttendanceSummary,
    pub requested_month: Option<AttendanceMonth>,
    pub remaining_seconds: i64,
}

#[derive(Debug, Deserialize)]
struct StoredClaimLocalDates {
    start: String,
    end: String,
}

struct AttendanceMonthRow {
    earning_month: String,
    claim_starts_at: String,
    deadline_at_exclusive: String,
    claim_local_dates: String,
    check_in_count: i64,
    earned_seconds: i64,
    claimed_seconds: i64,
    forfeited_seconds: i64,
    claim_count: i64,
    latest_claim_at: Option<String>,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for AttendanceMonthRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            earning_month: row.try_get("earning_month")?,
            claim_starts_at: row.try_get("claim_starts_at")?,
            deadline_at_exclusive: row.try_get("deadline_at_exclusive")?,
            claim_local_dates: row.try_get("claim_local_dates")?,
            check_in_count: row.try_get("check_in_count")?,
            earned_seconds: row.try_get("earned_seconds")?,
            claimed_seconds: row.try_get("claimed_seconds")?,
            forfeited_seconds: row.try_get("forfeited_seconds")?,
            claim_count: row.try_get("claim_count")?,
            latest_claim_at: row.try_get("latest_claim_at")?,
        })
    }
}

#[derive(Debug)]
struct MonthProjection {
    summary: AttendanceMonthSummary,
    balance: EarningMonthBalance,
}

#[derive(Debug, Clone)]
struct DaySnapshot {
    capped_cents: i64,
    daily_threshold_cents: i64,
    reward_seconds: i64,
}

#[derive(Debug)]
struct SettingsEvidence {
    version: i64,
    enabled: bool,
    effective_at: DateTime<Utc>,
}

/// Read one account's Attendance projections and optional month calendar from a
/// single finalized SQLite snapshot.
pub async fn account_snapshot(
    pool: &SqlitePool,
    account_id: &str,
    requested_month: Option<&str>,
) -> AppResult<AttendanceAccountSnapshot> {
    let requested_month = requested_month
        .map(super::attendance::validate_month_key)
        .transpose()?;
    let mut tx = crate::db::begin_immediate(pool).await?;
    let host_now = Utc::now();
    let offset = super::attendance::host_offset();
    let clock = super::attendance::finalize_in_transaction(&mut tx, host_now, offset).await?;
    let as_of = clock.utc_floor;
    let admin_local_date =
        attendance_domain::authoritative_local_date(as_of, offset, clock.open_local_date);
    let admin_local_month = month_key(admin_local_date);

    let (created_at, remaining_seconds): (String, i64) = sqlx::query_as(
        "SELECT created_at, remaining_seconds FROM accounts
         WHERE id = ? AND status = 'active'",
    )
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Unauthorized("active account required".to_owned()))?;
    let account_created_at = super::attendance::parse_stored_instant(&created_at)?;
    let settings = super::attendance::current_settings_version_in_transaction(&mut tx).await?;

    let mut totals = AttendanceTotals {
        earned_seconds: 0,
        claimed_seconds: 0,
        outstanding_seconds: 0,
        claimable_seconds: 0,
        expired_seconds: 0,
        forfeited_seconds: 0,
    };
    let mut months = BTreeMap::new();
    let month_rows: Vec<AttendanceMonthRow> = sqlx::query_as(
        "SELECT m.earning_month, m.claim_starts_at,
                m.deadline_at_exclusive, m.claim_local_dates,
                COALESCE((SELECT COUNT(*) FROM attendance_check_ins ci
                          WHERE ci.earning_month_id = m.id AND ci.account_id = m.account_id), 0)
                    AS check_in_count,
                COALESCE((SELECT SUM(ci.reward_seconds) FROM attendance_check_ins ci
                          WHERE ci.earning_month_id = m.id AND ci.account_id = m.account_id), 0)
                    AS earned_seconds,
                COALESCE((SELECT SUM(c.claimed_seconds) FROM attendance_claims c
                          WHERE c.earning_month_id = m.id AND c.account_id = m.account_id), 0)
                    AS claimed_seconds,
                COALESCE((SELECT SUM(f.forfeited_seconds) FROM attendance_forfeitures f
                          WHERE f.earning_month_id = m.id AND f.account_id = m.account_id), 0)
                    AS forfeited_seconds,
                (SELECT COUNT(*) FROM attendance_claims c
                 WHERE c.earning_month_id = m.id AND c.account_id = m.account_id) AS claim_count,
                (SELECT MAX(c.created_at) FROM attendance_claims c
                 WHERE c.earning_month_id = m.id AND c.account_id = m.account_id) AS latest_claim_at
         FROM attendance_earning_months m
         WHERE m.account_id = ? ORDER BY m.earning_month DESC",
    )
    .bind(account_id)
    .fetch_all(&mut *tx)
    .await?;
    for AttendanceMonthRow {
        earning_month,
        claim_starts_at,
        deadline_at_exclusive,
        claim_local_dates,
        check_in_count,
        earned_seconds,
        claimed_seconds,
        forfeited_seconds,
        claim_count,
        latest_claim_at,
    } in month_rows
    {
        let starts_at = super::attendance::parse_stored_instant(&claim_starts_at)?;
        let deadline = super::attendance::parse_stored_instant(&deadline_at_exclusive)?;
        let local_dates: StoredClaimLocalDates = serde_json::from_str(&claim_local_dates)
            .map_err(|_| AppError::Internal("invalid stored Attendance claim dates".to_owned()))?;
        let claim_starts_on = super::attendance::parse_stored_date(&local_dates.start)?;
        let deadline_date_inclusive = super::attendance::parse_stored_date(&local_dates.end)?;
        if claim_starts_on > deadline_date_inclusive {
            return Err(AppError::Internal(
                "invalid stored Attendance claim date range".to_owned(),
            ));
        }
        let balance = attendance_domain::derive_earning_month_balance(
            as_of,
            starts_at,
            deadline,
            earned_seconds,
            claimed_seconds,
            forfeited_seconds,
        )
        .map_err(|_| {
            AppError::Conflict("Attendance Earning Month does not reconcile".to_owned())
        })?;
        add_total(&mut totals.earned_seconds, balance.earned_seconds)?;
        add_total(&mut totals.claimed_seconds, balance.claimed_seconds)?;
        add_total(&mut totals.outstanding_seconds, balance.outstanding_seconds)?;
        add_total(&mut totals.claimable_seconds, balance.claimable_seconds)?;
        add_total(&mut totals.expired_seconds, balance.expired_seconds)?;
        add_total(&mut totals.forfeited_seconds, balance.forfeited_seconds)?;
        let latest_claim_at = latest_claim_at
            .as_deref()
            .map(super::attendance::parse_stored_instant)
            .transpose()?;
        let latest_claim_on = latest_claim_at.as_ref().map(|instant| {
            instant
                .with_timezone(&offset)
                .date_naive()
                .format("%Y-%m-%d")
                .to_string()
        });
        months.insert(
            earning_month.clone(),
            MonthProjection {
                summary: AttendanceMonthSummary {
                    earning_month,
                    state: Some(balance.state),
                    check_in_count,
                    earned_seconds: balance.earned_seconds,
                    claimed_seconds: balance.claimed_seconds,
                    outstanding_seconds: balance.outstanding_seconds,
                    claimable_seconds: balance.claimable_seconds,
                    expired_seconds: balance.expired_seconds,
                    forfeited_seconds: balance.forfeited_seconds,
                    claim_starts_at: Some(starts_at),
                    deadline_at_exclusive: Some(deadline),
                    claim_starts_on: Some(claim_starts_on.to_string()),
                    deadline_date_inclusive: Some(deadline_date_inclusive.to_string()),
                    claim_count,
                    latest_claim_at,
                    latest_claim_on,
                },
                balance,
            },
        );
    }

    let calendar_month_keys = calendar_month_keys(admin_local_date)?;
    let calendar_months = calendar_month_keys
        .iter()
        .map(|key| month_summary(&months, key))
        .collect::<Vec<_>>();
    let oldest_calendar_month = calendar_month_keys
        .last()
        .ok_or_else(|| AppError::Internal("Attendance calendar is empty".to_owned()))?;
    let older_claimable_months = months
        .iter()
        .rev()
        .filter(|(key, projection)| {
            key.as_str() < oldest_calendar_month.as_str()
                && projection.balance.claimable_seconds > 0
        })
        .map(|(_, projection)| projection.summary.clone())
        .collect();

    let today_key = admin_local_date.to_string();
    let check_in: Option<(i64, i64)> = sqlx::query_as(
        "SELECT daily_threshold_cents, reward_seconds FROM attendance_check_ins
         WHERE account_id = ? AND earning_date = ?",
    )
    .bind(account_id)
    .bind(&today_key)
    .fetch_optional(&mut *tx)
    .await?;
    let progress: Option<DaySnapshot> = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT capped_cents, daily_threshold_cents, reward_seconds
         FROM attendance_daily_progress WHERE account_id = ? AND earning_date = ?",
    )
    .bind(account_id)
    .bind(&today_key)
    .fetch_optional(&mut *tx)
    .await?
    .map(
        |(capped_cents, daily_threshold_cents, reward_seconds)| DaySnapshot {
            capped_cents,
            daily_threshold_cents,
            reward_seconds,
        },
    );
    let today = if let Some((daily_threshold_cents, reward_seconds)) = check_in {
        AttendanceToday {
            earning_date: today_key,
            status: AttendanceTodayStatus::CheckedIn,
            capped_cents: progress
                .as_ref()
                .map(|progress| progress.capped_cents)
                .unwrap_or(daily_threshold_cents),
            daily_threshold_cents,
            reward_seconds,
        }
    } else {
        let (capped_cents, daily_threshold_cents, reward_seconds) = progress
            .map(|progress| {
                (
                    progress.capped_cents,
                    progress.daily_threshold_cents,
                    progress.reward_seconds,
                )
            })
            .unwrap_or((0, settings.daily_threshold_cents, settings.reward_seconds));
        let account_exists_today = account_created_at <= as_of;
        AttendanceToday {
            earning_date: today_key,
            status: if settings.enabled && account_exists_today {
                AttendanceTodayStatus::InProgress
            } else {
                AttendanceTodayStatus::Ineligible
            },
            capped_cents,
            daily_threshold_cents,
            reward_seconds,
        }
    };

    let requested_month = match requested_month {
        Some(month_start) => {
            let key = month_key(month_start);
            let summary = month_summary(&months, &key);
            let days = month_days(
                &mut tx,
                account_id,
                account_created_at,
                as_of,
                admin_local_date,
                &settings,
                month_start,
            )
            .await?;
            Some(AttendanceMonth {
                summary,
                days,
                as_of,
                admin_local_date: admin_local_date.to_string(),
                admin_local_month: admin_local_month.clone(),
            })
        }
        None => None,
    };
    tx.commit().await?;
    Ok(AttendanceAccountSnapshot {
        summary: AttendanceSummary {
            enabled: settings.enabled,
            today,
            totals,
            calendar_months,
            older_claimable_months,
            as_of,
            admin_local_date: admin_local_date.to_string(),
            admin_local_month,
        },
        requested_month,
        remaining_seconds: remaining_seconds.max(0),
    })
}

fn month_summary(months: &BTreeMap<String, MonthProjection>, key: &str) -> AttendanceMonthSummary {
    months
        .get(key)
        .map(|projection| projection.summary.clone())
        .unwrap_or_else(|| AttendanceMonthSummary {
            earning_month: key.to_owned(),
            state: None,
            check_in_count: 0,
            earned_seconds: 0,
            claimed_seconds: 0,
            outstanding_seconds: 0,
            claimable_seconds: 0,
            expired_seconds: 0,
            forfeited_seconds: 0,
            claim_starts_at: None,
            deadline_at_exclusive: None,
            claim_starts_on: None,
            deadline_date_inclusive: None,
            claim_count: 0,
            latest_claim_at: None,
            latest_claim_on: None,
        })
}

async fn month_days(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: &str,
    account_created_at: DateTime<Utc>,
    as_of: DateTime<Utc>,
    admin_local_date: NaiveDate,
    settings: &super::attendance::AttendanceSettingsVersion,
    month_start: NaiveDate,
) -> AppResult<Vec<AttendanceDay>> {
    let month_end = month_start
        .checked_add_months(Months::new(1))
        .and_then(|first_of_next| first_of_next.pred_opt())
        .ok_or_else(|| AppError::Internal("Attendance calendar month overflow".to_owned()))?;
    let start_key = month_start.to_string();
    let end_key = month_end.to_string();
    let progress_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT earning_date, capped_cents, daily_threshold_cents, reward_seconds
         FROM attendance_daily_progress
         WHERE account_id = ? AND earning_date BETWEEN ? AND ?",
    )
    .bind(account_id)
    .bind(&start_key)
    .bind(&end_key)
    .fetch_all(&mut **tx)
    .await?;
    let progress_by_date = progress_rows
        .into_iter()
        .map(|(date, capped, threshold, reward)| {
            (
                date,
                DaySnapshot {
                    capped_cents: capped,
                    daily_threshold_cents: threshold,
                    reward_seconds: reward,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let check_in_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT earning_date, daily_threshold_cents, reward_seconds
         FROM attendance_check_ins
         WHERE account_id = ? AND earning_date BETWEEN ? AND ?",
    )
    .bind(account_id)
    .bind(&start_key)
    .bind(&end_key)
    .fetch_all(&mut **tx)
    .await?;
    let check_ins = check_in_rows
        .into_iter()
        .map(|(date, threshold, reward)| (date, (threshold, reward)))
        .collect::<BTreeMap<_, _>>();

    let interval_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT earning_date, starts_at, ends_at FROM attendance_eligible_intervals
         WHERE earning_date BETWEEN ? AND ? ORDER BY earning_date, starts_at",
    )
    .bind(&start_key)
    .bind(&end_key)
    .fetch_all(&mut **tx)
    .await?;
    let mut intervals_by_date: BTreeMap<String, Vec<Interval>> = BTreeMap::new();
    for (date, starts_at, ends_at) in interval_rows {
        intervals_by_date.entry(date).or_default().push(Interval {
            start: super::attendance::parse_stored_instant(&starts_at)?,
            end: super::attendance::parse_stored_instant(&ends_at)?,
        });
    }
    let setting_rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT version, enabled, effective_at
         FROM attendance_settings_versions ORDER BY version",
    )
    .fetch_all(&mut **tx)
    .await?;
    let setting_evidence = setting_rows
        .into_iter()
        .map(|(version, enabled, effective_at)| {
            Ok(SettingsEvidence {
                version,
                enabled: enabled == 1,
                effective_at: super::attendance::parse_stored_instant(&effective_at)?,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;

    let mut days = Vec::new();
    let mut date = month_start;
    while date <= month_end {
        let date_key = date.to_string();
        let check_in = check_ins.get(&date_key).copied();
        let progress = progress_by_date.get(&date_key);
        let (status, metrics) = if date > admin_local_date {
            (AttendanceDayStatus::Future, None)
        } else if let Some((threshold, reward)) = check_in {
            (
                AttendanceDayStatus::CheckedIn,
                Some(progress.cloned().unwrap_or(DaySnapshot {
                    capped_cents: threshold,
                    daily_threshold_cents: threshold,
                    reward_seconds: reward,
                })),
            )
        } else if date == admin_local_date {
            let metrics = progress.cloned().or_else(|| {
                (settings.enabled && account_created_at <= as_of).then_some(DaySnapshot {
                    capped_cents: 0,
                    daily_threshold_cents: settings.daily_threshold_cents,
                    reward_seconds: settings.reward_seconds,
                })
            });
            if settings.enabled && account_created_at <= as_of {
                (AttendanceDayStatus::TodayProgress, metrics)
            } else {
                (AttendanceDayStatus::Ineligible, progress.cloned())
            }
        } else if intervals_by_date.get(&date_key).is_some_and(|intervals| {
            has_eligible_overlap(intervals, &setting_evidence, account_created_at)
        }) {
            (AttendanceDayStatus::Missed, progress.cloned())
        } else {
            (AttendanceDayStatus::Ineligible, None)
        };
        days.push(AttendanceDay {
            earning_date: date_key,
            status,
            capped_cents: metrics.as_ref().map(|metrics| metrics.capped_cents),
            daily_threshold_cents: metrics
                .as_ref()
                .map(|metrics| metrics.daily_threshold_cents),
            reward_seconds: metrics.as_ref().map(|metrics| metrics.reward_seconds),
        });
        date = date
            .succ_opt()
            .ok_or_else(|| AppError::Internal("Attendance calendar date overflow".to_owned()))?;
    }
    Ok(days)
}

fn has_eligible_overlap(
    shop_open_intervals: &[Interval],
    settings: &[SettingsEvidence],
    account_created_at: DateTime<Utc>,
) -> bool {
    shop_open_intervals.iter().any(|interval| {
        let start = interval.start.max(account_created_at);
        if start >= interval.end {
            return false;
        }
        let mut boundaries = vec![start];
        boundaries.extend(
            settings
                .iter()
                .map(|version| version.effective_at)
                .filter(|effective_at| *effective_at > start && *effective_at < interval.end),
        );
        boundaries.sort_unstable();
        boundaries.dedup();
        boundaries.iter().enumerate().any(|(index, segment_start)| {
            let segment_end = boundaries.get(index + 1).copied().unwrap_or(interval.end);
            segment_end > *segment_start
                && settings
                    .iter()
                    .filter(|version| version.effective_at <= *segment_start)
                    .max_by_key(|version| version.version)
                    .is_some_and(|version| version.enabled)
        })
    })
}

fn calendar_month_keys(admin_local_date: NaiveDate) -> AppResult<Vec<String>> {
    let first = NaiveDate::from_ymd_opt(admin_local_date.year(), admin_local_date.month(), 1)
        .ok_or_else(|| AppError::Internal("invalid Admin-local Attendance date".to_owned()))?;
    (0..12)
        .map(|months| {
            first
                .checked_sub_months(Months::new(months))
                .map(month_key)
                .ok_or_else(|| AppError::Internal("Attendance calendar month underflow".to_owned()))
        })
        .collect()
}

fn month_key(date: NaiveDate) -> String {
    date.format("%Y-%m").to_string()
}

fn add_total(total: &mut i64, value: i64) -> AppResult<()> {
    *total = total
        .checked_add(value)
        .ok_or_else(|| AppError::Conflict("Attendance totals overflow".to_owned()))?;
    Ok(())
}

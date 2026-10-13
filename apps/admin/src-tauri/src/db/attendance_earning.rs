use super::attendance::{settings_version_at, AwardSnapshot, SourceKind};
use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, SecondsFormat, TimeZone, Utc};
use openpiso_core::{
    attendance as attendance_domain,
    audit::{AuditActor, AuditOutcome},
};
use sqlx::{Sqlite, SqlitePool, Transaction};

/// Source event and frozen date data admitted by the earning transaction.
pub struct EarningInput<'a> {
    pub account_id: &'a str,
    pub source_audit_event_id: &'a str,
    pub source_kind: SourceKind,
    pub cents: i64,
    pub source_at: DateTime<Utc>,
    pub offset: FixedOffset,
    pub earning_date: NaiveDate,
    pub award: AwardSnapshot,
    pub original_rejection_audit_event_id: Option<&'a str>,
    pub trigger_pc_id: Option<&'a str>,
}

struct CheckInInput<'a> {
    account_id: &'a str,
    earning_date: NaiveDate,
    award: AwardSnapshot,
    threshold_crossing_source_audit_event_id: &'a str,
    source_at: DateTime<Utc>,
    offset: FixedOffset,
    trigger_pc_id: Option<&'a str>,
}

/// Write one immutable Contribution and update capped daily progress.
/// The caller owns the transaction so the source, Check-In and Earning Month
/// commit together (ADR-0084).
pub async fn record_earning(
    tx: &mut Transaction<'_, Sqlite>,
    input: EarningInput<'_>,
) -> AppResult<()> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT source_audit_event_id FROM attendance_contributions
         WHERE source_audit_event_id = ?",
    )
    .bind(input.source_audit_event_id)
    .fetch_optional(&mut **tx)
    .await?;
    if existing.is_some() {
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO attendance_contributions
           (source_audit_event_id, source_kind, configuration_version,
            account_id, earning_date, cents, source_at,
            original_rejection_audit_event_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.source_audit_event_id)
    .bind(input.source_kind.as_str())
    .bind(input.award.configuration_version)
    .bind(input.account_id)
    .bind(input.earning_date.to_string())
    .bind(input.cents)
    .bind(input.source_at.to_rfc3339_opts(SecondsFormat::Millis, true))
    .bind(input.original_rejection_audit_event_id)
    .execute(&mut **tx)
    .await?;

    let date_key = input.earning_date.to_string();
    let progress: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT configuration_version, daily_threshold_cents, reward_seconds, capped_cents
         FROM attendance_daily_progress
         WHERE account_id = ? AND earning_date = ?",
    )
    .bind(input.account_id)
    .bind(&date_key)
    .fetch_optional(&mut **tx)
    .await?;

    match progress {
        Some((version, threshold, reward_seconds, capped)) => {
            if capped < threshold {
                let new_capped = if input.cents >= threshold - capped {
                    threshold
                } else {
                    capped + input.cents
                };
                sqlx::query(
                    "UPDATE attendance_daily_progress
                     SET capped_cents = ? WHERE account_id = ? AND earning_date = ?",
                )
                .bind(new_capped)
                .bind(input.account_id)
                .bind(&date_key)
                .execute(&mut **tx)
                .await?;
                if new_capped >= threshold {
                    return check_in(
                        tx,
                        CheckInInput {
                            account_id: input.account_id,
                            earning_date: input.earning_date,
                            award: AwardSnapshot {
                                configuration_version: version,
                                daily_threshold_cents: threshold,
                                reward_seconds,
                            },
                            threshold_crossing_source_audit_event_id: input.source_audit_event_id,
                            source_at: input.source_at,
                            offset: input.offset,
                            trigger_pc_id: input.trigger_pc_id,
                        },
                    )
                    .await;
                }
            }
            Ok(())
        }
        None => {
            sqlx::query(
                "INSERT INTO attendance_daily_progress
                   (account_id, earning_date, configuration_version,
                    daily_threshold_cents, reward_seconds,
                    first_source_audit_event_id, capped_cents)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(input.account_id)
            .bind(&date_key)
            .bind(input.award.configuration_version)
            .bind(input.award.daily_threshold_cents)
            .bind(input.award.reward_seconds)
            .bind(input.source_audit_event_id)
            .bind(input.cents.min(input.award.daily_threshold_cents))
            .execute(&mut **tx)
            .await?;
            if input.cents >= input.award.daily_threshold_cents {
                return check_in(
                    tx,
                    CheckInInput {
                        account_id: input.account_id,
                        earning_date: input.earning_date,
                        award: input.award,
                        threshold_crossing_source_audit_event_id: input.source_audit_event_id,
                        source_at: input.source_at,
                        offset: input.offset,
                        trigger_pc_id: input.trigger_pc_id,
                    },
                )
                .await;
            }
            Ok(())
        }
    }
}

/// Award the immutable Check-In and create the month's Earning Month if needed.
async fn check_in(tx: &mut Transaction<'_, Sqlite>, input: CheckInInput<'_>) -> AppResult<()> {
    let date_key = input.earning_date.to_string();
    let audit_event_id = audit::append(
        tx,
        NewAuditEvent {
            event_type: "attendance_check_in",
            outcome: AuditOutcome::Success,
            actor: AuditActor::System,
            actor_account_id: None,
            account_id: Some(input.account_id),
            pc_id: input.trigger_pc_id,
            amount_cents: None,
            seconds_delta: None,
            attendance_seconds_delta: Some(input.award.reward_seconds),
            points_delta_centi: None,
            cash_flag: false,
            summary: "Attendance Check-In earned".to_owned(),
            metadata: serde_json::json!({
                "earning_date": input.earning_date.to_string(),
                "daily_threshold_cents": input.award.daily_threshold_cents,
                "reward_seconds": input.award.reward_seconds,
            }),
        },
    )
    .await?;

    let earning_month = format!(
        "{:04}-{:02}",
        input.earning_date.year(),
        input.earning_date.month()
    );
    let month: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM attendance_earning_months
         WHERE account_id = ? AND earning_month = ?",
    )
    .bind(input.account_id)
    .bind(&earning_month)
    .fetch_optional(&mut **tx)
    .await?;
    let month_id = match month {
        Some(id) => id,
        None => {
            let month_start = input
                .earning_date
                .with_day(1)
                .ok_or_else(|| AppError::InvalidInput("attendance month overflow".to_owned()))?;
            let month_start_utc = month_start
                .and_time(chrono::NaiveTime::MIN)
                .and_local_timezone(input.offset)
                .single()
                .ok_or_else(|| {
                    AppError::InvalidInput(
                        "attendance month start is not a single local instant".to_owned(),
                    )
                })?
                .with_timezone(&Utc);
            // The rollout month may predate the seeded v1 row; freeze the
            // earliest immutable settings version in that case.
            let frozen_version = match settings_version_at(tx, month_start_utc).await? {
                Some(version) => version,
                None => sqlx::query_scalar(
                    "SELECT version FROM attendance_settings_versions
                     ORDER BY effective_at, version LIMIT 1",
                )
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| {
                    AppError::InvalidInput("Attendance settings history is empty".to_owned())
                })?,
            };
            let (claim_window_months, _): (i64, String) = sqlx::query_as(
                "SELECT claim_window_months, effective_at
                 FROM attendance_settings_versions WHERE version = ?",
            )
            .bind(frozen_version)
            .fetch_one(&mut **tx)
            .await?;
            let window_start = month_start
                .checked_add_months(chrono::Months::new(1))
                .ok_or_else(|| {
                    AppError::InvalidInput("Attendance claim window cannot start".to_owned())
                })?;
            let claim_starts_at = input
                .offset
                .from_local_datetime(&window_start.and_time(chrono::NaiveTime::MIN))
                .single()
                .ok_or_else(|| {
                    AppError::InvalidInput(
                        "Attendance claim window start is not a single local instant".to_owned(),
                    )
                })?
                .with_timezone(&Utc);
            let deadline = attendance_domain::claim_deadline(
                month_start,
                u32::try_from(claim_window_months).map_err(|_| {
                    AppError::InvalidInput("Attendance claim window is out of range".to_owned())
                })?,
                input.offset,
            )
            .ok_or_else(|| {
                AppError::InvalidInput("Attendance claim deadline cannot be computed".to_owned())
            })?;
            let claim_end_date = deadline
                .with_timezone(&input.offset)
                .date_naive()
                .pred_opt()
                .ok_or_else(|| {
                    AppError::InvalidInput("Attendance claim date overflow".to_owned())
                })?;
            let claim_local_dates = serde_json::json!({
                "start": window_start.to_string(),
                "end": claim_end_date.to_string(),
            })
            .to_string();
            sqlx::query(
                "INSERT INTO attendance_earning_months
                   (account_id, earning_month, claim_window_configuration_version,
                    claim_window_months, claim_starts_at, deadline_at_exclusive,
                    claim_local_dates)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(input.account_id)
            .bind(&earning_month)
            .bind(frozen_version)
            .bind(claim_window_months)
            .bind(claim_starts_at.to_rfc3339_opts(SecondsFormat::Millis, true))
            .bind(deadline.to_rfc3339_opts(SecondsFormat::Millis, true))
            .bind(claim_local_dates)
            .execute(&mut **tx)
            .await?;
            sqlx::query_scalar::<_, i64>(
                "SELECT id FROM attendance_earning_months
                WHERE account_id = ? AND earning_month = ?",
            )
            .bind(input.account_id)
            .bind(&earning_month)
            .fetch_one(&mut **tx)
            .await?
        }
    };

    sqlx::query(
        "INSERT INTO attendance_check_ins
           (audit_event_id, account_id, earning_date, earning_month_id,
            configuration_version, daily_threshold_cents, reward_seconds,
            threshold_crossing_source_audit_event_id, trigger_pc_id,
            original_earning_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&audit_event_id)
    .bind(input.account_id)
    .bind(&date_key)
    .bind(month_id)
    .bind(input.award.configuration_version)
    .bind(input.award.daily_threshold_cents)
    .bind(input.award.reward_seconds)
    .bind(input.threshold_crossing_source_audit_event_id)
    .bind(input.trigger_pc_id)
    .bind(input.source_at.to_rfc3339_opts(SecondsFormat::Millis, true))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Verify the rebuildable projection against immutable Contributions and each
/// day's first admitted configuration snapshot (ADR-0084).
pub async fn verify_and_rebuild_progress(tx: &mut Transaction<'_, Sqlite>) -> AppResult<()> {
    let contributions: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT account_id, earning_date, source_audit_event_id,
                configuration_version, cents
         FROM attendance_contributions
         ORDER BY account_id, earning_date, admission_sequence",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut expected = std::collections::BTreeMap::new();

    for (account_id, date_key, source_id, configuration_version, cents) in contributions {
        let key = (account_id, date_key);
        if !expected.contains_key(&key) {
            let (daily_threshold_cents, reward_seconds): (i64, i64) = sqlx::query_as(
                "SELECT daily_threshold_cents, reward_seconds
                 FROM attendance_settings_versions WHERE version = ?",
            )
            .bind(configuration_version)
            .fetch_one(&mut **tx)
            .await?;
            let award = AwardSnapshot {
                configuration_version,
                daily_threshold_cents,
                reward_seconds,
            };
            expected.insert(key.clone(), (award, source_id, 0_i64));
        }

        let (award, _, capped) = expected.get_mut(&key).ok_or_else(|| {
            AppError::InvalidInput("Attendance projection is incomplete".to_owned())
        })?;
        *capped += cents.min(award.daily_threshold_cents - *capped);
    }

    let rows: Vec<(String, String, i64, i64, i64, String, i64)> = sqlx::query_as(
        "SELECT account_id, earning_date, configuration_version,
                daily_threshold_cents, reward_seconds,
                first_source_audit_event_id, capped_cents
         FROM attendance_daily_progress",
    )
    .fetch_all(&mut **tx)
    .await?;
    let actual: std::collections::BTreeMap<_, _> = rows
        .into_iter()
        .map(
            |(account, date, version, threshold, reward, first_source, capped)| {
                (
                    (account, date),
                    (
                        AwardSnapshot {
                            configuration_version: version,
                            daily_threshold_cents: threshold,
                            reward_seconds: reward,
                        },
                        first_source,
                        capped,
                    ),
                )
            },
        )
        .collect();
    if actual == expected {
        return Ok(());
    }

    sqlx::query("DELETE FROM attendance_daily_progress")
        .execute(&mut **tx)
        .await?;
    for ((account_id, date_key), (award, first_source, capped)) in expected {
        sqlx::query(
            "INSERT INTO attendance_daily_progress
               (account_id, earning_date, configuration_version,
                daily_threshold_cents, reward_seconds,
                first_source_audit_event_id, capped_cents)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account_id)
        .bind(date_key)
        .bind(award.configuration_version)
        .bind(award.daily_threshold_cents)
        .bind(award.reward_seconds)
        .bind(first_source)
        .bind(capped)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Finalize Attendance dates and verify its rebuildable projection before the
/// Admin starts serving requests.
pub async fn prepare_startup_state(
    pool: &SqlitePool,
    host_utc: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<()> {
    super::attendance::finalize_calendar_dates(pool, host_utc, offset).await?;
    let mut tx = crate::db::begin_immediate(pool).await?;
    verify_and_rebuild_progress(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audit::{self, NewAuditEvent},
        db::{
            attendance::{finalize_calendar_dates, qualify_source},
            attendance_client, begin_immediate,
        },
        errors::AppResult,
    };
    use chrono::{Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
    use openpiso_core::audit::{AuditActor, AuditOutcome};
    use std::io::Write;

    // ---- S2 earning fixtures ------------------------------------------------

    /// A deterministic UTC offset for every earning test: all scenario
    /// instants are authored against +08:00 (Admin-local) and resolved by the
    /// S1 interval logic.
    fn offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).unwrap()
    }

    /// One calendar date's midnight-to-midnight eligible interval, seeded
    /// through the S1 finalization path so the earning seam is the same one
    /// production uses. The clock singleton is rewound to the date's eve
    /// before and after, since its seeded open date is the real host's today
    /// and the fixture must not advance authority past the test instants.
    async fn freeze_eligible_date(
        pool: &sqlx::SqlitePool,
        date: NaiveDate,
        offset: FixedOffset,
    ) -> AppResult<()> {
        freeze_eligible_date_with_schedule(pool, date, offset, None).await
    }

    async fn freeze_eligible_date_with_schedule(
        pool: &sqlx::SqlitePool,
        date: NaiveDate,
        offset: FixedOffset,
        open_period: Option<(i64, i64)>,
    ) -> AppResult<()> {
        let eve = date.pred_opt().unwrap();
        let eve_start = offset
            .from_local_datetime(&eve.and_time(chrono::NaiveTime::MIN))
            .single()
            .unwrap()
            .with_timezone(&Utc);
        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(eve_start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .bind(eve.to_string())
        .execute(pool)
        .await?;
        // Append fixture Shop Hours instead of rewriting seeded evidence.
        let version_id: i64 = sqlx::query_scalar(
            "INSERT INTO shop_hours_versions (enabled, last_call_lead_minutes, effective_at)
             VALUES (1, 0, ?) RETURNING id",
        )
        .bind(eve_start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .fetch_one(pool)
        .await?;
        let open_weekday = date.weekday().num_days_from_monday();
        for weekday in 0..7 {
            let schedule = match open_period {
                Some((open, close)) if weekday == open_weekday => {
                    ("open_period", Some(open), Some(close))
                }
                Some(_) => ("closed_all_day", None, None),
                None => ("open_24_hours", None, None),
            };
            sqlx::query(
                "INSERT INTO shop_hours_version_days
                   (shop_hours_version_id, weekday, mode, open_minute, close_minute)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(version_id)
            .bind(i64::from(weekday))
            .bind(schedule.0)
            .bind(schedule.1)
            .bind(schedule.2)
            .execute(pool)
            .await?;
        }
        let host_utc = eve_start + Duration::days(2);
        finalize_calendar_dates(pool, host_utc, offset).await?;
        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(eve_start.to_rfc3339_opts(SecondsFormat::Millis, true))
        .bind(eve.to_string())
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn disable_attendance(
        pool: &sqlx::SqlitePool,
        version: i64,
        effective_at: chrono::DateTime<Utc>,
    ) -> AppResult<()> {
        crate::test_utils::insert_attendance_settings(
            pool,
            version,
            false,
            1_000,
            3_600,
            effective_at,
        )
        .await
    }

    /// Write one accepted-coin audit event and its Attendance earning inside
    /// a single immediate transaction, exactly as the coin hook does.
    async fn earn_coin(
        pool: &sqlx::SqlitePool,
        account_id: &str,
        amount_cents: i64,
        source_at: chrono::DateTime<Utc>,
        offset: FixedOffset,
    ) -> AppResult<()> {
        let mut tx = begin_immediate(pool).await?;
        let mut event = NewAuditEvent::simple(
            "coin_accepted",
            AuditOutcome::Success,
            AuditActor::CoinDevice,
            "Accepted coin",
        );
        event.account_id = Some(account_id);
        event.amount_cents = Some(amount_cents);
        let audit_event_id = audit::append(&mut tx, event).await?;
        if let Some(qualification) = qualify_source(&mut tx, source_at, offset).await? {
            record_earning(
                &mut tx,
                EarningInput {
                    account_id,
                    source_audit_event_id: &audit_event_id,
                    source_kind: SourceKind::AcceptedCoin,
                    cents: amount_cents,
                    source_at,
                    offset,
                    earning_date: qualification.earning_date,
                    award: qualification.award,
                    original_rejection_audit_event_id: None,
                    trigger_pc_id: Some("pc-01"),
                },
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn progress_of(
        pool: &sqlx::SqlitePool,
        account_id: &str,
        date: &str,
    ) -> AppResult<Option<(i64, i64, i64, i64)>> {
        Ok(sqlx::query_as(
            "SELECT configuration_version, daily_threshold_cents, reward_seconds, capped_cents
             FROM attendance_daily_progress WHERE account_id = ? AND earning_date = ?",
        )
        .bind(account_id)
        .bind(date)
        .fetch_optional(pool)
        .await?)
    }

    async fn check_in_count(pool: &sqlx::SqlitePool) -> AppResult<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM attendance_check_ins")
                .fetch_one(pool)
                .await?,
        )
    }

    // ---- S2 earning scenarios ----------------------------------------------

    /// DOM-1: the first qualifying coin freezes threshold/reward; later edits
    /// never alter the snapshot.
    #[tokio::test]
    async fn first_contribution_freezes_threshold_and_reward_snapshot() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-snapshot").await;
        let account = crate::accounts::create(
            &pool,
            "snapshot-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        // v2 enables effective from the month start, so the Earning Month's
        // frozen claim-window version also resolves.
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        // 2026-01-11T18:00Z is 2026-01-12 02:00 Admin-local: the earning date.
        let at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();

        earn_coin(&pool, &account.id, 400, at, offset()).await?;
        let (version, threshold, reward, capped) = progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .unwrap();
        assert_eq!((version, threshold, reward, capped), (2, 1_000, 3_600, 400));
        assert_eq!(check_in_count(&pool).await?, 0);

        // A mid-day settings edit appends a new version; a later coin that
        // crosses the old threshold still awards the frozen v2 reward.
        crate::test_utils::enable_attendance(
            &pool,
            3,
            Utc.with_ymd_and_hms(2026, 1, 11, 19, 0, 0).unwrap(),
            2_000,
            7_200,
        )
        .await?;
        earn_coin(&pool, &account.id, 600, at + Duration::hours(1), offset()).await?;
        let after = progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .unwrap();
        assert_eq!(after, (2, 1_000, 3_600, 1_000));
        assert_eq!(check_in_count(&pool).await?, 1);
        let (check_in_version, check_in_reward): (i64, i64) = sqlx::query_as(
            "SELECT configuration_version, reward_seconds FROM attendance_check_ins",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!((check_in_version, check_in_reward), (2, 3_600));

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// DOM-2: coins across sessions/PCs cross the threshold once; one
    /// Account/date Check-In.
    #[tokio::test]
    async fn coins_across_pcs_cross_threshold_once() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-cross-pc").await;
        let account = crate::accounts::create(
            &pool,
            "cross-pc-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        let at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();

        earn_coin(&pool, &account.id, 400, at, offset()).await?;
        assert!(progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .is_some());
        assert_eq!(check_in_count(&pool).await?, 0);

        earn_coin(
            &pool,
            &account.id,
            600,
            at + Duration::minutes(30),
            offset(),
        )
        .await?;
        assert_eq!(check_in_count(&pool).await?, 1);
        let check_in_target: Option<String> = sqlx::query_scalar(
            "SELECT a.account_id
             FROM attendance_check_ins ci
             JOIN audit_log a ON a.id = ci.audit_event_id
             WHERE ci.account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(check_in_target.as_deref(), Some(account.id.as_str()));
        let (_, _, _, capped) = progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .unwrap();
        assert_eq!(capped, 1_000);

        // Further value cannot create a duplicate Check-In (DOM-6).
        earn_coin(&pool, &account.id, 900, at + Duration::hours(2), offset()).await?;
        assert_eq!(check_in_count(&pool).await?, 1);

        // Replaying one immutable source event is an idempotent no-op.
        let source_id: String = sqlx::query_scalar(
            "SELECT source_audit_event_id FROM attendance_contributions
             ORDER BY admission_sequence LIMIT 1",
        )
        .fetch_one(&pool)
        .await?;
        let mut tx = begin_immediate(&pool).await?;
        record_earning(
            &mut tx,
            EarningInput {
                account_id: &account.id,
                source_audit_event_id: &source_id,
                source_kind: SourceKind::AcceptedCoin,
                cents: 400,
                source_at: at,
                offset: offset(),
                earning_date: date,
                award: AwardSnapshot {
                    configuration_version: 2,
                    daily_threshold_cents: 1_000,
                    reward_seconds: 3_600,
                },
                original_rejection_audit_event_id: None,
                trigger_pc_id: Some("pc-01"),
            },
        )
        .await?;
        tx.commit().await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_contributions")
                .fetch_one(&pool)
                .await?,
            3
        );
        assert_eq!(check_in_count(&pool).await?, 1);

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// DOM-5: below-threshold value dies at midnight; no carry.
    #[tokio::test]
    async fn below_threshold_progress_does_not_carry_past_midnight() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-midnight").await;
        let account = crate::accounts::create(
            &pool,
            "midnight-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let day1 = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2026, 1, 13).unwrap();
        freeze_eligible_date(&pool, day1, offset).await?;
        freeze_eligible_date(&pool, day2, offset).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        // 2026-01-12 15:30 Admin-local: late in day 1.
        let late_day1 = Utc.with_ymd_and_hms(2026, 1, 12, 7, 30, 0).unwrap();

        earn_coin(&pool, &account.id, 500, late_day1, offset).await?;
        assert!(progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .is_some());

        // The next day's first coin starts at zero; it is not a continuation.
        // 2026-01-13 01:00 Admin-local.
        let next_morning = Utc.with_ymd_and_hms(2026, 1, 12, 17, 0, 0).unwrap();
        earn_coin(&pool, &account.id, 600, next_morning, offset).await?;
        let (_, _, _, capped) = progress_of(&pool, &account.id, "2026-01-13")
            .await?
            .unwrap();
        assert_eq!(capped, 600);
        assert_eq!(check_in_count(&pool).await?, 0);

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// DOM-3: an accepted Guest coin writes no Attendance facts while earning is enabled.
    #[tokio::test]
    async fn guest_coins_earn_nothing() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-guest").await;
        let offset = crate::db::attendance::host_offset();
        let now = Utc::now();
        let date = now.with_timezone(&offset).date_naive();
        freeze_eligible_date(&pool, date, offset).await?;
        crate::test_utils::enable_attendance(&pool, 2, now, 1_000, 3_600).await?;

        let coin = crate::db::record_coin_event(
            &pool,
            &crate::models::CoinEventInput {
                pc_id: "pc-01".to_owned(),
                amount_cents: 100,
                pulse_count: Some(1),
                timestamp: None,
            },
            crate::models::CoinTarget::Guest,
            &crate::models::CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        assert_eq!(coin.amount_cents, 100);
        for table in [
            "attendance_contributions",
            "attendance_daily_progress",
            "attendance_check_ins",
            "attendance_earning_months",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&pool)
                    .await?,
                0,
                "Guest coin must not write {table}"
            );
        }
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    /// The disabled path makes zero Attendance writes.
    #[tokio::test]
    async fn vouchers_adjustments_redemptions_and_wheel_prizes_do_not_earn() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-excluded-sources").await;
        let account = crate::accounts::create(
            &pool,
            "excluded-sources-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let now = Utc::now();
        crate::test_utils::enable_attendance(&pool, 2, now, 1_000, 3_600).await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 1000000 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        let voucher = crate::db::vouchers::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: Some("AT".to_owned()),
                suffix_length: Some(8),
                seconds_value: 60,
                points_value_centi: 0,
                sold_amount_cents: None,
                expires_at: None,
            },
        )
        .await?;
        crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &voucher.code).await?;
        crate::db::pcs::create_manual_adjustment(
            &pool,
            &crate::models::ManualAdjustmentInput {
                pc_id: "pc-01".to_owned(),
                seconds_delta: 60,
                reason: Some("Attendance exclusion fixture".to_owned()),
            },
        )
        .await?;

        let menu = crate::db::redemption_tiers::redemption_tier_menu(&pool).await?;
        let tier_id = menu
            .tiers
            .first()
            .map(|tier| tier.id)
            .ok_or_else(|| AppError::InvalidInput("seed Redemption Tier missing".to_owned()))?;
        crate::db::redemption_tiers::redeem_tier(
            &pool,
            &account.id,
            "pc-01",
            tier_id,
            menu.version,
        )
        .await?;

        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        struct WinningDraw;
        impl openpiso_core::wheel::SpinRng for WinningDraw {
            fn gen_below(&mut self, bound: i64) -> i64 {
                if bound > 1 {
                    1
                } else {
                    0
                }
            }
        }
        let spin =
            crate::db::points_wheel::spin_wheel(&pool, &account.id, "pc-01", &mut WinningDraw)
                .await?;
        assert!(spin.result.seconds_won > 0);

        for table in [
            "attendance_contributions",
            "attendance_daily_progress",
            "attendance_check_ins",
            "attendance_earning_months",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&pool)
                    .await?,
                0,
                "non-coin value sources must not write {table}"
            );
        }
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn disabled_attendance_writes_nothing() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-disabled").await;
        let account = crate::accounts::create(
            &pool,
            "disabled-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        let at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();
        // v1 stays disabled; the coin hook qualifies nothing.
        earn_coin(&pool, &account.id, 1_000, at, offset()).await?;
        for table in [
            "attendance_contributions",
            "attendance_daily_progress",
            "attendance_check_ins",
            "attendance_earning_months",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&pool)
                .await?;
            assert_eq!(count, 0, "{table} must stay empty while disabled");
        }
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// DOM-12: an eligible date can accrue live progress while Shop Closed.
    #[tokio::test]
    async fn coin_during_closed_hours_earns_on_eligible_date() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-eligible").await;
        let account = crate::accounts::create(
            &pool,
            "eligible-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        // Freeze a weekday schedule with 08:00-22:00 local Shop Open.
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date_with_schedule(&pool, date, offset(), Some((480, 1_320))).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        // 03:00 local is closed, but the date has an effective 08:00-22:00
        // Shop Open overlap (local 08:00-22:00 maps to 00:00-14:00 UTC).
        let closed_at = Utc.with_ymd_and_hms(2026, 1, 11, 19, 0, 0).unwrap();
        earn_coin(&pool, &account.id, 1_000, closed_at, offset()).await?;
        assert_eq!(
            progress_of(&pool, &account.id, "2026-01-12")
                .await?
                .unwrap()
                .3,
            1_000
        );
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[tokio::test]
    async fn coin_on_date_without_shop_open_overlap_earns_nothing() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-closed-date").await;
        let account = crate::accounts::create(
            &pool,
            "closed-date-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let offset = offset();
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let starts_at = offset
            .from_local_datetime(&date.and_time(chrono::NaiveTime::MIN))
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let next_date = date.succ_opt().unwrap();
        let ends_at = offset
            .from_local_datetime(&next_date.and_time(chrono::NaiveTime::MIN))
            .single()
            .unwrap()
            .with_timezone(&Utc);
        sqlx::query(
            "INSERT INTO shop_override_history (mode, effective_at, expires_at)
             VALUES ('force_closed', ?, ?)",
        )
        .bind(starts_at.to_rfc3339_opts(SecondsFormat::Millis, true))
        .bind(ends_at.to_rfc3339_opts(SecondsFormat::Millis, true))
        .execute(&pool)
        .await?;
        freeze_eligible_date(&pool, date, offset).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;

        earn_coin(
            &pool,
            &account.id,
            1_000,
            Utc.with_ymd_and_hms(2026, 1, 11, 19, 0, 0).unwrap(),
            offset,
        )
        .await?;
        assert!(progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_contributions")
                .fetch_one(&pool)
                .await?,
            0
        );
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// DOM-4 / REP-1 / REP-2: an Account Rejection Credit uses the original
    /// rejection instant and config, writes the source + progress + Check-In +
    /// audit atomically, and is idempotent per source event.
    #[tokio::test]
    async fn rejection_credit_earns_at_original_rejection_instant() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-rejection").await;
        let account = crate::accounts::create(
            &pool,
            "rejection-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        // The 30-day creditable window compares against the host clock, so the
        // rejection must sit one hour before the real now. Freeze eligibility
        // for its real local date and rewind the clock beneath the instants.
        let offset = offset();
        let rejection_at = Utc::now() - Duration::hours(1);
        let rejection_date = rejection_at.with_timezone(&offset).date_naive();
        freeze_eligible_date(&pool, rejection_date, offset).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;

        // A rejection recorded at that instant, credited after the shop
        // re-opened. Qualification reads the original instant.
        sqlx::query(
            "INSERT INTO audit_log
               (id, event_type, outcome, actor, account_id, pc_id, summary, metadata_json, created_at)
             VALUES ('rej-attend', 'coin_batch_rejected', 'failure', 'system', ?, 'pc-01',
                     'rejected', '{\"pulse_count\": 1}', ?)",
        )
        .bind(&account.id)
        .bind(rejection_at.format("%Y-%m-%d %H:%M:%S").to_string())
        .execute(&pool)
        .await?;

        // Credit it: frozen target, original instant.
        let credit = crate::db::rejection_credits::create(
            &pool,
            &crate::db::rejection_credits::RejectionCreditInput {
                rejection_audit_id: "rej-attend".to_owned(),
                denomination_cents: 1_000,
                coins: 1,
                dry_run: false,
            },
        )
        .await?;
        assert_eq!(credit.target_kind, "account");

        let date_key = rejection_date.to_string();
        let (_, threshold, reward, capped) =
            progress_of(&pool, &account.id, &date_key).await?.unwrap();
        assert_eq!((threshold, reward, capped), (1_000, 3_600, 1_000));
        assert_eq!(check_in_count(&pool).await?, 1);
        // The Check-In audit event carries only a positive Attendance delta.
        let (attendance_delta, seconds_delta): (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT attendance_seconds_delta, seconds_delta FROM audit_log
             WHERE event_type = 'attendance_check_in'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(attendance_delta, Some(3_600));
        assert_eq!(seconds_delta, None);

        // One contribution per source event; crediting the same rejection
        // again is refused by the count cap before any Attendance write.
        assert!(crate::db::rejection_credits::create(
            &pool,
            &crate::db::rejection_credits::RejectionCreditInput {
                rejection_audit_id: "rej-attend".to_owned(),
                denomination_cents: 1_000,
                coins: 1,
                dry_run: false,
            },
        )
        .await
        .is_err());
        assert_eq!(check_in_count(&pool).await?, 1);
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// CON-2: concurrent coins keep a consistent first-qualifier snapshot.
    /// Two simultaneous coins on a WAL pool cross the threshold exactly once
    /// and both write their immutable contributions.
    #[tokio::test]
    async fn concurrent_coins_cross_threshold_once() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-concurrent").await;
        let account = crate::accounts::create(
            &pool,
            "concurrent-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        let at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();

        let account_id = account.id.clone();
        let pool_a = pool.clone();
        let pool_b = pool.clone();
        let left =
            tokio::spawn(async move { earn_coin(&pool_a, &account_id, 600, at, offset()).await });
        let account_id = account.id.clone();
        let right =
            tokio::spawn(async move { earn_coin(&pool_b, &account_id, 700, at, offset()).await });
        left.await
            .expect("left coin task joins")
            .expect("left coin earns");
        right
            .await
            .expect("right coin task joins")
            .expect("right coin earns");

        let (version, threshold, reward, capped) = progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .expect("daily progress exists");
        assert_eq!(
            (version, threshold, reward, capped),
            (2, 1_000, 3_600, 1_000)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM attendance_check_ins
                 WHERE account_id = ? AND earning_date = '2026-01-12'",
            )
            .bind(&account.id)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM attendance_check_ins c
                 JOIN audit_log a ON a.id = c.audit_event_id
                 WHERE c.account_id = ? AND c.earning_date = '2026-01-12'
                   AND a.event_type = 'attendance_check_in'",
            )
            .bind(&account.id)
            .fetch_one(&pool)
            .await?,
            1
        );
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[tokio::test]
    async fn earning_failure_rolls_back_source_progress_month_and_audit() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-rollback").await;
        let account = crate::accounts::create(
            &pool,
            "rollback-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let offset = offset();
        freeze_eligible_date(&pool, date, offset).await?;
        let effective_at = Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap();
        crate::test_utils::enable_attendance(&pool, 2, effective_at, 1_000, 3_600).await?;
        let source_at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();

        let mut tx = begin_immediate(&pool).await?;
        let mut source = NewAuditEvent::simple(
            "coin_accepted",
            AuditOutcome::Success,
            AuditActor::CoinDevice,
            "Accepted coin",
        );
        source.account_id = Some(&account.id);
        source.amount_cents = Some(1_000);
        let source_id = audit::append(&mut tx, source).await?;
        let earning = record_earning(
            &mut tx,
            EarningInput {
                account_id: &account.id,
                source_audit_event_id: &source_id,
                source_kind: SourceKind::AcceptedCoin,
                cents: 1_000,
                source_at,
                offset,
                earning_date: date,
                award: AwardSnapshot {
                    configuration_version: 2,
                    daily_threshold_cents: 1_000,
                    reward_seconds: 3_600,
                },
                original_rejection_audit_event_id: None,
                trigger_pc_id: Some("missing-pc"),
            },
        )
        .await;
        assert!(earning.is_err());
        tx.rollback().await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE id = ?")
                .bind(source_id)
                .fetch_one(&pool)
                .await?,
            0
        );
        for table in [
            "attendance_contributions",
            "attendance_daily_progress",
            "attendance_check_ins",
            "attendance_earning_months",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&pool)
                    .await?,
                0,
                "{table} must roll back with the rejected coin transaction"
            );
        }
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[tokio::test]
    async fn earning_process_crash_recovers_and_retry_rederives() -> AppResult<()> {
        const CHILD_PATH: &str = "OPENPISO_ATTENDANCE_CRASH_DB_PATH";
        const CHILD_ACCOUNT: &str = "OPENPISO_ATTENDANCE_CRASH_ACCOUNT_ID";

        if let Ok(database_path) = std::env::var(CHILD_PATH) {
            let account_id = std::env::var(CHILD_ACCOUNT).expect("crash fixture account id");
            let pool =
                crate::test_utils::reopen_wal_test_pool(std::path::Path::new(&database_path)).await;
            let date = NaiveDate::from_ymd_opt(2026, 1, 12).expect("test earning date");
            let source_at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();
            let mut tx = begin_immediate(&pool).await?;
            let mut source = NewAuditEvent::simple(
                "coin_accepted",
                AuditOutcome::Success,
                AuditActor::CoinDevice,
                "Accepted coin before simulated process crash",
            );
            source.account_id = Some(&account_id);
            source.amount_cents = Some(1_000);
            let source_id = audit::append(&mut tx, source).await?;
            record_earning(
                &mut tx,
                EarningInput {
                    account_id: &account_id,
                    source_audit_event_id: &source_id,
                    source_kind: SourceKind::AcceptedCoin,
                    cents: 1_000,
                    source_at,
                    offset: offset(),
                    earning_date: date,
                    award: AwardSnapshot {
                        configuration_version: 2,
                        daily_threshold_cents: 1_000,
                        reward_seconds: 3_600,
                    },
                    original_rejection_audit_event_id: None,
                    trigger_pc_id: None,
                },
            )
            .await?;
            println!("simulated process exit with an uncommitted Attendance award");
            std::io::stdout().flush()?;
            std::process::exit(0);
        }

        let (pool, path) = crate::test_utils::wal_test_pool("attendance-crash-restart").await;
        let account = crate::accounts::create(
            &pool,
            "crash-restart-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let offset = offset();
        freeze_eligible_date(&pool, date, offset).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        pool.close().await;

        let child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "db::attendance_earning::tests::earning_process_crash_recovers_and_retry_rederives",
                "--nocapture",
            ])
            .env(CHILD_PATH, &path)
            .env(CHILD_ACCOUNT, &account.id)
            .output()?;
        assert!(
            child.status.success(),
            "crash fixture process failed: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout)
            .contains("simulated process exit with an uncommitted Attendance award"));

        let reopened = crate::test_utils::reopen_wal_test_pool(&path).await;
        crate::db::attendance_earning::prepare_startup_state(
            &reopened,
            Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap(),
            offset,
        )
        .await?;
        for table in [
            "attendance_contributions",
            "attendance_daily_progress",
            "attendance_check_ins",
            "attendance_earning_months",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&reopened)
                    .await?,
                0,
                "uncommitted crash state must not survive in {table}"
            );
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log
                 WHERE event_type = 'coin_accepted' AND account_id = ?",
            )
            .bind(&account.id)
            .fetch_one(&reopened)
            .await?,
            0
        );

        let source_at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();
        earn_coin(&reopened, &account.id, 1_000, source_at, offset).await?;
        crate::db::attendance_earning::prepare_startup_state(&reopened, source_at, offset).await?;
        reopened.close().await;

        let retried = crate::test_utils::reopen_wal_test_pool(&path).await;
        crate::db::attendance_earning::prepare_startup_state(&retried, source_at, offset).await?;
        let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_contributions),
                    (SELECT COUNT(*) FROM attendance_daily_progress),
                    (SELECT COUNT(*) FROM attendance_check_ins),
                    (SELECT COUNT(*) FROM attendance_earning_months),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_check_in')",
        )
        .fetch_one(&retried)
        .await?;
        assert_eq!(counts, (1, 1, 1, 1, 1));
        assert_eq!(
            progress_of(&retried, &account.id, "2026-01-12")
                .await?
                .expect("retried progress")
                .3,
            1_000
        );
        retried.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// DOM-10: disabling pauses new contributions; re-enabling resumes the
    /// same day's frozen snapshot.
    #[tokio::test]
    async fn disable_pauses_and_reenable_resumes_same_day_snapshot() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-re-enable").await;
        let account = crate::accounts::create(
            &pool,
            "resume-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        let at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();

        earn_coin(&pool, &account.id, 400, at, offset()).await?;
        assert_eq!(
            progress_of(&pool, &account.id, "2026-01-12")
                .await?
                .unwrap()
                .3,
            400
        );

        // Disable pauses new contributions: nothing changes.
        disable_attendance(
            &pool,
            3,
            Utc.with_ymd_and_hms(2026, 1, 11, 18, 30, 0).unwrap(),
        )
        .await?;
        earn_coin(
            &pool,
            &account.id,
            300,
            at + Duration::minutes(10),
            offset(),
        )
        .await?;
        assert_eq!(
            progress_of(&pool, &account.id, "2026-01-12")
                .await?
                .unwrap()
                .3,
            400
        );

        // Re-enabling resumes the same day's frozen snapshot (threshold and
        // reward stay from version 2), accumulating on top of the 400.
        crate::test_utils::enable_attendance(
            &pool,
            4,
            Utc.with_ymd_and_hms(2026, 1, 11, 19, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        earn_coin(
            &pool,
            &account.id,
            600,
            at + Duration::minutes(20),
            offset(),
        )
        .await?;
        let (version, threshold, reward, capped) = progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .unwrap();
        assert_eq!(
            (version, threshold, reward, capped),
            (2, 1_000, 3_600, 1_000)
        );
        assert_eq!(check_in_count(&pool).await?, 1);

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_uses_configuration_admitted_under_clock_regression() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-clock-regression").await;
        let account = crate::accounts::create(
            &pool,
            "clock-regression-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2026, 1, 11, 17, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        crate::test_utils::enable_attendance(
            &pool,
            3,
            Utc.with_ymd_and_hms(2026, 1, 11, 19, 0, 0).unwrap(),
            2_000,
            7_200,
        )
        .await?;

        // The host clock regressed to 18:50Z after version 3 was saved at
        // 19:00Z. Admission uses the current version; rebuilding must retain it.
        let source_at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 50, 0).unwrap();
        earn_coin(&pool, &account.id, 400, source_at, offset()).await?;
        assert_eq!(
            progress_of(&pool, &account.id, "2026-01-12").await?,
            Some((3, 2_000, 7_200, 400))
        );

        sqlx::query("DELETE FROM attendance_daily_progress")
            .execute(&pool)
            .await?;
        let mut tx = begin_immediate(&pool).await?;
        verify_and_rebuild_progress(&mut tx).await?;
        tx.commit().await?;

        assert_eq!(
            progress_of(&pool, &account.id, "2026-01-12").await?,
            Some((3, 2_000, 7_200, 400))
        );
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    /// REC-2 / S2 startup rule: a projection mismatch is rebuilt from the
    /// immutable Contributions.
    #[tokio::test]
    async fn projection_mismatch_is_rebuilt_at_startup() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-rebuild").await;
        let account = crate::accounts::create(
            &pool,
            "rebuild-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        crate::test_utils::enable_attendance(
            &pool,
            3,
            Utc.with_ymd_and_hms(2026, 1, 11, 17, 0, 0).unwrap(),
            2_000,
            7_200,
        )
        .await?;
        // The first Contribution is admitted later but has the effective
        // version-3 snapshot. A Rejection Credit admitted second carries an
        // earlier original source instant and version 2.
        let first_source_at = Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap();
        earn_coin(&pool, &account.id, 400, first_source_at, offset()).await?;
        let rejection_at = Utc.with_ymd_and_hms(2026, 1, 11, 16, 0, 0).unwrap();
        let mut tx = begin_immediate(&pool).await?;
        let mut rejection = NewAuditEvent::simple(
            "coin_batch_rejected",
            AuditOutcome::Failure,
            AuditActor::System,
            "Rejected coin batch",
        );
        rejection.account_id = Some(&account.id);
        rejection.pc_id = Some("pc-01");
        let rejection_id = audit::append(&mut tx, rejection).await?;
        let mut credit = NewAuditEvent::simple(
            "rejection_credit",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Credited rejected coin batch",
        );
        credit.account_id = Some(&account.id);
        credit.amount_cents = Some(1_600);
        let credit_id = audit::append(&mut tx, credit).await?;
        record_earning(
            &mut tx,
            EarningInput {
                account_id: &account.id,
                source_audit_event_id: &credit_id,
                source_kind: SourceKind::RejectionCredit,
                cents: 1_600,
                source_at: rejection_at,
                offset: offset(),
                earning_date: date,
                award: AwardSnapshot {
                    configuration_version: 2,
                    daily_threshold_cents: 1_000,
                    reward_seconds: 3_600,
                },
                original_rejection_audit_event_id: Some(&rejection_id),
                trigger_pc_id: Some("pc-01"),
            },
        )
        .await?;
        tx.commit().await?;
        assert_eq!(check_in_count(&pool).await?, 1);

        let sources: Vec<String> = sqlx::query_scalar(
            "SELECT source_audit_event_id FROM attendance_contributions
             ORDER BY admission_sequence",
        )
        .fetch_all(&pool)
        .await?;
        // Corrupt every projected snapshot field and add a row with no source.
        sqlx::query(
            "UPDATE attendance_daily_progress
             SET configuration_version = 2, daily_threshold_cents = 1_000,
                 reward_seconds = 3_600, first_source_audit_event_id = ?, capped_cents = 500
             WHERE account_id = ? AND earning_date = '2026-01-12'",
        )
        .bind(&sources[1])
        .bind(&account.id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO attendance_daily_progress
               (account_id, earning_date, configuration_version,
                daily_threshold_cents, reward_seconds,
                first_source_audit_event_id, capped_cents)
             VALUES (?, '2026-01-13', 2, 1_000, 3_600, ?, 500)",
        )
        .bind(&account.id)
        .bind(&sources[0])
        .execute(&pool)
        .await?;

        let mut tx = begin_immediate(&pool).await?;
        verify_and_rebuild_progress(&mut tx).await?;
        tx.commit().await?;
        let (version, threshold, reward, capped) = progress_of(&pool, &account.id, "2026-01-12")
            .await?
            .unwrap();
        assert_eq!(
            (version, threshold, reward, capped),
            (3, 2_000, 7_200, 2_000)
        );
        let first_source: String = sqlx::query_scalar(
            "SELECT first_source_audit_event_id FROM attendance_daily_progress
             WHERE account_id = ? AND earning_date = '2026-01-12'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(first_source, sources[0]);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_daily_progress")
                .fetch_one(&pool)
                .await?,
            1
        );

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    fn serialize_attendance_snapshot(
        snapshot: &attendance_client::AttendanceAccountSnapshot,
    ) -> serde_json::Value {
        serde_json::json!({
            "summary": &snapshot.summary,
            "requested_month": snapshot
                .requested_month
                .as_ref()
                .expect("earning month exists"),
        })
    }

    /// REC-3: a mid-month restart preserves the daily projection and derived
    /// Account state.
    #[tokio::test]
    async fn mid_month_restart_rebuilds_identical_attendance_state() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-restart").await;
        let account = crate::accounts::create(
            &pool,
            "restart-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let timezone = offset();
        let earning_date = NaiveDate::from_ymd_opt(2099, 3, 15).unwrap();
        let month_start = earning_date.with_day(1).unwrap();
        let effective_at = timezone
            .from_local_datetime(&month_start.and_time(chrono::NaiveTime::MIN))
            .single()
            .expect("month start should be unambiguous")
            .to_utc();
        let test_now = timezone
            .from_local_datetime(&earning_date.and_hms_opt(12, 0, 0).unwrap())
            .single()
            .expect("mid-month instant should be unambiguous")
            .to_utc();
        let earning_month = "2099-03";
        let created_at = effective_at.to_rfc3339_opts(SecondsFormat::Millis, true);
        sqlx::query("UPDATE accounts SET created_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(&account.id)
            .execute(&pool)
            .await?;
        crate::test_utils::enable_attendance(&pool, 2, effective_at, 1_000, 3_600).await?;
        freeze_eligible_date(&pool, earning_date, timezone).await?;
        let source_at = timezone
            .from_local_datetime(&earning_date.and_time(chrono::NaiveTime::MIN))
            .single()
            .expect("local midnight should be unambiguous")
            .to_utc();
        earn_coin(&pool, &account.id, 1_000, source_at, timezone).await?;
        crate::db::attendance_earning::prepare_startup_state(&pool, test_now, timezone).await?;

        let before =
            attendance_client::account_snapshot(&pool, &account.id, Some(earning_month)).await?;
        let before_state = serialize_attendance_snapshot(&before);
        let date_key = earning_date.to_string();
        let before_progress = progress_of(&pool, &account.id, &date_key).await?;
        assert_eq!(before_progress, Some((2, 1_000, 3_600, 1_000)));

        pool.close().await;
        let restarted_pool = crate::test_utils::reopen_wal_test_pool(&path).await;
        crate::db::attendance_earning::prepare_startup_state(&restarted_pool, test_now, timezone)
            .await?;

        let after =
            attendance_client::account_snapshot(&restarted_pool, &account.id, Some(earning_month))
                .await?;
        let after_state = serialize_attendance_snapshot(&after);
        assert_eq!(
            progress_of(&restarted_pool, &account.id, &date_key).await?,
            before_progress
        );
        assert_eq!(after_state, before_state);

        restarted_pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[tokio::test]
    async fn migration_guards_reject_unsuccessful_and_mismatched_sources() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-source-guards").await;
        let account = crate::accounts::create(
            &pool,
            "source-guard-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;

        for (event_type, outcome) in [
            ("coin_accepted", AuditOutcome::Failure),
            ("manual_adjustment", AuditOutcome::Success),
        ] {
            let mut tx = begin_immediate(&pool).await?;
            let mut event = NewAuditEvent::simple(
                event_type,
                outcome,
                AuditActor::Admin,
                "Attendance source guard fixture",
            );
            event.account_id = Some(&account.id);
            event.amount_cents = Some(100);
            let event_id = audit::append(&mut tx, event).await?;
            tx.commit().await?;

            let error = sqlx::query(
                "INSERT INTO attendance_contributions
                   (source_audit_event_id, source_kind, configuration_version,
                    account_id, earning_date, cents, source_at)
                 VALUES (?, 'coin_accepted', 1, ?, '2026-01-12', 100,
                         '2026-01-11T18:00:00.000Z')",
            )
            .bind(event_id)
            .bind(&account.id)
            .execute(&pool)
            .await
            .expect_err("invalid linked Audit event must be rejected");
            assert!(error
                .to_string()
                .contains("attendance contribution source event mismatch"));
        }

        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn earning_months_are_immutable() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-month-immutable").await;
        let account = crate::accounts::create(
            &pool,
            "month-immutable-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        sqlx::query(
            "INSERT INTO attendance_earning_months
               (account_id, earning_month, claim_window_configuration_version,
                claim_window_months, claim_starts_at, deadline_at_exclusive,
                claim_local_dates)
             VALUES (?, '2026-01', 1, 1, '2026-01-31T16:00:00.000Z',
                     '2026-02-28T16:00:00.000Z',
                     '{\"start\":\"2026-02-01\",\"end\":\"2026-02-28\"}')",
        )
        .bind(&account.id)
        .execute(&pool)
        .await?;

        let update_error = sqlx::query(
            "UPDATE attendance_earning_months SET claim_window_months = 2
             WHERE account_id = ? AND earning_month = '2026-01'",
        )
        .bind(&account.id)
        .execute(&pool)
        .await
        .expect_err("Earning Month updates must be rejected");
        assert!(update_error
            .to_string()
            .contains("immutable attendance evidence"));
        let delete_error = sqlx::query(
            "DELETE FROM attendance_earning_months
             WHERE account_id = ? AND earning_month = '2026-01'",
        )
        .bind(&account.id)
        .execute(&pool)
        .await
        .expect_err("Earning Month deletes must be rejected");
        assert!(delete_error
            .to_string()
            .contains("immutable attendance evidence"));

        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn rollout_month_uses_earliest_available_settings_version() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-rollout-month").await;
        let account = crate::accounts::create(
            &pool,
            "rollout-month-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        // The first seeded setting arrives after January's local month start,
        // but before this earning. The check-in uses the earliest version.
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2026, 1, 11, 17, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        earn_coin(
            &pool,
            &account.id,
            1_000,
            Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap(),
            offset(),
        )
        .await?;

        let (daily_version, month_version, starts_at, deadline, local_dates): (
            i64,
            i64,
            String,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT p.configuration_version, m.claim_window_configuration_version,
                    m.claim_starts_at, m.deadline_at_exclusive, m.claim_local_dates
             FROM attendance_daily_progress p
             JOIN attendance_earning_months m ON m.account_id = p.account_id
             WHERE p.account_id = ? AND p.earning_date = '2026-01-12'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!((daily_version, month_version), (2, 2));
        assert_eq!(starts_at, "2026-01-31T16:00:00.000Z");
        assert_eq!(deadline, "2026-02-28T16:00:00.000Z");
        let local_dates: serde_json::Value = serde_json::from_str(&local_dates)
            .map_err(|error| AppError::InvalidInput(error.to_string()))?;
        assert_eq!(local_dates["start"], "2026-02-01");
        assert_eq!(local_dates["end"], "2026-02-28");

        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    /// DOM-11: a claim-window edit affects future Earning Months only; a
    /// month already created keeps its frozen deadline.
    #[tokio::test]
    async fn window_edit_leaves_existing_month_deadline_frozen() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-window").await;
        let account = crate::accounts::create(
            &pool,
            "window-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let date = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        freeze_eligible_date(&pool, date, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        earn_coin(
            &pool,
            &account.id,
            1_000,
            Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap(),
            offset(),
        )
        .await?;
        let (claim_starts_at, frozen_deadline, claim_local_dates): (String, String, String) =
            sqlx::query_as(
                "SELECT claim_starts_at, deadline_at_exclusive, claim_local_dates
                 FROM attendance_earning_months
                 WHERE account_id = ? AND earning_month = '2026-01'",
            )
            .bind(&account.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(claim_starts_at, "2026-01-31T16:00:00.000Z");
        assert_eq!(frozen_deadline, "2026-02-28T16:00:00.000Z");
        let claim_local_dates: serde_json::Value = serde_json::from_str(&claim_local_dates)
            .map_err(|error| AppError::InvalidInput(error.to_string()))?;
        assert_eq!(claim_local_dates["start"], "2026-02-01");
        assert_eq!(claim_local_dates["end"], "2026-02-28");

        // The owner widens the window mid-month.
        crate::test_utils::insert_attendance_settings_with_window(
            &pool,
            3,
            true,
            1_000,
            3_600,
            3,
            Utc.with_ymd_and_hms(2026, 1, 11, 19, 0, 0).unwrap(),
        )
        .await?;
        let deadline: String = sqlx::query_scalar(
            "SELECT deadline_at_exclusive FROM attendance_earning_months
             WHERE account_id = ? AND earning_month = '2026-01'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(deadline, frozen_deadline);
        let (frozen_version, frozen_window): (i64, i64) = sqlx::query_as(
            "SELECT claim_window_configuration_version, claim_window_months
             FROM attendance_earning_months
             WHERE account_id = ? AND earning_month = '2026-01'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!((frozen_version, frozen_window), (2, 1));

        // A February Check-In freezes the new version.
        let february = NaiveDate::from_ymd_opt(2026, 2, 5).unwrap();
        freeze_eligible_date(&pool, february, offset()).await?;
        earn_coin(
            &pool,
            &account.id,
            1_000,
            Utc.with_ymd_and_hms(2026, 2, 4, 18, 0, 0).unwrap(),
            offset(),
        )
        .await?;
        let (version, window): (i64, i64) = sqlx::query_as(
            "SELECT claim_window_configuration_version, claim_window_months
             FROM attendance_earning_months
             WHERE account_id = ? AND earning_month = '2026-02'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!((version, window), (3, 3));

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// CAL-1: a coin is credited wholly to the server-fixed commit-time date;
    /// no straddle split across dates.
    #[tokio::test]
    async fn coin_wholly_credited_to_commit_date_without_straddle() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-straddle").await;
        let account = crate::accounts::create(
            &pool,
            "straddle-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let day1 = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2026, 1, 13).unwrap();
        freeze_eligible_date(&pool, day1, offset()).await?;
        freeze_eligible_date(&pool, day2, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        // One coin at 23:59 local on the 12th (15:59Z), well past the local
        // midnight boundary; it must land on one date only.
        earn_coin(
            &pool,
            &account.id,
            1_000,
            Utc.with_ymd_and_hms(2026, 1, 12, 15, 59, 0).unwrap(),
            offset(),
        )
        .await?;
        let dates: Vec<(String, i64)> = sqlx::query_as(
            "SELECT earning_date, capped_cents FROM attendance_daily_progress
             WHERE account_id = ?",
        )
        .bind(&account.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(dates.len(), 1);
        assert_eq!(dates[0].0, "2026-01-12");
        assert_eq!(dates[0].1, 1_000);
        assert_eq!(check_in_count(&pool).await?, 1);
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// CAL-2: local midnight belongs to the new date; the prior date ends
    /// immediately before it.
    #[tokio::test]
    async fn local_midnight_is_inclusive_for_the_new_date_and_exclusive_for_the_prior_date(
    ) -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-midnight-boundary").await;
        let account = crate::accounts::create(
            &pool,
            "midnight-boundary-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let offset = offset();
        let day1 = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2026, 1, 13).unwrap();
        freeze_eligible_date(&pool, day1, offset).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            600,
        )
        .await?;
        let at_start = offset
            .from_local_datetime(&day1.and_time(chrono::NaiveTime::MIN))
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let at_end = offset
            .from_local_datetime(&day2.and_time(chrono::NaiveTime::MIN))
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let before_end = at_end - Duration::milliseconds(1);

        earn_coin(&pool, &account.id, 1_000, at_start, offset).await?;
        earn_coin(&pool, &account.id, 1_000, before_end, offset).await?;
        earn_coin(&pool, &account.id, 1_000, at_end, offset).await?;

        let dates: Vec<String> = sqlx::query_scalar(
            "SELECT earning_date FROM attendance_contributions
             WHERE account_id = ? ORDER BY admission_sequence",
        )
        .bind(&account.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            dates,
            [day1.to_string(), day1.to_string(), day2.to_string()]
        );
        assert_eq!(check_in_count(&pool).await?, 2);
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// CAL-5: downtime across midnight awards nothing retroactively; a
    /// skipped eligible day stays missed.
    #[tokio::test]
    async fn downtime_across_midnight_creates_no_retroactive_award() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-downtime").await;
        let account = crate::accounts::create(
            &pool,
            "downtime-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let day1 = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2026, 1, 13).unwrap();
        freeze_eligible_date(&pool, day1, offset()).await?;
        freeze_eligible_date(&pool, day2, offset()).await?;
        crate::test_utils::enable_attendance(
            &pool,
            2,
            Utc.with_ymd_and_hms(2025, 12, 31, 16, 0, 0).unwrap(),
            1_000,
            3_600,
        )
        .await?;
        // Earn on day 1 only; the shop is down across midnight and reopens
        // on day 2. The skipped day earned nothing.
        earn_coin(
            &pool,
            &account.id,
            1_000,
            Utc.with_ymd_and_hms(2026, 1, 11, 18, 0, 0).unwrap(),
            offset(),
        )
        .await?;
        // Forward jump to the 14th finalizes the skipped dates from evidence;
        // no coin ever landed on the 13th.
        let host_utc = Utc.with_ymd_and_hms(2026, 1, 14, 18, 0, 0).unwrap();
        finalize_calendar_dates(&pool, host_utc, offset()).await?;
        let progress: Vec<(String,)> = sqlx::query_as(
            "SELECT earning_date FROM attendance_daily_progress WHERE account_id = ?",
        )
        .bind(&account.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(progress, vec![("2026-01-12".to_owned(),)]);
        assert_eq!(check_in_count(&pool).await?, 1);
        // The skipped eligible date is frozen but never earned.
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM attendance_calendar_dates
                 WHERE earning_date IN ('2026-01-13', '2026-01-14')"
            )
            .fetch_one(&pool)
            .await?,
            2
        );
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}

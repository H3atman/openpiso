use chrono::{DateTime, SecondsFormat, Utc};
use openpiso_core::{
    attendance::{self as attendance_domain, EarningMonthBalance},
    audit::{AuditActor, AuditOutcome},
};
use serde_json::json;
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendanceClaim {
    pub audit_event_id: String,
    pub request_id: Uuid,
    pub account_id: String,
    pub earning_month_id: i64,
    pub earning_month: String,
    pub claim_pc_id: Option<String>,
    pub claimed_seconds: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendanceMonthProjection {
    pub id: i64,
    pub account_id: String,
    pub earning_month: String,
    pub claim_starts_at: DateTime<Utc>,
    pub deadline_at_exclusive: DateTime<Utc>,
    pub balance: EarningMonthBalance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDenialReason {
    MonthNotFound,
    NotYetClaimable,
    NothingOutstanding,
    Expired,
    RequestIdConflict,
}

impl ClaimDenialReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MonthNotFound => "month_not_found",
            Self::NotYetClaimable => "not_yet_claimable",
            Self::NothingOutstanding => "nothing_outstanding",
            Self::Expired => "expired",
            Self::RequestIdConflict => "request_id_conflict",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed {
        claim: AttendanceClaim,
        month: AttendanceMonthProjection,
    },
    Denied {
        reason: ClaimDenialReason,
        month: Option<AttendanceMonthProjection>,
    },
}

type MonthFactsRow = (i64, String, String, String, String, i64, i64, i64);

#[derive(Debug)]
struct MonthFacts {
    id: i64,
    account_id: String,
    earning_month: String,
    claim_starts_at: String,
    deadline_at_exclusive: String,
    earned_seconds: i64,
    claimed_seconds: i64,
    forfeited_seconds: i64,
}

impl From<MonthFactsRow> for MonthFacts {
    fn from(
        (
            id,
            account_id,
            earning_month,
            claim_starts_at,
            deadline_at_exclusive,
            earned_seconds,
            claimed_seconds,
            forfeited_seconds,
        ): MonthFactsRow,
    ) -> Self {
        Self {
            id,
            account_id,
            earning_month,
            claim_starts_at,
            deadline_at_exclusive,
            earned_seconds,
            claimed_seconds,
            forfeited_seconds,
        }
    }
}

#[derive(Debug)]
struct ForfeitureAllocation {
    earning_month_id: i64,
    forfeited_seconds: i64,
}

pub(crate) struct AccountDeletionForfeiturePlan {
    allocations: Vec<ForfeitureAllocation>,
    pub(crate) total_seconds: i64,
}

const MONTH_FACTS_SELECT: &str = "
    m.id,
    m.account_id,
    m.earning_month,
    m.claim_starts_at,
    m.deadline_at_exclusive,
    COALESCE((
      SELECT SUM(ci.reward_seconds) FROM attendance_check_ins ci
      WHERE ci.earning_month_id = m.id
    ), 0) AS earned_seconds,
    COALESCE((
      SELECT SUM(c.claimed_seconds) FROM attendance_claims c
      WHERE c.earning_month_id = m.id
    ), 0) AS claimed_seconds,
    COALESCE((
      SELECT SUM(f.forfeited_seconds) FROM attendance_forfeitures f
      WHERE f.earning_month_id = m.id
    ), 0) AS forfeited_seconds";

/// Claim all claimable Attendance Time from one Earning Month.
/// `clock` supplies the host instant after the writer is reserved; the durable
/// Attendance floor remains authoritative for expiry.
pub async fn claim(
    pool: &SqlitePool,
    clock: impl FnOnce() -> DateTime<Utc>,
    account_id: &str,
    pc_id: Option<&str>,
    request_id: Uuid,
    earning_month: &str,
) -> AppResult<ClaimOutcome> {
    super::attendance::validate_month_key(earning_month)?;
    let mut tx = crate::db::begin_immediate(pool).await?;
    let remaining_seconds = sqlx::query_scalar::<_, i64>(
        "SELECT remaining_seconds FROM accounts WHERE id = ? AND status = 'active'",
    )
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(remaining_seconds) = remaining_seconds else {
        tx.rollback().await?;
        return Err(AppError::Unauthorized("active account required".to_owned()));
    };

    let attendance_clock = crate::db::attendance::finalize_in_transaction(
        &mut tx,
        clock(),
        crate::db::attendance::host_offset(),
    )
    .await?;
    let now = attendance_clock.utc_floor;
    let request_id_text = request_id.to_string();

    if let Some(existing) = claim_by_request_id(&mut tx, &request_id_text).await? {
        if existing.account_id == account_id && existing.earning_month == earning_month {
            let month = month_projection_by_key(&mut tx, account_id, earning_month, now)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict("claimed Attendance Month is missing".to_owned())
                })?;
            tx.commit().await?;
            return Ok(ClaimOutcome::Claimed {
                claim: existing,
                month,
            });
        }

        let month = month_projection_by_key(&mut tx, account_id, earning_month, now).await?;
        record_denial(
            &mut tx,
            account_id,
            pc_id,
            request_id,
            earning_month,
            ClaimDenialReason::RequestIdConflict,
        )
        .await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::Denied {
            reason: ClaimDenialReason::RequestIdConflict,
            month,
        });
    }

    let Some(month) = month_projection_by_key(&mut tx, account_id, earning_month, now).await?
    else {
        record_denial(
            &mut tx,
            account_id,
            pc_id,
            request_id,
            earning_month,
            ClaimDenialReason::MonthNotFound,
        )
        .await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::Denied {
            reason: ClaimDenialReason::MonthNotFound,
            month: None,
        });
    };

    let denial = match month.balance.state {
        attendance_domain::EarningMonthState::Earning => Some(ClaimDenialReason::NotYetClaimable),
        attendance_domain::EarningMonthState::Claimable => None,
        attendance_domain::EarningMonthState::Claimed
        | attendance_domain::EarningMonthState::Forfeited => {
            Some(ClaimDenialReason::NothingOutstanding)
        }
        attendance_domain::EarningMonthState::Expired => Some(ClaimDenialReason::Expired),
    };
    if let Some(reason) = denial {
        record_denial(
            &mut tx,
            account_id,
            pc_id,
            request_id,
            earning_month,
            reason,
        )
        .await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::Denied {
            reason,
            month: Some(month),
        });
    }

    let claimed_seconds = month.balance.claimable_seconds;
    let updated_remaining_seconds = remaining_seconds
        .checked_add(claimed_seconds)
        .ok_or_else(|| AppError::Conflict("remaining time overflow".to_owned()))?;
    let updated =
        sqlx::query("UPDATE accounts SET remaining_seconds = ? WHERE id = ? AND status = 'active'")
            .bind(updated_remaining_seconds)
            .bind(account_id)
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() != 1 {
        tx.rollback().await?;
        return Err(AppError::Unauthorized("active account required".to_owned()));
    }

    let created_at =
        DateTime::<Utc>::from_timestamp_millis(now.timestamp_millis()).ok_or_else(|| {
            AppError::Internal("Attendance claim timestamp is out of range".to_owned())
        })?;
    let created_at_value = created_at.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut event = NewAuditEvent::simple(
        "attendance_claim",
        AuditOutcome::Success,
        AuditActor::Account,
        "Attendance Time claimed",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = pc_id;
    event.seconds_delta = Some(claimed_seconds);
    event.attendance_seconds_delta = Some(-claimed_seconds);
    event.metadata = json!({
        "request_id": request_id_text,
        "earning_month": earning_month,
        "claimed_seconds": claimed_seconds,
    });
    let audit_event_id = audit::append(&mut tx, event).await?;
    sqlx::query(
        "INSERT INTO attendance_claims
           (audit_event_id, request_id, account_id, earning_month_id,
            claim_pc_id, claimed_seconds, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&audit_event_id)
    .bind(&request_id_text)
    .bind(account_id)
    .bind(month.id)
    .bind(pc_id)
    .bind(claimed_seconds)
    .bind(&created_at_value)
    .execute(&mut *tx)
    .await?;

    let claim = AttendanceClaim {
        audit_event_id,
        request_id,
        account_id: account_id.to_owned(),
        earning_month_id: month.id,
        earning_month: earning_month.to_owned(),
        claim_pc_id: pc_id.map(str::to_owned),
        claimed_seconds,
        created_at,
    };
    let fresh_month = month_projection_by_key(&mut tx, account_id, earning_month, now)
        .await?
        .ok_or_else(|| AppError::Conflict("claimed Attendance Month is missing".to_owned()))?;
    tx.commit().await?;
    Ok(ClaimOutcome::Claimed {
        claim,
        month: fresh_month,
    })
}

async fn claim_by_request_id(
    tx: &mut Transaction<'_, Sqlite>,
    request_id: &str,
) -> AppResult<Option<AttendanceClaim>> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            String,
            Option<String>,
            i64,
            String,
        ),
    >(
        "SELECT c.audit_event_id, c.request_id, c.account_id, c.earning_month_id,
                m.earning_month, c.claim_pc_id, c.claimed_seconds, c.created_at
         FROM attendance_claims c
         JOIN attendance_earning_months m ON m.id = c.earning_month_id
         WHERE c.request_id = ?",
    )
    .bind(request_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(
        |(
            audit_event_id,
            request_id,
            account_id,
            earning_month_id,
            earning_month,
            claim_pc_id,
            claimed_seconds,
            created_at,
        )| {
            Ok(AttendanceClaim {
                audit_event_id,
                request_id: Uuid::parse_str(&request_id)
                    .map_err(|_| AppError::Conflict("invalid stored claim UUID".to_owned()))?,
                account_id,
                earning_month_id,
                earning_month,
                claim_pc_id,
                claimed_seconds,
                created_at: super::attendance::parse_stored_instant(&created_at)?,
            })
        },
    )
    .transpose()
}

async fn month_projection_by_key(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: &str,
    earning_month: &str,
    now: DateTime<Utc>,
) -> AppResult<Option<AttendanceMonthProjection>> {
    let query = format!(
        "SELECT {MONTH_FACTS_SELECT}
         FROM attendance_earning_months m
         WHERE m.account_id = ? AND m.earning_month = ?"
    );
    let facts = sqlx::query_as::<_, MonthFactsRow>(&query)
        .bind(account_id)
        .bind(earning_month)
        .fetch_optional(&mut **tx)
        .await?
        .map(MonthFacts::from);
    facts.map(|facts| project_month(facts, now)).transpose()
}

async fn month_facts_for_account(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: &str,
) -> AppResult<Vec<MonthFacts>> {
    let query = format!(
        "SELECT {MONTH_FACTS_SELECT}
         FROM attendance_earning_months m
         WHERE m.account_id = ?
         ORDER BY m.earning_month"
    );
    Ok(sqlx::query_as::<_, MonthFactsRow>(&query)
        .bind(account_id)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(MonthFacts::from)
        .collect())
}

fn project_month(facts: MonthFacts, now: DateTime<Utc>) -> AppResult<AttendanceMonthProjection> {
    let claim_starts_at = super::attendance::parse_stored_instant(&facts.claim_starts_at)?;
    let deadline_at_exclusive =
        super::attendance::parse_stored_instant(&facts.deadline_at_exclusive)?;
    let balance = attendance_domain::derive_earning_month_balance(
        now,
        claim_starts_at,
        deadline_at_exclusive,
        facts.earned_seconds,
        facts.claimed_seconds,
        facts.forfeited_seconds,
    )
    .map_err(|_| AppError::Conflict("attendance month does not reconcile".to_owned()))?;
    Ok(AttendanceMonthProjection {
        id: facts.id,
        account_id: facts.account_id,
        earning_month: facts.earning_month,
        claim_starts_at,
        deadline_at_exclusive,
        balance,
    })
}

async fn record_denial(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: &str,
    pc_id: Option<&str>,
    request_id: Uuid,
    earning_month: &str,
    reason: ClaimDenialReason,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "attendance_claim",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Attendance Claim denied",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = pc_id;
    event.metadata = json!({
        "reason": reason.as_str(),
        "request_id": request_id.to_string(),
        "earning_month": earning_month,
    });
    audit::append(tx, event).await?;
    Ok(())
}

pub(crate) async fn plan_account_deletion_forfeitures(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: &str,
    deleted_at: DateTime<Utc>,
) -> AppResult<AccountDeletionForfeiturePlan> {
    let facts = month_facts_for_account(tx, account_id).await?;
    let mut allocations = Vec::new();
    let mut total_seconds = 0_i64;
    for facts in facts {
        let month = project_month(facts, deleted_at)?;
        let forfeited_seconds = month.balance.outstanding_seconds;
        if forfeited_seconds > 0 {
            total_seconds = total_seconds
                .checked_add(forfeited_seconds)
                .ok_or_else(|| AppError::Conflict("Attendance forfeiture overflow".to_owned()))?;
            allocations.push(ForfeitureAllocation {
                earning_month_id: month.id,
                forfeited_seconds,
            });
        }
    }
    Ok(AccountDeletionForfeiturePlan {
        allocations,
        total_seconds,
    })
}

pub(crate) async fn record_account_deletion_forfeitures(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: &str,
    deletion_audit_event_id: &str,
    deleted_at: DateTime<Utc>,
    plan: &AccountDeletionForfeiturePlan,
) -> AppResult<()> {
    let deleted_at = deleted_at.to_rfc3339_opts(SecondsFormat::Millis, true);
    for allocation in &plan.allocations {
        sqlx::query(
            "INSERT INTO attendance_forfeitures
               (account_id, earning_month_id, deletion_audit_event_id,
                forfeited_seconds, deleted_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(account_id)
        .bind(allocation.earning_month_id)
        .bind(deletion_audit_event_id)
        .bind(allocation.forfeited_seconds)
        .bind(&deleted_at)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::{self, AccountCreator},
        audit::{self, NewAuditEvent},
        db::{
            attendance::{AwardSnapshot, SourceKind},
            attendance_earning::{self, EarningInput},
            begin_immediate,
        },
        errors::{AppError, AppResult},
        test_utils::{insert_attendance_settings_with_window, wal_test_pool_with_max_connections},
    };
    use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
    use openpiso_core::audit::{AuditActor, AuditOutcome};
    use sqlx::SqlitePool;
    use std::{future::Future, path::PathBuf, pin::Pin, task::Poll};
    use uuid::Uuid;

    type TestSetup = (SqlitePool, PathBuf, String);

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("test timestamp should be RFC 3339")
            .to_utc()
    }

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 60 * 60).expect("test offset should be valid")
    }

    fn local_midnight(date: NaiveDate) -> DateTime<Utc> {
        offset()
            .from_local_datetime(&date.and_time(chrono::NaiveTime::MIN))
            .single()
            .expect("test midnight should be unambiguous")
            .to_utc()
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("test date should be valid")
    }

    fn month_key(date: NaiveDate) -> String {
        format!("{:04}-{:02}", date.year(), date.month())
    }

    async fn setup(name: &str, earning_dates: &[NaiveDate]) -> AppResult<TestSetup> {
        setup_with_max_connections(name, earning_dates, 5).await
    }

    async fn setup_with_max_connections(
        name: &str,
        earning_dates: &[NaiveDate],
        max_connections: u32,
    ) -> AppResult<TestSetup> {
        let (pool, path) = wal_test_pool_with_max_connections(
            &format!("attendance-claim-{name}"),
            max_connections,
        )
        .await;
        let account = accounts::create(
            &pool,
            &format!("claim-{name}"),
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        let effective_at = earning_dates
            .iter()
            .min()
            .and_then(|date| date.with_day(1))
            .and_then(|month_start| month_start.pred_opt())
            .map(local_midnight)
            .unwrap_or_else(|| utc("2026-01-01T00:00:00Z"));
        let initial_open_date = effective_at.with_timezone(&offset()).date_naive();
        sqlx::query(
            "UPDATE attendance_clock_state
             SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(effective_at.to_rfc3339_opts(SecondsFormat::Millis, true))
        .bind(initial_open_date.to_string())
        .execute(&pool)
        .await?;
        insert_attendance_settings_with_window(&pool, 2, true, 100, 600, 1, effective_at).await?;
        for earning_date in earning_dates {
            earn_award(&pool, &account.id, *earning_date).await?;
        }
        Ok((pool, path, account.id))
    }

    async fn poll_once_to_pending<F: Future>(mut future: Pin<&mut F>) {
        std::future::poll_fn(|context| match future.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("writer should wait for the held test connection"),
        })
        .await;
    }

    async fn earn_award(
        pool: &SqlitePool,
        account_id: &str,
        earning_date: NaiveDate,
    ) -> AppResult<()> {
        let source_at = offset()
            .from_local_datetime(&earning_date.and_hms_opt(12, 0, 0).expect("noon is valid"))
            .single()
            .expect("test noon should be unambiguous")
            .to_utc();
        let mut tx = begin_immediate(pool).await?;
        let mut source = NewAuditEvent::simple(
            "coin_accepted",
            AuditOutcome::Success,
            AuditActor::CoinDevice,
            "Accepted Attendance test coin",
        );
        source.account_id = Some(account_id);
        source.amount_cents = Some(100);
        let source_audit_event_id = audit::append(&mut tx, source).await?;
        attendance_earning::record_earning(
            &mut tx,
            EarningInput {
                account_id,
                source_audit_event_id: &source_audit_event_id,
                source_kind: SourceKind::AcceptedCoin,
                cents: 100,
                source_at,
                offset: offset(),
                earning_date,
                award: AwardSnapshot {
                    configuration_version: 2,
                    daily_threshold_cents: 100,
                    reward_seconds: 600,
                },
                original_rejection_audit_event_id: None,
                trigger_pc_id: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn claim_at(
        pool: &SqlitePool,
        account_id: &str,
        request_id: Uuid,
        earning_month: &str,
        now: DateTime<Utc>,
    ) -> AppResult<ClaimOutcome> {
        claim(
            pool,
            || now,
            account_id,
            Some("pc-01"),
            request_id,
            earning_month,
        )
        .await
    }

    async fn close(pool: SqlitePool, path: PathBuf) -> AppResult<()> {
        pool.close().await;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn claim_credits_all_attendance_seconds_with_matching_audit_and_detail() -> AppResult<()>
    {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("success", &[earning_date]).await?;
        let remaining_before: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let mut tx = begin_immediate(&pool).await?;
        let month_before =
            month_projection_by_key(&mut tx, &account_id, "2026-05", utc("2026-06-15T00:00:00Z"))
                .await?
                .expect("earned month should exist before claim");
        tx.rollback().await?;
        assert_eq!(remaining_before, 0);
        assert_eq!(month_before.balance.outstanding_seconds, 600);

        let request_id = Uuid::new_v4();
        let outcome = claim_at(
            &pool,
            &account_id,
            request_id,
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        let ClaimOutcome::Claimed { claim, month } = outcome else {
            panic!("claim should succeed")
        };
        assert_eq!(claim.claimed_seconds, 600);
        assert_eq!(claim.claim_pc_id.as_deref(), Some("pc-01"));
        assert_eq!(claim.request_id, request_id);
        assert_eq!(month.balance.earned_seconds, 600);
        assert_eq!(month.balance.claimed_seconds, 600);
        assert_eq!(month.balance.outstanding_seconds, 0);
        assert_eq!(
            month.balance.state,
            openpiso_core::attendance::EarningMonthState::Claimed
        );

        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 600);
        let audit: (String, String, Option<String>, Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT event_type, outcome, pc_id, seconds_delta, attendance_seconds_delta
             FROM audit_log WHERE id = ?",
        )
        .bind(&claim.audit_event_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            audit,
            (
                "attendance_claim".to_owned(),
                "success".to_owned(),
                Some("pc-01".to_owned()),
                Some(600),
                Some(-600)
            )
        );
        let detail: (String, String, String, Option<String>, i64) = sqlx::query_as(
            "SELECT audit_event_id, request_id, account_id, claim_pc_id, claimed_seconds
             FROM attendance_claims",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            detail,
            (
                claim.audit_event_id,
                request_id.to_string(),
                account_id,
                Some("pc-01".to_owned()),
                600
            )
        );
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_retry_returns_original_claim_without_double_credit() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("retry", &[earning_date]).await?;
        let request_id = Uuid::new_v4();
        let now = utc("2026-06-15T00:00:00Z");
        let first = claim_at(&pool, &account_id, request_id, "2026-05", now).await?;
        let retry = claim_at(&pool, &account_id, request_id, "2026-05", now).await?;
        let (
            ClaimOutcome::Claimed {
                claim: original, ..
            },
            ClaimOutcome::Claimed {
                claim: repeated, ..
            },
        ) = (first, retry)
        else {
            panic!("both attempts should return the original claim")
        };
        assert_eq!(repeated, original);
        let balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, 600);
        assert_eq!(counts, (1, 1));
        close(pool, path).await
    }

    #[tokio::test]
    async fn deleted_account_cannot_replay_attendance_claim_uuid() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("deleted-replay", &[earning_date]).await?;
        let request_id = Uuid::new_v4();
        claim_at(
            &pool,
            &account_id,
            request_id,
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        accounts::soft_delete_many_ids(&pool, std::slice::from_ref(&account_id)).await?;

        let before: (i64, i64, i64, String) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'),
                    (SELECT COUNT(*) FROM attendance_forfeitures),
                    (SELECT utc_floor FROM attendance_clock_state WHERE id = 1)",
        )
        .fetch_one(&pool)
        .await?;
        let replay = claim_at(
            &pool,
            &account_id,
            request_id,
            "2026-05",
            utc("2030-01-01T00:00:00Z"),
        )
        .await;
        assert!(matches!(replay, Err(AppError::Unauthorized(_))));
        let after: (i64, i64, i64, String) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'),
                    (SELECT COUNT(*) FROM attendance_forfeitures),
                    (SELECT utc_floor FROM attendance_clock_state WHERE id = 1)",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(after, before);
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_request_id_reused_for_another_month_is_audited_conflict() -> AppResult<()> {
        let dates = [date(2026, 5, 15), date(2026, 6, 15)];
        let (pool, path, account_id) = setup("request-conflict", &dates).await?;
        let request_id = Uuid::new_v4();
        let first = claim_at(
            &pool,
            &account_id,
            request_id,
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(first, ClaimOutcome::Claimed { .. }));
        let retry = claim_at(
            &pool,
            &account_id,
            request_id,
            "2026-06",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(
            retry,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::RequestIdConflict,
                ..
            }
        ));
        let reasons: Vec<String> = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason') FROM audit_log
             WHERE event_type = 'attendance_claim' AND outcome = 'denied' ORDER BY rowid",
        )
        .fetch_all(&pool)
        .await?;
        let details: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attendance_claims")
            .fetch_one(&pool)
            .await?;
        assert_eq!(reasons, ["request_id_conflict"]);
        assert_eq!(details, 1);
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_before_window_is_denied_and_audited() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("not-yet", &[earning_date]).await?;
        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-05-31T15:59:59Z"),
        )
        .await?;
        assert!(matches!(
            outcome,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::NotYetClaimable,
                ..
            }
        ));
        let balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let denial: (String, Option<i64>, Option<i64>, i64) = sqlx::query_as(
            "SELECT outcome, seconds_delta, attendance_seconds_delta,
                    (SELECT COUNT(*) FROM attendance_claims)
             FROM audit_log WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, 0);
        assert_eq!(denial, ("denied".to_owned(), None, None, 0));
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_of_missing_month_is_audited_without_a_detail() -> AppResult<()> {
        let (pool, path, account_id) = setup("missing-month", &[]).await?;
        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(
            outcome,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::MonthNotFound,
                month: None
            }
        ));
        let reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason') FROM audit_log
             WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "month_not_found");
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_at_exclusive_deadline_is_denied_as_expired() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("expired", &[earning_date]).await?;
        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-30T16:00:00Z"),
        )
        .await?;
        assert!(matches!(
            outcome,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::Expired,
                ..
            }
        ));
        let reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason') FROM audit_log
             WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "expired");
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_uses_persisted_utc_floor_for_expiry() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("floor-expiry", &[earning_date]).await?;
        sqlx::query("UPDATE attendance_clock_state SET utc_floor = ? WHERE id = 1")
            .bind("2026-07-02T00:00:00.000Z")
            .execute(&pool)
            .await?;

        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(
            outcome,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::Expired,
                ..
            }
        ));
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_after_a_full_claim_denies_with_nothing_outstanding() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("nothing-outstanding", &[earning_date]).await?;
        claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-16T00:00:00Z"),
        )
        .await?;
        assert!(matches!(
            outcome,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::NothingOutstanding,
                ..
            }
        ));
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'
                     AND outcome = 'denied')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(counts, (1, 1));
        close(pool, path).await
    }

    #[tokio::test]
    async fn late_check_in_reopens_claimed_month_for_supplemental_claim() -> AppResult<()> {
        let dates = [date(2026, 5, 15), date(2026, 5, 20)];
        let (pool, path, account_id) = setup("late-credit", &dates[..1]).await?;
        let first = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(first, ClaimOutcome::Claimed { .. }));
        earn_award(&pool, &account_id, dates[1]).await?;
        let late_now = utc("2026-06-20T00:00:00Z");
        let mut tx = begin_immediate(&pool).await?;
        let reopened = month_projection_by_key(&mut tx, &account_id, "2026-05", late_now)
            .await?
            .expect("late Check-In should retain its Earning Month");
        tx.rollback().await?;
        assert_eq!(
            reopened.balance.state,
            attendance_domain::EarningMonthState::Claimable
        );
        assert_eq!(reopened.balance.claimable_seconds, 600);

        let supplemental =
            claim_at(&pool, &account_id, Uuid::new_v4(), "2026-05", late_now).await?;
        let ClaimOutcome::Claimed { claim, month } = supplemental else {
            panic!("late award should reopen the month")
        };
        assert_eq!(claim.claimed_seconds, 600);
        assert_eq!(month.balance.earned_seconds, 1_200);
        assert_eq!(month.balance.claimed_seconds, 1_200);
        let balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(balance, 1_200);
        close(pool, path).await
    }

    #[tokio::test]
    async fn late_check_in_after_deadline_remains_expired() -> AppResult<()> {
        let host_offset = crate::db::attendance::host_offset();
        let today = Utc::now().with_timezone(&host_offset).date_naive();
        let month_start = today.with_day(1).expect("month start should exist");
        let seed_date = month_start
            .with_day(if today.day() == 1 { 2 } else { 1 })
            .expect("seed date should exist");
        let late_date = month_start
            .with_day(if today.day() == 1 { 1 } else { 2 })
            .expect("late earning date should exist");
        let (pool, path, account_id) = setup("late-check-in-expired", &[seed_date]).await?;
        let earning_month = month_key(seed_date);
        let deadline_date = month_start
            .checked_add_months(chrono::Months::new(2))
            .expect("claim deadline should exist");
        let deadline = host_offset
            .from_local_datetime(&deadline_date.and_time(chrono::NaiveTime::MIN))
            .single()
            .expect("claim deadline should be unambiguous")
            .to_utc();

        let expired =
            claim_at(&pool, &account_id, Uuid::new_v4(), &earning_month, deadline).await?;
        assert!(matches!(
            expired,
            ClaimOutcome::Denied {
                reason: ClaimDenialReason::Expired,
                ..
            }
        ));

        earn_award(&pool, &account_id, late_date).await?;
        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            &earning_month,
            Utc::now(),
        )
        .await?;
        let ClaimOutcome::Denied {
            reason,
            month: Some(month),
        } = outcome
        else {
            panic!("late Check-In should stay expired")
        };
        assert_eq!(reason, ClaimDenialReason::Expired);
        assert_eq!(
            month.balance.state,
            attendance_domain::EarningMonthState::Expired
        );
        assert_eq!(month.balance.earned_seconds, 1_200);
        assert_eq!(month.balance.outstanding_seconds, 0);
        assert_eq!(month.balance.claimable_seconds, 0);
        assert_eq!(month.balance.expired_seconds, 1_200);
        close(pool, path).await
    }

    async fn late_credit_and_expiry_claim_in_order(name: &str, claim_first: bool) -> AppResult<()> {
        let dates = [date(2026, 5, 15), date(2026, 5, 20)];
        let (pool, path, account_id) = setup_with_max_connections(name, &dates[..1], 1).await?;
        claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;

        let deadline = local_midnight(date(2026, 7, 1));
        let held = begin_immediate(&pool).await?;
        let mut claim = Box::pin(claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            deadline,
        ));
        let mut credit = Box::pin(earn_award(&pool, &account_id, dates[1]));
        if claim_first {
            poll_once_to_pending(claim.as_mut()).await;
            poll_once_to_pending(credit.as_mut()).await;
        } else {
            poll_once_to_pending(credit.as_mut()).await;
            poll_once_to_pending(claim.as_mut()).await;
        }
        held.commit().await?;

        let (outcome, credit_result) = tokio::join!(claim, credit);
        credit_result?;
        let outcome = outcome?;
        let ClaimOutcome::Denied {
            reason,
            month: Some(month_at_claim),
        } = outcome
        else {
            panic!("claim should be denied")
        };
        assert_eq!(
            reason,
            if claim_first {
                ClaimDenialReason::NothingOutstanding
            } else {
                ClaimDenialReason::Expired
            }
        );
        assert_eq!(
            month_at_claim.balance.state,
            if claim_first {
                attendance_domain::EarningMonthState::Claimed
            } else {
                attendance_domain::EarningMonthState::Expired
            }
        );
        assert_eq!(
            month_at_claim.balance.earned_seconds,
            if claim_first { 600 } else { 1_200 }
        );

        let mut tx = begin_immediate(&pool).await?;
        let month = month_projection_by_key(&mut tx, &account_id, "2026-05", deadline)
            .await?
            .expect("earned month should remain available for reconciliation");
        tx.rollback().await?;
        let totals: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT COALESCE((SELECT SUM(reward_seconds) FROM attendance_check_ins), 0),
                    COALESCE((SELECT SUM(claimed_seconds) FROM attendance_claims), 0),
                    (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'),
                    (SELECT remaining_seconds FROM accounts WHERE id = ?)",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(totals, (1_200, 600, 1, 2, 600));
        assert_eq!(
            month.balance.state,
            attendance_domain::EarningMonthState::Expired
        );
        assert_eq!(month.balance.earned_seconds, 1_200);
        assert_eq!(month.balance.claimed_seconds, 600);
        assert_eq!(month.balance.expired_seconds, 600);
        assert_eq!(month.balance.outstanding_seconds, 0);
        close(pool, path).await
    }

    #[tokio::test]
    async fn late_credit_and_expiry_claim_serialize_in_both_orders() -> AppResult<()> {
        late_credit_and_expiry_claim_in_order("claim-first", true).await?;
        late_credit_and_expiry_claim_in_order("credit-first", false).await
    }

    #[tokio::test]
    async fn claim_succeeds_while_attendance_is_disabled_and_shop_closed() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("disabled-closed", &[earning_date]).await?;
        insert_attendance_settings_with_window(
            &pool,
            3,
            false,
            100,
            600,
            1,
            utc("2026-06-01T00:00:00Z"),
        )
        .await?;
        sqlx::query("UPDATE shop_hours_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        sqlx::query(
            "UPDATE shop_hours_schedule
             SET mode = 'closed_all_day', open_minute = NULL, close_minute = NULL",
        )
        .execute(&pool)
        .await?;
        assert!(!crate::db::shop_hours::shop_status(&pool).await?.open);

        let outcome = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(outcome, ClaimOutcome::Claimed { .. }));
        close(pool, path).await
    }

    #[tokio::test]
    async fn concurrent_claims_for_one_month_credit_only_once() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("concurrent", &[earning_date]).await?;
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let first_now = utc("2026-06-15T00:00:00Z");
        let second_now = first_now;
        let (first, second) = tokio::join!(
            claim_at(&pool, &account_id, first_id, "2026-05", first_now),
            claim_at(&pool, &account_id, second_id, "2026-05", second_now),
        );
        let first = first?;
        let second = second?;
        let outcomes = [&first, &second];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ClaimOutcome::Claimed { .. }))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    ClaimOutcome::Denied {
                        reason: ClaimDenialReason::NothingOutstanding,
                        ..
                    }
                ))
                .count(),
            1
        );
        let balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, 600);
        assert_eq!(counts, (1, 2));
        close(pool, path).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_claim_and_account_deletion_reconcile_in_either_order() -> AppResult<()> {
        let now = Utc::now();
        let current_month = now
            .with_timezone(&offset())
            .date_naive()
            .with_day(1)
            .expect("month start should exist");
        let earning_month_start = current_month
            .checked_sub_months(chrono::Months::new(1))
            .expect("previous month should exist");
        let first_date = earning_month_start
            .with_day(10)
            .expect("first earning date should exist");
        let second_date = earning_month_start
            .with_day(20)
            .expect("second earning date should exist");
        let (pool, path, account_id) = setup("claim-delete-race", &[first_date]).await?;
        let earning_month = month_key(first_date);
        let initial_claim =
            claim_at(&pool, &account_id, Uuid::new_v4(), &earning_month, now).await?;
        assert!(matches!(
            initial_claim,
            ClaimOutcome::Claimed { claim, .. } if claim.claimed_seconds == 600
        ));
        earn_award(&pool, &account_id, second_date).await?;
        let before: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;

        let claim = claim_at(&pool, &account_id, Uuid::new_v4(), &earning_month, now);
        let deletion = accounts::soft_delete_many_ids(&pool, std::slice::from_ref(&account_id));
        let (claim_result, deletion_result) = tokio::join!(claim, deletion);
        deletion_result?;
        let after: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;
        let deletion_delta: Option<i64> = sqlx::query_scalar(
            "SELECT attendance_seconds_delta FROM audit_log
             WHERE event_type = 'account_deleted' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;

        let claimed_during_race = match claim_result {
            Ok(ClaimOutcome::Claimed { claim, .. }) => {
                assert_eq!(claim.claimed_seconds, 600);
                assert_eq!(after, (before.0 + 1, before.1 + 1));
                true
            }
            Err(AppError::Unauthorized(_)) => {
                assert_eq!(after, before);
                false
            }
            other => panic!("unexpected claim/deletion race result: {other:?}"),
        };

        let mut tx = begin_immediate(&pool).await?;
        let month = month_projection_by_key(&mut tx, &account_id, &earning_month, now)
            .await?
            .expect("race month should remain in the ledger");
        tx.rollback().await?;
        let balance = month.balance;
        assert_eq!(balance.earned_seconds, 1_200);
        assert_eq!(
            balance.earned_seconds,
            balance.claimed_seconds
                + balance.outstanding_seconds
                + balance.expired_seconds
                + balance.forfeited_seconds
        );
        if claimed_during_race {
            assert_eq!(balance.claimed_seconds, 1_200);
            assert_eq!(balance.forfeited_seconds, 0);
            assert_eq!(deletion_delta, None);
        } else {
            assert_eq!(balance.claimed_seconds, 600);
            assert_eq!(balance.forfeited_seconds, 600);
            assert_eq!(deletion_delta, Some(-600));
        }
        close(pool, path).await
    }

    #[tokio::test]
    async fn deletion_queued_before_claim_commits_first() -> AppResult<()> {
        let now = Utc::now();
        let earning_month_start = now
            .with_timezone(&offset())
            .date_naive()
            .with_day(1)
            .expect("month start should exist")
            .checked_sub_months(chrono::Months::new(1))
            .expect("earning month should exist");
        let earning_dates = [
            earning_month_start
                .with_day(15)
                .expect("first earning date should exist"),
            earning_month_start
                .with_day(20)
                .expect("late earning date should exist"),
        ];
        let (pool, path, account_id) =
            setup_with_max_connections("deletion-wins", &earning_dates[..1], 1).await?;
        let earning_month = month_key(earning_dates[0]);
        let initial_claim =
            claim_at(&pool, &account_id, Uuid::new_v4(), &earning_month, now).await?;
        assert!(matches!(
            initial_claim,
            ClaimOutcome::Claimed { claim, .. } if claim.claimed_seconds == 600
        ));
        earn_award(&pool, &account_id, earning_dates[1]).await?;

        let before: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(before, (1, 1));

        let held_write = begin_immediate(&pool).await?;
        let mut deletion = Box::pin(accounts::soft_delete_many_ids(
            &pool,
            std::slice::from_ref(&account_id),
        ));
        poll_once_to_pending(deletion.as_mut()).await;
        let mut claim = Box::pin(claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            &earning_month,
            now,
        ));
        poll_once_to_pending(claim.as_mut()).await;
        held_write.commit().await?;

        assert_eq!(
            deletion.await?.as_slice(),
            std::slice::from_ref(&account_id)
        );
        assert!(matches!(claim.await, Err(AppError::Unauthorized(_))));

        let after: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;
        let deletion_delta: Option<i64> = sqlx::query_scalar(
            "SELECT attendance_seconds_delta FROM audit_log
             WHERE event_type = 'account_deleted' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(after, before);
        assert_eq!(deletion_delta, Some(-600));

        let mut tx = begin_immediate(&pool).await?;
        let month = month_projection_by_key(&mut tx, &account_id, &earning_month, now)
            .await?
            .expect("deleted Account month should remain in the ledger");
        tx.rollback().await?;
        assert_eq!(month.balance.claimed_seconds, 600);
        assert_eq!(month.balance.outstanding_seconds, 0);
        assert_eq!(month.balance.forfeited_seconds, 600);
        close(pool, path).await
    }

    #[tokio::test]
    async fn account_deletion_forfeits_only_unclaimed_unexpired_seconds() -> AppResult<()> {
        let now = Utc::now();
        let current_date = now.with_timezone(&offset()).date_naive();
        let current_month = current_date.with_day(1).expect("month start should exist");
        let expired_date = current_month
            .checked_sub_months(chrono::Months::new(2))
            .and_then(|month| month.with_day(10))
            .expect("expired month date should exist");
        let claimed_date = current_month
            .checked_sub_months(chrono::Months::new(1))
            .and_then(|month| month.with_day(10))
            .expect("claimed month date should exist");
        let earning_dates = [expired_date, claimed_date, current_date];
        let (pool, path, account_id) = setup("deletion", &earning_dates).await?;
        let claimed_month = month_key(claimed_date);
        let outstanding_month = month_key(current_date);
        claim_at(&pool, &account_id, Uuid::new_v4(), &claimed_month, now).await?;

        accounts::soft_delete_many_ids(&pool, std::slice::from_ref(&account_id)).await?;
        let forfeitures: Vec<(String, i64)> = sqlx::query_as(
            "SELECT m.earning_month, f.forfeited_seconds
             FROM attendance_forfeitures f
             JOIN attendance_earning_months m ON m.id = f.earning_month_id
             WHERE f.account_id = ? ORDER BY m.earning_month",
        )
        .bind(&account_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(forfeitures, [(outstanding_month.clone(), 600)]);
        let deletion_delta: Option<i64> = sqlx::query_scalar(
            "SELECT attendance_seconds_delta FROM audit_log
             WHERE event_type = 'account_deleted' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(deletion_delta, Some(-600));

        let claim_audits_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        let deleted_claim =
            claim_at(&pool, &account_id, Uuid::new_v4(), &outstanding_month, now).await;
        assert!(matches!(deleted_claim, Err(AppError::Unauthorized(_))));
        let claim_audits_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(claim_audits_before, claim_audits_after);
        close(pool, path).await
    }

    #[tokio::test]
    async fn account_deletion_uses_persisted_utc_floor_for_expiry() -> AppResult<()> {
        let earning_date = Utc::now().with_timezone(&offset()).date_naive();
        let month_start = earning_date.with_day(1).expect("month start should exist");
        let deadline_date = month_start
            .checked_add_months(chrono::Months::new(2))
            .expect("claim deadline should exist");
        let future_floor = local_midnight(deadline_date)
            .checked_add_signed(chrono::Duration::seconds(1))
            .expect("future floor should be representable")
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let (pool, path, account_id) = setup("deletion-floor", &[earning_date]).await?;
        sqlx::query("UPDATE attendance_clock_state SET utc_floor = ? WHERE id = 1")
            .bind(future_floor)
            .execute(&pool)
            .await?;

        accounts::soft_delete_many_ids(&pool, std::slice::from_ref(&account_id)).await?;
        let forfeiture_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM attendance_forfeitures WHERE account_id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let deletion_delta: Option<i64> = sqlx::query_scalar(
            "SELECT attendance_seconds_delta FROM audit_log
             WHERE event_type = 'account_deleted' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(forfeiture_count, 0);
        assert_eq!(deletion_delta, None);
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_failure_after_balance_and_audit_rolls_back_every_write() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("rollback", &[earning_date]).await?;
        sqlx::query(
            "CREATE TRIGGER fail_attendance_claim_detail
             BEFORE INSERT ON attendance_claims
             BEGIN SELECT RAISE(ABORT, 'injected claim detail failure'); END",
        )
        .execute(&pool)
        .await?;
        let request_id = Uuid::new_v4();
        let result = claim_at(
            &pool,
            &account_id,
            request_id,
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await;
        assert!(matches!(result, Err(AppError::Database(_))));
        sqlx::query("DROP TRIGGER fail_attendance_claim_detail")
            .execute(&pool)
            .await?;
        let balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, 0);
        assert_eq!(counts, (0, 0));
        pool.close().await;

        let reopened = crate::test_utils::reopen_wal_test_pool(&path).await;
        let retried = claim_at(
            &reopened,
            &account_id,
            request_id,
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await?;
        assert!(matches!(
            retried,
            ClaimOutcome::Claimed { claim, .. } if claim.claimed_seconds == 600
        ));
        let recovered_balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&reopened)
                .await?;
        let recovered_counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM attendance_claims),
                    (SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'
                     AND outcome = 'success')",
        )
        .fetch_one(&reopened)
        .await?;
        assert_eq!(recovered_balance, 600);
        assert_eq!(recovered_counts, (1, 1));
        close(reopened, path).await
    }

    #[tokio::test]
    async fn claim_overflow_does_not_clamp_or_append_audit() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("overflow", &[earning_date]).await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = ? WHERE id = ?")
            .bind(i64::MAX)
            .bind(&account_id)
            .execute(&pool)
            .await?;
        let result = claim_at(
            &pool,
            &account_id,
            Uuid::new_v4(),
            "2026-05",
            utc("2026-06-15T00:00:00Z"),
        )
        .await;
        assert!(matches!(result, Err(AppError::Conflict(_))));
        let balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, i64::MAX);
        assert_eq!(audits, 0);
        close(pool, path).await
    }

    #[tokio::test]
    async fn claim_rejects_noncanonical_month_without_audit() -> AppResult<()> {
        let earning_date = date(2026, 5, 15);
        let (pool, path, account_id) = setup("invalid-month", &[earning_date]).await?;
        let result = claim(
            &pool,
            || utc("2026-06-15T00:00:00Z"),
            &account_id,
            Some("pc-01"),
            Uuid::new_v4(),
            "2026-5",
        )
        .await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'attendance_claim'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 0);
        close(pool, path).await
    }
}

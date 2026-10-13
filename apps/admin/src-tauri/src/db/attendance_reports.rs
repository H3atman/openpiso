use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use openpiso_core::{
    attendance::{
        self as attendance_domain, AttendanceEffectiveBonus, EarningMonthBalance, EarningMonthState,
    },
    pricing::PricingError,
    report::ReportPeriod,
};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool, Transaction};

use crate::errors::{AppError, AppResult};

const DEFAULT_PAGE_LIMIT: i64 = 50;
const MAX_PAGE_LIMIT: i64 = 200;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttendanceReportInput {
    pub period: ReportPeriod,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub month_limit: Option<i64>,
    pub month_offset: Option<i64>,
    pub account_limit: Option<i64>,
    pub account_offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceReport {
    pub period: ReportPeriod,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub as_of: DateTime<Utc>,
    pub admin_local_date: NaiveDate,
    pub admin_local_month: String,
    pub flow_totals: AttendanceFlowTotals,
    pub flow_buckets: Vec<AttendanceFlowBucket>,
    pub lifetime_totals: AttendanceLifetimeTotals,
    pub liability: AttendanceLiability,
    pub month_page: AttendancePage<AttendanceEarningMonthRow>,
    pub account_page: AttendancePage<AttendanceAccountRow>,
    pub economics_advisory: Option<AttendanceEconomicsAdvisory>,
    pub clock_regression_advisory: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AttendanceFlowTotals {
    pub check_ins: i64,
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub expired_seconds: i64,
    pub forfeited_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceFlowBucket {
    pub period_label: String,
    #[serde(flatten)]
    pub totals: AttendanceFlowTotals,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AttendanceLifetimeTotals {
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub outstanding_seconds: i64,
    pub expired_seconds: i64,
    pub forfeited_seconds: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AttendanceLiability {
    pub outstanding_seconds: i64,
    pub claimable_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct AttendanceSettingsSnapshot {
    pub settings: super::attendance::AttendanceSettingsVersion,
    pub economics_advisory: Option<AttendanceEconomicsAdvisory>,
    pub liability: AttendanceLiability,
    pub next_earning_month: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceEarningMonthRow {
    pub account_id: String,
    pub username: String,
    pub earning_month: String,
    pub state: EarningMonthState,
    pub claim_starts_at: DateTime<Utc>,
    pub deadline_at_exclusive: DateTime<Utc>,
    pub claim_deadline_local_date: String,
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub outstanding_seconds: i64,
    pub expired_seconds: i64,
    pub forfeited_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceAccountRow {
    pub account_id: String,
    pub username: String,
    pub check_ins: i64,
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub outstanding_seconds: i64,
    pub claimable_seconds: i64,
    pub expired_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendanceEconomicsAdvisory {
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
    pub rate_amount_cents: i64,
    pub rate_seconds: i64,
    pub effective_bonus: AttendanceEffectiveBonus,
    pub unsafe_bonus: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttendancePage<T> {
    pub rows: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy)]
struct PageBounds {
    limit: i64,
    offset: i64,
}

#[derive(Debug, Clone, Copy)]
struct DateRange {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

impl DateRange {
    fn includes(self, date: NaiveDate) -> bool {
        self.from.is_none_or(|from| date >= from) && self.to.is_none_or(|to| date <= to)
    }
}

#[derive(Debug, Default)]
struct AccountAccumulator {
    username: String,
    flow: AttendanceFlowTotals,
    outstanding_seconds: i64,
    claimable_seconds: i64,
}

#[derive(Debug, Default)]
struct FlowAccumulator {
    totals: AttendanceFlowTotals,
    buckets: BTreeMap<String, AttendanceFlowTotals>,
    accounts: BTreeMap<String, AccountAccumulator>,
}

#[derive(Debug, Clone, Copy, Default)]
struct FlowDelta {
    check_ins: i64,
    earned_seconds: i64,
    claimed_seconds: i64,
    expired_seconds: i64,
    forfeited_seconds: i64,
}

type EarningMonthFacts = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredClaimLocalDates {
    start: String,
    end: String,
}

impl FlowAccumulator {
    fn record(
        &mut self,
        account_id: &str,
        date: NaiveDate,
        period: ReportPeriod,
        range: DateRange,
        delta: FlowDelta,
    ) -> AppResult<()> {
        if !range.includes(date) {
            return Ok(());
        }
        let label = period_label(date, period);
        add_flow_delta(&mut self.totals, delta)?;
        add_flow_delta(self.buckets.entry(label).or_default(), delta)?;
        let account = self.accounts.get_mut(account_id).ok_or_else(|| {
            AppError::Conflict(
                "Attendance flow references an Account without an Earning Month".to_owned(),
            )
        })?;
        add_flow_delta(&mut account.flow, delta)?;
        Ok(())
    }
}

/// Reads Attendance facts in one immediate transaction, finalizing the clock
/// floor before deriving expiry. This writes no Audit Log events.
pub async fn attendance_report(
    pool: &SqlitePool,
    input: &AttendanceReportInput,
) -> AppResult<AttendanceReport> {
    attendance_report_at(pool, input, Utc::now(), super::attendance::host_offset()).await
}

async fn attendance_report_at(
    pool: &SqlitePool,
    input: &AttendanceReportInput,
    host_now: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<AttendanceReport> {
    let (date_range, month_page_bounds, account_page_bounds) = validate_input(input)?;
    let mut tx = crate::db::begin_immediate(pool).await?;
    let clock = crate::db::attendance::finalize_in_transaction(&mut tx, host_now, offset).await?;
    let as_of = clock.utc_floor;
    let host_local_date = host_now.with_timezone(&offset).date_naive();
    let admin_local_date =
        attendance_domain::authoritative_local_date(as_of, offset, clock.open_local_date);
    let admin_local_month = admin_local_date.format("%Y-%m").to_string();
    let clock_regression_advisory =
        host_now < clock.utc_floor || host_local_date < clock.open_local_date;

    let fact_rows: Vec<EarningMonthFacts> = sqlx::query_as(
        "SELECT m.account_id, a.username, m.earning_month,
                    m.claim_starts_at, m.deadline_at_exclusive, m.claim_local_dates,
                    COALESCE((SELECT SUM(ci.reward_seconds)
                              FROM attendance_check_ins ci
                              WHERE ci.earning_month_id = m.id
                                AND ci.account_id = m.account_id), 0) AS earned_seconds,
                    COALESCE((SELECT SUM(c.claimed_seconds)
                              FROM attendance_claims c
                              WHERE c.earning_month_id = m.id), 0) AS claimed_seconds,
                    COALESCE((SELECT SUM(f.forfeited_seconds)
                              FROM attendance_forfeitures f
                              WHERE f.earning_month_id = m.id
                                AND f.account_id = m.account_id), 0) AS forfeited_seconds
             FROM attendance_earning_months m
             JOIN accounts a ON a.id = m.account_id
             ORDER BY m.earning_month DESC, m.account_id ASC",
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut lifetime_totals = AttendanceLifetimeTotals::default();
    let mut liability = AttendanceLiability::default();
    let mut month_rows = Vec::with_capacity(fact_rows.len());
    let mut flows = FlowAccumulator::default();

    for (
        account_id,
        username,
        earning_month,
        claim_starts_at,
        deadline_at_exclusive,
        claim_local_dates,
        earned_seconds,
        claimed_seconds,
        forfeited_seconds,
    ) in fact_rows
    {
        let claim_starts_at = super::attendance::parse_stored_instant(&claim_starts_at)?;
        let deadline_at_exclusive =
            super::attendance::parse_stored_instant(&deadline_at_exclusive)?;
        let claim_deadline_local_date = parse_claim_deadline_local_date(&claim_local_dates)?;
        let balance = attendance_domain::derive_earning_month_balance(
            as_of,
            claim_starts_at,
            deadline_at_exclusive,
            earned_seconds,
            claimed_seconds,
            forfeited_seconds,
        )
        .map_err(|_| {
            AppError::Conflict("Attendance Earning Month does not reconcile".to_owned())
        })?;

        add_month_totals(&mut lifetime_totals, balance)?;
        add_to_total(
            &mut liability.outstanding_seconds,
            balance.outstanding_seconds,
            "Attendance Outstanding liability overflow",
        )?;
        add_to_total(
            &mut liability.claimable_seconds,
            balance.claimable_seconds,
            "Attendance Claimable liability overflow",
        )?;

        let account = flows.accounts.entry(account_id.clone()).or_default();
        account.username.clone_from(&username);
        add_to_total(
            &mut account.outstanding_seconds,
            balance.outstanding_seconds,
            "Attendance Account liability overflow",
        )?;
        add_to_total(
            &mut account.claimable_seconds,
            balance.claimable_seconds,
            "Attendance Account claimable liability overflow",
        )?;

        if balance.expired_seconds > 0 {
            flows.record(
                &account_id,
                deadline_at_exclusive.with_timezone(&offset).date_naive(),
                input.period,
                date_range,
                FlowDelta {
                    expired_seconds: balance.expired_seconds,
                    ..FlowDelta::default()
                },
            )?;
        }

        month_rows.push(AttendanceEarningMonthRow {
            account_id,
            username,
            earning_month,
            state: balance.state,
            claim_starts_at,
            deadline_at_exclusive,
            claim_deadline_local_date,
            earned_seconds: balance.earned_seconds,
            claimed_seconds: balance.claimed_seconds,
            outstanding_seconds: balance.outstanding_seconds,
            expired_seconds: balance.expired_seconds,
            forfeited_seconds: balance.forfeited_seconds,
        });
    }

    let mut check_in_query = QueryBuilder::<Sqlite>::new(
        "SELECT account_id, earning_date, reward_seconds
         FROM attendance_check_ins WHERE 1 = 1",
    );
    if let Some(from) = date_range.from {
        check_in_query
            .push(" AND earning_date >= ")
            .push_bind(from.to_string());
    }
    if let Some(to) = date_range.to {
        check_in_query
            .push(" AND earning_date <= ")
            .push_bind(to.to_string());
    }
    check_in_query.push(" ORDER BY earning_date, account_id");
    let check_ins: Vec<(String, String, i64)> =
        check_in_query.build_query_as().fetch_all(&mut *tx).await?;
    for (account_id, earning_date, reward_seconds) in check_ins {
        flows.record(
            &account_id,
            super::attendance::parse_stored_date(&earning_date)?,
            input.period,
            date_range,
            FlowDelta {
                check_ins: 1,
                earned_seconds: reward_seconds,
                ..FlowDelta::default()
            },
        )?;
    }

    let claims: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT account_id, created_at, claimed_seconds
         FROM attendance_claims ORDER BY created_at, audit_event_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (account_id, created_at, claimed_seconds) in claims {
        let date = super::attendance::parse_stored_instant(&created_at)?
            .with_timezone(&offset)
            .date_naive();
        flows.record(
            &account_id,
            date,
            input.period,
            date_range,
            FlowDelta {
                claimed_seconds,
                ..FlowDelta::default()
            },
        )?;
    }

    let forfeitures: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT account_id, deleted_at, forfeited_seconds
         FROM attendance_forfeitures ORDER BY deleted_at, earning_month_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (account_id, deleted_at, forfeited_seconds) in forfeitures {
        let date = super::attendance::parse_stored_instant(&deleted_at)?
            .with_timezone(&offset)
            .date_naive();
        flows.record(
            &account_id,
            date,
            input.period,
            date_range,
            FlowDelta {
                forfeited_seconds,
                ..FlowDelta::default()
            },
        )?;
    }

    let FlowAccumulator {
        totals: flow_totals,
        buckets,
        accounts,
    } = flows;
    let mut account_rows = accounts
        .into_iter()
        .map(|(account_id, account)| AttendanceAccountRow {
            account_id,
            username: account.username,
            check_ins: account.flow.check_ins,
            earned_seconds: account.flow.earned_seconds,
            claimed_seconds: account.flow.claimed_seconds,
            outstanding_seconds: account.outstanding_seconds,
            claimable_seconds: account.claimable_seconds,
            expired_seconds: account.flow.expired_seconds,
        })
        .collect::<Vec<_>>();
    account_rows.sort_by(|left, right| {
        right
            .outstanding_seconds
            .cmp(&left.outstanding_seconds)
            .then_with(|| {
                left.username
                    .to_lowercase()
                    .cmp(&right.username.to_lowercase())
            })
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    let month_page = make_page(month_rows, month_page_bounds)?;
    let account_page = make_page(account_rows, account_page_bounds)?;
    let flow_buckets = buckets
        .into_iter()
        .map(|(period_label, totals)| AttendanceFlowBucket {
            period_label,
            totals,
        })
        .collect();
    let economics_advisory = economics_advisory(&mut tx).await?;

    tx.commit().await?;
    Ok(AttendanceReport {
        period: input.period,
        from_date: input.from_date.clone(),
        to_date: input.to_date.clone(),
        as_of,
        admin_local_date,
        admin_local_month,
        flow_totals,
        flow_buckets,
        lifetime_totals,
        liability,
        month_page,
        account_page,
        economics_advisory,
        clock_regression_advisory,
    })
}

/// Reads the settings surface from one writer transaction and one finalized
/// Attendance clock, without constructing the full report.
pub async fn attendance_settings_snapshot(
    pool: &SqlitePool,
    host_now: DateTime<Utc>,
    offset: FixedOffset,
) -> AppResult<AttendanceSettingsSnapshot> {
    let mut tx = crate::db::begin_immediate(pool).await?;
    let clock = crate::db::attendance::finalize_in_transaction(&mut tx, host_now, offset).await?;
    let as_of = clock.utc_floor;
    let settings = super::attendance::current_settings_version_in_transaction(&mut tx).await?;
    let liability = attendance_liability(&mut tx, as_of).await?;
    let economics_advisory = economics_advisory_for_values_in_transaction(
        &mut tx,
        settings.daily_threshold_cents,
        settings.reward_seconds,
    )
    .await?;
    let admin_local_date =
        attendance_domain::authoritative_local_date(as_of, offset, clock.open_local_date);
    let next_earning_month = next_earning_month(admin_local_date)?;

    tx.commit().await?;
    Ok(AttendanceSettingsSnapshot {
        settings,
        economics_advisory,
        liability,
        next_earning_month,
    })
}

fn next_earning_month(admin_local_date: NaiveDate) -> AppResult<String> {
    use chrono::Datelike;

    let first_of_month =
        NaiveDate::from_ymd_opt(admin_local_date.year(), admin_local_date.month(), 1)
            .ok_or_else(|| AppError::Internal("invalid Admin-local Attendance date".to_owned()))?;
    first_of_month
        .checked_add_months(chrono::Months::new(1))
        .map(|date| date.format("%Y-%m").to_string())
        .ok_or_else(|| AppError::Internal("next Attendance month overflow".to_owned()))
}

async fn attendance_liability(
    tx: &mut Transaction<'_, Sqlite>,
    as_of: DateTime<Utc>,
) -> AppResult<AttendanceLiability> {
    let rows: Vec<(String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT m.claim_starts_at, m.deadline_at_exclusive,
                COALESCE((SELECT SUM(ci.reward_seconds)
                          FROM attendance_check_ins ci
                          WHERE ci.earning_month_id = m.id
                            AND ci.account_id = m.account_id), 0) AS earned_seconds,
                COALESCE((SELECT SUM(c.claimed_seconds)
                          FROM attendance_claims c
                          WHERE c.earning_month_id = m.id), 0) AS claimed_seconds,
                COALESCE((SELECT SUM(f.forfeited_seconds)
                          FROM attendance_forfeitures f
                          WHERE f.earning_month_id = m.id
                            AND f.account_id = m.account_id), 0) AS forfeited_seconds
         FROM attendance_earning_months m
         JOIN accounts a ON a.id = m.account_id",
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut liability = AttendanceLiability::default();
    for (claim_starts_at, deadline_at_exclusive, earned, claimed, forfeited) in rows {
        let balance = attendance_domain::derive_earning_month_balance(
            as_of,
            super::attendance::parse_stored_instant(&claim_starts_at)?,
            super::attendance::parse_stored_instant(&deadline_at_exclusive)?,
            earned,
            claimed,
            forfeited,
        )
        .map_err(|_| {
            AppError::Conflict("Attendance Earning Month does not reconcile".to_owned())
        })?;
        add_to_total(
            &mut liability.outstanding_seconds,
            balance.outstanding_seconds,
            "Attendance Outstanding liability overflow",
        )?;
        add_to_total(
            &mut liability.claimable_seconds,
            balance.claimable_seconds,
            "Attendance Claimable liability overflow",
        )?;
    }
    Ok(liability)
}

fn validate_input(input: &AttendanceReportInput) -> AppResult<(DateRange, PageBounds, PageBounds)> {
    let from = input
        .from_date
        .as_deref()
        .map(|date| super::attendance::validate_date(date, "from_date"))
        .transpose()?;
    let to = input
        .to_date
        .as_deref()
        .map(|date| super::attendance::validate_date(date, "to_date"))
        .transpose()?;
    if matches!((from, to), (Some(from), Some(to)) if from > to) {
        return Err(AppError::InvalidInput(
            "from_date must not be after to_date".to_owned(),
        ));
    }
    Ok((
        DateRange { from, to },
        page_bounds(input.month_limit, input.month_offset, "month")?,
        page_bounds(input.account_limit, input.account_offset, "account")?,
    ))
}

fn page_bounds(limit: Option<i64>, offset: Option<i64>, name: &str) -> AppResult<PageBounds> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    let offset = offset.unwrap_or(0);
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(AppError::InvalidInput(format!(
            "{name}_limit must be from 1 to {MAX_PAGE_LIMIT}"
        )));
    }
    if offset < 0 {
        return Err(AppError::InvalidInput(format!(
            "{name}_offset must not be negative"
        )));
    }
    Ok(PageBounds { limit, offset })
}

fn add_month_totals(
    totals: &mut AttendanceLifetimeTotals,
    balance: EarningMonthBalance,
) -> AppResult<()> {
    add_to_total(
        &mut totals.earned_seconds,
        balance.earned_seconds,
        "Attendance earned total overflow",
    )?;
    add_to_total(
        &mut totals.claimed_seconds,
        balance.claimed_seconds,
        "Attendance claimed total overflow",
    )?;
    add_to_total(
        &mut totals.outstanding_seconds,
        balance.outstanding_seconds,
        "Attendance outstanding total overflow",
    )?;
    add_to_total(
        &mut totals.expired_seconds,
        balance.expired_seconds,
        "Attendance expired total overflow",
    )?;
    add_to_total(
        &mut totals.forfeited_seconds,
        balance.forfeited_seconds,
        "Attendance forfeited total overflow",
    )?;
    Ok(())
}

fn add_to_total(total: &mut i64, value: i64, message: &str) -> AppResult<()> {
    *total = total
        .checked_add(value)
        .ok_or_else(|| AppError::Conflict(message.to_owned()))?;
    Ok(())
}

fn add_flow_delta(totals: &mut AttendanceFlowTotals, delta: FlowDelta) -> AppResult<()> {
    add_to_total(
        &mut totals.check_ins,
        delta.check_ins,
        "Attendance Check-In flow overflow",
    )?;
    add_to_total(
        &mut totals.earned_seconds,
        delta.earned_seconds,
        "Attendance earned flow overflow",
    )?;
    add_to_total(
        &mut totals.claimed_seconds,
        delta.claimed_seconds,
        "Attendance claimed flow overflow",
    )?;
    add_to_total(
        &mut totals.expired_seconds,
        delta.expired_seconds,
        "Attendance expired flow overflow",
    )?;
    add_to_total(
        &mut totals.forfeited_seconds,
        delta.forfeited_seconds,
        "Attendance forfeited flow overflow",
    )?;
    Ok(())
}

fn period_label(date: NaiveDate, period: ReportPeriod) -> String {
    match period {
        ReportPeriod::Day => date.format("%Y-%m-%d").to_string(),
        ReportPeriod::Week => date.format("%Y-W%W").to_string(),
        ReportPeriod::Month => date.format("%Y-%m").to_string(),
    }
}

fn make_page<T>(rows: Vec<T>, bounds: PageBounds) -> AppResult<AttendancePage<T>> {
    let total = i64::try_from(rows.len())
        .map_err(|_| AppError::Conflict("Attendance page count overflow".to_owned()))?;
    let start = usize::try_from(bounds.offset)
        .unwrap_or(usize::MAX)
        .min(rows.len());
    let limit = bounds.limit as usize;
    let end = start.saturating_add(limit).min(rows.len());
    let has_more = end < rows.len();
    Ok(AttendancePage {
        rows: rows.into_iter().skip(start).take(limit).collect(),
        total,
        limit: bounds.limit,
        offset: bounds.offset,
        has_more,
    })
}

async fn economics_advisory(
    tx: &mut Transaction<'_, Sqlite>,
) -> AppResult<Option<AttendanceEconomicsAdvisory>> {
    let settings: Option<(i64, i64)> = sqlx::query_as(
        "SELECT daily_threshold_cents, reward_seconds
         FROM attendance_settings_versions ORDER BY version DESC LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some((daily_threshold_cents, reward_seconds)) = settings else {
        return Ok(None);
    };
    economics_advisory_for_values_in_transaction(tx, daily_threshold_cents, reward_seconds).await
}

async fn economics_advisory_for_values_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    daily_threshold_cents: i64,
    reward_seconds: i64,
) -> AppResult<Option<AttendanceEconomicsAdvisory>> {
    let (rate_amount_cents, rate_seconds) = match super::rates::active_rate(&mut **tx).await {
        Ok(rate) => rate,
        Err(AppError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let effective_bonus = match attendance_domain::attendance_effective_bonus(
        daily_threshold_cents,
        reward_seconds,
        rate_amount_cents,
        rate_seconds,
    ) {
        Ok(bonus) => bonus,
        Err(PricingError::Overflow) => return Err(AppError::Pricing(PricingError::Overflow)),
        Err(_) => return Ok(None),
    };
    Ok(Some(AttendanceEconomicsAdvisory {
        daily_threshold_cents,
        reward_seconds,
        rate_amount_cents,
        rate_seconds,
        unsafe_bonus: effective_bonus.is_unsafe(),
        effective_bonus,
    }))
}

fn parse_claim_deadline_local_date(value: &str) -> AppResult<String> {
    let dates: StoredClaimLocalDates = serde_json::from_str(value)
        .map_err(|_| AppError::Internal("invalid stored Attendance claim dates".to_owned()))?;
    let start = super::attendance::parse_stored_date(&dates.start)?;
    let end = super::attendance::parse_stored_date(&dates.end)?;
    if start > end {
        return Err(AppError::Internal(
            "invalid stored Attendance claim date range".to_owned(),
        ));
    }
    Ok(end.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
    use openpiso_core::{attendance::EarningMonthState, report::ReportPeriod};
    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::*;
    use crate::{
        accounts::{self, AccountCreator},
        audit::{self, NewAuditEvent},
        db,
        errors::{AppError, AppResult},
    };

    fn offset() -> FixedOffset {
        db::attendance::host_offset()
    }

    fn instant(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("test instant is valid")
            .with_timezone(&Utc)
    }

    fn input() -> AttendanceReportInput {
        AttendanceReportInput {
            period: ReportPeriod::Day,
            from_date: None,
            to_date: None,
            month_limit: None,
            month_offset: None,
            account_limit: None,
            account_offset: None,
        }
    }

    async fn create_account(pool: &SqlitePool, username: &str) -> AppResult<String> {
        let password = Uuid::new_v4().to_string();
        Ok(
            accounts::create(pool, username, &password, AccountCreator::Admin, None)
                .await?
                .id,
        )
    }

    async fn read_economics_advisory(
        pool: &SqlitePool,
    ) -> AppResult<Option<AttendanceEconomicsAdvisory>> {
        let mut tx = pool.begin().await?;
        let advisory = economics_advisory(&mut tx).await?;
        tx.commit().await?;
        Ok(advisory)
    }

    async fn add_month(
        pool: &SqlitePool,
        account_id: &str,
        earning_month: &str,
        claim_starts_at: &str,
        deadline_at_exclusive: &str,
    ) -> AppResult<i64> {
        Ok(sqlx::query_scalar(
            "INSERT INTO attendance_earning_months
               (account_id, earning_month, claim_window_configuration_version,
                claim_window_months, claim_starts_at, deadline_at_exclusive,
                claim_local_dates)
             VALUES (?, ?, 2, 1, ?, ?, '{\"start\":\"2026-01-01\",\"end\":\"2026-01-31\"}')
             RETURNING id",
        )
        .bind(account_id)
        .bind(earning_month)
        .bind(claim_starts_at)
        .bind(deadline_at_exclusive)
        .fetch_one(pool)
        .await?)
    }

    async fn add_check_in(
        pool: &SqlitePool,
        account_id: &str,
        month_id: i64,
        earning_date: &str,
        source_at: &str,
        reward_seconds: i64,
    ) -> AppResult<()> {
        let mut tx = db::begin_immediate(pool).await?;
        let mut source = NewAuditEvent::simple(
            "coin_accepted",
            openpiso_core::audit::AuditOutcome::Success,
            openpiso_core::audit::AuditActor::CoinDevice,
            "Accepted coin",
        );
        source.account_id = Some(account_id);
        source.pc_id = Some("pc-01");
        source.amount_cents = Some(1_000);
        source.cash_flag = true;
        let source_event_id = audit::append(&mut tx, source).await?;
        sqlx::query(
            "INSERT INTO attendance_contributions
               (source_audit_event_id, source_kind, configuration_version,
                account_id, earning_date, cents, source_at)
             VALUES (?, 'coin_accepted', 2, ?, ?, 1000, ?)",
        )
        .bind(&source_event_id)
        .bind(account_id)
        .bind(earning_date)
        .bind(source_at)
        .execute(&mut *tx)
        .await?;

        let mut check_in = NewAuditEvent::simple(
            "attendance_check_in",
            openpiso_core::audit::AuditOutcome::Success,
            openpiso_core::audit::AuditActor::System,
            "Attendance Check-In",
        );
        check_in.account_id = Some(account_id);
        check_in.pc_id = Some("pc-01");
        check_in.attendance_seconds_delta = Some(reward_seconds);
        let check_in_event_id = audit::append(&mut tx, check_in).await?;
        sqlx::query(
            "INSERT INTO attendance_check_ins
               (audit_event_id, account_id, earning_date, earning_month_id,
                configuration_version, daily_threshold_cents, reward_seconds,
                threshold_crossing_source_audit_event_id, trigger_pc_id,
                original_earning_at)
             VALUES (?, ?, ?, ?, 2, 1000, ?, ?, 'pc-01', ?)",
        )
        .bind(check_in_event_id)
        .bind(account_id)
        .bind(earning_date)
        .bind(month_id)
        .bind(reward_seconds)
        .bind(source_event_id)
        .bind(source_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn add_claim(
        pool: &SqlitePool,
        account_id: &str,
        month_id: i64,
        claimed_seconds: i64,
        created_at: &str,
    ) -> AppResult<()> {
        let mut tx = db::begin_immediate(pool).await?;
        let mut event = NewAuditEvent::simple(
            "attendance_claim",
            openpiso_core::audit::AuditOutcome::Success,
            openpiso_core::audit::AuditActor::Account,
            "Attendance Time claimed",
        );
        event.actor_account_id = Some(account_id);
        event.account_id = Some(account_id);
        event.pc_id = Some("pc-01");
        event.seconds_delta = Some(claimed_seconds);
        event.attendance_seconds_delta = Some(-claimed_seconds);
        let event_id = audit::append(&mut tx, event).await?;
        sqlx::query(
            "INSERT INTO attendance_claims
               (audit_event_id, request_id, account_id, earning_month_id,
                claim_pc_id, claimed_seconds, created_at)
             VALUES (?, ?, ?, ?, 'pc-01', ?, ?)",
        )
        .bind(event_id)
        .bind(Uuid::new_v4().to_string())
        .bind(account_id)
        .bind(month_id)
        .bind(claimed_seconds)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn add_denied_claim(pool: &SqlitePool, account_id: &str) -> AppResult<()> {
        let mut event = NewAuditEvent::simple(
            "attendance_claim",
            openpiso_core::audit::AuditOutcome::Denied,
            openpiso_core::audit::AuditActor::Account,
            "Attendance Claim denied",
        );
        event.actor_account_id = Some(account_id);
        event.account_id = Some(account_id);
        audit::append_standalone(pool, event).await?;
        Ok(())
    }

    async fn add_forfeiture(
        pool: &SqlitePool,
        account_id: &str,
        month_id: i64,
        forfeited_seconds: i64,
        deleted_at: &str,
    ) -> AppResult<()> {
        let mut tx = db::begin_immediate(pool).await?;
        let mut event = NewAuditEvent::simple(
            "account_deleted",
            openpiso_core::audit::AuditOutcome::Success,
            openpiso_core::audit::AuditActor::Admin,
            "Account deleted",
        );
        event.account_id = Some(account_id);
        event.attendance_seconds_delta = Some(-forfeited_seconds);
        let event_id = audit::append(&mut tx, event).await?;
        sqlx::query(
            "INSERT INTO attendance_forfeitures
               (account_id, earning_month_id, deletion_audit_event_id,
                forfeited_seconds, deleted_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(account_id)
        .bind(month_id)
        .bind(event_id)
        .bind(forfeited_seconds)
        .bind(deleted_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn set_clock(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<()> {
        let date = now.with_timezone(&offset()).date_naive();
        sqlx::query(
            "UPDATE attendance_clock_state SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(now.to_rfc3339_opts(SecondsFormat::Millis, true))
        .bind(date.to_string())
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn assert_reconciles_at(pool: &SqlitePool, host_now: DateTime<Utc>) -> AppResult<()> {
        let report = attendance_report_at(pool, &input(), host_now, offset()).await?;
        let totals = report.lifetime_totals;
        assert_eq!(
            totals.earned_seconds,
            totals.claimed_seconds
                + totals.outstanding_seconds
                + totals.expired_seconds
                + totals.forfeited_seconds
        );
        Ok(())
    }

    async fn report_facts(pool: &SqlitePool, account_id: &str) -> AppResult<i64> {
        let before_deadline = instant("2026-02-20T00:00:00Z");
        set_clock(pool, before_deadline).await?;
        let month_id = add_month(
            pool,
            account_id,
            "2026-01",
            "2026-02-01T00:00:00Z",
            "2026-03-01T00:00:00Z",
        )
        .await?;
        assert_reconciles_at(pool, before_deadline).await?;
        add_check_in(
            pool,
            account_id,
            month_id,
            "2026-01-05",
            "2026-01-05T12:00:00Z",
            600,
        )
        .await?;
        assert_reconciles_at(pool, before_deadline).await?;
        add_claim(pool, account_id, month_id, 600, "2026-02-01T12:00:00Z").await?;
        assert_reconciles_at(pool, before_deadline).await?;
        // The late Check-In is attributed to its original earning date even
        // though the qualifying source arrived after the first claim.
        add_check_in(
            pool,
            account_id,
            month_id,
            "2026-01-20",
            "2026-02-10T12:00:00Z",
            200,
        )
        .await?;
        assert_reconciles_at(pool, before_deadline).await?;
        add_forfeiture(pool, account_id, month_id, 100, "2026-02-15T12:00:00Z").await?;
        assert_reconciles_at(pool, before_deadline).await?;
        Ok(month_id)
    }

    #[tokio::test]
    async fn attendance_settings_snapshot_uses_one_durable_clock() -> AppResult<()> {
        let pool = crate::test_utils::mem_test_pool().await;
        let account_id = create_account(&pool, "att-settings-snapshot-user").await?;
        crate::test_utils::enable_attendance(&pool, 2, instant("2025-01-01T00:00:00Z"), 1_000, 600)
            .await?;
        let month_id = add_month(
            &pool,
            &account_id,
            "2026-01",
            "2026-02-01T00:00:00Z",
            "2026-03-01T00:00:00Z",
        )
        .await?;
        add_check_in(
            &pool,
            &account_id,
            month_id,
            "2026-01-15",
            "2026-01-15T12:00:00Z",
            600,
        )
        .await?;

        set_clock(&pool, instant("2026-02-28T23:59:59Z")).await?;
        let before_deadline =
            attendance_settings_snapshot(&pool, instant("2026-02-01T12:00:00Z"), offset()).await?;
        assert_eq!(before_deadline.liability.outstanding_seconds, 600);
        assert_eq!(before_deadline.liability.claimable_seconds, 600);

        set_clock(&pool, instant("2026-03-01T00:00:00Z")).await?;
        let snapshot =
            attendance_settings_snapshot(&pool, instant("2026-02-01T12:00:00Z"), offset()).await?;

        assert_eq!(snapshot.liability.outstanding_seconds, 0);
        assert_eq!(snapshot.liability.claimable_seconds, 0);
        assert_eq!(snapshot.next_earning_month, "2026-04");
        Ok(())
    }

    #[tokio::test]
    async fn attendance_report_reconciles_and_expiry_survives_clock_regression() -> AppResult<()> {
        let pool = crate::test_utils::mem_test_pool().await;
        let account_id = create_account(&pool, "attendance-report-user").await?;
        crate::test_utils::enable_attendance(&pool, 2, instant("2025-01-01T00:00:00Z"), 1_000, 600)
            .await?;
        report_facts(&pool, &account_id).await?;
        let before_deadline = instant("2026-02-20T00:00:00Z");
        set_clock(&pool, before_deadline).await?;

        let mut filtered = input();
        filtered.from_date = Some("2026-01-01".to_owned());
        filtered.to_date = Some("2026-01-31".to_owned());
        let report = attendance_report_at(&pool, &filtered, before_deadline, offset()).await?;

        assert_eq!(
            report.admin_local_date,
            before_deadline.with_timezone(&offset()).date_naive()
        );
        assert_eq!(
            report.admin_local_month,
            report.admin_local_date.format("%Y-%m").to_string()
        );
        assert_eq!(report.lifetime_totals.earned_seconds, 800);
        assert_eq!(report.lifetime_totals.claimed_seconds, 600);
        assert_eq!(report.lifetime_totals.outstanding_seconds, 100);
        assert_eq!(report.lifetime_totals.expired_seconds, 0);
        assert_eq!(report.lifetime_totals.forfeited_seconds, 100);
        assert_eq!(
            report.lifetime_totals.earned_seconds,
            report.lifetime_totals.claimed_seconds
                + report.lifetime_totals.outstanding_seconds
                + report.lifetime_totals.expired_seconds
                + report.lifetime_totals.forfeited_seconds
        );
        assert_eq!(report.flow_totals.earned_seconds, 800);
        assert_eq!(report.flow_totals.claimed_seconds, 0);
        assert_eq!(report.flow_totals.expired_seconds, 0);
        assert_eq!(report.flow_totals.forfeited_seconds, 0);
        assert_eq!(report.liability.outstanding_seconds, 100);
        assert_eq!(report.liability.claimable_seconds, 100);
        assert_eq!(report.month_page.rows.len(), 1);
        assert_eq!(
            report.month_page.rows[0].claim_deadline_local_date,
            "2026-01-31"
        );
        assert_eq!(
            report.month_page.rows[0].state,
            EarningMonthState::Claimable
        );
        assert_eq!(report.month_page.rows[0].earned_seconds, 800);
        assert_eq!(report.account_page.rows.len(), 1);
        assert_eq!(report.account_page.rows[0].account_id, account_id);
        assert_eq!(report.account_page.rows[0].check_ins, 2);
        assert_eq!(report.account_page.rows[0].outstanding_seconds, 100);

        let mut weekly = filtered.clone();
        weekly.period = ReportPeriod::Week;
        let weekly_report = attendance_report_at(&pool, &weekly, before_deadline, offset()).await?;
        let week_labels: Vec<_> = weekly_report
            .flow_buckets
            .iter()
            .map(|bucket| bucket.period_label.as_str())
            .collect();
        assert_eq!(week_labels, ["2026-W01", "2026-W03"]);

        let mut monthly = filtered;
        monthly.period = ReportPeriod::Month;
        let monthly_report =
            attendance_report_at(&pool, &monthly, before_deadline, offset()).await?;
        let month_labels: Vec<_> = monthly_report
            .flow_buckets
            .iter()
            .map(|bucket| bucket.period_label.as_str())
            .collect();
        assert_eq!(month_labels, ["2026-01"]);

        let at_deadline = instant("2026-03-01T00:00:00Z");
        let audit_before_report: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await?;
        let expired = attendance_report_at(&pool, &input(), at_deadline, offset()).await?;
        assert_eq!(expired.as_of, at_deadline);
        assert_eq!(expired.lifetime_totals.outstanding_seconds, 0);
        assert_eq!(expired.lifetime_totals.expired_seconds, 100);
        assert_eq!(expired.flow_totals.expired_seconds, 100);
        assert_eq!(expired.liability.outstanding_seconds, 0);
        assert_eq!(expired.month_page.rows[0].state, EarningMonthState::Expired);

        let regressed = attendance_report_at(&pool, &input(), before_deadline, offset()).await?;
        assert_eq!(regressed.as_of, at_deadline);
        assert!(regressed.clock_regression_advisory);
        assert_eq!(regressed.lifetime_totals.expired_seconds, 100);
        assert_eq!(regressed.liability.outstanding_seconds, 0);
        let audit_after_reports: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await?;
        assert_eq!(audit_after_reports, audit_before_report);

        let claim = db::attendance_claims::claim(
            &pool,
            || before_deadline,
            &account_id,
            None,
            Uuid::new_v4(),
            "2026-01",
        )
        .await?;
        assert!(matches!(
            claim,
            db::attendance_claims::ClaimOutcome::Denied {
                reason: db::attendance_claims::ClaimDenialReason::Expired,
                ..
            }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn attendance_claim_is_counted_once_as_free_time_only() -> AppResult<()> {
        let pool = crate::test_utils::mem_test_pool().await;
        let account_id = create_account(&pool, "attendance-free-time-user").await?;
        crate::test_utils::enable_attendance(&pool, 2, instant("2025-01-01T00:00:00Z"), 1_000, 600)
            .await?;
        let month_id = add_month(
            &pool,
            &account_id,
            "2026-01",
            "2026-02-01T00:00:00Z",
            "2026-03-01T00:00:00Z",
        )
        .await?;
        add_check_in(
            &pool,
            &account_id,
            month_id,
            "2026-01-05",
            "2026-01-05T12:00:00Z",
            600,
        )
        .await?;
        let before = crate::reports::report_summary(&pool, ReportPeriod::Day, None, None).await?;
        assert_eq!(before.free_time_dispensed_seconds, 0);
        let before_deadline = instant("2026-02-20T00:00:00Z");
        set_clock(&pool, before_deadline).await?;
        let outcome = db::attendance_claims::claim(
            &pool,
            || before_deadline,
            &account_id,
            None,
            Uuid::new_v4(),
            "2026-01",
        )
        .await?;
        assert!(matches!(
            outcome,
            db::attendance_claims::ClaimOutcome::Claimed { claim, .. }
                if claim.claimed_seconds == 600
        ));
        add_denied_claim(&pool, &account_id).await?;

        let summary = crate::reports::report_summary(&pool, ReportPeriod::Day, None, None).await?;
        let account = crate::reports::account_report(&pool, &account_id, None, None).await?;
        assert_eq!(summary.free_time_dispensed_seconds, 600);
        assert_eq!(summary.cash_revenue_cents, before.cash_revenue_cents);
        assert_eq!(account.free_time_dispensed_seconds, 600);
        let claims: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM attendance_claims WHERE earning_month_id = ?")
                .bind(month_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(claims, 1);
        Ok(())
    }

    #[tokio::test]
    async fn attendance_report_validates_pagination_and_reports_independent_pages() -> AppResult<()>
    {
        let pool = crate::test_utils::mem_test_pool().await;
        let first = create_account(&pool, "attendance-page-first").await?;
        let second = create_account(&pool, "attendance-page-second").await?;
        crate::test_utils::enable_attendance(&pool, 2, instant("2025-01-01T00:00:00Z"), 1_000, 600)
            .await?;
        let first_month = add_month(
            &pool,
            &first,
            "2026-01",
            "2026-02-01T00:00:00Z",
            "2026-03-01T00:00:00Z",
        )
        .await?;
        add_check_in(
            &pool,
            &first,
            first_month,
            "2026-01-05",
            "2026-01-05T12:00:00Z",
            600,
        )
        .await?;
        let second_month = add_month(
            &pool,
            &second,
            "2026-02",
            "2026-03-01T00:00:00Z",
            "2026-04-01T00:00:00Z",
        )
        .await?;
        add_check_in(
            &pool,
            &second,
            second_month,
            "2026-02-05",
            "2026-02-05T12:00:00Z",
            600,
        )
        .await?;

        let mut request = input();
        request.month_limit = Some(1);
        request.month_offset = Some(0);
        request.account_limit = Some(1);
        request.account_offset = Some(1);
        let report = attendance_report_at(&pool, &request, Utc::now(), offset()).await?;
        assert_eq!(report.month_page.total, 2);
        assert!(report.month_page.has_more);
        assert_eq!(report.month_page.rows[0].earning_month, "2026-02");
        assert_eq!(report.account_page.total, 2);
        assert!(!report.account_page.has_more);
        assert_eq!(report.account_page.rows.len(), 1);

        let mut invalid = input();
        invalid.month_limit = Some(201);
        assert!(matches!(
            attendance_report_at(&pool, &invalid, Utc::now(), offset()).await,
            Err(AppError::InvalidInput(_))
        ));
        let mut invalid = input();
        invalid.account_offset = Some(-1);
        assert!(matches!(
            attendance_report_at(&pool, &invalid, Utc::now(), offset()).await,
            Err(AppError::InvalidInput(_))
        ));
        let mut invalid = input();
        invalid.from_date = Some("2026-02-30".to_owned());
        assert!(matches!(
            attendance_report_at(&pool, &invalid, Utc::now(), offset()).await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn attendance_report_clock_advisory_advances_floor_without_audit() -> AppResult<()> {
        let pool = crate::test_utils::mem_test_pool().await;
        let floor = instant("2026-02-20T00:00:00Z");
        set_clock(&pool, floor).await?;
        let audit_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await?;

        let regressed =
            attendance_report_at(&pool, &input(), instant("2026-02-19T12:00:00Z"), offset())
                .await?;
        assert!(regressed.clock_regression_advisory);
        assert_eq!(regressed.as_of, floor);
        let caught_up_at = instant("2026-02-20T12:00:00Z");
        let normal = attendance_report_at(&pool, &input(), caught_up_at, offset()).await?;
        assert!(!normal.clock_regression_advisory);
        assert_eq!(normal.as_of, caught_up_at);

        let saved_floor: String =
            sqlx::query_scalar("SELECT utc_floor FROM attendance_clock_state WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            saved_floor,
            caught_up_at.to_rfc3339_opts(SecondsFormat::Millis, true)
        );
        let audit_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await?;
        assert_eq!(audit_after, audit_before);
        Ok(())
    }

    #[tokio::test]
    async fn attendance_economics_advisory_is_informational_and_handles_infinity() -> AppResult<()>
    {
        let pool = crate::test_utils::mem_test_pool().await;
        crate::test_utils::enable_attendance(&pool, 2, instant("2025-01-01T00:00:00Z"), 1_001, 60)
            .await?;
        sqlx::query("UPDATE rates SET amount_cents = 100, seconds = 59 WHERE active = 1")
            .execute(&pool)
            .await?;
        let worst_case_before = db::coin_promo_settings(&pool)
            .await?
            .account_worst_case_basis_points;
        let finite = read_economics_advisory(&pool)
            .await?
            .expect("seeded Attendance settings exist");
        assert_eq!(
            finite.effective_bonus,
            openpiso_core::attendance::AttendanceEffectiveBonus::Finite {
                base_seconds: 590,
                basis_points: 1_016,
            }
        );
        assert!(!finite.unsafe_bonus);

        crate::test_utils::enable_attendance(&pool, 3, instant("2025-01-02T00:00:00Z"), 1, 60)
            .await?;
        let worst_case_after = db::coin_promo_settings(&pool)
            .await?
            .account_worst_case_basis_points;
        assert_eq!(worst_case_after, worst_case_before);

        sqlx::query("UPDATE rates SET seconds = 1 WHERE active = 1")
            .execute(&pool)
            .await?;
        let infinite = read_economics_advisory(&pool)
            .await?
            .expect("Attendance settings remain configured");
        assert_eq!(
            infinite.effective_bonus,
            openpiso_core::attendance::AttendanceEffectiveBonus::Infinite
        );
        assert!(infinite.unsafe_bonus);

        sqlx::query("UPDATE rates SET active = 0 WHERE active = 1")
            .execute(&pool)
            .await?;
        let missing_rate = attendance_report_at(&pool, &input(), Utc::now(), offset()).await?;
        assert!(missing_rate.economics_advisory.is_none());

        sqlx::query(
            "UPDATE rates SET active = 1, amount_cents = 1, seconds = 1
             WHERE id = (SELECT id FROM rates ORDER BY created_at DESC LIMIT 1)",
        )
        .execute(&pool)
        .await?;
        crate::test_utils::enable_attendance(
            &pool,
            4,
            instant("2025-01-03T00:00:00Z"),
            1,
            i64::MAX,
        )
        .await?;
        let large_finite = attendance_report_at(&pool, &input(), Utc::now(), offset())
            .await?
            .economics_advisory
            .expect("large finite bonus remains an advisory");
        assert_eq!(
            large_finite.effective_bonus,
            openpiso_core::attendance::AttendanceEffectiveBonus::Finite {
                base_seconds: 1,
                basis_points: i64::MAX,
            }
        );
        assert!(!large_finite.unsafe_bonus);
        Ok(())
    }
}

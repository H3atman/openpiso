use std::cmp::Ordering;

use chrono::{Datelike, Duration, Local, NaiveDateTime, TimeZone, Utc};
use openpiso_core::report::{category_for_event_type, AuditCategory, ReportPeriod};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{
    errors::{AppError, AppResult},
    models::{
        AuditFilterInput, AuditRow, PcReport, ReportBucket, ReportSummary, SalesReportOverview,
        SalesReportPc, SalesReportPeriod,
    },
};

fn validate_date_range(from_date: Option<&str>, to_date: Option<&str>) -> AppResult<()> {
    for date in from_date.into_iter().chain(to_date) {
        if !is_iso_date(date) {
            return Err(AppError::InvalidInput(format!(
                "date must be YYYY-MM-DD, got '{date}'"
            )));
        }
    }
    if let (Some(from), Some(to)) = (from_date, to_date) {
        if from > to {
            return Err(AppError::InvalidInput(
                "from_date must not be after to_date".to_owned(),
            ));
        }
    }
    Ok(())
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 {
        return false;
    }
    let separators = [4usize, 7];
    for (index, byte) in bytes.iter().enumerate() {
        if separators.contains(&index) {
            if byte != &b'-' {
                return false;
            }
        } else if !byte.is_ascii_digit() {
            return false;
        }
    }
    true
}

fn clamp_limit(value: Option<i64>, default: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(1, max)
}

/// Sum a column from a reporting view, grouped by the selected period column,
/// with optional date-range filtering. Shared by cash-revenue, free-time,
/// and points-minted queries (avoids three near-identical query blocks).
async fn report_view_buckets(
    pool: &SqlitePool,
    view: &str,
    sum_column: &str,
    period_column: &str,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> AppResult<Vec<(String, i64)>> {
    let mut query = QueryBuilder::<Sqlite>::new("SELECT ");
    query.push(period_column);
    query.push(" AS period_label, COALESCE(SUM(");
    query.push(sum_column);
    query.push("), 0) FROM ");
    query.push(view);
    query.push(" WHERE 1=1");
    if let Some(from) = from_date {
        query
            .push(" AND business_date >= ")
            .push_bind(from.to_owned());
    }
    if let Some(to) = to_date {
        query
            .push(" AND business_date <= ")
            .push_bind(to.to_owned());
    }
    query.push(" GROUP BY ");
    query.push(period_column);
    query.push(" ORDER BY ");
    query.push(period_column);
    Ok(query
        .build_query_as::<(String, i64)>()
        .fetch_all(pool)
        .await?)
}

/// Returns the unified report summary across the selected period and date
/// range. Cash Revenue and Free Time Dispensed are read from the canonical
/// audit_log views so the totals reconcile exactly with the event stream
/// (ADR-0014 / ADR-0015). Points Liability is a current snapshot.
pub async fn report_summary(
    pool: &SqlitePool,
    period: ReportPeriod,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> AppResult<ReportSummary> {
    validate_date_range(from_date, to_date)?;
    let period_column = period.sqlite_column();

    let cash_buckets = report_view_buckets(
        pool,
        "report_cash_revenue",
        "amount_cents",
        period_column,
        from_date,
        to_date,
    )
    .await?;
    let free_time_buckets = report_view_buckets(
        pool,
        "report_free_time_dispensed",
        "seconds_delta",
        period_column,
        from_date,
        to_date,
    )
    .await?;
    let points_minted_buckets = report_view_buckets(
        pool,
        "report_points_minted_by_wheel",
        "points_won_centi",
        period_column,
        from_date,
        to_date,
    )
    .await?;

    // Merge all three bucket sets into one list. A period label present in any
    // set gets a single bucket with values aggregated from all three.
    let mut labels: Vec<&str> = Vec::new();
    for (label, _, _) in cash_buckets
        .iter()
        .map(|(l, _)| (l.as_str(), 0_i64, 0_i64))
        .chain(
            free_time_buckets
                .iter()
                .map(|(l, _)| (l.as_str(), 0_i64, 0_i64)),
        )
        .chain(
            points_minted_buckets
                .iter()
                .map(|(l, _)| (l.as_str(), 0_i64, 0_i64)),
        )
    {
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels.sort();

    let mut buckets = Vec::with_capacity(labels.len());
    for label in labels {
        let cash = cash_buckets
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        let free = free_time_buckets
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        let minted = points_minted_buckets
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        buckets.push(ReportBucket {
            period_label: label.to_owned(),
            cash_revenue_cents: cash,
            free_time_dispensed_seconds: free,
            points_minted_by_wheel_centi: minted,
        });
    }

    let total_cash_revenue_cents: i64 = buckets.iter().map(|b| b.cash_revenue_cents).sum();
    let total_free_time_dispensed_seconds: i64 =
        buckets.iter().map(|b| b.free_time_dispensed_seconds).sum();
    let total_points_minted_by_wheel_centi: i64 =
        buckets.iter().map(|b| b.points_minted_by_wheel_centi).sum();
    let points_liability_centi = points_liability_total(pool).await?;

    Ok(ReportSummary {
        period,
        from_date: from_date.unwrap_or("").to_owned(),
        to_date: to_date.unwrap_or("").to_owned(),
        cash_revenue_cents: total_cash_revenue_cents,
        free_time_dispensed_seconds: total_free_time_dispensed_seconds,
        points_minted_by_wheel_centi: total_points_minted_by_wheel_centi,
        points_liability_centi,
        buckets,
    })
}

#[derive(Debug, Clone)]
struct SalesDateRange {
    from_date: String,
    to_date: String,
}

#[derive(Debug, Clone)]
struct SalesDateRanges {
    daily: SalesDateRange,
    weekly: SalesDateRange,
    monthly: SalesDateRange,
}

fn current_sales_date_ranges() -> SalesDateRanges {
    let today = Local::now().date_naive();
    let week_start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let month_start = today.with_day(1).unwrap_or(today);
    let week_end = week_start + Duration::days(6);

    SalesDateRanges {
        daily: SalesDateRange {
            from_date: today.format("%Y-%m-%d").to_string(),
            to_date: today.format("%Y-%m-%d").to_string(),
        },
        weekly: SalesDateRange {
            from_date: week_start.format("%Y-%m-%d").to_string(),
            to_date: week_end.format("%Y-%m-%d").to_string(),
        },
        monthly: SalesDateRange {
            from_date: month_start.format("%Y-%m-%d").to_string(),
            to_date: today.format("%Y-%m-%d").to_string(),
        },
    }
}

fn dated_sales_period(range: &SalesDateRange, cash_revenue_cents: i64) -> SalesReportPeriod {
    SalesReportPeriod {
        from_date: Some(range.from_date.clone()),
        to_date: Some(range.to_date.clone()),
        cash_revenue_cents,
    }
}

fn total_sales_period(cash_revenue_cents: i64) -> SalesReportPeriod {
    SalesReportPeriod {
        from_date: None,
        to_date: None,
        cash_revenue_cents,
    }
}

/// Returns the sales dashboard in one server-authoritative response. The
/// ranges are calculated from the admin host's local business date and all
/// values come from the canonical cash-revenue view (ADR-0014).
pub async fn sales_report_overview(pool: &SqlitePool) -> AppResult<SalesReportOverview> {
    let ranges = current_sales_date_ranges();
    let (daily, weekly, monthly, total) = sales_overview_totals(pool, &ranges).await?;
    let pcs = sales_overview_pcs(pool, &ranges).await?;

    Ok(SalesReportOverview {
        daily: dated_sales_period(&ranges.daily, daily),
        weekly: dated_sales_period(&ranges.weekly, weekly),
        monthly: dated_sales_period(&ranges.monthly, monthly),
        total: total_sales_period(total),
        pcs,
    })
}

async fn sales_overview_totals(
    pool: &SqlitePool,
    ranges: &SalesDateRanges,
) -> AppResult<(i64, i64, i64, i64)> {
    Ok(sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT
           COALESCE(SUM(CASE WHEN business_date BETWEEN ? AND ? THEN amount_cents ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN business_date BETWEEN ? AND ? THEN amount_cents ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN business_date BETWEEN ? AND ? THEN amount_cents ELSE 0 END), 0),
           COALESCE(SUM(amount_cents), 0)
         FROM report_cash_revenue",
    )
    .bind(&ranges.daily.from_date)
    .bind(&ranges.daily.to_date)
    .bind(&ranges.weekly.from_date)
    .bind(&ranges.weekly.to_date)
    .bind(&ranges.monthly.from_date)
    .bind(&ranges.monthly.to_date)
    .fetch_one(pool)
    .await?)
}

async fn sales_overview_pcs(
    pool: &SqlitePool,
    ranges: &SalesDateRanges,
) -> AppResult<Vec<SalesReportPc>> {
    let rows = sqlx::query_as::<_, (String, String, i64, i64, i64, i64)>(
        "SELECT
           p.id,
           p.name,
           COALESCE(SUM(CASE WHEN r.business_date BETWEEN ? AND ? THEN r.amount_cents ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN r.business_date BETWEEN ? AND ? THEN r.amount_cents ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN r.business_date BETWEEN ? AND ? THEN r.amount_cents ELSE 0 END), 0),
           COALESCE(SUM(r.amount_cents), 0)
         FROM pcs p
         LEFT JOIN report_cash_revenue r ON r.pc_id = p.id
         GROUP BY p.id, p.name",
    )
    .bind(&ranges.daily.from_date)
    .bind(&ranges.daily.to_date)
    .bind(&ranges.weekly.from_date)
    .bind(&ranges.weekly.to_date)
    .bind(&ranges.monthly.from_date)
    .bind(&ranges.monthly.to_date)
    .fetch_all(pool)
    .await?;

    let mut reports = rows
        .into_iter()
        .map(
            |(
                id,
                name,
                daily_sales_cents,
                weekly_sales_cents,
                monthly_sales_cents,
                total_sales_cents,
            )| SalesReportPc {
                id,
                name,
                daily_sales_cents,
                weekly_sales_cents,
                monthly_sales_cents,
                total_sales_cents,
            },
        )
        .collect::<Vec<_>>();
    reports.sort_by(|left, right| {
        natural_string_cmp(&left.name, &right.name).then_with(|| left.id.cmp(&right.id))
    });
    Ok(reports)
}

fn natural_string_cmp(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();

    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char))
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() =>
            {
                let left_number = take_digit_run(&mut left_chars);
                let right_number = take_digit_run(&mut right_chars);
                let comparison = compare_digit_runs(&left_number, &right_number);
                if comparison != Ordering::Equal {
                    return comparison;
                }
            }
            (Some(left_char), Some(right_char)) => {
                let comparison = left_char
                    .to_ascii_lowercase()
                    .cmp(&right_char.to_ascii_lowercase());
                if comparison != Ordering::Equal {
                    return comparison;
                }
                left_chars.next();
                right_chars.next();
            }
        }
    }
}

fn take_digit_run(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    while chars
        .peek()
        .is_some_and(|character| character.is_ascii_digit())
    {
        if let Some(character) = chars.next() {
            value.push(character);
        }
    }
    value
}

fn compare_digit_runs(left: &str, right: &str) -> Ordering {
    let left_trimmed = left.trim_start_matches('0');
    let right_trimmed = right.trim_start_matches('0');
    let left_normalized = if left_trimmed.is_empty() {
        "0"
    } else {
        left_trimmed
    };
    let right_normalized = if right_trimmed.is_empty() {
        "0"
    } else {
        right_trimmed
    };

    left_normalized
        .len()
        .cmp(&right_normalized.len())
        .then_with(|| left_normalized.cmp(right_normalized))
        .then_with(|| left.len().cmp(&right.len()))
}

/// Per-account report across the selected date range. Reconciles with
/// canonical audit events; never reads cached balances for historical totals.
pub async fn account_report(
    pool: &SqlitePool,
    account_id: &str,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> AppResult<PcReport> {
    validate_date_range(from_date, to_date)?;
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return Err(AppError::InvalidInput("account_id is required".to_owned()));
    }
    let username =
        sqlx::query_scalar::<_, Option<String>>("SELECT username FROM accounts WHERE id = ?")
            .bind(account_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| AppError::NotFound("account".to_owned()))?;

    let (cash_revenue_cents, seconds_sold) = sum_with_range(
        pool,
        "SELECT COALESCE(SUM(amount_cents), 0), COALESCE(SUM(seconds_delta), 0) FROM report_cash_revenue WHERE account_id = ",
        account_id,
        from_date,
        to_date,
    )
    .await?;

    let free_time_dispensed_seconds = scalar_with_range(
        pool,
        "SELECT COALESCE(SUM(seconds_delta), 0) FROM report_free_time_dispensed WHERE account_id = ",
        account_id,
        from_date,
        to_date,
    )
    .await?;

    Ok(PcReport {
        id: account_id.to_owned(),
        name: username.unwrap_or_default(),
        cash_revenue_cents,
        seconds_sold,
        free_time_dispensed_seconds,
        from_date: from_date.unwrap_or("").to_owned(),
        to_date: to_date.unwrap_or("").to_owned(),
    })
}

/// Per-PC report across the selected date range. Coins sold and manual
/// adjustments both anchor to the PC; sold vouchers do not, so the PC's
/// `cash_revenue_cents` is coins-only.
pub async fn pc_report(
    pool: &SqlitePool,
    pc_id: &str,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> AppResult<PcReport> {
    validate_date_range(from_date, to_date)?;
    let pc_id = pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("pc_id is required".to_owned()));
    }
    let pc_name = sqlx::query_scalar::<_, Option<String>>("SELECT name FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pc".to_owned()))?;

    let (cash_revenue_cents, seconds_sold) = sum_with_range(
        pool,
        "SELECT COALESCE(SUM(amount_cents), 0), COALESCE(SUM(seconds_delta), 0) FROM report_cash_revenue WHERE pc_id = ",
        pc_id,
        from_date,
        to_date,
    )
    .await?;

    let free_time_dispensed_seconds = scalar_with_range(
        pool,
        "SELECT COALESCE(SUM(seconds_delta), 0) FROM report_free_time_dispensed WHERE pc_id = ",
        pc_id,
        from_date,
        to_date,
    )
    .await?;

    Ok(PcReport {
        id: pc_id.to_owned(),
        name: pc_name.unwrap_or_default(),
        cash_revenue_cents,
        seconds_sold,
        free_time_dispensed_seconds,
        from_date: from_date.unwrap_or("").to_owned(),
        to_date: to_date.unwrap_or("").to_owned(),
    })
}

async fn sum_with_range(
    pool: &SqlitePool,
    sql_prefix: &str,
    id: &str,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> AppResult<(i64, i64)> {
    let mut query = QueryBuilder::<Sqlite>::new(sql_prefix);
    query.push_bind(id.to_owned());
    if let Some(from) = from_date {
        query
            .push(" AND business_date >= ")
            .push_bind(from.to_owned());
    }
    if let Some(to) = to_date {
        query
            .push(" AND business_date <= ")
            .push_bind(to.to_owned());
    }
    let row = query.build_query_as::<(i64, i64)>().fetch_one(pool).await?;
    Ok(row)
}

async fn scalar_with_range(
    pool: &SqlitePool,
    sql_prefix: &str,
    id: &str,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> AppResult<i64> {
    let mut query = QueryBuilder::<Sqlite>::new(sql_prefix);
    query.push_bind(id.to_owned());
    if let Some(from) = from_date {
        query
            .push(" AND business_date >= ")
            .push_bind(from.to_owned());
    }
    if let Some(to) = to_date {
        query
            .push(" AND business_date <= ")
            .push_bind(to.to_owned());
    }
    let row = query.build_query_as::<(i64,)>().fetch_one(pool).await?;
    Ok(row.0)
}

/// Current Points Liability: total unredeemed points across active accounts
/// (ADR-0014). Read from the `report_points_liability` view so the SQL stays
/// single-sourced.
pub async fn points_liability_total(pool: &SqlitePool) -> AppResult<i64> {
    let row = sqlx::query_scalar::<_, i64>(
        "SELECT total_points_liability_centi FROM report_points_liability",
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Searches the canonical audit log with the supplied filters. Returns rows
/// reverse-chronologically. The join to `accounts` and `pcs` is left-outer so
/// deleted accounts and legacy PC-less rows stay visible (ADR-0011).
pub async fn search_audit_log(
    pool: &SqlitePool,
    filter: &AuditFilterInput,
) -> AppResult<Vec<AuditRow>> {
    validate_date_range(filter.from_date.as_deref(), filter.to_date.as_deref())?;
    let limit = clamp_limit(filter.limit, 200, 1_000);
    let offset = filter.offset.unwrap_or(0).max(0);

    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT a.id, a.event_type, a.outcome, a.actor,
                CASE
                    WHEN a.event_type IN ('account_transfer_in', 'points_transfer_in')
                    THEN COALESCE(
                        json_extract(a.metadata_json, '$.from_account_id'),
                        a.actor_account_id
                    )
                    ELSE a.actor_account_id
                END AS actor_account_id,
                actor_acct.username AS actor_account_username,
                a.account_id, acct.username AS account_username,
                a.pc_id, p.name AS pc_name,
                a.amount_cents, a.seconds_delta, a.attendance_seconds_delta,
                a.points_delta_centi, a.cash_flag,
                a.summary, a.metadata_json, a.created_at
         FROM audit_log a
         LEFT JOIN accounts actor_acct ON actor_acct.id = CASE
             WHEN a.event_type IN ('account_transfer_in', 'points_transfer_in')
             THEN COALESCE(
                 json_extract(a.metadata_json, '$.from_account_id'),
                 a.actor_account_id
             )
             ELSE a.actor_account_id
         END
         LEFT JOIN accounts acct ON acct.id = a.account_id
         LEFT JOIN pcs p ON p.id = a.pc_id
         WHERE 1=1",
    );
    push_audit_filters(&mut query, filter);
    query
        .push(" ORDER BY a.created_at DESC, a.id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);

    let rows = query.build().fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| -> Result<AuditRow, sqlx::Error> {
            let event_type = row.try_get::<String, _>("event_type")?;
            Ok(AuditRow {
                id: row.try_get("id")?,
                event_type: event_type.clone(),
                category: category_label(&event_type),
                outcome: row.try_get("outcome")?,
                actor: row.try_get("actor")?,
                actor_account_id: row.try_get("actor_account_id")?,
                actor_account_username: row.try_get("actor_account_username")?,
                account_id: row.try_get("account_id")?,
                account_username: row.try_get("account_username")?,
                pc_id: row.try_get("pc_id")?,
                pc_name: row.try_get("pc_name")?,
                amount_cents: row.try_get("amount_cents")?,
                seconds_delta: row.try_get("seconds_delta")?,
                attendance_seconds_delta: row.try_get("attendance_seconds_delta")?,
                points_delta_centi: row.try_get("points_delta_centi")?,
                cash_flag: row.try_get::<i64, _>("cash_flag")? == 1,
                summary: row.try_get("summary")?,
                metadata_json: row.try_get("metadata_json")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Counts every audit row matching the filters, ignoring limit/offset, so the
/// viewer can render a total and a page count alongside the current page.
pub async fn count_audit_log(pool: &SqlitePool, filter: &AuditFilterInput) -> AppResult<i64> {
    validate_date_range(filter.from_date.as_deref(), filter.to_date.as_deref())?;
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT COUNT(*) FROM audit_log a
         LEFT JOIN accounts actor_acct ON actor_acct.id = CASE
             WHEN a.event_type IN ('account_transfer_in', 'points_transfer_in')
             THEN COALESCE(
                 json_extract(a.metadata_json, '$.from_account_id'),
                 a.actor_account_id
             )
             ELSE a.actor_account_id
         END
         LEFT JOIN accounts acct ON acct.id = a.account_id
         LEFT JOIN pcs p ON p.id = a.pc_id
         WHERE 1=1",
    );
    push_audit_filters(&mut query, filter);
    let row = query.build_query_as::<(i64,)>().fetch_one(pool).await?;
    Ok(row.0)
}

/// Appends every audit `WHERE` predicate shared by the viewer, the total count,
/// and the CSV export, so a filter can never mean one thing on screen and
/// another in the export (ADR-0015).
fn push_audit_filters(query: &mut QueryBuilder<'_, Sqlite>, filter: &AuditFilterInput) {
    if let Some(from) = filter.from_date.as_deref() {
        query
            .push(" AND date(a.created_at, 'localtime') >= ")
            .push_bind(from.to_owned());
    }
    if let Some(to) = filter.to_date.as_deref() {
        query
            .push(" AND date(a.created_at, 'localtime') <= ")
            .push_bind(to.to_owned());
    }
    if let Some(pc_id) = filter.pc_id.as_deref() {
        let pc_id = pc_id.trim();
        if !pc_id.is_empty() {
            query.push(" AND a.pc_id = ").push_bind(pc_id.to_owned());
        }
    }
    if let Some(account_id) = filter.account_id.as_deref() {
        let account_id = account_id.trim();
        if !account_id.is_empty() {
            query
                .push(" AND a.account_id = ")
                .push_bind(account_id.to_owned());
        }
    }
    if let Some(actors) = &filter.actor {
        if !actors.is_empty() {
            query.push(" AND a.actor IN (");
            for (i, actor) in actors.iter().enumerate() {
                if i > 0 {
                    query.push(", ");
                }
                query.push_bind(actor.to_owned());
            }
            query.push(")");
        }
    }
    if let Some(outcome) = filter.outcome.as_deref() {
        let outcome = outcome.trim();
        if !outcome.is_empty() {
            query
                .push(" AND a.outcome = ")
                .push_bind(outcome.to_owned());
        }
    }
    if let Some(category) = filter.category.as_deref() {
        let category = category.trim();
        if !category.is_empty() {
            let parsed = match category {
                "auth" => Some(AuditCategory::Auth),
                "money" => Some(AuditCategory::Money),
                "time" => Some(AuditCategory::Time),
                "points" => Some(AuditCategory::Points),
                "admin" => Some(AuditCategory::Admin),
                "system" => Some(AuditCategory::System),
                _ => None,
            };
            if let Some(parsed) = parsed {
                let event_types = parsed.event_types();
                // The event types come from a compile-time static array in
                // openpiso-core, not user input, so they are safe to inline
                // as SQL literals. This avoids a QueryBuilder IN-clause bind
                // ordering issue where the binds were not being matched to
                // their placeholders after the JOINs.
                query.push(" AND a.event_type IN (");
                for (index, event_type) in event_types.iter().enumerate() {
                    if index > 0 {
                        query.push(", ");
                    }
                    query.push("'").push(event_type).push("'");
                }
                query.push(")");
            }
        }
    }
    if let Some(event_types) = &filter.event_type {
        if !event_types.is_empty() {
            query.push(" AND a.event_type IN (");
            for (i, et) in event_types.iter().enumerate() {
                if i > 0 {
                    query.push(", ");
                }
                query.push_bind(et.to_owned());
            }
            query.push(")");
        }
    }
    if let Some(search) = filter.search.as_deref() {
        let search = search.trim();
        if !search.is_empty() {
            query
                .push(" AND (a.summary LIKE '%' || ")
                .push_bind(search.to_owned())
                .push(" || '%' OR actor_acct.username LIKE '%' || ")
                .push_bind(search.to_owned())
                .push(" || '%')");
        }
    }
}

/// Exports the audit rows matching the supplied filters as RFC 4180 CSV.
/// Reuses `search_audit_log` so the export always respects the same filters
/// as the on-screen viewer (ADR-0015). Always exports the whole filtered set
/// from the first row, so the viewer's current page never truncates the export.
pub async fn export_audit_csv(pool: &SqlitePool, filter: &AuditFilterInput) -> AppResult<String> {
    let export_filter = AuditFilterInput {
        limit: Some(filter.limit.unwrap_or(50_000).clamp(1, 100_000)),
        offset: None,
        from_date: filter.from_date.clone(),
        to_date: filter.to_date.clone(),
        pc_id: filter.pc_id.clone(),
        account_id: filter.account_id.clone(),
        actor: filter.actor.clone(),
        outcome: filter.outcome.clone(),
        category: filter.category.clone(),
        event_type: filter.event_type.clone(),
        search: filter.search.clone(),
    };
    let rows = search_audit_log(pool, &export_filter).await?;
    let mut csv = String::new();
    csv.push_str(&csv_header());
    for row in rows {
        csv.push_str("\r\n");
        csv.push_str(&csv_row(&row));
    }
    Ok(csv)
}

fn csv_header() -> String {
    [
        "created_at",
        "event_type",
        "category",
        "outcome",
        "actor",
        "actor_account_username",
        "account_id",
        "account_username",
        "pc_id",
        "pc_name",
        "amount_cents",
        "seconds_delta",
        "attendance_seconds_delta",
        "points_delta_centi",
        "cash_flag",
        "summary",
        "id",
    ]
    .join(",")
}

/// Renders a stored audit timestamp as local wall clock. `audit_log.created_at`
/// is written by SQLite's `CURRENT_TIMESTAMP`, which is UTC, while the viewer
/// renders it in the shop's local zone — the export has to agree with the
/// screen or the same event appears to happen at two different times.
fn to_local_timestamp(created_at: &str) -> String {
    match NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S") {
        Ok(naive) => Utc
            .from_utc_datetime(&naive)
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        // An unparseable timestamp is passed through rather than dropped so the
        // row still exports.
        Err(_) => created_at.to_owned(),
    }
}

fn csv_row(row: &AuditRow) -> String {
    let fields: [String; 17] = [
        csv_escape(&to_local_timestamp(&row.created_at)),
        csv_escape(&row.event_type),
        csv_escape(&row.category),
        csv_escape(&row.outcome),
        csv_escape(&row.actor),
        csv_escape(row.actor_account_username.as_deref().unwrap_or("")),
        csv_escape(row.account_id.as_deref().unwrap_or("")),
        csv_escape(row.account_username.as_deref().unwrap_or("")),
        csv_escape(row.pc_id.as_deref().unwrap_or("")),
        csv_escape(row.pc_name.as_deref().unwrap_or("")),
        csv_escape(&row.amount_cents.map(|v| v.to_string()).unwrap_or_default()),
        csv_escape(&row.seconds_delta.map(|v| v.to_string()).unwrap_or_default()),
        csv_escape(
            &row.attendance_seconds_delta
                .map(|v| v.to_string())
                .unwrap_or_default(),
        ),
        csv_escape(
            &row.points_delta_centi
                .map(|v| v.to_string())
                .unwrap_or_default(),
        ),
        csv_escape(if row.cash_flag { "1" } else { "0" }),
        csv_escape(&row.summary),
        csv_escape(&row.id),
    ];
    fields.join(",")
}

/// Escapes a single CSV field per RFC 4180: wrap the value in double quotes
/// when it contains a comma, double quote, newline, or carriage return, and
/// double any embedded double quotes.
pub fn csv_escape(value: &str) -> String {
    if value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// Returns the distinct `actor` values in the audit log for populating the
/// multi-select filter on the web-admin audit page.
pub async fn distinct_actors(pool: &SqlitePool) -> AppResult<Vec<String>> {
    let rows =
        sqlx::query_scalar::<_, String>("SELECT DISTINCT actor FROM audit_log ORDER BY actor")
            .fetch_all(pool)
            .await?;
    Ok(rows)
}

/// Returns the distinct `event_type` values in the audit log for populating
/// the multi-select filter on the web-admin audit page.
pub async fn distinct_event_types(pool: &SqlitePool) -> AppResult<Vec<String>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT event_type FROM audit_log ORDER BY event_type",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Categorizes a canonical event_type into the audit viewer's filter
/// category. Returns `"other"` for event types the repository does not yet
/// map, so the viewer never hides unknown future events.
pub fn category_label(event_type: &str) -> String {
    category_for_event_type(event_type)
        .map(|category| category.as_str().to_owned())
        .unwrap_or_else(|| "other".to_owned())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;
    use uuid::Uuid;

    use super::*;
    use crate::{
        accounts::{self, AccountCreator},
        db,
        models::{AuditFilterInput, CreateVoucherBatchInput, ManualAdjustmentInput},
    };

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        db::seed(&pool).await?;
        Ok(pool)
    }

    async fn test_account(pool: &SqlitePool, username: &str) -> AppResult<crate::models::Account> {
        accounts::create(pool, username, "password", AccountCreator::Admin, None).await
    }

    fn filter_with_limit(limit: i64) -> AuditFilterInput {
        AuditFilterInput {
            limit: Some(limit),
            offset: None,
            from_date: None,
            to_date: None,
            pc_id: None,
            account_id: None,
            actor: None,
            outcome: None,
            category: None,
            event_type: None,
            search: None,
        }
    }

    #[tokio::test]
    async fn report_summary_reconciles_cash_and_free_time_with_audit_events() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "report-user").await?;
        // Coin (cash + account time + points) on pc-01. The account must be
        // logged in so the coin routes to it and earns a whole point.
        let sessions = crate::sessions::SessionStore::default();
        let tokens = crate::auth::AccountTokens::default();
        let mut bound = account.clone();
        bound.remaining_seconds = 0;
        sessions.login(&pool, &tokens, &bound, "pc-01").await?;
        sessions
            .record_coin_event(
                &pool,
                &crate::models::CoinEventInput {
                    pc_id: "pc-01".to_owned(),
                    amount_cents: 5_000,
                    pulse_count: Some(1),
                    timestamp: None,
                },
                &crate::models::CoinSourceClaim::Simulator,
            )
            .await?;
        // Manual add-time on pc-02 (free time dispensed).
        db::create_manual_adjustment(
            &pool,
            &ManualAdjustmentInput {
                pc_id: "pc-02".to_owned(),
                seconds_delta: 600,
                reason: Some("comp time".to_owned()),
            },
        )
        .await?;
        // Promo voucher minted and redeemed (free time dispensed).
        let voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        db::redeem_voucher(&pool, &account.id, "pc-01", &voucher.code).await?;

        let summary = report_summary(&pool, ReportPeriod::Day, None, None).await?;

        // Cash revenue = 5_000 cents (one coin routed to the account).
        // Sold voucher = 0 (no sold voucher minted). Promo voucher mint is
        // non-cash.
        assert_eq!(summary.cash_revenue_cents, 5_000);
        // Free time dispensed = 600 manual + 300 promo redemption.
        assert_eq!(summary.free_time_dispensed_seconds, 900);
        // Points liability = 1.00 point = 100 centipoints (one point earned
        // on the 5_000 cent account coin at the default 5_000 cents-per-point
        // earn rate).
        assert_eq!(summary.points_liability_centi, 100);
        Ok(())
    }

    #[tokio::test]
    async fn coin_promo_reports_full_cash_and_bonus_only_free_time_by_account_and_pc(
    ) -> AppResult<()> {
        let pool = test_pool().await?;
        let promoted_account = test_account(&pool, "promoted-report-user").await?;
        let regular_account = test_account(&pool, "regular-report-user").await?;
        sqlx::query("INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (500, 120)")
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE coin_promo_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;

        let sessions = crate::sessions::SessionStore::default();
        let tokens = crate::auth::AccountTokens::default();
        sessions
            .login(&pool, &tokens, &promoted_account, "pc-01")
            .await?;
        sessions
            .login(&pool, &tokens, &regular_account, "pc-02")
            .await?;

        // pc-02's ₱2 is below the only tier, so no part of it earns a bonus.
        for (pc_id, amount_cents) in [("pc-01", 500), ("pc-02", 200)] {
            sessions
                .record_coin_event(
                    &pool,
                    &crate::models::CoinEventInput {
                        pc_id: pc_id.to_owned(),
                        amount_cents,
                        pulse_count: Some(1),
                        timestamp: None,
                    },
                    &crate::models::CoinSourceClaim::Simulator,
                )
                .await?;
        }

        let summary = report_summary(&pool, ReportPeriod::Day, None, None).await?;
        assert_eq!(summary.cash_revenue_cents, 700);
        assert_eq!(summary.free_time_dispensed_seconds, 120);

        let promoted = account_report(&pool, &promoted_account.id, None, None).await?;
        let regular = account_report(&pool, &regular_account.id, None, None).await?;
        assert_eq!(promoted.cash_revenue_cents, 500);
        assert_eq!(promoted.free_time_dispensed_seconds, 120);
        assert_eq!(regular.cash_revenue_cents, 200);
        assert_eq!(regular.free_time_dispensed_seconds, 0);

        let promoted_pc = pc_report(&pool, "pc-01", None, None).await?;
        let regular_pc = pc_report(&pool, "pc-02", None, None).await?;
        assert_eq!(promoted_pc.cash_revenue_cents, 500);
        assert_eq!(promoted_pc.free_time_dispensed_seconds, 120);
        assert_eq!(regular_pc.cash_revenue_cents, 200);
        assert_eq!(regular_pc.free_time_dispensed_seconds, 0);
        Ok(())
    }

    #[tokio::test]
    async fn report_summary_excludes_sold_voucher_redemption_from_free_time() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "sold-user").await?;
        let voucher = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 3_600,
                points_value_centi: 0,
                sold_amount_cents: Some(1_000),
                expires_at: None,
            },
        )
        .await?;
        db::redeem_voucher(&pool, &account.id, "pc-01", &voucher.code).await?;

        let summary = report_summary(&pool, ReportPeriod::Day, None, None).await?;

        // Cash revenue recognized at mint, not redemption.
        assert_eq!(summary.cash_revenue_cents, 1_000);
        // Sold voucher redemption is prepaid, not free time.
        assert_eq!(summary.free_time_dispensed_seconds, 0);
        Ok(())
    }

    #[tokio::test]
    async fn report_counts_both_sold_voucher_cash_paths_without_double_counting() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "voucher-report-user").await?;
        let single = db::create_voucher(
            &pool,
            &crate::models::CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 600,
                points_value_centi: 0,
                sold_amount_cents: Some(500),
                expires_at: None,
            },
        )
        .await?;
        db::redeem_voucher(&pool, &account.id, "pc-01", &single.code).await?;

        let batch = db::create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "RPT".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 1_200,
                points_value_centi: 0,
                cash_amount_cents: Some(1_000),
                expires_at: None,
            },
        )
        .await?;
        let batch_code: String = sqlx::query_scalar("SELECT code FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_one(&pool)
            .await?;
        db::redeem_voucher(&pool, &account.id, "pc-01", &batch_code).await?;

        let summary = report_summary(&pool, ReportPeriod::Day, None, None).await?;
        let report_rows: (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), SUM(amount_cents) FROM report_cash_revenue
             WHERE event_type IN ('voucher_sold', 'sold_voucher_redeemed')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(summary.cash_revenue_cents, 1_500);
        assert_eq!(report_rows, (2, 1_500));
        Ok(())
    }

    #[tokio::test]
    async fn points_liability_excludes_deleted_accounts() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "liability-user").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 7 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;
        assert_eq!(points_liability_total(&pool).await?, 7);

        accounts::soft_delete(&pool, &account.id).await?;
        assert_eq!(points_liability_total(&pool).await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn per_pc_report_filters_by_pc_and_date() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        db::simulate_coin(&pool, "pc-02", 200).await?;

        let pc_one = pc_report(&pool, "pc-01", None, None).await?;
        let pc_two = pc_report(&pool, "pc-02", None, None).await?;
        assert_eq!(pc_one.cash_revenue_cents, 100);
        assert_eq!(pc_one.seconds_sold, 300);
        assert_eq!(pc_two.cash_revenue_cents, 200);
        assert_eq!(pc_two.seconds_sold, 600);

        // Unknown PC ID -> NotFound.
        assert!(matches!(
            pc_report(&pool, "pc-missing", None, None).await,
            Err(AppError::NotFound(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn sales_report_overview_returns_current_ranges_and_all_pcs() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        sqlx::query("UPDATE pcs SET visible = 0 WHERE id = 'pc-04'")
            .execute(&pool)
            .await?;

        let overview = sales_report_overview(&pool).await?;
        let today = Local::now().date_naive();
        let today_string = today.format("%Y-%m-%d").to_string();
        let week_start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
        let week_end = week_start + Duration::days(6);
        let month_start = today.with_day(1).unwrap_or(today);
        let week_start_string = week_start.format("%Y-%m-%d").to_string();
        let week_end_string = week_end.format("%Y-%m-%d").to_string();
        let month_start_string = month_start.format("%Y-%m-%d").to_string();

        assert_eq!(
            overview.daily.from_date.as_deref(),
            Some(today_string.as_str())
        );
        assert_eq!(
            overview.daily.to_date.as_deref(),
            Some(today_string.as_str())
        );
        assert_eq!(
            overview.weekly.from_date.as_deref(),
            Some(week_start_string.as_str())
        );
        assert_eq!(
            overview.weekly.to_date.as_deref(),
            Some(week_end_string.as_str())
        );
        assert_eq!(
            overview.monthly.from_date.as_deref(),
            Some(month_start_string.as_str())
        );
        assert_eq!(
            overview.monthly.to_date.as_deref(),
            Some(today_string.as_str())
        );
        assert_eq!(overview.total.from_date, None);
        assert_eq!(overview.total.to_date, None);
        assert_eq!(overview.total.cash_revenue_cents, 100);

        assert_eq!(overview.pcs.len(), 4);
        assert_eq!(overview.pcs[0].id, "pc-01");
        assert_eq!(overview.pcs[0].daily_sales_cents, 100);
        assert_eq!(overview.pcs[0].total_sales_cents, 100);
        let hidden_pc = overview
            .pcs
            .iter()
            .find(|pc| pc.id == "pc-04")
            .expect("hidden PC should remain in owner reports");
        assert_eq!(hidden_pc.daily_sales_cents, 0);
        assert_eq!(hidden_pc.total_sales_cents, 0);
        Ok(())
    }

    #[tokio::test]
    async fn per_account_report_filters_by_account() -> AppResult<()> {
        let pool = test_pool().await?;
        let first = test_account(&pool, "first-report").await?;
        let second = test_account(&pool, "second-report").await?;
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 0, points_balance_centi = 0 WHERE id IN (?, ?)",
        )
        .bind(&first.id)
        .bind(&second.id)
        .execute(&pool)
        .await?;

        // Route a coin to the first account by logging it in.
        let sessions = crate::sessions::SessionStore::default();
        let tokens = crate::auth::AccountTokens::default();
        let mut first_bound = first.clone();
        first_bound.remaining_seconds = 0;
        sessions
            .login(&pool, &tokens, &first_bound, "pc-01")
            .await?;
        sessions
            .record_coin_event(
                &pool,
                &crate::models::CoinEventInput {
                    pc_id: "pc-01".to_owned(),
                    amount_cents: 100,
                    pulse_count: Some(1),
                    timestamp: None,
                },
                &crate::models::CoinSourceClaim::Simulator,
            )
            .await?;

        let first_report = account_report(&pool, &first.id, None, None).await?;
        let second_report = account_report(&pool, &second.id, None, None).await?;
        assert_eq!(first_report.cash_revenue_cents, 100);
        assert_eq!(first_report.seconds_sold, 300);
        assert_eq!(second_report.cash_revenue_cents, 0);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_date_range_is_rejected() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            report_summary(
                &pool,
                ReportPeriod::Day,
                Some("2026-06-22"),
                Some("2026-06-21"),
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            report_summary(
                &pool,
                ReportPeriod::Day,
                Some("2026-06-21"),
                Some("not-a-date"),
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn audit_viewer_returns_rows_reverse_chronologically() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        db::simulate_coin(&pool, "pc-02", 200).await?;

        let rows = search_audit_log(&pool, &filter_with_limit(10)).await?;
        assert_eq!(rows.len(), 2);
        assert!(rows[0].created_at >= rows[1].created_at);
        assert_eq!(
            rows.iter()
                .filter(|r| r.event_type == "coin_accepted")
                .count(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn audit_viewer_and_csv_keep_attendance_delta_separate() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, seconds_delta, attendance_seconds_delta, summary)
             VALUES ('attendance-claim-audit', 'attendance_claim', 'success', 'account', 3600, -3600, 'Attendance claimed')",
        )
        .execute(&pool)
        .await?;

        let filter = AuditFilterInput {
            event_type: Some(vec!["attendance_claim".to_owned()]),
            ..filter_with_limit(50)
        };
        let rows = search_audit_log(&pool, &filter).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seconds_delta, Some(3_600));
        assert_eq!(rows[0].attendance_seconds_delta, Some(-3_600));

        let export = export_audit_csv(&pool, &filter).await?;
        assert!(export
            .lines()
            .next()
            .is_some_and(|header| header
                .contains("seconds_delta,attendance_seconds_delta,points_delta_centi")));
        assert!(export
            .lines()
            .any(|line| line.ends_with("attendance-claim-audit") && line.contains(",3600,-3600,")));
        Ok(())
    }

    #[tokio::test]
    async fn audit_viewer_filters_by_pc_account_category_and_search() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "audit-filter").await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        db::create_manual_adjustment(
            &pool,
            &ManualAdjustmentInput {
                pc_id: "pc-02".to_owned(),
                seconds_delta: 300,
                reason: Some("refund courtesy".to_owned()),
            },
        )
        .await?;

        let pc_filter = AuditFilterInput {
            pc_id: Some("pc-01".to_owned()),
            ..filter_with_limit(50)
        };
        let pc_rows = search_audit_log(&pool, &pc_filter).await?;
        assert!(pc_rows.iter().all(|r| r.pc_id.as_deref() == Some("pc-01")));

        let account_filter = AuditFilterInput {
            account_id: Some(account.id.clone()),
            ..filter_with_limit(50)
        };
        let account_rows = search_audit_log(&pool, &account_filter).await?;
        assert!(account_rows
            .iter()
            .all(|r| r.account_id.as_deref() == Some(account.id.as_str())));

        let category_filter = AuditFilterInput {
            category: Some("time".to_owned()),
            ..filter_with_limit(50)
        };
        let category_rows = search_audit_log(&pool, &category_filter).await?;
        assert!(category_rows
            .iter()
            .all(|r| r.event_type == "manual_adjustment"));
        assert_eq!(category_rows.len(), 1);

        let search_filter = AuditFilterInput {
            search: Some("refund".to_owned()),
            ..filter_with_limit(50)
        };
        let search_rows = search_audit_log(&pool, &search_filter).await?;
        assert_eq!(search_rows.len(), 1);
        assert_eq!(search_rows[0].event_type, "manual_adjustment");
        Ok(())
    }

    #[tokio::test]
    async fn audit_viewer_resolves_initiating_account_for_transfer_rows_and_search() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let sender = test_account(&pool, "audit-sender").await?;
        let recipient = test_account(&pool, "audit-recipient").await?;

        // This represents a historical transfer-in row written before actor
        // attribution was corrected: the recipient was stored as the actor,
        // while metadata identifies the initiating sender.
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, actor_account_id, account_id,
              summary, metadata_json)
             VALUES (?, 'account_transfer_in', 'success', 'account', ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&recipient.id)
        .bind(&recipient.id)
        .bind("Received 1min 0sec from audit-sender")
        .bind(serde_json::json!({ "from_account_id": sender.id }).to_string())
        .execute(&pool)
        .await?;

        let rows = search_audit_log(&pool, &filter_with_limit(10)).await?;
        let transfer = rows
            .iter()
            .find(|row| row.event_type == "account_transfer_in")
            .expect("historical transfer should be present");
        assert_eq!(
            transfer.actor_account_id.as_deref(),
            Some(sender.id.as_str())
        );
        assert_eq!(
            transfer.actor_account_username.as_deref(),
            Some("audit-sender")
        );

        let search_rows = search_audit_log(
            &pool,
            &AuditFilterInput {
                search: Some("audit-sender".to_owned()),
                ..filter_with_limit(10)
            },
        )
        .await?;
        assert_eq!(search_rows.len(), 1);
        assert_eq!(search_rows[0].event_type, "account_transfer_in");
        Ok(())
    }

    #[tokio::test]
    async fn audit_viewer_categorizes_known_events_and_marks_unknown_as_other() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        let rows = search_audit_log(&pool, &filter_with_limit(10)).await?;
        let coin = rows
            .iter()
            .find(|r| r.event_type == "coin_accepted")
            .expect("coin event should be present");
        assert_eq!(coin.category, "money");

        // Insert an unknown event type directly to verify the fallback.
        // (Bypasses the audit helper because we want an event_type the
        // category map does not know.)
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, summary, metadata_json)
             VALUES (?, 'future_event', 'success', 'system', 'future', '{}')",
        )
        .bind(Uuid::new_v4().to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let rows = search_audit_log(&pool, &filter_with_limit(50)).await?;
        let future = rows
            .iter()
            .find(|r| r.event_type == "future_event")
            .expect("future event should be present");
        assert_eq!(future.category, "other");
        Ok(())
    }

    #[tokio::test]
    async fn audit_append_only_triggers_block_mutation() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        // The audit_log triggers installed in M1 reject any UPDATE or DELETE.
        // The M6 reporting views read from audit_log, so the append-only
        // guarantee also protects every report total.
        assert!(
            sqlx::query("UPDATE audit_log SET summary = summary WHERE 1=1")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query("DELETE FROM audit_log WHERE 1=1")
            .execute(&pool)
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn csv_export_respects_filters_and_escapes_correctly() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;
        db::create_manual_adjustment(
            &pool,
            &ManualAdjustmentInput {
                pc_id: "pc-02".to_owned(),
                seconds_delta: 300,
                reason: Some("has, comma and \"quote\"".to_owned()),
            },
        )
        .await?;

        let export = export_audit_csv(
            &pool,
            &AuditFilterInput {
                category: Some("time".to_owned()),
                ..filter_with_limit(50)
            },
        )
        .await?;
        assert!(export.starts_with(
            "created_at,event_type,category,outcome,actor,actor_account_username,account_id,"
        ));
        // Only the manual adjustment row should be present (one CRLF between
        // the header and the data row).
        assert_eq!(export.matches("\r\n").count(), 1);
        // The summary includes the reason; the CSV field is quoted because it
        // contains a comma, and embedded double quotes are doubled.
        assert!(export.contains("\"Added Guest Time manually: has, comma and \"\"quote\"\"\""));
        Ok(())
    }

    #[tokio::test]
    async fn audit_viewer_pages_without_gaps_or_repeats_and_reports_the_total() -> AppResult<()> {
        let pool = test_pool().await?;
        for _ in 0..12 {
            db::simulate_coin(&pool, "pc-01", 100).await?;
        }

        let filter = filter_with_limit(5);
        let total = count_audit_log(&pool, &filter).await?;
        assert!(total >= 12, "each coin writes at least one audit row");

        // Walk every page and collect the ids in order.
        let mut paged: Vec<String> = Vec::new();
        let mut offset = 0;
        while offset < total {
            let page = search_audit_log(
                &pool,
                &AuditFilterInput {
                    offset: Some(offset),
                    ..filter_with_limit(5)
                },
            )
            .await?;
            assert!(
                !page.is_empty(),
                "page at offset {offset} must not be empty"
            );
            paged.extend(page.into_iter().map(|row| row.id));
            offset += 5;
        }

        // The paged walk must reproduce the unpaged list exactly: no row
        // skipped at a page boundary, none served twice.
        let all = search_audit_log(&pool, &filter_with_limit(1_000)).await?;
        let expected: Vec<String> = all.into_iter().map(|row| row.id).collect();
        assert_eq!(paged, expected);
        assert_eq!(paged.len() as i64, total);

        // The count ignores limit/offset but still honours the filters, so it
        // has to agree with an unpaged search under the same filter.
        let filtered = AuditFilterInput {
            category: Some("money".to_owned()),
            ..filter_with_limit(1_000)
        };
        let filtered_total = count_audit_log(&pool, &filtered).await?;
        let filtered_rows = search_audit_log(&pool, &filtered).await?;
        assert_eq!(filtered_total, filtered_rows.len() as i64);
        assert!(filtered_total > 0 && filtered_total <= total);
        Ok(())
    }

    #[tokio::test]
    async fn csv_export_ignores_the_viewers_page_window() -> AppResult<()> {
        let pool = test_pool().await?;
        for _ in 0..12 {
            db::simulate_coin(&pool, "pc-01", 100).await?;
        }

        // The viewer is on page 2; the export must still contain every row,
        // starting from the newest, not just that page.
        let export = export_audit_csv(
            &pool,
            &AuditFilterInput {
                limit: None,
                offset: Some(5),
                ..filter_with_limit(5)
            },
        )
        .await?;
        let total = count_audit_log(&pool, &filter_with_limit(5)).await?;
        assert_eq!(export.matches("\r\n").count() as i64, total);
        Ok(())
    }

    #[tokio::test]
    async fn csv_export_emits_local_timestamps_matching_the_viewer() -> AppResult<()> {
        let pool = test_pool().await?;
        db::simulate_coin(&pool, "pc-01", 100).await?;

        let (utc, local): (String, String) = sqlx::query_as(
            "SELECT created_at, datetime(created_at, 'localtime')
               FROM audit_log ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await?;

        let export = export_audit_csv(&pool, &filter_with_limit(50)).await?;
        let first_field = export
            .lines()
            .nth(1)
            .unwrap_or_default()
            .split(',')
            .next()
            .unwrap_or_default()
            .to_owned();
        assert_eq!(
            first_field, local,
            "CSV must render the same local wall clock as the viewer (stored UTC: {utc})"
        );
        Ok(())
    }

    #[tokio::test]
    async fn csv_escape_handles_special_characters() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
        assert_eq!(csv_escape("a\rb"), "\"a\rb\"");
        assert_eq!(csv_escape(""), "");
    }

    /// Deterministic RNG that returns a pre-defined draw sequence for wheel
    /// outcome control in tests.
    struct QueuedRng {
        draws: Vec<i64>,
        position: usize,
    }

    impl openpiso_core::wheel::SpinRng for QueuedRng {
        fn gen_below(&mut self, _bound: i64) -> i64 {
            let value = self.draws[self.position % self.draws.len()];
            self.position += 1;
            value
        }
    }

    #[tokio::test]
    async fn report_summary_includes_points_minted_by_wheel_gross_not_net() -> AppResult<()> {
        let pool = test_pool().await?;

        // Configure a single wheel segment that awards 50 centipoints.
        sqlx::query("DELETE FROM wheel_segments")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO wheel_segments (id, position, label, seconds, points_centi, free_spin)
             VALUES (?, 1, ?, ?, ?, 0)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind("50 cp")
        .bind(0_i64)
        .bind(50_i64)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1, free_spin_cap = 0 WHERE id = 1")
            .execute(&pool)
            .await?;

        // Create an account with enough points to spin.
        let account = test_account(&pool, "minted-wheel").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 200 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        // Spin once: draw index 0 -> first segment (50 cp).
        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        let _mutation = db::points_wheel::spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;

        let summary = report_summary(&pool, ReportPeriod::Day, None, None).await?;

        // Gross points minted: 50. Net points change: 50 - 100 = -50.
        // The report exposes gross (points_minted_by_wheel_centi), not net.
        assert_eq!(
            summary.points_minted_by_wheel_centi, 50,
            "should report gross 50 centipoints minted"
        );
        assert_eq!(
            summary.free_time_dispensed_seconds, 0,
            "points-only spin adds no free time"
        );

        // Verify the audit trail shows net -50.
        let audit: (i64,) = sqlx::query_as(
            "SELECT points_delta_centi FROM audit_log WHERE event_type = 'wheel_spin'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit.0, -50, "audit net is -50 (gross 50 - cost 100)");
        Ok(())
    }

    #[tokio::test]
    async fn report_summary_points_minted_excludes_spins_that_won_nothing() -> AppResult<()> {
        let pool = test_pool().await?;

        // Default wheel has a 'nothing' segment at position 4 (index 3).
        // Force it to be the only segment so any draw wins nothing.
        sqlx::query("DELETE FROM wheel_segments")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO wheel_segments (id, position, label, seconds, points_centi, free_spin)
             VALUES (?, 1, 'nothing', 0, 0, 0)",
        )
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1, free_spin_cap = 0 WHERE id = 1")
            .execute(&pool)
            .await?;

        let account = test_account(&pool, "minted-nothing").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 200 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        let _mutation = db::points_wheel::spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;

        let summary = report_summary(&pool, ReportPeriod::Day, None, None).await?;

        // Won nothing -> gross points minted = 0.
        assert_eq!(summary.points_minted_by_wheel_centi, 0);
        Ok(())
    }
}

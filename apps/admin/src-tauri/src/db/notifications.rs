use openpiso_core::notification::{
    alert_event_types, classify_audit_event, notice_event_types, notification_event_types,
    NotificationSeverity,
};
use serde::Serialize;
use serde_json::json;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool, Transaction};

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{NotificationItem, NotificationListResponse},
};

const ROLLING_WINDOW_DAYS: i64 = 30;

/// Upper bound on ids per bulk acknowledgement, matching the maximum `limit`
/// the list endpoint accepts so one request can never open an unbounded
/// transaction. Longer owner selections arrive as sequential batches.
pub const MAX_BULK_ACKNOWLEDGE_IDS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSeverityFilter {
    Alert,
    Notice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationUnreadFilter {
    Read,
    Unread,
}

#[derive(Debug, Clone, Copy)]
pub struct NotificationListFilter {
    pub limit: i64,
    pub offset: i64,
    pub severity: Option<NotificationSeverityFilter>,
    pub unread: Option<NotificationUnreadFilter>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationUnreadCount {
    pub unread_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationSeen {
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertAcknowledgement {
    pub acknowledged_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkAlertAcknowledgement {
    pub acknowledged_count: i64,
}

#[derive(Debug, Clone)]
struct NotificationAuditRow {
    id: String,
    event_type: String,
    outcome: String,
    actor: String,
    actor_account_id: Option<String>,
    actor_account_username: Option<String>,
    account_id: Option<String>,
    account_username: Option<String>,
    pc_id: Option<String>,
    pc_name: Option<String>,
    incident_kind: Option<String>,
    amount_cents: Option<i64>,
    seconds_delta: Option<i64>,
    points_delta_centi: Option<i64>,
    summary: String,
    created_at: String,
    acknowledged_at: Option<String>,
    last_seen_at: String,
}

pub async fn list(
    pool: &SqlitePool,
    filter: NotificationListFilter,
) -> AppResult<NotificationListResponse> {
    if filter.offset < 0 {
        return Err(AppError::InvalidInput(
            "offset must be non-negative".to_owned(),
        ));
    }
    if filter.limit <= 0 {
        return Err(AppError::InvalidInput("limit must be positive".to_owned()));
    }

    let mut transaction = pool.begin().await?;
    let now = database_now(&mut transaction).await?;
    // One row beyond the page decides `has_more` without a second count query.
    let mut rows = notification_rows(&mut transaction, &now, filter).await?;
    transaction.commit().await?;

    let has_more = rows.len() as i64 > filter.limit;
    rows.truncate(filter.limit.max(0) as usize);
    let items = rows
        .into_iter()
        .filter_map(NotificationAuditRow::into_item)
        .collect::<Vec<_>>();
    Ok(NotificationListResponse { items, has_more })
}

pub async fn unread_count(pool: &SqlitePool) -> AppResult<NotificationUnreadCount> {
    let mut transaction = pool.begin().await?;
    let now = database_now(&mut transaction).await?;
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT COUNT(*)
         FROM audit_log a
         LEFT JOIN notification_alert_ack ack ON ack.audit_log_id = a.id
         CROSS JOIN notification_read_state state
         WHERE ",
    );
    push_window_predicate(&mut query, &now, None);
    query.push(" AND NOT ");
    push_read_predicate(&mut query);
    let unread_count = query
        .build_query_scalar::<i64>()
        .fetch_one(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(NotificationUnreadCount { unread_count })
}

pub async fn mark_seen(pool: &SqlitePool) -> AppResult<NotificationSeen> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let now = database_now(&mut transaction).await?;
    sqlx::query("UPDATE notification_read_state SET last_seen_at = ? WHERE id = 1")
        .bind(&now)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(NotificationSeen { last_seen_at: now })
}

pub async fn acknowledge(pool: &SqlitePool, audit_id: &str) -> AppResult<AlertAcknowledgement> {
    let audit_id = audit_id.trim();
    if audit_id.is_empty() {
        return Err(AppError::NotFound("notification".to_owned()));
    }

    let mut transaction = crate::db::begin_immediate(pool).await?;
    let now = database_now(&mut transaction).await?;
    if !is_acknowledgeable_alert(&mut transaction, &now, audit_id).await? {
        return Err(AppError::NotFound("notification".to_owned()));
    }

    if !insert_acknowledgement(&mut transaction, &now, audit_id).await? {
        let acknowledged_at = sqlx::query_scalar::<_, String>(
            "SELECT acknowledged_at
             FROM notification_alert_ack
             WHERE audit_log_id = ?",
        )
        .bind(audit_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        return Ok(AlertAcknowledgement { acknowledged_at });
    }

    append_acknowledged_event(&mut transaction, audit_id).await?;
    transaction.commit().await?;
    Ok(AlertAcknowledgement {
        acknowledged_at: now,
    })
}

/// Acknowledges an explicit set of Alerts in one transaction (ADR-0058).
/// Ineligible ids — unknown, non-Alert, or outside the rolling window — are
/// skipped rather than failing the batch, because the owner's list can go stale
/// between render and click. Already-acknowledged ids stay no-ops, so only
/// newly acknowledged Alerts append a `notification_acknowledged` event.
pub async fn acknowledge_many(
    pool: &SqlitePool,
    audit_ids: &[String],
) -> AppResult<BulkAlertAcknowledgement> {
    if audit_ids.is_empty() {
        return Err(AppError::InvalidInput(
            "audit_ids must not be empty".to_owned(),
        ));
    }
    if audit_ids.len() > MAX_BULK_ACKNOWLEDGE_IDS {
        return Err(AppError::InvalidInput(format!(
            "audit_ids must contain at most {MAX_BULK_ACKNOWLEDGE_IDS} ids"
        )));
    }

    let mut unique = Vec::new();
    for audit_id in audit_ids {
        let audit_id = audit_id.trim();
        if !audit_id.is_empty() && !unique.contains(&audit_id) {
            unique.push(audit_id);
        }
    }

    let mut transaction = crate::db::begin_immediate(pool).await?;
    let now = database_now(&mut transaction).await?;
    let mut acknowledged_count = 0;
    for audit_id in unique {
        if !is_acknowledgeable_alert(&mut transaction, &now, audit_id).await? {
            continue;
        }
        if !insert_acknowledgement(&mut transaction, &now, audit_id).await? {
            continue;
        }
        append_acknowledged_event(&mut transaction, audit_id).await?;
        acknowledged_count += 1;
    }
    transaction.commit().await?;
    Ok(BulkAlertAcknowledgement { acknowledged_count })
}

/// True when the id names an Alert inside the rolling notification window.
async fn is_acknowledgeable_alert(
    transaction: &mut Transaction<'_, Sqlite>,
    now: &str,
    audit_id: &str,
) -> AppResult<bool> {
    let event_type = sqlx::query_scalar::<_, String>(
        "SELECT event_type
         FROM audit_log
         WHERE id = ?
           AND created_at >= datetime(?, '-' || ? || ' days')
           AND created_at <= ?",
    )
    .bind(audit_id)
    .bind(now)
    .bind(ROLLING_WINDOW_DAYS)
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await?;

    Ok(event_type
        .is_some_and(|event_type| classify_audit_event(&event_type) == NotificationSeverity::Alert))
}

/// Inserts the acknowledgement row; false means it already existed.
async fn insert_acknowledgement(
    transaction: &mut Transaction<'_, Sqlite>,
    now: &str,
    audit_id: &str,
) -> AppResult<bool> {
    let inserted = sqlx::query(
        "INSERT INTO notification_alert_ack (audit_log_id, acknowledged_at)
         VALUES (?, ?)
         ON CONFLICT(audit_log_id) DO NOTHING",
    )
    .bind(audit_id)
    .bind(now)
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    Ok(inserted > 0)
}

async fn append_acknowledged_event(
    transaction: &mut Transaction<'_, Sqlite>,
    audit_id: &str,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "notification_acknowledged",
        openpiso_core::audit::AuditOutcome::Success,
        openpiso_core::audit::AuditActor::Admin,
        "Notification acknowledged",
    );
    event.metadata = json!({ "audit_id": audit_id });
    audit::append(transaction, event).await?;
    Ok(())
}

async fn database_now(transaction: &mut Transaction<'_, Sqlite>) -> AppResult<String> {
    Ok(sqlx::query_scalar::<_, String>("SELECT CURRENT_TIMESTAMP")
        .fetch_one(&mut **transaction)
        .await?)
}

async fn notification_rows(
    transaction: &mut Transaction<'_, Sqlite>,
    now: &str,
    filter: NotificationListFilter,
) -> AppResult<Vec<NotificationAuditRow>> {
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT a.id, a.event_type, a.outcome, a.actor, a.actor_account_id,
                actor_acct.username AS actor_account_username,
                a.account_id, acct.username AS account_username,
                a.pc_id, p.name AS pc_name, incident.kind AS incident_kind,
                a.amount_cents, a.seconds_delta, a.points_delta_centi,
                a.summary, a.created_at, ack.acknowledged_at,
                state.last_seen_at
         FROM audit_log a
         -- Keep this as the canonical actor-account join. The transfer-initiator
         -- override that attributes received-transfer events to the sender lives
         -- in the Audit search path; lift that override here if either transfer-in
         -- event type is ever promoted out of Silent severity.
         LEFT JOIN accounts actor_acct ON actor_acct.id = a.actor_account_id
         LEFT JOIN accounts acct ON acct.id = a.account_id
         LEFT JOIN pcs p ON p.id = a.pc_id
         LEFT JOIN pc_monitoring_incidents incident
           ON incident.open_audit_id = a.id OR incident.close_audit_id = a.id
         LEFT JOIN notification_alert_ack ack ON ack.audit_log_id = a.id
         CROSS JOIN notification_read_state state
         WHERE ",
    );
    push_window_predicate(&mut query, now, filter.severity);
    match filter.unread {
        Some(NotificationUnreadFilter::Read) => {
            query.push(" AND ");
            push_read_predicate(&mut query);
        }
        Some(NotificationUnreadFilter::Unread) => {
            query.push(" AND NOT ");
            push_read_predicate(&mut query);
        }
        None => {}
    }
    query
        .push(" ORDER BY a.created_at DESC, a.id DESC LIMIT ")
        // One extra row so the caller can report `has_more`.
        .push_bind(filter.limit.saturating_add(1))
        .push(" OFFSET ")
        .push_bind(filter.offset);
    let rows = query.build().fetch_all(&mut **transaction).await?;
    rows.into_iter()
        .map(|row| {
            Ok(NotificationAuditRow {
                id: row.try_get("id")?,
                event_type: row.try_get("event_type")?,
                outcome: row.try_get("outcome")?,
                actor: row.try_get("actor")?,
                actor_account_id: row.try_get("actor_account_id")?,
                actor_account_username: row.try_get("actor_account_username")?,
                account_id: row.try_get("account_id")?,
                account_username: row.try_get("account_username")?,
                pc_id: row.try_get("pc_id")?,
                pc_name: row.try_get("pc_name")?,
                incident_kind: row.try_get("incident_kind")?,
                amount_cents: row.try_get("amount_cents")?,
                seconds_delta: row.try_get("seconds_delta")?,
                points_delta_centi: row.try_get("points_delta_centi")?,
                summary: row.try_get("summary")?,
                created_at: row.try_get("created_at")?,
                acknowledged_at: row.try_get("acknowledged_at")?,
                last_seen_at: row.try_get("last_seen_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(Into::into)
}

/// Restricts a read to the severity-scoped event types inside the rolling
/// window. The Rust classifier stays the authority; this only narrows the
/// indexed scan on `idx_audit_log_event_type_created`.
fn push_window_predicate(
    query: &mut QueryBuilder<'_, Sqlite>,
    now: &str,
    severity: Option<NotificationSeverityFilter>,
) {
    query.push("a.event_type IN (");
    match severity {
        Some(NotificationSeverityFilter::Alert) => {
            push_event_types(query, alert_event_types().iter().copied());
        }
        Some(NotificationSeverityFilter::Notice) => {
            push_event_types(query, notice_event_types().iter().copied());
        }
        None => push_event_types(query, notification_event_types()),
    }
    query
        .push(") AND a.created_at >= datetime(")
        .push_bind(now.to_owned())
        .push(", '-' || ")
        .push_bind(ROLLING_WINDOW_DAYS)
        .push(" || ' days') AND a.created_at <= ")
        .push_bind(now.to_owned());
}

/// Read state in SQL: an Alert is read once acknowledged, a Notice once the
/// seen watermark has passed it.
fn push_read_predicate(query: &mut QueryBuilder<'_, Sqlite>) {
    query.push("(CASE WHEN a.event_type IN (");
    push_event_types(query, alert_event_types().iter().copied());
    query.push(
        ") THEN ack.acknowledged_at IS NOT NULL
              ELSE a.created_at <= state.last_seen_at END)",
    );
}

fn push_event_types<'a>(
    query: &mut QueryBuilder<'_, Sqlite>,
    event_types: impl Iterator<Item = &'a str>,
) {
    let mut separated = query.separated(", ");
    for event_type in event_types {
        separated.push_bind(event_type.to_owned());
    }
}

impl NotificationAuditRow {
    fn into_item(self) -> Option<NotificationItem> {
        let severity = classify_audit_event(&self.event_type);
        if severity == NotificationSeverity::Silent {
            return None;
        }
        let read = match severity {
            NotificationSeverity::Alert => self.acknowledged_at.is_some(),
            NotificationSeverity::Notice => self.created_at <= self.last_seen_at,
            NotificationSeverity::Silent => false,
        };
        let acknowledged_at = match severity {
            NotificationSeverity::Alert => self.acknowledged_at,
            NotificationSeverity::Notice | NotificationSeverity::Silent => None,
        };
        Some(NotificationItem {
            id: self.id,
            severity,
            event_type: self.event_type,
            outcome: self.outcome,
            actor: self.actor,
            actor_account_id: self.actor_account_id,
            actor_account_username: self.actor_account_username,
            account_id: self.account_id,
            account_username: self.account_username,
            pc_id: self.pc_id,
            pc_name: self.pc_name,
            incident_kind: self.incident_kind,
            amount_cents: self.amount_cents,
            seconds_delta: self.seconds_delta,
            points_delta_centi: self.points_delta_centi,
            summary: self.summary,
            created_at: self.created_at,
            read,
            acknowledged_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, errors::AppResult, test_utils};
    use chrono::{Duration, Utc};
    use sqlx::sqlite::SqlitePoolOptions;
    use uuid::Uuid;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        db::run_migrations(&pool, test_utils::migration_backup_dir().path()).await?;
        db::seed(&pool).await?;
        Ok(pool)
    }

    async fn insert_event(
        pool: &SqlitePool,
        id: &str,
        event_type: &str,
        created_at: &str,
        account_id: Option<&str>,
        pc_id: Option<&str>,
    ) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, account_id, pc_id, amount_cents,
              seconds_delta, points_delta_centi, summary, created_at)
             VALUES (?, ?, 'success', 'admin', ?, ?, 100, 60, 25, ?, ?)",
        )
        .bind(id)
        .bind(event_type)
        .bind(account_id)
        .bind(pc_id)
        .bind(format!("summary-{id}"))
        .bind(created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn insert_account(pool: &SqlitePool, id: &str, username: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO accounts
             (id, username, username_normalized, password_hash)
             VALUES (?, ?, ?, 'test-hash')",
        )
        .bind(id)
        .bind(username)
        .bind(username.to_ascii_lowercase())
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn insert_attributed_event(
        pool: &SqlitePool,
        id: &str,
        event_type: &str,
        actor: &str,
        actor_account_id: Option<&str>,
        account_id: Option<&str>,
        created_at: &str,
    ) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, actor_account_id, account_id,
              summary, created_at)
             VALUES (?, ?, 'success', ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(event_type)
        .bind(actor)
        .bind(actor_account_id)
        .bind(account_id)
        .bind(format!("summary-{id}"))
        .bind(created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn set_watermark(pool: &SqlitePool, value: &str) -> AppResult<()> {
        sqlx::query("UPDATE notification_read_state SET last_seen_at = ? WHERE id = 1")
            .bind(value)
            .execute(pool)
            .await?;
        Ok(())
    }

    fn recent(days: i64) -> String {
        (Utc::now() - Duration::days(days))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    #[tokio::test]
    async fn reachability_events_are_excluded_from_notifications_and_unread_count() -> AppResult<()>
    {
        let pool = test_pool().await?;
        set_watermark(&pool, "2000-01-01 00:00:00").await?;
        insert_event(
            &pool,
            "offline-recent",
            "pc_went_offline",
            &recent(1),
            None,
            Some("pc-01"),
        )
        .await?;
        insert_event(
            &pool,
            "online-recent",
            "pc_came_online",
            &recent(29),
            None,
            Some("pc-01"),
        )
        .await?;

        for severity in [
            None,
            Some(NotificationSeverityFilter::Alert),
            Some(NotificationSeverityFilter::Notice),
        ] {
            let result = list(
                &pool,
                NotificationListFilter {
                    limit: 50,
                    offset: 0,
                    severity,
                    unread: None,
                },
            )
            .await?;
            assert!(result.items.is_empty());
        }
        assert_eq!(unread_count(&pool).await?.unread_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn list_classifies_filters_orders_ties_and_paginates_after_filtering() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        set_watermark(&pool, "2000-01-01 00:00:00").await?;
        insert_event(&pool, "alert-a", "tamper_kill", &recent, None, None).await?;
        insert_event(&pool, "notice-b", "shop_opened", &recent, None, None).await?;
        insert_event(&pool, "silent", "login_failed", &recent, None, None).await?;
        insert_event(
            &pool,
            "notice-old",
            "shop_opened",
            "2000-01-01 00:00:00",
            None,
            None,
        )
        .await?;
        let database_now = sqlx::query_scalar::<_, String>("SELECT CURRENT_TIMESTAMP")
            .fetch_one(&pool)
            .await?;
        let boundary = sqlx::query_scalar::<_, String>("SELECT datetime(?, '-30 days')")
            .bind(&database_now)
            .fetch_one(&pool)
            .await?;
        insert_event(&pool, "boundary", "tamper_kill", &boundary, None, None).await?;

        let result = list(
            &pool,
            NotificationListFilter {
                limit: 1,
                offset: 0,
                severity: None,
                unread: Some(NotificationUnreadFilter::Unread),
            },
        )
        .await?;
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "notice-b");
        assert!(result.has_more);
        let all = list(
            &pool,
            NotificationListFilter {
                limit: 200,
                offset: 0,
                severity: None,
                unread: None,
            },
        )
        .await?;
        assert!(all.items.iter().any(|item| item.id == "boundary"));

        let alerts = list(
            &pool,
            NotificationListFilter {
                limit: 50,
                offset: 0,
                severity: Some(NotificationSeverityFilter::Alert),
                unread: None,
            },
        )
        .await?;
        assert_eq!(alerts.items[0].id, "alert-a");
        Ok(())
    }

    #[tokio::test]
    async fn list_resolves_actor_username_without_falling_back_to_target() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_account(&pool, "actor-account", "juan").await?;
        insert_account(&pool, "target-account", "maria").await?;

        insert_attributed_event(
            &pool,
            "account-event",
            "account_time_adjusted",
            "account",
            Some("actor-account"),
            Some("target-account"),
            &recent,
        )
        .await?;
        insert_attributed_event(
            &pool,
            "admin-event",
            "account_points_adjusted",
            "admin",
            None,
            Some("target-account"),
            &recent,
        )
        .await?;

        let result = list(
            &pool,
            NotificationListFilter {
                limit: 50,
                offset: 0,
                severity: None,
                unread: None,
            },
        )
        .await?;
        let account_event = result
            .items
            .iter()
            .find(|item| item.id == "account-event")
            .expect("account notification should be listed");
        assert_eq!(
            account_event.actor_account_id.as_deref(),
            Some("actor-account")
        );
        assert_eq!(
            account_event.actor_account_username.as_deref(),
            Some("juan")
        );
        assert_eq!(account_event.account_id.as_deref(), Some("target-account"));
        assert_eq!(account_event.account_username.as_deref(), Some("maria"));

        let admin_event = result
            .items
            .iter()
            .find(|item| item.id == "admin-event")
            .expect("admin notification should be listed");
        assert_eq!(admin_event.actor_account_id, None);
        assert_eq!(admin_event.actor_account_username, None);
        assert_eq!(admin_event.account_username.as_deref(), Some("maria"));
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_target_account_username_survives_soft_deletion() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_account(&pool, "deleted-account", "maria").await?;
        insert_attributed_event(
            &pool,
            "tamper-kill-deleted-account",
            "tamper_kill",
            "system",
            None,
            Some("deleted-account"),
            &recent,
        )
        .await?;
        sqlx::query(
            "UPDATE accounts
             SET status = 'deleted', deleted_at = CURRENT_TIMESTAMP
             WHERE id = 'deleted-account'",
        )
        .execute(&pool)
        .await?;

        let result = list(
            &pool,
            NotificationListFilter {
                limit: 50,
                offset: 0,
                severity: Some(NotificationSeverityFilter::Alert),
                unread: None,
            },
        )
        .await?;
        let notification = result
            .items
            .iter()
            .find(|item| item.id == "tamper-kill-deleted-account")
            .expect("tamper kill should be listed");
        assert_eq!(notification.account_id.as_deref(), Some("deleted-account"));
        assert_eq!(notification.account_username.as_deref(), Some("maria"));
        Ok(())
    }

    #[tokio::test]
    async fn account_login_throttle_notice_names_the_account() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_account(&pool, "account-id", "maria").await?;
        insert_attributed_event(
            &pool,
            "account-login-throttled",
            "account_login_throttled",
            "account",
            None,
            Some("account-id"),
            &recent,
        )
        .await?;

        let result = list(
            &pool,
            NotificationListFilter {
                limit: 50,
                offset: 0,
                severity: Some(NotificationSeverityFilter::Notice),
                unread: None,
            },
        )
        .await?;
        let notification = result
            .items
            .iter()
            .find(|item| item.event_type == "account_login_throttled")
            .expect("Account login throttle should be listed");
        assert_eq!(notification.severity, NotificationSeverity::Notice);
        assert_eq!(notification.account_id.as_deref(), Some("account-id"));
        assert_eq!(notification.account_username.as_deref(), Some("maria"));
        Ok(())
    }

    #[tokio::test]
    async fn seen_notices_stay_listed_and_offsets_page_in_sql() -> AppResult<()> {
        let pool = test_pool().await?;
        set_watermark(&pool, "2000-01-01 00:00:00").await?;
        insert_event(&pool, "notice-1", "shop_opened", &recent(1), None, None).await?;
        insert_event(&pool, "notice-2", "shop_closed", &recent(2), None, None).await?;
        insert_event(&pool, "notice-3", "shop_opened", &recent(3), None, None).await?;

        let filter = |offset: i64| NotificationListFilter {
            limit: 2,
            offset,
            severity: Some(NotificationSeverityFilter::Notice),
            unread: None,
        };
        let first = list(&pool, filter(0)).await?;
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["notice-1", "notice-2"]
        );
        assert!(first.has_more);
        let second = list(&pool, filter(2)).await?;
        assert_eq!(second.items[0].id, "notice-3");
        assert!(!second.has_more);

        // After the watermark advances the Notices stay listed, only read.
        mark_seen(&pool).await?;
        assert_eq!(unread_count(&pool).await?.unread_count, 0);
        let after_seen = list(&pool, filter(0)).await?;
        assert_eq!(after_seen.items.len(), 2);
        assert!(after_seen.items.iter().all(|item| item.read));
        Ok(())
    }

    #[tokio::test]
    async fn notice_watermark_and_alert_acknowledgement_define_read_state() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        set_watermark(&pool, "2000-01-01 00:00:00").await?;
        insert_event(&pool, "alert", "tamper_kill", &recent, None, None).await?;
        insert_event(&pool, "notice", "shop_opened", &recent, None, None).await?;

        let before = unread_count(&pool).await?;
        assert_eq!(before.unread_count, 2);
        mark_seen(&pool).await?;
        let after_seen = unread_count(&pool).await?;
        assert_eq!(after_seen.unread_count, 1);
        acknowledge(&pool, "alert").await?;
        assert_eq!(unread_count(&pool).await?.unread_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn acknowledgement_is_idempotent_and_rejects_silent_or_out_of_window() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_event(&pool, "alert", "tamper_kill", &recent, None, None).await?;
        insert_event(&pool, "notice", "shop_opened", &recent, None, None).await?;
        insert_event(
            &pool,
            "old",
            "tamper_kill",
            "2000-01-01 00:00:00",
            None,
            None,
        )
        .await?;

        let first = acknowledge(&pool, "alert").await?;
        let second = acknowledge(&pool, "alert").await?;
        assert_eq!(first.acknowledged_at, second.acknowledged_at);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'notification_acknowledged'",
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        assert!(matches!(
            acknowledge(&pool, "notice").await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            acknowledge(&pool, "old").await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            acknowledge(&pool, &Uuid::new_v4().to_string()).await,
            Err(AppError::NotFound(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn bulk_acknowledgement_skips_ineligible_ids_and_audits_each_new_alert() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_event(&pool, "alert-a", "tamper_kill", &recent, None, None).await?;
        insert_event(&pool, "alert-b", "manual_adjustment", &recent, None, None).await?;
        insert_event(&pool, "alert-acked", "pc_removed", &recent, None, None).await?;
        insert_event(&pool, "notice", "shop_opened", &recent, None, None).await?;
        insert_event(&pool, "silent", "login_failed", &recent, None, None).await?;
        insert_event(
            &pool,
            "old",
            "tamper_kill",
            "2000-01-01 00:00:00",
            None,
            None,
        )
        .await?;
        acknowledge(&pool, "alert-acked").await?;

        let result = acknowledge_many(
            &pool,
            &[
                "alert-a".to_owned(),
                " alert-b ".to_owned(),
                "alert-a".to_owned(),
                "alert-acked".to_owned(),
                "notice".to_owned(),
                "silent".to_owned(),
                "old".to_owned(),
                Uuid::new_v4().to_string(),
                "  ".to_owned(),
            ],
        )
        .await?;

        assert_eq!(result.acknowledged_count, 2);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_alert_ack")
                .fetch_one(&pool)
                .await?,
            3
        );
        // One audit row per newly acknowledged Alert, none for the repeat.
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'notification_acknowledged'",
            )
            .fetch_one(&pool)
            .await?,
            3
        );
        for id in ["notice", "silent", "old"] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM notification_alert_ack WHERE audit_log_id = ?",
                )
                .bind(id)
                .fetch_one(&pool)
                .await?,
                0
            );
        }

        // Repeating the same batch acknowledges nothing further.
        let repeat = acknowledge_many(&pool, &["alert-a".to_owned(), "alert-b".to_owned()]).await?;
        assert_eq!(repeat.acknowledged_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_acknowledgement_rejects_empty_and_oversized_batches() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            acknowledge_many(&pool, &[]).await,
            Err(AppError::InvalidInput(_))
        ));
        let oversized = vec!["alert".to_owned(); MAX_BULK_ACKNOWLEDGE_IDS + 1];
        assert!(matches!(
            acknowledge_many(&pool, &oversized).await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn bulk_acknowledgement_rolls_back_every_alert_together() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_event(&pool, "bulk-rollback-a", "tamper_kill", &recent, None, None).await?;
        insert_event(&pool, "bulk-rollback-b", "pc_removed", &recent, None, None).await?;
        sqlx::query(
            "CREATE TRIGGER notification_bulk_audit_failure
             BEFORE INSERT ON audit_log
             WHEN NEW.event_type = 'notification_acknowledged'
             BEGIN SELECT RAISE(ABORT, 'forced acknowledgement audit failure'); END",
        )
        .execute(&pool)
        .await?;

        assert!(acknowledge_many(
            &pool,
            &["bulk-rollback-a".to_owned(), "bulk-rollback-b".to_owned()],
        )
        .await
        .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_alert_ack")
                .fetch_one(&pool)
                .await?,
            0
        );
        sqlx::query("DROP TRIGGER notification_bulk_audit_failure")
            .execute(&pool)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn acknowledgement_rolls_back_read_state_and_audit_append_together() -> AppResult<()> {
        let pool = test_pool().await?;
        let recent = recent(1);
        insert_event(&pool, "rollback-alert", "tamper_kill", &recent, None, None).await?;
        sqlx::query(
            "CREATE TRIGGER notification_audit_failure
             BEFORE INSERT ON audit_log
             WHEN NEW.event_type = 'notification_acknowledged'
             BEGIN SELECT RAISE(ABORT, 'forced acknowledgement audit failure'); END",
        )
        .execute(&pool)
        .await?;

        assert!(acknowledge(&pool, "rollback-alert").await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM notification_alert_ack WHERE audit_log_id = 'rollback-alert'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'notification_acknowledged'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        sqlx::query("DROP TRIGGER notification_audit_failure")
            .execute(&pool)
            .await?;
        Ok(())
    }
}

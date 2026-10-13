//! Durable Telegram integration state (ADR-0076 / GitHub #345): the
//! single-row `telegram_config` table holding the connected bot identity, the
//! one private owner chat destination, and the DPAPI-protected bot token.
//!
//! Every row change commits in the same SQLite transaction as its generic
//! Audit event — connect appends `telegram_connected`, disconnect
//! `telegram_disconnected`. Audit metadata names identities only; the token is
//! never stored in or near the Audit Log.
//!
//! The whole integration is excluded from Backups (ADR-0076): both this table
//! and the `telegram_outbox` delivery queue are in
//! [`crate::backup::BACKUP_EXCLUDED_TABLES`], so a restored Admin starts
//! disconnected until the owner completes setup again.
//!
//! The outbox half (#346) lives here too: redacted delivery items are enqueued
//! inside the same transaction as their incident row change and Audit event,
//! never sent from it. The delivery worker ([`crate::telegram_delivery`])
//! reads committed items only, batches same-window lines into one message,
//! retries with bounded exponential backoff, and records delivery health that
//! Settings shows inline.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::{
    audit::{self, NewAuditEvent},
    errors::AppResult,
};

/// Delivery items whose creation instants share this fixed window batch into
/// one Telegram message (ADR-0076); local incidents stay separate.
pub(crate) const BATCH_WINDOW_SECS: i64 = 30;

/// An enqueued item retries with bounded exponential backoff starting here.
pub(crate) const BACKOFF_BASE_SECS: i64 = 30;

/// Backoff ceiling: once an item waits this long between attempts, failures
/// no longer grow the wait.
pub(crate) const BACKOFF_MAX_SECS: i64 = 3_600;

/// Total retry window per item (ADR-0076): after this long since creation an
/// item stops retrying and is marked abandoned, never sent.
pub(crate) const RETRY_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Sent and abandoned rows older than this are pruned by the delivery
/// worker, mirroring the Notifications retention window.
pub(crate) const PRUNE_FINISHED_AFTER_DAYS: i64 = 30;

/// One stored integration configuration. Never serializes the token: the
/// protected bytes are deliberately absent from this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramConfigRow {
    pub bot_username: String,
    pub chat_id: String,
    pub chat_title: String,
    /// Hex-encoded Windows DPAPI ciphertext under the Admin identity.
    pub token_protected: String,
    pub connected_at: String,
    /// When this configuration last passed a real test send.
    pub verified_at: String,
}

/// `(bot_username, chat_id, chat_title, token_protected, connected_at,
/// verified_at)`
type ConfigRow = (String, String, String, String, String, String);

fn map_config(row: ConfigRow) -> TelegramConfigRow {
    TelegramConfigRow {
        bot_username: row.0,
        chat_id: row.1,
        chat_title: row.2,
        token_protected: row.3,
        connected_at: row.4,
        verified_at: row.5,
    }
}

/// Read the stored configuration, or `None` while disconnected.
pub async fn config(pool: &SqlitePool) -> AppResult<Option<TelegramConfigRow>> {
    let row = sqlx::query_as::<_, ConfigRow>(
        "SELECT bot_username, chat_id, chat_title, token_protected, connected_at, verified_at
         FROM telegram_config WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(map_config))
}

/// Atomically replace any existing configuration with the newly tested one and
/// append the `telegram_connected` Alert in the same transaction. Called only
/// after a successful test send, so a failed replacement can never disturb the
/// previous working configuration.
pub async fn connect_with_audit(pool: &SqlitePool, config: &TelegramConfigRow) -> AppResult<()> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let mut event = NewAuditEvent::simple(
        "telegram_connected",
        openpiso_core::audit::AuditOutcome::Success,
        openpiso_core::audit::AuditActor::Admin,
        format!(
            "Telegram alerts connected to private chat of @{}",
            config.bot_username
        ),
    );
    // Identity metadata only — never the token.
    event.metadata = serde_json::json!({
        "bot_username": config.bot_username,
        "chat_id": config.chat_id,
        "chat_title": config.chat_title,
    });
    let audit_id = audit::append(&mut transaction, event).await?;
    // Delivery rows belong to the destination, which is about to be replaced.
    // Drop them atomically so old messages cannot reach a new owner chat and
    // Settings does not report the old destination's delivery history.
    sqlx::query("DELETE FROM telegram_outbox")
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO telegram_config (
             id, bot_username, chat_id, chat_title, token_protected,
             connected_at, verified_at, open_audit_id
         ) VALUES (1, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             bot_username = excluded.bot_username,
             chat_id = excluded.chat_id,
             chat_title = excluded.chat_title,
             token_protected = excluded.token_protected,
             connected_at = excluded.connected_at,
             verified_at = excluded.verified_at,
             open_audit_id = excluded.open_audit_id",
    )
    .bind(&config.bot_username)
    .bind(&config.chat_id)
    .bind(&config.chat_title)
    .bind(&config.token_protected)
    .bind(&config.connected_at)
    .bind(&config.verified_at)
    .bind(audit_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Delete the stored configuration and append the `telegram_disconnected`
/// Notice in the same transaction. Delivery rows are deleted with the
/// destination they were queued for, so nothing can ever send to a removed
/// chat; local PC Monitoring is untouched: incidents, settings, and history
/// belong to other tables entirely. Returns whether anything was deleted, so
/// a stale double click stays a success.
pub async fn disconnect_with_audit(pool: &SqlitePool) -> AppResult<bool> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let deleted = sqlx::query("DELETE FROM telegram_config WHERE id = 1")
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        > 0;
    if !deleted {
        transaction.commit().await?;
        return Ok(false);
    }
    sqlx::query("DELETE FROM telegram_outbox")
        .execute(&mut *transaction)
        .await?;
    let event = NewAuditEvent::simple(
        "telegram_disconnected",
        openpiso_core::audit::AuditOutcome::Success,
        openpiso_core::audit::AuditActor::Admin,
        "Telegram alerts disconnected; protected token and destination deleted",
    );
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Delivery outbox (#346)
// ---------------------------------------------------------------------------

/// One redacted delivery item to enqueue. `message_text` is final: composed
/// at enqueue time from display name, incident time, duration when known, and
/// closure reason, so the worker never touches incident data again.
pub(crate) struct NewOutboxItem {
    /// "opening" or "closure"; same-class items in one batch window share a
    /// message.
    pub item_class: &'static str,
    /// Incident kind is part of the batch key. A fleet mismatch must not be
    /// mixed with a Guard or Host Probe Coverage message.
    pub incident_kind: Option<&'static str>,
    pub message_text: String,
    /// The incident this delivers, if it is one. State messages carry none.
    pub incident_id: Option<String>,
}

/// Enqueue one delivery item inside the caller's transaction — always the
/// same transaction as its incident row change and Audit event. Nothing here
/// performs I/O beyond SQLite, and nothing is sent from this path.
pub(crate) async fn enqueue_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    item: &NewOutboxItem,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let id = uuid::Uuid::new_v4().to_string();
    // A later item joins the oldest still-pending same-kind cohort created in
    // the preceding 30 seconds. All members therefore share that cohort's
    // deadline instead of each waiting 30 seconds independently.
    let recent_cutoff = format_rfc3339(now - chrono::Duration::seconds(BATCH_WINDOW_SECS));
    let cohort_deadline: Option<String> = sqlx::query_scalar(
        "SELECT MIN(next_attempt_at)
         FROM telegram_outbox
         WHERE status = 'pending'
           AND item_class = ?
           AND incident_kind IS ?
           AND created_at >= ? AND created_at <= ?",
    )
    .bind(item.item_class)
    .bind(item.incident_kind)
    .bind(recent_cutoff)
    .bind(format_rfc3339(now))
    .fetch_one(&mut **transaction)
    .await?;
    let first_attempt_at = cohort_deadline
        .unwrap_or_else(|| format_rfc3339(now + chrono::Duration::seconds(BATCH_WINDOW_SECS)));
    sqlx::query(
        "INSERT INTO telegram_outbox (
             id, item_class, incident_kind, message_text, incident_id,
             created_at, status, attempts, next_attempt_at, batch_bucket
         ) VALUES (?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)",
    )
    .bind(&id)
    .bind(item.item_class)
    .bind(item.incident_kind)
    .bind(&item.message_text)
    .bind(&item.incident_id)
    .bind(format_rfc3339(now))
    // Hold the cohort until its shared window closes.
    .bind(first_attempt_at)
    .bind(now.timestamp().div_euclid(BATCH_WINDOW_SECS))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// One committed outbox item as the delivery worker sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboxItem {
    pub id: String,
    pub item_class: String,
    pub incident_kind: Option<String>,
    pub message_text: String,
    pub created_at: DateTime<Utc>,
    pub attempts: i64,
    /// Persisted fixed-window bucket retained for migrations and inspection;
    /// delivery applies the precise created-time window to each group.
    pub batch_bucket: i64,
}

type OutboxRow = (String, String, Option<String>, String, String, i64, i64);

fn map_outbox_row(row: OutboxRow) -> Option<OutboxItem> {
    Some(OutboxItem {
        id: row.0,
        item_class: row.1,
        incident_kind: row.2,
        message_text: row.3,
        created_at: parse_db_timestamp(&row.4)?,
        attempts: row.5,
        batch_bucket: row.6,
    })
}

/// Every pending item whose next attempt is due at `now`, oldest first.
pub(crate) async fn due_items(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<Vec<OutboxItem>> {
    let rows = sqlx::query_as::<_, OutboxRow>(
        "SELECT id, item_class, incident_kind, message_text, created_at, attempts, batch_bucket
         FROM telegram_outbox
         WHERE status = 'pending' AND next_attempt_at <= ?
         ORDER BY created_at, id",
    )
    .bind(format_rfc3339(now))
    .fetch_all(pool)
    .await?;
    // Rows are written by this module with parseable timestamps, so a
    // mapping failure would be a bug; surface it rather than silently
    // stranding an item in `pending` forever.
    let mut items = Vec::new();
    for row in rows {
        match map_outbox_row(row) {
            Some(item) => items.push(item),
            None => {
                tracing::warn!(
                    target: "openpiso_admin_lib::db::telegram",
                    "telegram_outbox row with unparseable timestamp skipped"
                );
            }
        }
    }
    Ok(items)
}

/// Mark items delivered. A crash between the network success and this write
/// re-sends them later — an accepted duplicate (ADR-0076); incidents, Audit
/// events, money, time, Points, and sessions are never duplicated by it.
pub(crate) async fn mark_sent(
    pool: &SqlitePool,
    ids: &[String],
    now: DateTime<Utc>,
) -> AppResult<()> {
    for id in ids {
        sqlx::query(
            "UPDATE telegram_outbox
             SET status = 'sent', sent_at = ?, attempts = attempts + 1
             WHERE id = ? AND status = 'pending'",
        )
        .bind(format_rfc3339(now))
        .bind(id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Mark pending rows that have reached the 24-hour retry window as abandoned.
/// This runs before the due-item query, so a worker that was stopped for a
/// day cannot send stale alerts on its next start.
pub(crate) async fn abandon_expired(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<u64> {
    let cutoff = format_rfc3339(now - chrono::Duration::seconds(RETRY_WINDOW_SECS));
    let result = sqlx::query(
        "UPDATE telegram_outbox
         SET status = 'abandoned',
             last_error = COALESCE(last_error, 'delivery expired after 24 hours'),
             last_error_at = COALESCE(last_error_at, ?)
         WHERE status = 'pending' AND created_at <= ?",
    )
    .bind(format_rfc3339(now))
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Record one failed attempt: exponential backoff capped at one hour, until
/// the item's 24-hour retry window closes and it is abandoned unsent.
pub(crate) async fn schedule_retry_or_abandon(
    pool: &SqlitePool,
    item: &OutboxItem,
    error: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let age = now.signed_duration_since(item.created_at).num_seconds();
    let (status, next_attempt_at) = if age >= RETRY_WINDOW_SECS {
        ("abandoned", None)
    } else {
        let attempts = item.attempts + 1;
        let delay_secs = BACKOFF_BASE_SECS
            .saturating_mul(1i64 << (attempts - 1).min(16))
            .min(BACKOFF_MAX_SECS);
        (
            "pending",
            Some(format_rfc3339(now + chrono::Duration::seconds(delay_secs))),
        )
    };
    sqlx::query(
        "UPDATE telegram_outbox
         SET status = ?, attempts = attempts + 1, next_attempt_at = COALESCE(?, next_attempt_at),
             last_error = ?, last_error_at = ?
         WHERE id = ?",
    )
    .bind(status)
    .bind(next_attempt_at)
    // Sanitized transport errors only; the token can never appear.
    .bind(error)
    .bind(format_rfc3339(now))
    .bind(&item.id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete finished (sent or abandoned) rows past the retention window.
pub(crate) async fn prune_finished(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<u64> {
    let cutoff = format_rfc3339(now - chrono::Duration::days(PRUNE_FINISHED_AFTER_DAYS));
    let result =
        sqlx::query("DELETE FROM telegram_outbox WHERE status != 'pending' AND created_at < ?")
            .bind(cutoff)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// Delivery health for Settings: last success, last error, pending state.
/// Never contains the token or any message content beyond sanitized errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeliveryHealth {
    /// When a message was last confirmed sent, if ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sent_at: Option<String>,
    /// The most recent sanitized delivery error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<String>,
    /// Items still queued for delivery (queued plus retrying).
    pub pending_count: i64,
    /// Pending items never yet attempted by the worker.
    pub queued_count: i64,
    /// Pending items rewaiting after at least one failed attempt.
    pub retrying_count: i64,
    /// Items abandoned unsent after their 24-hour retry window, still within
    /// the retention window. Settings explains the expiry from this count;
    /// nothing ever re-sends them.
    pub expired_count: i64,
}

/// Read delivery health. Never touches the token; error text comes only from
/// sanitized transport failures recorded by [`schedule_retry_or_abandon`].
pub async fn delivery_health(pool: &SqlitePool) -> AppResult<DeliveryHealth> {
    let (
        last_sent_at,
        last_error,
        last_error_at,
        pending_count,
        queued_count,
        retrying_count,
        expired_count,
    ): (
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT
           (SELECT MAX(sent_at) FROM telegram_outbox WHERE status = 'sent'),
           (SELECT last_error FROM telegram_outbox
             WHERE last_error IS NOT NULL ORDER BY last_error_at DESC LIMIT 1),
           (SELECT last_error_at FROM telegram_outbox
             WHERE last_error IS NOT NULL ORDER BY last_error_at DESC LIMIT 1),
           (SELECT COUNT(*) FROM telegram_outbox WHERE status = 'pending'),
           (SELECT COUNT(*) FROM telegram_outbox
             WHERE status = 'pending' AND attempts = 0),
           (SELECT COUNT(*) FROM telegram_outbox
             WHERE status = 'pending' AND attempts > 0),
           (SELECT COUNT(*) FROM telegram_outbox WHERE status = 'abandoned')",
    )
    .fetch_one(pool)
    .await?;
    Ok(DeliveryHealth {
        last_sent_at,
        last_error,
        last_error_at,
        pending_count,
        queued_count,
        retrying_count,
        expired_count,
    })
}

// ---------------------------------------------------------------------------
// Redacted message composition
// ---------------------------------------------------------------------------

/// Parse the two timestamp shapes OpenPiso stores: RFC 3339 (Rust-bound
/// columns like this outbox) and SQLite `CURRENT_TIMESTAMP`
/// (`YYYY-MM-DD HH:MM:SS`, UTC by definition).
pub(crate) fn parse_db_timestamp(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text.trim()) {
        return Some(parsed.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| Utc.from_utc_datetime(&naive))
}

pub(crate) fn format_rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn format_message_time(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Truthful observed span for a message. An incident that spans an Admin
/// restart was not continuously observed, so no duration is claimed.
fn observed_duration(
    opened_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
    interrupted: bool,
) -> String {
    if interrupted {
        return "observation interrupted by an Admin restart".to_owned();
    }
    let seconds = closed_at
        .signed_duration_since(opened_at)
        .num_seconds()
        .max(0);
    let minutes = seconds / 60;
    if minutes < 1 {
        "under a minute".to_owned()
    } else if minutes < 60 {
        format!(
            "about {minutes} minute{}",
            if minutes == 1 { "" } else { "s" }
        )
    } else {
        let hours = minutes / 60;
        let rest = minutes % 60;
        if rest == 0 {
            format!("about {hours} hour{}", if hours == 1 { "" } else { "s" })
        } else {
            format!("about {hours} h {rest} min")
        }
    }
}

/// Compose an opening line: kind label, PC display name, incident time.
pub(crate) fn compose_opening(pc_name: &str, kind_label: &str, opened_at: DateTime<Utc>) -> String {
    format!(
        "{kind_label} detected on {pc_name} at {}.",
        format_message_time(opened_at)
    )
}

/// Compose a closure line: label, name, closure time, observed duration when
/// known, and the closure reason. No other content may appear here.
pub(crate) fn compose_closure(
    pc_name: &str,
    kind_label: &str,
    opened_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
    interrupted: bool,
    reason_phrase: &str,
) -> String {
    let duration = observed_duration(opened_at, closed_at, interrupted);
    format!(
        "{kind_label} on {pc_name} closed at {} ({duration}): {reason_phrase}.",
        format_message_time(closed_at)
    )
}

/// Compose the resolved summary that replaces an undelivered opening and its
/// closure when the episode ends before the opening message is sent.
pub(crate) fn compose_resolved(
    pc_name: &str,
    kind_label: &str,
    opened_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
    interrupted: bool,
    reason_phrase: &str,
) -> String {
    let duration = observed_duration(opened_at, closed_at, interrupted);
    format!(
        "{kind_label} on {pc_name}: opened at {}, resolved at {} ({duration}); {reason_phrase}.",
        format_message_time(opened_at),
        format_message_time(closed_at)
    )
}

/// Compose the monitoring-disabled state message.
pub(crate) fn compose_monitoring_disabled(closed_incidents: usize, now: DateTime<Utc>) -> String {
    if closed_incidents == 0 {
        return format!(
            "PC Monitoring was disabled at {}.",
            format_message_time(now)
        );
    }
    format!(
        "PC Monitoring was disabled at {}; {} open incident{} closed as monitoring disabled.",
        format_message_time(now),
        closed_incidents,
        if closed_incidents == 1 {
            " was"
        } else {
            "s were"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    fn sample(protected_token: &str) -> TelegramConfigRow {
        TelegramConfigRow {
            bot_username: "shop_alerts_bot".to_owned(),
            chat_id: "123456789".to_owned(),
            chat_title: "Owner".to_owned(),
            token_protected: protected_token.to_owned(),
            connected_at: "2026-08-26T00:00:00+00:00".to_owned(),
            verified_at: "2026-08-26T00:00:01+00:00".to_owned(),
        }
    }

    #[tokio::test]
    async fn fresh_database_starts_disconnected() -> AppResult<()> {
        let pool = test_pool().await?;
        assert_eq!(config(&pool).await?, None);
        // Disconnecting nothing stays a successful no-op.
        assert!(!disconnect_with_audit(&pool).await?);
        Ok(())
    }

    #[tokio::test]
    async fn connect_commits_row_and_audit_together() -> AppResult<()> {
        let pool = test_pool().await?;
        connect_with_audit(&pool, &sample("aa")).await?;

        let stored = config(&pool).await?.expect("stored config");
        assert_eq!(stored.token_protected, "aa");
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'telegram_connected'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(count, 1);
        // Metadata carries identity only.
        let metadata: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'telegram_connected'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(metadata.contains("shop_alerts_bot"));
        assert!(!metadata.contains("secret"));
        Ok(())
    }

    #[tokio::test]
    async fn replacement_overwrites_the_single_row_and_audits_again() -> AppResult<()> {
        let pool = test_pool().await?;
        connect_with_audit(&pool, &sample("old-protected")).await?;
        connect_with_audit(&pool, &sample("new-protected")).await?;

        let stored = config(&pool).await?.expect("stored config");
        assert_eq!(stored.token_protected, "new-protected");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_config")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'telegram_connected'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 2);
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_deletes_row_and_appends_notice_once() -> AppResult<()> {
        let pool = test_pool().await?;
        connect_with_audit(&pool, &sample("protected")).await?;

        assert!(disconnect_with_audit(&pool).await?);
        assert_eq!(config(&pool).await?, None);
        assert!(!disconnect_with_audit(&pool).await?);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'telegram_disconnected'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(count, 1);
        Ok(())
    }
}

#[cfg(test)]
mod outbox_tests {
    use chrono::{TimeZone, Utc};
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    fn sample(protected_token: &str) -> TelegramConfigRow {
        TelegramConfigRow {
            bot_username: "shop_alerts_bot".to_owned(),
            chat_id: "123456789".to_owned(),
            chat_title: "Owner".to_owned(),
            token_protected: protected_token.to_owned(),
            connected_at: "2026-08-26T00:00:00+00:00".to_owned(),
            verified_at: "2026-08-26T00:00:01+00:00".to_owned(),
        }
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).single().expect("epoch")
    }

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    async fn enqueue_sample(
        pool: &SqlitePool,
        text: &str,
        now: DateTime<Utc>,
    ) -> AppResult<String> {
        let mut transaction = crate::db::begin_immediate(pool).await?;
        let item = NewOutboxItem {
            item_class: "closure",
            incident_kind: None,
            message_text: text.to_owned(),
            incident_id: None,
        };
        enqueue_in_transaction(&mut transaction, &item, now).await?;
        transaction.commit().await?;
        let id: String =
            sqlx::query_scalar("SELECT id FROM telegram_outbox ORDER BY created_at DESC LIMIT 1")
                .fetch_one(pool)
                .await?;
        Ok(id)
    }

    fn item_of(id: &str) -> NewOutboxItem {
        NewOutboxItem {
            item_class: "closure",
            incident_kind: None,
            message_text: format!("text {id}"),
            incident_id: None,
        }
    }

    #[tokio::test]
    async fn enqueued_items_wait_for_the_batch_window_before_delivery() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(1_000);
        let mut transaction = crate::db::begin_immediate(&pool).await?;
        enqueue_in_transaction(&mut transaction, &item_of("a"), now).await?;
        transaction.commit().await?;

        assert!(due_items(&pool, now).await?.is_empty());
        assert!(due_items(
            &pool,
            now + chrono::Duration::seconds(BATCH_WINDOW_SECS - 1)
        )
        .await?
        .is_empty());
        let due = due_items(&pool, now + chrono::Duration::seconds(BATCH_WINDOW_SECS)).await?;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].message_text, "text a");
        assert_eq!(due[0].attempts, 0);
        assert_eq!(
            delivery_health(&pool).await?.pending_count,
            1,
            "the queued item is the pending state Settings shows"
        );
        Ok(())
    }

    #[tokio::test]
    async fn same_window_items_share_a_batch_bucket() -> AppResult<()> {
        let pool = test_pool().await?;
        // Two items inside one 30-second window land in the same bucket; an
        // item past the window boundary lands in the next.
        let mut transaction = crate::db::begin_immediate(&pool).await?;
        enqueue_in_transaction(&mut transaction, &item_of("a"), at(BATCH_WINDOW_SECS)).await?;
        enqueue_in_transaction(
            &mut transaction,
            &item_of("b"),
            at(BATCH_WINDOW_SECS * 2 - 1),
        )
        .await?;
        enqueue_in_transaction(&mut transaction, &item_of("c"), at(BATCH_WINDOW_SECS * 2)).await?;
        transaction.commit().await?;

        let buckets: Vec<i64> =
            sqlx::query_scalar("SELECT batch_bucket FROM telegram_outbox ORDER BY message_text")
                .fetch_all(&pool)
                .await?;
        assert_eq!(buckets[0], buckets[1]);
        assert_eq!(buckets[2], buckets[1] + 1);
        Ok(())
    }

    #[tokio::test]
    async fn sent_items_leave_the_pending_queue_and_record_the_success() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(2_000);
        let id = enqueue_sample(&pool, "text", now).await?;

        mark_sent(&pool, &[id], now).await?;

        assert!(due_items(&pool, now).await?.is_empty());
        let health = delivery_health(&pool).await?;
        assert_eq!(health.pending_count, 0);
        assert_eq!(health.last_sent_at, Some(format_rfc3339(now)));
        assert_eq!(health.last_error, None);
        Ok(())
    }

    #[tokio::test]
    async fn failures_back_off_exponentially_then_recover() -> AppResult<()> {
        let pool = test_pool().await?;
        let created = at(3_000);
        let _id = enqueue_sample(&pool, "text", created).await?;
        let first_attempt = created + chrono::Duration::seconds(BATCH_WINDOW_SECS);
        let mut item = due_items(&pool, first_attempt).await?.remove(0);

        // First failure waits 30s, second 60s — bounded growth.
        schedule_retry_or_abandon(&pool, &item, "offline", first_attempt).await?;
        assert!(due_items(&pool, first_attempt).await?.is_empty());
        assert!(
            due_items(&pool, first_attempt + chrono::Duration::seconds(29))
                .await?
                .is_empty()
        );
        item = due_items(&pool, first_attempt + chrono::Duration::seconds(30))
            .await?
            .remove(0);
        assert_eq!(item.attempts, 1);

        let second_attempt = first_attempt + chrono::Duration::seconds(30);
        schedule_retry_or_abandon(&pool, &item, "offline", second_attempt).await?;
        item = due_items(&pool, second_attempt + chrono::Duration::seconds(60))
            .await?
            .remove(0);
        assert_eq!(item.attempts, 2);

        // The recorded error is what Settings will show inline; the item
        // moved from queued to retrying.
        let health = delivery_health(&pool).await?;
        assert_eq!(health.last_error.as_deref(), Some("offline"));
        assert_eq!(health.pending_count, 1);
        assert_eq!(health.queued_count, 0);
        assert_eq!(health.retrying_count, 1);
        assert_eq!(health.expired_count, 0);

        // Recovery delivers and clears the error state's pending count.
        mark_sent(
            &pool,
            &[item.id],
            second_attempt + chrono::Duration::seconds(61),
        )
        .await?;
        assert_eq!(delivery_health(&pool).await?.pending_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn backoff_is_capped_at_one_hour() -> AppResult<()> {
        let pool = test_pool().await?;
        let created = at(4_000);
        let id = enqueue_sample(&pool, "text", created).await?;
        let first_attempt = created + chrono::Duration::seconds(BATCH_WINDOW_SECS);
        // Drive many failed attempts without waiting for real backoff by
        // rewriting the attempt counter directly.
        sqlx::query("UPDATE telegram_outbox SET attempts = 20 WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await?;
        let item = due_items(&pool, first_attempt).await?.remove(0);

        schedule_retry_or_abandon(&pool, &item, "still offline", first_attempt).await?;

        let next: String =
            sqlx::query_scalar("SELECT next_attempt_at FROM telegram_outbox WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await?;
        let next = parse_db_timestamp(&next).expect("next attempt");
        assert_eq!(
            next.signed_duration_since(first_attempt),
            chrono::Duration::seconds(BACKOFF_MAX_SECS),
            "late-stage retries wait at most one hour"
        );
        Ok(())
    }

    #[tokio::test]
    async fn items_past_the_retry_window_are_abandoned_never_sent() -> AppResult<()> {
        let pool = test_pool().await?;
        let created = at(5_000);
        let id = enqueue_sample(&pool, "text", created).await?;
        let item = due_items(
            &pool,
            created + chrono::Duration::seconds(BATCH_WINDOW_SECS),
        )
        .await?
        .remove(0);

        // One failure just inside the window still reschedules...
        schedule_retry_or_abandon(
            &pool,
            &item,
            "offline",
            created + chrono::Duration::seconds(RETRY_WINDOW_SECS - 1),
        )
        .await?;
        let status: String = sqlx::query_scalar("SELECT status FROM telegram_outbox WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "pending");

        // ...but a failure attempted after the full window abandons it unsent.
        let late_attempt =
            created + chrono::Duration::seconds(RETRY_WINDOW_SECS + BACKOFF_MAX_SECS);
        let late = due_items(&pool, late_attempt).await?.remove(0);
        schedule_retry_or_abandon(&pool, &late, "gave up", late_attempt).await?;
        let (status, pending): (String, i64) = sqlx::query_as(
            "SELECT status, (SELECT COUNT(*) FROM telegram_outbox WHERE status = 'pending')
             FROM telegram_outbox WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(status, "abandoned");
        assert_eq!(pending, 0);
        Ok(())
    }

    #[tokio::test]
    async fn expired_items_show_as_expired_locally_until_pruned() -> AppResult<()> {
        let pool = test_pool().await?;
        let created = at(9_000);
        enqueue_sample(&pool, "never delivered", created).await?;
        // Freshly queued items are the queued state Settings shows.
        let health = delivery_health(&pool).await?;
        assert_eq!((health.queued_count, health.retrying_count), (1, 0));

        // The 24-hour window closes with the attempt still failing: the item
        // expires unsent and stays visible as expired until retention prunes
        // it — locally truthful, nothing re-sends it.
        let item = due_items(
            &pool,
            created + chrono::Duration::seconds(BATCH_WINDOW_SECS),
        )
        .await?
        .remove(0);
        schedule_retry_or_abandon(
            &pool,
            &item,
            "still offline",
            created + chrono::Duration::seconds(RETRY_WINDOW_SECS + BACKOFF_MAX_SECS),
        )
        .await?;

        let health = delivery_health(&pool).await?;
        assert_eq!(health.pending_count, 0);
        assert_eq!(health.expired_count, 1);
        assert_eq!(health.last_error.as_deref(), Some("still offline"));

        // Retention eventually removes the row and the expired state.
        prune_finished(
            &pool,
            created
                + chrono::Duration::days(PRUNE_FINISHED_AFTER_DAYS)
                + chrono::Duration::seconds(1),
        )
        .await?;
        assert_eq!(delivery_health(&pool).await?.expired_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn finished_rows_are_pruned_after_retention() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(6_000);
        let id = enqueue_sample(&pool, "text", now).await?;
        mark_sent(&pool, &[id], now).await?;

        assert_eq!(prune_finished(&pool, now).await?, 0);
        assert_eq!(
            prune_finished(
                &pool,
                now + chrono::Duration::days(PRUNE_FINISHED_AFTER_DAYS)
                    + chrono::Duration::seconds(1)
            )
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telegram_outbox")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_removes_every_delivery_row_inside_its_transaction() -> AppResult<()> {
        let pool = test_pool().await?;
        connect_with_audit(&pool, &sample("protected")).await?;
        enqueue_sample(&pool, "pending line", at(7_000)).await?;

        assert!(disconnect_with_audit(&pool).await?);

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telegram_outbox")
                .fetch_one(&pool)
                .await?,
            0,
            "disconnect deletes pending deliveries with the destination"
        );
        Ok(())
    }

    #[tokio::test]
    async fn replacing_the_destination_drops_old_pending_deliveries() -> AppResult<()> {
        let pool = test_pool().await?;
        connect_with_audit(&pool, &sample("old-protected")).await?;
        enqueue_sample(&pool, "must not reach the replacement", at(7_100)).await?;

        connect_with_audit(&pool, &sample("new-protected")).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telegram_outbox")
                .fetch_one(&pool)
                .await?,
            0,
            "old queued messages cannot reach the replacement destination"
        );
        Ok(())
    }

    #[tokio::test]
    async fn rolled_back_transactions_leave_no_delivery_item() -> AppResult<()> {
        let pool = test_pool().await?;
        let mut transaction = crate::db::begin_immediate(&pool).await?;
        enqueue_in_transaction(&mut transaction, &item_of("doomed"), at(8_000)).await?;
        transaction.rollback().await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telegram_outbox")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }

    #[test]
    fn db_timestamp_parsing_covers_both_storage_shapes() {
        let sqlite_now = parse_db_timestamp("2026-08-26 12:34:56").expect("sqlite shape");
        assert_eq!(
            sqlite_now.format("%Y-%m-%d %H:%M").to_string(),
            "2026-08-26 12:34"
        );
        let rfc3339 = parse_db_timestamp("2026-08-26T12:34:56Z").expect("rfc3339 shape");
        assert_eq!(sqlite_now, rfc3339);
        assert_eq!(parse_db_timestamp("not a time"), None);
    }

    #[test]
    fn composed_messages_carry_only_redacted_content() {
        let opened = parse_db_timestamp("2026-08-26 10:00:00").expect("opened");
        let closed = parse_db_timestamp("2026-08-26 10:07:30").expect("closed");

        let opening = compose_opening("PC 01", "Client Reachability Mismatch", opened);
        assert_eq!(
            opening,
            "Client Reachability Mismatch detected on PC 01 at 2026-08-26 10:00 UTC."
        );
        let closure = compose_closure(
            "PC 01",
            "Client Reachability Mismatch",
            opened,
            closed,
            false,
            "Client recovered",
        );
        assert_eq!(
            closure,
            "Client Reachability Mismatch on PC 01 closed at 2026-08-26 10:07 UTC \
             (about 7 minutes): Client recovered."
        );

        let resolved = compose_resolved(
            "PC 02",
            "Host Probe Coverage",
            opened,
            closed,
            false,
            "host answered probes again",
        );
        assert!(resolved.contains("opened at 2026-08-26 10:00 UTC"));
        assert!(resolved.contains("resolved at 2026-08-26 10:07 UTC"));

        // An interrupted episode never claims continuous duration.
        let interrupted = compose_closure(
            "PC 03",
            "Client Guard Coverage",
            opened,
            closed,
            true,
            "monitoring disabled",
        );
        assert!(interrupted.contains("observation interrupted by an Admin restart"));
        assert!(!interrupted.contains("minute"));

        let disable_empty = compose_monitoring_disabled(0, closed);
        let disable_some = compose_monitoring_disabled(2, closed);
        assert!(disable_empty.starts_with("PC Monitoring was disabled at 2026-08-26 10:07 UTC."));
        assert_eq!(
            disable_some,
            "PC Monitoring was disabled at 2026-08-26 10:07 UTC; 2 open incidents were closed as monitoring disabled."
        );
    }
}

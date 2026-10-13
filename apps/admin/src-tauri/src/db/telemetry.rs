//! Local Telemetry installation and durable Daily Summary outbox (ADR-0083).

use chrono::{DateTime, Duration, Utc};
use sqlx::{Row, SqlitePool};

use crate::errors::AppResult;

use super::telegram::{BACKOFF_BASE_SECS, BACKOFF_MAX_SECS};

#[derive(Debug, Clone)]
pub struct TelemetrySettings {
    pub installation_id: String,
    pub last_summary_period: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ActivePcCounts {
    pub clients_active_24h: i64,
    pub clients_active_30d: i64,
}

/// Mint one random ID only when a worker needs to summarise. Concurrent callers
/// read back the winning row rather than returning a losing UUID.
pub async fn installation(pool: &SqlitePool) -> AppResult<TelemetrySettings> {
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "INSERT INTO telemetry_settings (id, installation_id) VALUES (1, ?)
                 ON CONFLICT(id) DO NOTHING",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .execute(&mut *transaction)
    .await?;
    let row = sqlx::query(
        "SELECT installation_id, last_summary_period FROM telemetry_settings WHERE id = 1",
    )
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(TelemetrySettings {
        installation_id: row.try_get("installation_id")?,
        last_summary_period: row.try_get("last_summary_period")?,
    })
}

pub async fn upsert_summary(
    pool: &SqlitePool,
    period_utc8: &str,
    installation_id: &str,
    event_uuid: &str,
    payload: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let mut transaction = super::begin_immediate(pool).await?;
    let updated = sqlx::query(
        "UPDATE telemetry_settings SET last_summary_period = ?
                   WHERE id = 1 AND installation_id = ?",
    )
    .bind(period_utc8)
    .bind(installation_id)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(crate::errors::AppError::Conflict(
            "Telemetry installation changed".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO telemetry_outbox
                 (period_utc8, event_uuid, payload, next_attempt_at, created_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(period_utc8) DO UPDATE SET
                   event_uuid = excluded.event_uuid, payload = excluded.payload,
                   attempts = 0, next_attempt_at = excluded.next_attempt_at,
                   last_error = NULL, created_at = excluded.created_at",
    )
    .bind(period_utc8)
    .bind(event_uuid)
    .bind(payload)
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

#[derive(Debug)]
pub struct OutboxRow {
    pub period_utc8: String,
    pub event_uuid: String,
    pub payload: String,
    pub attempts: i64,
}

pub async fn due_rows(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<Vec<OutboxRow>> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT period_utc8, event_uuid, payload, attempts FROM telemetry_outbox
         WHERE next_attempt_at <= ? ORDER BY period_utc8",
    )
    .bind(now.to_rfc3339())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(period_utc8, event_uuid, payload, attempts)| OutboxRow {
            period_utc8,
            event_uuid,
            payload,
            attempts,
        })
        .collect())
}

pub async fn mark_sent(pool: &SqlitePool, rows: &[OutboxRow]) -> AppResult<()> {
    let mut transaction = super::begin_immediate(pool).await?;
    for row in rows {
        sqlx::query("DELETE FROM telemetry_outbox WHERE period_utc8 = ? AND event_uuid = ?")
            .bind(&row.period_utc8)
            .bind(&row.event_uuid)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

pub async fn mark_failed(
    pool: &SqlitePool,
    rows: &[OutboxRow],
    now: DateTime<Utc>,
    error: &str,
) -> AppResult<()> {
    let mut transaction = super::begin_immediate(pool).await?;
    for row in rows {
        let exponent = u32::try_from(row.attempts).unwrap_or(u32::MAX).min(7);
        let delay = (BACKOFF_BASE_SECS * (1_i64 << exponent)).min(BACKOFF_MAX_SECS);
        sqlx::query(
            "UPDATE telemetry_outbox SET attempts = attempts + 1,
              next_attempt_at = ?, last_error = ? WHERE period_utc8 = ? AND event_uuid = ?",
        )
        .bind((now + Duration::seconds(delay)).to_rfc3339())
        .bind(error)
        .bind(&row.period_utc8)
        .bind(&row.event_uuid)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

pub async fn drop_expired(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<u64> {
    let deleted = sqlx::query(
        "DELETE FROM telemetry_outbox WHERE
             datetime(created_at) < datetime(?)",
    )
    .bind((now - Duration::days(30)).to_rfc3339())
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected())
}

/// Inclusive 24-hour and 30-day windows, excluding timestamps after now.
pub async fn active_pc_counts(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<ActivePcCounts> {
    let (day, month): (i64, i64) = sqlx::query_as(
        "SELECT
           COUNT(*) FILTER (WHERE last_seen_at >= ? AND last_seen_at <= ?),
           COUNT(*) FILTER (WHERE last_seen_at >= ? AND last_seen_at <= ?)
         FROM pcs",
    )
    .bind(
        (now - Duration::hours(24))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
    .bind(now.format("%Y-%m-%d %H:%M:%S").to_string())
    .bind(
        (now - Duration::days(30))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
    .bind(now.format("%Y-%m-%d %H:%M:%S").to_string())
    .fetch_one(pool)
    .await?;
    Ok(ActivePcCounts {
        clients_active_24h: day,
        clients_active_30d: month,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mem_test_pool;

    #[tokio::test]
    async fn active_pc_counts_include_boundaries_and_exclude_future_heartbeats() -> AppResult<()> {
        let pool = mem_test_pool().await;
        for (id, seen) in [
            ("recent", "2026-09-22 12:00:00"),
            ("day-edge", "2026-09-21 12:00:00"),
            ("month-edge", "2026-08-23 12:00:00"),
            ("expired", "2026-08-23 11:59:59"),
            ("future", "2026-09-22 12:00:01"),
        ] {
            sqlx::query("INSERT INTO pcs (id, name, last_seen_at) VALUES (?, ?, ?)")
                .bind(id)
                .bind(id)
                .bind(seen)
                .execute(&pool)
                .await?;
        }
        let counts = active_pc_counts(
            &pool,
            chrono::DateTime::parse_from_rfc3339("2026-09-22T12:00:00Z")
                .expect("now")
                .with_timezone(&chrono::Utc),
        )
        .await?;
        assert_eq!(
            counts,
            ActivePcCounts {
                clients_active_24h: 2,
                clients_active_30d: 3
            }
        );
        Ok(())
    }
}

use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::{Sqlite, Transaction};

use crate::errors::AppResult;

#[derive(Debug)]
pub struct AccountSessionAtKill {
    pub account_id: String,
    pub session_id: String,
}

type AccountSessionHistoryRow = (String, String, String, Option<String>, Option<String>);

/// Parse both client RFC 3339 timestamps and SQLite's UTC timestamp format.
pub fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|timestamp| DateTime::<Utc>::from_naive_utc_and_offset(timestamp, Utc))
        })
}

pub async fn report_exists(
    transaction: &mut Transaction<'_, Sqlite>,
    pc_id: &str,
    kill_timestamp: &str,
) -> AppResult<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM audit_log
           WHERE event_type = 'tamper_kill'
             AND pc_id = ?
             AND json_extract(metadata_json, '$.kill_timestamp') = ?
         )",
    )
    .bind(pc_id)
    .bind(kill_timestamp)
    .fetch_one(&mut **transaction)
    .await
    .map_err(Into::into)
}

pub async fn report_subject(
    transaction: &mut Transaction<'_, Sqlite>,
    pc_id: &str,
    kill_timestamp: &str,
) -> AppResult<Option<String>> {
    sqlx::query_scalar(
        "SELECT json_extract(metadata_json, '$.subject')
         FROM audit_log
         WHERE event_type = 'tamper_kill'
           AND pc_id = ?
           AND json_extract(metadata_json, '$.kill_timestamp') = ?
         LIMIT 1",
    )
    .bind(pc_id)
    .bind(kill_timestamp)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(Into::into)
}

/// Find the historical Account Session that covered a trusted kill instant.
/// `server_restart` is an infrastructure closure, so its recorded end does
/// not disqualify the session from identifying the customer after restart.
pub async fn account_session_at_kill(
    transaction: &mut Transaction<'_, Sqlite>,
    pc_id: &str,
    kill_at: DateTime<Utc>,
) -> AppResult<Option<AccountSessionAtKill>> {
    let rows: Vec<AccountSessionHistoryRow> = sqlx::query_as(
        "SELECT session_id, account_id, started_at, ended_at, end_reason
         FROM account_session_history
         WHERE pc_id = ?
         ORDER BY started_at DESC",
    )
    .bind(pc_id)
    .fetch_all(&mut **transaction)
    .await?;

    for (session_id, account_id, started_at, ended_at, end_reason) in rows {
        let Some(started_at) = parse_timestamp(&started_at) else {
            continue;
        };
        if started_at > kill_at {
            continue;
        }
        let covered = match ended_at {
            None => true,
            Some(_ended_at) if end_reason.as_deref() == Some("server_restart") => true,
            Some(ended_at) => parse_timestamp(&ended_at).is_some_and(|ended_at| kill_at < ended_at),
        };
        if covered {
            return Ok(Some(AccountSessionAtKill {
                account_id,
                session_id,
            }));
        }
    }
    Ok(None)
}

pub async fn pc_name(
    transaction: &mut Transaction<'_, Sqlite>,
    pc_id: &str,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(&mut **transaction)
        .await?)
}

pub async fn guest_remaining_seconds(
    transaction: &mut Transaction<'_, Sqlite>,
    pc_id: &str,
) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = ?")
            .bind(pc_id)
            .fetch_optional(&mut **transaction)
            .await?
            .unwrap_or(0),
    )
}

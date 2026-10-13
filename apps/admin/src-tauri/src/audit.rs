use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use uuid::Uuid;

use crate::errors::AppResult;

pub struct NewAuditEvent<'a> {
    pub event_type: &'a str,
    pub outcome: AuditOutcome,
    pub actor: AuditActor,
    pub actor_account_id: Option<&'a str>,
    pub account_id: Option<&'a str>,
    pub pc_id: Option<&'a str>,
    pub amount_cents: Option<i64>,
    pub seconds_delta: Option<i64>,
    /// Attendance Time delta, separate from Remaining/Guest `seconds_delta`
    /// (ADR-0084). Positive on a Check-In; the claim/deletion signs arrive in
    /// later slices. `None` on every non-Attendance event.
    pub attendance_seconds_delta: Option<i64>,
    pub points_delta_centi: Option<i64>,
    pub cash_flag: bool,
    pub summary: String,
    pub metadata: Value,
}

impl<'a> NewAuditEvent<'a> {
    pub fn simple(
        event_type: &'a str,
        outcome: AuditOutcome,
        actor: AuditActor,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            event_type,
            outcome,
            actor,
            actor_account_id: None,
            account_id: None,
            pc_id: None,
            amount_cents: None,
            seconds_delta: None,
            attendance_seconds_delta: None,
            points_delta_centi: None,
            cash_flag: false,
            summary: summary.into(),
            metadata: Value::Object(Default::default()),
        }
    }
}

pub async fn append(
    transaction: &mut Transaction<'_, Sqlite>,
    event: NewAuditEvent<'_>,
) -> AppResult<String> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO audit_log (
           id, event_type, outcome, actor, actor_account_id, account_id, pc_id,
           amount_cents, seconds_delta, attendance_seconds_delta,
           points_delta_centi, cash_flag, summary, metadata_json
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(event.event_type)
    .bind(event.outcome.as_str())
    .bind(event.actor.as_str())
    .bind(event.actor_account_id)
    .bind(event.account_id)
    .bind(event.pc_id)
    .bind(event.amount_cents)
    .bind(event.seconds_delta)
    .bind(event.attendance_seconds_delta)
    .bind(event.points_delta_centi)
    .bind(i64::from(event.cash_flag))
    .bind(event.summary)
    .bind(event.metadata.to_string())
    .execute(&mut **transaction)
    .await?;
    Ok(id)
}

pub async fn append_standalone(
    pool: &sqlx::SqlitePool,
    event: NewAuditEvent<'_>,
) -> AppResult<String> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let id = append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(id)
}

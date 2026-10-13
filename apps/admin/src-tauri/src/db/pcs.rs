use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::{QueryBuilder, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{ManualAdjustment, ManualAdjustmentInput, Pc, PcSettings, PcSettingsInput},
    pc_identity,
};

pub async fn create_manual_adjustment(
    pool: &SqlitePool,
    input: &ManualAdjustmentInput,
) -> AppResult<ManualAdjustment> {
    let pc_id = input.pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("PC ID is required".to_owned()));
    }
    if input.seconds_delta == 0 {
        return Err(AppError::InvalidInput(
            "seconds delta must not be zero".to_owned(),
        ));
    }
    let reason = input
        .reason
        .as_deref()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty());

    let mut transaction = super::begin_immediate(pool).await?;
    let prior_remaining =
        sqlx::query_scalar::<_, i64>("SELECT remaining_seconds FROM pcs WHERE id = ?")
            .bind(pc_id)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    let pc_name = sqlx::query_scalar::<_, String>("SELECT name FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_one(&mut *transaction)
        .await?;

    let requested = input.seconds_delta;
    let applied = requested.clamp(-prior_remaining, i64::MAX);
    if applied == 0 {
        return Err(AppError::Conflict(
            "PC has no Guest Time to subtract".to_owned(),
        ));
    }

    let summary = if applied > 0 {
        match reason {
            Some(r) => format!("Added Guest Time manually: {r}"),
            None => "Added Guest Time manually".to_owned(),
        }
    } else {
        match reason {
            Some(r) => format!("Subtracted Guest Time manually: {r}"),
            None => "Subtracted Guest Time manually".to_owned(),
        }
    };

    sqlx::query(
        "UPDATE pcs
         SET remaining_seconds = MAX(remaining_seconds + ?, 0),
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(applied)
    .bind(pc_id)
    .execute(&mut *transaction)
    .await?;

    let mut event = NewAuditEvent::simple(
        "manual_adjustment",
        AuditOutcome::Success,
        AuditActor::Admin,
        &summary,
    );
    event.pc_id = Some(pc_id);
    event.seconds_delta = Some(applied);
    event.cash_flag = false;
    event.metadata = json!({
        "requested_seconds_delta": requested,
        "applied_seconds_delta": applied,
        "prior_remaining_seconds": prior_remaining,
        "reason": reason
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO manual_adjustments (id, pc_id, seconds_delta, reason, audit_event_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(pc_id)
    .bind(applied)
    .bind(reason)
    .bind(&audit_event_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let created_at =
        sqlx::query_scalar::<_, String>("SELECT created_at FROM manual_adjustments WHERE id = ?")
            .bind(&id)
            .fetch_one(pool)
            .await?;

    Ok(ManualAdjustment {
        id,
        pc_id: pc_id.to_owned(),
        pc_name,
        seconds_delta: applied,
        requested_seconds_delta: requested,
        reason: reason.map(|r| r.to_owned()),
        audit_event_id,
        created_at,
    })
}

pub async fn pcs(pool: &SqlitePool) -> AppResult<Vec<Pc>> {
    let shutdown_grace = super::settings::shutdown_grace_seconds(pool).await?;
    let mut pcs: Vec<Pc> =
        sqlx::query_as::<_, (String, String, String, i64, Option<String>, Option<i64>)>(
            "SELECT id, name, status, remaining_seconds, last_seen_at,
                clock_difference_ms
         FROM pcs WHERE visible = 1 ORDER BY name",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(Pc::from)
        .collect();
    super::settings::apply_shutdown_grace(&mut pcs, shutdown_grace);
    Ok(pcs)
}

// ---------------------------------------------------------------------------
// PC identity (ADR-0031 / plan §4.2 / fix plan §4 M2-M3).
//
// The admin server now owns which `pcs` row a machine is. The pure
// resolution decision lives in `pc_identity::resolve`; the DB layer here
// loads the candidate rows, applies the decision inside a transaction, and
// performs the bind-on-step-2 update atomically with the rest of the
// heartbeat (or whatever the caller is doing). The DB never trusts a
// body-supplied IP — the source IP is captured at the HTTP boundary via
// `ConnectInfo<SocketAddr>` and passed in by the call-site.
// ---------------------------------------------------------------------------

async fn pc_identity_rows_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
) -> AppResult<Vec<pc_identity::PcIdentityRow>> {
    Ok(
        sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT id, mac_address, registered_ip FROM pcs",
        )
        .fetch_all(&mut **transaction)
        .await?
        .into_iter()
        .map(
            |(id, mac_address, registered_ip)| pc_identity::PcIdentityRow {
                id,
                mac_address,
                registered_ip,
            },
        )
        .collect(),
    )
}

async fn apply_identity_decision_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    decision: &pc_identity::Decision,
    mac: Option<&str>,
) -> AppResult<Option<String>> {
    match decision {
        pc_identity::Decision::Reject => Ok(None),
        pc_identity::Decision::Identify { pc_id } => Ok(Some(pc_id.clone())),
        pc_identity::Decision::Bind { pc_id } => {
            let mac = mac
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::InvalidInput("bind decision requires a MAC".to_owned()))?;
            let updated = sqlx::query(
                "UPDATE pcs
                 SET mac_address = ?, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ? AND mac_address IS NULL",
            )
            .bind(mac)
            .bind(pc_id)
            .execute(&mut **transaction)
            .await?;
            if updated.rows_affected() == 0 {
                return Ok(None);
            }
            Ok(Some(pc_id.clone()))
        }
    }
}

/// Insert a new pre-registered PC row (ADR-0031 / plan §4 M5). The owner
/// sets a number/name + a static IP, leaving `mac_address` NULL so the
/// next boot from that IP binds the row. The id is required and must be
/// non-empty (callers normalize to e.g. `pc-05`); a duplicate id is a
/// 409.
#[allow(dead_code)] // M5: wired to the web-admin "register PC" surface.
pub async fn create_pc(
    pool: &SqlitePool,
    pc_id: &str,
    name: &str,
    registered_ip: Option<&str>,
) -> AppResult<Pc> {
    let pc_id = pc_id.trim();
    let name = name.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("PC ID is required".to_owned()));
    }
    if name.is_empty() {
        return Err(AppError::InvalidInput(
            "PC display name is required".to_owned(),
        ));
    }
    if name.len() > 80 {
        return Err(AppError::InvalidInput(
            "PC display name must not exceed 80 characters".to_owned(),
        ));
    }
    if let Some(ip) = registered_ip {
        if ip.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "registered IP must not be blank when provided".to_owned(),
            ));
        }
    }

    let mut transaction = super::begin_immediate(pool).await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(&mut *transaction)
        .await?;
    if exists.is_some() {
        return Err(AppError::Conflict(format!("PC '{pc_id}' already exists")));
    }
    sqlx::query(
        "INSERT INTO pcs (id, name, status, remaining_seconds, visible, registered_ip, mac_address)
         VALUES (?, ?, 'offline', 0, 1, ?, NULL)",
    )
    .bind(pc_id)
    .bind(name)
    .bind(
        registered_ip
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    )
    .execute(&mut *transaction)
    .await?;
    // The FK `pc_id` is intentionally left NULL and the id recorded in
    // metadata instead (ADR-0044). audit_log is append-only + FK-protected to
    // pcs, so a set pc_id would permanently pin this row and make a later
    // clean-row removal (`pc_removed`) impossible — the very row an owner
    // pre-registered by mistake could never be deleted. Only material events
    // (coins, adjustments, wheel spins) set the FK and rightly block removal.
    let mut event = NewAuditEvent::simple(
        "pc_pre_registered",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Pre-registered a PC for first-boot bind",
    );
    event.metadata = json!({
        "pc_id": pc_id,
        "name": name,
        "registered_ip": registered_ip,
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;

    pc(pool, pc_id).await
}

/// Hard-delete a clean PC row (issue #112 / ADR-0044). Removal is permitted
/// only for a row with no material history and no live session:
///
/// - no referencing rows in `coin_transactions`, `manual_adjustments`,
///   `wheel_spins`, or `audit_log`;
/// - `remaining_seconds = 0`;
/// - status neither `online` nor `in_use`.
///
/// A bound-but-offline MAC does not block removal (it is not history). Rows
/// with any of the above stay and must be hidden via the `visible` flag
/// instead. Every check and the delete run in one immediate transaction; the
/// pcs foreign keys keep protecting history as a backstop if a check is ever
/// missed.
///
/// The removal appends a `pc_removed` audit event whose FK `pc_id` is
/// intentionally NULL — the referenced pcs row is deleted in this same
/// transaction and audit_log is append-only + FK-protected, so a set pc_id
/// would abort the DELETE. The removed id, name, registered IP and MAC live in
/// the event metadata.
pub async fn remove_pc(pool: &SqlitePool, pc_id: &str) -> AppResult<()> {
    let pc_id = pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("PC ID is required".to_owned()));
    }

    let mut transaction = super::begin_immediate(pool).await?;

    let (status, name, remaining_seconds, registered_ip, mac_address) =
        sqlx::query_as::<_, (String, String, i64, Option<String>, Option<String>)>(
            "SELECT status, name, remaining_seconds, registered_ip, mac_address
             FROM pcs WHERE id = ?",
        )
        .bind(pc_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;

    if status == "online" || status == "in_use" {
        return Err(AppError::Conflict(
            "PC is online; hide it instead of removing it".to_owned(),
        ));
    }
    if remaining_seconds > 0 {
        return Err(AppError::Conflict(
            "PC has remaining Guest Time; hide it instead of removing it".to_owned(),
        ));
    }

    let referencing: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM coin_transactions WHERE pc_id = ?)
         + (SELECT COUNT(*) FROM manual_adjustments WHERE pc_id = ?)
         + (SELECT COUNT(*) FROM wheel_spins WHERE pc_id = ?)
         + (SELECT COUNT(*) FROM audit_log WHERE pc_id = ?)",
    )
    .bind(pc_id)
    .bind(pc_id)
    .bind(pc_id)
    .bind(pc_id)
    .fetch_one(&mut *transaction)
    .await?;
    if referencing > 0 {
        return Err(AppError::Conflict(
            "PC has coin, adjustment, or audit history; hide it instead of removing it".to_owned(),
        ));
    }

    let mut event = NewAuditEvent::simple(
        "pc_removed",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Removed a clean PC row",
    );
    // pc_id stays NULL by design — see the doc comment above.
    event.metadata = json!({
        "pc_id": pc_id,
        "name": name,
        "registered_ip": registered_ip,
        "mac_address": mac_address,
    });
    audit::append(&mut transaction, event).await?;

    sqlx::query("DELETE FROM pcs WHERE id = ?")
        .bind(pc_id)
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await?;
    Ok(())
}

pub async fn pc_settings(pool: &SqlitePool) -> AppResult<Vec<PcSettings>> {
    Ok(
        sqlx::query_as::<_, (String, String, i64, Option<String>, Option<String>)>(
            "SELECT id, name, visible, registered_ip, mac_address
         FROM pcs ORDER BY name",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(
            |(id, name, visible, registered_ip, mac_address)| PcSettings {
                id,
                name,
                visible: visible == 1,
                registered_ip,
                mac_address,
            },
        )
        .collect(),
    )
}

pub async fn update_pc_settings(
    pool: &SqlitePool,
    pc_id: &str,
    input: &PcSettingsInput,
) -> AppResult<PcSettings> {
    let pc_id = pc_id.trim();
    let name = input.name.trim();
    if pc_id.is_empty() || name.is_empty() {
        return Err(AppError::InvalidInput(
            "PC ID and display name are required".to_owned(),
        ));
    }
    if name.len() > 80 {
        return Err(AppError::InvalidInput(
            "PC display name must not exceed 80 characters".to_owned(),
        ));
    }

    let normalize_optional = |value: &Option<String>| -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let registered_ip = normalize_optional(&input.registered_ip);
    let mac_address = normalize_optional(&input.mac_address);

    let mut transaction = super::begin_immediate(pool).await?;
    let updated = sqlx::query_as::<_, (String, String, i64, Option<String>, Option<String>)>(
        "UPDATE pcs
         SET name = ?, visible = ?, registered_ip = ?, mac_address = ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ?
         RETURNING id, name, visible, registered_ip, mac_address",
    )
    .bind(name)
    .bind(if input.visible { 1_i64 } else { 0_i64 })
    .bind(&registered_ip)
    .bind(&mac_address)
    .bind(pc_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    // FK `pc_id` left NULL (id in metadata) for the same reason as
    // `pc_pre_registered`: keep a settings-only PC row removable (ADR-0044).
    let mut event = NewAuditEvent::simple(
        "pc_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated PC settings",
    );
    event.metadata = json!({
        "pc_id": pc_id,
        "name": name,
        "visible": input.visible,
        "registered_ip": registered_ip,
        "mac_address": mac_address,
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(PcSettings {
        id: updated.0,
        name: updated.1,
        visible: updated.2 == 1,
        registered_ip: updated.3,
        mac_address: updated.4,
    })
}

/// Update the timestamp/status fields of an already-known PC without
/// re-resolving identity. Used by tests that want to simulate a
/// connected PC after the bind step has happened via the HTTP layer.
#[cfg(test)]
pub async fn heartbeat_by_identity_only(pool: &SqlitePool, pc_id: &str) -> AppResult<Pc> {
    let mut transaction = super::begin_immediate(pool).await?;
    let prior_status: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    sqlx::query(
        "UPDATE pcs
         SET status = CASE WHEN remaining_seconds > 0 THEN 'in_use' ELSE 'online' END,
             last_seen_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(pc_id)
    .execute(&mut *transaction)
    .await?;
    if prior_status == "offline" {
        append_reachability_event(&mut transaction, "pc_came_online", pc_id, "heartbeat").await?;
    }
    transaction.commit().await?;
    pc(pool, pc_id).await
}

async fn append_reachability_event(
    transaction: &mut Transaction<'_, Sqlite>,
    event_type: &str,
    pc_id: &str,
    source: &str,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        event_type,
        AuditOutcome::Success,
        AuditActor::System,
        if event_type == "pc_came_online" {
            "PC came online"
        } else {
            "PC went offline"
        },
    );
    event.pc_id = Some(pc_id);
    event.metadata = json!({ "transition_source": source });
    audit::append(transaction, event).await?;
    Ok(())
}

/// The identity resolved while the heartbeat transaction is still open. The
/// session layer appends any Tamper Kill before committing the PC mutation.
pub(crate) struct IdentityHeartbeatMutation {
    pub pc_id: String,
}

/// Apply identity resolution, the heartbeat mutation, and its reachability
/// event to an existing transaction. Callers that have more authority-affecting
/// work to perform can append it before committing the transaction.
/// How one identity mutation should touch `pcs.last_guard_coverage`
/// (ADR-0076 / #344). The stored diagnostic must always reflect the current
/// Client process, never a stale report.
pub(crate) enum GuardCoverageWrite<'a> {
    /// A Heartbeat that carried the typed field: store this outcome.
    Report(&'a str),
    /// A real Heartbeat from a Client that omits the field (older build):
    /// the previous report no longer describes this Client, so reset to
    /// unknown — an omitted report can never keep a Guard Alert alive.
    ResetUnknown,
    /// A body-less Admin-side keepalive (the SSE event-stream resolve):
    /// carries no Guard evidence either way, so leave the stored value.
    Leave,
}

pub(crate) async fn heartbeat_by_identity_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    mac: Option<&str>,
    source_ip: &str,
    hostname: &str,
    client_version: &str,
    clock_difference_ms: Option<i64>,
    guard_coverage: GuardCoverageWrite<'_>,
) -> AppResult<Option<IdentityHeartbeatMutation>> {
    if hostname.trim().is_empty() {
        return Err(AppError::InvalidInput("hostname is required".to_owned()));
    }
    if client_version.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "client version is required".to_owned(),
        ));
    }

    let rows = pc_identity_rows_in_transaction(transaction).await?;
    let decision = pc_identity::resolve(mac, source_ip, &rows);
    let Some(pc_id) = apply_identity_decision_in_transaction(transaction, &decision, mac).await?
    else {
        return Ok(None);
    };
    let prior_status: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = ?")
        .bind(&pc_id)
        .fetch_one(&mut **transaction)
        .await?;
    // The Guard assignment has two static shapes: a bound value (a reported
    // outcome, or NULL for an omission reset), or a self-assignment that
    // leaves the stored diagnostic untouched for body-less keepalives.
    let guard_assignment = match guard_coverage {
        GuardCoverageWrite::Report(_) | GuardCoverageWrite::ResetUnknown => "?",
        GuardCoverageWrite::Leave => "last_guard_coverage",
    };
    let sql = format!(
        "UPDATE pcs
         SET status = CASE WHEN remaining_seconds > 0 THEN 'in_use' ELSE 'online' END,
             last_seen_at = CURRENT_TIMESTAMP,
             clock_difference_ms = COALESCE(?, clock_difference_ms),
             last_guard_coverage = {guard_assignment},
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ?"
    );
    let mut update = sqlx::query(&sql).bind(clock_difference_ms);
    match guard_coverage {
        GuardCoverageWrite::Report(value) => {
            update = update.bind(Some(value));
        }
        GuardCoverageWrite::ResetUnknown => {
            update = update.bind(Option::<&str>::None);
        }
        GuardCoverageWrite::Leave => {}
    }
    update.bind(&pc_id).execute(&mut **transaction).await?;
    if prior_status == "offline" {
        append_reachability_event(transaction, "pc_came_online", &pc_id, "heartbeat").await?;
    }

    Ok(Some(IdentityHeartbeatMutation { pc_id }))
}

/// Heartbeat that resolves identity by `(mac, source_ip)` and returns only
/// the authoritative PC for callers that do not need the previous heartbeat.
pub async fn heartbeat_by_identity(
    pool: &SqlitePool,
    mac: Option<&str>,
    source_ip: &str,
    hostname: &str,
    client_version: &str,
) -> AppResult<Option<Pc>> {
    let mut transaction = super::begin_immediate(pool).await?;
    let Some(heartbeat) = heartbeat_by_identity_in_transaction(
        &mut transaction,
        mac,
        source_ip,
        hostname,
        client_version,
        None,
        GuardCoverageWrite::Leave,
    )
    .await?
    else {
        return Ok(None);
    };
    transaction.commit().await?;
    pc(pool, &heartbeat.pc_id).await.map(Some)
}

/// Disconnect a PC resolved by `(mac, source_ip)` (ADR-0031). Returns
/// the resolved PC when the machine is known, or `None` on rejection.
pub async fn disconnect_pc_by_identity(
    pool: &SqlitePool,
    mac: Option<&str>,
    source_ip: &str,
) -> AppResult<Option<Pc>> {
    let mut transaction = super::begin_immediate(pool).await?;
    let rows = pc_identity_rows_in_transaction(&mut transaction).await?;
    let decision = pc_identity::resolve(mac, source_ip, &rows);
    let Some(pc_id) =
        apply_identity_decision_in_transaction(&mut transaction, &decision, mac).await?
    else {
        return Ok(None);
    };
    let prior_status: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = ?")
        .bind(&pc_id)
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::query(
        "UPDATE pcs
         SET status = 'offline', updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(&pc_id)
    .execute(&mut *transaction)
    .await?;
    if prior_status == "online" || prior_status == "in_use" {
        append_reachability_event(&mut transaction, "pc_went_offline", &pc_id, "disconnect")
            .await?;
    }
    transaction.commit().await?;
    pc(pool, &pc_id).await.map(Some)
}

/// Mark a PC offline by `pc_id` (test/admin helper, ADR-0031). The
/// client-facing disconnect endpoint resolves identity first and goes
/// through `disconnect_pc_by_identity` instead.
#[cfg(test)]
pub async fn disconnect_pc(pool: &SqlitePool, pc_id: &str) -> AppResult<Pc> {
    let mut transaction = super::begin_immediate(pool).await?;
    let prior_status: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    sqlx::query(
        "UPDATE pcs
         SET status = 'offline', updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(pc_id)
    .execute(&mut *transaction)
    .await?;
    if prior_status == "online" || prior_status == "in_use" {
        append_reachability_event(&mut transaction, "pc_went_offline", pc_id, "disconnect").await?;
    }
    transaction.commit().await?;
    pc(pool, pc_id).await
}

pub async fn pc(pool: &SqlitePool, pc_id: &str) -> AppResult<Pc> {
    let shutdown_grace = super::settings::shutdown_grace_seconds(pool).await?;
    let mut pc = sqlx::query_as::<_, (String, String, String, i64, Option<String>, Option<i64>)>(
        "SELECT id, name, status, remaining_seconds, last_seen_at,
                clock_difference_ms
         FROM pcs WHERE id = ?",
    )
    .bind(pc_id)
    .fetch_optional(pool)
    .await?
    .map(Pc::from)
    .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    pc.shutdown_grace_seconds = shutdown_grace;
    Ok(pc)
}

/// Resolve a registered PC id from a MAC address, read-only (ADR-0077).
///
/// The Guard Poll uses this instead of the heartbeat identity path: a Guard
/// must never create, bind, or rebind a `pcs` row. An unknown MAC simply has
/// no PC, and its poll receives no order.
pub async fn pc_id_by_mac(pool: &SqlitePool, mac: &str) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT id FROM pcs WHERE mac_address = ?")
            .bind(mac.trim().to_lowercase())
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn pc_is_visible(pool: &SqlitePool, pc_id: &str) -> AppResult<bool> {
    sqlx::query_scalar::<_, i64>("SELECT visible FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(pool)
        .await?
        .map(|visible| visible == 1)
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))
}

/// Heartbeats older than this read as Client-unreachable; the same window
/// `tick_guest_pcs` uses to flip status offline (ADR-0076 "Client
/// Reachability" is the heartbeat verdict, never the ICMP probe).
const HEARTBEAT_STALE_SECONDS: i64 = 15;

fn heartbeat_stale_modifier() -> String {
    format!("-{HEARTBEAT_STALE_SECONDS} seconds")
}

/// One PC row as a PC Monitoring manual check sees it: identity, visibility,
/// the normalized server-owned registered address, the current Client
/// Reachability verdict computed read-only from `status` + `last_seen_at`,
/// and the last Guard coverage diagnostic a Heartbeat reported (NULL until
/// a Client that reports the field is seen). Never mutates PC state.
#[derive(Debug)]
pub(crate) struct PcMonitoringRow {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub registered_ip: Option<String>,
    pub client_reachable: bool,
    pub guard_coverage: Option<String>,
}

const PC_MONITORING_PROJECTION: &str = "SELECT id, name, visible,
                NULLIF(TRIM(registered_ip), ''),
                status <> 'offline'
                  AND last_seen_at IS NOT NULL
                  AND datetime(last_seen_at) >= datetime('now', ?),
                last_guard_coverage
             FROM pcs";

fn map_pc_monitoring_row(
    row: (String, String, i64, Option<String>, bool, Option<String>),
) -> PcMonitoringRow {
    PcMonitoringRow {
        id: row.0,
        name: row.1,
        visible: row.2 == 1,
        registered_ip: row.3,
        client_reachable: row.4,
        guard_coverage: row.5,
    }
}

/// Fetch one PC for a manual Check now by exact id. `None` means the PC does
/// not exist; hidden and addressless PCs still come back so the check can
/// report them explicitly unavailable.
pub(crate) async fn pc_monitoring_row(
    pool: &SqlitePool,
    pc_id: &str,
) -> AppResult<Option<PcMonitoringRow>> {
    let pc_id = pc_id.trim();
    if pc_id.is_empty() {
        return Err(AppError::InvalidInput("PC ID is required".to_owned()));
    }
    Ok(
        sqlx::query_as::<_, (String, String, i64, Option<String>, bool, Option<String>)>(&format!(
            "{PC_MONITORING_PROJECTION} WHERE id = ?"
        ))
        // Statement order: the staleness window inside the projection comes
        // before the id predicate.
        .bind(heartbeat_stale_modifier())
        .bind(pc_id)
        .fetch_optional(pool)
        .await?
        .map(map_pc_monitoring_row),
    )
}

/// Every visible PC with a non-blank server-owned `registered_ip`, in display
/// order — the exact target set of a Check-now-all run.
pub(crate) async fn eligible_pc_monitoring_rows(
    pool: &SqlitePool,
) -> AppResult<Vec<PcMonitoringRow>> {
    Ok(
        sqlx::query_as::<_, (String, String, i64, Option<String>, bool, Option<String>)>(&format!(
            "{PC_MONITORING_PROJECTION}
             WHERE visible = 1 AND NULLIF(TRIM(registered_ip), '') IS NOT NULL
             ORDER BY name"
        ))
        .bind(heartbeat_stale_modifier())
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(map_pc_monitoring_row)
        .collect(),
    )
}

#[cfg(test)]
pub async fn tick_pcs(pool: &SqlitePool) -> AppResult<Vec<Pc>> {
    tick_guest_pcs(pool, &[]).await
}

pub async fn tick_guest_pcs(pool: &SqlitePool, active_pc_ids: &[String]) -> AppResult<Vec<Pc>> {
    let mut transaction = super::begin_immediate(pool).await?;
    let mut changed =
        sqlx::query_as::<_, (String, String, String, i64, Option<String>, Option<i64>)>(
            "UPDATE pcs
         SET status = 'offline', updated_at = CURRENT_TIMESTAMP
         WHERE status IN ('online', 'in_use')
           AND (
             last_seen_at IS NULL
             OR datetime(last_seen_at) < datetime('now', ?)
           )
         RETURNING id, name, status, remaining_seconds, last_seen_at,
                   clock_difference_ms",
        )
        .bind(heartbeat_stale_modifier())
        .fetch_all(&mut *transaction)
        .await?;
    for (id, _, _, _, _, _) in &changed {
        append_reachability_event(&mut transaction, "pc_went_offline", id, "stale_heartbeat")
            .await?;
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "UPDATE pcs
         SET remaining_seconds = MAX(remaining_seconds - 1, 0),
             status = CASE WHEN remaining_seconds - 1 > 0 THEN 'in_use' ELSE 'online' END,
             updated_at = CURRENT_TIMESTAMP
         WHERE remaining_seconds > 0
           AND status IN ('online', 'in_use')",
    );
    if !active_pc_ids.is_empty() {
        query.push(" AND id NOT IN (");
        let mut separated = query.separated(", ");
        for pc_id in active_pc_ids {
            separated.push_bind(pc_id);
        }
        separated.push_unseparated(")");
    }
    query.push(
        " RETURNING id, name, status, remaining_seconds, last_seen_at,\n                  clock_difference_ms",
    );
    let ticked = query
        .build_query_as::<(String, String, String, i64, Option<String>, Option<i64>)>()
        .fetch_all(&mut *transaction)
        .await?;
    changed.extend(ticked);
    transaction.commit().await?;
    let shutdown_grace = super::settings::shutdown_grace_seconds(pool).await?;
    let mut changed: Vec<Pc> = changed.into_iter().map(Pc::from).collect();
    super::settings::apply_shutdown_grace(&mut changed, shutdown_grace);
    Ok(changed)
}

#[cfg(test)]
mod tests {
    // Reachability events are Silent but retained as the canonical server-visible offline signal.
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    #[tokio::test]
    async fn heartbeat_marks_pc_online_and_tick_decrements_paid_time() -> AppResult<()> {
        let pool = test_pool().await?;
        crate::db::simulate_coin(&pool, "pc-01", 100).await?;

        let heartbeat_pc = heartbeat_by_identity_only(&pool, "pc-01").await?;
        assert_eq!(heartbeat_pc.status, "in_use");
        assert_eq!(heartbeat_pc.remaining_seconds, 300);

        tick_pcs(&pool).await?;
        let pc = pc(&pool, "pc-01").await?;
        assert_eq!(pc.status, "in_use");
        assert_eq!(pc.remaining_seconds, 299);
        Ok(())
    }

    #[tokio::test]
    async fn reachability_events_emit_once_per_status_edge() -> AppResult<()> {
        let pool = test_pool().await?;
        let first = heartbeat_by_identity_only(&pool, "pc-01").await?;
        assert_eq!(first.status, "online");
        heartbeat_by_identity_only(&pool, "pc-01").await?;
        disconnect_pc(&pool, "pc-01").await?;
        disconnect_pc(&pool, "pc-01").await?;
        heartbeat_by_identity_only(&pool, "pc-01").await?;

        let came_online: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_came_online' AND pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await?;
        let went_offline: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_went_offline' AND pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(came_online, 2);
        assert_eq!(went_offline, 1);
        Ok(())
    }

    #[tokio::test]
    async fn reachability_audit_failure_rolls_back_heartbeat_status() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "CREATE TRIGGER fail_pc_online
             BEFORE INSERT ON audit_log
             WHEN NEW.event_type = 'pc_came_online'
             BEGIN SELECT RAISE(ABORT, 'test audit failure'); END",
        )
        .execute(&pool)
        .await?;

        let error = heartbeat_by_identity_only(&pool, "pc-01").await;
        assert!(error.is_err());
        let status: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = 'pc-01'")
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "offline");
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_came_online'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_marks_pc_offline_and_stops_paid_time() -> AppResult<()> {
        let pool = test_pool().await?;
        crate::db::simulate_coin(&pool, "pc-01", 100).await?;
        heartbeat_by_identity_only(&pool, "pc-01").await?;

        let disconnected = disconnect_pc(&pool, "pc-01").await?;
        assert_eq!(disconnected.status, "offline");
        assert_eq!(disconnected.remaining_seconds, 300);

        tick_pcs(&pool).await?;
        let pc = pc(&pool, "pc-01").await?;
        assert_eq!(pc.status, "offline");
        assert_eq!(pc.remaining_seconds, 300);
        Ok(())
    }

    #[tokio::test]
    async fn stale_pc_goes_offline_without_consuming_time() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "UPDATE pcs
             SET status = 'in_use',
                 remaining_seconds = 300,
                 last_seen_at = datetime('now', '-16 seconds')
             WHERE id = 'pc-01'",
        )
        .execute(&pool)
        .await?;

        tick_pcs(&pool).await?;
        let pc = pc(&pool, "pc-01").await?;
        assert_eq!(pc.status, "offline");
        assert_eq!(pc.remaining_seconds, 300);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'pc_went_offline' AND pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 1);
        tick_pcs(&pool).await?;
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'pc_went_offline' AND pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn pc_settings_rename_and_visibility_are_transactional() -> AppResult<()> {
        let pool = test_pool().await?;
        let updated = update_pc_settings(
            &pool,
            "pc-01",
            &PcSettingsInput {
                name: "Counter One".to_owned(),
                visible: false,
                registered_ip: None,
                mac_address: None,
            },
        )
        .await?;
        assert_eq!(updated.name, "Counter One");
        assert!(!updated.visible);
        assert!(!pc_is_visible(&pool, "pc-01").await?);
        assert!(pcs(&pool).await?.iter().all(|pc| pc.id != "pc-01"));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn manual_add_time_clamps_at_zero_and_records_applied_delta() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE pcs SET remaining_seconds = 100 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;

        let adjustment = create_manual_adjustment(
            &pool,
            &ManualAdjustmentInput {
                pc_id: "pc-01".to_owned(),
                seconds_delta: -300,
                reason: Some("overpaid correction".to_owned()),
            },
        )
        .await?;

        assert_eq!(adjustment.requested_seconds_delta, -300);
        assert_eq!(adjustment.seconds_delta, -100);
        assert_eq!(adjustment.pc_name, "PC 01");
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 0);

        let audit: (i64, i64) = sqlx::query_as(
            "SELECT seconds_delta, cash_flag FROM audit_log
             WHERE event_type = 'manual_adjustment'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit, (-100, 0));
        let detail: (i64, Option<String>) =
            sqlx::query_as("SELECT seconds_delta, reason FROM manual_adjustments")
                .fetch_one(&pool)
                .await?;
        assert_eq!(detail, (-100, Some("overpaid correction".to_owned())));
        Ok(())
    }

    #[tokio::test]
    async fn manual_add_time_rejects_zero_delta_and_unknown_pc() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            create_manual_adjustment(
                &pool,
                &ManualAdjustmentInput {
                    pc_id: "pc-01".to_owned(),
                    seconds_delta: 0,
                    reason: None,
                },
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            create_manual_adjustment(
                &pool,
                &ManualAdjustmentInput {
                    pc_id: "pc-missing".to_owned(),
                    seconds_delta: 60,
                    reason: None,
                },
            )
            .await,
            Err(AppError::NotFound(_))
        ));
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'manual_adjustment'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn manual_subtract_time_refuses_when_pc_has_no_guest_time() -> AppResult<()> {
        let pool = test_pool().await?;
        // pc-01 starts with 0 remaining_seconds; subtracting any amount would
        // clamp to zero applied delta and must be rejected without writing an
        // audit event.
        assert!(matches!(
            create_manual_adjustment(
                &pool,
                &ManualAdjustmentInput {
                    pc_id: "pc-01".to_owned(),
                    seconds_delta: -60,
                    reason: None,
                },
            )
            .await,
            Err(AppError::Conflict(_))
        ));
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'manual_adjustment'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn manual_add_time_writes_one_linked_audit_and_detail_row() -> AppResult<()> {
        let pool = test_pool().await?;
        create_manual_adjustment(
            &pool,
            &ManualAdjustmentInput {
                pc_id: "pc-01".to_owned(),
                seconds_delta: 300,
                reason: Some("promo top-up".to_owned()),
            },
        )
        .await?;

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'manual_adjustment'",
        )
        .fetch_one(&pool)
        .await?;
        let detail_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM manual_adjustments")
            .fetch_one(&pool)
            .await?;
        assert_eq!(audit_count, 1);
        assert_eq!(detail_count, 1);
        let linked: Option<String> =
            sqlx::query_scalar("SELECT audit_event_id FROM manual_adjustments")
                .fetch_one(&pool)
                .await?;
        assert!(linked.is_some());
        Ok(())
    }
}

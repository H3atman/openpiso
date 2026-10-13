//! Durable PC Monitoring state (ADR-0076): the shop-wide enabled flag and the
//! `pc_monitoring_incidents` table.
//!
//! Every incident row change commits in the same SQLite transaction as its
//! generic Audit event — opening appends a `pc_monitoring_incident_opened`
//! Alert, closure a `pc_monitoring_incident_closed` Notice. The fixed
//! classifier (ADR-0058) owns those severities; the kind stays typed incident
//! data and never forks the event name.
//!
//! Incident paths are detect-and-report only: nothing here touches PC status,
//! sessions, money, Remaining Time, or Points.
//!
//! Per-PC **Monitoring Suppression** (#342) lives here too: one durable row
//! per suppressed PC with a server-computed absolute expiry. Suppression only
//! blocks new incident openings; it never closes or erases an episode that is
//! already open.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    audit::{self, NewAuditEvent},
    db::telegram::{
        compose_closure, compose_monitoring_disabled, compose_opening, compose_resolved,
        NewOutboxItem,
    },
    errors::AppResult,
};

/// Settings key behind the shop-wide PC Monitoring toggle. Absent means
/// disabled: an upgraded Admin never starts monitoring by surprise.
pub const SETTINGS_KEY_ENABLED: &str = "pc_monitoring_enabled";

/// Settings key behind Client Recovery Escalation (ADR-0077). Absent means
/// disabled: OpenPiso is never authorised to power off customer PCs by an
/// upgrade.
pub const SETTINGS_KEY_ESCALATION_ENABLED: &str = "client_recovery_escalation_enabled";

/// Settings key behind the owner-set Escalation Delay in whole seconds.
pub const SETTINGS_KEY_ESCALATION_DELAY: &str = "client_recovery_escalation_delay_secs";

/// Default Escalation Delay: the Guard must be visibly failing to recover the
/// Client for this long before the PC is powered off.
pub const DEFAULT_ESCALATION_DELAY_SECS: i64 = 60;

/// Hard floor for the Escalation Delay. Below this the decision rests on too
/// little evidence to justify turning a customer's PC off.
pub const MIN_ESCALATION_DELAY_SECS: i64 = 30;

/// Upper bound for the Escalation Delay. A value beyond this is an owner who
/// meant to disable the feature.
pub const MAX_ESCALATION_DELAY_SECS: i64 = 3_600;

/// One Escalated Power Off per PC per hour (ADR-0077). A diskless PC that
/// reboots straight back into the same broken state would otherwise cycle all
/// night on the escalation clock.
pub const ESCALATION_COOLDOWN_MINUTES: i64 = 60;

/// What kind of degraded coverage the episode records. All kinds share one
/// incident model (ADR-0076); #341 opened the first kind, and #343 adds
/// Host Probe Coverage. Client Guard Coverage stays reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKind {
    ClientReachabilityMismatch,
    HostProbeCoverage,
    ClientGuardCoverage,
}

impl IncidentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            IncidentKind::ClientReachabilityMismatch => "client_reachability_mismatch",
            IncidentKind::HostProbeCoverage => "host_probe_coverage",
            IncidentKind::ClientGuardCoverage => "client_guard_coverage",
        }
    }

    /// Owner-facing name used in Audit summaries and dashboard badges.
    pub fn label(&self) -> &'static str {
        match self {
            IncidentKind::ClientReachabilityMismatch => "Client Reachability Mismatch",
            IncidentKind::HostProbeCoverage => "Host Probe Coverage",
            IncidentKind::ClientGuardCoverage => "Client Guard Coverage",
        }
    }
}

/// Why an episode ended. Recovery needs positive evidence — an accepted
/// Client heartbeat (`client_recovered`), for Host Probe Coverage one
/// successful probe (`probe_recovered`), or for Client Guard Coverage the
/// same diagnostic reporting covered again (`guard_recovered`); anything
/// else closes as `host_lost` — recovery unconfirmed — or as
/// `monitoring_disabled`, never as recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentCloseReason {
    ClientRecovered,
    ProbeRecovered,
    GuardRecovered,
    HostLost,
    MonitoringDisabled,
    /// Client Recovery Escalation powered the PC off (ADR-0077). The host
    /// stops answering ICMP immediately afterwards, and closing that as
    /// `host_lost` would blame the machine for something we did.
    EscalatedPowerOff,
}

impl IncidentCloseReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            IncidentCloseReason::ClientRecovered => "client_recovered",
            IncidentCloseReason::ProbeRecovered => "probe_recovered",
            IncidentCloseReason::GuardRecovered => "guard_recovered",
            IncidentCloseReason::HostLost => "host_lost",
            IncidentCloseReason::MonitoringDisabled => "monitoring_disabled",
            IncidentCloseReason::EscalatedPowerOff => "escalated_power_off",
        }
    }

    /// Truthful reason copy shared by the closure Notice and the Telegram
    /// delivery message. The evidence that ended the episode depends on its
    /// kind: a mismatch ends when the host's answer or the heartbeat verdict
    /// changes; a coverage episode ends when probes answer again or when the
    /// heartbeat stops proving the host is running.
    fn phrase(&self, kind: IncidentKind) -> &'static str {
        match self {
            IncidentCloseReason::ClientRecovered => "Client recovered",
            IncidentCloseReason::ProbeRecovered => "host answered probes again",
            IncidentCloseReason::GuardRecovered => "Guard coverage restored",
            IncidentCloseReason::MonitoringDisabled => "monitoring disabled",
            IncidentCloseReason::EscalatedPowerOff => {
                "Admin powered the PC off after failed Client relaunches"
            }
            IncidentCloseReason::HostLost => match kind {
                IncidentKind::ClientReachabilityMismatch => {
                    "host stopped responding; Client recovery unconfirmed"
                }
                // Coverage loss and Guard loss both rest on the heartbeat
                // proving the host is running; when that proof goes, whether
                // coverage returned is unconfirmed.
                IncidentKind::HostProbeCoverage | IncidentKind::ClientGuardCoverage => {
                    "Client Reachability no longer proves the host is running"
                }
            },
        }
    }

    /// Truthful copy for the closure Notice.
    fn closure_summary(&self, pc_name: &str, kind: IncidentKind) -> String {
        format!(
            "{} on {pc_name} closed: {}",
            kind.label(),
            self.phrase(kind)
        )
    }
}

/// One active Monitoring Suppression as the owner surface sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonitoringSuppression {
    pub pc_id: String,
    /// Absolute expiry instant (RFC 3339 UTC), computed by Admin at creation.
    pub expires_at: String,
}

/// One durable incident episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PcMonitoringIncident {
    pub id: String,
    pub pc_id: String,
    pub kind: IncidentKind,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<IncidentCloseReason>,
    /// True once Admin restart interrupted continuous observation.
    pub observation_interrupted: bool,
}

fn parse_kind(value: &str) -> IncidentKind {
    match value {
        "host_probe_coverage" => IncidentKind::HostProbeCoverage,
        "client_guard_coverage" => IncidentKind::ClientGuardCoverage,
        _ => IncidentKind::ClientReachabilityMismatch,
    }
}

fn parse_close_reason(value: &str) -> Option<IncidentCloseReason> {
    match value {
        "host_lost" => Some(IncidentCloseReason::HostLost),
        "monitoring_disabled" => Some(IncidentCloseReason::MonitoringDisabled),
        "client_recovered" => Some(IncidentCloseReason::ClientRecovered),
        "probe_recovered" => Some(IncidentCloseReason::ProbeRecovered),
        "guard_recovered" => Some(IncidentCloseReason::GuardRecovered),
        "escalated_power_off" => Some(IncidentCloseReason::EscalatedPowerOff),
        _ => None,
    }
}

/// `(id, pc_id, kind, opened_at, closed_at, close_reason, observation_interrupted)`
type IncidentRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
);
/// Same shape plus the display name joined from `pcs`.
type IncidentCloseRow = (String, String, String, String, String, i64);

const INCIDENT_COLUMNS: &str =
    "id, pc_id, kind, opened_at, closed_at, close_reason, observation_interrupted";

fn map_incident(row: IncidentRow) -> PcMonitoringIncident {
    PcMonitoringIncident {
        id: row.0,
        pc_id: row.1,
        kind: parse_kind(&row.2),
        opened_at: row.3,
        closed_at: row.4,
        close_reason: row.5.as_deref().and_then(parse_close_reason),
        observation_interrupted: row.6 == 1,
    }
}

// ---------------------------------------------------------------------------
// Per-PC Monitoring Suppression (#342)
// ---------------------------------------------------------------------------

/// PCs whose Monitoring Suppression is still active at `now`. The one shared
/// gate every incident kind passes through before an opening may commit.
pub async fn active_suppression_pcs(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> AppResult<HashSet<String>> {
    Ok(active_suppressions(pool, now)
        .await?
        .into_iter()
        .map(|suppression| suppression.pc_id)
        .collect())
}

/// Active suppressions keyed by PC id for dashboard display. The value is the
/// absolute RFC 3339 UTC expiry exactly as stored.
pub async fn suppressions_by_pc(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> AppResult<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for suppression in active_suppressions(pool, now).await? {
        map.insert(suppression.pc_id, suppression.expires_at);
    }
    Ok(map)
}

/// Every suppression whose absolute expiry is still in the future. Expired
/// rows simply stop matching — lazily expiring keeps restarts and clock-free
/// ticks from ever extending a forgotten suppression.
async fn active_suppressions(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> AppResult<Vec<MonitoringSuppression>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT pc_id, expires_at FROM pc_monitoring_suppressions")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(pc_id, expires_at)| {
            let expiry = DateTime::parse_from_rfc3339(&expires_at).ok()?;
            (expiry > now).then_some(MonitoringSuppression { pc_id, expires_at })
        })
        .collect())
}

/// Create or replace the Monitoring Suppression for one PC, committing the
/// row and its Notice audit event in one transaction. `expires_at` must be a
/// server-computed absolute instant; callers validate duration policy.
pub async fn set_suppression_with_audit(
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
    duration_minutes: i64,
    now: DateTime<Utc>,
) -> AppResult<String> {
    let expires_at = now + chrono::Duration::minutes(duration_minutes);
    let expires_at_text = expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut transaction = super::begin_immediate(pool).await?;
    // Distinguish create from replace for the audit record. SQLite's
    // rows_affected stays 1 for both arms of an upsert, so ask directly.
    let replaced: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pc_monitoring_suppressions WHERE pc_id = ?)",
    )
    .bind(pc_id)
    .fetch_one(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO pc_monitoring_suppressions (pc_id, expires_at)
         VALUES (?, ?)
         ON CONFLICT(pc_id) DO UPDATE SET
           expires_at = excluded.expires_at,
           created_at = CURRENT_TIMESTAMP",
    )
    .bind(pc_id)
    .bind(&expires_at_text)
    .execute(&mut *transaction)
    .await?;

    let mut event = NewAuditEvent::simple(
        "pc_monitoring_suppression_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        format!(
            "Monitoring Suppression on {pc_name} {}: expires {expires_at_text}",
            if replaced { "replaced" } else { "set" }
        ),
    );
    event.pc_id = Some(pc_id);
    event.metadata = serde_json::json!({
        "action": if replaced { "replaced" } else { "created" },
        "duration_minutes": duration_minutes,
        "expires_at": expires_at_text,
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(expires_at_text)
}

/// Cancel the Monitoring Suppression for one PC, committing the deletion and
/// its Notice audit event in one transaction. Cancelling with no active row
/// writes nothing and returns `false` so repeated clicks stay harmless.
pub async fn cancel_suppression_with_audit(
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
) -> AppResult<bool> {
    let mut transaction = super::begin_immediate(pool).await?;
    let deleted = sqlx::query("DELETE FROM pc_monitoring_suppressions WHERE pc_id = ?")
        .bind(pc_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Ok(false);
    }
    let mut event = NewAuditEvent::simple(
        "pc_monitoring_suppression_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        format!("Monitoring Suppression on {pc_name} cancelled"),
    );
    event.pc_id = Some(pc_id);
    event.metadata = serde_json::json!({ "action": "cancelled" });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Shop-wide enabled flag
// ---------------------------------------------------------------------------

/// Shop-wide PC Monitoring state. Absent settings row means disabled, so an
/// upgrade never enables monitoring without an owner decision (ADR-0076).
pub async fn enabled(pool: &SqlitePool) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTINGS_KEY_ENABLED)
            .fetch_optional(pool)
            .await?
            .is_some_and(|value| value == "true"),
    )
}

// ---------------------------------------------------------------------------
// Incidents
// ---------------------------------------------------------------------------

/// Every still-open incident, any PC, any kind. Drives restart marking,
/// disable-time closures, and the monitor loop's idempotence checks.
pub async fn open_incidents(pool: &SqlitePool) -> AppResult<Vec<PcMonitoringIncident>> {
    Ok(sqlx::query_as::<_, IncidentRow>(&format!(
        "SELECT {INCIDENT_COLUMNS}
         FROM pc_monitoring_incidents WHERE closed_at IS NULL ORDER BY opened_at"
    ))
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(map_incident)
    .collect())
}

/// Active incident kinds per PC id for dashboard badges. Badges reflect the
/// durable open episodes, independent of Notification acknowledgement.
pub async fn active_kinds_by_pc(
    pool: &SqlitePool,
) -> AppResult<BTreeMap<String, Vec<IncidentKind>>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT pc_id, kind FROM pc_monitoring_incidents
         WHERE closed_at IS NULL ORDER BY pc_id, kind",
    )
    .fetch_all(pool)
    .await?;
    let mut map = BTreeMap::new();
    for (pc_id, kind) in rows {
        map.entry(pc_id)
            .or_insert_with(Vec::new)
            .push(parse_kind(&kind));
    }
    Ok(map)
}

/// Open one incident for (PC, kind) if none is open, committing the row and
/// its Alert audit event in one transaction. Repeated calls while the episode
/// persists are idempotent: the in-transaction existence check plus the unique
/// partial index return `Ok(None)` instead of producing a second incident or
/// Alert.
pub async fn open_incident_with_audit(
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
    kind: IncidentKind,
) -> AppResult<Option<String>> {
    open_incident_with_audit_at(pool, pc_id, pc_name, kind, Utc::now()).await
}

/// Testable form of [`open_incident_with_audit`] with an explicit timestamp.
/// Production callers use the server clock wrapper above.
pub async fn open_incident_with_audit_at(
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    now: DateTime<Utc>,
) -> AppResult<Option<String>> {
    let mut transaction = super::begin_immediate(pool).await?;
    let already_open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pc_monitoring_incidents
         WHERE pc_id = ? AND kind = ? AND closed_at IS NULL",
    )
    .bind(pc_id)
    .bind(kind.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    if already_open > 0 {
        return Ok(None);
    }

    let mut event = NewAuditEvent::simple(
        "pc_monitoring_incident_opened",
        AuditOutcome::Success,
        AuditActor::System,
        format!("{} detected on {}", kind.label(), pc_name),
    );
    event.pc_id = Some(pc_id);
    event.metadata = serde_json::json!({ "kind": kind.as_str() });
    let audit_id = audit::append(&mut transaction, event).await?;

    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO pc_monitoring_incidents
             (id, pc_id, kind, opened_at, open_audit_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(pc_id)
    .bind(kind.as_str())
    .bind(super::telegram::format_rfc3339(now))
    .bind(&audit_id)
    .execute(&mut *transaction)
    .await?;
    // Read back what CURRENT_TIMESTAMP stamped so the delivery item carries
    // the exact committed opening instant.
    let opened_at: String =
        sqlx::query_scalar("SELECT opened_at FROM pc_monitoring_incidents WHERE id = ?")
            .bind(&id)
            .fetch_one(&mut *transaction)
            .await?;
    enqueue_opening_delivery(&mut transaction, &id, pc_name, kind, &opened_at, now).await?;
    transaction.commit().await?;
    Ok(Some(id))
}

/// Close the open incident for one PC and kind with the given reason,
/// committing the row update and its Notice audit event in one transaction.
/// Returns the closed incident id, or `None` when nothing was open.
pub async fn close_incident_with_audit(
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    reason: IncidentCloseReason,
) -> AppResult<Option<String>> {
    close_incident_with_audit_at(pool, pc_id, pc_name, kind, reason, Utc::now()).await
}

/// Testable form of [`close_incident_with_audit`] with an explicit timestamp.
pub async fn close_incident_with_audit_at(
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    reason: IncidentCloseReason,
    now: DateTime<Utc>,
) -> AppResult<Option<String>> {
    let mut transaction = super::begin_immediate(pool).await?;
    let open: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, opened_at, observation_interrupted FROM pc_monitoring_incidents
         WHERE pc_id = ? AND kind = ? AND closed_at IS NULL",
    )
    .bind(pc_id)
    .bind(kind.as_str())
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((open_id, opened_at, interrupted)) = open else {
        return Ok(None);
    };
    close_in_transaction(&mut transaction, &open_id, reason, now).await?;
    append_closure_event(&mut transaction, &open_id, pc_id, pc_name, kind, reason).await?;
    let closed_at = closed_at_in_transaction(&mut transaction, &open_id).await?;
    enqueue_closure_delivery(
        &mut transaction,
        &open_id,
        pc_name,
        kind,
        reason,
        &opened_at,
        &closed_at,
        interrupted == 1,
        now,
    )
    .await?;
    transaction.commit().await?;
    Ok(Some(open_id))
}

/// Disable shop-wide monitoring and close every still-open incident as
/// `monitoring_disabled` — setting flip, all closure rows, and all their
/// Notice audit events commit atomically. Historical Alerts remain untouched.
/// Returns how many incidents were closed.
pub async fn disable_and_close_active(pool: &SqlitePool) -> AppResult<usize> {
    let mut transaction = super::begin_immediate(pool).await?;
    set_enabled_in_transaction(&mut transaction, false).await?;

    // Join the display name now so each closure summary is written from one
    // consistent read inside this same transaction.
    let open: Vec<IncidentCloseRow> = sqlx::query_as(
        "SELECT i.id, i.pc_id, i.kind, p.name, i.opened_at, i.observation_interrupted
         FROM pc_monitoring_incidents i
         JOIN pcs p ON p.id = i.pc_id
         WHERE i.closed_at IS NULL
         ORDER BY i.opened_at",
    )
    .fetch_all(&mut *transaction)
    .await?;

    let now = Utc::now();
    let closed_count = open.len();
    for (id, pc_id, kind, pc_name, opened_at, interrupted) in open {
        let kind = parse_kind(&kind);
        close_in_transaction(
            &mut transaction,
            &id,
            IncidentCloseReason::MonitoringDisabled,
            now,
        )
        .await?;
        append_closure_event(
            &mut transaction,
            &id,
            &pc_id,
            &pc_name,
            kind,
            IncidentCloseReason::MonitoringDisabled,
        )
        .await?;
        let closed_at = closed_at_in_transaction(&mut transaction, &id).await?;
        enqueue_closure_delivery(
            &mut transaction,
            &id,
            &pc_name,
            kind,
            IncidentCloseReason::MonitoringDisabled,
            &opened_at,
            &closed_at,
            interrupted == 1,
            now,
        )
        .await?;
    }
    append_settings_event(&mut transaction, false).await?;
    // One redacted state message for the shop-wide flip (ADR-0076), enqueued
    // only when Telegram is connected, in this same transaction.
    if telegram_connected(&mut transaction).await? {
        super::telegram::enqueue_in_transaction(
            &mut transaction,
            &NewOutboxItem {
                item_class: "closure",
                incident_kind: None,
                message_text: compose_monitoring_disabled(closed_count, now),
                incident_id: None,
            },
            now,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(closed_count)
}

/// Enable shop-wide monitoring, audited. Grace restarts from scratch on the
/// next tick because the monitor clears its in-memory state while disabled.
pub async fn enable(pool: &SqlitePool) -> AppResult<()> {
    let mut transaction = super::begin_immediate(pool).await?;
    set_enabled_in_transaction(&mut transaction, true).await?;
    append_settings_event(&mut transaction, true).await?;
    transaction.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client Recovery Escalation (ADR-0077)
// ---------------------------------------------------------------------------

/// The shop-wide escalation policy as the owner surface sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EscalationSettings {
    pub enabled: bool,
    pub delay_secs: i64,
}

impl Default for EscalationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            delay_secs: DEFAULT_ESCALATION_DELAY_SECS,
        }
    }
}

/// Read the escalation policy. An absent or unreadable row is the default:
/// disabled, at the default delay. A corrupt stored delay never widens the
/// window, because the floor is applied on read as well as on write.
pub async fn escalation_settings(pool: &SqlitePool) -> AppResult<EscalationSettings> {
    let enabled = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(SETTINGS_KEY_ESCALATION_ENABLED)
        .fetch_optional(pool)
        .await?
        .is_some_and(|value| value == "true");
    let delay_secs = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(SETTINGS_KEY_ESCALATION_DELAY)
        .fetch_optional(pool)
        .await?
        .and_then(|value| value.parse::<i64>().ok())
        .map(clamp_escalation_delay)
        .unwrap_or(DEFAULT_ESCALATION_DELAY_SECS);
    Ok(EscalationSettings {
        enabled,
        delay_secs,
    })
}

/// Hold the Escalation Delay inside its policy bounds. The floor is the
/// reason this exists: no stored value, however it got there, may authorise a
/// power off on less evidence than ADR-0077 requires.
pub fn clamp_escalation_delay(delay_secs: i64) -> i64 {
    delay_secs.clamp(MIN_ESCALATION_DELAY_SECS, MAX_ESCALATION_DELAY_SECS)
}

/// Write the escalation policy and its Notice audit event in one transaction.
/// The caller has already verified the current Owner PIN.
pub async fn set_escalation_settings_with_audit(
    pool: &SqlitePool,
    enabled: bool,
    delay_secs: i64,
) -> AppResult<EscalationSettings> {
    let delay_secs = clamp_escalation_delay(delay_secs);
    let mut transaction = super::begin_immediate(pool).await?;
    for (key, value) in [
        (
            SETTINGS_KEY_ESCALATION_ENABLED,
            if enabled {
                "true".to_owned()
            } else {
                "false".to_owned()
            },
        ),
        (SETTINGS_KEY_ESCALATION_DELAY, delay_secs.to_string()),
    ] {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at)
             VALUES (?, ?, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE
               SET value = excluded.value,
                   updated_at = CURRENT_TIMESTAMP",
        )
        .bind(key)
        .bind(value)
        .execute(&mut *transaction)
        .await?;
    }
    let mut event = NewAuditEvent::simple(
        "pc_recovery_escalation_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        if enabled {
            format!("Client Recovery Escalation enabled after {delay_secs}s")
        } else {
            "Client Recovery Escalation disabled".to_owned()
        },
    );
    event.metadata = serde_json::json!({
        "enabled": enabled,
        "delay_secs": delay_secs,
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(EscalationSettings {
        enabled,
        delay_secs,
    })
}

/// PCs still inside their Escalation Cooldown at `now`. Durable rather than
/// in-memory: an Admin restart must not license a second power off inside the
/// same hour.
pub async fn cooling_down_pcs(pool: &SqlitePool, now: DateTime<Utc>) -> AppResult<HashSet<String>> {
    let cutoff = now - chrono::Duration::minutes(ESCALATION_COOLDOWN_MINUTES);
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT pc_id FROM pc_escalation_cooldowns WHERE last_power_off_at > ?")
            .bind(super::telegram::format_rfc3339(cutoff))
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// Record one Escalated Power Off in the caller's authority transaction: the
/// Alert, the truthful closure of the open mismatch, its Telegram item, and
/// the cooldown stamp. The session end is appended by the caller in this same
/// transaction, so a PC is never recorded as powered off without its session
/// ending, and never the reverse.
pub(crate) async fn record_escalated_power_off_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    pc_id: &str,
    pc_name: &str,
    relaunch_attempts: i64,
    nonce: &str,
    now: DateTime<Utc>,
) -> AppResult<Option<String>> {
    let kind = IncidentKind::ClientReachabilityMismatch;
    let reason = IncidentCloseReason::EscalatedPowerOff;
    let open: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, opened_at, observation_interrupted FROM pc_monitoring_incidents
         WHERE pc_id = ? AND kind = ? AND closed_at IS NULL",
    )
    .bind(pc_id)
    .bind(kind.as_str())
    .fetch_optional(&mut **transaction)
    .await?;

    let closed_incident = match open {
        Some((incident_id, opened_at, interrupted)) => {
            close_in_transaction(transaction, &incident_id, reason, now).await?;
            append_closure_event(transaction, &incident_id, pc_id, pc_name, kind, reason).await?;
            let closed_at = closed_at_in_transaction(transaction, &incident_id).await?;
            enqueue_closure_delivery(
                transaction,
                &incident_id,
                pc_name,
                kind,
                reason,
                &opened_at,
                &closed_at,
                interrupted == 1,
                now,
            )
            .await?;
            Some(incident_id)
        }
        None => None,
    };

    let mut event = NewAuditEvent::simple(
        "pc_escalated_power_off",
        AuditOutcome::Success,
        AuditActor::System,
        format!("{pc_name} powered off after {relaunch_attempts} failed Client relaunches"),
    );
    event.pc_id = Some(pc_id);
    event.metadata = serde_json::json!({
        "incident_id": closed_incident,
        "relaunch_attempts": relaunch_attempts,
        "nonce": nonce,
    });
    audit::append(transaction, event).await?;

    sqlx::query(
        "INSERT INTO pc_escalation_cooldowns (pc_id, last_power_off_at)
         VALUES (?, ?)
         ON CONFLICT(pc_id) DO UPDATE SET last_power_off_at = excluded.last_power_off_at",
    )
    .bind(pc_id)
    .bind(super::telegram::format_rfc3339(now))
    .execute(&mut **transaction)
    .await?;

    Ok(closed_incident)
}

/// Mark every still-open incident as observation-interrupted. Called once at
/// Admin startup so display and delivery never claim a downtime span was
/// continuously observed. No new Alerts are produced: the original opening
/// event already exists.
pub async fn mark_observation_interrupted(pool: &SqlitePool) -> AppResult<u64> {
    let result = sqlx::query(
        "UPDATE pc_monitoring_incidents SET observation_interrupted = 1
         WHERE closed_at IS NULL AND observation_interrupted = 0",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Read back what CURRENT_TIMESTAMP stamped for a closure instant, so each
/// delivery item carries the exact committed time.
async fn closed_at_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    incident_id: &str,
) -> AppResult<String> {
    let closed_at: String =
        sqlx::query_scalar("SELECT closed_at FROM pc_monitoring_incidents WHERE id = ?")
            .bind(incident_id)
            .fetch_one(&mut **transaction)
            .await?;
    Ok(closed_at)
}

/// Whether Telegram delivery is configured, read inside the caller's
/// transaction so the enqueue decision sees exactly the configuration the
/// incident change commits against. Disconnected means nothing enqueues and
/// local history remains canonical on its own.
async fn telegram_connected(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> AppResult<bool> {
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_config WHERE id = 1")
        .fetch_one(&mut **transaction)
        .await?;
    Ok(stored > 0)
}

/// Enqueue one opening delivery item in the caller's transaction. A no-op
/// while Telegram is disconnected; a redacted line otherwise.
#[allow(clippy::too_many_arguments)]
async fn enqueue_opening_delivery(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    incident_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    opened_at_text: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    if !telegram_connected(transaction).await? {
        return Ok(());
    }
    // An unstorable instant must never become a fabricated message time; the
    // local Audit event already carries the canonical record either way.
    let Some(opened_at) = super::telegram::parse_db_timestamp(opened_at_text) else {
        return Ok(());
    };
    super::telegram::enqueue_in_transaction(
        transaction,
        &NewOutboxItem {
            item_class: "opening",
            incident_kind: Some(kind.as_str()),
            message_text: compose_opening(pc_name, kind.label(), opened_at),
            incident_id: Some(incident_id.to_owned()),
        },
        now,
    )
    .await
}

/// Enqueue the closure-side delivery item for one closed incident, replacing
/// an undelivered opening with one resolved summary (ADR-0076): if the
/// episode ends before its opening message was sent, the queue carries a
/// single resolved line instead of stale alarm then recovery.
#[allow(clippy::too_many_arguments)]
async fn enqueue_closure_delivery(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    incident_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    reason: IncidentCloseReason,
    opened_at_text: &str,
    closed_at_text: &str,
    observation_interrupted: bool,
    now: DateTime<Utc>,
) -> AppResult<()> {
    if !telegram_connected(transaction).await? {
        return Ok(());
    }
    let (Some(opened_at), Some(closed_at)) = (
        super::telegram::parse_db_timestamp(opened_at_text),
        super::telegram::parse_db_timestamp(closed_at_text),
    ) else {
        return Ok(());
    };
    // `pending` covers never-attempted and retrying items alike; `abandoned`
    // items were never delivered either. Only a confirmed `sent` opening
    // stands: any undelivered opening collapses into one resolved summary
    // that still names when the episode began.
    let undelivered_opening: Option<String> = sqlx::query_scalar(
        "SELECT id FROM telegram_outbox
         WHERE incident_id = ? AND item_class = 'opening' AND status != 'sent'",
    )
    .bind(incident_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let message_text = if undelivered_opening.is_some() {
        compose_resolved(
            pc_name,
            kind.label(),
            opened_at,
            closed_at,
            observation_interrupted,
            reason.phrase(kind),
        )
    } else {
        compose_closure(
            pc_name,
            kind.label(),
            opened_at,
            closed_at,
            observation_interrupted,
            reason.phrase(kind),
        )
    };
    if let Some(opening_id) = undelivered_opening {
        sqlx::query("DELETE FROM telegram_outbox WHERE id = ?")
            .bind(opening_id)
            .execute(&mut **transaction)
            .await?;
    }
    super::telegram::enqueue_in_transaction(
        transaction,
        &NewOutboxItem {
            item_class: "closure",
            incident_kind: Some(kind.as_str()),
            message_text,
            incident_id: Some(incident_id.to_owned()),
        },
        now,
    )
    .await
}

async fn close_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    incident_id: &str,
    reason: IncidentCloseReason,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE pc_monitoring_incidents
         SET closed_at = ?, close_reason = ?
         WHERE id = ? AND closed_at IS NULL",
    )
    .bind(super::telegram::format_rfc3339(now))
    .bind(reason.as_str())
    .bind(incident_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn append_closure_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    incident_id: &str,
    pc_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    reason: IncidentCloseReason,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "pc_monitoring_incident_closed",
        AuditOutcome::Success,
        AuditActor::System,
        reason.closure_summary(pc_name, kind),
    );
    event.pc_id = Some(pc_id);
    event.metadata = serde_json::json!({ "kind": kind.as_str(), "close_reason": reason.as_str() });
    let audit_id = audit::append(transaction, event).await?;
    sqlx::query("UPDATE pc_monitoring_incidents SET close_audit_id = ? WHERE id = ?")
        .bind(&audit_id)
        .bind(incident_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn append_settings_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    enabled_value: bool,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "pc_monitoring_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        if enabled_value {
            "PC Monitoring enabled"
        } else {
            "PC Monitoring disabled"
        },
    );
    event.metadata = serde_json::json!({ "enabled": enabled_value });
    audit::append(transaction, event).await?;
    Ok(())
}

async fn set_enabled_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    value: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES (?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
    )
    .bind(SETTINGS_KEY_ENABLED)
    .bind(if value { "true" } else { "false" })
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    /// Make `pc_id` a visible registered PC with a live Client heartbeat.
    /// Registered IPs are unique per PC, so each id gets its own last octet.
    async fn register_live(pool: &SqlitePool, pc_id: &str) -> AppResult<()> {
        let octet: u8 = pc_id
            .split('-')
            .next_back()
            .and_then(|suffix| suffix.parse().ok())
            .unwrap_or(1);
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: Some(format!("192.168.4.{octet}")),
                mac_address: None,
            },
        )
        .await?;
        crate::db::heartbeat_by_identity_only(pool, pc_id).await?;
        Ok(())
    }

    async fn audit_count(pool: &SqlitePool, event_type: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE event_type = ?")
            .bind(event_type)
            .fetch_one(pool)
            .await
            .expect("audit count")
    }

    async fn open_mismatch(pool: &SqlitePool, pc_id: &str) -> AppResult<Option<String>> {
        open_incident_with_audit(
            pool,
            pc_id,
            &format!("Name {pc_id}"),
            IncidentKind::ClientReachabilityMismatch,
        )
        .await
    }

    async fn close_mismatch(
        pool: &SqlitePool,
        pc_id: &str,
        reason: IncidentCloseReason,
    ) -> AppResult<Option<String>> {
        close_incident_with_audit(
            pool,
            pc_id,
            &format!("Name {pc_id}"),
            IncidentKind::ClientReachabilityMismatch,
            reason,
        )
        .await
    }

    #[tokio::test]
    async fn monitoring_defaults_to_disabled_on_a_fresh_database() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(!enabled(&pool).await?);
        Ok(())
    }

    #[tokio::test]
    async fn enable_and_disable_round_trip_the_setting() -> AppResult<()> {
        let pool = test_pool().await?;
        enable(&pool).await?;
        assert!(enabled(&pool).await?);
        disable_and_close_active(&pool).await?;
        assert!(!enabled(&pool).await?);
        Ok(())
    }

    #[tokio::test]
    async fn open_commits_incident_row_and_alert_audit_together() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;

        let id = open_mismatch(&pool, "pc-01")
            .await?
            .expect("first open opens");

        let incidents = open_incidents(&pool).await?;
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].id, id);
        assert_eq!(incidents[0].kind, IncidentKind::ClientReachabilityMismatch);
        assert_eq!(incidents[0].closed_at, None);
        assert!(!incidents[0].observation_interrupted);
        // One transaction produced both the row and its Alert.
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_opens_stay_idempotent() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;

        open_mismatch(&pool, "pc-01").await?.expect("first");
        for _ in 0..3 {
            let again = open_mismatch(&pool, "pc-01").await?;
            assert_eq!(again, None, "repeat ticks must not open or alert twice");
        }
        assert_eq!(open_incidents(&pool).await?.len(), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);

        // The database backstop holds even without the existence check.
        let direct = sqlx::query(
            "INSERT INTO pc_monitoring_incidents (id, pc_id, kind, open_audit_id)
             VALUES ('forced', 'pc-01', 'client_reachability_mismatch',
               (SELECT id FROM audit_log LIMIT 1))",
        )
        .execute(&pool)
        .await;
        assert!(
            direct.is_err(),
            "unique partial index must reject a second open episode"
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_opens_produce_exactly_one_incident_and_alert() -> AppResult<()> {
        let pool = std::sync::Arc::new(test_pool().await?);
        register_live(&pool, "pc-01").await?;

        let mut handles = Vec::new();
        for _ in 0..4 {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                open_incident_with_audit(
                    &pool,
                    "pc-01",
                    "Name pc-01",
                    IncidentKind::ClientReachabilityMismatch,
                )
                .await
            }));
        }
        let mut opened = 0;
        for handle in handles {
            if handle.await.unwrap().unwrap().is_some() {
                opened += 1;
            }
        }
        assert_eq!(opened, 1, "exactly one contender opens the episode");
        assert_eq!(open_incidents(&pool).await?.len(), 1);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn host_loss_closure_records_unconfirmed_recovery_notice() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        open_mismatch(&pool, "pc-01").await?.expect("open");

        let closed = close_mismatch(&pool, "pc-01", IncidentCloseReason::HostLost)
            .await?
            .expect("closes the open episode");

        let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT closed_at, close_reason FROM pc_monitoring_incidents WHERE id = ?",
        )
        .bind(&closed)
        .fetch_one(&pool)
        .await?;
        assert!(row.0.is_some(), "closed_at is stamped");
        assert_eq!(row.1.as_deref(), Some("host_lost"));
        assert!(open_incidents(&pool).await?.is_empty());
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 1);

        // The closure summary never claims recovery it did not see.
        let summary: String = sqlx::query_scalar(
            "SELECT summary FROM audit_log WHERE event_type = 'pc_monitoring_incident_closed'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(summary.contains("recovery unconfirmed"), "{summary}");
        Ok(())
    }

    #[tokio::test]
    async fn client_recovery_closure_names_the_recovery() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        open_mismatch(&pool, "pc-01").await?.expect("open");

        close_mismatch(&pool, "pc-01", IncidentCloseReason::ClientRecovered)
            .await?
            .expect("close");

        let (reason, summary): (String, String) = sqlx::query_as(
            "SELECT i.close_reason, a.summary FROM pc_monitoring_incidents i
             JOIN audit_log a ON a.id = i.close_audit_id",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "client_recovered");
        assert!(summary.contains("Client recovered"), "{summary}");
        Ok(())
    }

    #[tokio::test]
    async fn closing_without_an_open_episode_writes_nothing() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;

        let result = close_mismatch(&pool, "pc-01", IncidentCloseReason::ClientRecovered).await?;

        assert_eq!(result, None);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn disabling_closes_active_incidents_atomically_and_keeps_history() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        register_live(&pool, "pc-02").await?;
        open_mismatch(&pool, "pc-01").await?.expect("open pc-01");
        open_mismatch(&pool, "pc-02").await?.expect("open pc-02");
        enable(&pool).await?;
        let alerts_before = audit_count(&pool, "pc_monitoring_incident_opened").await;

        let closed = disable_and_close_active(&pool).await?;

        assert_eq!(closed, 2);
        assert!(!enabled(&pool).await?);
        assert!(open_incidents(&pool).await?.is_empty());
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT pc_id, close_reason FROM pc_monitoring_incidents ORDER BY pc_id",
        )
        .fetch_all(&pool)
        .await?;
        assert!(rows
            .iter()
            .all(|(_, reason)| reason.as_deref() == Some("monitoring_disabled")));
        // Historical Alerts remain; closures and the settings Notice exist.
        assert_eq!(
            audit_count(&pool, "pc_monitoring_incident_opened").await,
            alerts_before
        );
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 2);
        assert!(audit_count(&pool, "pc_monitoring_settings_updated").await >= 1);
        Ok(())
    }

    #[tokio::test]
    async fn restart_marks_open_episodes_interrupted_without_new_alerts() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        open_mismatch(&pool, "pc-01").await?.expect("open");

        let marked = mark_observation_interrupted(&pool).await?;

        assert_eq!(marked, 1);
        assert!(open_incidents(&pool).await?[0].observation_interrupted);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        // Idempotent across repeated restarts while still open.
        assert_eq!(mark_observation_interrupted(&pool).await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn active_kinds_map_feeds_dashboard_badges_per_pc() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        register_live(&pool, "pc-02").await?;
        open_mismatch(&pool, "pc-01").await?.expect("open pc-01");

        let map = active_kinds_by_pc(&pool).await?;

        assert_eq!(
            map.get("pc-01"),
            Some(&vec![IncidentKind::ClientReachabilityMismatch])
        );
        assert!(!map.contains_key("pc-02"));

        close_mismatch(&pool, "pc-01", IncidentCloseReason::ClientRecovered)
            .await?
            .expect("close");
        assert!(active_kinds_by_pc(&pool).await?.is_empty());
        Ok(())
    }

    // --- Host Probe Coverage (#343): same model, its own kind ---

    async fn open_coverage(pool: &SqlitePool, pc_id: &str) -> AppResult<Option<String>> {
        open_incident_with_audit(
            pool,
            pc_id,
            &format!("Name {pc_id}"),
            IncidentKind::HostProbeCoverage,
        )
        .await
    }

    #[tokio::test]
    async fn coverage_episode_is_idempotent_and_separate_per_kind() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;

        open_coverage(&pool, "pc-01").await?.expect("open coverage");
        for _ in 0..2 {
            assert_eq!(open_coverage(&pool, "pc-01").await?, None);
        }
        // The mismatch kind is an independent episode on the same PC.
        open_mismatch(&pool, "pc-01").await?.expect("open mismatch");

        let incidents = open_incidents(&pool).await?;
        assert_eq!(incidents.len(), 2);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn probe_recovery_closure_records_a_distinct_notice() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        open_coverage(&pool, "pc-01").await?.expect("open");

        close_incident_with_audit(
            &pool,
            "pc-01",
            "Name pc-01",
            IncidentKind::HostProbeCoverage,
            IncidentCloseReason::ProbeRecovered,
        )
        .await?
        .expect("closes the open episode");

        let (reason, summary): (String, String) = sqlx::query_as(
            "SELECT i.close_reason, a.summary FROM pc_monitoring_incidents i
             JOIN audit_log a ON a.id = i.close_audit_id",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "probe_recovered");
        assert!(summary.contains("answered probes again"), "{summary}");
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn coverage_premise_loss_closure_names_the_lost_proof() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        open_coverage(&pool, "pc-01").await?.expect("open");

        close_incident_with_audit(
            &pool,
            "pc-01",
            "Name pc-01",
            IncidentKind::HostProbeCoverage,
            IncidentCloseReason::HostLost,
        )
        .await?
        .expect("closes the open episode");

        let summary: String = sqlx::query_scalar(
            "SELECT a.summary FROM pc_monitoring_incidents i
             JOIN audit_log a ON a.id = i.close_audit_id",
        )
        .fetch_one(&pool)
        .await?;
        assert!(
            summary.contains("no longer proves the host is running"),
            "{summary}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn disabling_closes_a_coverage_episode_as_disabled() -> AppResult<()> {
        let pool = test_pool().await?;
        register_live(&pool, "pc-01").await?;
        open_coverage(&pool, "pc-01").await?.expect("open");

        disable_and_close_active(&pool).await?;

        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason.as_deref(), Some("monitoring_disabled"));
        Ok(())
    }
}

#[cfg(test)]
mod suppression_tests {
    use chrono::{DateTime, TimeZone, Utc};
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).single().expect("epoch")
    }

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    async fn audit_count(pool: &SqlitePool, event_type: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE event_type = ?")
            .bind(event_type)
            .fetch_one(pool)
            .await
            .expect("audit count")
    }

    #[tokio::test]
    async fn create_commits_row_and_notice_audit_together() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(1_000);
        let expires = set_suppression_with_audit(&pool, "pc-01", "PC 01", 30, now).await?;

        assert_eq!(expires, "1970-01-01T00:46:40Z");
        let active = active_suppressions(&pool, now).await?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].pc_id, "pc-01");
        assert_eq!(
            audit_count(&pool, "pc_monitoring_suppression_updated").await,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn replace_overwrites_the_absolute_expiry() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(2_000);
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 60, now).await?;
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 15, now).await?;

        // One row per PC: the replacement replaced the expiry in place.
        assert_eq!(active_suppressions(&pool, now).await?.len(), 1);
        assert_eq!(
            suppressions_by_pc(&pool, now).await?["pc-01"],
            "1970-01-01T00:48:20Z"
        );
        assert_eq!(
            audit_count(&pool, "pc_monitoring_suppression_updated").await,
            2
        );
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'pc_monitoring_suppression_updated'",
        )
        .fetch_all(&pool)
        .await?
        .into_iter()
        .map(|json: String| {
            serde_json::from_str::<serde_json::Value>(&json).expect("metadata json")["action"]
                .as_str()
                .expect("action")
                .to_owned()
        })
        .collect();
        assert_eq!(actions, vec!["created".to_owned(), "replaced".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn cancel_removes_the_row_and_audits_once() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(3_000);
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 30, now).await?;

        assert!(cancel_suppression_with_audit(&pool, "pc-01", "PC 01").await?);
        assert!(!cancel_suppression_with_audit(&pool, "pc-01", "PC 01").await?);
        assert!(active_suppressions(&pool, now).await?.is_empty());
        // Only the real cancellation is audited; the no-op wrote nothing.
        assert_eq!(
            audit_count(&pool, "pc_monitoring_suppression_updated").await,
            2
        );
        let action: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'pc_monitoring_suppression_updated'
             ORDER BY rowid DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await?;
        assert!(action.contains("cancelled"));
        Ok(())
    }

    #[tokio::test]
    async fn expired_rows_stop_matching_without_deletion() -> AppResult<()> {
        let pool = test_pool().await?;
        let now = at(4_000);
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 30, now).await?;
        assert_eq!(
            active_suppression_pcs(&pool, at(4_000 + 1_799))
                .await?
                .len(),
            1
        );
        // One second past the absolute expiry: lazily inactive.
        assert!(active_suppression_pcs(&pool, at(4_000 + 1_800))
            .await?
            .is_empty());
        // ...and a new suppression can be created afterwards.
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 10, at(4_000 + 1_801)).await?;
        assert_eq!(
            active_suppression_pcs(&pool, at(4_000 + 1_802)).await?,
            ["pc-01".to_owned()].into_iter().collect()
        );
        Ok(())
    }

    #[tokio::test]
    async fn suppressions_are_durable_across_a_fresh_pool() -> AppResult<()> {
        let directory = crate::test_utils::migration_backup_dir();
        let path = std::env::temp_dir().join(format!(
            "openpiso-suppression-durable-{}",
            uuid::Uuid::new_v4()
        ));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        crate::db::run_migrations(&pool, directory.path()).await?;
        crate::db::seed(&pool).await?;
        let now = at(5_000);
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 45, now).await?;
        pool.close().await;

        // A fresh Admin process reopens the same database file.
        let reopened = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let map = suppressions_by_pc(&reopened, at(5_100)).await?;
        assert_eq!(map["pc-01"], "1970-01-01T02:08:20Z");
        reopened.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[tokio::test]
    async fn suppression_paths_leave_pcs_sessions_money_time_points_untouched() -> AppResult<()> {
        let pool = test_pool().await?;
        let before: (i64, String) =
            sqlx::query_as("SELECT remaining_seconds, status FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let sessions_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await?;

        let now = at(6_000);
        set_suppression_with_audit(&pool, "pc-01", "PC 01", 30, now).await?;
        cancel_suppression_with_audit(&pool, "pc-01", "PC 01").await?;

        let after: (i64, String) =
            sqlx::query_as("SELECT remaining_seconds, status FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(before, after);
        assert_eq!(sessions_before, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM coin_transactions")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }
}

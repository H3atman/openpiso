// The Support Bundle builder (ADR-0078 / issues #360, #361): one
// self-contained, redacted Markdown document covering one PC and one window.
//
// Sections render in order — Environment, Verdict, Timeline, Anomalies, raw
// appendix — so the stored PC facts and the Verdict precede the data and
// survive truncation. The Verdict names the failure shape in plain language
// over six rules (foreign Client image, dead Guard, reconnect flap, Offline
// Shutdown Countdown, clock skew, coverage contradiction) or states plainly
// that no known shape matched. Its evidence is the same merged timeline
// inputs — Forwarded Client lines, Forwarded Guard lines, Admin's own
// diagnostic lines for that PC, and the SQLite context that recorded the
// symptom — plus the Guard Poll facts (ADR-0077), which are in-memory only
// and reach the builder from the endpoint. The Web Admin surface is later
// ticket #362.
//
// No HTTP or UI involvement — a free function taking the log directories
// (Admin's own diagnostics and the dedicated forwarded log), the database
// pool, a PC, and options, mirroring the Coin Debug Bundle builder in shape
// and in its redaction stance.
//
// Security: device secrets are structurally excluded — no query selects the
// `coin_devices.secret` column and every appendix field must be named by the
// whitelist — account identities are pseudonymised to stable per-bundle
// labels (the label replaces the username in every rendered text, the raw
// appendix's message included), and the raw appendix is re-serialised from a
// field whitelist.

use std::{collections::HashMap, path::Path};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::SqlitePool;
use tracing::warn;

use openpiso_core::forwarding::{ForwardedSource, FORWARDING_BUFFER_TARGET};

use crate::bundle_machinery::{
    finalize_document, format_timestamp, keep_newest, log_files_with_prefix, markdown_cell,
    parse_diagnostic_json_line, parse_diagnostic_log_dir, parse_flexible_utc, parse_log_files,
    serialise_appendix_entry, timestamp_in_window, truncate_timestamp, AccountLabelMap,
    BundleLayout, WindowInfo,
};
use crate::errors::AppResult;
use crate::forwarded_log::FORWARDED_LOG_FILE_PREFIX;
use crate::guard_poll::{GuardFacts, GUARD_POLL_STALE_AFTER_SECS};
use crate::host_diagnostics::HostDiagnostics;

// ── Options ───────────────────────────────────────────────────────────

/// The default window: six hours (ADR-0078) — a coverage problem accumulates
/// over a grace period and is noticed hours later, where a coin problem is a
/// burst.
pub const DEFAULT_WINDOW_SECONDS: i64 = 21_600;

/// Hard upper bound on the requested window, so one request can never turn
/// into an unbounded scan of the rotating logs and Audit Log.
pub const MAX_WINDOW_SECONDS: i64 = 86_400;

/// Newest Audit Log events fetched per bundle, so a 24-hour window on a busy
/// PC stays bounded; the newest are kept.
const AUDIT_FETCH_CAP: i64 = 1_000;

/// Newest appendix entries kept before the byte cap gets a chance; stated as
/// truncation when exceeded.
const APPENDIX_LINE_CAP: usize = 2_000;

/// Newest PC Monitoring Incidents fetched per PC, per state (open / closed).
const INCIDENT_FETCH_CAP: i64 = 20;

/// Verdict rule 5: a signed Client clock difference beyond this many
/// milliseconds is a clock badly out of step, not ordinary drift. Two
/// minutes: far wider than a time-synced PC ever drifts, small enough to
/// distort Heartbeat timing and Tamper Kill attribution.
const CLOCK_SKEW_THRESHOLD_MS: i64 = 120_000;

/// Verdict rule 3: one drop the Client came back from is a legitimate pipe
/// drop, not a flap; repeated drop-and-reconnect pairs are the flap shape.
const FLAP_MIN_RECONNECTS: u64 = 2;

/// Incident-anchored window (ADR-0078 / #363): the bundle covers one hour
/// before the incident to fifteen minutes after it, so evidence the incident
/// responded to stays in view no matter how long ago it fired.
pub const ANCHOR_BEFORE_SECONDS: i64 = 3_600;
pub const ANCHOR_AFTER_SECONDS: i64 = 900;

/// Configuration for the Support Bundle builder.
#[derive(Debug, Clone)]
pub struct SupportBundleOptions {
    /// Window duration (seconds) ending at generation time; defaults to
    /// 21,600 (6 hours) and never exceeds [`MAX_WINDOW_SECONDS`]. Wider than
    /// the Coin Debug Bundle's two hours by design (ADR-0078): a coverage
    /// problem accumulates over a grace period and is noticed hours later,
    /// where a coin problem is a burst. Ignored while `window_anchor` is set.
    pub window_seconds: i64,
    /// When set, the window is anchored to this instant — one hour before to
    /// fifteen minutes after — instead of ending at generation time. An
    /// owner opening a hours-old incident gets the slice that explains it,
    /// not a window of quiet that happens to be recent.
    pub window_anchor: Option<DateTime<Utc>>,
    /// Maximum timeline rows; defaults to 200.
    pub timeline_cap: usize,
    /// Hard ceiling in bytes for the entire document; defaults to ~250 KB
    /// (256 KiB), wider than the Coin Debug Bundle's 80 KB.
    pub size_cap: usize,
}

impl Default for SupportBundleOptions {
    fn default() -> Self {
        Self {
            window_seconds: DEFAULT_WINDOW_SECONDS,
            window_anchor: None,
            timeline_cap: 200,
            size_cap: 262_144,
        }
    }
}

// ── Field whitelist for the raw appendix ──────────────────────────────

/// Fields kept in the raw appendix. Any field not in this list is
/// structurally excluded — the same stance the Coin Debug Bundle takes. Note
/// what is deliberately absent: `account_id` (identities are pseudonymised
/// into the timeline, never echoed raw) and every secret-bearing column name.
const APPENDIX_FIELD_WHITELIST: &[&str] = &[
    "timestamp",
    "level",
    "target",
    "message",
    "pc_id",
    "source",
    "received_at",
];

/// The Support Bundle's binding of the shared size-cap machinery to its
/// section markers. Sections render in the documented order — Environment,
/// Admin Host, Verdict, Timeline, Anomalies, raw appendix — so the stored
/// facts and the findings precede the data and survive truncation, with the
/// appendix the first evidence sacrificed.
const SUPPORT_BUNDLE_LAYOUT: BundleLayout = BundleLayout {
    title: "# Support Bundle",
    appendix_marker: "## Raw Appendix",
    appendix_notice: "*(appendix truncated for size)*",
    timeline_marker: "## Timeline",
    anomalies_marker: "## Anomalies",
    anomalies_notice: "*(byte-bounded; optional anomaly detail omitted)*",
    environment_marker: "## Environment",
    // The Verdict section is ticket #361; the shared minimal fallback still
    // names the heading so the placeholder survives until then.
    verdict_marker: "## Verdict",
    section_notice: "*(byte-bounded)*",
};

// ── Parsed log structures ────────────────────────────────────────────

/// One log line attributed to the PC, from either Admin's own diagnostics or
/// the dedicated forwarded log.
#[derive(Debug, Clone)]
struct LogEntry {
    timestamp: String,
    level: String,
    message: String,
    raw_fields: HashMap<String, Value>,
    account_id: Option<String>,
    /// Which device surface forwarded the line; `None` for Admin's own
    /// lines.
    source: Option<ForwardedSource>,
    /// Synthetic drop marker emitted by the device-side forwarding buffer
    /// (ADR-0078): a visible gap, never an ordinary log line.
    is_drop_marker: bool,
}

// ── DB-side structures ────────────────────────────────────────────────

/// The PC's stored facts (ADR-0078): the Environment section states them
/// before any evidence.
#[derive(Debug, Clone)]
struct PcFacts {
    name: String,
    status: String,
    registered_ip: Option<String>,
    mac_address: Option<String>,
    last_seen_at: Option<String>,
    last_guard_coverage: Option<String>,
    /// Signed Admin-minus-Client clock measurement (migration 0040,
    /// diagnostic only): positive means the Client clock is behind.
    clock_difference_ms: Option<i64>,
}

/// One PC Monitoring Incident episode.
#[derive(Debug, Clone)]
struct IncidentRecord {
    kind: String,
    opened_at: String,
    closed_at: Option<String>,
    close_reason: Option<String>,
    observation_interrupted: bool,
    open_audit_id: String,
    close_audit_id: Option<String>,
}

/// One in-window Audit Log event.
#[derive(Debug, Clone)]
struct AuditRecord {
    id: String,
    event_type: String,
    outcome: String,
    summary: String,
    created_at: String,
    account_id: Option<String>,
    actor_account_id: Option<String>,
    metadata_json: String,
}

// ── Timeline row ──────────────────────────────────────────────────────

/// One merged timeline row. `source` discriminates client / guard / admin /
/// audit / incident so a reader knows which surface produced the line.
#[derive(Debug, Clone)]
struct TimelineRow {
    timestamp: String,
    source: &'static str,
    level: String,
    event: String,
}

// ── Main function ─────────────────────────────────────────────────────

/// Build a Support Bundle for one PC and one window: a bounded, redacted
/// Markdown document that merges the forwarded Client and Guard lines, Admin's
/// own lines for that PC, and the SQLite context into one timeline, then
/// names the failure shape in the Verdict. The window ends at generation
/// time, or — with `options.window_anchor` set — is anchored around a PC
/// Monitoring Incident (one hour before to fifteen minutes after, #363), so
/// an owner opening a hours-old incident gets the slice that explains it.
///
/// `guard_facts` is the PC's Guard Poll snapshot (ADR-0077): in-memory
/// state no log row or SQLite record holds, so the endpoint hands it over.
/// `None` means the Guard has never polled. `host` is Admin's own health
/// (ADR-0082) from the same collector the host-diagnostics endpoint serves,
/// so this bundle and the owner's screen cannot disagree.
pub async fn build_support_bundle(
    admin_log_dir: &Path,
    forwarded_log_dir: &Path,
    pool: &SqlitePool,
    pc_id: &str,
    options: &SupportBundleOptions,
    guard_facts: Option<&GuardFacts>,
    host: &HostDiagnostics,
) -> String {
    let pc_id = pc_id.trim();
    let generated_at = Utc::now();
    let mut window_info =
        compute_window_info(options.window_seconds, options.window_anchor, generated_at);

    // Filter every source before labels, joins, or the appendix are computed.
    // This makes the document describe the requested window, rather than
    // whatever old data remains on disk. Forwarded lines are windowed on the
    // device wall-clock stamp the sink stores — the event time — so the
    // timeline orders events by when they happened, not when Admin happened
    // to receive them (an Admin outage can delay the latter by hours; the
    // owner widens the window to cover it).
    let mut entries = parse_admin_log_entries(admin_log_dir);
    entries.extend(parse_forwarded_log_entries(forwarded_log_dir));
    let entries: Vec<LogEntry> = entries
        .into_iter()
        .filter(|entry| entry.pc_is(pc_id) && timestamp_in_window(&entry.timestamp, &window_info))
        .collect();

    let context = query_db(pool, pc_id, &window_info).await;
    // Open incidents (any age — they are the live symptom), episodes closed
    // inside the window, and episodes opened inside it even if they closed
    // after the window's end (the anchored incident itself, likely closed
    // long after the owner clicked).
    let incidents: Vec<IncidentRecord> = context
        .incidents
        .into_iter()
        .filter(|incident| {
            incident.closed_at.is_none()
                || timestamp_in_window(
                    incident.closed_at.as_deref().unwrap_or_default(),
                    &window_info,
                )
                || timestamp_in_window(&incident.opened_at, &window_info)
        })
        .collect();

    // Pseudonymise first so no username can leak into any rendered section.
    // Evidence naming an account whose username could not be resolved is
    // dropped rather than rendered unredacted.
    let account = build_account_labels(&entries, &context.audit_events, pool).await;
    let unresolved = &account.unresolved;
    let entries: Vec<LogEntry> = entries
        .into_iter()
        .filter(|entry| {
            entry
                .account_id
                .as_deref()
                .is_none_or(|account_id| !unresolved.contains(account_id))
        })
        .collect();
    let audit_events: Vec<AuditRecord> = context
        .audit_events
        .into_iter()
        .filter(|event| {
            event
                .account_id
                .as_deref()
                .is_none_or(|account_id| !unresolved.contains(account_id))
                && event
                    .actor_account_id
                    .as_deref()
                    .is_none_or(|account_id| !unresolved.contains(account_id))
        })
        .collect();
    let replacements = username_replacements(&account.usernames, &account.labels);
    let db_failed = context.failed || !unresolved.is_empty();

    let (timeline, timeline_was_capped) = build_timeline(
        &entries,
        &incidents,
        &audit_events,
        &account.labels,
        &replacements,
        &window_info,
        options,
    );
    window_info.timeline_truncated = timeline_was_capped;

    let anomalies = detect_anomalies(&entries, &incidents, &window_info);
    let (appendix_entries, appendix_was_capped) = appendix_entries(&entries, &window_info);
    window_info.appendix_truncated |= appendix_was_capped;

    let environment_section = build_environment_section(pc_id, &context.facts, db_failed);
    let host_section = build_host_section(host);
    let verdict_section = build_verdict_section(
        &entries,
        context.facts.as_ref(),
        guard_facts,
        &window_info,
        generated_at,
    );
    let timeline_section = build_timeline_section(&timeline, &window_info);
    let anomalies_section = build_anomalies_section(&anomalies);
    let appendix_section = build_appendix_section(&appendix_entries, &replacements);

    let mut document = format!(
        "# Support Bundle\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}",
        window_info.header(),
        environment_section,
        host_section,
        verdict_section,
        timeline_section,
        anomalies_section,
        appendix_section,
    );

    finalize_document(
        &mut document,
        options.size_cap,
        &SUPPORT_BUNDLE_LAYOUT,
        &mut window_info,
    );
    document
}

// ── Window computation ────────────────────────────────────────────────

fn compute_window_info(
    window_seconds: i64,
    window_anchor: Option<DateTime<Utc>>,
    generated_at: DateTime<Utc>,
) -> WindowInfo {
    let (start, end) = match window_anchor {
        Some(anchor) => (
            anchor - chrono::Duration::seconds(ANCHOR_BEFORE_SECONDS),
            anchor + chrono::Duration::seconds(ANCHOR_AFTER_SECONDS),
        ),
        None => {
            let window_seconds = window_seconds.clamp(0, MAX_WINDOW_SECONDS);
            (
                generated_at - chrono::Duration::seconds(window_seconds),
                generated_at,
            )
        }
    };
    WindowInfo {
        from: format_timestamp(start),
        to: format_timestamp(end),
        // The window policy is always applied, even when no old evidence
        // exists; keeping this explicit prevents an empty result from being
        // mistaken for an unbounded search.
        time_window_truncated: true,
        timeline_truncated: false,
        appendix_truncated: false,
        byte_truncated: false,
    }
}

// ── Log parsing ───────────────────────────────────────────────────────

fn parse_admin_log_entries(log_dir: &Path) -> Vec<LogEntry> {
    parse_diagnostic_log_dir(log_dir, parse_admin_log_line)
}

/// Admin's own diagnostic lines. Only lines carrying the PC's `pc_id` field
/// ever belong to a per-PC bundle; unattributed lines describe the Admin host
/// itself.
fn parse_admin_log_line(line: &str) -> Option<LogEntry> {
    let parsed = parse_diagnostic_json_line(line, APPENDIX_FIELD_WHITELIST)?;
    Some(entry_from_parsed(parsed, None))
}

fn parse_forwarded_log_entries(log_dir: &Path) -> Vec<LogEntry> {
    let files = log_files_with_prefix(log_dir, &[FORWARDED_LOG_FILE_PREFIX]);
    parse_log_files(files, parse_forwarded_log_line)
}

/// Forwarded Diagnostics land in the dedicated `forwarded.*.log` files, one
/// JSON object per line carrying the PC identity and the source
/// discriminator. A line without both is unattributable and skipped.
fn parse_forwarded_log_line(line: &str) -> Option<LogEntry> {
    let parsed = parse_diagnostic_json_line(line, APPENDIX_FIELD_WHITELIST)?;
    let source = parsed.json.get("source").and_then(|v| v.as_str())?;
    let source = match source {
        "client" => ForwardedSource::Client,
        "guard" => ForwardedSource::Guard,
        _ => return None,
    };
    let pc_id = parsed
        .json
        .get("pc_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if pc_id.trim().is_empty() {
        return None;
    }
    Some(entry_from_parsed(parsed, Some(source)))
}

fn entry_from_parsed(
    parsed: crate::bundle_machinery::DiagnosticLine,
    source: Option<ForwardedSource>,
) -> LogEntry {
    let is_drop_marker = parsed.target == FORWARDING_BUFFER_TARGET;
    LogEntry {
        timestamp: parsed.timestamp,
        level: parsed.level,
        message: parsed.message,
        raw_fields: parsed.raw_fields,
        account_id: parsed
            .json
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        source,
        is_drop_marker,
    }
}

impl LogEntry {
    fn pc_is(&self, pc_id: &str) -> bool {
        self.raw_fields
            .get("pc_id")
            .and_then(|v| v.as_str())
            .is_some_and(|value| value == pc_id)
    }
}

// ── Database queries ──────────────────────────────────────────────────

/// All SQLite context for one bundle, plus whether any query failed — a
/// failed query must be stated in the document, never left to read as
/// silence.
struct DbContext {
    facts: Option<PcFacts>,
    incidents: Vec<IncidentRecord>,
    audit_events: Vec<AuditRecord>,
    failed: bool,
}

async fn query_db(pool: &SqlitePool, pc_id: &str, window: &WindowInfo) -> DbContext {
    let mut failed = false;

    let facts = match query_pc_facts(pool, pc_id).await {
        Ok(facts) => facts,
        Err(e) => {
            warn!("support bundle: PC facts query failed: {e}");
            failed = true;
            None
        }
    };
    let incidents = match query_incidents(pool, pc_id).await {
        Ok(incidents) => incidents,
        Err(e) => {
            warn!("support bundle: incidents query failed: {e}");
            failed = true;
            Vec::new()
        }
    };
    let audit_events = match query_audit_events(pool, pc_id, window).await {
        Ok(events) => events,
        Err(e) => {
            warn!("support bundle: audit query failed: {e}");
            failed = true;
            Vec::new()
        }
    };

    DbContext {
        facts,
        incidents,
        audit_events,
        failed,
    }
}

/// Structural secret exclusion: no query here selects the `coin_devices`
/// table at all, let alone its `secret` column. `Ok(None)` means the PC does
/// not exist; `Err` means the query failed.
async fn query_pc_facts(pool: &SqlitePool, pc_id: &str) -> AppResult<Option<PcFacts>> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
        ),
    >(
        "SELECT name, status,
                NULLIF(TRIM(registered_ip), ''),
                NULLIF(TRIM(mac_address), ''),
                last_seen_at, last_guard_coverage, clock_difference_ms
         FROM pcs WHERE id = ?",
    )
    .bind(pc_id)
    .fetch_optional(pool)
    .await?
    .map(|row| {
        let (
            name,
            status,
            registered_ip,
            mac_address,
            last_seen_at,
            last_guard_coverage,
            clock_difference_ms,
        ) = row;
        PcFacts {
            name,
            status,
            registered_ip,
            mac_address,
            last_seen_at,
            last_guard_coverage,
            clock_difference_ms,
        }
    });
    Ok(row)
}

/// One raw `pc_monitoring_incidents` row, in column order.
type IncidentRowTuple = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    String,
    Option<String>,
);

/// Every still-open incident for the PC (any age) and the newest closed
/// episodes. The window cut for closed episodes is applied by the caller.
async fn query_incidents(pool: &SqlitePool, pc_id: &str) -> AppResult<Vec<IncidentRecord>> {
    const INCIDENT_COLUMNS: &str =
        "id, kind, opened_at, closed_at, close_reason, observation_interrupted, open_audit_id, close_audit_id";

    let open = sqlx::query_as::<_, IncidentRowTuple>(&format!(
        r#"
        SELECT {INCIDENT_COLUMNS}
        FROM pc_monitoring_incidents
        WHERE pc_id = ? AND closed_at IS NULL
        ORDER BY opened_at DESC
        LIMIT ?
        "#,
    ))
    .bind(pc_id)
    .bind(INCIDENT_FETCH_CAP)
    .fetch_all(pool)
    .await?;

    let closed = sqlx::query_as::<_, IncidentRowTuple>(&format!(
        r#"
        SELECT {INCIDENT_COLUMNS}
        FROM pc_monitoring_incidents
        WHERE pc_id = ? AND closed_at IS NOT NULL
        ORDER BY closed_at DESC
        LIMIT ?
        "#,
    ))
    .bind(pc_id)
    .bind(INCIDENT_FETCH_CAP)
    .fetch_all(pool)
    .await?;

    let mut records = map_incident_rows(open);
    records.extend(map_incident_rows(closed));
    Ok(records)
}

fn map_incident_rows(rows: Vec<IncidentRowTuple>) -> Vec<IncidentRecord> {
    rows.into_iter()
        .map(
            |(
                _id,
                kind,
                opened_at,
                closed_at,
                close_reason,
                interrupted,
                open_audit_id,
                close_audit_id,
            )| {
                IncidentRecord {
                    kind,
                    opened_at,
                    closed_at,
                    close_reason,
                    observation_interrupted: interrupted == 1,
                    open_audit_id,
                    close_audit_id,
                }
            },
        )
        .collect()
}

/// In-window Audit Log events for the PC, oldest first. Bounded to the
/// newest [`AUDIT_FETCH_CAP`] events so a 24-hour window on a busy PC stays
/// a bounded fetch; the timeline cap states any further cut.
async fn query_audit_events(
    pool: &SqlitePool,
    pc_id: &str,
    window: &WindowInfo,
) -> AppResult<Vec<AuditRecord>> {
    let mut rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    >(
        r#"
        SELECT id, event_type, outcome, summary, created_at,
               account_id, actor_account_id, metadata_json
        FROM audit_log
        WHERE pc_id = ?
          AND julianday(created_at) >= julianday(?)
          AND julianday(created_at) <= julianday(?)
        ORDER BY created_at DESC
        LIMIT ?
        "#,
    )
    .bind(pc_id)
    .bind(&window.from)
    .bind(&window.to)
    .bind(AUDIT_FETCH_CAP)
    .fetch_all(pool)
    .await?;
    // The query keeps the newest; the bundle renders oldest first.
    rows.reverse();

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                event_type,
                outcome,
                summary,
                created_at,
                account_id,
                actor_account_id,
                metadata_json,
            )| {
                AuditRecord {
                    id,
                    event_type,
                    outcome,
                    summary,
                    created_at,
                    account_id,
                    actor_account_id,
                    metadata_json,
                }
            },
        )
        .collect())
}

// ── Account pseudonymisation ──────────────────────────────────────────

/// The pseudonymisation state for one bundle: the label map, each labelled
/// account's username, and the candidates whose username lookup *errored*.
/// Evidence rows naming an unresolved account are dropped rather than
/// rendered unredacted — a redaction failure must never surface a username.
struct AccountLabels {
    labels: AccountLabelMap,
    usernames: HashMap<String, String>,
    unresolved: std::collections::BTreeSet<String>,
}

/// Label every account referenced by the window's evidence, and return the
/// username of each labelled account so rendered free text can be rewritten.
/// Audit `metadata_json` values are candidate ids too — a Points Transfer
/// names its recipient in metadata, and the recipient's username in the
/// summary must be pseudonymised just like the sender's.
async fn build_account_labels(
    entries: &[LogEntry],
    audit_events: &[AuditRecord],
    pool: &SqlitePool,
) -> AccountLabels {
    let mut candidates = collect_account_candidates(entries, audit_events);
    let mut unresolved = std::collections::BTreeSet::new();
    let mut usernames: HashMap<String, String> = HashMap::new();
    for account_id in candidates.clone() {
        match sqlx::query_scalar::<_, String>("SELECT username FROM accounts WHERE id = ?")
            .bind(&account_id)
            .fetch_optional(pool)
            .await
        {
            Ok(Some(username)) => {
                usernames.insert(account_id, username);
            }
            // Not an account id (a request id, a capture id, a username) —
            // never gets a label and never gets rewritten.
            Ok(None) => {
                candidates.remove(&account_id);
            }
            Err(e) => {
                warn!("support bundle: account lookup failed: {e}");
                candidates.remove(&account_id);
                unresolved.insert(account_id);
            }
        }
    }

    let mut labels = AccountLabelMap::default();
    let mut account_items: Vec<(String, &str)> = Vec::new();
    for event in audit_events {
        for account_id in [&event.account_id, &event.actor_account_id]
            .into_iter()
            .flatten()
        {
            if candidates.contains(account_id) {
                account_items.push((event.created_at.clone(), account_id));
            }
        }
    }
    for entry in entries {
        if let Some(account_id) = &entry.account_id {
            if candidates.contains(account_id) {
                account_items.push((entry.timestamp.clone(), account_id));
            }
        }
    }
    account_items.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, account_id) in &account_items {
        labels.label(account_id);
    }
    // Metadata-only candidates were never named by a column; they still get
    // labels (deterministic id order) so their usernames are rewritten too.
    for account_id in &candidates {
        if labels.get(account_id).is_none() {
            labels.label(account_id);
        }
    }
    AccountLabels {
        labels,
        usernames,
        unresolved,
    }
}

/// Account ids referenced by the evidence: audit columns plus every string
/// value inside `metadata_json`. Only ids that resolve to a real account get
/// labels, so stray uuid-shaped metadata keys (`request_id`, capture ids)
/// cost one lookup and nothing else.
fn collect_account_candidates(
    entries: &[LogEntry],
    audit_events: &[AuditRecord],
) -> std::collections::BTreeSet<String> {
    let mut candidates = std::collections::BTreeSet::new();
    for event in audit_events {
        if let Some(account_id) = &event.account_id {
            candidates.insert(account_id.clone());
        }
        if let Some(account_id) = &event.actor_account_id {
            candidates.insert(account_id.clone());
        }
        if let Ok(metadata) = serde_json::from_str::<Value>(&event.metadata_json) {
            collect_string_values(&metadata, &mut candidates);
        }
    }
    for entry in entries {
        if let Some(account_id) = &entry.account_id {
            candidates.insert(account_id.clone());
        }
    }
    candidates
}

fn collect_string_values(value: &Value, out: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::String(text) => {
            out.insert(text.clone());
        }
        Value::Array(items) => {
            for item in items {
                collect_string_values(item, out);
            }
        }
        Value::Object(object) => {
            for item in object.values() {
                collect_string_values(item, out);
            }
        }
        _ => {}
    }
}

/// Build the username → label view of a finished label map, longest username
/// first so overlapping names rewrite without clobbering each other.
fn username_replacements(
    usernames: &HashMap<String, String>,
    labels: &AccountLabelMap,
) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = usernames
        .iter()
        .filter_map(|(account_id, username)| {
            labels
                .get(account_id)
                .map(|label| (username.clone(), label.to_owned()))
        })
        .collect();
    pairs.sort_by_key(|pair| std::cmp::Reverse(pair.0.len()));
    pairs
}

/// Replace every labelled account's username with its label. Occurrences are
/// matched whole-token, so a short username cannot rewrite the middle of
/// another word.
fn replace_usernames(text: &str, replacements: &[(String, String)]) -> String {
    let mut result = text.to_owned();
    for (username, label) in replacements {
        if username.is_empty() {
            continue;
        }
        result = replace_one(&result, username, label);
    }
    result
}

/// Replace one whole-token username within `text`. `str::find` on a valid
/// UTF-8 needle only ever lands on a char boundary, so every slice below is
/// boundary-safe without a panic path.
fn replace_one(text: &str, username: &str, label: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(username) {
        let before_ok = !rest[..start].chars().next_back().is_some_and(is_word_char);
        let after = start + username.len();
        let after_ok = rest[after..]
            .chars()
            .next()
            .is_none_or(|ch| !is_word_char(ch));
        if before_ok && after_ok {
            out.push_str(&rest[..start]);
            out.push_str(label);
            rest = &rest[after..];
        } else {
            // Not a whole-token occurrence: copy past this one character and
            // keep scanning. The occurrence start is a char boundary, so at
            // least one character exists; if it somehow did not, copying the
            // whole remainder still terminates the loop.
            let keep_to = rest[start..]
                .chars()
                .next()
                .map(|ch| start + ch.len_utf8())
                .unwrap_or(rest.len());
            out.push_str(&rest[..keep_to]);
            rest = &rest[keep_to..];
        }
    }
    out.push_str(rest);
    out
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

// ── Timeline building ─────────────────────────────────────────────────

fn build_timeline(
    entries: &[LogEntry],
    incidents: &[IncidentRecord],
    audit_events: &[AuditRecord],
    account_labels: &AccountLabelMap,
    replacements: &[(String, String)],
    window: &WindowInfo,
    options: &SupportBundleOptions,
) -> (Vec<TimelineRow>, bool) {
    // A rendered incident edge already tells the story of the Audit event it
    // references; drop that duplicate so the merged timeline shows each
    // episode edge once.
    let mut audit_ids_rendered_by_incidents = std::collections::HashSet::new();
    for incident in incidents {
        if timestamp_in_window(&incident.opened_at, window) {
            audit_ids_rendered_by_incidents.insert(incident.open_audit_id.as_str());
        }
        if let (Some(closed_at), Some(close_audit_id)) =
            (&incident.closed_at, &incident.close_audit_id)
        {
            if timestamp_in_window(closed_at, window) {
                audit_ids_rendered_by_incidents.insert(close_audit_id.as_str());
            }
        }
    }

    let mut rows: Vec<TimelineRow> = Vec::new();

    for entry in entries {
        let source: &'static str = match entry.source {
            Some(ForwardedSource::Client) => "client",
            Some(ForwardedSource::Guard) => "guard",
            None => "admin",
        };
        let (level, event) = if entry.is_drop_marker {
            ("gap".to_owned(), format!("⚠ {}", entry.message))
        } else {
            let mut event = replace_usernames(&entry.message, replacements);
            if let Some(label) = entry
                .account_id
                .as_deref()
                .and_then(|account_id| account_labels.get(account_id))
            {
                event.push_str(&format!(" (account {label})"));
            }
            (entry.level.to_lowercase(), event)
        };
        rows.push(TimelineRow {
            timestamp: entry.timestamp.clone(),
            source,
            level,
            event,
        });
    }

    for event in audit_events {
        if audit_ids_rendered_by_incidents.contains(event.id.as_str()) {
            continue;
        }
        let summary = replace_usernames(&event.summary, replacements);
        rows.push(TimelineRow {
            timestamp: event.created_at.clone(),
            source: "audit",
            level: event.outcome.clone(),
            event: format!("[{}] {}", event.event_type, summary),
        });
    }

    for incident in incidents {
        let kind = incident_kind_label(&incident.kind);
        if timestamp_in_window(&incident.opened_at, window) {
            rows.push(TimelineRow {
                timestamp: incident.opened_at.clone(),
                source: "incident",
                level: "opened".to_owned(),
                event: format!("{kind} opened"),
            });
        }
        if let (Some(closed_at), Some(close_reason)) = (&incident.closed_at, &incident.close_reason)
        {
            if timestamp_in_window(closed_at, window) {
                rows.push(TimelineRow {
                    timestamp: closed_at.clone(),
                    source: "incident",
                    level: "closed".to_owned(),
                    event: format!("{kind} closed ({close_reason})"),
                });
            }
        }
    }

    rows.sort_by(|a, b| timestamp_order(&a.timestamp, &b.timestamp));

    // Apply the timeline cap: keep the newest rows.
    keep_newest(rows, options.timeline_cap)
}

fn timestamp_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_flexible_utc(a), parse_flexible_utc(b)) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

fn incident_kind_label(kind: &str) -> String {
    match kind {
        "client_reachability_mismatch" => "Client Reachability Mismatch",
        "host_probe_coverage" => "Host Probe Coverage",
        "client_guard_coverage" => "Client Guard Coverage",
        other => return other.to_owned(),
    }
    .to_owned()
}

// ── Anomaly detection ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct AnomalyEntry {
    kind: &'static str,
    timestamp: String,
    details: String,
}

fn detect_anomalies(
    entries: &[LogEntry],
    incidents: &[IncidentRecord],
    window: &WindowInfo,
) -> Vec<AnomalyEntry> {
    let mut anomalies = Vec::new();

    // An open PC Monitoring Incident is the live symptom the owner came for.
    for incident in incidents
        .iter()
        .filter(|incident| incident.closed_at.is_none())
    {
        let kind = incident_kind_label(&incident.kind);
        let interrupted = if incident.observation_interrupted {
            "; observation interrupted by an Admin restart"
        } else {
            ""
        };
        anomalies.push(AnomalyEntry {
            kind: "open_incident",
            timestamp: incident.opened_at.clone(),
            details: format!("{kind} open{interrupted}"),
        });
    }

    // Forwarded drop markers are visible gaps: aggregate them into one
    // anomaly so a silent loss is never mistaken for quiet evidence.
    let markers: Vec<&LogEntry> = entries
        .iter()
        .filter(|entry| entry.is_drop_marker && timestamp_in_window(&entry.timestamp, window))
        .collect();
    let dropped_total: u64 = markers
        .iter()
        .map(|marker| marker_dropped_count(&marker.message))
        .sum();
    if dropped_total > 0 {
        anomalies.push(AnomalyEntry {
            kind: "forwarded_lines_dropped",
            timestamp: markers
                .iter()
                .map(|marker| marker.timestamp.as_str())
                .min()
                .unwrap_or_default()
                .to_owned(),
            details: format!(
                "{dropped_total} forwarded diagnostic line(s) dropped by device buffer overflow across {} marker(s)",
                markers.len()
            ),
        });
    }

    anomalies.sort_by(|a, b| timestamp_order(&a.timestamp, &b.timestamp));
    anomalies
}

/// The marker's message starts with the dropped count (openpiso_core::
/// forwarding). If the wording ever drifts, the marker still represents at
/// least one lost line — a gap must never read as zero.
fn marker_dropped_count(message: &str) -> u64 {
    message
        .split_whitespace()
        .next()
        .and_then(|count| count.parse::<u64>().ok())
        .unwrap_or(1)
}

// ── Verdict ───────────────────────────────────────────────────────────

/// One matched failure shape, in the owner's plain language. The title names
/// the shape; the details carry the specifics that separate the three root
/// causes which produce an identical `unavailable` report.
struct VerdictFinding {
    title: &'static str,
    details: String,
}

/// What the Verdict says when no rule matched. Never an empty section:
/// silence must never read as "nothing wrong".
const NO_MATCH_STATEMENT: &str =
    "No known failure shape matched the evidence in this window. This is not \
a clean bill of health — read the Timeline and Anomalies below for what the \
evidence does show.";

/// Render the Verdict section: the matched failure shapes, or the explicit
/// no-match statement. The six rules (ADR-0078) evaluate over the window's
/// merged evidence in their documented order; each matched shape is named and
/// distinguished, so an owner reads the root cause without opening a single
/// log file. Rules 1, 3, and 6 exist to separate the three root causes that
/// produce an identical `unavailable` report, and more than one can match the
/// same window.
fn build_verdict_section(
    entries: &[LogEntry],
    facts: Option<&PcFacts>,
    guard_facts: Option<&GuardFacts>,
    window: &WindowInfo,
    generated_at: DateTime<Utc>,
) -> String {
    let findings: Vec<VerdictFinding> = [
        rule_foreign_image(entries),
        rule_guard_not_polling(entries, guard_facts, generated_at),
        rule_reconnect_flap(entries, window),
        rule_offline_shutdown(entries),
        rule_clock_skew(facts),
        rule_coverage_contradiction(facts, guard_facts, generated_at),
    ]
    .into_iter()
    .flatten()
    .collect();
    if findings.is_empty() {
        return format!("## Verdict\n\n{NO_MATCH_STATEMENT}\n");
    }
    let mut section = format!(
        "## Verdict\n\n{} failure shape(s) matched the evidence:\n\n",
        findings.len()
    );
    for finding in &findings {
        section.push_str(&format!("- **{}** — {}\n", finding.title, finding.details));
    }
    section
}

/// Seconds since Admin last heard from this PC's Guard, measured on the
/// bundle's clock; never negative.
fn guard_poll_age_secs(generated_at: DateTime<Utc>, facts: &GuardFacts) -> i64 {
    generated_at
        .signed_duration_since(facts.received_at)
        .num_seconds()
        .max(0)
}

/// The window's entries matching `keep`, oldest first.
fn entries_matching(entries: &[LogEntry], keep: impl Fn(&LogEntry) -> bool) -> Vec<&LogEntry> {
    let mut matched: Vec<&LogEntry> = entries.iter().filter(|entry| keep(entry)).collect();
    matched.sort_by(|a, b| timestamp_order(&a.timestamp, &b.timestamp));
    matched
}

/// Rule 1: the Guard rejected connect attempts from a foreign Client image.
/// Reports the rejection count, suppressed attempts included — the Guard logs
/// at most one line per logging interval and folds the rest into
/// `suppressed_connections` — and the expected and actual image paths from
/// the newest rejection, which may describe a different image than earlier
/// attempts in a mixed window.
fn rule_foreign_image(entries: &[LogEntry]) -> Option<VerdictFinding> {
    let rejections = entries_matching(entries, |entry| {
        entry.message.starts_with("rejecting foreign pipe client")
    });
    if rejections.is_empty() {
        return None;
    }
    let suppressed: u64 = rejections
        .iter()
        .filter_map(|entry| message_field(&entry.message, "suppressed_connections"))
        .filter_map(|count| count.parse::<u64>().ok())
        .sum();
    let count = rejections.len() as u64 + suppressed;
    let newest = rejections[rejections.len() - 1];
    let expected = message_field(&newest.message, "expected_image");
    let actual = message_field(&newest.message, "actual_image");
    let paths = match (expected, actual) {
        (Some(expected), Some(actual)) => format!(
            "the newest rejection names expected image `{}`, actual image `{}`",
            markdown_cell(expected),
            markdown_cell(actual),
        ),
        _ => "the newest rejection did not record the image paths".to_owned(),
    };
    Some(VerdictFinding {
        title: "Foreign Client image rejected by the Guard",
        details: format!(
            "the Guard rejected {count} connect attempt(s) from a foreign Client image \
             in this window; {paths}. Recognise a bad install or a side-loaded \
             build and reinstall the Client image.",
        ),
    })
}

/// Rule 2: no Guard lines were forwarded in the window and the Guard's last
/// poll is missing or stale — the Guard is not running or not polling. The
/// poll, not the forwarded log, is the liveness evidence: a Guard that logs
/// nothing can still be perfectly healthy, but one whose poll went quiet is
/// not running its pipe either.
fn rule_guard_not_polling(
    entries: &[LogEntry],
    guard_facts: Option<&GuardFacts>,
    generated_at: DateTime<Utc>,
) -> Option<VerdictFinding> {
    if entries
        .iter()
        .any(|entry| entry.source == Some(ForwardedSource::Guard))
    {
        return None;
    }
    let poll_stated = match guard_facts {
        // The Guard Poll registry is in-memory (ADR-0077): an Admin restart
        // resets it, so an empty record may mean "never polled" or "restart
        // since" — say both rather than claiming history.
        None => "Admin has no Guard Poll record for this PC — the Guard has \
                 never polled, or Admin restarted since its last one"
            .to_owned(),
        Some(facts) => {
            let age = guard_poll_age_secs(generated_at, facts);
            if age <= GUARD_POLL_STALE_AFTER_SECS {
                return None;
            }
            format!(
                "the Guard's last poll reached Admin {age} s ago, beyond the \
                 {GUARD_POLL_STALE_AFTER_SECS} s freshness bound"
            )
        }
    };
    Some(VerdictFinding {
        title: "Guard not running or not polling",
        details: format!(
            "{poll_stated} and no Guard lines were forwarded in this window. \
             The Guard service is likely not running — start it on that PC.",
        ),
    })
}

/// Rule 3: the Client's Guard pipe dropped and reconnected repeatedly — a
/// reconnect flap. A flap is a drop the Client came back from: the rule pairs
/// each Client disconnect edge ("Guard pipe closed; reconnecting") with the
/// next Client connect edge and fires only on repeated pairs, so closes with
/// no reconnect are never dressed up as flaps (the timeline shows them, and
/// rule 2 owns the Guard-gone shape). The duty cycle is the proportion of the
/// window spent disconnected, including a final drop with no reconnect yet.
fn rule_reconnect_flap(entries: &[LogEntry], window: &WindowInfo) -> Option<VerdictFinding> {
    let mut closes: Vec<DateTime<Utc>> = Vec::new();
    let mut connects: Vec<DateTime<Utc>> = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.source == Some(ForwardedSource::Client))
    {
        let Some(stamp) = parse_flexible_utc(&entry.timestamp) else {
            continue;
        };
        if entry.message.starts_with("Guard pipe closed") {
            closes.push(stamp);
        } else if entry.message.starts_with("connected to Guard pipe") {
            connects.push(stamp);
        }
    }
    closes.sort();
    connects.sort();
    let (Some(to), Some(from)) = (
        parse_flexible_utc(&window.to),
        parse_flexible_utc(&window.from),
    ) else {
        return None;
    };
    let window_secs = (to - from).num_seconds().max(0);
    // Every entry is already window-filtered, so each close sits inside the
    // window. Each close contributes until reconnect or window end; a
    // close inside an interval already counted is the same drop.
    let mut reconnects: u64 = 0;
    let mut disconnected_secs: i64 = 0;
    let mut counted_until: Option<DateTime<Utc>> = None;
    for close in &closes {
        if counted_until.is_some_and(|until| *close < until) {
            continue;
        }
        let next_connect = connects.iter().find(|connect| **connect > *close).copied();
        reconnects += u64::from(next_connect.is_some());
        let until = next_connect.unwrap_or(to);
        disconnected_secs += until.signed_duration_since(*close).num_seconds().max(0);
        counted_until = Some(until);
    }
    if reconnects < FLAP_MIN_RECONNECTS {
        return None;
    }
    let percent = if window_secs > 0 {
        disconnected_secs * 100 / window_secs
    } else {
        0
    };
    Some(VerdictFinding {
        title: "Client reconnect flap",
        details: format!(
            "the Client's Guard pipe dropped and reconnected {reconnects} time(s) \
             in this window — a reconnect flap, not a clean drop. It spent about \
             {percent}% of the window disconnected ({disconnected_secs} s \
             disconnected of a {window_secs} s window).",
        ),
    })
}

/// Rule 4: the Client armed its Offline Shutdown Countdown — it could not
/// reach Admin. Reports the last arm and whether the window shows the
/// cancellation, so a still-armed countdown is never read as resolved.
fn rule_offline_shutdown(entries: &[LogEntry]) -> Option<VerdictFinding> {
    let arms = entries_matching(entries, |entry| {
        entry
            .message
            .starts_with("Offline Shutdown Countdown armed")
    });
    if arms.is_empty() {
        return None;
    }
    let last_arm = arms[arms.len() - 1];
    let cancelled = entries
        .iter()
        .filter(|entry| {
            entry
                .message
                .starts_with("Offline Shutdown Countdown cancelled")
        })
        .filter(|entry| {
            timestamp_order(&last_arm.timestamp, &entry.timestamp) == std::cmp::Ordering::Less
        })
        .map(|entry| truncate_timestamp(&entry.timestamp))
        .min()
        .map(|stamp| format!("the window shows it cancelled again at {stamp}"))
        .unwrap_or_else(|| "no cancellation was recorded in the window".to_owned());
    Some(VerdictFinding {
        title: "Offline Shutdown Countdown armed",
        details: format!(
            "the Client could not reach Admin and armed its Offline Shutdown \
             Countdown (last armed at {}); {cancelled}. The PC could not reach \
             Admin — the Client did not fail its Guard.",
            truncate_timestamp(&last_arm.timestamp),
        ),
    })
}

/// Rule 5: the PC's clock is badly out of step with Admin's. Reports the
/// signed measurement and its direction (positive means the Client clock is
/// behind, per the stored `clock_difference_ms`).
fn rule_clock_skew(facts: Option<&PcFacts>) -> Option<VerdictFinding> {
    let difference = facts?.clock_difference_ms?;
    if difference.abs() <= CLOCK_SKEW_THRESHOLD_MS {
        return None;
    }
    let direction = if difference > 0 { "behind" } else { "ahead of" };
    Some(VerdictFinding {
        title: "Clock skew beyond threshold",
        details: format!(
            "the PC's clock differs from Admin's by {} s ({} s {direction} \
             Admin). Fix the PC's clock or its time sync before chasing a \
             phantom timing bug.",
            difference / 1_000,
            difference.abs() / 1_000,
        ),
    })
}

/// Rule 6: coverage contradiction — the stored Client Guard Coverage report
/// reads `unavailable` while the Guard's fresh poll reports the Client
/// connected. Two contradictory observations sampled at different times: the
/// finding states both and explicitly declines to name a cause, because the
/// observations alone do not prove which report is wrong (issue #368).
fn rule_coverage_contradiction(
    facts: Option<&PcFacts>,
    guard_facts: Option<&GuardFacts>,
    generated_at: DateTime<Utc>,
) -> Option<VerdictFinding> {
    let facts = facts?;
    if facts.last_guard_coverage.as_deref().map(str::trim) != Some("unavailable") {
        return None;
    }
    let guard = guard_facts?;
    let age = guard_poll_age_secs(generated_at, guard);
    if age > GUARD_POLL_STALE_AFTER_SECS || !guard.client_connected {
        return None;
    }
    Some(VerdictFinding {
        title: "Coverage contradiction",
        details: format!(
            "the stored Client Guard Coverage report reads `unavailable`, \
             while the Guard's poll {age} s ago reported the Client \
             connected. These observations were sampled at different times \
             and contradict each other; the observations alone do not prove \
             which report is wrong or what caused the mismatch.",
        ),
    })
}

/// Read one `key=value` tracing-context token out of a forwarded message,
/// e.g. `expected_image` from "rejecting foreign pipe client (process_id=99
/// actual_image=… expected_image=… suppressed_connections=2)". The value runs
/// to the next `identifier=` field boundary, so a path containing spaces
/// survives; a value that itself contains `identifier=` text would end it
/// early, which a plain-language summary tolerates — the raw appendix still
/// carries the exact line. A trailing `)` is the context wrapper, not part of
/// the value, and is trimmed.
fn message_field<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let start = message.find(&needle)? + needle.len();
    let rest = &message[start..];
    let end = rest
        .char_indices()
        .skip(1)
        .find(|(index, character)| *character == ' ' && is_field_boundary(&rest[*index + 1..]))
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    let value = &rest[..end];
    Some(value.strip_suffix(')').unwrap_or(value))
}

/// Whether `text` opens with an `identifier=` tracing field (lowercase snake
/// case, as tracing field names are).
fn is_field_boundary(text: &str) -> bool {
    let mut chars = text.char_indices();
    let Some((_, first)) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    for (index, character) in chars {
        if !(character.is_ascii_lowercase() || character == '_' || character.is_ascii_digit()) {
            return text[index..].starts_with('=');
        }
    }
    false
}

// ── Appendix selection ────────────────────────────────────────────────

fn appendix_entries<'a>(entries: &'a [LogEntry], window: &WindowInfo) -> (Vec<&'a LogEntry>, bool) {
    let mut in_window: Vec<&LogEntry> = entries
        .iter()
        .filter(|entry| timestamp_in_window(&entry.timestamp, window))
        .collect();
    in_window.sort_by(|a, b| timestamp_order(&a.timestamp, &b.timestamp));
    keep_newest(in_window, APPENDIX_LINE_CAP)
}

// ── Environment section ───────────────────────────────────────────────

fn build_environment_section(pc_id: &str, facts: &Option<PcFacts>, db_failed: bool) -> String {
    let cell = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("`{}`", markdown_cell(value)))
            .unwrap_or_else(|| "—".to_owned())
    };
    let (name, status, registered_ip, mac_address, last_seen_at, last_guard_coverage) = match facts
    {
        Some(facts) => (
            format!("`{}`", markdown_cell(&facts.name)),
            format!("`{}`", markdown_cell(&facts.status)),
            cell(&facts.registered_ip),
            cell(&facts.mac_address),
            cell(&facts.last_seen_at),
            cell(&facts.last_guard_coverage),
        ),
        None => (
            "`unknown PC`".to_owned(),
            "—".to_owned(),
            "—".to_owned(),
            "—".to_owned(),
            "—".to_owned(),
            "—".to_owned(),
        ),
    };
    // A failed SQLite query must be stated, never read as silence.
    let sqlite_context = if db_failed {
        "`query failed — the document may be missing context`".to_owned()
    } else {
        "`complete`".to_owned()
    };

    format!(
        r#"## Environment

| Key | Value |
|---|---|
| PC | `{pc}` |
| PC Name | {name} |
| Status | {status} |
| Registered IP | {registered_ip} |
| MAC | {mac_address} |
| Last Heartbeat | {last_seen_at} |
| Client Guard Coverage | {last_guard_coverage} |
| SQLite Context | {sqlite_context} |
"#,
        pc = markdown_cell(pc_id),
    )
}

// ── Admin Host section (ADR-0082) ─────────────────────────────────────

/// Render Admin's own health from the same `HostDiagnostics` the
/// host-diagnostics endpoint serves (#378): the Run Records newest first,
/// each with its version, PID, start, end, and Exit Reason — a start with no
/// end reads as a suspected hard exit unless it is the run that produced
/// this bundle — plus the panic file's presence, last-modified time, and its
/// newest 8 KB, stated as a slice when the file is larger.
fn build_host_section(host: &HostDiagnostics) -> String {
    let mut section = String::from(
        "## Admin Host\n\nAdmin's own health (ADR-0082), newest Run Record first.\n\n",
    );
    if host.runs.is_empty() {
        section.push_str("No Run Records on disk.\n\n");
    } else {
        section.push_str("| # | Version | PID | Started | Ended | Exit Reason |\n");
        section.push_str("|---|---|---|---|---|---|\n");
        for (index, run) in host.runs.iter().enumerate() {
            let (ended, exit) = match (&run.ended_at, run.suspected_hard_exit) {
                (Some(ended), _) => (
                    truncate_timestamp(ended),
                    run.exit_reason
                        .map(|reason| reason.as_str().to_owned())
                        .unwrap_or_else(|| "reason not recorded".to_owned()),
                ),
                (None, true) => ("—".to_owned(), "suspected hard exit".to_owned()),
                (None, false) => ("—".to_owned(), "still running".to_owned()),
            };
            section.push_str(&format!(
                "| {} | `{}` | {} | {} | {} | {} |\n",
                index + 1,
                markdown_cell(&run.version),
                run.pid,
                truncate_timestamp(&run.started_at),
                ended,
                exit,
            ));
        }
        section.push('\n');
    }

    let panic_file = &host.panic_file;
    if !panic_file.exists {
        section.push_str("**Panic file** (`logs/panic.log`): not present.\n");
        return section;
    }
    let last_written = panic_file
        .last_modified_at
        .as_deref()
        .map(truncate_timestamp)
        .unwrap_or_else(|| "unknown".to_owned());
    match &panic_file.newest_8kb {
        Some(slice) => {
            let scope = if panic_file.size_bytes > crate::host_diagnostics::PANIC_SLICE_BYTES as u64
            {
                format!(
                    "newest 8 KB of {} bytes (older bytes stay on disk)",
                    panic_file.size_bytes
                )
            } else {
                format!("full file, {} bytes", panic_file.size_bytes)
            };
            section.push_str(&format!(
                "**Panic file** (`logs/panic.log`): present, last written {last_written}; {scope}:\n\n```text\n{slice}\n```\n",
            ));
        }
        None => {
            section.push_str(&format!(
                "**Panic file** (`logs/panic.log`): present, last written \
                 {last_written}, but its bytes could not be read.\n",
            ));
        }
    }
    section
}

// ── Timeline section ──────────────────────────────────────────────────

fn build_timeline_section(rows: &[TimelineRow], window: &WindowInfo) -> String {
    if rows.is_empty() {
        return "## Timeline\n\nNo matching evidence in the selected window.\n".to_owned();
    }

    let mut table = format!(
        "## Timeline\n\nWindow: {} to {} | {} row(s) shown\n\n",
        window.from,
        window.to,
        rows.len()
    );

    table.push_str("| # | Timestamp | Source | Level | Event |\n");
    table.push_str("|---|---|---|---|---|\n");

    for (i, row) in rows.iter().enumerate() {
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            i + 1,
            truncate_timestamp(&row.timestamp),
            row.source,
            markdown_cell(&row.level),
            markdown_cell(&row.event),
        ));
    }

    table
}

// ── Anomalies section ─────────────────────────────────────────────────

fn build_anomalies_section(anomalies: &[AnomalyEntry]) -> String {
    if anomalies.is_empty() {
        return "## Anomalies\n\nNo anomalies detected.\n".to_owned();
    }

    let mut section = format!(
        "## Anomalies\n\n{} anomaly(ies) detected\n\n",
        anomalies.len()
    );

    section.push_str("| # | Kind | Timestamp | Details |\n");
    section.push_str("|---|---|---|---|\n");

    for (i, anomaly) in anomalies.iter().enumerate() {
        section.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            i + 1,
            anomaly.kind,
            truncate_timestamp(&anomaly.timestamp),
            markdown_cell(&anomaly.details),
        ));
    }

    section
}

// ── Appendix section (JSONL) ──────────────────────────────────────────

fn build_appendix_section(entries: &[&LogEntry], replacements: &[(String, String)]) -> String {
    if entries.is_empty() {
        return "## Raw Appendix\n\nNo log entries available.\n".to_owned();
    }

    let mut section = "## Raw Appendix\n\n```jsonl\n".to_owned();

    // Re-serialise from whitelist fields only; the parsed JSON object is
    // never passed through. The one field that carries free text —
    // `message` — is pseudonymised with the same per-bundle label map the
    // timeline uses, so a username cannot survive in the raw appendix.
    for entry in entries {
        let mut fields = entry.raw_fields.clone();
        if let Some(Value::String(message)) = fields.get_mut("message") {
            *message = replace_usernames(message, replacements);
        }
        if let Some(json) = serialise_appendix_entry(&fields) {
            section.push_str(&json);
            section.push('\n');
        }
    }

    section.push_str("```\n");
    section
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_record::{ExitReason, RunRecord, RunRecordStore};
    use openpiso_core::forwarding::FORWARDING_BUFFER_TARGET;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::fs::{self, File};
    use std::io::Write;

    const ADMIN_TARGET: &str = "openpiso_admin_lib::sessions";

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect("sqlite::memory:")
            .await
            .expect("test database should connect");
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await
            .expect("test migration should run");
        crate::db::seed(&pool).await.expect("test data should seed");
        pool
    }

    fn rfc3339_seconds_ago(seconds: u64) -> String {
        format_timestamp(Utc::now() - chrono::Duration::seconds(seconds as i64))
    }

    fn forwarded_line(
        source: &str,
        pc_id: &str,
        message: &str,
        seconds_ago: u64,
        extra: &str,
    ) -> String {
        forwarded_entry(
            source,
            pc_id,
            "warn",
            "openpiso_client::guard_client",
            message,
            seconds_ago,
            extra,
        )
    }

    /// A Forwarded Diagnostic fixture line in the exact shape the Admin sink
    /// writes: one JSON object per line with the device event time and the
    /// source discriminator. `message` must already be JSON-escaped (a
    /// Windows path needs its backslashes doubled).
    fn forwarded_entry(
        source: &str,
        pc_id: &str,
        level: &str,
        target: &str,
        message: &str,
        seconds_ago: u64,
        extra: &str,
    ) -> String {
        format!(
            r#"{{"received_at":"{}","timestamp":"{}","level":"{}","target":"{}","message":"{}","pc_id":"{}","source":"{}"{}}}"#,
            rfc3339_seconds_ago(0),
            rfc3339_seconds_ago(seconds_ago),
            level,
            target,
            message,
            pc_id,
            source,
            extra
        )
    }

    /// A Guard Poll snapshot as the endpoint would hand it over (ADR-0077):
    /// in-memory facts no log row records.
    fn guard_poll(seconds_ago: i64, client_connected: bool) -> GuardFacts {
        GuardFacts {
            received_at: Utc::now() - chrono::Duration::seconds(seconds_ago),
            client_connected,
            disarmed: false,
            relaunch_attempts: 0,
        }
    }

    fn admin_line(pc_id: &str, message: &str, seconds_ago: u64, extra: &str) -> String {
        format!(
            r#"{{"timestamp":"{}","level":"WARN","target":"{ADMIN_TARGET}","message":"{}","pc_id":"{}"{}}}"#,
            rfc3339_seconds_ago(seconds_ago),
            message,
            pc_id,
            extra
        )
    }

    fn write_log(dir: &Path, prefix: &str, lines: &[String]) {
        fs::create_dir_all(dir).expect("log dir should be created");
        let mut file = File::create(dir.join(format!("{prefix}.2026-09-05.log")))
            .expect("log file should be created");
        for line in lines {
            writeln!(file, "{line}").expect("log line should be written");
        }
    }

    async fn stamp_pc_facts(pool: &SqlitePool) {
        sqlx::query(
            "UPDATE pcs SET registered_ip = '192.168.4.10',
                    mac_address = 'aa:bb:cc:dd:ee:01',
                    last_seen_at = datetime('now'),
                    last_guard_coverage = 'unavailable'
             WHERE id = 'pc-01'",
        )
        .execute(pool)
        .await
        .expect("pc facts should update");
    }

    async fn insert_account(pool: &SqlitePool, id: &str, username: &str) {
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash)
             VALUES (?, ?, ?, 'test-hash')",
        )
        .bind(id)
        .bind(username)
        .bind(username)
        .execute(pool)
        .await
        .expect("account should insert");
    }

    async fn insert_audit_event(pool: &SqlitePool, account_id: &str, summary: &str) {
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, account_id, pc_id,
                                    points_delta_centi, cash_flag, summary, metadata_json)
             VALUES (?, 'points_transfer_requested', 'success', 'account', ?, 'pc-01',
                     0, 0, ?, '{}')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(account_id)
        .bind(summary)
        .execute(pool)
        .await
        .expect("audit event should insert");
    }

    fn section<'a>(document: &'a str, marker: &str) -> &'a str {
        let start = document
            .find(marker)
            .unwrap_or_else(|| panic!("section {marker} should be present"));
        let rest = &document[start + marker.len()..];
        let end = rest
            .find("\n\n---\n")
            .map(|offset| start + marker.len() + offset)
            .unwrap_or(document.len());
        &document[start..end]
    }

    fn header(document: &str) -> &str {
        &document[..document.find("\n\n---\n").expect("header should end")]
    }

    #[tokio::test]
    async fn merges_all_four_sources_into_one_ordered_timeline() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        write_log(
            admin_dir.path(),
            "diagnostics",
            &[
                admin_line("pc-01", "Admin saw this PC wobble", 60, ""),
                // A different PC's line must not join pc-01's bundle.
                admin_line("pc-02", "Other PC trouble", 60, ""),
            ],
        );
        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_line("client", "pc-01", "connected to Guard pipe", 120, ""),
                forwarded_line("guard", "pc-01", "Client connected", 90, ""),
                // Forwarded lines for another PC must not join either.
                forwarded_line("client", "pc-02", "unrelated", 60, ""),
            ],
        );

        let _ = crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            crate::db::monitoring::IncidentKind::ClientGuardCoverage,
        )
        .await
        .expect("incident should open");
        crate::db::monitoring::close_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            crate::db::monitoring::IncidentKind::ClientGuardCoverage,
            crate::db::monitoring::IncidentCloseReason::GuardRecovered,
        )
        .await
        .expect("incident should close");

        let account_id = uuid::Uuid::new_v4().to_string();
        insert_account(&pool, &account_id, "marlon").await;
        insert_audit_event(&pool, &account_id, "Session started for marlon").await;

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        assert!(document.starts_with("# Support Bundle"));
        let (environment, timeline, anomalies, appendix) = (
            document.find("## Environment").expect("Environment"),
            document.find("## Timeline").expect("Timeline"),
            document.find("## Anomalies").expect("Anomalies"),
            document.find("## Raw Appendix").expect("Appendix"),
        );
        assert!(environment < timeline && timeline < anomalies && anomalies < appendix);

        let timeline = section(&document, "## Timeline");
        assert!(timeline.contains("Admin saw this PC wobble"));
        assert!(timeline.contains("connected to Guard pipe"));
        assert!(timeline.contains("Client connected"));
        assert!(
            !timeline.contains("Other PC trouble"),
            "another PC's Admin line must be excluded"
        );
        assert!(
            !timeline.contains("unrelated"),
            "another PC's forwarded line must be excluded"
        );
        assert!(timeline.contains("Session started for Account A"));
        assert!(timeline.contains("Client Guard Coverage opened"));
        assert!(timeline.contains("closed (guard_recovered)"));
        // The incident edges stand in for their Audit events.
        assert!(!timeline.contains("pc_monitoring_incident_opened"));

        // An open-and-closed episode is not an anomaly; no drop markers in
        // this fixture either.
        let anomalies = section(&document, "## Anomalies");
        assert!(anomalies.contains("No anomalies detected"), "{anomalies}");

        let appendix = section(&document, "## Raw Appendix");
        assert!(appendix.contains("Admin saw this PC wobble"));
        assert!(appendix.contains("connected to Guard pipe"));
        assert!(appendix.contains(r#""source":"client""#));
        assert!(!appendix.contains("Other PC trouble"));
        assert!(!appendix.contains("unrelated"));
        assert!(!appendix.contains("pc_monitoring_incident"));
    }

    #[tokio::test]
    async fn a_diagnostic_request_slice_lands_in_the_bundle_with_its_truncation_statements() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        // A Diagnostic Request slice exactly as the Client uploads it: the
        // frame, a cap statement, and faithful full-log evidence — including
        // a DEBUG line — on target `openpiso_client::diagnostic_request` for
        // the frame lines (issue #364).
        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::diagnostic_request",
                    "Diagnostic Request slice (nonce abc-123): window 09:45:00Z to 10:00:00Z UTC, 2 of 9 window lines uploaded",
                    60,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::diagnostic_request",
                    "Diagnostic Request slice (nonce abc-123): byte cap truncated the slice; 7 older window lines were dropped",
                    60,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::diagnostic_request",
                    "Diagnostic Request slice (nonce abc-123): the window excluded 4 earlier log lines",
                    60,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "debug",
                    "openpiso_client::test",
                    "quiet evidence nobody forwarded",
                    120,
                    "",
                ),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let timeline = section(&document, "## Timeline");
        assert!(
            timeline.contains("Diagnostic Request slice (nonce abc-123)"),
            "the slice's frame is the bundle's statement of what was asked and cut"
        );
        assert!(
            timeline.contains("byte cap truncated the slice; 7 older window lines were dropped"),
            "cap truncation is stated: {timeline}"
        );
        assert!(
            timeline.contains("the window excluded 4 earlier log lines"),
            "window truncation is stated: {timeline}"
        );
        assert!(
            timeline.contains("quiet evidence nobody forwarded"),
            "the slice's evidence joins the timeline: {timeline}"
        );
        // DEBUG stays DEBUG, never relabelled INFO.
        assert!(timeline.contains("| debug |"));
        let appendix = section(&document, "## Raw Appendix");
        assert!(appendix.contains("quiet evidence nobody forwarded"));
        assert!(appendix.contains(r#""level":"debug""#));
    }

    #[tokio::test]
    async fn environment_states_the_stored_pc_facts() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let environment = section(&document, "## Environment");
        assert!(environment.contains("PC 01"));
        assert!(environment.contains("192.168.4.10"));
        assert!(environment.contains("aa:bb:cc:dd:ee:01"));
        assert!(environment.contains("Last Heartbeat"));
        assert!(!environment.contains("Last Heartbeat | —"));
        assert!(environment.contains("unavailable"));
    }

    #[tokio::test]
    async fn unknown_pc_renders_unknown_facts_without_failing() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-unknown",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let environment = section(&document, "## Environment");
        assert!(environment.contains("unknown PC"));
        assert!(document.contains("No matching evidence in the selected window."));
        assert!(document.contains("No anomalies detected."));
        assert!(document.contains("No log entries available."));
    }

    #[tokio::test]
    async fn device_secret_in_a_fixture_line_never_reaches_the_output() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        // The seeded coin device's secret is `simulator-only`; no query in
        // this builder selects `coin_devices`, and a leaked field on a fixture
        // line must be dropped by the appendix whitelist.
        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[forwarded_line(
                "guard",
                "pc-01",
                "harmless line",
                30,
                r#","device_secret":"simulator-only""#,
            )],
        );
        write_log(
            admin_dir.path(),
            "diagnostics",
            &[admin_line(
                "pc-01",
                "admin line",
                30,
                r#","secret":"simulator-only""#,
            )],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        assert!(
            !document.contains("simulator-only"),
            "a device secret must never appear: {document}"
        );
    }

    #[tokio::test]
    async fn usernames_are_replaced_by_stable_per_bundle_labels() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let sender = uuid::Uuid::new_v4().to_string();
        let recipient = uuid::Uuid::new_v4().to_string();
        insert_account(&pool, &sender, "marlon").await;
        insert_account(&pool, &recipient, "pedrito").await;

        // The first summary names the recipient outright; the second carries
        // the recipient's id in metadata. Both must be pseudonymised, with
        // the same account keeping the same label across both rows.
        insert_audit_event(&pool, &sender, "Requested 5-point transfer to pedrito").await;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, account_id, pc_id,
                                    points_delta_centi, cash_flag, summary, metadata_json)
             VALUES (?, 'points_transfer_requested', 'success', 'account', ?, 'pc-01',
                     0, 0, 'Confirmed transfer to pedrito', ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&sender)
        .bind(serde_json::json!({ "to_account_id": recipient }).to_string())
        .execute(&pool)
        .await
        .expect("audit event with metadata should insert");

        // An Admin line carrying the sender's account id too.
        write_log(
            admin_dir.path(),
            "diagnostics",
            &[admin_line(
                "pc-01",
                "session event for marlon",
                30,
                &format!(r#","account_id":"{sender}""#),
            )],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        assert!(
            !document.contains("marlon"),
            "username must not appear: {document}"
        );
        assert!(
            !document.contains("pedrito"),
            "metadata-named username must not appear: {document}"
        );
        assert!(document.contains("Account A"));
        assert!(document.contains("Account B"));
        assert!(document.contains("Requested 5-point transfer to Account B"));
        assert!(document.contains("Confirmed transfer to Account B"));
        assert!(document.contains("session event for Account A"));
    }

    #[tokio::test]
    async fn appendix_drops_fields_outside_the_whitelist() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[forwarded_line(
                "client",
                "pc-01",
                "keep me",
                30,
                r#","internal_token":"nope""#,
            )],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let appendix = section(&document, "## Raw Appendix");
        assert!(appendix.contains("keep me"));
        assert!(appendix.contains(r#""pc_id":"pc-01""#));
        assert!(
            !appendix.contains("internal_token"),
            "non-whitelisted fields must be dropped: {appendix}"
        );
        assert!(!document.contains("nope"));
    }

    #[tokio::test]
    async fn forwarded_drop_markers_surface_as_visible_gaps() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_line("client", "pc-01", "before the overflow", 60, ""),
                // The real drop marker's target is the synthetic forwarding
                // target, not a device log target.
                format!(
                    r#"{{"received_at":"{}","timestamp":"{}","level":"warn","target":"{FORWARDING_BUFFER_TARGET}","message":"3 forwarded diagnostics dropped: device buffer overflow","pc_id":"pc-01","source":"client"}}"#,
                    rfc3339_seconds_ago(50),
                    rfc3339_seconds_ago(50)
                ),
                forwarded_line("client", "pc-01", "after the overflow", 40, ""),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let timeline = section(&document, "## Timeline");
        assert!(
            timeline.contains("⚠ 3 forwarded diagnostics dropped: device buffer overflow"),
            "the gap must be visible in the timeline: {timeline}"
        );
        assert!(timeline.contains("| gap |"));
        let anomalies = section(&document, "## Anomalies");
        assert!(
            anomalies.contains("forwarded_lines_dropped"),
            "the drop must be an anomaly too: {anomalies}"
        );
        assert!(anomalies.contains("3 forwarded diagnostic line(s) dropped"));
    }

    #[test]
    fn drop_marker_target_is_what_makes_a_gap() {
        let marker_entry = entry_from_parsed(
            parse_diagnostic_json_line(
                &format!(
                    r#"{{"timestamp":"{}","level":"warn","target":"{FORWARDING_BUFFER_TARGET}","message":"2 forwarded diagnostics dropped: device buffer overflow","pc_id":"pc-01","source":"client"}}"#,
                    rfc3339_seconds_ago(10)
                ),
                APPENDIX_FIELD_WHITELIST,
            )
            .expect("marker line should parse"),
            Some(ForwardedSource::Client),
        );
        assert!(marker_entry.is_drop_marker);

        let ordinary = entry_from_parsed(
            parse_diagnostic_json_line(
                &format!(
                    r#"{{"timestamp":"{}","level":"warn","target":"openpiso_client","message":"dropped something","pc_id":"pc-01","source":"client"}}"#,
                    rfc3339_seconds_ago(10)
                ),
                APPENDIX_FIELD_WHITELIST,
            )
            .expect("ordinary line should parse"),
            Some(ForwardedSource::Client),
        );
        assert!(!ordinary.is_drop_marker);
    }

    #[tokio::test]
    async fn open_incident_is_an_anomaly_even_when_its_window_has_passed() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let _ = crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            crate::db::monitoring::IncidentKind::ClientReachabilityMismatch,
        )
        .await
        .expect("incident should open");

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                window_seconds: 60,
                ..Default::default()
            },
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let anomalies = section(&document, "## Anomalies");
        assert!(anomalies.contains("open_incident"));
        assert!(anomalies.contains("Client Reachability Mismatch open"));
    }

    #[tokio::test]
    async fn entries_outside_the_window_are_excluded() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        write_log(
            admin_dir.path(),
            "diagnostics",
            &[
                admin_line("pc-01", "ten minutes old", 600, ""),
                admin_line("pc-01", "one minute old", 60, ""),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                window_seconds: 300,
                ..Default::default()
            },
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let timeline = section(&document, "## Timeline");
        assert!(timeline.contains("one minute old"));
        assert!(!timeline.contains("ten minutes old"));
        assert!(
            header(&document).contains("Truncation:"),
            "the window policy must be stated"
        );
    }

    #[test]
    fn anchored_window_spans_one_hour_before_to_fifteen_minutes_after() {
        let anchor = Utc::now();
        let window = compute_window_info(86_400, Some(anchor), anchor);
        assert_eq!(
            window.from,
            format_timestamp(anchor - chrono::Duration::seconds(ANCHOR_BEFORE_SECONDS))
        );
        assert_eq!(
            window.to,
            format_timestamp(anchor + chrono::Duration::seconds(ANCHOR_AFTER_SECONDS))
        );
    }

    /// An Admin diagnostic line stamped at an absolute instant, for anchored
    /// windows that cannot be expressed as "seconds ago".
    fn admin_line_at(pc_id: &str, message: &str, timestamp: &str) -> String {
        format!(
            r#"{{"timestamp":"{timestamp}","level":"WARN","target":"{ADMIN_TARGET}","message":"{message}","pc_id":"{pc_id}"}}"#
        )
    }

    fn sqlite_timestamp(at: chrono::DateTime<Utc>) -> String {
        at.format("%Y-%m-%d %H:%M:%S").to_string()
    }

    #[tokio::test]
    async fn incident_anchor_windows_the_bundle_around_the_incident_not_now() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        // The owner opens the Notifications hours later; the anchor sits
        // three hours back, so "backwards from now" would return only quiet.
        let anchor = Utc::now() - chrono::Duration::hours(3);
        let at = |offset_seconds: i64| {
            format_timestamp(anchor + chrono::Duration::seconds(offset_seconds))
        };
        write_log(
            admin_dir.path(),
            "diagnostics",
            &[
                admin_line_at("pc-01", "guard pipe dropped", &at(-1_800)),
                admin_line_at("pc-01", "guard recovered", &at(600)),
                // Fresh noise after the anchored window's end must stay out.
                admin_line_at("pc-01", "fresh noise", &at(3 * 3_600 - 60)),
            ],
        );

        // The anchored incident itself, closed twenty minutes after it
        // opened — after the window's fifteen-minute tail.
        let _ = crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            crate::db::monitoring::IncidentKind::ClientGuardCoverage,
        )
        .await
        .expect("incident should open");
        crate::db::monitoring::close_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            crate::db::monitoring::IncidentKind::ClientGuardCoverage,
            crate::db::monitoring::IncidentCloseReason::GuardRecovered,
        )
        .await
        .expect("incident should close");
        sqlx::query("UPDATE pc_monitoring_incidents SET opened_at = ?, closed_at = ?")
            .bind(sqlite_timestamp(anchor - chrono::Duration::seconds(30)))
            .bind(sqlite_timestamp(anchor + chrono::Duration::seconds(1_200)))
            .execute(&pool)
            .await
            .expect("incident edges should be restamped");

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                window_anchor: Some(anchor),
                ..Default::default()
            },
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let timeline = section(&document, "## Timeline");
        assert!(timeline.contains("guard pipe dropped"));
        assert!(timeline.contains("guard recovered"));
        assert!(!timeline.contains("fresh noise"));
        // The anchored incident is the evidence the owner came for; it stays
        // even though it closed after the window's tail.
        assert!(timeline.contains("Client Guard Coverage opened"));
        let coverage = header(&document);
        assert!(coverage.contains(&at(-ANCHOR_BEFORE_SECONDS)));
        assert!(coverage.contains(&at(ANCHOR_AFTER_SECONDS)));
    }

    #[tokio::test]
    async fn oversized_document_still_renders_and_states_its_truncation() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let lines: Vec<String> = (0..80)
            .map(|index| admin_line("pc-01", &format!("noise line {index:03}"), index as u64, ""))
            .collect();
        write_log(admin_dir.path(), "diagnostics", &lines);

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                size_cap: 1_200,
                ..Default::default()
            },
            None,
            &HostDiagnostics::default(),
        )
        .await;

        assert!(document.len() <= 1_200, "hard byte ceiling must hold");
        assert!(document.is_char_boundary(document.len()));
        assert!(document.starts_with("# Support Bundle"));
        assert!(document.contains("## Environment"));
        assert!(document.contains("## Timeline"));
        assert!(
            header(&document).contains("appendix"),
            "the sacrificed appendix must be stated"
        );
    }

    // ── Admin Host section tests (ADR-0082 / #378) ──────────────────────
    //
    // The section renders from the same collector the host-diagnostics
    // endpoint serves, so the owner's screen and this bundle cannot
    // disagree.

    fn seed_record(
        version: &str,
        pid: u32,
        started: chrono::DateTime<Utc>,
        ended: Option<chrono::DateTime<Utc>>,
    ) -> RunRecord {
        RunRecord {
            version: version.to_owned(),
            pid,
            started_at: started,
            ended_at: ended,
            exit_reason: ended.map(|_| ExitReason::ApplicationExit),
            app_update_result: None,
        }
    }

    #[tokio::test]
    async fn the_admin_host_section_carries_the_run_records_and_panic_slice() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let data_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let base = Utc::now() - chrono::Duration::hours(6);
        crate::test_utils::seed_run_records(
            data_dir.path(),
            &[
                seed_record("0.8.0", 100, base, Some(base + chrono::Duration::hours(1))),
                // A start with no end on a past run: suspected hard exit.
                seed_record("0.9.0", 424_242, base + chrono::Duration::hours(2), None),
            ],
        );
        let logs_dir = data_dir.path().join("logs");
        let size_bytes = crate::test_utils::write_panic_slice_fixture(&logs_dir);
        assert!(size_bytes > crate::host_diagnostics::PANIC_SLICE_BYTES as u64);

        let store = RunRecordStore::new(
            data_dir
                .path()
                .join(crate::run_record::RUN_RECORDS_FILE_NAME),
            std::sync::Arc::new(crate::run_record::SystemClock),
        );
        // collect reads the panic file from the logs directory, exactly as
        // the endpoint does.
        let host = crate::host_diagnostics::collect(&store, &logs_dir);

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &host,
        )
        .await;

        let host_section = section(&document, "## Admin Host");
        assert!(host_section.contains("newest Run Record first"));
        assert!(host_section.contains("| 1 | `0.9.0` | 424242 |"));
        assert!(host_section.contains("suspected hard exit"));
        assert!(host_section.contains("| 2 | `0.8.0` | 100 |"));
        assert!(host_section.contains("application exit"));
        assert!(
            host_section.contains(&format!("newest 8 KB of {size_bytes} bytes")),
            "a file above the cap is stated as a slice: {host_section}"
        );
        assert!(host_section.contains("TAIL-MARKER crash evidence"));
        assert!(
            !host_section.contains("OLD-EVIDENCE"),
            "bytes older than the newest 8 KB stay on disk: {host_section}"
        );
    }

    #[tokio::test]
    async fn absent_host_evidence_and_the_live_run_render_plainly() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let host_section = section(&document, "## Admin Host");
        assert!(host_section.contains("No Run Records on disk."));
        assert!(host_section.contains("**Panic file** (`logs/panic.log`): not present."));

        // The live run's own unfinished record is a run in progress, never a
        // suspected hard exit.
        let data_dir = tempfile::tempdir().expect("temp dir");
        let store = RunRecordStore::new(
            data_dir
                .path()
                .join(crate::run_record::RUN_RECORDS_FILE_NAME),
            std::sync::Arc::new(crate::run_record::SystemClock),
        );
        store
            .start(env!("CARGO_PKG_VERSION"))
            .expect("the live run's record starts");
        let host = crate::host_diagnostics::collect(&store, data_dir.path());
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &host,
        )
        .await;
        let host_section = section(&document, "## Admin Host");
        assert!(host_section.contains("still running"));
        assert!(!host_section.contains("suspected hard exit"));
        assert!(host_section.contains("**Panic file** (`logs/panic.log`): not present."));
    }

    #[tokio::test]
    async fn the_cap_cuts_the_timeline_and_states_it_rather_than_the_host_slice() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let data_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        crate::test_utils::seed_run_records(
            data_dir.path(),
            &[seed_record("0.9.0", 424_242, Utc::now(), Some(Utc::now()))],
        );
        let logs_dir = data_dir.path().join("logs");
        let size_bytes = crate::test_utils::write_panic_slice_fixture(&logs_dir);
        let store = RunRecordStore::new(
            data_dir
                .path()
                .join(crate::run_record::RUN_RECORDS_FILE_NAME),
            std::sync::Arc::new(crate::run_record::SystemClock),
        );
        let host = crate::host_diagnostics::collect(&store, &logs_dir);

        // Enough noise that the appendix and the oldest timeline rows must
        // go before the byte ceiling is met — the Admin Host evidence is
        // never quietly traded for them.
        let lines: Vec<String> = (0..100)
            .map(|index| admin_line("pc-01", &format!("noise line {index:03}"), index as u64, ""))
            .collect();
        write_log(admin_dir.path(), "diagnostics", &lines);

        let cap = 10_000;
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                size_cap: cap,
                ..Default::default()
            },
            None,
            &host,
        )
        .await;

        assert!(document.len() <= cap, "hard byte ceiling must hold");
        let host_section = section(&document, "## Admin Host");
        assert!(
            host_section.contains(&format!("newest 8 KB of {size_bytes} bytes")),
            "the slice and its stated cut survive the cap: {host_section}"
        );
        assert!(host_section.contains("TAIL-MARKER crash evidence"));
        let truncation = header(&document);
        assert!(truncation.contains("appendix"), "{truncation}");
        assert!(
            truncation.contains("timeline"),
            "the timeline cut is stated, not silent: {truncation}"
        );
    }

    #[tokio::test]
    async fn timeline_cap_keeps_the_newest_rows_and_states_it() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        let lines: Vec<String> = (0..220)
            .map(|index| {
                admin_line(
                    "pc-01",
                    &format!("tick {index:03}"),
                    (600 - index) as u64,
                    "",
                )
            })
            .collect();
        write_log(admin_dir.path(), "diagnostics", &lines);

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let timeline = section(&document, "## Timeline");
        assert!(timeline.contains("200 row(s) shown"));
        assert!(timeline.contains("tick 219"));
        assert!(!timeline.contains("tick 000"));
        assert!(
            header(&document).contains("timeline"),
            "timeline truncation must be stated"
        );
    }

    #[test]
    fn boundary_check_never_splits_words() {
        // "ann" inside "channel" must survive; "ann" standalone must be
        // replaced.
        let mut labels = AccountLabelMap::default();
        labels.label("acct-1");
        let mut usernames = HashMap::new();
        usernames.insert("acct-1".to_owned(), "ann".to_owned());
        let replacements = username_replacements(&usernames, &labels);
        assert_eq!(
            replace_usernames("channel 9 and ann", &replacements),
            "channel 9 and Account A"
        );
        assert_eq!(
            replace_usernames("ann, anna and ann", &replacements),
            "Account A, anna and Account A"
        );
    }

    // ── Verdict tests (issue #361) ─────────────────────────────────────
    //
    // Every rule is tested through the rendered bundle: fixtures of real
    // forwarded lines plus the Guard Poll snapshot go in, the `## Verdict`
    // text an owner reads comes out.

    /// The Guard's real foreign-image rejection line: the paths ride the
    /// tracing context folded into the message, and the path itself contains
    /// a space.
    fn foreign_rejection_line(seconds_ago: u64, suppressed: u64) -> String {
        forwarded_entry(
            "guard",
            "pc-01",
            "warn",
            "openpiso_client_guard::pipe",
            &format!(
                r#"rejecting foreign pipe client (process_id=99 actual_image=C:\\Apps\\piso client (1).exe expected_image=C:\\Apps\\OpenPiso\\client.exe suppressed_connections={suppressed})"#
            ),
            seconds_ago,
            "",
        )
    }

    #[tokio::test]
    async fn foreign_image_rejection_names_both_paths_and_the_rejection_count() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                foreign_rejection_line(300, 0),
                foreign_rejection_line(240, 3),
            ],
        );
        write_log(
            admin_dir.path(),
            "diagnostics",
            &[admin_line("pc-01", "unrelated admin line", 60, "")],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        assert!(
            verdict.contains("Foreign Client image rejected by the Guard"),
            "{verdict}"
        );
        // Two logged lines plus three suppressed attempts.
        assert!(verdict.contains("5 connect attempt(s)"), "{verdict}");
        assert!(
            verdict.contains(r"expected image `C:\Apps\OpenPiso\client.exe`"),
            "{verdict}"
        );
        assert!(
            verdict.contains(r"actual image `C:\Apps\piso client (1).exe`"),
            "the path containing a space must survive: {verdict}"
        );
        assert!(!verdict.contains("Guard not running or not polling"));
        assert!(!verdict.contains("reconnect flap"));
        assert!(!verdict.contains("Coverage contradiction"));
    }

    #[tokio::test]
    async fn a_quiet_guard_with_a_stale_or_absent_poll_names_the_dead_guard() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            // The Guard's last poll is long past the freshness bound.
            Some(&guard_poll(400, false)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        assert!(
            verdict.contains("Guard not running or not polling"),
            "{verdict}"
        );
        assert!(
            verdict.contains("beyond the 35 s freshness bound"),
            "{verdict}"
        );
        assert!(
            verdict.contains("no Guard lines were forwarded"),
            "{verdict}"
        );
        assert!(!verdict.contains("Coverage contradiction"));

        // A Guard with no poll on record is the same shape, stated without
        // claiming history: an Admin restart resets the in-memory registry.
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;
        let verdict = section(&document, "## Verdict");
        assert!(verdict.contains("Guard not running or not polling"));
        assert!(
            verdict.contains("Admin has no Guard Poll record"),
            "{verdict}"
        );
    }

    #[tokio::test]
    async fn reconnect_flap_reports_the_share_of_the_window_spent_disconnected() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::guard_client",
                    "connected to Guard pipe (waited_ms=2)",
                    260,
                    "",
                ),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    240,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::guard_client",
                    "connected to Guard pipe (waited_ms=5)",
                    220,
                    "",
                ),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    200,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::guard_client",
                    "connected to Guard pipe (waited_ms=1)",
                    180,
                    "",
                ),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    160,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::guard_client",
                    "connected to Guard pipe (waited_ms=2)",
                    40,
                    "",
                ),
                forwarded_line("client", "pc-01", "Guard pipe closed; reconnecting", 20, ""),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                window_seconds: 300,
                ..Default::default()
            },
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        assert!(verdict.contains("Client reconnect flap"), "{verdict}");
        assert!(verdict.contains("3 time(s)"), "{verdict}");
        // Completed gaps plus the final ongoing 20 s disconnect.
        assert!(
            verdict.contains("60% of the window disconnected"),
            "{verdict}"
        );
        assert!(
            verdict.contains("180 s disconnected of a 300 s window"),
            "{verdict}"
        );
        assert!(!verdict.contains("Coverage contradiction"));
        assert!(!verdict.contains("Guard not running or not polling"));

        // Closes with no reconnect are not a flap: "repeated pipe close and
        // connect" needs both edges. The dead-Guard shape may name itself
        // instead; the flap must not.
        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    240,
                    "",
                ),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    200,
                    "",
                ),
            ],
        );
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                window_seconds: 300,
                ..Default::default()
            },
            None,
            &HostDiagnostics::default(),
        )
        .await;
        let verdict = section(&document, "## Verdict");
        assert!(
            !verdict.contains("reconnect flap"),
            "closes without reconnects are not a flap: {verdict}"
        );
        assert!(
            verdict.contains("Guard not running or not polling"),
            "{verdict}"
        );
    }

    #[tokio::test]
    async fn offline_shutdown_countdown_reports_the_arm_and_any_cancellation() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::offline_shutdown",
                    "Offline Shutdown Countdown armed: the admin server is unreachable (grace_seconds=120)",
                    120,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::offline_shutdown",
                    "Offline Shutdown Countdown cancelled: the admin server is reachable again",
                    60,
                    "",
                ),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        assert!(
            verdict.contains("Offline Shutdown Countdown armed"),
            "{verdict}"
        );
        assert!(verdict.contains("could not reach Admin"), "{verdict}");
        assert!(verdict.contains("cancelled again at"), "{verdict}");

        // Armed with no cancellation in the window must say so explicitly.
        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[forwarded_entry(
                "client",
                "pc-01",
                "info",
                "openpiso_client::offline_shutdown",
                "Offline Shutdown Countdown armed: the admin server is unreachable (grace_seconds=120)",
                120,
                "",
            )],
        );
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;
        let verdict = section(&document, "## Verdict");
        assert!(
            verdict.contains("no cancellation was recorded in the window"),
            "{verdict}"
        );
    }

    #[tokio::test]
    async fn clock_skew_beyond_threshold_is_named_with_its_direction() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        // Positive difference: the Client clock is behind Admin's.
        sqlx::query("UPDATE pcs SET clock_difference_ms = 130500 WHERE id = 'pc-01'")
            .execute(&pool)
            .await
            .expect("clock difference should update");
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;
        let verdict = section(&document, "## Verdict");
        assert!(verdict.contains("Clock skew beyond threshold"), "{verdict}");
        assert!(verdict.contains("130 s"), "{verdict}");
        assert!(verdict.contains("behind Admin"), "{verdict}");

        // Negative difference: the Client clock is ahead of Admin's.
        sqlx::query("UPDATE pcs SET clock_difference_ms = -130500 WHERE id = 'pc-01'")
            .execute(&pool)
            .await
            .expect("clock difference should update");
        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;
        let verdict = section(&document, "## Verdict");
        assert!(verdict.contains("ahead of Admin"), "{verdict}");
    }

    #[tokio::test]
    async fn coverage_contradiction_states_both_observations_without_a_cause() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            // The Guard reports a connected Client on a fresh poll while the
            // stored coverage report reads `unavailable`.
            Some(&guard_poll(5, true)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        assert!(verdict.contains("Coverage contradiction"), "{verdict}");
        // The finding states both observations: the stored `unavailable`
        // Heartbeat report and the recent connected Guard Poll...
        assert!(
            verdict.contains("the stored Client Guard Coverage report reads `unavailable`"),
            "{verdict}"
        );
        assert!(
            verdict.contains("while the Guard's poll 5 s ago reported the Client connected"),
            "{verdict}"
        );
        // ...without asserting the cause the observations cannot prove.
        assert!(
            verdict.contains("the observations alone do not prove which report is wrong"),
            "{verdict}"
        );
        assert!(!verdict.contains("reporting defect"), "{verdict}");
        assert!(!verdict.contains("not an outage"), "{verdict}");
        assert!(!verdict.contains("stale or wrong"), "{verdict}");
        assert!(!verdict.contains("Guard not running or not polling"));
        assert!(!verdict.contains("reconnect flap"));
    }

    /// The lifetime context a Client lifecycle line carries (issue #368)
    /// must survive into the rendered Timeline so contradictory evidence can
    /// be separated by process lifetime. A legacy line without the context
    /// still renders.
    #[tokio::test]
    async fn timeline_keeps_the_process_lifetime_context_of_lifecycle_lines() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                forwarded_line(
                    "client",
                    "pc-01",
                    "client startup milestone (milestone=setup_entered elapsed_ms=40 \
                     process_id=4242 launch=1a2b3c4d)",
                    600,
                    "",
                ),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Client Guard Coverage transitioned from covered to unavailable \
                     (process_id=4242 launch=1a2b3c4d)",
                    300,
                    "",
                ),
                // A legacy line from before the convention: no lifetime
                // fields, still rendered.
                forwarded_line(
                    "client",
                    "pc-01",
                    "connected to Guard pipe (waited_ms=2)",
                    100,
                    "",
                ),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            None,
            &HostDiagnostics::default(),
        )
        .await;

        let timeline = section(&document, "## Timeline");
        assert!(
            timeline.contains("process_id=4242 launch=1a2b3c4d"),
            "the lifetime context must survive into the Timeline: {timeline}"
        );
        assert!(timeline.contains("client startup milestone"), "{timeline}");
        assert!(
            timeline.contains("Client Guard Coverage transitioned from covered to unavailable"),
            "{timeline}"
        );
        assert!(
            timeline.contains("connected to Guard pipe (waited_ms=2)"),
            "a legacy line without lifetime fields still renders: {timeline}"
        );

        // Redaction is by construction: the appendix whitelist keeps the
        // message, so the diagnostic lifetime context survives it verbatim.
        let appendix = section(&document, "## Raw Appendix");
        assert!(
            appendix.contains("process_id=4242 launch=1a2b3c4d"),
            "the appendix must keep the lifetime context: {appendix}"
        );
    }

    #[tokio::test]
    async fn a_fixture_matching_no_rule_says_so_explicitly() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;

        // Healthy-looking evidence: coverage `covered`, clock within the
        // threshold, a fresh Guard poll reporting the Client connected, and
        // one ordinary Admin line.
        sqlx::query(
            "UPDATE pcs SET last_guard_coverage = 'covered',
                    clock_difference_ms = 30000
             WHERE id = 'pc-01'",
        )
        .execute(&pool)
        .await
        .expect("pc facts should update");
        write_log(
            admin_dir.path(),
            "diagnostics",
            &[admin_line("pc-01", "routine admin line", 60, "")],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions::default(),
            Some(&guard_poll(5, true)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        assert!(
            verdict.contains("No known failure shape matched"),
            "{verdict}"
        );
        assert!(verdict.contains("not a clean bill of health"), "{verdict}");
        assert!(
            !verdict.contains("failure shape(s) matched"),
            "no rule may claim a match: {verdict}"
        );
    }

    /// The originating incident (ADR-0078): one PC whose Client Guard
    /// Coverage read `unavailable`, where the window's evidence shows a
    /// foreign image being rejected, a reconnect flap, and a Guard that now
    /// reports the Client connected. The Verdict must separate all three
    /// shapes without further investigation.
    #[tokio::test]
    async fn the_originating_incident_separates_rules_1_3_and_6() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[
                foreign_rejection_line(300, 1),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    290,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::guard_client",
                    "connected to Guard pipe (waited_ms=3)",
                    285,
                    "",
                ),
                forwarded_line(
                    "client",
                    "pc-01",
                    "Guard pipe closed; reconnecting",
                    280,
                    "",
                ),
                forwarded_entry(
                    "client",
                    "pc-01",
                    "info",
                    "openpiso_client::guard_client",
                    "connected to Guard pipe (waited_ms=2)",
                    120,
                    "",
                ),
                forwarded_entry(
                    "guard",
                    "pc-01",
                    "info",
                    "openpiso_client_guard::pipe",
                    "Client connected (process_id=42)",
                    60,
                    "",
                ),
            ],
        );

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                window_seconds: 360,
                ..Default::default()
            },
            // The Guard polls fresh and reports the Client connected while
            // coverage still reads `unavailable`.
            Some(&guard_poll(5, true)),
            &HostDiagnostics::default(),
        )
        .await;

        let verdict = section(&document, "## Verdict");
        // Rule 1, with its distinguishing details.
        assert!(
            verdict.contains("Foreign Client image rejected by the Guard"),
            "{verdict}"
        );
        assert!(verdict.contains("2 connect attempt(s)"), "{verdict}");
        assert!(
            verdict.contains(r"expected image `C:\Apps\OpenPiso\client.exe`"),
            "{verdict}"
        );
        // Rule 3, with its duty cycle: 5 s + 160 s of a 360 s window.
        assert!(verdict.contains("Client reconnect flap"), "{verdict}");
        assert!(verdict.contains("2 time(s)"), "{verdict}");
        assert!(
            verdict.contains("45% of the window disconnected"),
            "{verdict}"
        );
        // Rule 6, with its contradiction: both observations stated, no
        // cause asserted.
        assert!(verdict.contains("Coverage contradiction"), "{verdict}");
        assert!(
            verdict.contains("the observations alone do not prove which report is wrong"),
            "{verdict}"
        );
        // The dead-Guard shape must not fire: Guard evidence exists and the
        // poll is fresh.
        assert!(!verdict.contains("Guard not running or not polling"));
        assert!(!verdict.contains("Offline Shutdown Countdown"));
    }

    #[tokio::test]
    async fn the_verdict_renders_above_the_timeline_and_survives_the_size_cap() {
        let admin_dir = tempfile::tempdir().expect("temp dir");
        let forwarded_dir = tempfile::tempdir().expect("temp dir");
        let pool = pool().await;
        stamp_pc_facts(&pool).await;

        write_log(
            forwarded_dir.path(),
            "forwarded",
            &[foreign_rejection_line(120, 2)],
        );
        let lines: Vec<String> = (0..200)
            .map(|index| admin_line("pc-01", &format!("noise line {index:03}"), index as u64, ""))
            .collect();
        write_log(admin_dir.path(), "diagnostics", &lines);

        let document = build_support_bundle(
            admin_dir.path(),
            forwarded_dir.path(),
            &pool,
            "pc-01",
            &SupportBundleOptions {
                size_cap: 1_600,
                ..Default::default()
            },
            Some(&guard_poll(5, false)),
            &HostDiagnostics::default(),
        )
        .await;

        assert!(document.len() <= 1_600, "hard byte ceiling must hold");
        let verdict_at = document.find("## Verdict").expect("Verdict must render");
        let timeline_at = document.find("## Timeline").expect("Timeline must render");
        assert!(verdict_at < timeline_at, "findings precede the data");
        let verdict = &document[verdict_at..timeline_at];
        // The Verdict's findings are intact, not a truncated fragment.
        assert!(
            verdict.contains("Foreign Client image rejected by the Guard"),
            "{verdict}"
        );
        assert!(verdict.contains("3 connect attempt(s)"), "{verdict}");
        assert!(
            verdict.contains(r"actual image `C:\Apps\piso client (1).exe`"),
            "{verdict}"
        );
        assert!(
            header(&document).contains("timeline"),
            "the cut timeline must be stated"
        );
    }

    #[test]
    fn message_field_reads_values_with_spaces_and_trims_the_wrapper() {
        let message = r"rejecting foreign pipe client (process_id=99 actual_image=C:\Apps\piso client (1).exe expected_image=C:\Apps\OpenPiso\client.exe suppressed_connections=2)";
        assert_eq!(
            message_field(message, "actual_image"),
            Some(r"C:\Apps\piso client (1).exe")
        );
        assert_eq!(
            message_field(message, "expected_image"),
            Some(r"C:\Apps\OpenPiso\client.exe")
        );
        assert_eq!(message_field(message, "suppressed_connections"), Some("2"));
        assert_eq!(message_field(message, "missing_key"), None);
        assert_eq!(message_field("no context at all", "expected_image"), None);
    }
}

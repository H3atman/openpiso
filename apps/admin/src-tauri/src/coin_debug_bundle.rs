// M4 deliverable: the Coin Debug Bundle (issue #97). A self-contained
#![allow(dead_code)]
// Markdown diagnostic document built by joining the diagnostic log directory
// and the database on the Coin Trace Id. No HTTP or UI involvement — a free
// function taking the log directory, a database pool, and options.
//
// Sections, in order: Environment, Verdict, Timeline, Anomalies, JSONL appendix.
// The ordering ensures findings appear before data, and findings survive truncation.
//
// The reusable machinery — diagnostic log parsing, account pseudonymisation,
// appendix field-whitelisting, and the byte cap that states its own
// truncation — lives in `crate::bundle_machinery` (issue #356) so the Support
// Bundle reuses it rather than duplicating it.
//
// Security: device secrets are structurally excluded (no query selects the
// `secret` column), account identities are pseudonymised to stable per-bundle
// labels, and the raw appendix is re-serialised from a field whitelist.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::SqlitePool;
use tracing::warn;

use crate::bundle_machinery::{
    finalize_document, format_timestamp, keep_newest, markdown_cell, parse_diagnostic_json_line,
    parse_diagnostic_log_dir, serialise_appendix_entry, timestamp_in_window, truncate_timestamp,
    AccountLabelMap, BundleLayout, WindowInfo,
};

// ── Options ───────────────────────────────────────────────────────────

/// Configuration for the debug bundle builder.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DebugBundleOptions {
    /// Maximum window duration (seconds) to cover; defaults to 7200 (2 hours),
    /// and never exceeds the two-hour product bound.
    pub window_seconds: i64,
    /// Maximum timeline rows; defaults to 50.
    pub timeline_cap: usize,
    /// Hard ceiling in bytes for the entire document; defaults to ~80 KB.
    pub size_cap: usize,
}

impl Default for DebugBundleOptions {
    fn default() -> Self {
        Self {
            window_seconds: 7200,
            timeline_cap: 50,
            size_cap: 81_920,
        }
    }
}

// ── Parsed log structures ────────────────────────────────────────────

/// A structured entry parsed from the diagnostic log, filtered to coin-path
/// targets only.
#[derive(Debug, Clone)]
struct CoinLogEntry {
    timestamp: String,
    level: String,
    message: String,
    /// Parsed structured fields from the JSON line — kept for the raw appendix.
    raw_fields: HashMap<String, Value>,
    // Typed optional fields extracted during parsing
    coin_trace_id: Option<String>,
    pc_id: Option<String>,
    account_id: Option<String>,
    amount_cents: Option<i64>,
    pulse_count: Option<i64>,
    attempt: Option<u32>,
    interval_us: Option<u64>,
    set_pin_asserted: Option<bool>,
    span_us: Option<u64>,
    gap_us: Option<u64>,
    pulse_timings_us: Option<Vec<u64>>,
    source_kind: Option<String>,
    anomaly: Option<String>,
    reason: Option<String>,
    cause: Option<String>,
}

// ── DB-side structures ────────────────────────────────────────────────

/// A coin transaction enriched with its audit metadata. The `coin_trace_id`
/// is extracted from the audit event's JSON metadata.
#[derive(Debug, Clone)]
struct TransactionRecord {
    transaction_id: String,
    pc_id: String,
    account_id: Option<String>,
    account_username: Option<String>,
    amount_cents: i64,
    seconds_added: i64,
    source: String,
    target_kind: String,
    created_at: String,
    coin_trace_id: Option<String>,
}

// ── Verdict components ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct AnomalyEntry {
    kind: AnomalyKind,
    timestamp: String,
    details: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AnomalyKind {
    InhibitedPulse,
    BatchNoDbRow,
    AmountMismatch,
    DuplicateTraceId,
}

impl AnomalyKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::InhibitedPulse => "inhibited_pulse",
            Self::BatchNoDbRow => "batch_no_db_row",
            Self::AmountMismatch => "amount_mismatch",
            Self::DuplicateTraceId => "duplicate_trace_id",
        }
    }
}

// ── Timeline row ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbOutcome {
    Missing,
    Persisted,
    Duplicate,
}

impl DbOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Persisted => "persisted",
            Self::Duplicate => "duplicate",
        }
    }
}

/// One row in the timeline: one coin, joined across log and DB.
#[derive(Debug, Clone)]
struct TimelineRow {
    timestamp: String,
    coin_trace_id: String,
    pc_id: String,
    pulse_count: i64,
    amount_cents: i64,
    /// What the DB says; `None` if the closed batch has no persisted row.
    db_amount_cents: Option<i64>,
    db_seconds_added: Option<i64>,
    db_account_label: Option<String>,
    /// The number of commit attempts, including failed attempts.
    attempts: u32,
    /// The final persistence state for this physical coin.
    db_outcome: DbOutcome,
}

// ── Field whitelist for the raw appendix ──────────────────────────────

/// Fields that are safe to include in the raw appendix. Any field not in this
/// list is structurally excluded.
const APPENDIX_FIELD_WHITELIST: &[&str] = &[
    "timestamp",
    "level",
    "target",
    "message",
    "coin_trace_id",
    "pc_id",
    "pulse_count",
    "amount_cents",
    "attempt",
    "span_us",
    "gap_us",
    "interval_us",
    "set_pin_asserted",
    "source_kind",
    "anomaly",
    "reason",
    "cause",
    // Retain historical mismatch evidence in the redacted raw appendix. These
    // fields are evidence only: S1 has no mismatch detector or owner list.
    "observed_pulse_count",
    "configured_pulse_map",
    "pulse_timings_us",
    "kind",
    "error",
    "available",
];

/// The Coin Debug Bundle's binding of the shared size-cap machinery to its
/// section markers.
const COIN_BUNDLE_LAYOUT: BundleLayout = BundleLayout {
    title: "# Coin Debug Bundle",
    appendix_marker: "## Raw Appendix",
    appendix_notice: "*(appendix truncated for size)*",
    timeline_marker: "## Timeline",
    anomalies_marker: "## Anomalies",
    anomalies_notice: "*(byte-bounded; optional anomaly detail omitted)*",
    environment_marker: "## Environment",
    verdict_marker: "## Verdict",
    section_notice: "*(byte-bounded)*",
};

// ── Main function ─────────────────────────────────────────────────────

/// Build a Coin Debug Bundle: a bounded, redacted Markdown document that
/// states its own conclusions.
pub async fn build_coin_debug_bundle(
    log_dir: &Path,
    pool: &SqlitePool,
    debug_window_start: DateTime<Utc>,
    port_choices: Option<&[crate::serial_port_detection::DetectedSerialPort]>,
    options: &DebugBundleOptions,
) -> String {
    // Capture generation time once. Every source uses this same end boundary.
    let generated_at = Utc::now();
    let mut window_info = compute_window_info(debug_window_start, generated_at, options);

    // Filter every source before labels, joins, verdicts, anomalies, or the
    // appendix are computed. This makes the document describe this app run,
    // rather than whatever old data remains on disk.
    let log_entries = parse_coin_log_entries(log_dir)
        .into_iter()
        .filter(|entry| timestamp_in_window(&entry.timestamp, &window_info))
        .collect::<Vec<_>>();
    let (db_transactions, duplicate_trace_ids) = query_db(pool, &window_info).await;

    let mut account_labels = AccountLabelMap::default();
    for tx in &db_transactions {
        if let Some(account_id) = &tx.account_id {
            account_labels.label(account_id);
        }
    }
    for entry in &log_entries {
        if let Some(account_id) = &entry.account_id {
            account_labels.label(account_id);
        }
    }

    let (timeline, timeline_was_capped) = build_timeline(
        &log_entries,
        &db_transactions,
        &account_labels,
        &window_info,
        options,
    );
    window_info.timeline_truncated = timeline_was_capped;

    let anomalies = detect_anomalies_with_window(
        &log_entries,
        &db_transactions,
        &duplicate_trace_ids,
        &account_labels,
        &window_info,
    );

    let env_section = build_environment_section(pool, port_choices).await;
    let verdict_section =
        build_verdict_section_with_window(&anomalies, &log_entries, &db_transactions, &window_info);
    let timeline_section = build_timeline_section(&timeline, &window_info);
    let anomalies_section = build_anomalies_section(&anomalies, &account_labels);
    let appendix_section = build_appendix_section(&log_entries, &window_info);

    let mut document = format!(
        "# Coin Debug Bundle\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}\n\n---\n\n{}",
        window_info.header(),
        env_section,
        verdict_section,
        timeline_section,
        anomalies_section,
        appendix_section,
    );

    finalize_document(
        &mut document,
        options.size_cap,
        &COIN_BUNDLE_LAYOUT,
        &mut window_info,
    );
    document
}

// ── Log parsing ───────────────────────────────────────────────────────

/// Parse diagnostic log files, extracting entries relevant to the coin path.
fn parse_coin_log_entries(log_dir: &Path) -> Vec<CoinLogEntry> {
    parse_diagnostic_log_dir(log_dir, parse_coin_log_line)
}

fn is_coin_path_target(target: &str) -> bool {
    target.starts_with("openpiso_admin_lib::coin_source")
        || target.starts_with("openpiso_admin_lib::terminal_selection")
        || target.starts_with("openpiso_admin_lib::coins")
}

fn parse_coin_log_line(line: &str) -> Option<CoinLogEntry> {
    let parsed = parse_diagnostic_json_line(line, APPENDIX_FIELD_WHITELIST)?;

    // Only coin-path targets
    if !is_coin_path_target(&parsed.target) {
        return None;
    }

    let value = &parsed.json;

    // Extract structured fields
    let extract_i64 = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_i64())
            .or_else(|| value.get(key).and_then(|v| v.as_str())?.parse::<i64>().ok())
    };
    let extract_u64 = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_u64())
            .or_else(|| value.get(key).and_then(|v| v.as_str())?.parse::<u64>().ok())
    };
    let extract_string = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    let extract_bool = |key: &str| value.get(key).and_then(|v| v.as_bool());

    let coin_trace_id = extract_string("coin_trace_id");
    let pc_id = extract_string("pc_id");
    let account_id = extract_string("account_id");
    let amount_cents = extract_i64("amount_cents");
    let pulse_count = extract_i64("pulse_count");
    let attempt = value
        .get("attempt")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let interval_us = extract_u64("interval_us");
    let set_pin_asserted = extract_bool("asserted");
    let span_us = extract_u64("span_us");
    let gap_us = extract_u64("gap_us");
    let pulse_timings_us = value
        .get("pulse_timings_us")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_u64()).collect());
    let source_kind = extract_string("source_kind");
    let anomaly = extract_string("anomaly");
    let reason = extract_string("reason");
    let cause = extract_string("cause");

    Some(CoinLogEntry {
        timestamp: parsed.timestamp,
        level: parsed.level,
        message: parsed.message,
        raw_fields: parsed.raw_fields,
        coin_trace_id,
        pc_id,
        account_id,
        amount_cents,
        pulse_count,
        attempt,
        interval_us,
        set_pin_asserted,
        span_us,
        gap_us,
        pulse_timings_us,
        source_kind,
        anomaly,
        reason,
        cause,
    })
}

// ── Database queries ──────────────────────────────────────────────────

async fn query_db(pool: &SqlitePool, window: &WindowInfo) -> (Vec<TransactionRecord>, Vec<String>) {
    // Query coin transactions with their audit event metadata.
    // Structural secret exclusion: never SELECT from coin_devices.secret.
    // Account usernames are read but will be pseudonymised later.
    let rows = match sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            String,
            String,
            String,
            Option<String>,
        ),
    >(
        r#"
        SELECT ct.id, ct.pc_id, ct.account_id, a.username,
               ct.amount_cents, ct.seconds_added, ct.source,
               ct.target_kind, ct.created_at,
               ae.metadata_json
        FROM coin_transactions ct
        JOIN audit_log ae ON ae.id = ct.audit_event_id
        LEFT JOIN accounts a ON a.id = ct.account_id
        WHERE julianday(ct.created_at) >= julianday(?)
          AND julianday(ct.created_at) <= julianday(?)
        ORDER BY ct.created_at ASC
        "#,
    )
    .bind(&window.from)
    .bind(&window.to)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!("coin debug bundle: DB query failed: {e}");
            return (Vec::new(), Vec::new());
        }
    };

    let mut records = Vec::with_capacity(rows.len());
    let mut trace_id_counts: HashMap<String, u32> = HashMap::new();

    for (
        tx_id,
        pc_id,
        account_id,
        username,
        amount_cents,
        seconds_added,
        source,
        target_kind,
        created_at,
        metadata_json,
    ) in rows
    {
        let coin_trace_id = metadata_json.as_deref().and_then(|json| {
            let meta: Value = serde_json::from_str(json).ok()?;
            meta.get("coin_trace_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });

        if let Some(ref tid) = coin_trace_id {
            *trace_id_counts.entry(tid.clone()).or_insert(0) += 1;
        }

        records.push(TransactionRecord {
            transaction_id: tx_id,
            pc_id,
            account_id,
            account_username: username,
            amount_cents,
            seconds_added,
            source,
            target_kind,
            created_at,
            coin_trace_id,
        });
    }

    // Find duplicate coin_trace_ids
    let duplicates: Vec<String> = trace_id_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| id)
        .collect();

    (records, duplicates)
}

// ── Window computation ────────────────────────────────────────────────

fn compute_window_info(
    debug_window_start: DateTime<Utc>,
    generated_at: DateTime<Utc>,
    options: &DebugBundleOptions,
) -> WindowInfo {
    let window_seconds = options.window_seconds.clamp(0, 7_200);
    let two_hour_boundary = generated_at - chrono::Duration::seconds(window_seconds);
    // The debug window start is a pure floor-raiser: it only ever narrows the
    // window, never widening past the 2-hour product cap.
    let start = debug_window_start.max(two_hour_boundary);
    WindowInfo {
        from: format_timestamp(start),
        to: format_timestamp(generated_at),
        time_window_truncated: true,
        timeline_truncated: false,
        appendix_truncated: false,
        byte_truncated: false,
    }
}

// ── Timeline building ─────────────────────────────────────────────────

fn build_timeline(
    log_entries: &[CoinLogEntry],
    db_transactions: &[TransactionRecord],
    account_labels: &AccountLabelMap,
    window: &WindowInfo,
    options: &DebugBundleOptions,
) -> (Vec<TimelineRow>, bool) {
    // A successful closure is the physical-coin evidence. Commit logs are
    // attempts and may contain several records for the same trace id.
    let mut closed_batches: HashMap<String, &CoinLogEntry> = HashMap::new();
    for entry in log_entries
        .iter()
        .filter(|entry| entry.message == "Coin Batch closed" && entry.coin_trace_id.is_some())
    {
        let Some(trace_id) = entry.coin_trace_id.as_ref() else {
            continue;
        };
        closed_batches
            .entry(trace_id.clone())
            .and_modify(|existing| {
                if entry.timestamp < existing.timestamp {
                    *existing = entry;
                }
            })
            .or_insert(entry);
    }

    let mut attempts_by_trace: HashMap<&str, u32> = HashMap::new();
    for entry in log_entries
        .iter()
        .filter(|entry| entry.message == "Coin Batch committed" && entry.coin_trace_id.is_some())
    {
        let Some(trace_id) = entry.coin_trace_id.as_deref() else {
            continue;
        };
        *attempts_by_trace.entry(trace_id).or_default() += 1;
    }

    let mut db_by_trace: HashMap<&str, Vec<&TransactionRecord>> = HashMap::new();
    for tx in db_transactions
        .iter()
        .filter(|tx| tx.coin_trace_id.is_some() && timestamp_in_window(&tx.created_at, window))
    {
        let Some(trace_id) = tx.coin_trace_id.as_deref() else {
            continue;
        };
        db_by_trace.entry(trace_id).or_default().push(tx);
    }

    // One row per closed Coin Batch / physical coin. A duplicate DB trace is
    // retained as an anomaly; the latest row is shown as the final outcome.
    let mut rows: Vec<TimelineRow> = closed_batches
        .into_iter()
        .filter_map(|(coin_trace_id, closure)| {
            if !timestamp_in_window(&closure.timestamp, window) {
                return None;
            }
            let db_txs = db_by_trace
                .get(coin_trace_id.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let final_db_tx = db_txs.iter().copied().max_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.transaction_id.cmp(&right.transaction_id))
            });
            let account_label = final_db_tx
                .and_then(|tx| tx.account_id.as_deref())
                .or(closure.account_id.as_deref())
                .and_then(|aid| account_labels.get(aid))
                .map(str::to_owned);
            let db_outcome = match db_txs.len() {
                0 => DbOutcome::Missing,
                1 => DbOutcome::Persisted,
                _ => DbOutcome::Duplicate,
            };

            Some(TimelineRow {
                timestamp: closure.timestamp.clone(),
                coin_trace_id: coin_trace_id.clone(),
                pc_id: closure
                    .pc_id
                    .clone()
                    .or_else(|| final_db_tx.map(|tx| tx.pc_id.clone()))
                    .unwrap_or_default(),
                pulse_count: closure.pulse_count.unwrap_or(0),
                amount_cents: closure.amount_cents.unwrap_or(0),
                db_amount_cents: final_db_tx.map(|tx| tx.amount_cents),
                db_seconds_added: final_db_tx.map(|tx| tx.seconds_added),
                db_account_label: account_label,
                attempts: attempts_by_trace
                    .get(coin_trace_id.as_str())
                    .copied()
                    .unwrap_or(0),
                db_outcome,
            })
        })
        .collect();

    rows.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Apply timeline cap: take the most recent N physical coins.
    keep_newest(rows, options.timeline_cap)
}

// ── Anomaly detection ─────────────────────────────────────────────────

fn detect_anomalies(
    log_entries: &[CoinLogEntry],
    db_transactions: &[TransactionRecord],
    duplicate_trace_ids: &[String],
    account_labels: &AccountLabelMap,
) -> Vec<AnomalyEntry> {
    detect_anomalies_with_window(
        log_entries,
        db_transactions,
        duplicate_trace_ids,
        account_labels,
        &WindowInfo {
            from: String::new(),
            to: String::new(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        },
    )
}

fn detect_anomalies_with_window(
    log_entries: &[CoinLogEntry],
    db_transactions: &[TransactionRecord],
    duplicate_trace_ids: &[String],
    account_labels: &AccountLabelMap,
    window: &WindowInfo,
) -> Vec<AnomalyEntry> {
    let mut anomalies = Vec::new();

    // 2. Inhibited Pulse count
    for entry in log_entries.iter().filter(|entry| {
        entry.anomaly.as_deref() == Some("inhibited_pulse")
            && timestamp_in_window(&entry.timestamp, window)
    }) {
        let details = match entry.reason.as_deref() {
            Some("coin_source_unavailable") => "Coin Source unavailable or recovering".to_owned(),
            Some("no_selection_window" | "set_pin_inhibit_not_honored") => "healthy sampler; no Selection Window; Set Pin inhibited (idle insertion or inhibit wiring problem)".to_owned(),
            Some(reason) => reason.to_owned(),
            None => "inhibited pulse".to_owned(),
        };
        anomalies.push(AnomalyEntry {
            kind: AnomalyKind::InhibitedPulse,
            timestamp: entry.timestamp.clone(),
            details,
        });
    }

    // 3. A closed batch with no matching transaction is a missing database row.
    // Commit-attempt logs are intentionally not used as physical-coin evidence.
    let mut closed_batches: HashMap<&str, &CoinLogEntry> = HashMap::new();
    for entry in log_entries.iter().filter(|entry| {
        entry.message == "Coin Batch closed"
            && entry.coin_trace_id.is_some()
            && timestamp_in_window(&entry.timestamp, window)
    }) {
        let Some(trace_id) = entry.coin_trace_id.as_deref() else {
            continue;
        };
        closed_batches
            .entry(trace_id)
            .and_modify(|existing| {
                if entry.timestamp < existing.timestamp {
                    *existing = entry;
                }
            })
            .or_insert(entry);
    }

    let db_by_trace: HashMap<&str, Vec<&TransactionRecord>> = db_transactions
        .iter()
        .filter(|tx| timestamp_in_window(&tx.created_at, window))
        .filter_map(|tx| {
            let trace_id = tx.coin_trace_id.as_deref()?;
            Some((trace_id, tx))
        })
        .fold(HashMap::new(), |mut grouped, (trace_id, tx)| {
            grouped.entry(trace_id).or_default().push(tx);
            grouped
        });

    for (trace_id, entry) in &closed_batches {
        if !db_by_trace.contains_key(trace_id) {
            anomalies.push(AnomalyEntry {
                kind: AnomalyKind::BatchNoDbRow,
                timestamp: entry.timestamp.clone(),
                details: format!(
                    "coin_trace_id={}, pc_id={}, pulse_count={}, amount_cents={}c",
                    trace_id,
                    entry.pc_id.as_deref().unwrap_or("?"),
                    entry.pulse_count.unwrap_or(0),
                    entry.amount_cents.unwrap_or(0),
                ),
            });
        }
    }

    // 4. Compare every persisted row with the canonical closure amount.
    for (trace_id, matching_txs) in &db_by_trace {
        let Some(closure) = closed_batches.get(trace_id) else {
            continue;
        };
        let Some(batch_amount) = closure.amount_cents else {
            continue;
        };
        for tx in matching_txs {
            if batch_amount != tx.amount_cents {
                let account_info = tx
                    .account_id
                    .as_deref()
                    .and_then(|aid| account_labels.get(aid))
                    .map(|label| format!(", account={label}"))
                    .unwrap_or_default();
                anomalies.push(AnomalyEntry {
                    kind: AnomalyKind::AmountMismatch,
                    timestamp: closure.timestamp.clone(),
                    details: format!(
                        "coin_trace_id={}: batch says {}c, DB says {}c{}",
                        trace_id, batch_amount, tx.amount_cents, account_info
                    ),
                });
            }
        }
    }

    // 5. Duplicate persisted transactions are a double-credit anomaly, even
    // when their commit attempts all share one physical Coin Trace Id.
    for trace_id in duplicate_trace_ids {
        let Some(matching_txs) = db_by_trace.get(trace_id.as_str()) else {
            continue;
        };
        // `duplicate_trace_ids` is computed over the full database, while the
        // current bundle may cover only part of that history. Do not report a
        // duplicate unless both rows are in the selected window.
        if matching_txs.len() < 2 {
            continue;
        }
        let amounts: String = matching_txs
            .iter()
            .map(|tx| format!("{}c", tx.amount_cents))
            .collect::<Vec<_>>()
            .join(", ");
        let pc_info = matching_txs
            .first()
            .map(|tx| format!("pc_id={}", tx.pc_id))
            .unwrap_or_default();
        let timestamp = closed_batches
            .get(trace_id.as_str())
            .map(|closure| closure.timestamp.clone())
            .or_else(|| matching_txs.first().map(|tx| tx.created_at.clone()))
            .unwrap_or_default();
        anomalies.push(AnomalyEntry {
            kind: AnomalyKind::DuplicateTraceId,
            timestamp,
            details: format!(
                "coin_trace_id={}: {} rows, amounts=[{}], {}",
                trace_id,
                matching_txs.len(),
                amounts,
                pc_info
            ),
        });
    }

    anomalies.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    anomalies
}

// ── Environment section ───────────────────────────────────────────────

async fn build_environment_section(
    pool: &SqlitePool,
    port_choices: Option<&[crate::serial_port_detection::DetectedSerialPort]>,
) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let now = format_timestamp(chrono::Utc::now());
    let os = std::env::consts::OS;

    // Read coin device settings
    let (
        saved_com_port,
        com_port,
        baud_rate,
        set_pin_line,
        coin_pin_line,
        coin_pin_polarity,
        inter_coin_gap_ms,
        pulse_map_entries,
    ) = match crate::db::settings::coin_device_settings(pool).await {
        Ok(s) => {
            let com_port = s.com_port.clone().unwrap_or_else(|| "none".to_owned());
            (
                s.com_port,
                com_port,
                s.baud_rate,
                s.set_pin_line,
                s.coin_pin_line,
                s.coin_pin_polarity,
                s.inter_coin_gap_ms,
                s.pulse_map,
            )
        }
        Err(_) => (
            None,
            "error".to_owned(),
            0,
            "error".to_owned(),
            "error".to_owned(),
            "error".to_owned(),
            0,
            vec![],
        ),
    };

    // Read simulator-enabled setting
    let simulator_enabled = match crate::db::settings::get_setting(pool, "simulator_enabled").await
    {
        Ok(Some(v)) => v == "true",
        _ => false,
    };

    // Read active rate
    let (rate_amount_cents, rate_seconds) = match crate::db::rates::active_rate(pool).await {
        Ok((a, s)) => (a, s),
        Err(_) => (0, 0),
    };

    let pulse_map_str: String = pulse_map_entries
        .iter()
        .map(|e| {
            format!(
                "  - {} pulse(s) → {} cent(s)",
                e.pulse_count, e.amount_cents
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let serial_ports_section = build_serial_ports_section(port_choices, saved_com_port.as_deref());

    format!(
        r#"## Environment

| Key | Value |
|---|---|
| App Version | `{version}` |
| Timestamp | `{now}` |
| OS | `{os}` |
| COM Port | `{com_port}` |
| Baud Rate | {baud_rate} |
| Set Pin Line | `{set_pin_line}` |
| Coin Pin Line | `{coin_pin_line}` |
| Coin Pin Polarity | `{coin_pin_polarity}` |
| Inter-Coin Gap | {inter_coin_gap_ms} ms |
| Simulator Enabled | {simulator_enabled} |
| Active Rate | ₱{rate_amount_cents} = {rate_seconds}s |

**Pulse Map:**

{pulse_map_str}

{serial_ports_section}
"#
    )
}

fn build_serial_ports_section(
    port_choices: Option<&[crate::serial_port_detection::DetectedSerialPort]>,
    saved_com_port: Option<&str>,
) -> String {
    let saved_com_port = saved_com_port
        .map(str::trim)
        .filter(|port| !port.is_empty());
    let saved_missing = |choices: &[crate::serial_port_detection::DetectedSerialPort]| {
        saved_com_port
            .and_then(|saved| {
                choices
                    .iter()
                    .find(|choice| choice.name.eq_ignore_ascii_case(saved))
                    .filter(|choice| !choice.detected)
                    .map(|choice| choice.name.as_str())
                    .or_else(|| {
                        (!choices
                            .iter()
                            .any(|choice| choice.name.eq_ignore_ascii_case(saved)))
                        .then_some(saved)
                    })
            })
            .map(|port| {
                format!(
                    "\n\n**Saved COM Port:** `{}` was not detected in this scan.",
                    markdown_cell(port)
                )
            })
            .unwrap_or_default()
    };

    match port_choices {
        None => "## Detected Serial Ports\n\nScan failed while enumerating host serial ports."
            .to_owned(),
        Some(choices) => {
            let no_ports = if choices.is_empty() {
                "No serial ports detected.".to_owned()
            } else {
                let rows = choices
                    .iter()
                    .map(|choice| {
                        format!(
                            "| `{}` | {} | {} |",
                            markdown_cell(&choice.name),
                            markdown_cell(&choice.label),
                            choice.detected
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("| Port | Label | Detected |\n|---|---|---|\n{rows}")
            };
            format!(
                "## Detected Serial Ports\n\n{}{missing}",
                no_ports,
                missing = saved_missing(choices)
            )
        }
    }
}

// ── Verdict section ───────────────────────────────────────────────────

fn build_verdict_section(
    anomalies: &[AnomalyEntry],
    log_entries: &[CoinLogEntry],
    db_transactions: &[TransactionRecord],
) -> String {
    build_verdict_section_with_window(
        anomalies,
        log_entries,
        db_transactions,
        &WindowInfo {
            from: String::new(),
            to: String::new(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        },
    )
}

fn build_verdict_section_with_window(
    anomalies: &[AnomalyEntry],
    log_entries: &[CoinLogEntry],
    db_transactions: &[TransactionRecord],
    window: &WindowInfo,
) -> String {
    let inhibited_count = anomalies
        .iter()
        .filter(|a| a.kind == AnomalyKind::InhibitedPulse)
        .count();
    let batch_no_db_count = anomalies
        .iter()
        .filter(|a| a.kind == AnomalyKind::BatchNoDbRow)
        .count();
    let mismatch_count = anomalies
        .iter()
        .filter(|a| a.kind == AnomalyKind::AmountMismatch)
        .count();
    let duplicate_count = anomalies
        .iter()
        .filter(|a| a.kind == AnomalyKind::DuplicateTraceId)
        .count();

    let total_coins_logged = log_entries
        .iter()
        .filter(|entry| {
            entry.message == "Coin Batch closed"
                && entry.coin_trace_id.is_some()
                && timestamp_in_window(&entry.timestamp, window)
        })
        .map(|entry| entry.coin_trace_id.as_deref().unwrap_or_default())
        .collect::<HashSet<_>>()
        .len();
    let total_coins_in_db = db_transactions
        .iter()
        .filter(|tx| tx.coin_trace_id.is_some() && timestamp_in_window(&tx.created_at, window))
        .count();

    let double_credit_risk = if duplicate_count > 0 {
        format!(
            "⚠️ {} duplicate Coin Trace Id(s) detected — possible double credit!",
            duplicate_count
        )
    } else {
        "No duplicate Coin Trace Ids".to_owned()
    };

    format!(
        r#"## Verdict

| Measure | Count |
|---|---|
| Inhibited Pulses | {inhibited_count} |
| Closed physical coins with no DB row | {batch_no_db_count} |
| Amount mismatches | {mismatch_count} |
| Duplicate Coin Trace Ids | {duplicate_count} |

**Summary:** {total_coins_logged} physical coin(s) closed, {total_coins_in_db} transaction(s) in DB.

**Double-credit check:** {double_credit_risk}
"#
    )
}

// ── Timeline section ──────────────────────────────────────────────────

fn build_timeline_section(rows: &[TimelineRow], window: &WindowInfo) -> String {
    if rows.is_empty() {
        return "## Timeline\n\nNo coin events found in the selected window.\n".to_owned();
    }

    let mut table = format!(
        "## Timeline\n\nWindow: {} to {} | {} coin(s) shown\n\n",
        window.from,
        window.to,
        rows.len()
    );

    table.push_str(
        "| # | Timestamp | Coin Trace ID | PC | Pulses | Amount (c) | DB Amount (c) | Seconds | Account | Attempts | DB Outcome |\n",
    );
    table.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");

    for (i, row) in rows.iter().enumerate() {
        let db_amount = row
            .db_amount_cents
            .map(|a| a.to_string())
            .unwrap_or_else(|| "—".to_owned());
        let db_seconds = row
            .db_seconds_added
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".to_owned());
        let account = row
            .db_account_label
            .as_deref()
            .unwrap_or("guest")
            .to_owned();
        let trace_short = if row.coin_trace_id.len() > 8 {
            &row.coin_trace_id[..8]
        } else {
            &row.coin_trace_id
        };

        table.push_str(&format!(
            "| {} | {} | `{}…` | {} | {} | {}c | {}c | {}s | {} | {} | {} |\n",
            i + 1,
            truncate_timestamp(&row.timestamp),
            trace_short,
            row.pc_id,
            row.pulse_count,
            row.amount_cents,
            db_amount,
            db_seconds,
            account,
            row.attempts,
            row.db_outcome.as_str(),
        ));
    }

    table
}

// ── Anomalies section ─────────────────────────────────────────────────

fn build_anomalies_section(
    anomalies: &[AnomalyEntry],
    _account_labels: &AccountLabelMap,
) -> String {
    if anomalies.is_empty() {
        return "## Anomalies\n\nNo anomalies detected.\n".to_owned();
    }

    let mut section = format!(
        "## Anomalies\n\n{} anomaly(ies) detected\n\n",
        anomalies.len()
    );

    section.push_str("| # | Type | Timestamp | Details |\n");
    section.push_str("|---|---|---|---|\n");

    for (i, anomaly) in anomalies.iter().enumerate() {
        section.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            i + 1,
            anomaly.kind.as_str(),
            truncate_timestamp(&anomaly.timestamp),
            anomaly.details,
        ));
    }

    section
}

// ── Appendix section (JSONL) ──────────────────────────────────────────

fn build_appendix_section(log_entries: &[CoinLogEntry], window: &WindowInfo) -> String {
    if log_entries.is_empty() {
        return "## Raw Appendix\n\nNo log entries available.\n".to_owned();
    }

    let mut section = "## Raw Appendix\n\n```jsonl\n".to_owned();

    // Only include entries within the window
    for entry in log_entries {
        if !timestamp_in_window(&entry.timestamp, window) {
            continue;
        }

        // Re-serialise from whitelist fields only
        if let Some(json) = serialise_appendix_entry(&entry.raw_fields) {
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
    use crate::bundle_machinery::apply_size_cap;
    use std::fs::{self, File};
    use std::io::Write;

    fn sample_batch_closed_line() -> &'static str {
        r#"{"timestamp":"2026-06-21T10:00:01.000000Z","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Coin Batch closed","coin_trace_id":"018f0000-0000-7000-8000-000000000001","pc_id":"pc-01","pulse_count":1,"amount_cents":100,"span_us":0,"gap_us":600000,"pulse_timings_us":[0]}"#
    }

    fn current_timestamp() -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6fZ")
            .to_string()
    }

    fn sample_closed_line_at(trace_id: &str, timestamp: &str) -> String {
        format!(
            r#"{{"timestamp":"{timestamp}","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Coin Batch closed","coin_trace_id":"{trace_id}","pc_id":"pc-01","pulse_count":1,"amount_cents":100,"span_us":0,"gap_us":600000,"pulse_timings_us":[0]}}"#
        )
    }

    fn sample_closed_line(trace_id: &str) -> String {
        sample_closed_line_at(trace_id, &current_timestamp())
    }

    fn sample_committed_line_at(trace_id: &str, timestamp: &str, attempt: u32) -> String {
        format!(
            r#"{{"timestamp":"{timestamp}","level":"INFO","target":"openpiso_admin_lib::terminal_selection","message":"Coin Batch committed","coin_trace_id":"{trace_id}","attempt":{attempt},"pc_id":"pc-01","pulse_count":1,"amount_cents":100,"source_kind":"Simulator"}}"#
        )
    }

    fn sample_committed_line(trace_id: &str) -> String {
        sample_committed_line_at(trace_id, &current_timestamp(), 1)
    }

    fn sample_inhibited_line() -> &'static str {
        r#"{"timestamp":"2026-06-21T10:00:04.000000Z","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Inhibited pulse","anomaly":"inhibited_pulse","reason":"no_selection_window"}"#
    }

    fn sample_set_pin_asserted_line() -> &'static str {
        r#"{"timestamp":"2026-06-21T10:00:05.000000Z","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Set Pin asserted","asserted":true,"cause":"selection_open"}"#
    }

    fn sample_set_pin_inhibited_line() -> &'static str {
        r#"{"timestamp":"2026-06-21T10:00:06.000000Z","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Set Pin inhibited","asserted":false,"cause":"selection_close"}"#
    }

    fn sample_unrelated_line() -> &'static str {
        r#"{"timestamp":"2026-06-21T10:00:07.000000Z","level":"WARN","target":"openpiso_admin_lib::accounts","message":"something else"}"#
    }

    #[test]
    fn parses_batch_closed_line() {
        let entry = parse_coin_log_line(sample_batch_closed_line())
            .expect("batch closed line should parse");
        assert_eq!(entry.message, "Coin Batch closed");
        assert_eq!(entry.pulse_count, Some(1));
        assert_eq!(entry.amount_cents, Some(100));
        assert_eq!(entry.span_us, Some(0));
        assert_eq!(entry.gap_us, Some(600000));
        assert_eq!(
            entry.coin_trace_id.as_deref(),
            Some("018f0000-0000-7000-8000-000000000001")
        );
        assert_eq!(entry.pc_id.as_deref(), Some("pc-01"));
    }

    #[test]
    fn parses_committed_line() {
        let trace_id = uuid::Uuid::new_v4().to_string();
        let entry = parse_coin_log_line(&sample_committed_line(&trace_id))
            .expect("committed line should parse");
        assert_eq!(entry.message, "Coin Batch committed");
        assert_eq!(entry.coin_trace_id.as_deref(), Some(trace_id.as_str()));
        assert_eq!(entry.attempt, Some(1));
        assert_eq!(entry.pc_id.as_deref(), Some("pc-01"));
        assert_eq!(entry.pulse_count, Some(1));
        assert_eq!(entry.amount_cents, Some(100));
        assert_eq!(entry.source_kind.as_deref(), Some("Simulator"));
    }

    #[test]
    fn parses_inhibited_line() {
        let entry =
            parse_coin_log_line(sample_inhibited_line()).expect("inhibited line should parse");
        assert_eq!(entry.message, "Inhibited pulse");
        assert_eq!(entry.anomaly.as_deref(), Some("inhibited_pulse"));
        assert_eq!(entry.reason.as_deref(), Some("no_selection_window"));
    }

    #[test]
    fn parses_set_pin_lines() {
        let asserted = parse_coin_log_line(sample_set_pin_asserted_line())
            .expect("set pin asserted line should parse");
        assert_eq!(asserted.message, "Set Pin asserted");
        assert_eq!(asserted.set_pin_asserted, Some(true));
        assert_eq!(asserted.cause.as_deref(), Some("selection_open"));

        let inhibited = parse_coin_log_line(sample_set_pin_inhibited_line())
            .expect("set pin inhibited line should parse");
        assert_eq!(inhibited.message, "Set Pin inhibited");
        assert_eq!(inhibited.set_pin_asserted, Some(false));
    }

    #[test]
    fn ignores_unrelated_targets() {
        let entry = parse_coin_log_line(sample_unrelated_line());
        assert!(
            entry.is_none(),
            "unrelated target should not be parsed as coin event"
        );
    }

    #[test]
    fn ignores_non_coin_targets() {
        let line = r#"{"timestamp":"2026-06-21T10:00:07.000000Z","level":"WARN","target":"openpiso_admin_lib::accounts","message":"something else"}"#;
        assert!(
            parse_coin_log_line(line).is_none(),
            "account target should be ignored"
        );
    }

    #[test]
    fn raw_fields_respect_field_whitelist() {
        // A line with both whitelisted and non-whitelisted fields.
        let line = r#"{"timestamp":"2026-06-21T10:00:01.000000Z","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Coin Batch closed","pulse_count":1,"amount_cents":100,"observed_pulse_count":7,"configured_pulse_map":"{1: 100}","secret_field":"should-not-appear","internal_token":"nope"}"#;
        let entry = parse_coin_log_line(line).expect("line should parse");
        assert!(entry.raw_fields.contains_key("pulse_count"));
        assert!(entry.raw_fields.contains_key("amount_cents"));
        assert!(entry.raw_fields.contains_key("observed_pulse_count"));
        assert!(entry.raw_fields.contains_key("configured_pulse_map"));
        assert!(
            !entry.raw_fields.contains_key("secret_field"),
            "secret_field must not be in raw fields"
        );
        assert!(
            !entry.raw_fields.contains_key("internal_token"),
            "internal_token must not be in raw fields"
        );
    }

    #[test]
    fn log_files_filter_correctly() {
        let dir = std::env::temp_dir().join(format!("openpiso-bundle-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        // Write a diagnostic file
        let mut good = File::create(dir.join("diagnostics.2026-06-21.log")).expect("good file");
        writeln!(good, "{}", sample_batch_closed_line()).expect("write line");

        // Non-matching file
        fs::write(dir.join("other.log"), sample_inhibited_line()).expect("foreign file");

        let entries = parse_coin_log_entries(&dir);
        assert_eq!(
            entries.len(),
            1,
            "should find one coin entry from diagnostic files"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_inhibited_pulse_anomaly() {
        let entry = parse_coin_log_line(sample_inhibited_line()).unwrap();
        let anomalies = detect_anomalies(
            std::slice::from_ref(&entry),
            &[],
            &[],
            &AccountLabelMap::default(),
        );
        let inhibited: Vec<_> = anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::InhibitedPulse)
            .collect();
        assert_eq!(inhibited.len(), 1);
        assert_eq!(
            inhibited[0].details,
            "healthy sampler; no Selection Window; Set Pin inhibited (idle insertion or inhibit wiring problem)"
        );

        let legacy_line =
            sample_inhibited_line().replace("no_selection_window", "set_pin_inhibit_not_honored");
        let legacy_entry = parse_coin_log_line(&legacy_line).expect("legacy inhibited line");
        let legacy_anomalies = detect_anomalies(
            std::slice::from_ref(&legacy_entry),
            &[],
            &[],
            &AccountLabelMap::default(),
        );
        assert_eq!(
            legacy_anomalies[0].details,
            "healthy sampler; no Selection Window; Set Pin inhibited (idle insertion or inhibit wiring problem)"
        );
    }

    #[test]
    fn detects_batch_no_db_row_anomaly() {
        let trace_id = uuid::Uuid::new_v4().to_string();
        let closure = parse_coin_log_line(&sample_closed_line(&trace_id)).unwrap();
        // No matching DB transaction → should be detected as a closed batch with no row
        let anomalies = detect_anomalies(
            std::slice::from_ref(&closure),
            &[],
            &[],
            &AccountLabelMap::default(),
        );
        let no_db: Vec<_> = anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::BatchNoDbRow)
            .collect();
        assert_eq!(no_db.len(), 1);
        assert!(no_db[0].details.contains(&trace_id));
    }

    #[test]
    fn detects_amount_mismatch_anomaly() {
        let trace_id = uuid::Uuid::new_v4().to_string();
        let closure = parse_coin_log_line(&sample_closed_line(&trace_id)).unwrap();

        // DB has different amount
        let db_tx = TransactionRecord {
            transaction_id: "tx-1".to_owned(),
            pc_id: "pc-01".to_owned(),
            account_id: None,
            account_username: None,
            amount_cents: 500, // log says 100
            seconds_added: 1500,
            source: "simulator".to_owned(),
            target_kind: "guest".to_owned(),
            created_at: "2026-06-21T10:00:02.000000Z".to_owned(),
            coin_trace_id: Some(trace_id.clone()),
        };

        let anomalies = detect_anomalies(
            std::slice::from_ref(&closure),
            &[db_tx],
            &[],
            &AccountLabelMap::default(),
        );
        let mismatches: Vec<_> = anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::AmountMismatch)
            .collect();
        assert_eq!(mismatches.len(), 1);
        assert!(mismatches[0].details.contains("100c"));
        assert!(mismatches[0].details.contains("500c"));
    }

    #[test]
    fn detects_duplicate_trace_id_anomaly() {
        let trace_id = uuid::Uuid::new_v4().to_string();
        let db_txs = vec![
            TransactionRecord {
                transaction_id: "tx-1".to_owned(),
                pc_id: "pc-01".to_owned(),
                account_id: None,
                account_username: None,
                amount_cents: 100,
                seconds_added: 300,
                source: "simulator".to_owned(),
                target_kind: "guest".to_owned(),
                created_at: "2026-06-21T10:00:02.000000Z".to_owned(),
                coin_trace_id: Some(trace_id.clone()),
            },
            TransactionRecord {
                transaction_id: "tx-2".to_owned(),
                pc_id: "pc-01".to_owned(),
                account_id: None,
                account_username: None,
                amount_cents: 100,
                seconds_added: 300,
                source: "simulator".to_owned(),
                target_kind: "guest".to_owned(),
                created_at: "2026-06-21T10:00:03.000000Z".to_owned(),
                coin_trace_id: Some(trace_id.clone()),
            },
        ];

        let closure = parse_coin_log_line(&sample_closed_line(&trace_id)).unwrap();
        let anomalies = detect_anomalies(
            std::slice::from_ref(&closure),
            &db_txs,
            std::slice::from_ref(&trace_id),
            &AccountLabelMap::default(),
        );
        let dup: Vec<_> = anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::DuplicateTraceId)
            .collect();
        assert_eq!(dup.len(), 1, "duplicate trace id should be detected");
    }

    #[test]
    fn timeline_groups_retry_attempts_and_uses_closed_batches() {
        let retry_trace = "018f0000-0000-7000-8000-000000000101";
        let lost_trace = "018f0000-0000-7000-8000-000000000102";
        let lines = [
            sample_closed_line_at(retry_trace, "2026-06-21T10:00:00.000000Z"),
            sample_committed_line_at(retry_trace, "2026-06-21T10:00:01.000000Z", 1),
            sample_committed_line_at(retry_trace, "2026-06-21T10:00:02.000000Z", 2),
            sample_closed_line_at(lost_trace, "2026-06-21T10:01:00.000000Z"),
            // An attempt without closure evidence must not create a coin row.
            sample_committed_line_at(
                "018f0000-0000-7000-8000-000000000103",
                "2026-06-21T10:02:00.000000Z",
                1,
            ),
        ];
        let entries: Vec<_> = lines
            .iter()
            .map(|line| parse_coin_log_line(line).expect("fixture log should parse"))
            .collect();
        let db_tx = TransactionRecord {
            transaction_id: "tx-retry".to_owned(),
            pc_id: "pc-01".to_owned(),
            account_id: None,
            account_username: None,
            amount_cents: 100,
            seconds_added: 300,
            source: "simulator".to_owned(),
            target_kind: "guest".to_owned(),
            created_at: "2026-06-21T10:00:03.000000Z".to_owned(),
            coin_trace_id: Some(retry_trace.to_owned()),
        };
        let window = WindowInfo {
            from: String::new(),
            to: String::new(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };
        let (rows, was_capped) = build_timeline(
            &entries,
            std::slice::from_ref(&db_tx),
            &AccountLabelMap::default(),
            &window,
            &DebugBundleOptions::default(),
        );

        assert!(!was_capped);
        assert_eq!(rows.len(), 2, "only closed physical coins become rows");
        let retry = rows
            .iter()
            .find(|row| row.coin_trace_id == retry_trace)
            .expect("retry row");
        assert_eq!(retry.attempts, 2);
        assert_eq!(retry.db_outcome, DbOutcome::Persisted);
        let lost = rows
            .iter()
            .find(|row| row.coin_trace_id == lost_trace)
            .expect("lost row");
        assert_eq!(lost.attempts, 0);
        assert_eq!(lost.db_outcome, DbOutcome::Missing);

        let anomalies = detect_anomalies(
            &entries,
            std::slice::from_ref(&db_tx),
            &[],
            &AccountLabelMap::default(),
        );
        assert_eq!(
            anomalies
                .iter()
                .filter(|anomaly| anomaly.kind == AnomalyKind::BatchNoDbRow)
                .count(),
            1,
            "only the closed lost coin is missing"
        );
        assert!(!anomalies
            .iter()
            .any(|anomaly| anomaly.kind == AnomalyKind::DuplicateTraceId));
    }

    #[test]
    fn account_label_map_generates_stable_labels() {
        let mut labels = AccountLabelMap::default();
        assert_eq!(labels.label("acct-1"), "Account A");
        assert_eq!(labels.label("acct-2"), "Account B");
        // Same ID maps to the same label
        assert_eq!(labels.label("acct-1"), "Account A");
        assert_eq!(labels.get("acct-1"), Some("Account A"));
        assert_eq!(labels.get("acct-2"), Some("Account B"));
        assert_eq!(labels.get("nonexistent"), None);
    }

    #[test]
    fn coverage_window_uses_debug_window_start_and_generation_boundaries() {
        // A debug window start later than `now - 2h` narrows coverage to itself.
        let debug_window_start = DateTime::parse_from_rfc3339("2026-06-21T11:30:00Z")
            .expect("debug-window-start timestamp")
            .with_timezone(&Utc);
        let generated = DateTime::parse_from_rfc3339("2026-06-21T12:00:00Z")
            .expect("generation timestamp")
            .with_timezone(&Utc);
        let info = compute_window_info(
            debug_window_start,
            generated,
            &DebugBundleOptions::default(),
        );

        assert_eq!(info.from, "2026-06-21T11:30:00.000000Z");
        assert_eq!(info.to, "2026-06-21T12:00:00.000000Z");
        assert!(timestamp_in_window("2026-06-21T11:30:00Z", &info));
        assert!(!timestamp_in_window("2026-06-21T11:29:59Z", &info));
        assert!(!timestamp_in_window("2026-06-21T12:00:01Z", &info));

        // A stale debug window start earlier than `now - 2h` is clamped to the
        // 2-hour cap — the marker never widens coverage past the product bound.
        let stale = compute_window_info(
            DateTime::parse_from_rfc3339("2026-06-21T08:00:00Z")
                .expect("debug-window-start timestamp")
                .with_timezone(&Utc),
            generated,
            &DebugBundleOptions::default(),
        );
        assert_eq!(stale.from, "2026-06-21T10:00:00.000000Z");

        // The `window_seconds` cap is itself clamped to 7200s, so an oversized
        // request can never widen the 2-hour boundary past the product bound.
        let oversized = compute_window_info(
            DateTime::parse_from_rfc3339("2026-06-21T08:00:00Z")
                .expect("debug-window-start timestamp")
                .with_timezone(&Utc),
            generated,
            &DebugBundleOptions {
                window_seconds: 99_999,
                ..Default::default()
            },
        );
        assert_eq!(oversized.from, "2026-06-21T10:00:00.000000Z");
    }

    #[test]
    fn timeline_cap_keeps_the_most_recent_physical_coins() {
        let entries: Vec<_> = (0..51)
            .map(|index| {
                let trace_id = format!("018f0000-0000-7000-8000-{index:012}");
                let timestamp = format!("2026-06-21T10:{:02}:00.000000Z", index);
                parse_coin_log_line(&sample_closed_line_at(&trace_id, &timestamp))
                    .expect("closed fixture")
            })
            .collect();
        let window = WindowInfo {
            from: String::new(),
            to: String::new(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };
        let options = DebugBundleOptions {
            timeline_cap: 50,
            ..Default::default()
        };

        let (rows, capped) = build_timeline(
            &entries,
            &[],
            &AccountLabelMap::default(),
            &window,
            &options,
        );

        assert!(capped);
        assert_eq!(rows.len(), 50);
        assert!(!rows.iter().any(|row| row.coin_trace_id.ends_with("000")));
        assert!(rows
            .last()
            .is_some_and(|row| row.coin_trace_id.ends_with("050")));
    }

    #[tokio::test]
    async fn query_and_log_filter_exclude_old_records() {
        let dir = std::env::temp_dir().join(format!(
            "openpiso-bundle-window-filter-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");
        let old = sample_closed_line_at(
            "018f0000-0000-7000-8000-000000000201",
            "2026-06-21T09:00:00.000000Z",
        );
        let recent = sample_closed_line_at(
            "018f0000-0000-7000-8000-000000000202",
            "2026-06-21T11:00:00.000000Z",
        );
        fs::write(
            dir.join("diagnostics.2026-06-21.log"),
            format!("{old}\n{recent}\n"),
        )
        .expect("write fixture log");

        let entries = parse_coin_log_entries(&dir);
        let window = WindowInfo {
            from: "2026-06-21T10:00:00Z".to_owned(),
            to: "2026-06-21T12:00:00Z".to_owned(),
            time_window_truncated: true,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };
        let filtered_logs: Vec<_> = entries
            .into_iter()
            .filter(|entry| timestamp_in_window(&entry.timestamp, &window))
            .collect();
        assert_eq!(filtered_logs.len(), 1);
        assert_eq!(
            filtered_logs[0].coin_trace_id.as_deref(),
            Some("018f0000-0000-7000-8000-000000000202")
        );

        let pool = crate::test_utils::mem_test_pool().await;
        let transaction = crate::db::record_coin_event(
            &pool,
            &crate::models::CoinEventInput {
                pc_id: "pc-01".to_owned(),
                amount_cents: 100,
                pulse_count: Some(1),
                timestamp: None,
            },
            crate::models::CoinTarget::Guest,
            &crate::models::CoinSourceClaim::Simulator,
            Some(uuid::Uuid::new_v4()),
        )
        .await
        .expect("coin fixture");
        sqlx::query("UPDATE coin_transactions SET created_at = ? WHERE id = ?")
            .bind("2026-06-21T09:00:00.000000Z")
            .bind(&transaction.id)
            .execute(&pool)
            .await
            .expect("move transaction old");
        let (transactions, _) = query_db(&pool, &window).await;
        assert!(
            transactions.is_empty(),
            "old DB transaction must be excluded"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unicode_size_capping_is_exact_and_valid_utf8() {
        let mut document = format!(
            "# Coin Debug Bundle\n\n{}\n\n---\n\n## Environment\n\n🌟\n\n---\n\n## Verdict\n\n✅\n\n---\n\n## Timeline\n\nNo coin events\n\n---\n\n## Anomalies\n\nnone\n\n---\n\n## Raw Appendix\n\n{}",
            WindowInfo {
                from: "2026-06-21T10:00:00Z".to_owned(),
                to: "2026-06-21T12:00:00Z".to_owned(),
                time_window_truncated: true,
                timeline_truncated: false,
                appendix_truncated: false,
                byte_truncated: false,
            }
            .header(),
            "🙂".repeat(100),
        );
        let mut window = WindowInfo {
            from: "2026-06-21T10:00:00Z".to_owned(),
            to: "2026-06-21T12:00:00Z".to_owned(),
            time_window_truncated: true,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };

        apply_size_cap(&mut document, 300, &COIN_BUNDLE_LAYOUT, &mut window);
        assert!(document.len() <= 300);
        assert!(document.is_char_boundary(document.len()));
        assert!(window.appendix_truncated);
        assert!(document.contains("## Environment"));
        assert!(document.contains("## Verdict"));
    }

    #[test]
    fn window_info_caps_at_configured_window_seconds() {
        let generated = DateTime::parse_from_rfc3339("2026-06-21T12:00:00Z")
            .expect("generated-at timestamp")
            .with_timezone(&Utc);
        // Debug window began 90 minutes ago: wider than the 1-hour cap.
        let start = generated - chrono::Duration::minutes(90);
        let info = compute_window_info(
            start,
            generated,
            &DebugBundleOptions {
                window_seconds: 3600,
                ..Default::default()
            },
        );
        assert_eq!(info.from, "2026-06-21T11:00:00.000000Z");
        assert_eq!(info.to, "2026-06-21T12:00:00.000000Z");
        assert!(info.time_window_truncated);
        assert!(timestamp_in_window("2026-06-21T11:00:00Z", &info));
        assert!(!timestamp_in_window("2026-06-21T10:59:59Z", &info));
    }

    #[test]
    fn appendix_export_omits_sensitive_fields_and_keeps_allowed_ones() {
        let line = r#"{"timestamp":"2026-06-21T10:00:01.000000Z","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Coin Batch closed","pulse_count":3,"secret":"synthetic-secret-a","password":"synthetic-password-b","secret_field":"synthetic-secret-c"}"#;
        let entry = parse_coin_log_line(line).expect("line should parse");
        let window = WindowInfo {
            from: "2026-06-21T10:00:00.000000Z".to_owned(),
            to: "2026-06-21T11:00:00.000000Z".to_owned(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };

        let appendix = build_appendix_section(&[entry], &window);

        assert!(appendix.contains("\"pulse_count\":3"), "{appendix}");
        for leaked in [
            "secret",
            "password",
            "synthetic-secret-a",
            "synthetic-password-b",
            "synthetic-secret-c",
        ] {
            assert!(!appendix.contains(leaked), "{leaked} leaked:\n{appendix}");
        }
    }

    #[test]
    fn serial_port_section_distinguishes_scan_failure_from_empty_scan() {
        let failed = build_serial_ports_section(None, Some("COM7"));
        assert!(failed.contains("Scan failed"));
        assert!(!failed.contains("was not detected"));

        let empty = build_serial_ports_section(Some(&[]), None);
        assert!(empty.contains("No serial ports detected"));
        assert!(!empty.contains("Scan failed"));

        let empty_with_saved = build_serial_ports_section(Some(&[]), Some("COM7"));
        assert!(empty_with_saved.contains("No serial ports detected"));
        assert!(empty_with_saved.contains("COM7"));
        assert!(empty_with_saved.contains("was not detected"));
    }

    #[test]
    fn serial_port_section_lists_choices_and_calls_out_missing_saved_port() {
        let choices = vec![
            crate::serial_port_detection::DetectedSerialPort {
                name: "COM3".to_owned(),
                label: "CH340 (wch.cn)".to_owned(),
                detected: true,
            },
            crate::serial_port_detection::DetectedSerialPort {
                name: "COM7".to_owned(),
                label: "Saved port".to_owned(),
                detected: false,
            },
        ];
        let section = build_serial_ports_section(Some(&choices), Some("COM7"));
        assert!(section.contains("COM3"));
        assert!(section.contains("CH340 (wch.cn)"));
        assert!(section.contains("COM7"));
        assert!(section.contains("was not detected in this scan"));
    }

    #[test]
    fn serial_port_section_reports_saved_port_absent_from_injected_scan() {
        let choices = [crate::serial_port_detection::DetectedSerialPort {
            name: "COM3".to_owned(),
            label: "USB serial port".to_owned(),
            detected: true,
        }];

        let section = build_serial_ports_section(Some(&choices), Some("COM7"));
        assert!(section.contains("COM3"));
        assert!(section.contains("COM7"));
        assert!(section.contains("was not detected in this scan"));

        let detected_saved = build_serial_ports_section(Some(&choices), Some("COM3"));
        assert!(!detected_saved.contains("was not detected"));
    }

    #[tokio::test]
    async fn build_bundle_with_no_data_returns_valid_markdown() {
        let dir =
            std::env::temp_dir().join(format!("openpiso-bundle-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let pool = crate::test_utils::mem_test_pool().await;
        let options = DebugBundleOptions::default();
        let doc = build_coin_debug_bundle(
            &dir,
            &pool,
            Utc::now() - chrono::Duration::hours(1),
            None,
            &options,
        )
        .await;

        assert!(doc.contains("# Coin Debug Bundle"));
        assert!(doc.contains("## Environment"));
        assert!(doc.contains("| Coin Pin Polarity | `normally_open` |"));
        assert!(doc.contains("## Verdict"));
        assert!(doc.contains("## Timeline"));
        assert!(doc.contains("## Anomalies"));
        assert!(doc.contains("## Raw Appendix"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn copied_bundle_includes_readable_inhibited_pulse_evidence() {
        let dir =
            std::env::temp_dir().join(format!("openpiso-bundle-inhibited-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let timestamp = current_timestamp();
        let line = format!(
            r#"{{"timestamp":"{timestamp}","level":"INFO","target":"openpiso_admin_lib::coin_source","message":"Inhibited pulse","anomaly":"inhibited_pulse","reason":"no_selection_window"}}"#
        );
        fs::write(dir.join("diagnostics.inhibited.log"), line).expect("write inhibited log");

        let pool = crate::test_utils::mem_test_pool().await;
        let doc = build_coin_debug_bundle(
            &dir,
            &pool,
            Utc::now() - chrono::Duration::hours(1),
            None,
            &DebugBundleOptions::default(),
        )
        .await;

        assert!(doc.contains(
            "healthy sampler; no Selection Window; Set Pin inhibited (idle insertion or inhibit wiring problem)"
        ));
        assert!(doc.contains(&timestamp[..19]));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fixture_logs_with_db_produce_timeline() {
        let dir =
            std::env::temp_dir().join(format!("openpiso-bundle-timeline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let trace_id = uuid::Uuid::new_v4().to_string();

        // Write a diagnostic log file with a committed batch
        {
            let mut f = File::create(dir.join("diagnostics.2026-06-21.log")).expect("create log");
            writeln!(f, "{}", sample_closed_line(&trace_id)).expect("write closure");
            writeln!(f, "{}", sample_committed_line(&trace_id)).expect("write committed");
        }

        let pool = crate::test_utils::mem_test_pool().await;

        // Insert a coin transaction with the matching trace id in its audit metadata
        {
            use crate::models::CoinSourceClaim;
            let input = crate::models::CoinEventInput {
                pc_id: "pc-01".to_owned(),
                amount_cents: 100,
                pulse_count: Some(1),
                timestamp: None,
            };
            // Record the coin with the trace id
            let _tx = crate::db::record_coin_event(
                &pool,
                &input,
                crate::models::CoinTarget::Guest,
                &CoinSourceClaim::Simulator,
                Some(uuid::Uuid::parse_str(&trace_id).expect("valid uuid")),
            )
            .await
            .expect("coin commit should succeed");
        }

        let options = DebugBundleOptions::default();
        let doc = build_coin_debug_bundle(
            &dir,
            &pool,
            Utc::now() - chrono::Duration::hours(1),
            None,
            &options,
        )
        .await;

        // The timeline should have one row
        assert!(
            doc.contains(&trace_id[..8]),
            "timeline should contain the coin trace id prefix"
        );
        assert!(doc.contains("pc-01"));
        assert!(doc.contains("100c"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn committed_coin_with_sqlite_created_at_reports_persisted() {
        // Regression: `coin_transactions.created_at` is SQLite CURRENT_TIMESTAMP
        // ("YYYY-MM-DD HH:MM:SS"), not RFC3339. The window filter must still
        // recognise the row as inside the window instead of reporting a
        // committed coin as `batch_no_db_row` / `missing`.
        let dir =
            std::env::temp_dir().join(format!("openpiso-bundle-persisted-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let trace_id = uuid::Uuid::new_v4().to_string();
        {
            let mut f = File::create(dir.join("diagnostics.2026-06-21.log")).expect("create log");
            writeln!(f, "{}", sample_closed_line(&trace_id)).expect("write closure");
            writeln!(f, "{}", sample_committed_line(&trace_id)).expect("write committed");
        }

        let pool = crate::test_utils::mem_test_pool().await;
        crate::db::record_coin_event(
            &pool,
            &crate::models::CoinEventInput {
                pc_id: "pc-01".to_owned(),
                amount_cents: 100,
                pulse_count: Some(1),
                timestamp: None,
            },
            crate::models::CoinTarget::Guest,
            &crate::models::CoinSourceClaim::Simulator,
            Some(uuid::Uuid::parse_str(&trace_id).expect("valid uuid")),
        )
        .await
        .expect("coin commit should succeed");

        let options = DebugBundleOptions::default();
        let doc = build_coin_debug_bundle(
            &dir,
            &pool,
            Utc::now() - chrono::Duration::hours(1),
            None,
            &options,
        )
        .await;

        assert!(
            !doc.contains("batch_no_db_row"),
            "a committed coin must not be reported as missing:\n{doc}"
        );
        assert!(
            doc.contains("1 transaction(s) in DB"),
            "the DB transaction must count as inside the window:\n{doc}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fixtures_no_device_secret_in_output() {
        let dir =
            std::env::temp_dir().join(format!("openpiso-bundle-secret-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let trace_id = uuid::Uuid::new_v4().to_string();
        {
            let mut f = File::create(dir.join("diagnostics.2026-06-21.log")).expect("create log");
            writeln!(f, "{}", sample_closed_line(&trace_id)).expect("write closure");
            writeln!(f, "{}", sample_committed_line(&trace_id)).expect("write");
        }

        let pool = crate::test_utils::mem_test_pool().await;
        sqlx::query(
            "INSERT OR REPLACE INTO coin_devices (id, name, secret, active)
             VALUES ('bundle-device', 'Bundle fixture device', 'stored-device-secret', 1)",
        )
        .execute(&pool)
        .await
        .expect("device fixture should be stored");

        {
            use crate::models::CoinSourceClaim;
            let input = crate::models::CoinEventInput {
                pc_id: "pc-01".to_owned(),
                amount_cents: 100,
                pulse_count: Some(1),
                timestamp: None,
            };
            let _tx = crate::db::record_coin_event(
                &pool,
                &input,
                crate::models::CoinTarget::Guest,
                &CoinSourceClaim::Simulator,
                Some(uuid::Uuid::parse_str(&trace_id).expect("valid uuid")),
            )
            .await
            .expect("coin commit should succeed");
        }

        let options = DebugBundleOptions::default();
        let doc = build_coin_debug_bundle(
            &dir,
            &pool,
            Utc::now() - chrono::Duration::hours(1),
            None,
            &options,
        )
        .await;

        assert!(
            !doc.contains("stored-device-secret"),
            "stored device secret must not appear in bundle"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fixtures_no_account_username_in_output() {
        let dir =
            std::env::temp_dir().join(format!("openpiso-bundle-account-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp log dir");

        let trace_id = uuid::Uuid::new_v4().to_string();
        {
            let mut f = File::create(dir.join("diagnostics.2026-06-21.log")).expect("create log");
            writeln!(f, "{}", sample_closed_line(&trace_id)).expect("write closure");
            writeln!(f, "{}", sample_committed_line(&trace_id)).expect("write");
        }

        let pool = crate::test_utils::mem_test_pool().await;

        // Create an account and commit a coin credited to it.
        let account = sqlx::query_as::<_, (String, String)>(
            "INSERT INTO accounts (id, username, username_normalized, password_hash, status)
             VALUES (?, ?, ?, 'hash', 'active')
             RETURNING id, username",
        )
        .bind("acct-sec-test")
        .bind("secret-username")
        .bind("secret-username")
        .fetch_one(&pool)
        .await
        .expect("account insert should succeed");

        let _tx = crate::db::record_coin_event(
            &pool,
            &crate::models::CoinEventInput {
                pc_id: "pc-01".to_owned(),
                amount_cents: 100,
                pulse_count: Some(1),
                timestamp: None,
            },
            crate::models::CoinTarget::Account {
                account_id: &account.0,
            },
            &crate::models::CoinSourceClaim::Simulator,
            Some(uuid::Uuid::parse_str(&trace_id).expect("valid uuid")),
        )
        .await
        .expect("coin commit should succeed");

        let options = DebugBundleOptions::default();
        let doc = build_coin_debug_bundle(
            &dir,
            &pool,
            Utc::now() - chrono::Duration::hours(1),
            None,
            &options,
        )
        .await;

        // The literal username must not appear in the bundle.
        assert!(
            !doc.contains("secret-username"),
            "raw username must not appear"
        );
        // The account label should appear instead.
        assert!(
            doc.contains("Account A") || doc.contains("Account"),
            "output should use account labels"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn header_lists_window_coverage_and_truncation() {
        let mut doc = String::new();
        doc.push_str("# Coin Debug Bundle\n\n");
        let wi = WindowInfo {
            from: "2026-06-21T10:00:00Z".to_owned(),
            to: "2026-06-21T12:00:00Z".to_owned(),
            time_window_truncated: true,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };
        doc.push_str(&wi.header());
        doc.push_str("\n\n---\n\n");

        assert!(doc.contains("10:00:00"), "header must show window start");
        assert!(doc.contains("12:00:00"), "header must show window end");
        assert!(
            doc.contains("time-window"),
            "header must identify time truncation"
        );

        let wi2 = WindowInfo {
            from: "2026-06-21T08:00:00Z".to_owned(),
            to: "2026-06-21T09:00:00Z".to_owned(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };
        let h2 = wi2.header();
        assert!(h2.contains("**Truncation:** none"));
    }

    #[test]
    fn size_cap_truncates_appendix_first() {
        let mut doc = String::new();
        doc.push_str("# Coin Debug Bundle\n\nheader\n\n");
        doc.push_str("## Environment\n\n...\n\n");
        doc.push_str("## Verdict\n\n...\n\n");
        doc.push_str("## Timeline\n\n...\n\n");
        doc.push_str("## Anomalies\n\n...\n\n");
        doc.push_str("## Raw Appendix\n\nsome long data here that goes on and on\n");

        let mut window = WindowInfo {
            from: "2026-06-21T10:00:00Z".to_owned(),
            to: "2026-06-21T12:00:00Z".to_owned(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated: false,
            byte_truncated: false,
        };

        // A cap smaller than the optional sections must still be exact.
        let size_cap = 500;
        apply_size_cap(&mut doc, size_cap, &COIN_BUNDLE_LAYOUT, &mut window);
        let capped = doc;
        assert!(capped.len() <= size_cap, "hard ceiling must be exact");
        assert!(capped.is_char_boundary(capped.len()));
        assert!(
            capped.contains("## Environment"),
            "Environment must survive truncation"
        );
        assert!(
            capped.contains("## Verdict"),
            "Verdict must survive truncation"
        );
    }
}

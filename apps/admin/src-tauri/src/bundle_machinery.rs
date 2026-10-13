// Shared machinery for Markdown diagnostic bundle builders (issue #356).
//
// The Coin Debug Bundle and the Support Bundle (ADR-0078) solve the same four
// problems identically: parsing the rotating JSON-lines diagnostic log,
// pseudonymising account identities to stable per-bundle labels,
// re-serialising a raw appendix from a field whitelist, and enforcing a byte
// cap that states its own truncation. Those pieces live here so a second
// builder reuses them rather than duplicating them.
//
// Security: this module is redaction infrastructure. Nothing here selects or
// parses device secrets — appendix fields are kept only when named by a
// caller-supplied whitelist, and pseudonymisation replaces account identities
// with per-bundle labels. Builders must keep secret-bearing columns and field
// names out of every whitelist they pass in.

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde_json::Value;

// ── Diagnostic log parsing ────────────────────────────────────────────

/// One structured entry parsed from the JSON-lines diagnostic log.
pub struct DiagnosticLine {
    /// The full parsed JSON object, for builder-specific typed extraction.
    /// This bypasses the appendix whitelist: never copy a field from it into
    /// bundle output — take appendix content from `raw_fields` only.
    pub json: Value,
    pub target: String,
    pub timestamp: String,
    pub level: String,
    pub message: String,
    /// Fields kept for the raw appendix: the whitelist-filtered subset of the
    /// line's own fields, re-serialised verbatim by `serialise_appendix_entry`.
    pub raw_fields: HashMap<String, Value>,
}

/// Parse one JSON-lines diagnostic entry, keeping only
/// `appendix_whitelist`-named fields for the raw appendix. Lines without a
/// string `target` and `message` are not diagnostic entries.
pub fn parse_diagnostic_json_line(
    line: &str,
    appendix_whitelist: &[&str],
) -> Option<DiagnosticLine> {
    let value: Value = serde_json::from_str(line).ok()?;

    let target = value.get("target")?.as_str()?.to_owned();
    let message = value.get("message")?.as_str()?.to_owned();
    let timestamp = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let level = value
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let raw_fields: HashMap<String, Value> = value
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| appendix_whitelist.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    Some(DiagnosticLine {
        json: value,
        target,
        timestamp,
        level,
        message,
        raw_fields,
    })
}

/// Re-serialise one raw-appendix entry from its whitelist-filtered fields.
pub fn serialise_appendix_entry(raw_fields: &HashMap<String, Value>) -> Option<String> {
    serde_json::to_string(raw_fields).ok()
}

/// Parse every diagnostic log file under `log_dir`, oldest file first,
/// applying `parse_line` to each non-empty line.
pub fn parse_diagnostic_log_dir<T>(
    log_dir: &Path,
    parse_line: impl Fn(&str) -> Option<T>,
) -> Vec<T> {
    parse_log_files(log_files_in_order(log_dir), parse_line)
}

/// Parse the given log files in the given order, applying `parse_line` to
/// each non-empty line.
pub fn parse_log_files<T>(
    files: Vec<std::path::PathBuf>,
    parse_line: impl Fn(&str) -> Option<T>,
) -> Vec<T> {
    let mut entries = Vec::new();
    for file_path in &files {
        let Ok(file) = File::open(file_path) else {
            continue;
        };
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(entry) = parse_line(line) {
                entries.push(entry);
            }
        }
    }
    entries
}

/// Diagnostic log files under `log_dir`, oldest first for chronological
/// order.
pub fn log_files_in_order(log_dir: &Path) -> Vec<std::path::PathBuf> {
    log_files_with_prefix(log_dir, &["diagnostics"])
}

/// Dated `.log` files under `log_dir` whose name starts with one of
/// `prefixes`, oldest first for chronological order. The Support Bundle uses
/// this to read the dedicated `forwarded.*.log` files (ADR-0078) separately
/// from Admin's own `diagnostics.*.log`.
pub fn log_files_with_prefix(log_dir: &Path, prefixes: &[&str]) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return Vec::new();
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "log")
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| prefixes.iter().any(|prefix| n.starts_with(prefix)))
        })
        .collect();
    files.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    files
}

// ── Account pseudonymisation ──────────────────────────────────────────

/// Stable per-bundle labels for account pseudonymisation.
#[derive(Debug, Default)]
pub struct AccountLabelMap {
    next_label: usize,
    map: HashMap<String, String>,
}

impl AccountLabelMap {
    pub fn label(&mut self, account_id: &str) -> String {
        if let Some(label) = self.map.get(account_id) {
            return label.clone();
        }
        let label = format!("Account {}", label_to_letter(self.next_label));
        self.next_label += 1;
        self.map.insert(account_id.to_owned(), label.clone());
        label
    }

    pub fn get(&self, account_id: &str) -> Option<&str> {
        self.map.get(account_id).map(|s| s.as_str())
    }
}

/// A, B, ..., Z, AA, AB, ...
pub fn label_to_letter(n: usize) -> String {
    if n < 26 {
        char::from(b'A' + n as u8).to_string()
    } else {
        format!("{}{}", label_to_letter(n / 26 - 1), label_to_letter(n % 26))
    }
}

// ── Coverage window ───────────────────────────────────────────────────

pub struct WindowInfo {
    pub from: String,
    pub to: String,
    /// The window-start policy is always applied, even when no old evidence
    /// exists. Keeping this explicit prevents an empty result from being
    /// mistaken for an unbounded search.
    pub time_window_truncated: bool,
    pub timeline_truncated: bool,
    pub appendix_truncated: bool,
    pub byte_truncated: bool,
}

impl WindowInfo {
    pub fn header(&self) -> String {
        let mut truncation = Vec::new();
        if self.time_window_truncated {
            truncation.push("time-window");
        }
        if self.timeline_truncated {
            truncation.push("timeline");
        }
        if self.appendix_truncated {
            truncation.push("appendix");
        }
        if self.byte_truncated {
            truncation.push("bytes");
        }
        let truncation = if truncation.is_empty() {
            "none".to_owned()
        } else {
            truncation.join(", ")
        };
        format!(
            "**Coverage:** `{}` to `{}`\n**Truncation:** {}",
            self.from, self.to, truncation
        )
    }
}

/// Parse either an RFC3339 timestamp (diagnostic logs, window bounds) or the
/// SQLite `CURRENT_TIMESTAMP` format "YYYY-MM-DD HH:MM:SS" (DB `created_at`,
/// stored in UTC). Both sources feed the same window comparison.
pub fn parse_flexible_utc(timestamp: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        return Some(parsed.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|naive| naive.and_utc())
}

pub fn timestamp_in_window(timestamp: &str, window: &WindowInfo) -> bool {
    let parsed_timestamp = parse_flexible_utc(timestamp);
    let from = parse_flexible_utc(&window.from);
    let to = parse_flexible_utc(&window.to);

    match (parsed_timestamp, from, to) {
        (Some(timestamp), from, to) => {
            from.is_none_or(|from| timestamp >= from) && to.is_none_or(|to| timestamp <= to)
        }
        _ => {
            (window.from.is_empty() || timestamp >= window.from.as_str())
                && (window.to.is_empty() || timestamp <= window.to.as_str())
        }
    }
}

pub fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

// ── Size capping ──────────────────────────────────────────────────────

/// Section names and notices a concrete bundle builder binds the size-cap
/// machinery to. Sections render in the documented order — Environment,
/// Verdict, Timeline, Anomalies, raw appendix — so findings precede data and
/// survive truncation, with the appendix the first evidence sacrificed.
pub struct BundleLayout {
    /// The document's title line, e.g. `"# Coin Debug Bundle"`.
    pub title: &'static str,
    /// Heading that begins the re-serialised raw appendix.
    pub appendix_marker: &'static str,
    /// Notice substituted when the appendix is dropped for size.
    pub appendix_notice: &'static str,
    /// Heading that begins the timeline.
    pub timeline_marker: &'static str,
    /// Heading that begins the anomalies section.
    pub anomalies_marker: &'static str,
    /// Notice substituted when anomaly detail is dropped for size.
    pub anomalies_notice: &'static str,
    /// Heading that begins the mandatory environment section.
    pub environment_marker: &'static str,
    /// Heading that begins the mandatory verdict section.
    pub verdict_marker: &'static str,
    /// Notice substituted for a mandatory section's body in the minimal
    /// fallback document.
    pub section_notice: &'static str,
}

/// Enforce a hard byte ceiling on a finished bundle document, sacrificing
/// optional evidence in order — appendix, oldest timeline rows, anomaly
/// detail — and stating every truncation in the window header. A pathological
/// ceiling below the mandatory sections still yields a valid UTF-8 bounded
/// document.
pub fn apply_size_cap(
    document: &mut String,
    max_bytes: usize,
    layout: &BundleLayout,
    window: &mut WindowInfo,
) {
    if document.len() <= max_bytes {
        return;
    }

    // Optional appendix content is the first evidence sacrificed.
    if drop_section_detail(document, layout.appendix_marker, layout.appendix_notice) {
        window.appendix_truncated = true;
        rewrite_header(document, layout, window);
    }
    if document.len() <= max_bytes {
        return;
    }

    // Keep the newest timeline rows by removing the oldest.
    while document.len() > max_bytes && remove_oldest_timeline_row(document, layout.timeline_marker)
    {
        window.timeline_truncated = true;
        rewrite_header(document, layout, window);
    }
    if document.len() <= max_bytes {
        return;
    }

    // Anomalies are optional evidence too. They are compacted only after the
    // appendix and timeline have been reduced.
    if drop_section_detail(document, layout.anomalies_marker, layout.anomalies_notice) {
        window.byte_truncated = true;
        rewrite_header(document, layout, window);
    }
    if document.len() <= max_bytes {
        return;
    }

    // Preserve the complete mandatory sections whenever the configured
    // ceiling permits it.
    window.byte_truncated = true;
    let timeline_pos = document
        .find(layout.timeline_marker)
        .unwrap_or(document.len());
    let mandatory = document[..timeline_pos].to_owned();
    if mandatory.len() <= max_bytes {
        *document = mandatory;
    } else {
        let minimal = format!(
            "{}\n\n{}\n\n---\n\n{}\n\n{}\n\n---\n\n{}\n\n{}\n",
            layout.title,
            window.header(),
            layout.environment_marker,
            layout.section_notice,
            layout.verdict_marker,
            layout.section_notice,
        );
        *document = truncate_utf8(&minimal, max_bytes);
    }
    rewrite_header(document, layout, window);
    if document.len() > max_bytes {
        *document = truncate_utf8(document, max_bytes);
    }
}

/// Drop an optional section's detail, leaving its heading and notice, and
/// report whether the section was present to be dropped.
fn drop_section_detail(document: &mut String, marker: &str, notice: &str) -> bool {
    let Some(pos) = document.rfind(marker) else {
        return false;
    };
    document.truncate(pos);
    document.push_str(marker);
    document.push_str("\n\n");
    document.push_str(notice);
    document.push('\n');
    true
}

/// Remove the oldest timeline data row from a rendered document, keeping the
/// newest rows.
pub fn remove_oldest_timeline_row(document: &mut String, timeline_marker: &str) -> bool {
    let Some(start) = document.find(timeline_marker) else {
        return false;
    };
    let end = document[start..]
        .find("\n\n---\n\n")
        .map(|offset| start + offset)
        .unwrap_or(document.len());
    let section = &document[start..end];
    let mut offset = 0;
    for line in section.split_inclusive('\n') {
        let fields = line.trim().trim_matches('|').split('|').collect::<Vec<_>>();
        let is_data_row = fields
            .first()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .is_some();
        if is_data_row {
            document.replace_range(start + offset..start + offset + line.len(), "");
            return true;
        }
        offset += line.len();
    }
    false
}

/// Rewrite the coverage header directly under the document title, so the
/// stated truncation always matches the capping just applied.
pub fn rewrite_header(document: &mut String, layout: &BundleLayout, window: &WindowInfo) {
    let prefix_len = layout.title.len() + "\n\n".len();
    let Some(separator) = document.find("\n\n---\n\n") else {
        return;
    };
    if separator < prefix_len {
        return;
    }
    let replacement = format!("{}\n\n---\n\n", window.header());
    document.replace_range(prefix_len..separator + "\n\n---\n\n".len(), &replacement);
}

pub fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_owned()
}

/// Apply the size cap, then rewrite the coverage header so the stated
/// truncation always matches what was just cut. Header text is part of the
/// hard ceiling too. Both concrete bundle builders end with this.
pub fn finalize_document(
    document: &mut String,
    max_bytes: usize,
    layout: &BundleLayout,
    window: &mut WindowInfo,
) {
    apply_size_cap(document, max_bytes, layout, window);
    rewrite_header(document, layout, window);
    if document.len() > max_bytes {
        window.byte_truncated = true;
        rewrite_header(document, layout, window);
        *document = truncate_utf8(document, max_bytes);
    }
}

/// Keep the newest `cap` rows of an already-chronological list, reporting
/// whether the cap actually cut anything.
pub fn keep_newest<T>(rows: Vec<T>, cap: usize) -> (Vec<T>, bool) {
    let was_capped = rows.len() > cap;
    let mut rows = rows;
    if was_capped {
        let start = rows.len() - cap;
        rows = rows.split_off(start);
    }
    (rows, was_capped)
}

// ── Shared rendering helpers ──────────────────────────────────────────

/// Escape free text for one Markdown table cell.
pub fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Shorten a timestamp to "YYYY-MM-DD hh:mm:ss" for table cells. RFC3339 and
/// SQLite `CURRENT_TIMESTAMP` stamps differ only after the 19th character.
pub fn truncate_timestamp(ts: &str) -> String {
    ts.chars().take(19).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keeps_only_whitelisted_fields_for_appendix() {
        let line = r#"{"timestamp":"2026-06-21T10:00:01Z","level":"INFO","target":"t::x","message":"m","pc_id":"pc-01","secret_field":"nope"}"#;
        let parsed =
            parse_diagnostic_json_line(line, &["timestamp", "level", "target", "message", "pc_id"])
                .expect("diagnostic line should parse");
        assert_eq!(parsed.target, "t::x");
        assert_eq!(parsed.message, "m");
        assert_eq!(
            parsed.raw_fields.get("pc_id").and_then(|v| v.as_str()),
            Some("pc-01")
        );
        assert!(
            !parsed.raw_fields.contains_key("secret_field"),
            "non-whitelisted fields must not reach the appendix"
        );
    }

    #[test]
    fn parse_rejects_lines_without_target_or_message() {
        assert!(parse_diagnostic_json_line("not json", &[]).is_none());
        assert!(parse_diagnostic_json_line(r#"{"message":"m"}"#, &[]).is_none());
        assert!(parse_diagnostic_json_line(r#"{"target":"t"}"#, &[]).is_none());
    }

    #[test]
    fn label_to_letter_cycles_correctly() {
        assert_eq!(label_to_letter(0), "A");
        assert_eq!(label_to_letter(1), "B");
        assert_eq!(label_to_letter(25), "Z");
        assert_eq!(label_to_letter(26), "AA");
        assert_eq!(label_to_letter(27), "AB");
        assert_eq!(label_to_letter(51), "AZ");
        assert_eq!(label_to_letter(52), "BA");
    }

    #[test]
    fn size_cap_is_generic_over_bundle_layout() {
        // A second builder binds the same machinery to its own markers.
        let layout = BundleLayout {
            title: "# Support Bundle",
            appendix_marker: "## Raw Evidence",
            appendix_notice: "*(evidence omitted)*",
            timeline_marker: "## Events",
            anomalies_marker: "## Findings",
            anomalies_notice: "*(findings omitted)*",
            environment_marker: "## Host",
            verdict_marker: "## Summary",
            section_notice: "*(omitted)*",
        };
        let window_fields = |appendix_truncated| WindowInfo {
            from: "2026-06-21T10:00:00Z".to_owned(),
            to: "2026-06-21T12:00:00Z".to_owned(),
            time_window_truncated: false,
            timeline_truncated: false,
            appendix_truncated,
            byte_truncated: false,
        };
        let mut document = format!(
            "# Support Bundle\n\n{}\n\n---\n\n## Host\n\nenv\n\n---\n\n## Summary\n\nverdict\n\n---\n\n## Events\n\nevents\n\n---\n\n## Findings\n\nfindings\n\n---\n\n## Raw Evidence\n\n{}",
            window_fields(false).header(),
            "x".repeat(400),
        );
        let mut window = window_fields(false);

        apply_size_cap(&mut document, 400, &layout, &mut window);

        assert!(document.len() <= 400);
        assert!(document.is_char_boundary(document.len()));
        assert!(window.appendix_truncated);
        assert!(document.starts_with("# Support Bundle"));
        assert!(document.contains("## Host"));
        assert!(document.contains("## Summary"));
        assert!(
            document.contains("appendix"),
            "truncation must be stated in the header"
        );
    }
}

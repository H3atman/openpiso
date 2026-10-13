//! Admin's Forwarded Diagnostics sink (ADR-0078).
//!
//! Appends accepted Forwarded Diagnostics to a dedicated daily-rotating JSON
//! file beside Admin's own diagnostics — never into SQLite and never
//! interleaved into Admin's own log, so a shop full of PCs cannot push Admin's
//! own lines out of retention. Every line carries the PC identity and the
//! source discriminator (`client` or `guard`) so the Support Bundle builder
//! can filter without guessing.

use std::{fs, io::Write, path::PathBuf};

use chrono::{DateTime, Local, Utc};
use openpiso_core::forwarding::{
    ForwardedDiagnostic, ForwardedSource, MAX_FORWARDED_BATCH_LINES, MAX_FORWARDED_MESSAGE_BYTES,
};
use serde::Deserialize;

use crate::errors::{AppError, AppResult};

pub const FORWARDED_LOG_FILE_PREFIX: &str = "forwarded";

/// HTTP wire format for the batched Forwarded Diagnostics endpoint. The PC
/// identity is resolved server-side from the `X-OpenPiso-Mac` header — the
/// body never carries it — and `source` says which device surface produced
/// the lines.
#[derive(Debug, Deserialize)]
pub struct ForwardedDiagnosticBatch {
    pub source: ForwardedSource,
    pub lines: Vec<ForwardedDiagnostic>,
    /// Echo of the Diagnostic Request nonce (ADR-0078): present only on the
    /// batches that carry the windowed, byte-capped log slice answering the
    /// owner's ask. Admin stops carrying the nonce on Heartbeat responses
    /// once it sees it back. Regular forwarding batches omit the field.
    #[serde(default)]
    pub diagnostic_request_nonce: Option<String>,
}

impl ForwardedDiagnosticBatch {
    /// All-or-nothing validation at the Rust boundary: one bad line rejects
    /// the whole batch, so nothing is partially accepted.
    pub fn validate(&self) -> AppResult<()> {
        if self.lines.is_empty() {
            return Err(AppError::InvalidInput(
                "a forwarded batch must carry at least one line".to_owned(),
            ));
        }
        if self.lines.len() > MAX_FORWARDED_BATCH_LINES {
            return Err(AppError::InvalidInput(format!(
                "a forwarded batch may carry at most {MAX_FORWARDED_BATCH_LINES} lines"
            )));
        }
        for line in &self.lines {
            if line.target.trim().is_empty() || line.message.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "forwarded lines must carry a target and a message".to_owned(),
                ));
            }
            if line.message.len() > MAX_FORWARDED_MESSAGE_BYTES {
                return Err(AppError::InvalidInput(format!(
                    "forwarded messages may be at most {MAX_FORWARDED_MESSAGE_BYTES} bytes"
                )));
            }
            if line.timestamp_unix_ms < 0 {
                return Err(AppError::InvalidInput(
                    "forwarded line timestamps must not be negative".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Appends accepted Forwarded Diagnostics to
/// `<log_dir>/forwarded.<date>.log`, one JSON object per line. Rotation is by
/// file name date; retention is Admin's existing seven-day log cleanup.
pub struct ForwardedLogSink {
    log_dir: PathBuf,
}

impl ForwardedLogSink {
    pub fn new(log_dir: PathBuf) -> Self {
        Self { log_dir }
    }

    pub fn append(
        &self,
        pc_id: &str,
        source: ForwardedSource,
        lines: &[ForwardedDiagnostic],
    ) -> AppResult<()> {
        if lines.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.log_dir)?;
        let received_at = Utc::now().to_rfc3339();
        let file_name = forwarded_file_name(Local::now());
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir.join(file_name))?;
        for line in lines {
            let record = serde_json::json!({
                // Admin's own receive time; tracing would stamp the same role
                // for Admin's own log lines.
                "received_at": received_at,
                // The device wall-clock time the line was captured, which can
                // be hours older after an Admin outage.
                "timestamp": unix_ms_to_rfc3339(line.timestamp_unix_ms),
                "level": line.level.label(),
                "target": line.target,
                "message": line.message,
                "pc_id": pc_id,
                "source": source.label(),
            });
            writeln!(file, "{record}")?;
        }
        file.flush()?;
        Ok(())
    }
}

fn forwarded_file_name(now: DateTime<Local>) -> String {
    format!("{FORWARDED_LOG_FILE_PREFIX}.{}.log", now.format("%Y-%m-%d"))
}

/// Sink for test servers whose tests never read the file. The dedicated
/// forwarded-diagnostics tests build their own temp-dir sink instead.
#[cfg(any(test, feature = "test-harness"))]
pub(crate) fn test_forwarded_log() -> std::sync::Arc<ForwardedLogSink> {
    std::sync::Arc::new(ForwardedLogSink::new(
        std::env::temp_dir().join(format!("openpiso-test-forwarded-{}", std::process::id())),
    ))
}

fn unix_ms_to_rfc3339(unix_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpiso_core::forwarding::ForwardedLevel;
    use tempfile::tempdir;

    fn sink() -> (ForwardedLogSink, tempfile::TempDir) {
        let dir = tempdir().expect("temp dir");
        (ForwardedLogSink::new(dir.path().to_path_buf()), dir)
    }

    fn line(level: ForwardedLevel, message: &str) -> ForwardedDiagnostic {
        ForwardedDiagnostic::new(level, "openpiso_client::tamper", message, 1_760_000_000_000)
    }

    #[test]
    fn accepted_lines_land_in_the_forwarded_log_with_pc_and_source() {
        let (sink, dir) = sink();
        sink.append(
            "pc-01",
            ForwardedSource::Client,
            &[line(ForwardedLevel::Warn, "watch out")],
        )
        .expect("append should succeed");
        sink.append(
            "pc-02",
            ForwardedSource::Guard,
            &[line(ForwardedLevel::Error, "broken")],
        )
        .expect("append should succeed");

        let files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("log dir should exist")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            files.len(),
            1,
            "same-day appends share one daily-rotating file: {files:?}"
        );
        assert!(files[0].starts_with(FORWARDED_LOG_FILE_PREFIX));

        let written = std::fs::read_to_string(dir.path().join(&files[0]))
            .expect("forwarded log should be readable");
        let parsed: Vec<serde_json::Value> = written
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line should be JSON"))
            .collect();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["pc_id"], "pc-01");
        assert_eq!(parsed[0]["source"], "client");
        assert_eq!(parsed[0]["level"], "warn");
        assert_eq!(parsed[0]["message"], "watch out");
        assert_eq!(parsed[0]["target"], "openpiso_client::tamper");
        assert_eq!(parsed[0]["timestamp"], "2025-10-09T08:53:20+00:00");
        assert_eq!(parsed[1]["pc_id"], "pc-02");
        assert_eq!(parsed[1]["source"], "guard");
    }

    #[test]
    fn empty_batches_write_nothing() {
        let (sink, dir) = sink();
        sink.append("pc-01", ForwardedSource::Client, &[])
            .expect("empty append should succeed");
        assert!(
            std::fs::read_dir(dir.path())
                .expect("log dir should exist")
                .next()
                .is_none(),
            "no file should be created for an empty batch"
        );
    }

    #[test]
    fn batch_validation_rejects_malformed_lines_without_partial_acceptance() {
        let too_long = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: vec![
                line(ForwardedLevel::Warn, "fine"),
                ForwardedDiagnostic::new(
                    ForwardedLevel::Error,
                    "openpiso_client",
                    "x".repeat(MAX_FORWARDED_MESSAGE_BYTES + 1),
                    1,
                ),
            ],
        };
        assert!(too_long.validate().is_err());

        let empty_message = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: vec![ForwardedDiagnostic::new(
                ForwardedLevel::Warn,
                "openpiso_client",
                "   ",
                1,
            )],
        };
        assert!(empty_message.validate().is_err());

        let missing_target = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: vec![ForwardedDiagnostic::new(
                ForwardedLevel::Warn,
                "  ",
                "message",
                1,
            )],
        };
        assert!(missing_target.validate().is_err());

        let negative_timestamp = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: vec![ForwardedDiagnostic::new(
                ForwardedLevel::Warn,
                "openpiso_client",
                "message",
                -1,
            )],
        };
        assert!(negative_timestamp.validate().is_err());

        let empty_batch = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: Vec::new(),
        };
        assert!(empty_batch.validate().is_err());

        let oversized_batch = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: std::iter::repeat_n(
                line(ForwardedLevel::Warn, "flood"),
                MAX_FORWARDED_BATCH_LINES + 1,
            )
            .collect(),
        };
        assert!(oversized_batch.validate().is_err());

        let valid = ForwardedDiagnosticBatch {
            source: ForwardedSource::Client,
            diagnostic_request_nonce: None,
            lines: vec![line(ForwardedLevel::Error, "fine")],
        };
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn file_name_rotates_by_local_date() {
        use chrono::TimeZone;
        let date = Local.with_ymd_and_hms(2026, 9, 5, 23, 59, 59).unwrap();
        assert_eq!(forwarded_file_name(date), "forwarded.2026-09-05.log");
    }
}

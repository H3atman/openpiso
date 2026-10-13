//! Admin's own host health (ADR-0082 / #378): the Run Records newest first
//! and the panic file's presence, last-modified time, and newest 8 KB.
//!
//! One collector serves both the owner's screen (`GET
//! /web-admin/host-diagnostics`) and the Support Bundle's Admin Host
//! section, so what the owner reads and what support reads can never
//! disagree. Collection is read-only and never errors: a missing or corrupt
//! Run Record file is an empty history and an absent panic file is a stated
//! absence — the record is history, not authority.

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::run_record::{CurrentRun, ExitReason, RunRecord, RunRecordStore};

/// How much of the panic file the owner sees: its newest 8 KB. The full file
/// stays on disk; the cap keeps the bundle honest instead of quietly
/// truncating the timeline to make room (ADR-0082).
pub const PANIC_SLICE_BYTES: usize = 8 * 1024;

/// Admin's own health at collection time.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HostDiagnostics {
    /// Run Records, newest first.
    pub runs: Vec<HostRunRecord>,
    pub panic_file: PanicFileEvidence,
}

/// One Run Record rendered for the owner, with the suspected-hard-exit
/// classification ADR-0082 defines applied.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostRunRecord {
    pub version: String,
    pub pid: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_reason: Option<ExitReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_update_result: Option<crate::app_update::UpdateResult>,
    /// A start with no end on any run but the live one (ADR-0082): a plainly
    /// stated absence of evidence, never a claim about the cause. The
    /// process serving this evidence is never suspected of having exited.
    pub suspected_hard_exit: bool,
}

/// The panic file's evidence. `newest_8kb` is a lossy-UTF-8 decode of the
/// file's newest [`PANIC_SLICE_BYTES`] bytes — the panic record is text —
/// itself never longer than [`PANIC_SLICE_BYTES`], and `None` when the file is
/// absent or unreadable.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PanicFileEvidence {
    pub exists: bool,
    pub size_bytes: u64,
    pub last_modified_at: Option<String>,
    pub newest_8kb: Option<String>,
}

/// Collect Admin's host health: Run Records newest first plus the panic
/// file's evidence. Never errors; every gap is stated as an absence.
pub fn collect(run_records: &RunRecordStore, log_dir: &Path) -> HostDiagnostics {
    // The live run is the exact record this process wrote, never merely one
    // carrying this PID: Windows reuses PIDs, so an older unfinished record
    // can hold the number this process now has (ADR-0082).
    let current = run_records.current_run();
    let runs = run_records
        .load_history()
        .into_iter()
        .rev()
        .map(|record| host_run_record(record, current.as_ref()))
        .collect();
    HostDiagnostics {
        runs,
        panic_file: collect_panic_file(log_dir),
    }
}

fn host_run_record(record: RunRecord, current: Option<&CurrentRun>) -> HostRunRecord {
    let is_current = current.is_some_and(|current| current.matches(&record));
    let suspected_hard_exit = record.ended_at.is_none() && !is_current;
    HostRunRecord {
        version: record.version,
        pid: record.pid,
        started_at: record.started_at.to_rfc3339(),
        ended_at: record.ended_at.map(|at| at.to_rfc3339()),
        exit_reason: record.exit_reason,
        app_update_result: record.app_update_result,
        suspected_hard_exit,
    }
}

fn collect_panic_file(log_dir: &Path) -> PanicFileEvidence {
    let path = log_dir.join(crate::diagnostics::PANIC_FILE_NAME);
    // One handle answers size, modification time, and bytes, so the three
    // parts of the reply describe one file rather than three moments. The
    // panic hook appends from another thread; a later append must not widen
    // what this response promised.
    let Ok(mut file) = fs::File::open(&path) else {
        return PanicFileEvidence::default();
    };
    let Ok(metadata) = file.metadata() else {
        return PanicFileEvidence::default();
    };
    let size_bytes = metadata.len();
    let last_modified_at = metadata
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|at| at.to_rfc3339());
    PanicFileEvidence {
        exists: true,
        size_bytes,
        last_modified_at,
        newest_8kb: read_newest_slice(&mut file, size_bytes),
    }
}

/// The newest `PANIC_SLICE_BYTES` bytes of the snapshot `size_bytes`
/// describes, read from the tail so a large panic file costs one bounded
/// read. The read is capped at `PANIC_SLICE_BYTES`, so bytes appended after
/// the snapshot cannot expand the returned slice, and the decoded text is
/// capped at the same size: a slice that starts mid-character, or holds
/// bytes that are not text at all, would otherwise grow by three bytes for
/// every replacement character and break the documented bound.
fn read_newest_slice<R: Read + Seek>(file: &mut R, size_bytes: u64) -> Option<String> {
    let start = size_bytes.saturating_sub(PANIC_SLICE_BYTES as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let capped = size_bytes - start;
    let mut bytes = Vec::with_capacity(capped as usize);
    file.take(capped).read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    trim_to_newest_bytes(&mut text, PANIC_SLICE_BYTES);
    Some(text)
}

/// Drops leading characters until the text is at most `limit` bytes, keeping
/// the newest evidence. Cutting on a character boundary can leave the result
/// a little under the limit, which the bound allows.
fn trim_to_newest_bytes(text: &mut String, limit: usize) {
    if text.len() <= limit {
        return;
    }
    let mut cut = text.len() - limit;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.drain(..cut);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_record::RUN_RECORDS_FILE_NAME;
    use chrono::TimeZone;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn at(secs: i64) -> DateTime<Utc> {
        let midnight = Utc
            .with_ymd_and_hms(2026, 9, 9, 0, 0, 0)
            .single()
            .expect("valid instant");
        midnight + chrono::Duration::seconds(secs)
    }

    fn record(version: &str, pid: u32, started_secs: i64, ended_secs: Option<i64>) -> RunRecord {
        RunRecord {
            version: version.to_owned(),
            pid,
            started_at: at(started_secs),
            ended_at: ended_secs.map(at),
            exit_reason: ended_secs.map(|_| ExitReason::ApplicationExit),
            app_update_result: None,
        }
    }

    #[test]
    fn records_report_newest_first_with_the_suspected_hard_exit_named() {
        let dir = tempdir().expect("temp dir");
        crate::test_utils::seed_run_records(
            dir.path(),
            &[
                record("0.8.0", 100, 0, Some(60)),
                record("0.9.0", 424_242, 120, None),
            ],
        );
        let store = RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        );

        let host = collect(&store, dir.path());

        assert_eq!(host.runs.len(), 2);
        let newest = &host.runs[0];
        assert_eq!(newest.version, "0.9.0");
        assert_eq!(newest.pid, 424_242);
        assert_eq!(newest.ended_at, None);
        assert_eq!(newest.exit_reason, None);
        assert!(
            newest.suspected_hard_exit,
            "a start with no end on a past run is a suspected hard exit"
        );
        let older = &host.runs[1];
        assert_eq!(older.version, "0.8.0");
        assert!(older.ended_at.is_some());
        assert_eq!(older.exit_reason, Some(ExitReason::ApplicationExit));
        assert!(!older.suspected_hard_exit);
    }

    #[test]
    fn the_live_run_is_never_a_suspected_hard_exit() {
        let dir = tempdir().expect("temp dir");
        let store = RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        );
        store
            .start(env!("CARGO_PKG_VERSION"))
            .expect("this run writes its record");

        let host = collect(&store, dir.path());

        assert_eq!(host.runs.len(), 1);
        assert!(
            !host.runs[0].suspected_hard_exit,
            "the process serving this evidence has not exited"
        );
    }

    #[test]
    fn an_older_unfinished_record_sharing_this_pid_is_a_suspected_hard_exit() {
        let dir = tempdir().expect("temp dir");
        // Windows reuses PIDs, so a dead run's record can carry the number
        // this process now has. Only the record this run actually wrote is
        // live; every other unfinished record is a suspected hard exit.
        crate::test_utils::seed_run_records(
            dir.path(),
            &[record("0.8.0", std::process::id(), 0, None)],
        );
        let store = RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        );
        store
            .start(env!("CARGO_PKG_VERSION"))
            .expect("this run writes its record");

        let host = collect(&store, dir.path());

        assert_eq!(host.runs.len(), 2);
        assert!(
            !host.runs[0].suspected_hard_exit,
            "the record this run wrote is the live one"
        );
        let older = &host.runs[1];
        assert_eq!(older.version, "0.8.0");
        assert_eq!(older.pid, std::process::id());
        assert!(
            older.suspected_hard_exit,
            "a reused PID must not pass an older run off as still running"
        );
    }

    #[test]
    fn the_slice_stays_within_the_cap_when_the_file_is_not_text() {
        // A tail that starts mid-character, or holds bytes that are not text
        // at all, decodes to three bytes per replacement character. The
        // documented 8 KB bound is about what the response carries, so the
        // decoded text is capped too.
        let snapshot_len = PANIC_SLICE_BYTES as u64;
        let mut source = std::io::Cursor::new(vec![0xFF_u8; PANIC_SLICE_BYTES]);

        let slice = read_newest_slice(&mut source, snapshot_len).expect("slice reads");

        assert!(
            slice.len() <= PANIC_SLICE_BYTES,
            "the decoded slice must not exceed the documented cap: {}",
            slice.len()
        );
        assert!(
            slice.ends_with('\u{FFFD}'),
            "the newest bytes are the ones kept"
        );
    }

    #[test]
    fn the_bounded_reader_ignores_bytes_appended_after_the_snapshot() {
        // A real append race would be flaky; supplying more bytes than the
        // recorded length states the same case deterministically.
        let snapshot_len = PANIC_SLICE_BYTES as u64;
        let mut bytes = vec![b'A'; PANIC_SLICE_BYTES];
        bytes.extend_from_slice(b"APPENDED-AFTER-THE-SNAPSHOT");
        let mut source = std::io::Cursor::new(bytes);

        let slice = read_newest_slice(&mut source, snapshot_len).expect("slice reads");

        assert_eq!(slice.len(), PANIC_SLICE_BYTES);
        assert!(
            !slice.contains("APPENDED-AFTER-THE-SNAPSHOT"),
            "bytes written after the snapshot cannot expand the slice"
        );
    }

    #[test]
    fn an_empty_history_and_an_absent_panic_file_report_cleanly() {
        let dir = tempdir().expect("temp dir");
        let store = RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        );

        let host = collect(&store, dir.path());

        assert_eq!(host, HostDiagnostics::default());
    }

    #[test]
    fn the_panic_slice_is_the_newest_eight_kilobytes() {
        let dir = tempdir().expect("temp dir");
        let store = RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        );
        let size_bytes = crate::test_utils::write_panic_slice_fixture(dir.path());

        let host = collect(&store, dir.path());

        let panic = &host.panic_file;
        assert!(panic.exists);
        assert_eq!(panic.size_bytes, size_bytes);
        assert!(panic.last_modified_at.is_some());
        let slice = panic.newest_8kb.as_deref().expect("slice is read");
        assert_eq!(slice.len(), PANIC_SLICE_BYTES, "the slice is exactly 8 KB");
        assert!(slice.contains("TAIL-MARKER crash evidence"));
        assert!(
            !slice.contains("OLD-EVIDENCE"),
            "bytes older than the newest 8 KB stay on disk"
        );
    }

    #[test]
    fn a_small_or_empty_panic_file_is_carried_whole() {
        let dir = tempdir().expect("temp dir");
        let store = RunRecordStore::new(
            dir.path().join(RUN_RECORDS_FILE_NAME),
            Arc::new(crate::run_record::SystemClock),
        );
        let panic_file = dir.path().join(crate::diagnostics::PANIC_FILE_NAME);
        fs::write(&panic_file, "small panic record\n").expect("panic file writes");
        let host = collect(&store, dir.path());
        assert_eq!(
            host.panic_file.newest_8kb.as_deref(),
            Some("small panic record\n"),
            "a file below the cap is carried whole"
        );

        fs::write(&panic_file, "").expect("empty panic file writes");
        let host = collect(&store, dir.path());
        assert!(host.panic_file.exists);
        assert_eq!(host.panic_file.size_bytes, 0);
        assert_eq!(host.panic_file.newest_8kb.as_deref(), Some(""));
    }
}

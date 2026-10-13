//! Admin Run Records (ADR-0082): one record per process lifetime — app
//! version, PID, start time, and, once the process exits through its own
//! shutdown path, an end time and Exit Reason.
//!
//! Records are a rolling set of the newest ten in a single JSON file in the
//! Admin data directory, deliberately outside the rotating log directory so
//! the ADR-0016 retention sweep never removes them. The file is fixed-size
//! state, not authority: it holds no money, time, session, or Points data, so
//! losing it costs history, not correctness.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::errors::AppResult;

/// The Run Record file name. It lives at the root of the Admin data
/// directory, never under `logs/`, so the retention sweep cannot reach it
/// (ADR-0082).
pub static RUN_RECORDS_FILE_NAME: &str = "run_records.json";

/// The rolling ceiling: the file keeps the newest ten records (ADR-0082).
const MAX_RUN_RECORDS: usize = 10;

/// Why the process ended, when it ended through its own shutdown path
/// (ADR-0082). A record without one is classified at startup as a
/// suspected hard exit instead — an absence of evidence, never a claim
/// about the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// The owner or OS closed the kiosk window.
    WindowClosed,
    /// An application-initiated exit.
    ApplicationExit,
    /// A Rust panic reached the hook.
    Panic,
}

impl ExitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ExitReason::WindowClosed => "window closed",
            ExitReason::ApplicationExit => "application exit",
            ExitReason::Panic => "panic",
        }
    }
}

/// One process lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub version: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    /// Set only when the process exited through its own shutdown path.
    pub ended_at: Option<DateTime<Utc>>,
    pub exit_reason: Option<ExitReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_update_result: Option<crate::app_update::UpdateResult>,
}

/// What the previous run's newest record says, classified at startup before
/// the new record is appended (ADR-0082).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousRunEnd {
    /// No previous records: the first run, or a lost history.
    None,
    /// The previous run reached its own shutdown path. The reason is absent
    /// only in a hand-trimmed record; this code always writes both together.
    Completed(Option<ExitReason>),
    /// The previous run has a start and no end: it died without reaching its
    /// own shutdown path. This is a plainly stated absence of evidence, never
    /// a claim about the cause.
    SuspectedHardExit,
}

/// Classifies the previous run from the stored history, whose records are in
/// append order. A newest record with a start and no end is a suspected hard
/// exit; a completed record names its Exit Reason.
pub fn classify(history: &[RunRecord]) -> PreviousRunEnd {
    match history.last() {
        None => PreviousRunEnd::None,
        Some(record) => match record.ended_at {
            Some(_) => PreviousRunEnd::Completed(record.exit_reason),
            None => PreviousRunEnd::SuspectedHardExit,
        },
    }
}

/// Wall-clock source, injected so tests can drive start and end times.
pub trait RunRecordClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl RunRecordClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Which stored record belongs to the process holding this store.
///
/// A PID alone cannot answer that: Windows reuses PIDs, so an older
/// unfinished record can carry the number this process now has. The start
/// timestamp this run actually wrote pins the record exactly, and it is held
/// in memory only — the persisted JSON is unchanged (ADR-0082).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentRun {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

impl CurrentRun {
    /// Whether a stored record is the one this run wrote.
    pub fn matches(&self, record: &RunRecord) -> bool {
        record.pid == self.pid && record.started_at == self.started_at
    }
}

/// Reads and writes the Run Record file.
pub struct RunRecordStore {
    path: PathBuf,
    clock: Arc<dyn RunRecordClock>,
    // Serializes load-modify-save across the event-loop threads and the
    // panic-hook thread; recovered on poisoning, never propagated.
    io_lock: Mutex<()>,
    // Set only once this run's startup record is safely on disk. Until then
    // this process owns no record, so completion has nothing to write.
    current: Mutex<Option<CurrentRun>>,
}

impl RunRecordStore {
    pub fn new(path: PathBuf, clock: Arc<dyn RunRecordClock>) -> Self {
        Self {
            path,
            clock,
            io_lock: Mutex::new(()),
            current: Mutex::new(None),
        }
    }

    /// This run's exact record identity, or `None` when the startup record
    /// was never written.
    pub fn current_run(&self) -> Option<CurrentRun> {
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The stored history, newest last. A missing or corrupt file is an
    /// empty history rather than an error: the record is history, not
    /// authority (ADR-0082).
    pub fn load_history(&self) -> Vec<RunRecord> {
        let Ok(contents) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        serde_json::from_str(&contents).unwrap_or_default()
    }

    /// Writes this run's startup record — version, PID, start time, no Exit
    /// Reason — after classifying the previous run from the history before
    /// the new record is appended. A write failure is returned, never fatal:
    /// losing the history costs history, not correctness.
    pub fn start(&self, version: &str) -> AppResult<PreviousRunEnd> {
        let _io = self.lock_io();
        let mut history = self.load_history();
        let classified = classify(&history);
        let current = CurrentRun {
            pid: std::process::id(),
            started_at: self.clock.now(),
        };
        history.push(RunRecord {
            version: version.to_owned(),
            pid: current.pid,
            started_at: current.started_at,
            ended_at: None,
            exit_reason: None,
            app_update_result: None,
        });
        // Claim the identity only once the record is on disk: a failed write
        // leaves this run owning nothing, so a later completion cannot
        // rewrite somebody else's history.
        self.save(history)?;
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(current);
        Ok(classified)
    }

    /// Preserve App Update outcomes in host evidence even after the Owner ack.
    pub fn record_app_update_result(
        &self,
        result: crate::app_update::UpdateResult,
    ) -> AppResult<()> {
        let Some(current) = self.current_run() else {
            return Ok(());
        };
        let _io = self.lock_io();
        let mut history = self.load_history();
        if let Some(record) = history
            .iter_mut()
            .rev()
            .find(|record| current.matches(record))
        {
            record.app_update_result = Some(result);
            self.save(history)?;
        }
        Ok(())
    }

    /// Completes this run's record with an end time and Exit Reason.
    /// Idempotent: a run already completed — window closed, then exit
    /// requested — is left alone, and only this run's exact record is
    /// touched. With no startup record written, there is nothing to
    /// complete and no search by PID. Returns whether a record was completed.
    pub fn complete(&self, reason: ExitReason) -> AppResult<bool> {
        // Read the identity before taking the I/O lock. The panic hook calls
        // this, and a panic raised while `start` still holds that lock would
        // otherwise re-enter a non-reentrant mutex and hang the process on
        // its way out.
        let Some(current) = self.current_run() else {
            return Ok(false);
        };
        let _io = self.lock_io();
        let mut history = self.load_history();
        let Some(record) = history
            .iter_mut()
            .rev()
            .find(|record| current.matches(record) && record.ended_at.is_none())
        else {
            return Ok(false);
        };
        record.ended_at = Some(self.clock.now());
        record.exit_reason = Some(reason);
        self.save(history)?;
        Ok(true)
    }

    /// Persists the history, trimmed to the newest `MAX_RUN_RECORDS`.
    fn save(&self, mut records: Vec<RunRecord>) -> AppResult<()> {
        if records.len() > MAX_RUN_RECORDS {
            let excess = records.len() - MAX_RUN_RECORDS;
            records.drain(..excess);
        }
        let json = serde_json::to_string_pretty(&records).map_err(|error| {
            crate::errors::AppError::InvalidInput(format!("could not encode Run Records: {error}"))
        })?;
        // Write beside the file and rename over it: a truncating write would
        // empty the history first and a failure after that point would leave
        // nothing behind. Losing history costs history, but only an actual
        // rename should ever cost it (ADR-0082).
        let staging = self.staging_path();
        fs::write(&staging, json)?;
        if let Err(error) = fs::rename(&staging, &self.path) {
            let _ = fs::remove_file(&staging);
            return Err(error.into());
        }
        Ok(())
    }

    /// The sibling the next save is staged in before it replaces the file.
    fn staging_path(&self) -> PathBuf {
        let mut name = match self.path.file_name() {
            Some(name) => name.to_os_string(),
            None => std::ffi::OsString::from(RUN_RECORDS_FILE_NAME),
        };
        name.push(".tmp");
        self.path.with_file_name(name)
    }

    fn lock_io(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tempfile::tempdir;

    const VERSION: &str = "9.9.9-test";

    struct TestClock {
        base: DateTime<Utc>,
        ticks: AtomicU32,
    }

    impl TestClock {
        fn at_seconds(base: &str) -> Self {
            Self {
                base: base.parse().expect("valid base instant"),
                ticks: AtomicU32::new(0),
            }
        }
    }

    impl RunRecordClock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            let tick = self.ticks.fetch_add(1, Ordering::SeqCst);
            self.base + chrono::Duration::seconds(i64::from(tick))
        }
    }

    fn store_in(dir: &tempfile::TempDir, clock: Arc<dyn RunRecordClock>) -> RunRecordStore {
        RunRecordStore::new(dir.path().join(RUN_RECORDS_FILE_NAME), clock)
    }

    fn write_history(path: &std::path::Path, records: &[RunRecord]) {
        fs::write(
            path,
            serde_json::to_string(records).expect("seed history serializes"),
        )
        .expect("seed history writes");
    }

    fn seeded_record(pid: u32, offset_secs: i64) -> RunRecord {
        use chrono::TimeZone;
        let base = Utc
            .with_ymd_and_hms(2026, 9, 1, 0, 0, 0)
            .single()
            .expect("valid instant");
        RunRecord {
            version: "0.0.0-seed".to_owned(),
            pid,
            started_at: base + chrono::Duration::seconds(offset_secs),
            ended_at: Some(base + chrono::Duration::seconds(offset_secs + 1)),
            exit_reason: Some(ExitReason::ApplicationExit),
            app_update_result: None,
        }
    }

    #[test]
    fn a_failed_save_leaves_the_stored_history_intact() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );
        write_history(&store.path, &[seeded_record(11, 0)]);

        // Block the staging file the save writes through, so the save fails
        // at the only point a failure is possible.
        fs::create_dir(store.staging_path()).expect("staging path is blocked");

        let failed = store.start(VERSION);

        assert!(failed.is_err(), "the blocked save must be reported");
        // History is not authority, but a failed write must never be what
        // destroys it (ADR-0082).
        let history = store.load_history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].pid, 11);
        // A failed run owns no record, so a later completion writes nothing.
        assert_eq!(store.current_run(), None);
    }

    #[test]
    fn a_save_leaves_no_staging_file_behind() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        store.start(VERSION).expect("start writes the record");

        assert!(!store.staging_path().exists());
        assert_eq!(store.load_history().len(), 1);
    }

    #[test]
    fn start_writes_a_record_with_version_pid_and_no_exit_reason() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        let classified = store.start(VERSION).expect("start writes the record");

        assert_eq!(classified, PreviousRunEnd::None);
        let history = store.load_history();
        assert_eq!(history.len(), 1);
        let record = &history[0];
        assert_eq!(record.version, VERSION);
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.started_at.to_rfc3339(), "2026-09-09T00:00:00+00:00");
        assert_eq!(record.ended_at, None);
        assert_eq!(record.exit_reason, None);
    }

    #[test]
    fn a_completed_previous_run_classifies_completed_with_its_reason() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        store.start(VERSION).expect("first run starts");
        store
            .complete(ExitReason::WindowClosed)
            .expect("first run completes");

        let classified = store.start(VERSION).expect("second run starts");
        assert_eq!(
            classified,
            PreviousRunEnd::Completed(Some(ExitReason::WindowClosed))
        );
    }

    #[test]
    fn an_unfinished_previous_run_is_a_suspected_hard_exit() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        store.start(VERSION).expect("first run starts");
        // No completion: the first "process" died without reaching its own
        // shutdown path. The next startup classifies it before appending.
        let classified = store.start(VERSION).expect("second run starts");

        assert_eq!(classified, PreviousRunEnd::SuspectedHardExit);
        let history = store.load_history();
        assert_eq!(history.len(), 2, "the new record is appended after");
        assert_eq!(classify(&history[..1]), PreviousRunEnd::SuspectedHardExit);
    }

    #[test]
    fn complete_sets_end_time_and_exit_reason() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        store.start(VERSION).expect("start writes the record");
        let completed = store
            .complete(ExitReason::ApplicationExit)
            .expect("complete writes the record");

        assert!(completed);
        let record = &store.load_history()[0];
        assert_eq!(record.exit_reason, Some(ExitReason::ApplicationExit));
        let ended_at = record.ended_at.expect("the run now has an end");
        assert!(
            ended_at > record.started_at,
            "end time comes from the clock"
        );
    }

    #[test]
    fn complete_is_idempotent_once_the_run_is_completed() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        store.start(VERSION).expect("start writes the record");
        store
            .complete(ExitReason::WindowClosed)
            .expect("window close completes the run");
        let again = store
            .complete(ExitReason::ApplicationExit)
            .expect("exit request finds nothing to do");

        assert!(!again, "the window-close completion must win");
        let record = &store.load_history()[0];
        assert_eq!(record.exit_reason, Some(ExitReason::WindowClosed));
    }

    #[test]
    fn complete_touches_only_this_runs_record() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(RUN_RECORDS_FILE_NAME);
        let foreign = RunRecord {
            ended_at: None,
            exit_reason: None,
            app_update_result: None,
            ..seeded_record(424_242, 0)
        };
        write_history(&path, std::slice::from_ref(&foreign));
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );
        store.start(VERSION).expect("start writes the record");

        let completed = store
            .complete(ExitReason::Panic)
            .expect("complete succeeds");

        assert!(completed);
        let history = store.load_history();
        assert_eq!(history[0], foreign, "a foreign run's record is untouched");
        assert_eq!(history[1].exit_reason, Some(ExitReason::Panic));
    }

    #[test]
    fn an_older_unfinished_record_sharing_this_pid_is_not_this_run() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(RUN_RECORDS_FILE_NAME);
        // Windows reuses PIDs, so a dead run's record can carry the number
        // this process now has. Only the start timestamp separates them.
        let reused_pid = RunRecord {
            ended_at: None,
            exit_reason: None,
            app_update_result: None,
            ..seeded_record(std::process::id(), 0)
        };
        write_history(&path, std::slice::from_ref(&reused_pid));
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );
        store.start(VERSION).expect("start writes the record");

        let completed = store
            .complete(ExitReason::WindowClosed)
            .expect("complete succeeds");

        assert!(completed);
        let history = store.load_history();
        assert_eq!(
            history[0], reused_pid,
            "the older run with the same PID stays open and untouched"
        );
        assert_eq!(history[1].exit_reason, Some(ExitReason::WindowClosed));
        assert!(history[1].ended_at.is_some());
    }

    #[test]
    fn a_failed_startup_write_leaves_completion_a_no_op() {
        let dir = tempdir().expect("temp dir");
        // A directory can be neither read as JSON nor written over, so the
        // startup record never lands and this run owns no record.
        let occupied = dir.path().join(RUN_RECORDS_FILE_NAME);
        fs::create_dir(&occupied).expect("directory takes the record path");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        assert!(store.start(VERSION).is_err(), "the startup write must fail");
        assert_eq!(store.current_run(), None);

        let completed = store
            .complete(ExitReason::ApplicationExit)
            .expect("completion is a no-op, never an error");

        assert!(!completed, "a run that owns no record completes nothing");
        assert!(
            occupied.is_dir(),
            "completion must not search for an older record to rewrite"
        );
    }

    #[test]
    fn the_rolling_trim_keeps_the_newest_ten() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(RUN_RECORDS_FILE_NAME);
        let seeded: Vec<RunRecord> = (0..12)
            .map(|offset| seeded_record(424_242, offset * 10))
            .collect();
        write_history(&path, &seeded);
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        store.start(VERSION).expect("start writes the record");

        let history = store.load_history();
        assert_eq!(history.len(), MAX_RUN_RECORDS);
        assert_eq!(
            history[0].started_at, seeded[3].started_at,
            "the four oldest seeded records are trimmed"
        );
        assert_eq!(
            history.last().map(|r| r.version.as_str()),
            Some(VERSION),
            "the newest record is this run's"
        );
    }

    #[test]
    fn a_missing_file_yields_an_empty_history_and_a_successful_start() {
        let dir = tempdir().expect("temp dir");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        assert!(store.load_history().is_empty());
        let classified = store
            .start(VERSION)
            .expect("a missing file never fails start");
        assert_eq!(classified, PreviousRunEnd::None);
        assert_eq!(store.load_history().len(), 1);
    }

    #[test]
    fn a_corrupt_file_yields_an_empty_history_rather_than_an_error() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(RUN_RECORDS_FILE_NAME);
        fs::write(&path, "{\"records\": [not json").expect("corrupt file writes");
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );

        assert!(store.load_history().is_empty());
        let classified = store
            .start(VERSION)
            .expect("a corrupt file never fails start");
        assert_eq!(classified, PreviousRunEnd::None, "nothing classifiable");
        assert_eq!(store.load_history().len(), 1, "the file is valid again");
    }

    #[test]
    fn classify_covers_empty_finished_and_unfinished_histories() {
        assert_eq!(classify(&[]), PreviousRunEnd::None);
        assert_eq!(
            classify(&[seeded_record(1, 0)]),
            PreviousRunEnd::Completed(Some(ExitReason::ApplicationExit))
        );
        assert_eq!(
            classify(&[
                seeded_record(1, 0),
                RunRecord {
                    ended_at: None,
                    exit_reason: None,
                    app_update_result: None,
                    ..seeded_record(2, 10)
                }
            ]),
            PreviousRunEnd::SuspectedHardExit
        );
    }

    #[test]
    fn exit_reasons_are_persisted_as_snake_case_words() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(RUN_RECORDS_FILE_NAME);
        let store = store_in(
            &dir,
            Arc::new(TestClock::at_seconds("2026-09-09T00:00:00Z")),
        );
        store.start(VERSION).expect("start writes the record");

        for (reason, json_word) in [
            (ExitReason::WindowClosed, "window_closed"),
            (ExitReason::ApplicationExit, "application_exit"),
            (ExitReason::Panic, "panic"),
        ] {
            fs::remove_file(&path).expect("clean slate per reason");
            store.start(VERSION).expect("start writes the record");
            store.complete(reason).expect("complete writes the record");
            let contents = fs::read_to_string(&path).expect("file reads");
            assert!(
                contents.contains(json_word),
                "{reason:?} must persist as {json_word}: {contents}"
            );
        }
    }
}

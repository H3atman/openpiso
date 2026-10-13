//! Durable diagnostics for the non-interactive Client Guard service (ADR-0016).

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{self, Rotation},
};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

const LOG_FILE_PREFIX: &str = "client-guard";
const LOG_FILE_SUFFIX: &str = "log";
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_LOG_FILES: usize = 8;

/// Initialize daily machine-wide logs and return the guard that flushes them.
pub fn init_tracing() -> Result<WorkerGuard, String> {
    let program_data = std::env::var_os("ProgramData")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ProgramData is unavailable".to_owned())?;
    let log_dir = PathBuf::from(program_data).join("OpenPiso").join("logs");
    fs::create_dir_all(&log_dir).map_err(|error| {
        format!(
            "could not create Guard log directory {}: {error}",
            log_dir.display()
        )
    })?;
    cleanup_old_logs(&log_dir, SystemTime::now());

    let appender = rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        // Bound retention during long service uptimes as well as at startup.
        // Eight files cover the current partial day plus about seven prior days.
        .max_log_files(MAX_LOG_FILES)
        .build(&log_dir)
        .map_err(|error| format!("could not initialize Guard log appender: {error}"))?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        .finish(appender);
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    registry()
        .with(
            fmt::Layer::new()
                .json()
                .flatten_event(true)
                .with_writer(writer)
                .with_filter(env_filter),
        )
        // Diagnostic Forwarding (ADR-0078): capture WARN, ERROR, and
        // allowlisted INFO lines into the shared buffer when the release gate
        // allows. `None` keeps the subscriber shape unchanged in a gated-off
        // development build.
        .with(crate::forwarding::forwarding_layer())
        .try_init()
        .map_err(|error| format!("could not install Guard tracing subscriber: {error}"))?;

    Ok(guard)
}

fn cleanup_old_logs(log_dir: &Path, now: SystemTime) {
    let Some(cutoff) = now.checked_sub(RETENTION) else {
        return;
    };
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };

    for path in entries.flatten().map(|entry| entry.path()) {
        if !is_guard_log_file(&path) {
            continue;
        }
        let expired = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| is_expired(modified, cutoff));
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}

fn is_guard_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(&format!("{LOG_FILE_PREFIX}.")))
        .and_then(|name| name.strip_suffix(&format!(".{LOG_FILE_SUFFIX}")))
        .is_some_and(|date| !date.is_empty())
}

fn is_expired(modified: SystemTime, cutoff: SystemTime) -> bool {
    modified < cutoff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_guard_daily_logs_match_retention_filter() {
        assert!(is_guard_log_file(Path::new("client-guard.2026-07-26.log")));
        assert!(!is_guard_log_file(Path::new("client-guard.log")));
        assert!(!is_guard_log_file(Path::new("diagnostics.2026-07-26.log")));
        assert!(!is_guard_log_file(Path::new("client-guard.2026-07-26.txt")));
    }

    #[test]
    fn retention_predicate_removes_only_older_files() {
        let cutoff = SystemTime::now();
        assert!(is_expired(cutoff - Duration::from_secs(1), cutoff));
        assert!(!is_expired(cutoff, cutoff));
        assert!(!is_expired(cutoff + Duration::from_secs(1), cutoff));
    }

    #[test]
    fn cleanup_removes_only_expired_guard_logs() {
        let dir = std::env::temp_dir().join(format!("client-guard-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temporary log directory");
        let guard_log = dir.join("client-guard.2026-07-26.log");
        let unrelated = dir.join("diagnostics.2026-07-26.log");
        fs::write(&guard_log, "guard").expect("guard log");
        fs::write(&unrelated, "other").expect("unrelated log");

        cleanup_old_logs(&dir, SystemTime::now() + RETENTION + Duration::from_secs(1));

        assert!(!guard_log.exists());
        assert!(unrelated.exists());
        let _ = fs::remove_dir_all(dir);
    }
}

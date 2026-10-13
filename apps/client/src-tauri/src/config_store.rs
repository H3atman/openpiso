use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tracing::warn;

use crate::state::{ClientConfig, PendingTamperKillReport};

const CONFIG_FILE_NAME: &str = "client-config.json";
const OFFLINE_SHUTDOWN_DEADLINE_FILE_NAME: &str = "offline-shutdown-deadline.json";
const TAMPER_KILL_REPORT_FILE_NAME: &str = "tamper-kill-report.json";
const TEMP_EXTENSION: &str = "json.tmp";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct OfflineShutdownDeadlineFile {
    pub deadline_unix_ms: i64,
}

/// The directory every `client-config.json` sibling file lives in. The
/// harness seam (`OPENPISO_CLIENT_CONFIG_DIR`, see the startup coordinator)
/// wins when set so tests and the process harness isolate persistence;
/// otherwise Tauri's per-identifier config directory is authoritative and
/// the pre-runtime sender agrees with it through the same platform default.
fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(dir) = crate::startup_coordinator::config_dir_override() {
        return Ok(dir);
    }
    app.path()
        .app_config_dir()
        .map_err(|error| format!("could not resolve client config directory: {error}"))
}

pub fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    config_dir(app).map(|dir| dir.join(CONFIG_FILE_NAME))
}

pub fn offline_shutdown_deadline_path(app: &AppHandle) -> Result<PathBuf, String> {
    config_dir(app).map(|dir| dir.join(OFFLINE_SHUTDOWN_DEADLINE_FILE_NAME))
}

pub fn tamper_kill_report_path(app: &AppHandle) -> Result<PathBuf, String> {
    config_dir(app).map(|dir| dir.join(TAMPER_KILL_REPORT_FILE_NAME))
}

/// Filesystem path of the authoritative pending-report file inside an
/// explicitly given config directory. The startup coordinator uses this so
/// the pre-runtime sender and the running receiver agree on one report
/// location (issue #367); without the harness override both resolve the
/// same platform directory as the AppHandle-based path above.
pub(crate) fn tamper_report_path_for(config_dir: &Path) -> PathBuf {
    config_dir.join(TAMPER_KILL_REPORT_FILE_NAME)
}

pub fn load(app: &AppHandle) -> Option<ClientConfig> {
    let path = match config_path(app) {
        Ok(path) => path,
        Err(error) => {
            warn!(target: "openpiso_client::config_store", "{error}");
            return None;
        }
    };
    match decode_config_file(&path) {
        Ok(config) => Some(config),
        Err(error) => {
            warn!(
                target: "openpiso_client::config_store",
                "Could not load client config from {}: {error}",
                path.display()
            );
            None
        }
    }
}

pub fn save(app: &AppHandle, config: &ClientConfig) -> Result<(), String> {
    let path = config_path(app)?;
    encode_config_file(&path, config).map_err(|error| {
        format!(
            "could not save client config to {}: {error}",
            path.display()
        )
    })
}

pub fn load_offline_shutdown_deadline(app: &AppHandle) -> Option<OfflineShutdownDeadlineFile> {
    let path = match offline_shutdown_deadline_path(app) {
        Ok(path) => path,
        Err(error) => {
            warn!(target: "openpiso_client::config_store", "{error}");
            return None;
        }
    };
    match decode_offline_shutdown_deadline_file(&path) {
        Ok(deadline) => Some(deadline),
        Err(error) => {
            warn!(
                target: "openpiso_client::config_store",
                "Could not load offline shutdown deadline from {}: {error}",
                path.display()
            );
            None
        }
    }
}

pub fn save_offline_shutdown_deadline(
    app: &AppHandle,
    deadline_unix_ms: i64,
) -> Result<(), String> {
    let path = offline_shutdown_deadline_path(app)?;
    encode_offline_shutdown_deadline_file(&path, &OfflineShutdownDeadlineFile { deadline_unix_ms })
        .map_err(|error| {
            format!(
                "could not save offline shutdown deadline to {}: {error}",
                path.display()
            )
        })
}

pub fn clear_offline_shutdown_deadline(app: &AppHandle) -> Result<(), String> {
    let path = offline_shutdown_deadline_path(app)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not remove offline shutdown deadline at {}: {error}",
            path.display()
        )),
    }
}

/// Read the Guard-supplied report from the relaunched Client's command line.
/// Malformed or absent arguments are ignored so ordinary launches remain
/// unaffected. Validation lives in the startup coordinator so the future
/// duplicate gate and the local handoff sender share one boundary.
pub fn load_guard_tamper_kill_report() -> Option<PendingTamperKillReport> {
    crate::startup_coordinator::parse_report_args(&std::env::args().collect::<Vec<_>>())
}

/// Persist a pending report outside the startup-recovery path. Its only
/// caller is the debug-only `dev_seed_tamper_kill_report` demo command,
/// so release builds neither compile nor warn about it.
#[cfg(debug_assertions)]
pub fn save_tamper_kill_report(
    app: &AppHandle,
    report: &PendingTamperKillReport,
) -> Result<(), String> {
    let path = tamper_kill_report_path(app)?;
    encode_tamper_kill_report_file(&path, report).map_err(|error| {
        format!(
            "could not save tamper kill report to {}: {error}",
            path.display()
        )
    })
}

pub fn decode_config(contents: &str) -> Result<ClientConfig, serde_json::Error> {
    serde_json::from_str(contents)
}

pub fn encode_config(config: &ClientConfig) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(config)
}

pub fn decode_offline_shutdown_deadline(
    contents: &str,
) -> Result<OfflineShutdownDeadlineFile, serde_json::Error> {
    serde_json::from_str(contents)
}

pub fn encode_offline_shutdown_deadline(
    deadline: &OfflineShutdownDeadlineFile,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(deadline)
}

/// The authoritative pending-report file. It carries the coalesced report
/// (absent once Admin acknowledged it) plus the bounded delivery ledger
/// that gives each local handoff delivery a durable identity, so a repeated
/// delivery of one report is recognized as a retry even after distinct
/// reports coalesced or the Client restarted (issues #367/#371, ADR-0079).
///
/// Historical files carry only the two report fields and decode unchanged
/// with an empty ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct TamperKillReportState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kill_timestamp_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_count: Option<i64>,
    /// Delivery identities already folded in, oldest first. Bounded by the
    /// startup coordinator before every write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered: Vec<PendingTamperKillReport>,
}

impl TamperKillReportState {
    /// The pending report, if this file still holds one.
    pub fn report(&self) -> Option<PendingTamperKillReport> {
        match (self.kill_timestamp_unix_ms, self.attempt_count) {
            (Some(kill_timestamp_unix_ms), Some(attempt_count)) => Some(PendingTamperKillReport {
                kill_timestamp_unix_ms,
                attempt_count,
            }),
            _ => None,
        }
    }

    /// Replace the pending report, keeping the delivery ledger.
    pub fn with_report(mut self, report: Option<PendingTamperKillReport>) -> Self {
        self.kill_timestamp_unix_ms = report.map(|report| report.kill_timestamp_unix_ms);
        self.attempt_count = report.map(|report| report.attempt_count);
        self
    }

    /// Nothing worth keeping on disk: no pending report and no delivery
    /// identities to recognize a retry by.
    pub fn is_empty(&self) -> bool {
        self.report().is_none() && self.delivered.is_empty()
    }
}

/// Decode the authoritative report file. A present report must satisfy the
/// same positive-integer contract as the command-line boundary; ledger
/// entries that do not are dropped, since they are only retry hints.
pub fn decode_tamper_kill_state(
    contents: &str,
) -> Result<TamperKillReportState, serde_json::Error> {
    let mut state: TamperKillReportState = serde_json::from_str(contents)?;
    match (state.kill_timestamp_unix_ms, state.attempt_count) {
        (Some(timestamp), Some(attempts)) => {
            if timestamp <= 0 || attempts <= 0 {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "tamper-kill report must carry positive integers",
                ));
            }
        }
        (None, None) => {}
        _ => {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "tamper-kill report must carry both a timestamp and an attempt count",
            ));
        }
    }
    state
        .delivered
        .retain(|entry| entry.kill_timestamp_unix_ms > 0 && entry.attempt_count > 0);
    Ok(state)
}

pub fn encode_tamper_kill_state(
    state: &TamperKillReportState,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(state)
}

pub fn decode_tamper_kill_report(
    contents: &str,
) -> Result<PendingTamperKillReport, serde_json::Error> {
    let report: PendingTamperKillReport = serde_json::from_str(contents)?;
    // The same positive-integer contract the command-line boundary enforces
    // (ADR-0079): a durable file is a hint, never authority, so a zero or
    // negative kill timestamp can never become a pending report.
    if report.kill_timestamp_unix_ms <= 0 {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "tamper-kill timestamp must be positive",
        ));
    }
    if report.attempt_count <= 0 {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "tamper-kill attempt count must be positive",
        ));
    }
    Ok(report)
}

pub fn decode_config_file(path: &Path) -> Result<ClientConfig, String> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("could not read file: {error}"))?;
    decode_config(&contents).map_err(|error| format!("invalid JSON: {error}"))
}

pub fn decode_offline_shutdown_deadline_file(
    path: &Path,
) -> Result<OfflineShutdownDeadlineFile, String> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("could not read file: {error}"))?;
    decode_offline_shutdown_deadline(&contents).map_err(|error| format!("invalid JSON: {error}"))
}

pub(crate) fn decode_tamper_kill_state_file(path: &Path) -> Result<TamperKillReportState, String> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("could not read file: {error}"))?;
    decode_tamper_kill_state(&contents).map_err(|error| format!("invalid report: {error}"))
}

/// The persisted state, treating a missing file as the empty state. A
/// corrupt file stays an error the caller must not silently overwrite.
pub(crate) fn read_tamper_kill_state_file(path: &Path) -> Result<TamperKillReportState, String> {
    if !path.exists() {
        return Ok(TamperKillReportState::default());
    }
    decode_tamper_kill_state_file(path)
}

pub(crate) fn decode_tamper_kill_report_file(
    path: &Path,
) -> Result<PendingTamperKillReport, String> {
    decode_tamper_kill_state_file(path)?
        .report()
        .ok_or_else(|| "invalid report: no pending tamper kill report".to_owned())
}

/// Write the whole authoritative state atomically. An empty state removes
/// the file, so a Client that never saw a report keeps no report file.
pub(crate) fn write_tamper_kill_state_file(
    path: &Path,
    state: &TamperKillReportState,
) -> io::Result<()> {
    if state.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }
    let contents = encode_tamper_kill_state(state).map_err(io::Error::other)?;
    write_atomic_file(path, &contents)
}

pub(crate) fn encode_tamper_kill_report_file(
    path: &Path,
    report: &PendingTamperKillReport,
) -> io::Result<()> {
    let state = read_tamper_kill_state_file(path)
        .unwrap_or_default()
        .with_report(Some(*report));
    write_tamper_kill_state_file(path, &state)
}

/// Clear the pending report after the server acknowledged exactly it. The
/// delivery ledger survives acknowledgement on purpose: a delayed retry of
/// an already-acknowledged delivery must not resurrect a pending report.
pub(crate) fn acknowledge_tamper_kill_report_file(
    path: &Path,
    acknowledged: PendingTamperKillReport,
) -> Result<bool, String> {
    let state = read_tamper_kill_state_file(path)?;
    match state.report() {
        None => return Ok(true),
        Some(persisted) if persisted != acknowledged => return Ok(false),
        Some(_) => {}
    }
    let cleared = state.with_report(None);
    write_tamper_kill_state_file(path, &cleared)
        .map(|()| true)
        .map_err(|error| {
            format!(
                "could not clear tamper kill report at {}: {error}",
                path.display()
            )
        })
}

pub fn encode_config_file(path: &Path, config: &ClientConfig) -> io::Result<()> {
    let contents = encode_config(config).map_err(io::Error::other)?;
    write_atomic_file(path, &contents)
}

pub fn encode_offline_shutdown_deadline_file(
    path: &Path,
    deadline: &OfflineShutdownDeadlineFile,
) -> io::Result<()> {
    let contents = encode_offline_shutdown_deadline(deadline).map_err(io::Error::other)?;
    write_atomic_file(path, &contents)
}

fn write_atomic_file(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(TEMP_EXTENSION);
    fs::write(&tmp_path, contents)?;
    atomic_rename(&tmp_path, path)
}

/// Atomic write shared with the startup coordinator's handoff inbox
/// (issue #367): the sender has no Tauri handle, but its durability
/// contract needs the same tmp-file-plus-rename semantics.
pub(crate) fn write_atomic_contents(path: &Path, contents: &str) -> io::Result<()> {
    write_atomic_file(path, contents)
}

#[cfg(not(windows))]
fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x00000001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x00000008;

    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let from = wide(from);
    let to = wide(to);
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config(view_mode: &str) -> ClientConfig {
        ClientConfig {
            server_url: "http://192.168.1.10:3210".to_owned(),
            pc_id: Some("pc-07".to_owned()),
            view_mode: view_mode.to_owned(),
            launch_at_startup: true,
            disable_windows_sign_out: true,
            offline_grace_seconds: 240,
        }
    }

    #[test]
    fn client_config_json_round_trips() {
        let config = sample_config("mini");
        let encoded = encode_config(&config).expect("encode config");
        let decoded = decode_config(&encoded).expect("decode config");
        assert_eq!(decoded.server_url, config.server_url);
        assert_eq!(decoded.pc_id, config.pc_id);
        assert_eq!(decoded.view_mode, config.view_mode);
        assert_eq!(decoded.launch_at_startup, config.launch_at_startup);
        assert_eq!(
            decoded.disable_windows_sign_out,
            config.disable_windows_sign_out
        );
        assert_eq!(decoded.offline_grace_seconds, config.offline_grace_seconds);
    }

    #[test]
    fn client_config_with_optional_pc_id_round_trips() {
        // A fresh install ships without a `pc_id`; the server adopts
        // one on the first heartbeat. The deserializer must keep the
        // `None` state — the old `pc-01` default is gone (ADR-0031).
        let mut config = sample_config("widget");
        config.pc_id = None;
        let encoded = encode_config(&config).expect("encode config");
        let decoded = decode_config(&encoded).expect("decode config");
        assert_eq!(decoded.pc_id, None);
    }

    #[test]
    fn empty_pc_id_is_treated_as_unset() {
        // Defensive: a stale `pc-01` written before ADR-0031 must not
        // be reused as a misleading cached identity.
        let decoded = decode_config(
            r#"{
  "server_url": "http://192.168.1.10:3210",
  "pc_id": "  "
}"#,
        )
        .expect("decode config");
        assert_eq!(decoded.pc_id, None);
    }

    #[test]
    fn disabled_windows_sign_out_round_trips() {
        let mut config = sample_config("widget");
        config.disable_windows_sign_out = false;
        let encoded = encode_config(&config).expect("encode config");
        let decoded = decode_config(&encoded).expect("decode config");
        assert!(!decoded.disable_windows_sign_out);
    }

    #[test]
    fn missing_windows_sign_out_defaults_to_disabled() {
        let decoded = decode_config(
            r#"{
  "server_url": "http://192.168.1.10:3210",
  "pc_id": "pc-07",
  "view_mode": "widget"
}"#,
        )
        .expect("decode config");
        assert!(!decoded.disable_windows_sign_out);
    }

    #[test]
    fn disabled_launch_at_startup_round_trips() {
        let mut config = sample_config("widget");
        config.launch_at_startup = false;
        let encoded = encode_config(&config).expect("encode config");
        let decoded = decode_config(&encoded).expect("decode config");
        assert!(!decoded.launch_at_startup);
    }

    #[test]
    fn missing_launch_at_startup_defaults_to_enabled() {
        let decoded = decode_config(
            r#"{
  "server_url": "http://192.168.1.10:3210",
  "pc_id": "pc-07",
  "view_mode": "widget"
}"#,
        )
        .expect("decode config");
        assert!(decoded.launch_at_startup);
    }

    #[test]
    fn missing_offline_grace_defaults_to_600_seconds() {
        let decoded = decode_config(
            r#"{
  "server_url": "http://192.168.1.10:3210",
  "pc_id": "pc-07",
  "view_mode": "widget"
}"#,
        )
        .expect("decode config");
        assert_eq!(decoded.offline_grace_seconds, 600);
    }

    #[test]
    fn offline_shutdown_deadline_round_trips() {
        let deadline = OfflineShutdownDeadlineFile {
            deadline_unix_ms: 1_725_000_123_456,
        };
        let encoded = encode_offline_shutdown_deadline(&deadline).expect("encode deadline");
        let decoded = decode_offline_shutdown_deadline(&encoded).expect("decode deadline");
        assert_eq!(decoded, deadline);
    }

    #[test]
    fn corrupt_offline_shutdown_deadline_is_rejected() {
        assert!(decode_offline_shutdown_deadline("{not-json").is_err());
        assert!(decode_offline_shutdown_deadline(r#"{"deadline_unix_ms":"later"}"#).is_err());
    }

    #[test]
    fn missing_offline_shutdown_deadline_file_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "openpiso-client-missing-deadline-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        assert!(!path.exists());
        let error = decode_offline_shutdown_deadline_file(&path).expect_err("missing file");
        assert!(error.contains("could not read file"));
    }

    #[test]
    fn corrupt_offline_shutdown_deadline_file_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "openpiso-client-corrupt-deadline-{}",
            std::process::id()
        ));
        let path = root.join(OFFLINE_SHUTDOWN_DEADLINE_FILE_NAME);
        fs::create_dir_all(&root).expect("create deadline test directory");
        fs::write(&path, "{not-json").expect("write corrupt deadline");

        let error = decode_offline_shutdown_deadline_file(&path).expect_err("corrupt file");
        assert!(error.contains("invalid JSON"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn offline_shutdown_deadline_file_replaces_existing_file() {
        let root = std::env::temp_dir().join(format!(
            "openpiso-client-offline-deadline-test-{}",
            std::process::id()
        ));
        let path = root.join(OFFLINE_SHUTDOWN_DEADLINE_FILE_NAME);
        let first = OfflineShutdownDeadlineFile {
            deadline_unix_ms: 100,
        };
        let second = OfflineShutdownDeadlineFile {
            deadline_unix_ms: 200,
        };

        encode_offline_shutdown_deadline_file(&path, &first).expect("save first deadline");
        encode_offline_shutdown_deadline_file(&path, &second).expect("replace deadline");

        let decoded = decode_offline_shutdown_deadline_file(&path).expect("read deadline");
        assert_eq!(decoded, second);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_config_string_is_rejected() {
        assert!(decode_config("{not-json").is_err());
    }

    #[test]
    fn missing_view_mode_defaults_to_widget() {
        let decoded = decode_config(
            r#"{
  "server_url": "http://192.168.1.10:3210",
  "pc_id": "pc-07"
}"#,
        )
        .expect("decode config");
        assert_eq!(decoded.view_mode, "widget");
    }

    #[test]
    fn unknown_view_mode_defaults_to_widget() {
        let decoded = decode_config(
            r#"{
  "server_url": "http://192.168.1.10:3210",
  "pc_id": "pc-07",
  "view_mode": "lock"
}"#,
        )
        .expect("decode config");
        assert_eq!(decoded.view_mode, "widget");
    }

    #[test]
    fn config_file_save_replaces_existing_file() {
        let root = std::env::temp_dir().join(format!(
            "openpiso-client-config-store-test-{}",
            std::process::id()
        ));
        let path = root.join(CONFIG_FILE_NAME);
        let first = sample_config("widget");
        let second = sample_config("mini");

        encode_config_file(&path, &first).expect("save first config");
        encode_config_file(&path, &second).expect("replace config");

        let decoded = decode_config_file(&path).expect("read saved config");
        assert_eq!(decoded.view_mode, "mini");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn guard_tamper_kill_arguments_are_typed_and_validated() {
        use crate::startup_coordinator::parse_report_args;

        let args = vec![
            "openpiso-client.exe".to_owned(),
            "--openpiso-tamper-kill".to_owned(),
            "1750000000000".to_owned(),
            "3".to_owned(),
        ];
        assert_eq!(
            parse_report_args(&args),
            Some(PendingTamperKillReport {
                kill_timestamp_unix_ms: 1_750_000_000_000,
                attempt_count: 3,
            })
        );
        assert!(parse_report_args(&args[..3]).is_none());
        assert!(parse_report_args(&[
            "--openpiso-tamper-kill".to_owned(),
            "0".to_owned(),
            "1".to_owned(),
        ])
        .is_none());
    }

    #[test]
    fn tamper_kill_report_round_trips() {
        let report = PendingTamperKillReport {
            kill_timestamp_unix_ms: 1_750_000_000_000,
            attempt_count: 3,
        };
        let encoded = serde_json::to_string_pretty(&report).expect("encode report");
        let decoded = decode_tamper_kill_report(&encoded).expect("decode report");
        assert_eq!(decoded, report);
        // The authoritative file round-trips through the state layer, whose
        // delivery ledger starts empty.
        let state = TamperKillReportState::default().with_report(Some(report));
        let encoded_state = encode_tamper_kill_state(&state).expect("encode state");
        assert_eq!(
            decode_tamper_kill_state(&encoded_state).expect("decode state"),
            state
        );
        assert_eq!(state.report(), Some(report));
    }

    #[test]
    fn tamper_kill_report_attempt_count_round_trips() {
        let decoded = decode_tamper_kill_report(
            r#"{
  "kill_timestamp_unix_ms": 1750000000000,
  "attempt_count": 1
}"#,
        )
        .expect("decode report");
        assert_eq!(decoded.attempt_count, 1);
    }

    #[test]
    fn corrupt_tamper_kill_report_is_rejected() {
        assert!(decode_tamper_kill_report("{not-json").is_err());
        assert!(decode_tamper_kill_report(r#"{"kill_timestamp_unix_ms":"invalid"}"#).is_err());
        assert!(decode_tamper_kill_report(
            r#"{"kill_timestamp_unix_ms":1750000000000,"attempt_count":0}"#
        )
        .is_err());
        assert!(decode_tamper_kill_report(
            r#"{"kill_timestamp_unix_ms":1750000000000,"attempt_count":-1}"#
        )
        .is_err());
        // A zero or negative kill timestamp is invalid at the durable
        // boundary too, matching the command-line boundary.
        assert!(
            decode_tamper_kill_report(r#"{"kill_timestamp_unix_ms":0,"attempt_count":1}"#).is_err()
        );
        assert!(decode_tamper_kill_report(
            r#"{"kill_timestamp_unix_ms":-1750000000000,"attempt_count":1}"#
        )
        .is_err());
        assert!(
            decode_tamper_kill_report(r#"{"kill_timestamp_unix_ms":0,"attempt_count":0}"#).is_err()
        );
    }

    #[test]
    fn missing_tamper_kill_report_file_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "openpiso-client-missing-tamper-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        assert!(!path.exists());
        let error = decode_tamper_kill_report_file(&path).expect_err("missing file");
        assert!(error.contains("could not read file"));
    }

    #[test]
    fn corrupt_tamper_kill_report_file_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "openpiso-client-corrupt-tamper-{}",
            std::process::id()
        ));
        let path = root.join(TAMPER_KILL_REPORT_FILE_NAME);
        fs::create_dir_all(&root).expect("create tamper report test directory");
        fs::write(&path, "{not-json").expect("write corrupt report");

        let error = decode_tamper_kill_report_file(&path).expect_err("corrupt file");
        assert!(error.contains("invalid report"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn second_tamper_kill_coalesces_without_losing_first_report() {
        let first = PendingTamperKillReport {
            kill_timestamp_unix_ms: 100,
            attempt_count: 1,
        };
        let second = PendingTamperKillReport {
            kill_timestamp_unix_ms: 200,
            attempt_count: 1,
        };

        assert_eq!(
            first.coalesce(second),
            PendingTamperKillReport {
                kill_timestamp_unix_ms: 100,
                attempt_count: 2,
            }
        );
    }

    #[test]
    fn stale_acknowledgement_does_not_clear_newer_report() {
        let root = std::env::temp_dir().join(format!(
            "openpiso-client-tamper-ack-test-{}",
            std::process::id()
        ));
        let path = root.join(TAMPER_KILL_REPORT_FILE_NAME);
        let sent = PendingTamperKillReport {
            kill_timestamp_unix_ms: 100,
            attempt_count: 1,
        };
        let current = sent.coalesce(PendingTamperKillReport {
            kill_timestamp_unix_ms: 200,
            attempt_count: 1,
        });
        encode_tamper_kill_report_file(&path, &current).expect("save current report");

        assert!(!acknowledge_tamper_kill_report_file(&path, sent).expect("acknowledge stale"));
        assert_eq!(
            decode_tamper_kill_report_file(&path).expect("read current report"),
            current
        );
        assert!(acknowledge_tamper_kill_report_file(&path, current).expect("acknowledge current"));
        assert!(!path.exists());

        let _ = fs::remove_dir_all(root);
    }
}

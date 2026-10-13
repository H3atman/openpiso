//! Windows process-level harness for the local Tamper Kill handoff
//! (issue #367, ADR-0079).
//!
//! This is the shared seam the startup tickets build on: it launches
//! **real child processes of the production Client binary** and observes
//! them only through the production handoff boundary — persistence on
//! disk and requests received by an isolated test Admin — never through
//! private flags or reimplemented transport code.
//!
//! Per test:
//!
//! - One **isolated test Admin**: an axum server on `127.0.0.1:0` that
//!   records every received Heartbeat request and answers with the same
//!   `PcResponse` shape the real Admin sends, including the authoritative
//!   `tamper_kill_acknowledged: "recorded"` when the test enables it. An
//!   "Admin outage" is emulated by rejecting heartbeats with `503` — the
//!   same report-retention branch as an unreachable server, but
//!   observable. Nothing here contacts an installed shop Admin or Guard
//!   service.
//! - One **isolated persistence directory**. The child processes inherit
//!   `OPENPISO_CLIENT_CONFIG_DIR`, so the sender inbox, the authoritative
//!   `tamper-kill-report.json`, the Client's own config, and its logs all
//!   resolve inside the directory (production never sets the variable).
//! - A **sender child** (`openpiso-client --openpiso-handoff-only
//!   --openpiso-tamper-kill <ts> <count>`), observed through its exit
//!   code and the inbox file it leaves behind.
//! - A **receiver child** (an ordinary `openpiso-client` launch),
//!   observed through what it persists and what it sends to the test
//!   Admin — the existing authoritative heartbeat/acknowledgement
//!   behaviour, not internal state.
//!
//! #369 (the duplicate startup gate) extends this harness below: real
//! competing launches of the production binary through the production
//! entrypoint ordering — sender check, then the arbitration gate — with
//! barrier-synchronized contenders, owner-death injection, denied and
//! incompatible coordination objects, and a 100-round synchronized stress
//! loop. Admission is observed only through the gate's own evidence lines,
//! exit codes, persistence, and requests at the test Admin.

#![cfg(windows)]

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::Stream;
use serde_json::{json, Value};
use tempfile::TempDir;

/// How long to wait for a receiver heartbeat observation (cold WebView2
/// boot included) or any other filesystem state.
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a sender child may take to persist and exit.
const SENDER_TIMEOUT: Duration = Duration::from_secs(30);

/// Serialize the scenarios even when `cargo test` runs this binary with
/// parallel test threads: each one spawns real child processes with their
/// own WebView2 runtime, and concurrent boots made the first harness runs
/// flaky. A poisoned lock (a panicked scenario) must not wedge the rest.
fn harness_lock() -> std::sync::MutexGuard<'static, ()> {
    static HARNESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    HARNESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Isolated test Admin
// ---------------------------------------------------------------------------

struct AdminState {
    /// Every heartbeat request body the receiver sent, in arrival order.
    heartbeats: Mutex<Vec<Value>>,
    /// The Admin starts healthy; only the outage test turns it "down".
    healthy: AtomicBool,
    /// When true, heartbeat responses carry the authoritative
    /// `tamper_kill_acknowledged: "recorded"`.
    ack_recorded: AtomicBool,
}

impl Default for AdminState {
    fn default() -> Self {
        Self {
            heartbeats: Mutex::new(Vec::new()),
            healthy: AtomicBool::new(true),
            ack_recorded: AtomicBool::new(false),
        }
    }
}

struct AdminServer {
    runtime: Option<tokio::runtime::Runtime>,
    url: String,
    state: Arc<AdminState>,
}

impl AdminServer {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test admin runtime");
        let (addr, state) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind isolated test admin");
            let addr = listener.local_addr().expect("test admin local addr");
            let state = Arc::new(AdminState::default());
            let app = Router::new()
                .route("/api/identity/heartbeat", post(heartbeat))
                .route("/api/identity/events", get(event_stream))
                .route("/api/theme", get(theme))
                .route("/api/shop", get(shop))
                .route("/api/forwarded-diagnostics", post(diagnostics))
                .with_state(Arc::clone(&state));
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            (addr, state)
        });
        Self {
            runtime: Some(runtime),
            url: format!("http://{addr}"),
            state,
        }
    }

    fn recorded_heartbeats(&self) -> Vec<Value> {
        self.state.heartbeats.lock().expect("heartbeats").clone()
    }

    fn set_healthy(&self, healthy: bool) {
        self.state.healthy.store(healthy, Ordering::Release);
    }

    fn set_ack_recorded(&self, recorded: bool) {
        self.state.ack_recorded.store(recorded, Ordering::Release);
    }
}

impl Drop for AdminServer {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

async fn heartbeat(State(state): State<Arc<AdminState>>, Json(body): Json<Value>) -> Response {
    state.heartbeats.lock().expect("heartbeats").push(body);
    if !state.healthy.load(Ordering::Acquire) {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut pc = json!({
        "id": "pc-harness-1",
        "name": "Harness PC",
        "status": "online",
        "remaining_seconds": 3600,
        "guest_remaining_seconds": 3600,
        "timer_mode": "guest",
        "active_session": null,
        "theme_version": 1,
    });
    if state.ack_recorded.load(Ordering::Acquire) {
        pc["tamper_kill_acknowledged"] = json!("recorded");
    }
    Json(pc).into_response()
}

/// Hold the SSE channel open with keep-alive comments only. The harness
/// never sends authoritative `pc.updated` snapshots; heartbeats own the
/// state in these tests.
async fn event_stream() -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let keep_alive = futures_util::stream::unfold((), |()| async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Some((Ok(Event::default().comment("keep-alive")), ()))
    });
    Sse::new(keep_alive).keep_alive(KeepAlive::default())
}

async fn theme() -> Json<Value> {
    Json(json!({
        "accent_color": "#0a0a0a",
        "mode_web_admin": "dark",
        "mode_kiosk": "dark",
        "mode_client": "dark",
        "version": 1,
    }))
}

async fn shop() -> Json<Value> {
    Json(json!({ "shop_name": "Harness Shop" }))
}

async fn diagnostics() -> Json<Value> {
    Json(json!({}))
}

// ---------------------------------------------------------------------------
// Isolated persistence and child processes
// ---------------------------------------------------------------------------

struct IsolatedMachine {
    temp: TempDir,
    config_dir: PathBuf,
}

impl IsolatedMachine {
    /// One isolated Client machine: config dir, report file, handoff
    /// inbox, and logs all resolve under one temporary directory, and the
    /// seeded config points the Client at the test Admin.
    fn seed_with_admin(admin: &AdminServer) -> Self {
        let temp = TempDir::new().expect("create isolated machine dir");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("create isolated config dir");
        std::fs::write(
            config_dir.join("client-config.json"),
            json!({
                "server_url": admin.url,
                "pc_id": null,
                "view_mode": "widget",
                "launch_at_startup": false,
                "disable_windows_sign_out": false,
                "offline_grace_seconds": 120,
            })
            .to_string(),
        )
        .expect("seed client config");
        Self { temp, config_dir }
    }

    fn main_report_path(&self) -> PathBuf {
        self.config_dir.join("tamper-kill-report.json")
    }

    /// A bare isolated machine without a seeded config: enough for probe
    /// contenders, which exit before any configuration is read.
    fn bare() -> Self {
        let temp = TempDir::new().expect("create isolated machine dir");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("create isolated config dir");
        Self { temp, config_dir }
    }

    fn inbox_files(&self) -> Vec<PathBuf> {
        let inbox = self.config_dir.join("tamper-handoff-inbox");
        std::fs::read_dir(inbox)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The admission markers admitted probe owners left behind, keyed by
    /// process id. Only a probe launch writes these.
    fn admission_markers(&self) -> Vec<(u32, i64)> {
        let dir = self.config_dir.join("arbitration-admissions");
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                    .filter_map(|contents| serde_json::from_str::<Value>(&contents).ok())
                    .filter_map(|value| {
                        Some((
                            value["process_id"].as_u64()? as u32,
                            value["admitted_at_unix_ms"].as_i64()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn clear_admission_markers(&self) {
        let _ = std::fs::remove_dir_all(self.config_dir.join("arbitration-admissions"));
    }

    /// The pending report the authoritative file still holds. After a server
    /// acknowledgement the file may survive carrying only the delivery
    /// ledger, which is not a pending report.
    fn persisted_report(&self) -> Option<ReportIdentity> {
        let contents = std::fs::read_to_string(self.main_report_path()).ok()?;
        let value: Value = serde_json::from_str(&contents).expect("persisted report JSON");
        Some(ReportIdentity {
            kill_timestamp_unix_ms: value["kill_timestamp_unix_ms"].as_i64()?,
            attempt_count: value["attempt_count"].as_i64()?,
        })
    }

    /// The delivery identities the authoritative file remembers.
    fn delivery_ledger(&self) -> Vec<ReportIdentity> {
        let Ok(contents) = std::fs::read_to_string(self.main_report_path()) else {
            return Vec::new();
        };
        let value: Value = serde_json::from_str(&contents).expect("persisted report JSON");
        value["delivered"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(ReportIdentity {
                            kill_timestamp_unix_ms: entry["kill_timestamp_unix_ms"].as_i64()?,
                            attempt_count: entry["attempt_count"].as_i64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A child Client process killed and reaped on drop, so a panicking test
/// cannot leak a running kiosk process.
struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn spawn(mut command: Command) -> Self {
        let child = command.spawn().expect("spawn client child process");
        Self { child }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn base_client_command(config_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openpiso-client"));
    command
        .env("OPENPISO_CLIENT_CONFIG_DIR", config_dir)
        // WebView2 must not share one user-data folder across concurrent
        // harness receivers.
        .env("WEBVIEW2_USER_DATA_FOLDER", config_dir.join("webview"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// A sender-only launch of the production Client binary, spawned but not
/// waited on and held at the harness start barrier, so a burst of senders
/// really does race for the same inbox capacity.
fn spawn_barriered_sender(
    config_dir: &Path,
    start_event: &str,
    report_args: &[String],
) -> ChildGuard {
    let mut command = base_client_command(config_dir);
    command
        .env(PROBE_START_EVENT_ENV, start_event)
        .arg("--openpiso-handoff-only")
        .args(report_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ChildGuard::spawn(command)
}

/// Wait for a spawned sender and read its exit code and evidence pipes.
fn wait_for_sender(child: &mut ChildGuard) -> (Option<i32>, String, String) {
    let deadline = Instant::now() + SENDER_TIMEOUT;
    loop {
        match child.child.try_wait().expect("poll sender") {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Some(mut pipe) = child.child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return (status.code(), stdout, stderr);
            }
            None if Instant::now() >= deadline => {
                panic!("sender child did not exit within {SENDER_TIMEOUT:?}")
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// A sender-only launch of the production Client binary: its exit code,
/// stdout, and stderr.
fn run_sender(config_dir: &Path, report_args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = base_client_command(config_dir);
    command
        .arg("--openpiso-handoff-only")
        .args(report_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard::spawn(command);
    let deadline = Instant::now() + SENDER_TIMEOUT;
    loop {
        match child.child.try_wait().expect("poll sender") {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Some(mut pipe) = child.child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return (status.code(), stdout, stderr);
            }
            None if Instant::now() >= deadline => {
                panic!("sender child did not exit within {SENDER_TIMEOUT:?}")
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// An ordinary production launch that becomes the running receiver. Its
/// stdout/stderr mirror (tracing's debug stdout output) lands in
/// `target/tmp/receiver-*.log` so a failed harness run on another machine
/// leaves a post-mortem behind; the isolated config dir's own logs are
/// deleted with the temporary directory.
fn spawn_receiver(machine: &IsolatedMachine) -> ChildGuard {
    let debug_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let _ = std::fs::create_dir_all(debug_dir);
    let log = std::fs::File::create(debug_dir.join(format!(
        "receiver-{}-{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    )))
    .expect("open receiver mirror log");
    let log_err = log.try_clone().expect("clone receiver mirror log");
    let mut command = base_client_command(&machine.config_dir);
    command.stdout(log).stderr(log_err);
    ChildGuard::spawn(command)
}

// ---------------------------------------------------------------------------
// Observation helpers
// ---------------------------------------------------------------------------

/// The two integers one report carries, in one named place so no reader or
/// assertion can transpose timestamp and attempt count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReportIdentity {
    kill_timestamp_unix_ms: i64,
    attempt_count: i64,
}

/// Harness shorthand matching [`submit`]'s argument order.
fn report(kill_timestamp_unix_ms: i64, attempt_count: i64) -> ReportIdentity {
    ReportIdentity {
        kill_timestamp_unix_ms,
        attempt_count,
    }
}

/// The report identity a heartbeat carried, if any. The timestamp travels
/// as an RFC 3339 string and is read back as the integer it started as.
fn tamper_fields(heartbeat: &Value) -> Option<ReportIdentity> {
    Some(ReportIdentity {
        attempt_count: heartbeat["tamper_kill_attempt_count"].as_i64()?,
        kill_timestamp_unix_ms: heartbeat["tamper_kill_timestamp"]
            .as_str()
            .and_then(parse_unix_ms)?,
    })
}

/// Snapshot of how many heartbeats have been recorded so far; pass it to
/// [`wait_for_heartbeat`] so a later wait can only match heartbeats that
/// arrive after the action under test.
fn heartbeat_baseline(admin: &AdminServer) -> usize {
    admin.recorded_heartbeats().len()
}

/// Wait until a heartbeat recorded after `since` satisfies `predicate`.
fn wait_for_heartbeat(
    admin: &AdminServer,
    since: usize,
    what: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        if let Some(found) = admin
            .recorded_heartbeats()
            .iter()
            .skip(since)
            .find(|heartbeat| predicate(heartbeat))
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; observed {} heartbeats since baseline {}",
            admin.recorded_heartbeats().len(),
            since
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn parse_unix_ms(rfc3339: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

/// First heartbeat without tamper fields: the receiver is up, connected,
/// and report-free.
fn wait_for_baseline(admin: &AdminServer) {
    wait_for_heartbeat(
        admin,
        0,
        "the report-free baseline heartbeat",
        |heartbeat| tamper_fields(heartbeat).is_none(),
    );
}

/// Submit a report through a real sender child and require the durable
/// accept: exit code 0 with the inbox file left behind.
fn submit(machine: &IsolatedMachine, timestamp: i64, attempt_count: i64) {
    let (code, stdout, stderr) = run_sender(
        &machine.config_dir,
        &[
            "--openpiso-tamper-kill",
            &timestamp.to_string(),
            &attempt_count.to_string(),
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "sender must durably accept the report (stdout: {stdout}, stderr: {stderr})"
    );
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// A report-bearing local sender can submit a validated report to the
/// production binary without starting a runtime: exit code 0, one durable
/// inbox file, no Admin traffic, and the authoritative file untouched.
/// Malformed, missing, and non-positive arguments are rejected without
/// side effects; an unpersistable inbox is a truthful failure, never a
/// silent drop.
#[test]
fn sender_only_submission_is_durable_inert_and_validated() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);

    submit(&machine, 1_750_000_000_000, 3);
    assert_eq!(
        machine.inbox_files().len(),
        1,
        "one report, one durable inbox file"
    );
    assert!(
        !machine.main_report_path().exists(),
        "sender never touches the authoritative file"
    );

    // Retrying the same delivery rewrites the same file.
    submit(&machine, 1_750_000_000_000, 3);
    assert_eq!(machine.inbox_files().len(), 1);

    // Malformed / non-positive / missing arguments are rejected.
    for bad_args in [
        vec![],
        vec!["--openpiso-tamper-kill"],
        vec!["--openpiso-tamper-kill", "not-a-number", "1"],
        vec!["--openpiso-tamper-kill", "0", "1"],
        vec!["--openpiso-tamper-kill", "1750000000000", "-2"],
        vec!["--openpiso-tamper-kill", "1750000000000"],
    ] {
        let (code, stdout, stderr) = run_sender(&machine.config_dir, &bad_args);
        assert_eq!(
            code,
            Some(2),
            "invalid args {bad_args:?} must fail closed ({stdout} / {stderr})"
        );
    }
    assert_eq!(
        machine.inbox_files().len(),
        1,
        "rejected arguments must not manufacture inbox files"
    );
    assert!(
        admin.recorded_heartbeats().is_empty(),
        "a sender-only launch must never send a heartbeat"
    );

    // An unpersistable inbox is exit code 3: durable acceptance was
    // promised, so failure must be truthful.
    let unwritable = machine.temp.path().join("occupied-config-dir");
    std::fs::write(&unwritable, b"occupied").expect("occupy the config path");
    let (code, _stdout, stderr) = run_sender(
        &unwritable,
        &["--openpiso-tamper-kill", "1750000000000", "1"],
    );
    assert_eq!(
        code,
        Some(3),
        "unpersistable submission must fail truthfully ({stderr})"
    );
}

/// The live receiver ingests a handed-off report, reports it through the
/// existing heartbeat, persists it before any inbox file is removed, and
/// clears it only on the server acknowledgement.
#[test]
fn live_receiver_reports_handoff_and_clears_on_server_ack() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);
    let receiver = spawn_receiver(&machine);
    wait_for_baseline(&admin);

    let (kill_timestamp, attempt_count) = (1_750_000_100_000, 3);
    let baseline = heartbeat_baseline(&admin);
    submit(&machine, kill_timestamp, attempt_count);

    let delivered = wait_for_heartbeat(
        &admin,
        baseline,
        "the heartbeat carrying the handed-off report",
        |heartbeat| tamper_fields(heartbeat) == Some(report(kill_timestamp, attempt_count)),
    );
    // Kill Report Age is derived fresh at send time from the client clock
    // and the reported kill instant (ADR-0048's protocol-only
    // millisecond exception).
    let client_time = delivered["client_time_unix_ms"]
        .as_i64()
        .expect("client time");
    let report_age = delivered["tamper_kill_report_age_ms"]
        .as_i64()
        .expect("report age");
    assert_eq!(report_age, (client_time - kill_timestamp).max(0));

    // Durable before reporting: the merged report is on disk and the
    // inbox is drained (persist happens before inbox removal).
    assert_eq!(
        machine.persisted_report(),
        Some(report(kill_timestamp, attempt_count))
    );
    assert!(machine.inbox_files().is_empty());

    // Clearing requires the server acknowledgement, not local delivery.
    let post_ingest_baseline = heartbeat_baseline(&admin);
    admin.set_ack_recorded(true);
    wait_until("the server-acknowledged report to be cleared", || {
        machine.persisted_report().is_none()
    });
    wait_for_heartbeat(
        &admin,
        post_ingest_baseline,
        "the post-acknowledgement report-free heartbeat",
        |heartbeat| tamper_fields(heartbeat).is_none(),
    );
    drop(receiver);
}

/// Repeated delivery of the same report must not inflate the attempt
/// count; the report identity is the delivery key.
#[test]
fn duplicate_delivery_does_not_inflate_attempt_counts() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);

    // Both deliveries land before any receiver exists: one durable file.
    submit(&machine, 1_750_000_200_000, 2);
    submit(&machine, 1_750_000_200_000, 2);
    assert_eq!(machine.inbox_files().len(), 1);

    let baseline = heartbeat_baseline(&admin);
    let receiver = spawn_receiver(&machine);
    wait_for_heartbeat(
        &admin,
        baseline,
        "the delivered report after startup recovery",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_000_200_000, 2)),
    );
    admin.set_ack_recorded(true);
    wait_until("the acknowledged report to be cleared", || {
        machine.persisted_report().is_none()
    });

    // The acknowledged delivery is remembered, so a late retry of it has no
    // new consequence: no resurrected pending report (issue #371).
    assert!(machine
        .delivery_ledger()
        .contains(&report(1_750_000_200_000, 2)));
    admin.set_ack_recorded(false);
    submit(&machine, 1_750_000_200_000, 2);
    wait_until("the retried delivery to be drained", || {
        machine.inbox_files().is_empty()
    });
    let heartbeats_after_retry = heartbeat_baseline(&admin);
    wait_until("a heartbeat after the retried delivery", || {
        admin.recorded_heartbeats().len() > heartbeats_after_retry
    });
    assert_eq!(
        machine.persisted_report(),
        None,
        "a retry of an acknowledged delivery must not resurrect a report"
    );
    assert!(
        admin
            .recorded_heartbeats()
            .iter()
            .skip(heartbeats_after_retry)
            .all(|heartbeat| tamper_fields(heartbeat).is_none()),
        "a retry of an acknowledged delivery must not reach Admin again"
    );

    // A genuinely distinct kill after the acknowledgement still lands.
    let fresh_baseline = heartbeat_baseline(&admin);
    submit(&machine, 1_750_000_250_000, 1);
    wait_for_heartbeat(
        &admin,
        fresh_baseline,
        "the next genuine report",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_000_250_000, 1)),
    );
    assert_eq!(
        machine.persisted_report(),
        Some(report(1_750_000_250_000, 1))
    );
    drop(receiver);
}

/// Distinct genuine reports keep the existing coalescing semantics: the
/// oldest kill timestamp stays the report name and the attempt count
/// coalesces. One server acknowledgement then clears the whole report.
#[test]
fn distinct_reports_coalesce_under_the_existing_semantics() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);
    let receiver = spawn_receiver(&machine);
    wait_for_baseline(&admin);

    submit(&machine, 1_750_000_300_000, 1);
    let first_baseline = heartbeat_baseline(&admin);
    wait_for_heartbeat(&admin, first_baseline, "the first report", |heartbeat| {
        tamper_fields(heartbeat) == Some(report(1_750_000_300_000, 1))
    });

    // A second, distinct death coalesces into the pending report.
    let second_baseline = heartbeat_baseline(&admin);
    submit(&machine, 1_750_000_400_000, 1);
    wait_for_heartbeat(
        &admin,
        second_baseline,
        "the coalesced report",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_000_300_000, 2)),
    );
    assert_eq!(
        machine.persisted_report(),
        Some(report(1_750_000_300_000, 2))
    );

    admin.set_ack_recorded(true);
    wait_until(
        "the coalesced report to be cleared after one acknowledgement",
        || machine.persisted_report().is_none(),
    );
    drop(receiver);
}

/// During an Admin outage the report stays durable and retries through the
/// existing heartbeat path; clearing still requires the existing server
/// acknowledgement, never a local delivery receipt.
#[test]
fn admin_outage_retains_report_until_server_acknowledgement() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    admin.set_healthy(false);
    let machine = IsolatedMachine::seed_with_admin(&admin);
    let receiver = spawn_receiver(&machine);

    submit(&machine, 1_750_000_500_000, 1);

    // The receiver keeps retrying while the Admin rejects it, and the
    // handed-off report is already durably ingested on the Client side.
    wait_until("the receiver to keep retrying during the outage", || {
        admin.recorded_heartbeats().len() >= 2
    });
    assert_eq!(
        machine.persisted_report(),
        Some(report(1_750_000_500_000, 1)),
        "an outage must not erase the report"
    );

    // Recovery delivers the durable report through the existing path.
    let recovery_baseline = heartbeat_baseline(&admin);
    admin.set_healthy(true);
    wait_for_heartbeat(
        &admin,
        recovery_baseline,
        "the report retried after the outage",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_000_500_000, 1)),
    );

    admin.set_ack_recorded(true);
    wait_until("the acknowledged report to be cleared", || {
        machine.persisted_report().is_none()
    });
    drop(receiver);
}

/// Receiver death around a durable acknowledgement: a report accepted by
/// the sender is recoverable by a successor that never saw the sender,
/// and a receiver that dies after persisting (but before the server
/// acknowledgement) is recovered by the next admitted Client without any
/// duplicate-consequence delivery.
#[test]
fn receiver_death_leaves_the_report_recoverable_by_a_successor() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);

    // Death before receipt: the sender's durable accept outlives the
    // receiver's absence entirely.
    submit(&machine, 1_750_000_600_000, 2);
    let first_baseline = heartbeat_baseline(&admin);
    let first_receiver = spawn_receiver(&machine);
    wait_for_heartbeat(
        &admin,
        first_baseline,
        "the report delivered by the first receiver",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_000_600_000, 2)),
    );
    assert_eq!(
        machine.persisted_report(),
        Some(report(1_750_000_600_000, 2))
    );

    // Death after persist, before the server acknowledgement.
    drop(first_receiver);
    assert_eq!(
        machine.persisted_report(),
        Some(report(1_750_000_600_000, 2)),
        "a receiver that dies before the ack leaves the report recoverable"
    );

    let second_baseline = heartbeat_baseline(&admin);
    let second_receiver = spawn_receiver(&machine);
    wait_for_heartbeat(
        &admin,
        second_baseline,
        "the recovered report from the successor",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_000_600_000, 2)),
    );
    assert!(machine.inbox_files().is_empty());

    admin.set_ack_recorded(true);
    wait_until("the recovered report to be cleared", || {
        machine.persisted_report().is_none()
    });
    drop(second_receiver);
}

// ---------------------------------------------------------------------------
// Startup arbitration (issue #369, ADR-0080)
//
// The production entrypoint ordering — sender check, then the atomic
// check-and-own gate — is exercised through real contender processes of the
// production binary. The test-only overrides activate the production gate in
// this debug build (`OPENPISO_CLIENT_ARBITRATION_ENFORCE`) and isolate the
// native coordination object (`OPENPISO_CLIENT_ARBITRATION_NAME`); production
// never sets either. The `--openpiso-arbitration-probe` launch substitutes an
// inert runtime behind the production admission boundary, so stress rounds
// need no WebView2 boot and no kiosk effect. Admission is observed only
// through the gate's own evidence lines and exit codes — never private flags.
// ---------------------------------------------------------------------------

/// Test-only activation of the production gate inside this debug build.
const ARBITRATION_ENFORCE_ENV: &str = "OPENPISO_CLIENT_ARBITRATION_ENFORCE";
/// Test-only coordination object name, isolating harness native objects.
const ARBITRATION_NAME_ENV: &str = "OPENPISO_CLIENT_ARBITRATION_NAME";
/// Test-only named-event barrier released to start real contenders.
const PROBE_START_EVENT_ENV: &str = "OPENPISO_CLIENT_PROBE_START_EVENT";
/// Test-only probe hold (milliseconds; `0` parks until killed).
const PROBE_HOLD_ENV: &str = "OPENPISO_CLIENT_PROBE_HOLD_MS";
/// The production probe launch flag (an inert runtime behind the gate).
const PROBE_FLAG: &str = "--openpiso-arbitration-probe";

/// The gate's compact console evidence lines.
/// The documented inbox maximum, mirrored here on purpose: the harness
/// observes the production boundary rather than importing its constants.
const MAX_HANDOFF_FILES: usize = 32;
/// The sender's truthful "could not persist" exit code.
const HANDOFF_EXIT_UNPERSISTED: i32 = 3;

/// Concurrent real sender processes near capacity cannot collectively take
/// the inbox past its documented maximum (issue #371 finding 4). Capacity
/// admission is a check-then-publish pair, so without interprocess
/// serialization every sender sees the same spare slot and all of them
/// publish.
#[test]
fn concurrent_senders_never_exceed_the_documented_inbox_capacity() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let inbox = machine.config_dir.join("tamper-handoff-inbox");
    std::fs::create_dir_all(&inbox).expect("create inbox");
    // One slot short of capacity, seeded through the production file shape.
    let seeded = MAX_HANDOFF_FILES - 1;
    for index in 0..seeded {
        let timestamp = 1_800_000_000_000_i64 + index as i64;
        std::fs::write(
            inbox.join(format!("report-{timestamp}-1.json")),
            json!({"kill_timestamp_unix_ms": timestamp, "attempt_count": 1}).to_string(),
        )
        .expect("seed an inbox report");
    }
    assert_eq!(machine.inbox_files().len(), seeded);

    // Sixteen distinct reports, all held at one barrier and released into
    // the same instant.
    let contenders = 16;
    let event_name = unique_object("handoff-capacity-start");
    let barrier = native::StartBarrier::create(&event_name);
    let mut senders: Vec<ChildGuard> = (0..contenders)
        .map(|index| {
            let timestamp = 1_900_000_000_000_i64 + index as i64;
            spawn_barriered_sender(
                &machine.config_dir,
                &event_name,
                &[
                    "--openpiso-tamper-kill".to_owned(),
                    timestamp.to_string(),
                    "1".to_owned(),
                ],
            )
        })
        .collect();
    // Every sender has reached the barrier by the time the last one spawned;
    // releasing now puts them all past the capacity check together.
    std::thread::sleep(Duration::from_millis(300));
    barrier.release();
    let outcomes: Vec<(Option<i32>, String, String)> =
        senders.iter_mut().map(wait_for_sender).collect();

    let accepted = outcomes
        .iter()
        .filter(|(code, _, _)| *code == Some(0))
        .count();
    let refused = outcomes
        .iter()
        .filter(|(code, _, _)| *code == Some(HANDOFF_EXIT_UNPERSISTED))
        .count();
    assert_eq!(
        accepted + refused,
        contenders,
        "every sender must exit with a truthful outcome: {outcomes:?}"
    );
    assert_eq!(
        accepted, 1,
        "exactly one slot was free, so exactly one sender may be accepted: {outcomes:?}"
    );
    let published = machine.inbox_files().len();
    assert_eq!(
        published, MAX_HANDOFF_FILES,
        "the inbox must never exceed its documented maximum, got {published}"
    );
    // Already-accepted reports survive, and a refusal is retryable rather
    // than a dropped report.
    for (code, _, stderr) in &outcomes {
        if *code == Some(HANDOFF_EXIT_UNPERSISTED) {
            assert!(
                stderr.contains("openpiso-handoff: failed"),
                "a refused sender must say so: {stderr:?}"
            );
        }
    }

    // At capacity, re-delivering an already-published report is still
    // accepted: it consumes no slot and drops no evidence.
    let seeded_timestamp = 1_800_000_000_000_i64;
    let (code, stdout, stderr) = run_sender(
        &machine.config_dir,
        &["--openpiso-tamper-kill", &seeded_timestamp.to_string(), "1"],
    );
    assert_eq!(
        code,
        Some(0),
        "an idempotent retry at capacity must be accepted: {stdout:?} {stderr:?}"
    );
    assert_eq!(machine.inbox_files().len(), MAX_HANDOFF_FILES);

    // The admission lock is released, not accumulated.
    assert!(
        !inbox.join(".admission.lock").exists(),
        "the admission lock must not outlive the senders"
    );
}

const ARBITRATION_ADMITTED: &str = "openpiso-arbitration: admitted";
const ARBITRATION_DUPLICATE: &str = "openpiso-arbitration: duplicate-suppressed";
const ARBITRATION_FAILURE: &str = "openpiso-arbitration: coordination-failure";
const ARBITRATION_HANDOFF_ACCEPTED: &str = "handoff=accepted";
const ARBITRATION_HANDOFF_FAILED: &str = "handoff=failed";

/// Synchronized stress rounds (issue #366: at least 100, multiple
/// contenders, varying order, owner-death injection).
const STRESS_ROUNDS: usize = 100;

fn unique_object(label: &str) -> String {
    format!(
        "Local\\openpiso-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0)
    )
}

#[cfg(windows)]
mod native {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL},
            Security::{
                Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
                PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
            },
            System::Threading::{CreateEventW, CreateMutexW, SetEvent},
        },
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A manual-reset event the harness releases so real contenders race
    /// simultaneously (the startup barrier).
    pub(super) struct StartBarrier {
        handle: HANDLE,
    }

    impl StartBarrier {
        pub(super) fn create(name: &str) -> Self {
            let encoded = wide(name);
            let handle = unsafe { CreateEventW(None, true, false, PCWSTR(encoded.as_ptr())) }
                .expect("create the harness start barrier");
            Self { handle }
        }

        pub(super) fn release(&self) {
            unsafe { SetEvent(self.handle) }.expect("release the harness start barrier");
        }
    }

    impl Drop for StartBarrier {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    /// The exit instant the kernel recorded for a finished child process, in
    /// Unix milliseconds. `None` while the process is still running or if the
    /// times cannot be read.
    pub(super) fn process_exit_unix_ms(child: &std::process::Child) -> Option<i64> {
        use std::os::windows::io::AsRawHandle;

        use windows::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};

        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let handle = HANDLE(child.as_raw_handle());
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        let ticks = ((exited.dwHighDateTime as u64) << 32) | u64::from(exited.dwLowDateTime);
        if ticks == 0 {
            // Still running: Windows leaves the exit time zeroed.
            return None;
        }
        // FILETIME counts 100-nanosecond ticks since 1601-01-01 UTC.
        const TICKS_TO_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
        Some((ticks.checked_sub(TICKS_TO_UNIX_EPOCH)? / 10_000) as i64)
    }

    /// A named object held open for a scenario's lifetime, so it exists
    /// while a contender process consults it.
    pub(super) struct HeldObject {
        handle: HANDLE,
    }

    impl HeldObject {
        /// A coordination-name mutex whose DACL denies every access to
        /// Everyone, so a contender's `CreateMutexW` is truthfully denied.
        pub(super) fn create_denied_mutex(name: &str) -> Self {
            let encoded = wide(name);
            // Deny MUTEX_ALL_ACCESS (0x1F0001) to Everyone (WD).
            let sddl = wide("D:(D;;0x001F0001;;;WD)");
            let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(sddl.as_ptr()),
                    1, // SDDL_REVISION_1
                    &mut descriptor,
                    None,
                )
            }
            .expect("build the deny-all security descriptor");
            let security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let handle = unsafe { CreateMutexW(Some(&security), false, PCWSTR(encoded.as_ptr())) }
                .expect("create the denied coordination object");
            unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
            Self { handle }
        }

        /// An *incompatible* object type (an event) under the coordination
        /// name, so a contender's `CreateMutexW` cannot consult it.
        pub(super) fn create_event_object(name: &str) -> Self {
            let encoded = wide(name);
            let handle = unsafe { CreateEventW(None, true, false, PCWSTR(encoded.as_ptr())) }
                .expect("create the incompatible coordination object");
            Self { handle }
        }
    }

    impl Drop for HeldObject {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

/// One contender process of the production binary, with its evidence pipes
/// captured. Killed and reaped on drop.
struct Contender {
    child: ChildGuard,
}

impl Contender {
    fn spawn(mut command: Command) -> Self {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        Self {
            child: ChildGuard::spawn(command),
        }
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.child.try_wait().expect("poll contender")
    }

    fn exit_code(&mut self) -> Option<i32> {
        self.exited().and_then(|status| status.code())
    }

    /// Request termination without waiting, so a burst of kills can be
    /// issued before any of them is reaped.
    fn terminate(&mut self) {
        let _ = self.child.child.kill();
    }

    /// Block until the process is gone, closing its ends of the evidence
    /// pipes (the harness drains its own ends in `evidence()`).
    fn reap(&mut self) {
        let _ = self.child.child.wait();
    }

    fn process_id(&self) -> u32 {
        self.child.child.id()
    }

    /// The instant the OS says this process exited, in Unix milliseconds.
    /// Only meaningful after the process is gone. This is the death half of
    /// an admitted runtime's lifetime: a killed owner cannot report its own
    /// release, but the kernel records when it stopped running.
    fn exited_at_unix_ms(&self) -> Option<i64> {
        native::process_exit_unix_ms(&self.child.child)
    }

    /// Both evidence pipes, read after the process is gone.
    fn evidence(&mut self) -> String {
        let mut evidence = String::new();
        if let Some(mut pipe) = self.child.child.stdout.take() {
            let _ = pipe.read_to_string(&mut evidence);
        }
        if let Some(mut pipe) = self.child.child.stderr.take() {
            let _ = pipe.read_to_string(&mut evidence);
        }
        evidence
    }
}

/// A probe contender command: the production binary through the production
/// entrypoint ordering, with the gate enforced and the object isolated.
fn probe_command(machine: &IsolatedMachine, object: &str) -> Command {
    let mut command = base_client_command(&machine.config_dir);
    command
        .env(ARBITRATION_ENFORCE_ENV, "1")
        .env(ARBITRATION_NAME_ENV, object)
        .arg(PROBE_FLAG);
    command
}

/// The same probe with the gate inert — the debug/non-Windows development
/// behavior, and the pre-fix startup ordering this ticket closes. Debug
/// scenarios only: a release child enforces the production gate natively,
/// so the inert capture below is meaningless there.
#[cfg(debug_assertions)]
fn inert_probe_command(machine: &IsolatedMachine, object: &str) -> Command {
    let mut command = base_client_command(&machine.config_dir);
    command.env(ARBITRATION_NAME_ENV, object).arg(PROBE_FLAG);
    command
}

/// Count one outcome kind across the contenders' evidence.
fn count_evidence(evidence: &[String], marker: &str) -> usize {
    evidence.iter().filter(|line| line.contains(marker)).count()
}

/// Kill every still-running contender (owner-death injection). Termination
/// is requested from every alive contender before any of them is reaped: a
/// blocking reap between two kills would hand a contender that is still
/// deciding the time to observe the first kill's abandoned ownership and
/// succeed it — correct gate behavior, but it would turn one injection into
/// a sequence of successions.
fn kill_survivors(contenders: &mut [Contender]) {
    for contender in contenders.iter_mut() {
        if contender.exited().is_none() {
            contender.terminate();
        }
    }
    for contender in contenders.iter_mut() {
        contender.reap();
    }
}

/// One admitted runtime's lifetime: from the instant the gate admitted it
/// (recorded by the admitted process itself, before it holds ownership) to
/// the instant the kernel says it stopped running. Both halves are integer
/// Unix milliseconds and both truncate, so the ordering between two real
/// instants is preserved and a strict comparison cannot invent an overlap.
#[derive(Debug, Clone, Copy)]
struct AdmittedLifetime {
    /// Read only through `Debug` in the failure message, where naming the
    /// process is what makes an overlap report actionable.
    #[allow(dead_code)]
    process_id: u32,
    admitted_at_unix_ms: i64,
    exited_at_unix_ms: i64,
}

/// The admitted lifetimes of the contenders in this round, built from each
/// admitted process's own marker plus the kernel's exit time. A contender
/// that never reached admission contributes nothing; a contender that was
/// admitted but whose exit time cannot be read is a hard failure rather than
/// a silently dropped interval.
fn admitted_lifetimes(
    machine: &IsolatedMachine,
    contenders: &[Contender],
) -> Vec<AdmittedLifetime> {
    let markers = machine.admission_markers();
    contenders
        .iter()
        .filter_map(|contender| {
            let process_id = contender.process_id();
            let admitted_at_unix_ms = markers
                .iter()
                .find(|(pid, _)| *pid == process_id)
                .map(|(_, at)| *at)?;
            let exited_at_unix_ms = contender.exited_at_unix_ms().unwrap_or_else(|| {
                panic!("no exit time for admitted process {process_id}; it must be reaped first")
            });
            Some(AdmittedLifetime {
                process_id,
                admitted_at_unix_ms,
                exited_at_unix_ms,
            })
        })
        .collect()
}

/// The first pair of admitted lifetimes that overlap in time, if any. Two
/// admitted runtimes overlap when each was admitted strictly before the
/// other exited — sequential succession after a death touches at a boundary
/// and is therefore not an overlap.
fn overlapping_admissions(
    lifetimes: &[AdmittedLifetime],
) -> Option<(AdmittedLifetime, AdmittedLifetime)> {
    for (index, first) in lifetimes.iter().enumerate() {
        for second in &lifetimes[index + 1..] {
            if first.admitted_at_unix_ms < second.exited_at_unix_ms
                && second.admitted_at_unix_ms < first.exited_at_unix_ms
            {
                return Some((*first, *second));
            }
        }
    }
    None
}

/// How many contenders are still running (the parked owner, if any).
fn count_running(contenders: &mut [Contender]) -> usize {
    let mut running = 0;
    for contender in contenders.iter_mut() {
        if contender.exited().is_none() {
            running += 1;
        }
    }
    running
}

/// A full production launch under the enforced gate (a real runtime:
/// placeholder adapters, Guard connection attempt, Tauri/WebView2, network
/// tasks) with a per-child WebView2 user-data folder. Its stdout/stderr
/// mirror lands in `target/tmp/` for post-mortem.
fn spawn_enforced_receiver(
    machine: &IsolatedMachine,
    object: &str,
    webview_suffix: &str,
    extra_args: &[&str],
) -> Contender {
    let debug_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let _ = std::fs::create_dir_all(debug_dir);
    let log = std::fs::File::create(debug_dir.join(format!(
        "receiver-{}-{}-{}.log",
        webview_suffix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or(0)
    )))
    .expect("open enforced receiver mirror log");
    let log_err = log.try_clone().expect("clone enforced receiver mirror log");
    let mut command = base_client_command(&machine.config_dir);
    command
        .env(ARBITRATION_ENFORCE_ENV, "1")
        .env(ARBITRATION_NAME_ENV, object)
        .env(
            "WEBVIEW2_USER_DATA_FOLDER",
            machine.config_dir.join(webview_suffix),
        );
    command.args(extra_args).stdout(log).stderr(log_err);
    Contender::spawn(command)
}

/// A report-bearing or plain duplicate launch of the full production binary
/// while the gate is enforced. Returns its exit code and evidence.
fn run_arbitrated_duplicate(
    machine: &IsolatedMachine,
    object: &str,
    extra_args: &[&str],
) -> (Option<i32>, String) {
    let mut command = base_client_command(&machine.config_dir);
    command
        .env(ARBITRATION_ENFORCE_ENV, "1")
        .env(ARBITRATION_NAME_ENV, object);
    command.args(extra_args);
    let mut contender = Contender::spawn(command);
    let code = wait_for_exit(&mut contender, "the arbitrated duplicate");
    (code, contender.evidence())
}

/// The pre-fix failure at this seam: with the gate inert — today's startup
/// ordering, which is also the preserved debug/non-Windows development
/// behavior — two competing launches are both admitted. This scenario must
/// keep showing two admissions until and unless the gate activates; the
/// enforced scenarios below prove exactly one is admitted after the fix.
/// Debug builds only: the child binary of a release test run enforces the
/// production gate natively (no override), so the inert capture cannot
/// exist there — release enforcement is what every other scenario runs.
#[cfg(debug_assertions)]
#[test]
fn inert_gate_admits_two_competing_launches_pre_fix_evidence() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("inert");
    let mut first = Contender::spawn(inert_probe_command(&machine, &object));
    let mut second = Contender::spawn(inert_probe_command(&machine, &object));
    // Both probes exit on their own after their default hold: both were
    // admitted, because an inert gate arbitrates nothing.
    let first_code = wait_for_exit(&mut first, "the first inert probe");
    let second_code = wait_for_exit(&mut second, "the second inert probe");
    assert_eq!(first_code, Some(0));
    assert_eq!(second_code, Some(0));
    let first_evidence = first.evidence();
    let second_evidence = second.evidence();
    assert!(
        first_evidence.contains(ARBITRATION_ADMITTED)
            && second_evidence.contains(ARBITRATION_ADMITTED),
        "the pre-fix failure at this seam is two admitted launches; got {first_evidence:?} / {second_evidence:?}"
    );
}

fn wait_for_exit(contender: &mut Contender, what: &str) -> Option<i32> {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        if let Some(code) = contender.exit_code() {
            return Some(code);
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} to exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The core post-fix proof at the seam: two simultaneous production
/// launches, one start barrier, real native object — exactly one admitted
/// runtime, one inert handled duplicate, no coordination failure.
#[test]
fn enforced_gate_admits_exactly_one_of_two_simultaneous_launches() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("gate");
    let barrier = native::StartBarrier::create(&format!("{object}-start"));
    let mut contenders = Vec::new();
    for _ in 0..2 {
        let mut command = probe_command(&machine, &object);
        command
            .env(PROBE_HOLD_ENV, "0")
            .env(PROBE_START_EVENT_ENV, format!("{object}-start"));
        contenders.push(Contender::spawn(command));
    }
    barrier.release();
    // The loser decides within milliseconds of the release and exits.
    wait_until("the losing contender to exit", || {
        count_running(&mut contenders) <= 1
    });
    // The remaining contender is the admitted owner, parked; kill it so the
    // pipes close and the evidence is readable.
    kill_survivors(&mut contenders);
    let evidence: Vec<String> = contenders.iter_mut().map(Contender::evidence).collect();
    assert_eq!(
        count_evidence(&evidence, ARBITRATION_ADMITTED),
        1,
        "exactly one admitted runtime per round: {evidence:?}"
    );
    assert_eq!(
        count_evidence(&evidence, ARBITRATION_DUPLICATE),
        1,
        "the loser is a handled duplicate: {evidence:?}"
    );
    assert_eq!(
        count_evidence(&evidence, ARBITRATION_FAILURE),
        0,
        "a normal race is not a coordination failure: {evidence:?}"
    );
}

/// Wait until a probe owner's durable local log records its admission. The
/// owner parks (hold `0`) holding the coordination object, so this is the
/// deterministic signal that the next spawn will lose arbitration — no
/// barrier race where the intended loser could win instead.
fn wait_for_owner_admission(machine: &IsolatedMachine, what: &str) {
    wait_until(what, || {
        let logs = machine.config_dir.join("logs");
        let Ok(entries) = std::fs::read_dir(&logs) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("client.") && name.ends_with(".log")) {
                return false;
            }
            std::fs::read_to_string(entry.path())
                .map(|contents| contents.contains("probe owner admitted"))
                .unwrap_or(false)
        })
    });
}

/// A report-bearing loser durably hands the Guard's report to the admitted
/// owner before its handled-success exit; a plain loser invents no report;
/// and ownership is recoverable by a successor after the owner dies.
#[test]
fn report_carrying_loser_hands_off_before_success_and_owner_death_recovers() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("loser");
    let mut owner_command = probe_command(&machine, &object);
    owner_command.env(PROBE_HOLD_ENV, "0");
    let mut owner = Contender::spawn(owner_command);
    // Deterministic owner-then-loser ordering: the owner parks holding the
    // object before the loser ever spawns.
    wait_for_owner_admission(&machine, "the probe owner to record its admission");
    let mut loser_command = probe_command(&machine, &object);
    loser_command.args(["--openpiso-tamper-kill", "175000000900000", "2"]);
    let mut loser = Contender::spawn(loser_command);
    assert_eq!(
        wait_for_exit(&mut loser, "the report-carrying loser"),
        Some(0),
        "a report-carrying loser reports a handled-success outcome"
    );
    let loser_evidence = loser.evidence();
    assert!(
        loser_evidence.contains(ARBITRATION_DUPLICATE),
        "{loser_evidence:?}"
    );
    assert!(
        loser_evidence.contains(ARBITRATION_HANDOFF_ACCEPTED),
        "the report must be durably handed off before success: {loser_evidence:?}"
    );
    assert_eq!(machine.inbox_files().len(), 1, "one durable inbox file");
    assert!(
        !machine.main_report_path().exists(),
        "the loser never touches the authoritative report"
    );

    // Owner death: the report survives and a successor acquires cleanly,
    // with no stale-file cleanup and no second admission overlap.
    kill_survivors(std::slice::from_mut(&mut owner));
    let mut successor = Contender::spawn(probe_command(&machine, &object));
    assert_eq!(
        wait_for_exit(&mut successor, "the successor probe"),
        Some(0),
        "a successor must acquire after owner death"
    );
    let successor_evidence = successor.evidence();
    assert!(
        successor_evidence.contains(ARBITRATION_ADMITTED),
        "the successor must be admitted: {successor_evidence:?}"
    );
}

/// Permission errors are truthful coordination failures, never silent
/// admission or a benign duplicate.
#[test]
fn denied_coordination_object_is_a_coordination_failure() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("denied");
    let denied = native::HeldObject::create_denied_mutex(&object);
    let mut contender = Contender::spawn(probe_command(&machine, &object));
    assert_eq!(
        wait_for_exit(&mut contender, "the denied contender"),
        Some(4),
        "denied access must exit nonzero"
    );
    let evidence = contender.evidence();
    assert!(
        evidence.contains(ARBITRATION_FAILURE),
        "denied access must be reported as a coordination failure: {evidence:?}"
    );
    assert!(
        !evidence.contains(ARBITRATION_ADMITTED) && !evidence.contains(ARBITRATION_DUPLICATE),
        "a denied contender must not pose as admitted or as a benign duplicate: {evidence:?}"
    );
    drop(denied);
}

/// An incompatible object type under the coordination name is a truthful
/// coordination failure too.
#[test]
fn incompatible_coordination_object_is_a_coordination_failure() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("incompatible");
    let incompatible = native::HeldObject::create_event_object(&object);
    let mut contender = Contender::spawn(probe_command(&machine, &object));
    assert_eq!(
        wait_for_exit(&mut contender, "the incompatible-object contender"),
        Some(4),
        "an incompatible object must exit nonzero"
    );
    let evidence = contender.evidence();
    assert!(
        evidence.contains(ARBITRATION_FAILURE),
        "an incompatible object must be reported as a coordination failure: {evidence:?}"
    );
    drop(incompatible);
}

/// 100 synchronized stress rounds with two to four real contenders. Launch
/// order genuinely varies — together, staggered, owner killed mid-decision,
/// owner exiting cleanly — and at most one runtime is ever admitted at any
/// instant: barrier and staggered rounds admit exactly one with every other
/// contender a handled duplicate, and the owner-death rounds kill an owner
/// that really reached admission, then show directly that no two admitted
/// lifetimes in the round overlapped before converging on one successor
/// without a coordination failure.
#[test]
fn synchronized_stress_admits_at_most_one_runtime_per_round() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("stress");
    for round in 0..STRESS_ROUNDS {
        let contenders_count = 2 + (round % 3);
        let event_name = format!("{object}-start-{round}");
        match round % 4 {
            0 => {
                // All contenders spawn, then the barrier releases them into
                // a same-instant race.
                let barrier = native::StartBarrier::create(&event_name);
                let mut contenders = Vec::new();
                for _ in 0..contenders_count {
                    let mut command = probe_command(&machine, &object);
                    command
                        .env(PROBE_HOLD_ENV, "0")
                        .env(PROBE_START_EVENT_ENV, &event_name);
                    contenders.push(Contender::spawn(command));
                }
                barrier.release();
                assert_stress_round(round, contenders_count, &mut contenders);
            }
            1 => {
                // Owner-death injection: kill an owner that actually reached
                // admission, while duplicates are still deciding.
                machine.clear_admission_markers();
                let barrier = native::StartBarrier::create(&event_name);
                let mut contenders = Vec::new();
                for _ in 0..contenders_count {
                    let mut command = probe_command(&machine, &object);
                    command
                        .env(PROBE_HOLD_ENV, "0")
                        .env(PROBE_START_EVENT_ENV, &event_name);
                    contenders.push(Contender::spawn(command));
                }
                barrier.release();
                // Killing every child before arbitration would not exercise
                // owner-death recovery at all, so wait for a real admission.
                wait_until(
                    "an owner to reach admission before the death injection",
                    || !machine.admission_markers().is_empty(),
                );
                kill_survivors(&mut contenders);
                // Direct evidence of the invariant: the admitted lifetimes
                // observed in this round do not overlap. A contender that was
                // still deciding may legitimately succeed the killed owner
                // through the abandoned ownership, which is a later,
                // non-overlapping lifetime — so this permits succession while
                // still rejecting two owners alive at the same instant.
                let lifetimes = admitted_lifetimes(&machine, &contenders);
                assert!(
                    !lifetimes.is_empty(),
                    "round {round}: no owner reached admission, so nothing was killed as an owner"
                );
                assert!(
                    overlapping_admissions(&lifetimes).is_none(),
                    "round {round} admitted overlapping runtimes: {:?}",
                    overlapping_admissions(&lifetimes)
                );
                let evidence: Vec<String> =
                    contenders.iter_mut().map(Contender::evidence).collect();
                assert!(
                    count_evidence(&evidence, ARBITRATION_ADMITTED) <= lifetimes.len(),
                    "round {round}: every admitted line must have a recorded lifetime: {evidence:?}"
                );
                assert_eq!(
                    count_evidence(&evidence, ARBITRATION_FAILURE),
                    0,
                    "round {round} must not fail coordination: {evidence:?}"
                );
                assert_successor_admits(round, &machine, &object);
            }
            2 => {
                // Staggered arrival: the first contender starts and releases
                // itself; later arrivals must find the object busy.
                let barrier = native::StartBarrier::create(&event_name);
                let mut command = probe_command(&machine, &object);
                command
                    .env(PROBE_HOLD_ENV, "0")
                    .env(PROBE_START_EVENT_ENV, &event_name);
                let mut contenders = vec![Contender::spawn(command)];
                barrier.release();
                std::thread::sleep(Duration::from_millis(40));
                for _ in 1..contenders_count {
                    let mut command = probe_command(&machine, &object);
                    command
                        .env(PROBE_HOLD_ENV, "0")
                        .env(PROBE_START_EVENT_ENV, &event_name);
                    contenders.push(Contender::spawn(command));
                }
                assert_stress_round(round, contenders_count, &mut contenders);
            }
            _ => {
                // Clean owner exit: the owner holds briefly and exits on its
                // own; the successor must acquire with no stale cleanup.
                let mut owner_command = probe_command(&machine, &object);
                owner_command.env(PROBE_HOLD_ENV, "300");
                let mut owner = Contender::spawn(owner_command);
                assert_eq!(
                    wait_for_exit(&mut owner, "the clean-exit owner"),
                    Some(0),
                    "round {round}: a clean owner exit is a handled exit"
                );
                assert!(
                    owner.evidence().contains(ARBITRATION_ADMITTED),
                    "round {round}: the clean-exit owner was admitted"
                );
                assert_successor_admits(round, &machine, &object);
            }
        }
    }
}

/// The owner-death assertion is only worth anything if it can go red. With
/// the gate inert — the pre-#369 startup behavior — two probes are both
/// admitted and both park, so their lifetimes genuinely overlap; the same
/// helper the stress rounds use must report that overlap. The enforced
/// counterpart of this scenario is round kind 1 above, which passes.
///
/// Debug-only: a release child enforces the production gate natively, so the
/// inert capture is not reproducible there.
#[cfg(debug_assertions)]
#[test]
fn the_owner_death_overlap_assertion_detects_a_real_overlap() {
    let _harness = harness_lock();
    let machine = IsolatedMachine::bare();
    let object = unique_object("overlap-seam");
    machine.clear_admission_markers();
    let mut contenders: Vec<Contender> = (0..2)
        .map(|_| {
            let mut command = inert_probe_command(&machine, &object);
            command.env(PROBE_HOLD_ENV, "0");
            Contender::spawn(command)
        })
        .collect();
    wait_until("both inert probes to be admitted", || {
        machine.admission_markers().len() == 2
    });
    kill_survivors(&mut contenders);
    let lifetimes = admitted_lifetimes(&machine, &contenders);
    assert_eq!(lifetimes.len(), 2, "the inert gate admits both launches");
    let overlap = overlapping_admissions(&lifetimes);
    assert!(
        overlap.is_some(),
        "the non-overlap assertion must detect two simultaneously admitted runtimes: {lifetimes:?}"
    );
}

/// Sequential ownership is not an overlap: an owner killed at T and a
/// successor admitted at or after T must pass the same check, so restoring
/// this evidence cannot reject legitimate succession.
#[test]
fn sequential_admissions_are_not_reported_as_an_overlap() {
    let killed = AdmittedLifetime {
        process_id: 1,
        admitted_at_unix_ms: 1_000,
        exited_at_unix_ms: 1_050,
    };
    let successor = AdmittedLifetime {
        process_id: 2,
        admitted_at_unix_ms: 1_050,
        exited_at_unix_ms: 1_090,
    };
    let third = AdmittedLifetime {
        process_id: 3,
        admitted_at_unix_ms: 1_090,
        exited_at_unix_ms: 1_200,
    };
    assert!(overlapping_admissions(&[killed, successor, third]).is_none());
    let overlapping = AdmittedLifetime {
        process_id: 4,
        admitted_at_unix_ms: 1_049,
        exited_at_unix_ms: 1_100,
    };
    assert!(overlapping_admissions(&[killed, overlapping]).is_some());
}

/// Ordinary-round assertions: exactly one admitted runtime, every other
/// contender an inert handled duplicate, no coordination failure. Kills the
/// parked owner so its evidence pipe closes.
fn assert_stress_round(round: usize, contenders_count: usize, contenders: &mut [Contender]) {
    wait_until("the losers to exit", || count_running(contenders) <= 1);
    kill_survivors(contenders);
    let evidence: Vec<String> = contenders.iter_mut().map(Contender::evidence).collect();
    assert_eq!(
        count_evidence(&evidence, ARBITRATION_ADMITTED),
        1,
        "round {round} admitted more than one runtime: {evidence:?}"
    );
    assert_eq!(
        count_evidence(&evidence, ARBITRATION_DUPLICATE),
        contenders_count - 1,
        "round {round} losers: {evidence:?}"
    );
    assert_eq!(
        count_evidence(&evidence, ARBITRATION_FAILURE),
        0,
        "round {round} must not fail coordination: {evidence:?}"
    );
}

/// After owner death or clean exit, a successor acquires ownership with no
/// stale-file cleanup and no manual intervention.
fn assert_successor_admits(round: usize, machine: &IsolatedMachine, object: &str) {
    let mut successor = Contender::spawn(probe_command(machine, object));
    assert_eq!(
        wait_for_exit(&mut successor, "the round successor"),
        Some(0),
        "round {round}: a successor must acquire after owner death"
    );
    let successor_evidence = successor.evidence();
    assert!(
        successor_evidence.contains(ARBITRATION_ADMITTED),
        "round {round}: the successor must be admitted: {successor_evidence:?}"
    );
}

/// The full production runtime under the enforced gate: two simultaneous
/// full launches admit exactly one runtime; a report-carrying duplicate
/// hands off to the running owner and reaches the existing
/// server-acknowledged path; a plain duplicate is inert and mutates no
/// unrelated configuration state.
#[test]
fn full_launches_admit_one_runtime_report_loser_hands_off_and_plain_loser_is_inert() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);
    let object = unique_object("full");

    // Two simultaneous full launches of the production binary.
    let mut first = spawn_enforced_receiver(&machine, &object, "webview-first", &[]);
    let mut second = spawn_enforced_receiver(&machine, &object, "webview-second", &[]);
    wait_until("one full launch to lose arbitration", || {
        first.exited().is_some() || second.exited().is_some()
    });
    let (loser_status, loser_alive) = match (first.exited(), second.exited()) {
        (Some(code), None) => (code, &mut second),
        (None, Some(code)) => (code, &mut first),
        (Some(_), Some(_)) => panic!("both full launches exited: no runtime was admitted"),
        (None, None) => unreachable!("waited for one exit above"),
    };
    assert_eq!(
        loser_status.code(),
        Some(0),
        "the losing full launch must be a handled duplicate"
    );
    // The loser's evidence landed in its mirror log; the survivor must stay
    // alive and reach the test Admin.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        loser_alive.exited().is_none(),
        "the surviving full launch must keep running as the runtime"
    );
    wait_for_baseline(&admin);
    assert!(
        machine.inbox_files().is_empty(),
        "an ordinary race must not invent a report"
    );
    assert!(
        !admin
            .recorded_heartbeats()
            .iter()
            .any(|heartbeat| tamper_fields(heartbeat).is_some()),
        "an ordinary race must not invent a Tamper Kill"
    );

    // Report-carrying duplicate (the Guard relaunch shape: typed report
    // arguments, no sender-only flag) hands off durably to the running
    // owner and reaches the existing authoritative acknowledgement path.
    let config_before = std::fs::read(machine.config_dir.join("client-config.json"))
        .expect("read config before duplicates");
    let (loser_exit, loser_evidence) = run_arbitrated_duplicate(
        &machine,
        &object,
        &["--openpiso-tamper-kill", "1750001000000", "3"],
    );
    assert_eq!(
        loser_exit,
        Some(0),
        "the report loser is a handled duplicate"
    );
    assert!(
        loser_evidence.contains(ARBITRATION_DUPLICATE)
            && loser_evidence.contains(ARBITRATION_HANDOFF_ACCEPTED),
        "the report loser must hand off before success: {loser_evidence:?}"
    );
    assert_eq!(machine.inbox_files().len(), 1);
    let handoff_baseline = heartbeat_baseline(&admin);
    wait_for_heartbeat(
        &admin,
        handoff_baseline,
        "the handed-off report through the running owner",
        |heartbeat| tamper_fields(heartbeat) == Some(report(1_750_001_000_000, 3)),
    );
    assert_eq!(
        machine.persisted_report(),
        Some(report(1_750_001_000_000, 3)),
        "the merged report is durable before any inbox removal"
    );
    assert!(machine.inbox_files().is_empty());
    admin.set_ack_recorded(true);
    wait_until("the handed-off report to be server-acknowledged", || {
        machine.persisted_report().is_none()
    });
    admin.set_ack_recorded(false);

    // A plain duplicate is inert: no report, no new heartbeat consequence,
    // no configuration mutation, and the runtime keeps running.
    let plain_baseline = heartbeat_baseline(&admin);
    let (plain_exit, plain_evidence) = run_arbitrated_duplicate(&machine, &object, &[]);
    assert_eq!(plain_exit, Some(0));
    assert!(
        plain_evidence.contains(ARBITRATION_DUPLICATE)
            && !plain_evidence.contains(ARBITRATION_HANDOFF_ACCEPTED)
            && !plain_evidence.contains(ARBITRATION_HANDOFF_FAILED),
        "a plain duplicate invents no handoff: {plain_evidence:?}"
    );
    assert!(machine.inbox_files().is_empty());
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        admin
            .recorded_heartbeats()
            .iter()
            .skip(plain_baseline)
            .all(|heartbeat| tamper_fields(heartbeat).is_none()),
        "a plain duplicate must not surface a report"
    );
    let config_after = std::fs::read(machine.config_dir.join("client-config.json"))
        .expect("read config after duplicates");
    assert_eq!(
        config_before, config_after,
        "duplicate launches must not mutate unrelated configuration state"
    );
    // The runtime is still the owner: another duplicate still loses.
    let (still_exit, _) = run_arbitrated_duplicate(&machine, &object, &[]);
    assert_eq!(still_exit, Some(0));
    // Drop (kill) the surviving runtime: the OS releases its ownership.
}

/// A full runtime that crashes releases ownership: a successor acquires
/// without stale-file cleanup and reaches the test Admin again.
#[test]
fn crashed_owner_ownership_is_recovered_by_a_successor() {
    let _harness = harness_lock();
    let admin = AdminServer::start();
    let machine = IsolatedMachine::seed_with_admin(&admin);
    let object = unique_object("crash");

    let first = spawn_enforced_receiver(&machine, &object, "webview-crash-first", &[]);
    wait_for_baseline(&admin);
    drop(first); // unclean death (kill)

    let second_baseline = heartbeat_baseline(&admin);
    let second = spawn_enforced_receiver(&machine, &object, "webview-crash-second", &[]);
    wait_for_heartbeat(
        &admin,
        second_baseline,
        "the successor runtime's first heartbeat",
        |_| true,
    );
    drop(second);
}

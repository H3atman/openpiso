//! PC Monitoring manual checks and the continuous monitor (ADR-0076).
//!
//! "Check now" compares **Host Reachability** (a live ICMP echo against the
//! PC's server-owned `registered_ip`) with **Client Reachability** (the
//! heartbeat verdict Admin already owns). It is strictly read-only: no
//! incident candidate, Audit row, Notification, session, money, Remaining
//! Time, or Points value changes.
//!
//! The continuous monitor ([`MonitoringService`]) runs on its own ten-second
//! cadence — never inside the one-second money and time timer — and turns two
//! sustained conditions into durable incidents: a Host-Reachable +
//! Client-Unreachable coexistence becomes a **Client Reachability Mismatch**
//! after 60 seconds (#341), and failed probes on a Client-reachable PC become
//! **Host Probe Coverage** loss after two minutes (#343). Per-PC
//! **Monitoring Suppression** (#342) gates both opening paths for every kind:
//! while a PC is suppressed no new incident may open, an already-open episode
//! stays open, and expiry restarts each full grace period from zero. Probes
//! live behind the [`HostProbe`] seam so every test injects a deterministic
//! fake; grace math runs against an injected [`MonitorClock`]. Production
//! wiring uses [`production_probe`]: a direct Windows IP Helper ICMP adapter
//! on Windows, and an explicit unsupported adapter elsewhere — a non-Windows
//! Admin can never manufacture a reachable result.
//!
//! Targets are always selected by Admin from server-owned registered IPs.
//! Nothing here accepts an address from any caller.

use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::net::Ipv4Addr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;
use tracing::error;
use uuid::Uuid;

use crate::{
    auth::AccountTokens,
    db,
    db::monitoring::IncidentKind,
    errors::{AppError, AppResult},
    guard_poll::{escalation_condition_holds, EscalationInputs, GuardRegistry},
    models::GuardCoverageReport,
    sessions::SessionStore,
};

/// Fixed v1 probe timeout (ADR-0076): one second per echo request.
#[cfg(windows)]
const PROBE_TIMEOUT_MS: u32 = 1_000;

/// Diagnostic payload carried by each echo request.
#[cfg(windows)]
const PAYLOAD: &[u8] = b"openpiso-host-probe";

/// Outcome of one host probe attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostProbeOutcome {
    /// An echo reply arrived within the timeout.
    Reply,
    /// No reply arrived within the timeout (or the probe could not run).
    Timeout,
    /// The stored address is not a usable IPv4 target.
    InvalidTarget,
    /// This platform cannot send probes (non-Windows Admin).
    Unsupported,
}

/// Seam for Host Reachability probes. Production uses the Windows ICMP
/// adapter; tests inject deterministic fakes keyed by registered address.
pub trait HostProbe: Send + Sync {
    fn probe(&self, registered_ip: &str) -> HostProbeOutcome;

    /// Whether this Admin platform can send probes at all. ADR-0076 keeps PC
    /// Monitoring Windows-only in v1: a non-Windows Admin refuses to enable
    /// monitoring instead of manufacturing unavailable episodes forever.
    fn supported(&self) -> bool;
}

/// Production adapter selection: real ICMP on Windows, explicit
/// unavailability everywhere else.
pub fn production_probe() -> Arc<dyn HostProbe> {
    #[cfg(windows)]
    {
        Arc::new(WindowsIcmpProbe)
    }
    #[cfg(not(windows))]
    {
        Arc::new(UnsupportedPlatformProbe)
    }
}

/// Non-Windows stand-in: every probe reports unsupported, never a reply.
/// Exists wherever the production selector can return it or tests may use it.
#[cfg(any(not(windows), test))]
pub struct UnsupportedPlatformProbe;

#[cfg(any(not(windows), test))]
impl HostProbe for UnsupportedPlatformProbe {
    fn probe(&self, _registered_ip: &str) -> HostProbeOutcome {
        HostProbeOutcome::Unsupported
    }

    fn supported(&self) -> bool {
        false
    }
}

/// Windows ICMP adapter (ADR-0076): one synchronous `IcmpSendEcho` call
/// through the Windows API with the fixed one-second timeout. It never
/// starts ping, PowerShell, a shell, or a sidecar process.
#[cfg(windows)]
pub struct WindowsIcmpProbe;

#[cfg(windows)]
impl HostProbe for WindowsIcmpProbe {
    fn probe(&self, registered_ip: &str) -> HostProbeOutcome {
        // A malformed stored address is bad Admin data, never evidence that
        // the host failed to answer.
        let address: Ipv4Addr = match registered_ip.trim().parse() {
            Ok(address) => address,
            Err(_) => return HostProbeOutcome::InvalidTarget,
        };
        match send_echo(address) {
            Ok(true) => HostProbeOutcome::Reply,
            Ok(false) | Err(_) => HostProbeOutcome::Timeout,
        }
    }

    fn supported(&self) -> bool {
        true
    }
}

/// Send exactly one ICMP echo and report whether an answer came back.
#[cfg(windows)]
fn send_echo(address: Ipv4Addr) -> std::io::Result<bool> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::IpHelper::{
        IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY,
    };

    // The destination is the IPv4 value the IP Helper API expects: the four
    // octets in wire order, exactly as `inet_addr` lays them down. Pack with
    // native endianness so the memory layout — not the arithmetic value —
    // matches; `from_be_bytes` would reverse the address on little-endian
    // Windows and silently ping the wrong host.
    let destination = u32::from_ne_bytes(address.octets());

    // SAFETY: `IcmpCreateFile` has no documented failure mode other than the
    // returned error; the handle is closed on every path.
    let handle: HANDLE =
        unsafe { IcmpCreateFile() }.map_err(|_| std::io::Error::last_os_error())?;

    let reply_buffer_size = (std::mem::size_of::<ICMP_ECHO_REPLY>() + PAYLOAD.len()) as u32;
    let mut buffer = vec![0u8; reply_buffer_size as usize];

    // SAFETY: `buffer` is sized exactly as `IcmpSendEcho` requires
    // (`sizeof(ICMP_ECHO_REPLY)` plus the payload), and the reply is read
    // back with an unaligned read of the first reply header only.
    let replies = unsafe {
        IcmpSendEcho(
            handle,
            destination,
            PAYLOAD.as_ptr().cast(),
            PAYLOAD.len() as u16,
            None,
            buffer.as_mut_ptr().cast(),
            reply_buffer_size,
            PROBE_TIMEOUT_MS,
        )
    };

    let answered = if replies == 0 {
        false
    } else {
        let reply = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<ICMP_ECHO_REPLY>()) };
        // `reply.Status == 0` is IP_STATUS_SUCCESS.
        reply.Status == 0
    };

    // SAFETY: `handle` was produced by `IcmpCreateFile` above and is closed
    // exactly once, after its only use.
    let _ = unsafe { IcmpCloseHandle(handle) };
    Ok(answered)
}

// ---------------------------------------------------------------------------
// Result contract (reserved for continuous PC Monitoring later)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostReachabilityState {
    Reachable,
    Unreachable,
    Unavailable,
}

/// Why no truthful Host Reachability verdict exists. Typed now so later Guard
/// coverage and continuous-monitoring work never changes the response shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostUnavailabilityReason {
    /// Non-Windows Admin cannot send probes.
    PlatformUnsupported,
    /// Hidden PCs are not monitored.
    PcHidden,
    /// No server-owned registered address to probe.
    RegisteredIpMissing,
    /// The stored registered address is not a valid IPv4 target.
    RegisteredIpInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostReachabilityFact {
    pub state: HostReachabilityState,
    /// Present only when `state` is `unavailable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<HostUnavailabilityReason>,
}

impl HostReachabilityFact {
    fn unavailable(reason: HostUnavailabilityReason) -> Self {
        Self {
            state: HostReachabilityState::Unavailable,
            reason: Some(reason),
        }
    }
}

/// Server's heartbeat verdict for the PC, mirroring what the dashboard shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientReachability {
    Reachable,
    Unreachable,
}

/// The last Client Guard Coverage diagnostic a Heartbeat reported for the PC
/// (ADR-0076 / #344). `covered` and `unavailable` come from a release Windows
/// Client's real held pipe connection; debug and non-Windows Clients report
/// `not_applicable`; a Client that never reported (older build) reads as
/// `unsupported` — unknown, and never a false Guard Alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardCoverageFact {
    Covered,
    Unavailable,
    NotApplicable,
    Unsupported,
}

impl GuardCoverageFact {
    /// Map one stored `pcs.last_guard_coverage` value. Only Admin-validated
    /// values can be stored; anything else degrades to unsupported rather
    /// than manufacturing a verdict. The wire spellings come from the one
    /// [`GuardCoverageReport`] mapping, so storage and display cannot drift.
    fn from_reported(value: Option<&str>) -> Self {
        match value.and_then(GuardCoverageReport::parse) {
            Some(GuardCoverageReport::Covered) => Self::Covered,
            Some(GuardCoverageReport::Unavailable) => Self::Unavailable,
            Some(GuardCoverageReport::NotApplicable) => Self::NotApplicable,
            None => Self::Unsupported,
        }
    }
}

/// One PC's non-mutating Check-now result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PcMonitoringCheck {
    pub pc_id: String,
    pub pc_name: String,
    pub host_reachability: HostReachabilityFact,
    pub client_reachability: ClientReachability,
    pub guard_coverage: GuardCoverageFact,
}

// ---------------------------------------------------------------------------
// Service entry points
// ---------------------------------------------------------------------------

/// Check one PC by exact id. Unknown ids are 404s; hidden or addressless PCs
/// come back explicitly unavailable, never healthy. The blocking probe runs
/// off the async workers — it can wait the full one-second timeout.
pub async fn check_pc<P>(
    pool: &SqlitePool,
    probe: &Arc<P>,
    pc_id: &str,
) -> AppResult<PcMonitoringCheck>
where
    P: HostProbe + ?Sized + 'static,
{
    let row = db::pcs::pc_monitoring_row(pool, pc_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;
    let probe = probe.clone();
    tokio::task::spawn_blocking(move || evaluate(&probe, &row))
        .await
        .map_err(|error| AppError::Io(std::io::Error::other(error.to_string())))
}

/// Check every eligible PC (visible with a registered address). One result
/// per eligible PC; hidden, removed, and addressless rows are excluded. The
/// whole probe loop runs off the async workers — each unreachable PC can
/// block for the full one-second timeout.
pub async fn check_all<P>(pool: &SqlitePool, probe: &Arc<P>) -> AppResult<Vec<PcMonitoringCheck>>
where
    P: HostProbe + ?Sized + 'static,
{
    let rows = db::pcs::eligible_pc_monitoring_rows(pool).await?;
    let probe = probe.clone();
    tokio::task::spawn_blocking(move || {
        rows.iter()
            .map(|row| evaluate(&probe, row))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|error| AppError::Io(std::io::Error::other(error.to_string())))
}

/// Map a probed outcome onto one row's facts. Pure and synchronous so the
/// blocking probe loop can run inside `spawn_blocking`.
fn evaluate<P>(probe: &Arc<P>, row: &db::pcs::PcMonitoringRow) -> PcMonitoringCheck
where
    P: HostProbe + ?Sized,
{
    let host_reachability = match (&row.visible, &row.registered_ip) {
        (false, _) => HostReachabilityFact::unavailable(HostUnavailabilityReason::PcHidden),
        (true, None) => {
            HostReachabilityFact::unavailable(HostUnavailabilityReason::RegisteredIpMissing)
        }
        (true, Some(registered_ip)) => match probe.probe(registered_ip) {
            HostProbeOutcome::Reply => HostReachabilityFact {
                state: HostReachabilityState::Reachable,
                reason: None,
            },
            HostProbeOutcome::Timeout => HostReachabilityFact {
                state: HostReachabilityState::Unreachable,
                reason: None,
            },
            HostProbeOutcome::InvalidTarget => {
                HostReachabilityFact::unavailable(HostUnavailabilityReason::RegisteredIpInvalid)
            }
            HostProbeOutcome::Unsupported => {
                HostReachabilityFact::unavailable(HostUnavailabilityReason::PlatformUnsupported)
            }
        },
    };

    PcMonitoringCheck {
        pc_id: row.id.clone(),
        pc_name: row.name.clone(),
        host_reachability,
        client_reachability: if row.client_reachable {
            ClientReachability::Reachable
        } else {
            ClientReachability::Unreachable
        },
        guard_coverage: GuardCoverageFact::from_reported(row.guard_coverage.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// Continuous monitoring loop (ADR-0076 / GitHub #341)
// ---------------------------------------------------------------------------

/// Whether `(pc_id, kind)` currently has an open durable episode.
fn is_open(
    open_kinds: &HashMap<String, HashSet<IncidentKind>>,
    pc_id: &str,
    kind: IncidentKind,
) -> bool {
    open_kinds
        .get(pc_id)
        .is_some_and(|kinds| kinds.contains(&kind))
}

/// Take one kind out of a PC's open-episode set once it closes this round;
/// reports whether an open episode existed so each closure runs exactly once.
fn take_open(
    open_kinds: &mut HashMap<String, HashSet<IncidentKind>>,
    pc_id: &str,
    kind: IncidentKind,
) -> bool {
    open_kinds
        .get_mut(pc_id)
        .is_some_and(|kinds| kinds.remove(&kind))
}

/// Close `(pc_id, kind)` exactly once with one reason: removes it from the
/// open-episode set and commits the durable closure plus its Notice audit
/// event. A no-op when no episode was open.
async fn close_once(
    open_kinds: &mut HashMap<String, HashSet<IncidentKind>>,
    pool: &SqlitePool,
    pc_id: &str,
    pc_name: &str,
    kind: IncidentKind,
    reason: db::monitoring::IncidentCloseReason,
) -> AppResult<()> {
    if take_open(open_kinds, pc_id, kind) {
        db::monitoring::close_incident_with_audit(pool, pc_id, pc_name, kind, reason).await?;
    }
    Ok(())
}

/// Fixed v1 probe cadence: one round every ten seconds, on its own task —
/// never inside the one-second money and time timer.
pub const MONITOR_INTERVAL_SECS: u64 = 10;

/// Fixed v1 mismatch grace: Host Reachability and missing Client Reachability
/// must coexist continuously this long before an incident opens.
pub const MISMATCH_GRACE_SECS: i64 = 60;

/// Fixed v1 coverage grace (ADR-0076): while the Client heartbeat proves a PC
/// is running, every ICMP probe must fail continuously this long before Host
/// Probe Coverage loss opens. A powered-off or Client-unreachable PC never
/// accumulates it.
pub const COVERAGE_GRACE_SECS: i64 = 120;

/// Fixed v1 maximum Monitoring Suppression duration (ADR-0076): a forgotten
/// maintenance window can never disable detection beyond one hour.
pub const MAX_SUPPRESSION_MINUTES: i64 = 60;

/// Clock seam for grace math. Production reads the system clock; tests inject
/// a mutable one so grace boundaries are deterministic.
pub trait MonitorClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock.
pub struct SystemClock;

impl MonitorClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// The continuous PC Monitoring loop. Owns only in-memory grace tracking;
/// every durable fact lives in `pc_monitoring_incidents` and the Audit Log
/// behind [`crate::db::monitoring`]. Incident paths never touch PC status,
/// sessions, money, Remaining Time, or Points.
pub struct MonitoringService {
    pool: SqlitePool,
    probe: Arc<dyn HostProbe>,
    clock: Arc<dyn MonitorClock>,
    /// Continuous degraded-condition start per (PC id, kind). Cleared whenever
    /// that kind's condition breaks, monitoring is disabled, or the process
    /// starts: a fresh map IS the full-grace startup debounce for every kind.
    grace_since: HashMap<(String, IncidentKind), DateTime<Utc>>,
    /// Continuous escalation-condition start per PC (ADR-0077). A second clock
    /// beside `grace_since`, on its own stricter condition, and in memory for
    /// the same reason: a restart must restart it rather than resume it.
    escalation_since: HashMap<String, DateTime<Utc>>,
    /// Present only when this Admin can actually act on an escalation. Absent
    /// in the read-only monitoring tests, which is why every escalation path
    /// below is skipped rather than faked when it is `None`.
    escalation: Option<EscalationDeps>,
}

/// What the escalation path needs beyond monitoring's own state: the session
/// authority that ends the bound session, the token store that revokes it, and
/// the registry that carries the order to the Guard.
#[derive(Clone)]
pub struct EscalationDeps {
    pub sessions: SessionStore,
    pub tokens: AccountTokens,
    pub guards: GuardRegistry,
}

impl MonitoringService {
    pub fn new(pool: SqlitePool, probe: Arc<dyn HostProbe>, clock: Arc<dyn MonitorClock>) -> Self {
        Self {
            pool,
            probe,
            clock,
            grace_since: HashMap::new(),
            escalation_since: HashMap::new(),
            escalation: None,
        }
    }

    /// Give this monitor the authority to act on Client Recovery Escalation
    /// (ADR-0077). Without it the monitor stays exactly what ADR-0076 built:
    /// detect and report only.
    pub fn with_escalation(mut self, deps: EscalationDeps) -> Self {
        self.escalation = Some(deps);
        self
    }

    /// Spawn the production loop on its own task: startup marking of
    /// downtime-spanning incidents, then one probe round per interval. A slow
    /// round delays the next round instead of bursting catch-up ticks.
    pub fn spawn(mut self) {
        tauri::async_runtime::spawn(async move {
            if let Err(error) = db::monitoring::mark_observation_interrupted(&self.pool).await {
                error!(
                    target: "openpiso_admin_lib::monitoring",
                    "startup incident marking failed: {error}"
                );
            }
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(MONITOR_INTERVAL_SECS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = self.tick().await {
                    error!(
                        target: "openpiso_admin_lib::monitoring",
                        "PC Monitoring tick failed: {error}"
                    );
                }
            }
        });
    }

    /// One probe round. Public so tests drive rounds against an injected
    /// clock with no real time passing.
    pub async fn tick(&mut self) -> AppResult<()> {
        let enabled = db::monitoring::enabled(&self.pool).await?;
        if !enabled || !self.probe.supported() {
            // Disabled (or impossible) monitoring holds no grace state, so
            // enabling later starts every episode's full grace from zero.
            self.grace_since.clear();
            return Ok(());
        }

        let rows = db::pcs::eligible_pc_monitoring_rows(&self.pool).await?;
        let checks = self.probe_rows(rows).await?;
        // Every open episode, any kind: drives per-kind idempotence and the
        // eligible-set cleanup below.
        let mut open_kinds: HashMap<String, HashSet<IncidentKind>> = HashMap::new();
        for incident in db::monitoring::open_incidents(&self.pool).await? {
            open_kinds
                .entry(incident.pc_id)
                .or_default()
                .insert(incident.kind);
        }
        // Monitoring Suppression freezes incident lifecycle for the PC. It
        // drops opening grace and prevents both new incidents and closures
        // until expiry, so maintenance evidence cannot rewrite an existing
        // episode.
        let suppressed_pcs =
            db::monitoring::active_suppression_pcs(&self.pool, self.clock.now()).await?;
        // Client Recovery Escalation (ADR-0077) reads its own policy and its
        // own durable cooldown. Both are read once per round so every PC in
        // this tick is judged against one consistent snapshot.
        let escalation_settings = db::monitoring::escalation_settings(&self.pool).await?;
        let cooling_down = if escalation_settings.enabled && self.escalation.is_some() {
            db::monitoring::cooling_down_pcs(&self.pool, self.clock.now()).await?
        } else {
            HashSet::new()
        };

        for check in &checks {
            let pc_id = &check.pc_id;
            if suppressed_pcs.contains(pc_id) {
                for kind in [
                    IncidentKind::ClientReachabilityMismatch,
                    IncidentKind::HostProbeCoverage,
                    IncidentKind::ClientGuardCoverage,
                ] {
                    self.clear_grace(pc_id, kind);
                }
                // Suppression is the owner's own maintenance window, so it
                // stops the escalation clock exactly as it stops every grace.
                self.escalation_since.remove(pc_id);
                continue;
            }
            let host_reachable = check.host_reachability.state == HostReachabilityState::Reachable;
            let client_reachable = check.client_reachability == ClientReachability::Reachable;

            // --- Client Reachability Mismatch (#341): host answers, Client
            // does not. One accepted heartbeat closes it as recovery; lost
            // host evidence closes it as unconfirmed.
            if client_reachable {
                self.clear_grace(pc_id, IncidentKind::ClientReachabilityMismatch);
                close_once(
                    &mut open_kinds,
                    &self.pool,
                    pc_id,
                    &check.pc_name,
                    IncidentKind::ClientReachabilityMismatch,
                    db::monitoring::IncidentCloseReason::ClientRecovered,
                )
                .await?;
            } else if host_reachable {
                self.track_or_open(
                    check,
                    IncidentKind::ClientReachabilityMismatch,
                    is_open(&open_kinds, pc_id, IncidentKind::ClientReachabilityMismatch),
                    suppressed_pcs.contains(pc_id),
                )
                .await?;
            } else {
                // Host evidence gone (timeout, invalid target): the grace
                // window resets, and any open episode closes truthfully as
                // unconfirmed rather than recovered.
                self.clear_grace(pc_id, IncidentKind::ClientReachabilityMismatch);
                close_once(
                    &mut open_kinds,
                    &self.pool,
                    pc_id,
                    &check.pc_name,
                    IncidentKind::ClientReachabilityMismatch,
                    db::monitoring::IncidentCloseReason::HostLost,
                )
                .await?;
            }

            // --- Host Probe Coverage (#343): the mirror image. Failed probes
            // mean something only while the heartbeat independently proves
            // the host is running — a powered-off or Client-unreachable PC
            // never opens this incident merely by not answering ICMP.
            if host_reachable {
                self.clear_grace(pc_id, IncidentKind::HostProbeCoverage);
                // One successful response ends a coverage episode as a
                // recovery Notice, whatever the heartbeat now says.
                close_once(
                    &mut open_kinds,
                    &self.pool,
                    pc_id,
                    &check.pc_name,
                    IncidentKind::HostProbeCoverage,
                    db::monitoring::IncidentCloseReason::ProbeRecovered,
                )
                .await?;
            } else if client_reachable {
                self.track_or_open(
                    check,
                    IncidentKind::HostProbeCoverage,
                    is_open(&open_kinds, pc_id, IncidentKind::HostProbeCoverage),
                    suppressed_pcs.contains(pc_id),
                )
                .await?;
            } else {
                // No heartbeat proving the host runs: failed probes say
                // nothing about coverage, so any open episode closes as
                // unconfirmed instead of lingering without its premise.
                self.clear_grace(pc_id, IncidentKind::HostProbeCoverage);
                close_once(
                    &mut open_kinds,
                    &self.pool,
                    pc_id,
                    &check.pc_name,
                    IncidentKind::HostProbeCoverage,
                    db::monitoring::IncidentCloseReason::HostLost,
                )
                .await?;
            }

            // --- Client Guard Coverage (#344): a heartbeat-reported
            // diagnostic, never authority. Only a release Windows Client's own
            // `unavailable` report degrades coverage; `not_applicable` (debug,
            // non-Windows) and `unsupported` (older Client that omits the
            // field) carry no evidence either way and can never open one.
            if !client_reachable {
                // Without the heartbeat proving the PC runs there is no
                // coverage evidence at all; an open episode closes truthfully
                // as unconfirmed instead of lingering without its premise.
                self.clear_grace(pc_id, IncidentKind::ClientGuardCoverage);
                close_once(
                    &mut open_kinds,
                    &self.pool,
                    pc_id,
                    &check.pc_name,
                    IncidentKind::ClientGuardCoverage,
                    db::monitoring::IncidentCloseReason::HostLost,
                )
                .await?;
            } else if check.guard_coverage == GuardCoverageFact::Covered {
                // Positive evidence from the same diagnostic that opened the
                // episode closes it as restored coverage.
                self.clear_grace(pc_id, IncidentKind::ClientGuardCoverage);
                close_once(
                    &mut open_kinds,
                    &self.pool,
                    pc_id,
                    &check.pc_name,
                    IncidentKind::ClientGuardCoverage,
                    db::monitoring::IncidentCloseReason::GuardRecovered,
                )
                .await?;
            } else if check.guard_coverage == GuardCoverageFact::Unavailable {
                self.track_or_open(
                    check,
                    IncidentKind::ClientGuardCoverage,
                    is_open(&open_kinds, pc_id, IncidentKind::ClientGuardCoverage),
                    suppressed_pcs.contains(pc_id),
                )
                .await?;
            } else {
                // Not applicable or not yet reported: no usable evidence.
                // Grace never accumulates; an episode opened by earlier real
                // reports stays open until covered returns or the heartbeat
                // premise ends — nothing here fabricates recovery.
                self.clear_grace(pc_id, IncidentKind::ClientGuardCoverage);
            }

            // --- Client Recovery Escalation (ADR-0077): the only path in this
            // loop that changes anything outside the incident record.
            self.evaluate_escalation(
                check,
                host_reachable,
                client_reachable,
                suppressed_pcs.contains(pc_id),
                escalation_settings,
                &cooling_down,
            )
            .await?;
        }

        // An open episode of any kind whose PC left the eligible set (hidden,
        // address removed, PC removed) has lost its observable target: close
        // it as loss instead of letting it linger open forever.
        for (pc_id, kinds) in open_kinds {
            if suppressed_pcs.contains(&pc_id) || checks.iter().any(|c| c.pc_id == pc_id) {
                continue;
            }
            let name = match db::pc(&self.pool, &pc_id).await {
                Ok(pc) => pc.name,
                Err(_) => pc_id.clone(),
            };
            for kind in kinds {
                db::monitoring::close_incident_with_audit(
                    &self.pool,
                    &pc_id,
                    &name,
                    kind,
                    db::monitoring::IncidentCloseReason::HostLost,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Run one PC's escalation clock for this tick.
    ///
    /// Every early return here is a decision *not* to power a PC off, and each
    /// one clears the clock rather than pausing it: the delay measures an
    /// uninterrupted failure, so a moment of recovery must cost the full wait
    /// again.
    async fn evaluate_escalation(
        &mut self,
        check: &PcMonitoringCheck,
        host_reachable: bool,
        client_reachable: bool,
        suppressed: bool,
        settings: db::monitoring::EscalationSettings,
        cooling_down: &HashSet<String>,
    ) -> AppResult<()> {
        let Some(deps) = self.escalation.clone() else {
            return Ok(());
        };
        let pc_id = &check.pc_id;
        // A Client that came back withdraws any standing order at once, even
        // while escalation is disabled: an order outliving its reason is the
        // one way this feature could power off a healthy PC.
        if client_reachable {
            deps.guards.clear_power_off(pc_id);
        }
        if !settings.enabled {
            self.escalation_since.remove(pc_id);
            return Ok(());
        }
        let now = self.clock.now();
        let inputs = EscalationInputs {
            host_reachable,
            client_reachable,
            suppressed,
            guard: deps.guards.facts(pc_id),
        };
        if !escalation_condition_holds(&inputs, now) {
            self.escalation_since.remove(pc_id);
            return Ok(());
        }
        // Already ordered, or powered off within the cooldown. Both mean the
        // decision has been made; a fresh episode must earn the full delay
        // again rather than inherit this one.
        if deps.guards.is_armed(pc_id) || cooling_down.contains(pc_id) {
            self.escalation_since.remove(pc_id);
            return Ok(());
        }
        let since = *self.escalation_since.entry(pc_id.clone()).or_insert(now);
        if now.signed_duration_since(since).num_seconds() < settings.delay_secs {
            return Ok(());
        }

        let relaunch_attempts = inputs
            .guard
            .as_ref()
            .map_or(0, |guard| guard.relaunch_attempts);
        let nonce = Uuid::new_v4().to_string();
        match deps
            .sessions
            .escalated_power_off(
                &self.pool,
                &deps.tokens,
                crate::sessions::EscalatedPowerOff {
                    pc_id,
                    pc_name: &check.pc_name,
                    relaunch_attempts,
                    nonce: &nonce,
                    now,
                },
            )
            .await
        {
            // The record commits first, then the order is armed. A Guard can
            // therefore never power a PC off without an Alert already written.
            Ok(()) => {
                deps.guards.arm_power_off(pc_id, nonce, now);
                self.escalation_since.remove(pc_id);
            }
            // A transient failure must not reset the observed continuity, and
            // must not arm an order the Audit Log does not explain.
            Err(error) => {
                tracing::warn!(
                    target: "openpiso_admin_lib::monitoring",
                    pc_id = %pc_id,
                    "could not record Escalated Power Off, retrying next tick: {error}"
                );
            }
        }
        Ok(())
    }

    /// Drop one kind's in-memory continuity for a PC.
    fn clear_grace(&mut self, pc_id: &str, kind: IncidentKind) {
        self.grace_since.remove(&(pc_id.to_owned(), kind));
    }

    /// Fixed opening grace per kind. Coverage thresholds are fixed v1 policy
    /// (ADR-0076), never owner settings.
    fn grace_secs(kind: IncidentKind) -> i64 {
        match kind {
            IncidentKind::ClientReachabilityMismatch => MISMATCH_GRACE_SECS,
            IncidentKind::HostProbeCoverage | IncidentKind::ClientGuardCoverage => {
                COVERAGE_GRACE_SECS
            }
        }
    }

    /// Track a continuously observed degraded condition and open its incident
    /// once the full grace elapses. Repeated ticks stay idempotent. This is
    /// the single opening path every incident kind uses, so the Monitoring
    /// Suppression check here covers all of them.
    async fn track_or_open(
        &mut self,
        check: &PcMonitoringCheck,
        kind: IncidentKind,
        already_open: bool,
        suppressed: bool,
    ) -> AppResult<()> {
        let key = (check.pc_id.clone(), kind);
        // While suppressed, no grace accumulates for any kind: dropping the
        // entry makes expiry restart every full grace period from zero instead
        // of opening an immediate Alert. An already-open episode is untouched.
        if suppressed || already_open {
            self.grace_since.remove(&key);
            return Ok(());
        }
        let now = self.clock.now();
        let since = *self.grace_since.entry(key.clone()).or_insert(now);
        if now.signed_duration_since(since).num_seconds() < Self::grace_secs(kind) {
            return Ok(());
        }
        match db::monitoring::open_incident_with_audit(
            &self.pool,
            &check.pc_id,
            &check.pc_name,
            kind,
        )
        .await
        {
            // Opened (or raced open elsewhere): the durable episode owns the
            // state now, so the in-memory tracker stands down.
            Ok(_) => {
                self.grace_since.remove(&key);
            }
            // A transient failure must not reset the observed continuity:
            // retry on the next tick from the same condition start.
            Err(error) => {
                tracing::warn!(
                    target: "openpiso_admin_lib::monitoring",
                    pc_id = %check.pc_id,
                    kind = %kind.as_str(),
                    "could not open monitoring incident, retrying next tick: {error}"
                );
            }
        }
        Ok(())
    }

    /// Probe every eligible row off the async workers — each unreachable host
    /// can block for the full one-second timeout.
    async fn probe_rows(
        &self,
        rows: Vec<db::pcs::PcMonitoringRow>,
    ) -> AppResult<Vec<PcMonitoringCheck>> {
        let probe = self.probe.clone();
        tokio::task::spawn_blocking(move || {
            rows.iter()
                .map(|row| evaluate(&probe, row))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| AppError::Io(std::io::Error::other(error.to_string())))
    }
}
/// Deterministic probe double shared by the unit tests here and the
/// owner-HTTP integration tests in `web_admin`.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{HostProbe, HostProbeOutcome};

    /// Fake keyed by registered address; records every probed address so
    /// tests can prove exactly which targets were used. Defaults to a
    /// supported platform; `set_unsupported` exercises the Windows-only gate.
    pub struct FakeHostProbe {
        outcomes: Mutex<HashMap<String, HostProbeOutcome>>,
        calls: Mutex<Vec<String>>,
        supported: bool,
    }

    impl Default for FakeHostProbe {
        fn default() -> Self {
            Self {
                outcomes: Mutex::new(HashMap::new()),
                calls: Mutex::new(Vec::new()),
                supported: true,
            }
        }
    }

    impl FakeHostProbe {
        /// Script an outcome for one address; unscripted addresses time out.
        pub fn script(&self, address: &str, outcome: HostProbeOutcome) -> Result<(), String> {
            match self.outcomes.lock() {
                Ok(mut outcomes) => {
                    outcomes.insert(address.to_owned(), outcome);
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            }
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("call log").clone()
        }

        /// Mark this fake as an unsupported platform (non-Windows Admin).
        pub fn set_supported(&mut self, supported: bool) {
            self.supported = supported;
        }
    }
    impl HostProbe for FakeHostProbe {
        fn probe(&self, registered_ip: &str) -> HostProbeOutcome {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(registered_ip.to_owned());
            }
            match self.outcomes.lock() {
                Ok(outcomes) => outcomes
                    .get(registered_ip)
                    .copied()
                    .unwrap_or(HostProbeOutcome::Timeout),
                Err(_) => HostProbeOutcome::Timeout,
            }
        }

        fn supported(&self) -> bool {
            self.supported
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::testing::FakeHostProbe;
    use super::*;

    fn probe_of() -> Arc<FakeHostProbe> {
        Arc::new(FakeHostProbe::default())
    }

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    async fn register(pool: &SqlitePool, pc_id: &str, ip: Option<&str>) -> AppResult<()> {
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: ip.map(str::to_owned),
                mac_address: None,
            },
        )
        .await?;
        Ok(())
    }

    async fn hide(pool: &SqlitePool, pc_id: &str) -> AppResult<()> {
        let settings = crate::db::pc_settings(pool)
            .await?
            .into_iter()
            .find(|settings| settings.id == pc_id)
            .expect("seeded PC");
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: settings.name,
                visible: false,
                registered_ip: settings.registered_ip,
                mac_address: None,
            },
        )
        .await?;
        Ok(())
    }

    fn reachable_fact() -> HostReachabilityFact {
        HostReachabilityFact {
            state: HostReachabilityState::Reachable,
            reason: None,
        }
    }

    fn unavailable_fact(reason: HostUnavailabilityReason) -> HostReachabilityFact {
        HostReachabilityFact {
            state: HostReachabilityState::Unavailable,
            reason: Some(reason),
        }
    }

    #[tokio::test]
    async fn reply_probe_reads_host_reachable_with_fresh_heartbeat() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("192.168.4.1")).await?;
        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        let probe = probe_of();
        probe
            .script("192.168.4.1", HostProbeOutcome::Reply)
            .expect("script");

        let check = check_pc(&pool, &probe, "pc-01").await?;

        assert_eq!(check.pc_id, "pc-01");
        assert_eq!(check.host_reachability, reachable_fact());
        assert_eq!(check.client_reachability, ClientReachability::Reachable);
        // No Heartbeat has reported a Guard diagnostic yet: unknown, never
        // fabricated (ADR-0076 / #344).
        assert_eq!(check.guard_coverage, GuardCoverageFact::Unsupported);
        assert_eq!(probe.calls(), vec!["192.168.4.1".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn check_reads_the_last_reported_guard_diagnostic() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("192.168.4.1")).await?;
        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        let probe = probe_of();

        sqlx::query("UPDATE pcs SET last_guard_coverage = 'unavailable' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let check = check_pc(&pool, &probe, "pc-01").await?;
        assert_eq!(check.guard_coverage, GuardCoverageFact::Unavailable);

        // Debug and non-Windows Clients report the check as not applicable.
        sqlx::query("UPDATE pcs SET last_guard_coverage = 'not_applicable' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let check = check_pc(&pool, &probe, "pc-01").await?;
        assert_eq!(check.guard_coverage, GuardCoverageFact::NotApplicable);

        // Only Admin-validated values can be stored; anything unexpected is
        // unsupported rather than manufactured coverage.
        sqlx::query("UPDATE pcs SET last_guard_coverage = 'nonsense' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let check = check_pc(&pool, &probe, "pc-01").await?;
        assert_eq!(check.guard_coverage, GuardCoverageFact::Unsupported);
        Ok(())
    }

    #[tokio::test]
    async fn timeout_probe_reads_host_unreachable_not_unavailable() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("192.168.4.1")).await?;
        // No fake outcome registered → the default is Timeout.

        let check = check_pc(&pool, &probe_of(), "pc-01").await?;

        assert_eq!(
            check.host_reachability.state,
            HostReachabilityState::Unreachable
        );
        assert_eq!(check.host_reachability.reason, None);
        Ok(())
    }

    #[tokio::test]
    async fn stale_heartbeat_reads_client_unreachable_while_host_answers() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("192.168.4.1")).await?;
        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        sqlx::query(
            "UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = 'pc-01'",
        )
        .execute(&pool)
        .await?;
        let probe = probe_of();
        probe
            .script("192.168.4.1", HostProbeOutcome::Reply)
            .expect("script");

        let check = check_pc(&pool, &probe, "pc-01").await?;

        assert_eq!(check.host_reachability, reachable_fact());
        assert_eq!(check.client_reachability, ClientReachability::Unreachable);
        Ok(())
    }

    #[tokio::test]
    async fn hidden_pc_is_explicitly_unavailable_and_never_probed() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("192.168.4.1")).await?;
        hide(&pool, "pc-01").await?;
        let probe = probe_of();
        probe
            .script("192.168.4.1", HostProbeOutcome::Reply)
            .expect("script");

        let check = check_pc(&pool, &probe, "pc-01").await?;

        assert_eq!(
            check.host_reachability,
            unavailable_fact(HostUnavailabilityReason::PcHidden)
        );
        assert_eq!(check.client_reachability, ClientReachability::Unreachable);
        assert!(probe.calls().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn addressless_pc_is_explicitly_unavailable() -> AppResult<()> {
        let pool = test_pool().await?;
        // Seeded PCs have no registered_ip.

        let check = check_pc(&pool, &probe_of(), "pc-02").await?;

        assert_eq!(
            check.host_reachability,
            unavailable_fact(HostUnavailabilityReason::RegisteredIpMissing)
        );
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_platform_reports_unavailable_never_reachable() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("192.168.4.1")).await?;

        let check = check_pc(&pool, &Arc::new(UnsupportedPlatformProbe), "pc-01").await?;

        assert_eq!(
            check.host_reachability,
            unavailable_fact(HostUnavailabilityReason::PlatformUnsupported)
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_registered_ip_reads_unavailable_not_unreachable() -> AppResult<()> {
        struct MalformedTargetProbe;
        impl HostProbe for MalformedTargetProbe {
            fn probe(&self, _registered_ip: &str) -> HostProbeOutcome {
                HostProbeOutcome::InvalidTarget
            }

            fn supported(&self) -> bool {
                true
            }
        }
        let pool = test_pool().await?;
        register(&pool, "pc-01", Some("not-an-ip")).await?;

        let check = check_pc(&pool, &Arc::new(MalformedTargetProbe), "pc-01").await?;

        assert_eq!(
            check.host_reachability,
            unavailable_fact(HostUnavailabilityReason::RegisteredIpInvalid)
        );
        Ok(())
    }

    /// End-to-end guard for the adapter's byte order: if the destination
    /// were packed with `from_be_bytes`, little-endian Windows would reverse
    /// the octets and 127.0.0.1 could never answer.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_adapter_reaches_loopback() {
        let probe = WindowsIcmpProbe;
        assert_eq!(probe.probe("127.0.0.1"), HostProbeOutcome::Reply);
        assert_eq!(probe.probe("not-an-ip"), HostProbeOutcome::InvalidTarget);
    }

    #[tokio::test]
    async fn unknown_pc_is_not_found() -> AppResult<()> {
        let pool = test_pool().await?;
        let result = check_pc(&pool, &probe_of(), "pc-99").await;
        assert!(matches!(result, Err(AppError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn blank_pc_id_is_invalid_input() -> AppResult<()> {
        let pool = test_pool().await?;
        let result = check_pc(&pool, &probe_of(), "   ").await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        Ok(())
    }

    #[tokio::test]
    async fn check_all_returns_one_result_per_eligible_pc_only() -> AppResult<()> {
        let pool = test_pool().await?;
        register(&pool, "pc-02", Some("192.168.4.102")).await?;
        register(&pool, "pc-01", Some("192.168.4.101")).await?;
        register(&pool, "pc-03", Some("192.168.4.103")).await?;
        hide(&pool, "pc-03").await?; // hidden registered PC
                                     // pc-04 stays visible but addressless.
        let probe = probe_of();
        probe
            .script("192.168.4.101", HostProbeOutcome::Reply)
            .expect("script");

        let checks = check_all(&pool, &probe).await?;

        let ids: Vec<&str> = checks.iter().map(|check| check.pc_id.as_str()).collect();
        assert_eq!(ids, vec!["pc-01", "pc-02"]);
        // Exactly the server-owned addresses of eligible PCs were probed, in
        // display order; hidden pc-03 and addressless pc-04 were skipped.
        assert_eq!(
            probe.calls(),
            vec!["192.168.4.101".to_owned(), "192.168.4.102".to_owned()]
        );
        assert_eq!(checks[0].host_reachability, reachable_fact());
        assert_eq!(
            checks[1].host_reachability.state,
            HostReachabilityState::Unreachable
        );
        Ok(())
    }

    #[tokio::test]
    async fn result_serializes_guard_contract_shape() {
        let check = PcMonitoringCheck {
            pc_id: "pc-01".to_owned(),
            pc_name: "PC 01".to_owned(),
            host_reachability: reachable_fact(),
            client_reachability: ClientReachability::Reachable,
            guard_coverage: GuardCoverageFact::Covered,
        };
        let json = serde_json::to_value(&check).expect("serializable");

        assert_eq!(
            json,
            serde_json::json!({
                "pc_id": "pc-01",
                "pc_name": "PC 01",
                "host_reachability": { "state": "reachable" },
                "client_reachability": "reachable",
                "guard_coverage": "covered"
            })
        );

        let unavailable = PcMonitoringCheck {
            host_reachability: unavailable_fact(HostUnavailabilityReason::PlatformUnsupported),
            ..check
        };
        let json = serde_json::to_value(&unavailable).expect("serializable");
        assert_eq!(
            json["host_reachability"],
            serde_json::json!({ "state": "unavailable", "reason": "platform_unsupported" })
        );
    }
}

/// Test support shared by the monitor-loop test modules below: a hand-moved
/// clock so grace boundaries are exact, plus the pool/settings/audit helpers
/// every loop test starts from.
#[cfg(test)]
mod support {
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Mutex;

    use super::*;
    use crate::db::monitoring::SETTINGS_KEY_ENABLED;

    /// Clock the tests move by hand so grace boundaries are exact.
    pub(super) struct MutableClock {
        now: Mutex<DateTime<Utc>>,
    }

    impl MutableClock {
        pub(super) fn starting_at(seconds: i64) -> Self {
            Self {
                now: Mutex::new(DateTime::<Utc>::from_timestamp(seconds, 0).expect("epoch")),
            }
        }

        pub(super) fn advance_seconds(&self, seconds: i64) {
            *self.now.lock().expect("clock") += chrono::Duration::seconds(seconds);
        }
    }

    impl MonitorClock for MutableClock {
        fn now(&self) -> DateTime<Utc> {
            *self.now.lock().expect("clock")
        }
    }

    pub(super) async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    pub(super) async fn set_enabled(pool: &SqlitePool, value: bool) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTINGS_KEY_ENABLED)
        .bind(if value { "true" } else { "false" })
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Register `pc_id` as visible with its own address, script the probe to
    /// answer it, then age the heartbeat out so the Client reads unreachable.
    pub(super) async fn register_mismatched_pc(
        pool: &SqlitePool,
        probe: &super::testing::FakeHostProbe,
        pc_id: &str,
    ) -> AppResult<()> {
        let octet: u8 = pc_id
            .split('-')
            .next_back()
            .and_then(|suffix| suffix.parse().ok())
            .unwrap_or(1);
        let ip = format!("192.168.9.{octet}");
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: Some(ip.clone()),
                mac_address: None,
            },
        )
        .await?;
        crate::db::heartbeat_by_identity_only(pool, pc_id).await?;
        probe
            .script(&ip, HostProbeOutcome::Reply)
            .expect("script reply");
        sqlx::query("UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = ?")
            .bind(pc_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub(super) async fn open_incident_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pc_monitoring_incidents WHERE closed_at IS NULL",
        )
        .fetch_one(pool)
        .await
        .expect("open count")
    }

    pub(super) async fn audit_count(pool: &SqlitePool, event_type: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE event_type = ?")
            .bind(event_type)
            .fetch_one(pool)
            .await
            .expect("audit count")
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::support::{
        audit_count, open_incident_count, register_mismatched_pc, set_enabled, test_pool,
        MutableClock,
    };
    use super::testing::FakeHostProbe;
    use super::*;

    #[test]
    fn spawn_does_not_require_caller_tokio_runtime() {
        let pool = tauri::async_runtime::block_on(async {
            sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
        })
        .expect("pool");
        let service = MonitoringService::new(
            pool,
            Arc::new(FakeHostProbe::default()),
            Arc::new(MutableClock::starting_at(1_000)),
        );

        service.spawn();
    }

    #[tokio::test]
    async fn mismatch_opens_only_after_sixty_continuous_seconds() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(1_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        // Condition observed but far below the grace window.
        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS - 1);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 0);

        // One more second of continuity crosses the threshold.
        clock.advance_seconds(1);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn returning_client_resets_the_grace_window() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(2_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(30);
        service.tick().await?;

        // The Client reports again: continuity breaks, grace starts over.
        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        service.tick().await?;

        sqlx::query("UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = ?")
            .bind("pc-01")
            .execute(&pool)
            .await?;
        clock.advance_seconds(35);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);

        // Only after another full grace does the episode open.
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn host_loss_before_grace_closes_nothing_and_resets() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(3_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        service.tick().await?;
        clock.advance_seconds(30);
        service.tick().await?;

        // Host stops answering: no incident may exist before or after.
        let ip = "192.168.9.1";
        probe
            .script(ip, HostProbeOutcome::Timeout)
            .expect("script timeout");
        clock.advance_seconds(35);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 0);

        // And when the host answers again, the full grace restarts.
        probe
            .script(ip, HostProbeOutcome::Reply)
            .expect("script reply");
        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS - 5);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        clock.advance_seconds(5);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn accepted_heartbeat_closes_the_episode_as_recovered() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(4_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);

        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        service.tick().await?;

        let row: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT closed_at, close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert!(row.0.is_some());
        assert_eq!(row.1.as_deref(), Some("client_recovered"));
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn host_loss_after_opening_records_unconfirmed_recovery() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(5_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);

        probe
            .script("192.168.9.1", HostProbeOutcome::Timeout)
            .expect("script timeout");
        service.tick().await?;

        let reason: String = sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
            .fetch_one(&pool)
            .await?;
        assert_eq!(reason, "host_lost");
        Ok(())
    }

    #[tokio::test]
    async fn repeated_ticks_past_grace_produce_exactly_one_episode() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(6_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS * 3);
        for _ in 0..6 {
            service.tick().await?;
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn disabled_monitoring_never_opens_or_closes_anything() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(7_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        // Settings key absent entirely: upgraded-shop default state.
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        clock.advance_seconds(MISMATCH_GRACE_SECS * 2);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_platform_never_opens_an_episode() -> AppResult<()> {
        let pool = test_pool().await?;
        let clock = Arc::new(MutableClock::starting_at(8_000));
        let mut unsupported = FakeHostProbe::default();
        unsupported.set_supported(false);
        let probe = Arc::new(unsupported);
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        clock.advance_seconds(MISMATCH_GRACE_SECS * 2);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn admin_restart_marks_interrupted_without_duplicating_alerts() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(9_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut pre_restart = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        pre_restart.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        pre_restart.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        let alerts_before = audit_count(&pool, "pc_monitoring_incident_opened").await;

        // Admin comes back up: startup marks downtime-spanning incidents, and
        // the fresh in-memory state grants a full grace from zero.
        crate::db::monitoring::mark_observation_interrupted(&pool).await?;
        let mut post_restart = MonitoringService::new(pool.clone(), probe, clock.clone());

        clock.advance_seconds(MISMATCH_GRACE_SECS * 2);
        for _ in 0..4 {
            post_restart.tick().await?;
        }

        assert_eq!(
            audit_count(&pool, "pc_monitoring_incident_opened").await,
            alerts_before
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?,
            1
        );
        let interrupted: i64 =
            sqlx::query_scalar("SELECT observation_interrupted FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(interrupted, 1);
        Ok(())
    }

    #[tokio::test]
    async fn hidden_pc_with_open_episode_closes_as_host_lost() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);

        // The owner hides the PC: the observable target is gone.
        let settings = crate::db::pc_settings(&pool)
            .await?
            .into_iter()
            .find(|settings| settings.id == "pc-01")
            .expect("seeded PC");
        crate::db::update_pc_settings(
            &pool,
            "pc-01",
            &crate::models::PcSettingsInput {
                name: settings.name,
                visible: false,
                registered_ip: settings.registered_ip,
                mac_address: None,
            },
        )
        .await?;
        service.tick().await?;

        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason.as_deref(), Some("host_lost"));
        assert_eq!(open_incident_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn incident_paths_leave_pcs_sessions_money_time_points_untouched() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(11_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let remaining_before: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let status_before: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = 'pc-01'")
            .fetch_one(&pool)
            .await?;
        let sessions_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await?;
        let coins_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&pool)
            .await?;
        // Audit rows outside the monitoring lifecycle must not grow.
        let unrelated_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type NOT IN (
               'pc_monitoring_incident_opened', 'pc_monitoring_incident_closed'
             )",
        )
        .fetch_one(&pool)
        .await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS * 2);
        service.tick().await?;
        service.tick().await?;

        let remaining_after: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let status_after: String = sqlx::query_scalar("SELECT status FROM pcs WHERE id = 'pc-01'")
            .fetch_one(&pool)
            .await?;
        assert_eq!(remaining_before, remaining_after);
        assert_eq!(status_before, status_after);
        assert_eq!(coins_before, 0);
        assert_eq!(sessions_before, 0);
        let unrelated_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type NOT IN (
               'pc_monitoring_incident_opened', 'pc_monitoring_incident_closed'
             )",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(unrelated_after, unrelated_before);
        Ok(())
    }

    #[tokio::test]
    async fn suppression_blocks_new_incidents_through_the_whole_grace() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(12_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 30, clock.now())
            .await?;

        // Far beyond the mismatch grace: still no incident while suppressed.
        clock.advance_seconds(MISMATCH_GRACE_SECS * 3);
        for _ in 0..3 {
            service.tick().await?;
        }
        assert_eq!(open_incident_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn open_incident_stays_open_when_suppression_begins() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(13_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);

        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 30, clock.now())
            .await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS * 2);
        service.tick().await?;
        service.tick().await?;

        // Suppression never erases or closes the prior evidence.
        assert_eq!(open_incident_count(&pool).await, 1);
        let kinds = db::monitoring::active_kinds_by_pc(&pool).await?;
        assert_eq!(
            kinds["pc-01"],
            vec![IncidentKind::ClientReachabilityMismatch]
        );
        Ok(())
    }

    #[tokio::test]
    async fn expiry_restarts_full_grace_instead_of_alerting() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(14_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 1, clock.now())
            .await?;
        // Ticks run while suppressed (grace stays dropped), right up to the
        // absolute expiry at +60s.
        clock.advance_seconds(30);
        service.tick().await?;
        clock.advance_seconds(29);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);

        // One second past the expiry the PC is monitored again: this first
        // unsuppressed observation must not open anything.
        clock.advance_seconds(2);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);

        // ...and the full grace restarts from zero.
        clock.advance_seconds(MISMATCH_GRACE_SECS - 5);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        clock.advance_seconds(5);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_suppression_needs_a_fresh_full_grace_again() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(15_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 30, clock.now())
            .await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?; // suppressed: nothing tracks
        assert!(db::monitoring::cancel_suppression_with_audit(&pool, "pc-01", "Name pc-01").await?);

        // First post-cancellation observation starts a brand-new grace.
        clock.advance_seconds(30);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        clock.advance_seconds(MISMATCH_GRACE_SECS - 5);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        clock.advance_seconds(5);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn admin_restart_keeps_an_active_suppression_blocking() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(16_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 60, clock.now())
            .await?;

        // Simulated Admin restart: startup marking plus a fresh service.
        db::monitoring::mark_observation_interrupted(&pool).await?;
        let mut post_restart = MonitoringService::new(pool.clone(), probe, clock.clone());
        clock.advance_seconds(MISMATCH_GRACE_SECS * 2);
        for _ in 0..3 {
            post_restart.tick().await?;
        }
        assert_eq!(open_incident_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn suppression_keeps_recovered_episode_open_until_expiry() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(17_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 30, clock.now())
            .await?;

        // The Client genuinely comes back during maintenance, but the prior
        // episode remains open until suppression expires.
        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason, None);

        clock.advance_seconds(30 * 60);
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 0);
        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason.as_deref(), Some("client_recovered"));
        Ok(())
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::support::{audit_count, set_enabled, test_pool, MutableClock};
    use super::testing::FakeHostProbe;
    use super::*;

    /// Register `pc_id` as visible with its own address and a FRESH heartbeat,
    /// while the probe stays unscripted — every round times out. That is the
    /// exact Host Probe Coverage loss condition.
    async fn register_unprobed_running_pc(pool: &SqlitePool, pc_id: &str) -> AppResult<String> {
        let octet: u8 = pc_id
            .split('-')
            .next_back()
            .and_then(|suffix| suffix.parse().ok())
            .unwrap_or(1);
        let ip = format!("192.168.11.{octet}");
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: Some(ip.clone()),
                mac_address: None,
            },
        )
        .await?;
        crate::db::heartbeat_by_identity_only(pool, pc_id).await?;
        Ok(ip)
    }

    async fn open_coverage_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pc_monitoring_incidents
             WHERE kind = 'host_probe_coverage' AND closed_at IS NULL",
        )
        .fetch_one(pool)
        .await
        .expect("open coverage count")
    }

    #[tokio::test]
    async fn coverage_opens_only_after_two_continuous_minutes_of_failed_probes() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(20_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        // Observed but far below the two-minute window.
        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 0);

        // Ten more seconds of continuity cross the threshold.
        clock.advance_seconds(10);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);

        // Repeated failed probes never duplicate the incident or its Alert.
        for _ in 0..4 {
            service.tick().await?;
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pc_monitoring_incidents WHERE kind = 'host_probe_coverage'"
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn powered_off_or_client_unreachable_pc_never_opens_coverage() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(21_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        // Age the heartbeat out: no Client Reachability proves the host runs.
        sqlx::query(
            "UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = 'pc-01'",
        )
        .execute(&pool)
        .await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        // Far beyond the coverage grace: failed probes alone mean nothing.
        clock.advance_seconds(COVERAGE_GRACE_SECS * 3);
        for _ in 0..3 {
            service.tick().await?;
        }
        assert_eq!(open_coverage_count(&pool).await, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn one_successful_probe_resets_the_candidate_and_the_full_grace_restarts() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(22_000));
        let ip = register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;

        // The host answers once: continuity breaks and nothing opens.
        probe.script(&ip, HostProbeOutcome::Reply).expect("reply");
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 0);

        // Probes fail again: a brand-new full grace starts from zero.
        probe
            .script(&ip, HostProbeOutcome::Timeout)
            .expect("timeout");
        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 0);
        clock.advance_seconds(10);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn successful_probe_closes_the_episode_with_a_recovery_notice() -> AppResult<()> {
        use openpiso_core::notification::{classify_audit_event, NotificationSeverity};

        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(23_000));
        let ip = register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);

        // One successful response closes it immediately.
        probe.script(&ip, HostProbeOutcome::Reply).expect("reply");
        service.tick().await?;

        let row: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT closed_at, close_reason FROM pc_monitoring_incidents
             WHERE kind = 'host_probe_coverage'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(row.0.is_some());
        assert_eq!(row.1.as_deref(), Some("probe_recovered"));
        assert_eq!(open_coverage_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 1);

        // Generic semantics: opening is an Alert, this recovery a Notice.
        let (event_type, summary): (String, String) = sqlx::query_as(
            "SELECT event_type, summary FROM audit_log
             WHERE event_type = 'pc_monitoring_incident_closed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            classify_audit_event(&event_type),
            NotificationSeverity::Notice
        );
        assert!(summary.contains("answered probes again"), "{summary}");
        let opened_type: String = sqlx::query_scalar(
            "SELECT event_type FROM audit_log WHERE event_type = 'pc_monitoring_incident_opened'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            classify_audit_event(&opened_type),
            NotificationSeverity::Alert
        );
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_loss_closes_an_open_coverage_episode_as_unconfirmed() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(24_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);

        // The heartbeat stops proving the host is running.
        sqlx::query(
            "UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = 'pc-01'",
        )
        .execute(&pool)
        .await?;
        service.tick().await?;

        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason.as_deref(), Some("host_lost"));
        assert_eq!(open_coverage_count(&pool).await, 0);
        // The closure copy names the lost proof instead of a host that answered.
        let summary: String = sqlx::query_scalar(
            "SELECT summary FROM audit_log WHERE event_type = 'pc_monitoring_incident_closed'",
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
    async fn suppression_blocks_coverage_and_expiry_restarts_the_full_two_minutes() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(25_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        db::monitoring::set_suppression_with_audit(&pool, "pc-01", "Name pc-01", 2, clock.now())
            .await?;
        // Ticks run while suppressed, right up to just before expiry at +120s.
        clock.advance_seconds(COVERAGE_GRACE_SECS - 5);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 0);

        // One second past expiry the first unsuppressed observation must not
        // alert: the full two-minute grace restarts from zero.
        clock.advance_seconds(COVERAGE_GRACE_SECS - 59);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 0);
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 0);
        clock.advance_seconds(10);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn admin_restart_marks_coverage_interrupted_and_regrants_full_grace() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(26_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut pre_restart = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        pre_restart.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        pre_restart.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);
        let alerts_before = audit_count(&pool, "pc_monitoring_incident_opened").await;

        // Admin comes back up: startup marks the downtime span, and the fresh
        // in-memory state grants a full grace from zero instead of re-alerting.
        crate::db::monitoring::mark_observation_interrupted(&pool).await?;
        let mut post_restart = MonitoringService::new(pool.clone(), probe, clock.clone());
        clock.advance_seconds(COVERAGE_GRACE_SECS - 5);
        for _ in 0..3 {
            post_restart.tick().await?;
        }
        assert_eq!(
            audit_count(&pool, "pc_monitoring_incident_opened").await,
            alerts_before
        );
        let interrupted: i64 =
            sqlx::query_scalar("SELECT observation_interrupted FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(interrupted, 1);

        // Continuity past the fresh grace still holds without duplicating.
        clock.advance_seconds(5);
        post_restart.tick().await?;
        assert_eq!(
            audit_count(&pool, "pc_monitoring_incident_opened").await,
            alerts_before
        );
        assert_eq!(open_coverage_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn hidden_pc_with_open_coverage_episode_closes_as_host_lost() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(27_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);

        // The owner hides the PC: the observable target is gone.
        let settings = crate::db::pc_settings(&pool)
            .await?
            .into_iter()
            .find(|settings| settings.id == "pc-01")
            .expect("seeded PC");
        crate::db::update_pc_settings(
            &pool,
            "pc-01",
            &crate::models::PcSettingsInput {
                name: settings.name,
                visible: false,
                registered_ip: settings.registered_ip,
                mac_address: None,
            },
        )
        .await?;
        service.tick().await?;

        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason.as_deref(), Some("host_lost"));
        assert_eq!(open_coverage_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_registered_target_still_counts_as_lost_coverage_while_running() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(28_000));
        crate::db::update_pc_settings(
            &pool,
            "pc-01",
            &crate::models::PcSettingsInput {
                name: "Name pc-01".to_owned(),
                visible: true,
                registered_ip: Some("not-an-ip".to_owned()),
                mac_address: None,
            },
        )
        .await?;
        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        // Admin cannot test Host Reachability at all: with the Client proving
        // the PC is running, that is exactly degraded coverage.
        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_coverage_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn coverage_paths_leave_pcs_sessions_money_time_points_untouched() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(29_000));
        register_unprobed_running_pc(&pool, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let before: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let unrelated_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type NOT IN (
               'pc_monitoring_incident_opened', 'pc_monitoring_incident_closed'
             )",
        )
        .fetch_one(&pool)
        .await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        clock.advance_seconds(COVERAGE_GRACE_SECS * 2);
        service.tick().await?;
        service.tick().await?;

        let after: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(before, after);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
                .fetch_one(&pool)
                .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM coin_transactions")
                .fetch_one(&pool)
                .await?,
            0
        );
        let unrelated_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type NOT IN (
               'pc_monitoring_incident_opened', 'pc_monitoring_incident_closed'
             )",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(unrelated_after, unrelated_before);
        Ok(())
    }
}

/// Client Guard Coverage (#344) loop behavior: the heartbeat-reported
/// diagnostic opens and closes exactly one durable episode per uninterrupted
/// loss, with the shared grace, suppression, restart, and cleanup machinery.
#[cfg(test)]
mod guard_tests {
    use openpiso_core::notification::{classify_audit_event, NotificationSeverity};

    use super::support::{audit_count, set_enabled, test_pool, MutableClock};
    use super::testing::FakeHostProbe;
    use super::*;

    /// Register `pc_id` as visible with its own address, script the probe to
    /// answer it, take a fresh heartbeat, then store the given Guard coverage
    /// diagnostic as if that Heartbeat had reported it.
    async fn register_running_pc_with_guard(
        pool: &SqlitePool,
        probe: &FakeHostProbe,
        pc_id: &str,
        guard_coverage: Option<&str>,
    ) -> AppResult<()> {
        let octet: u8 = pc_id
            .split('-')
            .next_back()
            .and_then(|suffix| suffix.parse().ok())
            .unwrap_or(1);
        let ip = format!("192.168.12.{octet}");
        // The host answers every round, so only the Guard diagnostic can
        // degrade: no Host Probe Coverage episode interferes here.
        probe.script(&ip, HostProbeOutcome::Reply).expect("script");
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("Name {pc_id}"),
                visible: true,
                registered_ip: Some(ip),
                mac_address: None,
            },
        )
        .await?;
        crate::db::heartbeat_by_identity_only(pool, pc_id).await?;
        sqlx::query("UPDATE pcs SET last_guard_coverage = ? WHERE id = ?")
            .bind(guard_coverage)
            .bind(pc_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    async fn open_guard_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pc_monitoring_incidents
             WHERE kind = 'client_guard_coverage' AND closed_at IS NULL",
        )
        .fetch_one(pool)
        .await
        .expect("open guard count")
    }

    #[tokio::test]
    async fn unavailable_opens_only_after_two_continuous_minutes() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(30_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 0);

        // Ten more seconds of continuity cross the threshold.
        clock.advance_seconds(10);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);

        // Repeated ticks never duplicate the episode or its Alert.
        for _ in 0..3 {
            service.tick().await?;
        }
        assert_eq!(open_guard_count(&pool).await, 1);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn not_applicable_and_omitted_never_open_an_incident() -> AppResult<()> {
        for diagnostic in [Some("not_applicable"), None] {
            let pool = test_pool().await?;
            let probe = Arc::new(FakeHostProbe::default());
            let clock = Arc::new(MutableClock::starting_at(31_000));
            register_running_pc_with_guard(&pool, &probe, "pc-01", diagnostic).await?;
            set_enabled(&pool, true).await?;
            let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

            // Far beyond the coverage grace: no usable evidence, no Alert —
            // an older Client that omits the field can never create one.
            clock.advance_seconds(COVERAGE_GRACE_SECS * 3);
            for _ in 0..3 {
                service.tick().await?;
            }
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_incidents")
                    .fetch_one(&pool)
                    .await?,
                0
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn covered_resets_the_grace_and_the_full_two_minutes_restarts() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(32_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;

        // One covered report breaks continuity; nothing opens.
        sqlx::query("UPDATE pcs SET last_guard_coverage = 'covered' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 0);

        // Unavailable again: a brand-new full grace starts from zero.
        sqlx::query("UPDATE pcs SET last_guard_coverage = 'unavailable' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 0);
        clock.advance_seconds(10);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn restored_coverage_closes_the_episode_as_a_recovery_notice() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(33_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);

        sqlx::query("UPDATE pcs SET last_guard_coverage = 'covered' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        service.tick().await?;

        let row: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT closed_at, close_reason FROM pc_monitoring_incidents
             WHERE kind = 'client_guard_coverage'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(row.0.is_some());
        assert_eq!(row.1.as_deref(), Some("guard_recovered"));
        assert_eq!(open_guard_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_closed").await, 1);

        // Generic semantics: opening is an Alert, this recovery a Notice.
        let (event_type, summary): (String, String) = sqlx::query_as(
            "SELECT event_type, summary FROM audit_log
             WHERE event_type = 'pc_monitoring_incident_closed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            classify_audit_event(&event_type),
            NotificationSeverity::Notice,
            "{summary}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_loss_closes_an_open_guard_episode_as_unconfirmed() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(34_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);

        // Age the heartbeat out: without Client Reachability proving the PC
        // runs there is no Guard evidence at all, so the episode closes as
        // unconfirmed instead of lingering open forever.
        sqlx::query(
            "UPDATE pcs SET last_seen_at = datetime('now', '-30 seconds') WHERE id = 'pc-01'",
        )
        .execute(&pool)
        .await?;
        service.tick().await?;

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT close_reason FROM pc_monitoring_incidents
             WHERE kind = 'client_guard_coverage'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason.as_deref(), Some("host_lost"));
        assert_eq!(open_guard_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn suppression_blocks_guard_incidents_and_expiry_restarts_the_grace() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(35_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;

        // Suppression blocks new Guard incidents through the whole grace.
        db::monitoring::set_suppression_with_audit(
            &pool,
            "pc-01",
            "Name pc-01",
            MAX_SUPPRESSION_MINUTES,
            clock.now(),
        )
        .await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());
        clock.advance_seconds(COVERAGE_GRACE_SECS * 2);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 0);
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 0);

        // Expiry restarts a full two-minute grace instead of alerting.
        clock.advance_seconds(MAX_SUPPRESSION_MINUTES * 60 + MONITOR_INTERVAL_SECS as i64);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 0);
        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 10);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 0);
        clock.advance_seconds(10);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn admin_restart_marks_guard_interrupted_without_duplicating_alerts() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(36_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe.clone(), clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);

        // A fresh service models a restarted Admin: startup marks the
        // still-open episode observation-interrupted and the empty in-memory
        // continuity map regrants full grace, but no new Alert appears.
        drop(service);
        db::monitoring::mark_observation_interrupted(&pool).await?;
        let mut restarted = MonitoringService::new(pool.clone(), probe, clock.clone());
        restarted.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS - 20);
        restarted.tick().await?;
        assert_eq!(audit_count(&pool, "pc_monitoring_incident_opened").await, 1);

        let interrupted: i64 = sqlx::query_scalar(
            "SELECT observation_interrupted FROM pc_monitoring_incidents
             WHERE kind = 'client_guard_coverage'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(interrupted, 1);
        assert_eq!(open_guard_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn hidden_pc_with_an_open_guard_episode_closes_as_host_lost() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(37_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);

        crate::db::update_pc_settings(
            &pool,
            "pc-01",
            &crate::models::PcSettingsInput {
                name: "Name pc-01".to_owned(),
                visible: false,
                registered_ip: Some("192.168.12.1".to_owned()),
                mac_address: None,
            },
        )
        .await?;
        service.tick().await?;

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT close_reason FROM pc_monitoring_incidents
             WHERE kind = 'client_guard_coverage'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason.as_deref(), Some("host_lost"));
        assert_eq!(open_guard_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn guard_paths_leave_pcs_sessions_money_time_points_untouched() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = Arc::new(FakeHostProbe::default());
        let clock = Arc::new(MutableClock::starting_at(38_000));
        register_running_pc_with_guard(&pool, &probe, "pc-01", Some("unavailable")).await?;
        set_enabled(&pool, true).await?;
        let before: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let mut service = MonitoringService::new(pool.clone(), probe, clock.clone());

        service.tick().await?;
        clock.advance_seconds(COVERAGE_GRACE_SECS * 2);
        service.tick().await?;
        assert_eq!(open_guard_count(&pool).await, 1);

        let after: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(before, after);
        Ok(())
    }
}

/// Client Recovery Escalation (ADR-0077): the escalation clock, its seven
/// conditions, the cooldown, and the proof that acting still leaves money,
/// Remaining Time, and PC status alone.
#[cfg(test)]
mod escalation_tests {
    use super::support::{
        audit_count, open_incident_count, register_mismatched_pc, set_enabled, test_pool,
        MutableClock,
    };
    use super::testing::FakeHostProbe;
    use super::*;

    fn probe_of() -> Arc<FakeHostProbe> {
        Arc::new(FakeHostProbe::default())
    }

    // -----------------------------------------------------------------------
    // Client Recovery Escalation (ADR-0077)
    // -----------------------------------------------------------------------

    /// Build a monitor that is allowed to act, plus the registry the Guard
    /// Poll writes into.
    fn escalating_service(
        pool: &SqlitePool,
        probe: Arc<FakeHostProbe>,
        clock: Arc<MutableClock>,
    ) -> (MonitoringService, GuardRegistry) {
        let guards = GuardRegistry::new();
        let service =
            MonitoringService::new(pool.clone(), probe, clock).with_escalation(EscalationDeps {
                sessions: SessionStore::default(),
                tokens: AccountTokens::default(),
                guards: guards.clone(),
            });
        (service, guards)
    }

    async fn set_escalation(pool: &SqlitePool, enabled: bool, delay_secs: i64) -> AppResult<()> {
        for (key, value) in [
            (
                crate::db::monitoring::SETTINGS_KEY_ESCALATION_ENABLED,
                if enabled {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                },
            ),
            (
                crate::db::monitoring::SETTINGS_KEY_ESCALATION_DELAY,
                delay_secs.to_string(),
            ),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value)
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    /// One Guard Poll reporting the full escalation-worthy state. Facts go
    /// stale, so every test re-reports before each tick exactly as a live
    /// Guard would.
    fn report_losing_guard(guards: &GuardRegistry, clock: &MutableClock, pc_id: &str) {
        guards.record_poll(
            pc_id,
            crate::guard_poll::GuardFacts {
                received_at: clock.now(),
                client_connected: false,
                disarmed: false,
                relaunch_attempts: 3,
            },
        );
    }

    async fn power_off_count(pool: &SqlitePool) -> i64 {
        audit_count(pool, "pc_escalated_power_off").await
    }

    #[tokio::test]
    async fn escalation_powers_off_only_after_the_full_delay() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        set_escalation(&pool, true, 60).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        clock.advance_seconds(59);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert_eq!(power_off_count(&pool).await, 0);
        assert!(!guards.is_armed("pc-01"));

        clock.advance_seconds(1);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert_eq!(power_off_count(&pool).await, 1);
        assert!(guards.is_armed("pc-01"));
        Ok(())
    }

    #[tokio::test]
    async fn a_guard_that_has_not_tried_a_relaunch_is_never_escalated_against() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        set_escalation(&pool, true, 60).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        for _ in 0..5 {
            guards.record_poll(
                "pc-01",
                crate::guard_poll::GuardFacts {
                    received_at: clock.now(),
                    client_connected: false,
                    disarmed: false,
                    relaunch_attempts: 0,
                },
            );
            service.tick().await?;
            clock.advance_seconds(30);
        }
        assert_eq!(power_off_count(&pool).await, 0);
        assert!(!guards.is_armed("pc-01"));
        Ok(())
    }

    #[tokio::test]
    async fn escalation_stays_inert_while_disabled() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        for _ in 0..5 {
            report_losing_guard(&guards, &clock, "pc-01");
            service.tick().await?;
            clock.advance_seconds(60);
        }
        // The mismatch still opens: detection is unchanged by the policy.
        assert_eq!(open_incident_count(&pool).await, 1);
        assert_eq!(power_off_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn a_power_off_closes_the_mismatch_as_our_doing_not_a_lost_host() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        // A delay longer than the mismatch grace, so the incident is provably
        // open before the power off closes it. The two clocks are independent
        // (ADR-0077), so a shorter delay legitimately fires first.
        set_escalation(&pool, true, 120).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        clock.advance_seconds(MISMATCH_GRACE_SECS);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert_eq!(open_incident_count(&pool).await, 1);
        assert_eq!(power_off_count(&pool).await, 0);

        clock.advance_seconds(60);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;

        let close_reason: Option<String> = sqlx::query_scalar(
            "SELECT close_reason FROM pc_monitoring_incidents WHERE pc_id = 'pc-01'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(close_reason.as_deref(), Some("escalated_power_off"));
        assert_eq!(power_off_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn the_cooldown_bounds_a_pc_that_reboots_into_the_same_fault() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        set_escalation(&pool, true, 30).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        clock.advance_seconds(30);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert_eq!(power_off_count(&pool).await, 1);

        // The PC comes back on its own, withdrawing the order, and fails the
        // same way again well inside the cooldown.
        guards.clear_power_off("pc-01");
        for _ in 0..6 {
            clock.advance_seconds(60);
            report_losing_guard(&guards, &clock, "pc-01");
            service.tick().await?;
        }
        assert_eq!(power_off_count(&pool).await, 1);

        // Past the hour, the same evidence earns a second power off.
        clock.advance_seconds(crate::db::monitoring::ESCALATION_COOLDOWN_MINUTES * 60);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        clock.advance_seconds(30);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert_eq!(power_off_count(&pool).await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_guard_that_stops_polling_stops_being_evidence() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        set_escalation(&pool, true, 30).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        // One poll, then silence: the Guard service stopped. Its last facts
        // must not carry the decision on their own.
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        for _ in 0..6 {
            clock.advance_seconds(60);
            service.tick().await?;
        }
        assert_eq!(power_off_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn a_returning_client_withdraws_a_standing_order() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        set_escalation(&pool, true, 30).await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        clock.advance_seconds(30);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert!(guards.is_armed("pc-01"));

        crate::db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        service.tick().await?;
        assert!(!guards.is_armed("pc-01"));
        Ok(())
    }

    #[tokio::test]
    async fn escalation_never_touches_pc_status_or_remaining_time() -> AppResult<()> {
        let pool = test_pool().await?;
        let probe = probe_of();
        let clock = Arc::new(MutableClock::starting_at(10_000));
        register_mismatched_pc(&pool, &probe, "pc-01").await?;
        set_enabled(&pool, true).await?;
        set_escalation(&pool, true, 30).await?;
        let before: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let (mut service, guards) = escalating_service(&pool, probe, clock.clone());

        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        clock.advance_seconds(30);
        report_losing_guard(&guards, &clock, "pc-01");
        service.tick().await?;
        assert_eq!(power_off_count(&pool).await, 1);

        let after: (String, i64) =
            sqlx::query_as("SELECT status, remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(before, after);
        Ok(())
    }
}

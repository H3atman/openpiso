// M1 deliverable: the coin-source boundary. The two implementations and the
// shared gap/batch helper are exercised by their own unit tests today. M2
// wires the active `CoinSource` to the new `TerminalSelection` and the
// existing `record_coin_event` path. The `try_next_batch_at` default on the
// trait gives M2 a deterministic test clock without forcing every
// implementation to override it. M2 ships the simulator in production (per
// the `dev-simulator` seed in AGENTS.md); the serial constructor and the
// pulse-injection helpers are kept ready for the M4 calibration pass and
// for unit tests, so a module-scope `dead_code` allow is justified.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use tokio::sync::mpsc::UnboundedSender;

use serialport::SerialPort;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoinSourceKind {
    Simulator,
    Serial,
}

/// Messages from the real Coin Source to the Admin audit path. The source owns
/// no database handle, so the channel is unbounded: an availability edge is a
/// rare, non-repeatable fact and must not be dropped under a burst.
#[derive(Debug)]
pub(crate) enum CoinSourceAuditMessage {
    Availability { available: bool, reason: String },
    SamplingAnomaly { reason: String, count: u32 },
}

pub(crate) type CoinSourceAuditSender = UnboundedSender<CoinSourceAuditMessage>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetPinLine {
    Rts,
    Dtr,
}

impl SetPinLine {
    pub fn parse(value: &str) -> AppResult<Self> {
        match value {
            "rts" => Ok(Self::Rts),
            "dtr" => Ok(Self::Dtr),
            _ => Err(AppError::InvalidInput(
                "set pin line must be rts or dtr".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoinPinLine {
    Cts,
    Dsr,
    Ri,
    Cd,
}

impl CoinPinLine {
    pub fn parse(value: &str) -> AppResult<Self> {
        match value {
            "cts" => Ok(Self::Cts),
            "dsr" => Ok(Self::Dsr),
            "ri" => Ok(Self::Ri),
            "cd" => Ok(Self::Cd),
            _ => Err(AppError::InvalidInput(
                "coin pin line must be cts, dsr, ri, or cd".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoinPinPolarity {
    NormallyOpen,
    NormallyClosed,
}

impl CoinPinPolarity {
    pub fn parse(value: &str) -> AppResult<Self> {
        match value {
            "normally_open" => Ok(Self::NormallyOpen),
            "normally_closed" => Ok(Self::NormallyClosed),
            _ => Err(AppError::InvalidInput(
                "coin pin polarity must be normally_open or normally_closed".to_owned(),
            )),
        }
    }

    fn normalize(self, raw_level: bool) -> bool {
        match self {
            Self::NormallyOpen => raw_level,
            Self::NormallyClosed => !raw_level,
        }
    }
}

/// Conservative polling default for typical pulse coin acceptors. Pulses
/// narrower than the polling period may be missed; characterize the installed
/// acceptor before customer use. This period is not operator-configurable.
pub const PULSE_SAMPLER_POLL_MS: Duration = Duration::from_millis(30);

/// Number of consecutive Coin Pin read failures before the sampler drops the
/// port and starts recovery, without relying on operating-system error text.
pub const SERIAL_READ_FAILURE_THRESHOLD: u32 = 20;

/// Initial and maximum delays between serial-port reopen attempts.
pub const SERIAL_RECONNECT_BASE_DELAY: Duration = Duration::from_secs(1);
pub const SERIAL_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);
/// Recovery wake-up granularity. It keeps shutdown responsive without
/// polling a missing port at the normal 5 ms sampling rate.
const SERIAL_RECOVERY_WAKE_INTERVAL: Duration = Duration::from_millis(100);

type SerialPortHandle = Box<dyn SerialPort + Send>;
type PortOpener = Arc<dyn Fn() -> AppResult<SerialPortHandle> + Send + Sync>;
type SharedSerialPort = Arc<Mutex<Option<SerialPortHandle>>>;

struct SamplerContext {
    port: SharedSerialPort,
    set_pin_line: SetPinLine,
    coin_pin_line: CoinPinLine,
    observation: Arc<Mutex<PulseObservation>>,
    set_pin_asserted: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
    sampling_healthy: Arc<AtomicBool>,
    sampling_fault_generation: Arc<AtomicU64>,
    inhibited_pulse_limiter: Arc<InhibitedPulseRateLimiter>,
    audit_sender: Option<CoinSourceAuditSender>,
    opener: PortOpener,
}

/// State for bounded, deterministic serial recovery. The production sampler
/// supplies wall-clock instants; tests advance this state explicitly.
#[derive(Debug, Default)]
struct SamplerRecoveryState {
    consecutive_failures: u32,
    reconnect_attempts: u32,
    next_reconnect_at: Option<Instant>,
    last_error: Option<String>,
}

impl SamplerRecoveryState {
    fn retry_delay(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(5);
        let multiplier = 1u32.checked_shl(shift).unwrap_or(32);
        SERIAL_RECONNECT_BASE_DELAY
            .checked_mul(multiplier)
            .unwrap_or(SERIAL_RECONNECT_MAX_DELAY)
            .min(SERIAL_RECONNECT_MAX_DELAY)
    }
}

fn bounded_reason(reason: impl std::fmt::Display) -> String {
    reason.to_string().chars().take(200).collect()
}

fn publish_audit_message(sender: Option<&CoinSourceAuditSender>, message: CoinSourceAuditMessage) {
    let Some(sender) = sender else {
        return;
    };
    if let Err(error) = sender.send(message) {
        warn!(
            target: "openpiso_admin_lib::coin_source",
            error = %error,
            "Coin Source audit consumer is gone; message dropped"
        );
    }
}

fn publish_availability_edge(
    available: &AtomicBool,
    sender: Option<&CoinSourceAuditSender>,
    next: bool,
    reason: impl std::fmt::Display,
) {
    if available.swap(next, Ordering::AcqRel) != next {
        publish_audit_message(
            sender,
            CoinSourceAuditMessage::Availability {
                available: next,
                reason: bounded_reason(reason),
            },
        );
    }
}

fn publish_sampling_anomaly(
    sender: Option<&CoinSourceAuditSender>,
    cause: InhibitedPulseCause,
    count: u32,
) {
    publish_audit_message(
        sender,
        CoinSourceAuditMessage::SamplingAnomaly {
            reason: cause.reason().to_owned(),
            count,
        },
    );
}

/// Conservative quiet-gap default for typical pulse coin acceptors. No pulse
/// received for this duration closes the current batch. Operators can adjust
/// `inter_coin_gap_ms` after characterizing their installed acceptor.
pub const DEFAULT_INTER_COIN_GAP: Duration = Duration::from_millis(600);

/// Rate-limiter for inhibited-pulse anomaly logging so a noisy line
/// cannot flood diagnostic files. Follows the same per-key sliding-window
/// pattern as `DiagnosticRateLimiter`.
const INHIBITED_PULSE_WINDOW: Duration = Duration::from_secs(10);
const INHIBITED_PULSE_MAX_PER_WINDOW: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum InhibitedPulseCause {
    CoinSourceUnavailable,
    NoSelectionWindow,
}

impl InhibitedPulseCause {
    fn reason(self) -> &'static str {
        match self {
            Self::CoinSourceUnavailable => "coin_source_unavailable",
            Self::NoSelectionWindow => "no_selection_window",
        }
    }

    fn summary(self, count: u32) -> String {
        let pulses = if count == 1 { "pulse" } else { "pulses" };
        match self {
            Self::CoinSourceUnavailable => format!(
                "Ignored {count} {pulses} because the Coin Source was unavailable or recovering."
            ),
            Self::NoSelectionWindow => format!(
                "Ignored {count} {pulses} while no Selection Window was open and the Set Pin was inhibited; check the Coin Debug Bundle for an idle insertion or an inhibit wiring problem."
            ),
        }
    }
}

pub(crate) fn sampling_anomaly_summary(reason: &str, count: u32) -> String {
    match reason {
        "coin_source_unavailable" => InhibitedPulseCause::CoinSourceUnavailable.summary(count),
        "no_selection_window" => InhibitedPulseCause::NoSelectionWindow.summary(count),
        _ => {
            let pulses = if count == 1 { "pulse" } else { "pulses" };
            format!("Ignored {count} {pulses} due to a coin sampling anomaly.")
        }
    }
}

#[derive(Debug)]
pub struct InhibitedPulseRateLimiter {
    inner: Mutex<Vec<Instant>>,
    aggregate: Mutex<HashMap<InhibitedPulseCause, (Instant, u32)>>,
}

impl InhibitedPulseRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            aggregate: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the anomaly should be logged (within the rate
    /// budget). Clock is injectable for testing.
    pub fn should_log(&self) -> bool {
        self.should_log_at(Instant::now())
    }

    fn should_log_at(&self, now: Instant) -> bool {
        let mut hits = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hits.retain(|seen| now.duration_since(*seen) < INHIBITED_PULSE_WINDOW);
        if hits.len() >= INHIBITED_PULSE_MAX_PER_WINDOW {
            return false;
        }
        hits.push(now);
        true
    }

    /// Count one anomaly in a bounded, cause-keyed window. Each cause keeps
    /// its own window, so a mixed period produces separate truthful summaries
    /// rather than assigning one cause to another cause's pulses.
    fn finish_window_at(
        &self,
        now: Instant,
        cause: InhibitedPulseCause,
    ) -> Option<(InhibitedPulseCause, u32)> {
        let mut aggregate = self
            .aggregate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((started, count)) = aggregate.get_mut(&cause) {
            if now.duration_since(*started) < INHIBITED_PULSE_WINDOW {
                *count = count.saturating_add(1).min(1000);
                return None;
            }
            let previous_count = *count;
            *started = now;
            *count = 1;
            return Some((cause, previous_count));
        }
        aggregate.insert(cause, (now, 1));
        None
    }

    fn flush_expired_at(&self, now: Instant) -> Vec<(InhibitedPulseCause, u32)> {
        let mut aggregate = self
            .aggregate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired: Vec<_> = aggregate
            .iter()
            .filter_map(|(cause, (started, count))| {
                (now.duration_since(*started) >= INHIBITED_PULSE_WINDOW).then_some((*cause, *count))
            })
            .collect();
        for (cause, _) in &expired {
            aggregate.remove(cause);
        }
        expired
    }
}

impl Default for InhibitedPulseRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// The facts produced by observing one Coin Pin sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulseObservationResult {
    pub edge_observed: bool,
    pub pulse_admitted: bool,
}

/// Edge detector and pulse accumulator extracted from the serial source so
/// it can be exercised directly by tests without a serial port and driven
/// by a future sampler thread. It records admitted pulses in the shared
/// buffer; a Set Pin inhibition only refuses a new train, not a train already
/// buffered. Batch draining reads the same buffer.
#[derive(Debug, Clone)]
pub struct PulseObservation {
    polarity: CoinPinPolarity,
    prev_coin_pin: Option<bool>,
    /// When the pin last transitioned low→high. Used to measure how long the
    /// line stays asserted (pulse width), logged on the falling edge. Real
    /// coin pulses are wide (tens of ms); floating-line noise spikes are
    /// narrow — the width distribution is what lets an owner size a debounce.
    high_since: Option<Instant>,
    last_rising_edge: Option<Instant>,
    pulses: Vec<Instant>,
}

impl PulseObservation {
    pub fn new() -> Self {
        Self::with_polarity(CoinPinPolarity::NormallyOpen)
    }

    pub fn with_polarity(polarity: CoinPinPolarity) -> Self {
        Self {
            polarity,
            prev_coin_pin: None,
            high_since: None,
            last_rising_edge: None,
            pulses: Vec::new(),
        }
    }

    /// Observe one Coin Pin sample and return whether an edge was observed.
    /// Use [`Self::observe_with_result`] when the caller also needs to know
    /// whether that edge was admitted into the buffered Coin Batch.
    pub fn observe(&mut self, pin_level: bool, set_pin_asserted: bool, now: Instant) -> bool {
        self.observe_with_result(pin_level, set_pin_asserted, now)
            .edge_observed
    }

    /// Observe one Coin Pin sample and expose both observation facts. The
    /// first sample seeds previous-level state without observing an edge. A
    /// rising edge is admitted when the Set Pin is asserted or when the
    /// shared pulse buffer already contains a pulse from the same train.
    /// Keeping the admission rule here avoids duplicating train state outside
    /// the pulse buffer.
    ///
    /// Logs at INFO for every rising edge (with interval, Set Pin state, and
    /// admission) and at DEBUG for every raw sample.
    pub fn observe_with_result(
        &mut self,
        pin_level: bool,
        set_pin_asserted: bool,
        now: Instant,
    ) -> PulseObservationResult {
        // Normalize wiring polarity exactly once at the observation boundary.
        // All edge detection and downstream batching sees pulse-asserted state.
        let pulse_asserted = self.polarity.normalize(pin_level);
        // The first real sample establishes the line state. It is not an
        // edge: an idle-high line must not become a phantom pulse at open or
        // after recovery.
        let previous = self.prev_coin_pin.replace(pulse_asserted);
        let rising = previous == Some(false) && pulse_asserted;
        let falling = previous == Some(true) && !pulse_asserted;

        // Raw per-sample pin state at DEBUG only — volume here is the full
        // roughly 33 Hz sample rate, kept separate from the edge-level INFO
        // path.
        debug!(
            target: "openpiso_admin_lib::coin_source",
            pin_level,
            set_pin_asserted,
            rising,
            "Pin sample"
        );

        let mut pulse_admitted = false;
        if rising {
            self.high_since = Some(now);
            let interval = self.last_rising_edge.map(|t| now.duration_since(t));
            // A pulse already in the buffer is the only train state needed to
            // let a straddling train finish after Set Pin inhibition.
            pulse_admitted = set_pin_asserted || !self.pulses.is_empty();
            info!(
                target: "openpiso_admin_lib::coin_source",
                interval_us = interval.map(|d| d.as_micros()).unwrap_or(0),
                set_pin_asserted,
                pulse_admitted,
                "Pulse observed"
            );
            self.last_rising_edge = Some(now);
            if pulse_admitted {
                self.pulses.push(now);
            }
        } else if falling {
            // Measure how long the line stayed asserted. Wide = real coin
            // pulse; narrow = likely floating-line noise. Owner uses this to
            // size a minimum-pulse-width debounce.
            if let Some(started) = self.high_since.take() {
                info!(
                    target: "openpiso_admin_lib::coin_source",
                    width_us = now.duration_since(started).as_micros() as u64,
                    set_pin_asserted,
                    "Pulse width"
                );
            }
        }

        PulseObservationResult {
            edge_observed: rising,
            pulse_admitted,
        }
    }

    /// Close a batch if the inter-coin gap has elapsed since the last
    /// pulse. Returns [`CoinBatchRead::None`] while still within the gap.
    pub fn close_batch_if_gap_elapsed(
        &mut self,
        now: Instant,
        config: &CoinSourceConfig,
    ) -> Result<CoinBatchRead, AppError> {
        close_batch_if_gap_elapsed(&mut self.pulses, now, config)
    }

    /// Inject a pulse timestamp directly (bypasses edge detection). Used
    /// by the simulator to stage pulses without a serial port.
    pub fn inject_pulse_at(&mut self, at: Instant) {
        self.pulses.push(at);
    }

    pub fn has_buffered_pulses(&self) -> bool {
        !self.pulses.is_empty()
    }

    pub fn pulse_count(&self) -> usize {
        self.pulses.len()
    }

    /// Remove and return every buffered pulse without applying the production
    /// quiet-gap/map rules. Calibration uses this while the source is
    /// exclusively owned by the owner workflow.
    pub fn take_pulses(&mut self) -> Vec<Instant> {
        std::mem::take(&mut self.pulses)
    }

    pub fn clear_pulses(&mut self) {
        self.reset();
    }

    /// Reset to a fresh, unseeded state (no pulses and no assumed pin level).
    pub fn reset(&mut self) {
        self.pulses.clear();
        self.prev_coin_pin = None;
        self.high_since = None;
        self.last_rising_edge = None;
    }
}

impl Default for PulseObservation {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed authority for automatic Coin Batch acceptance. The Pulse Map remains
/// persisted for physical calibration and historical Rejection Credit choices,
/// but it is never read by the live Coin Source.
pub const COIN_CENTS_PER_PULSE: i64 = 100;

/// Runtime configuration for a Coin Source. Pulse observations are closed by
/// the configured quiet gap; their value is always derived as checked pulse
/// count × [`COIN_CENTS_PER_PULSE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoinSourceConfig {
    pub inter_coin_gap: Duration,
}

impl CoinSourceConfig {
    /// Runtime settings intentionally exclude the persisted Pulse Map.
    pub fn default_config() -> Self {
        Self {
            inter_coin_gap: DEFAULT_INTER_COIN_GAP,
        }
    }

    pub fn validate(&self) -> AppResult<()> {
        Ok(())
    }
}

/// One observed physical Coin Batch. A batch carries only pulse evidence and
/// retry evidence; the live authority derives its amount as checked pulse
/// count × [`COIN_CENTS_PER_PULSE`] at the commit boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinBatch {
    pub coin_trace_id: Uuid,
    pub pulse_count: i64,
    pub pulse_timings_us: Vec<u64>,
    pub span_us: u64,
    pub gap_us: u64,
    pub attempt: u32,
}

impl CoinBatch {
    /// Derive the single authoritative automatic-credit amount from the
    /// observed pulse sum. No caller may supply or map this value.
    pub fn checked_amount_cents(&self) -> AppResult<i64> {
        let amount_cents = self
            .pulse_count
            .checked_mul(COIN_CENTS_PER_PULSE)
            .ok_or_else(|| {
                AppError::InvalidInput("Coin Batch pulse sum exceeds cents authority".to_owned())
            })?;
        if amount_cents <= 0 {
            return Err(AppError::InvalidInput(
                "Coin Batch pulse count must be positive".to_owned(),
            ));
        }
        Ok(amount_cents)
    }
}

/// Outcome of one batch read from a Coin Source. The error channel remains
/// reserved for transport and availability failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoinBatchRead {
    /// No batch is ready yet: the buffer is empty or still inside the
    /// inter-coin gap.
    None,
    /// A closed batch of pulse observations. Its amount is derived only at
    /// the authoritative commit boundary.
    Batch(CoinBatch),
}

/// Boundary for any coin source. Both the real serial acceptor and the
/// headless simulator implement this so the selection / commit code (M2) is
/// shared. The set pin is **fail-closed**: implementations start inhibited,
/// re-inhibit on `Drop`, and only `assert_set_pin` makes coins ingestible.
pub trait CoinSource: Send {
    /// Return the next batch-read outcome. The batch closes when
    /// `inter_coin_gap` elapses since the last pulse. Returns
    /// [`CoinBatchRead::None`] when no batch is ready yet. Implementations
    /// that do not need a deterministic clock (production serial/simulator)
    /// can leave the default in place.
    fn try_next_batch(&mut self) -> Result<CoinBatchRead, AppError> {
        self.try_next_batch_at(Instant::now())
    }

    /// Same as `try_next_batch`, but with an injectable clock so the M2
    /// selection state machine can test gap-bounded batching without sleeping.
    /// The default delegates to `try_next_batch`; the simulator and serial
    /// implementations override it so `now` (not wall time) closes the batch.
    fn try_next_batch_at(&mut self, now: Instant) -> Result<CoinBatchRead, AppError>;

    /// Drive the set pin to assert (enable the acceptor).
    fn assert_set_pin(&mut self) -> Result<(), AppError>;

    /// Drive the set pin to inhibit (disable the acceptor). Idempotent.
    fn inhibit_set_pin(&mut self) -> Result<(), AppError>;

    /// Whether the set pin is currently asserted.
    fn is_set_pin_asserted(&self) -> bool;

    /// Whether the source is currently usable. The M2 simulator is always
    /// `true`; the M4 serial source reports `false` when the COM port is
    /// closed, missing, or the handle is poisoned. The kiosk uses this
    /// to render the "out of order" screen instead of allowing an
    /// `open_selection` (which would assert the set pin against a dead
    /// source, fail to drain in-flight pulses, and leave the customer
    /// without a refund path).
    fn is_available(&self) -> bool;

    /// Whether the serial sampler is currently healthy. A source can remain
    /// available during a transient read failure while recovery is pending.
    fn is_sampling_healthy(&self) -> bool {
        true
    }

    /// Monotonic source-fault generation. Calibration captures fail if the
    /// sampler faults and recovers between two owner polls.
    fn sampling_fault_generation(&self) -> u64 {
        0
    }

    fn kind(&self) -> CoinSourceKind {
        CoinSourceKind::Simulator
    }

    fn inter_coin_gap(&self) -> Duration {
        DEFAULT_INTER_COIN_GAP
    }

    fn has_buffered_pulses(&self) -> bool {
        false
    }

    /// Drain raw pulse timestamps for an exclusive calibration workflow.
    /// Production batching must use `try_next_batch_at` instead.
    fn take_buffered_pulses(&mut self) -> Vec<Instant> {
        Vec::new()
    }

    fn clear_buffered_pulses(&mut self) {}
}

/// Programmatic pulse injector for hardware-free development and tests.
/// All time-dependent behavior is exposed via the `*_at(Instant)` helpers so
/// tests do not have to sleep.
pub struct SimulatorCoinSource {
    config: CoinSourceConfig,
    observation: PulseObservation,
    set_pin_asserted: bool,
}

impl SimulatorCoinSource {
    pub fn new(config: CoinSourceConfig) -> AppResult<Self> {
        config.validate()?;
        info!(
            target: "openpiso_admin_lib::coin_source",
            kind = "simulator",
            inter_coin_gap_us = config.inter_coin_gap.as_micros() as u64,
            "Coin source created"
        );
        Ok(Self {
            config,
            observation: PulseObservation::new(),
            // Fail-closed: a fresh source never lets coins through.
            set_pin_asserted: false,
        })
    }

    /// Inject a pulse at the given instant. Multiple pulses within
    /// `inter_coin_gap` accumulate into one batch.
    pub fn inject_pulse_at(&mut self, at: Instant) {
        self.observation.inject_pulse_at(at);
    }

    /// Inject a pulse at the current time.
    pub fn inject_pulse(&mut self) {
        self.inject_pulse_at(Instant::now());
    }
}

impl CoinSource for SimulatorCoinSource {
    fn try_next_batch_at(&mut self, now: Instant) -> Result<CoinBatchRead, AppError> {
        self.observation
            .close_batch_if_gap_elapsed(now, &self.config)
    }

    fn assert_set_pin(&mut self) -> Result<(), AppError> {
        self.set_pin_asserted = true;
        info!(
            target: "openpiso_admin_lib::coin_source",
            kind = "simulator",
            asserted = true,
            cause = "simulator::assert",
            "Set Pin asserted"
        );
        Ok(())
    }

    fn inhibit_set_pin(&mut self) -> Result<(), AppError> {
        self.set_pin_asserted = false;
        info!(
            target: "openpiso_admin_lib::coin_source",
            kind = "simulator",
            asserted = false,
            cause = "simulator::inhibit",
            "Set Pin inhibited"
        );
        Ok(())
    }

    fn is_set_pin_asserted(&self) -> bool {
        self.set_pin_asserted
    }

    fn is_available(&self) -> bool {
        // The simulator is always available; it is a pure in-memory
        // stand-in and never depends on hardware state.
        true
    }

    fn kind(&self) -> CoinSourceKind {
        CoinSourceKind::Simulator
    }

    fn inter_coin_gap(&self) -> Duration {
        self.config.inter_coin_gap
    }

    fn has_buffered_pulses(&self) -> bool {
        self.observation.has_buffered_pulses()
    }

    fn take_buffered_pulses(&mut self) -> Vec<Instant> {
        self.observation.take_pulses()
    }

    fn clear_buffered_pulses(&mut self) {
        self.observation.clear_pulses();
    }
}

impl Drop for SimulatorCoinSource {
    fn drop(&mut self) {
        // Fail-closed: a dropped simulator can never leave the acceptor
        // "asserted" in memory.
        self.set_pin_asserted = false;
    }
}

/// Dedicated pulse sampler thread. Polls the Coin Pin at ~30 ms, performs
/// rising-edge detection only, and records pulse timestamps into a shared
/// buffer. The 500 ms selection loop drains this buffer and never touches
/// the serial pin directly. The thread stops when shutdown is signalled
/// (on `SerialCoinSource` drop).
pub struct PulseSampler {
    join_handle: Option<thread::JoinHandle<()>>,
}

impl PulseSampler {
    fn start(context: SamplerContext, shutdown: Arc<AtomicBool>) -> Self {
        let join_handle = thread::spawn(move || {
            run_sampler(context, shutdown);
        });
        Self {
            join_handle: Some(join_handle),
        }
    }
}

/// Perform one sampler iteration. Keeping this clocked step separate from
/// the production thread makes recovery tests deterministic and avoids wall-
/// clock sleeps in the mock-port tests.
fn run_sampler_step(context: &SamplerContext, recovery: &mut SamplerRecoveryState, now: Instant) {
    let port_present = match context.port.lock() {
        Ok(guard) => guard.is_some(),
        Err(error) => {
            record_sampler_failure(
                context,
                recovery,
                now,
                format!("serial port lock poisoned: {error}"),
            );
            return;
        }
    };

    if !port_present {
        if recovery
            .next_reconnect_at
            .is_some_and(|retry_at| now < retry_at)
        {
            return;
        }

        recovery.reconnect_attempts = recovery.reconnect_attempts.saturating_add(1);
        let attempt = recovery.reconnect_attempts;
        info!(
            target: "openpiso_admin_lib::coin_source",
            attempt,
            error = recovery.last_error.as_deref().unwrap_or("unknown"),
            available = false,
            "Retrying coin port reopen"
        );
        match (context.opener)() {
            Ok(mut reopened) => match write_set_pin(reopened.as_mut(), context.set_pin_line, false)
            {
                Ok(()) => {
                    if let Ok(mut guard) = context.port.lock() {
                        *guard = Some(reopened);
                    } else {
                        info!(
                            target: "openpiso_admin_lib::coin_source",
                            attempt,
                            error = "serial port lock poisoned while installing reopened port",
                            available = false,
                            "Coin port reopen failed"
                        );
                        recovery.next_reconnect_at = Some(now + recovery.retry_delay(attempt));
                        return;
                    }
                    if let Ok(mut obs) = context.observation.lock() {
                        obs.reset();
                    }
                    context.set_pin_asserted.store(false, Ordering::Release);
                    context.sampling_healthy.store(true, Ordering::Release);
                    publish_availability_edge(
                        &context.available,
                        context.audit_sender.as_ref(),
                        true,
                        "serial_reopen",
                    );
                    let previous_error = recovery.last_error.take().unwrap_or_default();
                    info!(
                        target: "openpiso_admin_lib::coin_source",
                        attempt,
                        error = %previous_error,
                        available = true,
                        "Coin port reopened; sampling recovered"
                    );
                    *recovery = SamplerRecoveryState::default();
                }
                Err(error) => {
                    recovery.last_error = Some(error.to_string());
                    recovery.next_reconnect_at = Some(now + recovery.retry_delay(attempt));
                    info!(
                        target: "openpiso_admin_lib::coin_source",
                        attempt,
                        error = %error,
                        available = false,
                        retry_in_ms = recovery.retry_delay(attempt).as_millis() as u64,
                        "Coin port reopen failed"
                    );
                }
            },
            Err(error) => {
                recovery.last_error = Some(error.to_string());
                recovery.next_reconnect_at = Some(now + recovery.retry_delay(attempt));
                info!(
                    target: "openpiso_admin_lib::coin_source",
                    attempt,
                    error = %error,
                    available = false,
                    retry_in_ms = recovery.retry_delay(attempt).as_millis() as u64,
                    "Coin port reopen failed"
                );
            }
        }
        return;
    }

    let read_result = match context.port.lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(port) => read_coin_pin(port.as_mut(), context.coin_pin_line)
                .map_err(|error| error.to_string()),
            None => return,
        },
        Err(error) => Err(format!("serial port lock poisoned: {error}")),
    };

    let pin_level = match read_result {
        Ok(level) => {
            if recovery.consecutive_failures > 0 {
                info!(
                    target: "openpiso_admin_lib::coin_source",
                    consecutive_failures = recovery.consecutive_failures,
                    error = recovery.last_error.as_deref().unwrap_or("unknown"),
                    available = context.available.load(Ordering::Acquire),
                    "Coin port read recovered"
                );
            }
            recovery.consecutive_failures = 0;
            context.sampling_healthy.store(true, Ordering::Release);
            level
        }
        Err(error) => {
            record_sampler_failure(context, recovery, now, error);
            return;
        }
    };

    // Read Set Pin state while holding the same observation lock used to
    // append pulses. An inhibit followed by an observation drain then gives
    // each sampler iteration one clear state: an edge observed after
    // inhibition is admitted only when the existing buffer proves a train is
    // already in flight.
    let observation = if let Ok(mut obs) = context.observation.lock() {
        let set_pin_state = context.set_pin_asserted.load(Ordering::Acquire);
        obs.observe_with_result(pin_level, set_pin_state, now)
    } else {
        PulseObservationResult {
            edge_observed: false,
            pulse_admitted: false,
        }
    };
    if observation.edge_observed && !observation.pulse_admitted {
        let cause = if !context.available.load(Ordering::Acquire)
            || !context.sampling_healthy.load(Ordering::Acquire)
        {
            InhibitedPulseCause::CoinSourceUnavailable
        } else {
            InhibitedPulseCause::NoSelectionWindow
        };
        if context.inhibited_pulse_limiter.should_log_at(now) {
            info!(
                target: "openpiso_admin_lib::coin_source",
                anomaly = "inhibited_pulse",
                reason = cause.reason(),
                "Inhibited pulse"
            );
        }
        if let Some((cause, count)) = context.inhibited_pulse_limiter.finish_window_at(now, cause) {
            publish_sampling_anomaly(context.audit_sender.as_ref(), cause, count);
        }
    }
    for (cause, count) in context.inhibited_pulse_limiter.flush_expired_at(now) {
        publish_sampling_anomaly(context.audit_sender.as_ref(), cause, count);
    }
}

fn record_sampler_failure(
    context: &SamplerContext,
    recovery: &mut SamplerRecoveryState,
    now: Instant,
    error: String,
) {
    recovery.consecutive_failures = recovery.consecutive_failures.saturating_add(1);
    recovery.last_error = Some(error.clone());
    context
        .sampling_fault_generation
        .fetch_add(1, Ordering::AcqRel);
    context.sampling_healthy.store(false, Ordering::Release);
    context.set_pin_asserted.store(false, Ordering::Release);
    if let Ok(mut obs) = context.observation.lock() {
        // A partial pulse train cannot cross a failed/reopened port boundary.
        obs.reset();
    }
    if let Ok(mut guard) = context.port.lock() {
        if let Some(port) = guard.as_mut() {
            if let Err(inhibit_error) = write_set_pin(port.as_mut(), context.set_pin_line, false) {
                publish_availability_edge(
                    &context.available,
                    context.audit_sender.as_ref(),
                    false,
                    "set_pin_inhibition_failed",
                );
                guard.take();
                info!(
                    target: "openpiso_admin_lib::coin_source",
                    error = %inhibit_error,
                    "Coin port fail-closed inhibit failed"
                );
            }
        }
    }
    if recovery.consecutive_failures == 1 {
        info!(
            target: "openpiso_admin_lib::coin_source",
            consecutive_failures = recovery.consecutive_failures,
            error = %error,
            available = context.available.load(Ordering::Acquire),
            "Coin port read failure"
        );
    }
    if recovery.consecutive_failures >= SERIAL_READ_FAILURE_THRESHOLD {
        publish_availability_edge(
            &context.available,
            context.audit_sender.as_ref(),
            false,
            "serial_sampler_failure_threshold",
        );
        if let Ok(mut guard) = context.port.lock() {
            guard.take();
        }
        recovery.next_reconnect_at = Some(now + SERIAL_RECONNECT_BASE_DELAY);
        recovery.reconnect_attempts = 0;
        info!(
            target: "openpiso_admin_lib::coin_source",
            consecutive_failures = recovery.consecutive_failures,
            threshold = SERIAL_READ_FAILURE_THRESHOLD,
            error = %error,
            available = false,
            retry_in_ms = SERIAL_RECONNECT_BASE_DELAY.as_millis() as u64,
            "Coin port failure threshold reached; sampling stopped"
        );
    }
}

/// The sampler thread's main loop. Reopen attempts sleep until their bounded
/// backoff deadline, so an absent port cannot turn into a tight retry loop.
fn run_sampler(context: SamplerContext, shutdown: Arc<AtomicBool>) {
    let mut recovery = SamplerRecoveryState::default();
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let now = Instant::now();
        run_sampler_step(&context, &mut recovery, now);
        let sleep_for = recovery
            .next_reconnect_at
            .map(|retry_at| retry_at.saturating_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.min(SERIAL_RECOVERY_WAKE_INTERVAL))
            .unwrap_or(PULSE_SAMPLER_POLL_MS);
        thread::sleep(sleep_for);
    }
}

/// Real coin acceptor wired to the server's COM port. A dedicated sampler
/// thread polls the **coin pin** (Carrier Detect by default — see ADR-0023 /
/// `docs/hardware.md`) at ~30 ms and records rising edges into a shared buffer.
/// The **set pin** (Request To Send by default) is driven by the main thread.
/// Both lines are owner-editable in M4; the defaults are pinned here for M1.
///
/// `SerialPort` methods require `&mut self`, so the port lives behind a
/// `Mutex` for interior mutability. The lock is held only for the duration
/// of one platform call (negligible). The wrapper itself is `Send`; it
/// does not need to be `Sync` because the consumer owns it.
pub struct SerialCoinSource {
    config: CoinSourceConfig,
    /// Shared serial handle. `None` means the sampler dropped a failed port
    /// and is waiting for the next bounded reopen attempt.
    port: SharedSerialPort,
    /// Shared pulse observation buffer written by the sampler thread and
    /// drained by `try_next_batch_at`.
    observation: Arc<Mutex<PulseObservation>>,
    set_pin_asserted: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
    sampling_healthy: Arc<AtomicBool>,
    sampling_fault_generation: Arc<AtomicU64>,
    inhibited_pulse_limiter: Arc<InhibitedPulseRateLimiter>,
    audit_sender: Option<CoinSourceAuditSender>,
    opener: PortOpener,
    /// Signal the sampler thread to shut down on drop.
    shutdown: Arc<AtomicBool>,
    sampler: Option<PulseSampler>,
    set_pin_line: SetPinLine,
    coin_pin_line: CoinPinLine,
}

impl SerialCoinSource {
    /// Open the serial port and inhibit the set pin before returning
    /// (fail-closed at construction — see ADR-0023). Spawns the dedicated
    /// pulse sampler thread.
    pub fn open(config: CoinSourceConfig, port_name: &str, baud_rate: u32) -> AppResult<Self> {
        Self::open_with_lines(
            config,
            port_name,
            baud_rate,
            SetPinLine::Rts,
            CoinPinLine::Cd,
        )
    }

    pub fn open_with_lines(
        config: CoinSourceConfig,
        port_name: &str,
        baud_rate: u32,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
    ) -> AppResult<Self> {
        Self::open_with_lines_and_polarity(
            config,
            port_name,
            baud_rate,
            set_pin_line,
            coin_pin_line,
            CoinPinPolarity::NormallyOpen,
        )
    }

    pub fn open_with_lines_and_polarity(
        config: CoinSourceConfig,
        port_name: &str,
        baud_rate: u32,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
        polarity: CoinPinPolarity,
    ) -> AppResult<Self> {
        Self::open_with_lines_and_polarity_and_audit_sender(
            config,
            port_name,
            baud_rate,
            set_pin_line,
            coin_pin_line,
            polarity,
            None,
        )
    }

    pub(crate) fn open_with_lines_and_polarity_and_audit_sender(
        config: CoinSourceConfig,
        port_name: &str,
        baud_rate: u32,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
        polarity: CoinPinPolarity,
        audit_sender: Option<CoinSourceAuditSender>,
    ) -> AppResult<Self> {
        config.validate()?;
        let port_name = port_name.to_owned();
        let opener: PortOpener = Arc::new(move || {
            let mut port: SerialPortHandle = serialport::new(&port_name, baud_rate)
                .open()
                .map_err(|error| {
                    AppError::InvalidInput(format!("could not open {port_name}: {error}"))
                })?;
            // Power the coin-pin pull-up before the port is used (initial open
            // and every recovery reopen both route through this opener).
            assert_pullup_dtr(port.as_mut(), set_pin_line)?;
            Ok(port)
        });
        let mut port = opener()?;
        // Drive the set pin to its inhibited level *before* the source is
        // ever observed by the rest of the system.
        write_set_pin(port.as_mut(), set_pin_line, false)?;

        let port = Arc::new(Mutex::new(Some(port)));
        let observation = Arc::new(Mutex::new(PulseObservation::with_polarity(polarity)));
        let set_pin_asserted = Arc::new(AtomicBool::new(false));
        let available = Arc::new(AtomicBool::new(true));
        let sampling_healthy = Arc::new(AtomicBool::new(true));
        let sampling_fault_generation = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let inhibited_pulse_limiter = Arc::new(InhibitedPulseRateLimiter::new());
        let sampler = PulseSampler::start(
            SamplerContext {
                port: Arc::clone(&port),
                set_pin_line,
                coin_pin_line,
                observation: Arc::clone(&observation),
                set_pin_asserted: Arc::clone(&set_pin_asserted),
                available: Arc::clone(&available),
                sampling_healthy: Arc::clone(&sampling_healthy),
                sampling_fault_generation: Arc::clone(&sampling_fault_generation),
                inhibited_pulse_limiter: Arc::clone(&inhibited_pulse_limiter),
                audit_sender: audit_sender.clone(),
                opener: Arc::clone(&opener),
            },
            Arc::clone(&shutdown),
        );

        Ok(Self {
            config,
            port,
            observation,
            set_pin_asserted,
            available,
            sampling_healthy,
            sampling_fault_generation,
            inhibited_pulse_limiter,
            audit_sender,
            opener,
            shutdown,
            sampler: Some(sampler),
            set_pin_line,
            coin_pin_line,
        })
    }

    /// Construct from a pre-built (typically mocked) port without spawning
    /// the sampler thread (test-only — tests drive edge detection directly).
    #[cfg(test)]
    fn from_port(config: CoinSourceConfig, port: Box<dyn SerialPort + Send>) -> AppResult<Self> {
        Self::from_port_with_lines(config, port, SetPinLine::Rts, CoinPinLine::Cd)
    }

    #[cfg(test)]
    fn from_port_with_lines(
        config: CoinSourceConfig,
        port: Box<dyn SerialPort + Send>,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
    ) -> AppResult<Self> {
        Self::from_port_with_lines_and_polarity(
            config,
            port,
            set_pin_line,
            coin_pin_line,
            CoinPinPolarity::NormallyOpen,
        )
    }

    #[cfg(test)]
    fn from_port_with_lines_and_polarity(
        config: CoinSourceConfig,
        port: Box<dyn SerialPort + Send>,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
        polarity: CoinPinPolarity,
    ) -> AppResult<Self> {
        let opener: PortOpener = Arc::new(|| {
            Err(AppError::Conflict(
                "test port opener not configured".to_owned(),
            ))
        });
        Self::from_port_with_lines_and_polarity_and_opener(
            config,
            port,
            set_pin_line,
            coin_pin_line,
            polarity,
            opener,
        )
    }

    #[cfg(test)]
    fn from_port_with_lines_and_polarity_and_opener(
        config: CoinSourceConfig,
        mut port: Box<dyn SerialPort + Send>,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
        polarity: CoinPinPolarity,
        opener: PortOpener,
    ) -> AppResult<Self> {
        config.validate()?;
        write_set_pin(port.as_mut(), set_pin_line, false)?;
        Ok(Self {
            config,
            port: Arc::new(Mutex::new(Some(port))),
            observation: Arc::new(Mutex::new(PulseObservation::with_polarity(polarity))),
            set_pin_asserted: Arc::new(AtomicBool::new(false)),
            available: Arc::new(AtomicBool::new(true)),
            sampling_healthy: Arc::new(AtomicBool::new(true)),
            sampling_fault_generation: Arc::new(AtomicU64::new(0)),
            inhibited_pulse_limiter: Arc::new(InhibitedPulseRateLimiter::new()),
            audit_sender: None,
            opener,
            shutdown: Arc::new(AtomicBool::new(false)),
            sampler: None,
            set_pin_line,
            coin_pin_line,
        })
    }

    /// Run one sampler iteration with an injected clock (test-only).
    #[cfg(test)]
    fn sampler_step_at(&mut self, now: Instant, recovery: &mut SamplerRecoveryState) {
        let context = SamplerContext {
            port: Arc::clone(&self.port),
            set_pin_line: self.set_pin_line,
            coin_pin_line: self.coin_pin_line,
            observation: Arc::clone(&self.observation),
            set_pin_asserted: Arc::clone(&self.set_pin_asserted),
            available: Arc::clone(&self.available),
            sampling_healthy: Arc::clone(&self.sampling_healthy),
            sampling_fault_generation: Arc::clone(&self.sampling_fault_generation),
            inhibited_pulse_limiter: Arc::clone(&self.inhibited_pulse_limiter),
            audit_sender: self.audit_sender.clone(),
            opener: Arc::clone(&self.opener),
        };
        run_sampler_step(&context, recovery, now);
    }

    /// Inject a pulse timestamp directly into the shared observation buffer
    /// (test-only, bypasses the sampler thread).
    #[cfg(test)]
    pub(crate) fn inject_pulse_at(&mut self, at: Instant) {
        if let Ok(mut obs) = self.observation.lock() {
            obs.inject_pulse_at(at);
        }
    }
}

impl CoinSource for SerialCoinSource {
    fn try_next_batch_at(&mut self, now: Instant) -> Result<CoinBatchRead, AppError> {
        // The sampler thread handles all pin reads and edge detection.
        // This method only drains the shared observation buffer.
        if !self.available.load(Ordering::Acquire) || !self.sampling_healthy.load(Ordering::Acquire)
        {
            return Err(AppError::InvalidInput(
                "coin pin sampler is unavailable during serial recovery".to_owned(),
            ));
        }
        let mut observation = self
            .observation
            .lock()
            .map_err(|_| AppError::Conflict("observation state poisoned".to_owned()))?;
        observation.close_batch_if_gap_elapsed(now, &self.config)
    }

    fn assert_set_pin(&mut self) -> Result<(), AppError> {
        if !self.available.load(Ordering::Acquire) || !self.sampling_healthy.load(Ordering::Acquire)
        {
            return Err(AppError::Conflict(
                "coin source is unavailable during serial recovery".to_owned(),
            ));
        }
        let mut port_guard = self
            .port
            .lock()
            .map_err(|_| AppError::Conflict("serial port lock poisoned".to_owned()))?;
        let Some(port) = port_guard.as_mut() else {
            publish_availability_edge(
                &self.available,
                self.audit_sender.as_ref(),
                false,
                "set_pin_assertion_during_recovery",
            );
            self.set_pin_asserted.store(false, Ordering::Release);
            return Err(AppError::Conflict(
                "coin source is unavailable during serial recovery".to_owned(),
            ));
        };
        if let Err(error) = write_set_pin(port.as_mut(), self.set_pin_line, true) {
            publish_availability_edge(
                &self.available,
                self.audit_sender.as_ref(),
                false,
                "set_pin_assertion_failed",
            );
            self.set_pin_asserted.store(false, Ordering::Release);
            self.sampling_fault_generation
                .fetch_add(1, Ordering::AcqRel);
            self.sampling_healthy.store(false, Ordering::Release);
            port_guard.take();
            info!(
                target: "openpiso_admin_lib::coin_source",
                error = %error,
                available = false,
                "Set Pin assertion failed"
            );
            return Err(error);
        }
        self.set_pin_asserted.store(true, Ordering::Release);
        info!(
            target: "openpiso_admin_lib::coin_source",
            asserted = true,
            line = ?self.set_pin_line,
            cause = "serial::assert",
            "Set Pin asserted"
        );
        Ok(())
    }

    fn inhibit_set_pin(&mut self) -> Result<(), AppError> {
        let mut port_guard = self
            .port
            .lock()
            .map_err(|_| AppError::Conflict("serial port lock poisoned".to_owned()))?;
        let Some(port) = port_guard.as_mut() else {
            publish_availability_edge(
                &self.available,
                self.audit_sender.as_ref(),
                false,
                "set_pin_inhibition_during_recovery",
            );
            self.set_pin_asserted.store(false, Ordering::Release);
            return Err(AppError::Conflict(
                "coin source is unavailable during serial recovery".to_owned(),
            ));
        };
        if let Err(error) = write_set_pin(port.as_mut(), self.set_pin_line, false) {
            publish_availability_edge(
                &self.available,
                self.audit_sender.as_ref(),
                false,
                "set_pin_inhibition_failed",
            );
            self.set_pin_asserted.store(false, Ordering::Release);
            self.sampling_fault_generation
                .fetch_add(1, Ordering::AcqRel);
            self.sampling_healthy.store(false, Ordering::Release);
            port_guard.take();
            info!(
                target: "openpiso_admin_lib::coin_source",
                error = %error,
                available = false,
                "Set Pin inhibition failed"
            );
            return Err(error);
        }
        self.set_pin_asserted.store(false, Ordering::Release);
        info!(
            target: "openpiso_admin_lib::coin_source",
            asserted = false,
            line = ?self.set_pin_line,
            cause = "serial::inhibit",
            "Set Pin inhibited"
        );
        Ok(())
    }

    fn is_set_pin_asserted(&self) -> bool {
        self.set_pin_asserted.load(Ordering::Acquire)
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
            && self.port.lock().map(|port| port.is_some()).unwrap_or(false)
    }

    fn is_sampling_healthy(&self) -> bool {
        self.sampling_healthy.load(Ordering::Acquire)
    }

    fn sampling_fault_generation(&self) -> u64 {
        self.sampling_fault_generation.load(Ordering::Acquire)
    }

    fn kind(&self) -> CoinSourceKind {
        CoinSourceKind::Serial
    }

    fn inter_coin_gap(&self) -> Duration {
        self.config.inter_coin_gap
    }

    fn has_buffered_pulses(&self) -> bool {
        self.observation
            .lock()
            .map(|o| o.has_buffered_pulses())
            .unwrap_or(false)
    }

    fn take_buffered_pulses(&mut self) -> Vec<Instant> {
        self.observation
            .lock()
            .map(|mut observation| observation.take_pulses())
            .unwrap_or_default()
    }

    fn clear_buffered_pulses(&mut self) {
        if let Ok(mut observation) = self.observation.lock() {
            observation.clear_pulses();
        }
    }
}

impl Drop for SerialCoinSource {
    fn drop(&mut self) {
        // Signal the sampler thread to stop and wait for it to finish.
        self.shutdown.store(true, Ordering::Release);
        if let Some(mut sampler) = self.sampler.take() {
            if let Some(handle) = sampler.join_handle.take() {
                let _ = handle.join();
            }
        }
        // Fail-closed: a dropped serial source leaves the acceptor
        // inhibited. We swallow the error and log it; we cannot return it
        // from `Drop`.
        // The port is behind an Arc; a failed lock means another holder
        // may still be accessing it — we just skip the inhibit write.
        match self.port.lock() {
            Ok(mut port) => {
                if let Some(port) = port.as_mut() {
                    if let Err(error) = write_set_pin(port.as_mut(), self.set_pin_line, false) {
                        warn!(
                            target: "openpiso_admin_lib::coin_source",
                            "could not inhibit set pin on drop: {error}"
                        );
                    }
                }
            }
            Err(error) => warn!(
                target: "openpiso_admin_lib::coin_source",
                "serial port lock poisoned on drop: {error}"
            ),
        }
        self.set_pin_asserted.store(false, Ordering::Release);
    }
}

/// Assert DTR as a steady voltage source for the coin-pin pull-up. Field-proven
/// (2026-07-25 line probe): this acceptor wiring powers the coin line's pull-up
/// from DTR.
/// With DTR low the coin line floats and emits ~200 Hz noise (phantom credits)
/// while real coin pulses never register; with DTR high the line is silent at
/// idle and coins read cleanly. Skipped when DTR is itself the set/enable pin,
/// which `write_set_pin` already drives.
fn assert_pullup_dtr(port: &mut dyn SerialPort, set_pin_line: SetPinLine) -> AppResult<()> {
    if set_pin_line == SetPinLine::Dtr {
        return Ok(());
    }
    port.write_data_terminal_ready(true)
        .map_err(|error| AppError::InvalidInput(format!("could not assert DTR pull-up: {error}")))
}

fn write_set_pin(port: &mut dyn SerialPort, line: SetPinLine, asserted: bool) -> AppResult<()> {
    let result = match line {
        SetPinLine::Rts => port.write_request_to_send(asserted),
        SetPinLine::Dtr => port.write_data_terminal_ready(asserted),
    };
    result.map_err(|error| {
        AppError::InvalidInput(format!(
            "could not {} set pin: {error}",
            if asserted { "assert" } else { "inhibit" }
        ))
    })
}

fn read_coin_pin(port: &mut dyn SerialPort, line: CoinPinLine) -> serialport::Result<bool> {
    match line {
        CoinPinLine::Cts => port.read_clear_to_send(),
        CoinPinLine::Dsr => port.read_data_set_ready(),
        CoinPinLine::Ri => port.read_ring_indicator(),
        CoinPinLine::Cd => port.read_carrier_detect(),
    }
}

pub enum ConfiguredCoinSource {
    Simulator(SimulatorCoinSource),
    Serial(SerialCoinSource),
    Unavailable,
}

impl ConfiguredCoinSource {
    pub fn from_settings(
        config: CoinSourceConfig,
        settings: &crate::models::CoinDeviceSettings,
    ) -> AppResult<Self> {
        Self::from_settings_with_audit_sender(config, settings, None)
    }

    pub(crate) fn from_settings_with_audit_sender(
        config: CoinSourceConfig,
        settings: &crate::models::CoinDeviceSettings,
        audit_sender: Option<CoinSourceAuditSender>,
    ) -> AppResult<Self> {
        let polarity = CoinPinPolarity::parse(&settings.coin_pin_polarity)?;
        let Some(port_name) = settings.com_port.as_deref() else {
            return Self::simulator(config);
        };
        let baud_rate = u32::try_from(settings.baud_rate)
            .map_err(|_| AppError::InvalidInput("baud rate is too large".to_owned()))?;
        match Self::serial_with_polarity_and_audit_sender(
            config,
            port_name,
            baud_rate,
            SetPinLine::parse(&settings.set_pin_line)?,
            CoinPinLine::parse(&settings.coin_pin_line)?,
            polarity,
            audit_sender,
        ) {
            Ok(source) => Ok(source),
            Err(error) => {
                info!(
                    target: "openpiso_admin_lib::coin_source",
                    port = %port_name,
                    error = %error,
                    available = false,
                    "Coin source unavailable"
                );
                Ok(Self::Unavailable)
            }
        }
    }

    pub fn simulator(config: CoinSourceConfig) -> AppResult<Self> {
        SimulatorCoinSource::new(config).map(Self::Simulator)
    }

    pub fn serial(
        config: CoinSourceConfig,
        port_name: &str,
        baud_rate: u32,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
    ) -> AppResult<Self> {
        Self::serial_with_polarity(
            config,
            port_name,
            baud_rate,
            set_pin_line,
            coin_pin_line,
            CoinPinPolarity::NormallyOpen,
        )
    }

    pub fn serial_with_polarity(
        config: CoinSourceConfig,
        port_name: &str,
        baud_rate: u32,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
        polarity: CoinPinPolarity,
    ) -> AppResult<Self> {
        Self::serial_with_polarity_and_audit_sender(
            config,
            port_name,
            baud_rate,
            set_pin_line,
            coin_pin_line,
            polarity,
            None,
        )
    }

    pub(crate) fn serial_with_polarity_and_audit_sender(
        config: CoinSourceConfig,
        port_name: &str,
        baud_rate: u32,
        set_pin_line: SetPinLine,
        coin_pin_line: CoinPinLine,
        polarity: CoinPinPolarity,
        audit_sender: Option<CoinSourceAuditSender>,
    ) -> AppResult<Self> {
        SerialCoinSource::open_with_lines_and_polarity_and_audit_sender(
            config,
            port_name,
            baud_rate,
            set_pin_line,
            coin_pin_line,
            polarity,
            audit_sender,
        )
        .map(Self::Serial)
    }

    pub fn inject_simulator_pulse_at(&mut self, at: Instant) -> AppResult<()> {
        match self {
            Self::Simulator(source) => {
                source.inject_pulse_at(at);
                Ok(())
            }
            Self::Serial(_) => Err(AppError::Conflict(
                "coin simulator is unavailable while serial hardware is configured".to_owned(),
            )),
            Self::Unavailable => Err(AppError::Conflict(
                "coin source is being reconfigured".to_owned(),
            )),
        }
    }
}

impl CoinSource for ConfiguredCoinSource {
    fn try_next_batch_at(&mut self, now: Instant) -> Result<CoinBatchRead, AppError> {
        match self {
            Self::Simulator(source) => source.try_next_batch_at(now),
            Self::Serial(source) => source.try_next_batch_at(now),
            Self::Unavailable => Ok(CoinBatchRead::None),
        }
    }

    fn assert_set_pin(&mut self) -> Result<(), AppError> {
        match self {
            Self::Simulator(source) => source.assert_set_pin(),
            Self::Serial(source) => source.assert_set_pin(),
            Self::Unavailable => Err(AppError::Conflict("coin source is unavailable".to_owned())),
        }
    }

    fn inhibit_set_pin(&mut self) -> Result<(), AppError> {
        match self {
            Self::Simulator(source) => source.inhibit_set_pin(),
            Self::Serial(source) => source.inhibit_set_pin(),
            Self::Unavailable => Ok(()),
        }
    }

    fn is_set_pin_asserted(&self) -> bool {
        match self {
            Self::Simulator(source) => source.is_set_pin_asserted(),
            Self::Serial(source) => source.is_set_pin_asserted(),
            Self::Unavailable => false,
        }
    }

    fn is_available(&self) -> bool {
        match self {
            Self::Simulator(source) => source.is_available(),
            Self::Serial(source) => source.is_available(),
            Self::Unavailable => false,
        }
    }

    fn is_sampling_healthy(&self) -> bool {
        match self {
            Self::Simulator(source) => source.is_sampling_healthy(),
            Self::Serial(source) => source.is_sampling_healthy(),
            Self::Unavailable => false,
        }
    }

    fn sampling_fault_generation(&self) -> u64 {
        match self {
            Self::Simulator(source) => source.sampling_fault_generation(),
            Self::Serial(source) => source.sampling_fault_generation(),
            Self::Unavailable => 0,
        }
    }

    fn kind(&self) -> CoinSourceKind {
        match self {
            Self::Simulator(source) => source.kind(),
            Self::Serial(source) => source.kind(),
            Self::Unavailable => CoinSourceKind::Serial,
        }
    }

    fn inter_coin_gap(&self) -> Duration {
        match self {
            Self::Simulator(source) => source.inter_coin_gap(),
            Self::Serial(source) => source.inter_coin_gap(),
            Self::Unavailable => DEFAULT_INTER_COIN_GAP,
        }
    }

    fn has_buffered_pulses(&self) -> bool {
        match self {
            Self::Simulator(source) => source.has_buffered_pulses(),
            Self::Serial(source) => source.has_buffered_pulses(),
            Self::Unavailable => false,
        }
    }

    fn take_buffered_pulses(&mut self) -> Vec<Instant> {
        match self {
            Self::Simulator(source) => source.take_buffered_pulses(),
            Self::Serial(source) => source.take_buffered_pulses(),
            Self::Unavailable => Vec::new(),
        }
    }

    fn clear_buffered_pulses(&mut self) {
        match self {
            Self::Simulator(source) => source.clear_buffered_pulses(),
            Self::Serial(source) => source.clear_buffered_pulses(),
            Self::Unavailable => {}
        }
    }
}

/// Helper: compute microsecond offsets from the first pulse for logging.
fn timing_offsets_relative(timings: &[Instant]) -> Vec<u64> {
    let Some(first) = timings.first() else {
        return vec![];
    };
    timings
        .iter()
        .map(|t| t.duration_since(*first).as_micros() as u64)
        .collect()
}

/// Close the current batch (if any) once the inter-coin gap has elapsed
/// since the last pulse. Returns [`CoinBatchRead::None`] while pulses are
/// still arriving within the gap. Every positive pulse count closes as a
/// batch; the amount is deliberately not computed here or read from the
/// persisted Pulse Map.
fn close_batch_if_gap_elapsed(
    pulses: &mut Vec<Instant>,
    now: Instant,
    config: &CoinSourceConfig,
) -> Result<CoinBatchRead, AppError> {
    if pulses.is_empty() {
        return Ok(CoinBatchRead::None);
    }
    let Some(last) = pulses.last().copied() else {
        return Ok(CoinBatchRead::None);
    };
    let gap = now.duration_since(last);
    if gap < config.inter_coin_gap {
        return Ok(CoinBatchRead::None);
    }
    let pulse_timings = std::mem::take(pulses);
    let pulse_count = i64::try_from(pulse_timings.len())
        .map_err(|_| AppError::InvalidInput("pulse count exceeds integer authority".to_owned()))?;
    let span = pulse_timings
        .last()
        .and_then(|last| {
            pulse_timings
                .first()
                .map(|first| last.duration_since(*first))
        })
        .unwrap_or(Duration::ZERO);
    let timings_us = timing_offsets_relative(&pulse_timings);

    Ok(CoinBatchRead::Batch(CoinBatch {
        coin_trace_id: Uuid::new_v4(),
        pulse_count,
        pulse_timings_us: timings_us,
        span_us: span.as_micros() as u64,
        gap_us: config.inter_coin_gap.as_micros() as u64,
        attempt: 1,
    }))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        fmt, io,
        sync::Arc,
    };

    use super::*;
    use tracing::{field::Visit, Subscriber};
    use tracing_subscriber::{layer::Context, prelude::*, registry::LookupSpan, Layer};

    #[derive(Clone, Default)]
    struct EventCapture(Arc<Mutex<Vec<CapturedEvent>>>);

    #[derive(Debug)]
    struct CapturedEvent {
        target: String,
        fields: HashMap<String, String>,
    }

    struct FieldVisitor<'a>(&'a mut HashMap<String, String>);

    impl Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    impl<S> Layer<S> for EventCapture
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            let mut fields = HashMap::new();
            event.record(&mut FieldVisitor(&mut fields));
            self.0.lock().expect("captured events").push(CapturedEvent {
                target: event.metadata().target().to_owned(),
                fields,
            });
        }
    }

    fn default_config() -> CoinSourceConfig {
        CoinSourceConfig::default_config()
    }

    fn expect_batch(outcome: CoinBatchRead) -> CoinBatch {
        match outcome {
            CoinBatchRead::Batch(batch) => batch,
            other => panic!("expected a credited CoinBatch, got {other:?}"),
        }
    }

    #[test]
    fn default_config_is_valid_and_matches_bench_map() {
        let config = CoinSourceConfig::default_config();
        config.validate().expect("defaults should be valid");
        assert_eq!(config.inter_coin_gap, DEFAULT_INTER_COIN_GAP);
    }

    // ── PulseObservation direct tests ────────────────────────────────

    #[test]
    fn pulse_observation_starts_empty_and_pin_low() {
        let mut obs = PulseObservation::new();
        assert!(!obs.has_buffered_pulses());
        assert_eq!(obs.pulse_count(), 0);
        // A low pin on the first call does not record an edge.
        assert!(!obs.observe(false, true, Instant::now()));
        assert!(!obs.has_buffered_pulses());
    }

    #[test]
    fn first_idle_high_sample_seeds_without_pulse_or_credit() {
        let config = default_config();
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();

        assert!(!obs.observe(true, true, t0));
        assert_eq!(obs.pulse_count(), 0);
        assert_eq!(
            obs.close_batch_if_gap_elapsed(t0 + config.inter_coin_gap, &config)
                .expect("seeded idle line is not an error"),
            CoinBatchRead::None
        );

        // A low sample followed by a high sample is a real transition after
        // seeding and is admitted while the Set Pin is asserted.
        assert!(!obs.observe(false, true, t0 + Duration::from_millis(1)));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(2)));
        assert_eq!(obs.pulse_count(), 1);
    }

    #[test]
    fn coin_pin_polarity_rejects_invalid_persisted_value() {
        assert!(matches!(
            CoinPinPolarity::parse("backwards"),
            Err(AppError::InvalidInput(message))
                if message == "coin pin polarity must be normally_open or normally_closed"
        ));
    }

    #[test]
    fn normally_open_observation_preserves_rising_edge_behavior() {
        let mut obs = PulseObservation::with_polarity(CoinPinPolarity::NormallyOpen);
        let t0 = Instant::now();
        assert!(!obs.observe(false, true, t0));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(1)));
        assert!(!obs.observe(true, true, t0 + Duration::from_millis(2)));
        assert_eq!(obs.pulse_count(), 1);
    }

    #[test]
    fn normally_closed_observation_normalizes_idle_high_and_low_pulse() {
        let mut obs = PulseObservation::with_polarity(CoinPinPolarity::NormallyClosed);
        let t0 = Instant::now();

        // NC idle is high: it must not look like an asserted pulse.
        assert!(!obs.observe(true, true, t0));
        // A coin pulls the line low, which normalizes to one asserted edge.
        assert!(obs.observe(false, true, t0 + Duration::from_millis(10)));
        assert!(!obs.observe(false, true, t0 + Duration::from_millis(20)));
        // Returning high ends the pulse without creating another one.
        assert!(!obs.observe(true, true, t0 + Duration::from_millis(30)));
        assert_eq!(obs.pulse_count(), 1);
    }

    #[test]
    fn pulse_observation_records_rising_edge_when_set_pin_asserted() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        // The first sample seeds the line; the following low→high transition
        // with Set Pin asserted is recorded.
        assert!(!obs.observe(false, true, t0));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(1)));
        assert!(obs.has_buffered_pulses());
        assert_eq!(obs.pulse_count(), 1);
        // A second call with the same high pin is not a rising edge.
        assert!(!obs.observe(true, true, t0 + Duration::from_millis(2)));
        assert_eq!(obs.pulse_count(), 1, "only one pulse recorded");
    }

    #[test]
    fn pulse_observation_does_not_record_when_set_pin_inhibited() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        // Seed low, then observe a rising edge while Set Pin is not asserted.
        assert!(!obs.observe(false, false, t0));
        assert!(obs.observe(true, false, t0 + Duration::from_millis(1)));
        assert!(!obs.has_buffered_pulses(), "no accumulation when inhibited");
        // Subsequent rising edge with asserted Set Pin does record.
        // First fall (to reset edge detection), then rise.
        obs.observe(false, false, t0 + Duration::from_millis(2));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(3)));
        assert!(obs.has_buffered_pulses());
        assert_eq!(obs.pulse_count(), 1);
    }

    #[test]
    fn pulse_observation_rising_edge_returned_regardless_of_set_pin() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        // The first sample seeds state even when Set Pin is not asserted.
        assert!(!obs.observe(false, false, t0));
        // A later rising edge is reported regardless of Set Pin.
        assert!(obs.observe(true, false, t0 + Duration::from_millis(1)));
        // High → low is not a rising edge.
        assert!(!obs.observe(false, false, t0 + Duration::from_millis(2)));
    }

    #[test]
    fn pulse_observation_admits_straddling_train_after_inhibit() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        obs.observe(false, true, t0);

        let first = obs.observe_with_result(true, true, t0 + Duration::from_millis(1));
        assert_eq!(
            first,
            PulseObservationResult {
                edge_observed: true,
                pulse_admitted: true,
            }
        );
        obs.observe(false, true, t0 + Duration::from_millis(2));

        // The Set Pin is now inhibited, but the first pulse proves a Coin
        // Batch is already buffered, so the train remains one batch.
        let straddling = obs.observe_with_result(true, false, t0 + Duration::from_millis(3));
        assert_eq!(
            straddling,
            PulseObservationResult {
                edge_observed: true,
                pulse_admitted: true,
            }
        );
        assert_eq!(obs.pulse_count(), 2);
    }

    #[test]
    fn pulse_observation_refuses_inhibited_edge_with_empty_buffer() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        obs.observe(false, false, t0);

        let refused = obs.observe_with_result(true, false, t0 + Duration::from_millis(1));
        assert_eq!(
            refused,
            PulseObservationResult {
                edge_observed: true,
                pulse_admitted: false,
            }
        );
        assert_eq!(obs.pulse_count(), 0);
    }

    #[test]
    fn pulse_observation_inject_pulse_bypasses_edge_detection() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        obs.inject_pulse_at(t0);
        assert!(obs.has_buffered_pulses());
        assert_eq!(obs.pulse_count(), 1);
    }

    #[test]
    fn pulse_observation_close_batch_if_gap_elapsed_uses_injected_pulses() {
        let config = default_config();
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        obs.inject_pulse_at(t0);
        // Within gap → no batch.
        assert_eq!(
            obs.close_batch_if_gap_elapsed(t0 + Duration::from_millis(50), &config)
                .expect("within-gap call is not an error"),
            CoinBatchRead::None
        );
        assert!(obs.has_buffered_pulses(), "buffer preserved within gap");
        // After gap → batch closes.
        let batch = expect_batch(
            obs.close_batch_if_gap_elapsed(
                t0 + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10),
                &config,
            )
            .expect("batch should close"),
        );
        assert_eq!(batch.pulse_count, 1);
        assert_eq!(batch.checked_amount_cents().unwrap(), 100);
        assert!(!obs.has_buffered_pulses(), "buffer cleared after close");
    }

    #[test]
    fn pulse_observation_reset_clears_state() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        obs.observe(false, true, t0);
        obs.observe(true, true, t0 + Duration::from_millis(1));
        obs.observe(false, true, t0 + Duration::from_millis(10));
        obs.observe(true, true, t0 + Duration::from_millis(20));
        assert_eq!(obs.pulse_count(), 2);
        obs.reset();
        assert_eq!(obs.pulse_count(), 0);
        assert!(!obs.has_buffered_pulses());
        // After reset, the first sample seeds again rather than creating an
        // edge; only the following low→high transition is admitted.
        assert!(!obs.observe(true, true, t0));
        assert!(!obs.observe(false, true, t0 + Duration::from_millis(1)));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(2)));
    }

    #[test]
    fn pulse_observation_tracks_multiple_falling_rising_cycles() {
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        // Seed low, then rise (recorded), fall, rise (recorded), fall, rise
        // (recorded).
        assert!(!obs.observe(false, true, t0));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(1)));
        assert!(!obs.observe(false, true, t0 + Duration::from_millis(10)));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(20)));
        assert!(!obs.observe(false, true, t0 + Duration::from_millis(30)));
        assert!(obs.observe(true, true, t0 + Duration::from_millis(40)));
        assert!(!obs.observe(false, true, t0 + Duration::from_millis(50)));
        assert_eq!(obs.pulse_count(), 3);
    }

    #[test]
    fn pulse_observation_accumulated_pulses_via_observe_are_drained_by_close() {
        let config = default_config();
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        // Seed the line, then observe three pulses (with Set Pin asserted).
        obs.observe(false, true, t0);
        obs.observe(true, true, t0 + Duration::from_millis(1));
        obs.observe(false, true, t0 + Duration::from_millis(10));
        obs.observe(true, true, t0 + Duration::from_millis(20));
        obs.observe(false, true, t0 + Duration::from_millis(30));
        obs.observe(true, true, t0 + Duration::from_millis(40));
        obs.observe(false, true, t0 + Duration::from_millis(50));
        assert_eq!(obs.pulse_count(), 3);
        let after =
            t0 + Duration::from_millis(50) + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            obs.close_batch_if_gap_elapsed(after, &config)
                .expect("three-pulse close is not an error"),
        );
        assert_eq!(batch.pulse_count, 3);
        assert_eq!(batch.checked_amount_cents().unwrap(), 300);
        assert!(!obs.has_buffered_pulses(), "buffer cleared after close");
    }

    #[test]
    fn simulator_is_inhibited_at_construction() {
        let source = SimulatorCoinSource::new(default_config()).expect("valid config");
        assert!(!source.is_set_pin_asserted());
    }

    #[test]
    fn simulator_is_always_available() {
        // The simulator is a pure in-memory stand-in and never depends on
        // hardware state, so the kiosk must always see it as available.
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        assert!(source.is_available());
        source.assert_set_pin().expect("assert should succeed");
        assert!(
            source.is_available(),
            "asserting the set pin does not affect availability"
        );
    }

    #[test]
    fn serial_is_available_against_an_open_mock_port() {
        // The serial source's availability is tied to the OS handle. The
        // mock port is healthy; this test pins the contract so M4's
        // port-closed failure path can be added without regressing the
        // happy path.
        let (mock, _shared) = MockSerialPort::with_readings([]);
        let source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        assert!(source.is_available());
    }

    #[test]
    fn simulator_set_pin_transitions_round_trip() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        assert!(!source.is_set_pin_asserted());
        source.assert_set_pin().expect("assert should succeed");
        assert!(source.is_set_pin_asserted());
        source.inhibit_set_pin().expect("inhibit should succeed");
        assert!(!source.is_set_pin_asserted());
        // Inhibit is idempotent.
        source.inhibit_set_pin().expect("inhibit is idempotent");
        assert!(!source.is_set_pin_asserted());
    }

    #[test]
    fn single_pulse_maps_to_one_peso() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        let start = Instant::now();
        source.inject_pulse_at(start);
        // Within the gap: no batch yet.
        let mid = start + Duration::from_millis(50);
        assert_eq!(
            source
                .try_next_batch_at(mid)
                .expect("within-gap call is not an error"),
            CoinBatchRead::None
        );
        // After the gap: the batch is closed and mapped.
        let after = start + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            source
                .try_next_batch_at(after)
                .expect("post-gap call is not an error"),
        );
        assert_eq!(batch.pulse_count, 1);
        assert_eq!(batch.checked_amount_cents().unwrap(), 100);
        // The source is empty after returning a batch.
        assert_eq!(
            source
                .try_next_batch_at(after)
                .expect("empty call is not an error"),
            CoinBatchRead::None
        );
    }

    #[test]
    fn five_pulses_map_to_five_pesos() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        let start = Instant::now();
        for offset_ms in [0, 20, 40, 60, 80] {
            source.inject_pulse_at(start + Duration::from_millis(offset_ms));
        }
        // The gap is measured from the *last* pulse (at +80 ms), so a poll
        // must be at least 200 ms after that, not after `start`.
        let after =
            start + Duration::from_millis(80) + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            source
                .try_next_batch_at(after)
                .expect("post-gap call is not an error"),
        );
        assert_eq!(batch.pulse_count, 5);
        assert_eq!(batch.checked_amount_cents().unwrap(), 500);
        assert_eq!(
            batch.pulse_timings_us,
            vec![0, 20_000, 40_000, 60_000, 80_000]
        );
        assert_eq!(batch.span_us, 80_000);
        assert_eq!(batch.gap_us, DEFAULT_INTER_COIN_GAP.as_micros() as u64);
        assert_eq!(batch.attempt, 1);
    }

    #[test]
    fn pulses_within_gap_accumulate_into_one_batch() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        let start = Instant::now();
        // Three pulses, all well within the gap.
        source.inject_pulse_at(start);
        source.inject_pulse_at(start + Duration::from_millis(20));
        source.inject_pulse_at(start + Duration::from_millis(40));
        // Polling throughout the gap (relative to the last pulse at +40 ms)
        // returns no batch yet.
        let last_pulse = start + Duration::from_millis(40);
        let gap_end = last_pulse + DEFAULT_INTER_COIN_GAP;
        let mid_gap = last_pulse + DEFAULT_INTER_COIN_GAP / 2;
        assert_eq!(
            source
                .try_next_batch_at(mid_gap)
                .expect("within-gap call is not an error"),
            CoinBatchRead::None
        );
        // A poll just before the gap closes still returns None.
        let just_before_close = gap_end - Duration::from_millis(1);
        assert_eq!(
            source
                .try_next_batch_at(just_before_close)
                .expect("just-before-close call is not an error"),
            CoinBatchRead::None
        );
        // After the gap elapses, every positive pulse count closes as a batch.
        let after = gap_end + Duration::from_millis(10);
        let batch = expect_batch(
            source
                .try_next_batch_at(after)
                .expect("arbitrary pulse count is not an error"),
        );
        assert_eq!(batch.pulse_count, 3);
        assert_eq!(batch.checked_amount_cents().unwrap(), 300);
    }

    #[test]
    fn arbitrary_pulse_count_closes_and_uses_fixed_authority() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        let start = Instant::now();
        for offset_ms in 0..7 {
            source.inject_pulse_at(start + Duration::from_millis(offset_ms * 10));
        }
        let after =
            start + Duration::from_millis(60) + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            source
                .try_next_batch_at(after)
                .expect("arbitrary pulse count is not an error"),
        );
        assert_eq!(batch.pulse_count, 7);
        assert_eq!(batch.checked_amount_cents().unwrap(), 700);
        assert_eq!(
            source
                .try_next_batch_at(after)
                .expect("buffer is empty after close"),
            CoinBatchRead::None
        );
    }

    #[test]
    fn gap_resets_after_each_closed_batch() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        let t0 = Instant::now();
        // First batch: one pulse.
        source.inject_pulse_at(t0);
        let after_first = t0 + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let first = expect_batch(
            source
                .try_next_batch_at(after_first)
                .expect("first batch is not an error"),
        );
        assert_eq!(first.pulse_count, 1);
        assert_eq!(first.checked_amount_cents().unwrap(), 100);
        // Second batch starts a fresh timer from its own last pulse.
        source.inject_pulse_at(after_first);
        source.inject_pulse_at(after_first + Duration::from_millis(10));
        // A poll at +15 ms after the *second* pulse is still inside the
        // second batch's gap.
        let mid_second = after_first + Duration::from_millis(15);
        assert_eq!(
            source
                .try_next_batch_at(mid_second)
                .expect("mid-second call is not an error"),
            CoinBatchRead::None
        );
        // After the second gap: a two-pulse batch closes independently.
        let after_second = after_first
            + Duration::from_millis(10)
            + DEFAULT_INTER_COIN_GAP
            + Duration::from_millis(10);
        let second = expect_batch(
            source
                .try_next_batch_at(after_second)
                .expect("second batch is not an error"),
        );
        assert_eq!(second.pulse_count, 2);
        assert_eq!(second.checked_amount_cents().unwrap(), 200);
    }

    #[test]
    fn empty_pulse_buffer_never_returns_a_batch() {
        let mut source = SimulatorCoinSource::new(default_config()).expect("valid config");
        // No pulses at all; long after the gap.
        let now = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            source
                .try_next_batch_at(now)
                .expect("empty call is not an error"),
            CoinBatchRead::None
        );
    }

    #[test]
    fn gap_zero_closes_immediately_after_a_pulse() {
        let mut config = default_config();
        config.inter_coin_gap = Duration::ZERO;
        let mut source = SimulatorCoinSource::new(config).expect("valid config");
        let start = Instant::now();
        // No pulses → no batch.
        assert_eq!(
            source
                .try_next_batch_at(start)
                .expect("empty call is not an error"),
            CoinBatchRead::None
        );
        // One pulse at the exact same instant closes the batch on the next
        // call (zero gap means any duration_since >= 0).
        source.inject_pulse_at(start);
        let batch = expect_batch(
            source
                .try_next_batch_at(start)
                .expect("zero-gap call is not an error"),
        );
        assert_eq!(batch.pulse_count, 1);
        assert_eq!(batch.checked_amount_cents().unwrap(), 100);
    }

    #[test]
    fn shared_gap_helper_matches_simulator_semantics() {
        // The serial source's edge path also goes through
        // `close_batch_if_gap_elapsed`; this test guards the helper's
        // invariants directly so a regression in the shared logic fails
        // here even if the simulator is green.
        let config = default_config();
        let mut pulses: Vec<Instant> = Vec::new();
        let start = Instant::now();
        pulses.push(start);
        pulses.push(start + Duration::from_millis(20));
        // Within gap → no batch, buffer preserved.
        assert_eq!(
            close_batch_if_gap_elapsed(&mut pulses, start + Duration::from_millis(50), &config)
                .expect("within-gap call is not an error"),
            CoinBatchRead::None
        );
        assert_eq!(pulses.len(), 2, "the buffer must not be cleared early");
        // After gap (last pulse at +20 ms + gap), the two-pulse batch closes.
        let after =
            start + Duration::from_millis(20) + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            close_batch_if_gap_elapsed(&mut pulses, after, &config)
                .expect("batch close is not an error"),
        );
        assert_eq!(batch.pulse_count, 2);
        assert_eq!(batch.checked_amount_cents().unwrap(), 200);
        assert!(pulses.is_empty(), "the buffer must be cleared on close");
    }

    /// A mock `SerialPort` that records set-pin writes and returns scripted
    /// coin-pin readings. The inner state is shared via `Arc<Mutex<...>>`
    /// so a test can inspect writes **after** the source has been dropped.
    /// Only constructed under `cfg(test)`.
    struct MockSerialPort {
        shared: Arc<Mutex<MockShared>>,
    }

    struct MockShared {
        coin_pin_readings: VecDeque<bool>,
        set_pin_writes: Vec<bool>,
        dtr_writes: Vec<bool>,
        read_error: bool,
    }

    impl MockSerialPort {
        fn with_readings(
            readings: impl IntoIterator<Item = bool>,
        ) -> (Self, Arc<Mutex<MockShared>>) {
            let shared = Arc::new(Mutex::new(MockShared {
                coin_pin_readings: readings.into_iter().collect(),
                set_pin_writes: Vec::new(),
                dtr_writes: Vec::new(),
                read_error: false,
            }));
            (
                Self {
                    shared: shared.clone(),
                },
                shared,
            )
        }

        fn with_read_error() -> (Self, Arc<Mutex<MockShared>>) {
            let (port, shared) = Self::with_readings([]);
            shared.lock().expect("mock shared state").read_error = true;
            (port, shared)
        }
    }

    impl io::Read for MockSerialPort {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl io::Write for MockSerialPort {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for MockSerialPort {
        fn name(&self) -> Option<String> {
            Some("MOCK".to_owned())
        }
        fn baud_rate(&self) -> serialport::Result<u32> {
            Ok(9600)
        }
        fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
            Ok(serialport::DataBits::Eight)
        }
        fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
            Ok(serialport::FlowControl::None)
        }
        fn parity(&self) -> serialport::Result<serialport::Parity> {
            Ok(serialport::Parity::None)
        }
        fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
            Ok(serialport::StopBits::One)
        }
        fn timeout(&self) -> Duration {
            Duration::ZERO
        }
        fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
            Ok(())
        }
        fn set_data_bits(&mut self, _: serialport::DataBits) -> serialport::Result<()> {
            Ok(())
        }
        fn set_flow_control(&mut self, _: serialport::FlowControl) -> serialport::Result<()> {
            Ok(())
        }
        fn set_parity(&mut self, _: serialport::Parity) -> serialport::Result<()> {
            Ok(())
        }
        fn set_stop_bits(&mut self, _: serialport::StopBits) -> serialport::Result<()> {
            Ok(())
        }
        fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
            Ok(())
        }
        fn write_request_to_send(&mut self, level: bool) -> serialport::Result<()> {
            self.shared
                .lock()
                .expect("mock shared state")
                .set_pin_writes
                .push(level);
            Ok(())
        }
        fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()> {
            self.shared
                .lock()
                .expect("mock shared state")
                .dtr_writes
                .push(level);
            Ok(())
        }
        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            self.read_carrier_detect()
        }
        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            self.read_carrier_detect()
        }
        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            self.read_carrier_detect()
        }
        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            let mut shared = self.shared.lock().expect("mock shared state");
            if shared.read_error {
                return Err(serialport::Error::new(
                    serialport::ErrorKind::Io(io::ErrorKind::Other),
                    "forced read failure",
                ));
            }
            Ok(shared.coin_pin_readings.pop_front().unwrap_or(false))
        }
        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        fn bytes_to_write(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        fn clear(&self, _: serialport::ClearBuffer) -> serialport::Result<()> {
            Ok(())
        }
        fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
            Err(serialport::Error::new(
                serialport::ErrorKind::Io(io::ErrorKind::Other),
                "mock port cannot be cloned",
            ))
        }
        fn set_break(&self) -> serialport::Result<()> {
            Ok(())
        }
        fn clear_break(&self) -> serialport::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn serial_source_inhibits_set_pin_at_construction_and_on_drop() {
        // The mock's shared state is the only way to observe set-pin writes
        // *after* the source has been dropped.
        let (mock, shared) = MockSerialPort::with_readings([]);
        {
            let _source = SerialCoinSource::from_port(default_config(), Box::new(mock))
                .expect("constructor should accept a valid mock port");
        }
        let writes = shared
            .lock()
            .expect("mock shared state")
            .set_pin_writes
            .clone();
        assert_eq!(
            writes,
            vec![false, false],
            "set pin must be inhibited at construction and on drop"
        );
    }

    #[test]
    fn serial_source_drives_set_pin_through_assert_and_inhibit() {
        let (mock, shared) = MockSerialPort::with_readings([]);
        let mut source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        assert!(!source.is_set_pin_asserted());
        source.assert_set_pin().expect("assert should succeed");
        assert!(source.is_set_pin_asserted());
        source.inhibit_set_pin().expect("inhibit should succeed");
        assert!(!source.is_set_pin_asserted());
        // Three writes recorded: inhibit (ctor), assert, inhibit.
        let writes = shared
            .lock()
            .expect("mock shared state")
            .set_pin_writes
            .clone();
        assert_eq!(writes, vec![false, true, false]);
    }

    #[test]
    fn configured_source_uses_unavailable_for_missing_serial_port() {
        let settings = crate::models::CoinDeviceSettings {
            com_port: Some("OPENPISO_MISSING_TEST_PORT".to_owned()),
            baud_rate: 9600,
            set_pin_line: "rts".to_owned(),
            coin_pin_line: "cd".to_owned(),
            coin_pin_polarity: "normally_open".to_owned(),
            inter_coin_gap_ms: 200,
            inactivity_timeout_seconds: 15,
            hard_cap_seconds: 180,
            pulse_map: vec![crate::models::PulseMapEntry {
                pulse_count: 1,
                amount_cents: 100,
            }],
            calibration_required: false,
        };

        let capture = EventCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let source = tracing::subscriber::with_default(subscriber, || {
            ConfiguredCoinSource::from_settings(default_config(), &settings)
                .expect("bad hardware config should degrade to unavailable")
        });

        assert!(!source.is_available());
        assert!(matches!(source, ConfiguredCoinSource::Unavailable));
        let events = capture.0.lock().expect("captured events");
        let unavailable = events
            .iter()
            .find(|event| {
                event.target == "openpiso_admin_lib::coin_source"
                    && event.fields.contains_key("available")
                    && event.fields.contains_key("error")
            })
            .expect("unavailable source diagnostic should be machine-readable");
        assert_eq!(
            unavailable.fields.get("available"),
            Some(&"false".to_owned())
        );

        let mut invalid_settings = settings;
        invalid_settings.coin_pin_polarity = "invalid".to_owned();
        assert!(matches!(
            ConfiguredCoinSource::from_settings(default_config(), &invalid_settings),
            Err(AppError::InvalidInput(message))
                if message == "coin pin polarity must be normally_open or normally_closed"
        ));
    }

    #[test]
    fn serial_source_honors_configured_dtr_and_cts_lines() {
        // The DTR/CTS line config is verified by checking that assert_set_pin
        // writes DTR (not RTS) and that the source correctly drains a pulse
        // injected directly into the shared observation buffer.
        let (mock, shared) = MockSerialPort::with_readings([]);
        let mut source = SerialCoinSource::from_port_with_lines(
            default_config(),
            Box::new(mock),
            SetPinLine::Dtr,
            CoinPinLine::Cts,
        )
        .expect("open configured mock");

        source.assert_set_pin().expect("assert DTR");
        // Inject a pulse directly (simulating what the sampler thread would do)
        // and verify the batch closes after the gap.
        let start = Instant::now();
        source.inject_pulse_at(start);
        // Within gap: no batch yet.
        assert_eq!(
            source
                .try_next_batch_at(start + Duration::from_millis(50))
                .expect("within-gap call is not an error"),
            CoinBatchRead::None
        );
        // After gap: batch closes.
        let after = start + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            source
                .try_next_batch_at(after)
                .expect("post-gap call is not an error"),
        );
        assert_eq!(batch.pulse_count, 1);
        assert_eq!(batch.checked_amount_cents().unwrap(), 100);

        let shared = shared.lock().expect("mock shared state");
        assert!(shared.set_pin_writes.is_empty(), "RTS must not be touched");
        assert_eq!(shared.dtr_writes, vec![false, true]);
    }

    #[test]
    fn pullup_dtr_asserts_high_when_set_pin_is_rts() {
        // When the set pin is RTS, DTR is a spare output that must be driven
        // high on open to power the coin-line pull-up (else the line floats).
        let (mut mock, shared) = MockSerialPort::with_readings([]);
        assert_pullup_dtr(&mut mock, SetPinLine::Rts).expect("assert DTR pull-up");
        let shared = shared.lock().expect("mock shared state");
        assert_eq!(shared.dtr_writes, vec![true], "DTR must be asserted high");
        assert!(
            shared.set_pin_writes.is_empty(),
            "RTS must not be touched by the pull-up"
        );
    }

    #[test]
    fn pullup_dtr_is_noop_when_dtr_is_the_set_pin() {
        // When DTR is the set/enable pin, the set-pin logic owns it; the
        // pull-up helper must not fight that by forcing it high.
        let (mut mock, shared) = MockSerialPort::with_readings([]);
        assert_pullup_dtr(&mut mock, SetPinLine::Dtr).expect("no-op is not an error");
        let shared = shared.lock().expect("mock shared state");
        assert!(
            shared.dtr_writes.is_empty(),
            "DTR must be left to the set-pin logic"
        );
    }

    #[test]
    fn serial_source_accumulates_and_drains_pulses_through_shared_buffer() {
        // Three pulses injected into the shared buffer should accumulate and
        // close as one three-pulse batch.
        let (mock, _shared) = MockSerialPort::with_readings([]);
        let mut source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        let start = Instant::now();

        // Inject three pulses directly (simulating sampler thread output).
        source.inject_pulse_at(start);
        source.inject_pulse_at(start + Duration::from_millis(20));
        source.inject_pulse_at(start + Duration::from_millis(40));

        // Early polls inside the gap return no batch.
        for offset_ms in [0, 10, 20, 30, 50, 100] {
            assert_eq!(
                source
                    .try_next_batch_at(start + Duration::from_millis(offset_ms))
                    .expect("within-gap call is not an error"),
                CoinBatchRead::None,
                "no batch should close within the gap at {offset_ms} ms"
            );
        }
        // After the gap (last pulse at +40 ms), the three-pulse batch closes.
        let after =
            start + Duration::from_millis(40) + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            source
                .try_next_batch_at(after)
                .expect("batch close is not an error"),
        );
        assert_eq!(batch.pulse_count, 3);
        assert_eq!(batch.checked_amount_cents().unwrap(), 300);
    }

    #[test]
    fn serial_sampler_reopens_after_threshold_without_enabling_set_pin() {
        let (failed_port, _failed_shared) = MockSerialPort::with_read_error();
        let (reopened_port, reopened_shared) = MockSerialPort::with_readings([true, false, true]);
        let reopened = Arc::new(Mutex::new(Some(reopened_port)));
        let opener: PortOpener = {
            let reopened = Arc::clone(&reopened);
            Arc::new(move || {
                reopened
                    .lock()
                    .expect("reopen queue")
                    .take()
                    .map(|port| Box::new(port) as SerialPortHandle)
                    .ok_or_else(|| AppError::Conflict("no test port left".to_owned()))
            })
        };
        let mut source = SerialCoinSource::from_port_with_lines_and_polarity_and_opener(
            default_config(),
            Box::new(failed_port),
            SetPinLine::Dtr,
            CoinPinLine::Cts,
            CoinPinPolarity::NormallyOpen,
            opener,
        )
        .expect("open failed test port");
        let mut recovery = SamplerRecoveryState::default();
        let start = Instant::now();

        for failure in 0..SERIAL_READ_FAILURE_THRESHOLD {
            source.sampler_step_at(start + Duration::from_millis(failure as u64), &mut recovery);
        }
        assert!(
            !source.is_available(),
            "threshold must mark source unavailable"
        );
        assert!(
            !source.is_set_pin_asserted(),
            "failure must inhibit Set Pin"
        );
        assert!(matches!(
            source.try_next_batch_at(start + Duration::from_secs(2)),
            Err(AppError::InvalidInput(_))
        ));

        source.sampler_step_at(
            start
                + Duration::from_millis(SERIAL_READ_FAILURE_THRESHOLD as u64)
                + SERIAL_RECONNECT_BASE_DELAY,
            &mut recovery,
        );
        assert!(
            source.is_available(),
            "successful reopen restores availability"
        );
        assert!(
            !source.is_set_pin_asserted(),
            "reopen must not assert Set Pin"
        );
        assert!(
            !source.has_buffered_pulses(),
            "pulses do not cross the failed-port boundary"
        );

        // The first level after recovery seeds the sampler while inhibited;
        // it is neither an edge nor a pulse. Only a subsequent real
        // transition after Selection Window assertion may be creditable.
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        source.audit_sender = Some(sender);
        source.sampler_step_at(start + Duration::from_secs(3), &mut recovery);
        assert!(!source.has_buffered_pulses());
        assert!(receiver.try_recv().is_err(), "seeding emits no anomaly");
        source
            .assert_set_pin()
            .expect("recovered port can assert later");
        source.sampler_step_at(start + Duration::from_secs(4), &mut recovery);
        assert!(!source.has_buffered_pulses());
        source.sampler_step_at(start + Duration::from_secs(5), &mut recovery);
        assert!(source.has_buffered_pulses());
        assert_eq!(
            reopened_shared
                .lock()
                .expect("reopened shared state")
                .dtr_writes,
            vec![false, true]
        );
    }

    #[test]
    fn serial_reopen_backoff_is_bounded_and_deterministic() {
        let (failed_port, _shared) = MockSerialPort::with_read_error();
        let attempts = Arc::new(Mutex::new(0u32));
        let opener: PortOpener = {
            let attempts = Arc::clone(&attempts);
            Arc::new(move || {
                *attempts.lock().expect("attempt counter") += 1;
                Err(AppError::InvalidInput("port absent".to_owned()))
            })
        };
        let mut source = SerialCoinSource::from_port_with_lines_and_polarity_and_opener(
            default_config(),
            Box::new(failed_port),
            SetPinLine::Rts,
            CoinPinLine::Cd,
            CoinPinPolarity::NormallyOpen,
            opener,
        )
        .expect("open failed test port");
        let mut recovery = SamplerRecoveryState::default();
        let start = Instant::now();
        for failure in 0..SERIAL_READ_FAILURE_THRESHOLD {
            source.sampler_step_at(start + Duration::from_millis(failure as u64), &mut recovery);
        }
        let retry = start
            + Duration::from_millis(SERIAL_READ_FAILURE_THRESHOLD as u64)
            + SERIAL_RECONNECT_BASE_DELAY;
        source.sampler_step_at(retry, &mut recovery);
        let first_retry = *attempts.lock().expect("attempt counter");
        source.sampler_step_at(retry + Duration::from_millis(1), &mut recovery);
        assert_eq!(
            *attempts.lock().expect("attempt counter"),
            first_retry,
            "retry deadline prevents a tight reopen loop"
        );
        assert!(
            recovery.retry_delay(100) <= SERIAL_RECONNECT_MAX_DELAY,
            "backoff remains bounded"
        );
    }

    // ── Issue #95 acceptance criteria: pulse sampler ───────────────────

    #[test]
    fn realistic_five_pulse_train_assembles_into_single_coin_batch() {
        // Five highs (12 samples each) separated by lows (4 samples each).
        // Every actual sampler instant is driven through the mock serial port
        // at the real poll period; no transition-only shortcut or wall-clock
        // sleep can hide a cadence regression. The initial low sample seeds
        // the idle line.
        let readings = std::iter::once(false).chain((0..=64).map(|sample| sample % 16 < 12));
        let (mock, _shared) = MockSerialPort::with_readings(readings);
        let mut source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        source.assert_set_pin().expect("assert Set Pin");
        let mut recovery = SamplerRecoveryState::default();
        let t0 = Instant::now();

        for sample in 0..=65 {
            source.sampler_step_at(t0 + PULSE_SAMPLER_POLL_MS * sample, &mut recovery);
        }

        // Rising edges land at sample indices 1/17/33/49/65 → 30 ms apart per
        // step gives 30/510/990/1470/1950 ms. The 480 ms spacing stays under the
        // 600 ms inter-coin gap, so all five pulses form one batch.
        let last_rise = t0 + Duration::from_millis(1950);
        let batch = expect_batch(
            source
                .try_next_batch_at(last_rise + DEFAULT_INTER_COIN_GAP)
                .expect("batch close should succeed"),
        );
        assert_eq!(batch.pulse_count, 5);
        assert_eq!(batch.checked_amount_cents().unwrap(), 500);
        assert_eq!(
            batch.pulse_timings_us,
            vec![0, 480_000, 960_000, 1_440_000, 1_920_000]
        );
    }

    #[test]
    fn sampler_does_not_anomaly_log_admitted_straddling_edge() {
        let (mock, _shared) = MockSerialPort::with_readings([false, true, false, true, false]);
        let mut source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        let mut recovery = SamplerRecoveryState::default();
        let start = Instant::now();
        let capture = EventCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            source.sampler_step_at(start, &mut recovery);
            source.assert_set_pin().expect("assert Set Pin");
            source.sampler_step_at(start + Duration::from_millis(1), &mut recovery);
            source.sampler_step_at(start + Duration::from_millis(2), &mut recovery);
            source.inhibit_set_pin().expect("inhibit Set Pin");
            source.sampler_step_at(start + Duration::from_millis(3), &mut recovery);
            source.sampler_step_at(start + Duration::from_millis(4), &mut recovery);
        });

        assert_eq!(
            source
                .observation
                .lock()
                .expect("observation")
                .pulse_count(),
            2
        );
        assert!(!capture
            .0
            .lock()
            .expect("captured events")
            .iter()
            .any(|event| event.fields.contains_key("anomaly")));
    }

    #[test]
    fn pulses_while_set_pin_inhibited_never_form_batch() {
        // A rising edge observed while inhibited with an empty buffer is
        // refused and cannot form a Coin Batch.
        let config = default_config();
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();

        // Seed low, then observe a rising edge while Set Pin is not
        // asserted: the edge is detected but no pulse is accumulated.
        obs.observe(false, false, t0);
        assert!(
            obs.observe(true, false, t0 + Duration::from_millis(1)),
            "rising edge is still detected regardless of set pin"
        );
        assert!(
            !obs.has_buffered_pulses(),
            "no pulse accumulated while inhibited"
        );
        assert_eq!(obs.pulse_count(), 0);

        // Even after the gap, no batch forms.
        let after = t0 + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        assert_eq!(
            obs.close_batch_if_gap_elapsed(after, &config)
                .expect("close on empty buffer is not an error"),
            CoinBatchRead::None
        );
    }

    #[test]
    fn arbitrary_pulse_count_is_a_valued_batch() {
        let config = default_config();
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();
        obs.observe(false, true, t0);

        for i in 0..7 {
            obs.observe(true, true, t0 + Duration::from_millis(i * 10 + 1));
            obs.observe(false, true, t0 + Duration::from_millis(i * 10 + 5));
        }
        let after =
            t0 + Duration::from_millis(6 * 10) + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            obs.close_batch_if_gap_elapsed(after, &config)
                .expect("arbitrary pulse count is not an error"),
        );
        assert_eq!(batch.pulse_count, 7);
        assert_eq!(batch.checked_amount_cents().unwrap(), 700);
    }

    #[test]
    fn sampler_poll_period_is_named_constant() {
        // The sampler poll period is a named constant, not an inline literal.
        // The conservative default remains 30 ms.
        assert_eq!(PULSE_SAMPLER_POLL_MS, Duration::from_millis(30));
    }

    #[test]
    fn ten_pulse_train_stays_one_batch_under_new_gap_default() {
        // A 10-pulse ₱10 coin at realistic spacing (~80 ms between pulses
        // = 720 ms from first to last) must remain one batch under the
        // new 600 ms default gap. Since the gap is measured from the LAST
        // pulse, the train completes within 720 ms and the gap of 600 ms
        // after the last pulse means the total quiet window is 720 + 600 =
        // 1320 ms from the first pulse. The previous 200 ms default would
        // split this train into multiple batches.
        let config = default_config();
        let mut obs = PulseObservation::new();
        let t0 = Instant::now();

        // Seed idle low, then emit 10 pulses, 80 ms apart → from t0 to
        // t0+720ms.
        obs.observe(false, true, t0 - Duration::from_millis(1));
        for pulse_index in 0..10 {
            let rise_time = t0 + Duration::from_millis(pulse_index * 80);
            obs.observe(true, true, rise_time);
            obs.observe(false, true, rise_time + Duration::from_millis(60));
        }

        // Verify all 10 pulses are in the buffer (none split off).
        assert_eq!(obs.pulse_count(), 10, "all 10 pulses must be in one buffer");

        // Close the batch after the gap from the last pulse.
        let last_rise = t0 + Duration::from_millis(9 * 80);
        let after = last_rise + DEFAULT_INTER_COIN_GAP + Duration::from_millis(10);
        let batch = expect_batch(
            obs.close_batch_if_gap_elapsed(after, &config)
                .expect("10-pulse close should succeed"),
        );
        assert_eq!(batch.pulse_count, 10);
        assert_eq!(batch.checked_amount_cents().unwrap(), 1000);
    }

    // ── InhibitedPulseRateLimiter ────────────────────────────────────

    #[test]
    fn inhibited_pulse_flood_logs_are_bounded_and_resume_after_window() {
        let readings = (0..14).map(|sample| sample % 2 == 1);
        let (mock, _shared) = MockSerialPort::with_readings(readings);
        let mut source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        let mut recovery = SamplerRecoveryState::default();
        let start = Instant::now();
        let capture = EventCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            for sample in 0..12 {
                source.sampler_step_at(start + PULSE_SAMPLER_POLL_MS * sample, &mut recovery);
            }
            let pre_window_anomalies = capture
                .0
                .lock()
                .expect("captured events")
                .iter()
                .filter(|event| event.fields.contains_key("anomaly"))
                .count();
            assert_eq!(pre_window_anomalies, INHIBITED_PULSE_MAX_PER_WINDOW);

            source.sampler_step_at(start + INHIBITED_PULSE_WINDOW, &mut recovery);
            source.sampler_step_at(
                start + INHIBITED_PULSE_WINDOW + PULSE_SAMPLER_POLL_MS,
                &mut recovery,
            );
        });

        let events = capture.0.lock().expect("captured events");
        let anomalies = events
            .iter()
            .filter(|event| {
                event.target == "openpiso_admin_lib::coin_source"
                    && event
                        .fields
                        .get("anomaly")
                        .is_some_and(|value| value == "\"inhibited_pulse\"")
            })
            .count();
        assert_eq!(anomalies, INHIBITED_PULSE_MAX_PER_WINDOW + 1);
    }

    #[test]
    fn sampler_steps_publish_observable_causes_and_pulse_counts() {
        let readings = [false, true, false, true, false, true, false, true, false];
        let (mock, _shared) = MockSerialPort::with_readings(readings);
        let mut source =
            SerialCoinSource::from_port(default_config(), Box::new(mock)).expect("open mock");
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        source.audit_sender = Some(sender);
        let mut recovery = SamplerRecoveryState::default();
        let start = Instant::now();

        // The first healthy sample only seeds the line. The next rising edge
        // with no Selection Window has an observable cause.
        source.sampler_step_at(start, &mut recovery);
        source.sampler_step_at(start + Duration::from_millis(1), &mut recovery);

        // A pulse while the source is unavailable gets its own aggregate.
        source.available.store(false, Ordering::Release);
        source.sampler_step_at(start + Duration::from_millis(2), &mut recovery);
        source.sampler_step_at(start + Duration::from_millis(3), &mut recovery);

        // Recovery returns to the healthy, no-Selection-Window diagnosis.
        // Without electrical feedback, subsequent pulses carry the same
        // observable cause rather than inventing a wiring diagnosis.
        source.available.store(true, Ordering::Release);
        source.sampling_healthy.store(true, Ordering::Release);
        source.sampler_step_at(start + Duration::from_millis(4), &mut recovery);
        source.sampler_step_at(start + Duration::from_millis(5), &mut recovery);
        source.sampler_step_at(start + Duration::from_millis(6), &mut recovery);
        source.sampler_step_at(start + Duration::from_millis(7), &mut recovery);
        source.sampler_step_at(
            start + INHIBITED_PULSE_WINDOW + Duration::from_millis(10),
            &mut recovery,
        );

        let messages: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        let mut anomalies: Vec<_> = messages
            .into_iter()
            .filter_map(|message| match message {
                CoinSourceAuditMessage::SamplingAnomaly { reason, count } => Some((reason, count)),
                CoinSourceAuditMessage::Availability { .. } => None,
            })
            .collect();
        anomalies.sort();
        assert_eq!(
            anomalies,
            vec![
                ("coin_source_unavailable".to_owned(), 1),
                ("no_selection_window".to_owned(), 3),
            ]
        );
    }

    #[test]
    fn inhibited_pulse_limiter_allows_burst_and_rate_limits() {
        let limiter = InhibitedPulseRateLimiter::new();
        let now = Instant::now();

        // First INHIBITED_PULSE_MAX_PER_WINDOW calls are all allowed.
        for i in 0..INHIBITED_PULSE_MAX_PER_WINDOW {
            assert!(
                limiter.should_log_at(now + Duration::from_millis(i as u64)),
                "call {} should be within budget",
                i + 1
            );
        }
        // Next call within the same window is denied.
        assert!(
            !limiter.should_log_at(now + Duration::from_millis(1)),
            "excess call must be rate-limited"
        );
        // After the window elapses, a new call is allowed.
        assert!(
            limiter.should_log_at(now + INHIBITED_PULSE_WINDOW + Duration::from_millis(1)),
            "post-window call must be admitted again"
        );
    }

    #[test]
    fn inhibited_pulse_aggregate_reports_bounded_window_count() {
        let limiter = InhibitedPulseRateLimiter::new();
        let now = Instant::now();
        for offset in 0..7 {
            assert_eq!(
                limiter.finish_window_at(
                    now + Duration::from_millis(offset),
                    InhibitedPulseCause::NoSelectionWindow,
                ),
                None
            );
        }
        assert_eq!(
            limiter.finish_window_at(
                now + INHIBITED_PULSE_WINDOW + Duration::from_millis(1),
                InhibitedPulseCause::NoSelectionWindow,
            ),
            Some((InhibitedPulseCause::NoSelectionWindow, 7))
        );
    }

    #[test]
    fn inhibited_pulse_aggregate_is_keyed_by_cause() {
        let limiter = InhibitedPulseRateLimiter::new();
        let now = Instant::now();
        assert_eq!(
            limiter.finish_window_at(now, InhibitedPulseCause::NoSelectionWindow),
            None
        );
        assert_eq!(
            limiter.finish_window_at(
                now + Duration::from_millis(1),
                InhibitedPulseCause::CoinSourceUnavailable,
            ),
            None
        );
        let mut expired =
            limiter.flush_expired_at(now + Duration::from_millis(1) + INHIBITED_PULSE_WINDOW);
        expired.sort_by_key(|(cause, _)| cause.reason());
        assert_eq!(
            expired,
            vec![
                (InhibitedPulseCause::CoinSourceUnavailable, 1),
                (InhibitedPulseCause::NoSelectionWindow, 1),
            ]
        );
    }

    #[test]
    fn sampling_anomaly_summaries_report_pulses_and_cause() {
        assert_eq!(
            sampling_anomaly_summary("coin_source_unavailable", 1),
            "Ignored 1 pulse because the Coin Source was unavailable or recovering."
        );
        assert_eq!(
            sampling_anomaly_summary("no_selection_window", 2),
            "Ignored 2 pulses while no Selection Window was open and the Set Pin was inhibited; check the Coin Debug Bundle for an idle insertion or an inhibit wiring problem."
        );
        assert!(sampling_anomaly_summary("no_selection_window", 3)
            .contains("idle insertion or an inhibit wiring problem"));
    }

    #[test]
    fn inhibited_pulse_limiter_aging_removes_old_hits() {
        // Staggered hits: old ones expire, new ones fill the budget.
        let limiter = InhibitedPulseRateLimiter::new();
        let now = Instant::now();

        // Place two old hits that will expire.
        limiter.should_log_at(now);
        limiter.should_log_at(now + Duration::from_millis(1));

        // After the window, only the two old hits should have expired, making
        // room for INHIBITED_PULSE_MAX_PER_WINDOW new ones.
        let later = now + INHIBITED_PULSE_WINDOW + Duration::from_millis(1);
        for i in 0..INHIBITED_PULSE_MAX_PER_WINDOW {
            assert!(
                limiter.should_log_at(later + Duration::from_millis(i as u64)),
                "expired window freed budget for call {}",
                i + 1
            );
        }
        // Budget exhausted again.
        assert!(
            !limiter.should_log_at(later + Duration::from_millis(1)),
            "budget exhausted after refill"
        );
    }
}

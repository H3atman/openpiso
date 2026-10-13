// M2 deliverable: the server-owned **Terminal Selection** singleton
// (ADR-0023). At most one selection is active; a second `open` while one
// is active is rejected with `"terminal in use"`. The set pin is asserted
// only while a Selection Window is open and is re-inhibited on every close
// path (Enter, Inactivity, HardCap, Manual, Shutdown) and on `Drop`, so an
// orphan coin is impossible in normal operation. Each accepted `CoinBatch`
// is committed immediately through the existing `sessions.record_coin_event`
// path with `pc_id = active.pc_id` — the device payload never carries a
// `pc_id` (ADR-0008 / ADR-0023).
//
// The state machine is generic over `S: CoinSource` so the M2 tests can
// drive a `SimulatorCoinSource` directly with an injectable clock; the
// production wiring uses the simulator too (per the `dev-simulator` seed in
// AGENTS.md) until M4 calibrates the real serial.

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    coin_source::{
        CoinBatch, CoinBatchRead, CoinSource, CoinSourceAuditSender, CoinSourceKind,
        ConfiguredCoinSource, SimulatorCoinSource,
    },
    errors::{AppError, AppResult},
    events::EventHub,
    models::{CoinSourceClaim, CoinTransaction},
    sessions::SessionStore,
};

/// Default quiet-period timeout: a Selection Window closes 15 seconds after
/// the last accepted coin (per ADR-0023). Owner-editable in M4.
pub const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(15);

/// Default hard cap: a Selection Window always closes 180 seconds after it
/// opened, regardless of activity (per ADR-0023). Owner-editable in M4.
pub const DEFAULT_HARD_CAP: Duration = Duration::from_secs(180);

/// Extra scheduling margin after the latest possible straddling pulse and its
/// closing quiet gap.
const IN_FLIGHT_DRAIN_HEADROOM: Duration = Duration::from_millis(100);

/// Canonical reason a `TerminalSelection` was rejected for "already open".
/// Surfaced verbatim in the API so the frontend can render a stable message.
pub const TERMINAL_IN_USE: &str = "terminal in use";

/// Why a Selection Window was closed. The set pin is always re-inhibited
/// regardless of reason; the variant is recorded for owner auditing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Customer pressed Enter on the kiosk (M3 surface; M2 receives it via
    /// the `close_selection` Tauri command).
    Enter,
    /// Inactivity timeout elapsed without a new accepted coin.
    Inactivity,
    /// Hard cap reached regardless of activity.
    HardCap,
    /// Owner / kiosk explicitly closed the window.
    Manual,
    /// Process is shutting down — the `Drop` impl on the source inhibits the
    /// set pin; the selection snapshot is still broadcast so the frontend
    /// can update its UI before the process exits.
    Shutdown,
}

impl CloseReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Inactivity => "inactivity",
            Self::HardCap => "hard_cap",
            Self::Manual => "manual",
            Self::Shutdown => "shutdown",
        }
    }
}

/// In-memory record of a single open Selection Window. All times use the
/// monotonic `Instant` clock so timer math is unaffected by wall-clock
/// changes; the unix-ms timestamp is captured once at open for serialization
/// (the broadcast payload is the only consumer of absolute time).
#[derive(Debug, Clone)]
pub struct ActiveSelection {
    pub pc_id: String,
    /// Wall-clock-monotonic anchor for the selection. Not currently read
    /// outside the timer math, but kept on the struct so the M3 kiosk can
    /// display an "elapsed since open" counter without recomputing it.
    #[allow(dead_code)]
    pub opened_at_instant: Instant,
    pub opened_at_unix_ms: i64,
    pub last_activity_at: Instant,
    pub hard_cap_deadline: Instant,
    pub inactivity_timeout: Duration,
    /// Echo of the config's hard cap, kept for introspection. The M3 kiosk
    /// can show "remaining cap" and "total cap" without consulting config.
    #[allow(dead_code)]
    pub hard_cap: Duration,
    pub inserted_amount_cents: i64,
    pub inserted_seconds: i64,
}

impl ActiveSelection {
    pub fn inactivity_deadline(&self) -> Instant {
        self.last_activity_at
            .checked_add(self.inactivity_timeout)
            .unwrap_or(self.last_activity_at)
    }

    pub fn snapshot(&self, now: Instant) -> SelectionState {
        let inactivity_remaining = self
            .inactivity_deadline()
            .saturating_duration_since(now)
            .as_secs()
            .min(i64::MAX as u64) as i64;
        let hard_cap_remaining = self
            .hard_cap_deadline
            .saturating_duration_since(now)
            .as_secs()
            .min(i64::MAX as u64) as i64;
        SelectionState {
            pc_id: self.pc_id.clone(),
            opened_at_unix_ms: self.opened_at_unix_ms,
            inactivity_remaining_seconds: inactivity_remaining.max(0),
            hard_cap_remaining_seconds: hard_cap_remaining.max(0),
            inserted_amount_cents: self.inserted_amount_cents,
            inserted_seconds: self.inserted_seconds,
        }
    }

    /// Returns the close reason a tick at `now` should fire, if any. The
    /// hard cap is checked first so it always wins on a simultaneous hit.
    pub fn classify_timeout(&self, now: Instant) -> Option<CloseReason> {
        if now >= self.hard_cap_deadline {
            Some(CloseReason::HardCap)
        } else if now >= self.inactivity_deadline() {
            Some(CloseReason::Inactivity)
        } else {
            None
        }
    }
}

/// Serializable Selection Window snapshot broadcast to the frontend. The
/// `Instant` clock is monotonic and not JSON-friendly, so relative durations
/// are pre-computed at snapshot time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectionState {
    pub pc_id: String,
    pub opened_at_unix_ms: i64,
    pub inactivity_remaining_seconds: i64,
    pub hard_cap_remaining_seconds: i64,
    pub inserted_amount_cents: i64,
    pub inserted_seconds: i64,
}

/// Tunable timers for a `TerminalSelection`. M2 ships the ADR-0023 defaults
/// as constants; M4 reads the values from the owner coin-device settings.
#[derive(Debug, Clone, Copy)]
pub struct TerminalSelectionConfig {
    pub inactivity_timeout: Duration,
    pub hard_cap: Duration,
}

/// Guided physical denomination calibration is deliberately shorter than a
/// shop Selection Window and is never a coin transaction. The owner records
/// five individual samples so a single noisy pulse train cannot become money.
pub const CALIBRATION_REQUIRED_SAMPLES: usize = 5;
pub const CALIBRATION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const CALIBRATION_ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationPhase {
    Capturing,
    Completed,
    Cancelled,
    Expired,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationStatus {
    pub capture_id: String,
    pub target_amount_cents: i64,
    pub phase: CalibrationPhase,
    pub sample_counts: Vec<i64>,
    pub sample_armed: bool,
    pub current_pulse_count: i64,
    pub required_samples: usize,
    pub started_at_unix_ms: i64,
    pub idle_deadline_unix_ms: i64,
    pub absolute_deadline_unix_ms: i64,
    pub failure_reason: Option<String>,
}

struct CalibrationSession {
    capture_id: Uuid,
    target_amount_cents: i64,
    phase: CalibrationPhase,
    sample_counts: Vec<i64>,
    sample_armed: bool,
    completion_audit_pending: bool,
    sampling_fault_generation: u64,
    current_pulses: Vec<Instant>,
    started_at: Instant,
    idle_deadline: Instant,
    absolute_deadline: Instant,
    started_at_unix_ms: i64,
    idle_deadline_unix_ms: i64,
    absolute_deadline_unix_ms: i64,
    failure_reason: Option<String>,
}

impl CalibrationSession {
    fn snapshot(&self) -> CalibrationStatus {
        CalibrationStatus {
            capture_id: self.capture_id.to_string(),
            target_amount_cents: self.target_amount_cents,
            phase: self.phase,
            sample_counts: self.sample_counts.clone(),
            sample_armed: self.sample_armed,
            current_pulse_count: self.current_pulses.len() as i64,
            required_samples: CALIBRATION_REQUIRED_SAMPLES,
            started_at_unix_ms: self.started_at_unix_ms,
            idle_deadline_unix_ms: self.idle_deadline_unix_ms,
            absolute_deadline_unix_ms: self.absolute_deadline_unix_ms,
            failure_reason: self.failure_reason.clone(),
        }
    }
}

impl TerminalSelectionConfig {
    pub fn defaults() -> Self {
        Self {
            inactivity_timeout: DEFAULT_INACTIVITY_TIMEOUT,
            hard_cap: DEFAULT_HARD_CAP,
        }
    }
}

/// A completed coin batch plus the canonical transaction the server
/// committed for it. Returned by `try_accept_next_batch` so callers (the
/// background poll loop, tests) can verify the commit landed.
#[derive(Debug, Clone)]
pub struct AcceptedBatch {
    #[allow(dead_code)]
    pub batch: CoinBatch,
    #[allow(dead_code)]
    pub transaction: CoinTransaction,
}

struct TerminalSelectionInner<S: CoinSource> {
    source: S,
    active: Option<ActiveSelection>,
    closing: bool,
    closing_pc_id: Option<String>,
    pending_batch: Option<CoinBatch>,
    calibration: Option<CalibrationSession>,
    config: TerminalSelectionConfig,
}

/// Server-owned singleton. The wrapper is cheap to clone (an `Arc` plus a
/// few handles) so the background poll task and the Tauri commands share
/// the same instance.
pub struct TerminalSelection<S: CoinSource> {
    inner: Arc<Mutex<TerminalSelectionInner<S>>>,
    pool: SqlitePool,
    sessions: SessionStore,
    events: EventHub,
    coin_audit_sender: Option<CoinSourceAuditSender>,
}

impl<S: CoinSource> Clone for TerminalSelection<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            pool: self.pool.clone(),
            sessions: self.sessions.clone(),
            events: self.events.clone(),
            coin_audit_sender: self.coin_audit_sender.clone(),
        }
    }
}

impl<S: CoinSource> TerminalSelection<S> {
    /// Construct a fresh selection. The wrapped source is re-inhibited as a
    /// defence-in-depth measure; the `SerialCoinSource` already inhibits on
    /// `open`, and `SimulatorCoinSource::new` starts with the pin off, so
    /// this is a no-op for the M2 wiring but cheap insurance.
    pub fn new(
        source: S,
        config: TerminalSelectionConfig,
        pool: SqlitePool,
        sessions: SessionStore,
        events: EventHub,
    ) -> AppResult<Self> {
        Self::new_with_coin_audit_sender(source, config, pool, sessions, events, None)
    }

    pub(crate) fn new_with_coin_audit_sender(
        source: S,
        config: TerminalSelectionConfig,
        pool: SqlitePool,
        sessions: SessionStore,
        events: EventHub,
        coin_audit_sender: Option<CoinSourceAuditSender>,
    ) -> AppResult<Self> {
        let mut source = source;
        source.inhibit_set_pin()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(TerminalSelectionInner {
                source,
                active: None,
                closing: false,
                closing_pc_id: None,
                pending_batch: None,
                calibration: None,
                config,
            })),
            pool,
            sessions,
            events,
            coin_audit_sender,
        })
    }

    /// Open a Selection Window for `pc_id` at the given clock. Production
    /// callers use `open`; tests use `open_at` with an injected `Instant`.
    pub async fn open(&self, pc_id: &str) -> AppResult<SelectionState> {
        self.open_at(pc_id, Instant::now()).await
    }

    pub async fn open_at(&self, pc_id: &str, now: Instant) -> AppResult<SelectionState> {
        let pc_id = pc_id.trim();
        if pc_id.is_empty() {
            return Err(AppError::InvalidInput("PC ID is required".to_owned()));
        }
        // Validate the PC exists before claiming the terminal. The selection
        // is the global lock; rejecting unknown PCs here keeps a typo from
        // burning the terminal for the whole hard cap.
        crate::db::pc(&self.pool, pc_id)
            .await
            .map_err(|error| match error {
                AppError::NotFound(_) => AppError::NotFound(format!("PC '{pc_id}'")),
                other => other,
            })?;
        if !crate::db::pc_is_visible(&self.pool, pc_id).await? {
            return Err(AppError::Conflict(format!(
                "PC '{pc_id}' is hidden from the Central Terminal"
            )));
        }

        let mut inner = self.inner.lock().await;
        let shop_status = crate::db::shop_status(&self.pool).await?;
        if !shop_status.open {
            warn!(
                target: "openpiso_admin_lib::terminal_selection",
                pc_id = %pc_id,
                reason = "shop_closed",
                next_change_at = ?shop_status.next_change_at,
                "Selection Window refused while Shop Closed"
            );
            return Err(AppError::ShopClosed);
        }
        if inner.active.is_some()
            || inner.closing
            || inner.pending_batch.is_some()
            || inner
                .calibration
                .as_ref()
                .is_some_and(calibration_blocks_terminal)
            || inner.source.has_buffered_pulses()
        {
            return Err(AppError::Conflict(TERMINAL_IN_USE.to_owned()));
        }
        // Fail-closed ordering: assert the set pin *before* the selection is
        // visible. If the wire call fails the mutex is released with the
        // pin still inhibited, matching the post-`close` invariant.
        inner.source.assert_set_pin()?;
        info!(
            target: "openpiso_admin_lib::terminal_selection",
            pc_id = %pc_id,
            cause = "selection_open",
            "Set Pin asserted"
        );

        let opened_at_unix_ms = unix_millis_from_system();
        let active = ActiveSelection {
            pc_id: pc_id.to_owned(),
            opened_at_instant: now,
            opened_at_unix_ms,
            last_activity_at: now,
            hard_cap_deadline: now.checked_add(inner.config.hard_cap).ok_or_else(|| {
                AppError::InvalidInput("selection hard cap is too large".to_owned())
            })?,
            inactivity_timeout: inner.config.inactivity_timeout,
            hard_cap: inner.config.hard_cap,
            inserted_amount_cents: 0,
            inserted_seconds: 0,
        };
        let snapshot = active.snapshot(now);
        inner.active = Some(active);
        drop(inner);

        self.publish(Some(snapshot.clone()));
        Ok(snapshot)
    }

    /// Close a Selection Window. Production callers use `close`; the `_at`
    /// variant takes an explicit clock for tests and for the auto-close
    /// paths (`tick`, `shutdown`). After the Set Pin is inhibited, the source
    /// drains the buffered train, including any straddling edges admitted
    /// while that buffer is non-empty, and credits it to the just-selected PC
    /// (ADR-0023 in-flight rule).
    pub async fn close(&self, reason: CloseReason) -> AppResult<Option<SelectionState>> {
        let now = Instant::now();
        let drain_until = self.drain_deadline(now).await?;
        let (snapshot, drained_pc) = self.begin_close(reason, now).await?;
        if let Some(pc_id) = drained_pc {
            self.drain_in_flight(&pc_id, Instant::now, drain_until)
                .await;
        }
        self.finish_close_if_drained().await?;
        Ok(snapshot)
    }

    /// Test-only close: same as `close` but without the in-flight drain.
    /// Production callers should use `close`; the test suite uses this to
    /// assert the set-pin / snapshot invariants independently from drain.
    #[allow(dead_code)]
    pub async fn close_at(
        &self,
        reason: CloseReason,
        now: Instant,
    ) -> AppResult<Option<SelectionState>> {
        let (snapshot, _) = self.begin_close(reason, now).await?;
        self.finish_close().await;
        Ok(snapshot)
    }

    /// Internal close: inhibits the set pin, removes the active selection,
    /// and returns both the broadcast snapshot and the just-selected PC id
    /// (so callers can drain in-flight batches). Returns `(None, None)` if
    /// no window was open.
    async fn begin_close(
        &self,
        _reason: CloseReason,
        now: Instant,
    ) -> AppResult<(Option<SelectionState>, Option<String>)> {
        let mut inner = self.inner.lock().await;
        if inner.active.is_none() {
            return Ok((None, None));
        }
        if inner.closing {
            return Err(AppError::Conflict(
                "terminal selection is closing".to_owned(),
            ));
        };
        // Fail-closed: inhibit the set pin while the selection is still
        // active. If the wire call fails, keep the selected PC in memory so
        // any swallowed coin still has a route and the next tick can retry.
        inner.source.inhibit_set_pin()?;
        info!(
            target: "openpiso_admin_lib::terminal_selection",
            reason = ?_reason,
            cause = "selection_close",
            "Set Pin inhibited"
        );
        let Some(active) = inner.active.take() else {
            return Ok((None, None));
        };
        let drained_pc = active.pc_id.clone();
        let snapshot = active.snapshot(now);
        inner.closing = true;
        inner.closing_pc_id = Some(drained_pc.clone());
        drop(inner);
        self.publish(None);
        Ok((Some(snapshot), Some(drained_pc)))
    }

    async fn finish_close(&self) {
        let mut inner = self.inner.lock().await;
        inner.closing = false;
        inner.closing_pc_id = None;
    }

    async fn finish_close_if_drained(&self) -> AppResult<()> {
        {
            let inner = self.inner.lock().await;
            if inner.pending_batch.is_some() || inner.source.has_buffered_pulses() {
                return Err(AppError::Conflict(
                    "terminal selection is closing with an unresolved in-flight coin".to_owned(),
                ));
            }
        }
        let mut inner = self.inner.lock().await;
        inner.closing = false;
        inner.closing_pc_id = None;
        Ok(())
    }

    async fn drain_deadline(&self, now: Instant) -> AppResult<Instant> {
        let inter_coin_gap = self.inner.lock().await.source.inter_coin_gap();
        // A train's next pulse may arrive almost one full quiet gap after
        // inhibition. Keep draining for another full gap after that tail so
        // the completed Coin Batch can close normally.
        now.checked_add(inter_coin_gap)
            .and_then(|deadline| deadline.checked_add(inter_coin_gap))
            .and_then(|deadline| deadline.checked_add(IN_FLIGHT_DRAIN_HEADROOM))
            .ok_or_else(|| AppError::InvalidInput("drain window is too large".to_owned()))
    }

    /// Retry an unresolved close left behind by a transient read/commit error.
    /// The terminal remains locked until the buffered coin is either credited
    /// to the original selected PC or there is no buffered state left.
    pub async fn retry_closing_drain(&self) -> AppResult<bool> {
        let pc_id = {
            let inner = self.inner.lock().await;
            if !inner.closing {
                return Ok(false);
            }
            inner.closing_pc_id.clone()
        };
        let Some(pc_id) = pc_id else {
            self.finish_close().await;
            return Ok(false);
        };
        let now = Instant::now();
        let drain_until = self.drain_deadline(now).await?;
        self.drain_in_flight(&pc_id, Instant::now, drain_until)
            .await;
        self.finish_close_if_drained().await?;
        Ok(true)
    }

    /// Current selection snapshot, or `None` if no window is open.
    pub async fn get(&self) -> Option<SelectionState> {
        self.get_at(Instant::now()).await
    }

    pub async fn get_at(&self, now: Instant) -> Option<SelectionState> {
        let inner = self.inner.lock().await;
        inner.active.as_ref().map(|active| active.snapshot(now))
    }

    /// Whether the wrapped `CoinSource` is currently usable. M3 exposes
    /// this so the kiosk can render the "out of order" screen and refuse
    /// to open a window against a dead source. The M2 simulator is
    /// always `true`; M4's serial source reports `false` on a poisoned
    /// or dropped COM handle.
    pub async fn is_source_available(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.source.is_available()
    }

    /// Auto-close path: if the active window has timed out, close it and
    /// return the reason. Returns `None` if no window is open or no timeout
    /// has fired. Drains in-flight batches on a timeout-driven close for
    /// the same reason `close` does.
    pub async fn tick(&self) -> Option<CloseReason> {
        self.tick_at(Instant::now()).await
    }

    pub async fn tick_at(&self, now: Instant) -> Option<CloseReason> {
        let reason = {
            let inner = self.inner.lock().await;
            inner.active.as_ref().and_then(|a| a.classify_timeout(now))
        };
        let reason = reason?;
        match self.close(reason).await {
            Ok(Some(_)) | Ok(None) => Some(reason),
            Err(error) => {
                warn!(
                    target: "openpiso_admin_lib::terminal_selection",
                    "selection timeout close failed: {error}"
                );
                None
            }
        }
    }

    /// Try to read the next closed `CoinBatch` from the source and, if one
    /// is available, commit it to the active selection's PC. Shop Hours is
    /// intentionally not rechecked here: a Selection Window already open at
    /// the Shop Closed boundary runs to its natural close and keeps crediting
    /// coins (ADR-0053 / issue #175). Resets the inactivity timer on success.
    /// Returns `Ok(None)` when there is no selection or no batch is ready.
    pub async fn try_accept_next_batch(&self) -> AppResult<Option<AcceptedBatch>> {
        self.try_accept_next_batch_at(Instant::now()).await
    }

    pub async fn try_accept_next_batch_at(&self, now: Instant) -> AppResult<Option<AcceptedBatch>> {
        // The lock is held across the source read AND the DB commit so a
        // concurrent `close` cannot drop the coin between "I read a batch"
        // and "I credited it" (ADR-0023: each accepted coin commits
        // immediately). The selection is a single global lock anyway, so
        // serialising its operations is the intended cost.
        let mut inner = self.inner.lock().await;
        if inner.closing
            || inner
                .calibration
                .as_ref()
                .is_some_and(calibration_blocks_terminal)
        {
            return Ok(None);
        }
        let pc_id = match inner.active.as_ref() {
            Some(active) => active.pc_id.clone(),
            None => return Ok(None),
        };
        let (mut batch, freshly_closed) = match inner.pending_batch.take() {
            Some(batch) => (batch, false),
            None => match inner.source.try_next_batch_at(now)? {
                CoinBatchRead::Batch(batch) => (batch, true),
                CoinBatchRead::None => return Ok(None),
            },
        };
        if freshly_closed {
            log_batch_closed(&pc_id, &batch);
        }
        let source_kind = inner.source.kind();
        let transaction = match self.commit_batch(&pc_id, &mut batch, source_kind).await {
            Ok(transaction) => transaction,
            Err(error) => {
                // Retain the batch (with its trace id and incremented attempt) for retry.
                inner.pending_batch = Some(batch);
                if let Err(inhibit_error) = inner.source.inhibit_set_pin() {
                    warn!(
                        target: "openpiso_admin_lib::terminal_selection",
                        "coin commit and fail-closed inhibit both failed: {error}; {inhibit_error}"
                    );
                }
                return Err(error);
            }
        };
        if let Some(active) = inner.active.as_mut() {
            active.last_activity_at = now;
            active.inserted_amount_cents = active
                .inserted_amount_cents
                .saturating_add(transaction.amount_cents);
            active.inserted_seconds = active
                .inserted_seconds
                .saturating_add(transaction.seconds_added);
            let snapshot = active.snapshot(now);
            drop(inner);

            if let Ok(pc) = crate::db::pc(&self.pool, &pc_id).await {
                self.events
                    .publish_pc_updated(self.sessions.enrich_pc(pc).await);
            }
            self.publish(Some(snapshot));
        } else {
            // Race: a concurrent close cleared the selection between our
            // read and the commit. The commit already landed against the
            // just-selected PC, so the coin is not lost. We just don't
            // update the activity timer or broadcast a fresh snapshot —
            // the close path will broadcast the closed state.
            drop(inner);
        }

        self.events.publish_coin_accepted(transaction.clone());
        Ok(Some(AcceptedBatch { batch, transaction }))
    }

    /// Drain any batch the source has buffered in its gap window and credit
    /// it to `pc_id` (the just-selected PC). Stops once the drain deadline
    /// elapses. The Set Pin is already inhibited at this point, but the reader
    /// may still observe the tail of a train already in the buffer; only an
    /// edge with no buffered train is refused.
    /// Each `now_fn` call advances the gap check; the loop iterates until
    /// the deadline regardless of whether the source returns `None` (the
    /// next call to `now_fn` returns a later time, and the gap check
    /// eventually succeeds).
    pub async fn drain_in_flight(
        &self,
        pc_id: &str,
        now_fn: impl Fn() -> Instant + Send + Sync,
        drain_until: Instant,
    ) -> usize {
        let mut drained = 0;
        loop {
            let now = now_fn();
            if now >= drain_until {
                break;
            }
            let (batch_result, source_kind, freshly_closed) = {
                let mut inner = self.inner.lock().await;
                let (batch_result, freshly_closed) = match inner.pending_batch.take() {
                    Some(batch) => (Ok(CoinBatchRead::Batch(batch)), false),
                    None => (inner.source.try_next_batch_at(now), true),
                };
                (batch_result, inner.source.kind(), freshly_closed)
            };
            match batch_result {
                Ok(CoinBatchRead::Batch(mut batch)) => {
                    if freshly_closed {
                        log_batch_closed(pc_id, &batch);
                    }
                    match self.commit_batch(pc_id, &mut batch, source_kind).await {
                        Ok(transaction) => {
                            drained += 1;
                            self.events.publish_coin_accepted(transaction.clone());
                            if let Ok(pc) = crate::db::pc(&self.pool, pc_id).await {
                                self.events
                                    .publish_pc_updated(self.sessions.enrich_pc(pc).await);
                            }
                            tracing::info!(
                                target: "openpiso_admin_lib::terminal_selection",
                                coin_trace_id = %batch.coin_trace_id,
                                pc_id = %pc_id,
                                pulse_count = batch.pulse_count,
                                seconds_added = transaction.seconds_added,
                                "Drained in-flight batch after close"
                            );
                        }
                        Err(error) => {
                            self.inner.lock().await.pending_batch = Some(batch);
                            tracing::error!(
                                target: "openpiso_admin_lib::terminal_selection",
                                pc_id = %pc_id,
                                "in-flight commit failed: {error}"
                            );
                            break;
                        }
                    }
                }
                Ok(CoinBatchRead::None) => {
                    // No batch yet — the gap hasn't elapsed. Yield briefly so
                    // the production clock advances; the next loop iteration
                    // calls `now_fn` again and the gap check eventually
                    // closes a batch if one is pending.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => {
                    tracing::error!(
                        target: "openpiso_admin_lib::terminal_selection",
                        pc_id = %pc_id,
                        "in-flight batch read failed: {error}"
                    );
                    break;
                }
            }
        }
        drained
    }

    /// Test-only accessor: hold the lock for tests that need to inject
    /// pulses or assert the set-pin state. Exposes the private inner type
    /// intentionally — only compiled under `#[cfg(test)]`, only callable
    /// from the test module.
    #[cfg(test)]
    #[allow(private_interfaces)]
    pub(crate) async fn lock_source(
        &self,
    ) -> tokio::sync::MutexGuard<'_, TerminalSelectionInner<S>> {
        self.inner.lock().await
    }

    async fn commit_batch(
        &self,
        pc_id: &str,
        batch: &mut CoinBatch,
        source_kind: CoinSourceKind,
    ) -> AppResult<CoinTransaction> {
        use crate::models::CoinEventInput;
        let claim = match source_kind {
            CoinSourceKind::Simulator => CoinSourceClaim::Simulator,
            CoinSourceKind::Serial => CoinSourceClaim::SerialTerminal,
        };
        let amount_cents = batch.checked_amount_cents()?;
        let input = CoinEventInput {
            pc_id: pc_id.to_owned(),
            amount_cents,
            pulse_count: Some(batch.pulse_count),
            timestamp: None,
        };
        // Log each commit attempt with trace id, batch details, and derived
        // amount. The derivation is intentionally at the authority boundary.
        info!(
            target: "openpiso_admin_lib::terminal_selection",
            coin_trace_id = %batch.coin_trace_id,
            attempt = batch.attempt,
            pc_id = %pc_id,
            pulse_count = batch.pulse_count,
            amount_cents,
            source_kind = ?source_kind,
            "Coin Batch committed"
        );
        let result = self
            .sessions
            .record_coin_event_with_trace(&self.pool, &input, &claim, Some(batch.coin_trace_id))
            .await;
        batch.attempt += 1;
        result
    }

    fn publish(&self, state: Option<SelectionState>) {
        self.events.publish_selection_state(state);
    }
}

fn log_batch_closed(pc_id: &str, batch: &CoinBatch) {
    info!(
        target: "openpiso_admin_lib::coin_source",
        coin_trace_id = %batch.coin_trace_id,
        pc_id = %pc_id,
        pulse_count = batch.pulse_count,
        amount_cents = ?batch.checked_amount_cents(),
        pulse_timings_us = ?batch.pulse_timings_us,
        span_us = batch.span_us,
        gap_us = batch.gap_us,
        "Coin Batch closed"
    );
}

impl TerminalSelection<SimulatorCoinSource> {
    /// Inject simulator pulses through the active Terminal Selection. This is
    /// intentionally selection-scoped: callers cannot provide a PC or amount.
    pub async fn inject_simulator_pulses(&self, pulse_count: i64) -> AppResult<SelectionState> {
        if pulse_count <= 0 {
            return Err(AppError::InvalidInput(
                "pulse count must be positive".to_owned(),
            ));
        }

        let mut inner = self.inner.lock().await;
        let Some(active) = inner.active.as_ref() else {
            return Err(AppError::Conflict(
                "no active terminal selection".to_owned(),
            ));
        };
        let now = Instant::now();
        let snapshot = active.snapshot(now);
        if !inner.source.is_set_pin_asserted() {
            return Err(AppError::Conflict(
                "terminal selection is not accepting coins".to_owned(),
            ));
        }

        for _ in 0..pulse_count {
            inner.source.inject_pulse_at(now);
        }
        Ok(snapshot)
    }
}

impl TerminalSelection<ConfiguredCoinSource> {
    pub async fn start_calibration(
        &self,
        target_amount_cents: i64,
    ) -> AppResult<CalibrationStatus> {
        self.start_calibration_at(target_amount_cents, Instant::now())
            .await
    }

    pub async fn start_calibration_at(
        &self,
        target_amount_cents: i64,
        now: Instant,
    ) -> AppResult<CalibrationStatus> {
        if !(1..=1_000_000).contains(&target_amount_cents) {
            return Err(AppError::InvalidInput(
                "calibration amount must be between 1 and 1000000 cents".to_owned(),
            ));
        }
        let mut inner = self.inner.lock().await;
        if inner.source.kind() != CoinSourceKind::Serial {
            return Err(AppError::Conflict(
                "physical calibration requires a configured serial coin source".to_owned(),
            ));
        }
        if !inner.source.is_available() || !inner.source.is_sampling_healthy() {
            return Err(AppError::Conflict(
                "coin source is unavailable for physical calibration".to_owned(),
            ));
        }
        if inner.active.is_some()
            || inner.closing
            || inner.pending_batch.is_some()
            || inner.source.has_buffered_pulses()
        {
            return Err(AppError::Conflict(TERMINAL_IN_USE.to_owned()));
        }
        if inner
            .calibration
            .as_ref()
            .is_some_and(|capture| capture.phase == CalibrationPhase::Capturing)
        {
            return Err(AppError::Conflict(
                "calibration is already active".to_owned(),
            ));
        }
        // A terminal result is bounded in-memory state. Starting a new
        // attempt explicitly discards a prior completed/cancelled result.
        inner.calibration = None;

        inner.source.clear_buffered_pulses();
        inner.source.inhibit_set_pin()?;
        let started_at_unix_ms = unix_millis_from_system();
        let idle_deadline = now
            .checked_add(CALIBRATION_IDLE_TIMEOUT)
            .ok_or_else(|| AppError::InvalidInput("calibration timeout is too large".to_owned()))?;
        let absolute_deadline = now
            .checked_add(CALIBRATION_ABSOLUTE_TIMEOUT)
            .ok_or_else(|| AppError::InvalidInput("calibration timeout is too large".to_owned()))?;
        let capture = CalibrationSession {
            capture_id: Uuid::new_v4(),
            target_amount_cents,
            phase: CalibrationPhase::Capturing,
            sample_counts: Vec::new(),
            sample_armed: true,
            completion_audit_pending: false,
            sampling_fault_generation: inner.source.sampling_fault_generation(),
            current_pulses: Vec::new(),
            started_at: now,
            idle_deadline,
            absolute_deadline,
            started_at_unix_ms,
            idle_deadline_unix_ms: unix_deadline_millis(started_at_unix_ms, now, idle_deadline),
            absolute_deadline_unix_ms: unix_deadline_millis(
                started_at_unix_ms,
                now,
                absolute_deadline,
            ),
            failure_reason: None,
        };
        inner.calibration = Some(capture);
        if let Err(error) = inner.source.assert_set_pin() {
            if let Some(capture) = inner.calibration.as_mut() {
                capture.phase = CalibrationPhase::Failed;
                capture.failure_reason = Some(error.to_string());
            }
            return Err(error);
        }
        inner
            .calibration
            .as_ref()
            .map(CalibrationSession::snapshot)
            .ok_or_else(|| AppError::Conflict("calibration state was lost".to_owned()))
    }

    pub async fn poll_calibration(&self) -> AppResult<Option<CalibrationStatus>> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        Ok(inner.calibration.as_ref().map(CalibrationSession::snapshot))
    }

    pub async fn current_calibration(&self) -> AppResult<Option<CalibrationStatus>> {
        self.poll_calibration().await
    }

    pub async fn calibration_status(&self, capture_id: &str) -> AppResult<CalibrationStatus> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        let capture = inner
            .calibration
            .as_ref()
            .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?;
        if capture.capture_id.to_string() != capture_id {
            return Err(AppError::NotFound("calibration capture".to_owned()));
        }
        Ok(capture.snapshot())
    }

    pub async fn record_calibration_sample(
        &self,
        capture_id: &str,
    ) -> AppResult<CalibrationStatus> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        let matches = inner
            .calibration
            .as_ref()
            .is_some_and(|capture| capture.capture_id.to_string() == capture_id);
        if !matches {
            return Err(AppError::NotFound("calibration capture".to_owned()));
        }
        if inner
            .calibration
            .as_ref()
            .is_some_and(|capture| capture.phase != CalibrationPhase::Capturing)
        {
            return Err(AppError::Conflict(
                "calibration capture is no longer recording".to_owned(),
            ));
        }
        if inner
            .calibration
            .as_ref()
            .is_some_and(|capture| !capture.sample_armed)
        {
            return Err(AppError::Conflict(
                "arm the next calibration sample before recording it".to_owned(),
            ));
        }
        if let Err(error) = inner.source.inhibit_set_pin() {
            if let Some(capture) = inner.calibration.as_mut() {
                capture.current_pulses.clear();
                capture.sample_armed = false;
                capture.completion_audit_pending = false;
                capture.phase = CalibrationPhase::Failed;
                capture.failure_reason = Some(error.to_string());
            }
            inner.source.clear_buffered_pulses();
            return Err(error);
        }
        let trailing_pulses = inner.source.take_buffered_pulses();
        if !trailing_pulses.is_empty() {
            if let Some(capture) = inner.calibration.as_mut() {
                capture.current_pulses.extend(trailing_pulses);
            }
        }
        let sample_count = inner
            .calibration
            .as_ref()
            .map(|capture| capture.current_pulses.len())
            .unwrap_or(0);
        if sample_count == 0 {
            inner.source.assert_set_pin()?;
            return Err(AppError::Conflict(
                "no pulses captured; insert one coin before recording the sample".to_owned(),
            ));
        }
        let now = Instant::now();
        {
            if let Some(capture) = inner.calibration.as_mut() {
                capture.sample_armed = false;
            }
            let capture = inner
                .calibration
                .as_mut()
                .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?;
            capture.sample_counts.push(sample_count as i64);
            capture.current_pulses.clear();
            capture.idle_deadline = now.checked_add(CALIBRATION_IDLE_TIMEOUT).ok_or_else(|| {
                AppError::InvalidInput("calibration timeout is too large".to_owned())
            })?;
            capture.idle_deadline_unix_ms = unix_deadline_millis(
                capture.started_at_unix_ms,
                capture.started_at,
                capture.idle_deadline,
            );
            if capture.sample_counts.len() >= CALIBRATION_REQUIRED_SAMPLES {
                let first = capture.sample_counts[0];
                if capture.sample_counts.iter().all(|sample| *sample == first) {
                    capture.phase = CalibrationPhase::Completed;
                    capture.completion_audit_pending = true;
                } else {
                    capture.phase = CalibrationPhase::Failed;
                    capture.failure_reason = Some(
                        "calibration samples are inconsistent; mapping was not proposed".to_owned(),
                    );
                }
            }
        }
        inner
            .calibration
            .as_ref()
            .map(CalibrationSession::snapshot)
            .ok_or_else(|| AppError::Conflict("calibration state was lost".to_owned()))
    }

    pub async fn arm_calibration_sample(&self, capture_id: &str) -> AppResult<CalibrationStatus> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        let already_armed = inner
            .calibration
            .as_ref()
            .filter(|capture| capture.capture_id.to_string() == capture_id)
            .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?
            .sample_armed;
        if inner
            .calibration
            .as_ref()
            .is_some_and(|capture| capture.phase != CalibrationPhase::Capturing)
        {
            return Err(AppError::Conflict(
                "calibration capture is no longer recording".to_owned(),
            ));
        }
        if already_armed {
            return inner
                .calibration
                .as_ref()
                .map(CalibrationSession::snapshot)
                .ok_or_else(|| AppError::Conflict("calibration state was lost".to_owned()));
        }
        inner.source.clear_buffered_pulses();
        inner.source.assert_set_pin()?;
        let capture = inner
            .calibration
            .as_mut()
            .ok_or_else(|| AppError::Conflict("calibration state was lost".to_owned()))?;
        capture.sample_armed = true;
        Ok(capture.snapshot())
    }

    pub async fn fail_calibration(
        &self,
        capture_id: &str,
        reason: String,
    ) -> AppResult<CalibrationStatus> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        let status = {
            let capture = inner
                .calibration
                .as_mut()
                .filter(|capture| capture.capture_id.to_string() == capture_id)
                .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?;
            capture.current_pulses.clear();
            capture.completion_audit_pending = false;
            capture.phase = CalibrationPhase::Failed;
            capture.failure_reason = Some(reason);
            capture.snapshot()
        };
        inner.source.clear_buffered_pulses();
        inner.source.inhibit_set_pin()?;
        Ok(status)
    }

    pub async fn prepare_calibration_cancel(&self, capture_id: &str) -> AppResult<()> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        let matches = inner
            .calibration
            .as_ref()
            .is_some_and(|capture| capture.capture_id.to_string() == capture_id);
        if !matches {
            return Err(AppError::NotFound("calibration capture".to_owned()));
        }
        if inner.source.is_available() {
            inner.source.inhibit_set_pin()?;
        }
        inner.source.clear_buffered_pulses();
        let capture = inner
            .calibration
            .as_mut()
            .ok_or_else(|| AppError::Conflict("calibration state was lost".to_owned()))?;
        capture.current_pulses.clear();
        capture.sample_armed = false;
        Ok(())
    }

    pub async fn finish_calibration_cancel(
        &self,
        capture_id: &str,
    ) -> AppResult<CalibrationStatus> {
        let mut inner = self.inner.lock().await;
        let capture = inner
            .calibration
            .as_mut()
            .filter(|capture| capture.capture_id.to_string() == capture_id)
            .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?;
        capture.completion_audit_pending = false;
        capture.phase = CalibrationPhase::Cancelled;
        Ok(capture.snapshot())
    }

    pub async fn cancel_calibration(&self, capture_id: &str) -> AppResult<CalibrationStatus> {
        self.prepare_calibration_cancel(capture_id).await?;
        self.finish_calibration_cancel(capture_id).await
    }

    pub async fn calibration_mapping(&self, capture_id: &str) -> AppResult<(i64, i64)> {
        let mut inner = self.inner.lock().await;
        Self::refresh_calibration_locked(&mut inner, Instant::now())?;
        let capture = inner
            .calibration
            .as_ref()
            .filter(|capture| capture.capture_id.to_string() == capture_id)
            .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?;
        if capture.completion_audit_pending {
            return Err(AppError::Conflict(
                "calibration completion audit is still pending".to_owned(),
            ));
        }
        calibration_mapping_from_status(&capture.snapshot())
    }

    pub async fn mark_calibration_completion_audited(&self, capture_id: &str) -> AppResult<()> {
        let mut inner = self.inner.lock().await;
        let capture = inner
            .calibration
            .as_mut()
            .filter(|capture| capture.capture_id.to_string() == capture_id)
            .ok_or_else(|| AppError::NotFound("calibration capture".to_owned()))?;
        if capture.phase != CalibrationPhase::Completed {
            return Err(AppError::Conflict(
                "calibration capture is no longer completed".to_owned(),
            ));
        }
        capture.completion_audit_pending = false;
        Ok(())
    }

    fn refresh_calibration_locked(
        inner: &mut TerminalSelectionInner<ConfiguredCoinSource>,
        now: Instant,
    ) -> AppResult<()> {
        let capturing = inner
            .calibration
            .as_ref()
            .is_some_and(|capture| capture.phase == CalibrationPhase::Capturing);
        if !capturing {
            return Ok(());
        }
        let idle_expired = inner.calibration.as_ref().is_some_and(|capture| {
            capture.phase == CalibrationPhase::Capturing && now >= capture.idle_deadline
        });
        let pulses = inner.source.take_buffered_pulses();
        if let Some(capture) = inner.calibration.as_mut() {
            if capture.phase == CalibrationPhase::Capturing && !idle_expired && !pulses.is_empty() {
                capture.current_pulses.extend(pulses);
                if let Some(idle_deadline) = now.checked_add(CALIBRATION_IDLE_TIMEOUT) {
                    capture.idle_deadline = idle_deadline;
                    capture.idle_deadline_unix_ms = unix_deadline_millis(
                        capture.started_at_unix_ms,
                        capture.started_at,
                        idle_deadline,
                    );
                }
            }
        }
        let expired = inner.calibration.as_ref().is_some_and(|capture| {
            capture.phase == CalibrationPhase::Capturing
                && (idle_expired || now >= capture.absolute_deadline)
        });
        let source_unhealthy = inner.calibration.as_ref().is_some_and(|capture| {
            capture.phase == CalibrationPhase::Capturing
                && (!inner.source.is_sampling_healthy()
                    || inner.source.sampling_fault_generation()
                        != capture.sampling_fault_generation)
        });
        let unavailable = inner.calibration.as_ref().is_some_and(|capture| {
            capture.phase == CalibrationPhase::Capturing
                && (!inner.source.is_available() || source_unhealthy)
        });
        if expired || unavailable {
            let reason = if !inner.source.is_available() {
                "coin source became unavailable"
            } else if source_unhealthy {
                "coin source sampler faulted"
            } else {
                "calibration capture timed out"
            };
            let inhibit_error = inner.source.inhibit_set_pin().err();
            inner.source.clear_buffered_pulses();
            if let Some(capture) = inner.calibration.as_mut() {
                capture.current_pulses.clear();
                capture.phase = if unavailable || inhibit_error.is_some() {
                    CalibrationPhase::Failed
                } else {
                    CalibrationPhase::Expired
                };
                capture.failure_reason = Some(match inhibit_error {
                    Some(error) => format!("{reason}; could not inhibit coin source: {error}"),
                    None => reason.to_owned(),
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn reconfigure_after<F, Fut, T>(
        &self,
        candidate_settings: &crate::models::CoinDeviceSettings,
        candidate_source_config: crate::coin_source::CoinSourceConfig,
        candidate_selection_config: TerminalSelectionConfig,
        current_settings: &crate::models::CoinDeviceSettings,
        current_source_config: crate::coin_source::CoinSourceConfig,
        calibration: Option<(&str, i64, i64)>,
        persist: F,
    ) -> AppResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AppResult<T>>,
    {
        let mut inner = self.inner.lock().await;
        let applying_calibration = calibration.is_some();
        let calibration_blocks_reconfigure = inner.calibration.as_ref().is_some_and(|capture| {
            matches!(
                capture.phase,
                CalibrationPhase::Capturing | CalibrationPhase::Completed
            )
        });
        if inner.active.is_some()
            || inner.closing
            || inner.pending_batch.is_some()
            || (!applying_calibration && calibration_blocks_reconfigure)
        {
            return Err(AppError::Conflict(
                "coin-device settings cannot change while the terminal is in use".to_owned(),
            ));
        }
        // The caller read `current_settings` before taking this lock. Re-read
        // under the lock so a stale normal Save cannot overwrite a mapping
        // applied by a concurrent calibration (or vice versa).
        if crate::db::coin_device_settings(&self.pool).await? != *current_settings {
            return Err(AppError::Conflict(
                "coin-device settings changed; reload and retry".to_owned(),
            ));
        }
        if let Some((capture_id, expected_pulse_count, expected_amount_cents)) = calibration {
            let capture = inner.calibration.as_ref().ok_or_else(|| {
                AppError::Conflict("calibration capture is no longer available".to_owned())
            })?;
            if capture.capture_id.to_string() != capture_id {
                return Err(AppError::Conflict("calibration capture changed".to_owned()));
            }
            let actual = calibration_mapping_from_status(&capture.snapshot())?;
            if actual != (expected_pulse_count, expected_amount_cents) {
                return Err(AppError::Conflict("calibration capture changed".to_owned()));
            }
        }
        if inner.source.is_available() {
            inner.source.inhibit_set_pin()?;
        }
        let previous = std::mem::replace(&mut inner.source, ConfiguredCoinSource::Unavailable);
        drop(previous);

        let candidate = match ConfiguredCoinSource::from_settings_with_audit_sender(
            candidate_source_config,
            candidate_settings,
            self.coin_audit_sender.clone(),
        ) {
            Ok(source) => source,
            Err(error) => {
                inner.source = ConfiguredCoinSource::from_settings_with_audit_sender(
                    current_source_config,
                    current_settings,
                    self.coin_audit_sender.clone(),
                )
                .unwrap_or(ConfiguredCoinSource::Unavailable);
                return Err(error);
            }
        };
        let result = match persist().await {
            Ok(result) => result,
            Err(error) => {
                drop(candidate);
                inner.source = ConfiguredCoinSource::from_settings_with_audit_sender(
                    current_source_config,
                    current_settings,
                    self.coin_audit_sender.clone(),
                )
                .unwrap_or(ConfiguredCoinSource::Unavailable);
                return Err(error);
            }
        };
        inner.source = candidate;
        inner.config = candidate_selection_config;
        if calibration.is_some()
            || inner
                .calibration
                .as_ref()
                .is_some_and(|capture| capture.phase == CalibrationPhase::Failed)
        {
            inner.calibration = None;
        }
        Ok(result)
    }

    pub async fn inject_simulator_pulses(&self, pulse_count: i64) -> AppResult<SelectionState> {
        if pulse_count <= 0 {
            return Err(AppError::InvalidInput(
                "pulse count must be positive".to_owned(),
            ));
        }

        let mut inner = self.inner.lock().await;
        let Some(active) = inner.active.as_ref() else {
            return Err(AppError::Conflict(
                "no active terminal selection".to_owned(),
            ));
        };
        let now = Instant::now();
        let snapshot = active.snapshot(now);
        if !inner.source.is_set_pin_asserted() {
            return Err(AppError::Conflict(
                "terminal selection is not accepting coins".to_owned(),
            ));
        }

        for _ in 0..pulse_count {
            inner.source.inject_simulator_pulse_at(now)?;
        }
        Ok(snapshot)
    }
}

fn calibration_blocks_terminal(capture: &CalibrationSession) -> bool {
    matches!(
        capture.phase,
        CalibrationPhase::Capturing | CalibrationPhase::Completed | CalibrationPhase::Failed
    )
}

fn calibration_mapping_from_status(status: &CalibrationStatus) -> AppResult<(i64, i64)> {
    if status.phase != CalibrationPhase::Completed {
        return Err(AppError::Conflict(
            "calibration must complete before applying a mapping".to_owned(),
        ));
    }
    let Some(&pulse_count) = status.sample_counts.first() else {
        return Err(AppError::Conflict(
            "calibration did not capture any samples".to_owned(),
        ));
    };
    if pulse_count <= 0
        || status
            .sample_counts
            .iter()
            .any(|count| *count != pulse_count)
    {
        return Err(AppError::Conflict(
            "calibration samples are inconsistent; mapping was not proposed".to_owned(),
        ));
    }
    Ok((pulse_count, status.target_amount_cents))
}

fn unix_deadline_millis(start_unix_ms: i64, start: Instant, deadline: Instant) -> i64 {
    let delta = deadline.saturating_duration_since(start).as_millis();
    start_unix_ms.saturating_add(delta.min(i64::MAX as u128) as i64)
}

fn unix_millis_from_system() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| {
            let millis = duration.as_millis();
            if millis > i64::MAX as u128 {
                i64::MAX
            } else {
                millis as i64
            }
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex as StdMutex};

    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        accounts,
        auth::AccountTokens,
        coin_source::{
            CoinSourceKind, ConfiguredCoinSource, PulseObservation, SimulatorCoinSource,
            DEFAULT_INTER_COIN_GAP,
        },
        db, events,
    };

    /// Short inter-coin gap so batch-focused tests finish quickly. This is
    /// separate from the terminal timer config; the production default gap
    /// (~600 ms) is too slow for deterministic unit tests.
    fn fast_coin_config() -> crate::coin_source::CoinSourceConfig {
        let mut config = crate::coin_source::CoinSourceConfig::default_config();
        config.inter_coin_gap = Duration::from_millis(5);
        config
    }

    #[test]
    fn calibration_mapping_requires_five_consistent_samples() {
        let status = CalibrationStatus {
            capture_id: "capture".to_owned(),
            target_amount_cents: 2000,
            phase: CalibrationPhase::Completed,
            sample_counts: vec![20, 20, 20, 20, 20],
            sample_armed: false,
            current_pulse_count: 0,
            required_samples: CALIBRATION_REQUIRED_SAMPLES,
            started_at_unix_ms: 0,
            idle_deadline_unix_ms: 0,
            absolute_deadline_unix_ms: 0,
            failure_reason: None,
        };
        assert_eq!(
            calibration_mapping_from_status(&status).unwrap(),
            (20, 2000)
        );

        let mut inconsistent = status.clone();
        inconsistent.sample_counts[4] = 19;
        assert!(matches!(
            calibration_mapping_from_status(&inconsistent),
            Err(AppError::Conflict(message)) if message.contains("inconsistent")
        ));

        let mut incomplete = status;
        incomplete.phase = CalibrationPhase::Capturing;
        assert!(matches!(
            calibration_mapping_from_status(&incomplete),
            Err(AppError::Conflict(message)) if message.contains("complete")
        ));
    }

    #[tokio::test]
    async fn polling_without_calibration_preserves_production_pulses() -> AppResult<()> {
        let source = ConfiguredCoinSource::simulator(fast_coin_config())?;
        let selection = selection_with_source(source).await?;
        let pulse_at = Instant::now();
        {
            let mut inner = selection.lock_source().await;
            inner.source.assert_set_pin()?;
            inner.source.inject_simulator_pulse_at(pulse_at)?;
        }

        assert!(selection.poll_calibration().await?.is_none());
        let inner = selection.lock_source().await;
        assert!(inner.source.has_buffered_pulses());
        Ok(())
    }

    #[tokio::test]
    async fn calibration_samples_are_non_transactional_and_fail_closed() -> AppResult<()> {
        let source = ConfiguredCoinSource::simulator(fast_coin_config())?;
        let selection = selection_with_source(source).await?;
        let now = Instant::now();
        let capture_id = Uuid::new_v4();
        let capture_id_string = capture_id.to_string();
        {
            let mut inner = selection.lock_source().await;
            inner.calibration = Some(CalibrationSession {
                capture_id,
                target_amount_cents: 2000,
                phase: CalibrationPhase::Capturing,
                sample_counts: Vec::new(),
                sample_armed: true,
                completion_audit_pending: false,
                sampling_fault_generation: 0,
                current_pulses: Vec::new(),
                started_at: now,
                idle_deadline: now + CALIBRATION_IDLE_TIMEOUT,
                absolute_deadline: now + CALIBRATION_ABSOLUTE_TIMEOUT,
                started_at_unix_ms: 0,
                idle_deadline_unix_ms: 60_000,
                absolute_deadline_unix_ms: 300_000,
                failure_reason: None,
            });
            inner.source.assert_set_pin()?;
        }

        for sample_index in 0..CALIBRATION_REQUIRED_SAMPLES {
            if sample_index > 0 {
                selection.arm_calibration_sample(&capture_id_string).await?;
            }
            let pulse_at = Instant::now();
            let mut inner = selection.lock_source().await;
            for offset in 0..20 {
                inner
                    .source
                    .inject_simulator_pulse_at(pulse_at + Duration::from_millis(offset))?;
            }
            drop(inner);
            let status = selection
                .record_calibration_sample(&capture_id_string)
                .await?;
            if status.phase == CalibrationPhase::Capturing {
                continue;
            }
            assert_eq!(status.phase, CalibrationPhase::Completed);
        }

        let status = selection.calibration_status(&capture_id_string).await?;
        assert_eq!(status.sample_counts, vec![20, 20, 20, 20, 20]);
        let inner = selection.lock_source().await;
        assert!(!inner.source.is_set_pin_asserted());
        let transactions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&selection.pool)
            .await?;
        assert_eq!(transactions, 0);
        Ok(())
    }

    #[tokio::test]
    async fn expired_calibration_inhibits_and_discards_pulses() -> AppResult<()> {
        let source = ConfiguredCoinSource::simulator(fast_coin_config())?;
        let selection = selection_with_source(source).await?;
        let now = Instant::now();
        let capture_id = Uuid::new_v4();
        let capture_id_string = capture_id.to_string();
        {
            let mut inner = selection.lock_source().await;
            inner.calibration = Some(CalibrationSession {
                capture_id,
                target_amount_cents: 2000,
                phase: CalibrationPhase::Capturing,
                sample_counts: Vec::new(),
                sample_armed: true,
                completion_audit_pending: false,
                sampling_fault_generation: 0,
                current_pulses: Vec::new(),
                started_at: now,
                idle_deadline: now - Duration::from_millis(1),
                absolute_deadline: now + CALIBRATION_ABSOLUTE_TIMEOUT,
                started_at_unix_ms: 0,
                idle_deadline_unix_ms: 0,
                absolute_deadline_unix_ms: 300_000,
                failure_reason: None,
            });
            inner.source.assert_set_pin()?;
            inner.source.inject_simulator_pulse_at(now)?;
        }
        let status = selection.calibration_status(&capture_id_string).await?;
        assert_eq!(status.phase, CalibrationPhase::Expired);
        assert_eq!(status.current_pulse_count, 0);
        let inner = selection.lock_source().await;
        assert!(!inner.source.is_set_pin_asserted());
        assert!(!inner.source.has_buffered_pulses());
        drop(inner);
        assert!(selection.open("pc-01").await.is_ok());
        let _ = selection
            .close_at(CloseReason::Manual, Instant::now())
            .await;
        Ok(())
    }

    /// Fast timers so the lifecycle tests finish in well under a second.
    fn fast_config() -> TerminalSelectionConfig {
        TerminalSelectionConfig {
            inactivity_timeout: Duration::from_millis(100),
            hard_cap: Duration::from_millis(500),
        }
    }

    fn standard_config() -> TerminalSelectionConfig {
        TerminalSelectionConfig {
            inactivity_timeout: DEFAULT_INACTIVITY_TIMEOUT,
            hard_cap: DEFAULT_HARD_CAP,
        }
    }

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        db::seed(&pool).await?;
        Ok(pool)
    }

    fn test_events() -> EventHub {
        events::EventHub::without_ui()
    }

    fn shop_hours_input(
        enabled: bool,
        mode: crate::models::ShopHoursDayMode,
    ) -> crate::models::ShopHoursSettingsInput {
        crate::models::ShopHoursSettingsInput {
            enabled,
            last_call_lead_minutes: 15,
            days: (0..7)
                .map(|weekday| crate::models::ShopHoursDayInput {
                    weekday,
                    mode,
                    open_minute: None,
                    close_minute: None,
                })
                .collect(),
        }
    }

    async fn fresh_selection() -> AppResult<TerminalSelection<SimulatorCoinSource>> {
        fresh_selection_with(fast_config()).await
    }

    async fn fresh_selection_with(
        config: TerminalSelectionConfig,
    ) -> AppResult<TerminalSelection<SimulatorCoinSource>> {
        fresh_selection_with_coin_config(
            config,
            crate::coin_source::CoinSourceConfig::default_config(),
        )
        .await
    }

    async fn fresh_selection_with_coin_config(
        config: TerminalSelectionConfig,
        coin_config: crate::coin_source::CoinSourceConfig,
    ) -> AppResult<TerminalSelection<SimulatorCoinSource>> {
        let pool = test_pool().await?;
        let sessions = SessionStore::default();
        let events = test_events();
        let source = SimulatorCoinSource::new(coin_config)?;
        TerminalSelection::new(source, config, pool, sessions, events)
    }

    async fn selection_with_source<S: CoinSource>(source: S) -> AppResult<TerminalSelection<S>> {
        let pool = test_pool().await?;
        let sessions = SessionStore::default();
        let events = test_events();
        TerminalSelection::new(source, fast_config(), pool, sessions, events)
    }

    /// Test-only source that feeds a train's first half while the Set Pin is
    /// asserted, then feeds the remaining half from `inhibit_set_pin`. This
    /// lets the public production `close` path exercise a real straddling
    /// buffer without a serial thread or wall-clock race.
    struct StraddlingSource {
        observation: PulseObservation,
        set_pin_asserted: bool,
    }

    impl StraddlingSource {
        fn new() -> Self {
            Self {
                observation: PulseObservation::new(),
                set_pin_asserted: false,
            }
        }

        fn stage_first_half(&mut self) {
            let start = Instant::now()
                .checked_sub(Duration::from_millis(15))
                .expect("test clock must support a past train");
            self.observation.observe(false, true, start);
            for pulse in 0..15 {
                let rise = start + Duration::from_millis(pulse + 1);
                self.observation.observe(true, true, rise);
                self.observation
                    .observe(false, true, rise + Duration::from_micros(500));
            }
        }
    }

    impl CoinSource for StraddlingSource {
        fn try_next_batch_at(&mut self, now: Instant) -> Result<CoinBatchRead, AppError> {
            self.observation.close_batch_if_gap_elapsed(
                now,
                &crate::coin_source::CoinSourceConfig {
                    inter_coin_gap: Duration::from_millis(5),
                },
            )
        }

        fn assert_set_pin(&mut self) -> Result<(), AppError> {
            self.set_pin_asserted = true;
            Ok(())
        }

        fn inhibit_set_pin(&mut self) -> Result<(), AppError> {
            self.set_pin_asserted = false;
            let start = Instant::now();
            for pulse in 0..15 {
                let rise = start + Duration::from_millis(pulse + 1);
                self.observation.observe_with_result(true, false, rise);
                self.observation.observe_with_result(
                    false,
                    false,
                    rise + Duration::from_micros(500),
                );
            }
            Ok(())
        }

        fn is_set_pin_asserted(&self) -> bool {
            self.set_pin_asserted
        }

        fn is_available(&self) -> bool {
            true
        }

        fn kind(&self) -> CoinSourceKind {
            CoinSourceKind::Simulator
        }

        fn inter_coin_gap(&self) -> Duration {
            Duration::from_millis(5)
        }

        fn has_buffered_pulses(&self) -> bool {
            self.observation.has_buffered_pulses()
        }
    }

    /// Test-only double: yields scripted batch-read outcomes in order, then
    /// [`CoinBatchRead::None`] forever. Counts every read so tests can assert
    /// the drain loop's termination behaviour.
    struct ScriptedReadSource {
        reads: VecDeque<Result<CoinBatchRead, AppError>>,
        read_calls: usize,
        set_pin_asserted: bool,
    }

    impl ScriptedReadSource {
        fn new(reads: Vec<Result<CoinBatchRead, AppError>>) -> Self {
            Self {
                reads: reads.into_iter().collect(),
                read_calls: 0,
                set_pin_asserted: false,
            }
        }
    }

    impl CoinSource for ScriptedReadSource {
        fn try_next_batch_at(&mut self, _now: Instant) -> Result<CoinBatchRead, AppError> {
            self.read_calls += 1;
            match self.reads.pop_front() {
                Some(result) => result,
                None => Ok(CoinBatchRead::None),
            }
        }

        fn assert_set_pin(&mut self) -> Result<(), AppError> {
            self.set_pin_asserted = true;
            Ok(())
        }

        fn inhibit_set_pin(&mut self) -> Result<(), AppError> {
            self.set_pin_asserted = false;
            Ok(())
        }

        fn is_set_pin_asserted(&self) -> bool {
            self.set_pin_asserted
        }

        fn is_available(&self) -> bool {
            true
        }

        fn kind(&self) -> CoinSourceKind {
            CoinSourceKind::Simulator
        }

        fn inter_coin_gap(&self) -> Duration {
            Duration::from_millis(5)
        }
    }

    struct CloseFailingSource {
        set_pin_asserted: bool,
        inhibit_calls: usize,
    }

    impl CloseFailingSource {
        fn new() -> Self {
            Self {
                set_pin_asserted: false,
                inhibit_calls: 0,
            }
        }
    }

    impl CoinSource for CloseFailingSource {
        fn try_next_batch_at(&mut self, _now: Instant) -> Result<CoinBatchRead, AppError> {
            Ok(CoinBatchRead::None)
        }

        fn assert_set_pin(&mut self) -> Result<(), AppError> {
            self.set_pin_asserted = true;
            Ok(())
        }

        fn inhibit_set_pin(&mut self) -> Result<(), AppError> {
            self.inhibit_calls += 1;
            if self.inhibit_calls > 1 {
                return Err(AppError::InvalidInput("inhibit failed".to_owned()));
            }
            self.set_pin_asserted = false;
            Ok(())
        }

        fn is_set_pin_asserted(&self) -> bool {
            self.set_pin_asserted
        }

        fn is_available(&self) -> bool {
            true
        }

        fn kind(&self) -> CoinSourceKind {
            CoinSourceKind::Simulator
        }

        fn inter_coin_gap(&self) -> Duration {
            Duration::from_millis(50)
        }

        fn has_buffered_pulses(&self) -> bool {
            false
        }
    }

    fn now_after(start: Instant, offset: Duration) -> Instant {
        start
            .checked_add(offset)
            .expect("test clock offset must not overflow")
    }

    /// A `now_fn` for `drain_in_flight` tests that yields a pre-recorded
    /// sequence of times in order. Once exhausted it returns a value past
    /// `drain_until` so the drain loop terminates.
    fn scripted_clock(times: Vec<Instant>) -> impl Fn() -> Instant + Send + Sync {
        let index = Arc::new(StdMutex::new(0usize));
        move || {
            let mut guard = index.lock().expect("scripted clock mutex");
            let position = *guard;
            *guard += 1;
            times.get(position).copied().unwrap_or_else(Instant::now)
        }
    }

    #[tokio::test]
    async fn new_selection_is_inhibited_at_construction() -> AppResult<()> {
        let selection = fresh_selection().await?;
        let guard = selection.lock_source().await;
        assert!(
            !guard.source.is_set_pin_asserted(),
            "the set pin must start inhibited (fail-closed)"
        );
        assert!(
            guard.active.is_none(),
            "no selection window is open at construction"
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_asserts_set_pin_and_close_releases_it() -> AppResult<()> {
        let selection = fresh_selection().await?;
        let start = Instant::now();

        let snapshot = selection.open_at("pc-01", start).await?;
        assert_eq!(snapshot.pc_id, "pc-01");
        {
            let guard = selection.lock_source().await;
            assert!(
                guard.source.is_set_pin_asserted(),
                "set pin must be asserted while a window is open"
            );
            assert!(guard.active.is_some());
        }

        let closed = selection.close_at(CloseReason::Enter, start).await?;
        assert!(closed.is_some(), "close must return the snapshot");
        let guard = selection.lock_source().await;
        assert!(
            !guard.source.is_set_pin_asserted(),
            "set pin must be inhibited on close (fail-closed)"
        );
        assert!(guard.active.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn failed_close_keeps_selection_active_and_does_not_report_timeout() -> AppResult<()> {
        let selection = selection_with_source(CloseFailingSource::new()).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;

        let close_result = selection.close_at(CloseReason::Enter, start).await;
        assert!(
            matches!(close_result, Err(AppError::InvalidInput(_))),
            "close must surface the failed inhibit"
        );
        {
            let guard = selection.lock_source().await;
            assert!(
                guard.source.is_set_pin_asserted(),
                "failed inhibit means the source may still be asserted"
            );
            assert_eq!(
                guard.active.as_ref().map(|active| active.pc_id.as_str()),
                Some("pc-01"),
                "selection must stay active so any swallowed coin still routes"
            );
        }

        let reason = selection
            .tick_at(now_after(start, Duration::from_millis(150)))
            .await;
        assert_eq!(
            reason, None,
            "tick must not report a successful timeout close when inhibit failed"
        );
        assert!(selection.get().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn open_is_rejected_while_shop_is_closed_without_claiming_terminal() -> AppResult<()> {
        let selection = fresh_selection().await?;
        db::update_shop_hours(
            &selection.pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await?;

        let result = selection.open_at("pc-01", Instant::now()).await;
        assert!(matches!(result, Err(AppError::ShopClosed)));
        assert!(selection.get().await.is_none());
        let guard = selection.lock_source().await;
        assert!(!guard.source.is_set_pin_asserted());
        drop(guard);
        let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&selection.pool)
            .await?;
        assert_eq!(
            audit_count, 2,
            "Shop Hours save and the recorded transition are the audit events"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shop_overrides_gate_new_selection_windows() -> AppResult<()> {
        let selection = fresh_selection().await?;
        db::update_shop_hours(
            &selection.pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await?;
        db::set_shop_override(
            &selection.pool,
            &crate::models::ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceOpen,
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            },
        )
        .await?;
        selection.open_at("pc-01", Instant::now()).await?;
        selection
            .close_at(CloseReason::Manual, Instant::now())
            .await?;

        db::set_shop_override(
            &selection.pool,
            &crate::models::ShopOverrideInput {
                mode: openpiso_core::shop_hours::ShopOverrideMode::ForceClosed,
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            },
        )
        .await?;
        let result = selection.open_at("pc-01", Instant::now()).await;
        assert!(matches!(result, Err(AppError::ShopClosed)));
        Ok(())
    }

    #[tokio::test]
    async fn disabled_shop_hours_keeps_terminal_coin_intake_working() -> AppResult<()> {
        let selection = fresh_selection().await?;
        db::update_shop_hours(
            &selection.pool,
            &shop_hours_input(false, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await?;
        selection.open_at("pc-01", Instant::now()).await?;
        assert!(selection.get().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn an_open_selection_finishes_normally_after_shop_closes() -> AppResult<()> {
        let selection = fresh_selection_with_coin_config(fast_config(), fast_coin_config()).await?;
        let start = Instant::now();
        db::update_shop_hours(
            &selection.pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::Open24Hours),
        )
        .await?;
        selection.open_at("pc-01", start).await?;

        {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(10)));
        }
        db::update_shop_hours(
            &selection.pool,
            &shop_hours_input(true, crate::models::ShopHoursDayMode::ClosedAllDay),
        )
        .await?;

        let accepted = selection
            .try_accept_next_batch_at(now_after(start, Duration::from_millis(20)))
            .await?
            .expect("the already-open Selection Window still accepts coins");
        assert_eq!(accepted.transaction.seconds_added, 300);
        assert!(selection.get().await.is_some());
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&selection.pool)
                .await?;
        assert_eq!(remaining, 300);
        Ok(())
    }

    #[tokio::test]
    async fn second_open_is_rejected_with_terminal_in_use() -> AppResult<()> {
        let selection = fresh_selection().await?;
        let start = Instant::now();

        selection.open_at("pc-01", start).await?;
        let result = selection.open_at("pc-02", start).await;
        match result {
            Err(AppError::Conflict(message)) => {
                assert_eq!(message, TERMINAL_IN_USE);
            }
            other => panic!("expected terminal-in-use conflict, got {other:?}"),
        }

        // The first selection is still intact and the pin still asserted.
        let guard = selection.lock_source().await;
        assert!(guard.source.is_set_pin_asserted());
        assert_eq!(
            guard.active.as_ref().map(|a| a.pc_id.as_str()),
            Some("pc-01")
        );
        drop(guard);

        // After close, a new open for a different PC succeeds.
        selection.close_at(CloseReason::Enter, start).await?;
        let reopened = selection.open_at("pc-02", start).await?;
        assert_eq!(reopened.pc_id, "pc-02");
        Ok(())
    }

    #[tokio::test]
    async fn open_with_unknown_pc_is_rejected() -> AppResult<()> {
        let selection = fresh_selection().await?;
        let result = selection.open("pc-does-not-exist").await;
        assert!(matches!(result, Err(AppError::NotFound(_))));
        let guard = selection.lock_source().await;
        assert!(
            !guard.source.is_set_pin_asserted(),
            "a rejected open must leave the set pin inhibited"
        );
        assert!(guard.active.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn hidden_pc_cannot_open_a_terminal_selection() -> AppResult<()> {
        let selection = fresh_selection().await?;
        crate::db::update_pc_settings(
            &selection.pool_for_test(),
            "pc-01",
            &crate::models::PcSettingsInput {
                name: "PC 01".to_owned(),
                visible: false,
                registered_ip: None,
                mac_address: None,
            },
        )
        .await?;
        assert!(matches!(
            selection.open("pc-01").await,
            Err(AppError::Conflict(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn close_without_active_selection_is_a_noop() -> AppResult<()> {
        let selection = fresh_selection().await?;
        let result = selection.close(CloseReason::Manual).await?;
        assert!(result.is_none());
        let guard = selection.lock_source().await;
        assert!(!guard.source.is_set_pin_asserted());
        assert!(guard.active.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn get_returns_none_when_no_window_is_open() -> AppResult<()> {
        let selection = fresh_selection().await?;
        assert!(selection.get().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn get_returns_snapshot_while_open_and_none_after_close() -> AppResult<()> {
        let selection = fresh_selection().await?;
        let start = Instant::now();
        let snapshot = selection.open_at("pc-01", start).await?;
        let current = selection
            .get_at(now_after(start, Duration::from_millis(10)))
            .await
            .expect("get must return Some while a window is open");
        assert_eq!(current.pc_id, snapshot.pc_id);
        assert_eq!(current.opened_at_unix_ms, snapshot.opened_at_unix_ms);
        assert!(
            current.inactivity_remaining_seconds <= snapshot.inactivity_remaining_seconds,
            "inactivity remaining must not grow between samples"
        );

        selection.close_at(CloseReason::Enter, start).await?;
        assert!(selection.get().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn inactivity_timer_closes_and_resets_per_accepted_coin() -> AppResult<()> {
        let selection = fresh_selection_with_coin_config(fast_config(), fast_coin_config()).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;

        // No activity: tick at inactivity_timeout → close.
        let reason = selection
            .tick_at(now_after(start, Duration::from_millis(150)))
            .await;
        assert_eq!(reason, Some(CloseReason::Inactivity));
        assert!(selection.get().await.is_none());

        // A new window opens; an accepted coin must reset the inactivity
        // timer so the window survives past the original timeout. With the
        // 100ms inactivity config, the post-coin deadline is
        // (accept_time + 100ms); we tick just inside and just outside.
        selection.open_at("pc-01", start).await?;
        {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(20)));
        }
        let accept_at = now_after(start, Duration::from_millis(30));
        let accepted = selection
            .try_accept_next_batch_at(accept_at)
            .await?
            .expect("a coin should be accepted after the reset");
        assert_eq!(accepted.batch.pulse_count, 1);

        // Just past the original inactivity deadline but within the
        // post-coin window (accept_at + 100ms) → still open.
        assert_eq!(
            selection
                .tick_at(accept_at + Duration::from_millis(50))
                .await,
            None,
            "the accepted coin must reset the inactivity timer"
        );
        // After the post-coin inactivity window → closed.
        assert_eq!(
            selection
                .tick_at(accept_at + Duration::from_millis(150))
                .await,
            Some(CloseReason::Inactivity)
        );
        Ok(())
    }

    #[tokio::test]
    async fn hard_cap_closes_independent_of_activity() -> AppResult<()> {
        let mut source_config = crate::coin_source::CoinSourceConfig::default_config();
        source_config.inter_coin_gap = Duration::from_millis(5);
        let selection = selection_with_source(SimulatorCoinSource::new(source_config)?).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;

        // Constant activity should not extend the hard cap. Inject a pulse
        // every 20ms; the short test gap lets the source batch each one
        // independently.
        for i in 0..30 {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(i * 20)));
            drop(guard);
            selection
                .try_accept_next_batch_at(now_after(start, Duration::from_millis(i * 20 + 30)))
                .await?;
        }
        // 500ms (the hard cap in the test config) has passed; the window
        // closes regardless of the constant coin stream.
        assert_eq!(
            selection
                .tick_at(now_after(start, Duration::from_millis(510)))
                .await,
            Some(CloseReason::HardCap)
        );
        Ok(())
    }

    #[tokio::test]
    async fn hard_cap_takes_precedence_over_inactivity() -> AppResult<()> {
        // When both timers fire on the same tick, hard cap wins. This is
        // what `ActiveSelection::classify_timeout` enforces; pin a regression
        // test so a future refactor cannot silently swap the order.
        let selection = fresh_selection().await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        assert_eq!(
            selection
                .tick_at(now_after(start, Duration::from_millis(500)))
                .await,
            Some(CloseReason::HardCap)
        );
        Ok(())
    }

    #[tokio::test]
    async fn in_flight_batch_credits_just_selected_pc_on_close() -> AppResult<()> {
        let coin_config = fast_coin_config();
        let selection = fresh_selection_with_coin_config(fast_config(), coin_config).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;

        // Inject a single pulse and immediately close — the gap has not
        // elapsed so the source's batch buffer is non-empty and no batch
        // is closed yet. The close inhibits the set pin; the drain must
        // still credit the pulse to the just-selected PC.
        {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(10)));
        }
        let snapshot = selection.close_at(CloseReason::Enter, start).await?;
        assert!(snapshot.is_some());
        let guard = selection.lock_source().await;
        assert!(
            !guard.source.is_set_pin_asserted(),
            "close must inhibit the set pin before drain"
        );
        drop(guard);

        // Drain with a scripted clock that crosses the gap. The drain
        // stops when the source's buffer is empty and the deadline arrives.
        let times = vec![
            now_after(start, Duration::from_millis(12)), // before gap
            now_after(start, Duration::from_millis(18)), // gap elapsed → batch
            now_after(start, Duration::from_millis(50)), // no more pulses
        ];
        let now_fn = scripted_clock(times);
        let drained = selection
            .drain_in_flight(
                "pc-01",
                now_fn,
                now_after(start, Duration::from_millis(100)),
            )
            .await;
        assert_eq!(drained, 1, "exactly one batch must drain");

        // The PC received the credit through the canonical coin path.
        let pc_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&selection.pool_for_test())
                .await?;
        assert!(
            pc_seconds > 0,
            "the in-flight coin must credit the selected PC; got {pc_seconds} seconds"
        );
        let coin_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&selection.pool_for_test())
            .await?;
        assert_eq!(coin_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn production_close_drains_straddling_thirty_pulse_batch() -> AppResult<()> {
        let selection = selection_with_source(StraddlingSource::new()).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut guard = selection.lock_source().await;
            guard.source.stage_first_half();
        }

        // Production close inhibits the Set Pin; the source then admits the
        // remaining 15 edges because the first 15 pulses are buffered.
        let closed = selection.close(CloseReason::Enter).await?;
        assert!(closed.is_some());
        let guard = selection.lock_source().await;
        assert!(!guard.source.is_set_pin_asserted());
        drop(guard);

        let transaction_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions WHERE pc_id = 'pc-01'")
                .fetch_one(&selection.pool)
                .await?;
        assert_eq!(transaction_count, 1);
        let amount_cents: i64 =
            sqlx::query_scalar("SELECT amount_cents FROM coin_transactions WHERE pc_id = 'pc-01'")
                .fetch_one(&selection.pool)
                .await?;
        let seconds_added: i64 =
            sqlx::query_scalar("SELECT seconds_added FROM coin_transactions WHERE pc_id = 'pc-01'")
                .fetch_one(&selection.pool)
                .await?;
        assert_eq!(amount_cents, 3_000);
        assert_eq!(seconds_added, 9_000);

        let remaining_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&selection.pool)
                .await?;
        assert_eq!(remaining_seconds, 9_000);
        Ok(())
    }

    #[tokio::test]
    async fn production_close_waits_a_full_quiet_gap_after_delayed_tail() -> AppResult<()> {
        let mut source_config = fast_coin_config();
        source_config.inter_coin_gap = Duration::from_millis(200);
        let selection = fresh_selection_with_coin_config(fast_config(), source_config).await?;
        selection.open("pc-01").await?;
        {
            let mut inner = selection.lock_source().await;
            inner.source.inject_pulse_at(Instant::now());
        }

        let closing = selection.clone();
        let close_task = tokio::spawn(async move { closing.close(CloseReason::Enter).await });

        // Arrive after the old 100 ms headroom but before one inter-coin gap.
        // Close must then allow a complete quiet gap after this tail.
        tokio::time::sleep(Duration::from_millis(150)).await;
        {
            let mut inner = selection.lock_source().await;
            assert!(!inner.source.is_set_pin_asserted());
            assert!(inner.source.has_buffered_pulses());
            inner.source.inject_pulse_at(Instant::now());
        }

        assert!(close_task
            .await
            .map_err(|error| { AppError::Conflict(format!("close task failed: {error}")) })??
            .is_some());
        let amount_cents: i64 =
            sqlx::query_scalar("SELECT amount_cents FROM coin_transactions WHERE pc_id = 'pc-01'")
                .fetch_one(&selection.pool)
                .await?;
        assert_eq!(amount_cents, 200);
        let inner = selection.lock_source().await;
        assert!(!inner.closing);
        assert!(!inner.source.has_buffered_pulses());
        Ok(())
    }

    #[tokio::test]
    async fn drain_stops_on_transport_error() -> AppResult<()> {
        // Genuine transport/availability failures still surface as errors
        // and must still break the drain loop: the source is failing, so
        // further polling cannot recover buffered coins.
        let source = ScriptedReadSource::new(vec![Err(AppError::InvalidInput(
            "coin pin sampler unavailable".to_owned(),
        ))]);
        let selection = selection_with_source(source).await?;
        let drained = selection
            .drain_in_flight(
                "pc-01",
                scripted_clock(vec![Instant::now()]),
                Instant::now() + Duration::from_millis(100),
            )
            .await;
        assert_eq!(drained, 0, "a transport error credits nothing");
        let read_calls = selection.lock_source().await.source.read_calls;
        assert_eq!(
            read_calls, 1,
            "the drain loop must stop after the first transport error"
        );
        Ok(())
    }

    #[tokio::test]
    async fn adversarial_pulse_map_does_not_change_pulse_sum_credit() -> AppResult<()> {
        let selection = fresh_selection_with_coin_config(fast_config(), fast_coin_config()).await?;
        sqlx::query("DELETE FROM coin_device_pulse_map")
            .execute(&selection.pool_for_test())
            .await?;
        sqlx::query(
            "INSERT INTO coin_device_pulse_map (pulse_count, amount_cents) VALUES (1, 777)",
        )
        .execute(&selection.pool_for_test())
        .await?;

        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut guard = selection.lock_source().await;
            for _ in 0..9 {
                guard.source.inject_pulse_at(start);
            }
        }
        let accepted = selection
            .try_accept_next_batch_at(now_after(start, Duration::from_millis(10)))
            .await?
            .expect("the nine-pulse batch should close");
        assert_eq!(accepted.batch.pulse_count, 9);
        assert_eq!(accepted.transaction.amount_cents, 900);
        assert_eq!(accepted.transaction.seconds_added, 2_700);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM coin_transactions")
                .fetch_one(&selection.pool_for_test())
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'coin_batch_rejected'",
            )
            .fetch_one(&selection.pool_for_test())
            .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn pulse_sum_overflow_retains_pending_batch_and_inhibits_set_pin() -> AppResult<()> {
        let batch = CoinBatch {
            coin_trace_id: Uuid::new_v4(),
            pulse_count: i64::MAX,
            pulse_timings_us: Vec::new(),
            span_us: 0,
            gap_us: 5_000,
            attempt: 1,
        };
        let selection = selection_with_source(ScriptedReadSource::new(vec![Ok(
            CoinBatchRead::Batch(batch),
        )]))
        .await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;

        let result = selection
            .try_accept_next_batch_at(now_after(start, Duration::from_millis(10)))
            .await;
        assert!(
            matches!(result, Err(AppError::InvalidInput(message)) if message.contains("pulse sum"))
        );
        let guard = selection.lock_source().await;
        assert!(
            !guard.source.is_set_pin_asserted(),
            "overflow must inhibit Set Pin"
        );
        assert_eq!(
            guard
                .pending_batch
                .as_ref()
                .map(|pending| pending.pulse_count),
            Some(i64::MAX),
            "overflow must retain the physical batch for retry evidence"
        );
        drop(guard);
        let transactions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&selection.pool)
            .await?;
        assert_eq!(transactions, 0);
        Ok(())
    }

    #[tokio::test]
    async fn try_accept_next_batch_routes_to_selected_pc_as_guest() -> AppResult<()> {
        let selection = fresh_selection_with_coin_config(fast_config(), fast_coin_config()).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(0)));
        }
        let accepted = selection
            .try_accept_next_batch_at(now_after(start, Duration::from_millis(50)))
            .await?
            .expect("a batch should close after the gap elapses");
        assert_eq!(accepted.batch.pulse_count, 1);
        assert_eq!(accepted.transaction.amount_cents, 100);
        assert_eq!(accepted.transaction.pc_id, "pc-01");
        assert_eq!(accepted.transaction.target_kind, "guest");
        assert_eq!(accepted.transaction.account_id, None);
        assert_eq!(accepted.transaction.points_earned_centi, 0);

        let pc_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&selection.pool_for_test())
                .await?;
        assert_eq!(
            pc_seconds, 300,
            "₱1 must equal 300 seconds at the default rate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn accepted_coins_accumulate_on_the_selection_snapshot() -> AppResult<()> {
        // The kiosk insert screen renders `inserted_amount_cents` /
        // `inserted_seconds` straight off the broadcast snapshot. If the
        // accepted batch is credited to the PC but never accumulated on the
        // ActiveSelection, the screen sits at ₱0.00 / +0s while the money
        // lands. Assert both the returned snapshot and a later `get_at`.
        let selection = fresh_selection_with_coin_config(fast_config(), fast_coin_config()).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut guard = selection.lock_source().await;
            guard.source.inject_pulse_at(start);
        }
        selection
            .try_accept_next_batch_at(now_after(start, Duration::from_millis(50)))
            .await?
            .expect("a batch should close after the gap elapses");

        let state = selection
            .get_at(now_after(start, Duration::from_millis(60)))
            .await
            .expect("the window must still be open after one coin");
        assert_eq!(
            state.inserted_amount_cents, 100,
            "the insert screen must show the ₱1 that was credited"
        );
        assert_eq!(
            state.inserted_seconds, 300,
            "the insert screen must show the seconds the server added"
        );
        Ok(())
    }

    #[tokio::test]
    async fn simulator_pulses_route_through_active_selection_only() -> AppResult<()> {
        let selection = fresh_selection().await?;
        assert!(
            matches!(
                selection.inject_simulator_pulses(1).await,
                Err(AppError::Conflict(_))
            ),
            "simulator pulses require an active selection"
        );

        selection.open("pc-02").await?;
        selection.inject_simulator_pulses(5).await?;
        let accepted = selection
            .try_accept_next_batch_at(Instant::now() + DEFAULT_INTER_COIN_GAP)
            .await?
            .expect("simulated pulse batch should close");
        assert_eq!(accepted.batch.pulse_count, 5);
        assert_eq!(accepted.transaction.amount_cents, 500);
        assert_eq!(accepted.transaction.pc_id, "pc-02");

        let pc_one_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&selection.pool_for_test())
                .await?;
        let pc_two_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-02'")
                .fetch_one(&selection.pool_for_test())
                .await?;
        assert_eq!(pc_one_seconds, 0);
        assert!(pc_two_seconds > 0);
        Ok(())
    }

    #[tokio::test]
    async fn try_accept_next_batch_routes_to_logged_in_account_with_points() -> AppResult<()> {
        // When the selected PC has an active account session, the same
        // CoinBatch path must credit the account (time + points), not the
        // PC. This is the routing rule from ADR-0008 exercised end-to-end
        // through the selection state machine.
        let pool = test_pool().await?;
        let account = accounts::create(
            &pool,
            "router",
            "password",
            accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        sqlx::query(
            "INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (1000, 120)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE coin_promo_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        let sessions = SessionStore::default();
        sessions
            .login(&pool, &AccountTokens::default(), &account, "pc-01")
            .await?;
        let events = test_events();
        let source = SimulatorCoinSource::new(fast_coin_config())?;
        let selection = TerminalSelection::new(source, standard_config(), pool, sessions, events)?;

        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(0)));
        }
        // ₱10 = 10 pulses (default map) → 3000 seconds, earn carry test.
        for _ in 1..10 {
            let mut guard = selection.lock_source().await;
            guard
                .source
                .inject_pulse_at(now_after(start, Duration::from_millis(5)));
        }
        let accepted = selection
            .try_accept_next_batch_at(now_after(start, Duration::from_millis(60)))
            .await?
            .expect("a 10-pulse batch should close");
        assert_eq!(accepted.batch.pulse_count, 10);
        assert_eq!(accepted.transaction.amount_cents, 1000);
        assert_eq!(accepted.transaction.target_kind, "account");
        assert_eq!(
            accepted.transaction.account_id.as_deref(),
            Some(account.id.as_str())
        );
        assert_eq!(accepted.transaction.seconds_added, 3_120);
        assert_eq!(accepted.transaction.coin_promo_bonus_seconds, 120);
        assert!(accepted.transaction.points_earned_centi >= 0);

        let (account_seconds, _account_points, guest_seconds): (i64, i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT remaining_seconds FROM accounts WHERE id = ?),
               (SELECT points_balance_centi FROM accounts WHERE id = ?),
               (SELECT remaining_seconds FROM pcs WHERE id = 'pc-01')",
        )
        .bind(&account.id)
        .bind(&account.id)
        .fetch_one(&selection.pool_for_test())
        .await?;
        assert_eq!(
            guest_seconds, 0,
            "guest PC must not be credited when an account is logged in"
        );
        assert_eq!(account_seconds, 3_120, "the account must receive all time");
        Ok(())
    }

    #[tokio::test]
    async fn new_selection_with_real_timer_defaults_uses_adr_values() -> AppResult<()> {
        // Guard the constants so a future tuning change must be deliberate.
        assert_eq!(DEFAULT_INACTIVITY_TIMEOUT, Duration::from_secs(15));
        assert_eq!(DEFAULT_HARD_CAP, Duration::from_secs(180));
        let cfg = TerminalSelectionConfig::defaults();
        assert_eq!(cfg.inactivity_timeout, DEFAULT_INACTIVITY_TIMEOUT);
        assert_eq!(cfg.hard_cap, DEFAULT_HARD_CAP);
        Ok(())
    }

    #[tokio::test]
    async fn closing_blocks_reopen_until_configured_gap_is_fully_drained() -> AppResult<()> {
        let mut source_config = crate::coin_source::CoinSourceConfig::default_config();
        source_config.inter_coin_gap = Duration::from_millis(800);
        let source = SimulatorCoinSource::new(source_config)?;
        let selection = selection_with_source(source).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut inner = selection.lock_source().await;
            inner
                .source
                .inject_pulse_at(start + Duration::from_millis(10));
        }

        let (_snapshot, _pc_id) = selection
            .begin_close(CloseReason::Enter, start + Duration::from_millis(20))
            .await?;
        assert!(matches!(
            selection
                .open_at("pc-02", start + Duration::from_millis(30))
                .await,
            Err(AppError::Conflict(_))
        ));

        let drain_until = (start + Duration::from_millis(20))
            .checked_add(Duration::from_millis(1_600))
            .and_then(|d| d.checked_add(IN_FLIGHT_DRAIN_HEADROOM))
            .unwrap();
        let drained = selection
            .drain_in_flight(
                "pc-01",
                scripted_clock(vec![
                    start + Duration::from_millis(500),
                    start + Duration::from_millis(820),
                    start + Duration::from_millis(850),
                ]),
                drain_until,
            )
            .await;
        assert_eq!(drained, 1);
        selection.finish_close().await;
        selection
            .open_at("pc-02", start + Duration::from_millis(900))
            .await?;

        let pc_01_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&selection.pool_for_test())
                .await?;
        let pc_02_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-02'")
                .fetch_one(&selection.pool_for_test())
                .await?;
        assert_eq!(pc_01_seconds, 300);
        assert_eq!(pc_02_seconds, 0);
        Ok(())
    }

    #[tokio::test]
    async fn failed_mid_transaction_coin_commit_is_retried_exactly_once() -> AppResult<()> {
        let selection = fresh_selection_with(standard_config()).await?;
        let start = Instant::now();
        selection.open_at("pc-01", start).await?;
        {
            let mut inner = selection.lock_source().await;
            inner.source.inject_pulse_at(start);
        }
        sqlx::query(
            "CREATE TRIGGER fail_coin_insert BEFORE INSERT ON coin_transactions
             BEGIN SELECT RAISE(FAIL, 'forced coin insert failure'); END",
        )
        .execute(&selection.pool_for_test())
        .await?;

        let first = selection
            .try_accept_next_batch_at(start + DEFAULT_INTER_COIN_GAP)
            .await;
        assert!(matches!(first, Err(AppError::Database(_))));
        let after_failure: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds,
                    (SELECT COUNT(*) FROM coin_transactions)
             FROM pcs WHERE id = 'pc-01'",
        )
        .fetch_one(&selection.pool_for_test())
        .await?;
        assert_eq!(after_failure, (0, 0));
        let trace_id = {
            let inner = selection.lock_source().await;
            let pending = inner.pending_batch.as_ref().expect("failed batch retained");
            assert_eq!(pending.attempt, 2);
            assert!(!inner.source.is_set_pin_asserted());
            pending.coin_trace_id
        };

        sqlx::query("DROP TRIGGER fail_coin_insert")
            .execute(&selection.pool_for_test())
            .await?;
        let retried = selection
            .try_accept_next_batch_at(start + DEFAULT_INTER_COIN_GAP)
            .await?
            .expect("pending batch should retry");
        assert_eq!(retried.transaction.seconds_added, 300);
        assert_eq!(retried.batch.coin_trace_id, trace_id);
        assert_eq!(retried.batch.attempt, 3);
        let after_retry: (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT remaining_seconds,
                    (SELECT COUNT(*) FROM coin_transactions),
                    (SELECT json_extract(a.metadata_json, '$.coin_trace_id')
                     FROM coin_transactions c
                     JOIN audit_log a ON a.id = c.audit_event_id)
             FROM pcs WHERE id = 'pc-01'",
        )
        .fetch_one(&selection.pool_for_test())
        .await?;
        assert_eq!(after_retry.0, 300);
        assert_eq!(after_retry.1, 1);
        assert_eq!(
            after_retry.2.as_deref(),
            Some(trace_id.to_string().as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn is_source_available_reports_simulator_state() -> AppResult<()> {
        // The M2 simulator source is always available; the kiosk uses this
        // to render the normal flow rather than the "out of order" screen.
        let selection = fresh_selection().await?;
        assert!(
            selection.is_source_available().await,
            "the simulator source is always available"
        );
        Ok(())
    }

    /// Test-only accessor: expose the pool so SQL asserts can confirm the
    /// routing landed in the canonical `pcs` / `coin_transactions` rows.
    impl TerminalSelection<SimulatorCoinSource> {
        #[allow(dead_code)]
        pub fn pool_for_test(&self) -> SqlitePool {
            self.pool.clone()
        }
    }
}

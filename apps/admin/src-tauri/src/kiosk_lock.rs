//! Ephemeral Kiosk Lock state for the Central Terminal window (ADR-0056).
//!
//! The kiosk window boots always-on-top so no other window can cover the
//! customer surface. The owner releases it with a hidden gesture
//! (double-click the Shop Name) plus the owner PIN; re-engaging it needs no
//! PIN because that is the safe direction.
//!
//! The state is deliberately in-memory only — no settings row, no migration.
//! Every launch starts locked, which is the whole point of "the kiosk starts
//! always on top". A process restart is therefore also the crude recovery
//! path if the owner walks away mid-unlock.
//!
//! Brute-force protection reuses the pure [`AttemptState`] from
//! [`crate::login_throttle`] (the same backoff the Web Admin PIN login uses).
//! There is no per-IP key here: the gesture is physically local to the
//! terminal, so a single attempt counter is the whole population.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    login_throttle::{AttemptState, Decision, FAILURE_THRESHOLD},
    owner,
};

/// Shared, ephemeral Kiosk Lock. Cloneable handle over a single state cell.
#[derive(Clone, Default)]
pub struct KioskLock {
    inner: Arc<Mutex<LockState>>,
}

struct LockState {
    locked: bool,
    attempts: AttemptState,
}

/// Locked is the only valid starting state (ADR-0056): the window is born
/// always-on-top and Rust asserts it again during setup.
impl Default for LockState {
    fn default() -> Self {
        Self {
            locked: true,
            attempts: AttemptState::default(),
        }
    }
}

impl KioskLock {
    /// Build the lock around the state the window actually ended up in.
    /// The setup hook passes the *result* of pinning the window, never an
    /// assumption: if the OS refused, the header must say so rather than
    /// claim a pin the window does not have.
    pub fn new(locked: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LockState {
                locked,
                attempts: AttemptState::default(),
            })),
        }
    }

    pub async fn is_locked(&self) -> bool {
        self.inner.lock().await.locked
    }

    /// Re-engage the lock. Needs no PIN and never fails: tightening the
    /// kiosk is always allowed. Pending failure/backoff state is left
    /// untouched so re-locking cannot be used to wipe a lockout.
    pub async fn engage(&self) {
        self.inner.lock().await.locked = true;
    }

    /// Whether a release may be attempted at `now`, or how long the caller
    /// is locked out for. Checked *before* the PIN is verified so a lockout
    /// costs nothing to enforce.
    pub async fn release_decision(&self, now: DateTime<Utc>) -> Decision {
        self.inner.lock().await.attempts.decision(now)
    }

    /// Record a rejected release. Returns `Some(lockout_seconds)` only on the
    /// transition into a new lockout — the single point worth auditing.
    pub async fn record_release_failure(&self, now: DateTime<Utc>) -> Option<i64> {
        self.inner.lock().await.attempts.record_failure(now)
    }

    /// Record an accepted release: the kiosk is unlocked and the backoff
    /// history is cleared.
    pub async fn record_release_success(&self) {
        let mut state = self.inner.lock().await;
        state.locked = false;
        state.attempts.record_success();
    }
}

/// Audit an applied lock change. Called only once the window flag and the
/// in-memory state have both moved, so the log records what happened rather
/// than what was attempted.
pub async fn audit_state_change(pool: &SqlitePool, locked: bool) -> AppResult<()> {
    let event = if locked {
        NewAuditEvent::simple(
            "kiosk_lock_engaged",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Kiosk Lock re-engaged (always-on-top on)",
        )
    } else {
        NewAuditEvent::simple(
            "kiosk_lock_released",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Kiosk Lock released (always-on-top off)",
        )
    };
    audit::append_standalone(pool, event).await?;
    Ok(())
}

/// Decide whether a release may proceed, and audit the outcome.
///
/// Deliberately does *not* touch the lock state or the window: the caller
/// applies the window flag first and only then calls
/// [`KioskLock::record_release_success`], so a failed window call cannot
/// leave the kiosk reporting a state the OS does not have. Split out from
/// the Tauri command so the PIN, throttle, and audit rules are testable
/// without an event loop or a window handle.
pub async fn authorize_release(
    pool: &SqlitePool,
    lock: &KioskLock,
    pin: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    // A locked-out terminal is rejected before the PIN is verified, so the
    // credential is never exercised under a brute-force attempt.
    if let Decision::Locked { retry_after_secs } = lock.release_decision(now).await {
        return Err(AppError::Unauthorized(format!(
            "too many attempts; try again in {retry_after_secs}s"
        )));
    }

    if owner::verify_pin(pool, pin).await? {
        return Ok(());
    }

    // Audit every denial — the kiosk is physically exposed, so each failed
    // unlock is worth seeing — plus the transition into a lockout.
    audit::append_standalone(
        pool,
        NewAuditEvent::simple(
            "kiosk_lock_release_denied",
            AuditOutcome::Denied,
            AuditActor::Admin,
            "Kiosk Lock release denied (invalid owner PIN)",
        ),
    )
    .await?;
    if let Some(lockout_seconds) = lock.record_release_failure(now).await {
        let mut event = NewAuditEvent::simple(
            "kiosk_lock_release_throttled",
            AuditOutcome::Denied,
            AuditActor::Admin,
            "Kiosk Lock release throttled after repeated failures",
        );
        event.metadata = json!({
            "consecutive_failures": FAILURE_THRESHOLD,
            "lockout_seconds": lockout_seconds,
        });
        audit::append_standalone(pool, event).await?;
    }
    Err(AppError::Unauthorized("invalid owner PIN".to_owned()))
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::db;

    const PIN: &str = "owner-pin-1234";

    async fn pool_with_owner() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        owner::setup(&pool, PIN).await?;
        Ok(pool)
    }

    async fn audit_count(pool: &SqlitePool, event_type: &str) -> AppResult<i64> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE event_type = ?")
                .bind(event_type)
                .fetch_one(pool)
                .await?,
        )
    }

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    #[tokio::test]
    async fn starts_locked() {
        assert!(KioskLock::default().is_locked().await);
    }

    #[tokio::test]
    async fn release_unlocks_and_engage_relocks() {
        let lock = KioskLock::default();
        lock.record_release_success().await;
        assert!(!lock.is_locked().await);
        lock.engage().await;
        assert!(lock.is_locked().await);
    }

    #[tokio::test]
    async fn repeated_failures_lock_out_before_the_pin_is_checked() {
        let lock = KioskLock::default();
        let now = t0();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert_eq!(lock.record_release_failure(now).await, None);
            assert_eq!(lock.release_decision(now).await, Decision::Allow);
        }
        assert!(lock.record_release_failure(now).await.is_some());
        assert!(matches!(
            lock.release_decision(now).await,
            Decision::Locked { .. }
        ));
        // The kiosk is still locked: a throttled attempt never released it.
        assert!(lock.is_locked().await);
    }

    #[tokio::test]
    async fn re_engaging_does_not_clear_a_lockout() {
        let lock = KioskLock::default();
        let now = t0();
        for _ in 0..FAILURE_THRESHOLD {
            lock.record_release_failure(now).await;
        }
        lock.engage().await;
        assert!(matches!(
            lock.release_decision(now).await,
            Decision::Locked { .. }
        ));
    }

    #[tokio::test]
    async fn a_correct_pin_authorizes_a_release_without_unlocking_yet() -> AppResult<()> {
        let pool = pool_with_owner().await?;
        let lock = KioskLock::default();

        authorize_release(&pool, &lock, PIN, t0()).await?;

        // Authorization alone must not move the state — the caller applies the
        // window flag first, then records the success.
        assert!(lock.is_locked().await);
        assert_eq!(audit_count(&pool, "kiosk_lock_release_denied").await?, 0);
        // A kiosk unlock is not a Web Admin sign-in and must not be logged as
        // one, or the login history stops meaning anything.
        assert_eq!(audit_count(&pool, "owner_login_succeeded").await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn applied_changes_are_audited_under_their_own_event_types() -> AppResult<()> {
        let pool = pool_with_owner().await?;

        audit_state_change(&pool, false).await?;
        audit_state_change(&pool, true).await?;

        assert_eq!(audit_count(&pool, "kiosk_lock_released").await?, 1);
        assert_eq!(audit_count(&pool, "kiosk_lock_engaged").await?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_wrong_pin_is_denied_and_audited() -> AppResult<()> {
        let pool = pool_with_owner().await?;
        let lock = KioskLock::default();

        let error = authorize_release(&pool, &lock, "not-the-pin", t0())
            .await
            .expect_err("wrong PIN is rejected");

        assert!(matches!(error, AppError::Unauthorized(_)));
        assert!(lock.is_locked().await);
        assert_eq!(audit_count(&pool, "kiosk_lock_release_denied").await?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_wrong_pins_throttle_and_audit_the_lockout_once() -> AppResult<()> {
        let pool = pool_with_owner().await?;
        let lock = KioskLock::default();
        let now = t0();

        for _ in 0..FAILURE_THRESHOLD {
            assert!(authorize_release(&pool, &lock, "wrong", now).await.is_err());
        }
        assert_eq!(audit_count(&pool, "kiosk_lock_release_throttled").await?, 1);

        // While locked out, even the *correct* PIN is refused — and the
        // refusal is not counted as another denial, because the credential
        // is never verified.
        let denials_before = audit_count(&pool, "kiosk_lock_release_denied").await?;
        let error = authorize_release(&pool, &lock, PIN, now)
            .await
            .expect_err("a locked-out terminal refuses every attempt");
        assert!(
            matches!(&error, AppError::Unauthorized(message) if message.contains("try again in")),
            "expected a retry-after message, got {error}"
        );
        assert_eq!(
            audit_count(&pool, "kiosk_lock_release_denied").await?,
            denials_before
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_successful_release_clears_the_backoff() {
        let lock = KioskLock::default();
        let now = t0();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            lock.record_release_failure(now).await;
        }
        lock.record_release_success().await;
        lock.engage().await;
        // Back to a clean slate: it takes a full threshold to lock again.
        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert_eq!(lock.record_release_failure(now).await, None);
        }
        assert!(lock.record_release_failure(now).await.is_some());
    }
}

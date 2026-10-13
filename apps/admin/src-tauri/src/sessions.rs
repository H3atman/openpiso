use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    session::{SessionEndReason, TimerMode},
};
use serde_json::json;
use sqlx::{QueryBuilder, Sqlite, SqlitePool, Transaction};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    auth::{AccountToken, AccountTokens},
    db,
    errors::{AppError, AppResult},
    models::{
        Account, ActiveSession, CoinEventInput, CoinSourceClaim, CoinTarget, CoinTransaction,
        GuardCoverageReport, Pc, PointsTransferRequest,
    },
};

/// One Escalated Power Off decision, as the monitor hands it over (ADR-0077).
#[derive(Debug, Clone, Copy)]
pub struct EscalatedPowerOff<'a> {
    pub pc_id: &'a str,
    pub pc_name: &'a str,
    /// Relaunch attempts the Guard reported for this absence — the evidence
    /// that local recovery was tried and failed.
    pub relaunch_attempts: i64,
    /// Idempotence key the Guard acts on exactly once.
    pub nonce: &'a str,
    pub now: chrono::DateTime<chrono::Utc>,
}

#[derive(Default)]
struct Bindings {
    by_account: HashMap<String, ActiveSession>,
    account_by_pc: HashMap<String, String>,
}

/// Only these methods edit the maps, so account and PC lookups never disagree.
impl Bindings {
    fn for_pc(&self, pc_id: &str) -> Option<&ActiveSession> {
        self.account_by_pc
            .get(pc_id)
            .and_then(|account_id| self.by_account.get(account_id))
    }

    fn install(&mut self, session: ActiveSession) {
        self.remove(&session.account_id);
        self.remove_pc(&session.pc_id);
        self.account_by_pc
            .insert(session.pc_id.clone(), session.account_id.clone());
        self.by_account.insert(session.account_id.clone(), session);
    }

    fn remove(&mut self, account_id: &str) -> Option<ActiveSession> {
        let session = self.by_account.remove(account_id)?;
        self.account_by_pc.remove(&session.pc_id);
        Some(session)
    }

    fn remove_pc(&mut self, pc_id: &str) -> Option<ActiveSession> {
        let account_id = self.account_by_pc.remove(pc_id)?;
        self.by_account.remove(&account_id)
    }
}

#[derive(Clone, Default)]
pub struct SessionStore(Arc<Mutex<Bindings>>);

/// What an Owner Sign-Out actually did. The owner clicks from a dashboard
/// snapshot that refreshes every few seconds, so "there was nothing left to
/// sign out" is an ordinary outcome, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignOutOutcome {
    SignedOut { username: String },
    AlreadyEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TamperKillSubject {
    Account,
    Guest,
    None,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TamperKillUnknownReason {
    MissingReportAge,
    InvalidReportAge,
    CurrentOccupantNotAttributed,
}

impl TamperKillUnknownReason {
    fn description(self) -> (&'static str, &'static str) {
        match self {
            Self::MissingReportAge => (
                "missing_report_age",
                "Client did not report a Kill Report Age; it may be an older Client",
            ),
            Self::InvalidReportAge => (
                "invalid_report_age",
                "Kill Report Age was malformed or exceeded the 24-hour sanity limit",
            ),
            Self::CurrentOccupantNotAttributed => (
                "current_occupant_not_attributed",
                "no Account Session covered the kill instant; the current occupant was not blamed",
            ),
        }
    }

    fn as_str(self) -> &'static str {
        self.description().0
    }

    fn summary(self) -> &'static str {
        self.description().1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TamperKillReportAge {
    Missing,
    Valid(i64),
    Invalid,
}

impl TamperKillReportAge {
    fn milliseconds(self) -> Option<i64> {
        match self {
            Self::Valid(age) => Some(age),
            Self::Missing | Self::Invalid => None,
        }
    }
}

pub struct TamperKillReport {
    pub kill_timestamp: String,
    pub report_age: TamperKillReportAge,
    pub attempt_count: i64,
    pub received_at: chrono::DateTime<chrono::Utc>,
}

/// Maximum Kill Report Age accepted for a server-anchored kill instant.
/// This is a sanity limit, not a freshness window.
pub const MAX_TAMPER_KILL_REPORT_AGE_MS: i64 = 24 * 60 * 60 * 1_000;

pub struct IdentityHeartbeatInput<'a> {
    pub mac: Option<&'a str>,
    pub source_ip: &'a str,
    pub hostname: &'a str,
    pub client_version: &'a str,
    pub client_time_unix_ms: Option<i64>,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub report: Option<TamperKillReport>,
    /// Last-mile Guard Coverage diagnostic from the HTTP boundary (ADR-0076).
    /// `None` when the Heartbeat omitted the field (older Client); it is
    /// stored as the PC's latest diagnostic and never read for authority.
    pub guard_coverage: Option<GuardCoverageReport>,
}

pub struct IdentityHeartbeatResult {
    pub pc: Pc,
    pub tamper_kill_subject: Option<TamperKillSubject>,
}

struct TamperKillCommit {
    subject: TamperKillSubject,
    kicked_session: Option<ActiveSession>,
}

impl TamperKillSubject {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Guest => "guest",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }

    fn from_str(value: Option<&str>) -> Self {
        match value {
            Some("account") => Self::Account,
            Some("guest") => Self::Guest,
            Some("none") => Self::None,
            _ => Self::Unknown,
        }
    }
}

/// Result of an immediate Time Transfer at the session layer: the sender's
/// refreshed live session plus, when the recipient is signed in somewhere,
/// the PC whose timer the caller should re-publish (ADR-0030).
pub struct TimeTransferSessions {
    pub sender: ActiveSession,
    pub recipient_pc_id: Option<String>,
}

/// Result of an owner-resolved Points Transfer at the session layer: the
/// persisted request plus the PCs whose live sessions were refreshed, so the
/// caller can re-publish their timers at once (ADR-0030).
pub struct PointsTransferSessions {
    pub request: PointsTransferRequest,
    pub changed_pc_ids: Vec<String>,
}

/// Result of an owner-initiated balance adjustment at the session layer: the
/// updated account plus, when that account is signed in somewhere, the PC
/// whose timer the caller should re-publish.
pub struct AccountAdjustSessions {
    pub account: Account,
    pub pc_id: Option<String>,
}

impl SessionStore {
    /// Close Account Session history rows left open by the previous Admin
    /// process. This does not recreate any live binding; restart recovery is
    /// history-only per ADR-0013.
    pub async fn recover_open_sessions(pool: &SqlitePool) -> AppResult<()> {
        db::recover_open_account_sessions(pool).await
    }

    pub async fn login(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        account: &Account,
        pc_id: &str,
    ) -> AppResult<(ActiveSession, String)> {
        self.login_inner(pool, tokens, account, pc_id, false).await
    }

    /// Establish a session after HTTP credentials were verified. The
    /// successful-login audit is part of the same SQLite transaction as the
    /// binding, so a stale credential cannot produce a false success event.
    pub async fn login_after_authentication(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        account: &Account,
        pc_id: &str,
    ) -> AppResult<(ActiveSession, String)> {
        self.login_inner(pool, tokens, account, pc_id, true).await
    }

    async fn login_inner(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        account: &Account,
        pc_id: &str,
        record_success_audit: bool,
    ) -> AppResult<(ActiveSession, String)> {
        let pc_id = pc_id.trim();
        if pc_id.is_empty() {
            return Err(AppError::InvalidInput("PC ID is required".to_owned()));
        }

        let mut bindings = self.0.lock().await;
        let mut transaction = crate::db::begin_immediate(pool).await?;
        let account_is_active = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                 SELECT 1 FROM accounts WHERE id = ? AND status = 'active'
             )",
        )
        .bind(&account.id)
        .fetch_one(&mut *transaction)
        .await?;
        if account_is_active == 0 {
            transaction.rollback().await?;
            return Err(AppError::Unauthorized(
                "invalid username or password".to_owned(),
            ));
        }
        if record_success_audit {
            let mut event = NewAuditEvent::simple(
                "login_succeeded",
                AuditOutcome::Success,
                AuditActor::Account,
                "Customer authenticated",
            );
            event.actor_account_id = Some(&account.id);
            event.account_id = Some(&account.id);
            event.pc_id = Some(pc_id);
            audit::append(&mut transaction, event).await?;
        }
        let guest_remaining_seconds =
            sqlx::query_scalar::<_, i64>("SELECT remaining_seconds FROM pcs WHERE id = ?")
                .bind(pc_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;

        let previous_for_account = bindings.by_account.get(&account.id).cloned();
        let previous_account_for_pc = bindings.for_pc(pc_id).cloned();
        let resumed = previous_for_account
            .as_ref()
            .is_some_and(|session| session.pc_id == pc_id);

        if let Some(previous) = previous_for_account
            .as_ref()
            .filter(|session| session.pc_id != pc_id)
        {
            append_end_event(
                &mut transaction,
                previous,
                SessionEndReason::AutoKick,
                Some(pc_id),
            )
            .await?;
        }
        if let Some(previous) = previous_account_for_pc
            .as_ref()
            .filter(|session| session.account_id != account.id)
        {
            append_end_event(
                &mut transaction,
                previous,
                SessionEndReason::TakenOver,
                Some(&account.id),
            )
            .await?;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let started_at = previous_for_account
            .as_ref()
            .filter(|previous| previous.pc_id == pc_id)
            .map(|previous| previous.started_at.clone())
            .unwrap_or_else(|| now);
        let session = ActiveSession {
            id: previous_for_account
                .as_ref()
                .filter(|previous| previous.pc_id == pc_id)
                .map(|previous| previous.id.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            account_id: account.id.clone(),
            username: account.username.clone(),
            pc_id: pc_id.to_owned(),
            remaining_seconds: account.remaining_seconds.max(0),
            points_balance_centi: account.points_balance_centi.max(0),
            claimable_guest_seconds: guest_remaining_seconds.max(0),
            started_at,
        };
        if !resumed {
            db::start_account_session(&mut transaction, &session).await?;
        }
        let mut event = NewAuditEvent::simple(
            "login",
            AuditOutcome::Success,
            AuditActor::Account,
            if resumed {
                "Resumed active account session"
            } else {
                "Started active account session"
            },
        );
        event.actor_account_id = Some(&account.id);
        event.account_id = Some(&account.id);
        event.pc_id = Some(pc_id);
        event.metadata = json!({
            "session_id": session.id,
            "resumed": resumed
        });
        audit::append(&mut transaction, event).await?;
        transaction.commit().await?;

        bindings.install(session.clone());
        let token = tokens.issue(&account.id, pc_id, &session.id).await;
        Ok((session, token))
    }

    pub async fn claim(&self, pool: &SqlitePool, token: &AccountToken) -> AppResult<ActiveSession> {
        let mut bindings = self.0.lock().await;
        let session = require_current(&bindings, token)?.clone();
        let mut transaction = crate::db::begin_immediate(pool).await?;
        let guest_seconds =
            sqlx::query_scalar::<_, i64>("SELECT remaining_seconds FROM pcs WHERE id = ?")
                .bind(&session.pc_id)
                .fetch_one(&mut *transaction)
                .await?;
        if guest_seconds <= 0 {
            transaction.rollback().await?;
            let mut event = NewAuditEvent::simple(
                "claim",
                AuditOutcome::Denied,
                AuditActor::Account,
                "Guest Time claim denied",
            );
            event.actor_account_id = Some(&session.account_id);
            event.account_id = Some(&session.account_id);
            event.pc_id = Some(&session.pc_id);
            event.metadata = json!({ "reason": "no_guest_time" });
            audit::append_standalone(pool, event).await?;
            return Err(AppError::Conflict(
                "there is no Guest Time to claim".to_owned(),
            ));
        }

        sqlx::query(
            "UPDATE pcs SET remaining_seconds = 0, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&session.pc_id)
        .execute(&mut *transaction)
        .await?;
        let remaining_seconds = sqlx::query_scalar::<_, i64>(
            "UPDATE accounts
             SET remaining_seconds = remaining_seconds + ?, updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND status = 'active'
             RETURNING remaining_seconds",
        )
        .bind(guest_seconds)
        .bind(&session.account_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| AppError::NotFound("active account".to_owned()))?;
        let mut event = NewAuditEvent::simple(
            "claim",
            AuditOutcome::Success,
            AuditActor::Account,
            "Moved all Guest Time to the account",
        );
        event.actor_account_id = Some(&session.account_id);
        event.account_id = Some(&session.account_id);
        event.pc_id = Some(&session.pc_id);
        event.metadata = json!({ "transferred_seconds": guest_seconds });
        audit::append(&mut transaction, event).await?;
        transaction.commit().await?;

        let current = bindings
            .by_account
            .get_mut(&session.account_id)
            .ok_or_else(|| AppError::Conflict("session is no longer active".to_owned()))?;
        current.remaining_seconds = remaining_seconds;
        current.claimable_guest_seconds = 0;
        Ok(current.clone())
    }

    pub async fn logout(&self, pool: &SqlitePool, token: &AccountToken) -> AppResult<()> {
        let mut bindings = self.0.lock().await;
        let session = match require_current(&bindings, token) {
            Ok(session) => session.clone(),
            Err(AppError::Unauthorized(_)) => {
                // Session already ended (e.g. tick expired it, or it was
                // displaced by another login). Treat as already-logged-out
                // and return success — no active binding to clean up.
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut transaction = crate::db::begin_immediate(pool).await?;
        append_end_event(&mut transaction, &session, SessionEndReason::Logout, None).await?;
        transaction.commit().await?;
        bindings.remove(&session.account_id);
        Ok(())
    }

    /// End the Active Session on `pc_id` on the owner's behalf. The lifecycle
    /// effect is exactly a Logout (ADR-0001/0006): the balance stays on the
    /// account, the binding is dropped, and the PC falls back to its own Guest
    /// Time. Only the Audit Log distinguishes the two.
    ///
    /// Keyed on `session_id` rather than `pc_id` alone: the dashboard row the
    /// owner clicked may be seconds stale, and "end whatever is on this PC"
    /// would eject whoever took the seat in the meantime.
    pub async fn owner_sign_out(
        &self,
        pool: &SqlitePool,
        pc_id: &str,
        session_id: &str,
    ) -> AppResult<SignOutOutcome> {
        let mut bindings = self.0.lock().await;
        let session = match bindings.for_pc(pc_id) {
            Some(session) if session.id == session_id => session.clone(),
            _ => return Ok(SignOutOutcome::AlreadyEnded),
        };
        let mut transaction = crate::db::begin_immediate(pool).await?;
        append_owner_sign_out_event(&mut transaction, &session).await?;
        transaction.commit().await?;
        bindings.remove(&session.account_id);
        Ok(SignOutOutcome::SignedOut {
            username: session.username,
        })
    }

    pub async fn record_coin_event(
        &self,
        pool: &SqlitePool,
        input: &CoinEventInput,
        claim: &CoinSourceClaim,
    ) -> AppResult<CoinTransaction> {
        self.record_coin_event_inner(pool, input, claim, None).await
    }

    pub async fn record_coin_event_with_trace(
        &self,
        pool: &SqlitePool,
        input: &CoinEventInput,
        claim: &CoinSourceClaim,
        coin_trace_id: Option<Uuid>,
    ) -> AppResult<CoinTransaction> {
        self.record_coin_event_inner(pool, input, claim, coin_trace_id)
            .await
    }

    async fn record_coin_event_inner(
        &self,
        pool: &SqlitePool,
        input: &CoinEventInput,
        claim: &CoinSourceClaim,
        coin_trace_id: Option<Uuid>,
    ) -> AppResult<CoinTransaction> {
        let mut bindings = self.0.lock().await;
        let active_account_id = bindings.account_by_pc.get(input.pc_id.trim()).cloned();
        let target = active_account_id
            .as_deref()
            .map(|account_id| CoinTarget::Account { account_id })
            .unwrap_or(CoinTarget::Guest);

        let transaction = db::record_coin_event(pool, input, target, claim, coin_trace_id).await?;

        if let Some(account_id) = active_account_id {
            if let Some(session) = bindings.by_account.get_mut(&account_id) {
                session.remaining_seconds = session
                    .remaining_seconds
                    .saturating_add(transaction.seconds_added);
                session.points_balance_centi = session
                    .points_balance_centi
                    .saturating_add(transaction.points_earned_centi);
            }
        }

        Ok(transaction)
    }

    pub async fn redeem_voucher(
        &self,
        pool: &SqlitePool,
        token: &AccountToken,
        code: &str,
    ) -> AppResult<ActiveSession> {
        let mut bindings = self.0.lock().await;
        let session = require_current(&bindings, token)?.clone();
        let mutation = db::redeem_voucher(pool, &session.account_id, &session.pc_id, code).await?;
        let current = bindings
            .by_account
            .get_mut(&session.account_id)
            .ok_or_else(|| AppError::Conflict("session is no longer active".to_owned()))?;
        current.remaining_seconds = mutation.account.remaining_seconds;
        current.points_balance_centi = mutation.account.points_balance_centi;
        Ok(current.clone())
    }

    pub async fn redeem_points(
        &self,
        pool: &SqlitePool,
        token: &AccountToken,
        tier_id: i64,
        version: i64,
    ) -> AppResult<ActiveSession> {
        let mut bindings = self.0.lock().await;
        let session = require_current(&bindings, token)?.clone();
        let mutation =
            db::redeem_tier(pool, &session.account_id, &session.pc_id, tier_id, version).await?;
        let current = bindings
            .by_account
            .get_mut(&session.account_id)
            .ok_or_else(|| AppError::Conflict("session is no longer active".to_owned()))?;
        current.remaining_seconds = mutation.account.remaining_seconds;
        current.points_balance_centi = mutation.account.points_balance_centi;
        Ok(current.clone())
    }

    /// Validate the live in-memory Account Session before reading customer data.
    pub async fn validate_attendance_session(&self, token: &AccountToken) -> AppResult<()> {
        let bindings = self.0.lock().await;
        require_current(&bindings, token).map(|_| ())
    }

    /// Claim Attendance Time while holding the session binding lock, then
    /// refresh the active session from the committed Account balance.
    pub async fn claim_attendance(
        &self,
        pool: &SqlitePool,
        token: &AccountToken,
        request_id: Uuid,
        earning_month: &str,
    ) -> AppResult<db::attendance_claims::ClaimOutcome> {
        let mut bindings = self.0.lock().await;
        let session = require_current(&bindings, token)?.clone();
        let outcome = db::attendance_claims::claim(
            pool,
            chrono::Utc::now,
            &session.account_id,
            Some(&session.pc_id),
            request_id,
            earning_month,
        )
        .await?;
        if matches!(
            &outcome,
            db::attendance_claims::ClaimOutcome::Claimed { .. }
        ) {
            let account = crate::accounts::get(pool, &session.account_id).await?;
            let current = bindings
                .by_account
                .get_mut(&session.account_id)
                .ok_or_else(|| AppError::Conflict("session is no longer active".to_owned()))?;
            current.remaining_seconds = account.remaining_seconds.max(0);
            current.points_balance_centi = account.points_balance_centi.max(0);
        }
        Ok(outcome)
    }

    /// Immediate, server-authoritative Time Transfer from the signed-in
    /// account to another account by Username (ADR-0030). The atomic debit /
    /// credit and audit pair live in [`crate::accounts::transfer_time`]; here
    /// we refresh the sender's cached session so the live timer reflects the
    /// new balance at once. The recipient's cached session (if they are on
    /// another PC) re-syncs from the authoritative balance on its next tick.
    pub async fn transfer_time(
        &self,
        pool: &SqlitePool,
        token: &AccountToken,
        to_username: &str,
        seconds: i64,
    ) -> AppResult<TimeTransferSessions> {
        let mut bindings = self.0.lock().await;
        let session = require_current(&bindings, token)?.clone();
        let outcome =
            crate::accounts::transfer_time(pool, &session.account_id, to_username, seconds).await?;
        let current = bindings
            .by_account
            .get_mut(&session.account_id)
            .ok_or_else(|| AppError::Conflict("session is no longer active".to_owned()))?;
        current.remaining_seconds = outcome.sender.remaining_seconds;
        current.points_balance_centi = outcome.sender.points_balance_centi;
        let sender_session = current.clone();
        // If the recipient is signed in on some PC, refresh their cached
        // session to the authoritative new balance and report the PC so the
        // caller can publish a fresh timer at once — otherwise the credit
        // would look stale until the recipient's next tick.
        let recipient_pc_id = bindings
            .by_account
            .get_mut(&outcome.recipient_id)
            .map(|recipient| {
                recipient.remaining_seconds = outcome.recipient_remaining_seconds;
                recipient.pc_id.clone()
            });
        Ok(TimeTransferSessions {
            sender: sender_session,
            recipient_pc_id,
        })
    }

    /// Owner-confirmed Points Transfer (ADR-0030). The atomic move and audit
    /// pair live in [`crate::accounts::confirm_points_transfer`]; here we also
    /// refresh the cached live sessions of any signed-in party so heartbeats
    /// report the new balances immediately instead of after a re-login.
    pub async fn confirm_points_transfer(
        &self,
        pool: &SqlitePool,
        code: &str,
    ) -> AppResult<PointsTransferSessions> {
        let mut bindings = self.0.lock().await;
        let request = crate::accounts::confirm_points_transfer(pool, code).await?;
        let mut changed_pc_ids = Vec::new();
        for account_id in [&request.from_account_id, &request.to_account_id] {
            let Some(session) = bindings.by_account.get_mut(account_id) else {
                continue;
            };
            let balance = sqlx::query_scalar::<_, i64>(
                "SELECT points_balance_centi FROM accounts WHERE id = ?",
            )
            .bind(account_id)
            .fetch_one(pool)
            .await?;
            session.points_balance_centi = balance.max(0);
            changed_pc_ids.push(session.pc_id.clone());
        }
        Ok(PointsTransferSessions {
            request,
            changed_pc_ids,
        })
    }

    /// Owner-initiated time adjustment (web-admin). The atomic clamp-at-zero
    /// update and audit live in [`crate::accounts::adjust_time`]; here we also
    /// refresh a live bound session so heartbeats report the new balance
    /// immediately instead of after a re-login.
    pub async fn adjust_time(
        &self,
        pool: &SqlitePool,
        account_id: &str,
        seconds_delta: i64,
    ) -> AppResult<AccountAdjustSessions> {
        let mut bindings = self.0.lock().await;
        let account = crate::accounts::adjust_time(pool, account_id, seconds_delta).await?;
        let pc_id = refresh_bound_session(&mut bindings, &account);
        Ok(AccountAdjustSessions { account, pc_id })
    }

    /// Owner-initiated points adjustment (web-admin). Mirrors [`Self::adjust_time`].
    pub async fn adjust_points(
        &self,
        pool: &SqlitePool,
        account_id: &str,
        points_delta_centi: i64,
    ) -> AppResult<AccountAdjustSessions> {
        let mut bindings = self.0.lock().await;
        let account = crate::accounts::adjust_points(pool, account_id, points_delta_centi).await?;
        let pc_id = refresh_bound_session(&mut bindings, &account);
        Ok(AccountAdjustSessions { account, pc_id })
    }

    /// Owner-initiated **Rejection Credit** (ADR-0069). Holds the binding lock
    /// while the credit commits so a live bound session sees the credited time
    /// and points at once, exactly as an owner adjustment does. A guest credit
    /// touches the PC row and refreshes no session.
    pub async fn credit_rejection(
        &self,
        pool: &SqlitePool,
        input: &db::RejectionCreditInput,
    ) -> AppResult<(db::RejectionCreditResult, Option<String>)> {
        let mut bindings = self.0.lock().await;
        let credit = db::create_rejection_credit(pool, input).await?;
        let pc_id = match (credit.dry_run, credit.account_id.as_deref()) {
            (false, Some(account_id)) => {
                let account = crate::accounts::get(pool, account_id).await?;
                refresh_bound_session(&mut bindings, &account)
            }
            _ => None,
        };
        Ok((credit, pc_id))
    }

    pub async fn spin_wheel(
        &self,
        pool: &SqlitePool,
        token: &AccountToken,
    ) -> AppResult<(ActiveSession, crate::models::WheelSpinResult)> {
        let mut bindings = self.0.lock().await;
        let session = require_current(&bindings, token)?.clone();
        let mut rng = db::ThreadSpinRng;
        let mutation = db::spin_wheel(pool, &session.account_id, &session.pc_id, &mut rng).await?;
        let current = bindings
            .by_account
            .get_mut(&session.account_id)
            .ok_or_else(|| AppError::Conflict("session is no longer active".to_owned()))?;
        current.remaining_seconds = mutation.account.remaining_seconds;
        current.points_balance_centi = mutation.account.points_balance_centi;
        Ok((current.clone(), mutation.result))
    }

    pub async fn remove_account(&self, account_id: &str) -> Option<String> {
        let mut bindings = self.0.lock().await;
        bindings.remove(account_id).map(|session| session.pc_id)
    }

    pub async fn tick_accounts(&self, pool: &SqlitePool) -> AppResult<Vec<String>> {
        let mut bindings = self.0.lock().await;
        let sessions = bindings.by_account.values().cloned().collect::<Vec<_>>();
        if sessions.is_empty() {
            return Ok(Vec::new());
        }

        let mut transaction = crate::db::begin_immediate(pool).await?;

        // Build the set of offline PCs so we skip their sessions.
        let pc_ids: Vec<&str> = sessions.iter().map(|s| s.pc_id.as_str()).collect();
        let mut online_pc_ids: HashSet<String> = HashSet::new();
        if !pc_ids.is_empty() {
            let mut q = QueryBuilder::<Sqlite>::new("SELECT id FROM pcs WHERE id IN (");
            let mut sep = q.separated(", ");
            for id in &pc_ids {
                sep.push_bind(id);
            }
            sep.push_unseparated(") AND status != 'offline'");
            for row in q
                .build_query_scalar::<String>()
                .fetch_all(&mut *transaction)
                .await?
            {
                online_pc_ids.insert(row);
            }
        }

        let mut expired_accounts = Vec::new();
        let mut changed_pcs = Vec::new();
        let mut remaining_by_account = HashMap::new();
        for session in &sessions {
            if !online_pc_ids.contains(session.pc_id.as_str()) {
                continue;
            }
            let remaining = sqlx::query_scalar::<_, i64>(
                "UPDATE accounts
                 SET remaining_seconds = MAX(remaining_seconds - 1, 0),
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ? AND status = 'active' AND remaining_seconds > 0
                 RETURNING remaining_seconds",
            )
            .bind(&session.account_id)
            .fetch_optional(&mut *transaction)
            .await?;
            let Some(remaining) = remaining else {
                let account_state = sqlx::query_as::<_, (i64, String)>(
                    "SELECT remaining_seconds, status FROM accounts WHERE id = ?",
                )
                .bind(&session.account_id)
                .fetch_optional(&mut *transaction)
                .await?;
                let Some((_remaining, status)) = account_state else {
                    // Account was deleted — end the session.
                    append_end_event(&mut transaction, session, SessionEndReason::Expired, None)
                        .await?;
                    expired_accounts.push(session.account_id.clone());
                    changed_pcs.push(session.pc_id.clone());
                    continue;
                };
                // Keep the binding as long as the account is active, even
                // when remaining_seconds is 0 — the customer may need the
                // session to redeem a voucher, claim Guest Time, or receive
                // a time transfer. Only unbind on deactivation or deletion.
                if status != "active" {
                    append_end_event(&mut transaction, session, SessionEndReason::Expired, None)
                        .await?;
                    expired_accounts.push(session.account_id.clone());
                    changed_pcs.push(session.pc_id.clone());
                    continue;
                }
                // Active account with 0 remaining_seconds — the timer didn't
                // change this tick, but keep the binding so the customer can
                // redeem a voucher, claim Guest Time, or receive a time
                // transfer without re-logging in.
                continue;
            };
            remaining_by_account.insert(session.account_id.clone(), remaining);
            changed_pcs.push(session.pc_id.clone());
            if remaining == 0 {
                // The account drained to zero mid-session. Broadcast the
                // zero balance but keep the binding so the customer can
                // redeem a voucher, claim Guest Time, or receive a time
                // transfer without re-logging in.
            }
        }
        transaction.commit().await?;

        for (account_id, remaining) in remaining_by_account {
            if let Some(session) = bindings.by_account.get_mut(&account_id) {
                session.remaining_seconds = remaining;
            }
        }
        for account_id in expired_accounts {
            bindings.remove(&account_id);
        }
        changed_pcs.sort();
        changed_pcs.dedup();
        Ok(changed_pcs)
    }

    /// Remove the active session binding for a PC that has gone offline
    /// (explicit disconnect or heartbeat timeout). Returns the account_id
    /// that was bound, or `None` if the PC had no active session.
    pub async fn disconnect_pc(&self, pc_id: &str) -> Option<String> {
        let mut bindings = self.0.lock().await;
        bindings.remove_pc(pc_id).map(|session| session.account_id)
    }

    pub async fn active_pc_ids(&self) -> Vec<String> {
        self.0.lock().await.account_by_pc.keys().cloned().collect()
    }

    /// Return the Account Session currently bound to a PC, if any. The
    /// selection layer uses this for truthful target attribution; the Account
    /// remains the subject, never the actor, of system coin events.
    pub async fn active_session_for_pc(&self, pc_id: &str) -> Option<ActiveSession> {
        self.0.lock().await.for_pc(pc_id).cloned()
    }

    /// Handle a Tamper Kill report together with the identity heartbeat that
    /// delivered it. Identity resolution, the PC heartbeat, reachability
    /// event, Tamper Kill audit, and Tamper Kick all commit as one transaction.
    pub async fn heartbeat_by_identity_with_report(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        input: IdentityHeartbeatInput<'_>,
    ) -> AppResult<Option<IdentityHeartbeatResult>> {
        let mut bindings = self.0.lock().await;
        let mut transaction = crate::db::begin_immediate(pool).await?;
        let clock_difference_ms = input.client_time_unix_ms.map(|client_time| {
            input
                .received_at
                .timestamp_millis()
                .saturating_sub(client_time)
        });
        let guard_write = match input.guard_coverage {
            Some(report) => db::pcs::GuardCoverageWrite::Report(report.as_str()),
            // A real Heartbeat without the field comes from an older Client:
            // the previous report no longer describes this process.
            None => db::pcs::GuardCoverageWrite::ResetUnknown,
        };
        let Some(heartbeat) = db::pcs::heartbeat_by_identity_in_transaction(
            &mut transaction,
            input.mac,
            input.source_ip,
            input.hostname,
            input.client_version,
            clock_difference_ms,
            guard_write,
        )
        .await?
        else {
            return Ok(None);
        };

        let tamper_kill = if let Some(report) = input.report {
            Some(
                Self::record_tamper_kill_in_transaction(
                    &mut transaction,
                    &bindings,
                    &heartbeat.pc_id,
                    report,
                )
                .await?,
            )
        } else {
            None
        };
        transaction.commit().await?;

        let tamper_kill_subject = tamper_kill.as_ref().map(|commit| commit.subject);
        if let Some(commit) = tamper_kill {
            Self::release_bound_session(&mut bindings, tokens, commit.kicked_session).await;
        }
        let pc = db::pc(pool, &heartbeat.pc_id).await?;
        Ok(Some(IdentityHeartbeatResult {
            pc,
            tamper_kill_subject,
        }))
    }

    /// Record an Escalated Power Off for one PC (ADR-0077).
    ///
    /// The Alert, the truthful mismatch closure, the Telegram item, the
    /// cooldown stamp, and the session end all commit together: a PC is never
    /// recorded as powered off while its customer's session keeps burning
    /// time, and never the reverse. Balance, Remaining Time, Guest Time, and
    /// Points are untouched — this ends a session, it confiscates nothing.
    pub async fn escalated_power_off(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        decision: EscalatedPowerOff<'_>,
    ) -> AppResult<()> {
        let EscalatedPowerOff {
            pc_id,
            pc_name,
            relaunch_attempts,
            nonce,
            now,
        } = decision;
        let mut bindings = self.0.lock().await;
        let mut transaction = crate::db::begin_immediate(pool).await?;
        db::monitoring::record_escalated_power_off_in_transaction(
            &mut transaction,
            pc_id,
            pc_name,
            relaunch_attempts,
            nonce,
            now,
        )
        .await?;
        let session = bindings.for_pc(pc_id).cloned();
        if let Some(session) = &session {
            append_end_event(
                &mut transaction,
                session,
                SessionEndReason::EscalatedPowerOff,
                None,
            )
            .await?;
        }
        transaction.commit().await?;
        Self::release_bound_session(&mut bindings, tokens, session).await;
        Ok(())
    }

    /// Handle a tamper-kill report from a PC and report whether it was
    /// recorded. Kept as the compatibility seam for existing callers.
    pub async fn handle_tamper_kill(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        pc_id: &str,
        kill_timestamp: &str,
        attempt_count: i64,
    ) -> AppResult<bool> {
        self.handle_tamper_kill_with_subject(pool, tokens, pc_id, kill_timestamp, attempt_count)
            .await
            .map(|_| true)
    }

    /// Handle a legacy tamper-kill report using the current time as the server
    /// receipt time. Without Kill Report Age, attribution is intentionally
    /// unknown.
    pub async fn handle_tamper_kill_with_subject(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        pc_id: &str,
        kill_timestamp: &str,
        attempt_count: i64,
    ) -> AppResult<TamperKillSubject> {
        self.handle_tamper_kill_with_report(
            pool,
            tokens,
            pc_id,
            TamperKillReport {
                kill_timestamp: kill_timestamp.to_owned(),
                report_age: TamperKillReportAge::Missing,
                attempt_count,
                received_at: chrono::Utc::now(),
            },
        )
        .await
    }

    /// Handle a Tamper Kill report at the public heartbeat seam. Attribution
    /// uses durable Account Session history at the server-derived kill instant,
    /// while only the still-live matching session receives a Tamper Kick.
    pub async fn handle_tamper_kill_with_report(
        &self,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        pc_id: &str,
        report: TamperKillReport,
    ) -> AppResult<TamperKillSubject> {
        let mut bindings = self.0.lock().await;
        let mut transaction = crate::db::begin_immediate(pool).await?;
        let commit =
            Self::record_tamper_kill_in_transaction(&mut transaction, &bindings, pc_id, report)
                .await?;
        transaction.commit().await?;
        let subject = commit.subject;
        Self::release_bound_session(&mut bindings, tokens, commit.kicked_session).await;
        Ok(subject)
    }

    /// Append the Tamper Kill and any Tamper Kick to an already-open
    /// authority transaction. The in-memory binding is changed only after the
    /// caller commits, so a failed audit write leaves the session intact.
    async fn record_tamper_kill_in_transaction(
        transaction: &mut Transaction<'_, Sqlite>,
        bindings: &Bindings,
        pc_id: &str,
        report: TamperKillReport,
    ) -> AppResult<TamperKillCommit> {
        let TamperKillReport {
            kill_timestamp,
            report_age,
            attempt_count,
            received_at,
        } = report;

        let already_recorded =
            db::tamper::report_exists(transaction, pc_id, &kill_timestamp).await?;
        if already_recorded {
            let subject = TamperKillSubject::from_str(
                db::tamper::report_subject(transaction, pc_id, &kill_timestamp)
                    .await?
                    .as_deref(),
            );
            return Ok(TamperKillCommit {
                subject,
                kicked_session: None,
            });
        }

        // The device timestamp is deliberately not parsed or compared with
        // the Admin clock. It is only the stable name used for duplicate
        // detection. The Admin anchors the kill instant to its own receipt
        // clock using the Client-measured elapsed age.
        let report_age_ms = report_age.milliseconds();
        let trusted_kill = report_age_ms
            .filter(|age| (0..=MAX_TAMPER_KILL_REPORT_AGE_MS).contains(age))
            .and_then(|age| received_at.checked_sub_signed(chrono::Duration::milliseconds(age)));
        let session = bindings.for_pc(pc_id).cloned();
        let historical_session = match trusted_kill {
            Some(kill_at) => {
                db::tamper::account_session_at_kill(transaction, pc_id, kill_at).await?
            }
            None => None,
        };
        let pc_name = db::tamper::pc_name(transaction, pc_id)
            .await?
            .unwrap_or_else(|| pc_id.to_owned());

        let (subject, account_id, attributed_session_id) =
            if let Some(historical_session) = historical_session {
                (
                    TamperKillSubject::Account,
                    Some(historical_session.account_id),
                    Some(historical_session.session_id),
                )
            } else if trusted_kill.is_none() || session.is_some() {
                (TamperKillSubject::Unknown, None, None)
            } else if db::tamper::guest_remaining_seconds(transaction, pc_id).await? > 0 {
                (TamperKillSubject::Guest, None, None)
            } else {
                (TamperKillSubject::None, None, None)
            };

        let unknown_reason = if subject == TamperKillSubject::Unknown {
            match report_age {
                TamperKillReportAge::Invalid => Some(TamperKillUnknownReason::InvalidReportAge),
                TamperKillReportAge::Missing => Some(TamperKillUnknownReason::MissingReportAge),
                TamperKillReportAge::Valid(_) if trusted_kill.is_none() => {
                    Some(TamperKillUnknownReason::InvalidReportAge)
                }
                TamperKillReportAge::Valid(_) if session.is_some() => {
                    Some(TamperKillUnknownReason::CurrentOccupantNotAttributed)
                }
                TamperKillReportAge::Valid(_) => None,
            }
        } else {
            None
        };

        let mut metadata = json!({
            "subject": subject.as_str(),
            "kill_timestamp": kill_timestamp,
            "report_age_ms": report_age_ms,
            "attempt_count": attempt_count,
        });
        if let Some(reason) = unknown_reason {
            if let Some(object) = metadata.as_object_mut() {
                object.insert("reason".to_owned(), json!(reason.as_str()));
            }
        }

        let summary = match subject {
            TamperKillSubject::Account => {
                format!("Tamper Kill on {pc_name} during an Account session")
            }
            TamperKillSubject::Guest => {
                format!("Tamper Kill on {pc_name} during a Guest session")
            }
            TamperKillSubject::None => {
                format!("Tamper Kill on {pc_name} with no active session")
            }
            TamperKillSubject::Unknown => match unknown_reason {
                Some(reason) => format!("Tamper Kill on {pc_name}: {}", reason.summary()),
                None => format!("Tamper Kill on {pc_name} with undetermined session attribution"),
            },
        };
        let mut event = NewAuditEvent::simple(
            "tamper_kill",
            AuditOutcome::Success,
            AuditActor::System,
            &summary,
        );
        event.pc_id = Some(pc_id);
        if let Some(ref aid) = account_id {
            event.account_id = Some(aid);
        }
        event.metadata = metadata;
        audit::append(transaction, event).await?;

        let kicked_session = if subject == TamperKillSubject::Account {
            if let (Some(s), Some(session_id)) = (&session, attributed_session_id.as_deref()) {
                if s.id == session_id {
                    append_end_event(transaction, s, SessionEndReason::TamperKick, None).await?;
                    Some(s.clone())
                } else {
                    None
                }
            } else {
                None
            }
        } else if subject == TamperKillSubject::Guest {
            let mut kick = NewAuditEvent::simple(
                SessionEndReason::TamperKick.event_type(),
                AuditOutcome::Success,
                AuditActor::System,
                "Guest session ended due to tamper kill",
            );
            kick.pc_id = Some(pc_id);
            kick.metadata = json!({ "subject": "guest" });
            audit::append(transaction, kick).await?;
            None
        } else {
            None
        };

        Ok(TamperKillCommit {
            subject,
            kicked_session,
        })
    }

    /// Drop the in-memory binding and revoke the account token for a session
    /// the caller has already ended durably. Shared by every forced end —
    /// Tamper Kick and Escalated Power Off — because the in-memory effect is
    /// identical; only the recorded cause differs.
    async fn release_bound_session(
        bindings: &mut Bindings,
        tokens: &AccountTokens,
        kicked_session: Option<ActiveSession>,
    ) {
        if let Some(session) = kicked_session {
            bindings.remove(&session.account_id);
            tokens.revoke_account(&session.account_id).await;
        }
    }

    pub async fn enrich_pc(&self, mut pc: Pc) -> Pc {
        let bindings = self.0.lock().await;
        let session = bindings.for_pc(&pc.id).cloned();
        if let Some(session) = session {
            pc.remaining_seconds = session.remaining_seconds.max(0);
            pc.timer_mode = TimerMode::Account;
            pc.active_session = Some(session);
            if pc.status != "offline" {
                pc.status = "in_use".to_owned();
            }
        }
        pc
    }

    pub async fn enrich_pcs(&self, pcs: Vec<Pc>) -> Vec<Pc> {
        let bindings = self.0.lock().await;
        pcs.into_iter()
            .map(|mut pc| {
                let session = bindings.for_pc(&pc.id).cloned();
                if let Some(session) = session {
                    pc.remaining_seconds = session.remaining_seconds.max(0);
                    pc.timer_mode = TimerMode::Account;
                    pc.active_session = Some(session);
                    if pc.status != "offline" {
                        pc.status = "in_use".to_owned();
                    }
                }
                pc
            })
            .collect()
    }
}

/// Copy an account's authoritative balances into its live bound session (if
/// any) and report the PC to re-publish.
fn refresh_bound_session(bindings: &mut Bindings, account: &Account) -> Option<String> {
    let session = bindings.by_account.get_mut(&account.id)?;
    session.remaining_seconds = account.remaining_seconds.max(0);
    session.points_balance_centi = account.points_balance_centi.max(0);
    Some(session.pc_id.clone())
}

fn require_current<'a>(
    bindings: &'a Bindings,
    token: &AccountToken,
) -> AppResult<&'a ActiveSession> {
    let session = bindings
        .by_account
        .get(&token.account_id)
        .ok_or_else(|| AppError::Unauthorized("account session is not active".to_owned()))?;
    if session.pc_id != token.pc_id || session.id != token.session_id {
        return Err(AppError::Unauthorized(
            "account session token is stale".to_owned(),
        ));
    }
    Ok(session)
}

async fn append_end_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: &ActiveSession,
    reason: SessionEndReason,
    displaced_by: Option<&str>,
) -> AppResult<()> {
    db::end_account_session(transaction, &session.id, reason).await?;
    let actor = if reason == SessionEndReason::Logout {
        AuditActor::Account
    } else {
        AuditActor::System
    };
    let mut event = NewAuditEvent::simple(
        reason.event_type(),
        AuditOutcome::Success,
        actor,
        match reason {
            SessionEndReason::Logout => "Customer logged out and preserved account time",
            SessionEndReason::AutoKick => "Account moved to another PC",
            SessionEndReason::TakenOver => "PC was taken over by another account",
            SessionEndReason::Expired => "Account Remaining Time reached zero",
            SessionEndReason::TamperKick => "Account session ended due to tamper kill",
            SessionEndReason::EscalatedPowerOff => {
                "Account session ended because the PC was powered off by Client Recovery Escalation"
            }
        },
    );
    if actor == AuditActor::Account {
        event.actor_account_id = Some(&session.account_id);
    }
    event.account_id = Some(&session.account_id);
    event.pc_id = Some(&session.pc_id);
    event.metadata = json!({
        "session_id": session.id,
        "displaced_by": displaced_by
    });
    audit::append(transaction, event).await?;
    Ok(())
}

/// Close the session history row as a Logout — reports and Account Session
/// history treat an Owner Sign-Out identically to one — while recording the
/// Audit Log entry against the owner. Reusing `append_end_event` here would
/// stamp `AuditActor::Account` and the customer's `actor_account_id` onto an
/// action the customer did not take, so a later dispute would read as "the
/// customer logged themselves out".
async fn append_owner_sign_out_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: &ActiveSession,
) -> AppResult<()> {
    db::end_account_session(transaction, &session.id, SessionEndReason::Logout).await?;
    let mut event = NewAuditEvent::simple(
        "owner_sign_out",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Owner signed the account out of the PC; account time preserved",
    );
    event.account_id = Some(&session.account_id);
    event.pc_id = Some(&session.pc_id);
    event.metadata = json!({
        "session_id": session.id,
        "end_reason": SessionEndReason::Logout.event_type()
    });
    audit::append(transaction, event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::{accounts, db, models::CreateVoucherInput};

    fn coin_input(pc_id: &str, amount_cents: i64) -> CoinEventInput {
        CoinEventInput {
            pc_id: pc_id.to_owned(),
            amount_cents,
            pulse_count: Some(1),
            timestamp: None,
        }
    }

    fn simulator_claim() -> CoinSourceClaim {
        CoinSourceClaim::Simulator
    }

    async fn setup() -> AppResult<(SqlitePool, SessionStore, Account, Account)> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        db::seed(&pool).await?;
        let first = accounts::create(
            &pool,
            "first",
            "password",
            accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let second = accounts::create(
            &pool,
            "second",
            "password",
            accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        Ok((pool, SessionStore::default(), first, second))
    }

    async fn tamper_kill_with_age(
        sessions: &SessionStore,
        pool: &SqlitePool,
        tokens: &AccountTokens,
        pc_id: &str,
        kill_timestamp: &str,
        report_age_ms: Option<i64>,
        attempt_count: i64,
    ) -> AppResult<TamperKillSubject> {
        sessions
            .handle_tamper_kill_with_report(
                pool,
                tokens,
                pc_id,
                TamperKillReport {
                    kill_timestamp: kill_timestamp.to_owned(),
                    report_age: report_age_ms
                        .map(TamperKillReportAge::Valid)
                        .unwrap_or(TamperKillReportAge::Missing),
                    attempt_count,
                    received_at: chrono::Utc::now(),
                },
            )
            .await
    }

    #[tokio::test]
    async fn login_resolves_account_and_pc_contention_atomically() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        let tokens = AccountTokens::default();
        sessions.login(&pool, &tokens, &first, "pc-01").await?;
        sessions.login(&pool, &tokens, &second, "pc-02").await?;
        let (final_session, _) = sessions.login(&pool, &tokens, &first, "pc-02").await?;

        assert_eq!(final_session.account_id, first.id);
        assert_eq!(sessions.active_pc_ids().await, vec!["pc-02".to_owned()]);
        let auto_kicks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'auto_kick'")
                .fetch_one(&pool)
                .await?;
        let takeovers: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'taken_over'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(auto_kicks, 1);
        assert_eq!(takeovers, 1);
        Ok(())
    }

    async fn assert_bound(sessions: &SessionStore, session: &ActiveSession, pc_id: &str) {
        let by_pc = sessions.active_session_for_pc(pc_id).await;
        assert_eq!(by_pc.map(|s| s.id), Some(session.id.clone()));
        let token = AccountToken {
            account_id: session.account_id.clone(),
            pc_id: pc_id.to_owned(),
            session_id: session.id.clone(),
        };
        assert!(sessions.validate_attendance_session(&token).await.is_ok());
    }

    async fn assert_unbound(sessions: &SessionStore, session: &ActiveSession, pc_id: &str) {
        let by_pc = sessions.active_session_for_pc(pc_id).await;
        assert_ne!(by_pc.map(|s| s.id), Some(session.id.clone()));
        let token = AccountToken {
            account_id: session.account_id.clone(),
            pc_id: pc_id.to_owned(),
            session_id: session.id.clone(),
        };
        assert!(sessions.validate_attendance_session(&token).await.is_err());
    }

    #[tokio::test]
    async fn both_lookups_agree_through_login_replacement_and_every_exit() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        let tokens = AccountTokens::default();

        // Same-PC resume keeps the session; a move leaves nothing on the old PC.
        let (a1, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        let (resumed, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        assert_eq!(resumed.id, a1.id);
        assert_bound(&sessions, &a1, "pc-01").await;
        let (a2, _) = sessions.login(&pool, &tokens, &first, "pc-02").await?;
        assert_unbound(&sessions, &a1, "pc-01").await;
        assert_bound(&sessions, &a2, "pc-02").await;

        // A failed login transaction leaves existing bindings untouched.
        let missing = sessions.login(&pool, &tokens, &first, "pc-missing").await;
        assert!(matches!(missing, Err(AppError::NotFound(_))));
        assert_bound(&sessions, &a2, "pc-02").await;

        // Occupied-PC replacement unbinds the displaced account.
        let (b1, _) = sessions.login(&pool, &tokens, &second, "pc-02").await?;
        assert_unbound(&sessions, &a2, "pc-02").await;
        assert_bound(&sessions, &b1, "pc-02").await;

        // Logout.
        let token = AccountToken {
            account_id: second.id.clone(),
            pc_id: "pc-02".to_owned(),
            session_id: b1.id.clone(),
        };
        sessions.logout(&pool, &token).await?;
        assert_unbound(&sessions, &b1, "pc-02").await;

        // Owner Sign-Out.
        let (a3, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        sessions.owner_sign_out(&pool, "pc-01", &a3.id).await?;
        assert_unbound(&sessions, &a3, "pc-01").await;

        // Account removal and PC disconnect.
        let (a4, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        assert_eq!(
            sessions.remove_account(&first.id).await.as_deref(),
            Some("pc-01")
        );
        assert_unbound(&sessions, &a4, "pc-01").await;
        let (a5, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        assert_eq!(
            sessions.disconnect_pc("pc-01").await.as_deref(),
            Some(first.id.as_str())
        );
        assert_unbound(&sessions, &a5, "pc-01").await;

        // Tick expiry of a deleted account.
        let (b2, _) = sessions.login(&pool, &tokens, &second, "pc-02").await?;
        db::heartbeat_by_identity_only(&pool, "pc-02").await?;
        accounts::soft_delete(&pool, &second.id).await?;
        sessions.tick_accounts(&pool).await?;
        assert_unbound(&sessions, &b2, "pc-02").await;

        // Forced release unbinds both ways and, uniquely, revokes the token.
        let (a6, token_a6) = sessions.login(&pool, &tokens, &first, "pc-03").await?;
        sessions
            .escalated_power_off(
                &pool,
                &tokens,
                EscalatedPowerOff {
                    pc_id: "pc-03",
                    pc_name: "PC 03",
                    relaunch_attempts: 3,
                    nonce: "nonce-1",
                    now: chrono::Utc::now(),
                },
            )
            .await?;
        assert_unbound(&sessions, &a6, "pc-03").await;
        assert!(tokens.get(&token_a6).await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn late_login_audit_failure_rolls_back_replacement_and_keeps_bindings() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        let tokens = AccountTokens::default();
        let (a1, token_a) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        let (b1, token_b) = sessions.login(&pool, &tokens, &second, "pc-02").await?;
        sqlx::query(
            "CREATE TRIGGER fail_login_audit
             BEFORE INSERT ON audit_log
             WHEN NEW.event_type = 'login'
             BEGIN SELECT RAISE(ABORT, 'test audit failure'); END",
        )
        .execute(&pool)
        .await?;

        // Login audit is the last write, so earlier replacement writes must roll back.
        let error = sessions
            .login(&pool, &tokens, &first, "pc-02")
            .await
            .expect_err("login audit failure must fail login");
        assert!(matches!(&error, AppError::Database(_)));
        assert!(error.to_string().contains("test audit failure"));

        assert_bound(&sessions, &a1, "pc-01").await;
        assert_bound(&sessions, &b1, "pc-02").await;
        let a_token = tokens
            .get(&token_a)
            .await
            .map(|t| (t.account_id, t.pc_id, t.session_id));
        let b_token = tokens
            .get(&token_b)
            .await
            .map(|t| (t.account_id, t.pc_id, t.session_id));
        assert_eq!(
            a_token,
            Some((first.id.clone(), "pc-01".to_owned(), a1.id.clone()))
        );
        assert_eq!(
            b_token,
            Some((second.id.clone(), "pc-02".to_owned(), b1.id.clone()))
        );

        let history: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT session_id, ended_at FROM account_session_history ORDER BY session_id",
        )
        .fetch_all(&pool)
        .await?;
        let mut expected = vec![(a1.id.clone(), None), (b1.id.clone(), None)];
        expected.sort();
        assert_eq!(history, expected);
        let displacement_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type IN ('auto_kick', 'taken_over')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(displacement_audits, 0);
        Ok(())
    }

    #[tokio::test]
    async fn login_rechecks_account_status_inside_the_binding_transaction() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let tokens = AccountTokens::default();
        accounts::soft_delete(&pool, &first.id).await?;

        let result = sessions.login(&pool, &tokens, &first, "pc-01").await;
        assert!(matches!(result, Err(AppError::Unauthorized(_))));
        assert!(sessions.active_pc_ids().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn transfer_time_refreshes_recipient_live_session_and_reports_pc() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        let tokens = AccountTokens::default();
        // Fund both so each login keeps an active binding.
        sqlx::query("UPDATE accounts SET remaining_seconds = 600 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 60 WHERE id = ?")
            .bind(&second.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        let mut second = second;
        second.remaining_seconds = 60;
        let (sender_session, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        sessions.login(&pool, &tokens, &second, "pc-02").await?;
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: sender_session.id,
        };

        let result = sessions.transfer_time(&pool, &token, "second", 300).await?;
        // The sender's cached session reflects the debit immediately.
        assert_eq!(result.sender.remaining_seconds, 300);
        // The recipient is signed in on pc-02, so the caller is told to
        // re-publish that PC's timer.
        assert_eq!(result.recipient_pc_id.as_deref(), Some("pc-02"));
        // The recipient's enriched PC shows the credited balance at once
        // (360s), not the stale pre-transfer 60s.
        let recipient_pc = sessions.enrich_pc(db::pc(&pool, "pc-02").await?).await;
        assert_eq!(recipient_pc.remaining_seconds, 360);
        Ok(())
    }

    #[tokio::test]
    async fn transfer_time_to_offline_recipient_reports_no_pc() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 600 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 60 WHERE id = ?")
            .bind(&second.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        let (sender_session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: sender_session.id,
        };
        // The recipient ("second") is not signed in anywhere, so there is no
        // live PC to refresh — the DB still moves the points authoritatively.
        let result = sessions.transfer_time(&pool, &token, "second", 300).await?;
        assert_eq!(result.recipient_pc_id, None);
        let recipient_balance: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&second.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(recipient_balance, 360);
        Ok(())
    }

    #[tokio::test]
    async fn confirm_points_transfer_refreshes_live_sessions() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        let tokens = AccountTokens::default();
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 600, points_balance_centi = 500 WHERE id = ?",
        )
        .bind(&first.id)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 60 WHERE id = ?")
            .bind(&second.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        first.points_balance_centi = 500;
        let mut second = second;
        second.remaining_seconds = 60;
        sessions.login(&pool, &tokens, &first, "pc-01").await?;
        sessions.login(&pool, &tokens, &second, "pc-02").await?;

        let created = accounts::request_points_transfer(&pool, &first.id, "second", 200).await?;
        let outcome = sessions
            .confirm_points_transfer(&pool, &created.code)
            .await?;

        // Both parties are signed in, so both PCs must be reported for
        // re-publish and both cached sessions must show the moved points
        // without a re-login.
        let mut changed = outcome.changed_pc_ids.clone();
        changed.sort();
        assert_eq!(changed, vec!["pc-01".to_owned(), "pc-02".to_owned()]);
        let sender_pc = sessions.enrich_pc(db::pc(&pool, "pc-01").await?).await;
        assert_eq!(
            sender_pc
                .active_session
                .as_ref()
                .map(|session| session.points_balance_centi),
            Some(300)
        );
        let recipient_pc = sessions.enrich_pc(db::pc(&pool, "pc-02").await?).await;
        assert_eq!(
            recipient_pc
                .active_session
                .as_ref()
                .map(|session| session.points_balance_centi),
            Some(200)
        );
        Ok(())
    }

    #[tokio::test]
    async fn owner_adjustments_refresh_live_session() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let tokens = AccountTokens::default();
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 600, points_balance_centi = 500 WHERE id = ?",
        )
        .bind(&first.id)
        .execute(&pool)
        .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        first.points_balance_centi = 500;
        sessions.login(&pool, &tokens, &first, "pc-01").await?;

        let time = sessions.adjust_time(&pool, &first.id, 300).await?;
        assert_eq!(time.pc_id.as_deref(), Some("pc-01"));
        let points = sessions.adjust_points(&pool, &first.id, -200).await?;
        assert_eq!(points.pc_id.as_deref(), Some("pc-01"));

        let pc = sessions.enrich_pc(db::pc(&pool, "pc-01").await?).await;
        let session = pc.active_session.as_ref();
        assert_eq!(session.map(|s| s.remaining_seconds), Some(900));
        assert_eq!(session.map(|s| s.points_balance_centi), Some(300));
        Ok(())
    }

    #[tokio::test]
    async fn owner_adjustment_to_offline_account_reports_no_pc() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 100 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let outcome = sessions.adjust_points(&pool, &first.id, 50).await?;
        assert_eq!(outcome.pc_id, None);
        assert_eq!(outcome.account.points_balance_centi, 150);
        Ok(())
    }

    #[tokio::test]
    async fn claim_moves_guest_time_exactly_once() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE pcs SET remaining_seconds = 300 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: session.id,
        };
        let claimed = sessions.claim(&pool, &token).await?;
        assert_eq!(claimed.remaining_seconds, 300);
        assert!(matches!(
            sessions.claim(&pool, &token).await,
            Err(AppError::Conflict(_))
        ));
        let guest: i64 = sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
            .fetch_one(&pool)
            .await?;
        let account: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&first.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!((guest, account), (0, 300));
        Ok(())
    }

    #[tokio::test]
    async fn zero_balance_login_keeps_binding_while_guest_time_remains() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 0 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE pcs SET remaining_seconds = 300 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 0;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        // A tick must not kick the zero-balance session while Guest Time remains.
        let changed = sessions.tick_accounts(&pool).await?;
        assert!(changed.is_empty());
        assert_eq!(sessions.active_pc_ids().await, vec!["pc-01".to_owned()]);
        let expired: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'expired'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(expired, 0);

        // The customer can still claim after the tick that would have kicked them.
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: session.id,
        };
        let claimed = sessions.claim(&pool, &token).await?;
        assert_eq!(claimed.remaining_seconds, 300);
        Ok(())
    }

    #[tokio::test]
    async fn draining_to_zero_keeps_binding_when_guest_time_remains() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 1 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE pcs SET remaining_seconds = 120 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 1;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        // The account drains 1 -> 0, but the PC still has claimable Guest Time.
        sessions.tick_accounts(&pool).await?;

        assert_eq!(sessions.active_pc_ids().await, vec!["pc-01".to_owned()]);
        let expired: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'expired'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(expired, 0);
        Ok(())
    }

    #[tokio::test]
    async fn account_session_survives_disconnect_and_logout_preserves_balance() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 60 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 60;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: session.id,
        };

        sessions.tick_accounts(&pool).await?;
        db::disconnect_pc(&pool, "pc-01").await?;
        sessions.tick_accounts(&pool).await?;
        sessions.logout(&pool, &token).await?;
        sessions.tick_accounts(&pool).await?;

        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&first.id)
                .fetch_one(&pool)
                .await?;
        // One tick while online consumed 1 second; disconnect paused it; logout
        // preserved the remaining balance.
        assert_eq!(remaining, 59);
        Ok(())
    }

    #[tokio::test]
    async fn a_new_store_restores_no_stale_binding_but_keeps_balance() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 60 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let restarted = SessionStore::default();
        assert!(restarted.active_pc_ids().await.is_empty());
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(first.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 60);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_logins_leave_only_one_binding_for_the_account() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let tokens = AccountTokens::default();
        let first_login = sessions.login(&pool, &tokens, &first, "pc-01");
        let second_login = sessions.login(&pool, &tokens, &first, "pc-02");
        let (first_result, second_result) = tokio::join!(first_login, second_login);
        first_result?;
        second_result?;

        let active = sessions.active_pc_ids().await;
        assert_eq!(active.len(), 1);
        assert!(active[0] == "pc-01" || active[0] == "pc-02");
        Ok(())
    }

    #[tokio::test]
    async fn relogin_on_the_same_pc_resumes_the_existing_session() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let tokens = AccountTokens::default();
        let (original, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;
        let (resumed, _) = sessions.login(&pool, &tokens, &first, "pc-01").await?;

        assert_eq!(resumed.id, original.id);
        let displacement_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type IN ('auto_kick', 'taken_over')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(displacement_events, 0);
        Ok(())
    }

    #[tokio::test]
    async fn owner_sign_out_ends_binding_and_preserves_account_balance() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 60 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 60;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let outcome = sessions.owner_sign_out(&pool, "pc-01", &session.id).await?;
        assert_eq!(
            outcome,
            SignOutOutcome::SignedOut {
                username: "first".to_owned()
            }
        );

        // The binding is gone: a tick can no longer drain the account.
        sessions.tick_accounts(&pool).await?;
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&first.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 60);

        // Session history records it as a Logout (the lifecycle is identical).
        let ended: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT ended_at, end_reason
             FROM account_session_history
             WHERE session_id = ?",
        )
        .bind(&session.id)
        .fetch_one(&pool)
        .await?;
        assert!(ended.0.is_some());
        assert_eq!(ended.1.as_deref(), Some("logout"));

        // The Audit Log records it as an owner action, never as the customer's
        // own logout — a later dispute must not read as "they left themselves".
        let audited: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT actor, actor_account_id, account_id, pc_id
             FROM audit_log
             WHERE event_type = 'owner_sign_out'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audited.0, "admin");
        assert_eq!(audited.1, None);
        assert_eq!(audited.2.as_deref(), Some(first.id.as_str()));
        assert_eq!(audited.3.as_deref(), Some("pc-01"));

        let customer_logouts: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'logout'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(customer_logouts, 0);
        Ok(())
    }

    #[tokio::test]
    async fn owner_sign_out_with_stale_session_id_is_a_no_op() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        // The owner clicked from a dashboard snapshot that has since moved on.
        let outcome = sessions
            .owner_sign_out(&pool, "pc-01", "a-session-that-already-ended")
            .await?;
        assert_eq!(outcome, SignOutOutcome::AlreadyEnded);

        // The live session was left strictly alone.
        let ended_at: Option<String> =
            sqlx::query_scalar("SELECT ended_at FROM account_session_history WHERE session_id = ?")
                .bind(&session.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(ended_at, None);
        let sign_out_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_sign_out'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(sign_out_events, 0);
        Ok(())
    }

    #[tokio::test]
    async fn owner_sign_out_on_a_pc_with_no_session_is_a_no_op() -> AppResult<()> {
        let (pool, sessions, _, _) = setup().await?;
        let outcome = sessions
            .owner_sign_out(&pool, "pc-01", "any-session")
            .await?;
        assert_eq!(outcome, SignOutOutcome::AlreadyEnded);
        Ok(())
    }

    #[tokio::test]
    async fn account_session_history_tracks_start_and_logout() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let started: (String, String, String, String, Option<String>) = sqlx::query_as(
            "SELECT session_id, account_id, pc_id, started_at, ended_at
             FROM account_session_history",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(started.0, session.id);
        assert_eq!(started.1, first.id);
        assert_eq!(started.2, "pc-01");
        assert_eq!(started.3, session.started_at);
        assert_eq!(started.4, None);

        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: session.id,
        };
        sessions.logout(&pool, &token).await?;

        let ended: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT ended_at, end_reason
             FROM account_session_history
             WHERE session_id = ?",
        )
        .bind(&token.session_id)
        .fetch_one(&pool)
        .await?;
        assert!(ended.0.is_some());
        assert_eq!(ended.1.as_deref(), Some("logout"));
        Ok(())
    }

    #[tokio::test]
    async fn restart_recovery_closes_history_at_last_heartbeat_once() -> AppResult<()> {
        let (pool, sessions, first, second) = setup().await?;
        let (first_session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        let (second_session, _) = sessions
            .login(&pool, &AccountTokens::default(), &second, "pc-02")
            .await?;
        sqlx::query(
            "UPDATE pcs
             SET last_seen_at = CASE id
                 WHEN 'pc-01' THEN '2026-08-07 12:34:56'
                 ELSE NULL
             END
             WHERE id IN ('pc-01', 'pc-02')",
        )
        .execute(&pool)
        .await?;

        SessionStore::recover_open_sessions(&pool).await?;

        let first_ended: (String, String) = sqlx::query_as(
            "SELECT ended_at, end_reason
             FROM account_session_history WHERE session_id = ?",
        )
        .bind(&first_session.id)
        .fetch_one(&pool)
        .await?;
        let second_ended: (String, String) = sqlx::query_as(
            "SELECT ended_at, end_reason
             FROM account_session_history WHERE session_id = ?",
        )
        .bind(&second_session.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            first_ended,
            (
                "2026-08-07 12:34:56".to_owned(),
                "server_restart".to_owned()
            )
        );
        assert_eq!(second_ended.1, "server_restart");

        let first_end_again: String =
            sqlx::query_scalar("SELECT ended_at FROM account_session_history WHERE session_id = ?")
                .bind(&first_session.id)
                .fetch_one(&pool)
                .await?;
        SessionStore::recover_open_sessions(&pool).await?;
        let first_end_after_repeat: String =
            sqlx::query_scalar("SELECT ended_at FROM account_session_history WHERE session_id = ?")
                .bind(&first_session.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(first_end_after_repeat, first_end_again);

        let session_end_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type IN ('logout', 'auto_kick', 'taken_over', 'expired', 'tamper_kick')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(session_end_audits, 0);
        assert_eq!(sessions.active_pc_ids().await.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn guest_and_account_timers_drain_only_their_authoritative_balances() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query(
            "UPDATE pcs
             SET remaining_seconds = 10, status = 'in_use', last_seen_at = CURRENT_TIMESTAMP
             WHERE id IN ('pc-01', 'pc-02')",
        )
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 10 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 10;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-02")
            .await?;

        sessions.tick_accounts(&pool).await?;
        db::tick_guest_pcs(&pool, &sessions.active_pc_ids().await).await?;

        let guest_pc_one: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let parked_guest_pc_two: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-02'")
                .fetch_one(&pool)
                .await?;
        let account_remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(first.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            (guest_pc_one, parked_guest_pc_two, account_remaining),
            (9, 10, 9)
        );
        Ok(())
    }

    #[tokio::test]
    async fn draining_to_zero_keeps_binding_for_voucher_or_guest_claim() -> AppResult<()> {
        // An account whose remaining_seconds ticks from 1 to 0 must keep its
        // session binding so the customer can redeem a voucher, claim Guest
        // Time, or receive a time transfer without re-logging in.
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 1 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 1;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        db::heartbeat_by_identity_only(&pool, "pc-01").await?;

        sessions.tick_accounts(&pool).await?;

        // Session must survive the tick even though remaining_seconds hit 0.
        assert!(
            !sessions.active_pc_ids().await.is_empty(),
            "session was unbound after draining to 0"
        );
        // No expired event should be logged for an active account.
        let expired: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'expired'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(expired, 0);
        Ok(())
    }

    #[tokio::test]
    async fn logout_after_draining_to_zero_returns_ok() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 1 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 1;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: session.id,
        };

        // Drain from 1 -> 0 — binding survives (draining_to_zero_keeps_binding
        // covers longevity). Logout of a zero-balance session must succeed.
        sessions.tick_accounts(&pool).await?;
        assert!(
            !sessions.active_pc_ids().await.is_empty(),
            "session was unbound after draining to 0"
        );

        let result = sessions.logout(&pool, &token).await;
        assert!(
            result.is_ok(),
            "logout after draining to zero should succeed, but got: {result:?}"
        );

        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&first.id)
                .fetch_one(&pool)
                .await?;
        // Balance must be preserved (already 0, no time consumed by logout).
        assert_eq!(remaining, 0);
        Ok(())
    }

    #[tokio::test]
    async fn zero_time_session_can_redeem_voucher_after_tick() -> AppResult<()> {
        // Bug: an account with 0 remaining_seconds and no guest time was
        // expired by tick_accounts, making redeem_voucher fail with
        // "account session is not active". The session must survive so the
        // customer can add time via voucher, transfer, or claim.
        let (pool, sessions, first, _) = setup().await?;

        // Create a promo voucher worth 600 seconds.
        let voucher = db::create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 600,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;

        let tokens = AccountTokens::default();
        // Login with 0 time and no guest time on the PC.
        let (session, _raw_token) = sessions.login(&pool, &tokens, &first, "pc-01").await?;

        // Tick — this would previously expire the session because
        // remaining_seconds = 0 and there is no claimable guest time.
        db::heartbeat_by_identity_only(&pool, "pc-01").await?;
        sessions.tick_accounts(&pool).await?;

        // Session must still be active.
        assert!(
            !sessions.active_pc_ids().await.is_empty(),
            "session was expired by tick even though account is active with 0 time"
        );

        // Redeem the voucher — must succeed.
        let token = AccountToken {
            account_id: first.id.clone(),
            pc_id: "pc-01".to_owned(),
            session_id: session.id,
        };
        let result = sessions.redeem_voucher(&pool, &token, &voucher.code).await;
        assert!(
            result.is_ok(),
            "expected voucher redemption to succeed, got: {result:?}"
        );
        assert_eq!(result.unwrap().remaining_seconds, 600);
        Ok(())
    }

    #[tokio::test]
    async fn coin_event_routes_to_logged_in_account_with_points() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let transaction = sessions
            .record_coin_event(&pool, &coin_input("pc-01", 5_000), &simulator_claim())
            .await?;

        assert_eq!(transaction.target_kind, "account");
        assert_eq!(transaction.account_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(transaction.points_earned_centi, 100);
        let account: (i64, i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi, point_earn_carry_centicents
             FROM accounts WHERE id = ?",
        )
        .bind(&first.id)
        .fetch_one(&pool)
        .await?;
        let guest: i64 = sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
            .fetch_one(&pool)
            .await?;
        let audit: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT account_id, points_delta_centi FROM audit_log
             WHERE event_type = 'coin_accepted'
             ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account, (15_000, 100, 0));
        assert_eq!(guest, 0);
        assert_eq!(audit, (Some(first.id), Some(100)));
        Ok(())
    }

    #[tokio::test]
    async fn guest_coin_event_credits_pc_without_points_or_carry() -> AppResult<()> {
        let (pool, sessions, _, _) = setup().await?;

        let transaction = sessions
            .record_coin_event(&pool, &coin_input("pc-01", 5_000), &simulator_claim())
            .await?;

        assert_eq!(transaction.target_kind, "guest");
        assert_eq!(transaction.account_id, None);
        assert_eq!(transaction.points_earned_centi, 0);
        let guest: i64 = sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
            .fetch_one(&pool)
            .await?;
        let carry_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM accounts WHERE point_earn_carry_centicents > 0",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(guest, 15_000);
        assert_eq!(carry_rows, 0);
        Ok(())
    }

    #[tokio::test]
    async fn logged_in_sub_threshold_coin_events_earn_fractional_points() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let first_coin = sessions
            .record_coin_event(&pool, &coin_input("pc-01", 2_000), &simulator_claim())
            .await?;
        let second_coin = sessions
            .record_coin_event(&pool, &coin_input("pc-01", 3_000), &simulator_claim())
            .await?;

        let account: (i64, i64) = sqlx::query_as(
            "SELECT points_balance_centi, point_earn_carry_centicents FROM accounts WHERE id = ?",
        )
        .bind(&first.id)
        .fetch_one(&pool)
        .await?;
        // Fractional earning (ADR-0038): each sub-threshold coin pays out
        // immediately (₱20 = 0.40 pt, ₱30 = 0.60 pt); nothing carries.
        assert_eq!(first_coin.points_earned_centi, 40);
        assert_eq!(second_coin.points_earned_centi, 60);
        assert_eq!(account, (100, 0));
        Ok(())
    }

    #[tokio::test]
    async fn failed_coin_event_rolls_back_account_time_and_points() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        sqlx::query(
            "INSERT INTO coin_devices (id, name, secret, active)
             VALUES ('device-01', 'Test Coin Device', 'test-secret', 1)",
        )
        .execute(&pool)
        .await?;

        let failed_input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 5_000,
            pulse_count: Some(1),
            timestamp: None,
        };
        let failed_claim = CoinSourceClaim::RemoteDevice {
            device_id: "device-01".to_owned(),
            secret: "wrong-secret".to_owned(),
        };
        assert!(matches!(
            sessions
                .record_coin_event(&pool, &failed_input, &failed_claim)
                .await,
            Err(AppError::Unauthorized(_))
        ));

        let account: (i64, i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi, point_earn_carry_centicents
             FROM accounts WHERE id = ?",
        )
        .bind(&first.id)
        .fetch_one(&pool)
        .await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&pool)
            .await?;
        assert_eq!(account, (0, 0, 0));
        assert_eq!(count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_account_coin_events_preserve_points_carry() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let first_coin = coin_input("pc-01", 2_500);
        let second_coin = coin_input("pc-01", 2_500);
        let claim = simulator_claim();
        let (first_result, second_result) = tokio::join!(
            sessions.record_coin_event(&pool, &first_coin, &claim),
            sessions.record_coin_event(&pool, &second_coin, &claim)
        );
        first_result?;
        second_result?;

        let account: (i64, i64) = sqlx::query_as(
            "SELECT points_balance_centi, point_earn_carry_centicents FROM accounts WHERE id = ?",
        )
        .bind(&first.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(account, (100, 0));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Tamper-kill tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn tamper_kill_account_session_writes_audit_and_kick_and_preserves_balance(
    ) -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 600, points_balance_centi = 50 WHERE id = ?",
        )
        .bind(&first.id)
        .execute(&pool)
        .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        first.points_balance_centi = 50;
        let tokens = AccountTokens::default();
        let token = sessions.login(&pool, &tokens, &first, "pc-01").await?.1;
        let kill_ts = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE pcs SET last_seen_at = ? WHERE id = 'pc-01'")
            .bind(&kill_ts)
            .execute(&pool)
            .await?;

        let subject =
            tamper_kill_with_age(&sessions, &pool, &tokens, "pc-01", &kill_ts, Some(0), 3).await?;
        assert_eq!(subject, TamperKillSubject::Account);

        // Exactly one tamper_kill and one tamper_kick audit row.
        let tamper_kill_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        let tamper_kick_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kick'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(tamper_kill_count, 1);
        assert_eq!(tamper_kick_count, 1);

        // Balance and points are untouched.
        let (remaining_seconds, points_balance_centi): (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&first.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(remaining_seconds, 600);
        assert_eq!(points_balance_centi, 50);

        // tamper_kill metadata carries expected fields.
        let meta: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        let parsed: serde_json::Value = serde_json::from_str(&meta).expect("metadata JSON");
        assert_eq!(parsed["subject"], "account");
        assert!(parsed.get("reason").is_none());
        assert_eq!(parsed["attempt_count"], 3);
        assert_eq!(parsed["kill_timestamp"], kill_ts);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(summary, "Tamper Kill on PC 01 during an Account session");

        // tamper_kill has the correct account_id and pc_id.
        let (account_id, pc_id): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT account_id, pc_id FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(pc_id.as_deref(), Some("pc-01"));

        // No logout or auto_kick rows.
        let logout_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type IN ('logout', 'auto_kick')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(logout_count, 0);

        // Session is unbound after tamper_kick.
        assert!(
            sessions.active_pc_ids().await.is_empty(),
            "session should be unbound after tamper_kick"
        );
        assert!(
            tokens.get(&token).await.is_none(),
            "account token should be revoked after tamper_kick"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_attributes_a_session_that_ended_before_report() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        sessions
            .logout(
                &pool,
                &AccountToken {
                    account_id: first.id.clone(),
                    pc_id: "pc-01".to_owned(),
                    session_id: session.id.clone(),
                },
            )
            .await?;
        sqlx::query(
            "UPDATE account_session_history
             SET ended_at = '9999-01-01 00:00:00', end_reason = 'logout'
             WHERE session_id = ?",
        )
        .bind(&session.id)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE pcs SET last_seen_at = ? WHERE id = 'pc-01'")
            .bind(&session.started_at)
            .execute(&pool)
            .await?;

        tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            &session.started_at,
            Some(0),
            1,
        )
        .await?;

        let (account_id, metadata): (Option<String>, String) = sqlx::query_as(
            "SELECT account_id, metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&metadata).expect("metadata JSON")["subject"],
            "account"
        );
        assert_eq!(sessions.active_pc_ids().await.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_attributes_session_closed_by_server_restart() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        let (session, _) = sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        sqlx::query("UPDATE pcs SET last_seen_at = ? WHERE id = 'pc-01'")
            .bind(&session.started_at)
            .execute(&pool)
            .await?;
        SessionStore::recover_open_sessions(&pool).await?;

        let after_restart = SessionStore::default();
        tamper_kill_with_age(
            &after_restart,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            &chrono::Utc::now().to_rfc3339(),
            Some(0),
            1,
        )
        .await?;

        let (account_id, metadata): (Option<String>, String) = sqlx::query_as(
            "SELECT account_id, metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&metadata).expect("metadata JSON")["subject"],
            "account"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_before_last_heartbeat_is_unknown() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        sqlx::query("UPDATE pcs SET last_seen_at = '2026-08-07 12:00:00' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;

        sessions
            .handle_tamper_kill(
                &pool,
                &AccountTokens::default(),
                "pc-01",
                "2026-08-07T11:59:59+00:00",
                1,
            )
            .await?;

        let (account_id, metadata): (Option<String>, String) = sqlx::query_as(
            "SELECT account_id, metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account_id, None);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&metadata).expect("metadata JSON")["subject"],
            "unknown"
        );
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            summary,
            "Tamper Kill on PC 01: Client did not report a Kill Report Age; it may be an older Client"
        );
        let reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason') FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "missing_report_age");
        assert_eq!(sessions.active_pc_ids().await.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_over_limit_age_records_invalid_reason() -> AppResult<()> {
        let (pool, sessions, _, _) = setup().await?;
        let subject = tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            "over-limit-report",
            Some(MAX_TAMPER_KILL_REPORT_AGE_MS + 1),
            1,
        )
        .await?;
        assert_eq!(subject, TamperKillSubject::Unknown);

        let (reason, summary): (String, String) = sqlx::query_as(
            "SELECT json_extract(metadata_json, '$.reason'), summary
             FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "invalid_report_age");
        assert_eq!(
            summary,
            "Tamper Kill on PC 01: Kill Report Age was malformed or exceeded the 24-hour sanity limit"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_does_not_blame_current_occupant_and_records_reason() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        sqlx::query(
            "UPDATE account_session_history
             SET started_at = '9999-01-01 00:00:00'
             WHERE account_id = ?",
        )
        .bind(&first.id)
        .execute(&pool)
        .await?;

        let subject = tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            "current-occupant-report",
            Some(0),
            1,
        )
        .await?;
        assert_eq!(subject, TamperKillSubject::Unknown);

        let (reason, summary): (String, String) = sqlx::query_as(
            "SELECT json_extract(metadata_json, '$.reason'), summary
             FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(reason, "current_occupant_not_attributed");
        assert_eq!(
            summary,
            "Tamper Kill on PC 01: no Account Session covered the kill instant; the current occupant was not blamed"
        );
        assert_eq!(sessions.active_pc_ids().await.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_tamper_kill_with_changed_attempt_count_is_not_duplicated() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        let kill_ts = chrono::Utc::now().to_rfc3339();

        sessions
            .handle_tamper_kill(&pool, &AccountTokens::default(), "pc-01", &kill_ts, 1)
            .await?;
        sessions
            .handle_tamper_kill(&pool, &AccountTokens::default(), "pc-01", &kill_ts, 2)
            .await?;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_guest_session_writes_guest_subject_and_kick() -> AppResult<()> {
        let (pool, sessions, _, _) = setup().await?;
        // Give pc-01 guest time, no account logged in.
        sqlx::query("UPDATE pcs SET remaining_seconds = 300 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE pcs SET last_seen_at = '2026-06-20T09:59:00+00:00' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;

        let subject = tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            "2026-06-20T10:00:00+00:00",
            Some(0),
            1,
        )
        .await?;
        assert_eq!(subject, TamperKillSubject::Guest);

        // Exactly one tamper_kill and one guest tamper_kick.
        let tamper_kill_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        let tamper_kick_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kick'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(tamper_kill_count, 1);
        assert_eq!(tamper_kick_count, 1);

        // Subject is guest, no account_id.
        let meta: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        let parsed: serde_json::Value = serde_json::from_str(&meta).expect("metadata JSON");
        assert_eq!(parsed["subject"], "guest");
        assert!(parsed.get("reason").is_none());
        assert_eq!(parsed["attempt_count"], 1);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(summary, "Tamper Kill on PC 01 during a Guest session");

        let account_id: Option<String> =
            sqlx::query_scalar("SELECT account_id FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        assert!(
            account_id.is_none(),
            "guest kill should have null account_id"
        );

        // Guest Time is unchanged.
        let guest_time: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(guest_time, 300);
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_idle_pc_writes_none_subject_and_no_kick() -> AppResult<()> {
        let (pool, sessions, _, _) = setup().await?;
        // pc-01 has 0 guest time and no account session.
        sqlx::query("UPDATE pcs SET last_seen_at = '2026-06-20 09:59:00' WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;

        let subject = tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            "2026-06-20T10:00:00+00:00",
            Some(0),
            1,
        )
        .await?;
        assert_eq!(subject, TamperKillSubject::None);

        let tamper_kill_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        let tamper_kick_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kick'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(tamper_kill_count, 1);
        assert_eq!(tamper_kick_count, 0);

        let meta: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        let parsed: serde_json::Value = serde_json::from_str(&meta).expect("metadata JSON");
        assert_eq!(parsed["subject"], "none");
        assert!(parsed.get("reason").is_none());
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM audit_log WHERE event_type = 'tamper_kill'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(summary, "Tamper Kill on PC 01 with no active session");
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_before_session_start_is_unknown_but_after_start_is_attributed(
    ) -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 600 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let subject = tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            "predates-session",
            Some(3_600_000),
            1,
        )
        .await?;
        assert_eq!(subject, TamperKillSubject::Unknown);

        // This reason is only reachable after a valid age reached the historical lookup.
        let (account_id, reason): (Option<String>, String) = sqlx::query_as(
            "SELECT account_id, json_extract(metadata_json, '$.reason')
             FROM audit_log WHERE event_type = 'tamper_kill'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account_id, None);
        assert_eq!(reason, "current_occupant_not_attributed");
        let tamper_kick_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'tamper_kick'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(tamper_kick_count, 0);
        assert_eq!(sessions.active_pc_ids().await.len(), 1);

        let subject = tamper_kill_with_age(
            &sessions,
            &pool,
            &AccountTokens::default(),
            "pc-01",
            "after-session-start",
            Some(0),
            1,
        )
        .await?;
        assert_eq!(subject, TamperKillSubject::Account);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_tamper_kill_report_is_acknowledged_exactly_once() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;
        let kill_ts = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE pcs SET last_seen_at = ? WHERE id = 'pc-01'")
            .bind(&kill_ts)
            .execute(&pool)
            .await?;

        assert_eq!(
            tamper_kill_with_age(
                &sessions,
                &pool,
                &AccountTokens::default(),
                "pc-01",
                &kill_ts,
                Some(0),
                3,
            )
            .await?,
            TamperKillSubject::Account
        );
        assert_eq!(
            tamper_kill_with_age(
                &sessions,
                &pool,
                &AccountTokens::default(),
                "pc-01",
                &kill_ts,
                Some(0),
                3,
            )
            .await?,
            TamperKillSubject::Account
        );

        let counts: (i64, i64) = sqlx::query_as(
            "SELECT
               SUM(event_type = 'tamper_kill'),
               SUM(event_type = 'tamper_kick')
             FROM audit_log",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(counts, (1, 1));
        Ok(())
    }

    #[tokio::test]
    async fn invalid_or_future_tamper_kill_timestamp_never_blames_current_account() -> AppResult<()>
    {
        for kill_ts in ["not-a-timestamp", "9999-12-31T23:59:59+00:00"] {
            let (pool, sessions, first, _) = setup().await?;
            sessions
                .login(&pool, &AccountTokens::default(), &first, "pc-01")
                .await?;

            sessions
                .handle_tamper_kill(&pool, &AccountTokens::default(), "pc-01", kill_ts, 1)
                .await?;

            let (account_id, metadata): (Option<String>, String) = sqlx::query_as(
                "SELECT account_id, metadata_json FROM audit_log WHERE event_type = 'tamper_kill'",
            )
            .fetch_one(&pool)
            .await?;
            assert_eq!(account_id, None);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&metadata).expect("metadata JSON")
                    ["subject"],
                "unknown"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn tamper_kill_never_produces_logout_or_auto_kick() -> AppResult<()> {
        let (pool, sessions, first, _) = setup().await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 600 WHERE id = ?")
            .bind(&first.id)
            .execute(&pool)
            .await?;
        let mut first = first;
        first.remaining_seconds = 600;
        sessions
            .login(&pool, &AccountTokens::default(), &first, "pc-01")
            .await?;

        let kill_ts = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE pcs SET last_seen_at = ? WHERE id = 'pc-01'")
            .bind(&kill_ts)
            .execute(&pool)
            .await?;
        sessions
            .handle_tamper_kill(&pool, &AccountTokens::default(), "pc-01", &kill_ts, 1)
            .await?;

        // Only tamper_kill and tamper_kick exist — no logout, auto_kick,
        // taken_over, or expired.
        let other_ends: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type IN ('logout', 'auto_kick', 'taken_over', 'expired')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(other_ends, 0);
        Ok(())
    }
}

use std::collections::{HashMap, HashSet};

use openpiso_core::{
    account::{normalize_username, normalize_username_lenient},
    audit::{AuditActor, AuditOutcome},
    time::format_labelled_duration,
    transfer::{validate_transfer, TransferError, POINTS_TRANSFER_TTL_SECONDS},
};
use serde_json::json;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    auth::{hash_password, hash_password_unchecked, is_bcrypt_hash, verify_password},
    errors::{AppError, AppResult},
    models::{
        Account, PendingPointsTransfer, PointsTransferRequest, PointsTransferRequestCreated,
        PointsTransferStatus,
    },
};

const MAX_ACCOUNT_DELETE_BATCH_SIZE: usize = 1_000;
const MAX_DELETE_FAILURE_METADATA_IDS: usize = 32;

#[derive(Clone, Copy)]
pub enum AccountCreator {
    SelfService,
    Admin,
}

impl AccountCreator {
    fn actor(self) -> AuditActor {
        match self {
            Self::SelfService => AuditActor::Account,
            Self::Admin => AuditActor::Admin,
        }
    }
}

pub async fn list(pool: &SqlitePool) -> AppResult<Vec<Account>> {
    Ok(sqlx::query_as::<
        _,
        (
            String,
            String,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        ),
    >(
        "SELECT accounts.id, accounts.username, accounts.remaining_seconds,
                    accounts.points_balance_centi, accounts.status, accounts.created_at,
                    accounts.deleted_at, accounts.imported_from, accounts.imported_at,
                    login_events.last_login_at,
                    accounts.imported_from IS NOT NULL
                      AND accounts.password_hash LIKE '$2%$%' AS legacy_not_logged_in
             FROM accounts
             LEFT JOIN (
                 SELECT account_id, MAX(created_at) AS last_login_at
                 FROM audit_log
                 WHERE event_type = 'login_succeeded'
                   AND outcome = 'success'
                 GROUP BY account_id
             ) AS login_events ON login_events.account_id = accounts.id
             ORDER BY accounts.username_normalized",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Account::from)
    .collect())
}

pub async fn create(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    creator: AccountCreator,
    pc_id: Option<&str>,
) -> AppResult<Account> {
    let (username, username_normalized) =
        normalize_username(username).map_err(|error| AppError::InvalidInput(error.to_string()))?;
    let password_hash = hash_password(password)?;
    let account_id = Uuid::new_v4().to_string();
    let mut transaction = crate::db::begin_immediate(pool).await?;

    let username_reserved = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(
             SELECT 1 FROM accounts WHERE username_normalized = ?
         )",
    )
    .bind(&username_normalized)
    .fetch_one(&mut *transaction)
    .await?;
    if username_reserved != 0 {
        transaction.rollback().await?;
        let mut event = NewAuditEvent::simple(
            "account_create_failed",
            AuditOutcome::Failure,
            creator.actor(),
            "Account creation failed",
        );
        event.pc_id = pc_id;
        event.metadata = json!({ "username_normalized": username_normalized });
        audit::append_standalone(pool, event).await?;
        return Err(AppError::Conflict(
            "username is already reserved".to_owned(),
        ));
    }

    let inserted = sqlx::query(
        "INSERT INTO accounts (id, username, username_normalized, password_hash)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&account_id)
    .bind(&username)
    .bind(&username_normalized)
    .bind(password_hash)
    .execute(&mut *transaction)
    .await;

    if let Err(error) = inserted {
        transaction.rollback().await?;
        let mut event = NewAuditEvent::simple(
            "account_create_failed",
            AuditOutcome::Failure,
            creator.actor(),
            "Account creation failed",
        );
        event.pc_id = pc_id;
        event.metadata = json!({ "username_normalized": username_normalized });
        audit::append_standalone(pool, event).await?;
        if is_unique_violation(&error) {
            return Err(AppError::Conflict(
                "username is already reserved".to_owned(),
            ));
        }
        return Err(error.into());
    }

    let mut event = NewAuditEvent::simple(
        "account_created",
        AuditOutcome::Success,
        creator.actor(),
        "Created customer account",
    );
    event.account_id = Some(&account_id);
    if matches!(creator, AccountCreator::SelfService) {
        event.actor_account_id = Some(&account_id);
    }
    event.pc_id = pc_id;
    event.metadata = json!({ "username": username });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    get(pool, &account_id).await
}

/// Return the normalized Username when it resolves to an active Account.
/// Invalid and unknown Usernames return `None` without creating throttle state.
pub async fn active_username_normalized(
    pool: &SqlitePool,
    username: &str,
) -> AppResult<Option<String>> {
    let Ok((_, normalized)) = normalize_username_lenient(username) else {
        return Ok(None);
    };
    let active = sqlx::query_scalar::<_, String>(
        "SELECT username_normalized FROM accounts
         WHERE username_normalized = ? AND status = 'active'",
    )
    .bind(&normalized)
    .fetch_optional(pool)
    .await?;
    Ok(active)
}

/// Return the active Account ID for a normalized Username when it still
/// resolves. Callers use this for canonical Audit Log attribution.
async fn active_account_id_for_normalized_username(
    pool: &SqlitePool,
    username_normalized: &str,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT id FROM accounts
         WHERE username_normalized = ? AND status = 'active'",
    )
    .bind(username_normalized)
    .fetch_optional(pool)
    .await?)
}

/// Record the one Audit Log event emitted when an Account enters lockout.
pub async fn audit_login_throttled(
    pool: &SqlitePool,
    username_normalized: &str,
    source_ip: &str,
    pc_id: Option<&str>,
    consecutive_failures: u32,
    lockout_seconds: i64,
) -> AppResult<()> {
    let account_id = active_account_id_for_normalized_username(pool, username_normalized).await?;
    let mut event = NewAuditEvent::simple(
        "account_login_throttled",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Account login throttled after repeated failures",
    );
    event.actor_account_id = account_id.as_deref();
    event.account_id = account_id.as_deref();
    event.pc_id = pc_id;
    event.metadata = json!({
        "username_normalized": username_normalized,
        "source_ip": source_ip,
        "consecutive_failures": consecutive_failures,
        "lockout_seconds": lockout_seconds,
    });
    audit::append_standalone(pool, event).await?;
    Ok(())
}

/// Record a signup denial caused by Shop Closed without creating an account.
pub async fn audit_shop_closed_signup(
    pool: &SqlitePool,
    username: &str,
    pc_id: Option<&str>,
) -> AppResult<()> {
    let normalized_username = normalize_username(username)
        .ok()
        .map(|(_, normalized)| normalized);
    let mut event = NewAuditEvent::simple(
        "account_create_failed",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Account signup denied while Shop Closed",
    );
    event.pc_id = pc_id;
    event.metadata = json!({
        "reason": "shop_closed",
        "username_normalized": normalized_username,
    });
    audit::append_standalone(pool, event).await?;
    Ok(())
}

/// Record a login denial caused by Shop Closed without checking credentials.
/// The account lookup is only for honest audit attribution; a missing or
/// malformed username still receives the same machine-readable denial at the
/// HTTP boundary.
pub async fn audit_shop_closed_login(
    pool: &SqlitePool,
    username: &str,
    pc_id: Option<&str>,
) -> AppResult<()> {
    let normalized_username = normalize_username_lenient(username)
        .ok()
        .map(|(_, normalized)| normalized);
    let account_id = if let Some(normalized) = normalized_username.as_deref() {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM accounts
             WHERE username_normalized = ? AND status = 'active'",
        )
        .bind(normalized)
        .fetch_optional(pool)
        .await?
    } else {
        None
    };

    let mut event = NewAuditEvent::simple(
        "login_failed",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Account login denied while Shop Closed",
    );
    event.actor_account_id = account_id.as_deref();
    event.account_id = account_id.as_deref();
    event.pc_id = pc_id;
    event.metadata = json!({
        "reason": "shop_closed",
        "username_normalized": normalized_username,
    });
    audit::append_standalone(pool, event).await?;
    Ok(())
}

/// Test setup audits immediately; production defers to session binding.
#[cfg(test)]
pub async fn authenticate(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    pc_id: Option<&str>,
) -> AppResult<Account> {
    authenticate_inner(pool, username, password, pc_id, true).await
}

/// Authenticate credentials for the HTTP login flow. The successful-login
/// audit is deferred until the Active Session binding transaction succeeds, so
/// a concurrent Account deletion cannot leave a false `login_succeeded` row.
pub async fn authenticate_for_session(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    pc_id: Option<&str>,
) -> AppResult<Account> {
    authenticate_inner(pool, username, password, pc_id, false).await
}

async fn authenticate_inner(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    pc_id: Option<&str>,
    record_success_audit: bool,
) -> AppResult<Account> {
    let (_, username_normalized) = normalize_username_lenient(username)
        .map_err(|error| AppError::InvalidInput(error.to_string()))?;
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id, username, password_hash, remaining_seconds, points_balance_centi,
                status, created_at, deleted_at, imported_from, imported_at
         FROM accounts
         WHERE username_normalized = ? AND status = 'active'",
    )
    .bind(&username_normalized)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        // Authentication resolves only active identities. A deleted-only
        // history lookup is used solely to retain the existing, useful audit
        // attribution and denial reason; it never supplies credentials.
        let deleted_account_id = sqlx::query_scalar::<_, String>(
            "SELECT id FROM accounts
             WHERE username_normalized = ? AND status = 'deleted'
             ORDER BY deleted_at DESC, id DESC LIMIT 1",
        )
        .bind(&username_normalized)
        .fetch_optional(pool)
        .await?;
        let mut event = NewAuditEvent::simple(
            "login_failed",
            AuditOutcome::Denied,
            if deleted_account_id.is_some() {
                AuditActor::Account
            } else {
                AuditActor::System
            },
            "Account login denied",
        );
        event.actor_account_id = deleted_account_id.as_deref();
        event.account_id = deleted_account_id.as_deref();
        event.pc_id = pc_id;
        event.metadata = json!({
            "reason": if deleted_account_id.is_some() {
                "deleted_account"
            } else {
                "unknown_username"
            },
            "username_normalized": username_normalized
        });
        audit::append_standalone(pool, event).await?;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_owned(),
        ));
    };

    let account_id = row.0.clone();
    let stored_hash = row.2.clone();
    let denial_reason = if row.5 == "deleted" {
        Some("deleted_account")
    } else if !verify_password(password, &stored_hash) {
        Some("bad_password")
    } else {
        None
    };
    if let Some(reason) = denial_reason {
        let mut event = NewAuditEvent::simple(
            "login_failed",
            AuditOutcome::Denied,
            AuditActor::Account,
            "Account login denied",
        );
        event.actor_account_id = Some(&account_id);
        event.account_id = Some(&account_id);
        event.pc_id = pc_id;
        event.metadata = json!({ "reason": reason });
        audit::append_standalone(pool, event).await?;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_owned(),
        ));
    }

    // Transitional: upgrade a legacy bcrypt credential to Argon2.
    // Compare-and-swap on the stored hash prevents overwriting a concurrent
    // Password Change or Reset (see `replace_verified_password`).
    if is_bcrypt_hash(&stored_hash) {
        upgrade_credential(pool, &account_id, password, &stored_hash).await?;
    }

    if record_success_audit {
        let mut event = NewAuditEvent::simple(
            "login_succeeded",
            AuditOutcome::Success,
            AuditActor::Account,
            "Customer authenticated",
        );
        event.actor_account_id = Some(&account_id);
        event.account_id = Some(&account_id);
        event.pc_id = pc_id;
        audit::append_standalone(pool, event).await?;
    }
    Ok(Account {
        id: row.0,
        username: row.1,
        remaining_seconds: row.3,
        points_balance_centi: row.4,
        status: row.5,
        created_at: row.6,
        deleted_at: row.7,
        imported_from: row.8,
        imported_at: row.9,
        legacy_not_logged_in: false,
        last_login_at: None,
    })
}

/// After a successful bcrypt verification, re-hash the password with Argon2
/// and replace the stored credential using compare-and-swap. If the hash has
/// already been upgraded by a concurrent login (or superseded by a Password
/// Change / Reset), this is a safe no-op — the hash changed so the old bcrypt
/// hash no longer matches `WHERE password_hash = ?`.
///
/// The audit event carries no password material.
async fn upgrade_credential(
    pool: &SqlitePool,
    account_id: &str,
    password: &str,
    old_bcrypt_hash: &str,
) -> AppResult<()> {
    // Use the unchecked hasher: the password was already verified through
    // bcrypt, so length validation is unnecessary and would incorrectly
    // reject passwords shorter than 4 or longer than 128 characters that
    // the original system allowed.
    let new_hash = hash_password_unchecked(password)?;
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let replaced = sqlx::query(
        "UPDATE accounts
         SET password_hash = ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active' AND password_hash = ?",
    )
    .bind(&new_hash)
    .bind(account_id)
    .bind(old_bcrypt_hash)
    .execute(&mut *transaction)
    .await?;
    if replaced.rows_affected() == 1 {
        let mut event = NewAuditEvent::simple(
            "credential_upgraded",
            AuditOutcome::Success,
            AuditActor::Account,
            "Legacy bcrypt credential re-hashed to Argon2",
        );
        event.actor_account_id = Some(account_id);
        event.account_id = Some(account_id);
        audit::append(&mut transaction, event).await?;
    }
    // rows_affected == 0 means another writer (concurrent login, Password
    // Change, or Reset) already moved the hash — no-op is correct.
    transaction.commit().await?;
    Ok(())
}

pub async fn reset_password(
    pool: &SqlitePool,
    account_id: &str,
    password: &str,
) -> AppResult<Account> {
    let password_hash = hash_password(password)?;
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let updated = sqlx::query(
        "UPDATE accounts
         SET password_hash = ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active'",
    )
    .bind(password_hash)
    .bind(account_id)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() == 0 {
        transaction.rollback().await?;
        let mut event = NewAuditEvent::simple(
            "account_password_reset_failed",
            AuditOutcome::Denied,
            AuditActor::Admin,
            "Customer password reset denied",
        );
        event.metadata = json!({ "requested_account_id": account_id });
        audit::append_standalone(pool, event).await?;
        return Err(AppError::NotFound("active account".to_owned()));
    }
    let mut event = NewAuditEvent::simple(
        "account_password_reset",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Reset customer password",
    );
    event.account_id = Some(account_id);
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    get(pool, account_id).await
}

#[allow(dead_code)]
pub async fn soft_delete(pool: &SqlitePool, account_id: &str) -> AppResult<Account> {
    let mut accounts = soft_delete_many(pool, &[account_id.to_owned()]).await?;
    Ok(accounts.remove(0))
}

/// Owner-initiated atomic deletion of an explicit Account set. Every ID is
/// checked as active while holding the same immediate transaction that applies
/// all tombstones, forfeitures, and audit events. A stale or invalid selection
/// rolls back the whole batch and emits exactly one failure event separately.
pub async fn soft_delete_many(
    pool: &SqlitePool,
    account_ids: &[String],
) -> AppResult<Vec<Account>> {
    let deleted_ids = soft_delete_many_ids(pool, account_ids).await?;
    let mut deleted = Vec::with_capacity(deleted_ids.len());
    for account_id in deleted_ids {
        deleted.push(get(pool, &account_id).await?);
    }
    Ok(deleted)
}

/// Apply an owner Account deletion batch and return only the committed IDs.
/// The owner HTTP handler uses this form because it needs no post-commit
/// Account reads before revoking live access.
pub async fn soft_delete_many_ids(
    pool: &SqlitePool,
    account_ids: &[String],
) -> AppResult<Vec<String>> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let unique_ids = account_ids.iter().collect::<HashSet<_>>();
    let invalid_shape = account_ids.is_empty()
        || account_ids.len() > MAX_ACCOUNT_DELETE_BATCH_SIZE
        || account_ids
            .iter()
            .any(|account_id| account_id.trim().is_empty())
        || unique_ids.len() != account_ids.len();

    let mut balances = HashMap::new();
    if !invalid_shape {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, remaining_seconds, points_balance_centi
             FROM accounts WHERE status = 'active' AND id IN (",
        );
        let mut separated = query.separated(", ");
        for account_id in account_ids {
            separated.push_bind(account_id);
        }
        separated.push_unseparated(")");
        for row in query
            .build_query_as::<(String, i64, i64)>()
            .fetch_all(&mut *transaction)
            .await?
        {
            balances.insert(row.0, (row.1, row.2));
        }
    }

    let missing = account_ids
        .iter()
        .filter(|account_id| !balances.contains_key(*account_id))
        .count();
    if invalid_shape || missing != 0 {
        transaction.rollback().await?;
        append_delete_failed(pool, account_ids).await?;
        return Err(if invalid_shape {
            AppError::InvalidInput("invalid account deletion request".to_owned())
        } else {
            AppError::NotFound("active accounts".to_owned())
        });
    }

    let deleted_at = crate::db::attendance::finalize_in_transaction(
        &mut transaction,
        chrono::Utc::now(),
        crate::db::attendance::host_offset(),
    )
    .await?
    .utc_floor;
    for account_id in account_ids {
        let (remaining_seconds, points_balance_centi) = balances
            .get(account_id)
            .copied()
            .ok_or_else(|| AppError::NotFound("active account".to_owned()))?;
        let forfeiture_plan = crate::db::attendance_claims::plan_account_deletion_forfeitures(
            &mut transaction,
            account_id,
            deleted_at,
        )
        .await?;
        sqlx::query(
            "UPDATE accounts
             SET status = 'deleted', remaining_seconds = 0, points_balance_centi = 0,
                 deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND status = 'active'",
        )
        .bind(account_id)
        .execute(&mut *transaction)
        .await?;
        let mut event = NewAuditEvent::simple(
            "account_deleted",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Soft-deleted customer account and forfeited balances",
        );
        event.account_id = Some(account_id);
        event.seconds_delta = Some(-remaining_seconds);
        event.points_delta_centi = Some(-points_balance_centi);
        event.attendance_seconds_delta =
            (forfeiture_plan.total_seconds > 0).then_some(-forfeiture_plan.total_seconds);
        let deletion_audit_event_id = audit::append(&mut transaction, event).await?;
        crate::db::attendance_claims::record_account_deletion_forfeitures(
            &mut transaction,
            account_id,
            &deletion_audit_event_id,
            deleted_at,
            &forfeiture_plan,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(account_ids.to_vec())
}

async fn append_delete_failed(pool: &SqlitePool, account_ids: &[String]) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "account_delete_failed",
        AuditOutcome::Denied,
        AuditActor::Admin,
        "Customer account deletion batch denied",
    );
    let sampled_account_ids = account_ids
        .iter()
        .take(MAX_DELETE_FAILURE_METADATA_IDS)
        .cloned()
        .collect::<Vec<_>>();
    event.metadata = json!({
        "requested_count": account_ids.len(),
        "requested_account_ids_sample": sampled_account_ids,
        "sample_truncated": account_ids.len() > MAX_DELETE_FAILURE_METADATA_IDS,
    });
    audit::append_standalone(pool, event).await?;
    Ok(())
}

/// Owner-initiated **time adjustment** on an active account (web-admin). The
/// signed `seconds_delta` is applied atomically; the result is clamped at
/// zero so remaining time never goes negative (durable invariant). The
/// applied delta (which may differ from the requested one when a deduction
/// would underflow) is written to the audit event. DB-only: callers with a
/// live bound session go through [`crate::sessions::SessionStore::adjust_time`]
/// so the cached session is refreshed at once.
pub async fn adjust_time(
    pool: &SqlitePool,
    account_id: &str,
    seconds_delta: i64,
) -> AppResult<Account> {
    adjust_balance(pool, account_id, Balance::Time, seconds_delta).await
}

/// Owner-initiated **points adjustment** on an active account (web-admin).
/// Mirrors [`adjust_time`]: signed `points_delta_centi`, clamped at zero, audited
/// with the actually-applied delta.
pub async fn adjust_points(
    pool: &SqlitePool,
    account_id: &str,
    points_delta_centi: i64,
) -> AppResult<Account> {
    adjust_balance(pool, account_id, Balance::Points, points_delta_centi).await
}

#[derive(Clone, Copy)]
enum Balance {
    Time,
    Points,
}

impl Balance {
    fn column(self) -> &'static str {
        match self {
            Self::Time => "remaining_seconds",
            Self::Points => "points_balance_centi",
        }
    }

    fn event_type(self) -> &'static str {
        match self {
            Self::Time => "account_time_adjusted",
            Self::Points => "account_points_adjusted",
        }
    }
}

async fn adjust_balance(
    pool: &SqlitePool,
    account_id: &str,
    balance: Balance,
    delta: i64,
) -> AppResult<Account> {
    if delta == 0 {
        return Err(AppError::InvalidInput(
            "adjustment must be non-zero".to_owned(),
        ));
    }
    let column = balance.column();
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let current = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT {column} FROM accounts WHERE id = ? AND status = 'active'"
    ))
    .bind(account_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(current) = current else {
        transaction.rollback().await?;
        return Err(AppError::NotFound("active account".to_owned()));
    };
    // Clamp at zero: remaining time and points never go below zero.
    let new_value = (current + delta).max(0);
    let applied_delta = new_value - current;
    sqlx::query(&format!(
        "UPDATE accounts SET {column} = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?"
    ))
    .bind(new_value)
    .bind(account_id)
    .execute(&mut *transaction)
    .await?;

    let mut event = NewAuditEvent::simple(
        balance.event_type(),
        AuditOutcome::Success,
        AuditActor::Admin,
        "Owner adjusted account balance",
    );
    event.account_id = Some(account_id);
    match balance {
        Balance::Time => event.seconds_delta = Some(applied_delta),
        Balance::Points => event.points_delta_centi = Some(applied_delta),
    }
    event.metadata = json!({ "requested_delta": delta, "applied_delta": applied_delta });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    get(pool, account_id).await
}

pub async fn get(pool: &SqlitePool, account_id: &str) -> AppResult<Account> {
    sqlx::query_as::<
        _,
        (
            String,
            String,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        ),
    >(
        "SELECT accounts.id, accounts.username, accounts.remaining_seconds,
                accounts.points_balance_centi, accounts.status, accounts.created_at,
                accounts.deleted_at, accounts.imported_from, accounts.imported_at,
                login_events.last_login_at,
                accounts.imported_from IS NOT NULL
                  AND accounts.password_hash LIKE '$2%$%' AS legacy_not_logged_in
         FROM accounts
         LEFT JOIN (
             SELECT account_id, MAX(created_at) AS last_login_at
             FROM audit_log
             WHERE event_type = 'login_succeeded'
               AND outcome = 'success'
             GROUP BY account_id
         ) AS login_events ON login_events.account_id = accounts.id
         WHERE accounts.id = ?",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?
    .map(Account::from)
    .ok_or_else(|| AppError::NotFound(format!("account '{account_id}'")))
}

// ---------------------------------------------------------------------------
// Account-to-account transfers (ADR-0030) + Password Change (ADR-0037).
//
// Time Transfer is immediate, atomic, and server-authoritative. Points
// Transfer is owner-confirmed via a single-use, time-boxed 6-digit code
// that the customer reads off the Client and the owner reads back. Both
// functions re-read the sender's balance inside the SQLite transaction
// and validate via `openpiso_core::transfer::validate_transfer`, so the
// money-and-time rules (≥0, at least one currency > 0, sender has enough)
// stay in one place and the two paths cannot diverge.
//
// Self-transfer is rejected at the username layer: the recipient lookup
// normalizes the supplied username and refuses to match the sender's own
// normalized identity.
//
// These helpers take a resolved `from_account_id`/`account_id`, not the raw
// session token. The phase-2 HTTP layer MUST derive that id from the verified
// token, never from the request body — the durable invariant "never trust a
// frontend-supplied id" lives at that boundary (the input structs carry only
// `token`, no caller-supplied account id).
// ---------------------------------------------------------------------------

/// Customer-initiated **Time Transfer** to another account by normalized
/// Username (ADR-0030). Atomic: re-reads the sender's balance inside the
/// transaction, validates via the core module, debits/credits, inserts the
/// `account_transfers` detail row plus the `account_transfer_out` and
/// `account_transfer_in` audit events, and returns the refreshed sender
/// `Account`. Self-drain to zero is allowed (normal expiry follows
/// ADR-0022).
/// Outcome of an immediate Time Transfer (ADR-0030). Carries the refreshed
/// `sender` account plus the recipient's id and new balance so the caller can
/// refresh the recipient's live session/timer on whichever PC they are bound
/// to — the credit must not look stale until the recipient's next tick.
pub struct TimeTransferOutcome {
    pub sender: Account,
    pub recipient_id: String,
    pub recipient_remaining_seconds: i64,
}

pub async fn transfer_time(
    pool: &SqlitePool,
    from_account_id: &str,
    to_username: &str,
    seconds: i64,
) -> AppResult<TimeTransferOutcome> {
    let to_username = to_username.trim();
    if to_username.is_empty() {
        return Err(AppError::InvalidInput("recipient is required".to_owned()));
    }
    if seconds <= 0 {
        return Err(AppError::InvalidInput(
            "seconds to transfer must be positive".to_owned(),
        ));
    }
    let (_, to_username_normalized) = normalize_username(to_username)
        .map_err(|error| AppError::InvalidInput(error.to_string()))?;

    let mut transaction = crate::db::begin_immediate(pool).await?;
    let sender = sqlx::query_as::<_, (String, String, i64, i64, String)>(
        "SELECT username, username_normalized, remaining_seconds, points_balance_centi, status
         FROM accounts WHERE id = ?",
    )
    .bind(from_account_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("account '{from_account_id}'")))?;
    let (sender_username, sender_normalized, sender_seconds, sender_points, sender_status) = sender;
    if sender_status != "active" {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "sender account is not active".to_owned(),
        ));
    }
    if sender_normalized == to_username_normalized {
        transaction.rollback().await?;
        return Err(AppError::InvalidInput(
            "cannot transfer to yourself".to_owned(),
        ));
    }
    validate_transfer(sender_seconds, sender_points, seconds, 0).map_err(transfer_error_to_app)?;

    let recipient = sqlx::query_as::<_, (String, String)>(
        "SELECT id, status FROM accounts
         WHERE username_normalized = ? AND status = 'active'",
    )
    .bind(&to_username_normalized)
    .fetch_optional(&mut *transaction)
    .await?;
    let (to_account_id, to_status) = recipient
        .ok_or_else(|| AppError::NotFound(format!("recipient '{to_username}' does not exist")))?;
    if to_status != "active" {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "recipient account is not active".to_owned(),
        ));
    }

    let debited = sqlx::query_as::<
        _,
        (
            String,
            String,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        ),
    >(
        "UPDATE accounts
         SET remaining_seconds = remaining_seconds - ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active' AND remaining_seconds >= ?
         RETURNING id, username, remaining_seconds, points_balance_centi, status,
                   created_at, deleted_at, imported_from, imported_at,
                   imported_from IS NOT NULL AND password_hash LIKE '$2%$%'",
    )
    .bind(seconds)
    .bind(from_account_id)
    .bind(seconds)
    .fetch_optional(&mut *transaction)
    .await?
    .map(Account::from)
    .ok_or_else(|| AppError::Conflict("insufficient seconds".to_owned()))?;

    let recipient_remaining_seconds = sqlx::query_scalar::<_, i64>(
        "UPDATE accounts
         SET remaining_seconds = remaining_seconds + ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active'
         RETURNING remaining_seconds",
    )
    .bind(seconds)
    .bind(&to_account_id)
    .fetch_one(&mut *transaction)
    .await?;

    let out_summary = format!(
        "Sent {} to {to_username}",
        format_labelled_duration(seconds)
    );
    let mut out_event = NewAuditEvent::simple(
        "account_transfer_out",
        AuditOutcome::Success,
        AuditActor::Account,
        &out_summary,
    );
    out_event.actor_account_id = Some(from_account_id);
    out_event.account_id = Some(from_account_id);
    out_event.seconds_delta = Some(-seconds);
    out_event.cash_flag = false;
    out_event.metadata = json!({
        "to_account_id": to_account_id,
        "to_username": to_username,
        "seconds": seconds
    });
    let out_audit_id = audit::append(&mut transaction, out_event).await?;

    sqlx::query(
        "INSERT INTO account_transfers
           (id, from_account_id, to_account_id, seconds, audit_event_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(from_account_id)
    .bind(&to_account_id)
    .bind(seconds)
    .bind(&out_audit_id)
    .execute(&mut *transaction)
    .await?;

    let in_summary = format!(
        "Received {} from {sender_username}",
        format_labelled_duration(seconds)
    );
    let mut in_event = NewAuditEvent::simple(
        "account_transfer_in",
        AuditOutcome::Success,
        AuditActor::Account,
        &in_summary,
    );
    // The sender initiated the transfer; the recipient remains the target
    // account represented by `account_id`.
    in_event.actor_account_id = Some(from_account_id);
    in_event.account_id = Some(&to_account_id);
    in_event.seconds_delta = Some(seconds);
    in_event.cash_flag = false;
    in_event.metadata = json!({
        "from_account_id": from_account_id,
        "from_username": sender_username,
        "seconds": seconds
    });
    audit::append(&mut transaction, in_event).await?;

    transaction.commit().await?;
    Ok(TimeTransferOutcome {
        sender: debited,
        recipient_id: to_account_id,
        recipient_remaining_seconds,
    })
}

/// Customer-initiated **Points Transfer Request** to another account by
/// normalized Username (ADR-0030). The customer reads the returned 6-digit
/// `code` off their Client; the owner enters that code in web-admin to
/// confirm. The pending row is durable — a server restart does not drop
/// the request. The sender is **soft-checked** for the points (the
/// confirm step re-validates atomically, so the user could spend/spin
/// between request and confirm).
pub async fn request_points_transfer(
    pool: &SqlitePool,
    from_account_id: &str,
    to_username: &str,
    points_centi: i64,
) -> AppResult<PointsTransferRequestCreated> {
    let to_username = to_username.trim();
    if to_username.is_empty() {
        return Err(AppError::InvalidInput("recipient is required".to_owned()));
    }
    if points_centi <= 0 {
        return Err(AppError::InvalidInput(
            "points to transfer must be positive".to_owned(),
        ));
    }
    let (_, to_username_normalized) = normalize_username(to_username)
        .map_err(|error| AppError::InvalidInput(error.to_string()))?;

    let mut transaction = crate::db::begin_immediate(pool).await?;
    let sender = sqlx::query_as::<_, (String, i64, String)>(
        "SELECT username_normalized, points_balance_centi, status
         FROM accounts WHERE id = ?",
    )
    .bind(from_account_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("account '{from_account_id}'")))?;
    let (sender_normalized, sender_points, sender_status) = sender;
    if sender_status != "active" {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "sender account is not active".to_owned(),
        ));
    }
    if sender_normalized == to_username_normalized {
        transaction.rollback().await?;
        return Err(AppError::InvalidInput(
            "cannot transfer to yourself".to_owned(),
        ));
    }
    // Soft pre-check via the shared validator so the request and the
    // immediate time path apply the same money rules (ADR-0030 §5.2); the
    // confirm step re-validates atomically against the live balance.
    if let Err(error) = validate_transfer(0, sender_points, 0, points_centi) {
        transaction.rollback().await?;
        return Err(transfer_error_to_app(error));
    }

    let recipient = sqlx::query_as::<_, (String, String)>(
        "SELECT id, status FROM accounts
         WHERE username_normalized = ? AND status = 'active'",
    )
    .bind(&to_username_normalized)
    .fetch_optional(&mut *transaction)
    .await?;
    let (to_account_id, to_status) = recipient
        .ok_or_else(|| AppError::NotFound(format!("recipient '{to_username}' does not exist")))?;
    if to_status != "active" {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "recipient account is not active".to_owned(),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let code = generate_unique_pending_code(&mut transaction).await?;

    let requested_points = format_points_centi(points_centi);
    let request_summary = format!("Requested {requested_points}-point transfer to {to_username}");
    let mut event = NewAuditEvent::simple(
        "points_transfer_requested",
        AuditOutcome::Success,
        AuditActor::Account,
        &request_summary,
    );
    event.actor_account_id = Some(from_account_id);
    event.account_id = Some(from_account_id);
    event.points_delta_centi = Some(0);
    event.cash_flag = false;
    event.metadata = json!({
        "request_id": id,
        "code": code,
        "to_account_id": to_account_id,
        "to_username": to_username,
        "points_centi": points_centi
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    sqlx::query(
        "INSERT INTO points_transfer_requests
           (id, from_account_id, to_account_id, points_centi, code, status, audit_event_id)
         VALUES (?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(&id)
    .bind(from_account_id)
    .bind(&to_account_id)
    .bind(points_centi)
    .bind(&code)
    .bind(&audit_event_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(PointsTransferRequestCreated { id, code })
}

/// Owner-initiated confirmation of a pending Points Transfer Request by
/// 6-digit `code` (ADR-0030). Re-validates and moves the points atomically;
/// the sender may have spent/spun in the interim and is rejected on the
/// second check. The result is the persisted `PointsTransferRequest` so
/// the owner UI can show the resolved status.
pub async fn confirm_points_transfer(
    pool: &SqlitePool,
    code: &str,
) -> AppResult<PointsTransferRequest> {
    let normalized_code = normalize_transfer_code(code)?;
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let request = load_pending_request(&mut transaction, &normalized_code).await?;
    let (request_id, from_account_id, to_account_id, points_centi) = (
        request.id,
        request.from_account_id,
        request.to_account_id,
        request.points_centi,
    );
    let (from_status, sender_points) = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, points_balance_centi FROM accounts WHERE id = ?",
    )
    .bind(&from_account_id)
    .fetch_one(&mut *transaction)
    .await?;
    let to_status = sqlx::query_scalar::<_, String>("SELECT status FROM accounts WHERE id = ?")
        .bind(&to_account_id)
        .fetch_one(&mut *transaction)
        .await?;
    if from_status != "active" || to_status != "active" {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "transfer parties are not active".to_owned(),
        ));
    }

    // Re-validate against the live sender balance via the shared validator
    // (ADR-0030 §5.2): the sender may have spent or spun between request and
    // confirm. The guarded debit below is the atomic backstop.
    if let Err(error) = validate_transfer(0, sender_points, 0, points_centi) {
        transaction.rollback().await?;
        return Err(transfer_error_to_app(error));
    }

    // Claim the request first: only one confirm can flip `pending` →
    // `confirmed`. A concurrent or double-submitted confirm sees
    // `rows_affected == 0` and bails before any points move, so the code is
    // strictly single-use regardless of connection-pool interleaving.
    let claimed = sqlx::query(
        "UPDATE points_transfer_requests
         SET status = 'confirmed', resolved_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'pending'",
    )
    .bind(&request_id)
    .execute(&mut *transaction)
    .await?;
    if claimed.rows_affected() != 1 {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "transfer code is no longer pending".to_owned(),
        ));
    }

    let debited = sqlx::query_as::<_, (i64,)>(
        "UPDATE accounts
         SET points_balance_centi = points_balance_centi - ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active' AND points_balance_centi >= ?
         RETURNING points_balance_centi",
    )
    .bind(points_centi)
    .bind(&from_account_id)
    .bind(points_centi)
    .fetch_optional(&mut *transaction)
    .await?;
    if debited.is_none() {
        transaction.rollback().await?;
        return Err(AppError::Conflict("insufficient points".to_owned()));
    }
    sqlx::query(
        "UPDATE accounts
         SET points_balance_centi = points_balance_centi + ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active'",
    )
    .bind(points_centi)
    .bind(&to_account_id)
    .execute(&mut *transaction)
    .await?;

    let from_username =
        sqlx::query_scalar::<_, String>("SELECT username FROM accounts WHERE id = ?")
            .bind(&from_account_id)
            .fetch_one(&mut *transaction)
            .await?;
    let to_username = sqlx::query_scalar::<_, String>("SELECT username FROM accounts WHERE id = ?")
        .bind(&to_account_id)
        .fetch_one(&mut *transaction)
        .await?;
    let sent_points = format_points_centi(points_centi);
    let out_summary = format!("Sent {sent_points} points to {to_username} (owner-confirmed)");
    let mut out_event = NewAuditEvent::simple(
        "points_transfer_out",
        AuditOutcome::Success,
        AuditActor::Account,
        &out_summary,
    );
    out_event.actor_account_id = Some(&from_account_id);
    out_event.account_id = Some(&from_account_id);
    out_event.points_delta_centi = Some(-points_centi);
    out_event.cash_flag = false;
    out_event.metadata = json!({
        "request_id": request_id,
        "code": normalized_code,
        "to_account_id": to_account_id,
        "to_username": to_username,
        "points_centi": points_centi
    });
    audit::append(&mut transaction, out_event).await?;

    let in_summary =
        format!("Received {sent_points} points from {from_username} (owner-confirmed)");
    let mut in_event = NewAuditEvent::simple(
        "points_transfer_in",
        AuditOutcome::Success,
        AuditActor::Account,
        &in_summary,
    );
    // The sender initiated the transfer; the recipient remains the target
    // account represented by `account_id`.
    in_event.actor_account_id = Some(&from_account_id);
    in_event.account_id = Some(&to_account_id);
    in_event.points_delta_centi = Some(points_centi);
    in_event.cash_flag = false;
    in_event.metadata = json!({
        "request_id": request_id,
        "code": normalized_code,
        "from_account_id": from_account_id,
        "from_username": from_username,
        "points_centi": points_centi
    });
    audit::append(&mut transaction, in_event).await?;

    transaction.commit().await?;
    load_request_by_id(pool, &request_id).await
}

/// Owner-initiated rejection of a pending Points Transfer Request by
/// 6-digit `code` (ADR-0030). Marks the request `rejected`, writes the
/// `points_transfer_rejected` audit event, and does not move points. The
/// resolved request is returned so the owner UI can show the new status.
pub async fn reject_points_transfer(
    pool: &SqlitePool,
    code: &str,
) -> AppResult<PointsTransferRequest> {
    let normalized_code = normalize_transfer_code(code)?;
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let request = load_pending_request(&mut transaction, &normalized_code).await?;
    let request_id = request.id;
    let rejected = sqlx::query(
        "UPDATE points_transfer_requests
         SET status = 'rejected', resolved_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'pending'",
    )
    .bind(&request_id)
    .execute(&mut *transaction)
    .await?;
    if rejected.rows_affected() != 1 {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "transfer code is no longer pending".to_owned(),
        ));
    }
    let mut event = NewAuditEvent::simple(
        "points_transfer_rejected",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Owner rejected points transfer request",
    );
    event.metadata = json!({
        "request_id": request_id,
        "code": normalized_code
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    load_request_by_id(pool, &request_id).await
}

/// Customer-facing message for a concurrent password change conflict.
pub(crate) const PASSWORD_CHANGE_CONFLICT_MSG: &str = "password change conflict";

async fn audit_password_change_denial(pool: &SqlitePool, account_id: Option<&str>, reason: &str) {
    let mut denial = NewAuditEvent::simple(
        "account_password_change_failed",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Account password change denied",
    );
    denial.actor_account_id = account_id;
    denial.account_id = account_id;
    denial.metadata = json!({ "reason": reason });
    let _ = audit::append_standalone(pool, denial).await;
}

/// Customer self-service Password Change (ADR-0037). Synchronous Argon2 work
/// happens outside SQLite transactions. Replacement uses
/// compare-and-swap semantics, so a concurrently superseded credential cannot
/// overwrite the winner. Account Session and Active Session remain valid.
pub async fn change_password(
    pool: &SqlitePool,
    account_id: &str,
    old_password: &str,
    new_password: &str,
) -> AppResult<Account> {
    // Input validation runs before any account lookup so invalid requests
    // keep their pre-existing validation-error semantics regardless of
    // account state (ADR-0037: keep existing validation order).
    if old_password.is_empty() {
        return Err(AppError::InvalidInput(
            "old password is required".to_owned(),
        ));
    }
    crate::auth::validate_password(new_password)?;

    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT password_hash, status FROM accounts WHERE id = ?",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    let Some((password_hash, status)) = row else {
        audit_password_change_denial(pool, None, "account_not_active").await;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_owned(),
        ));
    };
    if status != "active" {
        audit_password_change_denial(pool, Some(account_id), "account_not_active").await;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_owned(),
        ));
    }

    let verified = verify_password(old_password, &password_hash);
    if !verified {
        // Best-effort: a failed audit write must not turn the denial into a
        // 500. The customer always gets the generic unauthorized response.
        audit_password_change_denial(pool, Some(account_id), "bad_password").await;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_owned(),
        ));
    }
    let new_hash = hash_password(new_password)?;
    replace_verified_password(pool, account_id, &password_hash, &new_hash).await
}

async fn replace_verified_password(
    pool: &SqlitePool,
    account_id: &str,
    verified_hash: &str,
    new_hash: &str,
) -> AppResult<Account> {
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let replaced = sqlx::query(
        "UPDATE accounts
         SET password_hash = ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active' AND password_hash = ?",
    )
    .bind(new_hash)
    .bind(account_id)
    .bind(verified_hash)
    .execute(&mut *transaction)
    .await?;
    if replaced.rows_affected() != 1 {
        // Re-check account status within the write transaction's view to
        // distinguish a superseded credential from an inactive/removed Account.
        // Must query before rollback.
        let current_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM accounts WHERE id = ?")
                .bind(account_id)
                .fetch_optional(&mut *transaction)
                .await?;
        transaction.rollback().await?;

        let account_inactive = current_status.as_deref() != Some("active");
        let (reason, error) = if account_inactive {
            // Account was deleted, removed, or never existed at all —
            // unauthorized/session-ended semantics, not 409.
            (
                "account_not_active",
                AppError::Unauthorized("invalid username or password".to_owned()),
            )
        } else {
            // Account is still active — the hash was superseded by a concurrent
            // change. Keep current 409 + stale_credential.
            (
                "stale_credential",
                AppError::Conflict(PASSWORD_CHANGE_CONFLICT_MSG.to_owned()),
            )
        };
        audit_password_change_denial(pool, Some(account_id), reason).await;
        return Err(error);
    }
    let mut event = NewAuditEvent::simple(
        "account_password_changed",
        AuditOutcome::Success,
        AuditActor::Account,
        "Account password changed by the customer",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    get(pool, account_id).await
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database_error) if database_error.is_unique_violation()
    )
}

fn transfer_error_to_app(error: TransferError) -> AppError {
    match error {
        TransferError::InsufficientSeconds { .. } => {
            AppError::Conflict("insufficient seconds".to_owned())
        }
        TransferError::InsufficientPoints { .. } => {
            AppError::Conflict("insufficient points".to_owned())
        }
        other => AppError::InvalidInput(other.to_string()),
    }
}

fn generate_six_digit_code() -> String {
    let n: u32 = rand::random_range(0..1_000_000);
    format!("{n:06}")
}

/// Allocate a 6-digit code that is not currently in use by another `pending`
/// request. Codes are unique only among live requests (a partial unique
/// index enforces this), so resolved codes are free to recur. The bounded
/// retry handles the rare in-process clash; the partial unique index is the
/// hard backstop against a concurrent insert racing the count.
async fn generate_unique_pending_code(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> AppResult<String> {
    for _ in 0..16 {
        let candidate = generate_six_digit_code();
        let clash: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM points_transfer_requests
             WHERE code = ? AND status = 'pending'",
        )
        .bind(&candidate)
        .fetch_one(&mut **transaction)
        .await?;
        if clash == 0 {
            return Ok(candidate);
        }
    }
    Err(AppError::Conflict(
        "could not allocate a unique transfer code; please try again".to_owned(),
    ))
}

/// Render integer centipoints as a two-decimal points string for audit
/// summaries (`150` → `1.50`). Wire values stay integer centipoints
/// (ADR-0038); this is display-only text inside human-readable summaries.
fn format_points_centi(points_centi: i64) -> String {
    let sign = if points_centi < 0 { "-" } else { "" };
    let abs = points_centi.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

fn normalize_transfer_code(code: &str) -> AppResult<String> {
    let normalized = code.trim();
    if normalized.is_empty() {
        return Err(AppError::InvalidInput(
            "transfer code is required".to_owned(),
        ));
    }
    if normalized.len() != 6 || !normalized.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::InvalidInput(
            "transfer code must be 6 digits".to_owned(),
        ));
    }
    Ok(normalized.to_owned())
}

struct PendingRequest {
    id: String,
    from_account_id: String,
    to_account_id: String,
    points_centi: i64,
}

async fn load_pending_request(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    code: &str,
) -> AppResult<PendingRequest> {
    // The `expired` flag enforces the "time-boxed" guarantee on read: a
    // request older than the TTL is no longer confirmable even if the
    // background sweep has not yet marked it `expired` (ADR-0030).
    let row = sqlx::query_as::<_, (String, String, String, i64, String, bool)>(
        "SELECT id, from_account_id, to_account_id, points_centi, status,
                datetime(created_at) <= datetime('now', ?) AS expired
         FROM points_transfer_requests WHERE code = ?",
    )
    .bind(format!("-{POINTS_TRANSFER_TTL_SECONDS} seconds"))
    .bind(code)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((id, from_account_id, to_account_id, points_centi, status, expired)) = row else {
        return Err(AppError::NotFound("transfer code".to_owned()));
    };
    match PointsTransferStatus::from_str(&status) {
        Some(PointsTransferStatus::Pending) if expired => {
            Err(AppError::Conflict("transfer code has expired".to_owned()))
        }
        Some(PointsTransferStatus::Pending) => Ok(PendingRequest {
            id,
            from_account_id,
            to_account_id,
            points_centi,
        }),
        Some(PointsTransferStatus::Confirmed) => Err(AppError::Conflict(
            "transfer code has already been confirmed".to_owned(),
        )),
        Some(PointsTransferStatus::Rejected) => Err(AppError::Conflict(
            "transfer code has already been rejected".to_owned(),
        )),
        Some(PointsTransferStatus::Expired) => {
            Err(AppError::Conflict("transfer code has expired".to_owned()))
        }
        None => Err(AppError::Conflict(
            "transfer code has unknown status".to_owned(),
        )),
    }
}

/// List the live (`pending`) Points Transfer Requests for the owner's
/// approvals view (`GET /web-admin/transfer-requests`, plan §6). Both
/// usernames are joined in so the owner can match the customer at the
/// counter. Resolved rows (confirmed/rejected/expired) are excluded; the
/// HTTP layer sweeps past-TTL rows to `expired` before calling this so a
/// stale request never lingers in the list.
pub async fn list_pending_points_transfer_requests(
    pool: &SqlitePool,
) -> AppResult<Vec<PendingPointsTransfer>> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, i64, String, String)>(
        "SELECT request.id,
                request.from_account_id, sender.username,
                request.to_account_id, recipient.username,
                request.points_centi, request.code, request.created_at
         FROM points_transfer_requests request
         JOIN accounts sender ON sender.id = request.from_account_id
         JOIN accounts recipient ON recipient.id = request.to_account_id
         WHERE request.status = 'pending'
         ORDER BY request.created_at ASC, request.id ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                from_account_id,
                from_username,
                to_account_id,
                to_username,
                points_centi,
                code,
                created_at,
            )| PendingPointsTransfer {
                id,
                from_account_id,
                from_username,
                to_account_id,
                to_username,
                points_centi,
                code,
                created_at,
            },
        )
        .collect())
}

async fn load_request_by_id(pool: &SqlitePool, id: &str) -> AppResult<PointsTransferRequest> {
    let row = sqlx::query_as::<_, (String, String, String, i64, String, String, Option<String>)>(
        "SELECT id, from_account_id, to_account_id, points_centi, code, status, resolved_at
         FROM points_transfer_requests WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("transfer request".to_owned()))?;
    let (request_id, from_account_id, to_account_id, points_centi, code, status, resolved_at) = row;
    let status = PointsTransferStatus::from_str(&status)
        .ok_or_else(|| AppError::Conflict("transfer code has unknown status".to_owned()))?;
    let created_at = sqlx::query_scalar::<_, String>(
        "SELECT created_at FROM points_transfer_requests WHERE id = ?",
    )
    .bind(&request_id)
    .fetch_one(pool)
    .await?;
    Ok(PointsTransferRequest {
        id: request_id,
        from_account_id,
        to_account_id,
        points_centi,
        code,
        status,
        created_at,
        resolved_at,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Barrier;

    use super::*;
    use crate::test_utils;

    /// Delegates to the shared in-memory test pool.
    async fn test_pool() -> SqlitePool {
        test_utils::mem_test_pool().await
    }

    /// Delegates to the shared WAL test pool.
    async fn wal_test_pool(name: &str) -> (SqlitePool, std::path::PathBuf) {
        test_utils::wal_test_pool(name).await
    }

    #[tokio::test]
    async fn account_password_is_hashed_and_login_is_audited() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "Juan_01",
            "secret-password",
            AccountCreator::SelfService,
            Some("pc-01"),
        )
        .await?;
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = ?")
                .bind(&account.id)
                .fetch_one(&pool)
                .await?;
        assert_ne!(stored_hash, "secret-password");

        let authenticated =
            authenticate(&pool, "juan_01", "secret-password", Some("pc-01")).await?;
        assert_eq!(authenticated.id, account.id);
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE account_id = ? AND event_type IN ('account_created', 'login_succeeded')",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(events, 2);
        Ok(())
    }

    #[tokio::test]
    async fn username_is_unique_case_insensitively_and_stays_retired() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(&pool, "Customer", "password", AccountCreator::Admin, None).await?;
        assert!(matches!(
            create(&pool, "customer", "password", AccountCreator::Admin, None).await,
            Err(AppError::Conflict(_))
        ));
        soft_delete(&pool, &account.id).await?;
        assert!(matches!(
            create(&pool, "CUSTOMER", "password", AccountCreator::Admin, None).await,
            Err(AppError::Conflict(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn deleted_account_cannot_login_and_history_is_preserved() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "history-user",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        sqlx::query(
            "UPDATE accounts SET remaining_seconds = 600, points_balance_centi = 350 WHERE id = ?",
        )
        .bind(&account.id)
        .execute(&pool)
        .await?;
        let deleted = soft_delete(&pool, &account.id).await?;
        assert_eq!(deleted.status, "deleted");
        assert_eq!(deleted.remaining_seconds, 0);
        assert_eq!(deleted.points_balance_centi, 0);
        // Forfeiture audits the exact fractional balance (3.50 points).
        let forfeited: (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT seconds_delta, points_delta_centi FROM audit_log
             WHERE event_type = 'account_deleted' AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(forfeited, (Some(-600), Some(-350)));
        assert!(matches!(
            authenticate(&pool, "history-user", "password", Some("pc-01")).await,
            Err(AppError::Unauthorized(_))
        ));
        let historical_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE account_id = ?")
                .bind(&account.id)
                .fetch_one(&pool)
                .await?;
        assert!(historical_rows >= 3);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_delete_is_atomic_and_audits_actual_forfeitures() -> AppResult<()> {
        let pool = test_pool().await;
        let first = funded_account(&pool, "bulk-first", 600, 350).await?;
        let second = funded_account(&pool, "bulk-second", 90, 7).await?;
        let deleted = soft_delete_many(&pool, &[first.id.clone(), second.id.clone()]).await?;

        assert_eq!(deleted.len(), 2);
        assert!(deleted.iter().all(|account| account.status == "deleted"));
        let audits: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT account_id, seconds_delta, points_delta_centi
             FROM audit_log WHERE event_type = 'account_deleted' ORDER BY created_at, id",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(audits.len(), 2);
        assert!(audits.contains(&(first.id, Some(-600), Some(-350))));
        assert!(audits.contains(&(second.id, Some(-90), Some(-7))));
        Ok(())
    }

    #[tokio::test]
    async fn oversized_bulk_delete_records_bounded_failure_metadata() -> AppResult<()> {
        let pool = test_pool().await;
        let account_ids = (0..1_001)
            .map(|index| format!("account-{index}"))
            .collect::<Vec<_>>();

        assert!(matches!(
            soft_delete_many(&pool, &account_ids).await,
            Err(AppError::InvalidInput(_))
        ));
        let metadata: String = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_log
             WHERE event_type = 'account_delete_failed'",
        )
        .fetch_one(&pool)
        .await?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata).expect("failure metadata should be JSON");
        assert_eq!(metadata["requested_count"], 1_001);
        assert_eq!(
            metadata["requested_account_ids_sample"]
                .as_array()
                .map(Vec::len),
            Some(32)
        );
        assert_eq!(metadata["sample_truncated"], true);
        assert!(!metadata.to_string().contains("account-1000"));
        Ok(())
    }

    #[tokio::test]
    async fn bulk_delete_stale_selection_changes_nothing_and_audits_once() -> AppResult<()> {
        let pool = test_pool().await;
        let active = funded_account(&pool, "bulk-active", 600, 350).await?;
        let deleted = create(
            &pool,
            "bulk-deleted",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        soft_delete(&pool, &deleted.id).await?;
        let result = soft_delete_many(&pool, &[active.id.clone(), deleted.id.clone()]).await;

        assert!(matches!(result, Err(AppError::NotFound(_))));
        let active_state: (String, i64, i64) = sqlx::query_as(
            "SELECT status, remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&active.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(active_state, ("active".to_owned(), 600, 350));
        let failed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_delete_failed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(failed, 1);
        Ok(())
    }

    #[tokio::test]
    async fn login_resolves_only_the_active_identity_when_history_shares_username() -> AppResult<()>
    {
        let pool = test_pool().await;
        let deleted = create(
            &pool,
            "shared-history",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        soft_delete(&pool, &deleted.id).await?;
        let active_id = "active-shared-history";
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash)
             VALUES (?, ?, ?, ?)",
        )
        .bind(active_id)
        .bind("Shared-History")
        .bind("shared-history")
        .bind(hash_password("new-password")?)
        .execute(&pool)
        .await?;

        assert!(matches!(
            authenticate(&pool, "shared-history", "old-password", None).await,
            Err(AppError::Unauthorized(_))
        ));
        let authenticated = authenticate(&pool, "shared-history", "new-password", None).await?;
        assert_eq!(authenticated.id, active_id);
        Ok(())
    }

    #[tokio::test]
    async fn reset_changes_credentials_without_exposing_password() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "reset-user",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        reset_password(&pool, &account.id, "new-password").await?;
        assert!(matches!(
            authenticate(&pool, "reset-user", "old-password", None).await,
            Err(AppError::Unauthorized(_))
        ));
        authenticate(&pool, "reset-user", "new-password", None).await?;
        let reset_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE account_id = ? AND event_type = 'account_password_reset'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(reset_events, 1);
        Ok(())
    }

    async fn assert_transfer_untouched(
        pool: &SqlitePool,
        sender_id: &str,
        sender_seconds: i64,
        recipient_username: &str,
        recipient_seconds: i64,
    ) -> AppResult<()> {
        let sender: i64 = sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
            .bind(sender_id)
            .fetch_one(pool)
            .await?;
        let recipient: i64 = sqlx::query_scalar(
            "SELECT remaining_seconds FROM accounts WHERE username_normalized = ?",
        )
        .bind(recipient_username)
        .fetch_one(pool)
        .await?;
        let details: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_transfers")
            .fetch_one(pool)
            .await?;
        assert_eq!(
            (sender, recipient, details),
            (sender_seconds, recipient_seconds, 0)
        );
        Ok(())
    }

    async fn funded_account(
        pool: &SqlitePool,
        username: &str,
        seconds: i64,
        points: i64,
    ) -> AppResult<Account> {
        let account = create(pool, username, "password", AccountCreator::Admin, None).await?;
        sqlx::query(
            "UPDATE accounts
             SET remaining_seconds = ?, points_balance_centi = ?
             WHERE id = ?",
        )
        .bind(seconds)
        .bind(points)
        .bind(&account.id)
        .execute(pool)
        .await?;
        Ok(Account {
            remaining_seconds: seconds,
            points_balance_centi: points,
            ..account
        })
    }

    #[tokio::test]
    async fn adjust_time_and_points_apply_signed_deltas_and_audit() -> AppResult<()> {
        let pool = test_pool().await;
        let account = funded_account(&pool, "adjust-user", 300, 5).await?;

        let after_add = adjust_time(&pool, &account.id, 120).await?;
        assert_eq!(after_add.remaining_seconds, 420);
        let after_sub = adjust_points(&pool, &account.id, -2).await?;
        assert_eq!(after_sub.points_balance_centi, 3);

        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE account_id = ?
               AND event_type IN ('account_time_adjusted', 'account_points_adjusted')",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(events, 2);
        Ok(())
    }

    #[tokio::test]
    async fn adjust_clamps_at_zero_and_records_applied_delta() -> AppResult<()> {
        let pool = test_pool().await;
        let account = funded_account(&pool, "clamp-user", 60, 1).await?;

        // Over-deduct: balance floors at zero, not negative.
        let time = adjust_time(&pool, &account.id, -500).await?;
        assert_eq!(time.remaining_seconds, 0);
        let points = adjust_points(&pool, &account.id, -9).await?;
        assert_eq!(points.points_balance_centi, 0);

        // The audit event records the actually-applied delta (-60), not -500.
        let applied: Option<i64> = sqlx::query_scalar(
            "SELECT seconds_delta FROM audit_log WHERE event_type = 'account_time_adjusted'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(applied, Some(-60));
        Ok(())
    }

    #[tokio::test]
    async fn adjust_rejects_zero_and_inactive_account() -> AppResult<()> {
        let pool = test_pool().await;
        let account = funded_account(&pool, "reject-adjust", 60, 1).await?;
        assert!(matches!(
            adjust_time(&pool, &account.id, 0).await,
            Err(AppError::InvalidInput(_))
        ));
        soft_delete(&pool, &account.id).await?;
        assert!(matches!(
            adjust_points(&pool, &account.id, 5).await,
            Err(AppError::NotFound(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn transfer_time_moves_seconds_atomically_and_writes_audit_pair() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "time-sender", 600, 5).await?;
        let recipient = funded_account(&pool, "time-recipient", 60, 0).await?;

        let updated = transfer_time(&pool, &sender.id, "time-recipient", 300).await?;
        assert_eq!(updated.sender.remaining_seconds, 300);
        assert_eq!(updated.sender.points_balance_centi, 5);
        assert_eq!(updated.recipient_id, recipient.id);
        assert_eq!(updated.recipient_remaining_seconds, 360);
        let recipient_after: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&recipient.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(recipient_after, (360, 0));
        // One detail row + two audit events, no transfer of points.
        let detail: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_transfers")
            .fetch_one(&pool)
            .await?;
        assert_eq!(detail, 1);
        let out_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_transfer_out'",
        )
        .fetch_one(&pool)
        .await?;
        let in_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_transfer_in'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(out_count, 1);
        assert_eq!(in_count, 1);
        let (seconds_delta, points_delta_centi): (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT seconds_delta, points_delta_centi FROM audit_log WHERE event_type = 'account_transfer_out'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(seconds_delta, Some(-300));
        assert_eq!(points_delta_centi, None);
        let outgoing_summary: String = sqlx::query_scalar(
            "SELECT summary FROM audit_log
             WHERE event_type = 'account_transfer_out'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(outgoing_summary, "Sent 5min 0sec to time-recipient");
        let incoming_summary: String = sqlx::query_scalar(
            "SELECT summary FROM audit_log
             WHERE event_type = 'account_transfer_in'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(incoming_summary, "Received 5min 0sec from time-sender");
        let incoming_actor: Option<String> = sqlx::query_scalar(
            "SELECT actor_account_id FROM audit_log
             WHERE event_type = 'account_transfer_in'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(incoming_actor.as_deref(), Some(sender.id.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn transfer_time_rejects_self_transfer_and_unknown_recipient() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "self-sender", 600, 0).await?;
        assert!(matches!(
            transfer_time(&pool, &sender.id, "self-sender", 60).await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            transfer_time(&pool, &sender.id, "no-such-user", 60).await,
            Err(AppError::NotFound(_))
        ));
        // No transfer must have moved any balance or written a detail row.
        let sender_remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM accounts WHERE id = ?")
                .bind(&sender.id)
                .fetch_one(&pool)
                .await?;
        let details: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_transfers")
            .fetch_one(&pool)
            .await?;
        assert_eq!(sender_remaining, 600);
        assert_eq!(details, 0);
        Ok(())
    }

    #[tokio::test]
    async fn transfer_time_rejects_zero_seconds_as_invalid_input() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "zero-sender", 600, 0).await?;
        let _recipient = funded_account(&pool, "zero-recipient", 5, 0).await?;
        let result = transfer_time(&pool, &sender.id, "zero-recipient", 0).await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        assert_transfer_untouched(&pool, &sender.id, 600, "zero-recipient", 5).await
    }

    #[tokio::test]
    async fn transfer_time_allows_self_drain_to_zero() -> AppResult<()> {
        // Normal expiry follows; the customer may hand off every second
        // they have left (ADR-0022).
        let pool = test_pool().await;
        let sender = funded_account(&pool, "drain-sender", 120, 0).await?;
        let _recipient = funded_account(&pool, "drain-recipient", 0, 0).await?;
        let updated = transfer_time(&pool, &sender.id, "drain-recipient", 120).await?;
        assert_eq!(updated.sender.remaining_seconds, 0);
        Ok(())
    }

    #[tokio::test]
    async fn transfer_time_rejects_positive_amount_above_balance() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "overdraw-sender", 60, 0).await?;
        let _recipient = funded_account(&pool, "overdraw-recipient", 5, 0).await?;
        let result = transfer_time(&pool, &sender.id, "overdraw-recipient", 100).await;
        assert!(matches!(result, Err(AppError::Conflict(_))));
        assert_transfer_untouched(&pool, &sender.id, 60, "overdraw-recipient", 5).await
    }

    #[tokio::test]
    async fn request_points_transfer_creates_pending_row_and_audit_event() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "pt-sender", 0, 12).await?;
        let _recipient = funded_account(&pool, "pt-recipient", 0, 0).await?;
        let created = request_points_transfer(&pool, &sender.id, "pt-recipient", 5).await?;
        assert_eq!(created.code.len(), 6);
        assert!(created.code.bytes().all(|byte| byte.is_ascii_digit()));
        let request: (String, i64, String) = sqlx::query_as(
            "SELECT code, points_centi, status FROM points_transfer_requests WHERE id = ?",
        )
        .bind(&created.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(request, (created.code.clone(), 5, "pending".to_owned()));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'points_transfer_requested'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        // Sender balance is unchanged until the owner confirms.
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(sender_points, 12);
        Ok(())
    }

    #[tokio::test]
    async fn request_points_transfer_rejects_self_and_unknown_recipient() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "pt-self", 0, 5).await?;
        assert!(matches!(
            request_points_transfer(&pool, &sender.id, "pt-self", 1).await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            request_points_transfer(&pool, &sender.id, "pt-missing", 1).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            request_points_transfer(&pool, &sender.id, "pt-self", 0).await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn confirm_points_transfer_moves_points_and_writes_audit_pair() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "confirm-sender", 0, 10).await?;
        let recipient = funded_account(&pool, "confirm-recipient", 0, 2).await?;
        let created = request_points_transfer(&pool, &sender.id, "confirm-recipient", 6).await?;

        let resolved = confirm_points_transfer(&pool, &created.code).await?;
        assert_eq!(resolved.status, PointsTransferStatus::Confirmed);
        assert!(resolved.resolved_at.is_some());
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender.id)
                .fetch_one(&pool)
                .await?;
        let recipient_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&recipient.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!((sender_points, recipient_points), (4, 8));
        let out_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'points_transfer_out'",
        )
        .fetch_one(&pool)
        .await?;
        let in_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'points_transfer_in'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(out_count, 1);
        assert_eq!(in_count, 1);
        let incoming_actor: Option<String> = sqlx::query_scalar(
            "SELECT actor_account_id FROM audit_log
             WHERE event_type = 'points_transfer_in'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(incoming_actor.as_deref(), Some(sender.id.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn confirm_points_transfer_rejects_reused_or_unknown_codes() -> AppResult<()> {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "reuse-sender", 0, 5).await?;
        let _recipient = funded_account(&pool, "reuse-recipient", 0, 0).await?;
        let created = request_points_transfer(&pool, &sender.id, "reuse-recipient", 2).await?;
        confirm_points_transfer(&pool, &created.code).await?;
        // A second confirm must refuse — the code is single-use.
        assert!(matches!(
            confirm_points_transfer(&pool, &created.code).await,
            Err(AppError::Conflict(_))
        ));
        // Unknown code is a 404.
        assert!(matches!(
            confirm_points_transfer(&pool, "000000").await,
            Err(AppError::NotFound(_))
        ));
        // Malformed code is rejected at the boundary.
        assert!(matches!(
            confirm_points_transfer(&pool, "abc").await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn confirm_points_transfer_re_validates_points_at_confirm_time() -> AppResult<()> {
        // The customer spends down the points between request and confirm;
        // the re-validation in confirm_points_transfer must reject the
        // request and leave balances unchanged.
        let pool = test_pool().await;
        let sender = funded_account(&pool, "revalidate-sender", 0, 5).await?;
        let recipient = funded_account(&pool, "revalidate-recipient", 0, 0).await?;
        let created = request_points_transfer(&pool, &sender.id, "revalidate-recipient", 4).await?;
        // Simulate a points spend in the interim.
        sqlx::query("UPDATE accounts SET points_balance_centi = 1 WHERE id = ?")
            .bind(&sender.id)
            .execute(&pool)
            .await?;
        // The first confirm must reject because the sender no longer
        // has the requested 4 points; the rolled-back transaction leaves
        // the request still `pending` so a second confirm attempt will
        // see the same `pending` state and the same under-balance check.
        assert!(matches!(
            confirm_points_transfer(&pool, &created.code).await,
            Err(AppError::Conflict(_))
        ));
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(sender_points, 1);
        assert_eq!(recipient.points_balance_centi, 0);
        // The request row remains `pending` so the sweep can pick it up
        // on the next pass.
        let status: String =
            sqlx::query_scalar("SELECT status FROM points_transfer_requests WHERE id = ?")
                .bind(&created.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(status, "pending");
        Ok(())
    }

    #[tokio::test]
    async fn confirm_and_reject_refuse_codes_past_ttl_on_read() -> AppResult<()> {
        // A request older than the TTL is no longer confirmable or
        // rejectable, even before the background sweep marks it `expired`
        // (ADR-0030 "time-boxed" guarantee enforced on read).
        let pool = test_pool().await;
        let sender = funded_account(&pool, "ttl-sender", 0, 10).await?;
        let _recipient = funded_account(&pool, "ttl-recipient", 0, 0).await?;
        let created = request_points_transfer(&pool, &sender.id, "ttl-recipient", 4).await?;
        // Age the request past the TTL.
        sqlx::query(
            "UPDATE points_transfer_requests SET created_at = datetime('now', ?) WHERE id = ?",
        )
        .bind(format!("-{} seconds", POINTS_TRANSFER_TTL_SECONDS + 60))
        .bind(&created.id)
        .execute(&pool)
        .await?;
        assert!(matches!(
            confirm_points_transfer(&pool, &created.code).await,
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            reject_points_transfer(&pool, &created.code).await,
            Err(AppError::Conflict(_))
        ));
        // No points moved; the row stays `pending` for the sweep to expire.
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(sender_points, 10);
        let status: String =
            sqlx::query_scalar("SELECT status FROM points_transfer_requests WHERE id = ?")
                .bind(&created.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(status, "pending");
        Ok(())
    }

    #[tokio::test]
    async fn pending_code_uniqueness_is_scoped_to_live_requests() -> AppResult<()> {
        // A code is reusable once its prior request is resolved: the partial
        // unique index only constrains `pending` rows. Confirm one request,
        // then prove the same code value can be re-seeded as a new pending
        // request without tripping the unique index.
        let pool = test_pool().await;
        let sender = funded_account(&pool, "reuse-code-sender", 0, 10).await?;
        let recipient = funded_account(&pool, "reuse-code-recipient", 0, 0).await?;
        let created = request_points_transfer(&pool, &sender.id, "reuse-code-recipient", 2).await?;
        confirm_points_transfer(&pool, &created.code).await?;

        // Re-seed a fresh pending request carrying the now-resolved code.
        let event_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, summary, metadata_json)
             VALUES (?, 'points_transfer_requested', 'success', 'account', 'reuse', '{}')",
        )
        .bind(&event_id)
        .execute(&pool)
        .await?;
        let reused = sqlx::query(
            "INSERT INTO points_transfer_requests
               (id, from_account_id, to_account_id, points_centi, code, status, audit_event_id)
             VALUES (?, ?, ?, 1, ?, 'pending', ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&sender.id)
        .bind(&recipient.id)
        .bind(&created.code)
        .bind(&event_id)
        .execute(&pool)
        .await;
        assert!(reused.is_ok(), "resolved code must be reusable: {reused:?}");
        Ok(())
    }

    #[tokio::test]
    async fn reject_points_transfer_marks_request_rejected_without_moving_points() -> AppResult<()>
    {
        let pool = test_pool().await;
        let sender = funded_account(&pool, "reject-sender", 0, 5).await?;
        let recipient = funded_account(&pool, "reject-recipient", 0, 0).await?;
        let created = request_points_transfer(&pool, &sender.id, "reject-recipient", 3).await?;
        let resolved = reject_points_transfer(&pool, &created.code).await?;
        assert_eq!(resolved.status, PointsTransferStatus::Rejected);
        let sender_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&sender.id)
                .fetch_one(&pool)
                .await?;
        let recipient_points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&recipient.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!((sender_points, recipient_points), (5, 0));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'points_transfer_rejected'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn change_password_rotates_hash_and_keeps_session_token_valid() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "self-rotator",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        let new_account =
            change_password(&pool, &account.id, "old-password", "new-password").await?;
        assert_eq!(new_account.id, account.id);
        // The stored hash rotated; the plaintext never appears.
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = ?")
                .bind(&account.id)
                .fetch_one(&pool)
                .await?;
        assert_ne!(stored_hash, "old-password");
        assert_ne!(stored_hash, "new-password");
        // The customer can authenticate with the new password; the old
        // one is rejected.
        assert!(matches!(
            authenticate(&pool, "self-rotator", "old-password", None).await,
            Err(AppError::Unauthorized(_))
        ));
        let reauthed = authenticate(&pool, "self-rotator", "new-password", None).await?;
        assert_eq!(reauthed.id, account.id);
        // Audit: a single `account_password_changed` event (distinct from
        // the admin `account_password_reset`).
        let changed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_password_changed' AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        let reset: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_password_reset' AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(changed, 1);
        assert_eq!(reset, 0);
        Ok(())
    }

    async fn prepare_password_change(
        pool: &SqlitePool,
        account_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> AppResult<(String, String)> {
        let password_hash: String = sqlx::query_scalar(
            "SELECT password_hash FROM accounts WHERE id = ? AND status = 'active'",
        )
        .bind(account_id)
        .fetch_one(pool)
        .await?;
        assert!(verify_password(old_password, &password_hash));
        Ok((password_hash, hash_password(new_password)?))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overlapping_password_changes_are_first_writer_wins() -> AppResult<()> {
        let (pool, path) = wal_test_pool("password-race").await;
        let account = create(&pool, "racer", "old-password", AccountCreator::Admin, None).await?;

        // Prepare both requests through credential verification and hashing,
        // then release both conditional writes together. Production remains
        // hook-free; this test coordinates at the extracted write phase.
        let first_hashes =
            prepare_password_change(&pool, &account.id, "old-password", "first-password").await?;
        let second_hashes =
            prepare_password_change(&pool, &account.id, "old-password", "second-password").await?;
        assert_eq!(first_hashes.0, second_hashes.0);

        let barrier = Arc::new(Barrier::new(2));
        let run = |hashes: (String, String), barrier: Arc<Barrier>| {
            let pool = pool.clone();
            let account_id = account.id.clone();
            async move {
                barrier.wait().await;
                replace_verified_password(&pool, &account_id, &hashes.0, &hashes.1).await
            }
        };
        let (first, second) = tokio::join!(
            run(first_hashes, Arc::clone(&barrier)),
            run(second_hashes, barrier)
        );

        assert_eq!(
            usize::from(first.is_ok()) + usize::from(second.is_ok()),
            1,
            "unexpected outcomes: first={first:?}, second={second:?}"
        );
        assert_eq!(
            usize::from(matches!(first, Err(AppError::Conflict(_))))
                + usize::from(matches!(second, Err(AppError::Conflict(_)))),
            1,
            "unexpected outcomes: first={first:?}, second={second:?}"
        );

        let first_works = authenticate(&pool, "racer", "first-password", None)
            .await
            .is_ok();
        let second_works = authenticate(&pool, "racer", "second-password", None)
            .await
            .is_ok();
        assert_ne!(first_works, second_works);
        assert!(authenticate(&pool, "racer", "old-password", None)
            .await
            .is_err());
        let changed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_password_changed'",
        )
        .fetch_one(&pool)
        .await?;
        let stale: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_password_change_failed'
               AND metadata_json LIKE '%stale_credential%'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!((changed, stale), (1, 1));
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    /// Production-like overlap between a full Password Change (through the
    /// real `change_password` entry point, Argon2 included) and an unrelated
    /// authority write representative of the per-second Active Session timer
    /// (ADR-0037). The change must complete without `SQLITE_BUSY_SNAPSHOT`
    /// (code 517) or any raw database error, and the timer-like writer must
    /// keep committing while Argon2 runs — password computation never
    /// reserves the writer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn password_change_succeeds_alongside_timer_like_writes() -> AppResult<()> {
        let (pool, path) = wal_test_pool("password-timer-overlap").await;
        let account = create(
            &pool,
            "timer-overlap",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        sqlx::query("UPDATE pcs SET remaining_seconds = 1000000 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let pool = pool.clone();
            let stop = Arc::clone(&stop);
            tokio::spawn(async move {
                let mut writes = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    sqlx::query(
                        "UPDATE pcs SET remaining_seconds = remaining_seconds - 1
                         WHERE id = 'pc-01'",
                    )
                    .execute(&pool)
                    .await?;
                    writes += 1;
                }
                Ok::<u32, sqlx::Error>(writes)
            })
        };

        let changed = change_password(&pool, &account.id, "old-password", "new-password").await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let writes = writer
            .await
            .expect("timer-like writer task should not panic")?;

        changed?;
        assert!(
            writes > 0,
            "timer-like writer must keep committing during Password Change"
        );
        authenticate(&pool, "timer-overlap", "new-password", None).await?;
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn password_change_does_not_reactivate_an_account_inactivated_after_verification(
    ) -> AppResult<()> {
        let (pool, path) = wal_test_pool("password-inactive").await;
        let account = create(
            &pool,
            "inactivated-race",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;

        // Complete verification first, commit removal, then attempt the
        // conditional write with the exact hash that was verified.
        let (verified_hash, new_hash) =
            prepare_password_change(&pool, &account.id, "old-password", "new-password").await?;
        sqlx::query("UPDATE accounts SET status = 'deleted' WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;
        let result = replace_verified_password(&pool, &account.id, &verified_hash, &new_hash).await;
        assert!(matches!(result, Err(AppError::Unauthorized(_))));

        let (status, password_hash): (String, String) =
            sqlx::query_as("SELECT status, password_hash FROM accounts WHERE id = ?")
                .bind(&account.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(status, "deleted");

        assert!(verify_password("old-password", &password_hash));
        assert!(!verify_password("new-password", &password_hash));

        // The failed audit event (if any) must not use reason stale_credential.
        let stale_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_password_change_failed'
               AND metadata_json LIKE '%stale_credential%'
               AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            stale_count, 0,
            "no stale_credential audit for inactive account"
        );
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn change_password_for_missing_account_is_generic_and_audited() -> AppResult<()> {
        let pool = test_pool().await;
        let account_id = "removed-account";

        let error = change_password(&pool, account_id, "old-password", "new-password")
            .await
            .expect_err("missing account must be unauthorized");
        assert!(matches!(
            error,
            AppError::Unauthorized(ref message) if message == "invalid username or password"
        ));

        let denial: (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(metadata_json), '') FROM audit_log
             WHERE event_type = 'account_password_change_failed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(denial.0, 1);
        assert!(denial.1.contains("account_not_active"));
        Ok(())
    }

    #[tokio::test]
    async fn change_password_on_already_inactive_account_returns_unauthorized() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "already-inactive",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        soft_delete(&pool, &account.id).await?;
        assert!(matches!(
            change_password(&pool, &account.id, "old-password", "new-password").await,
            Err(AppError::Unauthorized(_))
        ));
        // The failed audit event must not use reason stale_credential.
        let stale_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'account_password_change_failed'
               AND metadata_json LIKE '%stale_credential%'
               AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            stale_count, 0,
            "no stale_credential audit for already-inactive account"
        );
        // Account remains deleted, not modified.
        let (status, password_hash): (String, String) =
            sqlx::query_as("SELECT status, password_hash FROM accounts WHERE id = ?")
                .bind(&account.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(status, "deleted");
        assert!(verify_password("old-password", &password_hash));
        Ok(())
    }

    #[tokio::test]
    async fn change_password_rejects_wrong_old_password_with_audited_denial() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "wrong-old",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        assert!(matches!(
            change_password(&pool, &account.id, "WRONG", "new-password").await,
            Err(AppError::Unauthorized(_))
        ));
        // The denial is audited under a distinct event type so a future
        // fraud signal can count it without mixing with admin resets.
        let denials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'account_password_change_failed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(denials, 1);
        // The old password still works because the change did not commit.
        authenticate(&pool, "wrong-old", "old-password", None).await?;
        Ok(())
    }

    #[tokio::test]
    async fn change_password_enforces_signup_length_rule() -> AppResult<()> {
        let pool = test_pool().await;
        let account = create(
            &pool,
            "short-new",
            "old-password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        // The validator reuses the same `auth::validate_password` rule as
        // signup (4–128 chars).
        assert!(matches!(
            change_password(&pool, &account.id, "old-password", "ab").await,
            Err(AppError::InvalidInput(_))
        ));
        let too_long = "x".repeat(129);
        assert!(matches!(
            change_password(&pool, &account.id, "old-password", &too_long).await,
            Err(AppError::InvalidInput(_))
        ));
        // Old password still works.
        authenticate(&pool, "short-new", "old-password", None).await?;
        Ok(())
    }

    #[tokio::test]
    async fn bcrypt_login_upgrades_to_argon2_and_still_authenticates() -> AppResult<()> {
        let pool = test_pool().await;
        let account_id = Uuid::new_v4().to_string();
        let bcrypt_hash = bcrypt::hash("old-bcrypt-pw", 4).expect("bcrypt hash should succeed");
        assert!(is_bcrypt_hash(&bcrypt_hash));
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash, status)
             VALUES (?, ?, ?, ?, 'active')",
        )
        .bind(&account_id)
        .bind("bcrypt-user")
        .bind("bcrypt-user")
        .bind(&bcrypt_hash)
        .execute(&pool)
        .await?;

        // Login with the correct password — succeeds and upgrades.
        let account = authenticate(&pool, "bcrypt-user", "old-bcrypt-pw", None).await?;
        assert_eq!(account.id, account_id);
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        assert!(
            !is_bcrypt_hash(&stored_hash),
            "credential should have been upgraded to Argon2"
        );
        assert!(
            stored_hash.starts_with("$argon2"),
            "stored hash should be Argon2 format, got: {stored_hash}"
        );

        // The same password still authenticates on the next login.
        let again = authenticate(&pool, "bcrypt-user", "old-bcrypt-pw", None).await?;
        assert_eq!(again.id, account_id);

        // A wrong password is rejected with unchanged semantics.
        assert!(matches!(
            authenticate(&pool, "bcrypt-user", "wrong-password", None).await,
            Err(AppError::Unauthorized(_))
        ));

        // The credential_upgraded audit event exists.
        let upgraded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'credential_upgraded' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(upgraded, 1, "credential_upgraded audit event missing");
        Ok(())
    }

    #[tokio::test]
    async fn bcrypt_upgrade_does_not_overwrite_concurrent_password_change() -> AppResult<()> {
        let pool = test_pool().await;
        let account_id = Uuid::new_v4().to_string();
        let bcrypt_hash = bcrypt::hash("shared-password", 4).expect("bcrypt hash should succeed");
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash, status)
             VALUES (?, ?, ?, ?, 'active')",
        )
        .bind(&account_id)
        .bind("bcrypt-race")
        .bind("bcrypt-race")
        .bind(&bcrypt_hash)
        .execute(&pool)
        .await?;

        // Simulate a concurrent password change that replaces the hash.
        let new_hash = hash_password("new-admin-pw")?;
        sqlx::query("UPDATE accounts SET password_hash = ? WHERE id = ?")
            .bind(&new_hash)
            .bind(&account_id)
            .execute(&pool)
            .await?;

        // The upgrade_credential uses compare-and-swap on the old bcrypt hash.
        // Since the hash changed, it becomes a safe no-op.
        upgrade_credential(&pool, &account_id, "shared-password", &bcrypt_hash).await?;

        // The new password must still work after the no-op upgrade attempt.
        let account = authenticate(&pool, "bcrypt-race", "new-admin-pw", None).await?;
        assert_eq!(account.id, account_id);
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(stored_hash, new_hash, "hash must not have been reverted");

        // No credential_upgraded audit event was written.
        let upgraded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'credential_upgraded' AND account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(upgraded, 0, "no upgrade event expected after race loss");
        Ok(())
    }

    #[tokio::test]
    async fn password_change_on_bcrypt_account_replaces_hash_directly() -> AppResult<()> {
        let pool = test_pool().await;
        let account_id = Uuid::new_v4().to_string();
        let bcrypt_hash = bcrypt::hash("original-pw", 4).expect("bcrypt hash should succeed");
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash, status)
             VALUES (?, ?, ?, ?, 'active')",
        )
        .bind(&account_id)
        .bind("bcrypt-change")
        .bind("bcrypt-change")
        .bind(&bcrypt_hash)
        .execute(&pool)
        .await?;

        // Password Change works on an imported Account.
        let account = change_password(&pool, &account_id, "original-pw", "new-changed-pw").await?;
        assert_eq!(account.id, account_id);

        // Login succeeds with the new password.
        authenticate(&pool, "bcrypt-change", "new-changed-pw", None).await?;
        // The old password is rejected.
        assert!(matches!(
            authenticate(&pool, "bcrypt-change", "original-pw", None).await,
            Err(AppError::Unauthorized(_))
        ));
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await?;
        assert!(
            stored_hash.starts_with("$argon2"),
            "after password change, hash should be Argon2"
        );
        Ok(())
    }
}

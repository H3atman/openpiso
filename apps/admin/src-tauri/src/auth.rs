use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

/// An Owner Session ends after this long without human input (ADR-0074).
const OWNER_IDLE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// An Owner Session ends this long after issue, whatever its activity. Also
/// the `Max-Age` of its cookie.
pub const OWNER_ABSOLUTE_LIMIT: Duration = Duration::from_secs(12 * 60 * 60);

/// A freshly issued Owner Session: the opaque token that goes into the
/// HttpOnly cookie, and the CSRF token the browser keeps in memory.
pub struct OwnerGrant {
    pub token: String,
    pub csrf: String,
}

struct OwnerSessionRecord {
    csrf: String,
    issued_at: Instant,
    last_activity: Instant,
}

impl OwnerSessionRecord {
    fn is_live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.issued_at) < OWNER_ABSOLUTE_LIMIT
            && now.saturating_duration_since(self.last_activity) < OWNER_IDLE_LIMIT
    }
}

/// In-memory Owner Sessions (ADR-0074): issuance, validation, human activity,
/// expiry, and revocation. Admin restart invalidates every session.
#[derive(Clone)]
pub struct OwnerTokens {
    sessions: Arc<RwLock<HashMap<String, OwnerSessionRecord>>>,
    credential_change: Arc<Mutex<()>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl Default for OwnerTokens {
    fn default() -> Self {
        Self {
            sessions: Arc::default(),
            credential_change: Arc::default(),
            clock: Arc::new(Instant::now),
        }
    }
}

impl OwnerTokens {
    /// Build a store whose idle/absolute expiry follows `clock`, so tests
    /// control time without editing session records.
    #[cfg(test)]
    pub fn with_clock(clock: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            clock,
            ..Self::default()
        }
    }

    /// Serialize login token issuance with credential rotation and revocation.
    pub async fn credential_guard(&self) -> OwnedMutexGuard<()> {
        self.credential_change.clone().lock_owned().await
    }

    pub async fn issue(&self) -> OwnerGrant {
        let now = (self.clock)();
        let grant = OwnerGrant {
            token: Uuid::new_v4().to_string(),
            csrf: Uuid::new_v4().to_string(),
        };
        let mut sessions = self.sessions.write().await;
        sessions.retain(|_, record| record.is_live(now));
        sessions.insert(
            grant.token.clone(),
            OwnerSessionRecord {
                csrf: grant.csrf.clone(),
                issued_at: now,
                last_activity: now,
            },
        );
        grant
    }

    /// The CSRF token of a live Owner Session, or `None` when the token is
    /// unknown, revoked, or expired. Reads never count as activity.
    pub async fn csrf_for(&self, token: &str) -> Option<String> {
        let now = (self.clock)();
        let mut sessions = self.sessions.write().await;
        let record = sessions.get(token)?;
        if record.is_live(now) {
            return Some(record.csrf.clone());
        }
        sessions.remove(token);
        None
    }

    /// Record explicit human input (pointer, keyboard, touch) for a live
    /// session, restarting its idle window. The absolute limit is unaffected.
    pub async fn record_human_activity(&self, token: &str) {
        let now = (self.clock)();
        if let Some(record) = self.sessions.write().await.get_mut(token) {
            if record.is_live(now) {
                record.last_activity = now;
            }
        }
    }

    /// Revoke an owner session token (web-admin logout, ADR-0024). Idempotent:
    /// revoking an unknown token is a no-op.
    pub async fn revoke(&self, token: &str) {
        self.sessions.write().await.remove(token);
    }

    /// Revoke every owner session except the caller after credential rotation.
    pub async fn revoke_all_except(&self, retained_token: &str) {
        self.sessions
            .write()
            .await
            .retain(|token, _| token == retained_token);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountToken {
    pub account_id: String,
    pub pc_id: String,
    pub session_id: String,
}

#[derive(Clone, Default)]
pub struct AccountTokens(Arc<RwLock<HashMap<String, AccountToken>>>);

impl AccountTokens {
    pub async fn issue(&self, account_id: &str, pc_id: &str, session_id: &str) -> String {
        let token = Uuid::new_v4().to_string();
        let mut tokens = self.0.write().await;
        tokens.retain(|_, value| value.account_id != account_id);
        tokens.insert(
            token.clone(),
            AccountToken {
                account_id: account_id.to_owned(),
                pc_id: pc_id.to_owned(),
                session_id: session_id.to_owned(),
            },
        );
        token
    }

    pub async fn get(&self, token: &str) -> Option<AccountToken> {
        self.0.read().await.get(token).cloned()
    }

    pub async fn revoke(&self, token: &str) {
        self.0.write().await.remove(token);
    }

    pub async fn revoke_account(&self, account_id: &str) {
        self.0
            .write()
            .await
            .retain(|_, token| token.account_id != account_id);
    }
}

pub fn validate_password(password: &str) -> AppResult<()> {
    if !(4..=128).contains(&password.len()) {
        return Err(AppError::InvalidInput(
            "password must be between 4 and 128 characters".to_owned(),
        ));
    }
    Ok(())
}

pub fn hash_password(password: &str) -> AppResult<String> {
    validate_password(password)?;
    hash_password_unchecked(password)
}

/// Hash a password into an Argon2 string without length validation.
/// Used internally by `hash_password` and by `upgrade_credential` for
/// passwords already verified through bcrypt.
pub fn hash_password_unchecked(password: &str) -> AppResult<String> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| AppError::Password(error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AppError::Password(error.to_string()))
}

/// Check if a stored hash is a legacy bcrypt credential (transitional).
pub fn is_bcrypt_hash(encoded_hash: &str) -> bool {
    encoded_hash.starts_with("$2a$")
        || encoded_hash.starts_with("$2b$")
        || encoded_hash.starts_with("$2y$")
}

/// Verify a password against a stored hash. Supports Argon2 (native) and
/// bcrypt (legacy/transitional).
pub fn verify_password(password: &str, encoded_hash: &str) -> bool {
    if is_bcrypt_hash(encoded_hash) {
        bcrypt::verify(password, encoded_hash).unwrap_or(false)
    } else {
        PasswordHash::new(encoded_hash).ok().is_some_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_supports_bcrypt_legacy_hash() -> AppResult<()> {
        // A known bcrypt hash for "secret-password" at cost 4 (fast).
        let bcrypt_hash = bcrypt::hash("secret-password", 4).expect("bcrypt hash should succeed");
        assert!(is_bcrypt_hash(&bcrypt_hash));
        assert!(verify_password("secret-password", &bcrypt_hash));
        assert!(!verify_password("wrong-password", &bcrypt_hash));
        assert!(!is_bcrypt_hash("$argon2id$v=19$..."));
        Ok(())
    }

    #[test]
    fn password_hash_never_contains_plaintext_and_verifies() -> AppResult<()> {
        let hash = hash_password("secret-password")?;
        assert!(!hash.contains("secret-password"));
        assert!(verify_password("secret-password", &hash));
        assert!(!verify_password("wrong-password", &hash));
        Ok(())
    }
}

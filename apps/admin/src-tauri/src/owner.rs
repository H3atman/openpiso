use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    audit::{self, NewAuditEvent},
    auth::{hash_password, verify_password},
    errors::{AppError, AppResult},
};

pub async fn is_configured(pool: &SqlitePool) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM owner_credentials")
            .fetch_one(pool)
            .await?
            > 0,
    )
}

pub async fn setup(pool: &SqlitePool, pin: &str) -> AppResult<()> {
    let password_hash = hash_password(pin).map_err(|error| match error {
        AppError::InvalidInput(_) => {
            AppError::InvalidInput("owner PIN must be between 4 and 128 characters".to_owned())
        }
        other => other,
    })?;
    let client_pin_hash = crate::db::settings::hash_owner_pin(pin)?;
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let inserted = sqlx::query("INSERT INTO owner_credentials (id, password_hash) VALUES (1, ?)")
        .bind(password_hash)
        .execute(&mut *transaction)
        .await;
    if let Err(error) = inserted {
        transaction.rollback().await?;
        let mut event = NewAuditEvent::simple(
            "owner_setup_failed",
            AuditOutcome::Denied,
            AuditActor::Admin,
            "Owner credential setup denied",
        );
        event.metadata = json!({ "reason": "already_configured" });
        audit::append_standalone(pool, event).await?;
        if matches!(&error, sqlx::Error::Database(database_error) if database_error.is_unique_violation())
        {
            return Err(AppError::Conflict(
                "owner PIN is already configured".to_owned(),
            ));
        }
        return Err(error.into());
    }
    crate::db::settings::write_owner_pin(&mut transaction, &client_pin_hash).await?;
    audit::append(
        &mut transaction,
        NewAuditEvent::simple(
            "owner_configured",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Configured owner PIN",
        ),
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn rotate(pool: &SqlitePool, current_pin: &str, new_pin: &str) -> AppResult<()> {
    let password_hash = hash_password(new_pin).map_err(|error| match error {
        AppError::InvalidInput(_) => {
            AppError::InvalidInput("owner PIN must be between 4 and 128 characters".to_owned())
        }
        other => other,
    })?;
    let mut transaction = crate::db::begin_immediate(pool).await?;
    let stored_hash =
        sqlx::query_scalar::<_, String>("SELECT password_hash FROM owner_credentials WHERE id = 1")
            .fetch_optional(&mut *transaction)
            .await?;
    if !stored_hash
        .as_deref()
        .is_some_and(|hash| verify_password(current_pin, hash))
    {
        audit::append(
            &mut transaction,
            NewAuditEvent::simple(
                "owner_credential_change_denied",
                AuditOutcome::Denied,
                AuditActor::Admin,
                "Web Admin PIN change denied",
            ),
        )
        .await?;
        transaction.commit().await?;
        return Err(AppError::Unauthorized("invalid owner PIN".to_owned()));
    }

    sqlx::query("UPDATE owner_credentials SET password_hash = ? WHERE id = 1")
        .bind(password_hash)
        .execute(&mut *transaction)
        .await?;
    audit::append(
        &mut transaction,
        NewAuditEvent::simple(
            "owner_credential_changed",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Changed Web Admin PIN",
        ),
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Verify `pin` against the stored owner credential **without auditing**.
///
/// [`authenticate`] wraps this with the Web Admin login audit trail. Callers
/// that are not a Web Admin login (the Kiosk Lock release, ADR-0056) use this
/// directly and write their own audit event, so a kiosk unlock never shows up
/// as a Web Admin sign-in.
pub async fn verify_pin(pool: &SqlitePool, pin: &str) -> AppResult<bool> {
    let stored_hash =
        sqlx::query_scalar::<_, String>("SELECT password_hash FROM owner_credentials WHERE id = 1")
            .fetch_optional(pool)
            .await?;
    Ok(stored_hash
        .as_deref()
        .is_some_and(|hash| verify_password(pin, hash)))
}

pub async fn authenticate(pool: &SqlitePool, pin: &str) -> AppResult<()> {
    let success = verify_pin(pool, pin).await?;
    let event = if success {
        NewAuditEvent::simple(
            "owner_login_succeeded",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Owner authenticated",
        )
    } else {
        NewAuditEvent::simple(
            "owner_login_failed",
            AuditOutcome::Denied,
            AuditActor::Admin,
            "Owner authentication denied",
        )
    };
    audit::append_standalone(pool, event).await?;
    if success {
        Ok(())
    } else {
        Err(AppError::Unauthorized("invalid owner PIN".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::db;

    #[tokio::test]
    async fn owner_setup_and_login_are_hashed_and_audited() -> AppResult<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        setup(&pool, "owner-password").await?;
        assert!(is_configured(&pool).await?);
        assert!(db::verify_owner_pin(&pool, "owner-password").await?);
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM owner_credentials WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        assert_ne!(stored_hash, "owner-password");
        assert!(authenticate(&pool, "owner-password").await.is_ok());
        assert!(matches!(
            authenticate(&pool, "wrong-password").await,
            Err(AppError::Unauthorized(_))
        ));
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type IN (
               'owner_configured', 'owner_login_succeeded', 'owner_login_failed'
             )",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(events, 3);
        Ok(())
    }
}

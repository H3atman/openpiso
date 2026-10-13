use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::{Sqlite, SqlitePool};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{Account, AccountBalanceMutation, CreateVoucherInput, Voucher},
};

pub async fn create_voucher(pool: &SqlitePool, input: &CreateVoucherInput) -> AppResult<Voucher> {
    let prefix = validate_voucher_code_parts(
        input.prefix.as_deref().unwrap_or("OP"),
        input.suffix_length.unwrap_or(8),
    )?;
    let suffix_length = input.suffix_length.unwrap_or(8);
    if input.seconds_value < 0 || input.points_value_centi < 0 {
        return Err(AppError::InvalidInput(
            "voucher values cannot be negative".to_owned(),
        ));
    }
    if input.seconds_value == 0 && input.points_value_centi == 0 {
        return Err(AppError::InvalidInput(
            "voucher must include time or points".to_owned(),
        ));
    }
    if let Some(expires_at) = input.expires_at.as_deref() {
        chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| AppError::InvalidInput("expires_at must be RFC 3339".to_owned()))?;
    }
    let sold_amount_cents = input.sold_amount_cents.unwrap_or(0);
    if sold_amount_cents < 0 {
        return Err(AppError::InvalidInput(
            "cash amount cannot be negative".to_owned(),
        ));
    }
    let kind = if sold_amount_cents > 0 {
        "sold"
    } else {
        "promo"
    };

    let id = Uuid::new_v4().to_string();
    let mut transaction = super::begin_immediate(pool).await?;
    let code = generate_unique_voucher_code(&mut transaction, &prefix, suffix_length).await?;
    let mut event = NewAuditEvent::simple(
        if kind == "sold" {
            "voucher_sold"
        } else {
            "voucher_minted"
        },
        AuditOutcome::Success,
        AuditActor::Admin,
        if kind == "sold" {
            "Sold voucher recorded"
        } else {
            "Promo voucher minted"
        },
    );
    event.amount_cents = (kind == "sold").then_some(sold_amount_cents);
    event.seconds_delta = Some(input.seconds_value);
    event.points_delta_centi = Some(input.points_value_centi);
    event.cash_flag = kind == "sold";
    event.metadata = json!({
        "voucher_id": id,
        "code": code,
        "kind": kind,
        "expires_at": input.expires_at
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    let code_normalized = normalize_voucher_code(&code)?;
    sqlx::query(
        "INSERT INTO vouchers (
           id, code, kind, seconds_value, points_value_centi, sold_amount_cents,
           sold_at, expires_at, audit_event_id, code_normalized
         ) VALUES (?, ?, ?, ?, ?, ?, CASE WHEN ? = 'sold' THEN CURRENT_TIMESTAMP ELSE NULL END, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&code)
    .bind(kind)
    .bind(input.seconds_value)
    .bind(input.points_value_centi)
    .bind(sold_amount_cents)
    .bind(kind)
    .bind(&input.expires_at)
    .bind(audit_event_id)
    .bind(&code_normalized)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    voucher(pool, &id).await
}

pub async fn vouchers(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
    batch_id: Option<&str>,
    search: Option<&str>,
    status: Option<&str>,
) -> AppResult<Vec<Voucher>> {
    let safe_limit = limit.clamp(1, 200);
    let safe_offset = offset.max(0);
    if let Some(status) = status {
        if !matches!(status, "active" | "redeemed" | "voided") {
            return Err(AppError::InvalidInput("invalid voucher status".to_owned()));
        }
    }
    let normalized_search = search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_voucher_code)
        .transpose()?;
    Ok(sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT v.id, v.code, v.kind, v.status,
                v.seconds_value, v.points_value_centi, v.sold_amount_cents,
                v.created_at, v.sold_at, v.redeemed_by_account_id, a.username,
                v.redeemed_at, v.expires_at, v.batch_id, b.name, b.prefix
         FROM vouchers v
         LEFT JOIN accounts a ON a.id = v.redeemed_by_account_id
         LEFT JOIN voucher_batches b ON b.id = v.batch_id
         WHERE (? IS NULL OR v.batch_id = ?)
           AND (? IS NULL OR v.code_normalized LIKE '%' || ? || '%')
           AND (? IS NULL OR v.status = ?)
         ORDER BY v.created_at DESC, v.id DESC
         LIMIT ? OFFSET ?",
    )
    .bind(batch_id)
    .bind(batch_id)
    .bind(&normalized_search)
    .bind(&normalized_search)
    .bind(status)
    .bind(status)
    .bind(safe_limit)
    .bind(safe_offset)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Voucher::from)
    .collect())
}

pub async fn redeem_voucher(
    pool: &SqlitePool,
    account_id: &str,
    pc_id: &str,
    code: &str,
) -> AppResult<AccountBalanceMutation> {
    let normalized_code = normalize_voucher_code(code)?;
    let mut transaction = super::begin_immediate(pool).await?;
    let voucher = sqlx::query_as::<
        _,
        (
            String,
            String,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id, kind, seconds_value, points_value_centi, status, expires_at, batch_id, code
         FROM vouchers WHERE code_normalized = ?",
    )
    .bind(&normalized_code)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some((
        voucher_id,
        kind,
        seconds_value,
        points_value_centi,
        status,
        expires_at,
        batch_id,
        voucher_code,
    )) = voucher
    else {
        append_voucher_rejection(
            &mut transaction,
            account_id,
            pc_id,
            &normalized_code,
            "invalid",
        )
        .await?;
        transaction.commit().await?;
        return Err(AppError::NotFound("voucher code".to_owned()));
    };

    if status == "voided" {
        append_voucher_rejection(
            &mut transaction,
            account_id,
            pc_id,
            &normalized_code,
            "voided",
        )
        .await?;
        transaction.commit().await?;
        return Err(AppError::Conflict(
            "voucher is from a voided batch".to_owned(),
        ));
    }
    if status != "active" {
        append_voucher_rejection(
            &mut transaction,
            account_id,
            pc_id,
            &normalized_code,
            "spent",
        )
        .await?;
        transaction.commit().await?;
        return Err(AppError::Conflict(
            "voucher has already been redeemed".to_owned(),
        ));
    }

    if expires_at.is_some() {
        let expired = sqlx::query_scalar::<_, i64>(
            "SELECT CASE WHEN datetime(?) <= datetime('now') THEN 1 ELSE 0 END",
        )
        .bind(&expires_at)
        .fetch_one(&mut *transaction)
        .await?;
        if expired == 1 {
            append_voucher_rejection(
                &mut transaction,
                account_id,
                pc_id,
                &normalized_code,
                "expired",
            )
            .await?;
            transaction.commit().await?;
            return Err(AppError::Conflict("voucher is expired".to_owned()));
        }
    }

    let account = update_account_balance(
        &mut transaction,
        account_id,
        seconds_value,
        points_value_centi,
    )
    .await?;

    // Batch-sold vouchers recognise cash at redemption (ADR-0040):
    // sold_voucher_redeemed carries cash_flag = true and the cash amount.
    // Single-mint sold vouchers keep cash_flag = false (cash was booked at mint).
    let is_batch_sold = batch_id.is_some() && kind == "sold";
    let sold_amount = if is_batch_sold {
        sqlx::query_scalar::<_, i64>("SELECT sold_amount_cents FROM vouchers WHERE id = ?")
            .bind(&voucher_id)
            .fetch_one(&mut *transaction)
            .await?
    } else {
        0
    };
    let event_type = if kind == "sold" {
        "sold_voucher_redeemed"
    } else {
        "promo_voucher_redeemed"
    };
    let mut event = NewAuditEvent::simple(
        event_type,
        AuditOutcome::Success,
        AuditActor::Account,
        "Voucher redeemed",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = Some(pc_id);
    event.amount_cents = is_batch_sold.then_some(sold_amount);
    event.seconds_delta = Some(seconds_value);
    event.points_delta_centi = Some(points_value_centi);
    event.cash_flag = is_batch_sold;
    event.metadata = json!({
        "voucher_id": voucher_id,
        "code": voucher_code,
        "kind": kind,
        "batch_id": batch_id
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    let redeemed = sqlx::query_scalar::<_, String>(
        "UPDATE vouchers
         SET status = 'redeemed',
             redeemed_by_account_id = ?,
             redeemed_at = CURRENT_TIMESTAMP,
             redeemed_audit_event_id = ?
         WHERE id = ? AND status = 'active'
         RETURNING redeemed_at",
    )
    .bind(account_id)
    .bind(audit_event_id)
    .bind(&voucher_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if redeemed.is_none() {
        transaction.rollback().await?;
        let mut denial = super::begin_immediate(pool).await?;
        append_voucher_rejection(&mut denial, account_id, pc_id, &normalized_code, "spent").await?;
        denial.commit().await?;
        return Err(AppError::Conflict(
            "voucher has already been redeemed".to_owned(),
        ));
    }

    transaction.commit().await?;
    Ok(AccountBalanceMutation { account })
}

async fn voucher(pool: &SqlitePool, id: &str) -> AppResult<Voucher> {
    sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT v.id, v.code, v.kind, v.status, v.seconds_value, v.points_value_centi,
                v.sold_amount_cents, v.created_at, v.sold_at,
                v.redeemed_by_account_id, a.username, v.redeemed_at, v.expires_at,
                v.batch_id, b.name, b.prefix
         FROM vouchers v
         LEFT JOIN accounts a ON a.id = v.redeemed_by_account_id
         LEFT JOIN voucher_batches b ON b.id = v.batch_id
         WHERE v.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .map(Voucher::from)
    .ok_or_else(|| AppError::NotFound("voucher".to_owned()))
}

pub async fn void_voucher(pool: &SqlitePool, voucher_id: &str) -> AppResult<Voucher> {
    let mut transaction = super::begin_immediate(pool).await?;
    let (id, code, kind, batch_id) = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT id, code, kind, batch_id FROM vouchers WHERE id = ?",
    )
    .bind(voucher_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound("voucher".to_owned()))?;

    let active = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM vouchers WHERE id = ? AND status = 'active')",
    )
    .bind(voucher_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !active {
        return Err(AppError::Conflict(
            "only active vouchers can be voided".to_owned(),
        ));
    }

    let summary = format!("Voucher {code} voided");
    let mut event = NewAuditEvent::simple(
        "voucher_voided",
        AuditOutcome::Success,
        AuditActor::Admin,
        &summary,
    );
    event.metadata = json!({
        "voucher_id": id,
        "code": code,
        "kind": kind,
        "batch_id": batch_id,
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    let voided = sqlx::query_scalar::<_, String>(
        "UPDATE vouchers
         SET status = 'voided', voided_audit_event_id = ?
         WHERE id = ? AND status = 'active'
         RETURNING id",
    )
    .bind(audit_event_id)
    .bind(voucher_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if voided.is_none() {
        transaction.rollback().await?;
        return Err(AppError::Conflict(
            "only active vouchers can be voided".to_owned(),
        ));
    }

    transaction.commit().await?;
    voucher(pool, voucher_id).await
}

pub(super) const UNMISTAKABLE_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub(super) fn validate_voucher_code_parts(prefix: &str, suffix_length: u8) -> AppResult<String> {
    let prefix = prefix.trim().to_ascii_uppercase();
    if !(2..=8).contains(&prefix.len()) {
        return Err(AppError::InvalidInput(
            "prefix must be 2-8 characters".to_owned(),
        ));
    }
    if !prefix
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(AppError::InvalidInput(
            "prefix must contain only A-Z and 0-9".to_owned(),
        ));
    }
    if !(6..=16).contains(&suffix_length) {
        return Err(AppError::InvalidInput(
            "suffix_length must be 6-16".to_owned(),
        ));
    }
    Ok(prefix)
}

pub(super) fn generate_voucher_code(prefix: &str, suffix_length: u8) -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let suffix: String = (0..suffix_length)
        .map(|_| UNMISTAKABLE_CHARS[rng.random_range(0..UNMISTAKABLE_CHARS.len())] as char)
        .collect();
    let grouped = suffix
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-");
    format!("{prefix}-{grouped}")
}

async fn generate_unique_voucher_code(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    prefix: &str,
    suffix_length: u8,
) -> AppResult<String> {
    for _ in 0..5 {
        let code = generate_voucher_code(prefix, suffix_length);
        let normalized = normalize_voucher_code(&code)?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM vouchers WHERE code_normalized = ?)")
                .bind(normalized)
                .fetch_one(&mut **transaction)
                .await?;
        if !exists {
            return Ok(code);
        }
    }
    Err(AppError::Conflict(
        "voucher code collision after retries".to_owned(),
    ))
}

pub(super) fn normalize_voucher_code(code: &str) -> AppResult<String> {
    let normalized = code.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        return Err(AppError::InvalidInput(
            "voucher code is required".to_owned(),
        ));
    }
    let stripped: String = normalized.chars().filter(|&c| c != '-').collect();
    if stripped.is_empty() || stripped.len() > 64 {
        return Err(AppError::InvalidInput("invalid voucher code".to_owned()));
    }
    if !stripped.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(AppError::InvalidInput("invalid voucher code".to_owned()));
    }
    Ok(stripped)
}

pub(super) async fn update_account_balance(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    account_id: &str,
    seconds_delta: i64,
    points_delta_centi: i64,
) -> AppResult<Account> {
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
            bool,
        ),
    >(
        "UPDATE accounts
         SET remaining_seconds = remaining_seconds + ?,
             points_balance_centi = points_balance_centi + ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active'
         RETURNING id, username, remaining_seconds, points_balance_centi, status, created_at,
                   deleted_at, imported_from, imported_at,
                   imported_from IS NOT NULL AND password_hash LIKE '$2%$%'",
    )
    .bind(seconds_delta)
    .bind(points_delta_centi)
    .bind(account_id)
    .fetch_optional(&mut **transaction)
    .await?
    .map(Account::from)
    .ok_or_else(|| AppError::NotFound("active account".to_owned()))
}

async fn append_voucher_rejection(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    account_id: &str,
    pc_id: &str,
    code: &str,
    reason: &str,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "voucher_rejected",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Voucher redemption rejected",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = Some(pc_id);
    event.metadata = json!({
        "code": code,
        "reason": reason
    });
    audit::append(transaction, event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::accounts::{self, AccountCreator};

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        super::super::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await?;
        super::super::seed(&pool).await?;
        Ok(pool)
    }

    async fn file_test_pool() -> AppResult<SqlitePool> {
        let path = std::env::temp_dir().join(format!("openpiso-test-{}.db", Uuid::new_v4()));
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        super::super::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await?;
        super::super::seed(&pool).await?;
        Ok(pool)
    }

    async fn test_account(pool: &SqlitePool, username: &str) -> AppResult<Account> {
        accounts::create(pool, username, "password", AccountCreator::Admin, None).await
    }

    #[tokio::test]
    async fn single_voucher_uses_requested_code_format_and_defaults() -> AppResult<()> {
        let pool = test_pool().await?;
        let custom = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: Some(" promo ".to_owned()),
                suffix_length: Some(6),
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        assert!(custom.code.starts_with("PROMO-"));
        assert_eq!(custom.code.replace('-', "").len(), 11);

        let default = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        assert!(default.code.starts_with("OP-"));
        assert_eq!(default.code.replace('-', "").len(), 10);

        for (prefix, suffix_length) in [("X", 8), ("VALID", 5), ("BAD!", 8)] {
            let result = create_voucher(
                &pool,
                &CreateVoucherInput {
                    prefix: Some(prefix.to_owned()),
                    suffix_length: Some(suffix_length),
                    seconds_value: 300,
                    points_value_centi: 0,
                    sold_amount_cents: Some(0),
                    expires_at: None,
                },
            )
            .await;
            assert!(matches!(result, Err(AppError::InvalidInput(_))));
        }
        Ok(())
    }

    #[tokio::test]
    async fn sold_voucher_records_cash_at_mint() -> AppResult<()> {
        let pool = test_pool().await?;
        let voucher = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 3_600,
                points_value_centi: 2,
                sold_amount_cents: Some(1_000),
                expires_at: None,
            },
        )
        .await?;

        assert_eq!(voucher.kind, "sold");
        assert_eq!(voucher.sold_amount_cents, 1_000);
        assert!(voucher.sold_at.is_some());
        let audit: (i64, i64, i64) = sqlx::query_as(
            "SELECT amount_cents, seconds_delta, points_delta_centi
             FROM audit_log WHERE event_type = 'voucher_sold'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit, (1_000, 3_600, 2));
        Ok(())
    }

    #[tokio::test]
    async fn individual_void_keeps_sold_revenue_and_prevents_redemption() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "voided-voucher-user").await?;
        let voucher = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(500),
                expires_at: None,
            },
        )
        .await?;

        let voided = void_voucher(&pool, &voucher.id).await?;
        assert_eq!(voided.status, "voided");
        assert!(matches!(
            redeem_voucher(&pool, &account.id, "pc-01", &voucher.code).await,
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            void_voucher(&pool, &voucher.id).await,
            Err(AppError::Conflict(_))
        ));

        let revenue_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_sold'")
                .fetch_one(&pool)
                .await?;
        let void_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_voided'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(revenue_events, 1);
        assert_eq!(void_events, 1);
        Ok(())
    }

    #[tokio::test]
    async fn single_mint_lookup_is_case_and_dash_insensitive() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "normalized-single").await?;
        let voucher = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        let expected_normalized = voucher.code.replace('-', "");
        let stored_normalized: String =
            sqlx::query_scalar("SELECT code_normalized FROM vouchers WHERE id = ?")
                .bind(&voucher.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(stored_normalized, expected_normalized);
        assert!(
            sqlx::query("UPDATE vouchers SET code_normalized = NULL WHERE id = ?")
                .bind(&voucher.id)
                .execute(&pool)
                .await
                .is_err()
        );
        let hand_typed = format!("  {}  ", expected_normalized.to_ascii_lowercase());

        let mutation = redeem_voucher(&pool, &account.id, "pc-01", &hand_typed).await?;

        assert_eq!(mutation.account.remaining_seconds, 300);
        Ok(())
    }

    #[tokio::test]
    async fn promo_voucher_redemption_is_non_cash_free_value() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "promo-user").await?;
        let voucher = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 600,
                points_value_centi: 1,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;

        let mutation = redeem_voucher(&pool, &account.id, "pc-01", &voucher.code).await?;

        assert_eq!(mutation.account.remaining_seconds, 600);
        assert_eq!(mutation.account.points_balance_centi, 1);
        let audit: (i64, i64, i64) = sqlx::query_as(
            "SELECT cash_flag, seconds_delta, points_delta_centi
             FROM audit_log WHERE event_type = 'promo_voucher_redeemed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit, (0, 600, 1));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_voucher_redemption_pays_out_once() -> AppResult<()> {
        let pool = file_test_pool().await?;
        let first = test_account(&pool, "first-voucher").await?;
        let second = test_account(&pool, "second-voucher").await?;
        let voucher = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 900,
                points_value_centi: 3,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;

        let first_redeem = redeem_voucher(&pool, &first.id, "pc-01", &voucher.code);
        let second_redeem = redeem_voucher(&pool, &second.id, "pc-02", &voucher.code);
        let (first_result, second_result) = tokio::join!(first_redeem, second_redeem);

        assert!(first_result.is_ok() ^ second_result.is_ok());
        let totals: (i64, i64) = sqlx::query_as(
            "SELECT SUM(remaining_seconds), SUM(points_balance_centi)
             FROM accounts WHERE id IN (?, ?)",
        )
        .bind(&first.id)
        .bind(&second.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(totals, (900, 3));
        let redeemed_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM vouchers WHERE status = 'redeemed'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(redeemed_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn spent_voucher_rejection_is_audited_without_balance_change() -> AppResult<()> {
        let pool = test_pool().await?;
        let first = test_account(&pool, "spent-first").await?;
        let second = test_account(&pool, "spent-second").await?;
        let voucher = create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 300,
                points_value_centi: 0,
                sold_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        redeem_voucher(&pool, &first.id, "pc-01", &voucher.code).await?;

        assert!(matches!(
            redeem_voucher(&pool, &second.id, "pc-02", &voucher.code).await,
            Err(AppError::Conflict(_))
        ));
        let second_balance: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&second.id)
        .fetch_one(&pool)
        .await?;
        let rejections: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_rejected'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(second_balance, (0, 0));
        assert_eq!(rejections, 1);
        Ok(())
    }
}

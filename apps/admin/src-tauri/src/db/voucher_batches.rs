use std::collections::HashMap;

use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{
        CreateVoucherBatchInput, Voucher, VoucherBatch, VoucherBatchDetail, VoucherBatchSummary,
    },
};

const BATCH_NAME_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const BATCH_NAME_LENGTH: usize = 5;

pub async fn create_voucher_batch(
    pool: &SqlitePool,
    input: &CreateVoucherBatchInput,
) -> AppResult<VoucherBatch> {
    // --- Validate input ---
    let suffix_length = input.suffix_length.unwrap_or(8);
    let prefix = super::vouchers::validate_voucher_code_parts(&input.prefix, suffix_length)?;

    let count = input.count;
    if !(1..=1000).contains(&count) {
        return Err(AppError::InvalidInput("count must be 1-1000".to_owned()));
    }

    let seconds_value = input.seconds_value;
    let points_value_centi = input.points_value_centi;
    if seconds_value < 0 || points_value_centi < 0 {
        return Err(AppError::InvalidInput(
            "values cannot be negative".to_owned(),
        ));
    }
    if seconds_value == 0 && points_value_centi == 0 {
        return Err(AppError::InvalidInput(
            "batch must include time or points".to_owned(),
        ));
    }

    let cash_amount_cents = input.cash_amount_cents.unwrap_or(0);
    if cash_amount_cents < 0 {
        return Err(AppError::InvalidInput(
            "cash_amount_cents cannot be negative".to_owned(),
        ));
    }

    let kind = if cash_amount_cents > 0 {
        "sold"
    } else {
        "promo"
    };

    if let Some(expires_at) = input.expires_at.as_deref() {
        chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| AppError::InvalidInput("expires_at must be RFC 3339".to_owned()))?;
    }

    let batch_id = Uuid::new_v4().to_string();

    // --- Start transaction ---
    let mut transaction = super::begin_immediate(pool).await?;
    let batch_name = generate_unique_batch_name(&mut transaction).await?;

    // Write audit event for the batch mint.
    let mint_summary = format!("Voucher batch {batch_name} minted: {prefix} x{count}");
    let mut event = NewAuditEvent::simple(
        "voucher_batch_minted",
        AuditOutcome::Success,
        AuditActor::Admin,
        &mint_summary,
    );
    event.cash_flag = false;
    event.metadata = json!({
        "batch_id": &batch_id,
        "batch_name": &batch_name,
        "prefix": &prefix,
        "count": count,
        "seconds_value": seconds_value,
        "points_value_centi": points_value_centi,
        "cash_amount_cents": cash_amount_cents,
        "kind": kind,
        "expires_at": input.expires_at
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    // Insert the batch row.
    sqlx::query(
        "INSERT INTO voucher_batches (
           id, name, prefix, suffix_length, count, kind,
           seconds_value, points_value_centi, cash_amount_cents,
           expires_at, audit_event_id
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&batch_id)
    .bind(&batch_name)
    .bind(&prefix)
    .bind(suffix_length as i64)
    .bind(count)
    .bind(kind)
    .bind(seconds_value)
    .bind(points_value_centi)
    .bind(cash_amount_cents)
    .bind(&input.expires_at)
    .bind(&audit_event_id)
    .execute(&mut *transaction)
    .await?;

    // Generate and insert each voucher. All vouchers share the batch's
    // audit_event_id (no per-card events at mint). On UNIQUE collision
    // (extremely rare with 32-char codes), retry with a fresh code.
    let mut generated_codes: Vec<String> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let voucher_id = Uuid::new_v4().to_string();
        let code = insert_voucher_with_retry(
            &mut transaction,
            VoucherInsertParams {
                voucher_id: &voucher_id,
                prefix: &prefix,
                suffix_length,
                kind,
                seconds_value,
                points_value_centi,
                cash_amount_cents,
                audit_event_id: &audit_event_id,
                expires_at: &input.expires_at,
                batch_id: &batch_id,
            },
        )
        .await?;
        generated_codes.push(code);
    }

    transaction.commit().await?;

    // Read back the batch row to return.
    let batch = voucher_batch_by_id(pool, &batch_id).await?;
    Ok(batch)
}

fn generate_batch_name() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..BATCH_NAME_LENGTH)
        .map(|_| BATCH_NAME_CHARS[rng.random_range(0..BATCH_NAME_CHARS.len())] as char)
        .collect()
}

async fn generate_unique_batch_name(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> AppResult<String> {
    const MAX_ATTEMPTS: u32 = 10;
    for _ in 0..MAX_ATTEMPTS {
        let name = generate_batch_name();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM voucher_batches WHERE name = ? COLLATE BINARY)",
        )
        .bind(&name)
        .fetch_one(&mut **transaction)
        .await?;
        if !exists {
            return Ok(name);
        }
    }
    Err(AppError::Conflict(
        "voucher batch name collision after retries".to_owned(),
    ))
}

/// Parameters for inserting a single batch voucher.
struct VoucherInsertParams<'a> {
    voucher_id: &'a str,
    prefix: &'a str,
    suffix_length: u8,
    kind: &'a str,
    seconds_value: i64,
    points_value_centi: i64,
    cash_amount_cents: i64,
    audit_event_id: &'a str,
    expires_at: &'a Option<String>,
    batch_id: &'a str,
}

/// Insert one voucher code with retry on UNIQUE collision (up to 5 attempts).
async fn insert_voucher_with_retry(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    params: VoucherInsertParams<'_>,
) -> AppResult<String> {
    let prefix = params.prefix.to_owned();
    let suffix_length = params.suffix_length;
    insert_voucher_with_retry_using(transaction, params, || {
        crate::db::vouchers::generate_voucher_code(&prefix, suffix_length)
    })
    .await
}

async fn insert_voucher_with_retry_using<F>(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    params: VoucherInsertParams<'_>,
    mut generate_code: F,
) -> AppResult<String>
where
    F: FnMut() -> String,
{
    const MAX_ATTEMPTS: u32 = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        let code = generate_code();
        let code_normalized = super::vouchers::normalize_voucher_code(&code)?;
        let result = sqlx::query(
            "INSERT INTO vouchers (
               id, code, kind, status, seconds_value, points_value_centi,
               sold_amount_cents, audit_event_id, expires_at, batch_id,
               code_normalized
             ) VALUES (?, ?, ?, 'active', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(params.voucher_id)
        .bind(&code)
        .bind(params.kind)
        .bind(params.seconds_value)
        .bind(params.points_value_centi)
        .bind(params.cash_amount_cents)
        .bind(params.audit_event_id)
        .bind(params.expires_at)
        .bind(params.batch_id)
        .bind(code_normalized)
        .execute(&mut **transaction)
        .await;

        match result {
            Ok(_) => return Ok(code),
            Err(ref error) if is_unique_violation(error) && attempt < MAX_ATTEMPTS => {
                continue;
            }
            Err(error) if is_unique_violation(&error) => {
                return Err(AppError::Conflict(
                    "voucher code collision after retries".to_owned(),
                ));
            }
            Err(error) => return Err(AppError::Database(error)),
        }
    }
    unreachable!()
}

/// Check if a sqlx error is a UNIQUE constraint violation.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(ref db_err) = error {
        // SQLite error code 1555 = SQLITE_CONSTRAINT_UNIQUE (SQLite 3.45+)
        // Fall back to checking the message for older libsqlite3-sys bindings.
        db_err.code().as_deref() == Some("1555")
            || db_err.message().contains("UNIQUE constraint failed")
    } else {
        false
    }
}

/// Read a single voucher batch by ID.
async fn voucher_batch_by_id(pool: &SqlitePool, id: &str) -> AppResult<VoucherBatch> {
    sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            i64,
            String,
            i64,
            i64,
            i64,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id, name, prefix, suffix_length, count, kind,
                seconds_value, points_value_centi, cash_amount_cents,
                expires_at, created_at, voided_at, voided_audit_event_id
         FROM voucher_batches WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .map(VoucherBatch::from)
    .ok_or_else(|| AppError::NotFound("voucher batch".to_owned()))
}

/// List all batches, most recent first.
pub async fn list_voucher_batches(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<VoucherBatchSummary>> {
    let batches = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            i64,
            String,
            i64,
            i64,
            i64,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id, name, prefix, suffix_length, count, kind,
                seconds_value, points_value_centi, cash_amount_cents,
                expires_at, created_at, voided_at, voided_audit_event_id
         FROM voucher_batches ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
    )
    .bind(limit.clamp(1, 200))
    .bind(offset.max(0))
    .fetch_all(pool)
    .await?;

    let redeemed_counts = sqlx::query_as::<_, (String, i64)>(
        "SELECT batch_id, COUNT(*) FROM vouchers
         WHERE batch_id IS NOT NULL AND status = 'redeemed' GROUP BY batch_id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect::<HashMap<_, _>>();

    Ok(batches
        .into_iter()
        .map(VoucherBatch::from)
        .map(|batch| VoucherBatchSummary {
            redeemed_count: redeemed_counts.get(&batch.id).copied().unwrap_or(0),
            batch,
        })
        .collect())
}

pub async fn get_voucher_batch(pool: &SqlitePool, id: &str) -> AppResult<VoucherBatchDetail> {
    let batch = voucher_batch_by_id(pool, id).await?;
    let vouchers = sqlx::query_as::<
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
         WHERE v.batch_id = ? ORDER BY v.code",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Voucher::from)
    .collect::<Vec<_>>();
    let redeemed_count = vouchers
        .iter()
        .filter(|voucher| voucher.status == "redeemed")
        .count() as i64;
    Ok(VoucherBatchDetail {
        batch,
        redeemed_count,
        vouchers,
    })
}

/// Void every active card in a batch in the same transaction as its batch
/// marker and single audit event. Redeemed cards remain unchanged.
pub async fn void_voucher_batch(pool: &SqlitePool, batch_id: &str) -> AppResult<VoucherBatch> {
    let mut transaction = super::begin_immediate(pool).await?;
    let claimed = sqlx::query_as::<_, (String, String)>(
        "UPDATE voucher_batches
         SET voided_at = CURRENT_TIMESTAMP
         WHERE id = ? AND voided_at IS NULL
         RETURNING name, prefix",
    )
    .bind(batch_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((batch_name, prefix)) = claimed else {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM voucher_batches WHERE id = ?)")
                .bind(batch_id)
                .fetch_one(&mut *transaction)
                .await?;
        return if exists {
            Err(AppError::Conflict(
                "voucher batch is already voided".to_owned(),
            ))
        } else {
            Err(AppError::NotFound("voucher batch".to_owned()))
        };
    };

    let active_count = sqlx::query(
        "UPDATE vouchers SET status = 'voided'
         WHERE batch_id = ? AND status = 'active'",
    )
    .bind(batch_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected() as i64;

    let summary =
        format!("Voucher batch {batch_name} ({prefix}) voided ({active_count} active cards)");
    let mut event = NewAuditEvent::simple(
        "voucher_batch_voided",
        AuditOutcome::Success,
        AuditActor::Admin,
        &summary,
    );
    event.metadata = json!({
        "batch_id": batch_id,
        "batch_name": &batch_name,
        "prefix": &prefix,
        "active_cards_voided": active_count
    });
    event.cash_flag = false;
    let audit_event_id = audit::append(&mut transaction, event).await?;

    sqlx::query("UPDATE voucher_batches SET voided_audit_event_id = ? WHERE id = ?")
        .bind(&audit_event_id)
        .bind(batch_id)
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await?;
    voucher_batch_by_id(pool, batch_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{self, AccountCreator};
    use crate::models::{Account, CreateVoucherInput};
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    async fn test_account(pool: &SqlitePool, username: &str) -> AppResult<Account> {
        accounts::create(pool, username, "password", AccountCreator::Admin, None).await
    }

    #[test]
    fn code_format_is_prefix_dash_grouped() {
        let code = crate::db::vouchers::generate_voucher_code("PROMO", 8);
        assert!(code.starts_with("PROMO-"));
        // PROMO- + XXXX-XXXX = 5 + 1 + 9 = 15
        assert_eq!(code.len(), 15);
        // Make sure it only contains chars from the unambiguous set plus prefix letters/dashes.
        for ch in code.chars().skip(6) {
            // After PROMO- prefix
            if ch != '-' {
                assert!(
                    crate::db::vouchers::UNMISTAKABLE_CHARS.contains(&(ch as u8)),
                    "char {ch} not in unambiguous set"
                );
            }
        }
    }

    #[test]
    fn code_format_supports_variable_suffix_length() {
        let short = crate::db::vouchers::generate_voucher_code("TEST", 6);
        // TEST- + XXXX-XX = 4 + 1 + 4 + 1 + 2 = 12
        assert_eq!(short.len(), 12);
        assert!(short.starts_with("TEST-"));

        let long = crate::db::vouchers::generate_voucher_code("TEST", 16);
        // TEST- + XXXX-XXXX-XXXX-XXXX = 4 + 1 + 4+1+4+1+4+1+4 = 5 + 19 = 24
        assert_eq!(long.len(), 24);
        assert!(long.starts_with("TEST-"));
    }

    #[test]
    fn chars_are_from_unmistakable_set() {
        let code = crate::db::vouchers::generate_voucher_code("A", 8);
        // All non-dash characters after "A-" prefix must be from the unambiguous set.
        for ch in code.chars().skip(2) {
            if ch != '-' {
                assert!(
                    crate::db::vouchers::UNMISTAKABLE_CHARS.contains(&(ch as u8)),
                    "unexpected char {ch}"
                );
            }
        }
    }

    #[tokio::test]
    async fn collision_retries_with_a_fresh_code() -> AppResult<()> {
        let pool = test_pool().await?;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "RETRY".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        let existing_code: String =
            sqlx::query_scalar("SELECT code FROM vouchers WHERE batch_id = ?")
                .bind(&batch.id)
                .fetch_one(&pool)
                .await?;
        let audit_event_id: String =
            sqlx::query_scalar("SELECT audit_event_id FROM voucher_batches WHERE id = ?")
                .bind(&batch.id)
                .fetch_one(&pool)
                .await?;
        let voucher_id = Uuid::new_v4().to_string();
        let expires_at = None;
        let mut codes = [existing_code, "RETRY-ZZZZ-ZZZZ".to_owned()].into_iter();
        let mut transaction = crate::db::begin_immediate(&pool).await?;
        let inserted = insert_voucher_with_retry_using(
            &mut transaction,
            VoucherInsertParams {
                voucher_id: &voucher_id,
                prefix: "RETRY",
                suffix_length: 8,
                kind: "promo",
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: 0,
                audit_event_id: &audit_event_id,
                expires_at: &expires_at,
                batch_id: &batch.id,
            },
            || codes.next().expect("retry should consume only two codes"),
        )
        .await?;
        transaction.commit().await?;

        assert_eq!(inserted, "RETRY-ZZZZ-ZZZZ");
        let inserted_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM vouchers WHERE code = ?")
                .bind(inserted)
                .fetch_one(&pool)
                .await?;
        assert_eq!(inserted_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn create_batch_basic_happy_path() -> AppResult<()> {
        let pool = test_pool().await?;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "P2OMO".to_owned(),
                suffix_length: Some(8),
                count: 3,
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;

        assert_eq!(batch.name.len(), BATCH_NAME_LENGTH);
        assert!(batch
            .name
            .bytes()
            .all(|character| BATCH_NAME_CHARS.contains(&character)));
        assert_eq!(batch.prefix, "P2OMO");
        assert_eq!(batch.kind, "promo");
        assert_eq!(batch.count, 3);
        assert!(batch.voided_at.is_none());
        assert_eq!(batch.seconds_value, 600);

        // Check 3 voucher rows were created
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT code, kind, status FROM vouchers WHERE batch_id = ? ORDER BY code",
        )
        .bind(&batch.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(rows.len(), 3);
        for (code, kind, status) in &rows {
            assert!(code.starts_with("P2OMO-"));
            assert_eq!(kind, "promo");
            assert_eq!(status, "active");
        }
        // Codes should be unique
        let mut codes: Vec<_> = rows.iter().map(|r| r.0.clone()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), 3);

        // Check audit event
        let audit: (String, String, i64) = sqlx::query_as(
            "SELECT event_type, summary, cash_flag FROM audit_log
             WHERE event_type = 'voucher_batch_minted'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit.0, "voucher_batch_minted");
        assert_eq!(
            audit.1,
            format!("Voucher batch {} minted: P2OMO x3", batch.name)
        );
        assert_eq!(audit.2, 0);
        Ok(())
    }

    #[tokio::test]
    async fn create_batch_sold_kind_inferred() -> AppResult<()> {
        let pool = test_pool().await?;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "SOLD".to_owned(),
                suffix_length: Some(8),
                count: 2,
                seconds_value: 3600,
                points_value_centi: 0,
                cash_amount_cents: Some(5000), // ₱50 card
                expires_at: None,
            },
        )
        .await?;

        assert_eq!(batch.kind, "sold");
        assert_eq!(batch.cash_amount_cents, 5000);
        let rows: Vec<String> = sqlx::query_scalar("SELECT kind FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_all(&pool)
            .await?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|k| k == "sold"));
        Ok(())
    }

    #[tokio::test]
    async fn create_batch_invalid_input_is_rejected() -> AppResult<()> {
        let pool = test_pool().await?;

        // Prefix too short
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "X".to_owned(),
                    suffix_length: Some(8),
                    count: 1,
                    seconds_value: 600,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: None,
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));

        // Prefix too long
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "ABCDEFGH9".to_owned(),
                    suffix_length: Some(8),
                    count: 1,
                    seconds_value: 600,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: None,
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));

        // Prefix invalid chars
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "HELLO-".to_owned(),
                    suffix_length: Some(8),
                    count: 1,
                    seconds_value: 600,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: None,
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));

        // Suffix length out of range
        for suffix_length in [5, 17] {
            assert!(matches!(
                create_voucher_batch(
                    &pool,
                    &CreateVoucherBatchInput {
                        prefix: "PROMO".to_owned(),
                        suffix_length: Some(suffix_length),
                        count: 1,
                        seconds_value: 600,
                        points_value_centi: 0,
                        cash_amount_cents: Some(0),
                        expires_at: None,
                    }
                )
                .await,
                Err(AppError::InvalidInput(_))
            ));
        }

        // Zero values
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "PROMO".to_owned(),
                    suffix_length: Some(8),
                    count: 1,
                    seconds_value: 0,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: None,
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));

        // Count out of range
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "PROMO".to_owned(),
                    suffix_length: Some(8),
                    count: 0,
                    seconds_value: 600,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: None,
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "PROMO".to_owned(),
                    suffix_length: Some(8),
                    count: 1001,
                    seconds_value: 600,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: None,
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));

        // Bad expiry
        assert!(matches!(
            create_voucher_batch(
                &pool,
                &CreateVoucherBatchInput {
                    prefix: "PROMO".to_owned(),
                    suffix_length: Some(8),
                    count: 1,
                    seconds_value: 600,
                    points_value_centi: 0,
                    cash_amount_cents: Some(0),
                    expires_at: Some("not-a-date".to_owned()),
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));

        Ok(())
    }

    #[tokio::test]
    async fn batch_vouchers_can_be_redeemed() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "redeemer").await?;

        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "PROMO".to_owned(),
                suffix_length: Some(8),
                count: 2,
                seconds_value: 900,
                points_value_centi: 5,
                cash_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;

        // Fetch the voucher codes
        let codes: Vec<String> = sqlx::query_scalar(
            "SELECT code FROM vouchers WHERE batch_id = ? AND status = 'active' ORDER BY code",
        )
        .bind(&batch.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(codes.len(), 2);

        // Hand-typed batch codes resolve without dashes or exact case.
        let hand_typed = codes[0].replace('-', "").to_ascii_lowercase();
        let mutation =
            crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &hand_typed).await?;
        assert_eq!(mutation.account.remaining_seconds, 900);
        assert_eq!(mutation.account.points_balance_centi, 5);

        // Can't redeem twice
        assert!(matches!(
            crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &codes[0]).await,
            Err(AppError::Conflict(_))
        ));

        Ok(())
    }

    #[tokio::test]
    async fn migration_backfills_preexisting_logical_voids() -> AppResult<()> {
        let pool = test_pool().await?;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "OLDVOID".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        sqlx::query("UPDATE voucher_batches SET voided_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(&batch.id)
            .execute(&pool)
            .await?;

        sqlx::raw_sql(include_str!(
            "../../migrations/0019_physical_voucher_void_status.sql"
        ))
        .execute(&pool)
        .await?;

        let status: String = sqlx::query_scalar("SELECT status FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "voided");
        Ok(())
    }

    #[tokio::test]
    async fn void_batch_prevents_redeeming_active_cards() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "voidtest").await?;

        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "VOID".to_owned(),
                suffix_length: Some(8),
                count: 3,
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: Some(1000),
                expires_at: None,
            },
        )
        .await?;

        let codes: Vec<String> = sqlx::query_scalar(
            "SELECT code FROM vouchers WHERE batch_id = ? AND status = 'active' ORDER BY code",
        )
        .bind(&batch.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(codes.len(), 3);

        // Redeem one before void.
        crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &codes[0]).await?;

        // Void the batch.
        void_voucher_batch(&pool, &batch.id).await?;

        // Should still be able to void again? No - already voided.
        assert!(matches!(
            void_voucher_batch(&pool, &batch.id).await,
            Err(AppError::Conflict(_))
        ));

        // Already redeemed card is unaffected (still redeemed).
        let status: String = sqlx::query_scalar("SELECT status FROM vouchers WHERE code = ?")
            .bind(&codes[0])
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "redeemed");

        let persisted_statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM vouchers WHERE batch_id = ? ORDER BY code")
                .bind(&batch.id)
                .fetch_all(&pool)
                .await?;
        assert_eq!(
            persisted_statuses
                .iter()
                .filter(|status| status.as_str() == "voided")
                .count(),
            2
        );

        // Voided card fails with a distinct denial and no balance change.
        let balance_before: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert!(matches!(
            crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &codes[1]).await,
            Err(AppError::Conflict(message)) if message == "voucher is from a voided batch"
        ));
        let balance_after: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        let denial_reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason') FROM audit_log
             WHERE event_type = 'voucher_rejected' ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance_after, balance_before);
        assert_eq!(denial_reason, "voided");

        // No separate voucher_sold event was written (batch-sold cash is in redeem event).
        let voucher_sold_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_sold'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            voucher_sold_count, 0,
            "no separate voucher_sold event for batch-sold"
        );

        // Check void audit event.
        let audit: (String, String, i64, i64, String) = sqlx::query_as(
            "SELECT event_type, summary, cash_flag,
                    json_extract(metadata_json, '$.active_cards_voided'), id
             FROM audit_log WHERE event_type = 'voucher_batch_voided'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit.0, "voucher_batch_voided");
        assert_eq!(
            audit.1,
            format!(
                "Voucher batch {} (VOID) voided (2 active cards)",
                batch.name
            )
        );
        assert_eq!(audit.2, 0);
        assert_eq!(audit.3, 2);
        let void_event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_batch_voided'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(void_event_count, 1);
        let linked_event_id: String =
            sqlx::query_scalar("SELECT voided_audit_event_id FROM voucher_batches WHERE id = ?")
                .bind(&batch.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(linked_event_id, audit.4);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_void_writes_one_audit_event() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("batch-void-race").await;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "RACE".to_owned(),
                suffix_length: Some(8),
                count: 2,
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;

        let first = void_voucher_batch(&pool, &batch.id);
        let second = void_voucher_batch(&pool, &batch.id);
        let (first_result, second_result) = tokio::join!(first, second);

        assert!(first_result.is_ok() ^ second_result.is_ok());
        let failure = if first_result.is_err() {
            first_result.expect_err("first void should fail")
        } else {
            second_result.expect_err("second void should fail")
        };
        assert!(matches!(failure, AppError::Conflict(_)));
        let event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_batch_voided'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(event_count, 1);

        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_redeem_and_void_pays_out_or_voids_once() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("batch-redeem-void-race").await;
        let account = test_account(&pool, "batch-redeem-void-race").await?;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "RACE".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 900,
                points_value_centi: 3,
                cash_amount_cents: Some(0),
                expires_at: None,
            },
        )
        .await?;
        let code: String = sqlx::query_scalar("SELECT code FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_one(&pool)
            .await?;

        let redeem = crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &code);
        let void = void_voucher_batch(&pool, &batch.id);
        let (redeem_result, void_result) = tokio::join!(redeem, void);
        assert!(redeem_result.is_ok() || void_result.is_ok());

        let status: String = sqlx::query_scalar("SELECT status FROM vouchers WHERE code = ?")
            .bind(&code)
            .fetch_one(&pool)
            .await?;
        let balance: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        let redemption_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'promo_voucher_redeemed' AND outcome = 'success'",
        )
        .fetch_one(&pool)
        .await?;

        match status.as_str() {
            "redeemed" => {
                assert_eq!(balance, (900, 3));
                assert_eq!(redemption_count, 1);
            }
            "voided" => {
                assert_eq!(balance, (0, 0));
                assert_eq!(redemption_count, 0);
            }
            other => panic!("race left unexpected voucher status {other}"),
        }

        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn existing_single_mint_voucher_still_works() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "legacy").await?;

        let voucher = crate::db::vouchers::create_voucher(
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

        assert_eq!(voucher.status, "active");
        assert!(voucher.batch_id.is_none());

        let mutation =
            crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &voucher.code).await?;
        assert_eq!(mutation.account.remaining_seconds, 300);
        Ok(())
    }

    #[tokio::test]
    async fn batch_sold_redemption_recognises_cash() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "cashbuyer").await?;

        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "CASH".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 3600,
                points_value_centi: 0,
                cash_amount_cents: Some(2000), // ₱20 card
                expires_at: None,
            },
        )
        .await?;

        let code: String = sqlx::query_scalar("SELECT code FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_one(&pool)
            .await?;

        // Redeem the batch-sold card.
        crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &code).await?;

        // Batch-sold redemption carries cash; no separate voucher_sold event.
        let batch_event: (Option<i64>, i64, i64, String) = sqlx::query_as(
            "SELECT amount_cents, seconds_delta, cash_flag,
                    json_extract(metadata_json, '$.batch_id')
             FROM audit_log WHERE event_type = 'sold_voucher_redeemed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(batch_event, (Some(2_000), 3_600, 1, batch.id));
        let batch_voucher_sold_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'voucher_sold'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(batch_voucher_sold_count, 0);

        // Single-mint sold cash was booked at mint, so redemption is non-cash.
        let single = crate::db::vouchers::create_voucher(
            &pool,
            &CreateVoucherInput {
                prefix: None,
                suffix_length: None,
                seconds_value: 600,
                points_value_centi: 0,
                sold_amount_cents: Some(500),
                expires_at: None,
            },
        )
        .await?;
        crate::db::vouchers::redeem_voucher(&pool, &account.id, "pc-01", &single.code).await?;
        let single_event: (Option<i64>, i64) = sqlx::query_as(
            "SELECT amount_cents, cash_flag FROM audit_log
             WHERE event_type = 'sold_voucher_redeemed'
             AND json_extract(metadata_json, '$.batch_id') IS NULL",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(single_event, (None, 0));
        let voucher_sold_cash: (i64, i64) = sqlx::query_as(
            "SELECT amount_cents, cash_flag FROM audit_log WHERE event_type = 'voucher_sold'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(voucher_sold_cash, (500, 1));

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_batch_redemption_pays_out_and_recognises_cash_once() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("batch-voucher-race").await;
        let first = test_account(&pool, "batch-race-first").await?;
        let second = test_account(&pool, "batch-race-second").await?;
        let batch = create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "RACE".to_owned(),
                suffix_length: Some(8),
                count: 1,
                seconds_value: 900,
                points_value_centi: 3,
                cash_amount_cents: Some(1_000),
                expires_at: None,
            },
        )
        .await?;
        let code: String = sqlx::query_scalar("SELECT code FROM vouchers WHERE batch_id = ?")
            .bind(&batch.id)
            .fetch_one(&pool)
            .await?;

        let first_redeem = crate::db::vouchers::redeem_voucher(&pool, &first.id, "pc-01", &code);
        let second_redeem = crate::db::vouchers::redeem_voucher(&pool, &second.id, "pc-02", &code);
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
        let cash_events: (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(amount_cents), 0) FROM audit_log
             WHERE event_type = 'sold_voucher_redeemed' AND cash_flag = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(cash_events, (1, 1_000));
        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn batch_audit_event_has_no_cash_flag() -> AppResult<()> {
        let pool = test_pool().await?;

        create_voucher_batch(
            &pool,
            &CreateVoucherBatchInput {
                prefix: "REV".to_owned(),
                suffix_length: Some(6),
                count: 1,
                seconds_value: 600,
                points_value_centi: 0,
                cash_amount_cents: Some(1000),
                expires_at: None,
            },
        )
        .await?;

        let audit: (i64, String) = sqlx::query_as(
            "SELECT cash_flag, event_type FROM audit_log
             WHERE event_type = 'voucher_batch_minted'",
        )
        .fetch_one(&pool)
        .await?;
        // cash_flag is false at mint, even for sold batches
        assert_eq!(audit.0, 0, "no cash at mint");

        Ok(())
    }
}

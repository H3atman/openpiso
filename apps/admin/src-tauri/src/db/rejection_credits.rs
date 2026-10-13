//! Rejection Credit (ADR-0069): the owner re-credits a coin the acceptor
//! physically took but never valued.
//!
//! Every credit is linked to one `coin_batch_rejected` Audit Log row, and both
//! its subjects are **frozen** at that row — the recorded Account if there was
//! one, otherwise that PC's Guest Time. Live occupancy is never read, so a
//! stale click cannot pay one customer's money to another.
//!
//! The credited value is computed by the same [`compute_coin_outcome`] the coin
//! path uses, so a Rejection Credit is worth exactly what the accepted coin
//! would have been worth: Shop Rate seconds, Points with the centicent carry,
//! and any exact-match Coin Promo bonus for an Account target; base Shop Rate
//! seconds only for a Guest target.

use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    pricing::{compute_coin_outcome, CoinOutcomeTarget},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    db::{attendance, attendance_earning},
    errors::{AppError, AppResult},
};

/// Rolling window a rejection stays creditable for. Matches the Notification
/// view (ADR-0058): the owner cannot act on evidence that is no longer visible.
const CREDITABLE_WINDOW_DAYS: i64 = 30;

/// Upper bound on one creditable-rejection page, matching the Notification
/// list's maximum so the two surfaces can never disagree about the window.
const MAX_REJECTION_ROWS: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectionListFilter<'a> {
    pub account_id: Option<&'a str>,
    pub creditable_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectionCreditInput {
    pub rejection_audit_id: String,
    pub denomination_cents: i64,
    pub coins: i64,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreditableRejection {
    pub rejection_audit_id: String,
    pub created_at: String,
    pub pc_id: Option<String>,
    pub pc_name: Option<String>,
    pub account_id: Option<String>,
    pub account_username: Option<String>,
    pub target_kind: String,
    pub account_deleted: bool,
    pub pulse_count: Option<i64>,
    pub cap_coins: i64,
    pub credited_coins: i64,
    pub remaining_coins: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreditableRejectionList {
    pub items: Vec<CreditableRejection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectionDenominations {
    pub denominations_cents: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectionCreditResult {
    pub dry_run: bool,
    pub rejection_audit_id: String,
    /// `None` on a dry run: nothing was written.
    pub audit_event_id: Option<String>,
    pub target_kind: String,
    pub account_id: Option<String>,
    pub account_username: Option<String>,
    pub pc_id: String,
    pub pc_name: String,
    pub denomination_cents: i64,
    pub coins: i64,
    pub amount_cents: i64,
    /// Total credited, including any Coin Promo bonus.
    pub seconds_added: i64,
    pub coin_promo_bonus_seconds: i64,
    pub points_earned_centi: i64,
    pub created_at: Option<String>,
}

/// The distinct denominations of the **current** Pulse Map, ascending.
///
/// This is the only legal source for a credit amount. The map's *counts* are
/// what failed; the denominations the shop accepts are still right, and an
/// historical rejection's pulse count has no automatic amount to prefill from.
pub async fn denominations(pool: &SqlitePool) -> AppResult<RejectionDenominations> {
    let denominations_cents = sqlx::query_scalar::<_, i64>(
        "SELECT DISTINCT amount_cents FROM coin_device_pulse_map ORDER BY amount_cents",
    )
    .fetch_all(pool)
    .await?;
    Ok(RejectionDenominations {
        denominations_cents,
    })
}

/// Coin Batch Rejections inside the rolling window, newest first.
///
/// `account_id` narrows to rejections whose **recorded** Account is that
/// account. Rows that are fully credited stay listed unless `creditable_only`
/// is set: the Notification list is append-only evidence, so a credited
/// rejection must still render its "Credited" marker.
pub async fn list_rejections(
    pool: &SqlitePool,
    filter: RejectionListFilter<'_>,
) -> AppResult<CreditableRejectionList> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        ),
    >(
        "SELECT a.id, a.created_at, a.pc_id, p.name AS pc_name,
                a.account_id, acct.username, acct.status, a.metadata_json,
                COALESCE(
                  (SELECT SUM(r.coins_credited) FROM rejection_credits r
                   WHERE r.rejection_audit_id = a.id),
                  0
                ) AS credited_coins
         FROM audit_log a
         LEFT JOIN pcs p ON p.id = a.pc_id
         LEFT JOIN accounts acct ON acct.id = a.account_id
         WHERE a.event_type = 'coin_batch_rejected'
           AND a.created_at >= datetime(CURRENT_TIMESTAMP, '-' || ? || ' days')
           AND (? IS NULL OR a.account_id = ?)
         ORDER BY a.created_at DESC, a.id DESC
         LIMIT ?",
    )
    .bind(CREDITABLE_WINDOW_DAYS)
    .bind(filter.account_id)
    .bind(filter.account_id)
    .bind(MAX_REJECTION_ROWS)
    .fetch_all(pool)
    .await?;

    let items = rows
        .into_iter()
        .map(
            |(
                rejection_audit_id,
                created_at,
                pc_id,
                pc_name,
                account_id,
                account_username,
                account_status,
                metadata_json,
                credited_coins,
            )| {
                let (pulse_count, cap_coins) = cap_from_metadata(metadata_json.as_deref());
                CreditableRejection {
                    rejection_audit_id,
                    created_at,
                    pc_id,
                    pc_name,
                    target_kind: if account_id.is_some() {
                        "account"
                    } else {
                        "guest"
                    }
                    .to_owned(),
                    account_deleted: account_id.is_some()
                        && account_status.as_deref() != Some("active"),
                    account_id,
                    account_username,
                    pulse_count,
                    cap_coins,
                    credited_coins,
                    remaining_coins: (cap_coins - credited_coins).max(0),
                }
            },
        )
        .filter(|item| !filter.creditable_only || item.remaining_coins > 0)
        .collect();
    Ok(CreditableRejectionList { items })
}

/// How many coins one rejection row may ever be credited for, and the observed
/// pulse count when it is a per-batch rejection.
///
/// A per-batch rejection is exactly one coin. A closing suppression summary
/// (ADR-0064) reports N suppressed batches and is creditable up to that count,
/// in one or more credits — which is precisely the jamming-acceptor case that
/// would otherwise be uncreditable, because individual rows were never emitted.
fn cap_from_metadata(metadata_json: Option<&str>) -> (Option<i64>, i64) {
    let metadata = metadata_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .unwrap_or(serde_json::Value::Null);
    let pulse_count = metadata
        .get("pulse_count")
        .and_then(serde_json::Value::as_i64);
    if pulse_count.is_some() {
        return (pulse_count, 1);
    }
    let suppressed = metadata
        .get("suppressed_count")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        .max(0);
    (None, suppressed)
}

/// Credit a rejected coin to its frozen target.
///
/// `dry_run` computes the identical outcome and rolls back, so the owner's
/// confirmation dialog previews the exact seconds and points the server will
/// credit rather than recomputing pricing in the browser.
pub async fn create(
    pool: &SqlitePool,
    input: &RejectionCreditInput,
) -> AppResult<RejectionCreditResult> {
    let rejection_audit_id = input.rejection_audit_id.trim();
    if rejection_audit_id.is_empty() {
        return Err(AppError::NotFound("coin batch rejection".to_owned()));
    }
    if input.coins <= 0 {
        return Err(AppError::InvalidInput(
            "coin count must be positive".to_owned(),
        ));
    }
    if input.denomination_cents <= 0 {
        return Err(AppError::InvalidInput(
            "denomination must be positive".to_owned(),
        ));
    }

    let mut tx = super::begin_immediate(pool).await?;
    let credit_at = chrono::Utc::now();

    // The denomination must be one the current Pulse Map actually maps. The
    // owner never types a peso amount.
    let mapped = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM coin_device_pulse_map WHERE amount_cents = ?",
    )
    .bind(input.denomination_cents)
    .fetch_one(&mut *tx)
    .await?;
    if mapped == 0 {
        return Err(AppError::InvalidInput(
            "denomination is not in the current Pulse Map".to_owned(),
        ));
    }

    let rejection = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT event_type, pc_id, account_id, metadata_json
         FROM audit_log
         WHERE id = ?
           AND created_at >= datetime(CURRENT_TIMESTAMP, '-' || ? || ' days')",
    )
    .bind(rejection_audit_id)
    .bind(CREDITABLE_WINDOW_DAYS)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((event_type, pc_id, recorded_account_id, metadata_json)) = rejection else {
        return Err(AppError::NotFound("coin batch rejection".to_owned()));
    };
    if event_type != "coin_batch_rejected" {
        return Err(AppError::NotFound("coin batch rejection".to_owned()));
    }
    let Some(pc_id) = pc_id else {
        return Err(AppError::NotFound(
            "the rejection records no PC to credit".to_owned(),
        ));
    };

    let (pulse_count, cap_coins) = cap_from_metadata(metadata_json.as_deref());
    // The double-credit guard. Re-read inside this transaction so two
    // concurrent credits cannot both pass; there is no unique index, because a
    // suppression summary is creditable in parts.
    let credited_coins = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(coins_credited), 0) FROM rejection_credits
         WHERE rejection_audit_id = ?",
    )
    .bind(rejection_audit_id)
    .fetch_one(&mut *tx)
    .await?;
    if credited_coins + input.coins > cap_coins {
        return Err(AppError::InvalidInput(format!(
            "this rejection has {} of {cap_coins} coins left to credit",
            (cap_coins - credited_coins).max(0)
        )));
    }

    let pc_name = sqlx::query_scalar::<_, String>("SELECT name FROM pcs WHERE id = ?")
        .bind(&pc_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("PC '{pc_id}'")))?;

    // Attribution is frozen at the rejection. A recorded Account that has since
    // been deleted is refused rather than silently falling back to the PC's
    // Guest Time, which would hand a deleted customer's money to whoever sits
    // down next (ADR-0011 forfeits a deleted balance on purpose).
    let account = match recorded_account_id.as_deref() {
        Some(account_id) => {
            let found = sqlx::query_as::<_, (String, String, i64)>(
                "SELECT id, username, point_earn_carry_centicents
                 FROM accounts WHERE id = ? AND status = 'active'",
            )
            .bind(account_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(found) = found else {
                return Err(AppError::Conflict(
                    "the Account recorded on this rejection has been deleted".to_owned(),
                ));
            };
            Some(found)
        }
        None => None,
    };

    let (rate_amount_cents, rate_seconds) =
        super::rates::active_rate_in_transaction(&mut tx).await?;
    let earn_cents_per_point = sqlx::query_scalar::<_, i64>(
        "SELECT earn_cents_per_point FROM economy_settings WHERE id = 1",
    )
    .fetch_one(&mut *tx)
    .await?;
    let promo_bonus_seconds = sqlx::query_scalar::<_, i64>(
        "SELECT tier.bonus_seconds
         FROM coin_promo_settings settings
         JOIN coin_promo_tiers tier ON tier.amount_cents = ?
         WHERE settings.id = 1 AND settings.enabled = 1",
    )
    .bind(input.denomination_cents)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(0);

    // One outcome per coin, chaining the carry exactly as N accepted coins
    // would have. A Coin Promo tier matches one coin's amount, never a total.
    let mut seconds_added = 0_i64;
    let mut coin_promo_bonus_seconds = 0_i64;
    let mut points_earned_centi = 0_i64;
    let mut carry_centicents = account.as_ref().map(|(_, _, carry)| *carry);
    for _ in 0..input.coins {
        let target = match carry_centicents {
            Some(prior_carry_centicents) => CoinOutcomeTarget::Account {
                prior_carry_centicents,
            },
            None => CoinOutcomeTarget::Guest,
        };
        let outcome = compute_coin_outcome(
            input.denomination_cents,
            target,
            promo_bonus_seconds,
            earn_cents_per_point,
            rate_amount_cents,
            rate_seconds,
        )?;
        seconds_added += outcome.seconds_added;
        coin_promo_bonus_seconds += outcome.coin_promo_bonus_seconds;
        points_earned_centi += outcome.points_earned_centi;
        if carry_centicents.is_some() {
            carry_centicents = Some(outcome.new_carry_centicents);
        }
    }

    let amount_cents = input.denomination_cents * input.coins;
    let target_kind = if account.is_some() {
        "account"
    } else {
        "guest"
    };
    let mut result = RejectionCreditResult {
        dry_run: input.dry_run,
        rejection_audit_id: rejection_audit_id.to_owned(),
        audit_event_id: None,
        target_kind: target_kind.to_owned(),
        account_id: account.as_ref().map(|(id, _, _)| id.clone()),
        account_username: account.as_ref().map(|(_, username, _)| username.clone()),
        pc_id: pc_id.clone(),
        pc_name,
        denomination_cents: input.denomination_cents,
        coins: input.coins,
        amount_cents,
        seconds_added,
        coin_promo_bonus_seconds,
        points_earned_centi,
        created_at: None,
    };

    if input.dry_run {
        tx.rollback().await?;
        return Ok(result);
    }

    match account.as_ref() {
        Some((account_id, _, _)) => {
            sqlx::query(
                "UPDATE accounts
                 SET remaining_seconds = remaining_seconds + ?,
                     points_balance_centi = points_balance_centi + ?,
                     point_earn_carry_centicents = ?,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ? AND status = 'active'",
            )
            .bind(seconds_added)
            .bind(points_earned_centi)
            .bind(carry_centicents.unwrap_or(0))
            .bind(account_id)
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query(
                "UPDATE pcs
                 SET remaining_seconds = remaining_seconds + ?, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?",
            )
            .bind(seconds_added)
            .bind(&pc_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    let summary = match account.as_ref() {
        Some((_, username, _)) => format!(
            "Credited {} rejected coin(s) to Account {username}",
            input.coins
        ),
        None => format!(
            "Credited {} rejected coin(s) to {} Guest Time",
            input.coins, result.pc_name
        ),
    };
    let mut event = NewAuditEvent::simple(
        "rejection_credit",
        AuditOutcome::Success,
        AuditActor::Admin,
        &summary,
    );
    event.pc_id = Some(&pc_id);
    event.account_id = account.as_ref().map(|(id, _, _)| id.as_str());
    event.amount_cents = Some(amount_cents);
    event.seconds_delta = Some(seconds_added);
    event.points_delta_centi = Some(points_earned_centi);
    // The coin is physically in the coin box, so this is Cash Revenue
    // (ADR-0014, ADR-0069) — not Free Time Dispensed. Only the promo split
    // reaches the cost line, through the `rejection_credits` detail row.
    event.cash_flag = true;
    event.metadata = json!({
        "rejection_audit_id": rejection_audit_id,
        "pulse_count": pulse_count,
        "denomination_cents": input.denomination_cents,
        "coins": input.coins,
        "coin_promo_bonus_seconds": coin_promo_bonus_seconds,
        "target_kind": target_kind,
    });
    let audit_event_id = audit::append(&mut tx, event).await?;

    // Disabled Attendance must preserve the legacy credit path without even
    // reading the original rejection timestamp. Otherwise qualification uses
    // the frozen Account and the settings/Shop Hours evidence at that instant.
    if let Some((account_id, _, _)) = account.as_ref() {
        if attendance::enabled_in_transaction(&mut tx).await? {
            let offset = attendance::host_offset();
            let original_rejection_at =
                sqlx::query_scalar::<_, String>("SELECT created_at FROM audit_log WHERE id = ?")
                    .bind(rejection_audit_id)
                    .fetch_one(&mut *tx)
                    .await?;
            // audit_log.created_at stores SQLite CURRENT_TIMESTAMP (UTC, no zone).
            let original_rejection_at = chrono::NaiveDateTime::parse_from_str(
                &original_rejection_at,
                "%Y-%m-%d %H:%M:%S%.f",
            )
            .map_err(|error| {
                AppError::InvalidInput(format!("invalid stored rejection instant: {error}"))
            })?
            .and_utc();
            if let Some(qualification) =
                attendance::qualify_rejection(&mut tx, original_rejection_at, credit_at, offset)
                    .await?
            {
                attendance_earning::record_earning(
                    &mut tx,
                    attendance_earning::EarningInput {
                        account_id,
                        source_audit_event_id: &audit_event_id,
                        source_kind: attendance::SourceKind::RejectionCredit,
                        cents: amount_cents,
                        source_at: original_rejection_at,
                        offset,
                        earning_date: qualification.earning_date,
                        award: qualification.award,
                        original_rejection_audit_event_id: Some(rejection_audit_id),
                        trigger_pc_id: Some(&pc_id),
                    },
                )
                .await?;
            }
        }
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO rejection_credits
         (id, rejection_audit_id, audit_event_id, pc_id, account_id, target_kind,
          denomination_cents, coins_credited, seconds_added,
          coin_promo_bonus_seconds, points_earned_centi)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(rejection_audit_id)
    .bind(&audit_event_id)
    .bind(&pc_id)
    .bind(
        account
            .as_ref()
            .map(|(account_id, _, _)| account_id.as_str()),
    )
    .bind(target_kind)
    .bind(input.denomination_cents)
    .bind(input.coins)
    .bind(seconds_added)
    .bind(coin_promo_bonus_seconds)
    .bind(points_earned_centi)
    .execute(&mut *tx)
    .await?;

    let created_at =
        sqlx::query_scalar::<_, String>("SELECT created_at FROM rejection_credits WHERE id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;

    result.audit_event_id = Some(audit_event_id);
    result.created_at = Some(created_at);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts::AccountCreator, test_utils};
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        super::super::run_migrations(&pool, test_utils::migration_backup_dir().path()).await?;
        super::super::seed(&pool).await?;
        Ok(pool)
    }

    async fn insert_rejection(
        pool: &SqlitePool,
        id: &str,
        account_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, account_id, pc_id, summary, metadata_json)
             VALUES (?, 'coin_batch_rejected', 'failure', 'system', ?, 'pc-01', 'rejected', ?)",
        )
        .bind(id)
        .bind(account_id)
        .bind(metadata.to_string())
        .execute(pool)
        .await?;
        Ok(())
    }

    fn input(rejection_audit_id: &str, coins: i64) -> RejectionCreditInput {
        RejectionCreditInput {
            rejection_audit_id: rejection_audit_id.to_owned(),
            denomination_cents: 100,
            coins,
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn guest_credit_matches_an_accepted_guest_coin() -> AppResult<()> {
        let pool = test_pool().await?;
        insert_rejection(&pool, "rej-guest", None, json!({ "pulse_count": 11 })).await?;

        let credit = create(&pool, &input("rej-guest", 1)).await?;
        assert_eq!(credit.target_kind, "guest");
        assert_eq!(credit.seconds_added, 300);
        assert_eq!(credit.points_earned_centi, 0);
        assert_eq!(credit.coin_promo_bonus_seconds, 0);
        assert_eq!(credit.amount_cents, 100);

        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 300);
        // Cash Revenue, never a coin transaction.
        let (cash_flag, event_type): (i64, String) = sqlx::query_as(
            "SELECT cash_flag, event_type FROM audit_log WHERE event_type = 'rejection_credit'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(cash_flag, 1);
        assert_eq!(event_type, "rejection_credit");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM coin_transactions")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn disabled_attendance_credit_skips_original_timestamp_parsing() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = crate::accounts::create(
            &pool,
            "disabled-attendance-credit",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        sqlx::query(
            "INSERT INTO audit_log
               (id, event_type, outcome, actor, account_id, pc_id, summary,
                metadata_json, created_at)
             VALUES ('rej-disabled-malformed-time', 'coin_batch_rejected', 'failure',
                     'system', ?, 'pc-01', 'rejected', '{\"pulse_count\":11}',
                     '9999-not-an-instant')",
        )
        .bind(&account.id)
        .execute(&pool)
        .await?;

        let credit = create(&pool, &input("rej-disabled-malformed-time", 1)).await?;
        assert_eq!(credit.target_kind, "account");
        assert_eq!(credit.amount_cents, 100);
        assert!(credit.audit_event_id.is_some());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_contributions")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn account_credit_earns_points_and_coin_promo() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = crate::accounts::create(
            &pool,
            "rejected-customer",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        sqlx::query("INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (100, 120)")
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE coin_promo_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        insert_rejection(
            &pool,
            "rej-account",
            Some(&account.id),
            json!({ "pulse_count": 11 }),
        )
        .await?;

        let credit = create(&pool, &input("rej-account", 1)).await?;
        assert_eq!(credit.target_kind, "account");
        assert_eq!(credit.seconds_added, 420);
        assert_eq!(credit.coin_promo_bonus_seconds, 120);
        assert!(credit.points_earned_centi > 0);

        let (seconds, points): (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(seconds, 420);
        assert_eq!(points, credit.points_earned_centi);

        // The promo split — and only the promo split — reaches the cost line.
        let free_time: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(seconds_delta), 0) FROM report_free_time_dispensed",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(free_time, 120);
        let cash: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(amount_cents), 0) FROM report_cash_revenue")
                .fetch_one(&pool)
                .await?;
        assert_eq!(cash, 100);
        Ok(())
    }

    #[tokio::test]
    async fn dry_run_previews_without_writing() -> AppResult<()> {
        let pool = test_pool().await?;
        insert_rejection(&pool, "rej-preview", None, json!({ "pulse_count": 11 })).await?;

        let preview = create(
            &pool,
            &RejectionCreditInput {
                dry_run: true,
                ..input("rej-preview", 1)
            },
        )
        .await?;
        assert_eq!(preview.seconds_added, 300);
        assert!(preview.audit_event_id.is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rejection_credits")
                .fetch_one(&pool)
                .await?,
            0
        );
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }

    #[tokio::test]
    async fn count_cap_bounds_batches_and_summaries() -> AppResult<()> {
        let pool = test_pool().await?;
        insert_rejection(&pool, "rej-once", None, json!({ "pulse_count": 11 })).await?;
        create(&pool, &input("rej-once", 1)).await?;
        assert!(matches!(
            create(&pool, &input("rej-once", 1)).await,
            Err(AppError::InvalidInput(_))
        ));

        // A suppression summary is creditable in parts, up to its count.
        insert_rejection(
            &pool,
            "rej-summary",
            None,
            json!({ "pulse_count": serde_json::Value::Null, "suppressed_count": 3 }),
        )
        .await?;
        create(&pool, &input("rej-summary", 2)).await?;
        assert!(matches!(
            create(&pool, &input("rej-summary", 2)).await,
            Err(AppError::InvalidInput(_))
        ));
        create(&pool, &input("rej-summary", 1)).await?;

        let list = list_rejections(
            &pool,
            RejectionListFilter {
                account_id: None,
                creditable_only: true,
            },
        )
        .await?;
        assert!(list.items.is_empty(), "nothing is creditable once capped");
        Ok(())
    }

    #[tokio::test]
    async fn deleted_recorded_account_is_refused() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = crate::accounts::create(
            &pool,
            "gone-customer",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        insert_rejection(
            &pool,
            "rej-deleted",
            Some(&account.id),
            json!({ "pulse_count": 11 }),
        )
        .await?;
        sqlx::query("UPDATE accounts SET status = 'deleted' WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        assert!(matches!(
            create(&pool, &input("rej-deleted", 1)).await,
            Err(AppError::Conflict(_))
        ));
        let listed = list_rejections(
            &pool,
            RejectionListFilter {
                account_id: None,
                creditable_only: false,
            },
        )
        .await?;
        assert!(listed.items[0].account_deleted);
        // Never falls back to the PC's Guest Time.
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }

    #[tokio::test]
    async fn amount_must_come_from_the_current_pulse_map() -> AppResult<()> {
        let pool = test_pool().await?;
        insert_rejection(&pool, "rej-amount", None, json!({ "pulse_count": 11 })).await?;
        assert!(matches!(
            create(
                &pool,
                &RejectionCreditInput {
                    denomination_cents: 2500,
                    ..input("rej-amount", 1)
                }
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        assert_eq!(
            denominations(&pool).await?.denominations_cents,
            vec![100, 500, 1000, 2000]
        );
        Ok(())
    }

    #[tokio::test]
    async fn only_rejection_rows_inside_the_window_are_creditable() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, pc_id, summary)
             VALUES ('not-a-rejection', 'coin_accepted', 'success', 'coin_device', 'pc-01', 'x')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO audit_log
             (id, event_type, outcome, actor, pc_id, summary, metadata_json, created_at)
             VALUES ('rej-stale', 'coin_batch_rejected', 'failure', 'system', 'pc-01', 'old',
                     '{\"pulse_count\":11}', datetime(CURRENT_TIMESTAMP, '-31 days'))",
        )
        .execute(&pool)
        .await?;

        assert!(matches!(
            create(&pool, &input("rej-stale", 1)).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            create(&pool, &input("not-a-rejection", 1)).await,
            Err(AppError::NotFound(_))
        ));
        let listed = list_rejections(
            &pool,
            RejectionListFilter {
                account_id: None,
                creditable_only: false,
            },
        )
        .await?;
        assert!(listed.items.is_empty());
        Ok(())
    }
}

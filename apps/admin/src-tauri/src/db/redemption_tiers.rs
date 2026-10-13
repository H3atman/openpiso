use std::collections::HashSet;

use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::{Sqlite, SqlitePool};

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{
        Account, AccountBalanceMutation, RedemptionTier, RedemptionTierInput, RedemptionTierMenu,
        RedemptionTiersInput,
    },
};

const MIN_TIERS: usize = 1;
const MAX_TIERS: usize = 20;
const MIN_POINTS: i64 = 5;
const POINTS_STEP: i64 = 5;
const SECONDS_PER_MINUTE: i64 = 60;

fn validated_rows(input: &RedemptionTiersInput) -> AppResult<Vec<(i64, i64)>> {
    if !(MIN_TIERS..=MAX_TIERS).contains(&input.tiers.len()) {
        return Err(AppError::InvalidInput(
            "redemption tiers must contain between 1 and 20 rows".to_owned(),
        ));
    }

    let mut points_seen = HashSet::with_capacity(input.tiers.len());
    let mut rows = Vec::with_capacity(input.tiers.len());
    for RedemptionTierInput { points, minutes } in &input.tiers {
        if *points < MIN_POINTS || points % POINTS_STEP != 0 {
            return Err(AppError::InvalidInput(
                "tier points must be at least 5 and a multiple of 5".to_owned(),
            ));
        }
        if !points_seen.insert(*points) {
            return Err(AppError::InvalidInput(
                "tier points must be unique".to_owned(),
            ));
        }
        if *minutes < 1 {
            return Err(AppError::InvalidInput(
                "tier minutes must be at least 1".to_owned(),
            ));
        }
        let seconds = minutes
            .checked_mul(SECONDS_PER_MINUTE)
            .ok_or_else(|| AppError::InvalidInput("tier minutes are too large".to_owned()))?;
        rows.push((*points, seconds));
    }
    rows.sort_unstable_by_key(|row| row.0);
    Ok(rows)
}

pub async fn redemption_tiers(pool: &SqlitePool) -> AppResult<Vec<RedemptionTier>> {
    Ok(sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT id, points, seconds FROM redemption_tiers ORDER BY points ASC",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(id, points, seconds)| RedemptionTier {
        id,
        points,
        seconds,
    })
    .collect())
}

fn most_generous(tiers: Vec<RedemptionTier>) -> AppResult<RedemptionTier> {
    tiers
        .into_iter()
        .max_by(|left, right| {
            (i128::from(left.seconds) * i128::from(right.points))
                .cmp(&(i128::from(right.seconds) * i128::from(left.points)))
                .then_with(|| right.points.cmp(&left.points))
        })
        .ok_or_else(|| AppError::NotFound("redemption tier".to_owned()))
}

pub async fn most_generous_redemption_tier(pool: &SqlitePool) -> AppResult<RedemptionTier> {
    most_generous(redemption_tiers(pool).await?)
}

pub(super) async fn most_generous_redemption_tier_in_transaction(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> AppResult<RedemptionTier> {
    let tiers = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT id, points, seconds FROM redemption_tiers ORDER BY points ASC",
    )
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .map(|(id, points, seconds)| RedemptionTier {
        id,
        points,
        seconds,
    })
    .collect();
    most_generous(tiers)
}

pub async fn redemption_tier_menu(pool: &SqlitePool) -> AppResult<RedemptionTierMenu> {
    let version = sqlx::query_scalar("SELECT version FROM redemption_tier_menu WHERE id = 1")
        .fetch_one(pool)
        .await?;
    Ok(RedemptionTierMenu {
        version,
        tiers: redemption_tiers(pool).await?,
    })
}

pub async fn redeem_tier(
    pool: &SqlitePool,
    account_id: &str,
    pc_id: &str,
    tier_id: i64,
    version: i64,
) -> AppResult<AccountBalanceMutation> {
    let mut transaction = super::begin_immediate(pool).await?;
    let current_version: i64 =
        sqlx::query_scalar("SELECT version FROM redemption_tier_menu WHERE id = 1")
            .fetch_one(&mut *transaction)
            .await?;
    if version != current_version {
        return Err(AppError::Conflict(
            "redemption tier menu is stale; refresh tiers and try again".to_owned(),
        ));
    }

    let tier = sqlx::query_as::<_, (i64, i64)>(
        "SELECT points, seconds FROM redemption_tiers WHERE id = ?",
    )
    .bind(tier_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("redemption tier '{tier_id}'")))?;
    let points_centi = tier
        .0
        .checked_mul(openpiso_core::pricing::CENTIPOINTS_PER_POINT)
        .ok_or_else(|| AppError::InvalidInput("redemption tier points are too large".to_owned()))?;

    let updated = sqlx::query_as::<
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
         SET points_balance_centi = points_balance_centi - ?,
             remaining_seconds = remaining_seconds + ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active' AND points_balance_centi >= ?
         RETURNING id, username, remaining_seconds, points_balance_centi, status, created_at,
                   deleted_at, imported_from, imported_at,
                   imported_from IS NOT NULL AND password_hash LIKE '$2%$%'",
    )
    .bind(points_centi)
    .bind(tier.1)
    .bind(account_id)
    .bind(points_centi)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(account) = updated.map(Account::from) else {
        let mut event = NewAuditEvent::simple(
            "redeem_denied",
            AuditOutcome::Denied,
            AuditActor::Account,
            "Points redemption denied",
        );
        event.actor_account_id = Some(account_id);
        event.account_id = Some(account_id);
        event.pc_id = Some(pc_id);
        event.points_delta_centi = Some(-points_centi);
        event.metadata = json!({
            "reason": "insufficient_points",
            "tier_id": tier_id,
            "menu_version": version,
        });
        audit::append(&mut transaction, event).await?;
        transaction.commit().await?;
        return Err(AppError::Conflict("insufficient points".to_owned()));
    };

    let mut event = NewAuditEvent::simple(
        "points_redeemed",
        AuditOutcome::Success,
        AuditActor::Account,
        "Redemption Tier redeemed for guaranteed time",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = Some(pc_id);
    event.seconds_delta = Some(tier.1);
    event.points_delta_centi = Some(-points_centi);
    event.cash_flag = false;
    event.metadata = json!({
        "tier_id": tier_id,
        "menu_version": version,
        "points": tier.0,
        "seconds": tier.1,
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(AccountBalanceMutation { account })
}

pub async fn update_redemption_tiers(
    pool: &SqlitePool,
    input: &RedemptionTiersInput,
) -> AppResult<Vec<RedemptionTier>> {
    let rows = validated_rows(input)?;
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query("DELETE FROM redemption_tiers")
        .execute(&mut *transaction)
        .await?;
    for (points, seconds) in &rows {
        sqlx::query("INSERT INTO redemption_tiers (points, seconds) VALUES (?, ?)")
            .bind(points)
            .bind(seconds)
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query("UPDATE redemption_tier_menu SET version = version + 1 WHERE id = 1")
        .execute(&mut *transaction)
        .await?;

    let mut event = NewAuditEvent::simple(
        "redemption_tiers_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Redemption Tiers",
    );
    event.metadata = json!({
        "tiers": rows.iter().map(|(points, seconds)| json!({
            "points": points,
            "seconds": seconds,
        })).collect::<Vec<_>>()
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    redemption_tiers(pool).await
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::accounts::{self, AccountCreator};

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    fn input(tiers: &[(i64, i64)]) -> RedemptionTiersInput {
        RedemptionTiersInput {
            tiers: tiers
                .iter()
                .map(|(points, minutes)| RedemptionTierInput {
                    points: *points,
                    minutes: *minutes,
                })
                .collect(),
        }
    }

    #[test]
    fn validation_accepts_all_boundaries() {
        assert!(validated_rows(&input(&[(5, 1)])).is_ok());
        let twenty = (1..=20).map(|value| (value * 5, 1)).collect::<Vec<_>>();
        assert!(validated_rows(&input(&twenty)).is_ok());
        assert!(validated_rows(&input(&[(i64::MAX - 2, 1)])).is_ok());
        assert!(validated_rows(&input(&[(5, i64::MAX / 60)])).is_ok());
    }

    #[test]
    fn validation_rejects_every_rule_and_overflow() {
        assert!(validated_rows(&input(&[])).is_err());
        let twenty_one = (1..=21).map(|value| (value * 5, 1)).collect::<Vec<_>>();
        assert!(validated_rows(&input(&twenty_one)).is_err());
        assert!(validated_rows(&input(&[(0, 1)])).is_err());
        assert!(validated_rows(&input(&[(4, 1)])).is_err());
        assert!(validated_rows(&input(&[(6, 1)])).is_err());
        assert!(validated_rows(&input(&[(5, 1), (5, 2)])).is_err());
        assert!(validated_rows(&input(&[(5, 0)])).is_err());
        assert!(validated_rows(&input(&[(5, -1)])).is_err());
        assert!(validated_rows(&input(&[(5, i64::MAX)])).is_err());
    }

    #[tokio::test]
    async fn replacement_persists_sorted_seconds_and_is_atomic_on_invalid_input() -> AppResult<()> {
        let pool = test_pool().await?;
        let saved = update_redemption_tiers(&pool, &input(&[(20, 120), (5, 1), (10, 60)])).await?;
        assert_eq!(
            saved,
            vec![
                RedemptionTier {
                    id: saved[0].id,
                    points: 5,
                    seconds: 60
                },
                RedemptionTier {
                    id: saved[1].id,
                    points: 10,
                    seconds: 3_600
                },
                RedemptionTier {
                    id: saved[2].id,
                    points: 20,
                    seconds: 7_200
                },
            ]
        );

        assert!(update_redemption_tiers(&pool, &input(&[(6, 1)]))
            .await
            .is_err());
        assert_eq!(redemption_tiers(&pool).await?, saved);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'redemption_tiers_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 1);
        assert_eq!(redemption_tier_menu(&pool).await?.version, 2);
        Ok(())
    }

    #[tokio::test]
    async fn most_generous_tier_uses_highest_seconds_per_point() -> AppResult<()> {
        let pool = test_pool().await?;
        let saved =
            update_redemption_tiers(&pool, &input(&[(5, 60), (10, 150), (20, 240), (25, 375)]))
                .await?;

        let most_generous = most_generous_redemption_tier(&pool).await?;
        assert_eq!(most_generous, saved[1]);

        let mut transaction = crate::db::begin_immediate(&pool).await?;
        assert_eq!(
            most_generous_redemption_tier_in_transaction(&mut transaction).await?,
            saved[1]
        );
        transaction.rollback().await?;
        Ok(())
    }

    #[tokio::test]
    async fn most_generous_tier_compares_ratios_without_integer_truncation() -> AppResult<()> {
        let pool = test_pool().await?;
        let saved = update_redemption_tiers(&pool, &input(&[(5, 1), (10, 3)])).await?;
        assert_eq!(most_generous_redemption_tier(&pool).await?, saved[1]);
        Ok(())
    }

    #[tokio::test]
    async fn tier_redemption_revalidates_menu_and_is_atomic() -> AppResult<()> {
        let pool = test_pool().await?;
        let account =
            accounts::create(&pool, "tier-user", "password", AccountCreator::Admin, None).await?;
        sqlx::query(
            "UPDATE accounts SET points_balance_centi = 2500, remaining_seconds = 10 WHERE id = ?",
        )
        .bind(&account.id)
        .execute(&pool)
        .await?;
        update_redemption_tiers(&pool, &input(&[(5, 1), (10, 3)])).await?;
        let menu = redemption_tier_menu(&pool).await?;
        let five = &menu.tiers[0];
        let ten = &menu.tiers[1];

        let first = redeem_tier(&pool, &account.id, "pc-01", five.id, menu.version).await?;
        assert_eq!(
            (
                first.account.points_balance_centi,
                first.account.remaining_seconds
            ),
            (2000, 70)
        );
        let second = redeem_tier(&pool, &account.id, "pc-01", ten.id, menu.version).await?;
        assert_eq!(
            (
                second.account.points_balance_centi,
                second.account.remaining_seconds
            ),
            (1000, 250)
        );
        let third = redeem_tier(&pool, &account.id, "pc-01", five.id, menu.version).await?;
        assert_eq!(
            (
                third.account.points_balance_centi,
                third.account.remaining_seconds
            ),
            (500, 310)
        );

        let before = (
            third.account.points_balance_centi,
            third.account.remaining_seconds,
        );
        assert!(matches!(
            redeem_tier(&pool, &account.id, "pc-01", ten.id, menu.version).await,
            Err(AppError::Conflict(message)) if message == "insufficient points"
        ));
        let denials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'redeem_denied' AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(denials, 1);
        assert!(matches!(
            redeem_tier(&pool, &account.id, "pc-01", 999_999, menu.version).await,
            Err(AppError::NotFound(message)) if message == "redemption tier '999999'"
        ));
        update_redemption_tiers(&pool, &input(&[(5, 2)])).await?;
        assert!(matches!(
            redeem_tier(&pool, &account.id, "pc-01", five.id, menu.version).await,
            Err(AppError::Conflict(message)) if message.contains("stale")
        ));

        let after: (i64, i64) = sqlx::query_as(
            "SELECT points_balance_centi, remaining_seconds FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(after, before);
        let logged: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'points_redeemed' AND account_id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(logged, 3);
        Ok(())
    }
}

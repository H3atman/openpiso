use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    pricing::{effective_bonus_basis_points, TierRate},
};
use serde_json::json;
use sqlx::{Sqlite, SqlitePool};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{EconomySettings, KioskConfig, ShopRate, ShopRateInput},
};

const MIN_SHOP_RATE_AMOUNT_CENTS: i64 = 100;
const MAX_SHOP_RATE_AMOUNT_CENTS: i64 = 100_000;
const MAX_SHOP_RATE_SECONDS: i64 = 86_400;

pub(crate) async fn active_rate<'e, E>(executor: E) -> AppResult<(i64, i64)>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT amount_cents, seconds FROM rates
         WHERE active = 1 ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::NotFound("active rate".to_owned()))
}

pub async fn shop_rate(pool: &SqlitePool) -> AppResult<ShopRate> {
    let (amount_cents, seconds) = active_rate(pool).await?;
    Ok(ShopRate {
        amount_cents,
        seconds,
    })
}

pub async fn update_shop_rate(pool: &SqlitePool, input: &ShopRateInput) -> AppResult<ShopRate> {
    validate_shop_rate(input)?;

    let mut transaction = super::begin_immediate(pool).await?;
    let before = sqlx::query_as::<_, (i64, i64)>(
        "SELECT amount_cents, seconds FROM rates
         WHERE active = 1 ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound("active rate".to_owned()))?;

    sqlx::query("UPDATE rates SET active = 0 WHERE active = 1")
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO rates (id, name, amount_cents, seconds, active)
         VALUES (?, ?, ?, ?, 1)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(derived_rate_name(input.amount_cents, input.seconds))
    .bind(input.amount_cents)
    .bind(input.seconds)
    .execute(&mut *transaction)
    .await?;

    let mut event = NewAuditEvent::simple(
        "shop_rate_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Shop Rate",
    );
    event.metadata = json!({
        "before": { "amount_cents": before.0, "seconds": before.1 },
        "after": { "amount_cents": input.amount_cents, "seconds": input.seconds },
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;

    Ok(ShopRate {
        amount_cents: input.amount_cents,
        seconds: input.seconds,
    })
}

fn validate_shop_rate(input: &ShopRateInput) -> AppResult<()> {
    if !(MIN_SHOP_RATE_AMOUNT_CENTS..=MAX_SHOP_RATE_AMOUNT_CENTS).contains(&input.amount_cents)
        || input.amount_cents % 100 != 0
    {
        return Err(AppError::InvalidInput(
            "Shop Rate amount must be a whole peso from 1 to 1,000".to_owned(),
        ));
    }
    if !(1..=MAX_SHOP_RATE_SECONDS).contains(&input.seconds) {
        return Err(AppError::InvalidInput(
            "Shop Rate seconds must be from 1 to 86,400".to_owned(),
        ));
    }
    Ok(())
}

fn derived_rate_name(amount_cents: i64, seconds: i64) -> String {
    let amount_pesos = amount_cents / 100;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let remaining_seconds = seconds % 60;
    let duration = if hours > 0 {
        format!("{hours} h {minutes} min {remaining_seconds} sec")
    } else if minutes > 0 {
        format!("{minutes} min {remaining_seconds} sec")
    } else {
        format!("{remaining_seconds} sec")
    };
    format!("₱{amount_pesos} = {duration}")
}

/// Read-only snapshot of the active time-purchase rate for the kiosk's
/// "equivalent time" preview (ADR-0008). Authority for committed time stays
/// in `record_coin_event`; this only lets the insert screen mirror the
/// owner's configured rate instead of a hard-coded constant.
pub async fn kiosk_config(pool: &SqlitePool) -> AppResult<KioskConfig> {
    let (rate_amount_cents, rate_seconds) = active_rate(pool).await?;
    Ok(KioskConfig {
        rate_amount_cents,
        rate_seconds,
    })
}

pub(super) async fn active_rate_in_transaction(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> AppResult<(i64, i64)> {
    active_rate(&mut **transaction).await
}

pub(super) fn economy_settings_from_values(
    earn_cents_per_point: i64,
    unsafe_bonus_threshold_basis_points: i64,
    rate_amount_cents: i64,
    rate_seconds: i64,
    tier: TierRate,
) -> AppResult<EconomySettings> {
    let effective_bonus_basis_points =
        effective_bonus_basis_points(rate_amount_cents, rate_seconds, earn_cents_per_point, tier)?;
    Ok(EconomySettings {
        earn_cents_per_point,
        rate_amount_cents,
        rate_seconds,
        effective_bonus_basis_points,
        unsafe_bonus_threshold_basis_points,
        unsafe_bonus: effective_bonus_basis_points > unsafe_bonus_threshold_basis_points,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

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

    #[tokio::test]
    async fn kiosk_config_mirrors_the_active_rate_and_tracks_edits() -> AppResult<()> {
        let pool = test_pool().await?;

        let config = kiosk_config(&pool).await?;
        assert_eq!(config.rate_amount_cents, 100);
        assert_eq!(config.rate_seconds, 300);

        let credited = super::super::simulate_coin(&pool, "pc-01", 250).await?;
        let preview = 250 * config.rate_seconds / config.rate_amount_cents;
        assert_eq!(credited.seconds_added, preview);

        sqlx::query("UPDATE rates SET seconds = 600 WHERE id = 'rate-default'")
            .execute(&pool)
            .await?;
        let updated = kiosk_config(&pool).await?;
        assert_eq!(updated.rate_amount_cents, 100);
        assert_eq!(updated.rate_seconds, 600);
        Ok(())
    }
}

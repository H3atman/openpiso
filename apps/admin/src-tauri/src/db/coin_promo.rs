use std::collections::HashSet;

use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    pricing::{calculate_seconds, coin_promo_effective_bonus_basis_points},
};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{
        CoinPromoMenu, CoinPromoMenuTier, CoinPromoSettings, CoinPromoSettingsInput, CoinPromoTier,
        CoinPromoTierInput,
    },
};

const SECONDS_PER_MINUTE: i64 = 60;

fn validated_rows(input: &CoinPromoSettingsInput) -> AppResult<Vec<(i64, i64)>> {
    let mut amounts = HashSet::with_capacity(input.tiers.len());
    let mut rows = Vec::with_capacity(input.tiers.len());
    for CoinPromoTierInput {
        amount_cents,
        bonus_minutes,
    } in &input.tiers
    {
        if *amount_cents <= 0 {
            return Err(AppError::InvalidInput(
                "Coin Promo Tier amounts must be positive".to_owned(),
            ));
        }
        if !amounts.insert(*amount_cents) {
            return Err(AppError::InvalidInput(
                "Coin Promo Tier amounts must be unique".to_owned(),
            ));
        }
        if *bonus_minutes <= 0 {
            return Err(AppError::InvalidInput(
                "Coin Promo Tier bonuses must be positive".to_owned(),
            ));
        }
        let bonus_seconds = bonus_minutes
            .checked_mul(SECONDS_PER_MINUTE)
            .ok_or_else(|| {
                AppError::InvalidInput("Coin Promo Tier bonus is too large".to_owned())
            })?;
        rows.push((*amount_cents, bonus_seconds));
    }
    rows.sort_unstable_by_key(|row| row.0);
    Ok(rows)
}

async fn settings_from_rows(
    pool: &SqlitePool,
    enabled: bool,
    rows: Vec<(i64, i64)>,
) -> AppResult<CoinPromoSettings> {
    let economy = super::settings::economy_settings(pool).await?;
    let mut highest_coin_promo_basis_points = 0_i64;
    let mut tiers = Vec::with_capacity(rows.len());
    for (amount_cents, bonus_seconds) in rows {
        let base_seconds = calculate_seconds(
            amount_cents,
            economy.rate_amount_cents,
            economy.rate_seconds,
        )
        .map_err(|_| AppError::InvalidInput("Coin Promo Tier amount is too large".to_owned()))?;
        base_seconds.checked_add(bonus_seconds).ok_or_else(|| {
            AppError::InvalidInput("Coin Promo Tier bonus is too large".to_owned())
        })?;
        let effective_bonus_basis_points = coin_promo_effective_bonus_basis_points(
            amount_cents,
            bonus_seconds,
            economy.rate_amount_cents,
            economy.rate_seconds,
        )
        .map_err(|_| AppError::InvalidInput("Coin Promo Tier bonus is too large".to_owned()))?;
        highest_coin_promo_basis_points =
            highest_coin_promo_basis_points.max(effective_bonus_basis_points);
        tiers.push(CoinPromoTier {
            amount_cents,
            bonus_seconds,
            effective_bonus_basis_points,
        });
    }
    let account_worst_case_basis_points = economy
        .effective_bonus_basis_points
        .checked_add(highest_coin_promo_basis_points)
        .ok_or_else(|| AppError::InvalidInput("Coin Promo bonus is too large".to_owned()))?;
    Ok(CoinPromoSettings {
        enabled,
        tiers,
        account_worst_case_basis_points,
        unsafe_bonus_threshold_basis_points: economy.unsafe_bonus_threshold_basis_points,
        unsafe_bonus: account_worst_case_basis_points > economy.unsafe_bonus_threshold_basis_points,
    })
}

pub async fn coin_promo_settings(pool: &SqlitePool) -> AppResult<CoinPromoSettings> {
    let enabled: bool =
        sqlx::query_scalar::<_, i64>("SELECT enabled FROM coin_promo_settings WHERE id = 1")
            .fetch_one(pool)
            .await?
            != 0;
    let rows = sqlx::query_as::<_, (i64, i64)>(
        "SELECT amount_cents, bonus_seconds FROM coin_promo_tiers ORDER BY amount_cents ASC",
    )
    .fetch_all(pool)
    .await?;
    settings_from_rows(pool, enabled, rows).await
}

pub async fn coin_promo_menu(pool: &SqlitePool) -> AppResult<CoinPromoMenu> {
    let settings = coin_promo_settings(pool).await?;
    let tiers = if settings.enabled {
        settings
            .tiers
            .into_iter()
            .map(|tier| CoinPromoMenuTier {
                amount_cents: tier.amount_cents,
                bonus_seconds: tier.bonus_seconds,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(CoinPromoMenu {
        enabled: settings.enabled,
        tiers,
    })
}

pub async fn preview_coin_promo_settings(
    pool: &SqlitePool,
    input: &CoinPromoSettingsInput,
) -> AppResult<CoinPromoSettings> {
    settings_from_rows(pool, input.enabled, validated_rows(input)?).await
}

pub async fn update_coin_promo_settings(
    pool: &SqlitePool,
    input: &CoinPromoSettingsInput,
) -> AppResult<CoinPromoSettings> {
    let rows = validated_rows(input)?;
    // Complete every economy/overflow check before opening the write transaction.
    settings_from_rows(pool, input.enabled, rows.clone()).await?;

    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query("DELETE FROM coin_promo_tiers")
        .execute(&mut *transaction)
        .await?;
    for (amount_cents, bonus_seconds) in &rows {
        sqlx::query("INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (?, ?)")
            .bind(amount_cents)
            .bind(bonus_seconds)
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query("UPDATE coin_promo_settings SET enabled = ? WHERE id = 1")
        .bind(input.enabled)
        .execute(&mut *transaction)
        .await?;

    let mut event = NewAuditEvent::simple(
        "coin_promo_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Coin Promo settings",
    );
    event.metadata = json!({
        "enabled": input.enabled,
        "tiers": rows.iter().map(|(amount_cents, bonus_seconds)| json!({
            "amount_cents": amount_cents,
            "bonus_seconds": bonus_seconds,
        })).collect::<Vec<_>>(),
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    coin_promo_settings(pool).await
}

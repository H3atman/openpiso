//! The one home of Coin Outcome application: deciding what coins are worth to
//! a frozen target and landing that on its balance.
//!
//! Both the Coin Batch path (`coins.rs`) and the Rejection Credit path
//! (`rejection_credits.rs`) go through [`apply`], so the two cannot price a
//! coin differently. Everything else about a coin event or a credit (the
//! Audit Event, Attendance, the detail row, the guards) stays with its caller.

use openpiso_core::pricing::{
    coin_promo_match, compute_coin_outcome, CoinOutcomeTarget, CoinPromoTierRate,
};
use sqlx::{Sqlite, Transaction};

use crate::errors::AppResult;

/// The balance a Coin Outcome lands on. Frozen by the caller: this module
/// never reads live occupancy.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BalanceTarget<'a> {
    Account { account_id: &'a str },
    GuestTime { pc_id: &'a str },
}

/// The totals written to the balance, plus the snapshot values callers record
/// afterwards.
#[derive(Debug, Clone)]
pub(crate) struct AppliedCoinOutcome {
    /// Total credited, including any Coin Promo bonus.
    pub seconds_added: i64,
    pub coin_promo_bonus_seconds: i64,
    pub points_earned_centi: i64,
    /// The part of the coin amounts a Coin Promo Tier covered. Reported for
    /// either target, though only an Account earns the bonus.
    pub coin_promo_covered_amount_cents: i64,
    pub earn_cents_per_point: i64,
    /// `None` for a Guest Time target.
    pub account_username: Option<String>,
    /// The Account's Points carry after the last coin. `None` for Guest Time.
    pub carry_centicents: Option<i64>,
}

/// The target Account is missing or not active. Each caller rewords this into
/// its own owner-facing error.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AccountNotActive;

/// Price `coin_amounts_cents` for `target` and write the balance once, inside
/// the caller's open transaction.
///
/// Snapshots the active Shop Rate, the earn rate and the enabled Coin Promo
/// tiers, then prices each coin on its own, as a one-coin Coin Batch, with the
/// Points carry chained from one coin to the next. The amounts are never
/// summed: a Coin Batch passes one amount (the train's pulse sum), a Rejection
/// Credit passes one amount per coin. To preview, call this and roll the
/// transaction back.
pub(crate) async fn apply(
    tx: &mut Transaction<'_, Sqlite>,
    target: BalanceTarget<'_>,
    coin_amounts_cents: impl IntoIterator<Item = i64>,
) -> AppResult<Result<AppliedCoinOutcome, AccountNotActive>> {
    let (rate_amount_cents, rate_seconds) = super::rates::active_rate_in_transaction(tx).await?;
    let coin_promo_tiers = sqlx::query_as::<_, (i64, i64)>(
        "SELECT tier.amount_cents, tier.bonus_seconds
         FROM coin_promo_settings settings
         JOIN coin_promo_tiers tier
         WHERE settings.id = 1 AND settings.enabled = 1",
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|(amount_cents, bonus_seconds)| CoinPromoTierRate {
        amount_cents,
        bonus_seconds,
    })
    .collect::<Vec<_>>();
    let earn_cents_per_point: i64 =
        sqlx::query_scalar("SELECT earn_cents_per_point FROM economy_settings WHERE id = 1")
            .fetch_one(&mut **tx)
            .await?;

    let mut applied = AppliedCoinOutcome {
        seconds_added: 0,
        coin_promo_bonus_seconds: 0,
        points_earned_centi: 0,
        coin_promo_covered_amount_cents: 0,
        earn_cents_per_point,
        account_username: None,
        carry_centicents: None,
    };
    if let BalanceTarget::Account { account_id } = target {
        let account = sqlx::query_as::<_, (String, i64)>(
            "SELECT username, point_earn_carry_centicents
             FROM accounts WHERE id = ? AND status = 'active'",
        )
        .bind(account_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some((username, carry_centicents)) = account else {
            return Ok(Err(AccountNotActive));
        };
        applied.account_username = Some(username);
        applied.carry_centicents = Some(carry_centicents);
    }

    for amount_cents in coin_amounts_cents {
        // The greedy decomposition of this one amount (ADR-0070): a coin with
        // no tier of its own still earns the smaller tiers it covers.
        let promo = coin_promo_match(amount_cents, &coin_promo_tiers)?;
        let outcome = compute_coin_outcome(
            amount_cents,
            match applied.carry_centicents {
                Some(prior_carry_centicents) => CoinOutcomeTarget::Account {
                    prior_carry_centicents,
                },
                None => CoinOutcomeTarget::Guest,
            },
            promo.bonus_seconds,
            earn_cents_per_point,
            rate_amount_cents,
            rate_seconds,
        )?;
        applied.seconds_added += outcome.seconds_added;
        applied.coin_promo_bonus_seconds += outcome.coin_promo_bonus_seconds;
        applied.points_earned_centi += outcome.points_earned_centi;
        applied.coin_promo_covered_amount_cents += promo.covered_amount_cents;
        if applied.carry_centicents.is_some() {
            applied.carry_centicents = Some(outcome.new_carry_centicents);
        }
    }

    match target {
        BalanceTarget::Account { account_id } => {
            sqlx::query(
                "UPDATE accounts
                 SET remaining_seconds = remaining_seconds + ?,
                     points_balance_centi = points_balance_centi + ?,
                     point_earn_carry_centicents = ?,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ? AND status = 'active'",
            )
            .bind(applied.seconds_added)
            .bind(applied.points_earned_centi)
            .bind(applied.carry_centicents.unwrap_or(0))
            .bind(account_id)
            .execute(&mut **tx)
            .await?;
        }
        BalanceTarget::GuestTime { pc_id } => {
            sqlx::query(
                "UPDATE pcs
                 SET remaining_seconds = remaining_seconds + ?, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?",
            )
            .bind(applied.seconds_added)
            .bind(pc_id)
            .execute(&mut **tx)
            .await?;
        }
    }

    Ok(Ok(applied))
}

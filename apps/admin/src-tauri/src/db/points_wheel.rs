use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    wheel::{SpinRng, WheelConfig, WheelError, WHEEL_SPIN_COST_CENTIPOINTS},
};
use serde_json::json;
use sqlx::{Sqlite, SqlitePool};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    models::{
        Account, ClientWheelSettings, OwnerWheelSegmentRow, WheelSegmentRow, WheelSettings,
        WheelSettingsInput, WheelSpinMutation, WheelSpinResult, WheelSpinStep,
    },
};

pub struct ThreadSpinRng;

impl SpinRng for ThreadSpinRng {
    fn gen_below(&mut self, bound: i64) -> i64 {
        rand::random_range(0..bound)
    }
}

fn wheel_error_to_app(error: WheelError) -> AppError {
    match error {
        WheelError::NegativeHouseEdge => AppError::Conflict(error.to_string()),
        other => AppError::InvalidInput(other.to_string()),
    }
}

fn segment_to_domain(row: &WheelSegmentRow) -> openpiso_core::wheel::WheelSegment {
    openpiso_core::wheel::WheelSegment {
        label: row.label.clone(),
        seconds: row.seconds,
        points_centi: row.points_centi,
        free_spin: row.free_spin,
    }
}

async fn load_wheel_config(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> AppResult<(WheelConfig, i64)> {
    let (enabled, bokya_rate_basis_points, free_spin_cap, _spin_duration, version) =
        sqlx::query_as::<_, (i64, Option<i64>, i64, i64, i64)>(
            "SELECT enabled, bokya_rate_basis_points, free_spin_cap, spin_duration, version
             FROM wheel_settings WHERE id = 1",
        )
        .fetch_one(&mut **transaction)
        .await?;
    let rows = sqlx::query_as::<_, (String, i64, i64, i64)>(
        "SELECT label, seconds, points_centi, free_spin FROM wheel_segments ORDER BY position, id",
    )
    .fetch_all(&mut **transaction)
    .await?;
    let mut segments = Vec::with_capacity(rows.len());
    for (label, seconds, points_centi, free_spin) in rows {
        segments.push(openpiso_core::wheel::WheelSegment {
            label,
            seconds,
            points_centi,
            free_spin: free_spin == 1,
        });
    }
    Ok((
        WheelConfig {
            enabled: enabled == 1,
            bokya_rate_basis_points,
            free_spin_cap,
            segments,
        },
        version,
    ))
}

async fn active_shop_rate_point_value_seconds(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> AppResult<i64> {
    let (rate_amount_cents, rate_seconds) =
        super::rates::active_rate_in_transaction(transaction).await?;
    let earn_cents_per_point: i64 =
        sqlx::query_scalar("SELECT earn_cents_per_point FROM economy_settings WHERE id = 1")
            .fetch_one(&mut **transaction)
            .await?;
    let point_value = i128::from(earn_cents_per_point)
        .checked_mul(i128::from(rate_seconds))
        .ok_or_else(|| AppError::InvalidInput("Shop Rate point value is too large".to_owned()))?
        / i128::from(rate_amount_cents);
    i64::try_from(point_value)
        .map_err(|_| AppError::InvalidInput("Shop Rate point value is too large".to_owned()))
}

fn wheel_settings_view(
    config: &WheelConfig,
    version: i64,
    spin_duration: i64,
    password_confirmation_required: bool,
    tier: crate::models::RedemptionTier,
    shop_rate_point_value_seconds: i64,
) -> AppResult<WheelSettings> {
    let house_edge_basis_points = config
        .house_edge_basis_points(tier.rate())
        .map_err(wheel_error_to_app)?;
    let point_value_seconds = config
        .point_value_seconds(tier.rate())
        .map_err(wheel_error_to_app)?;
    let shop_rate_house_edge_basis_points =
        config.house_edge_basis_points_against_point_value(shop_rate_point_value_seconds);
    let segments = config
        .segments
        .iter()
        .zip(config.segment_probability_basis_points())
        .map(|(segment, probability_basis_points)| OwnerWheelSegmentRow {
            label: segment.label.clone(),
            seconds: segment.seconds,
            points_centi: segment.points_centi,
            free_spin: segment.free_spin,
            probability_basis_points,
        })
        .collect();
    Ok(WheelSettings {
        enabled: config.enabled,
        bokya_rate_basis_points: config.bokya_rate_basis_points,
        free_spin_cap: config.free_spin_cap,
        spin_duration,
        password_confirmation_required,
        version,
        segments,
        expected_payout_seconds: config.expected_payout_seconds().round() as i64,
        expected_payout_points_centi: config.expected_payout_points_centi().round() as i64,
        point_value_seconds,
        house_edge_basis_points,
        unsafe_house_edge: house_edge_basis_points <= 0,
        shop_rate_point_value_seconds,
        shop_rate_house_edge_basis_points,
        unsafe_shop_rate_house_edge: shop_rate_house_edge_basis_points <= 0,
    })
}

/// Whether `POST /api/wheel/spin` must see the Account's password.
pub async fn wheel_password_confirmation_required(pool: &SqlitePool) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT password_confirmation_required FROM wheel_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?
        == 1)
}

pub async fn wheel_enabled(pool: &SqlitePool) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT enabled FROM wheel_settings WHERE id = 1")
            .fetch_one(pool)
            .await?
            == 1,
    )
}

fn segment_rows(config: &WheelConfig) -> Vec<WheelSegmentRow> {
    config
        .segments
        .iter()
        .map(|segment| WheelSegmentRow {
            label: segment.label.clone(),
            seconds: segment.seconds,
            points_centi: segment.points_centi,
            free_spin: segment.free_spin,
        })
        .collect()
}

pub async fn wheel_settings(pool: &SqlitePool) -> AppResult<WheelSettings> {
    let mut transaction = super::begin_immediate(pool).await?;
    let (config, version) = load_wheel_config(&mut transaction).await?;
    config.validate().map_err(wheel_error_to_app)?;
    let (spin_duration, password_confirmation_required) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT spin_duration, password_confirmation_required FROM wheel_settings WHERE id = 1",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let tier =
        super::redemption_tiers::most_generous_redemption_tier_in_transaction(&mut transaction)
            .await?;
    let shop_rate_point_value_seconds =
        active_shop_rate_point_value_seconds(&mut transaction).await?;
    let settings = wheel_settings_view(
        &config,
        version,
        spin_duration,
        password_confirmation_required == 1,
        tier,
        shop_rate_point_value_seconds,
    )?;
    transaction.commit().await?;
    Ok(settings)
}

pub async fn client_wheel_settings(pool: &SqlitePool) -> AppResult<ClientWheelSettings> {
    let settings = wheel_settings(pool).await?;
    Ok(ClientWheelSettings {
        enabled: settings.enabled,
        free_spin_cap: settings.free_spin_cap,
        spin_duration: settings.spin_duration,
        password_confirmation_required: settings.password_confirmation_required,
        version: settings.version,
        segments: settings
            .segments
            .into_iter()
            .map(|segment| WheelSegmentRow {
                label: segment.label,
                seconds: segment.seconds,
                points_centi: segment.points_centi,
                free_spin: segment.free_spin,
            })
            .collect(),
        expected_payout_seconds: settings.expected_payout_seconds,
        expected_payout_points_centi: settings.expected_payout_points_centi,
        point_value_seconds: settings.point_value_seconds,
        house_edge_basis_points: settings.house_edge_basis_points,
        unsafe_house_edge: settings.unsafe_house_edge,
    })
}

pub async fn update_wheel_settings(
    pool: &SqlitePool,
    input: &WheelSettingsInput,
) -> AppResult<WheelSettings> {
    if !(1..=120).contains(&input.spin_duration) {
        return Err(AppError::InvalidInput(
            "spin duration must be between 1 and 120 seconds".to_owned(),
        ));
    }
    let mut segments = Vec::with_capacity(input.segments.len());
    for row in &input.segments {
        segments.push(segment_to_domain(row));
    }
    let config = WheelConfig {
        enabled: input.enabled,
        bokya_rate_basis_points: input.bokya_rate_basis_points,
        free_spin_cap: input.free_spin_cap,
        segments,
    };

    let mut transaction = super::begin_immediate(pool).await?;
    config.validate().map_err(wheel_error_to_app)?;
    let tier =
        super::redemption_tiers::most_generous_redemption_tier_in_transaction(&mut transaction)
            .await?;
    // Refuse a money-printer wheel against the most generous Redemption Tier
    // (time-axis guardrail only, ADR-0046).
    config
        .ensure_positive_house_edge(tier.rate())
        .map_err(wheel_error_to_app)?;

    let version = sqlx::query_scalar::<_, i64>(
        "UPDATE wheel_settings
         SET enabled = ?, bokya_rate_basis_points = ?, free_spin_cap = ?, spin_duration = ?,
             password_confirmation_required = ?,
             version = version + 1, updated_at = CURRENT_TIMESTAMP
         WHERE id = 1
         RETURNING version",
    )
    .bind(i64::from(input.enabled))
    .bind(input.bokya_rate_basis_points)
    .bind(input.free_spin_cap)
    .bind(input.spin_duration)
    .bind(i64::from(input.password_confirmation_required))
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query("DELETE FROM wheel_segments")
        .execute(&mut *transaction)
        .await?;
    for (index, row) in input.segments.iter().enumerate() {
        sqlx::query(
            "INSERT INTO wheel_segments (id, position, label, seconds, points_centi, free_spin)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(index as i64 + 1)
        .bind(&row.label)
        .bind(row.seconds)
        .bind(row.points_centi)
        .bind(i64::from(row.free_spin))
        .execute(&mut *transaction)
        .await?;
    }

    let house_edge_basis_points = config
        .house_edge_basis_points(tier.rate())
        .map_err(wheel_error_to_app)?;
    let mut event = NewAuditEvent::simple(
        "wheel_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Spin the Wheel configuration",
    );
    event.metadata = json!({
        "enabled": input.enabled,
        "bokya_rate_basis_points": input.bokya_rate_basis_points,
        "free_spin_cap": input.free_spin_cap,
        "spin_duration": input.spin_duration,
        "password_confirmation_required": input.password_confirmation_required,
        "version": version,
        "house_edge_basis_points": house_edge_basis_points,
        "segment_count": input.segments.len()
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;

    wheel_settings(pool).await
}

pub async fn spin_wheel<R: SpinRng>(
    pool: &SqlitePool,
    account_id: &str,
    pc_id: &str,
    rng: &mut R,
) -> AppResult<WheelSpinMutation> {
    let mut transaction = super::begin_immediate(pool).await?;
    let (config, version) = load_wheel_config(&mut transaction).await?;

    if !config.enabled {
        append_spin_denial(&mut transaction, account_id, pc_id, "disabled").await?;
        transaction.commit().await?;
        return Err(AppError::Conflict(
            "Spin the Wheel is currently disabled".to_owned(),
        ));
    }

    if matches!(
        config.validate(),
        Err(WheelError::NoBokyaSegment | WheelError::NoWinSegment)
    ) {
        append_spin_denial(
            &mut transaction,
            account_id,
            pc_id,
            "contradictory_bokya_rate",
        )
        .await?;
        transaction.commit().await?;
        return Err(AppError::Conflict(
            "Spin the Wheel has a contradictory Bokya Rate".to_owned(),
        ));
    }

    let tier =
        super::redemption_tiers::most_generous_redemption_tier_in_transaction(&mut transaction)
            .await?;
    if config.ensure_positive_house_edge(tier.rate()).is_err() {
        append_spin_denial(&mut transaction, account_id, pc_id, "negative_house_edge").await?;
        transaction.commit().await?;
        return Err(AppError::Conflict(
            "Spin the Wheel is temporarily unavailable".to_owned(),
        ));
    }

    // Deduct exactly one whole point (100 centipoints), atomically gated on
    // sufficient balance.
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
         SET points_balance_centi = points_balance_centi - ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'active' AND points_balance_centi >= ?
         RETURNING id, username, remaining_seconds, points_balance_centi, status, created_at,
                   deleted_at, imported_from, imported_at,
                   imported_from IS NOT NULL AND password_hash LIKE '$2%$%'",
    )
    .bind(WHEEL_SPIN_COST_CENTIPOINTS)
    .bind(account_id)
    .bind(WHEEL_SPIN_COST_CENTIPOINTS)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(debited_row) = debited else {
        append_spin_denial(&mut transaction, account_id, pc_id, "insufficient_points").await?;
        transaction.commit().await?;
        return Err(AppError::Conflict(
            "at least one point is required to spin".to_owned(),
        ));
    };
    // The points debit does not touch Remaining Time, so this is the balance
    // the account carried into the spin — recorded for fairness reporting.
    let remaining_before_seconds = debited_row.2;

    let outcome = config.resolve(rng).map_err(wheel_error_to_app)?;

    // Credit won time AND won points (gross) in one update.
    let points_delta_centi = outcome
        .points_won_centi
        .saturating_sub(WHEEL_SPIN_COST_CENTIPOINTS);
    let account = sqlx::query_as::<
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
    .bind(outcome.seconds_won)
    .bind(outcome.points_won_centi)
    .bind(account_id)
    .fetch_optional(&mut *transaction)
    .await?
    .map(Account::from)
    .ok_or_else(|| AppError::NotFound("active account".to_owned()))?;

    let outcome_json = serde_json::to_string(&outcome.steps)
        .map_err(|error| AppError::InvalidInput(error.to_string()))?;

    // Audit carries **net** points delta (spent - won = -100 + points_won_centi).
    let mut event = NewAuditEvent::simple(
        "wheel_spin",
        AuditOutcome::Success,
        AuditActor::Account,
        "Spun the wheel",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = Some(pc_id);
    event.seconds_delta = Some(outcome.seconds_won);
    event.points_delta_centi = Some(points_delta_centi);
    event.cash_flag = false;
    event.metadata = json!({
        "config_version": version,
        "bokya_rate_basis_points": config.bokya_rate_basis_points,
        "remaining_before_seconds": remaining_before_seconds,
        "free_spins_triggered": outcome.free_spins_triggered,
        "seconds_won": outcome.seconds_won,
        "points_won_centi": outcome.points_won_centi,
        "steps": outcome.steps
    });
    let audit_event_id = audit::append(&mut transaction, event).await?;

    // Spin detail carries the **gross** points won (gross turnover needed for
    // reporting, ADR-0046) plus the Remaining Time the account had before the
    // spin resolved.
    sqlx::query(
        "INSERT INTO wheel_spins (
           id, account_id, pc_id, points_spent_centi, seconds_won,
           points_won_centi, free_spins_triggered, config_version,
           bokya_rate_basis_points, remaining_before_seconds, outcome_json, audit_event_id
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(account_id)
    .bind(pc_id)
    .bind(WHEEL_SPIN_COST_CENTIPOINTS)
    .bind(outcome.seconds_won)
    .bind(outcome.points_won_centi)
    .bind(outcome.free_spins_triggered)
    .bind(version)
    .bind(config.bokya_rate_basis_points)
    .bind(remaining_before_seconds)
    .bind(&outcome_json)
    .bind(audit_event_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let steps = outcome
        .steps
        .iter()
        .map(|step| WheelSpinStep {
            segment_index: step.segment_index,
            label: config
                .segments
                .get(step.segment_index)
                .map(|segment| segment.label.clone())
                .unwrap_or_default(),
            seconds: step.seconds,
            points_centi: step.points_centi,
            free_spin: step.free_spin,
        })
        .collect();

    Ok(WheelSpinMutation {
        account,
        result: WheelSpinResult {
            seconds_won: outcome.seconds_won,
            points_won_centi: outcome.points_won_centi,
            free_spins_triggered: outcome.free_spins_triggered,
            segments: segment_rows(&config),
            steps,
        },
    })
}

async fn append_spin_denial(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    account_id: &str,
    pc_id: &str,
    reason: &str,
) -> AppResult<()> {
    let mut event = NewAuditEvent::simple(
        "wheel_spin_denied",
        AuditOutcome::Denied,
        AuditActor::Account,
        "Wheel spin denied",
    );
    event.actor_account_id = Some(account_id);
    event.account_id = Some(account_id);
    event.pc_id = Some(pc_id);
    event.metadata = json!({ "reason": reason });
    audit::append(transaction, event).await?;
    Ok(())
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
        super::super::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await?;
        super::super::seed(&pool).await?;
        Ok(pool)
    }

    async fn test_account(pool: &SqlitePool, username: &str) -> AppResult<Account> {
        accounts::create(pool, username, "password", AccountCreator::Admin, None).await
    }

    struct QueuedRng {
        draws: Vec<i64>,
        position: usize,
    }

    impl SpinRng for QueuedRng {
        fn gen_below(&mut self, bound: i64) -> i64 {
            let value = self.draws[self.position] % bound;
            self.position += 1;
            value
        }
    }

    #[tokio::test]
    async fn spin_is_denied_when_the_wheel_is_disabled() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = test_account(&pool, "wheel-off").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 500 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        assert!(matches!(
            spin_wheel(&pool, &account.id, "pc-01", &mut rng).await,
            Err(AppError::Conflict(_))
        ));
        let points: i64 =
            sqlx::query_scalar("SELECT points_balance_centi FROM accounts WHERE id = ?")
                .bind(&account.id)
                .fetch_one(&pool)
                .await?;
        let denials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'wheel_spin_denied'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(points, 500);
        assert_eq!(denials, 1);
        Ok(())
    }

    #[tokio::test]
    async fn spin_deducts_one_point_and_credits_won_time() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        let account = test_account(&pool, "wheel-win").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 300 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        // Draw 1 -> the second slice of the seeded default, a terminal
        // 300-second time prize. Segments are equally likely while Bokya Rate
        // is unset, so the draw is the index (ADR-0047, ADR-0049).
        let mut rng = QueuedRng {
            draws: vec![1],
            position: 0,
        };
        let mutation = spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;

        assert_eq!(mutation.result.seconds_won, 300);
        assert_eq!(mutation.result.points_won_centi, 0);
        assert_eq!(mutation.result.free_spins_triggered, 0);
        assert_eq!(mutation.result.segments.len(), 12);
        assert_eq!(mutation.result.segments[0].label, "Try again");
        assert_eq!(mutation.result.segments[1].label, "5 minutes");
        assert_eq!(mutation.account.points_balance_centi, 200); // 300 - 100 cost = 200
        assert_eq!(mutation.account.remaining_seconds, 300);
        let audit: (i64, i64, i64) = sqlx::query_as(
            "SELECT cash_flag, seconds_delta, points_delta_centi
             FROM audit_log WHERE event_type = 'wheel_spin'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit, (0, 300, -100)); // net: -100 centipoints
        let detail: (i64, i64, i64, i64, Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT points_spent_centi, seconds_won, points_won_centi, config_version,
                    bokya_rate_basis_points, remaining_before_seconds
             FROM wheel_spins",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(detail, (100, 300, 0, 1, None, Some(0))); // gross plus stamped configuration
        let audit_remaining_before: Option<i64> = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.remaining_before_seconds')
             FROM audit_log WHERE event_type = 'wheel_spin'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_remaining_before, Some(0));
        let audit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'wheel_spin'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(audit_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn spin_awards_points_when_segment_has_points_prize() -> AppResult<()> {
        let pool = test_pool().await?;

        // Replace segments with one that awards points
        sqlx::query("DELETE FROM wheel_segments")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO wheel_segments (id, position, label, seconds, points_centi, free_spin)
             VALUES (?, 1, ?, ?, ?, 0)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind("50 cp")
        .bind(0_i64)
        .bind(50_i64)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1, free_spin_cap = 0 WHERE id = 1")
            .execute(&pool)
            .await?;

        let account = test_account(&pool, "wheel-pts").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 200 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        let mutation = spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;

        // Won 50 centipoints, spent 100, net -50, balance: 200 - 100 + 50 = 150
        assert_eq!(mutation.result.points_won_centi, 50);
        assert_eq!(mutation.account.points_balance_centi, 150);
        let audit: (i64, i64) = sqlx::query_as(
            "SELECT points_delta_centi, seconds_delta
             FROM audit_log WHERE event_type = 'wheel_spin'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit, (-50, 0)); // net: -50 centipoints
        let detail: (i64, i64) =
            sqlx::query_as("SELECT points_won_centi, points_spent_centi FROM wheel_spins")
                .fetch_one(&pool)
                .await?;
        assert_eq!(detail, (50, 100)); // gross: 50 won, 100 spent
        Ok(())
    }

    #[tokio::test]
    async fn spin_resolves_a_capped_free_spin_chain() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        let account = test_account(&pool, "wheel-chain").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 100 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        // Draw 10 is the lone free-spin slice of the seeded default; the
        // follow-up draw 1 lands on a "5 minutes" slice and ends the chain.
        let mut rng = QueuedRng {
            draws: vec![10, 1],
            position: 0,
        };
        let mutation = spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;

        assert_eq!(mutation.result.free_spins_triggered, 1);
        assert_eq!(mutation.result.seconds_won, 300);
        assert_eq!(mutation.account.points_balance_centi, 0); // 100 - 100 cost + 0 won = 0
        Ok(())
    }

    #[tokio::test]
    async fn spin_with_insufficient_points_is_denied_without_credit() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        let account = test_account(&pool, "wheel-broke").await?;

        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        assert!(matches!(
            spin_wheel(&pool, &account.id, "pc-01", &mut rng).await,
            Err(AppError::Conflict(_))
        ));
        let balance: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        let spins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wheel_spins")
            .fetch_one(&pool)
            .await?;
        let denials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log
             WHERE event_type = 'wheel_spin_denied'
               AND json_extract(metadata_json, '$.reason') = 'insufficient_points'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, (0, 0));
        assert_eq!(spins, 0);
        assert_eq!(denials, 1);
        Ok(())
    }

    #[tokio::test]
    async fn unsafe_persisted_wheel_is_denied_and_changes_no_balance() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE wheel_segments SET seconds = 100000 WHERE position = 1")
            .execute(&pool)
            .await?;
        let account = test_account(&pool, "wheel-unsafe").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 500 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;
        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        assert!(matches!(
            spin_wheel(&pool, &account.id, "pc-01", &mut rng).await,
            Err(AppError::Conflict(_))
        ));
        let balance: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        let denials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'wheel_spin_denied' AND json_extract(metadata_json, '$.reason') = 'negative_house_edge'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, (0, 500));
        assert_eq!(denials, 1);
        Ok(())
    }

    #[tokio::test]
    async fn update_wheel_settings_refuses_a_negative_house_edge() -> AppResult<()> {
        let pool = test_pool().await?;
        let input = WheelSettingsInput {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            spin_duration: 5,
            password_confirmation_required: false,
            segments: vec![
                WheelSegmentRow {
                    label: "1 hour".into(),
                    seconds: 3_600,
                    points_centi: 0,
                    free_spin: false,
                },
                WheelSegmentRow {
                    label: "Try again".into(),
                    seconds: 0,
                    points_centi: 0,
                    free_spin: false,
                },
            ],
        };
        // Expected 1800s > 1440s point value -> refused, config unchanged.
        assert!(matches!(
            update_wheel_settings(&pool, &input).await,
            Err(AppError::Conflict(_))
        ));
        let segment_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wheel_segments")
            .fetch_one(&pool)
            .await?;
        let version: i64 = sqlx::query_scalar("SELECT version FROM wheel_settings WHERE id = 1")
            .fetch_one(&pool)
            .await?;
        assert_eq!(segment_count, 12);
        assert_eq!(version, 1);
        Ok(())
    }

    #[tokio::test]
    async fn update_refuses_contradictory_bokya_rate_without_writes() -> AppResult<()> {
        let pool = test_pool().await?;
        let input = WheelSettingsInput {
            enabled: true,
            bokya_rate_basis_points: Some(5_000),
            free_spin_cap: 0,
            spin_duration: 9,
            password_confirmation_required: false,
            segments: vec![WheelSegmentRow {
                label: "Winner".into(),
                seconds: 60,
                points_centi: 0,
                free_spin: false,
            }],
        };

        assert!(matches!(
            update_wheel_settings(&pool, &input).await,
            Err(AppError::InvalidInput(_))
        ));
        let settings: (i64, Option<i64>, i64, i64, i64) = sqlx::query_as(
            "SELECT enabled, bokya_rate_basis_points, free_spin_cap, spin_duration, version
             FROM wheel_settings WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        let segment_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wheel_segments")
            .fetch_one(&pool)
            .await?;
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'wheel_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(settings, (0, None, 1, 5, 1));
        assert_eq!(segment_count, 12);
        assert_eq!(audits, 0);
        Ok(())
    }

    #[tokio::test]
    async fn wheel_settings_use_most_generous_tier_for_point_value() -> AppResult<()> {
        let pool = test_pool().await?;
        super::super::update_redemption_tiers(
            &pool,
            &crate::models::RedemptionTiersInput {
                tiers: vec![
                    crate::models::RedemptionTierInput {
                        points: 5,
                        minutes: 60,
                    },
                    crate::models::RedemptionTierInput {
                        points: 10,
                        minutes: 360,
                    },
                    crate::models::RedemptionTierInput {
                        points: 20,
                        minutes: 240,
                    },
                ],
            },
        )
        .await?;

        let settings = wheel_settings(&pool).await?;
        assert_eq!(settings.point_value_seconds, 2_160);
        // (2160 - 270.8333) / 2160 = 87.46%
        assert_eq!(settings.house_edge_basis_points, 8_746);
        assert!(!settings.unsafe_house_edge);
        Ok(())
    }

    #[tokio::test]
    async fn shop_rate_advisory_rechecks_without_changing_wheel_save_guardrail() -> AppResult<()> {
        let pool = test_pool().await?;
        let initial = wheel_settings(&pool).await?;
        assert_eq!(initial.shop_rate_point_value_seconds, 15_000);
        assert!(!initial.unsafe_shop_rate_house_edge);

        // The existing default wheel remains valid against the Redemption
        // Tier, but a very cheap Shop Rate makes the paid-time value of one
        // earned point smaller than its expected payout.
        super::super::update_shop_rate(
            &pool,
            &crate::models::ShopRateInput {
                amount_cents: 100,
                seconds: 2,
            },
        )
        .await?;
        let unsafe_settings = wheel_settings(&pool).await?;
        assert_eq!(unsafe_settings.shop_rate_point_value_seconds, 100);
        assert!(unsafe_settings.unsafe_shop_rate_house_edge);
        assert!(!unsafe_settings.unsafe_house_edge);

        // Shop Rate safety is an advisory and must not alter the Wheel save's
        // existing Redemption Tier refusal behaviour.
        let saved = update_wheel_settings(
            &pool,
            &WheelSettingsInput {
                enabled: unsafe_settings.enabled,
                bokya_rate_basis_points: unsafe_settings.bokya_rate_basis_points,
                free_spin_cap: unsafe_settings.free_spin_cap,
                spin_duration: unsafe_settings.spin_duration,
                password_confirmation_required: unsafe_settings.password_confirmation_required,
                segments: unsafe_settings
                    .segments
                    .iter()
                    .map(|segment| WheelSegmentRow {
                        label: segment.label.clone(),
                        seconds: segment.seconds,
                        points_centi: segment.points_centi,
                        free_spin: segment.free_spin,
                    })
                    .collect(),
            },
        )
        .await?;
        assert!(saved.unsafe_shop_rate_house_edge);

        super::super::update_shop_rate(
            &pool,
            &crate::models::ShopRateInput {
                amount_cents: 100,
                seconds: 300,
            },
        )
        .await?;
        assert!(!wheel_settings(&pool).await?.unsafe_shop_rate_house_edge);
        Ok(())
    }

    #[tokio::test]
    async fn update_wheel_settings_persists_a_safe_configuration() -> AppResult<()> {
        let pool = test_pool().await?;
        let input = WheelSettingsInput {
            enabled: true,
            bokya_rate_basis_points: Some(7_500),
            free_spin_cap: 2,
            spin_duration: 5,
            password_confirmation_required: true,
            segments: vec![
                WheelSegmentRow {
                    label: "2 minutes".into(),
                    seconds: 120,
                    points_centi: 0,
                    free_spin: false,
                },
                WheelSegmentRow {
                    label: "Try again".into(),
                    seconds: 0,
                    points_centi: 0,
                    free_spin: false,
                },
                WheelSegmentRow {
                    label: "Free spin".into(),
                    seconds: 0,
                    points_centi: 0,
                    free_spin: true,
                },
            ],
        };
        let settings = update_wheel_settings(&pool, &input).await?;

        assert!(settings.enabled);
        assert_eq!(settings.bokya_rate_basis_points, Some(7_500));
        assert_eq!(settings.free_spin_cap, 2);
        assert_eq!(settings.spin_duration, 5);
        assert!(settings.password_confirmation_required);
        assert_eq!(settings.version, 2);
        assert_eq!(
            settings
                .segments
                .iter()
                .map(|segment| segment.probability_basis_points)
                .collect::<Vec<_>>(),
            vec![1_250, 7_500, 1_250]
        );
        assert_eq!(settings.segments.len(), 3);
        assert!(settings.house_edge_basis_points > 0);
        assert!(!settings.unsafe_house_edge);
        let saved_rate: Option<i64> =
            sqlx::query_scalar("SELECT bokya_rate_basis_points FROM wheel_settings WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        let audit_rate: i64 = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.bokya_rate_basis_points')
             FROM audit_log WHERE event_type = 'wheel_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(saved_rate, Some(7_500));
        assert_eq!(audit_rate, 7_500);
        Ok(())
    }

    #[tokio::test]
    async fn update_wheel_settings_rejects_invalid_spin_duration() -> AppResult<()> {
        let pool = test_pool().await?;
        for spin_duration in [0, 121] {
            let input = WheelSettingsInput {
                enabled: false,
                bokya_rate_basis_points: None,
                free_spin_cap: 1,
                spin_duration,
                password_confirmation_required: false,
                segments: vec![WheelSegmentRow {
                    label: "Nothing".into(),
                    seconds: 0,
                    points_centi: 0,
                    free_spin: false,
                }],
            };
            assert!(matches!(
                update_wheel_settings(&pool, &input).await,
                Err(AppError::InvalidInput(_))
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn points_heavy_configuration_is_not_refused() -> AppResult<()> {
        let pool = test_pool().await?;

        // A points-only wheel that is provably self-sustaining (200 cp expected
        // vs 100 cp cost) MUST be accepted — the deliberate asymmetric guardrail
        // (ADR-0046).
        let input = WheelSettingsInput {
            enabled: true,
            bokya_rate_basis_points: Some(0),
            free_spin_cap: 0,
            spin_duration: 5,
            password_confirmation_required: false,
            segments: vec![WheelSegmentRow {
                label: "2 points".into(),
                seconds: 0,
                points_centi: 200,
                free_spin: false,
            }],
        };
        let settings = update_wheel_settings(&pool, &input).await?;
        assert!(settings.enabled);
        assert_eq!(settings.segments.len(), 1);
        assert_eq!(settings.expected_payout_points_centi, 200);
        assert!(settings.house_edge_basis_points > 0); // time edge is 0 < 1440
        assert!(!settings.unsafe_house_edge);
        Ok(())
    }

    #[tokio::test]
    async fn contradictory_bokya_rate_is_denied_without_mutation() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "UPDATE wheel_settings SET enabled = 1, bokya_rate_basis_points = 5000 WHERE id = 1",
        )
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE wheel_segments SET seconds = 60, points_centi = 0, free_spin = 0")
            .execute(&pool)
            .await?;
        let account = test_account(&pool, "wheel-contradictory").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 500 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;
        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };

        assert!(matches!(
            spin_wheel(&pool, &account.id, "pc-01", &mut rng).await,
            Err(AppError::Conflict(_))
        ));
        let balance: (i64, i64) = sqlx::query_as(
            "SELECT remaining_seconds, points_balance_centi FROM accounts WHERE id = ?",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await?;
        let reason: String = sqlx::query_scalar(
            "SELECT json_extract(metadata_json, '$.reason')
             FROM audit_log WHERE event_type = 'wheel_spin_denied'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(balance, (0, 500));
        assert_eq!(reason, "contradictory_bokya_rate");
        Ok(())
    }

    #[tokio::test]
    async fn configured_bokya_rate_drives_and_stamps_the_spin() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "UPDATE wheel_settings SET enabled = 1, bokya_rate_basis_points = 9000 WHERE id = 1",
        )
        .execute(&pool)
        .await?;
        let account = test_account(&pool, "wheel-biased").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 100 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;
        let mut rng = QueuedRng {
            draws: vec![8_999],
            position: 0,
        };

        let mutation = spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;
        let stamped_rate: Option<i64> =
            sqlx::query_scalar("SELECT bokya_rate_basis_points FROM wheel_spins")
                .fetch_one(&pool)
                .await?;
        assert_eq!(mutation.result.seconds_won, 0);
        assert_eq!(mutation.result.points_won_centi, 0);
        assert_eq!(stamped_rate, Some(9_000));
        Ok(())
    }

    #[tokio::test]
    async fn spin_awards_both_time_and_points() -> AppResult<()> {
        let pool = test_pool().await?;

        // Set up a single segment awarding both time and points
        sqlx::query("DELETE FROM wheel_segments")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO wheel_segments (id, position, label, seconds, points_centi, free_spin)
             VALUES (?, 1, ?, ?, ?, 0)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind("10m + 5pts")
        .bind(600_i64)
        .bind(500_i64)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1, free_spin_cap = 0 WHERE id = 1")
            .execute(&pool)
            .await?;

        let account = test_account(&pool, "wheel-both").await?;
        sqlx::query("UPDATE accounts SET points_balance_centi = 200 WHERE id = ?")
            .bind(&account.id)
            .execute(&pool)
            .await?;

        let mut rng = QueuedRng {
            draws: vec![0],
            position: 0,
        };
        let mutation = spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;

        assert_eq!(mutation.result.seconds_won, 600);
        assert_eq!(mutation.result.points_won_centi, 500);
        assert_eq!(mutation.account.remaining_seconds, 600);
        // 200 - 100 (cost) + 500 (won) = 600
        assert_eq!(mutation.account.points_balance_centi, 600);
        Ok(())
    }

    /// The draw must never depend on the spinner's Remaining Time. Customer
    /// folklore claims spinning from a zero balance yields better jackpot odds;
    /// this differential test feeds the **same** RNG stream to a timed and an
    /// out-of-time account and requires identical outcomes on every trial.
    #[tokio::test]
    async fn spin_odds_do_not_depend_on_remaining_time() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        let timed = test_account(&pool, "wheel-timed").await?;
        let broke = test_account(&pool, "wheel-broke").await?;
        for account in [&timed, &broke] {
            sqlx::query("UPDATE accounts SET points_balance_centi = 1000000 WHERE id = ?")
                .bind(&account.id)
                .execute(&pool)
                .await?;
        }
        sqlx::query("UPDATE accounts SET remaining_seconds = 3600 WHERE id = ?")
            .bind(&timed.id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE accounts SET remaining_seconds = 0 WHERE id = ?")
            .bind(&broke.id)
            .execute(&pool)
            .await?;

        // One shared LCG so both accounts see byte-identical streams; the
        // chain length varies with the drawn segment, so streams are long
        // enough to cover capped free-spin chains without bound checks.
        let trials = 200;
        let mut lcg: i64 = 0x2545F4914F6CDD1D;
        let mut next_draw = |bound: i64| -> i64 {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33).rem_euclid(bound.max(1))
        };
        let draws: Vec<i64> = (0..trials * 64).map(|_| next_draw(100_000)).collect();

        for trial in 0..trials {
            let mut broke_rng = QueuedRng {
                draws: draws.clone(),
                position: trial * 64,
            };
            let mut timed_rng = QueuedRng {
                draws: draws.clone(),
                position: trial * 64,
            };
            let broke_mutation = spin_wheel(&pool, &broke.id, "pc-01", &mut broke_rng).await?;
            let timed_mutation = spin_wheel(&pool, &timed.id, "pc-01", &mut timed_rng).await?;
            assert_eq!(
                broke_mutation.result.seconds_won, timed_mutation.result.seconds_won,
                "trial {trial}: time won diverged by remaining time"
            );
            assert_eq!(
                broke_mutation.result.points_won_centi, timed_mutation.result.points_won_centi,
                "trial {trial}: points won diverged by remaining time"
            );
            assert_eq!(
                broke_mutation.result.free_spins_triggered,
                timed_mutation.result.free_spins_triggered,
                "trial {trial}: free-spin chain diverged by remaining time"
            );
        }
        Ok(())
    }

    /// Every spin detail row stamps the Remaining Time the account carried
    /// into the spin (pre-spin balance), so jackpot rates can be broken down
    /// by starting balance when customers allege balance-dependent odds.
    #[tokio::test]
    async fn spin_records_the_remaining_time_the_account_brought_in() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE wheel_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;
        let account = test_account(&pool, "wheel-stamped").await?;
        sqlx::query(
            "UPDATE accounts SET points_balance_centi = 200, remaining_seconds = 1800
             WHERE id = ?",
        )
        .bind(&account.id)
        .execute(&pool)
        .await?;

        // Draw 1 -> the seeded default's second slice, a terminal 300-second
        // time prize, mirroring spin_deducts_one_point_and_credits_won_time.
        let mut rng = QueuedRng {
            draws: vec![1],
            position: 0,
        };
        let mutation = spin_wheel(&pool, &account.id, "pc-01", &mut rng).await?;
        assert_eq!(mutation.result.seconds_won, 300);

        let stamped: Option<i64> =
            sqlx::query_scalar("SELECT remaining_before_seconds FROM wheel_spins")
                .fetch_one(&pool)
                .await?;
        assert_eq!(stamped, Some(1800));
        Ok(())
    }
}

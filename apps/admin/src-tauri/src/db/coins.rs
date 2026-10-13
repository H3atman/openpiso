use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    pricing::{
        coin_promo_match as coin_promo_match_for_amount, compute_coin_outcome, CoinOutcomeTarget,
        CoinPromoTierRate,
    },
};
use serde_json::json;
use sqlx::{SqlitePool, Transaction};
use uuid::Uuid;

use tracing::warn;

use crate::{
    audit::{self, NewAuditEvent},
    db::{attendance, attendance_earning},
    errors::{AppError, AppResult},
    models::{CoinEventInput, CoinSourceClaim, CoinTarget, CoinTransaction, TodayReport},
};

use chrono::Utc;

pub const LOCAL_SERIAL_DEVICE_ID: &str = "central-terminal-serial";

/// Authenticated source derived from a [`CoinSourceClaim`] inside the
/// coin-event transaction (ADR-0033).
pub(crate) struct ResolvedSource {
    pub device_id: String,
    pub source: String,
}

/// Resolve and authenticate a [`CoinSourceClaim`] inside the coin-event
/// transaction. Runs the device lookup, secret comparison, simulator-enabled
/// gate, and spoof-guard backstop, returning the canonical device id and
/// source label. Called right after `begin()` so authentication and the
/// money mutation stay in one transaction (no TOCTOU window).
async fn resolve_coin_source(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    claim: &CoinSourceClaim,
) -> AppResult<ResolvedSource> {
    match claim {
        CoinSourceClaim::SerialTerminal => {
            let (_secret, active) = sqlx::query_as::<_, (String, i64)>(
                "SELECT secret, active FROM coin_devices WHERE id = ?",
            )
            .bind(LOCAL_SERIAL_DEVICE_ID)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| {
                warn!(target: "openpiso_admin_lib::coins", reason = "serial_device_missing", "Coin commit rejected");
                AppError::Unauthorized("local serial device not configured".to_owned())
            })?;
            if active != 1 {
                warn!(target: "openpiso_admin_lib::coins", reason = "serial_device_inactive", "Coin commit rejected");
                return Err(AppError::Unauthorized(
                    "local serial device is not active".to_owned(),
                ));
            }
            Ok(ResolvedSource {
                device_id: LOCAL_SERIAL_DEVICE_ID.to_owned(),
                source: "serial".to_owned(),
            })
        }
        CoinSourceClaim::Simulator => {
            let simulator_enabled = sqlx::query_scalar::<_, String>(
                "SELECT value FROM settings WHERE key = 'simulator_enabled'",
            )
            .fetch_optional(&mut **tx)
            .await?
            .is_some_and(|value| value == "true");
            if !simulator_enabled {
                warn!(target: "openpiso_admin_lib::coins", reason = "simulator_disabled", claim_kind = "local", "Coin commit rejected");
                return Err(AppError::Unauthorized(
                    "coin simulator is disabled".to_owned(),
                ));
            }
            Ok(ResolvedSource {
                device_id: "dev-simulator".to_owned(),
                source: "simulator".to_owned(),
            })
        }
        CoinSourceClaim::RemoteDevice { device_id, secret } => {
            let trimmed_device_id = device_id.trim();
            if trimmed_device_id.is_empty() {
                warn!(target: "openpiso_admin_lib::coins", reason = "device_id_missing", "Coin commit rejected");
                return Err(AppError::InvalidInput(
                    "coin device ID is required".to_owned(),
                ));
            }
            // Spoof-guard backstop: a remote caller may not claim the local
            // serial device id (type-level guarantee at the HTTP edge, plus
            // this in-transaction defence-in-depth).
            if trimmed_device_id == LOCAL_SERIAL_DEVICE_ID {
                warn!(target: "openpiso_admin_lib::coins", reason = "remote_serial_spoof", "Coin commit rejected");
                return Err(AppError::Unauthorized(
                    "the local serial coin source is not available through HTTP".to_owned(),
                ));
            }
            // Simulator device reached via HTTP — gated by the setting, not
            // by a secret (preserving existing behaviour).
            if trimmed_device_id == "dev-simulator" {
                let simulator_enabled = sqlx::query_scalar::<_, String>(
                    "SELECT value FROM settings WHERE key = 'simulator_enabled'",
                )
                .fetch_optional(&mut **tx)
                .await?
                .is_some_and(|value| value == "true");
                if !simulator_enabled {
                    warn!(target: "openpiso_admin_lib::coins", reason = "simulator_disabled", claim_kind = "remote", "Coin commit rejected");
                    return Err(AppError::Unauthorized(
                        "coin simulator is disabled".to_owned(),
                    ));
                }
                return Ok(ResolvedSource {
                    device_id: "dev-simulator".to_owned(),
                    source: "simulator".to_owned(),
                });
            }
            // Real remote device: require a matching secret.
            let (device_secret, device_active) = sqlx::query_as::<_, (String, i64)>(
                "SELECT secret, active FROM coin_devices WHERE id = ?",
            )
            .bind(trimmed_device_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| {
                warn!(target: "openpiso_admin_lib::coins", reason = "device_unknown", device_id = %trimmed_device_id, "Coin commit rejected");
                AppError::Unauthorized("unknown coin device".to_owned())
            })?;
            if device_active != 1 {
                warn!(target: "openpiso_admin_lib::coins", reason = "device_inactive", device_id = %trimmed_device_id, "Coin commit rejected");
                return Err(AppError::Unauthorized(
                    "coin device is not active".to_owned(),
                ));
            }
            if secret.is_empty() {
                warn!(target: "openpiso_admin_lib::coins", reason = "device_signature_missing", device_id = %trimmed_device_id, "Coin commit rejected");
                return Err(AppError::Unauthorized(
                    "device signature is required".to_owned(),
                ));
            }
            if secret.as_bytes() != device_secret.as_bytes() {
                warn!(target: "openpiso_admin_lib::coins", reason = "device_signature_invalid", device_id = %trimmed_device_id, "Coin commit rejected");
                return Err(AppError::Unauthorized(
                    "invalid device signature".to_owned(),
                ));
            }
            Ok(ResolvedSource {
                device_id: trimmed_device_id.to_owned(),
                source: "coin_device".to_owned(),
            })
        }
    }
}

pub async fn transactions(pool: &SqlitePool, limit: i64) -> AppResult<Vec<CoinTransaction>> {
    let safe_limit = limit.clamp(1, 100);
    Ok(sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            String,
        ),
    >(
        "SELECT t.id, t.pc_id, p.name AS pc_name, t.account_id, a.username,
                t.amount_cents, t.seconds_added, t.coin_promo_bonus_seconds,
                t.points_earned_centi, t.source,
                t.target_kind, t.created_at
         FROM coin_transactions t
         JOIN pcs p ON p.id = t.pc_id
         LEFT JOIN accounts a ON a.id = t.account_id
         ORDER BY t.created_at DESC, t.id DESC
         LIMIT ?",
    )
    .bind(safe_limit)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(CoinTransaction::from)
    .collect())
}

pub async fn today_report(pool: &SqlitePool) -> AppResult<TodayReport> {
    let report = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT
           COALESCE(SUM(amount_cents), 0) AS sales_cents,
           COUNT(*) AS transaction_count,
           COALESCE(SUM(seconds_added), 0) AS seconds_sold
         FROM coin_transactions
         WHERE date(created_at, 'localtime') = date('now', 'localtime')",
    )
    .fetch_one(pool)
    .await?;
    Ok(TodayReport::from(report))
}

#[cfg(test)]
pub async fn simulate_coin(
    pool: &SqlitePool,
    pc_id: &str,
    amount_cents: i64,
) -> AppResult<CoinTransaction> {
    record_coin_event(
        pool,
        &CoinEventInput {
            pc_id: pc_id.to_owned(),
            amount_cents,
            pulse_count: Some(1),
            timestamp: None,
        },
        CoinTarget::Guest,
        &CoinSourceClaim::Simulator,
        None,
    )
    .await
}

pub(crate) fn validate_coin_event_input(input: &CoinEventInput) -> AppResult<()> {
    let pc_id = input.pc_id.trim();

    if pc_id.is_empty() {
        warn!(target: "openpiso_admin_lib::coins", reason = "pc_id_missing", "Coin commit rejected");
        return Err(AppError::InvalidInput("PC ID is required".to_owned()));
    }
    if input.amount_cents <= 0 {
        warn!(target: "openpiso_admin_lib::coins", reason = "amount_non_positive", amount_cents = input.amount_cents, "Coin commit rejected");
        return Err(AppError::InvalidInput(
            "coin amount must be positive".to_owned(),
        ));
    }
    if input.pulse_count.is_some_and(|count| count <= 0) {
        warn!(target: "openpiso_admin_lib::coins", reason = "pulse_count_non_positive", pulse_count = ?input.pulse_count, "Coin commit rejected");
        return Err(AppError::InvalidInput(
            "pulse count must be positive when provided".to_owned(),
        ));
    }
    if let Some(timestamp) = input.timestamp.as_deref() {
        chrono::DateTime::parse_from_rfc3339(timestamp).map_err(|_| {
            warn!(target: "openpiso_admin_lib::coins", reason = "timestamp_invalid", device_timestamp = %timestamp, "Coin commit rejected");
            AppError::InvalidInput("timestamp must be RFC 3339".to_owned())
        })?;
    }

    Ok(())
}

pub async fn record_coin_event(
    pool: &SqlitePool,
    input: &CoinEventInput,
    target: CoinTarget<'_>,
    claim: &CoinSourceClaim,
    coin_trace_id: Option<Uuid>,
) -> AppResult<CoinTransaction> {
    let result = record_coin_event_inner(pool, input, target, claim, coin_trace_id).await;
    if let Err(error) = &result {
        let reason = match error {
            AppError::Io(_) => Some("filesystem_error"),
            AppError::Database(_) => Some("database_error"),
            AppError::Pricing(_) => Some("pricing_error"),
            AppError::Password(_) => Some("password_error"),
            AppError::Internal(_) => Some("internal_error"),
            AppError::Conflict(_) => Some("conflict"),
            AppError::AttendanceSettingsConflict { .. } => None,
            AppError::ShopClosed => Some("shop_closed"),
            AppError::Throttled { .. } => None,
            AppError::InvalidInput(_)
            | AppError::Unauthorized(_)
            | AppError::NotFound(_)
            // A window error can never reach the coin pipeline (it is raised
            // only by the desktop Kiosk Lock, ADR-0056), and the Telegram
            // transport can never be part of coin ingestion, so neither
            // carries a coin-failure reason.
            | AppError::Window(_)
            | AppError::TelegramUnavailable(_)
            | AppError::TelemetryUnavailable(_)
            | AppError::RestoreRejected { .. }
            | AppError::AttendanceUnauthorized
            | AppError::AttendanceInvalidInput(_)
            | AppError::AttendanceNotFound
            | AppError::AttendanceClaimDenied { .. }
            | AppError::UnsupportedMigrationHistory(_) => None,
        };
        if let Some(reason) = reason {
            warn!(
                target: "openpiso_admin_lib::coins",
                reason,
                pc_id = %input.pc_id.trim(),
                coin_trace_id = ?coin_trace_id,
                error = %error,
                "Coin commit rejected"
            );
        }
    }
    result
}

async fn record_coin_event_inner(
    pool: &SqlitePool,
    input: &CoinEventInput,
    target: CoinTarget<'_>,
    claim: &CoinSourceClaim,
    coin_trace_id: Option<Uuid>,
) -> AppResult<CoinTransaction> {
    validate_coin_event_input(input)?;
    let pc_id = input.pc_id.trim();

    let mut transaction = super::begin_immediate(pool).await?;
    let pc_name = sqlx::query_scalar::<_, String>("SELECT name FROM pcs WHERE id = ?")
        .bind(pc_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| {
            warn!(target: "openpiso_admin_lib::coins", reason = "pc_missing", pc_id = %pc_id, "Coin commit rejected");
            AppError::NotFound(format!("PC '{pc_id}'"))
        })?;

    let resolved = resolve_coin_source(&mut transaction, claim).await?;

    let (rate_amount_cents, rate_seconds) =
        super::rates::active_rate_in_transaction(&mut transaction).await?;

    // A Coin Batch is a whole coin train, not one coin (ADR-0070), so the
    // amount is decomposed into tier amounts rather than matched exactly.
    let coin_promo_tiers = sqlx::query_as::<_, (i64, i64)>(
        "SELECT tier.amount_cents, tier.bonus_seconds
         FROM coin_promo_settings settings
         JOIN coin_promo_tiers tier
         WHERE settings.id = 1 AND settings.enabled = 1",
    )
    .fetch_all(&mut *transaction)
    .await?
    .into_iter()
    .map(|(amount_cents, bonus_seconds)| CoinPromoTierRate {
        amount_cents,
        bonus_seconds,
    })
    .collect::<Vec<_>>();
    let coin_promo_match = coin_promo_match_for_amount(input.amount_cents, &coin_promo_tiers)?;
    let candidate_coin_promo_bonus_seconds = coin_promo_match.bonus_seconds;

    let transaction_id = Uuid::new_v4().to_string();
    let earn_cents_per_point: i64 =
        sqlx::query_scalar("SELECT earn_cents_per_point FROM economy_settings WHERE id = 1")
            .fetch_one(&mut *transaction)
            .await?;

    let (
        account_id,
        account_username,
        seconds_added,
        coin_promo_bonus_seconds,
        points_earned_centi,
        carry_centicents,
        target_kind,
    ) = match target {
        CoinTarget::Guest => {
            let outcome = compute_coin_outcome(
                input.amount_cents,
                CoinOutcomeTarget::Guest,
                candidate_coin_promo_bonus_seconds,
                earn_cents_per_point,
                rate_amount_cents,
                rate_seconds,
            )?;
            (
                None,
                None,
                outcome.seconds_added,
                outcome.coin_promo_bonus_seconds,
                outcome.points_earned_centi,
                None,
                "guest",
            )
        }
        CoinTarget::Account { account_id } => {
            let account = sqlx::query_as::<_, (String, i64)>(
                "SELECT username, point_earn_carry_centicents
                 FROM accounts WHERE id = ? AND status = 'active'",
            )
            .bind(account_id)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or_else(|| {
                warn!(
                    target: "openpiso_admin_lib::coins",
                    reason = "account_missing_or_inactive",
                    account_id = %account_id,
                    pc_id = %pc_id,
                    "Coin commit rejected"
                );
                AppError::NotFound("active account".to_owned())
            })?;
            let outcome = compute_coin_outcome(
                input.amount_cents,
                CoinOutcomeTarget::Account {
                    prior_carry_centicents: account.1,
                },
                candidate_coin_promo_bonus_seconds,
                earn_cents_per_point,
                rate_amount_cents,
                rate_seconds,
            )?;
            (
                Some(account_id),
                Some(account.0),
                outcome.seconds_added,
                outcome.coin_promo_bonus_seconds,
                outcome.points_earned_centi,
                Some(outcome.new_carry_centicents),
                "account",
            )
        }
    };

    if let Some(account_id) = account_id {
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
        .execute(&mut *transaction)
        .await?;
    } else {
        sqlx::query(
            "UPDATE pcs
             SET remaining_seconds = remaining_seconds + ?, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?",
        )
        .bind(seconds_added)
        .bind(pc_id)
        .execute(&mut *transaction)
        .await?;
    }

    let mut audit_event = NewAuditEvent::simple(
        "coin_accepted",
        AuditOutcome::Success,
        AuditActor::CoinDevice,
        if account_id.is_some() {
            "Accepted coin and credited account time and points"
        } else {
            "Accepted coin and credited guest time"
        },
    );
    audit_event.account_id = account_id;
    audit_event.pc_id = Some(pc_id);
    audit_event.amount_cents = Some(input.amount_cents);
    audit_event.seconds_delta = Some(seconds_added);
    audit_event.points_delta_centi = Some(points_earned_centi);
    audit_event.cash_flag = true;
    let coin_trace_id_str = coin_trace_id.map(|id| id.to_string());
    audit_event.metadata = json!({
        "device_id": resolved.device_id,
        "pulse_count": input.pulse_count,
        "device_timestamp": input.timestamp,
        "source": resolved.source,
        "target_kind": target_kind,
        "earn_cents_per_point": earn_cents_per_point,
        "point_earn_carry_centicents": carry_centicents,
        "coin_promo_amount_cents": (coin_promo_match.covered_amount_cents > 0)
            .then_some(coin_promo_match.covered_amount_cents),
        "coin_promo_bonus_seconds": coin_promo_bonus_seconds,
        "coin_trace_id": coin_trace_id_str
    });
    let audit_event_id = audit::append(&mut transaction, audit_event).await?;

    // Attendance earning rides the accepted-coin transaction: the source
    // contribution, capped progress, Check-In and Earning Month commit with
    // the coin (ADR-0084). A guest target or a disabled feature writes zero
    // Attendance rows and the path behaves exactly as before.
    if let Some(account_id) = account_id {
        let source_at = Utc::now();
        let offset = attendance::host_offset();
        if let Some(qualification) =
            attendance::qualify_source(&mut transaction, source_at, offset).await?
        {
            attendance_earning::record_earning(
                &mut transaction,
                attendance_earning::EarningInput {
                    account_id,
                    source_audit_event_id: &audit_event_id,
                    source_kind: attendance::SourceKind::AcceptedCoin,
                    cents: input.amount_cents,
                    source_at,
                    offset,
                    earning_date: qualification.earning_date,
                    award: qualification.award,
                    original_rejection_audit_event_id: None,
                    trigger_pc_id: Some(pc_id),
                },
            )
            .await?;
        }
    }

    sqlx::query(
        "INSERT INTO coin_transactions
         (id, pc_id, account_id, device_id, amount_cents, seconds_added,
          coin_promo_bonus_seconds, points_earned_centi, pulse_count, source,
          target_kind, audit_event_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&transaction_id)
    .bind(pc_id)
    .bind(account_id)
    .bind(&resolved.device_id)
    .bind(input.amount_cents)
    .bind(seconds_added)
    .bind(coin_promo_bonus_seconds)
    .bind(points_earned_centi)
    .bind(input.pulse_count)
    .bind(&resolved.source)
    .bind(target_kind)
    .bind(audit_event_id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;

    let created = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            String,
        ),
    >(
        "SELECT t.id, t.pc_id, ? AS pc_name, t.account_id, ? AS account_username,
                t.amount_cents, t.seconds_added, t.coin_promo_bonus_seconds,
                t.points_earned_centi, t.source,
                t.target_kind, t.created_at
         FROM coin_transactions t WHERE t.id = ?",
    )
    .bind(pc_name)
    .bind(account_username)
    .bind(transaction_id)
    .fetch_one(pool)
    .await?;

    Ok(CoinTransaction::from(created))
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

    #[tokio::test]
    async fn accepted_account_coin_records_attendance_earning() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = accounts::create(
            &pool,
            "attendance-coin-account",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        crate::test_utils::enable_attendance(&pool, 2, Utc::now(), 1_000, 3_600).await?;
        let input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 1_000,
            pulse_count: Some(10),
            timestamp: None,
        };

        let coin = record_coin_event(
            &pool,
            &input,
            CoinTarget::Account {
                account_id: &account.id,
            },
            &CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        let source_event_id: String =
            sqlx::query_scalar("SELECT audit_event_id FROM coin_transactions WHERE id = ?")
                .bind(coin.id)
                .fetch_one(&pool)
                .await?;
        let contribution_source: String =
            sqlx::query_scalar("SELECT source_audit_event_id FROM attendance_contributions")
                .fetch_one(&pool)
                .await?;

        assert_eq!(contribution_source, source_event_id);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_check_ins")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_earning_months")
                .fetch_one(&pool)
                .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn accepted_coin_and_settings_edit_share_one_writer_order() -> AppResult<()> {
        let (pool, path) = crate::test_utils::wal_test_pool("attendance-coin-settings-race").await;
        let account = accounts::create(
            &pool,
            "attendance-race-account",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        crate::test_utils::enable_attendance(&pool, 2, Utc::now(), 1_000, 3_600).await?;
        let input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 100,
            pulse_count: Some(1),
            timestamp: None,
        };
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let settings_pool = pool.clone();
        let settings_barrier = barrier.clone();
        let (settings_ready, ready) = tokio::sync::oneshot::channel();
        let settings_task = tokio::spawn(async move {
            let mut tx = super::super::begin_immediate(&settings_pool).await?;
            settings_ready
                .send(())
                .expect("settings readiness receiver exists");
            settings_barrier.wait().await;
            let effective_at = Utc::now();
            sqlx::query(
                "INSERT INTO attendance_settings_versions
                   (version, enabled, daily_threshold_cents, reward_seconds,
                    claim_window_months, effective_at)
                 VALUES (3, 1, 2_000, 7_200, 1, ?)",
            )
            .bind(effective_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok::<(), AppError>(())
        });
        ready
            .await
            .map_err(|error| AppError::InvalidInput(error.to_string()))?;

        let coin_pool = pool.clone();
        let coin_barrier = barrier.clone();
        let account_id = account.id.clone();
        let coin_task = tokio::spawn(async move {
            coin_barrier.wait().await;
            record_coin_event(
                &coin_pool,
                &input,
                CoinTarget::Account {
                    account_id: &account_id,
                },
                &CoinSourceClaim::Simulator,
                None,
            )
            .await
        });
        let coin = coin_task
            .await
            .map_err(|error| AppError::InvalidInput(error.to_string()))??;
        settings_task
            .await
            .map_err(|error| AppError::InvalidInput(error.to_string()))??;

        let (configuration_version, earning_date): (i64, String) = sqlx::query_as(
            "SELECT configuration_version, earning_date FROM attendance_contributions",
        )
        .fetch_one(&pool)
        .await?;
        let actual: (i64, i64, i64) = sqlx::query_as(
            "SELECT configuration_version, daily_threshold_cents, reward_seconds
             FROM attendance_daily_progress WHERE earning_date = ?",
        )
        .bind(earning_date)
        .fetch_one(&pool)
        .await?;
        assert_eq!(configuration_version, 3);
        assert_eq!(actual, (3, 2_000, 7_200));
        assert_eq!(coin.amount_cents, 100);

        pool.close().await;
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn simulation_updates_pc_and_logs_transaction() -> AppResult<()> {
        let pool = test_pool().await?;
        let created = simulate_coin(&pool, "pc-01", 100).await?;

        assert_eq!(created.seconds_added, 300);
        let remaining: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&pool)
                .await?;
        let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await?;
        assert_eq!(remaining, 300);
        assert_eq!(audit_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn coin_promo_applies_only_to_enabled_exact_account_coins() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = accounts::create(
            &pool,
            "promo-account",
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

        let input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 100,
            pulse_count: Some(1),
            timestamp: None,
        };
        let promoted = record_coin_event(
            &pool,
            &input,
            CoinTarget::Account {
                account_id: &account.id,
            },
            &CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        assert_eq!(promoted.seconds_added, 420);
        assert_eq!(promoted.coin_promo_bonus_seconds, 120);

        let metadata: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>(
                "SELECT a.metadata_json FROM audit_log a
                 JOIN coin_transactions c ON c.audit_event_id = a.id WHERE c.id = ?",
            )
            .bind(&promoted.id)
            .fetch_one(&pool)
            .await?,
        )
        .expect("audit metadata must be valid JSON");
        assert_eq!(metadata["coin_promo_amount_cents"], 100);
        assert_eq!(metadata["coin_promo_bonus_seconds"], 120);

        let promoted_points = promoted.points_earned_centi;
        sqlx::query("DELETE FROM coin_promo_tiers WHERE amount_cents = 100")
            .execute(&pool)
            .await?;
        let unmatched = record_coin_event(
            &pool,
            &input,
            CoinTarget::Account {
                account_id: &account.id,
            },
            &CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        assert_eq!(unmatched.seconds_added, 300);
        assert_eq!(unmatched.coin_promo_bonus_seconds, 0);
        assert_eq!(unmatched.points_earned_centi, promoted_points);

        sqlx::query("INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (100, 120)")
            .execute(&pool)
            .await?;
        let guest = record_coin_event(
            &pool,
            &input,
            CoinTarget::Guest,
            &CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        assert_eq!(guest.seconds_added, 300);
        assert_eq!(guest.coin_promo_bonus_seconds, 0);

        sqlx::query("UPDATE coin_promo_settings SET enabled = 0 WHERE id = 1")
            .execute(&pool)
            .await?;
        let disabled = record_coin_event(
            &pool,
            &input,
            CoinTarget::Account {
                account_id: &account.id,
            },
            &CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        assert_eq!(disabled.seconds_added, 300);
        assert_eq!(disabled.coin_promo_bonus_seconds, 0);
        Ok(())
    }

    #[tokio::test]
    async fn merged_coin_train_earns_every_coin_promo_tier_it_contains() -> AppResult<()> {
        let pool = test_pool().await?;
        let account = accounts::create(
            &pool,
            "promo-account-11",
            "password",
            AccountCreator::Admin,
            None,
        )
        .await?;
        // Owner authors a ₱10 tier: +25 min.
        sqlx::query(
            "INSERT INTO coin_promo_tiers (amount_cents, bonus_seconds) VALUES (1000, 1500)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE coin_promo_settings SET enabled = 1 WHERE id = 1")
            .execute(&pool)
            .await?;

        // ₱10 then ₱1 in one train: 11 pulses, one 1100-cent batch (ADR-0070).
        let input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 1_100,
            pulse_count: Some(11),
            timestamp: None,
        };
        let credited = record_coin_event(
            &pool,
            &input,
            CoinTarget::Account {
                account_id: &account.id,
            },
            &CoinSourceClaim::Simulator,
            None,
        )
        .await?;
        // Base 3300s for ₱11 plus the ₱10 tier's 1500s bonus.
        assert_eq!(credited.coin_promo_bonus_seconds, 1_500);
        assert_eq!(credited.seconds_added, 4_800);

        let metadata: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>(
                "SELECT a.metadata_json FROM audit_log a
                 JOIN coin_transactions c ON c.audit_event_id = a.id WHERE c.id = ?",
            )
            .bind(&credited.id)
            .fetch_one(&pool)
            .await?,
        )
        .expect("audit metadata must be valid JSON");
        // The ₱1 remainder is uncovered: only ₱10 of the ₱11 earned a bonus.
        assert_eq!(metadata["coin_promo_amount_cents"], 1_000);
        assert_eq!(metadata["coin_promo_bonus_seconds"], 1_500);
        Ok(())
    }

    #[tokio::test]
    async fn simulation_is_rejected_when_disabled() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE settings SET value = 'false' WHERE key = 'simulator_enabled'")
            .execute(&pool)
            .await?;

        assert!(matches!(
            simulate_coin(&pool, "pc-01", 100).await,
            Err(AppError::Unauthorized(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn real_device_event_requires_matching_secret() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query(
            "INSERT INTO coin_devices (id, name, secret, active)
             VALUES ('device-01', 'Test Coin Device', 'test-secret', 1)",
        )
        .execute(&pool)
        .await?;

        let input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 100,
            pulse_count: Some(1),
            timestamp: Some("2026-06-20T10:00:00Z".to_owned()),
        };
        let wrong_claim = CoinSourceClaim::RemoteDevice {
            device_id: "device-01".to_owned(),
            secret: "wrong-secret".to_owned(),
        };
        assert!(matches!(
            record_coin_event(&pool, &input, CoinTarget::Guest, &wrong_claim, None).await,
            Err(AppError::Unauthorized(_))
        ));

        let correct_claim = CoinSourceClaim::RemoteDevice {
            device_id: "device-01".to_owned(),
            secret: "test-secret".to_owned(),
        };
        let created =
            record_coin_event(&pool, &input, CoinTarget::Guest, &correct_claim, None).await?;
        assert_eq!(created.source, "coin_device");
        assert_eq!(created.seconds_added, 300);
        Ok(())
    }

    #[tokio::test]
    async fn local_serial_device_is_reserved_for_trusted_terminal_path() -> AppResult<()> {
        let pool = test_pool().await?;
        let input = CoinEventInput {
            pc_id: "pc-01".to_owned(),
            amount_cents: 100,
            pulse_count: Some(1),
            timestamp: None,
        };
        // Remote device (the HTTP path) cannot claim the local serial id.
        let remote_claim = CoinSourceClaim::RemoteDevice {
            device_id: LOCAL_SERIAL_DEVICE_ID.to_owned(),
            secret: String::new(),
        };
        assert!(matches!(
            record_coin_event(&pool, &input, CoinTarget::Guest, &remote_claim, None).await,
            Err(AppError::Unauthorized(_))
        ));
        // The SerialTerminal claim is the only path allowed to use it.
        let serial_claim = CoinSourceClaim::SerialTerminal;
        let created =
            record_coin_event(&pool, &input, CoinTarget::Guest, &serial_claim, None).await?;
        assert_eq!(created.source, "serial");
        assert_eq!(created.seconds_added, 300);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_pc_does_not_create_transaction() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            simulate_coin(&pool, "missing", 100).await,
            Err(AppError::NotFound(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0);
        Ok(())
    }
}

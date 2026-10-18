pub mod account_sessions;
pub mod app_update;
pub mod attendance;
pub mod attendance_claims;
pub mod attendance_client;
pub mod attendance_earning;
pub mod attendance_reports;
pub mod backup_schedule;
pub mod coin_promo;
pub mod coins;
pub mod diagnostics_mode;
pub mod monitoring;
pub mod notifications;
pub mod pcs;
pub mod points_wheel;
pub mod rates;
pub mod redemption_tiers;
pub mod rejection_credits;
pub mod settings;
pub mod shop_hours;
pub mod tamper;
pub mod telegram;
pub mod telemetry;
pub mod theme;
pub mod voucher_batches;
pub mod vouchers;

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Connection, Sqlite, SqlitePool, Transaction,
};
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    models::{DashboardPcMonitoring, DashboardSnapshot, Pc},
};

pub use account_sessions::{
    end as end_account_session, recover_open as recover_open_account_sessions,
    start as start_account_session,
};
pub use backup_schedule::{backup_schedule_settings, update_backup_schedule};
pub use coin_promo::{
    coin_promo_menu, coin_promo_settings, preview_coin_promo_settings, update_coin_promo_settings,
};
#[cfg(test)]
pub use coins::simulate_coin;
pub(crate) use coins::validate_coin_event_input;
pub use coins::LOCAL_SERIAL_DEVICE_ID;
pub use coins::{record_coin_event, today_report, transactions};
pub use notifications::{
    acknowledge as acknowledge_notification, acknowledge_many as acknowledge_notifications,
    list as list_notifications, mark_seen as mark_notifications_seen,
    unread_count as notification_unread_count, AlertAcknowledgement, BulkAlertAcknowledgement,
    NotificationListFilter, NotificationSeen, NotificationSeverityFilter, NotificationUnreadCount,
    NotificationUnreadFilter,
};
pub use pcs::{
    create_manual_adjustment, create_pc, disconnect_pc_by_identity, heartbeat_by_identity, pc,
    pc_is_visible, pc_settings, pcs, remove_pc, tick_guest_pcs, update_pc_settings,
};
#[cfg(test)]
pub use pcs::{disconnect_pc, heartbeat_by_identity_only};
pub use points_wheel::{
    client_wheel_settings, spin_wheel, update_wheel_settings, wheel_enabled,
    wheel_password_confirmation_required, wheel_settings, ThreadSpinRng,
};
pub use rates::{kiosk_config, shop_rate, update_shop_rate};
pub use redemption_tiers::{
    redeem_tier, redemption_tier_menu, redemption_tiers, update_redemption_tiers,
};
pub use rejection_credits::{
    create as create_rejection_credit, denominations as rejection_denominations,
    list_rejections as list_coin_batch_rejections, CreditableRejectionList, RejectionCreditInput,
    RejectionCreditResult, RejectionDenominations, RejectionListFilter,
};
pub use settings::{
    coin_device_settings, coin_source_config_from_settings, economy_settings,
    expire_pending_points_transfer_requests, get_owner_pin_hash, offline_grace_seconds,
    selection_config_from_settings, set_offline_grace_seconds, set_owner_pin, set_shop_name,
    set_shutdown_grace_seconds, shop_name, shutdown_grace_seconds, update_coin_device_settings,
    update_coin_device_settings_with_calibration_audit, update_economy_settings,
    update_voucher_template, validate_offline_grace_seconds, validate_shutdown_grace_seconds,
    verify_owner_pin, voucher_template, DEFAULT_SHUTDOWN_GRACE_SECONDS,
};
pub use shop_hours::{
    clear_shop_override, observe_shop_status, set_shop_override, shop_hours_settings, shop_status,
    update_shop_hours,
};
pub use theme::{theme_settings, theme_version, update_theme_settings};
pub use voucher_batches::{
    create_voucher_batch, get_voucher_batch, list_voucher_batches, void_voucher_batch,
};
pub use vouchers::{create_voucher, redeem_voucher, void_voucher, vouchers};

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 59;

pub async fn connect(database_path: &Path, backup_directory: &Path) -> AppResult<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        // A contending writer waits up to this long for the current writer to
        // commit before erroring, so `BEGIN IMMEDIATE` transactions queue
        // instead of failing (ADR-0044). Only consulted once a transaction has
        // actually reserved the writer, which is why every read-then-write path
        // must begin immediate — see `begin_immediate`.
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    run_migrations(&pool, backup_directory).await?;
    seed(&pool).await?;
    Ok(pool)
}

/// Begin a write transaction with `BEGIN IMMEDIATE` (ADR-0044).
///
/// In WAL mode a plain deferred `BEGIN` acquires the writer lazily, on the
/// first write. If another connection commits between the transaction's first
/// read and that lock upgrade, SQLite fails the upgrade immediately with
/// `SQLITE_BUSY` (code 5) and never consults `busy_timeout`. The admin's 1s
/// timer tick is a constant writer, so any settings save that reads before it
/// writes can lose that race. `BEGIN IMMEDIATE` reserves the writer up front,
/// so a contending writer blocks on `busy_timeout` and queues instead of
/// erroring.
///
/// Use this for every production read-then-write transaction. Read-only
/// transactions do not need it.
pub async fn begin_immediate(pool: &SqlitePool) -> AppResult<Transaction<'static, Sqlite>> {
    Ok(pool.begin_with("BEGIN IMMEDIATE").await?)
}

pub(crate) async fn run_migrations(pool: &SqlitePool, backup_directory: &Path) -> AppResult<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _openpiso_migrations (
           version INTEGER PRIMARY KEY,
           applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         )",
    )
    .execute(pool)
    .await?;

    let migrations: &[(i64, &str)] = &[
        (1_i64, include_str!("../../migrations/0001_initial.sql")),
        (
            2_i64,
            include_str!("../../migrations/0002_accounts_audit.sql"),
        ),
        (
            3_i64,
            include_str!("../../migrations/0003_points_economy.sql"),
        ),
        (
            4_i64,
            include_str!("../../migrations/0004_vouchers_redemption.sql"),
        ),
        (
            5_i64,
            include_str!("../../migrations/0005_spin_the_wheel.sql"),
        ),
        (
            6_i64,
            include_str!("../../migrations/0006_reporting_audit_review.sql"),
        ),
        (
            7_i64,
            include_str!("../../migrations/0007_coin_device_settings.sql"),
        ),
        (
            8_i64,
            include_str!("../../migrations/0008_terminal_selection_timers.sql"),
        ),
        (
            9_i64,
            include_str!("../../migrations/0009_account_transfers.sql"),
        ),
        (
            10_i64,
            include_str!("../../migrations/0010_pc_identity.sql"),
        ),
        (
            11_i64,
            include_str!("../../migrations/0011_shutdown_grace_setting.sql"),
        ),
        (
            12_i64,
            include_str!("../../migrations/0012_theme_settings.sql"),
        ),
        (
            13_i64,
            include_str!("../../migrations/0013_fractional_points_centipoints.sql"),
        ),
        (
            14_i64,
            include_str!("../../migrations/0014_redemption_tiers.sql"),
        ),
        (
            15_i64,
            include_str!("../../migrations/0015_redemption_tier_api.sql"),
        ),
        (
            16_i64,
            include_str!("../../migrations/0016_voucher_batches.sql"),
        ),
        (
            17_i64,
            include_str!("../../migrations/0017_dash_insensitive_codes.sql"),
        ),
        (
            18_i64,
            include_str!("../../migrations/0018_voucher_template.sql"),
        ),
        (
            19_i64,
            include_str!("../../migrations/0019_physical_voucher_void_status.sql"),
        ),
        (
            20_i64,
            include_str!("../../migrations/0020_voucher_batch_names.sql"),
        ),
        (
            21_i64,
            include_str!("../../migrations/0021_individual_voucher_void.sql"),
        ),
        (
            22_i64,
            include_str!("../../migrations/0022_inter_coin_gap_default.sql"),
        ),
        (
            23_i64,
            include_str!("../../migrations/0023_coin_pin_polarity.sql"),
        ),
        (
            24_i64,
            include_str!("../../migrations/0024_offline_grace_setting.sql"),
        ),
        (
            25_i64,
            include_str!("../../migrations/0025_wheel_segment_rebuild.sql"),
        ),
        (
            26_i64,
            include_str!("../../migrations/0026_points_minted_by_wheel.sql"),
        ),
        (
            27_i64,
            include_str!("../../migrations/0027_equal_slice_wheel_segments.sql"),
        ),
        (28_i64, include_str!("../../migrations/0028_bokya_rate.sql")),
        (29_i64, include_str!("../../migrations/0029_coin_promo.sql")),
        (
            30_i64,
            include_str!("../../migrations/0030_coin_promo_reporting.sql"),
        ),
        (31_i64, include_str!("../../migrations/0031_shop_hours.sql")),
        (
            32_i64,
            include_str!("../../migrations/0032_shop_hours_audit.sql"),
        ),
        (
            33_i64,
            include_str!("../../migrations/0033_backup_schedule.sql"),
        ),
        (
            34_i64,
            include_str!("../../migrations/0034_backup_schedule_next_due.sql"),
        ),
        (
            35_i64,
            include_str!("../../migrations/0035_account_import_provenance.sql"),
        ),
        (
            36_i64,
            include_str!("../../migrations/0036_notification_read_state.sql"),
        ),
        (
            37_i64,
            include_str!("../../migrations/0037_active_account_username_uniqueness.sql"),
        ),
        (
            38_i64,
            include_str!("../../migrations/0038_fresh_coin_20_candidate.sql"),
        ),
        (
            39_i64,
            include_str!("../../migrations/0039_account_session_history.sql"),
        ),
        (
            40_i64,
            include_str!("../../migrations/0040_pc_clock_difference.sql"),
        ),
        (
            41_i64,
            include_str!("../../migrations/0041_rejection_credits.sql"),
        ),
        (
            42_i64,
            include_str!("../../migrations/0042_pc_monitoring_incidents.sql"),
        ),
        (
            43_i64,
            include_str!("../../migrations/0043_pc_monitoring_suppressions.sql"),
        ),
        (
            44_i64,
            include_str!("../../migrations/0044_host_probe_coverage_close_reason.sql"),
        ),
        (
            45_i64,
            include_str!("../../migrations/0045_pc_guard_coverage_diagnostic.sql"),
        ),
        (
            46_i64,
            include_str!("../../migrations/0046_guard_recovered_close_reason.sql"),
        ),
        (
            47_i64,
            include_str!("../../migrations/0047_telegram_integration.sql"),
        ),
        (
            48_i64,
            include_str!("../../migrations/0048_telegram_outbox.sql"),
        ),
        (
            49_i64,
            include_str!("../../migrations/0049_telegram_delivery_batches.sql"),
        ),
        (
            50_i64,
            include_str!("../../migrations/0050_wheel_remaining_before_seconds.sql"),
        ),
        (
            51_i64,
            include_str!("../../migrations/0051_client_recovery_escalation.sql"),
        ),
        (
            52_i64,
            include_str!("../../migrations/0052_diagnostics_mode.sql"),
        ),
        (53_i64, include_str!("../../migrations/0053_telemetry.sql")),
        (
            54_i64,
            include_str!("../../migrations/0054_onboarding_grace_defaults.sql"),
        ),
        (
            55_i64,
            include_str!("../../migrations/0055_attendance_foundation.sql"),
        ),
        (
            56_i64,
            include_str!("../../migrations/0056_attendance_earning.sql"),
        ),
        (
            57_i64,
            include_str!("../../migrations/0057_attendance_claims.sql"),
        ),
        (
            58_i64,
            include_str!("../../migrations/0058_attendance_reporting.sql"),
        ),
        (
            59_i64,
            include_str!("../../migrations/0059_wheel_password_confirmation.sql"),
        ),
    ];
    let known_version = migrations.last().map(|(version, _)| *version).unwrap_or(0);
    debug_assert_eq!(known_version, CURRENT_SCHEMA_VERSION);
    let applied_versions: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM _openpiso_migrations WHERE version != 0 ORDER BY version",
    )
    .fetch_all(pool)
    .await?;
    for (index, version) in applied_versions.iter().enumerate() {
        if *version < 0 || *version > known_version {
            return Err(AppError::UnsupportedMigrationHistory(format!(
                "database has migration {version} but this Admin knows up to {known_version}"
            )));
        }
        let expected = index as i64 + 1;
        if *version != expected {
            return Err(AppError::UnsupportedMigrationHistory(format!(
                "migration history is missing version {expected}"
            )));
        }
    }

    // Migration 0038 has a deliberate fresh-install-only data seed. Persist
    // the install classification in the existing migration table before
    // running any migration: migrations commit one at a time, so a crash
    // after v37 must not make a fresh database look like an upgrade when it
    // resumes at v38. Version 0 is reserved for this bootstrap marker and is
    // excluded from MAX(version) by virtue of being below every real migration.
    const FRESH_INSTALL_MARKER: i64 = 0;
    let fresh_install = if sqlx::query_scalar::<_, i64>(
        "SELECT version FROM _openpiso_migrations WHERE version = ?",
    )
    .bind(FRESH_INSTALL_MARKER)
    .fetch_optional(pool)
    .await?
    .is_some()
    {
        true
    } else {
        let is_fresh =
            sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _openpiso_migrations")
                .fetch_one(pool)
                .await?
                .is_none();
        if is_fresh {
            sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (?)")
                .bind(FRESH_INSTALL_MARKER)
                .execute(pool)
                .await?;
        }
        is_fresh
    };

    let mut pending = Vec::new();
    for (version, migration) in migrations.iter().copied() {
        let applied = sqlx::query_scalar::<_, i64>(
            "SELECT version FROM _openpiso_migrations WHERE version = ?",
        )
        .bind(version)
        .fetch_optional(pool)
        .await?
        .is_some();
        if !applied {
            pending.push((version, migration));
        }
    }
    if pending.is_empty() {
        return Ok(());
    }

    let from_version =
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(pool)
            .await?
            .unwrap_or(0);
    let to_version = migrations
        .last()
        .map(|(version, _)| *version)
        .unwrap_or(from_version);
    let artifact =
        crate::backup::write_pre_migration_backup(pool, backup_directory, from_version, to_version)
            .await?;

    for (version, migration) in pending {
        if version == 37 {
            // Migration 0037 rebuilds the accounts table to remove SQLite's
            // original table-level UNIQUE constraint. Foreign-key checks must
            // be disabled before BEGIN; SQLite ignores that PRAGMA inside a
            // transaction. The migration itself remains atomic.
            let mut connection = pool.acquire().await?;
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await?;
            let migration_result: AppResult<()> = async {
                let mut transaction = connection.begin().await?;
                sqlx::raw_sql(migration).execute(&mut *transaction).await?;
                sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (?)")
                    .bind(version)
                    .execute(&mut *transaction)
                    .await?;
                transaction.commit().await?;
                Ok(())
            }
            .await;
            let foreign_keys_result = sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&mut *connection)
                .await;
            migration_result?;
            foreign_keys_result?;
        } else {
            let mut transaction = pool.begin().await?;
            sqlx::raw_sql(migration).execute(&mut *transaction).await?;
            if version == 38 && fresh_install {
                sqlx::query(
                    "INSERT INTO coin_device_pulse_map (pulse_count, amount_cents)
                     VALUES (20, 2000)",
                )
                .execute(&mut *transaction)
                .await?;
                sqlx::query(
                    "INSERT INTO settings (key, value) VALUES ('coin_calibration_required', 'true')
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                )
                .execute(&mut *transaction)
                .await?;
            }
            sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (?)")
                .bind(version)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
        }
    }
    if let Some(mirror_error) = artifact.mirror_error.as_deref() {
        if let Err(error) = backup_schedule::set_mirror_error(pool, Some(mirror_error)).await {
            warn!(
                target: "openpiso_admin_lib::db",
                "could not persist the migration Backup mirror advisory: {error}"
            );
        }
    }
    Ok(())
}

pub(crate) async fn seed(pool: &SqlitePool) -> AppResult<()> {
    let mut transaction = pool.begin().await?;

    sqlx::query(
        "INSERT OR IGNORE INTO settings (key, value) VALUES
         ('shop_name', 'OpenPiso Shop'),
         ('simulator_enabled', 'true'),
         ('shutdown_grace_seconds', '300'),
         ('offline_grace_seconds', '600')",
    )
    .execute(&mut *transaction)
    .await?;

    for (id, name) in [
        ("pc-01", "PC 01"),
        ("pc-02", "PC 02"),
        ("pc-03", "PC 03"),
        ("pc-04", "PC 04"),
    ] {
        sqlx::query("INSERT OR IGNORE INTO pcs (id, name) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(&mut *transaction)
            .await?;
    }

    sqlx::query(
        "INSERT OR IGNORE INTO rates (id, name, amount_cents, seconds, active)
         VALUES ('rate-default', '₱1 = 5 minutes', 100, 300, 1)",
    )
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT OR IGNORE INTO coin_devices (id, name, secret, active)
         VALUES ('dev-simulator', 'Development Simulator', 'simulator-only', 1)",
    )
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT OR IGNORE INTO coin_devices (id, name, secret, active)
         VALUES (?, 'Central Terminal Serial Acceptor', '', 1)",
    )
    .bind(LOCAL_SERIAL_DEVICE_ID)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(())
}

/// Build the M1 `CoinSourceConfig` from saved settings.
#[cfg(test)]
pub async fn coin_source_config(
    pool: &SqlitePool,
) -> AppResult<crate::coin_source::CoinSourceConfig> {
    let settings = settings::coin_device_settings(pool).await?;
    settings::coin_source_config_from_settings(&settings)
}

pub async fn dashboard(pool: &SqlitePool) -> AppResult<DashboardSnapshot> {
    let shop_name = settings::shop_name(pool).await?;
    let shutdown_grace = settings::shutdown_grace_seconds(pool).await?;

    let mut pcs: Vec<Pc> =
        sqlx::query_as::<_, (String, String, String, i64, Option<String>, Option<i64>)>(
            "SELECT id, name, status, remaining_seconds, last_seen_at,
                clock_difference_ms
         FROM pcs ORDER BY name",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(Pc::from)
        .collect();
    settings::apply_shutdown_grace(&mut pcs, shutdown_grace);

    let recent_transactions = transactions(pool, 10).await?;
    let today = today_report(pool).await?;
    let now = Utc::now();
    let pc_monitoring = DashboardPcMonitoring {
        enabled: monitoring::enabled(pool).await?,
        active_incidents: monitoring::active_kinds_by_pc(pool).await?,
        suppressions: monitoring::suppressions_by_pc(pool, now).await?,
        escalation_enabled: monitoring::escalation_settings(pool).await?.enabled,
    };

    Ok(DashboardSnapshot {
        shop_name,
        pcs,
        recent_transactions,
        today,
        pc_monitoring,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use uuid::Uuid;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        seed(&pool).await?;
        Ok(pool)
    }

    async fn schema_v35_mem_pool() -> SqlitePool {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        for (version, migration) in [
            (
                13_i64,
                include_str!("../../migrations/0013_fractional_points_centipoints.sql"),
            ),
            (
                14,
                include_str!("../../migrations/0014_redemption_tiers.sql"),
            ),
            (
                15,
                include_str!("../../migrations/0015_redemption_tier_api.sql"),
            ),
            (
                16,
                include_str!("../../migrations/0016_voucher_batches.sql"),
            ),
            (
                17,
                include_str!("../../migrations/0017_dash_insensitive_codes.sql"),
            ),
            (
                18,
                include_str!("../../migrations/0018_voucher_template.sql"),
            ),
            (
                19,
                include_str!("../../migrations/0019_physical_voucher_void_status.sql"),
            ),
            (
                20,
                include_str!("../../migrations/0020_voucher_batch_names.sql"),
            ),
            (
                21,
                include_str!("../../migrations/0021_individual_voucher_void.sql"),
            ),
            (
                22,
                include_str!("../../migrations/0022_inter_coin_gap_default.sql"),
            ),
            (
                23,
                include_str!("../../migrations/0023_coin_pin_polarity.sql"),
            ),
            (
                24,
                include_str!("../../migrations/0024_offline_grace_setting.sql"),
            ),
            (
                25,
                include_str!("../../migrations/0025_wheel_segment_rebuild.sql"),
            ),
            (
                26,
                include_str!("../../migrations/0026_points_minted_by_wheel.sql"),
            ),
            (
                27,
                include_str!("../../migrations/0027_equal_slice_wheel_segments.sql"),
            ),
            (28, include_str!("../../migrations/0028_bokya_rate.sql")),
            (29, include_str!("../../migrations/0029_coin_promo.sql")),
            (
                30,
                include_str!("../../migrations/0030_coin_promo_reporting.sql"),
            ),
            (31, include_str!("../../migrations/0031_shop_hours.sql")),
            (
                32,
                include_str!("../../migrations/0032_shop_hours_audit.sql"),
            ),
            (
                33,
                include_str!("../../migrations/0033_backup_schedule.sql"),
            ),
            (
                34,
                include_str!("../../migrations/0034_backup_schedule_next_due.sql"),
            ),
            (
                35,
                include_str!("../../migrations/0035_account_import_provenance.sql"),
            ),
        ] {
            sqlx::raw_sql(migration)
                .execute(&pool)
                .await
                .expect("schema v35 migration should apply");
            sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (?)")
                .bind(version)
                .execute(&pool)
                .await
                .expect("schema v35 migration should be recorded");
        }
        pool
    }

    async fn schema_v37_mem_pool() -> AppResult<SqlitePool> {
        let pool = schema_v35_mem_pool().await;
        sqlx::raw_sql(include_str!(
            "../../migrations/0036_notification_read_state.sql"
        ))
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (36)")
            .execute(&pool)
            .await?;
        let mut connection = pool.acquire().await?;
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await?;
        let migration_result: AppResult<()> = async {
            let mut transaction = connection.begin().await?;
            sqlx::raw_sql(include_str!(
                "../../migrations/0037_active_account_username_uniqueness.sql"
            ))
            .execute(&mut *transaction)
            .await?;
            sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (37)")
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok(())
        }
        .await;
        let foreign_keys_result = sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await;
        migration_result?;
        foreign_keys_result?;
        drop(connection);
        Ok(pool)
    }

    /// Builds a pool at schema version 13 so the Redemption Tiers seed can be
    /// tested independently from normal startup.
    async fn pre_redemption_tiers_pool() -> AppResult<SqlitePool> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        sqlx::raw_sql(include_str!(
            "../../migrations/0013_fractional_points_centipoints.sql"
        ))
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (13)")
            .execute(&pool)
            .await?;
        Ok(pool)
    }

    #[tokio::test]
    async fn pending_migrations_write_cataloged_pre_migration_artifacts() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('mem_backup_marker', 'v12-mem-marker')",
        )
        .execute(&pool)
        .await?;
        let work = crate::test_utils::migration_backup_dir();
        let directory = work.path().join("backups");

        run_migrations(&pool, &directory).await?;

        let mut filenames = std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        filenames.sort();
        assert_eq!(filenames.len(), 2);
        let target = format!("v12-to-v{CURRENT_SCHEMA_VERSION}");
        assert!(filenames.iter().any(|name| {
            name.ends_with(&format!("-{target}.sql")) && name.starts_with("openpiso-pre_migration-")
        }));
        assert!(filenames.iter().any(|name| {
            name.ends_with(&format!("-{target}.db")) && name.starts_with("openpiso-pre_migration-")
        }));

        let sql_name = filenames
            .iter()
            .find(|name| name.ends_with(".sql"))
            .expect("SQL Pre-Migration Backup");
        let sql = std::fs::read_to_string(directory.join(sql_name))?;
        let header = crate::backup::parse_backup_header(&sql)?;
        assert_eq!(header.kind, "pre_migration");
        assert_eq!(header.schema_version, 12);
        assert_eq!(header.sets.len(), crate::backup::BackupSet::all().len());
        assert!(header.verify_checksum(&sql));

        let catalog = crate::backup::list_backups(&directory).await?;
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].kind, "pre_migration");
        assert_eq!(catalog[0].backup_sets.len(), 7);
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&pool)
            .await?;
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        let live_marker: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'mem_backup_marker'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(live_marker, "v12-mem-marker");

        // Default connect options never create a missing file, so a bad path fails here.
        let companion = directory.join(
            filenames
                .iter()
                .find(|name| name.ends_with(".db"))
                .expect("database companion"),
        );
        assert!(std::fs::metadata(&companion)?.len() > 0);
        let copy = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&companion))
            .await?;
        let copy_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&copy)
            .await?;
        let copy_marker: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'mem_backup_marker'")
                .fetch_one(&copy)
                .await?;
        assert_eq!(copy_version, 12);
        assert_eq!(copy_marker, "v12-mem-marker");
        copy.close().await;

        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn attendance_migrations_create_report_query_indexes() -> AppResult<()> {
        let pool = test_pool().await?;
        let expected_indexes: [(&str, &str, &[&str]); 10] = [
            (
                "idx_attendance_settings_effective",
                "attendance_settings_versions",
                &["effective_at", "version"],
            ),
            (
                "idx_shop_hours_versions_effective",
                "shop_hours_versions",
                &["effective_at", "id"],
            ),
            (
                "idx_attendance_intervals_date_start",
                "attendance_eligible_intervals",
                &["earning_date", "starts_at"],
            ),
            (
                "idx_attendance_contributions_account_date",
                "attendance_contributions",
                &["account_id", "earning_date"],
            ),
            (
                "idx_attendance_earning_months_deadline",
                "attendance_earning_months",
                &["deadline_at_exclusive"],
            ),
            (
                "idx_attendance_check_ins_date",
                "attendance_check_ins",
                &["earning_date"],
            ),
            (
                "idx_attendance_check_ins_account_month",
                "attendance_check_ins",
                &["account_id", "earning_month_id"],
            ),
            (
                "idx_attendance_claims_month_created",
                "attendance_claims",
                &["earning_month_id", "created_at"],
            ),
            (
                "idx_attendance_claims_account_created",
                "attendance_claims",
                &["account_id", "created_at"],
            ),
            (
                "idx_attendance_forfeitures_account_month_deleted",
                "attendance_forfeitures",
                &["account_id", "earning_month_id", "deleted_at"],
            ),
        ];

        for (index, table, expected_columns) in expected_indexes {
            let table_name: Option<String> = sqlx::query_scalar(
                "SELECT tbl_name FROM sqlite_master WHERE type = 'index' AND name = ?",
            )
            .bind(index)
            .fetch_optional(&pool)
            .await?;
            assert_eq!(table_name.as_deref(), Some(table));

            let pragma = format!("PRAGMA index_info('{index}')");
            let columns: Vec<String> = sqlx::query_as::<_, (i64, i64, String)>(&pragma)
                .fetch_all(&pool)
                .await?
                .into_iter()
                .map(|(_, _, column)| column)
                .collect();
            assert_eq!(
                columns,
                expected_columns
                    .iter()
                    .map(|column| (*column).to_owned())
                    .collect::<Vec<_>>(),
                "index {index} columns"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn fresh_install_seeds_disabled_attendance_defaults() -> AppResult<()> {
        let pool = test_pool().await?;
        let settings: Vec<(i64, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT version, enabled, daily_threshold_cents, reward_seconds, claim_window_months
             FROM attendance_settings_versions ORDER BY version",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(settings, vec![(1, 0, 1_000, 3_600, 1)]);
        Ok(())
    }

    #[tokio::test]
    async fn attendance_migration_rejects_invalid_settings_and_clock_rows() -> AppResult<()> {
        let pool = crate::test_utils::mem_test_pool().await;
        assert!(sqlx::query("INSERT INTO attendance_settings_versions (version, enabled, daily_threshold_cents, reward_seconds, claim_window_months, effective_at) VALUES (2, 2, 1000, 3600, 1, '2026-01-01T00:00:00Z')")
            .execute(&pool).await.is_err());
        assert!(sqlx::query(
            "UPDATE attendance_settings_versions SET reward_seconds = 60 WHERE version = 1"
        )
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query("INSERT INTO attendance_settings_versions (version, enabled, daily_threshold_cents, reward_seconds, claim_window_months, effective_at) VALUES (2, 0, 1000, 3600, 1, '2026-01-01T00:00:00+08:00')")
            .execute(&pool).await.is_err());
        assert!(sqlx::query("INSERT INTO attendance_calendar_dates (earning_date, finalized_at, restart_observed_utc_offset) VALUES ('2026-01-01', 'not-an-instant', 0)")
            .execute(&pool).await.is_err());
        assert!(sqlx::query("INSERT INTO attendance_clock_state (id, utc_floor, open_local_date) VALUES (2, '2026-01-01T00:00:00Z', '2026-01-01')")
            .execute(&pool).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn attendance_migration_seeds_upgrade_evidence_without_expired_override() -> AppResult<()>
    {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::raw_sql("CREATE TABLE audit_log (id TEXT PRIMARY KEY); CREATE TABLE shop_hours_settings (id INTEGER PRIMARY KEY, enabled INTEGER, last_call_lead_minutes INTEGER); CREATE TABLE shop_hours_schedule (weekday INTEGER PRIMARY KEY, mode TEXT, open_minute INTEGER, close_minute INTEGER); CREATE TABLE shop_override (id INTEGER PRIMARY KEY, mode TEXT, expires_at TEXT); INSERT INTO shop_hours_settings VALUES (1, 1, 20); INSERT INTO shop_hours_schedule VALUES (0, 'open_period', 600, 1200); INSERT INTO shop_override VALUES (1, 'force_open', '2000-01-01T00:00:00Z');")
            .execute(&pool).await?;
        sqlx::raw_sql(include_str!(
            "../../migrations/0055_attendance_foundation.sql"
        ))
        .execute(&pool)
        .await?;
        let settings: (i64, i64, i64, i64, i64) = sqlx::query_as("SELECT version, enabled, daily_threshold_cents, reward_seconds, claim_window_months FROM attendance_settings_versions")
            .fetch_one(&pool).await?;
        assert_eq!(settings, (1, 0, 1000, 3600, 1));
        let hours: (i64, i64) =
            sqlx::query_as("SELECT enabled, last_call_lead_minutes FROM shop_hours_versions")
                .fetch_one(&pool)
                .await?;
        assert_eq!(hours, (1, 20));
        let days: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shop_hours_version_days WHERE weekday = 0 AND open_minute = 600 AND close_minute = 1200")
            .fetch_one(&pool).await?;
        assert_eq!(days, 1);
        let overrides: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shop_override_history")
            .fetch_one(&pool)
            .await?;
        assert_eq!(overrides, 0);
        assert!(sqlx::query("DELETE FROM shop_hours_versions WHERE id = 1")
            .execute(&pool)
            .await
            .is_err());
        assert!(
            sqlx::query("DELETE FROM attendance_settings_versions WHERE version = 1")
                .execute(&pool)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn attendance_migration_seeds_active_override_on_fresh_install() -> AppResult<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::raw_sql("CREATE TABLE audit_log (id TEXT PRIMARY KEY); CREATE TABLE shop_hours_settings (id INTEGER PRIMARY KEY, enabled INTEGER, last_call_lead_minutes INTEGER); CREATE TABLE shop_hours_schedule (weekday INTEGER PRIMARY KEY, mode TEXT, open_minute INTEGER, close_minute INTEGER); CREATE TABLE shop_override (id INTEGER PRIMARY KEY, mode TEXT, expires_at TEXT); INSERT INTO shop_hours_settings VALUES (1, 0, 15); INSERT INTO shop_hours_schedule VALUES (0, 'open_period', 480, 1320); INSERT INTO shop_override VALUES (1, 'force_closed', '2999-01-01T00:00:00+00:00');")
            .execute(&pool).await?;
        sqlx::raw_sql(include_str!(
            "../../migrations/0055_attendance_foundation.sql"
        ))
        .execute(&pool)
        .await?;
        let (mode, expiry): (String, String) =
            sqlx::query_as("SELECT mode, expires_at FROM shop_override_history")
                .fetch_one(&pool)
                .await?;
        assert_eq!(mode, "force_closed");
        assert!(expiry.ends_with('Z'));
        let (floor, date): (String, String) = sqlx::query_as(
            "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert!(floor.ends_with('Z'));
        assert_eq!(date.len(), 10);
        assert!(
            sqlx::query("DELETE FROM shop_override_history WHERE id = 1")
                .execute(&pool)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn onboarding_grace_migration_preserves_custom_values() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        sqlx::query("INSERT INTO settings (key, value) VALUES ('offline_grace_seconds', '120')")
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!(
            "../../migrations/0054_onboarding_grace_defaults.sql"
        ))
        .execute(&pool)
        .await?;
        for (key, expected) in [
            ("shutdown_grace_seconds", "300"),
            ("offline_grace_seconds", "600"),
        ] {
            let actual: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_one(&pool)
                .await?;
            assert_eq!(actual, expected);
        }
        for (key, value) in [
            ("shutdown_grace_seconds", "60"),
            ("offline_grace_seconds", "240"),
        ] {
            sqlx::query("UPDATE settings SET value = ? WHERE key = ?")
                .bind(value)
                .bind(key)
                .execute(&pool)
                .await?;
        }
        sqlx::raw_sql(include_str!(
            "../../migrations/0054_onboarding_grace_defaults.sql"
        ))
        .execute(&pool)
        .await?;
        for (key, expected) in [
            ("shutdown_grace_seconds", "60"),
            ("offline_grace_seconds", "240"),
        ] {
            let actual: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_one(&pool)
                .await?;
            assert_eq!(actual, expected);
        }
        Ok(())
    }

    #[tokio::test]
    async fn coin_20_candidate_is_fresh_install_only() -> AppResult<()> {
        let fresh = crate::test_utils::schema_v12_mem_pool().await;
        // The v12 fixture is intentionally an existing installation: its
        // migration history is non-empty, so 0038 must not alter the map.
        run_migrations(&fresh, crate::test_utils::migration_backup_dir().path()).await?;
        let upgraded_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM coin_device_pulse_map WHERE pulse_count = 20")
                .fetch_one(&fresh)
                .await?;
        assert_eq!(upgraded_count, 0);
        let new_database = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        run_migrations(
            &new_database,
            crate::test_utils::migration_backup_dir().path(),
        )
        .await?;
        let fresh_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM coin_device_pulse_map WHERE pulse_count = 20")
                .fetch_one(&new_database)
                .await?;
        assert_eq!(fresh_count, 1);

        // An interruption after v37 leaves a contiguous history. The marker
        // determines whether v38 seeds the fresh-install candidate on resume.
        let resumed_upgrade = schema_v37_mem_pool().await?;
        run_migrations(
            &resumed_upgrade,
            crate::test_utils::migration_backup_dir().path(),
        )
        .await?;
        let resumed_upgrade_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM coin_device_pulse_map WHERE pulse_count = 20")
                .fetch_one(&resumed_upgrade)
                .await?;
        assert_eq!(resumed_upgrade_count, 0);

        let resumed_fresh = schema_v37_mem_pool().await?;
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (0)")
            .execute(&resumed_fresh)
            .await?;
        run_migrations(
            &resumed_fresh,
            crate::test_utils::migration_backup_dir().path(),
        )
        .await?;
        let resumed_fresh_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM coin_device_pulse_map WHERE pulse_count = 20")
                .fetch_one(&resumed_fresh)
                .await?;
        assert_eq!(resumed_fresh_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn newer_migration_history_is_rejected_before_backup_or_marker() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (?)")
            .bind(CURRENT_SCHEMA_VERSION + 1)
            .execute(&pool)
            .await?;
        let directory = crate::test_utils::migration_backup_dir();

        let error = run_migrations(&pool, directory.path()).await.unwrap_err();
        assert!(matches!(error, AppError::UnsupportedMigrationHistory(_)));
        assert!(error
            .to_string()
            .contains(&format!("migration {}", CURRENT_SCHEMA_VERSION + 1)));
        assert!(error
            .to_string()
            .contains(&format!("up to {CURRENT_SCHEMA_VERSION}")));
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        let marker: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _openpiso_migrations WHERE version = 0")
                .fetch_one(&pool)
                .await?;
        assert_eq!(marker, 0);
        Ok(())
    }

    #[tokio::test]
    async fn migration_history_gap_is_rejected_before_backup() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        sqlx::query("DELETE FROM _openpiso_migrations WHERE version = 6")
            .execute(&pool)
            .await?;
        let directory = crate::test_utils::migration_backup_dir();

        let error = run_migrations(&pool, directory.path()).await.unwrap_err();
        assert!(matches!(error, AppError::UnsupportedMigrationHistory(_)));
        assert!(error.to_string().contains("missing version 6"));
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn no_pending_migrations_write_no_backup_artifacts() -> AppResult<()> {
        let pool = test_pool().await?;
        let directory =
            std::env::temp_dir().join(format!("openpiso-no-migration-{}", Uuid::new_v4()));

        run_migrations(&pool, &directory).await?;

        assert!(!directory.exists());
        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn active_username_migration_preserves_tombstones_and_foreign_keys() -> AppResult<()> {
        let pool = schema_v35_mem_pool().await;
        let old_id = "historical-account";
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash)
             VALUES (?, 'Reimportable', 'reimportable', 'hash')",
        )
        .bind(old_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "UPDATE accounts SET status = 'deleted', deleted_at = CURRENT_TIMESTAMP
             WHERE id = ?",
        )
        .bind(old_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, account_id, summary)
             VALUES ('historical-account-deleted', 'account_deleted', 'success', 'admin', ?,
                     'Soft-deleted customer account')",
        )
        .bind(old_id)
        .execute(&pool)
        .await?;

        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;

        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await?;
        assert_eq!(foreign_keys, 1);
        assert!(sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await?
            .is_empty());

        let new_id = "reimported-account";
        sqlx::query(
            "INSERT INTO accounts (id, username, username_normalized, password_hash)
             VALUES (?, ?, ?, ?)",
        )
        .bind(new_id)
        .bind("Reimportable")
        .bind("reimportable")
        .bind("hash")
        .execute(&pool)
        .await?;
        crate::accounts::soft_delete(&pool, new_id).await?;

        let deleted_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM accounts
             WHERE username_normalized = 'reimportable' AND status = 'deleted'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(deleted_count, 2);
        let preserved_audit: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE account_id = ? AND event_type = 'account_deleted'",
        )
        .bind(old_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(preserved_audit, 1);
        Ok(())
    }

    #[tokio::test]
    async fn telemetry_migration_has_no_consent_and_constrains_outbox() -> AppResult<()> {
        let pool = test_pool().await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telemetry_settings")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0);
        assert!(
            sqlx::query("INSERT INTO telemetry_settings (id) VALUES (1)")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query(
            "INSERT INTO telemetry_settings (id, installation_id) VALUES (2, 'test-id')"
        )
        .execute(&pool)
        .await
        .is_err());
        let columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as("PRAGMA table_info(telemetry_settings)")
                .fetch_all(&pool)
                .await?;
        assert!(!columns
            .iter()
            .any(|column| column.1 == "enabled" || column.1 == "enabled_at"));

        sqlx::query(
            "INSERT INTO telemetry_outbox (period_utc8, event_uuid, payload, next_attempt_at)
             VALUES ('2026-09-23', 'event-1', '{}', '2026-09-23T00:00:00Z')",
        )
        .execute(&pool)
        .await?;
        assert!(sqlx::query(
            "INSERT INTO telemetry_outbox (period_utc8, event_uuid, payload, next_attempt_at)
             VALUES ('2026-09-23', 'event-2', '{}', '2026-09-23T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            "INSERT INTO telemetry_outbox (period_utc8, event_uuid, payload, attempts, next_attempt_at)
             VALUES ('2026-09-24', 'event-2', '{}', -1, '2026-09-23T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn notification_migration_creates_read_state_and_enforces_foreign_keys() -> AppResult<()>
    {
        let pool = test_pool().await?;

        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&pool)
            .await?;
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM notification_read_state WHERE id = 1",
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_alert_ack")
                .fetch_one(&pool)
                .await?,
            0
        );
        assert!(sqlx::query(
            "INSERT INTO notification_read_state (id, last_seen_at) VALUES (2, CURRENT_TIMESTAMP)"
        )
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query("INSERT INTO notification_alert_ack (audit_log_id, acknowledged_at) VALUES ('missing', CURRENT_TIMESTAMP)")
            .execute(&pool)
            .await
            .is_err());
        sqlx::query("INSERT INTO audit_log (id, event_type, outcome, actor, summary) VALUES ('alert', 'tamper_kill', 'success', 'system', 'alert')")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO notification_alert_ack (audit_log_id, acknowledged_at) VALUES ('alert', CURRENT_TIMESTAMP)")
            .execute(&pool)
            .await?;
        assert!(sqlx::query("INSERT INTO notification_alert_ack (audit_log_id, acknowledged_at) VALUES ('alert', CURRENT_TIMESTAMP)")
            .execute(&pool)
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn notification_migration_baselines_only_preexisting_alerts() -> AppResult<()> {
        let pool = schema_v35_mem_pool().await;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, summary) VALUES
             ('alert', 'tamper_kill', 'success', 'system', 'alert'),
             ('notice', 'shop_opened', 'success', 'system', 'notice'),
             ('silent', 'login_failed', 'denied', 'account', 'silent')",
        )
        .execute(&pool)
        .await?;

        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;

        let acknowledged: Vec<String> = sqlx::query_scalar(
            "SELECT audit_log_id FROM notification_alert_ack ORDER BY audit_log_id",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(acknowledged, vec!["alert"]);
        let watermark: String =
            sqlx::query_scalar("SELECT last_seen_at FROM notification_read_state WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        assert!(!watermark.is_empty());
        let notification_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'notification_acknowledged'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(notification_events, 0);
        Ok(())
    }

    #[tokio::test]
    async fn unwritable_backup_directory_fails_before_migration() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        let directory =
            std::env::temp_dir().join(format!("openpiso-unwritable-{}", Uuid::new_v4()));
        std::fs::write(&directory, b"not a directory")?;

        let error = run_migrations(&pool, &directory)
            .await
            .expect_err("migration must fail when the Backup directory cannot be written");
        let message = error.to_string().to_ascii_lowercase();
        assert!(
            message.contains("filesystem error")
                && (message.contains("directory")
                    || message.contains("permission")
                    || message.contains("file exists")
                    || message.contains("cannot create")),
            "error must preserve the OS cause: {error}"
        );
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&pool)
            .await?;
        assert_eq!(version, 12);

        pool.close().await;
        let _ = std::fs::remove_file(directory);
        Ok(())
    }

    /// File-backed WAL database (single connection, short busy timeout) at
    /// genuine schema v12, replayed from the canonical Backup SQL of the shared
    /// in-memory v12 fixture so the on-disk companion path is exercised.
    async fn schema_v12_file_pool(work: &Path) -> AppResult<SqlitePool> {
        let source = crate::test_utils::schema_v12_mem_pool().await;
        let artifact =
            crate::backup::write_pre_migration_backup(&source, &work.join("fixture"), 12, 12)
                .await?;
        source.close().await;
        let options = SqliteConnectOptions::new()
            .filename(work.join("live.sqlite"))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_millis(100));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::raw_sql(&std::fs::read_to_string(artifact.path)?)
            .execute(&pool)
            .await?;
        Ok(pool)
    }

    #[tokio::test]
    async fn file_backed_migration_companion_holds_wal_state_at_pre_migration_version(
    ) -> AppResult<()> {
        let work = crate::test_utils::migration_backup_dir();
        let pool = schema_v12_file_pool(work.path()).await?;
        // Committed only to the WAL: the companion must still contain it.
        sqlx::query("INSERT INTO settings (key, value) VALUES ('wal_probe', 'kept')")
            .execute(&pool)
            .await?;
        let directory = work.path().join("backups");

        run_migrations(&pool, &directory).await?;

        let companion = std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find(|path| path.extension().is_some_and(|ext| ext == "db"))
            .expect("database companion");
        let copy = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&companion))
            .await?;
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&copy)
            .await?;
        let probe: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'wal_probe'")
                .fetch_one(&copy)
                .await?;
        assert_eq!(version, 12);
        assert_eq!(probe, "kept");
        copy.close().await;
        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn incomplete_wal_checkpoint_removes_sql_backup_and_blocks_migration() -> AppResult<()> {
        use sqlx::sqlite::SqliteConnection;

        let work = crate::test_utils::migration_backup_dir();
        let pool = schema_v12_file_pool(work.path()).await?;
        let database = work.path().join("live.sqlite");
        // A reader holding an older snapshot keeps the TRUNCATE checkpoint busy.
        let mut reader =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&database))
                .await?;
        sqlx::query("BEGIN").execute(&mut reader).await?;
        sqlx::query("SELECT COUNT(*) FROM settings")
            .fetch_one(&mut reader)
            .await?;
        sqlx::query("INSERT INTO settings (key, value) VALUES ('after_snapshot', '1')")
            .execute(&pool)
            .await?;
        let directory = work.path().join("backups");

        run_migrations(&pool, &directory)
            .await
            .expect_err("incomplete checkpoint must fail migration");

        assert_eq!(std::fs::read_dir(&directory)?.count(), 0);
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&pool)
            .await?;
        assert_eq!(version, 12);
        reader.close().await?;
        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn redemption_tiers_migration_seeds_default_pair_exactly() -> AppResult<()> {
        let pool = pre_redemption_tiers_pool().await?;
        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        let tier: (i64, i64) = sqlx::query_as("SELECT points, seconds FROM redemption_tiers")
            .fetch_one(&pool)
            .await?;
        assert_eq!(tier, (10, 14_400));
        Ok(())
    }

    #[tokio::test]
    async fn redemption_tiers_migration_grandfathers_invalid_pair_exactly() -> AppResult<()> {
        let pool = pre_redemption_tiers_pool().await?;
        sqlx::query(
            "UPDATE economy_settings SET redeem_points = 3, redeem_seconds = 17 WHERE id = 1",
        )
        .execute(&pool)
        .await?;

        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;

        let tier: (i64, i64) = sqlx::query_as("SELECT points, seconds FROM redemption_tiers")
            .fetch_one(&pool)
            .await?;
        assert_eq!(tier, (3, 17));
        Ok(())
    }

    #[tokio::test]
    async fn centipoint_migration_backfills_x100_and_pays_out_carry() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        // Whole-point world: 12 points balance, ₱25 (2 500 cents) of carry at
        // the default ₱50/point earn rate, plus historical rows in every
        // renamed table.
        sqlx::query(
            "INSERT INTO accounts
               (id, username, username_normalized, password_hash,
                points_balance, point_earn_carry_cents)
             VALUES ('acct-1', 'legacy', 'legacy', 'hash', 12, 2500)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, points_delta, summary)
             VALUES ('evt-1', 'points_redeemed', 'success', 'account', -10, 'legacy redeem')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO audit_log (id, event_type, outcome, actor, summary)
             VALUES ('evt-mint', 'voucher_minted', 'success', 'admin', 'legacy mint')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO vouchers
               (id, code, kind, status, seconds_value, points_value, audit_event_id)
             VALUES ('v-1', 'CODE01', 'promo', 'active', 0, 3, 'evt-mint')",
        )
        .execute(&pool)
        .await?;

        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;

        // Balance ×100 plus the carry payout: 2 500 cents = 250 000
        // centicents / 5 000 = 50 centipoints, no sub-centipoint remainder.
        let account: (i64, i64) = sqlx::query_as(
            "SELECT points_balance_centi, point_earn_carry_centicents
             FROM accounts WHERE id = 'acct-1'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account, (1_250, 0));
        let delta: i64 =
            sqlx::query_scalar("SELECT points_delta_centi FROM audit_log WHERE id = 'evt-1'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(delta, -1_000);
        let voucher: i64 =
            sqlx::query_scalar("SELECT points_value_centi FROM vouchers WHERE id = 'v-1'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(voucher, 300);
        let liability: i64 =
            sqlx::query_scalar("SELECT total_points_liability_centi FROM report_points_liability")
                .fetch_one(&pool)
                .await?;
        assert_eq!(liability, 1_250);
        Ok(())
    }

    #[tokio::test]
    async fn centipoint_migration_carry_payout_keeps_sub_centipoint_remainder() -> AppResult<()> {
        let pool = crate::test_utils::schema_v12_mem_pool().await;
        // An earn rate not divisible by 100: carry of 17 cents at 333
        // cents/point is 1 700 centicents = 5 centipoints + 35 centicents.
        sqlx::query("UPDATE economy_settings SET earn_cents_per_point = 333 WHERE id = 1")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO accounts
               (id, username, username_normalized, password_hash,
                points_balance, point_earn_carry_cents)
             VALUES ('acct-2', 'oddrate', 'oddrate', 'hash', 0, 17)",
        )
        .execute(&pool)
        .await?;

        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;

        let account: (i64, i64) = sqlx::query_as(
            "SELECT points_balance_centi, point_earn_carry_centicents
             FROM accounts WHERE id = 'acct-2'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(account, (5, 35));
        Ok(())
    }

    #[tokio::test]
    async fn immediate_write_transactions_queue_instead_of_erroring() -> AppResult<()> {
        // A file-backed WAL pool is required: `sqlite::memory:` connections
        // don't share a database, so they cannot contend for the writer.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("openpiso-busy-{nanos}.sqlite"));
        let pool = connect(&path, crate::test_utils::migration_backup_dir().path()).await?;

        // Hold an immediate write transaction open on one connection; it has
        // reserved the writer.
        let mut held = begin_immediate(&pool).await?;
        sqlx::query("UPDATE settings SET value = 'held' WHERE key = 'shop_name'")
            .execute(&mut *held)
            .await?;

        // On a second connection, another immediate write transaction must
        // block on busy_timeout and queue — not fail with SQLITE_BUSY (code 5).
        let pool2 = pool.clone();
        let contender = tokio::spawn(async move {
            let mut tx = begin_immediate(&pool2).await?;
            sqlx::query("UPDATE settings SET value = 'contender' WHERE key = 'shop_name'")
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok::<(), crate::errors::AppError>(())
        });

        // Let the contender reach BEGIN IMMEDIATE and block on the writer lock,
        // then release the writer so the queued contender proceeds.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        held.commit().await?;

        contender
            .await
            .expect("contender task should join")
            .expect("queued immediate write should succeed, not error with SQLITE_BUSY");

        let value: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'shop_name'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(value, "contender");

        pool.close().await;
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn audit_log_is_append_only() -> AppResult<()> {
        let pool = test_pool().await?;
        coins::simulate_coin(&pool, "pc-01", 100).await?;
        assert!(sqlx::query("UPDATE audit_log SET summary = 'changed'")
            .execute(&pool)
            .await
            .is_err());
        assert!(sqlx::query("DELETE FROM audit_log")
            .execute(&pool)
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn migration_backfills_financial_history_without_duplicate_coin_events() -> AppResult<()>
    {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query(
            "CREATE TABLE _openpiso_migrations (
               version INTEGER PRIMARY KEY,
               applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             )",
        )
        .execute(&pool)
        .await?;
        sqlx::raw_sql(include_str!("../../migrations/0001_initial.sql"))
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (1)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO pcs (id, name) VALUES ('pc-test', 'PC Test')")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO coin_transactions (
               id, pc_id, amount_cents, seconds_added, source
             ) VALUES ('coin-old', 'pc-test', 100, 300, 'simulator')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO audit_logs (
               id, action, entity_type, entity_id
             ) VALUES ('audit-old', 'coin.accepted', 'coin_transaction', 'coin-old')",
        )
        .execute(&pool)
        .await?;

        run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;

        let coin_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_type = 'coin_accepted'")
                .fetch_one(&pool)
                .await?;
        let legacy_duplicates: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE id = 'audit-old'")
                .fetch_one(&pool)
                .await?;
        let (linked_event, coin_promo_bonus_seconds): (String, i64) = sqlx::query_as(
            "SELECT audit_event_id, coin_promo_bonus_seconds
             FROM coin_transactions WHERE id = 'coin-old'",
        )
        .fetch_one(&pool)
        .await?;
        let (coin_promo_enabled, coin_promo_tier_count): (i64, i64) = sqlx::query_as(
            "SELECT enabled, (SELECT COUNT(*) FROM coin_promo_tiers)
             FROM coin_promo_settings WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(coin_events, 1);
        assert_eq!(legacy_duplicates, 0);
        assert_eq!(linked_event, "m1-coin-coin-old");
        assert_eq!(coin_promo_bonus_seconds, 0);
        assert_eq!(coin_promo_enabled, 0);
        assert_eq!(coin_promo_tier_count, 0);
        Ok(())
    }
}

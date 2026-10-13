/// Shared test utilities for the admin crate.
///
/// Provides file-backed WAL test pool used by both repository-level
/// and HTTP-level concurrency tests — one shared helper, not duplicated
/// pool builders.
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db;
use crate::run_record::{RunRecord, RUN_RECORDS_FILE_NAME};

pub(crate) fn migration_backup_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("migration Backup directory should be created")
}

/// Seed the Run Record file (ADR-0082) under `dir` with `records` in append
/// order (newest last). Shared by the host-evidence tests so the file-write
/// shape is written once.
pub(crate) fn seed_run_records(dir: &std::path::Path, records: &[RunRecord]) {
    std::fs::write(
        dir.join(RUN_RECORDS_FILE_NAME),
        serde_json::to_string(records).expect("seed history serializes"),
    )
    .expect("seed history writes");
}

/// Plant a panic file at `<log_dir>/panic.log` shaped for the slice tests:
/// one marker older than the newest 8 KB, filler pushing it past the cap,
/// and the newest evidence at the tail. Returns the full byte size, so a
/// test can assert the reported `size_bytes`.
pub(crate) fn write_panic_slice_fixture(log_dir: &std::path::Path) -> u64 {
    std::fs::create_dir_all(log_dir).expect("logs dir creates");
    let older = "OLD-EVIDENCE\n";
    let filler = "x".repeat(crate::host_diagnostics::PANIC_SLICE_BYTES + 4096 - older.len());
    let body = format!("{older}{filler}TAIL-MARKER crash evidence\n");
    std::fs::write(log_dir.join(crate::diagnostics::PANIC_FILE_NAME), &body)
        .expect("panic file writes");
    body.len() as u64
}

/// WAL-mode, multi-connection test pool backed by a temp file. Returns the
/// pool and the file path so the caller can delete the file after closing.
///
/// `name` is a human-readable slug embedded in the filename for debugging.
pub(crate) async fn wal_test_pool(name: &str) -> (SqlitePool, PathBuf) {
    wal_test_pool_with_max_connections(name, 5).await
}

/// File-backed WAL test pool with a caller-selected connection limit.
pub(crate) async fn wal_test_pool_with_max_connections(
    name: &str,
    max_connections: u32,
) -> (SqlitePool, PathBuf) {
    let path = std::env::temp_dir().join(format!("openpiso-{name}-{}.sqlite", Uuid::new_v4()));
    let pool = connect_wal_test_pool(&path, true, max_connections).await;
    let backup_directory = migration_backup_dir();
    db::run_migrations(&pool, backup_directory.path())
        .await
        .expect("test migration should run");
    db::seed(&pool).await.expect("test data should seed");
    (pool, path)
}

/// Reopen an existing file-backed test database without migrating or seeding.
pub(crate) async fn reopen_wal_test_pool(path: &Path) -> SqlitePool {
    connect_wal_test_pool(path, false, 5).await
}

async fn connect_wal_test_pool(
    path: &Path,
    create_if_missing: bool,
    max_connections: u32,
) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create_if_missing)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
        .expect("test WAL database should connect")
}

/// In-memory pool at schema version 12, before the centipoints migration.
/// Shared so both migration tests and Backup round-trip tests can produce
/// artifacts from an older schema without duplicating pool builders.
pub(crate) async fn schema_v12_mem_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("schema v12 database should connect");
    sqlx::query(
        "CREATE TABLE _openpiso_migrations (
           version INTEGER PRIMARY KEY,
           applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         )",
    )
    .execute(&pool)
    .await
    .expect("migration history table should be created");
    for (version, migration) in [
        (1_i64, include_str!("../migrations/0001_initial.sql")),
        (2, include_str!("../migrations/0002_accounts_audit.sql")),
        (3, include_str!("../migrations/0003_points_economy.sql")),
        (
            4,
            include_str!("../migrations/0004_vouchers_redemption.sql"),
        ),
        (5, include_str!("../migrations/0005_spin_the_wheel.sql")),
        (
            6,
            include_str!("../migrations/0006_reporting_audit_review.sql"),
        ),
        (
            7,
            include_str!("../migrations/0007_coin_device_settings.sql"),
        ),
        (
            8,
            include_str!("../migrations/0008_terminal_selection_timers.sql"),
        ),
        (9, include_str!("../migrations/0009_account_transfers.sql")),
        (10, include_str!("../migrations/0010_pc_identity.sql")),
        (
            11,
            include_str!("../migrations/0011_shutdown_grace_setting.sql"),
        ),
        (12, include_str!("../migrations/0012_theme_settings.sql")),
    ] {
        sqlx::raw_sql(migration)
            .execute(&pool)
            .await
            .expect("schema v12 migration should apply");
        sqlx::query("INSERT INTO _openpiso_migrations (version) VALUES (?)")
            .bind(version)
            .execute(&pool)
            .await
            .expect("schema v12 migration should be recorded");
    }
    pool
}

/// Append one immutable Attendance settings version for repository tests.
pub(crate) async fn enable_attendance(
    pool: &SqlitePool,
    version: i64,
    effective_at: chrono::DateTime<chrono::Utc>,
    threshold_cents: i64,
    reward_seconds: i64,
) -> crate::errors::AppResult<()> {
    insert_attendance_settings(
        pool,
        version,
        true,
        threshold_cents,
        reward_seconds,
        effective_at,
    )
    .await
}

pub(crate) async fn insert_attendance_settings(
    pool: &SqlitePool,
    version: i64,
    enabled: bool,
    threshold_cents: i64,
    reward_seconds: i64,
    effective_at: chrono::DateTime<chrono::Utc>,
) -> crate::errors::AppResult<()> {
    insert_attendance_settings_with_window(
        pool,
        version,
        enabled,
        threshold_cents,
        reward_seconds,
        1,
        effective_at,
    )
    .await
}

pub(crate) async fn insert_attendance_settings_with_window(
    pool: &SqlitePool,
    version: i64,
    enabled: bool,
    threshold_cents: i64,
    reward_seconds: i64,
    claim_window_months: i64,
    effective_at: chrono::DateTime<chrono::Utc>,
) -> crate::errors::AppResult<()> {
    sqlx::query(
        "INSERT INTO attendance_settings_versions
           (version, enabled, daily_threshold_cents, reward_seconds,
            claim_window_months, effective_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(version)
    .bind(i64::from(enabled))
    .bind(threshold_cents)
    .bind(reward_seconds)
    .bind(claim_window_months)
    .bind(effective_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    .execute(pool)
    .await?;
    Ok(())
}

/// In-memory test pool (single connection). Suitable for most unit tests
/// that do not need concurrent access.
pub(crate) async fn mem_test_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("test in-memory database should connect");
    let backup_directory = migration_backup_dir();
    db::run_migrations(&pool, backup_directory.path())
        .await
        .expect("test migration should run");
    db::seed(&pool).await.expect("test data should seed");
    pool
}

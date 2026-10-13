//! Readable, self-describing SQL Backups.
//!
//! The dumper deliberately uses the SQLite schema catalog instead of a list of
//! Rust queries. This keeps a full Backup complete when a later migration adds
//! a table. The table-to-Backup Set map remains explicit so an unmapped table is
//! caught by the guard test rather than silently exported under the wrong name.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    path::{Path, PathBuf},
};

use chrono::Utc;
use libsqlite3_sys::{
    sqlite3_backup_finish, sqlite3_backup_init, sqlite3_backup_step, SQLITE_DONE, SQLITE_OK,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePoolOptions},
    Connection, Row, Sqlite, SqlitePool, Transaction,
};
use tokio::fs as async_fs;
use tracing::warn;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

const HEADER_MARKER: &str = "-- body-begins\n";
pub const PRE_MIGRATION_RETENTION_COUNT: usize = 10;
pub const PRE_RESTORE_RETENTION_COUNT: usize = 10;

/// Tables whose rows are never exported to any Backup. Full Backups retain
/// their empty table and index definitions so the replayed schema matches the
/// retained migration history. Telegram tokens are DPAPI-protected under one
/// Windows identity, so a restored or moved Admin starts disconnected until
/// the owner reconnects. Telemetry state is excluded (ADR-0083), so a restored
/// Admin starts opted out with no shared Installation ID. Every entry must name
/// a real table (guard-tested), so renamed tables cannot silently leak data.
pub const BACKUP_EXCLUDED_TABLES: &[&str] = &[
    "telegram_config",
    "telegram_outbox",
    "telemetry_settings",
    "telemetry_outbox",
];

/// The reason a Backup was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupKind {
    Manual,
    Scheduled,
    PreMigration,
    PreRestore,
}

impl BackupKind {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "scheduled" => Some(Self::Scheduled),
            "pre_migration" => Some(Self::PreMigration),
            "pre_restore" => Some(Self::PreRestore),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::PreMigration => "pre_migration",
            Self::PreRestore => "pre_restore",
        }
    }
}

/// The seven explicit export boundaries in a Backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackupSet {
    Accounts,
    Configuration,
    Ledger,
    Vouchers,
    Terminals,
    RuntimeState,
    Credentials,
}

impl BackupSet {
    pub const fn all() -> [Self; 7] {
        [
            Self::Accounts,
            Self::Configuration,
            Self::Ledger,
            Self::Vouchers,
            Self::Terminals,
            Self::RuntimeState,
            Self::Credentials,
        ]
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accounts => "Accounts",
            Self::Configuration => "Configuration",
            Self::Ledger => "Ledger",
            Self::Vouchers => "Vouchers",
            Self::Terminals => "Terminals",
            Self::RuntimeState => "Runtime State",
            Self::Credentials => "Credentials",
        }
    }

    pub fn parse_name(value: &str) -> Option<Self> {
        Self::all().into_iter().find(|set| set.as_str() == value)
    }
}

/// The parsed metadata at the beginning of a Backup file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupHeader {
    pub created_at: String,
    pub kind: String,
    pub schema_version: i64,
    pub app_version: String,
    pub sets: BTreeSet<BackupSet>,
    pub row_counts: BTreeMap<String, i64>,
    pub sha256: String,
    pub warning: String,
    pub catch_up: bool,
    body_offset: usize,
}

impl BackupHeader {
    /// Verify the SHA-256 recorded in this header against the complete body.
    pub fn verify_checksum(&self, sql: &str) -> bool {
        sql.get(self.body_offset..)
            .is_some_and(|body| sha256_hex(body.as_bytes()) == self.sha256)
    }
}

/// The result of writing a Backup.
#[derive(Debug, Clone)]
pub struct BackupArtifact {
    pub path: PathBuf,
    pub header: BackupHeader,
    pub(crate) mirror_error: Option<String>,
}

/// Filesystem-derived metadata shown in the owner Backup Catalog.
#[derive(Debug, Clone, Serialize)]
pub struct BackupCatalogEntry {
    pub filename: String,
    pub kind: String,
    pub created_at: String,
    pub schema_version: i64,
    pub size_bytes: u64,
    pub backup_sets: Vec<String>,
    pub catch_up: bool,
}

/// Where a restore candidate came from. Catalog names are resolved inside the
/// primary Backup directory; paths are accepted only when absolute.
#[derive(Debug, Clone)]
pub enum RestoreSource {
    Catalog(String),
    AbsolutePath(PathBuf),
    /// A browser upload staged in a temporary file. The original filename is
    /// retained for the Pending Restore descriptor, while the path is never
    /// exposed to the owner API.
    Uploaded {
        path: PathBuf,
        filename: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingRestore {
    pub filename: String,
    pub schema_version: i64,
    pub requested_at: String,
    pub mirror_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingRestoreMarker {
    filename: String,
    schema_version: i64,
    requested_at: String,
    staged_path: PathBuf,
    #[serde(default)]
    outgoing_path: Option<PathBuf>,
    #[serde(default)]
    live_moved: bool,
    #[serde(default)]
    mirror_error: Option<String>,
}

const PENDING_RESTORE_FILENAME: &str = ".pending-restore.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaObjectKind {
    Table,
    Index,
    View,
    Trigger,
}

struct SchemaObject {
    kind: SchemaObjectKind,
    name: String,
    table_name: String,
    sql: String,
}

/// Write a readable SQL Backup to `directory` from one consistent database
/// read transaction. The migration history table is always included.
pub async fn write_backup(
    pool: &SqlitePool,
    directory: &Path,
    kind: BackupKind,
    selected_sets: &[BackupSet],
) -> AppResult<BackupArtifact> {
    let scheduled_retention = if kind == BackupKind::Scheduled {
        Some(configured_scheduled_retention(pool).await?)
    } else {
        None
    };
    let selected_sets = selected_sets.iter().copied().collect::<BTreeSet<_>>();
    if kind == BackupKind::PreMigration && selected_sets.len() != BackupSet::all().len() {
        return Err(AppError::InvalidInput(
            "Pre-Migration Backups must include every Backup Set".to_owned(),
        ));
    }
    let (sql, header) = build_backup(pool, kind, &selected_sets, false).await?;
    let filename = format!(
        "openpiso-{}-{}-v{}.sql",
        kind.as_str(),
        Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
        header.schema_version
    );
    write_sql_file(directory, &filename, &sql).await?;
    let path = directory.join(filename);
    let mirror_error = mirror_backup_if_configured(pool, &path).await;
    prune_after_write(directory, kind, scheduled_retention).await;

    Ok(BackupArtifact {
        path,
        header,
        mirror_error,
    })
}

/// Write a scheduled Backup, recording whether it was produced after a missed
/// due instant (for example, after the Admin was powered off).
pub async fn write_scheduled_backup(
    pool: &SqlitePool,
    directory: &Path,
    selected_sets: &[BackupSet],
    catch_up: bool,
) -> AppResult<BackupArtifact> {
    let retention = configured_scheduled_retention(pool).await?;
    let selected_sets = selected_sets.iter().copied().collect::<BTreeSet<_>>();
    let (sql, header) = build_backup(pool, BackupKind::Scheduled, &selected_sets, catch_up).await?;
    let filename = format!(
        "openpiso-{}-{}-v{}.sql",
        BackupKind::Scheduled.as_str(),
        Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
        header.schema_version
    );
    write_sql_file(directory, &filename, &sql).await?;
    let path = directory.join(filename);
    let mirror_error = mirror_backup_if_configured(pool, &path).await;
    prune_after_write(directory, BackupKind::Scheduled, Some(retention)).await;

    Ok(BackupArtifact {
        path,
        header,
        mirror_error,
    })
}

/// Write the mandatory all-Sets Backup made immediately before migrations.
/// The schema version in the header is the pre-migration version; the filename
/// also identifies the target version that will be applied. Success means both
/// primary artifacts exist: the SQL Backup and its database companion.
#[doc(hidden)]
pub async fn write_pre_migration_backup(
    pool: &SqlitePool,
    directory: &Path,
    from_version: i64,
    to_version: i64,
) -> AppResult<BackupArtifact> {
    let (sql, header) = build_backup(
        pool,
        BackupKind::PreMigration,
        &BackupSet::all().into_iter().collect(),
        false,
    )
    .await?;
    let filename = format!(
        "openpiso-pre_migration-{}-{}-v{}-to-v{}.sql",
        Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
        Uuid::new_v4(),
        from_version,
        to_version
    );
    write_sql_file(directory, &filename, &sql).await?;
    let path = directory.join(filename);
    let mirror_error = mirror_backup_if_configured(pool, &path).await;
    prune_after_write(directory, BackupKind::PreMigration, None).await;
    // The Pre-Migration Backup is the SQL file plus its database companion;
    // without the companion the SQL file is removed and migration fails closed.
    if let Err(error) = write_database_companion(pool, &path).await {
        let _ = async_fs::remove_file(&path).await;
        return Err(error);
    }

    Ok(BackupArtifact {
        path,
        header,
        mirror_error,
    })
}

async fn write_database_companion(pool: &SqlitePool, sql_path: &Path) -> AppResult<()> {
    let companion_path = sql_path.with_extension("db");
    let temporary_path = companion_path.with_file_name(format!(
        ".{}.{}.tmp",
        companion_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("database.db"),
        Uuid::new_v4()
    ));
    let mut database_path = None;
    for row in sqlx::query("PRAGMA database_list").fetch_all(pool).await? {
        if row.try_get::<String, _>("name")? == "main" {
            let path = row.try_get::<String, _>("file")?;
            if !path.is_empty() {
                database_path = Some(PathBuf::from(path));
            }
            break;
        }
    }

    if let Some(database_path) = database_path {
        let checkpoint = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(pool)
            .await?;
        let busy = checkpoint.try_get::<i64, _>("busy")?;
        let log = checkpoint.try_get::<i64, _>("log")?;
        let checkpointed = checkpoint.try_get::<i64, _>("checkpointed")?;
        if busy != 0 || log != checkpointed {
            return Err(AppError::Conflict(
                "SQLite WAL checkpoint was incomplete while creating the pre-migration database companion"
                    .to_owned(),
            ));
        }
        std::fs::copy(&database_path, &temporary_path)?;
    } else {
        copy_in_memory_database(pool, &temporary_path).await?;
    }
    std::fs::rename(&temporary_path, companion_path)?;
    Ok(())
}

async fn copy_in_memory_database(pool: &SqlitePool, destination_path: &Path) -> AppResult<()> {
    let options = SqliteConnectOptions::new()
        .filename(destination_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let mut destination = SqliteConnection::connect_with(&options).await?;
    let mut source = pool.acquire().await?;
    let main = CString::new("main")
        .map_err(|_| AppError::InvalidInput("invalid SQLite database name".to_owned()))?;
    let (step, finish) = {
        let mut destination_handle = destination.lock_handle().await?;
        let mut source_handle = source.lock_handle().await?;
        // SAFETY: both SQLx connections are exclusively locked for the duration
        // of these synchronous calls; the backup handle is finalized exactly
        // once before either connection guard is released.
        let backup = unsafe {
            sqlite3_backup_init(
                destination_handle.as_raw_handle().as_ptr(),
                main.as_ptr(),
                source_handle.as_raw_handle().as_ptr(),
                main.as_ptr(),
            )
        };
        if backup.is_null() {
            return Err(AppError::Database(sqlx::Error::Protocol(
                "SQLite backup initialization failed".to_owned(),
            )));
        }
        // SAFETY: `backup` is non-null and remains valid until finish; SQLite
        // owns the handle and requires exactly one finish call.
        let step = unsafe { sqlite3_backup_step(backup, -1) };
        let finish = unsafe { sqlite3_backup_finish(backup) };
        (step, finish)
    };
    destination.close().await?;
    if step != SQLITE_DONE || finish != SQLITE_OK {
        return Err(AppError::Database(sqlx::Error::Protocol(format!(
            "SQLite backup failed (step {step}, finish {finish})"
        ))));
    }
    Ok(())
}

async fn write_sql_file(directory: &Path, filename: &str, sql: &str) -> AppResult<()> {
    async_fs::create_dir_all(directory).await?;
    let path = directory.join(filename);
    let temporary = directory.join(format!(".{filename}.{}.tmp", Uuid::new_v4()));
    async_fs::write(&temporary, sql.as_bytes()).await?;
    async_fs::rename(&temporary, path).await?;
    Ok(())
}

/// Scan the primary Backup directory and parse each SQL file's self-describing
/// header. The filesystem, not SQLite, is the Catalog source of truth.
pub async fn list_backups(directory: &Path) -> AppResult<Vec<BackupCatalogEntry>> {
    async_fs::create_dir_all(directory).await?;
    let mut entries = async_fs::read_dir(directory).await?;
    let mut backups = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let path = entry.path();
        if !file_type.is_file() || path.extension().and_then(|value| value.to_str()) != Some("sql")
        {
            continue;
        }
        let filename = entry.file_name().to_string_lossy().into_owned();
        let sql = async_fs::read_to_string(&path).await?;
        let header = parse_backup_header(&sql)?;
        let size_bytes = entry.metadata().await?.len();
        backups.push(BackupCatalogEntry {
            filename,
            kind: header.kind,
            created_at: header.created_at,
            schema_version: header.schema_version,
            size_bytes,
            backup_sets: header
                .sets
                .iter()
                .map(|set| set.as_str().to_owned())
                .collect(),
            catch_up: header.catch_up,
        });
    }
    backups.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.filename.cmp(&left.filename))
    });
    Ok(backups)
}

/// Return the durable marker path. The marker lives beside the Backup Catalog
/// and survives replacement of the SQLite database.
pub fn pending_restore_path(directory: &Path) -> PathBuf {
    directory.join(PENDING_RESTORE_FILENAME)
}

/// Read the current Pending Restore. A missing or malformed marker is treated
/// as absent so a damaged marker cannot prevent the Admin from starting.
pub async fn pending_restore(directory: &Path) -> AppResult<Option<PendingRestore>> {
    let Some(marker) = read_pending_restore_marker(directory).await? else {
        return Ok(None);
    };
    Ok(Some(PendingRestore {
        filename: marker.filename,
        schema_version: marker.schema_version,
        requested_at: marker.requested_at,
        mirror_error: marker.mirror_error,
    }))
}

async fn read_pending_restore_marker(directory: &Path) -> AppResult<Option<PendingRestoreMarker>> {
    let path = pending_restore_path(directory);
    let bytes = match async_fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let marker = match serde_json::from_slice::<PendingRestoreMarker>(&bytes) {
        Ok(marker) => marker,
        Err(error) => {
            warn!(
                target: "openpiso_admin_lib::backup",
                path = %path.display(),
                error = %error,
                "ignoring malformed Pending Restore marker"
            );
            return Ok(None);
        }
    };
    if !marker.staged_path.is_absolute() {
        warn!(
            target: "openpiso_admin_lib::backup",
            path = %path.display(),
            staged_path = %marker.staged_path.display(),
            "ignoring Pending Restore marker with a non-absolute staged path"
        );
        return Ok(None);
    }
    let staged_exists = marker.staged_path.try_exists()?;
    let outgoing_exists = if marker.live_moved {
        match marker.outgoing_path.as_deref() {
            Some(path) => path.try_exists()?,
            None => false,
        }
    } else {
        false
    };
    if !(staged_exists || outgoing_exists) {
        warn!(
            target: "openpiso_admin_lib::backup",
            path = %path.display(),
            staged_path = %marker.staged_path.display(),
            "ignoring Pending Restore marker with a missing staged database"
        );
        return Ok(None);
    }
    Ok(Some(marker))
}

/// Cancel a Pending Restore and remove its staged database.
pub async fn cancel_pending_restore(directory: &Path) -> AppResult<bool> {
    let marker_path = pending_restore_path(directory);
    let bytes = match async_fs::read(&marker_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let marker = serde_json::from_slice::<PendingRestoreMarker>(&bytes).ok();
    if let Some(marker) = marker {
        remove_database_files(&marker.staged_path).await?;
    }
    match async_fs::remove_file(marker_path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn write_pending_marker(directory: &Path, marker: &PendingRestoreMarker) -> AppResult<()> {
    async_fs::create_dir_all(directory).await?;
    let marker_path = pending_restore_path(directory);
    let temporary_marker = directory.join(format!(
        ".{PENDING_RESTORE_FILENAME}.{}.tmp",
        Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(marker).map_err(|error| {
        AppError::InvalidInput(format!("could not encode Pending Restore: {error}"))
    })?;
    async_fs::write(&temporary_marker, bytes).await?;
    async_fs::OpenOptions::new()
        .write(true)
        .open(&temporary_marker)
        .await?
        .sync_all()
        .await?;
    if let Err(error) = async_fs::rename(&temporary_marker, &marker_path).await {
        let _ = async_fs::remove_file(&temporary_marker).await;
        return Err(error.into());
    }
    Ok(())
}

/// Stage a whole-database restore while leaving the live pool untouched.
/// Acceptance and precondition gates deliberately run before any replay.
pub async fn stage_restore(
    pool: &SqlitePool,
    directory: &Path,
    source: RestoreSource,
    active_pc_ids: &[String],
) -> AppResult<PendingRestore> {
    stage_restore_owned(
        pool.clone(),
        directory.to_owned(),
        source,
        active_pc_ids.to_owned(),
    )
    .await
}

pub(crate) async fn stage_restore_owned(
    pool: SqlitePool,
    directory: PathBuf,
    source: RestoreSource,
    active_pc_ids: Vec<String>,
) -> AppResult<PendingRestore> {
    if pending_restore(&directory).await?.is_some() {
        return Err(AppError::Conflict(
            "a Pending Restore already exists; cancel it before staging another".to_owned(),
        ));
    }

    let (source_path, filename) = match source {
        RestoreSource::Catalog(filename) => {
            let valid = filename.starts_with("openpiso-")
                && filename.ends_with(".sql")
                && !filename.contains('/')
                && !filename.contains('\\')
                && filename.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
                });
            if !valid {
                return Err(AppError::RestoreRejected {
                    gate: "header".to_owned(),
                    message: "Backup identifier is not a valid Catalog filename".to_owned(),
                    blocking_pcs: Vec::new(),
                });
            }
            (directory.join(&filename), filename)
        }
        RestoreSource::AbsolutePath(path) => {
            let filename = path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| AppError::RestoreRejected {
                    gate: "header".to_owned(),
                    message: "Backup path has no valid filename".to_owned(),
                    blocking_pcs: Vec::new(),
                })?
                .to_owned();
            if !path.is_absolute()
                || path.extension().and_then(|value| value.to_str()) != Some("sql")
            {
                return Err(AppError::RestoreRejected {
                    gate: "header".to_owned(),
                    message: "restore path must be an absolute .sql file path".to_owned(),
                    blocking_pcs: Vec::new(),
                });
            }
            (path, filename)
        }
        RestoreSource::Uploaded { path, filename } => {
            if Path::new(&filename)
                .extension()
                .and_then(|value| value.to_str())
                != Some("sql")
            {
                return Err(AppError::RestoreRejected {
                    gate: "header".to_owned(),
                    message: "uploaded restore must be a `.sql` file".to_owned(),
                    blocking_pcs: Vec::new(),
                });
            }
            (path, filename)
        }
    };

    let sql = async_fs::read_to_string(&source_path)
        .await
        .map_err(|error| AppError::RestoreRejected {
            gate: "header".to_owned(),
            message: if error.kind() == std::io::ErrorKind::NotFound {
                "Backup file not found".to_owned()
            } else {
                format!("could not read Backup file: {error}")
            },
            blocking_pcs: Vec::new(),
        })?;
    let header = parse_backup_header(&sql).map_err(|error| AppError::RestoreRejected {
        gate: "header".to_owned(),
        message: error.to_string(),
        blocking_pcs: Vec::new(),
    })?;
    if !header.verify_checksum(&sql) {
        return Err(AppError::RestoreRejected {
            gate: "checksum".to_owned(),
            message: "Backup checksum does not match its body".to_owned(),
            blocking_pcs: Vec::new(),
        });
    }
    if BackupSet::all()
        .into_iter()
        .any(|set| !header.sets.contains(&set))
    {
        return Err(AppError::RestoreRejected {
            gate: "backup_sets".to_owned(),
            message: "Backup does not cover all seven Backup Sets".to_owned(),
            blocking_pcs: Vec::new(),
        });
    }
    if header.schema_version > crate::db::CURRENT_SCHEMA_VERSION {
        return Err(AppError::RestoreRejected {
            gate: "schema_version".to_owned(),
            message: format!(
                "Backup schema version {} is newer than the running app schema version {}",
                header.schema_version,
                crate::db::CURRENT_SCHEMA_VERSION
            ),
            blocking_pcs: Vec::new(),
        });
    }

    let blocking_pcs = restore_blocking_pcs(pool.clone(), active_pc_ids.clone()).await?;
    if !blocking_pcs.is_empty() {
        return Err(AppError::RestoreRejected {
            gate: "precondition".to_owned(),
            message: format!(
                "Restore is blocked while PCs have active use, Remaining Time, or an active account binding; blocking PCs: {}",
                blocking_pcs.join(", ")
            ),
            blocking_pcs,
        });
    }

    async_fs::create_dir_all(&directory).await?;
    let staged_path = directory
        .parent()
        .unwrap_or(directory.as_path())
        .join(format!(".openpiso-staged-{}.db", Uuid::new_v4()));
    let staging_result = stage_database(sql, staged_path.clone(), header.clone()).await;
    if let Err(error) = staging_result {
        remove_database_files(&staged_path).await?;
        return Err(error);
    }

    let marker = PendingRestoreMarker {
        filename,
        schema_version: header.schema_version,
        requested_at: Utc::now().to_rfc3339(),
        staged_path,
        outgoing_path: None,
        live_moved: false,
        mirror_error: None,
    };
    if let Err(error) = write_pending_marker(&directory, &marker).await {
        remove_database_files(&marker.staged_path).await?;
        return Err(error);
    }

    Ok(PendingRestore {
        filename: marker.filename,
        schema_version: marker.schema_version,
        requested_at: marker.requested_at,
        mirror_error: marker.mirror_error,
    })
}

pub(crate) async fn restore_blocking_pcs(
    pool: SqlitePool,
    active_pc_ids: Vec<String>,
) -> AppResult<Vec<String>> {
    let active_pc_ids = active_pc_ids.into_iter().collect::<BTreeSet<_>>();
    let rows = sqlx::query("SELECT id, name, status, remaining_seconds FROM pcs ORDER BY name")
        .fetch_all(&pool)
        .await?;
    let mut blocking = Vec::new();
    for row in rows {
        let id = row.try_get::<String, _>("id")?;
        let name = row.try_get::<String, _>("name")?;
        let status = row.try_get::<String, _>("status")?;
        let remaining = row.try_get::<i64, _>("remaining_seconds")?;
        if status == "in_use" || remaining > 0 || active_pc_ids.contains(&id) {
            blocking.push(name);
        }
    }
    Ok(blocking)
}

async fn stage_database(sql: String, staged_path: PathBuf, header: BackupHeader) -> AppResult<()> {
    let options = SqliteConnectOptions::new()
        .filename(&staged_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let staging_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let result = async {
        if let Err(error) = sqlx::raw_sql(&sql).execute(&staging_pool).await {
            let message = error.to_string();
            return Err(AppError::RestoreRejected {
                gate: if message.to_ascii_lowercase().contains("foreign key") {
                    "foreign_keys".to_owned()
                } else {
                    "replay".to_owned()
                },
                message: format!("Backup replay failed: {message}"),
                blocking_pcs: Vec::new(),
            });
        }
        // Row counts are verified here, on the freshly replayed database and
        // before any migration runs. At this point the staged database must be
        // an exact reproduction of the file, so equality is meaningful for every
        // table and no per-table tolerance is needed. Verifying after
        // forward-migration cannot work: migrations both insert rows (0018 and
        // 0024 add settings keys) and rebuild tables (0025 and 0027 rewrite the
        // wheel tables), so every older Backup would be refused as corrupt.
        for (table, expected) in &header.row_counts {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&staging_pool)
            .await?;
            if exists == 0 {
                return Err(AppError::RestoreRejected {
                    gate: "row_counts".to_owned(),
                    message: format!("staged Backup is missing table {table}"),
                    blocking_pcs: Vec::new(),
                });
            }
            let actual = sqlx::query_scalar::<_, i64>(&format!(
                "SELECT COUNT(*) FROM {}",
                quote_identifier(table)
            ))
            .fetch_one(&staging_pool)
            .await?;
            if actual != *expected {
                return Err(AppError::RestoreRejected {
                    gate: "row_counts".to_owned(),
                    message: format!(
                        "staged Backup row count mismatch for {table}: expected {expected}, found {actual}"
                    ),
                    blocking_pcs: Vec::new(),
                });
            }
        }

        // Forward-migrate the verified replay to the running schema, then check
        // integrity of the result the owner would actually get.
        let migration_dir = tempfile::tempdir()?;
        crate::db::run_migrations(&staging_pool, migration_dir.path()).await?;
        crate::db::seed(&staging_pool).await?;

        let foreign_key_rows = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&staging_pool)
            .await?;
        if !foreign_key_rows.is_empty() {
            return Err(AppError::RestoreRejected {
                gate: "foreign_keys".to_owned(),
                message: "staged Backup failed foreign-key integrity verification".to_owned(),
                blocking_pcs: Vec::new(),
            });
        }

        let checkpoint = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&staging_pool)
            .await?;
        let busy = checkpoint.try_get::<i64, _>("busy")?;
        let log = checkpoint.try_get::<i64, _>("log")?;
        let checkpointed = checkpoint.try_get::<i64, _>("checkpointed")?;
        if busy != 0 || log != checkpointed {
            return Err(AppError::Conflict(
                "SQLite WAL checkpoint was incomplete for staged Restore".to_owned(),
            ));
        }
        Ok::<(), AppError>(())
    }
    .await;
    staging_pool.close().await;
    result
}

/// Apply a verified Pending Restore before opening the normal live pool.
pub async fn apply_pending_restore(
    database_path: &Path,
    directory: &Path,
) -> AppResult<Option<PendingRestore>> {
    let marker_path = pending_restore_path(directory);
    let Some(mut marker) = read_pending_restore_marker(directory).await? else {
        return Ok(None);
    };

    let mut pending = PendingRestore {
        filename: marker.filename.clone(),
        schema_version: marker.schema_version,
        requested_at: marker.requested_at.clone(),
        mirror_error: marker.mirror_error.clone(),
    };

    // Recovery cases are resolved before opening SQLite. The marker is written
    // before the first rename, so a crash cannot strand the live database
    // without a durable description of how to finish the swap.
    if marker.live_moved {
        if !database_path.exists() {
            let outgoing_path = marker.outgoing_path.as_ref().ok_or_else(|| {
                AppError::Conflict("Pending Restore has no outgoing database path".to_owned())
            })?;
            if !outgoing_path.exists() || !marker.staged_path.exists() {
                return Err(AppError::Conflict(
                    "Pending Restore swap is incomplete and cannot be recovered safely".to_owned(),
                ));
            }
            remove_database_sidecars(&marker.staged_path).await?;
            async_fs::rename(&marker.staged_path, database_path).await?;
        } else if marker
            .outgoing_path
            .as_ref()
            .is_some_and(|path| path.exists())
            && !marker.staged_path.exists()
        {
            // The staged file was moved successfully and only marker cleanup
            // was interrupted.
            let Some(outgoing_path) = marker.outgoing_path.as_ref() else {
                return Err(AppError::Conflict(
                    "Pending Restore has no outgoing database path".to_owned(),
                ));
            };
            remove_database_files(outgoing_path).await?;
            async_fs::remove_file(&marker_path).await?;
            return Ok(Some(pending));
        }
    }

    if !marker.live_moved {
        let options = SqliteConnectOptions::new()
            .filename(database_path)
            .create_if_missing(false)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let live_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let blocking_pcs = restore_blocking_pcs(live_pool.clone(), Vec::new()).await?;
        if !blocking_pcs.is_empty() {
            let error = AppError::RestoreRejected {
                gate: "precondition".to_owned(),
                message: format!(
                    "Restore is blocked while PCs have active use, Remaining Time, or an active account binding; blocking PCs: {}",
                    blocking_pcs.join(", ")
                ),
                blocking_pcs,
            };
            live_pool.close().await;
            return Err(error);
        }
        let artifact = write_backup(
            &live_pool,
            directory,
            BackupKind::PreRestore,
            &BackupSet::all(),
        )
        .await?;
        marker.mirror_error = artifact.mirror_error.clone();
        pending.mirror_error = marker.mirror_error.clone();
        write_pending_marker(directory, &marker).await?;
        let checkpoint = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&live_pool)
            .await?;
        let busy = checkpoint.try_get::<i64, _>("busy")?;
        let log = checkpoint.try_get::<i64, _>("log")?;
        let checkpointed = checkpoint.try_get::<i64, _>("checkpointed")?;
        live_pool.close().await;
        if busy != 0 || log != checkpointed {
            return Err(AppError::Conflict(
                "SQLite WAL checkpoint was incomplete before applying Pending Restore".to_owned(),
            ));
        }

        let outgoing_path = marker.outgoing_path.clone().unwrap_or_else(|| {
            database_path
                .with_file_name(format!(".openpiso-outgoing-restore-{}.db", Uuid::new_v4()))
        });
        marker.outgoing_path = Some(outgoing_path);
        marker.live_moved = true;
        write_pending_marker(directory, &marker).await?;
    }

    let outgoing_path = marker.outgoing_path.as_ref().ok_or_else(|| {
        AppError::Conflict("Pending Restore has no outgoing database path".to_owned())
    })?;
    if database_path.exists() && !outgoing_path.exists() {
        remove_database_sidecars(database_path).await?;
        async_fs::rename(database_path, outgoing_path).await?;
    }
    if !database_path.exists() {
        remove_database_sidecars(&marker.staged_path).await?;
        if let Err(error) = async_fs::rename(&marker.staged_path, database_path).await {
            let _ = async_fs::rename(outgoing_path, database_path).await;
            return Err(error.into());
        }
    }
    if let Err(error) = async_fs::remove_file(&marker_path).await {
        return Err(error.into());
    }
    remove_database_files(outgoing_path).await?;
    Ok(Some(pending))
}

async fn remove_database_sidecars(path: &Path) -> AppResult<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
        match async_fs::remove_file(sidecar).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn remove_database_files(path: &Path) -> AppResult<()> {
    match async_fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    remove_database_sidecars(path).await
}

/// Keep only the newest Backups of one kind. Retention domains never cross
/// Backup Kind boundaries, and non-SQL artifacts such as pre-migration `.db`
/// companions are never part of the Catalog scan.
pub async fn prune_backups(directory: &Path, kind: BackupKind, retain: usize) -> AppResult<()> {
    if kind == BackupKind::Manual {
        return Ok(());
    }
    let backups = list_backups(directory)
        .await?
        .into_iter()
        .filter(|entry| entry.kind == kind.as_str())
        .collect::<Vec<_>>();
    for entry in backups.into_iter().skip(retain) {
        let backup_path = directory.join(&entry.filename);
        match async_fs::remove_file(&backup_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if kind == BackupKind::PreMigration {
            let companion_path = backup_path.with_extension("db");
            match async_fs::remove_file(companion_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

/// Compatibility wrapper for callers that only need scheduled retention.
pub async fn prune_scheduled_backups(directory: &Path, retain: usize) -> AppResult<()> {
    prune_backups(directory, BackupKind::Scheduled, retain).await
}

async fn configured_scheduled_retention(pool: &SqlitePool) -> AppResult<usize> {
    usize::try_from(
        crate::db::backup_schedule_settings(pool)
            .await?
            .retention_count,
    )
    .map_err(|_| AppError::InvalidInput("Backup retention count is invalid".to_owned()))
}

async fn prune_after_write(directory: &Path, kind: BackupKind, scheduled_retention: Option<usize>) {
    let retention = match kind {
        BackupKind::Manual => return,
        BackupKind::Scheduled => scheduled_retention,
        BackupKind::PreMigration => Some(PRE_MIGRATION_RETENTION_COUNT),
        BackupKind::PreRestore => Some(PRE_RESTORE_RETENTION_COUNT),
    };
    let Some(retention) = retention else {
        return;
    };
    if let Err(error) = prune_backups(directory, kind, retention).await {
        warn!(
            target: "openpiso_admin_lib::backup",
            kind = kind.as_str(),
            "Backup retention pruning failed: {error}"
        );
    }
}

/// Copy a primary Backup to the optional Mirror Directory. The caller decides
/// whether a mirror failure is advisory or fatal.
pub async fn mirror_backup(path: &Path, mirror_directory: &Path) -> AppResult<()> {
    async_fs::create_dir_all(mirror_directory).await?;
    let filename = path
        .file_name()
        .ok_or_else(|| AppError::InvalidInput("Backup path has no filename".to_owned()))?;
    let destination = mirror_directory.join(filename);
    let temporary = mirror_directory.join(format!(
        ".{}.{}.tmp",
        filename.to_string_lossy(),
        Uuid::new_v4()
    ));
    async_fs::copy(path, &temporary).await?;
    async_fs::rename(&temporary, destination).await?;
    Ok(())
}

/// Mirror a successfully written Backup without changing the primary result.
/// Mirror state is persistent so the owner can see failures between Backups.
pub(crate) async fn mirror_backup_if_configured(pool: &SqlitePool, path: &Path) -> Option<String> {
    let mirror_directory = match crate::db::backup_schedule::configured_mirror_directory(pool).await
    {
        Ok(Some(directory)) => directory,
        Ok(None) => return None,
        Err(error) => {
            warn!(
                target: "openpiso_admin_lib::backup",
                "could not read Backup mirror configuration: {error}"
            );
            return None;
        }
    };

    let mirror_result = mirror_backup(path, Path::new(&mirror_directory)).await;
    match mirror_result {
        Ok(()) => {
            if let Err(error) = crate::db::backup_schedule::set_mirror_error(pool, None).await {
                warn!(
                    target: "openpiso_admin_lib::backup",
                    "could not clear Backup mirror advisory: {error}"
                );
            }
            None
        }
        Err(error) => {
            let error_string = error.to_string();
            warn!(
                target: "openpiso_admin_lib::backup",
                directory = mirror_directory,
                "Backup mirror copy failed: {error_string}"
            );
            if let Err(state_error) =
                crate::db::backup_schedule::set_mirror_error(pool, Some(&error_string)).await
            {
                warn!(
                    target: "openpiso_admin_lib::backup",
                    "could not persist Backup mirror advisory: {state_error}"
                );
            }
            Some(error_string)
        }
    }
}

/// Parse the header of a Backup and locate its checksummed body.
pub fn parse_backup_header(sql: &str) -> AppResult<BackupHeader> {
    if !sql.starts_with("-- OpenPiso Backup\n") {
        return Err(AppError::InvalidInput(
            "Backup does not start with an OpenPiso header".to_owned(),
        ));
    }
    let Some(body_offset) = sql
        .find(HEADER_MARKER)
        .map(|offset| offset + HEADER_MARKER.len())
    else {
        return Err(AppError::InvalidInput(
            "Backup header has no body marker".to_owned(),
        ));
    };
    let header_text = &sql[..body_offset - HEADER_MARKER.len()];
    let mut values = BTreeMap::<&str, &str>::new();
    let mut row_counts = BTreeMap::new();
    let mut sets = BTreeSet::new();
    let mut saw_sets = false;
    let mut saw_warning = false;
    let mut catch_up = false;

    for line in header_text.lines().skip(1) {
        let Some(value) = line.strip_prefix("-- ") else {
            return Err(AppError::InvalidInput("malformed Backup header".to_owned()));
        };
        let Some((key, value)) = value.split_once(": ") else {
            return Err(AppError::InvalidInput(
                "malformed Backup header field".to_owned(),
            ));
        };
        if key == "row_count" {
            let Some((table, count)) = value.split_once('=') else {
                return Err(AppError::InvalidInput(
                    "malformed Backup row count".to_owned(),
                ));
            };
            let count = count.parse::<i64>().map_err(|_| {
                AppError::InvalidInput("malformed Backup row count value".to_owned())
            })?;
            row_counts.insert(table.to_owned(), count);
        } else if key == "backup_sets" {
            saw_sets = true;
            if !value.is_empty() {
                for name in value.split(", ") {
                    let Some(set) = BackupSet::parse_name(name) else {
                        return Err(AppError::InvalidInput(format!(
                            "unknown Backup Set: {name}"
                        )));
                    };
                    sets.insert(set);
                }
            }
        } else {
            if key == "warning" {
                saw_warning = true;
            } else if key == "catch_up" {
                catch_up = value.parse::<bool>().map_err(|_| {
                    AppError::InvalidInput("Backup catch-up flag is malformed".to_owned())
                })?;
            }
            values.insert(key, value);
        }
    }

    if !saw_sets || !saw_warning {
        return Err(AppError::InvalidInput(
            "Backup header is missing required metadata".to_owned(),
        ));
    }
    if !row_counts.contains_key("_openpiso_migrations") {
        return Err(AppError::InvalidInput(
            "Backup header is missing migration row count".to_owned(),
        ));
    }
    if row_counts.values().any(|count| *count < 0) {
        return Err(AppError::InvalidInput(
            "Backup row count cannot be negative".to_owned(),
        ));
    }

    let required = |key: &str| {
        values
            .get(key)
            .copied()
            .ok_or_else(|| AppError::InvalidInput(format!("Backup header is missing {key}")))
    };
    let schema_version = required("schema_version")?.parse::<i64>().map_err(|_| {
        AppError::InvalidInput("Backup schema version is not an integer".to_owned())
    })?;
    if schema_version < 0 {
        return Err(AppError::InvalidInput(
            "Backup schema version cannot be negative".to_owned(),
        ));
    }
    let kind = required("backup_kind")?;
    if BackupKind::from_str(kind).is_none() {
        return Err(AppError::InvalidInput(format!(
            "unknown Backup Kind: {kind}"
        )));
    }
    let sha256 = required("sha256")?.to_owned();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::InvalidInput(
            "Backup SHA-256 is malformed".to_owned(),
        ));
    }

    Ok(BackupHeader {
        created_at: required("created_at")?.to_owned(),
        kind: kind.to_owned(),
        schema_version,
        app_version: required("app_version")?.to_owned(),
        sets,
        row_counts,
        sha256,
        warning: required("warning")?.to_owned(),
        catch_up,
        body_offset,
    })
}

async fn build_backup(
    pool: &SqlitePool,
    kind: BackupKind,
    selected_sets: &BTreeSet<BackupSet>,
    catch_up: bool,
) -> AppResult<(String, BackupHeader)> {
    // Do not use an ordinary pool query here. Every catalog, schema and row
    // read must use this one deferred transaction to share one WAL snapshot.
    let mut transaction = pool.begin().await?;
    let schema_version =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(version), 0) FROM _openpiso_migrations")
            .fetch_one(&mut *transaction)
            .await?;
    let objects = schema_objects(&mut transaction, selected_sets).await?;
    let tables = objects
        .iter()
        .filter(|object| object.kind == SchemaObjectKind::Table)
        .collect::<Vec<_>>();

    let mut row_counts = BTreeMap::new();
    let mut inserts = String::new();
    let migration_table = tables
        .iter()
        .find(|table| table.name == "_openpiso_migrations");
    for table in tables.iter().filter(|table| {
        table.name != "_openpiso_migrations"
            && !BACKUP_EXCLUDED_TABLES.contains(&table.name.as_str())
    }) {
        let columns = table_columns(&mut transaction, &table.name).await?;
        let (table_inserts, count) = table_data(&mut transaction, &table.name, &columns).await?;
        row_counts.insert(table.name.clone(), count);
        inserts.push_str(&table_inserts);
    }
    // Migration history is deliberately after every application table's data,
    // matching the documented dump shape and keeping it easy to identify.
    if let Some(table) = migration_table {
        let columns = table_columns(&mut transaction, &table.name).await?;
        let (table_inserts, count) = table_data(&mut transaction, &table.name, &columns).await?;
        row_counts.insert(table.name.clone(), count);
        inserts.push_str(&table_inserts);
    }

    let mut body = String::from("PRAGMA foreign_keys = OFF;\nBEGIN TRANSACTION;\n");
    append_objects(&mut body, &objects, SchemaObjectKind::Table);
    append_objects(&mut body, &objects, SchemaObjectKind::Index);
    append_objects(&mut body, &objects, SchemaObjectKind::View);
    body.push_str(&inserts);
    append_objects(&mut body, &objects, SchemaObjectKind::Trigger);
    body.push_str("COMMIT;\nPRAGMA foreign_keys = ON;\n");

    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let app_version = env!("CARGO_PKG_VERSION").to_owned();
    let checksum = sha256_hex(body.as_bytes());
    let warning = "WARNING: this Backup contains secrets when Credentials or Terminals are included; do not share or commit it.";
    let sets_text = selected_sets
        .iter()
        .map(|set| set.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut header_text = format!(
        "-- OpenPiso Backup\n-- created_at: {created_at}\n-- backup_kind: {}\n-- schema_version: {schema_version}\n-- app_version: {app_version}\n-- backup_sets: {sets_text}\n",
        kind.as_str()
    );
    for (table, count) in &row_counts {
        header_text.push_str(&format!("-- row_count: {table}={count}\n"));
    }
    header_text.push_str(&format!(
        "-- sha256: {checksum}\n-- warning: {warning}\n-- catch_up: {catch_up}\n{HEADER_MARKER}"
    ));
    let sql = format!("{header_text}{body}");
    transaction.commit().await?;

    let header = parse_backup_header(&sql)?;
    Ok((sql, header))
}

async fn schema_objects(
    transaction: &mut Transaction<'_, Sqlite>,
    selected_sets: &BTreeSet<BackupSet>,
) -> AppResult<Vec<SchemaObject>> {
    let rows = sqlx::query(
        "SELECT type, name, tbl_name, sql FROM sqlite_master
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
           AND type IN ('table', 'index', 'view', 'trigger')",
    )
    .fetch_all(&mut **transaction)
    .await?;
    let full_backup = selected_sets.len() == BackupSet::all().len();
    let mut objects = Vec::new();
    for row in rows {
        let kind = match row.try_get::<String, _>("type")?.as_str() {
            "table" => SchemaObjectKind::Table,
            "index" => SchemaObjectKind::Index,
            "view" => SchemaObjectKind::View,
            "trigger" => SchemaObjectKind::Trigger,
            _ => continue,
        };
        let name = row.try_get::<String, _>("name")?;
        let table_name = row.try_get::<String, _>("tbl_name")?;
        let excluded = BACKUP_EXCLUDED_TABLES.contains(&table_name.as_str());
        if kind == SchemaObjectKind::Table
            && table_name != "_openpiso_migrations"
            && !excluded
            && table_backup_set(&table_name).is_none()
        {
            return Err(AppError::InvalidInput(format!(
                "database table is not assigned to a Backup Set: {table_name}"
            )));
        }
        let included = !excluded
            && (table_name == "_openpiso_migrations"
                || table_backup_set(&table_name).is_some_and(|set| selected_sets.contains(&set)));
        // A full, restorable Backup keeps empty schema for excluded integration
        // tables. Migration history then matches the replayed structure while
        // Telegram still restores disconnected with no token or queue rows.
        let schema_only = excluded && full_backup;
        let include = match kind {
            SchemaObjectKind::Table => included || schema_only,
            SchemaObjectKind::Index | SchemaObjectKind::Trigger => included || schema_only,
            // Views are useful schema, but partial exports cannot safely
            // include views that reference omitted tables.
            SchemaObjectKind::View => full_backup,
        };
        if include {
            objects.push(SchemaObject {
                kind,
                name,
                table_name,
                sql: row.try_get("sql")?,
            });
        }
    }
    objects.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.table_name.cmp(&right.table_name))
    });
    Ok(objects)
}

async fn table_columns(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &str,
) -> AppResult<Vec<String>> {
    let query = format!("PRAGMA table_info({})", quote_identifier(table));
    Ok(sqlx::query(&query)
        .fetch_all(&mut **transaction)
        .await?
        .into_iter()
        .map(|row| row.try_get::<String, _>("name"))
        .collect::<Result<Vec<_>, sqlx::Error>>()?)
}

async fn table_data(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &str,
    columns: &[String],
) -> AppResult<(String, i64)> {
    if columns.is_empty() {
        return Ok((String::new(), 0));
    }
    let quoted_columns = columns
        .iter()
        .map(|column| format!("quote({})", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!("SELECT {quoted_columns} FROM {}", quote_identifier(table));
    let rows = sqlx::query(&query).fetch_all(&mut **transaction).await?;
    let mut output = String::new();
    let quoted_table = quote_identifier(table);
    let column_list = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    for row in &rows {
        let values = (0..columns.len())
            .map(|index| row.try_get::<String, _>(index))
            .collect::<Result<Vec<_>, sqlx::Error>>()?
            .join(", ");
        output.push_str(&format!(
            "INSERT INTO {quoted_table} ({column_list}) VALUES ({values});\n"
        ));
    }
    Ok((output, rows.len() as i64))
}

fn append_objects(body: &mut String, objects: &[SchemaObject], kind: SchemaObjectKind) {
    for object in objects.iter().filter(|object| object.kind == kind) {
        body.push_str(object.sql.trim_end());
        body.push_str(";\n");
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The explicit table-to-Backup Set map. `_openpiso_migrations` is intentionally
/// absent: it is always included and belongs to no Set.
pub fn table_backup_set(table: &str) -> Option<BackupSet> {
    let set = match table {
        "accounts"
        | "account_transfers"
        | "points_transfer_requests"
        | "account_session_history"
        // Daily progress is the only Attendance projection that travels with
        // the Account (ADR-0084); Earning Months carry frozen claim windows.
        | "attendance_daily_progress"
        | "attendance_earning_months" => BackupSet::Accounts,
        "settings"
        | "rates"
        | "economy_settings"
        | "coin_device_settings"
        | "coin_device_pulse_map"
        | "theme_settings"
        | "wheel_settings"
        | "wheel_segments"
        | "redemption_tiers"
        | "redemption_tier_menu"
        | "coin_promo_settings"
        | "coin_promo_tiers"
        | "shop_hours_settings"
        | "shop_hours_schedule"
        | "backup_schedule_settings"
        // Diagnostics Mode is shop-wide configuration (ADR-0081): a restored
        // Admin keeps the owner's chosen surface visibility.
        | "diagnostics_mode_settings"
        | "attendance_settings_versions"
        | "shop_hours_versions"
        | "shop_hours_version_days"
        | "shop_override_history"
        | "attendance_calendar_dates"
        | "attendance_eligible_intervals" => BackupSet::Configuration,
        "audit_logs" | "audit_log" | "coin_transactions" | "manual_adjustments"
        | "rejection_credits" | "wheel_spins"
        // Attendance sources, claims and forfeitures are immutable ledger facts (ADR-0084).
        | "attendance_contributions"
        | "attendance_check_ins"
        | "attendance_claims"
        | "attendance_forfeitures" => BackupSet::Ledger,
        // PC Monitoring Incidents are permanent owner-reviewable evidence
        // (ADR-0076), so they travel with the Ledger.
        "pc_monitoring_incidents" => BackupSet::Ledger,
        // Monitoring Suppression is temporary monitoring configuration: it
        // must survive an upgrade/restore as configuration, but it is not
        // evidence and expires by policy (ADR-0076).
        "pc_monitoring_suppressions" => BackupSet::Configuration,
        // The Escalation Cooldown is a short-lived rate limit, not evidence:
        // the Alert and the incident closure carry the history (ADR-0077).
        "pc_escalation_cooldowns" => BackupSet::Configuration,
        "vouchers" | "voucher_batches" => BackupSet::Vouchers,
        "pcs" | "coin_devices" | "sessions" => BackupSet::Terminals,
        "shop_override"
        | "shop_hours_runtime_state"
        | "notification_read_state"
        | "notification_alert_ack"
        | "attendance_clock_state" => BackupSet::RuntimeState,
        "owner_credentials" => BackupSet::Credentials,
        _ => return None,
    };
    Some(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{mem_test_pool, reopen_wal_test_pool, wal_test_pool};
    use chrono::{Datelike, Duration, FixedOffset, NaiveTime, TimeZone, Utc};
    use sqlx::SqlitePool;

    async fn dump(pool: &SqlitePool, sets: &[BackupSet]) -> String {
        let dir = tempfile_dir();
        let artifact = write_backup(pool, &dir, BackupKind::Manual, sets)
            .await
            .expect("Backup should write");
        let sql = std::fs::read_to_string(&artifact.path).expect("Backup should be readable");
        let _ = std::fs::remove_dir_all(dir);
        sql
    }

    fn tempfile_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("openpiso-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp Backup directory");
        path
    }

    #[tokio::test]
    async fn configured_mirror_copies_every_backup_kind_and_records_failures() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let primary = tempfile_dir();
        let mirror = tempfile_dir();
        sqlx::query("UPDATE backup_schedule_settings SET mirror_directory = ?")
            .bind(mirror.to_string_lossy().as_ref())
            .execute(&pool)
            .await?;

        let manual = write_backup(&pool, &primary, BackupKind::Manual, &BackupSet::all()).await?;
        let scheduled = write_scheduled_backup(&pool, &primary, &BackupSet::all(), false).await?;
        let pre_restore =
            write_backup(&pool, &primary, BackupKind::PreRestore, &BackupSet::all()).await?;
        let pre_migration = write_pre_migration_backup(&pool, &primary, 34, 35).await?;

        for artifact in [manual, scheduled, pre_restore, pre_migration] {
            let filename = artifact.path.file_name().expect("Backup filename");
            assert_eq!(
                std::fs::read(&artifact.path)?,
                std::fs::read(mirror.join(filename))?
            );
        }
        let mirror_error: Option<String> =
            sqlx::query_scalar("SELECT mirror_error FROM backup_schedule_settings WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        assert_eq!(mirror_error, None);

        let failed_mirror = primary.join("not-a-directory");
        std::fs::write(&failed_mirror, b"not a directory")?;
        sqlx::query("UPDATE backup_schedule_settings SET mirror_directory = ?")
            .bind(failed_mirror.to_string_lossy().as_ref())
            .execute(&pool)
            .await?;
        let failed = write_backup(&pool, &primary, BackupKind::Manual, &BackupSet::all()).await?;
        assert!(failed.path.exists());
        assert!(list_backups(&primary)
            .await?
            .iter()
            .any(|entry| entry.filename == failed.path.file_name().unwrap().to_string_lossy()));
        let mirror_error: String = sqlx::query_scalar::<_, Option<String>>(
            "SELECT mirror_error FROM backup_schedule_settings WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?
        .expect("mirror failure advisory");
        assert!(mirror_error.contains("filesystem error"));

        sqlx::query("UPDATE backup_schedule_settings SET mirror_directory = ?")
            .bind(mirror.to_string_lossy().as_ref())
            .execute(&pool)
            .await?;
        write_backup(&pool, &primary, BackupKind::Manual, &BackupSet::all()).await?;
        let mirror_error: Option<String> =
            sqlx::query_scalar("SELECT mirror_error FROM backup_schedule_settings WHERE id = 1")
                .fetch_one(&pool)
                .await?;
        assert_eq!(mirror_error, None);

        let _ = std::fs::remove_dir_all(primary);
        let _ = std::fs::remove_dir_all(mirror);
        pool.close().await;
        Ok(())
    }

    fn write_catalog_fixture(directory: &Path, kind: BackupKind, index: usize) {
        let body = "SELECT 1;\n";
        let sql = format!(
            "-- OpenPiso Backup\n-- created_at: 2000-01-01T00:{index:02}:00Z\n-- backup_kind: {}\n-- schema_version: 34\n-- app_version: 0.6.9\n-- backup_sets: Accounts\n-- row_count: _openpiso_migrations=0\n-- sha256: {}\n-- warning: fixture\n-- catch_up: false\n-- body-begins\n{body}",
            kind.as_str(),
            sha256_hex(body.as_bytes())
        );
        std::fs::write(
            directory.join(format!("fixture-{}-{index}.sql", kind.as_str())),
            sql,
        )
        .expect("fixture Backup");
    }

    #[tokio::test]
    async fn full_dump_replays_with_triggers_created_last() {
        let (pool, path) = wal_test_pool("backup-round-trip").await;
        crate::db::coins::simulate_coin(&pool, "pc-01", 100)
            .await
            .expect("representative ledger row");
        sqlx::query("INSERT INTO accounts (id, username, username_normalized, password_hash) VALUES ('acct-backup', 'Backup User', 'backup user', 'hash')")
            .execute(&pool)
            .await
            .expect("representative account row");
        sqlx::query(
            "INSERT INTO owner_credentials (id, password_hash) VALUES (1, 'owner-secret-hash')",
        )
        .execute(&pool)
        .await
        .expect("representative credential row");
        sqlx::query("INSERT INTO audit_log (id, event_type, outcome, actor, summary) VALUES ('evt-voucher-backup', 'voucher_minted', 'success', 'admin', 'backup test voucher')")
            .execute(&pool)
            .await
            .expect("representative voucher audit row");
        sqlx::query("INSERT INTO vouchers (id, code, kind, status, seconds_value, points_value_centi, audit_event_id, code_normalized) VALUES ('voucher-backup', 'BACKUP-01', 'promo', 'active', 600, 0, 'evt-voucher-backup', 'BACKUP01')")
            .execute(&pool)
            .await
            .expect("representative voucher row");
        let sql = dump(&pool, &BackupSet::all()).await;
        assert!(sql.contains("simulator-only"));
        assert!(sql.contains("owner-secret-hash"));

        let replay = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("replay database");
        sqlx::raw_sql(&sql)
            .execute(&replay)
            .await
            .expect("stock SQLite-compatible replay should succeed");
        let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
            .fetch_one(&replay)
            .await
            .expect("replayed accounts");
        let transactions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coin_transactions")
            .fetch_one(&replay)
            .await
            .expect("replayed transactions");
        let secret: String =
            sqlx::query_scalar("SELECT password_hash FROM owner_credentials WHERE id = 1")
                .fetch_one(&replay)
                .await
                .expect("replayed credential");
        let voucher_code: String =
            sqlx::query_scalar("SELECT code FROM vouchers WHERE id = 'voucher-backup'")
                .fetch_one(&replay)
                .await
                .expect("replayed voucher");
        assert_eq!(accounts, 1);
        assert_eq!(transactions, 1);
        assert_eq!(secret, "owner-secret-hash");
        assert_eq!(voucher_code, "BACKUP-01");

        let tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .expect("source table list");
        for table in tables {
            if BACKUP_EXCLUDED_TABLES.contains(&table.as_str()) {
                // Deliberately absent from every Backup, so the replay cannot
                // have them (ADR-0076: restore leaves Telegram disconnected).
                continue;
            }
            let source_count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {}",
                quote_identifier(&table)
            ))
            .fetch_one(&pool)
            .await
            .expect("source row count");
            let replay_count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {}",
                quote_identifier(&table)
            ))
            .fetch_one(&replay)
            .await
            .expect("replayed row count");
            assert_eq!(replay_count, source_count, "row count mismatch for {table}");
        }

        replay.close().await;
        pool.close().await;
        remove_wal_files(&path);
    }

    #[tokio::test]
    async fn backup_restore_preserves_attendance_facts_and_frozen_dates() -> AppResult<()> {
        let (pool, path) = wal_test_pool("backup-attendance-preserve").await;
        let directory = tempfile_dir();
        let offset = FixedOffset::east_opt(8 * 60 * 60).expect("valid test offset");
        let earning_date = Utc::now()
            .with_timezone(&offset)
            .date_naive()
            .checked_sub_signed(Duration::days(60))
            .expect("test earning date");
        let month_start = earning_date.with_day(1).expect("month start");
        let month_key = format!("{:04}-{:02}", earning_date.year(), earning_date.month());
        let effective_at = offset
            .from_local_datetime(&month_start.and_time(NaiveTime::MIN))
            .single()
            .expect("month start should be unambiguous")
            .to_utc();
        crate::test_utils::insert_attendance_settings_with_window(
            &pool,
            2,
            true,
            100,
            600,
            12,
            effective_at,
        )
        .await?;

        let claimed_account = crate::accounts::create(
            &pool,
            "backup-claimed-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let deleted_account = crate::accounts::create(
            &pool,
            "backup-deleted-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;

        let local_date = earning_date.to_string();
        let finalized_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let interval_start = offset
            .from_local_datetime(
                &earning_date
                    .and_hms_opt(8, 0, 0)
                    .expect("interval start should be valid"),
            )
            .single()
            .expect("interval start should be unambiguous")
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let interval_end = offset
            .from_local_datetime(
                &earning_date
                    .and_hms_opt(22, 0, 0)
                    .expect("interval end should be valid"),
            )
            .single()
            .expect("interval end should be unambiguous")
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        sqlx::query(
            "INSERT INTO attendance_calendar_dates
               (earning_date, finalized_at, restart_observed_utc_offset)
             VALUES (?, ?, ?)",
        )
        .bind(&local_date)
        .bind(finalized_at)
        .bind(offset.local_minus_utc())
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO attendance_eligible_intervals (earning_date, starts_at, ends_at)
             VALUES (?, ?, ?)",
        )
        .bind(&local_date)
        .bind(&interval_start)
        .bind(&interval_end)
        .execute(&pool)
        .await?;

        for account_id in [&claimed_account.id, &deleted_account.id] {
            let source_at = offset
                .from_local_datetime(
                    &earning_date
                        .and_hms_opt(12, 0, 0)
                        .expect("source instant should be valid"),
                )
                .single()
                .expect("source instant should be unambiguous")
                .to_utc();
            let mut tx = crate::db::begin_immediate(&pool).await?;
            let mut source = crate::audit::NewAuditEvent::simple(
                "coin_accepted",
                openpiso_core::audit::AuditOutcome::Success,
                openpiso_core::audit::AuditActor::CoinDevice,
                "Accepted Attendance Backup test coin",
            );
            source.account_id = Some(account_id);
            source.amount_cents = Some(100);
            let source_audit_event_id = crate::audit::append(&mut tx, source).await?;
            crate::db::attendance_earning::record_earning(
                &mut tx,
                crate::db::attendance_earning::EarningInput {
                    account_id,
                    source_audit_event_id: &source_audit_event_id,
                    source_kind: crate::db::attendance::SourceKind::AcceptedCoin,
                    cents: 100,
                    source_at,
                    offset,
                    earning_date,
                    award: crate::db::attendance::AwardSnapshot {
                        configuration_version: 2,
                        daily_threshold_cents: 100,
                        reward_seconds: 600,
                    },
                    original_rejection_audit_event_id: None,
                    trigger_pc_id: None,
                },
            )
            .await?;
            tx.commit().await?;
        }

        let claim = crate::db::attendance_claims::claim(
            &pool,
            Utc::now,
            &claimed_account.id,
            None,
            Uuid::new_v4(),
            &month_key,
        )
        .await?;
        assert!(matches!(
            claim,
            crate::db::attendance_claims::ClaimOutcome::Claimed { claim, .. }
                if claim.claimed_seconds == 600
        ));
        crate::accounts::soft_delete_many_ids(&pool, std::slice::from_ref(&deleted_account.id))
            .await?;

        let months_before: Vec<(String, String, i64, String, String, String)> = sqlx::query_as(
            "SELECT account_id, earning_month, claim_window_months, claim_starts_at,
                    deadline_at_exclusive, claim_local_dates
             FROM attendance_earning_months ORDER BY account_id",
        )
        .fetch_all(&pool)
        .await?;
        let clock_before: (String, String) = sqlx::query_as(
            "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
        )
        .fetch_one(&pool)
        .await?;
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let filename = artifact
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("Backup filename")
            .to_owned();
        stage_restore(&pool, &directory, RestoreSource::Catalog(filename), &[]).await?;
        pool.close().await;
        apply_pending_restore(&path, &directory)
            .await?
            .expect("staged Attendance Backup should be applied");

        let restored = reopen_wal_test_pool(&path).await;
        for (table, expected) in [
            ("attendance_settings_versions", 2),
            ("attendance_clock_state", 1),
            ("attendance_calendar_dates", 1),
            ("attendance_eligible_intervals", 1),
            ("attendance_contributions", 2),
            ("attendance_daily_progress", 2),
            ("attendance_earning_months", 2),
            ("attendance_check_ins", 2),
            ("attendance_claims", 1),
            ("attendance_forfeitures", 1),
        ] {
            let actual: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&restored)
                .await?;
            assert_eq!(actual, expected, "restored Attendance rows in {table}");
        }
        let months_after: Vec<(String, String, i64, String, String, String)> = sqlx::query_as(
            "SELECT account_id, earning_month, claim_window_months, claim_starts_at,
                    deadline_at_exclusive, claim_local_dates
             FROM attendance_earning_months ORDER BY account_id",
        )
        .fetch_all(&restored)
        .await?;
        assert_eq!(months_after, months_before);
        let clock_after: (String, String) = sqlx::query_as(
            "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
        )
        .fetch_one(&restored)
        .await?;
        assert_eq!(clock_after, clock_before);
        let interval_after: (String, String, String, i64) = sqlx::query_as(
            "SELECT d.earning_date, i.starts_at, i.ends_at, d.restart_observed_utc_offset
             FROM attendance_calendar_dates d
             JOIN attendance_eligible_intervals i ON i.earning_date = d.earning_date",
        )
        .fetch_one(&restored)
        .await?;
        assert_eq!(
            interval_after,
            (
                local_date,
                interval_start,
                interval_end,
                i64::from(offset.local_minus_utc())
            )
        );
        let account_statuses: Vec<(String, String)> =
            sqlx::query_as("SELECT id, status FROM accounts WHERE id IN (?, ?) ORDER BY id")
                .bind(&claimed_account.id)
                .bind(&deleted_account.id)
                .fetch_all(&restored)
                .await?;
        assert_eq!(account_statuses.len(), 2);
        assert!(account_statuses
            .iter()
            .any(|(_, status)| status == "active"));
        assert!(account_statuses
            .iter()
            .any(|(_, status)| status == "deleted"));

        restored.close().await;
        let _ = std::fs::remove_dir_all(directory);
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn restoring_an_earlier_backup_keeps_new_coin_on_monotonic_open_date() -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-earlier-attendance").await;
        let directory = tempfile_dir();
        let offset = FixedOffset::east_opt(8 * 60 * 60).expect("valid test offset");
        let old_floor = Utc
            .with_ymd_and_hms(2099, 4, 2, 0, 0, 0)
            .single()
            .expect("old clock floor");
        let open_date = old_floor.with_timezone(&offset).date_naive();
        sqlx::query(
            "UPDATE attendance_clock_state
             SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(old_floor.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .bind(open_date.to_string())
        .execute(&pool)
        .await?;
        crate::test_utils::insert_attendance_settings_with_window(
            &pool,
            2,
            true,
            100,
            600,
            12,
            Utc.with_ymd_and_hms(2099, 3, 1, 0, 0, 0)
                .single()
                .expect("settings effective time"),
        )
        .await?;
        let schedule_id: i64 = sqlx::query_scalar(
            "INSERT INTO shop_hours_versions
               (enabled, last_call_lead_minutes, effective_at)
             VALUES (1, 0, '2099-03-01T00:00:00.000Z') RETURNING id",
        )
        .fetch_one(&pool)
        .await?;
        for weekday in 0..7 {
            sqlx::query(
                "INSERT INTO shop_hours_version_days
                   (shop_hours_version_id, weekday, mode, open_minute, close_minute)
                 VALUES (?, ?, 'open_24_hours', NULL, NULL)",
            )
            .bind(schedule_id)
            .bind(weekday)
            .execute(&pool)
            .await?;
        }
        let account = crate::accounts::create(
            &pool,
            "restore-earlier-account",
            "password",
            crate::accounts::AccountCreator::Admin,
            None,
        )
        .await?;
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let filename = artifact
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("Backup filename")
            .to_owned();

        let later_floor = old_floor + Duration::days(4);
        let later_date = later_floor.with_timezone(&offset).date_naive();
        sqlx::query(
            "UPDATE attendance_clock_state
             SET utc_floor = ?, open_local_date = ? WHERE id = 1",
        )
        .bind(later_floor.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .bind(later_date.to_string())
        .execute(&pool)
        .await?;
        stage_restore(&pool, &directory, RestoreSource::Catalog(filename), &[]).await?;
        pool.close().await;
        apply_pending_restore(&path, &directory)
            .await?
            .expect("earlier Attendance Backup should be applied");

        let restored = reopen_wal_test_pool(&path).await;
        let regressed_host = Utc
            .with_ymd_and_hms(2099, 4, 1, 12, 0, 0)
            .single()
            .expect("regressed host time");
        crate::db::attendance_earning::prepare_startup_state(&restored, regressed_host, offset)
            .await?;
        let clock: (String, String) = sqlx::query_as(
            "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
        )
        .fetch_one(&restored)
        .await?;
        assert_eq!(
            clock,
            (
                old_floor.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                open_date.to_string()
            )
        );
        assert!(regressed_host.with_timezone(&offset).date_naive() < open_date);

        let mut tx = crate::db::begin_immediate(&restored).await?;
        let Some(qualification) =
            crate::db::attendance::qualify_source(&mut tx, regressed_host, offset).await?
        else {
            panic!("open, enabled Attendance should qualify the restored coin")
        };
        assert_eq!(qualification.earning_date, open_date);
        let mut source = crate::audit::NewAuditEvent::simple(
            "coin_accepted",
            openpiso_core::audit::AuditOutcome::Success,
            openpiso_core::audit::AuditActor::CoinDevice,
            "Accepted post-Restore coin",
        );
        source.account_id = Some(&account.id);
        source.amount_cents = Some(100);
        let source_audit_event_id = crate::audit::append(&mut tx, source).await?;
        crate::db::attendance_earning::record_earning(
            &mut tx,
            crate::db::attendance_earning::EarningInput {
                account_id: &account.id,
                source_audit_event_id: &source_audit_event_id,
                source_kind: crate::db::attendance::SourceKind::AcceptedCoin,
                cents: 100,
                source_at: regressed_host,
                offset,
                earning_date: qualification.earning_date,
                award: qualification.award,
                original_rejection_audit_event_id: None,
                trigger_pc_id: None,
            },
        )
        .await?;
        tx.commit().await?;

        let contribution: (String, String) = sqlx::query_as(
            "SELECT earning_date, source_at FROM attendance_contributions
             WHERE source_audit_event_id = ?",
        )
        .bind(source_audit_event_id)
        .fetch_one(&restored)
        .await?;
        assert_eq!(contribution.0, open_date.to_string());
        assert_eq!(
            contribution.1,
            regressed_host.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        let final_clock: (String, String) = sqlx::query_as(
            "SELECT utc_floor, open_local_date FROM attendance_clock_state WHERE id = 1",
        )
        .fetch_one(&restored)
        .await?;
        assert_eq!(final_clock, clock);

        restored.close().await;
        let _ = std::fs::remove_dir_all(directory);
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn header_round_trips_and_checksum_detects_tampering() {
        let (pool, path) = wal_test_pool("backup-header").await;
        let sql = dump(&pool, &BackupSet::all()).await;
        let header = parse_backup_header(&sql).expect("header should parse");
        assert!(header.verify_checksum(&sql));
        assert!(!header.verify_checksum(&sql.replace("BEGIN TRANSACTION", "BEGIN IMMEDIATE")));
        pool.close().await;
        remove_wal_files(&path);
    }

    #[tokio::test]
    async fn migration_history_is_always_present_and_set_map_is_complete() {
        let (pool, path) = wal_test_pool("backup-map").await;
        let sql = dump(&pool, &[BackupSet::Accounts]).await;
        assert!(sql.contains("CREATE TABLE _openpiso_migrations"));
        assert!(sql.contains("INSERT INTO \"_openpiso_migrations\""));
        assert!(!sql.contains("simulator-only"));
        assert!(!sql.contains("owner-secret-hash"));

        let tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&pool)
        .await
        .expect("schema catalog query");
        for table in &tables {
            assert!(
                table.as_str() == "_openpiso_migrations"
                    || BACKUP_EXCLUDED_TABLES.contains(&table.as_str())
                    || table_backup_set(table).is_some(),
                "unmapped table: {table}"
            );
        }
        // Every exclusion must name a real table, so a rename cannot leave a
        // stale entry silently exempting nothing while a new unmapped table
        // would fail the guard above.
        for excluded in BACKUP_EXCLUDED_TABLES {
            assert!(
                tables.iter().any(|table| table == excluded),
                "excluded table does not exist: {excluded}"
            );
        }
        pool.close().await;
        remove_wal_files(&path);
    }

    #[tokio::test]
    async fn telemetry_installation_and_outbox_are_not_exported_or_restored() -> AppResult<()> {
        let (pool, path) = wal_test_pool("backup-telemetry-excluded").await;
        let id = crate::db::telemetry::installation(&pool)
            .await?
            .installation_id;
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-22T16:00:00Z")
            .expect("test time")
            .with_timezone(&Utc);
        crate::db::telemetry::upsert_summary(&pool, "2026-09-23", &id, "event-uuid", "{}", now)
            .await?;
        let sql = dump(&pool, &BackupSet::all()).await;
        assert!(sql.contains("CREATE TABLE telemetry_settings"));
        assert!(sql.contains("CREATE TABLE telemetry_outbox"));
        assert!(!sql.contains("INSERT INTO \"telemetry_settings\""));
        assert!(!sql.contains("INSERT INTO \"telemetry_outbox\""));
        assert!(!sql.contains(&id));

        let replay = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::raw_sql(&sql).execute(&replay).await?;
        for table in ["telemetry_settings", "telemetry_outbox"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&replay)
                .await?;
            assert_eq!(count, 0, "{table} must restore empty");
        }
        assert!(crate::db::telemetry::due_rows(&replay, now)
            .await?
            .is_empty());
        let new_id = crate::db::telemetry::installation(&replay).await?;
        assert_ne!(new_id.installation_id, id);
        replay.close().await;
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn telegram_integration_is_never_exported_even_when_connected() -> AppResult<()> {
        let (pool, path) = wal_test_pool("backup-telegram-excluded").await;
        // A fully connected integration with a secret-looking protected token.
        crate::db::telegram::connect_with_audit(
            &pool,
            &crate::db::telegram::TelegramConfigRow {
                bot_username: "shop_alerts_bot".to_owned(),
                chat_id: "424242".to_owned(),
                chat_title: "Owner".to_owned(),
                token_protected: "a7deadbeefsecret".to_owned(),
                connected_at: "2026-08-26T00:00:00+00:00".to_owned(),
                verified_at: "2026-08-26T00:00:01+00:00".to_owned(),
            },
        )
        .await?;

        // A complete export keeps empty Telegram schema because migration 49
        // remains applied. It carries no integration data or protected token,
        // so restore starts structurally valid and disconnected.
        let sql = dump(&pool, &BackupSet::all()).await;
        assert!(sql.contains("CREATE TABLE telegram_config"));
        assert!(sql.contains("CREATE TABLE telegram_outbox"));
        assert!(!sql.contains("INSERT INTO \"telegram_config\""));
        assert!(!sql.contains("INSERT INTO \"telegram_outbox\""));
        assert!(!sql.contains("a7deadbeefsecret"));
        let header = parse_backup_header(&sql).expect("header should parse");
        assert!(!header.row_counts.contains_key("telegram_config"));
        assert!(!header.row_counts.contains_key("telegram_outbox"));

        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn backup_reads_one_wal_snapshot_while_writer_runs() {
        let (pool, path) = wal_test_pool("backup-snapshot").await;
        sqlx::query("INSERT INTO accounts (id, username, username_normalized, password_hash) VALUES ('acct-snapshot', 'before', 'snapshot-before', 'hash')")
            .execute(&pool)
            .await
            .expect("snapshot account");
        let writer_pool = pool.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            let mut transaction = crate::db::begin_immediate(&writer_pool)
                .await
                .expect("writer transaction");
            sqlx::query("UPDATE accounts SET username = 'after' WHERE id = 'acct-snapshot'")
                .execute(&mut *transaction)
                .await
                .expect("account update");
            sqlx::query("UPDATE settings SET value = 'after' WHERE key = 'shop_name'")
                .execute(&mut *transaction)
                .await
                .expect("settings update");
            transaction.commit().await.expect("writer commit");
        });
        let directory = tempfile_dir();
        let artifact = write_backup(
            &pool,
            &directory,
            BackupKind::Manual,
            &[BackupSet::Accounts, BackupSet::Configuration],
        )
        .await
        .expect("snapshot Backup");
        writer.await.expect("writer task");
        let sql = std::fs::read_to_string(&artifact.path).expect("snapshot SQL");
        let replay = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("replay database");
        sqlx::raw_sql(&sql)
            .execute(&replay)
            .await
            .expect("snapshot replay");
        let username: String =
            sqlx::query_scalar("SELECT username FROM accounts WHERE id = 'acct-snapshot'")
                .fetch_one(&replay)
                .await
                .expect("replayed account");
        let shop_name: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'shop_name'")
                .fetch_one(&replay)
                .await
                .expect("replayed setting");
        assert_eq!(username == "before", shop_name == "OpenPiso Shop");
        assert_eq!(username == "after", shop_name == "after");
        replay.close().await;
        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
    }

    #[tokio::test]
    async fn pre_migration_backup_requires_all_sets() {
        let (pool, path) = wal_test_pool("backup-pre-migration").await;
        let directory = tempfile_dir();
        let error = write_backup(
            &pool,
            &directory,
            BackupKind::PreMigration,
            &[BackupSet::Accounts],
        )
        .await
        .expect_err("partial Pre-Migration Backup should be rejected");
        assert!(error.to_string().contains("every Backup Set"));
        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
    }

    #[tokio::test]
    async fn successful_writers_apply_recovery_retention_and_preserve_manual_backups() {
        let pool = mem_test_pool().await;
        let directory = tempfile_dir();

        for index in 0..PRE_RESTORE_RETENTION_COUNT {
            write_catalog_fixture(&directory, BackupKind::PreRestore, index);
        }
        write_backup(&pool, &directory, BackupKind::PreRestore, &BackupSet::all())
            .await
            .expect("Pre-Restore Backup should write");
        let entries = list_backups(&directory).await.expect("Backup Catalog");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::PreRestore.as_str())
                .count(),
            PRE_RESTORE_RETENTION_COUNT
        );

        for index in 0..PRE_MIGRATION_RETENTION_COUNT {
            write_catalog_fixture(&directory, BackupKind::PreMigration, index);
        }
        std::fs::write(
            directory.join("fixture-pre_migration-0.db"),
            b"database companion",
        )
        .expect("database companion");
        write_pre_migration_backup(&pool, &directory, 34, 35)
            .await
            .expect("Pre-Migration Backup should write");
        let entries = list_backups(&directory).await.expect("Backup Catalog");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::PreMigration.as_str())
                .count(),
            PRE_MIGRATION_RETENTION_COUNT
        );
        assert!(!directory.join("fixture-pre_migration-0.db").exists());

        for index in 0..12 {
            write_catalog_fixture(&directory, BackupKind::Manual, index);
        }
        write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all())
            .await
            .expect("manual Backup should write");
        let entries = list_backups(&directory).await.expect("Backup Catalog");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::Manual.as_str())
                .count(),
            13
        );

        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
    }

    #[tokio::test]
    async fn retention_is_independent_per_backup_kind_and_manual_is_unlimited() {
        let directory = tempfile_dir();
        for kind in [
            BackupKind::Scheduled,
            BackupKind::PreMigration,
            BackupKind::PreRestore,
            BackupKind::Manual,
        ] {
            for index in 0..12 {
                write_catalog_fixture(&directory, kind, index);
            }
        }

        prune_backups(&directory, BackupKind::Scheduled, 2)
            .await
            .expect("scheduled retention");
        prune_backups(
            &directory,
            BackupKind::PreMigration,
            PRE_MIGRATION_RETENTION_COUNT,
        )
        .await
        .expect("Pre-Migration retention");
        prune_backups(
            &directory,
            BackupKind::PreRestore,
            PRE_RESTORE_RETENTION_COUNT,
        )
        .await
        .expect("Pre-Restore retention");
        prune_backups(&directory, BackupKind::Manual, 0)
            .await
            .expect("manual retention no-op");

        let entries = list_backups(&directory).await.expect("Backup Catalog");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::Scheduled.as_str())
                .count(),
            2
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::PreMigration.as_str())
                .count(),
            PRE_MIGRATION_RETENTION_COUNT
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::PreRestore.as_str())
                .count(),
            PRE_RESTORE_RETENTION_COUNT
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == BackupKind::Manual.as_str())
                .count(),
            12
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn backup_sets_match_schema_decision() {
        assert_eq!(table_backup_set("accounts"), Some(BackupSet::Accounts));
        assert_eq!(
            table_backup_set("owner_credentials"),
            Some(BackupSet::Credentials)
        );
        assert_eq!(table_backup_set("_openpiso_migrations"), None);
        assert_eq!(table_backup_set("future_table"), None);
        // Monitoring Suppression is temporary configuration, not evidence.
        assert_eq!(
            table_backup_set("pc_monitoring_suppressions"),
            Some(BackupSet::Configuration)
        );
        // So is the Escalation Cooldown: the Alert is the evidence.
        assert_eq!(
            table_backup_set("pc_escalation_cooldowns"),
            Some(BackupSet::Configuration)
        );
        // Attendance projection and frozen months travel with Accounts.
        assert_eq!(
            table_backup_set("attendance_daily_progress"),
            Some(BackupSet::Accounts)
        );
        assert_eq!(
            table_backup_set("attendance_earning_months"),
            Some(BackupSet::Accounts)
        );
        // Contributions and Check-Ins are immutable source facts.
        assert_eq!(
            table_backup_set("attendance_contributions"),
            Some(BackupSet::Ledger)
        );
        assert_eq!(
            table_backup_set("attendance_check_ins"),
            Some(BackupSet::Ledger)
        );
        assert_eq!(
            table_backup_set("attendance_claims"),
            Some(BackupSet::Ledger)
        );
        assert_eq!(
            table_backup_set("attendance_forfeitures"),
            Some(BackupSet::Ledger)
        );
    }

    #[tokio::test]
    async fn staged_restore_accepts_catalog_and_absolute_path_then_can_be_cancelled(
    ) -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-stage").await;
        let directory = tempfile_dir();
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let filename = artifact
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("Backup filename")
            .to_owned();

        let pending = stage_restore(
            &pool,
            &directory,
            RestoreSource::Catalog(filename.clone()),
            &[],
        )
        .await?;
        assert_eq!(pending.filename, filename);
        let marker: PendingRestoreMarker =
            serde_json::from_slice(&std::fs::read(pending_restore_path(&directory))?)
                .expect("valid Pending Restore marker");
        assert!(marker.staged_path.exists());
        assert!(pending_restore(&directory).await?.is_some());
        assert!(cancel_pending_restore(&directory).await?);
        assert!(!marker.staged_path.exists());
        assert!(pending_restore(&directory).await?.is_none());

        let absolute = directory.join("absolute-source.sql");
        std::fs::copy(&artifact.path, &absolute)?;
        let pending = stage_restore(
            &pool,
            &directory,
            RestoreSource::AbsolutePath(absolute),
            &[],
        )
        .await?;
        assert_eq!(pending.schema_version, crate::db::CURRENT_SCHEMA_VERSION);
        assert!(cancel_pending_restore(&directory).await?);

        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn staged_restore_refuses_tampering_and_blocking_pcs_without_touching_live_db(
    ) -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-gates").await;
        let directory = tempfile_dir();
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let before = std::fs::read(&path)?;
        let filename = artifact
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let sql = std::fs::read_to_string(&artifact.path)?
            .replace("BEGIN TRANSACTION", "BEGIN IMMEDIATE");
        std::fs::write(&artifact.path, sql)?;
        let checksum = stage_restore(
            &pool,
            &directory,
            RestoreSource::Catalog(filename.clone()),
            &[],
        )
        .await
        .expect_err("tampered body must be refused");
        assert!(checksum.to_string().contains("checksum"));
        assert_eq!(std::fs::read(&path)?, before);

        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let filename = artifact
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        sqlx::query("UPDATE pcs SET remaining_seconds = 300 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        let blocked = stage_restore(&pool, &directory, RestoreSource::Catalog(filename), &[])
            .await
            .expect_err("PC with Remaining Time must block restore");
        assert!(blocked.to_string().contains("PC 01"));
        assert!(blocked.to_string().contains("blocking PCs"));
        assert_eq!(std::fs::read(&path)?, before);

        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn malformed_pending_restore_marker_is_a_logged_noop() -> AppResult<()> {
        let directory = tempfile_dir();
        let database_path = directory.join("openpiso.db");
        std::fs::write(
            pending_restore_path(&directory),
            b"{ this is not a Pending Restore marker",
        )?;

        assert!(apply_pending_restore(&database_path, &directory)
            .await?
            .is_none());
        assert!(pending_restore(&directory).await?.is_none());
        assert!(pending_restore_path(&directory).exists());

        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test]
    async fn applying_pending_restore_keeps_outgoing_database_as_pre_restore_backup(
    ) -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-apply").await;
        let directory = tempfile_dir();
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let filename = artifact
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        stage_restore(&pool, &directory, RestoreSource::Catalog(filename), &[]).await?;
        let marker: PendingRestoreMarker =
            serde_json::from_slice(&std::fs::read(pending_restore_path(&directory))?)
                .expect("valid Pending Restore marker");
        let staged_path = marker.staged_path.clone();
        sqlx::query("UPDATE pcs SET remaining_seconds = 300 WHERE id = 'pc-01'")
            .execute(&pool)
            .await?;
        pool.close().await;
        let blocked = apply_pending_restore(&path, &directory)
            .await
            .expect_err("launch must re-check restore preconditions");
        assert!(blocked.to_string().contains("PC 01"));
        assert!(pending_restore(&directory).await?.is_some());

        let cleanup_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}", path.display()))
            .await?;
        sqlx::query("UPDATE pcs SET remaining_seconds = 0 WHERE id = 'pc-01'")
            .execute(&cleanup_pool)
            .await?;
        cleanup_pool.close().await;

        let applied = apply_pending_restore(&path, &directory)
            .await?
            .expect("Pending Restore should be applied");
        assert!(pending_restore(&directory).await?.is_none());
        assert!(!staged_path.exists());
        let catalog = list_backups(&directory).await?;
        let pre_restore = catalog
            .iter()
            .find(|entry| entry.kind == BackupKind::PreRestore.as_str())
            .expect("Pre-Restore Backup should be cataloged");
        let pre_restore_sql = std::fs::read_to_string(directory.join(&pre_restore.filename))?;
        let pre_restore_header = parse_backup_header(&pre_restore_sql)?;
        assert!(pre_restore_header.verify_checksum(&pre_restore_sql));
        assert_eq!(pre_restore_header.kind, BackupKind::PreRestore.as_str());
        assert!(!catalog
            .iter()
            .any(|entry| entry.kind == BackupKind::PreMigration.as_str()));
        assert_eq!(
            applied.filename,
            artifact
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );

        let restored_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}", path.display()))
            .await?;
        let restored_seconds: i64 =
            sqlx::query_scalar("SELECT remaining_seconds FROM pcs WHERE id = 'pc-01'")
                .fetch_one(&restored_pool)
                .await?;
        assert_eq!(restored_seconds, 0);
        let schema: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
            .fetch_one(&restored_pool)
            .await?;
        assert_eq!(schema, crate::db::CURRENT_SCHEMA_VERSION);
        restored_pool.close().await;

        let _ = std::fs::remove_dir_all(directory);
        remove_wal_files(&path);
        Ok(())
    }

    /// Replace a Backup's body and re-stamp the header checksum over it, so the
    /// altered file reaches the gate under test instead of failing on checksum.
    fn replace_body(sql: &str, body: &str) -> String {
        let stamped = rewrite_header_field(sql, "sha256", &sha256_hex(body.as_bytes()));
        let header_end = stamped.find(HEADER_MARKER).expect("header marker") + HEADER_MARKER.len();
        format!("{}{body}", &stamped[..header_end])
    }

    /// Rewrite one header field in place. The checksum covers only the body, so
    /// a header-only edit stays consistent and exercises the gate under test.
    fn rewrite_header_field(sql: &str, key: &str, value: &str) -> String {
        let header_end = sql.find(HEADER_MARKER).expect("Backup header marker");
        let line_start = sql[..header_end]
            .find(&format!("-- {key}: "))
            .expect("header field should be present");
        let line_end = line_start
            + sql[line_start..]
                .find('\n')
                .expect("header field should end in a newline");
        format!(
            "{}-- {key}: {value}{}",
            &sql[..line_start],
            &sql[line_end..]
        )
    }

    #[tokio::test]
    async fn staged_restore_refuses_partial_sets_and_a_newer_schema_version() -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-acceptance").await;
        let directory = tempfile_dir();

        let partial = write_backup(
            &pool,
            &directory,
            BackupKind::Manual,
            &[BackupSet::Accounts],
        )
        .await?;
        let partial_error = stage_restore(
            &pool,
            &directory,
            RestoreSource::AbsolutePath(partial.path.clone()),
            &[],
        )
        .await
        .expect_err("a Set-restricted dump must be refused as restore input");
        match &partial_error {
            AppError::RestoreRejected { gate, message, .. } => {
                assert_eq!(gate, "backup_sets");
                assert!(message.contains("all seven Backup Sets"));
            }
            other => panic!("expected a Backup Set refusal, got {other}"),
        }
        assert!(pending_restore(&directory).await?.is_none());

        let full = write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;
        let newer = directory.join("newer-schema.sql");
        let newer_sql = rewrite_header_field(
            &std::fs::read_to_string(&full.path)?,
            "schema_version",
            &(crate::db::CURRENT_SCHEMA_VERSION + 1).to_string(),
        );
        std::fs::write(&newer, &newer_sql)?;
        // The gate must reject on version alone, not on a broken checksum.
        assert!(parse_backup_header(&newer_sql)?.verify_checksum(&newer_sql));
        let newer_error = stage_restore(&pool, &directory, RestoreSource::AbsolutePath(newer), &[])
            .await
            .expect_err("a dump from a newer schema version must be refused");
        match &newer_error {
            AppError::RestoreRejected { gate, message, .. } => {
                assert_eq!(gate, "schema_version");
                assert!(message.contains("is newer than the running app schema version"));
            }
            other => panic!("expected a schema version refusal, got {other}"),
        }
        assert!(pending_restore(&directory).await?.is_none());

        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn staged_restore_refuses_a_dump_that_breaks_foreign_key_integrity() -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-foreign-keys").await;
        let directory = tempfile_dir();
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;

        // The dump replays with foreign keys off, so a dangling reference only
        // surfaces in the verification step. The header row count is kept honest
        // so the foreign-key gate is what refuses this file, not the row counts.
        let sql = std::fs::read_to_string(&artifact.path)?;
        let corrupted_body = sql
            .get(parse_backup_header(&sql)?.body_offset..)
            .expect("Backup body")
            .replace(
                "COMMIT;\n",
                "INSERT INTO \"sessions\" (\"id\", \"pc_id\", \"status\") VALUES ('session-orphan', 'pc-does-not-exist', 'idle');\nCOMMIT;\n",
            );
        assert!(sql.contains("-- row_count: sessions=0\n"));
        let counted = sql.replace("-- row_count: sessions=0\n", "-- row_count: sessions=1\n");
        let corrupted = directory.join("corrupted-foreign-keys.sql");
        std::fs::write(&corrupted, replace_body(&counted, &corrupted_body))?;

        let error = stage_restore(
            &pool,
            &directory,
            RestoreSource::AbsolutePath(corrupted),
            &[],
        )
        .await
        .expect_err("a dump with a dangling reference must be refused");
        match &error {
            AppError::RestoreRejected { gate, message, .. } => {
                assert_eq!(gate, "foreign_keys");
                // Replay failures share this gate, so assert the verification
                // step is what caught the dangling reference.
                assert!(message.contains("failed foreign-key integrity verification"));
            }
            other => panic!("expected a foreign-key refusal, got {other}"),
        }
        assert!(pending_restore(&directory).await?.is_none());

        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn staged_restore_refuses_a_dump_that_lost_rows() -> AppResult<()> {
        let (pool, path) = wal_test_pool("restore-row-counts").await;
        sqlx::query("INSERT INTO accounts (id, username, username_normalized, password_hash) VALUES ('acct-dropped', 'Dropped User', 'dropped user', 'hash')")
            .execute(&pool)
            .await?;
        let directory = tempfile_dir();
        let artifact =
            write_backup(&pool, &directory, BackupKind::Manual, &BackupSet::all()).await?;

        let sql = std::fs::read_to_string(&artifact.path)?;
        let body = sql
            .get(parse_backup_header(&sql)?.body_offset..)
            .expect("Backup body")
            .lines()
            .filter(|line| !line.contains("'acct-dropped'"))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let truncated = directory.join("lost-rows.sql");
        std::fs::write(&truncated, replace_body(&sql, &body))?;

        let error = stage_restore(
            &pool,
            &directory,
            RestoreSource::AbsolutePath(truncated),
            &[],
        )
        .await
        .expect_err("a dump missing rows its header recorded must be refused");
        match &error {
            AppError::RestoreRejected { gate, message, .. } => {
                assert_eq!(gate, "row_counts");
                assert!(message.contains("row count mismatch for accounts"));
            }
            other => panic!("expected a row count refusal, got {other}"),
        }
        assert!(pending_restore(&directory).await?.is_none());

        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    #[tokio::test]
    async fn an_older_schema_dump_forward_migrates_with_data_intact() -> AppResult<()> {
        let older_pool = crate::test_utils::schema_v12_mem_pool().await;
        crate::db::seed(&older_pool).await?;
        sqlx::query("INSERT INTO accounts (id, username, username_normalized, password_hash) VALUES ('acct-v12', 'Legacy User', 'legacy user', 'legacy-hash')")
            .execute(&older_pool)
            .await?;
        sqlx::query(
            "UPDATE accounts
             SET remaining_seconds = 420, points_balance = 7, point_earn_carry_cents = 3
             WHERE id = 'acct-v12'",
        )
        .execute(&older_pool)
        .await?;
        sqlx::query("UPDATE settings SET value = 'Legacy Shop' WHERE key = 'shop_name'")
            .execute(&older_pool)
            .await?;
        sqlx::query(
            "INSERT INTO audit_log
               (id, event_type, outcome, actor, account_id, seconds_delta, cash_flag,
                summary, metadata_json, created_at)
             VALUES ('legacy-audit-v12', 'manual_adjustment', 'success', 'admin',
                     'acct-v12', 17, 0, 'Legacy adjustment', '{\"source\":\"pre-attendance\"}',
                     '2025-01-02 03:04:05')",
        )
        .execute(&older_pool)
        .await?;
        let older_directory = tempfile_dir();
        let older_artifact = write_backup(
            &older_pool,
            &older_directory,
            BackupKind::Manual,
            &BackupSet::all(),
        )
        .await?;
        assert_eq!(
            parse_backup_header(&std::fs::read_to_string(&older_artifact.path)?)?.schema_version,
            12
        );
        older_pool.close().await;

        let (pool, path) = wal_test_pool("restore-forward-migrate").await;
        let directory = tempfile_dir();
        let source = directory.join("older-schema.sql");
        std::fs::copy(&older_artifact.path, &source)?;

        let pending =
            stage_restore(&pool, &directory, RestoreSource::AbsolutePath(source), &[]).await?;
        assert_eq!(pending.schema_version, 12);
        let marker: PendingRestoreMarker =
            serde_json::from_slice(&std::fs::read(pending_restore_path(&directory))?)
                .expect("valid Pending Restore marker");

        let staged = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}", marker.staged_path.display()))
            .await?;
        let staged_version: i64 =
            sqlx::query_scalar("SELECT MAX(version) FROM _openpiso_migrations")
                .fetch_one(&staged)
                .await?;
        assert_eq!(staged_version, crate::db::CURRENT_SCHEMA_VERSION);
        let migration_history: (i64, i64, i64) =
            sqlx::query_as("SELECT COUNT(*), MIN(version), MAX(version) FROM _openpiso_migrations")
                .fetch_one(&staged)
                .await?;
        assert_eq!(
            migration_history,
            (
                crate::db::CURRENT_SCHEMA_VERSION,
                1,
                crate::db::CURRENT_SCHEMA_VERSION
            )
        );
        let account_snapshot: (String, String, i64, i64, i64) = sqlx::query_as(
            "SELECT username, password_hash, remaining_seconds, points_balance_centi,
                    point_earn_carry_centicents
             FROM accounts WHERE id = 'acct-v12'",
        )
        .fetch_one(&staged)
        .await?;
        assert_eq!(
            account_snapshot,
            (
                "Legacy User".to_owned(),
                "legacy-hash".to_owned(),
                420,
                700,
                300
            )
        );
        let shop_name: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'shop_name'")
                .fetch_one(&staged)
                .await?;
        assert_eq!(shop_name, "Legacy Shop");
        let legacy_audit: (String, Option<String>, Option<i64>, String, String) = sqlx::query_as(
            "SELECT event_type, account_id, seconds_delta, metadata_json, created_at
             FROM audit_log WHERE id = 'legacy-audit-v12'",
        )
        .fetch_one(&staged)
        .await?;
        assert_eq!(
            legacy_audit,
            (
                "manual_adjustment".to_owned(),
                Some("acct-v12".to_owned()),
                Some(17),
                "{\"source\":\"pre-attendance\"}".to_owned(),
                "2025-01-02 03:04:05".to_owned()
            )
        );
        // Insert-or-ignore seeding re-adds settings keys an older database lacks
        // without clobbering restored values.
        let seeded_keys: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM settings WHERE key IN ('shop_name', 'simulator_enabled', 'shutdown_grace_seconds', 'offline_grace_seconds')",
        )
        .fetch_one(&staged)
        .await?;
        assert_eq!(seeded_keys, 4);
        staged.close().await;

        assert!(cancel_pending_restore(&directory).await?);
        let _ = std::fs::remove_dir_all(older_directory);
        let _ = std::fs::remove_dir_all(directory);
        pool.close().await;
        remove_wal_files(&path);
        Ok(())
    }

    fn remove_wal_files(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}

use sqlx::{sqlite::SqliteRow, Row, SqlitePool};

use crate::{
    errors::{AppError, AppResult},
    models::{ThemeMode, ThemeSettings, ThemeSettingsInput},
};

pub async fn theme_settings(pool: &SqlitePool) -> AppResult<ThemeSettings> {
    let row = sqlx::query(
        "SELECT accent_color, mode_web_admin, mode_kiosk, mode_client, version
         FROM theme_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    theme_from_row(row)
}

pub async fn update_theme_settings(
    pool: &SqlitePool,
    input: &ThemeSettingsInput,
) -> AppResult<ThemeSettings> {
    // Required by the full-record wire shape; last-write-wins deliberately
    // does not use the caller's version as a concurrency precondition.
    let _client_version = input.version;
    let accent_color = input.accent_color.to_ascii_lowercase();
    if !valid_accent_color(&accent_color) {
        return Err(AppError::InvalidInput(
            "accent_color must be a #rrggbb hex color".to_owned(),
        ));
    }

    let mut transaction = super::begin_immediate(pool).await?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE theme_settings
         SET accent_color = ?, mode_web_admin = ?, mode_kiosk = ?, mode_client = ?,
             version = version + 1, updated_at = datetime('now')
         WHERE id = 1
         RETURNING version",
    )
    .bind(&accent_color)
    .bind(input.mode_web_admin.as_str())
    .bind(input.mode_kiosk.as_str())
    .bind(input.mode_client.as_str())
    .fetch_one(&mut *transaction)
    .await?;

    let settings = sqlx::query(
        "SELECT accent_color, mode_web_admin, mode_kiosk, mode_client, version
         FROM theme_settings WHERE id = 1 AND version = ?",
    )
    .bind(version)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    theme_from_row(settings)
}

pub async fn theme_version(pool: &SqlitePool) -> AppResult<i64> {
    sqlx::query_scalar("SELECT version FROM theme_settings WHERE id = 1")
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

fn theme_from_row(row: SqliteRow) -> AppResult<ThemeSettings> {
    Ok(ThemeSettings {
        accent_color: row.try_get("accent_color")?,
        mode_web_admin: parse_mode(row.try_get("mode_web_admin")?)?,
        mode_kiosk: parse_mode(row.try_get("mode_kiosk")?)?,
        mode_client: parse_mode(row.try_get("mode_client")?)?,
        version: row.try_get("version")?,
    })
}

fn parse_mode(value: &str) -> AppResult<ThemeMode> {
    match value {
        "light" => Ok(ThemeMode::Light),
        "dark" => Ok(ThemeMode::Dark),
        _ => Err(AppError::InvalidInput(
            "stored theme mode must be light or dark".to_owned(),
        )),
    }
}

fn valid_accent_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

impl ThemeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    #[tokio::test]
    async fn theme_write_normalizes_color_bumps_version_and_writes_no_audit_event() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let seeded = theme_settings(&pool).await?;
        assert_eq!(seeded.accent_color, "#15803d");
        assert_eq!(seeded.version, 1);

        let updated = update_theme_settings(
            &pool,
            &ThemeSettingsInput {
                accent_color: "#AABBCC".to_owned(),
                mode_web_admin: ThemeMode::Dark,
                mode_kiosk: ThemeMode::Light,
                mode_client: ThemeMode::Dark,
                version: 1,
            },
        )
        .await?;
        assert_eq!(updated.accent_color, "#aabbcc");
        assert_eq!(updated.version, 2);

        let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await?;
        assert_eq!(audit_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn theme_write_rejects_invalid_color_without_bumping_version() -> AppResult<()> {
        let pool = test_pool().await?;
        let result = update_theme_settings(
            &pool,
            &ThemeSettingsInput {
                accent_color: "#abc".to_owned(),
                mode_web_admin: ThemeMode::Light,
                mode_kiosk: ThemeMode::Light,
                mode_client: ThemeMode::Light,
                version: 1,
            },
        )
        .await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        assert_eq!(theme_version(&pool).await?, 1);
        Ok(())
    }
}

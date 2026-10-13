use std::collections::BTreeMap;
use std::time::Duration;

use openpiso_core::audit::{AuditActor, AuditOutcome};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    audit::{self, NewAuditEvent},
    auth::{hash_password, verify_password},
    coin_source::CoinSourceConfig,
    errors::{AppError, AppResult},
    models::{
        CoinDeviceSettings, CoinDeviceSettingsInput, EconomySettings, EconomySettingsInput,
        PulseMapEntry, VoucherTemplate, VoucherTemplateInput,
    },
    terminal_selection::TerminalSelectionConfig,
};

pub async fn economy_settings(pool: &SqlitePool) -> AppResult<EconomySettings> {
    let (rate_amount_cents, rate_seconds) = super::rates::active_rate(pool).await?;
    let (earn_cents_per_point, unsafe_bonus_threshold_basis_points) =
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT earn_cents_per_point, unsafe_bonus_threshold_basis_points
             FROM economy_settings WHERE id = 1",
        )
        .fetch_one(pool)
        .await?;
    let tier = super::redemption_tiers::most_generous_redemption_tier(pool).await?;
    super::rates::economy_settings_from_values(
        earn_cents_per_point,
        unsafe_bonus_threshold_basis_points,
        rate_amount_cents,
        rate_seconds,
        tier.rate(),
    )
}

pub async fn update_economy_settings(
    pool: &SqlitePool,
    input: &EconomySettingsInput,
) -> AppResult<EconomySettings> {
    if input.earn_cents_per_point <= 0 {
        return Err(AppError::InvalidInput(
            "earn cents per point must be positive".to_owned(),
        ));
    }
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "UPDATE economy_settings
         SET earn_cents_per_point = ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = 1",
    )
    .bind(input.earn_cents_per_point)
    .execute(&mut *transaction)
    .await?;
    let mut event = NewAuditEvent::simple(
        "economy_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated points economy settings",
    );
    event.metadata = json!({
        "earn_cents_per_point": input.earn_cents_per_point
    });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    economy_settings(pool).await
}

/// Read the owner-editable coin-device settings (migration 0007). `com_port`
/// is normalized to `None` when blank so the wiring can fall back to the
/// simulator source. The pulse map is returned ordered by pulse count.
pub async fn coin_device_settings(pool: &SqlitePool) -> AppResult<CoinDeviceSettings> {
    let row = sqlx::query_as::<_, (Option<String>, i64, String, String, String, i64, i64, i64)>(
        "SELECT com_port, baud_rate, set_pin_line, coin_pin_line, coin_pin_polarity,
                inter_coin_gap_ms, inactivity_timeout_seconds, hard_cap_seconds
         FROM coin_device_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    let pulse_map = sqlx::query_as::<_, (i64, i64)>(
        "SELECT pulse_count, amount_cents FROM coin_device_pulse_map ORDER BY pulse_count",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(pulse_count, amount_cents)| PulseMapEntry {
        pulse_count,
        amount_cents,
    })
    .collect();
    let calibration_required = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'coin_calibration_required'",
    )
    .fetch_optional(pool)
    .await?
    .is_some_and(|value| value == "true");
    crate::coin_source::CoinPinPolarity::parse(&row.4)?;
    Ok(CoinDeviceSettings {
        com_port: row.0.filter(|port| !port.trim().is_empty()),
        baud_rate: row.1,
        set_pin_line: row.2,
        coin_pin_line: row.3,
        coin_pin_polarity: row.4,
        inter_coin_gap_ms: row.5,
        inactivity_timeout_seconds: row.6,
        hard_cap_seconds: row.7,
        pulse_map,
        calibration_required,
    })
}

/// Build the runtime `CoinSourceConfig` from saved settings. The persisted
/// Pulse Map is intentionally ignored: live authority is fixed at 100 cents
/// per observed pulse. The map remains available to calibration and the
/// historical Rejection Credit workflow only.
pub fn coin_source_config_from_settings(
    settings: &CoinDeviceSettings,
) -> AppResult<CoinSourceConfig> {
    build_coin_source_config(settings.inter_coin_gap_ms)
}

pub fn selection_config_from_settings(
    settings: &CoinDeviceSettings,
) -> AppResult<TerminalSelectionConfig> {
    if settings.inactivity_timeout_seconds <= 0
        || settings.hard_cap_seconds < settings.inactivity_timeout_seconds
    {
        return Err(AppError::InvalidInput(
            "selection timers must be positive and the hard cap must be at least the inactivity timeout"
                .to_owned(),
        ));
    }
    let inactivity = u64::try_from(settings.inactivity_timeout_seconds)
        .map_err(|_| AppError::InvalidInput("invalid inactivity timeout".to_owned()))?;
    let hard_cap = u64::try_from(settings.hard_cap_seconds)
        .map_err(|_| AppError::InvalidInput("invalid selection hard cap".to_owned()))?;
    Ok(TerminalSelectionConfig {
        inactivity_timeout: Duration::from_secs(inactivity),
        hard_cap: Duration::from_secs(hard_cap),
    })
}

/// Validate the runtime gap. Pulse Map validation remains separate because
/// the map is persisted for Rejection Credit and calibration, never embedded
/// in the runtime Coin Source configuration.
fn build_coin_source_config(inter_coin_gap_ms: i64) -> AppResult<CoinSourceConfig> {
    if !(1..=10_000).contains(&inter_coin_gap_ms) {
        return Err(AppError::InvalidInput(
            "inter-coin gap must be between 1 and 10000 milliseconds".to_owned(),
        ));
    }
    Ok(CoinSourceConfig {
        inter_coin_gap: Duration::from_millis(inter_coin_gap_ms as u64),
    })
}

fn validate_pulse_map(
    inter_coin_gap_ms: i64,
    entries: &[PulseMapEntry],
) -> AppResult<BTreeMap<i64, i64>> {
    build_coin_source_config(inter_coin_gap_ms)?;
    let mut pulse_map = BTreeMap::new();
    for entry in entries {
        if entry.pulse_count <= 0 || entry.amount_cents <= 0 {
            return Err(AppError::InvalidInput(
                "pulse map entries must have positive pulse counts and cents".to_owned(),
            ));
        }
        if pulse_map
            .insert(entry.pulse_count, entry.amount_cents)
            .is_some()
        {
            return Err(AppError::InvalidInput(format!(
                "pulse map contains duplicate pulse count {}",
                entry.pulse_count
            )));
        }
    }
    if pulse_map.is_empty() {
        return Err(AppError::InvalidInput(
            "pulse map must contain at least one entry".to_owned(),
        ));
    }
    Ok(pulse_map)
}

/// Persist owner-edited coin-device settings inside a single transaction and
/// record a canonical audit event (ADR-0015). The pulse map is rewritten from
/// the validated map so the stored rows always satisfy the
/// `CoinSourceConfig` invariants. Duplicate pulse counts are rejected.
/// Returns the freshly read settings.
pub async fn update_coin_device_settings(
    pool: &SqlitePool,
    input: &CoinDeviceSettingsInput,
) -> AppResult<CoinDeviceSettings> {
    update_coin_device_settings_inner(pool, input, None).await
}

pub async fn update_coin_device_settings_with_calibration_audit(
    pool: &SqlitePool,
    input: &CoinDeviceSettingsInput,
    capture_id: &str,
    pulse_count: i64,
    amount_cents: i64,
    clears_candidate_gate: bool,
) -> AppResult<CoinDeviceSettings> {
    update_coin_device_settings_inner(
        pool,
        input,
        Some((capture_id, pulse_count, amount_cents, clears_candidate_gate)),
    )
    .await
}

async fn update_coin_device_settings_inner(
    pool: &SqlitePool,
    input: &CoinDeviceSettingsInput,
    calibration_audit: Option<(&str, i64, i64, bool)>,
) -> AppResult<CoinDeviceSettings> {
    if !matches!(input.set_pin_line.as_str(), "rts" | "dtr") {
        return Err(AppError::InvalidInput(
            "set pin line must be rts or dtr".to_owned(),
        ));
    }
    if !matches!(input.coin_pin_line.as_str(), "cts" | "dsr" | "ri" | "cd") {
        return Err(AppError::InvalidInput(
            "coin pin line must be cts, dsr, ri, or cd".to_owned(),
        ));
    }
    crate::coin_source::CoinPinPolarity::parse(&input.coin_pin_polarity)?;
    if input.baud_rate <= 0 {
        return Err(AppError::InvalidInput(
            "baud rate must be positive".to_owned(),
        ));
    }
    // Validates the gap and rejects duplicate pulse counts before the
    // INSERT below reaches the primary-key constraint.
    let current = coin_device_settings(pool).await?;
    let inactivity_timeout_seconds = input
        .inactivity_timeout_seconds
        .unwrap_or(current.inactivity_timeout_seconds);
    let hard_cap_seconds = input.hard_cap_seconds.unwrap_or(current.hard_cap_seconds);
    if inactivity_timeout_seconds <= 0 {
        return Err(AppError::InvalidInput(
            "selection inactivity timeout must be positive".to_owned(),
        ));
    }
    if hard_cap_seconds <= 0 || hard_cap_seconds < inactivity_timeout_seconds {
        return Err(AppError::InvalidInput(
            "selection hard cap must be positive and at least the inactivity timeout".to_owned(),
        ));
    }
    let pulse_map = validate_pulse_map(input.inter_coin_gap_ms, &input.pulse_map)?;
    if current.calibration_required && calibration_audit.is_none() {
        let current_map = current
            .pulse_map
            .iter()
            .map(|entry| (entry.pulse_count, entry.amount_cents))
            .collect::<BTreeMap<_, _>>();
        if current_map != pulse_map {
            return Err(AppError::Conflict(
                "the fresh-install candidate pulse map is locked until physical calibration Apply"
                    .to_owned(),
            ));
        }
    }
    let com_port = input
        .com_port
        .as_deref()
        .map(str::trim)
        .filter(|port| !port.is_empty())
        .map(str::to_owned);

    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "UPDATE coin_device_settings
         SET com_port = ?, baud_rate = ?, set_pin_line = ?, coin_pin_line = ?,
             coin_pin_polarity = ?, inter_coin_gap_ms = ?, inactivity_timeout_seconds = ?, hard_cap_seconds = ?,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = 1",
    )
    .bind(&com_port)
    .bind(input.baud_rate)
    .bind(&input.set_pin_line)
    .bind(&input.coin_pin_line)
    .bind(&input.coin_pin_polarity)
    .bind(input.inter_coin_gap_ms)
    .bind(inactivity_timeout_seconds)
    .bind(hard_cap_seconds)
    .execute(&mut *transaction)
    .await?;

    sqlx::query("DELETE FROM coin_device_pulse_map")
        .execute(&mut *transaction)
        .await?;
    for (pulse_count, amount_cents) in &pulse_map {
        sqlx::query("INSERT INTO coin_device_pulse_map (pulse_count, amount_cents) VALUES (?, ?)")
            .bind(pulse_count)
            .bind(amount_cents)
            .execute(&mut *transaction)
            .await?;
    }
    if calibration_audit.is_some_and(|(_, _, _, clears_candidate_gate)| clears_candidate_gate) {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('coin_calibration_required', 'false')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(&mut *transaction)
        .await?;
    }

    let mut event = NewAuditEvent::simple(
        "coin_device_settings_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated coin-device settings",
    );
    event.metadata = json!({
        "com_port": com_port,
        "baud_rate": input.baud_rate,
        "set_pin_line": input.set_pin_line,
        "coin_pin_line": input.coin_pin_line,
        "coin_pin_polarity": input.coin_pin_polarity,
        "inter_coin_gap_ms": input.inter_coin_gap_ms,
        "inactivity_timeout_seconds": inactivity_timeout_seconds,
        "hard_cap_seconds": hard_cap_seconds,
        "pulse_map_entries": pulse_map.len()
    });
    audit::append(&mut transaction, event).await?;
    if let Some((capture_id, pulse_count, amount_cents, _)) = calibration_audit {
        let mut calibration_event = NewAuditEvent::simple(
            "coin_calibration_applied",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Physical coin calibration mapping applied",
        );
        calibration_event.metadata = json!({
            "capture_id": capture_id,
            "pulse_count": pulse_count,
            "amount_cents": amount_cents,
        });
        audit::append(&mut transaction, calibration_event).await?;
    }
    transaction.commit().await?;

    coin_device_settings(pool).await
}

// ---------------------------------------------------------------------------
// Typed settings (key/value table)
// ---------------------------------------------------------------------------

/// Read a single settings row by key. Returns `None` when the key has never
/// been set, so callers can apply their own default. Migrations seed
/// `shop_name` and `simulator_enabled`; everything else is owner-edited.
pub async fn get_setting(pool: &SqlitePool, key: &str) -> AppResult<Option<String>> {
    if key.is_empty() {
        return Err(AppError::InvalidInput("setting key is required".to_owned()));
    }
    Ok(
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?,
    )
}

/// Insert-or-update a settings row. Uses SQLite's `ON CONFLICT(key) DO UPDATE`
/// so the call works for both first-write and later edits without a
/// separate read.
#[allow(dead_code)] // Wired into the HTTP layer in phase 2 (plan §10).
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    if key.is_empty() {
        return Err(AppError::InvalidInput("setting key is required".to_owned()));
    }
    if value.is_empty() {
        return Err(AppError::InvalidInput(
            "setting value must not be empty".to_owned(),
        ));
    }
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES (?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
    )
    .bind(key)
    .bind(value)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Typed read of the owner-configurable shop name (ADR-0027). Falls back
/// to the M1 seed when the row is missing so a fresh install still renders
/// the dashboard with a sensible default.
pub async fn shop_name(pool: &SqlitePool) -> AppResult<String> {
    Ok(get_setting(pool, "shop_name")
        .await?
        .unwrap_or_else(|| "OpenPiso Shop".to_owned()))
}

pub const DEFAULT_SHUTDOWN_GRACE_SECONDS: i64 = 300;
const MIN_SHUTDOWN_GRACE_SECONDS: i64 = 5;
const MAX_SHUTDOWN_GRACE_SECONDS: i64 = 300;
pub const DEFAULT_OFFLINE_GRACE_SECONDS: i64 = 600;

pub fn validate_shutdown_grace_seconds(seconds: i64) -> AppResult<i64> {
    if !(MIN_SHUTDOWN_GRACE_SECONDS..=MAX_SHUTDOWN_GRACE_SECONDS).contains(&seconds) {
        return Err(AppError::InvalidInput(format!(
            "shutdown grace must be between {MIN_SHUTDOWN_GRACE_SECONDS} and {MAX_SHUTDOWN_GRACE_SECONDS} seconds"
        )));
    }
    Ok(seconds)
}

pub(crate) fn apply_shutdown_grace(pcs: &mut [crate::models::Pc], shutdown_grace_seconds: i64) {
    for pc in pcs {
        pc.shutdown_grace_seconds = shutdown_grace_seconds;
    }
}

pub async fn shutdown_grace_seconds(pool: &SqlitePool) -> AppResult<i64> {
    let value = get_setting(pool, "shutdown_grace_seconds")
        .await?
        .unwrap_or_else(|| DEFAULT_SHUTDOWN_GRACE_SECONDS.to_string());
    let parsed = value
        .parse::<i64>()
        .unwrap_or(DEFAULT_SHUTDOWN_GRACE_SECONDS);
    validate_shutdown_grace_seconds(parsed)
}

pub async fn set_shutdown_grace_seconds(pool: &SqlitePool, seconds: i64) -> AppResult<i64> {
    let seconds = validate_shutdown_grace_seconds(seconds)?;
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES ('shutdown_grace_seconds', ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
    )
    .bind(seconds.to_string())
    .execute(&mut *transaction)
    .await?;
    let mut event = NewAuditEvent::simple(
        "shutdown_grace_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated shutdown grace period",
    );
    event.metadata = json!({ "shutdown_grace_seconds": seconds });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(seconds)
}

pub fn validate_offline_grace_seconds(seconds: i64) -> AppResult<i64> {
    if seconds <= 0 {
        return Err(AppError::InvalidInput(
            "offline grace must be a positive number of seconds".to_owned(),
        ));
    }
    Ok(seconds)
}

pub async fn offline_grace_seconds(pool: &SqlitePool) -> AppResult<i64> {
    let value = get_setting(pool, "offline_grace_seconds")
        .await?
        .unwrap_or_else(|| DEFAULT_OFFLINE_GRACE_SECONDS.to_string());
    let parsed = value
        .parse::<i64>()
        .unwrap_or(DEFAULT_OFFLINE_GRACE_SECONDS);
    validate_offline_grace_seconds(parsed)
}

pub async fn set_offline_grace_seconds(pool: &SqlitePool, seconds: i64) -> AppResult<i64> {
    let seconds = validate_offline_grace_seconds(seconds)?;
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES ('offline_grace_seconds', ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
    )
    .bind(seconds.to_string())
    .execute(&mut *transaction)
    .await?;
    let mut event = NewAuditEvent::simple(
        "offline_grace_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated Offline Grace duration",
    );
    event.metadata = json!({ "offline_grace_seconds": seconds });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(seconds)
}

/// Owner-only edit of the shop name. Validated at the Rust boundary:
/// non-empty after trimming, and a length cap so a malicious or accidental
/// paste cannot bloat the dashboard or the client Lock.
pub async fn set_shop_name(pool: &SqlitePool, name: &str) -> AppResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("shop name is required".to_owned()));
    }
    if trimmed.chars().count() > 80 {
        return Err(AppError::InvalidInput(
            "shop name must not exceed 80 characters".to_owned(),
        ));
    }
    let mut transaction = super::begin_immediate(pool).await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES ('shop_name', ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
    )
    .bind(trimmed)
    .execute(&mut *transaction)
    .await?;
    let mut event = NewAuditEvent::simple(
        "shop_name_updated",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Updated shop name",
    );
    event.metadata = json!({ "shop_name": trimmed });
    audit::append(&mut transaction, event).await?;
    transaction.commit().await?;
    Ok(trimmed.to_owned())
}

pub const DEFAULT_VOUCHER_TEMPLATE_HTML: &str = r#"<article class="voucher-card">
  <header><strong>{{shop_name}}</strong><span>{{prefix}}</span></header>
  <div class="voucher-code">{{code}}</div>
  <dl>
    <div><dt>Time</dt><dd>{{time}}</dd></div>
    <div><dt>Points</dt><dd>{{points}}</dd></div>
    <div><dt>Price</dt><dd>{{price}}</dd></div>
    <div><dt>Expires</dt><dd>{{expires_at}}</dd></div>
  </dl>
</article>"#;

pub const DEFAULT_VOUCHER_TEMPLATE_CSS: &str = r#"@page { size: A4 portrait; margin: 10mm; }
* { box-sizing: border-box; }
body { margin: 0; color: #111827; font-family: Arial, sans-serif; }
.voucher-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 6mm; }
.voucher-card { min-height: 58mm; padding: 6mm; border: 1px dashed #64748b; border-radius: 3mm; break-inside: avoid; page-break-inside: avoid; }
.voucher-card header { display: flex; justify-content: space-between; gap: 4mm; font-size: 10pt; text-transform: uppercase; }
.voucher-code { margin: 8mm 0; font: 700 18pt monospace; text-align: center; letter-spacing: 0.08em; }
.voucher-card dl { display: grid; grid-template-columns: repeat(2, 1fr); gap: 3mm 6mm; margin: 0; }
.voucher-card dl div { display: flex; justify-content: space-between; gap: 2mm; }
.voucher-card dt { color: #64748b; }
.voucher-card dd { margin: 0; font-weight: 700; }
@media print { .voucher-card { break-inside: avoid; page-break-inside: avoid; } }"#;

const MAX_VOUCHER_TEMPLATE_PART_BYTES: usize = 100_000;

pub async fn voucher_template(pool: &SqlitePool) -> AppResult<VoucherTemplate> {
    let html = get_setting(pool, "voucher_template_html")
        .await?
        .unwrap_or_else(|| DEFAULT_VOUCHER_TEMPLATE_HTML.to_owned());
    let css = get_setting(pool, "voucher_template_css")
        .await?
        .unwrap_or_else(|| DEFAULT_VOUCHER_TEMPLATE_CSS.to_owned());
    Ok(VoucherTemplate {
        html,
        css,
        default_html: DEFAULT_VOUCHER_TEMPLATE_HTML.to_owned(),
        default_css: DEFAULT_VOUCHER_TEMPLATE_CSS.to_owned(),
    })
}

fn validate_voucher_template_part(label: &str, value: &str) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::InvalidInput(format!(
            "voucher template {label} is required"
        )));
    }
    if value.len() > MAX_VOUCHER_TEMPLATE_PART_BYTES {
        return Err(AppError::InvalidInput(format!(
            "voucher template {label} must not exceed {MAX_VOUCHER_TEMPLATE_PART_BYTES} bytes"
        )));
    }
    Ok(())
}

pub async fn update_voucher_template(
    pool: &SqlitePool,
    input: &VoucherTemplateInput,
) -> AppResult<VoucherTemplate> {
    validate_voucher_template_part("HTML", &input.html)?;
    validate_voucher_template_part("CSS", &input.css)?;

    let mut transaction = super::begin_immediate(pool).await?;
    for (key, value) in [
        ("voucher_template_html", input.html.as_str()),
        ("voucher_template_css", input.css.as_str()),
    ] {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at)
             VALUES (?, ?, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE
               SET value = excluded.value,
                   updated_at = CURRENT_TIMESTAMP",
        )
        .bind(key)
        .bind(value)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    voucher_template(pool).await
}

/// Persist the owner PIN used by the Client-side gate. The PIN is hashed
/// with the same Argon2 path the account passwords use; the plaintext never
/// touches SQLite and is never returned through any API. The web-admin
/// login uses the separate `owner_credentials` row — this setting is the
/// **shared across all PCs** Client-side gate (ADR-0027).
pub async fn set_owner_pin(pool: &SqlitePool, pin: &str) -> AppResult<String> {
    let hashed = hash_owner_pin(pin)?;
    let mut transaction = super::begin_immediate(pool).await?;
    write_owner_pin(&mut transaction, &hashed).await?;
    transaction.commit().await?;
    Ok(hashed)
}

pub(crate) fn hash_owner_pin(pin: &str) -> AppResult<String> {
    let trimmed = pin.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("owner PIN is required".to_owned()));
    }
    if !(4..=128).contains(&trimmed.chars().count()) {
        return Err(AppError::InvalidInput(
            "owner PIN must be between 4 and 128 characters".to_owned(),
        ));
    }
    hash_password(trimmed)
}

pub(crate) async fn write_owner_pin(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    hashed: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES ('owner_pin', ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
    )
    .bind(hashed)
    .execute(&mut **transaction)
    .await?;
    let event = NewAuditEvent::simple(
        "owner_pin_changed",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Owner PIN changed",
    );
    audit::append(transaction, event).await?;
    Ok(())
}

/// Server-side comparison for the Client's `POST /api/owner/verify-pin`
/// request. Returns `false` when the owner has not configured a PIN yet so
/// the Client Lock treats the situation as "no owner gate" rather than a
/// hard failure. Failures and successes are audited (success without
/// metadata so the hash never lands in the log).
pub async fn verify_owner_pin(pool: &SqlitePool, pin: &str) -> AppResult<bool> {
    let stored_hash = match get_setting(pool, "owner_pin").await? {
        Some(hash) if !hash.is_empty() => hash,
        _ => {
            let mut event = NewAuditEvent::simple(
                "owner_pin_verify_unconfigured",
                AuditOutcome::Denied,
                AuditActor::Admin,
                "Owner PIN verification denied (no PIN configured)",
            );
            event.metadata = json!({ "reason": "unconfigured" });
            audit::append_standalone(pool, event).await?;
            return Ok(false);
        }
    };
    let matches = verify_password(pin, &stored_hash);
    let event = if matches {
        NewAuditEvent::simple(
            "owner_pin_verify_succeeded",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Owner PIN verified",
        )
    } else {
        NewAuditEvent::simple(
            "owner_pin_verify_failed",
            AuditOutcome::Denied,
            AuditActor::Admin,
            "Owner PIN verification denied",
        )
    };
    audit::append_standalone(pool, event).await?;
    Ok(matches)
}

/// Return the stored owner PIN hash so the Client Lock can cache it for
/// offline verification (ADR-0027 §3). `None` when the owner has not set a PIN
/// yet — the Client must then fall back to a strictly-online flow. The caller
/// (`POST /api/owner/verify-pin`) only returns this hash after a successful
/// verify, so it never reaches a caller that has not proven the PIN.
pub async fn get_owner_pin_hash(pool: &SqlitePool) -> AppResult<Option<String>> {
    Ok(get_setting(pool, "owner_pin")
        .await?
        .filter(|value| !value.is_empty()))
}

/// Sweep `points_transfer_requests` whose `created_at` is older than the
/// TTL the owner is willing to leave requests open. The default TTL is the
/// ADR-0030 ~10 minutes; callers can override. The sweep writes a
/// `points_transfer_expired` audit event per row and updates `status` to
/// `expired` and `resolved_at` to the current timestamp. Idempotent: a row
/// already `confirmed` / `rejected` / `expired` is left alone.
// The HTTP owner approvals list calls this on read so a past-TTL request never
// lingers as `pending`; a future background sweeper can call the same function.
pub async fn expire_pending_points_transfer_requests(
    pool: &SqlitePool,
    max_age_seconds: i64,
) -> AppResult<u64> {
    if max_age_seconds < 1 {
        return Err(AppError::InvalidInput(
            "transfer request TTL must be positive".to_owned(),
        ));
    }
    let mut transaction = super::begin_immediate(pool).await?;
    let candidates = sqlx::query_as::<_, (String, String)>(
        "SELECT id, audit_event_id
         FROM points_transfer_requests
         WHERE status = 'pending'
           AND datetime(created_at) <= datetime('now', ?)",
    )
    .bind(format!("-{max_age_seconds} seconds"))
    .fetch_all(&mut *transaction)
    .await?;
    let mut count = 0_u64;
    for (id, _audit_event_id) in candidates {
        let mut event = NewAuditEvent::simple(
            "points_transfer_expired",
            AuditOutcome::Success,
            AuditActor::System,
            "Points transfer request expired",
        );
        event.metadata = json!({ "request_id": id });
        audit::append(&mut transaction, event).await?;
        sqlx::query(
            "UPDATE points_transfer_requests
             SET status = 'expired', resolved_at = CURRENT_TIMESTAMP
             WHERE id = ? AND status = 'pending'",
        )
        .bind(&id)
        .execute(&mut *transaction)
        .await?;
        count += 1;
    }
    transaction.commit().await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use uuid::Uuid;

    use super::*;
    use crate::accounts::{self, AccountCreator};
    use crate::models::Account;

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

    #[allow(dead_code)]
    async fn file_test_pool() -> AppResult<SqlitePool> {
        let path = std::env::temp_dir().join(format!("openpiso-test-{}.db", Uuid::new_v4()));
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        super::super::run_migrations(&pool, crate::test_utils::migration_backup_dir().path())
            .await?;
        super::super::seed(&pool).await?;
        Ok(pool)
    }

    async fn test_account(pool: &SqlitePool, username: &str) -> AppResult<Account> {
        accounts::create(pool, username, "password", AccountCreator::Admin, None).await
    }

    #[tokio::test]
    async fn coin_device_settings_default_to_m1_constants() -> AppResult<()> {
        let pool = test_pool().await?;
        let settings = coin_device_settings(&pool).await?;
        assert!(settings.com_port.is_none());
        assert_eq!(settings.baud_rate, 9600);
        assert_eq!(settings.set_pin_line, "rts");
        assert_eq!(settings.coin_pin_line, "cd");
        assert_eq!(settings.coin_pin_polarity, "normally_open");
        assert_eq!(settings.inter_coin_gap_ms, 600);
        assert_eq!(settings.inactivity_timeout_seconds, 15);
        assert_eq!(settings.hard_cap_seconds, 180);
        assert_eq!(settings.pulse_map.len(), 4);

        // Runtime Coin Source configuration contains only the gap. The
        // persisted map is not live pricing authority.
        let config = super::super::coin_source_config(&pool).await?;
        assert_eq!(config.inter_coin_gap, Duration::from_millis(600));
        let mut alternate = settings.clone();
        alternate.pulse_map = vec![PulseMapEntry {
            pulse_count: 1,
            amount_cents: 777,
        }];
        assert_eq!(
            coin_source_config_from_settings(&settings)?,
            coin_source_config_from_settings(&alternate)?
        );
        assert!(settings.calibration_required);
        Ok(())
    }

    #[tokio::test]
    async fn coin_device_settings_round_trip_and_validate() -> AppResult<()> {
        let pool = test_pool().await?;
        sqlx::query("UPDATE settings SET value = 'false' WHERE key = 'coin_calibration_required'")
            .execute(&pool)
            .await?;
        let updated = update_coin_device_settings(
            &pool,
            &CoinDeviceSettingsInput {
                com_port: Some("COM3".to_owned()),
                baud_rate: 19_200,
                set_pin_line: "dtr".to_owned(),
                coin_pin_line: "cts".to_owned(),
                coin_pin_polarity: "normally_closed".to_owned(),
                inter_coin_gap_ms: 300,
                inactivity_timeout_seconds: Some(20),
                hard_cap_seconds: Some(240),
                pulse_map: vec![
                    PulseMapEntry {
                        pulse_count: 1,
                        amount_cents: 100,
                    },
                    PulseMapEntry {
                        pulse_count: 2,
                        amount_cents: 250,
                    },
                ],
            },
        )
        .await?;
        assert_eq!(updated.com_port.as_deref(), Some("COM3"));
        assert_eq!(updated.baud_rate, 19_200);
        assert_eq!(updated.set_pin_line, "dtr");
        assert_eq!(updated.coin_pin_polarity, "normally_closed");
        assert_eq!(updated.inter_coin_gap_ms, 300);
        assert_eq!(updated.inactivity_timeout_seconds, 20);
        assert_eq!(updated.hard_cap_seconds, 240);
        assert_eq!(updated.pulse_map.len(), 2);

        // The new gap flows into the runtime config; map edits do not.
        let config = super::super::coin_source_config(&pool).await?;
        assert_eq!(config.inter_coin_gap, Duration::from_millis(300));

        // One audit event records the change (ADR-0015).
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'coin_device_settings_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);

        // An empty pulse map and bad line are rejected; nothing is written.
        assert!(matches!(
            update_coin_device_settings(
                &pool,
                &CoinDeviceSettingsInput {
                    com_port: None,
                    baud_rate: 9600,
                    set_pin_line: "rts".to_owned(),
                    coin_pin_line: "cd".to_owned(),
                    coin_pin_polarity: "normally_open".to_owned(),
                    inter_coin_gap_ms: 200,
                    inactivity_timeout_seconds: None,
                    hard_cap_seconds: None,
                    pulse_map: vec![],
                },
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            update_coin_device_settings(
                &pool,
                &CoinDeviceSettingsInput {
                    com_port: None,
                    baud_rate: 9600,
                    set_pin_line: "bogus".to_owned(),
                    coin_pin_line: "cd".to_owned(),
                    coin_pin_polarity: "normally_open".to_owned(),
                    inter_coin_gap_ms: 200,
                    inactivity_timeout_seconds: None,
                    hard_cap_seconds: None,
                    pulse_map: vec![PulseMapEntry {
                        pulse_count: 1,
                        amount_cents: 100,
                    }],
                },
            )
            .await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            update_coin_device_settings(
                &pool,
                &CoinDeviceSettingsInput {
                    com_port: None,
                    baud_rate: 9600,
                    set_pin_line: "rts".to_owned(),
                    coin_pin_line: "cd".to_owned(),
                    coin_pin_polarity: "normally_open".to_owned(),
                    inter_coin_gap_ms: 200,
                    inactivity_timeout_seconds: None,
                    hard_cap_seconds: None,
                    pulse_map: vec![
                        PulseMapEntry {
                            pulse_count: 20,
                            amount_cents: 2000,
                        },
                        PulseMapEntry {
                            pulse_count: 20,
                            amount_cents: 1000,
                        },
                    ],
                },
            )
            .await,
            Err(AppError::InvalidInput(error)) if error.contains("duplicate pulse count")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn shop_name_setting_round_trips_through_typed_helpers() -> AppResult<()> {
        let pool = test_pool().await?;
        // Seed installs the M1 default.
        assert_eq!(shop_name(&pool).await?, "OpenPiso Shop");
        let persisted = set_shop_name(&pool, "  My Shop  ").await?;
        assert_eq!(persisted, "My Shop");
        assert_eq!(shop_name(&pool).await?, "My Shop");
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_name_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_grace_setting_round_trips_and_validates() -> AppResult<()> {
        let pool = test_pool().await?;

        assert_eq!(shutdown_grace_seconds(&pool).await?, 300);
        assert_eq!(set_shutdown_grace_seconds(&pool, 45).await?, 45);
        assert_eq!(shutdown_grace_seconds(&pool).await?, 45);
        assert!(matches!(
            set_shutdown_grace_seconds(&pool, 4).await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            set_shutdown_grace_seconds(&pool, 301).await,
            Err(AppError::InvalidInput(_))
        ));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shutdown_grace_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn offline_grace_setting_round_trips_and_requires_positive_seconds() -> AppResult<()> {
        let pool = test_pool().await?;

        assert_eq!(offline_grace_seconds(&pool).await?, 600);
        assert_eq!(set_offline_grace_seconds(&pool, 240).await?, 240);
        assert_eq!(offline_grace_seconds(&pool).await?, 240);
        assert!(matches!(
            set_offline_grace_seconds(&pool, 0).await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            set_offline_grace_seconds(&pool, -1).await,
            Err(AppError::InvalidInput(_))
        ));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'offline_grace_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn set_shop_name_rejects_empty_or_oversized_values() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            set_shop_name(&pool, "   ").await,
            Err(AppError::InvalidInput(_))
        ));
        let oversized = "x".repeat(81);
        assert!(matches!(
            set_shop_name(&pool, &oversized).await,
            Err(AppError::InvalidInput(_))
        ));
        // No audit row should have been written for either failure.
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'shop_name_updated'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 0);
        Ok(())
    }

    #[tokio::test]
    async fn voucher_template_defaults_are_seeded_and_updates_are_atomic() -> AppResult<()> {
        let pool = test_pool().await?;
        let seeded = voucher_template(&pool).await?;
        assert_eq!(seeded.html, DEFAULT_VOUCHER_TEMPLATE_HTML);
        assert_eq!(seeded.css, DEFAULT_VOUCHER_TEMPLATE_CSS);
        assert!(seeded.css.contains("@page"));
        assert!(seeded.css.contains(".voucher-grid"));

        let updated = update_voucher_template(
            &pool,
            &VoucherTemplateInput {
                html: "<article>{{code}}</article>".to_owned(),
                css: "@page { margin: 5mm; }".to_owned(),
            },
        )
        .await?;
        assert_eq!(updated.html, "<article>{{code}}</article>");
        assert_eq!(updated.css, "@page { margin: 5mm; }");
        assert_eq!(updated.default_html, DEFAULT_VOUCHER_TEMPLATE_HTML);
        assert_eq!(updated.default_css, DEFAULT_VOUCHER_TEMPLATE_CSS);
        Ok(())
    }

    #[tokio::test]
    async fn voucher_template_rejects_blank_or_oversized_parts() -> AppResult<()> {
        let pool = test_pool().await?;
        for input in [
            VoucherTemplateInput {
                html: " ".to_owned(),
                css: "valid".to_owned(),
            },
            VoucherTemplateInput {
                html: "valid".to_owned(),
                css: " ".to_owned(),
            },
            VoucherTemplateInput {
                html: "x".repeat(MAX_VOUCHER_TEMPLATE_PART_BYTES + 1),
                css: "valid".to_owned(),
            },
        ] {
            assert!(matches!(
                update_voucher_template(&pool, &input).await,
                Err(AppError::InvalidInput(_))
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn owner_pin_round_trips_through_hashed_helpers_and_never_exposes_plaintext(
    ) -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(get_owner_pin_hash(&pool).await?.is_none());
        let hashed = set_owner_pin(&pool, "owner-pin").await?;
        assert!(!hashed.is_empty());
        assert_ne!(hashed, "owner-pin");
        let stored = get_owner_pin_hash(&pool)
            .await?
            .expect("hash should be persisted");
        assert_eq!(stored, hashed);
        // The plaintext never appears in settings; the row holds only the hash.
        let raw: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'owner_pin'")
            .fetch_one(&pool)
            .await?;
        assert!(!raw.contains("owner-pin"));
        // Verify accepts the right PIN and rejects the wrong one.
        assert!(verify_owner_pin(&pool, "owner-pin").await?);
        assert!(!verify_owner_pin(&pool, "wrong-pin").await?);
        let change_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_pin_changed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(change_audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn verify_owner_pin_denies_when_unconfigured() -> AppResult<()> {
        let pool = test_pool().await?;
        // A fresh install has no owner_pin row; verification must report
        // false (not crash) and audit the denial so the trail is honest.
        assert!(!verify_owner_pin(&pool, "anything").await?);
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_pin_verify_unconfigured'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 1);
        Ok(())
    }

    #[tokio::test]
    async fn set_owner_pin_rejects_out_of_range_values() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            set_owner_pin(&pool, "").await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            set_owner_pin(&pool, "ab").await,
            Err(AppError::InvalidInput(_))
        ));
        let too_long = "x".repeat(129);
        assert!(matches!(
            set_owner_pin(&pool, &too_long).await,
            Err(AppError::InvalidInput(_))
        ));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'owner_pin_changed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audits, 0);
        Ok(())
    }

    #[tokio::test]
    async fn get_setting_round_trips_through_set_setting() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(get_setting(&pool, "owner_pin").await?.is_none());
        set_setting(&pool, "owner_pin", "stub-hash").await?;
        assert_eq!(
            get_setting(&pool, "owner_pin").await?,
            Some("stub-hash".to_owned())
        );
        set_setting(&pool, "owner_pin", "rotated-hash").await?;
        assert_eq!(
            get_setting(&pool, "owner_pin").await?,
            Some("rotated-hash".to_owned())
        );
        assert!(matches!(
            get_setting(&pool, "").await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            set_setting(&pool, "owner_pin", "").await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn points_transfer_request_sweep_marks_old_pending_rows_expired() -> AppResult<()> {
        let pool = test_pool().await?;
        let sender = test_account(&pool, "sweep-sender").await?;
        let recipient = test_account(&pool, "sweep-recipient").await?;

        // Seed one pending request at the given age so the sweep can be
        // exercised end-to-end without coupling to the production request
        // path (which lives in `accounts.rs`).
        async fn seed_pending(
            pool: &SqlitePool,
            sender: &str,
            recipient: &str,
            age_seconds: i64,
            code: &str,
        ) -> AppResult<()> {
            let event_id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO audit_log
                   (id, event_type, outcome, actor, summary, metadata_json)
                 VALUES (?, 'points_transfer_requested', 'success', 'account', 'Sweep seed', '{}')",
            )
            .bind(&event_id)
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO points_transfer_requests
                   (id, from_account_id, to_account_id, points_centi, code, status,
                    created_at, audit_event_id)
                 VALUES (?, ?, ?, 5, ?, 'pending', datetime('now', ?), ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(sender)
            .bind(recipient)
            .bind(code)
            .bind(format!("-{age_seconds} seconds"))
            .bind(&event_id)
            .execute(pool)
            .await?;
            Ok(())
        }

        // Two old pending rows, one fresh pending row. The sweep must
        // expire only the old ones.
        seed_pending(&pool, &sender.id, &recipient.id, 120, "000001").await?;
        seed_pending(&pool, &sender.id, &recipient.id, 30, "000002").await?;
        seed_pending(&pool, &sender.id, &recipient.id, 120, "000003").await?;
        // A seed row already resolved must be left alone.
        let resolved_id = Uuid::new_v4().to_string();
        let resolved_event = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO audit_log
               (id, event_type, outcome, actor, summary, metadata_json)
             VALUES (?, 'points_transfer_requested', 'success', 'account', 'Resolved seed', '{}')",
        )
        .bind(&resolved_event)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO points_transfer_requests
               (id, from_account_id, to_account_id, points_centi, code, status,
                created_at, resolved_at, audit_event_id)
             VALUES (?, ?, ?, 5, '999999', 'confirmed', datetime('now', '-120 seconds'),
                     datetime('now', '-30 seconds'), ?)",
        )
        .bind(&resolved_id)
        .bind(&sender.id)
        .bind(&recipient.id)
        .bind(&resolved_event)
        .execute(&pool)
        .await?;

        let expired = expire_pending_points_transfer_requests(&pool, 60).await?;
        assert_eq!(expired, 2);

        let confirmed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM points_transfer_requests WHERE status = 'confirmed'",
        )
        .fetch_one(&pool)
        .await?;
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM points_transfer_requests WHERE status = 'pending'",
        )
        .fetch_one(&pool)
        .await?;
        let expired_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM points_transfer_requests WHERE status = 'expired'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(confirmed, 1);
        assert_eq!(pending, 1);
        assert_eq!(expired_count, 2);
        // The sweep is idempotent: a second pass must not touch the
        // already-expired rows.
        assert_eq!(expire_pending_points_transfer_requests(&pool, 60).await?, 0);
        let sweep_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type = 'points_transfer_expired'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(sweep_events, 2);
        Ok(())
    }

    #[tokio::test]
    async fn points_transfer_request_sweep_rejects_non_positive_ttl() -> AppResult<()> {
        let pool = test_pool().await?;
        assert!(matches!(
            expire_pending_points_transfer_requests(&pool, 0).await,
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            expire_pending_points_transfer_requests(&pool, -1).await,
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }
}

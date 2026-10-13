use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use openpiso_core::{
    account::normalize_username_lenient,
    audit::{AuditActor, AuditOutcome},
};
use serde::Serialize;
use serde_json::{json, Number, Value};
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    db,
    errors::{AppError, AppResult},
};

const SOURCE_SYSTEM: &str = "martec";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccountImportOutcome {
    Created,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccountImportSkipReason {
    AlreadyImported,
    ExistingAccountConflict,
    DisabledAtSource,
    InvalidUsername,
    InvalidPasswordHash,
    DuplicateUsername,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AccountImportRow {
    pub outcome: AccountImportOutcome,
    pub original_username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_username: Option<String>,
    pub remaining_seconds: i64,
    pub points_balance_centi: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<AccountImportSkipReason>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AccountImportTotals {
    pub created: usize,
    pub skipped: usize,
    pub total_seconds: i64,
    pub total_points_centi: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AccountImportReport {
    pub rows: Vec<AccountImportRow>,
    pub totals: AccountImportTotals,
}

struct SourceRow {
    source_id: String,
    password_hash: String,
    enabled: bool,
    last_seen: Option<String>,
    remaining_seconds: i64,
    points_balance_centi: i64,
}

/// Validate and preview a Martec account export without changing SQLite.
pub(crate) async fn preview(pool: &SqlitePool, request: Value) -> AppResult<AccountImportReport> {
    let (dry_run, source_rows) = parse_request(request)?;
    if !dry_run {
        return Err(invalid_payload("dry_run must be true for a preview"));
    }
    let (existing_usernames, already_imported_usernames) = existing_usernames(pool).await?;
    build_report(
        &source_rows,
        &existing_usernames,
        &already_imported_usernames,
    )
}

/// Commit a Martec account export. Planning and all account/audit writes share
/// one BEGIN IMMEDIATE transaction, so the preview and commit use the same
/// classification logic and an unexpected database error rolls back the run.
pub(crate) async fn commit(pool: &SqlitePool, request: Value) -> AppResult<AccountImportReport> {
    let (dry_run, source_rows) = parse_request(request)?;
    if dry_run {
        return Err(invalid_payload("dry_run must be false for a commit"));
    }

    let mut transaction = db::begin_immediate(pool).await?;
    let (existing_usernames, already_imported_usernames) =
        existing_usernames_in_transaction(&mut transaction).await?;
    let report = build_report(
        &source_rows,
        &existing_usernames,
        &already_imported_usernames,
    )?;
    let imported_at = Utc::now().to_rfc3339();

    for (report_row, (original_username, source)) in report.rows.iter().zip(source_rows.iter()) {
        if report_row.outcome != AccountImportOutcome::Created {
            continue;
        }
        let (final_username, username_normalized) =
            normalize_username_lenient(original_username)
                .map_err(|error| AppError::InvalidInput(error.to_string()))?;
        let password_hash = bcrypt_hash(&source.password_hash)
            .ok_or_else(|| invalid_payload("valid report row has an invalid bcrypt hash"))?;
        let account_id = Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO accounts (
               id, username, username_normalized, password_hash,
               remaining_seconds, points_balance_centi, imported_from, imported_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&account_id)
        .bind(&final_username)
        .bind(&username_normalized)
        .bind(password_hash)
        .bind(source.remaining_seconds)
        .bind(source.points_balance_centi)
        .bind(SOURCE_SYSTEM)
        .bind(&imported_at)
        .execute(&mut *transaction)
        .await?;

        let mut event = NewAuditEvent::simple(
            "account_created",
            AuditOutcome::Success,
            AuditActor::Admin,
            "Imported legacy customer account",
        );
        event.account_id = Some(&account_id);
        event.seconds_delta = Some(source.remaining_seconds);
        event.points_delta_centi = Some(source.points_balance_centi);
        event.cash_flag = false;
        event.metadata =
            import_account_metadata(original_username, &final_username, source, &imported_at);
        audit::append(&mut transaction, event).await?;
    }

    let mut summary = NewAuditEvent::simple(
        "account_import_completed",
        AuditOutcome::Success,
        AuditActor::Admin,
        "Completed Martec account import",
    );
    summary.cash_flag = false;
    summary.metadata = json!({
        "source_system": SOURCE_SYSTEM,
        "created_count": report.totals.created,
        "skipped_count": report.totals.skipped,
        "total_seconds": report.totals.total_seconds,
        "total_points_centi": report.totals.total_points_centi,
        "imported_at": imported_at,
    });
    audit::append(&mut transaction, summary).await?;

    transaction.commit().await?;
    Ok(report)
}

fn import_account_metadata(
    original_username: &str,
    final_username: &str,
    source: &SourceRow,
    imported_at: &str,
) -> Value {
    let mut metadata = json!({
        "source_system": SOURCE_SYSTEM,
        "martec_account_id": source.source_id,
        "martec_last_seen": source.last_seen,
        "imported_at": imported_at,
    });
    if final_username != original_username {
        metadata["original_username"] = Value::String(original_username.to_owned());
    }
    metadata
}

fn parse_request(request: Value) -> AppResult<(bool, BTreeMap<String, SourceRow>)> {
    let object = request
        .as_object()
        .ok_or_else(|| invalid_payload("request must be a JSON object"))?;
    let dry_run = object
        .get("dry_run")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid_payload("dry_run must be a boolean"))?;

    let payload = match (object.get("accounts"), object.get("payload")) {
        (Some(_), Some(_)) => {
            return Err(invalid_payload(
                "provide one Martec payload in accounts or payload",
            ));
        }
        (Some(payload), None) | (None, Some(payload)) => payload,
        (None, None) => {
            // Also accept the export object directly beside dry_run. This
            // keeps the HTTP seam compatible with callers that add the flag
            // to the original AccountsData.json object.
            let mut direct_payload = object.clone();
            direct_payload.remove("dry_run");
            if direct_payload.is_empty() {
                return Err(invalid_payload("Martec payload is required"));
            }
            return Ok((dry_run, parse_payload(&Value::Object(direct_payload))?));
        }
    };

    Ok((dry_run, parse_payload(payload)?))
}

fn parse_payload(payload: &Value) -> AppResult<BTreeMap<String, SourceRow>> {
    let object = payload
        .as_object()
        .ok_or_else(|| invalid_payload("Martec payload must be a JSON object"))?;
    if object.is_empty() {
        return Err(invalid_payload("Martec payload must contain account rows"));
    }

    object
        .iter()
        .map(|(username, row)| parse_row(row).map(|row| (username.clone(), row)))
        .collect()
}

fn parse_row(row: &Value) -> AppResult<SourceRow> {
    let object = row
        .as_object()
        .ok_or_else(|| invalid_payload("each Martec account row must be an object"))?;
    let source_id = object
        .get("id")
        .ok_or_else(|| invalid_payload("each Martec account row needs id"))?;
    let source_id = if let Some(value) = source_id.as_str() {
        value.to_owned()
    } else if let Some(value) = source_id.as_number() {
        value.to_string()
    } else {
        return Err(invalid_payload(
            "Martec account id must be a string or number",
        ));
    };

    // Password material is row-level validation: a missing/non-string value
    // is reported as an invalid bcrypt hash rather than aborting every other
    // row in an otherwise recognizable Martec export.
    let password_hash = object
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let credits = object
        .get("credits")
        .and_then(Value::as_number)
        .ok_or_else(|| invalid_payload("each Martec account row needs numeric credits"))?;
    let points = object
        .get("points")
        .and_then(Value::as_number)
        .ok_or_else(|| invalid_payload("each Martec account row needs numeric points"))?;
    let enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid_payload("each Martec account row needs boolean enabled"))?;
    let last_seen = object
        .get("last")
        .ok_or_else(|| invalid_payload("each Martec account row needs last"))?;
    let last_seen = if last_seen.is_null() {
        None
    } else if let Some(value) = last_seen.as_str() {
        Some(value.to_owned())
    } else {
        return Err(invalid_payload(
            "Martec account last must be a string or null",
        ));
    };

    Ok(SourceRow {
        source_id,
        password_hash,
        enabled,
        last_seen,
        remaining_seconds: round_scaled(credits, 60)?,
        points_balance_centi: round_scaled(points, 100)?,
    })
}

async fn existing_usernames(pool: &SqlitePool) -> AppResult<(BTreeSet<String>, BTreeSet<String>)> {
    let active = sqlx::query_scalar::<_, String>(
        "SELECT username_normalized FROM accounts WHERE status = 'active'",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let already_imported = sqlx::query_scalar::<_, String>(
        "SELECT username_normalized
         FROM accounts
         WHERE status = 'active' AND imported_from = ?",
    )
    .bind(SOURCE_SYSTEM)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    Ok((active, already_imported))
}

async fn existing_usernames_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
) -> AppResult<(BTreeSet<String>, BTreeSet<String>)> {
    let active = sqlx::query_scalar::<_, String>(
        "SELECT username_normalized FROM accounts WHERE status = 'active'",
    )
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let already_imported = sqlx::query_scalar::<_, String>(
        "SELECT username_normalized
         FROM accounts
         WHERE status = 'active' AND imported_from = ?",
    )
    .bind(SOURCE_SYSTEM)
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    Ok((active, already_imported))
}

fn build_report(
    source_rows: &BTreeMap<String, SourceRow>,
    existing_usernames: &BTreeSet<String>,
    already_imported_usernames: &BTreeSet<String>,
) -> AppResult<AccountImportReport> {
    let mut rows = Vec::with_capacity(source_rows.len());
    let mut seen_usernames = BTreeSet::new();
    let mut totals = AccountImportTotals {
        created: 0,
        skipped: 0,
        total_seconds: 0,
        total_points_centi: 0,
    };

    for (original_username, source) in source_rows {
        let normalized = normalize_username_lenient(original_username).ok();
        let final_username = normalized
            .as_ref()
            .filter(|(display, _)| *display != *original_username)
            .map(|(display, _)| display.clone());

        let reason = match normalized.as_ref() {
            None => Some(AccountImportSkipReason::InvalidUsername),
            Some((_, username_normalized))
                if already_imported_usernames.contains(username_normalized) =>
            {
                Some(AccountImportSkipReason::AlreadyImported)
            }
            Some((_, username_normalized)) if existing_usernames.contains(username_normalized) => {
                Some(AccountImportSkipReason::ExistingAccountConflict)
            }
            Some((_, username_normalized)) if !source.enabled => {
                Some(AccountImportSkipReason::DisabledAtSource)
            }
            Some(_) if bcrypt_hash(&source.password_hash).is_none() => {
                Some(AccountImportSkipReason::InvalidPasswordHash)
            }
            Some((_, username_normalized))
                if !seen_usernames.insert(username_normalized.clone()) =>
            {
                Some(AccountImportSkipReason::DuplicateUsername)
            }
            Some(_) => None,
        };

        let (outcome, skip_reason) = match reason {
            Some(reason) => {
                totals.skipped += 1;
                (AccountImportOutcome::Skipped, Some(reason))
            }
            None => {
                totals.created += 1;
                totals.total_seconds =
                    totals
                        .total_seconds
                        .checked_add(source.remaining_seconds)
                        .ok_or_else(|| invalid_payload("imported seconds total is too large"))?;
                totals.total_points_centi = totals
                    .total_points_centi
                    .checked_add(source.points_balance_centi)
                    .ok_or_else(|| invalid_payload("imported points total is too large"))?;
                (AccountImportOutcome::Created, None)
            }
        };

        rows.push(AccountImportRow {
            outcome,
            original_username: original_username.clone(),
            final_username,
            remaining_seconds: source.remaining_seconds,
            points_balance_centi: source.points_balance_centi,
            skip_reason,
        });
    }

    Ok(AccountImportReport { rows, totals })
}

fn bcrypt_hash(value: &str) -> Option<&str> {
    let hash = value
        .strip_prefix("b'")
        .and_then(|wrapped| wrapped.strip_suffix('\''))
        .unwrap_or(value);
    let bytes = hash.as_bytes();
    if bytes.len() != 60
        || !(hash.starts_with("$2a$") || hash.starts_with("$2b$") || hash.starts_with("$2y$"))
        || bytes[6] != b'$'
        || !bytes[4..6].iter().all(u8::is_ascii_digit)
    {
        return None;
    }

    let cost = (bytes[4] - b'0') * 10 + (bytes[5] - b'0');
    if !(4..=31).contains(&cost) {
        return None;
    }
    if bytes[7..].iter().all(|byte| {
        b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".contains(byte)
    }) {
        Some(hash)
    } else {
        None
    }
}

/// Round a JSON decimal number after scaling it by an integer, without using
/// floating point. This keeps the Rust/database boundary integer-only and
/// makes values such as `7.9e-10` deterministically round to zero.
fn round_scaled(number: &Number, multiplier: u128) -> AppResult<i64> {
    let raw = number.to_string();
    let (negative, unsigned) = match raw.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, raw.as_str()),
    };
    if negative {
        return Ok(0);
    }
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (
            mantissa,
            exponent
                .parse::<i32>()
                .map_err(|_| invalid_payload("numeric exponent is invalid"))?,
        ),
        None => (unsigned, 0),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (mantissa, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_payload("numeric value is invalid"));
    }
    let digits = format!("{whole}{fraction}");
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Ok(0);
    }
    let numerator = significant
        .parse::<u128>()
        .map_err(|_| invalid_payload("numeric value is too large"))?
        .checked_mul(multiplier)
        .ok_or_else(|| invalid_payload("numeric value is too large"))?;
    let scale = i64::from(fraction.len() as u32) - i64::from(exponent);

    let rounded = if scale <= 0 {
        let power = ten_to_the_power(
            u32::try_from(-scale).map_err(|_| invalid_payload("numeric value is too large"))?,
        )?;
        numerator
            .checked_mul(power)
            .ok_or_else(|| invalid_payload("numeric value is too large"))?
    } else if scale > 38 {
        // Any larger divisor is greater than twice the largest representable
        // numerator here, so it necessarily rounds to zero.
        0
    } else {
        let divisor = ten_to_the_power(
            u32::try_from(scale).map_err(|_| invalid_payload("numeric value is too large"))?,
        )?;
        let quotient = numerator / divisor;
        let remainder = numerator % divisor;
        quotient
            .checked_add(u128::from(remainder >= divisor - remainder))
            .ok_or_else(|| invalid_payload("numeric value is too large"))?
    };

    i64::try_from(rounded).map_err(|_| invalid_payload("converted balance is too large"))
}

fn ten_to_the_power(power: u32) -> AppResult<u128> {
    10_u128
        .checked_pow(power)
        .ok_or_else(|| invalid_payload("numeric value is too large"))
}

fn invalid_payload(message: &str) -> AppError {
    AppError::InvalidInput(format!("invalid Martec account import: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_decimal_values_without_floating_point() {
        let dust = Number::from_f64(7.9e-10).expect("valid JSON number");
        let half = Number::from_f64(1.005).expect("valid JSON number");
        assert_eq!(round_scaled(&dust, 60).expect("dust conversion"), 0);
        assert_eq!(round_scaled(&half, 100).expect("point conversion"), 101);
    }

    #[test]
    fn validates_wrapped_and_unwrapped_bcrypt_hashes() {
        let hash = "$2b$12$.....................................................";
        assert_eq!(bcrypt_hash(hash), Some(hash));
        assert_eq!(bcrypt_hash(&format!("b'{hash}'")), Some(hash));
        assert_eq!(bcrypt_hash("not-a-bcrypt-hash"), None);
    }
}

//! Cash Collection (ADR-0091): the owner records emptying the coin box or
//! taking the counter's voucher cash. It is custody only; it never moves a
//! sales or revenue figure.
//!
//! Uncollected(kind) = all-time sales of the kind minus the sum of that kind's
//! collections. There is deliberately no timestamp cut-off: audit timestamps
//! are second-precision, so a coin can share a second with a collection.

use openpiso_core::{
    audit::{AuditActor, AuditOutcome},
    report::CashCollectionKind,
};
use serde::Serialize;
use serde_json::json;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::{
    audit::{self, NewAuditEvent},
    errors::{AppError, AppResult},
    reports,
};

#[derive(Debug, Clone, Serialize)]
pub struct CashCollection {
    pub kind: CashCollectionKind,
    pub amount_cents: i64,
    pub collected_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashOnHandKind {
    pub uncollected_cents: i64,
    /// `None` until the first collection of this kind.
    pub last_collection: Option<LastCollection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastCollection {
    pub amount_cents: i64,
    pub collected_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashOnHand {
    pub coin_box: CashOnHandKind,
    pub voucher_cash: CashOnHandKind,
}

async fn uncollected_cents(
    conn: &mut SqliteConnection,
    kind: CashCollectionKind,
) -> AppResult<i64> {
    let sales = reports::sales_total_cents(&mut *conn, kind.sales_kind()).await?;
    let collected: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_cents), 0) FROM cash_collections WHERE kind = ?",
    )
    .bind(kind.as_str())
    .fetch_one(&mut *conn)
    .await?;
    Ok(sales - collected)
}

async fn cash_on_hand_kind(
    conn: &mut SqliteConnection,
    kind: CashCollectionKind,
) -> AppResult<CashOnHandKind> {
    let last_collection = sqlx::query_as::<_, (i64, String)>(
        "SELECT amount_cents, collected_at FROM cash_collections
         WHERE kind = ? ORDER BY collected_at DESC, rowid DESC LIMIT 1",
    )
    .bind(kind.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .map(|(amount_cents, collected_at)| LastCollection {
        amount_cents,
        collected_at,
    });
    Ok(CashOnHandKind {
        uncollected_cents: uncollected_cents(conn, kind).await?.max(0),
        last_collection,
    })
}

/// The Cash on hand panel's read. Independent of any report range.
pub async fn cash_on_hand(pool: &SqlitePool) -> AppResult<CashOnHand> {
    // One read transaction: both kinds' sales and collections share a snapshot.
    let mut tx = pool.begin().await?;
    let coin_box = cash_on_hand_kind(&mut tx, CashCollectionKind::CoinBox).await?;
    let voucher_cash = cash_on_hand_kind(&mut tx, CashCollectionKind::VoucherCash).await?;
    tx.commit().await?;
    Ok(CashOnHand {
        coin_box,
        voucher_cash,
    })
}

/// Records everything of `kind` not yet collected. The amount is computed here,
/// inside the same `BEGIN IMMEDIATE` transaction that inserts the row, so two
/// racing calls cannot both record the same cash. Refused at ₱0.
pub async fn collect_cash(
    pool: &SqlitePool,
    kind: CashCollectionKind,
) -> AppResult<CashCollection> {
    let mut tx = super::begin_immediate(pool).await?;
    let amount_cents = uncollected_cents(&mut tx, kind).await?;
    if amount_cents <= 0 {
        return Err(AppError::Conflict(
            "there is no uncollected cash to collect".to_owned(),
        ));
    }
    let collected_at: String = sqlx::query_scalar(
        "INSERT INTO cash_collections (id, kind, amount_cents) VALUES (?, ?, ?)
         RETURNING collected_at",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(kind.as_str())
    .bind(amount_cents)
    .fetch_one(&mut *tx)
    .await?;
    // Non-cash on purpose: the collection moves custody, not revenue.
    audit::append(
        &mut tx,
        NewAuditEvent {
            amount_cents: Some(amount_cents),
            summary: format!("Collected {} cash", kind.as_str()),
            metadata: json!({ "kind": kind.as_str() }),
            ..NewAuditEvent::simple(
                "cash_collected",
                AuditOutcome::Success,
                AuditActor::Admin,
                "",
            )
        },
    )
    .await?;
    tx.commit().await?;
    Ok(CashCollection {
        kind,
        amount_cents,
        collected_at,
    })
}

//! Telegram delivery worker (ADR-0076 / GitHub #346): sends committed
//! `telegram_outbox` items to the connected private owner chat.
//!
//! The worker is deliberately dumb. Every message's redacted text was already
//! composed at enqueue time inside the same SQLite transaction as its
//! incident row change and Audit event, so this module never reads incident,
//! PC-status, session, money, time, or Points data and can never acknowledge
//! or close a local Alert — it only writes delivery state back to the outbox.
//!
//! Guarantees and accepted trade-offs:
//! - Local history stays canonical: items are sent only after their
//!   transaction committed, so a Telegram failure loses no evidence.
//! - The queue survives Admin restart; the loop runs on its own task on a
//!   ten-second cadence, outside the one-second money and time authority
//!   timer.
//! - Same-window items of one class batch into a single `sendMessage`, so a
//!   fleet-wide failure arrives as one message while local incidents remain
//!   separate (ADR-0076).
//! - Failures retry with bounded exponential backoff (30 s doubling to a
//!   one-hour ceiling) for at most 24 hours per item, then abandon unsent.
//! - Telegram offers no delivery idempotency here: an ambiguous network
//!   outcome (crash between send and write-back) may duplicate a message but
//!   cannot duplicate incidents, Audit events, money, time, Points, or
//!   sessions.
//! - Finished rows older than 30 days are pruned, mirroring Notifications.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tracing::error;

use crate::{
    db,
    errors::{AppError, AppResult},
    telegram::{TelegramTransport, TokenProtector},
};

/// Fixed v1 delivery cadence: one sweep every ten seconds on its own task,
/// matching the monitoring loop and never inside the money/time timer.
pub const DELIVERY_INTERVAL_SECS: u64 = 10;

/// Telegram rejects messages over 4096 characters. Keep a lower byte ceiling
/// so a large fleet is split into bounded messages without splitting a PC line.
const MAX_BATCH_MESSAGE_BYTES: usize = 3_500;

/// Clock seam so backoff, batching windows, and the 24-hour retry window are
/// deterministic in tests.
pub trait DeliveryClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock.
pub struct SystemClock;

impl DeliveryClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    item_class: String,
    incident_kind: Option<String>,
}

pub struct DeliveryService {
    pool: SqlitePool,
    transport: Arc<dyn TelegramTransport>,
    protector: Arc<dyn TokenProtector>,
    clock: Arc<dyn DeliveryClock>,
}

impl DeliveryService {
    pub fn new(
        pool: SqlitePool,
        transport: Arc<dyn TelegramTransport>,
        protector: Arc<dyn TokenProtector>,
        clock: Arc<dyn DeliveryClock>,
    ) -> Self {
        Self {
            pool,
            transport,
            protector,
            clock,
        }
    }

    /// Spawn the production loop on its own task.
    pub fn spawn(self) {
        tauri::async_runtime::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(DELIVERY_INTERVAL_SECS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = self.tick().await {
                    error!(
                        target: "openpiso_admin_lib::telegram_delivery",
                        "Telegram delivery tick failed: {error}"
                    );
                }
            }
        });
    }

    /// One delivery sweep. Public so tests drive it against an injected
    /// clock with no real time passing.
    pub async fn tick(&self) -> AppResult<()> {
        let now = self.clock.now();
        // Expiry is checked independently of the retry schedule. If Admin was
        // stopped for longer than the retry window, the next tick must not
        // send stale queued work simply because it was never attempted.
        db::telegram::abandon_expired(&self.pool, now).await?;
        db::telegram::prune_finished(&self.pool, now).await?;
        // Disconnected: nothing to deliver and nothing pending (disconnect
        // deleted the queue), so the sweep is a cheap no-op.
        let Some(config) = db::telegram::config(&self.pool).await? else {
            return Ok(());
        };
        let token = self.token(&config.token_protected)?;

        // Group by delivery class and incident kind first. Then form rolling
        // 30-second windows from each group's oldest item. This matches the
        // policy's "created within 30 seconds" wording instead of making an
        // item at an arbitrary Unix bucket edge miss its fleet batch.
        let mut groups: BTreeMap<BatchKey, Vec<db::telegram::OutboxItem>> = BTreeMap::new();
        for item in db::telegram::due_items(&self.pool, now).await? {
            groups
                .entry(BatchKey {
                    item_class: item.item_class.clone(),
                    incident_kind: item.incident_kind.clone(),
                })
                .or_default()
                .push(item);
        }

        for (_, items) in groups {
            for batch in Self::window_batches(items) {
                for chunk in Self::bounded_chunks(batch) {
                    let text = chunk
                        .iter()
                        .map(|item| item.message_text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let ids = chunk.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
                    match self
                        .transport
                        .send_message(&token, &config.chat_id, &text)
                        .await
                    {
                        Ok(_) => db::telegram::mark_sent(&self.pool, &ids, now).await?,
                        // The transport's errors are sanitized (#345): safe to store
                        // for Settings' inline last-error display.
                        Err(send_error) => {
                            let rendered = send_error.to_string();
                            for item in &chunk {
                                db::telegram::schedule_retry_or_abandon(
                                    &self.pool, item, &rendered, now,
                                )
                                .await?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Split a same-kind group into rolling windows. Items are already ordered
    /// by creation time, and a new window starts when an item is more than 30
    /// seconds after the first item in the current window.
    fn window_batches(items: Vec<db::telegram::OutboxItem>) -> Vec<Vec<db::telegram::OutboxItem>> {
        let mut batches = Vec::new();
        let mut current = Vec::new();
        let mut window_start = None;
        for item in items {
            let starts_new_window = window_start.is_some_and(|start| {
                item.created_at.signed_duration_since(start).num_seconds()
                    > db::telegram::BATCH_WINDOW_SECS
            });
            if starts_new_window {
                batches.push(current);
                current = Vec::new();
                window_start = None;
            }
            if window_start.is_none() {
                window_start = Some(item.created_at);
            }
            current.push(item);
        }
        if !current.is_empty() {
            batches.push(current);
        }
        batches
    }

    /// Split a same-kind fleet batch before it reaches Telegram's message
    /// limit. A line is always kept intact, so each PC's display name, time,
    /// and reason remain recognizable.
    fn bounded_chunks(items: Vec<db::telegram::OutboxItem>) -> Vec<Vec<db::telegram::OutboxItem>> {
        let mut chunks = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes = 0;
        for item in items {
            let separator_bytes = if current.is_empty() { 0 } else { 1 };
            let item_bytes = item.message_text.len();
            if !current.is_empty()
                && current_bytes + separator_bytes + item_bytes > MAX_BATCH_MESSAGE_BYTES
            {
                chunks.push(current);
                current = Vec::new();
                current_bytes = 0;
            }
            current_bytes += if current.is_empty() { 0 } else { 1 };
            current_bytes += item_bytes;
            current.push(item);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        chunks
    }

    /// Decrypt the protected token in memory only. A token that can no longer
    /// be read (for example after an identity change) fails sanitized and the
    /// items simply retry; nothing is ever logged with the token itself.
    fn token(&self, token_protected: &str) -> AppResult<String> {
        let ciphertext = crate::telegram::hex_decode(token_protected)?;
        let plaintext = self.protector.unprotect(&ciphertext)?;
        String::from_utf8(plaintext).map_err(|_| {
            AppError::TelegramUnavailable(
                "Stored Telegram token is not readable on this installation".to_owned(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::TimeZone;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::{
        db::telegram::TelegramConfigRow,
        telegram::{
            hex_encode,
            testing::{FakeProtector, FakeTransport},
        },
    };

    const TOKEN: &str = "123456:ABC-DEF_secret-token-value";
    pub(super) const CHAT_ID: &str = "424242";

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).single().expect("epoch")
    }

    /// Mutable injected clock so backoff, batching, and retry-window math are
    /// deterministic.
    pub(super) struct FakeClock(Mutex<DateTime<Utc>>);

    impl FakeClock {
        pub(super) fn new(start: DateTime<Utc>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(start)))
        }

        pub(super) fn advance(&self, seconds: i64) {
            *self.0.lock().expect("fake clock") = self.now() + chrono::Duration::seconds(seconds);
        }
    }

    impl DeliveryClock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().expect("fake clock")
        }
    }

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    #[test]
    fn spawn_does_not_require_caller_tokio_runtime() {
        let pool = tauri::async_runtime::block_on(async {
            SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
        })
        .expect("pool");
        let service = DeliveryService::new(
            pool,
            Arc::new(FakeTransport::new()),
            Arc::new(FakeProtector::new(true)),
            FakeClock::new(at(1_000)),
        );

        service.spawn();
    }

    /// Store a connected configuration whose protected token the given
    /// protector can read back — the same shape the real connect path writes.
    pub(super) async fn connect(pool: &SqlitePool, protector: &FakeProtector) -> AppResult<()> {
        let protected = protector.protect(TOKEN.as_bytes())?;
        crate::db::telegram::connect_with_audit(
            pool,
            &TelegramConfigRow {
                bot_username: "shop_alerts_bot".to_owned(),
                chat_id: CHAT_ID.to_owned(),
                chat_title: "Owner".to_owned(),
                token_protected: hex_encode(&protected),
                connected_at: "2026-08-26T00:00:00Z".to_owned(),
                verified_at: "2026-08-26T00:00:00Z".to_owned(),
            },
        )
        .await
    }

    pub(super) async fn enqueue(
        pool: &SqlitePool,
        item_class: &'static str,
        text: &str,
        now: DateTime<Utc>,
    ) -> AppResult<()> {
        enqueue_kind(pool, item_class, None, text, now).await
    }

    pub(super) async fn enqueue_kind(
        pool: &SqlitePool,
        item_class: &'static str,
        incident_kind: Option<&'static str>,
        text: &str,
        now: DateTime<Utc>,
    ) -> AppResult<()> {
        let mut transaction = crate::db::begin_immediate(pool).await?;
        crate::db::telegram::enqueue_in_transaction(
            &mut transaction,
            &crate::db::telegram::NewOutboxItem {
                item_class,
                incident_kind,
                message_text: text.to_owned(),
                incident_id: None,
            },
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub(super) async fn pending_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM telegram_outbox WHERE status = 'pending'",
        )
        .fetch_one(pool)
        .await
        .expect("pending count")
    }

    pub(super) fn service_of(
        pool: &SqlitePool,
        transport: &Arc<FakeTransport>,
        protector: Arc<dyn TokenProtector>,
        clock: &Arc<FakeClock>,
    ) -> DeliveryService {
        DeliveryService::new(
            pool.clone(),
            transport.clone() as Arc<dyn TelegramTransport>,
            protector,
            clock.clone() as Arc<dyn DeliveryClock>,
        )
    }

    #[tokio::test]
    async fn delivers_due_items_to_the_connected_chat_and_records_success() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(1_000));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        enqueue(
            &pool,
            "closure",
            "Client Reachability Mismatch on PC 01 closed.",
            at(1_000),
        )
        .await?;
        clock.advance(30);

        service.tick().await?;

        assert_eq!(
            transport.sends(),
            vec![(
                TOKEN.to_owned(),
                CHAT_ID.to_owned(),
                "Client Reachability Mismatch on PC 01 closed.".to_owned()
            )]
        );
        let health = crate::db::telegram::delivery_health(&pool).await?;
        assert_eq!(health.pending_count, 0);
        assert_eq!(
            health.last_sent_at,
            Some(crate::db::telegram::format_rfc3339(at(1_030)))
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnected_admin_never_attempts_delivery() -> AppResult<()> {
        let pool = test_pool().await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(2_000));
        let service = service_of(
            &pool,
            &transport,
            Arc::new(FakeProtector::new(true)),
            &clock,
        );
        enqueue(&pool, "closure", "leftover", at(2_000)).await?;

        service.tick().await?;

        assert!(transport.sends().is_empty());
        assert_eq!(pending_count(&pool).await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn same_window_items_batch_into_one_message_and_windows_do_not() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(3_000));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        // Two same-window openings (as a fleet-wide failure produces)...
        enqueue(&pool, "opening", "line one", at(3_000)).await?;
        enqueue(&pool, "opening", "line two", at(3_010)).await?;
        // ...and one closure past the window boundary.
        enqueue(&pool, "closure", "closure line", at(3_030)).await?;
        clock.advance(60);

        service.tick().await?;

        let sends = transport.sends();
        assert_eq!(sends.len(), 2, "one batched opening message, one closure");
        // Groups send in stable (class, bucket) order; closures sort first.
        assert_eq!(sends[0].2, "closure line");
        assert_eq!(sends[1].2, "line one\nline two");
        assert_eq!(pending_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn later_item_uses_the_first_items_batch_deadline() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(3_000));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        enqueue_kind(
            &pool,
            "opening",
            Some("client_reachability_mismatch"),
            "line one",
            at(3_000),
        )
        .await?;
        enqueue_kind(
            &pool,
            "opening",
            Some("client_reachability_mismatch"),
            "line two",
            at(3_020),
        )
        .await?;
        clock.advance(30);

        service.tick().await?;

        assert_eq!(transport.sends().len(), 1);
        assert_eq!(transport.sends()[0].2, "line one\nline two");
        assert_eq!(pending_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn items_across_a_unix_bucket_edge_still_batch_within_thirty_seconds() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(3_070));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        enqueue_kind(
            &pool,
            "opening",
            Some("client_reachability_mismatch"),
            "line one",
            at(3_010),
        )
        .await?;
        enqueue_kind(
            &pool,
            "opening",
            Some("client_reachability_mismatch"),
            "line two",
            at(3_039),
        )
        .await?;

        service.tick().await?;

        assert_eq!(transport.sends().len(), 1);
        assert_eq!(transport.sends()[0].2, "line one\nline two");
        Ok(())
    }

    #[tokio::test]
    async fn different_incident_kinds_never_share_a_fleet_message() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(3_600));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        enqueue_kind(
            &pool,
            "opening",
            Some("client_reachability_mismatch"),
            "Client Reachability Mismatch detected on PC 01 at 2026-08-26 10:00 UTC.",
            at(3_500),
        )
        .await?;
        enqueue_kind(
            &pool,
            "opening",
            Some("client_guard_coverage"),
            "Client Guard Coverage detected on PC 02 at 2026-08-26 10:00 UTC.",
            at(3_501),
        )
        .await?;

        service.tick().await?;

        let sends = transport.sends();
        assert_eq!(sends.len(), 2);
        assert!(sends
            .iter()
            .all(|send| !(send.2.contains("PC 01") && send.2.contains("PC 02"))));
        assert!(sends.iter().any(|send| send.2.contains("PC 01")));
        assert!(sends.iter().any(|send| send.2.contains("PC 02")));
        Ok(())
    }

    #[tokio::test]
    async fn expired_never_attempted_items_are_abandoned_before_delivery() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let created = at(3_900);
        let clock =
            FakeClock::new(created + chrono::Duration::seconds(db::telegram::RETRY_WINDOW_SECS));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        enqueue(&pool, "closure", "stale", created).await?;

        service.tick().await?;

        assert!(transport.sends().is_empty());
        let health = db::telegram::delivery_health(&pool).await?;
        assert_eq!(health.pending_count, 0);
        assert_eq!(health.expired_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn large_same_kind_fleet_batches_are_split_before_telegram() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(3_980));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        for index in 0..100 {
            enqueue_kind(
                &pool,
                "opening",
                Some("client_reachability_mismatch"),
                &format!("Client Reachability Mismatch detected on PC {index:03} at 2026-08-26 10:00 UTC."),
                at(3_950),
            )
            .await?;
        }

        service.tick().await?;

        let sends = transport.sends();
        assert!(sends.len() > 1);
        assert!(sends
            .iter()
            .all(|send| send.2.len() <= MAX_BATCH_MESSAGE_BYTES));
        assert_eq!(pending_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn failed_delivery_backs_off_then_delivers_on_recovery() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(4_000));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        enqueue(&pool, "closure", "queued while offline", at(4_000)).await?;

        // Down network: the attempt fails and schedules +30 s.
        transport.set_network_down(true);
        clock.advance(30);
        service.tick().await?;
        assert!(transport.sends().is_empty());
        assert_eq!(pending_count(&pool).await, 1);
        let health = crate::db::telegram::delivery_health(&pool).await?;
        assert!(health
            .last_error
            .as_deref()
            .is_some_and(|error| !error.is_empty()));
        assert_eq!(
            health.last_error_at,
            Some(crate::db::telegram::format_rfc3339(at(4_030)))
        );

        // Still inside backoff: nothing retries yet.
        clock.advance(29);
        service.tick().await?;
        assert!(transport.sends().is_empty());

        // Backoff elapsed and network recovered: exactly one delivery.
        transport.set_network_down(false);
        clock.advance(1);
        service.tick().await?;
        assert_eq!(transport.sends().len(), 1);
        assert_eq!(pending_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn items_past_the_retry_window_abandon_instead_of_sending_forever() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(5_000));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        enqueue(&pool, "closure", "too late", at(5_000)).await?;
        sqlx::query("UPDATE telegram_outbox SET attempts = 20 WHERE message_text = 'too late'")
            .execute(&pool)
            .await?;

        // A failure attempted beyond the 24-hour window abandons unsent.
        transport.set_network_down(true);
        clock.advance(25 * 60 * 60);
        service.tick().await?;
        let status: String = sqlx::query_scalar("SELECT status FROM telegram_outbox LIMIT 1")
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "abandoned");

        // Recovery never resurrects an abandoned item.
        transport.set_network_down(false);
        clock.advance(60);
        service.tick().await?;
        assert!(transport.sends().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn an_ambiguous_success_re_sends_one_recognizable_duplicate() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(7_000));
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        let line = "Client Reachability Mismatch on PC 01 closed at \
                    2026-08-26 10:07 UTC (about 7 minutes): Client recovered.";
        enqueue(&pool, "closure", line, at(7_000)).await?;

        // Ambiguous network outcome (#347): the message reached Telegram but
        // Admin died before recording it — the row is still pending.
        clock.advance(30);
        let item = db::telegram::due_items(&pool, clock.now()).await?.remove(0);
        transport
            .send_message(TOKEN, CHAT_ID, &item.message_text)
            .await?;
        // …and no write-back happened: that was the crash.

        // The next process re-sends the identical redacted line — a duplicate
        // the owner recognizes by PC display name and timestamp — and only a
        // confirmed send ever marks the item delivered.
        service.tick().await?;

        let sends = transport.sends();
        assert_eq!(sends.len(), 2, "at-least-once: the owner may see two");
        assert_eq!(sends[0].2, sends[1].2, "the duplicate is recognizable");
        assert_eq!(pending_count(&pool).await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn queue_survives_a_restart_and_delivers_after_reconnect() -> AppResult<()> {
        let directory = crate::test_utils::migration_backup_dir();
        let path =
            std::env::temp_dir().join(format!("openpiso-outbox-restart-{}", uuid::Uuid::new_v4()));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let first = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        crate::db::run_migrations(&first, directory.path()).await?;
        crate::db::seed(&first).await?;
        let protector = FakeProtector::new(true);
        connect(&first, &protector).await?;
        enqueue(&first, "closure", "survives restart", at(6_000)).await?;
        first.close().await;

        // A fresh Admin process reopens the same database file and delivers.
        let reopened = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(at(6_050));
        let service = service_of(&reopened, &transport, Arc::new(protector), &clock);
        service.tick().await?;

        assert_eq!(transport.sends()[0].2, "survives restart");
        assert_eq!(pending_count(&reopened).await, 0);
        reopened.close().await;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}
// ---------------------------------------------------------------------------
// End-to-end: incident transitions commit their delivery items atomically
// ---------------------------------------------------------------------------

#[cfg(test)]
mod incident_delivery_tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::tests::{connect, pending_count, service_of, FakeClock, CHAT_ID};
    use super::*;
    use crate::{
        db::{
            monitoring::{IncidentCloseReason, IncidentKind},
            telegram::delivery_health,
        },
        telegram::testing::{FakeProtector, FakeTransport},
    };

    /// Incident paths use the server clock. Put the fake delivery clock one
    /// minute ahead of setup so transaction time never makes a boundary test
    /// race the first 30-second batch window.
    fn start_now() -> DateTime<Utc> {
        Utc::now() + chrono::Duration::minutes(1)
    }

    async fn test_pool() -> AppResult<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::db::run_migrations(&pool, crate::test_utils::migration_backup_dir().path()).await?;
        crate::db::seed(&pool).await?;
        Ok(pool)
    }

    async fn register_pc(pool: &SqlitePool, pc_id: &str) -> AppResult<()> {
        crate::db::update_pc_settings(
            pool,
            pc_id,
            &crate::models::PcSettingsInput {
                name: format!("PC {pc_id}"),
                visible: true,
                registered_ip: Some(format!(
                    "192.168.4.{}",
                    pc_id.trim_start_matches("pc-").parse::<u8>().unwrap_or(1)
                )),
                mac_address: None,
            },
        )
        .await?;
        Ok(())
    }

    async fn outbox_texts(pool: &SqlitePool) -> AppResult<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT message_text FROM telegram_outbox ORDER BY created_at, id",
        )
        .fetch_all(pool)
        .await?)
    }

    #[tokio::test]
    async fn open_and_close_commit_incident_audit_and_delivery_atomically_with_collapse(
    ) -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        register_pc(&pool, "pc-01").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        // Opening commits its incident row, Alert audit event, and delivery
        // item in one transaction — before any network attempt.
        let incident_id = crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
        )
        .await?
        .expect("opens");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_monitoring_incident_opened'"
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        let texts = outbox_texts(&pool).await?;
        assert_eq!(texts.len(), 1);
        let opening = &texts[0];
        assert!(opening.contains("Client Reachability Mismatch detected on PC 01"));
        assert!(!opening.contains("192.168.4.1"), "{opening}");
        assert_eq!(pending_count(&pool).await, 1);

        // The episode ends before the opening was delivered: the queue
        // replaces both intended messages with one resolved summary.
        crate::db::monitoring::close_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
            IncidentCloseReason::ClientRecovered,
        )
        .await?
        .expect("closes");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = 'pc_monitoring_incident_closed'"
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        let texts = outbox_texts(&pool).await?;
        assert_eq!(texts.len(), 1, "the undelivered opening was replaced");
        let resolved = &texts[0];
        assert!(resolved.contains("opened at"), "{resolved}");
        assert!(resolved.contains("resolved at"), "{resolved}");
        assert!(resolved.contains("Client recovered"), "{resolved}");
        assert!(!resolved.contains("192.168"), "{resolved}");

        // Delivery sends exactly one message and changes no local evidence:
        // incidents stay closed as they were, Audit rows untouched, and the
        // Alert is never acknowledged by delivery.
        let (audit_before, acks_before): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM audit_log),
                    (SELECT COUNT(*) FROM notification_alert_ack)",
        )
        .fetch_one(&pool)
        .await?;
        clock.advance(30);
        service.tick().await?;

        let sends = transport.sends();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].1, CHAT_ID);
        assert_eq!(sends[0].2, texts[0]);
        assert_eq!(pending_count(&pool).await, 0);
        let (audit_after, acks_after): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM audit_log),
                    (SELECT COUNT(*) FROM notification_alert_ack)",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!((audit_before, acks_before), (audit_after, acks_after));
        let _ = incident_id;
        Ok(())
    }

    #[tokio::test]
    async fn fleet_wide_failure_keeps_incidents_separate_but_bounds_the_messages() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        // A simultaneous heartbeat failure on every PC: each PC still gets
        // its own durable incident.
        for pc in ["pc-01", "pc-02", "pc-03"] {
            register_pc(&pool, pc).await?;
            crate::db::monitoring::open_incident_with_audit(
                &pool,
                pc,
                &format!("PC {pc}"),
                IncidentKind::ClientReachabilityMismatch,
            )
            .await?
            .expect("opens");
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?,
            3,
            "local incidents stay per-PC"
        );

        // Delivery stays bounded: one sweep sends the same-window openings
        // as one message carrying every affected display name.
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);
        clock.advance(30);
        service.tick().await?;

        let sends = transport.sends();
        assert_eq!(sends.len(), 1, "fleet openings batch into one message");
        assert_eq!(sends[0].2.matches("detected on").count(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn closure_after_a_delivered_opening_sends_its_own_closure_message() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        register_pc(&pool, "pc-01").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
        )
        .await?
        .expect("opens");
        clock.advance(30);
        service.tick().await?;
        assert_eq!(transport.sends().len(), 1);
        assert!(transport.sends()[0].2.contains("detected on PC 01"));

        // Now the opening is already delivered, so closure travels alone as
        // a plain closure line, not a resolved summary.
        clock.advance(10);
        crate::db::monitoring::close_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
            IncidentCloseReason::ClientRecovered,
        )
        .await?
        .expect("closes");
        let texts = outbox_texts(&pool).await?;
        assert_eq!(texts.len(), 2);
        // Same-second enqueues can sort either way; match by content.
        let closures: Vec<&String> = texts
            .iter()
            .filter(|text| text.contains("closed at"))
            .collect();
        assert_eq!(closures.len(), 1);
        let closure = closures[0];
        assert!(closure.contains("Client recovered"), "{closure}");
        assert!(!closure.contains("resolved"), "{closure}");
        let openings: Vec<&String> = texts
            .iter()
            .filter(|text| text.contains("detected on"))
            .collect();
        assert_eq!(openings.len(), 1);

        clock.advance(30);
        service.tick().await?;
        assert_eq!(transport.sends().len(), 2);
        assert_eq!(transport.sends()[1].2, *closure);
        Ok(())
    }

    #[tokio::test]
    async fn delivery_never_acknowledges_the_local_alert_or_rewrites_history() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        register_pc(&pool, "pc-01").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
        )
        .await?
        .expect("opens");
        let open_audit_id: String =
            sqlx::query_scalar("SELECT open_audit_id FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        crate::db::notifications::acknowledge(&pool, &open_audit_id).await?;

        clock.advance(30);
        service.tick().await?;
        clock.advance(10);
        crate::db::monitoring::close_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
            IncidentCloseReason::HostLost,
        )
        .await?
        .expect("closes");
        clock.advance(30);
        service.tick().await?;

        // The owner's acknowledgement survived delivery untouched, and the
        // closed episode keeps its truthful reason.
        let acks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_alert_ack")
            .fetch_one(&pool)
            .await?;
        assert_eq!(acks, 1);
        let reason: Option<String> =
            sqlx::query_scalar("SELECT close_reason FROM pc_monitoring_incidents")
                .fetch_one(&pool)
                .await?;
        assert_eq!(reason.as_deref(), Some("host_lost"));
        Ok(())
    }

    #[tokio::test]
    async fn disabling_enqueues_closures_and_the_state_message_when_connected() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        register_pc(&pool, "pc-01").await?;
        register_pc(&pool, "pc-02").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC pc-01",
            IncidentKind::ClientReachabilityMismatch,
        )
        .await?
        .expect("opens");
        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-02",
            "PC pc-02",
            IncidentKind::HostProbeCoverage,
        )
        .await?
        .expect("opens");

        let closed = crate::db::monitoring::disable_and_close_active(&pool).await?;
        assert_eq!(closed, 2);

        // Two per-closure items plus one shop-wide state message, all from
        // the same transaction window; batching joins them into one message.
        let texts = outbox_texts(&pool).await?;
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|text| text.starts_with("PC Monitoring was disabled")),
            "{texts:?}"
        );
        clock.advance(30);
        service.tick().await?;
        let health = delivery_health(&pool).await?;
        assert_eq!(health.pending_count, 0);
        assert!(health.last_sent_at.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn disabling_while_disconnected_enqueues_nothing_but_still_closes() -> AppResult<()> {
        let pool = test_pool().await?;
        register_pc(&pool, "pc-01").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(
            &pool,
            &transport,
            Arc::new(FakeProtector::new(true)),
            &clock,
        );

        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC pc-01",
            IncidentKind::ClientReachabilityMismatch,
        )
        .await?
        .expect("opens");
        let alerts_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type LIKE 'pc_monitoring%'",
        )
        .fetch_one(&pool)
        .await?;

        assert_eq!(
            crate::db::monitoring::disable_and_close_active(&pool).await?,
            1
        );

        assert_eq!(outbox_texts(&pool).await?.len(), 0);
        service.tick().await?;
        assert!(transport.sends().is_empty());
        // Local history stayed canonical without Telegram.
        let events_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE event_type LIKE 'pc_monitoring%'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(events_after > alerts_before);
        Ok(())
    }

    #[tokio::test]
    async fn closure_after_an_abandoned_opening_still_names_the_episode_start() -> AppResult<()> {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        register_pc(&pool, "pc-01").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let _service = service_of(&pool, &transport, Arc::new(protector), &clock);

        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
        )
        .await?
        .expect("opens");

        // The opening exhausts its 24-hour retry window and is abandoned
        // unsent; the owner never received it.
        sqlx::query("UPDATE telegram_outbox SET status = 'abandoned'")
            .execute(&pool)
            .await?;

        crate::db::monitoring::close_incident_with_audit(
            &pool,
            "pc-01",
            "PC 01",
            IncidentKind::ClientReachabilityMismatch,
            IncidentCloseReason::ClientRecovered,
        )
        .await?
        .expect("closes");

        // An opening that was never delivered still collapses: the queue
        // carries one resolved summary naming both instants.
        let texts = outbox_texts(&pool).await?;
        assert_eq!(texts.len(), 1, "{texts:?}");
        let resolved = &texts[0];
        assert!(resolved.contains("opened at"), "{resolved}");
        assert!(resolved.contains("resolved at"), "{resolved}");
        Ok(())
    }

    #[tokio::test]
    async fn later_kinds_flow_through_the_generic_path_without_transport_changes() -> AppResult<()>
    {
        let pool = test_pool().await?;
        let protector = FakeProtector::new(true);
        connect(&pool, &protector).await?;
        register_pc(&pool, "pc-01").await?;
        let transport = Arc::new(FakeTransport::new());
        let clock = FakeClock::new(start_now());
        let service = service_of(&pool, &transport, Arc::new(protector), &clock);

        crate::db::monitoring::open_incident_with_audit(
            &pool,
            "pc-01",
            "PC pc-01",
            IncidentKind::ClientGuardCoverage,
        )
        .await?
        .expect("opens");

        // The label flows through the one generic opening path; adding a
        // future kind needs no new transport code here.
        let texts = outbox_texts(&pool).await?;
        assert!(
            texts[0].starts_with("Client Guard Coverage detected on PC pc-01"),
            "{texts:?}"
        );
        clock.advance(30);
        service.tick().await?;
        assert_eq!(transport.sends().len(), 1);
        Ok(())
    }
}

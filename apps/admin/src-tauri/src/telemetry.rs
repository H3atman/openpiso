//! Always-on Telemetry worker (ADR-0083). Only committed, allowlisted Daily
//! Summaries leave the Admin; PostHog never influences local authority.

use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::{error, info};

use crate::{
    db,
    errors::{AppError, AppResult},
    telegram::BoxFuture,
};

const TOKEN: Option<&str> = option_env!("OPENPISO_POSTHOG_TOKEN");
const HOST: &str = match option_env!("OPENPISO_POSTHOG_HOST") {
    Some(host) => host,
    None => "https://us.i.posthog.com",
};
const INTERVAL_SECS: u64 = 60;

pub fn available() -> bool {
    TOKEN.is_some_and(|token| !token.is_empty())
}

pub struct ProductionConfig {
    pub token: &'static str,
    pub host: &'static str,
}

pub fn production_config() -> Option<ProductionConfig> {
    if available() {
        TOKEN.map(|token| ProductionConfig { token, host: HOST })
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailySummaryEvent {
    event: String,
    distinct_id: String,
    uuid: String,
    timestamp: String,
    properties: SummaryProperties,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SummaryProperties {
    schema_version: u8,
    period_utc8: String,
    admin_version: String,
    clients_active_24h: i64,
    clients_active_30d: i64,
    #[serde(rename = "$process_person_profile")]
    process_person_profile: bool,
    #[serde(rename = "$geoip_disable")]
    geoip_disable: bool,
    #[serde(rename = "$lib")]
    lib: String,
}

#[derive(Serialize)]
pub struct Batch {
    api_key: String,
    batch: Vec<DailySummaryEvent>,
    sent_at: String,
}

fn period(now: DateTime<Utc>) -> String {
    // Fixed Manila offset: never depend on the Admin host timezone or DST.
    match FixedOffset::east_opt(8 * 3600) {
        Some(offset) => now.with_timezone(&offset).format("%Y-%m-%d").to_string(),
        None => now.format("%Y-%m-%d").to_string(),
    }
}

fn summary(
    now: DateTime<Utc>,
    installation_id: String,
    counts: db::telemetry::ActivePcCounts,
) -> DailySummaryEvent {
    DailySummaryEvent {
        event: "admin_daily_summary".to_owned(),
        distinct_id: installation_id,
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        properties: SummaryProperties {
            schema_version: 1,
            period_utc8: period(now),
            admin_version: env!("CARGO_PKG_VERSION").to_owned(),
            clients_active_24h: counts.clients_active_24h,
            clients_active_30d: counts.clients_active_30d,
            process_person_profile: false,
            geoip_disable: true,
            lib: "openpiso-admin".to_owned(),
        },
    }
}

/// No implementation may expose the token, body, or remote response in errors.
pub trait TelemetryTransport: Send + Sync {
    fn send_batch<'a>(&'a self, body: &'a Batch) -> BoxFuture<'a, AppResult<()>>;
}

pub struct HttpTelemetryTransport {
    client: reqwest::Client,
    url: String,
}

impl HttpTelemetryTransport {
    pub fn new(host: &str) -> AppResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| AppError::TelemetryUnavailable("HTTP client unavailable".to_owned()))?;
        Ok(Self {
            client,
            url: format!("{}/batch/", host.trim_end_matches('/')),
        })
    }
}

impl TelemetryTransport for HttpTelemetryTransport {
    fn send_batch<'a>(&'a self, body: &'a Batch) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async move {
            let response = self
                .client
                .post(&self.url)
                .json(body)
                .send()
                .await
                .map_err(|_| AppError::TelemetryUnavailable("network unavailable".to_owned()))?;
            if !response.status().is_success() {
                return Err(AppError::TelemetryUnavailable(format!(
                    "request failed (HTTP {})",
                    response.status()
                )));
            }
            Ok(())
        })
    }
}

pub trait TelemetryClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;
impl TelemetryClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub struct TelemetryService {
    pool: SqlitePool,
    transport: Arc<dyn TelemetryTransport>,
    clock: Arc<dyn TelemetryClock>,
    api_key: String,
}

impl TelemetryService {
    pub fn new(
        pool: SqlitePool,
        transport: Arc<dyn TelemetryTransport>,
        clock: Arc<dyn TelemetryClock>,
        api_key: String,
    ) -> Self {
        Self {
            pool,
            transport,
            clock,
            api_key,
        }
    }

    pub fn spawn(self) {
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(INTERVAL_SECS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = self.tick().await {
                    error!(target: "openpiso_admin_lib::telemetry", "Telemetry tick failed: {error}");
                }
            }
        });
    }

    pub async fn tick(&self) -> AppResult<()> {
        let now = self.clock.now();
        db::telemetry::drop_expired(&self.pool, now).await?;
        let today = period(now);
        let settings = db::telemetry::installation(&self.pool).await?;
        if settings.last_summary_period.as_deref() != Some(&today) {
            let counts = db::telemetry::active_pc_counts(&self.pool, now).await?;
            let event = summary(now, settings.installation_id, counts);
            let payload = serde_json::to_string(&event).map_err(|_| {
                AppError::InvalidInput("Telemetry event serialization failed".to_owned())
            })?;
            db::telemetry::upsert_summary(
                &self.pool,
                &today,
                &event.distinct_id,
                &event.uuid,
                &payload,
                now,
            )
            .await?;
        }
        let rows = db::telemetry::due_rows(&self.pool, now).await?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut events = Vec::with_capacity(rows.len());
        for row in &rows {
            let event = serde_json::from_str(&row.payload).map_err(|_| {
                AppError::InvalidInput("Stored Telemetry event is invalid".to_owned())
            })?;
            events.push(event);
        }
        let body = Batch {
            api_key: self.api_key.clone(),
            batch: events,
            sent_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };
        match self.transport.send_batch(&body).await {
            Ok(()) => {
                db::telemetry::mark_sent(&self.pool, &rows).await?;
                info!(target: "openpiso_admin_lib::telemetry", rows = rows.len(), "Telemetry batch sent");
            }
            Err(error) => {
                db::telemetry::mark_failed(&self.pool, &rows, now, &error.to_string()).await?;
                info!(target: "openpiso_admin_lib::telemetry", rows = rows.len(), "Telemetry batch failed");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::Value;
    use sqlx::SqlitePool;

    use super::*;
    use crate::{db, errors::AppResult, test_utils::mem_test_pool};

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("test instant")
            .with_timezone(&Utc)
    }

    struct FakeClock(Mutex<DateTime<Utc>>);
    impl FakeClock {
        fn new(now: DateTime<Utc>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(now)))
        }
        fn set(&self, now: DateTime<Utc>) {
            *self.0.lock().expect("clock") = now;
        }
    }
    impl TelemetryClock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().expect("clock")
        }
    }

    #[derive(Default)]
    struct FakeTransport {
        bodies: Mutex<Vec<Value>>,
        fail: Mutex<bool>,
    }
    impl FakeTransport {
        fn bodies(&self) -> Vec<Value> {
            self.bodies.lock().expect("bodies").clone()
        }
        fn set_fail(&self, fail: bool) {
            *self.fail.lock().expect("fail") = fail;
        }
    }
    impl TelemetryTransport for FakeTransport {
        fn send_batch<'a>(&'a self, body: &'a Batch) -> BoxFuture<'a, AppResult<()>> {
            Box::pin(async move {
                self.bodies
                    .lock()
                    .expect("bodies")
                    .push(serde_json::to_value(body).expect("batch"));
                if *self.fail.lock().expect("fail") {
                    return Err(crate::errors::AppError::InvalidInput("offline".to_owned()));
                }
                Ok(())
            })
        }
    }
    fn service(
        pool: &SqlitePool,
        clock: Arc<FakeClock>,
        transport: Arc<FakeTransport>,
    ) -> TelemetryService {
        TelemetryService::new(pool.clone(), transport, clock, "phc_test".to_owned())
    }
    fn event(body: &Value) -> &Value {
        &body["batch"][0]
    }

    #[tokio::test]
    async fn first_tick_mints_installation_id_only_when_summary_is_taken() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telemetry_settings")
            .fetch_one(&pool)
            .await?;
        assert_eq!(before, 0);
        let transport = Arc::new(FakeTransport::default());
        service(
            &pool,
            FakeClock::new(at("2026-09-22T15:59:59Z")),
            transport.clone(),
        )
        .tick()
        .await?;
        let after = db::telemetry::installation(&pool).await?;
        let id = after.installation_id;
        assert_eq!(transport.bodies().len(), 1);
        assert_eq!(event(&transport.bodies()[0])["distinct_id"], id);
        assert_eq!(after.last_summary_period.as_deref(), Some("2026-09-22"));
        Ok(())
    }

    #[tokio::test]
    async fn first_tick_snapshots_then_once_per_utc8_day() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let clock = FakeClock::new(at("2026-09-22T15:59:59Z"));
        let transport = Arc::new(FakeTransport::default());
        let service = service(&pool, clock.clone(), transport.clone());
        for _ in 0..8 {
            service.tick().await?;
        }
        let bodies = transport.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(event(&bodies[0])["properties"]["period_utc8"], "2026-09-22");
        clock.set(at("2026-09-22T16:00:00Z"));
        service.tick().await?;
        assert_eq!(transport.bodies().len(), 2);
        assert_eq!(
            event(&transport.bodies()[0])["distinct_id"],
            event(&transport.bodies()[1])["distinct_id"]
        );
        assert_eq!(
            event(&transport.bodies()[1])["properties"]["period_utc8"],
            "2026-09-23"
        );
        Ok(())
    }

    #[tokio::test]
    async fn failure_retries_same_uuid_after_backoff_then_deletes() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let clock = FakeClock::new(at("2026-09-22T16:00:00Z"));
        let transport = Arc::new(FakeTransport::default());
        transport.set_fail(true);
        let service = service(&pool, clock.clone(), transport.clone());
        service.tick().await?;
        clock.set(at("2026-09-22T16:00:29Z"));
        service.tick().await?;
        assert_eq!(transport.bodies().len(), 1);
        transport.set_fail(false);
        clock.set(at("2026-09-22T16:00:30Z"));
        service.tick().await?;
        let bodies = transport.bodies();
        assert_eq!(bodies.len(), 2);
        assert_eq!(event(&bodies[0])["uuid"], event(&bodies[1])["uuid"]);
        assert_eq!(
            event(&bodies[0])["timestamp"],
            event(&bodies[1])["timestamp"]
        );
        assert!(db::telemetry::due_rows(&pool, at("2026-09-23T00:00:00Z"))
            .await?
            .is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn expired_unsent_rows_are_not_sent() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let old = at("2026-08-01T00:00:00Z");
        db::telemetry::upsert_summary(
            &pool,
            "2026-08-01",
            &db::telemetry::installation(&pool).await?.installation_id,
            "old-event",
            "{}",
            old,
        )
        .await?;
        let transport = Arc::new(FakeTransport::default());
        service(
            &pool,
            FakeClock::new(at("2026-09-22T16:00:00Z")),
            transport.clone(),
        )
        .tick()
        .await?;
        assert_eq!(transport.bodies().len(), 1);
        assert_eq!(
            transport.bodies()[0]["batch"]
                .as_array()
                .expect("batch")
                .len(),
            1
        );
        assert_eq!(
            db::telemetry::due_rows(&pool, at("2026-09-23T00:00:00Z"))
                .await?
                .len(),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn payload_is_closed_allowlist_with_send_and_snapshot_times() -> AppResult<()> {
        let pool = mem_test_pool().await;
        let transport = Arc::new(FakeTransport::default());
        service(
            &pool,
            FakeClock::new(at("2026-09-22T16:00:00Z")),
            transport.clone(),
        )
        .tick()
        .await?;
        let bodies = transport.bodies();
        let body = &bodies[0];
        assert_eq!(
            body.as_object()
                .expect("object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["api_key", "batch", "sent_at"]
        );
        assert_eq!(
            event(body)
                .as_object()
                .expect("event")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["distinct_id", "event", "properties", "timestamp", "uuid"]
        );
        assert_eq!(
            event(body)["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "$geoip_disable",
                "$lib",
                "$process_person_profile",
                "admin_version",
                "clients_active_24h",
                "clients_active_30d",
                "period_utc8",
                "schema_version"
            ]
        );
        assert_eq!(event(body)["properties"]["schema_version"], 1);
        assert_eq!(event(body)["properties"]["period_utc8"], "2026-09-23");
        assert_eq!(
            event(body)["properties"]["admin_version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(event(body)["properties"]["clients_active_24h"], 0);
        assert_eq!(event(body)["properties"]["clients_active_30d"], 0);
        assert_eq!(event(body)["properties"]["$lib"], "openpiso-admin");
        assert_eq!(event(body)["properties"]["$process_person_profile"], false);
        assert_eq!(event(body)["properties"]["$geoip_disable"], true);
        assert_eq!(event(body)["event"], "admin_daily_summary");
        assert_eq!(event(body)["timestamp"], "2026-09-22T16:00:00Z");
        assert_eq!(body["sent_at"], "2026-09-22T16:00:00Z");
        assert_eq!(body["api_key"], "phc_test");
        Ok(())
    }

    #[test]
    fn spawn_does_not_require_caller_tokio_runtime() {
        let pool = tauri::async_runtime::block_on(async {
            sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
        })
        .expect("pool");
        service(
            &pool,
            FakeClock::new(Utc.timestamp_opt(1_000, 0).single().expect("epoch")),
            Arc::new(FakeTransport::default()),
        )
        .spawn();
    }
}

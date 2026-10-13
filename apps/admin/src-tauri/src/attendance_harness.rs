use std::net::SocketAddr;

use chrono::{Datelike, Offset, TimeZone};
use openpiso_core::audit::{AuditActor, AuditOutcome};

use crate::{
    account_login_throttle::AccountLoginThrottle,
    audit::{self, NewAuditEvent},
    auth::AccountTokens,
    db,
    diagnostic_request::DiagnosticRequestRegistry,
    events::EventHub,
    guard_poll::GuardRegistry,
    server::router_with_throttle,
    sessions::SessionStore,
    test_utils,
};

const REWARD_SECONDS: i64 = 120;

pub struct AttendanceFixture {
    pub server_url: String,
    pub token: String,
    pub earning_months: [String; 2],
    server: tokio::task::JoinHandle<()>,
}

impl Drop for AttendanceFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub async fn spawn_attendance_fixture() -> AttendanceFixture {
    let pool = test_utils::mem_test_pool().await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("harness listener should bind");
    let address = listener.local_addr().expect("listener address");
    let app = router_with_throttle(
        pool.clone(),
        EventHub::without_ui(),
        AccountTokens::default(),
        SessionStore::default(),
        AccountLoginThrottle::default(),
        GuardRegistry::new(),
        crate::forwarded_log::test_forwarded_log(),
        DiagnosticRequestRegistry::new(),
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("harness server should run");
    });
    // Built before seeding so a seeding failure still aborts the server via Drop.
    let mut fixture = AttendanceFixture {
        server_url: format!("http://{address}"),
        token: String::new(),
        earning_months: [String::new(), String::new()],
        server,
    };

    let signup = reqwest::Client::new()
        .post(format!("{}/api/accounts/signup", fixture.server_url))
        .json(&serde_json::json!({
            "username": "attendance-harness",
            "password": "password",
            "pc_id": "pc-01"
        }))
        .send()
        .await
        .expect("harness signup should complete")
        .json::<serde_json::Value>()
        .await
        .expect("harness signup should return JSON");
    fixture.token = signup["token"].as_str().expect("token").to_owned();
    let account_id = signup["account"]["id"].as_str().expect("account id");

    let local_now = chrono::Local::now();
    let offset = local_now.offset().fix();
    let current_month = chrono::NaiveDate::from_ymd_opt(local_now.year(), local_now.month(), 1)
        .expect("current month start");
    let month_starts = [1, 2].map(|back| {
        current_month
            .checked_sub_months(chrono::Months::new(back))
            .expect("earlier month exists")
    });
    let effective_at = offset
        .from_local_datetime(&month_starts[1].and_time(chrono::NaiveTime::MIN))
        .single()
        .expect("settings effective time is unambiguous")
        .with_timezone(&chrono::Utc);
    test_utils::insert_attendance_settings_with_window(
        &pool,
        2,
        true,
        100,
        REWARD_SECONDS,
        2,
        effective_at,
    )
    .await
    .expect("Attendance settings should be enabled");

    for month_start in &month_starts {
        let earning_date = month_start.with_day(15).expect("valid earning date");
        let source_at = offset
            .from_local_datetime(&earning_date.and_hms_opt(12, 0, 0).expect("valid time"))
            .single()
            .expect("source time is unambiguous")
            .with_timezone(&chrono::Utc);
        let mut transaction = db::begin_immediate(&pool)
            .await
            .expect("earning transaction begins");
        let mut source = NewAuditEvent::simple(
            "coin_accepted",
            AuditOutcome::Success,
            AuditActor::CoinDevice,
            "Accepted Attendance harness coin",
        );
        source.account_id = Some(account_id);
        source.pc_id = Some("pc-01");
        source.amount_cents = Some(100);
        let source_audit_event_id = audit::append(&mut transaction, source)
            .await
            .expect("source audit event appends");
        db::attendance_earning::record_earning(
            &mut transaction,
            db::attendance_earning::EarningInput {
                account_id,
                source_audit_event_id: &source_audit_event_id,
                source_kind: db::attendance::SourceKind::AcceptedCoin,
                cents: 100,
                source_at,
                offset,
                earning_date,
                award: db::attendance::AwardSnapshot {
                    configuration_version: 2,
                    daily_threshold_cents: 100,
                    reward_seconds: REWARD_SECONDS,
                },
                original_rejection_audit_event_id: None,
                trigger_pc_id: Some("pc-01"),
            },
        )
        .await
        .expect("accepted coin creates a Check-In");
        transaction.commit().await.expect("earning commits");
    }
    fixture.earning_months = month_starts.map(|start| start.format("%Y-%m").to_string());
    fixture
}

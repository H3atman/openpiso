use openpiso_core::attendance_wire::{AttendanceClaimResponse, AttendanceMonth, AttendanceSummary};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::State;

use crate::state::{AppState, SharedState};

#[derive(Debug, Serialize)]
struct AttendanceClaimRequest {
    request_id: String,
    earning_month: String,
}

#[derive(Debug, Deserialize)]
struct AttendanceApiError {
    error: Option<String>,
    message: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AttendanceClaimError {
    pub error: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

const INVALID_ATTENDANCE_REQUEST: &str = "The Daily Reward request was invalid.";

fn is_canonical_month(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 7
        || bytes[4] != b'-'
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let year = value[..4].parse::<u16>().ok();
    let month = value[5..].parse::<u8>().ok();
    year.is_some_and(|year| year > 0) && month.is_some_and(|month| (1..=12).contains(&month))
}

fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

fn attendance_url(server_url: &str, path_segments: &[&str]) -> Result<reqwest::Url, String> {
    let mut url =
        reqwest::Url::parse(server_url).map_err(|_| "could not reach admin server".to_owned())?;
    let mut path = url
        .path_segments_mut()
        .map_err(|_| "could not reach admin server".to_owned())?;
    path.pop_if_empty();
    path.extend(["api", "attendance"]);
    path.extend(path_segments.iter().copied());
    drop(path);
    Ok(url)
}

impl AttendanceClaimError {
    fn new(error: &str, message: impl Into<String>, status_code: Option<u16>) -> Self {
        Self {
            error: error.to_owned(),
            message: message.into(),
            reason: None,
            status_code,
        }
    }
}

fn claim_denial_message(reason: Option<&str>) -> &'static str {
    match reason {
        Some("month_not_found") => "This Daily Reward month could not be found.",
        Some("not_yet_claimable") => "This Daily Reward is not ready to claim.",
        Some("nothing_outstanding") => "There is no Daily Reward Time to claim.",
        Some("expired") => "This Daily Reward has expired.",
        Some("request_id_conflict") => "This Daily Reward request could not be completed.",
        _ => "The Daily Reward could not be claimed.",
    }
}

async fn attendance_error(response: reqwest::Response, operation: &str) -> AttendanceClaimError {
    let status = response.status();
    let status_code = Some(status.as_u16());
    let body = response.json::<AttendanceApiError>().await.ok();

    if status == reqwest::StatusCode::CONFLICT
        && body.as_ref().and_then(|error| error.error.as_deref()) == Some("attendance_claim_denied")
    {
        let reason = body.and_then(|error| error.reason);
        let mut error = AttendanceClaimError::new(
            "attendance_claim_denied",
            claim_denial_message(reason.as_deref()),
            status_code,
        );
        error.reason = reason;
        return error;
    }

    match status {
        reqwest::StatusCode::UNAUTHORIZED => AttendanceClaimError::new(
            "unauthorized",
            "Your account session ended. Log in again to view Daily Reward.",
            status_code,
        ),
        reqwest::StatusCode::BAD_REQUEST => AttendanceClaimError::new(
            "invalid_input",
            body.and_then(|error| error.message)
                .unwrap_or_else(|| "The Daily Reward request was invalid.".to_owned()),
            status_code,
        ),
        reqwest::StatusCode::NOT_FOUND => AttendanceClaimError::new(
            "not_found",
            "This Daily Reward month is unavailable.",
            status_code,
        ),
        _ => {
            tracing::error!(
                target: "openpiso_client::attendance",
                operation,
                status = %status,
                "Admin rejected a Daily Reward request"
            );
            AttendanceClaimError::new(
                "http_error",
                "Daily Reward is unavailable right now.",
                status_code,
            )
        }
    }
}

async fn attendance_json<T: DeserializeOwned>(
    shared: &SharedState,
    method: reqwest::Method,
    path_segments: &[&str],
    body: Option<&AttendanceClaimRequest>,
    operation: &str,
) -> Result<(T, String), AttendanceClaimError> {
    let auth = shared.account_auth.read().await.clone().ok_or_else(|| {
        AttendanceClaimError::new("unauthorized", "Log in to view Daily Reward.", None)
    })?;
    let config = shared.config.read().await.clone();
    let endpoint = attendance_url(&config.server_url, path_segments)
        .map_err(|message| AttendanceClaimError::new("transport", message, None))?;
    let mut request = reqwest::Client::new()
        .request(method, endpoint)
        .bearer_auth(&auth.token);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().await.map_err(|error| {
        tracing::error!(
            target: "openpiso_client::attendance",
            operation,
            "could not reach admin server: {error:?}"
        );
        AttendanceClaimError::new("transport", "could not reach admin server", None)
    })?;
    if !response.status().is_success() {
        return Err(attendance_error(response, operation).await);
    }
    let result = response.json::<T>().await.map_err(|error| {
        tracing::error!(
            target: "openpiso_client::attendance",
            operation,
            "Admin returned an invalid Daily Reward response: {error:?}"
        );
        AttendanceClaimError::new(
            "invalid_response",
            "Daily Reward is unavailable right now.",
            None,
        )
    })?;
    Ok((result, auth.token))
}

#[tauri::command]
pub async fn get_attendance_summary(
    state: State<'_, AppState>,
) -> Result<AttendanceSummary, String> {
    let (summary, _) = attendance_json(
        &state.shared,
        reqwest::Method::GET,
        &[],
        None,
        "summary read",
    )
    .await
    .map_err(|error| error.message)?;
    Ok(summary)
}

#[tauri::command]
pub async fn get_attendance_month(
    state: State<'_, AppState>,
    earning_month: String,
) -> Result<AttendanceMonth, String> {
    if !is_canonical_month(&earning_month) {
        return Err(INVALID_ATTENDANCE_REQUEST.to_owned());
    }
    let (month, _) = attendance_json(
        &state.shared,
        reqwest::Method::GET,
        &["months", &earning_month],
        None,
        "month read",
    )
    .await
    .map_err(|error| error.message)?;
    Ok(month)
}

#[tauri::command]
pub async fn claim_attendance(
    state: State<'_, AppState>,
    request_id: String,
    earning_month: String,
) -> Result<AttendanceClaimResponse, AttendanceClaimError> {
    if !is_canonical_uuid(&request_id) || !is_canonical_month(&earning_month) {
        return Err(AttendanceClaimError::new(
            "invalid_input",
            INVALID_ATTENDANCE_REQUEST,
            Some(reqwest::StatusCode::BAD_REQUEST.as_u16()),
        ));
    }
    let request = AttendanceClaimRequest {
        request_id,
        earning_month,
    };
    let (claim, token) = attendance_json(
        &state.shared,
        reqwest::Method::POST,
        &["claims"],
        Some(&request),
        "claim",
    )
    .await?;
    apply_claim_response(&state.shared, &token, &claim).await;
    Ok(claim)
}

async fn apply_claim_response(
    shared: &SharedState,
    expected_token: &str,
    response: &AttendanceClaimResponse,
) -> bool {
    let remaining_seconds = response.remaining_seconds.max(0);
    {
        let mut account_auth = shared.account_auth.write().await;
        let Some(auth) = account_auth
            .as_mut()
            .filter(|auth| auth.token == expected_token)
        else {
            return false;
        };
        auth.account.remaining_seconds = remaining_seconds;
        auth.session.remaining_seconds = remaining_seconds;
    }

    {
        let mut timer = shared.timer.write().await;
        timer.remaining_seconds = remaining_seconds;
        timer.expired = remaining_seconds == 0;
        if remaining_seconds > 0 && timer.power_state == "shutdown" {
            timer.power_state = "active".to_owned();
            timer.shutdown_in_seconds = None;
        }
    }
    if remaining_seconds > 0 {
        *shared.shutdown_deadline.write().await = None;
        *shared.shutdown_issued_at.write().await = None;
    }
    *shared.countdown_anchor.write().await = Some(crate::state::CountdownAnchor {
        remaining_seconds,
        synchronized_at: std::time::Instant::now(),
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AccountAuthState, AccountSummary, ActiveSession, SharedState};
    use openpiso_core::attendance::EarningMonthState;
    use openpiso_core::attendance_wire::{
        AttendanceClaimFact, AttendanceDayStatus, AttendanceMonthSummary, AttendanceTodayStatus,
        AttendanceTotals,
    };
    use std::time::Instant;

    #[test]
    fn claim_input_requires_canonical_month_and_uuid() {
        assert!(is_canonical_month("2026-09"));
        assert!(!is_canonical_month("2026-9"));
        assert!(!is_canonical_month("2026-13"));
        assert!(!is_canonical_month("0000-09"));
        assert!(is_canonical_uuid("17c70f12-7252-489c-bbe7-1292694c85cc"));
        assert!(!is_canonical_uuid("17C70F12-7252-489C-BBE7-1292694C85CC"));
        assert!(!is_canonical_uuid("17c70f12-7252-489c-bbe7-1292694c85c"));
    }

    fn month_summary() -> serde_json::Value {
        serde_json::json!({
            "earning_month": "2026-08",
            "state": "claimable",
            "check_in_count": 2,
            "earned_seconds": 7200,
            "claimed_seconds": 0,
            "outstanding_seconds": 7200,
            "claimable_seconds": 7200,
            "expired_seconds": 0,
            "forfeited_seconds": 0,
            "claim_starts_at": "2026-09-01T00:00:00+08:00",
            "deadline_at_exclusive": "2026-10-01T00:00:00+08:00",
            "claim_starts_on": "2026-09-01",
            "deadline_date_inclusive": "2026-09-30",
            "claim_count": 0,
            "latest_claim_at": null
        })
    }

    #[test]
    fn summary_deserializes_daily_reward_contract_fields() {
        let month = month_summary();
        let response: AttendanceSummary = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "today": {
                "earning_date": "2026-09-26",
                "status": "in_progress",
                "capped_cents": 500,
                "daily_threshold_cents": 1000,
                "reward_seconds": 3600
            },
            "totals": {
                "earned_seconds": 7200,
                "claimed_seconds": 0,
                "outstanding_seconds": 7200,
                "claimable_seconds": 7200,
                "expired_seconds": 0,
                "forfeited_seconds": 0
            },
            "calendar_months": [month.clone()],
            "older_claimable_months": [],
            "as_of": "2026-09-26T12:00:00Z",
            "admin_local_date": "2026-09-26",
            "admin_local_month": "2026-09"
        }))
        .expect("valid summary response");

        assert_eq!(response.today.status, AttendanceTodayStatus::InProgress);
        assert_eq!(response.today.capped_cents, 500);
        assert_eq!(response.calendar_months[0].earning_month, "2026-08");
        assert_eq!(
            response.calendar_months[0].state,
            Some(EarningMonthState::Claimable)
        );
        assert_eq!(response.calendar_months[0].claimable_seconds, 7200);
        assert_eq!(
            response.calendar_months[0]
                .deadline_date_inclusive
                .as_deref(),
            Some("2026-09-30")
        );
    }

    #[test]
    fn month_deserializes_flat_summary_and_day_calendar() {
        let mut month = month_summary();
        month["earning_month"] = serde_json::json!("2026-09");
        month["state"] = serde_json::Value::Null;
        month["check_in_count"] = serde_json::json!(0);
        month["earned_seconds"] = serde_json::json!(0);
        month["claimed_seconds"] = serde_json::json!(0);
        month["outstanding_seconds"] = serde_json::json!(0);
        month["claimable_seconds"] = serde_json::json!(0);
        month["expired_seconds"] = serde_json::json!(0);
        month["forfeited_seconds"] = serde_json::json!(0);
        month["claim_starts_at"] = serde_json::Value::Null;
        month["deadline_at_exclusive"] = serde_json::Value::Null;
        month["claim_starts_on"] = serde_json::Value::Null;
        month["deadline_date_inclusive"] = serde_json::Value::Null;
        month["claim_count"] = serde_json::json!(0);
        month["latest_claim_at"] = serde_json::Value::Null;
        month["days"] = serde_json::json!([{
            "earning_date": "2026-09-26",
            "status": "today_progress",
            "capped_cents": 250,
            "daily_threshold_cents": 1000,
            "reward_seconds": 3600
        }]);
        month["as_of"] = serde_json::json!("2026-09-26T12:00:00Z");
        month["admin_local_date"] = serde_json::json!("2026-09-26");
        month["admin_local_month"] = serde_json::json!("2026-09");

        let response: AttendanceMonth =
            serde_json::from_value(month).expect("valid month response");
        assert_eq!(response.summary.earning_month, "2026-09");
        assert_eq!(response.days.len(), 1);
        assert_eq!(response.days[0].status, AttendanceDayStatus::TodayProgress);
        assert_eq!(response.days[0].capped_cents, Some(250));

        let serialized = serde_json::to_value(response).expect("serializable month response");
        assert_eq!(serialized["earning_month"], "2026-09");
        assert!(serialized.get("summary").is_none());
        assert_eq!(serialized["days"][0]["status"], "today_progress");
    }

    #[test]
    fn claim_request_uses_the_server_wire_field_names() {
        let request = AttendanceClaimRequest {
            request_id: "17c70f12-7252-489c-bbe7-1292694c85cc".to_owned(),
            earning_month: "2026-08".to_owned(),
        };

        assert_eq!(
            serde_json::to_value(request).expect("serializable claim request"),
            serde_json::json!({
                "request_id": "17c70f12-7252-489c-bbe7-1292694c85cc",
                "earning_month": "2026-08"
            })
        );
    }

    async fn shared_for(server_url: &str, token: &str) -> SharedState {
        let shared = SharedState::default();
        shared.config.write().await.server_url = server_url.to_owned();
        *shared.account_auth.write().await = Some(AccountAuthState {
            token: token.to_owned(),
            account: AccountSummary {
                id: "harness".to_owned(),
                username: "attendance-harness".to_owned(),
                remaining_seconds: 0,
                points_balance_centi: 0,
                status: "active".to_owned(),
            },
            session: ActiveSession {
                id: "harness".to_owned(),
                account_id: "harness".to_owned(),
                username: "attendance-harness".to_owned(),
                pc_id: "pc-01".to_owned(),
                remaining_seconds: 0,
                points_balance_centi: 0,
                claimable_guest_seconds: 0,
            },
        });
        shared
    }

    #[tokio::test]
    async fn attendance_json_against_real_admin_summary_month_claim_and_retry() {
        use openpiso_admin_lib::attendance_harness::spawn_attendance_fixture;

        let admin = spawn_attendance_fixture().await;
        let shared = shared_for(&admin.server_url, &admin.token).await;
        let [recent, older] = admin.earning_months.clone();

        let (summary, token): (AttendanceSummary, _) =
            attendance_json(&shared, reqwest::Method::GET, &[], None, "summary read")
                .await
                .expect("summary through real Admin");
        assert_eq!(token, admin.token);
        assert!(summary.enabled);
        assert_eq!(summary.totals.earned_seconds, 240);
        assert_eq!(summary.totals.claimable_seconds, 240);
        assert_eq!(summary.totals.claimed_seconds, 0);
        for month in [&recent, &older] {
            let listed = summary
                .calendar_months
                .iter()
                .chain(&summary.older_claimable_months)
                .find(|candidate| &candidate.earning_month == month)
                .expect("seeded month is listed");
            assert_eq!(listed.state, Some(EarningMonthState::Claimable));
            assert_eq!(listed.claimable_seconds, 120);
            assert_eq!(listed.check_in_count, 1);
        }

        let (month, _): (AttendanceMonth, _) = attendance_json(
            &shared,
            reqwest::Method::GET,
            &["months", &recent],
            None,
            "month read",
        )
        .await
        .expect("month through real Admin (flattened summary)");
        assert_eq!(month.summary.earning_month, recent);
        assert_eq!(month.summary.claimable_seconds, 120);
        assert_eq!(month.summary.state, Some(EarningMonthState::Claimable));
        assert!(month.summary.deadline_date_inclusive.is_some());
        assert!(month
            .days
            .iter()
            .any(|day| day.earning_date.ends_with("-15")));

        let request = AttendanceClaimRequest {
            request_id: "17c70f12-7252-489c-bbe7-1292694c85cc".to_owned(),
            earning_month: recent.clone(),
        };
        let claim = || async {
            attendance_json::<AttendanceClaimResponse>(
                &shared,
                reqwest::Method::POST,
                &["claims"],
                Some(&request),
                "claim",
            )
            .await
            .expect("claim through real Admin")
            .0
        };
        let first = claim().await;
        assert_eq!(first.claim.request_id.to_string(), request.request_id);
        assert_eq!(first.claim.earning_month, recent);
        assert_eq!(first.claim.claimed_seconds, 120);
        assert_eq!(first.remaining_seconds, 120);
        assert_eq!(first.month.state, Some(EarningMonthState::Claimed));
        assert_eq!(first.month.claimed_seconds, 120);

        let retry = claim().await;
        assert_eq!(retry.claim, first.claim);
        assert_eq!(retry.remaining_seconds, 120);
        assert_eq!(retry.totals.claimed_seconds, 120);
        assert_eq!(retry.month.claim_count, 1);
    }

    #[tokio::test]
    async fn attendance_json_maps_real_admin_unauthorized_for_invalid_bearer() {
        use openpiso_admin_lib::attendance_harness::spawn_attendance_fixture;

        let admin = spawn_attendance_fixture().await;
        let shared = shared_for(&admin.server_url, "not-a-real-token").await;
        let error = attendance_json::<AttendanceSummary>(
            &shared,
            reqwest::Method::GET,
            &[],
            None,
            "summary read",
        )
        .await
        .expect_err("invalid bearer must be refused");
        assert_eq!(error.error, "unauthorized");
        assert_eq!(error.status_code, Some(401));
    }

    #[tokio::test]
    async fn successful_claim_refreshes_the_local_session_and_timer() {
        let shared = SharedState::default();
        let token = "daily-reward-token";
        *shared.account_auth.write().await = Some(AccountAuthState {
            token: token.to_owned(),
            account: AccountSummary {
                id: "account-1".to_owned(),
                username: "customer".to_owned(),
                remaining_seconds: 0,
                points_balance_centi: 0,
                status: "active".to_owned(),
            },
            session: ActiveSession {
                id: "session-1".to_owned(),
                account_id: "account-1".to_owned(),
                username: "customer".to_owned(),
                pc_id: "pc-01".to_owned(),
                remaining_seconds: 0,
                points_balance_centi: 0,
                claimable_guest_seconds: 0,
            },
        });
        {
            let mut timer = shared.timer.write().await;
            timer.remaining_seconds = 0;
            timer.expired = true;
            timer.power_state = "shutdown".to_owned();
            timer.shutdown_in_seconds = Some(30);
        }
        *shared.shutdown_deadline.write().await = Some(Instant::now());
        *shared.shutdown_issued_at.write().await = Some(Instant::now());

        let response: AttendanceClaimResponse = serde_json::from_value(serde_json::json!({
            "claim": {
                "audit_event_id": "event-1",
                "request_id": "17c70f12-7252-489c-bbe7-1292694c85cc",
                "earning_month": "2026-08",
                "claimed_seconds": 3600,
                "created_at": "2026-09-26T12:00:00Z"
            },
            "remaining_seconds": 3600,
            "totals": {
                "earned_seconds": 3600,
                "claimed_seconds": 3600,
                "outstanding_seconds": 0,
                "claimable_seconds": 0,
                "expired_seconds": 0,
                "forfeited_seconds": 0
            },
            "month": {
                "earning_month": "2026-08",
                "state": "claimed",
                "check_in_count": 1,
                "earned_seconds": 3600,
                "claimed_seconds": 3600,
                "outstanding_seconds": 0,
                "claimable_seconds": 0,
                "expired_seconds": 0,
                "forfeited_seconds": 0,
                "claim_starts_at": "2026-09-01T00:00:00+08:00",
                "deadline_at_exclusive": "2026-10-01T00:00:00+08:00",
                "claim_starts_on": "2026-09-01",
                "deadline_date_inclusive": "2026-09-30",
                "claim_count": 1,
                "latest_claim_at": "2026-09-26T12:00:00Z",
                "latest_claim_on": "2026-09-26"
            },
            "as_of": "2026-09-26T12:00:00Z",
            "admin_local_date": "2026-09-26",
            "admin_local_month": "2026-09"
        }))
        .expect("valid claim response");

        apply_claim_response(&shared, token, &response).await;

        {
            let auth = shared.account_auth.read().await;
            let auth = auth.as_ref().expect("account auth is retained");
            assert_eq!(auth.account.remaining_seconds, 3600);
            assert_eq!(auth.session.remaining_seconds, 3600);
        }

        {
            let timer = shared.timer.read().await;
            assert_eq!(timer.remaining_seconds, 3600);
            assert!(!timer.expired);
            assert_eq!(timer.power_state, "active");
            assert_eq!(timer.shutdown_in_seconds, None);
        }

        assert!(shared.shutdown_deadline.read().await.is_none());
        assert!(shared.shutdown_issued_at.read().await.is_none());
        let anchor = shared
            .countdown_anchor
            .read()
            .await
            .expect("countdown is re-anchored");
        assert_eq!(anchor.remaining_seconds, 3600);
        assert!(anchor.synchronized_at <= Instant::now());
    }

    #[tokio::test]
    async fn claim_response_does_not_overwrite_a_replaced_account_session() {
        let shared = SharedState::default();
        *shared.account_auth.write().await = Some(AccountAuthState {
            token: "new-token".to_owned(),
            account: AccountSummary {
                id: "new-account".to_owned(),
                username: "new-customer".to_owned(),
                remaining_seconds: 120,
                points_balance_centi: 0,
                status: "active".to_owned(),
            },
            session: ActiveSession {
                id: "new-session".to_owned(),
                account_id: "new-account".to_owned(),
                username: "new-customer".to_owned(),
                pc_id: "pc-01".to_owned(),
                remaining_seconds: 120,
                points_balance_centi: 0,
                claimable_guest_seconds: 0,
            },
        });
        let response = AttendanceClaimResponse {
            claim: AttendanceClaimFact {
                audit_event_id: "event-1".to_owned(),
                request_id: "17c70f12-7252-489c-bbe7-1292694c85cc"
                    .parse()
                    .expect("valid uuid"),
                earning_month: "2026-08".to_owned(),
                claimed_seconds: 3600,
                created_at: "2026-09-26T12:00:00Z".parse().expect("valid instant"),
            },
            remaining_seconds: 3600,
            totals: AttendanceTotals {
                earned_seconds: 3600,
                claimed_seconds: 3600,
                outstanding_seconds: 0,
                claimable_seconds: 0,
                expired_seconds: 0,
                forfeited_seconds: 0,
            },
            month: AttendanceMonthSummary {
                earning_month: "2026-08".to_owned(),
                state: Some(EarningMonthState::Claimed),
                check_in_count: 1,
                earned_seconds: 3600,
                claimed_seconds: 3600,
                outstanding_seconds: 0,
                claimable_seconds: 0,
                expired_seconds: 0,
                forfeited_seconds: 0,
                claim_starts_at: None,
                deadline_at_exclusive: None,
                claim_starts_on: None,
                deadline_date_inclusive: None,
                claim_count: 1,
                latest_claim_at: Some("2026-09-26T12:00:00Z".parse().expect("valid instant")),
                latest_claim_on: Some("2026-09-26".to_owned()),
            },
            as_of: "2026-09-26T12:00:00Z".parse().expect("valid instant"),
            admin_local_date: "2026-09-26".to_owned(),
            admin_local_month: "2026-09".to_owned(),
        };

        apply_claim_response(&shared, "old-token", &response).await;

        {
            let auth = shared.account_auth.read().await;
            let auth = auth.as_ref().expect("replacement auth is retained");
            assert_eq!(auth.token, "new-token");
            assert_eq!(auth.account.remaining_seconds, 120);
        }
        assert_eq!(shared.timer.read().await.remaining_seconds, 0);
        assert!(shared.countdown_anchor.read().await.is_none());
    }
}

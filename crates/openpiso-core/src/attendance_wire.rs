//! The Daily Reward wire contract between Admin's `/api/attendance` routes and
//! the Client. Admin produces these payloads from its own projections; the
//! Client decodes and forwards them to the frontend unchanged.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::attendance::EarningMonthState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceTotals {
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub outstanding_seconds: i64,
    pub claimable_seconds: i64,
    pub expired_seconds: i64,
    pub forfeited_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttendanceTodayStatus {
    CheckedIn,
    InProgress,
    Ineligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceToday {
    pub earning_date: String,
    pub status: AttendanceTodayStatus,
    pub capped_cents: i64,
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceMonthSummary {
    pub earning_month: String,
    pub state: Option<EarningMonthState>,
    pub check_in_count: i64,
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub outstanding_seconds: i64,
    pub claimable_seconds: i64,
    pub expired_seconds: i64,
    pub forfeited_seconds: i64,
    pub claim_starts_at: Option<DateTime<Utc>>,
    pub deadline_at_exclusive: Option<DateTime<Utc>>,
    pub claim_starts_on: Option<String>,
    pub deadline_date_inclusive: Option<String>,
    pub claim_count: i64,
    pub latest_claim_at: Option<DateTime<Utc>>,
    /// Absent from older Admin payloads.
    #[serde(default)]
    pub latest_claim_on: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceSummary {
    pub enabled: bool,
    pub today: AttendanceToday,
    pub totals: AttendanceTotals,
    pub calendar_months: Vec<AttendanceMonthSummary>,
    pub older_claimable_months: Vec<AttendanceMonthSummary>,
    pub as_of: DateTime<Utc>,
    pub admin_local_date: String,
    pub admin_local_month: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttendanceDayStatus {
    CheckedIn,
    Missed,
    Ineligible,
    Future,
    TodayProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceDay {
    pub earning_date: String,
    pub status: AttendanceDayStatus,
    pub capped_cents: Option<i64>,
    pub daily_threshold_cents: Option<i64>,
    pub reward_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceMonth {
    #[serde(flatten)]
    pub summary: AttendanceMonthSummary,
    pub days: Vec<AttendanceDay>,
    pub as_of: DateTime<Utc>,
    pub admin_local_date: String,
    pub admin_local_month: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceClaimFact {
    pub audit_event_id: String,
    pub request_id: Uuid,
    pub earning_month: String,
    pub claimed_seconds: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceClaimResponse {
    pub claim: AttendanceClaimFact,
    pub remaining_seconds: i64,
    pub totals: AttendanceTotals,
    pub month: AttendanceMonthSummary,
    pub as_of: DateTime<Utc>,
    pub admin_local_date: String,
    pub admin_local_month: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn month_json() -> serde_json::Value {
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
            "claim_starts_at": "2026-08-31T16:00:00Z",
            "deadline_at_exclusive": "2026-09-30T16:00:00Z",
            "claim_starts_on": "2026-09-01",
            "deadline_date_inclusive": "2026-09-30",
            "claim_count": 0,
            "latest_claim_at": null,
            "latest_claim_on": null
        })
    }

    #[test]
    fn month_summary_round_trips_with_snake_case_state_and_utc_instants() {
        let decoded: AttendanceMonthSummary = serde_json::from_value(month_json()).unwrap();
        assert_eq!(decoded.state, Some(EarningMonthState::Claimable));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), month_json());
    }

    #[test]
    fn older_month_without_latest_claim_on_decodes_and_reserializes_null() {
        let mut older = month_json();
        older.as_object_mut().unwrap().remove("latest_claim_on");
        let decoded: AttendanceMonthSummary = serde_json::from_value(older).unwrap();
        assert_eq!(decoded.latest_claim_on, None);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), month_json());
    }

    #[test]
    fn offset_instants_decode_and_serialize_as_utc() {
        let mut offset = month_json();
        offset["claim_starts_at"] = "2026-09-01T00:00:00+08:00".into();
        let decoded: AttendanceMonthSummary = serde_json::from_value(offset).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap()["claim_starts_at"],
            "2026-08-31T16:00:00Z"
        );
    }

    #[test]
    fn month_flattens_summary_beside_days() {
        let mut month = month_json();
        month["days"] = serde_json::json!([{
            "earning_date": "2026-09-26",
            "status": "today_progress",
            "capped_cents": 250,
            "daily_threshold_cents": 1000,
            "reward_seconds": null
        }]);
        month["as_of"] = "2026-09-26T12:00:00Z".into();
        month["admin_local_date"] = "2026-09-26".into();
        month["admin_local_month"] = "2026-09".into();

        let decoded: AttendanceMonth = serde_json::from_value(month.clone()).unwrap();
        assert_eq!(decoded.days[0].status, AttendanceDayStatus::TodayProgress);
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert!(encoded.get("summary").is_none());
        assert_eq!(encoded, month);
    }

    #[test]
    fn claim_serializes_request_id_as_canonical_string() {
        let claim = serde_json::json!({
            "audit_event_id": "event-1",
            "request_id": "17c70f12-7252-489c-bbe7-1292694c85cc",
            "earning_month": "2026-08",
            "claimed_seconds": 3600,
            "created_at": "2026-09-26T12:00:00.123Z"
        });
        let decoded: AttendanceClaimFact = serde_json::from_value(claim.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), claim);
    }

    #[test]
    fn non_canonical_values_are_rejected() {
        let mut bad_state = month_json();
        bad_state["state"] = "Claimable".into();
        assert!(serde_json::from_value::<AttendanceMonthSummary>(bad_state).is_err());

        let claim = serde_json::json!({
            "audit_event_id": "event-1",
            "request_id": "not-a-uuid",
            "earning_month": "2026-08",
            "claimed_seconds": 1,
            "created_at": "2026-09-26T12:00:00Z"
        });
        assert!(serde_json::from_value::<AttendanceClaimFact>(claim).is_err());
    }
}

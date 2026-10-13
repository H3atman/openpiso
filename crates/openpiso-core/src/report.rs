use serde::{Deserialize, Serialize};

/// Reporting aggregation period (ADR-0014: day / week / month).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportPeriod {
    Day,
    Week,
    Month,
}

impl ReportPeriod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    pub const fn sqlite_column(self) -> &'static str {
        match self {
            Self::Day => "business_date",
            Self::Week => "business_week",
            Self::Month => "business_month",
        }
    }
}

/// Audit viewer event-category filter (ADR-0015:
/// Auth/Money/Time/Points/Admin/System). Each event type maps to exactly one
/// category so the filter is mutually exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditCategory {
    Auth,
    Money,
    Time,
    Points,
    Admin,
    System,
}

impl AuditCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Money => "money",
            Self::Time => "time",
            Self::Points => "points",
            Self::Admin => "admin",
            Self::System => "system",
        }
    }

    /// Returns the audit_log event_type values that belong to this category.
    /// Kept in the shared crate so the Rust backend and any future typed
    /// clients agree on the filter boundaries.
    pub const fn event_types(self) -> &'static [&'static str] {
        match self {
            Self::Auth => &[
                "login",
                "logout",
                "auto_kick",
                "taken_over",
                "expired",
                "tamper_kick",
                "claim",
                "login_succeeded",
                "login_failed",
                "account_created",
                "account_create_failed",
                "account_password_reset",
                "account_password_reset_failed",
                "account_deleted",
                "account_delete_failed",
            ],
            Self::Money => &[
                "coin_accepted",
                "voucher_sold",
                "voucher_minted",
                "sold_voucher_redeemed",
                "promo_voucher_redeemed",
                "voucher_rejected",
            ],
            Self::Time => &[
                "manual_adjustment",
                "attendance_check_in",
                "attendance_claim",
            ],
            Self::Points => &[
                "points_redeemed",
                "redeem_denied",
                "wheel_spin",
                "wheel_spin_denied",
            ],
            Self::Admin => &[
                "economy_settings_updated",
                "attendance_settings_updated",
                "redemption_tiers_updated",
                "coin_promo_settings_updated",
                "wheel_settings_updated",
                "shop_hours_settings_updated",
                "shop_rate_updated",
                "shop_override_set",
                "shop_override_cleared",
                "voucher_batch_minted",
                "voucher_batch_voided",
                "account_import_completed",
            ],
            Self::System => &["legacy_event", "tamper_kill", "shop_opened", "shop_closed"],
        }
    }
}

/// Maps a canonical audit event_type to its reporting category. Returns
/// `None` for event types the repository does not yet know (so future event
/// types remain visible in the unfiltered viewer instead of being hidden).
pub fn category_for_event_type(event_type: &str) -> Option<AuditCategory> {
    [
        AuditCategory::Auth,
        AuditCategory::Money,
        AuditCategory::Time,
        AuditCategory::Points,
        AuditCategory::Admin,
        AuditCategory::System,
    ]
    .into_iter()
    .find(|category| category.event_types().contains(&event_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_period_round_trips() {
        for (period, expected) in [
            (ReportPeriod::Day, "day"),
            (ReportPeriod::Week, "week"),
            (ReportPeriod::Month, "month"),
        ] {
            assert_eq!(period.as_str(), expected);
            assert_eq!(
                period.sqlite_column(),
                match period {
                    ReportPeriod::Day => "business_date",
                    ReportPeriod::Week => "business_week",
                    ReportPeriod::Month => "business_month",
                }
            );
        }
    }

    #[test]
    fn audit_categories_are_mutually_exclusive() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for category in [
            AuditCategory::Auth,
            AuditCategory::Money,
            AuditCategory::Time,
            AuditCategory::Points,
            AuditCategory::Admin,
            AuditCategory::System,
        ] {
            for event_type in category.event_types() {
                assert!(
                    seen.insert(event_type),
                    "event_type {event_type} claimed by two audit categories"
                );
            }
        }
    }

    #[test]
    fn category_lookup_recognizes_known_event_types() {
        assert_eq!(
            category_for_event_type("coin_accepted"),
            Some(AuditCategory::Money)
        );
        assert_eq!(
            category_for_event_type("manual_adjustment"),
            Some(AuditCategory::Time)
        );
        assert_eq!(
            category_for_event_type("attendance_check_in"),
            Some(AuditCategory::Time)
        );
        assert_eq!(
            category_for_event_type("attendance_claim"),
            Some(AuditCategory::Time)
        );
        assert_eq!(
            category_for_event_type("attendance_settings_updated"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("wheel_spin"),
            Some(AuditCategory::Points)
        );
        assert_eq!(
            category_for_event_type("economy_settings_updated"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("redemption_tiers_updated"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("coin_promo_settings_updated"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("shop_hours_settings_updated"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("shop_rate_updated"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("shop_override_set"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("shop_override_cleared"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("voucher_batch_minted"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(
            category_for_event_type("voucher_batch_voided"),
            Some(AuditCategory::Admin)
        );
        assert_eq!(category_for_event_type("unknown_future_event"), None);
    }
}

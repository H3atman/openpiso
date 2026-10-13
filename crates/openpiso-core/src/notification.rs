use serde::{Deserialize, Serialize};

/// Fixed owner-facing severity for an Audit Log event (ADR-0058).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Alert,
    Notice,
    Silent,
}

const ALERT_EVENT_TYPES: &[&str] = &[
    "account_deleted",
    "account_points_adjusted",
    "account_time_adjusted",
    "account_transfer_out",
    "kiosk_lock_release_throttled",
    "manual_adjustment",
    "owner_login_throttled",
    "pc_escalated_power_off",
    "pc_monitoring_incident_opened",
    "pc_removed",
    "points_transfer_out",
    // A Rejection Credit is the only owner action that writes Cash Revenue
    // from a typed number, so it is at least as owner-visible as the account
    // balance adjustments above (ADR-0069).
    "rejection_credit",
    "restore_applied",
    "tamper_kill",
    "telegram_connected",
    "voucher_batch_voided",
    "voucher_voided",
];

const NOTICE_EVENT_TYPES: &[&str] = &[
    "account_import_completed",
    "account_login_throttled",
    "account_password_reset",
    "autostart_changed",
    "autostart_change_failed",
    "backup_created",
    "backup_deleted",
    "backup_settings_changed",
    "backup_schedule_caught_up",
    "backup_schedule_failed",
    "coin_device_recovered",
    "coin_device_settings_updated",
    "coin_device_unavailable",
    "coin_promo_settings_updated",
    "coin_sampling_anomaly",
    "coin_batch_rejected",
    "economy_settings_updated",
    "kiosk_lock_engaged",
    "kiosk_lock_released",
    "offline_grace_updated",
    "owner_configured",
    "owner_credential_changed",
    "owner_pin_changed",
    "pc_monitoring_incident_closed",
    "pc_monitoring_settings_updated",
    "pc_recovery_escalation_settings_updated",
    "pc_monitoring_suppression_updated",
    "pc_pre_registered",
    "pc_settings_updated",
    "points_transfer_expired",
    "points_transfer_rejected",
    "points_transfer_requested",
    "redemption_tiers_updated",
    "restore_cancelled",
    "restore_requested",
    "shop_closed",
    "shop_hours_settings_updated",
    "shop_name_updated",
    "shop_opened",
    "shop_override_cleared",
    "shop_override_set",
    "shutdown_grace_updated",
    "telegram_disconnected",
    "voucher_batch_minted",
    "wheel_settings_updated",
];

/// Classifies an Audit Log event without considering its outcome, actor, or
/// values. Unknown event types remain silent until deliberately allowlisted.
pub fn classify_audit_event(event_type: &str) -> NotificationSeverity {
    if ALERT_EVENT_TYPES.contains(&event_type) {
        NotificationSeverity::Alert
    } else if NOTICE_EVENT_TYPES.contains(&event_type) {
        NotificationSeverity::Notice
    } else {
        NotificationSeverity::Silent
    }
}

/// Event types classified as Alerts, for severity-scoped indexed reads. The
/// classifier remains the authority; consumers must still classify rows before
/// exposing them.
pub fn alert_event_types() -> &'static [&'static str] {
    ALERT_EVENT_TYPES
}

/// Event types classified as Notices, for severity-scoped indexed reads.
pub fn notice_event_types() -> &'static [&'static str] {
    NOTICE_EVENT_TYPES
}

/// Returns the fixed union of Alert and Notice event types for indexed reads.
pub fn notification_event_types() -> impl Iterator<Item = &'static str> {
    ALERT_EVENT_TYPES
        .iter()
        .chain(NOTICE_EVENT_TYPES.iter())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_alert_event() {
        for event_type in [
            "account_deleted",
            "account_points_adjusted",
            "account_time_adjusted",
            "account_transfer_out",
            "kiosk_lock_release_throttled",
            "manual_adjustment",
            "owner_login_throttled",
            "pc_monitoring_incident_opened",
            "pc_removed",
            "points_transfer_out",
            "rejection_credit",
            "restore_applied",
            "tamper_kill",
            "telegram_connected",
            "voucher_batch_voided",
            "voucher_voided",
        ] {
            assert_eq!(
                classify_audit_event(event_type),
                NotificationSeverity::Alert
            );
        }
    }

    #[test]
    fn classifies_every_notice_event() {
        for event_type in [
            "account_import_completed",
            "account_login_throttled",
            "account_password_reset",
            "autostart_changed",
            "autostart_change_failed",
            "backup_created",
            "backup_deleted",
            "backup_settings_changed",
            "backup_schedule_caught_up",
            "backup_schedule_failed",
            "coin_device_recovered",
            "coin_device_settings_updated",
            "coin_device_unavailable",
            "coin_promo_settings_updated",
            "coin_sampling_anomaly",
            "coin_batch_rejected",
            "economy_settings_updated",
            "kiosk_lock_engaged",
            "kiosk_lock_released",
            "offline_grace_updated",
            "owner_configured",
            "owner_credential_changed",
            "owner_pin_changed",
            "pc_monitoring_incident_closed",
            "pc_monitoring_settings_updated",
            "pc_monitoring_suppression_updated",
            "pc_pre_registered",
            "pc_settings_updated",
            "points_transfer_expired",
            "points_transfer_rejected",
            "points_transfer_requested",
            "redemption_tiers_updated",
            "restore_cancelled",
            "restore_requested",
            "shop_closed",
            "shop_hours_settings_updated",
            "shop_name_updated",
            "shop_opened",
            "shop_override_cleared",
            "shop_override_set",
            "shutdown_grace_updated",
            "telegram_disconnected",
            "voucher_batch_minted",
            "wheel_settings_updated",
        ] {
            assert_eq!(
                classify_audit_event(event_type),
                NotificationSeverity::Notice
            );
        }
    }

    // `pc_went_offline`/`pc_came_online` were owner-facing Notices until
    // the 2026-08-09 ADR-0058 amendment and were demoted to Silent on
    // purpose: reachability is already live in the PC grid, while the audit
    // rows remain available as server-visible diagnostic evidence.
    #[test]
    fn reachability_events_classify_as_silent() {
        for event_type in ["pc_went_offline", "pc_came_online"] {
            assert_eq!(
                classify_audit_event(event_type),
                NotificationSeverity::Silent
            );
        }
    }

    // ADR-0076: every PC Monitoring Incident opening is an Alert, every
    // closure a Notice, and the shop-wide enable/disable toggle a Notice.
    // The incident kind stays typed incident data; it never forks the event
    // name.
    #[test]
    fn pc_monitoring_incident_lifecycle_classifications_are_fixed() {
        assert_eq!(
            classify_audit_event("pc_monitoring_incident_opened"),
            NotificationSeverity::Alert
        );
        for event_type in [
            "pc_monitoring_incident_closed",
            "pc_monitoring_settings_updated",
            "pc_monitoring_suppression_updated",
        ] {
            assert_eq!(
                classify_audit_event(event_type),
                NotificationSeverity::Notice
            );
        }
    }

    #[test]
    fn keeps_non_notification_events_silent() {
        for event_type in [
            "login_failed",
            "tamper_kick",
            "coin_accepted",
            "unknown_future_event",
        ] {
            assert_eq!(
                classify_audit_event(event_type),
                NotificationSeverity::Silent
            );
        }
    }
}

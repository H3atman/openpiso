use openpiso_core::session::PowerState;

/// Remaining-Time threshold (seconds) below which the client shows a
/// non-blocking low-time warning before expiry (ADR-0022).
pub const WARNING_SECONDS: i64 = 120;

/// Server-authoritative power command for a connected PC (ADR-0005, ADR-0022,
/// ADR-0034). A pure function of the PC's connection status and authoritative
/// Remaining Time, evaluated at the single broadcast chokepoint so every
/// delivery channel (heartbeat response, SSE initial snapshot, SSE updates)
/// carries the same command.
///
/// Any *connected* PC at zero Remaining Time is commanded to shut down — there
/// is no longer a "freshly booted at zero is left alone" exception (ADR-0034
/// supersedes the positive→zero rule). A PC with no paid time runs the grace
/// countdown and powers off so it cannot be used for free; the customer tops up
/// (coins at the terminal, or logging into an account that has time) to cancel
/// it. An `offline` PC is never commanded — there is nothing connected to act
/// on the command.
pub fn power_state(status: &str, remaining: i64) -> PowerState {
    if status == "offline" {
        return PowerState::Active;
    }
    if remaining <= 0 {
        return PowerState::Shutdown;
    }
    if remaining <= WARNING_SECONDS {
        return PowerState::Warning;
    }
    PowerState::Active
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_zero_time_pc_is_commanded_to_shut_down() {
        // A guest PC reporting zero ("online") and an account PC reporting zero
        // ("in_use") are both commanded off — including a fresh boot that never
        // held positive time (ADR-0034).
        assert_eq!(power_state("online", 0), PowerState::Shutdown);
        assert_eq!(power_state("in_use", 0), PowerState::Shutdown);
    }

    #[test]
    fn offline_pc_is_never_commanded() {
        // Nothing is connected to run the countdown or power off.
        assert_eq!(power_state("offline", 0), PowerState::Active);
    }

    #[test]
    fn low_positive_time_warns_and_a_top_up_clears_it() {
        assert_eq!(power_state("in_use", WARNING_SECONDS), PowerState::Warning);
        assert_eq!(power_state("in_use", 1), PowerState::Warning);
        // A coin or login raising the balance above zero leaves the warning
        // band; well above it the PC runs normally.
        assert_eq!(
            power_state("in_use", WARNING_SECONDS + 1),
            PowerState::Active
        );
        assert_eq!(power_state("in_use", 3600), PowerState::Active);
    }
}

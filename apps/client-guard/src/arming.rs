//! Pure Client Guard arming policy (ADR-0048).
//!
//! Owner quit is the only clean exit that stands the Guard down. A new Client
//! connection re-arms it; a new Guard process starts armed after reboot.

use crate::classification::DeathClassification;
use openpiso_core::guard::CleanExitReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmState {
    Armed,
    StoodDown,
}

impl ArmState {
    pub const fn initial() -> Self {
        Self::Armed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmEvent {
    ClientStarted,
    ClientDisconnected(DeathClassification),
}

/// Apply one observed lifecycle event to the Guard's arm/disarm state.
pub const fn transition(state: ArmState, event: ArmEvent) -> ArmState {
    match event {
        ArmEvent::ClientStarted => ArmState::Armed,
        ArmEvent::ClientDisconnected(DeathClassification::CleanExit(
            CleanExitReason::OwnerQuit,
        )) => ArmState::StoodDown,
        ArmEvent::ClientDisconnected(_) => state,
    }
}

/// A Tamper Kill relaunches only while this boot's Guard is armed.
pub const fn should_relaunch(state: ArmState, classification: DeathClassification) -> bool {
    matches!(
        (state, classification),
        (ArmState::Armed, DeathClassification::TamperKill)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_quit_stands_down_until_the_next_client_start() {
        let state = transition(
            ArmState::Armed,
            ArmEvent::ClientDisconnected(DeathClassification::CleanExit(
                CleanExitReason::OwnerQuit,
            )),
        );
        assert_eq!(state, ArmState::StoodDown);
        assert_eq!(transition(state, ArmEvent::ClientStarted), ArmState::Armed);
    }

    #[test]
    fn shutdown_exits_do_not_stand_down() {
        for reason in [
            CleanExitReason::ExpiryPowerOff,
            CleanExitReason::OfflineShutdown,
        ] {
            assert_eq!(
                transition(
                    ArmState::Armed,
                    ArmEvent::ClientDisconnected(DeathClassification::CleanExit(reason)),
                ),
                ArmState::Armed
            );
        }
    }

    #[test]
    fn stood_down_guard_does_not_relaunch_until_client_starts() {
        let state = ArmState::StoodDown;
        assert_eq!(
            transition(
                state,
                ArmEvent::ClientDisconnected(DeathClassification::TamperKill)
            ),
            ArmState::StoodDown
        );
        assert!(!should_relaunch(state, DeathClassification::TamperKill));
        assert!(should_relaunch(
            ArmState::Armed,
            DeathClassification::TamperKill
        ));
    }

    #[test]
    fn a_new_guard_boot_starts_armed() {
        assert_eq!(ArmState::initial(), ArmState::Armed);
    }
}

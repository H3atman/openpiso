use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerMode {
    Guest,
    Account,
}

/// What the server is currently commanding a connected client to do about
/// expiry. Streamed on `pc.updated` so the client can show a low-time warning,
/// run the shutdown grace countdown, or proceed normally. See ADR-0005 and
/// ADR-0022.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    /// Normal operation; no expiry action.
    #[default]
    Active,
    /// Remaining Time is low; show a non-blocking top-up warning.
    Warning,
    /// Time reached zero on a connected PC; run the grace countdown and power
    /// off when it elapses.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    Logout,
    AutoKick,
    TakenOver,
    Expired,
    TamperKick,
    /// Client Recovery Escalation powered the PC off (ADR-0077). It carries no
    /// attribution and never changes balance, time, or Points.
    EscalatedPowerOff,
}

impl SessionEndReason {
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::Logout => "logout",
            Self::AutoKick => "auto_kick",
            Self::TakenOver => "taken_over",
            Self::Expired => "expired",
            Self::TamperKick => "tamper_kick",
            Self::EscalatedPowerOff => "escalated_power_off",
        }
    }
}

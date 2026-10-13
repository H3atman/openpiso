//! The Escalated Power Off (ADR-0077).
//!
//! Forced and immediate, because the machine this runs on is already failing
//! to run the one application it exists to run, and a graceful request can be
//! refused by whatever is hung. Release Windows powers off; debug and
//! non-Windows builds log the intent, matching ADR-0045's countdown and the
//! rest of the Guard's release-Windows gating.

/// Whether this build actually powers the machine off.
pub const POWER_OFF_ENFORCED: bool = cfg!(all(windows, not(debug_assertions)));

/// Power the PC off now. A failure returns its text so the caller can log it
/// and retry on the next poll; nothing escalates beyond this.
pub fn power_off() -> Result<(), String> {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        std::process::Command::new("shutdown")
            .args([
                "/s",
                "/f",
                "/t",
                "0",
                "/c",
                "OpenPiso Client recovery failed",
            ])
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    #[cfg(not(all(windows, not(debug_assertions))))]
    {
        tracing::info!(
            target: "openpiso_client_guard::power",
            "Escalated Power Off suppressed (debug or non-Windows build); a release build would power off now"
        );
        Ok(())
    }
}

/// Pure decision: should this order be acted on?
///
/// An order is a desired state, not a message, so the Guard may read the same
/// one many times. Acting once per nonce is the whole of its idempotence, and
/// an order without a nonce is refused rather than acted on blind.
pub fn should_power_off(power_off: bool, nonce: Option<&str>, acted_nonce: Option<&str>) -> bool {
    match (power_off, nonce) {
        (true, Some(nonce)) => acted_nonce != Some(nonce),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unseen_order_is_acted_on_once() {
        assert!(should_power_off(true, Some("abc"), None));
        assert!(should_power_off(true, Some("abc"), Some("older")));
    }

    #[test]
    fn re_reading_the_same_order_does_not_act_again() {
        assert!(!should_power_off(true, Some("abc"), Some("abc")));
    }

    #[test]
    fn a_healthy_answer_never_powers_a_pc_off() {
        assert!(!should_power_off(false, Some("abc"), None));
        assert!(!should_power_off(false, None, None));
    }

    #[test]
    fn an_order_without_a_nonce_is_refused() {
        assert!(!should_power_off(true, None, None));
    }
}

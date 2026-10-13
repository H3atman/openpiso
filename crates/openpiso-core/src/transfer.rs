use thiserror::Error;

/// Default time-to-live for a Points Transfer Request (ADR-0030, ~10 min).
///
/// A request whose age exceeds this is no longer confirmable: the
/// owner-confirm/reject path rejects it on read (even before the background
/// sweep runs), and the sweep marks past-TTL `pending` rows `expired`. Both
/// paths share this constant so the "time-boxed" guarantee is enforced in one
/// place.
pub const POINTS_TRANSFER_TTL_SECONDS: i64 = 600;

/// Outcome of validating a single account-to-account transfer (ADR-0030).
///
/// Self-transfer is rejected at the username layer (a transfer asks for a
/// recipient by normalized Username, and the requester can never resolve to
/// themselves), so this validator only checks the value envelope. It is
/// reused by both the immediate time path and the owner-confirmed points
/// path so the two stay consistent on the money-and-time rules.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransferError {
    #[error("transfer must include a positive number of seconds or points")]
    BothZero,
    #[error("seconds to transfer must not be negative")]
    NegativeSeconds,
    #[error("points to transfer must not be negative")]
    NegativePoints,
    #[error("sender has {sender} seconds; cannot transfer {requested} seconds")]
    InsufficientSeconds { sender: i64, requested: i64 },
    #[error("sender has {sender} points; cannot transfer {requested} points")]
    InsufficientPoints { sender: i64, requested: i64 },
}

/// Pure validator for a single-currency or combined transfer.
///
/// `from_seconds` / `from_points` are the sender's current balances (already
/// re-read inside the transaction). `seconds` / `points` are the amounts
/// the customer is asking to move. The function enforces, in order:
///
/// 1. Both requested amounts are non-negative (negative inputs are an
///    invariant violation at the boundary; the HTTP layer should have
///    rejected them, but the validator is the last line of defence).
/// 2. At least one of `seconds` or `points` is strictly positive — a
///    no-op transfer would still write audit rows, so it is rejected.
/// 3. The sender's balance covers the requested amount for each currency
///    they are spending.
///
/// Self-drain to zero is allowed for time (normal expiry follows per
/// ADR-0022); balance never goes below zero.
pub fn validate_transfer(
    from_seconds: i64,
    from_points: i64,
    seconds: i64,
    points: i64,
) -> Result<(), TransferError> {
    if seconds < 0 {
        return Err(TransferError::NegativeSeconds);
    }
    if points < 0 {
        return Err(TransferError::NegativePoints);
    }
    if seconds == 0 && points == 0 {
        return Err(TransferError::BothZero);
    }
    if seconds > 0 && from_seconds < seconds {
        return Err(TransferError::InsufficientSeconds {
            sender: from_seconds,
            requested: seconds,
        });
    }
    if points > 0 && from_points < points {
        return Err(TransferError::InsufficientPoints {
            sender: from_points,
            requested: points,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_and_zero_is_rejected() {
        assert_eq!(
            validate_transfer(600, 5, 0, 0),
            Err(TransferError::BothZero)
        );
    }

    #[test]
    fn negative_seconds_is_rejected() {
        assert_eq!(
            validate_transfer(600, 5, -1, 0),
            Err(TransferError::NegativeSeconds)
        );
    }

    #[test]
    fn negative_points_is_rejected() {
        assert_eq!(
            validate_transfer(600, 5, 0, -1),
            Err(TransferError::NegativePoints)
        );
    }

    #[test]
    fn exact_balance_seconds_is_allowed() {
        assert_eq!(validate_transfer(600, 5, 600, 0), Ok(()));
    }

    #[test]
    fn exact_balance_points_is_allowed() {
        assert_eq!(validate_transfer(600, 5, 0, 5), Ok(()));
    }

    #[test]
    fn insufficient_seconds_is_rejected() {
        assert_eq!(
            validate_transfer(300, 5, 600, 0),
            Err(TransferError::InsufficientSeconds {
                sender: 300,
                requested: 600
            })
        );
    }

    #[test]
    fn insufficient_points_is_rejected() {
        assert_eq!(
            validate_transfer(600, 2, 0, 5),
            Err(TransferError::InsufficientPoints {
                sender: 2,
                requested: 5
            })
        );
    }

    #[test]
    fn zero_balance_with_zero_transfer_is_rejected_as_noop() {
        // The no-op check fires before the balance check, so a zero-balance
        // sender asking for nothing is still a no-op.
        assert_eq!(validate_transfer(0, 0, 0, 0), Err(TransferError::BothZero));
    }

    #[test]
    fn positive_seconds_with_zero_balance_is_rejected() {
        assert_eq!(
            validate_transfer(0, 5, 1, 0),
            Err(TransferError::InsufficientSeconds {
                sender: 0,
                requested: 1
            })
        );
    }

    #[test]
    fn positive_points_with_zero_balance_is_rejected() {
        assert_eq!(
            validate_transfer(600, 0, 0, 1),
            Err(TransferError::InsufficientPoints {
                sender: 0,
                requested: 1
            })
        );
    }

    #[test]
    fn combined_transfer_validates_each_currency_independently() {
        // Both legs satisfy the balance check; the request is allowed.
        assert_eq!(validate_transfer(600, 5, 300, 5), Ok(()));
    }

    #[test]
    fn combined_transfer_with_one_leg_insufficient_is_rejected() {
        // Seconds leg is fine, points leg is short — points check fires
        // after the seconds check would have passed.
        assert_eq!(
            validate_transfer(600, 2, 300, 5),
            Err(TransferError::InsufficientPoints {
                sender: 2,
                requested: 5
            })
        );
    }

    #[test]
    fn self_drain_to_zero_seconds_is_allowed() {
        // Normal expiry follows the same shape (ADR-0022): a customer may
        // hand off every second they have left.
        assert_eq!(validate_transfer(120, 3, 120, 0), Ok(()));
    }
}

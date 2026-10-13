use thiserror::Error;

/// Centipoints per whole point (ADR-0038). The canonical points unit is the
/// integer centipoint; whole points appear only in Redemption Tiers, the
/// wheel's fixed cost, and display.
pub const CENTIPOINTS_PER_POINT: i64 = 100;

/// Centicents per cent. Earning carry is stored in centicents so the
/// sub-centipoint remainder is exact for any `earn_cents_per_point`,
/// including values not divisible by 100 (ADR-0038).
pub const CENTICENTS_PER_CENT: i64 = 100;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PricingError {
    #[error("amount must be positive")]
    InvalidAmount,
    #[error("rate amount and seconds must be positive")]
    InvalidRate,
    #[error("point economy settings must be positive")]
    InvalidEconomy,
    #[error("calculated duration is too large")]
    Overflow,
}

pub fn calculate_seconds(
    amount_cents: i64,
    rate_amount_cents: i64,
    rate_seconds: i64,
) -> Result<i64, PricingError> {
    if amount_cents <= 0 {
        return Err(PricingError::InvalidAmount);
    }
    if rate_amount_cents <= 0 || rate_seconds <= 0 {
        return Err(PricingError::InvalidRate);
    }

    amount_cents
        .checked_mul(rate_seconds)
        .map(|value| value / rate_amount_cents)
        .filter(|seconds| *seconds > 0)
        .ok_or(PricingError::Overflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointsEarned {
    pub points_centi: i64,
    pub carry_centicents: i64,
}

/// Fractional earning (ADR-0038): paid cents convert directly to centipoints.
///
/// One whole point costs `earn_cents_per_point` cents, so one centipoint
/// costs exactly `earn_cents_per_point` **centicents** — dividing the
/// eligible centicents by the earn rate yields centipoints with a
/// sub-centipoint remainder that carries, exactly, for any earn rate.
pub fn calculate_points_earned(
    amount_cents: i64,
    prior_carry_centicents: i64,
    earn_cents_per_point: i64,
) -> Result<PointsEarned, PricingError> {
    if amount_cents <= 0 {
        return Err(PricingError::InvalidAmount);
    }
    if prior_carry_centicents < 0 || earn_cents_per_point <= 0 {
        return Err(PricingError::InvalidEconomy);
    }

    let eligible_centicents = amount_cents
        .checked_mul(CENTICENTS_PER_CENT)
        .and_then(|value| value.checked_add(prior_carry_centicents))
        .ok_or(PricingError::Overflow)?;
    Ok(PointsEarned {
        points_centi: eligible_centicents / earn_cents_per_point,
        carry_centicents: eligible_centicents % earn_cents_per_point,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoinOutcome {
    pub seconds_added: i64,
    pub coin_promo_bonus_seconds: i64,
    pub points_earned_centi: i64,
    pub new_carry_centicents: i64,
}

pub enum CoinOutcomeTarget {
    Guest,
    Account { prior_carry_centicents: i64 },
}

pub fn compute_coin_outcome(
    amount_cents: i64,
    target: CoinOutcomeTarget,
    coin_promo_bonus_seconds: i64,
    earn_cents_per_point: i64,
    rate_amount_cents: i64,
    rate_seconds: i64,
) -> Result<CoinOutcome, PricingError> {
    if coin_promo_bonus_seconds < 0 {
        return Err(PricingError::InvalidEconomy);
    }
    let base_seconds = calculate_seconds(amount_cents, rate_amount_cents, rate_seconds)?;
    match target {
        CoinOutcomeTarget::Guest => Ok(CoinOutcome {
            seconds_added: base_seconds,
            coin_promo_bonus_seconds: 0,
            points_earned_centi: 0,
            new_carry_centicents: 0,
        }),
        CoinOutcomeTarget::Account {
            prior_carry_centicents,
        } => {
            let points = calculate_points_earned(
                amount_cents,
                prior_carry_centicents,
                earn_cents_per_point,
            )?;
            Ok(CoinOutcome {
                seconds_added: base_seconds
                    .checked_add(coin_promo_bonus_seconds)
                    .ok_or(PricingError::Overflow)?,
                coin_promo_bonus_seconds,
                points_earned_centi: points.points_centi,
                new_carry_centicents: points.carry_centicents,
            })
        }
    }
}

/// One Coin Promo Tier as the greedy decomposition consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoinPromoTierRate {
    pub amount_cents: i64,
    pub bonus_seconds: i64,
}

/// What a Coin Batch amount matched against the authored Coin Promo Tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoinPromoMatch {
    /// The part of the amount that a tier covered. Zero when nothing matched.
    pub covered_amount_cents: i64,
    pub bonus_seconds: i64,
}

/// Decompose a Coin Batch amount into Coin Promo Tier amounts, largest tier
/// first, and sum their bonuses (ADR-0052 as amended by ADR-0070).
///
/// A batch is a whole coin *train*, not one coin: ADR-0070 credits the pulse
/// sum, and consecutive coins concatenate with no seam, so ₱10 + ₱1 arrives as
/// one 1100-cent amount. Matching the total against a tier exactly would drop
/// the bonus of every coin in a merged train. A tier may therefore repeat —
/// two ₱10 coins in one train are two ₱10 coins — and the remainder no tier
/// covers earns nothing.
///
/// Greedy is bounded by design: every peso the decomposition covers is priced
/// at some tier's bonus rate and the remainder is priced at zero, so a batch
/// can never exceed the most generous tier's Effective Bonus %. A single coin
/// still resolves to exactly its own tier, so nothing about the single-coin
/// case changes.
pub fn coin_promo_match(
    amount_cents: i64,
    tiers: &[CoinPromoTierRate],
) -> Result<CoinPromoMatch, PricingError> {
    if amount_cents <= 0 {
        return Err(PricingError::InvalidAmount);
    }
    let mut ordered: Vec<CoinPromoTierRate> = tiers
        .iter()
        .copied()
        .filter(|tier| tier.amount_cents > 0 && tier.bonus_seconds > 0)
        .collect();
    ordered.sort_unstable_by_key(|tier| core::cmp::Reverse(tier.amount_cents));

    let mut remaining = amount_cents;
    let mut matched = CoinPromoMatch::default();
    for tier in ordered {
        if remaining < tier.amount_cents {
            continue;
        }
        let count = remaining / tier.amount_cents;
        let covered = count * tier.amount_cents;
        remaining -= covered;
        matched.covered_amount_cents += covered;
        matched.bonus_seconds = count
            .checked_mul(tier.bonus_seconds)
            .and_then(|bonus| matched.bonus_seconds.checked_add(bonus))
            .ok_or(PricingError::Overflow)?;
        if remaining == 0 {
            break;
        }
    }
    Ok(matched)
}

/// The points→seconds exchange of one Redemption Tier, as consumed by the
/// safety math here and in [`crate::wheel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierRate {
    pub points: i64,
    pub seconds: i64,
}

pub fn coin_promo_effective_bonus_basis_points(
    amount_cents: i64,
    bonus_seconds: i64,
    rate_amount_cents: i64,
    rate_seconds: i64,
) -> Result<i64, PricingError> {
    if bonus_seconds <= 0 {
        return Err(PricingError::InvalidEconomy);
    }
    let base_seconds = calculate_seconds(amount_cents, rate_amount_cents, rate_seconds)?;
    let numerator = i128::from(bonus_seconds)
        .checked_mul(10_000)
        .ok_or(PricingError::Overflow)?;
    i64::try_from(numerator / i128::from(base_seconds)).map_err(|_| PricingError::Overflow)
}

pub fn effective_bonus_basis_points(
    rate_amount_cents: i64,
    rate_seconds: i64,
    earn_cents_per_point: i64,
    tier: TierRate,
) -> Result<i64, PricingError> {
    if rate_amount_cents <= 0 || rate_seconds <= 0 {
        return Err(PricingError::InvalidRate);
    }
    if earn_cents_per_point <= 0 || tier.points <= 0 || tier.seconds <= 0 {
        return Err(PricingError::InvalidEconomy);
    }

    let bonus_numerator = i128::from(tier.seconds)
        .checked_mul(i128::from(rate_amount_cents))
        .and_then(|value| value.checked_mul(10_000))
        .ok_or(PricingError::Overflow)?;
    let bonus_denominator = i128::from(tier.points)
        .checked_mul(i128::from(earn_cents_per_point))
        .and_then(|value| value.checked_mul(i128::from(rate_seconds)))
        .ok_or(PricingError::Overflow)?;
    i64::try_from(bonus_numerator / bonus_denominator).map_err(|_| PricingError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rate_adds_five_minutes_per_peso() {
        assert_eq!(calculate_seconds(100, 100, 300), Ok(300));
        assert_eq!(calculate_seconds(500, 100, 300), Ok(1500));
    }

    #[test]
    fn truncates_fractional_seconds_for_a_non_dividing_shop_rate() {
        // A ₱1 coin at ₱3 = 601 seconds would buy 200⅓ seconds; the
        // authoritative integer-division result is 200 seconds.
        assert_eq!(calculate_seconds(100, 300, 601), Ok(200));
    }

    #[test]
    fn rejects_invalid_values() {
        assert_eq!(
            calculate_seconds(0, 100, 300),
            Err(PricingError::InvalidAmount)
        );
        assert_eq!(
            calculate_seconds(100, 0, 300),
            Err(PricingError::InvalidRate)
        );
    }

    #[test]
    fn sub_threshold_purchase_earns_fractional_points_immediately() {
        // ₱25 at ₱50/point = 0.50 points on the spot, no carry (ADR-0038).
        assert_eq!(
            calculate_points_earned(2_500, 0, 5_000),
            Ok(PointsEarned {
                points_centi: 50,
                carry_centicents: 0
            })
        );
        // ₱20 = 0.40 points exactly.
        assert_eq!(
            calculate_points_earned(2_000, 0, 5_000),
            Ok(PointsEarned {
                points_centi: 40,
                carry_centicents: 0
            })
        );
        // ₱130 = 2.60 points.
        assert_eq!(
            calculate_points_earned(13_000, 0, 5_000),
            Ok(PointsEarned {
                points_centi: 260,
                carry_centicents: 0
            })
        );
    }

    #[test]
    fn earn_rate_not_divisible_by_100_carries_exact_centicents() {
        // ₱1 at earn rate 333 cents/point: 10000 centicents / 333 =
        // 30 centipoints, remainder 10 centicents.
        assert_eq!(
            calculate_points_earned(100, 0, 333),
            Ok(PointsEarned {
                points_centi: 30,
                carry_centicents: 10
            })
        );
        // Prior carry participates exactly: 10000 + 10 = 10010 -> 30 r 20.
        assert_eq!(
            calculate_points_earned(100, 10, 333),
            Ok(PointsEarned {
                points_centi: 30,
                carry_centicents: 20
            })
        );
        // Carry never reaches one centipoint's worth (333 centicents).
        let earned = calculate_points_earned(999, 332, 333).unwrap();
        assert!(earned.carry_centicents < 333);
    }

    #[test]
    fn earn_conserves_value_exactly() {
        // centicents in == centipoints * rate + carry, for an awkward rate.
        for (cents, prior) in [(1, 0), (99, 17), (12_345, 332), (5_000, 1)] {
            let earned = calculate_points_earned(cents, prior, 333).unwrap();
            assert_eq!(
                cents * CENTICENTS_PER_CENT + prior,
                earned.points_centi * 333 + earned.carry_centicents
            );
        }
    }

    #[test]
    fn coin_promo_match_decomposes_a_merged_train_largest_tier_first() {
        let tiers = [
            CoinPromoTierRate {
                amount_cents: 100,
                bonus_seconds: 120,
            },
            CoinPromoTierRate {
                amount_cents: 1_000,
                bonus_seconds: 1_500,
            },
        ];
        // ₱10 + ₱1 in one train: both coins keep their bonus.
        assert_eq!(
            coin_promo_match(1_100, &tiers),
            Ok(CoinPromoMatch {
                covered_amount_cents: 1_100,
                bonus_seconds: 1_620
            })
        );
        // A single coin still resolves to exactly its own tier.
        assert_eq!(
            coin_promo_match(1_000, &tiers),
            Ok(CoinPromoMatch {
                covered_amount_cents: 1_000,
                bonus_seconds: 1_500
            })
        );
    }

    #[test]
    fn coin_promo_match_prefers_the_larger_tier_over_repeating_a_smaller_one() {
        let tiers = [
            CoinPromoTierRate {
                amount_cents: 1_000,
                bonus_seconds: 1_500,
            },
            CoinPromoTierRate {
                amount_cents: 2_000,
                bonus_seconds: 3_600,
            },
        ];
        // Two ₱10 coins in one train reach ₱20, so the ₱20 tier applies once
        // rather than the ₱10 tier twice — the escalating ladder pays out.
        assert_eq!(
            coin_promo_match(2_000, &tiers),
            Ok(CoinPromoMatch {
                covered_amount_cents: 2_000,
                bonus_seconds: 3_600
            })
        );
        // ₱30 is one ₱20 then one ₱10, never three ₱10.
        assert_eq!(
            coin_promo_match(3_000, &tiers),
            Ok(CoinPromoMatch {
                covered_amount_cents: 3_000,
                bonus_seconds: 5_100
            })
        );
    }

    #[test]
    fn coin_promo_match_repeats_a_tier_and_leaves_the_remainder_unpriced() {
        let tiers = [CoinPromoTierRate {
            amount_cents: 1_000,
            bonus_seconds: 1_500,
        }];
        // Two ₱10 coins in one train earn the tier twice.
        assert_eq!(
            coin_promo_match(2_000, &tiers),
            Ok(CoinPromoMatch {
                covered_amount_cents: 2_000,
                bonus_seconds: 3_000
            })
        );
        // The ₱1 no tier covers earns nothing.
        assert_eq!(
            coin_promo_match(2_100, &tiers),
            Ok(CoinPromoMatch {
                covered_amount_cents: 2_000,
                bonus_seconds: 3_000
            })
        );
        // Below every tier: no bonus at all.
        assert_eq!(coin_promo_match(500, &tiers), Ok(CoinPromoMatch::default()));
    }

    #[test]
    fn coin_promo_match_never_exceeds_the_most_generous_tier_rate() {
        // ₱20 pays a worse rate per peso than ₱10 here, so greedy taking the
        // larger tier first can only stay under the ₱10 tier's ratio.
        let tiers = [
            CoinPromoTierRate {
                amount_cents: 1_000,
                bonus_seconds: 1_500,
            },
            CoinPromoTierRate {
                amount_cents: 2_000,
                bonus_seconds: 2_000,
            },
        ];
        let best_bonus_per_cent = 1_500_f64 / 1_000_f64;
        for amount_cents in (100..10_000).step_by(100) {
            let matched = coin_promo_match(amount_cents, &tiers).unwrap();
            assert!(
                matched.bonus_seconds as f64 <= best_bonus_per_cent * amount_cents as f64,
                "amount {amount_cents} exceeded the most generous tier rate"
            );
        }
    }

    #[test]
    fn coin_promo_match_ignores_invalid_rows_and_rejects_invalid_amounts() {
        let tiers = [
            CoinPromoTierRate {
                amount_cents: 0,
                bonus_seconds: 600,
            },
            CoinPromoTierRate {
                amount_cents: 100,
                bonus_seconds: 0,
            },
        ];
        assert_eq!(coin_promo_match(500, &tiers), Ok(CoinPromoMatch::default()));
        assert_eq!(
            coin_promo_match(0, &tiers),
            Err(PricingError::InvalidAmount)
        );
    }

    #[test]
    fn coin_promo_effective_bonus_uses_paid_time_and_integer_basis_points() {
        assert_eq!(
            coin_promo_effective_bonus_basis_points(500, 600, 100, 300),
            Ok(4_000)
        );
        assert_eq!(
            coin_promo_effective_bonus_basis_points(500, 601, 100, 300),
            Ok(4_006)
        );
    }

    #[test]
    fn effective_bonus_uses_integer_basis_points() {
        assert_eq!(
            effective_bonus_basis_points(
                100,
                300,
                5_000,
                TierRate {
                    points: 10,
                    seconds: 14_400
                }
            ),
            Ok(960)
        );
    }

    #[test]
    fn effective_bonus_rejects_unrepresentable_results() {
        assert_eq!(
            effective_bonus_basis_points(
                1,
                1,
                1,
                TierRate {
                    points: 1,
                    seconds: i64::MAX
                }
            ),
            Err(PricingError::Overflow)
        );
    }

    #[test]
    fn guest_coin_outcome_earns_zero_points() {
        let outcome =
            compute_coin_outcome(100, CoinOutcomeTarget::Guest, 0, 5_000, 100, 300).unwrap();
        assert_eq!(outcome.seconds_added, 300);
        assert_eq!(outcome.points_earned_centi, 0);
        assert_eq!(outcome.new_carry_centicents, 0);
    }

    #[test]
    fn account_coin_outcome_earns_fractional_points() {
        // ₱20 at ₱50/point: 0.40 points immediately, nothing carried.
        let outcome = compute_coin_outcome(
            2_000,
            CoinOutcomeTarget::Account {
                prior_carry_centicents: 0,
            },
            0,
            5_000,
            100,
            300,
        )
        .unwrap();
        assert_eq!(outcome.seconds_added, 6_000);
        assert_eq!(outcome.points_earned_centi, 40);
        assert_eq!(outcome.new_carry_centicents, 0);

        // ₱120 at ₱50/point = 2.40 points.
        let outcome = compute_coin_outcome(
            12_000,
            CoinOutcomeTarget::Account {
                prior_carry_centicents: 0,
            },
            0,
            5_000,
            100,
            300,
        )
        .unwrap();
        assert_eq!(outcome.seconds_added, 36_000);
        assert_eq!(outcome.points_earned_centi, 240);
        assert_eq!(outcome.new_carry_centicents, 0);
    }

    #[test]
    fn coin_promo_is_additive_for_accounts_and_zero_for_guests() {
        let account = compute_coin_outcome(
            100,
            CoinOutcomeTarget::Account {
                prior_carry_centicents: 0,
            },
            120,
            5_000,
            100,
            300,
        )
        .unwrap();
        let guest =
            compute_coin_outcome(100, CoinOutcomeTarget::Guest, 120, 5_000, 100, 300).unwrap();

        assert_eq!(account.seconds_added, 420);
        assert_eq!(account.coin_promo_bonus_seconds, 120);
        assert_eq!(guest.seconds_added, 300);
        assert_eq!(guest.coin_promo_bonus_seconds, 0);
    }

    #[test]
    fn coin_promo_does_not_change_points_or_carry() {
        let without = compute_coin_outcome(
            100,
            CoinOutcomeTarget::Account {
                prior_carry_centicents: 10,
            },
            0,
            333,
            100,
            300,
        )
        .unwrap();
        let with = compute_coin_outcome(
            100,
            CoinOutcomeTarget::Account {
                prior_carry_centicents: 10,
            },
            120,
            333,
            100,
            300,
        )
        .unwrap();

        assert_eq!(with.points_earned_centi, without.points_earned_centi);
        assert_eq!(with.new_carry_centicents, without.new_carry_centicents);
    }

    #[test]
    fn coin_promo_addition_reports_pricing_overflow() {
        assert_eq!(
            compute_coin_outcome(
                1,
                CoinOutcomeTarget::Account {
                    prior_carry_centicents: 0,
                },
                2,
                1,
                1,
                i64::MAX - 1,
            ),
            Err(PricingError::Overflow)
        );
    }
}

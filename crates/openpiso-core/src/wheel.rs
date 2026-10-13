use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pricing::TierRate;

/// Maximum number of consecutive free spins the chain may grant before a forced
/// stop. Bounds both the expected-value math and a lucky customer's streak.
pub const MAX_FREE_SPIN_CAP: i64 = 10;

/// A paid spin costs exactly 1.00 point — 100 centipoints. The wheel is a
/// whole-unit exception to fractional points (ADR-0038); fractional bets are
/// not allowed, and the house-edge math keeps measuring against one whole
/// point's redemption value (ADR-0012).
pub const WHEEL_SPIN_COST_CENTIPOINTS: i64 = 100;

/// Total probability assigned across all Wheel Segments.
pub const WHEEL_PROBABILITY_BASIS_POINTS: i64 = 10_000;

/// Upper bound on the number of segments to keep the editor and the
/// expected-value math well-conditioned. With Bokya Rate unset, this also sets
/// the odds granularity floor at 1 slice in 24, about 4.2% (ADR-0047).
pub const MAX_SEGMENTS: usize = 24;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WheelError {
    #[error("wheel must have at least one segment")]
    NoSegments,
    #[error("wheel cannot have more than {0} segments")]
    TooManySegments(usize),
    #[error("wheel must include at least one segment that does not grant a free spin")]
    NoTerminalSegment,
    #[error("segment seconds must be non-negative")]
    InvalidSegmentSeconds,
    #[error("segment points_centi must be non-negative")]
    InvalidPoints,
    #[error("free spin cap must be between 0 and {0}")]
    InvalidFreeSpinCap(i64),
    #[error("Bokya Rate must be between 0 and 10000 basis points")]
    InvalidBokyaRate,
    #[error("a positive Bokya Rate requires at least one Bokya segment")]
    NoBokyaSegment,
    #[error("a Bokya Rate below 10000 requires at least one win segment")]
    NoWinSegment,
    #[error("Redemption Tier must have positive points and seconds")]
    InvalidRedemptionTier,
    #[error("wheel values are too large")]
    ArithmeticOverflow,
    #[error("expected payout meets or exceeds the redemption value of a point")]
    NegativeHouseEdge,
}

/// A Wheel Segment is a prize pair: it awards a label, an amount of seconds,
/// an amount of points (centipoints), and optionally grants a Free Spin.
/// There is no tagged kind — "win nothing" is a segment awarding neither time
/// nor points and not granting a Free Spin. A single segment can award both
/// time and points and grant a Free Spin simultaneously.
///
/// Segments carry no probability weight (ADR-0047). Every segment occupies
/// exactly `1/n` of the circle. It is also `1/n` likely when Bokya Rate is unset;
/// when set, repeated segments distribute probability within the Bokya or win
/// group (ADR-0049).
// Serde's default of ignoring unknown fields is relied on here: an older client
// still sending the removed `weight` degrades rather than erroring (ADR-0047).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WheelSegment {
    pub label: String,
    /// Seconds awarded (non-negative).
    pub seconds: i64,
    /// Centipoints awarded (non-negative).
    pub points_centi: i64,
    /// Whether landing on this segment grants a free spin.
    pub free_spin: bool,
}

impl WheelSegment {
    /// Bokya is derived from a segment awarding no time, points, or Free Spin.
    pub fn is_bokya(&self) -> bool {
        self.seconds == 0 && self.points_centi == 0 && !self.free_spin
    }
}

/// Server-authoritative wheel configuration. Prizes are time, points,
/// win-nothing, or an extra free spin — any combination per segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WheelConfig {
    pub enabled: bool,
    /// Optional probability that one landing is Bokya, in basis points.
    /// Unset preserves the uniform segment draw.
    pub bokya_rate_basis_points: Option<i64>,
    pub free_spin_cap: i64,
    pub segments: Vec<WheelSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpinStep {
    pub segment_index: usize,
    pub seconds: i64,
    pub points_centi: i64,
    pub free_spin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpinOutcome {
    pub steps: Vec<SpinStep>,
    pub seconds_won: i64,
    pub points_won_centi: i64,
    pub free_spins_triggered: i64,
}

/// Source of randomness for a spin. Abstracted so production uses a real RNG
/// while tests inject a deterministic sequence.
pub trait SpinRng {
    /// Returns a value in `[0, bound)`. `bound` is guaranteed to be positive.
    fn gen_below(&mut self, bound: i64) -> i64;
}

impl WheelConfig {
    /// Validates the structural and per-segment invariants. Independent of
    /// `enabled` so a disabled wheel is still required to be well-formed.
    pub fn validate(&self) -> Result<(), WheelError> {
        if self.segments.is_empty() {
            return Err(WheelError::NoSegments);
        }
        if self.segments.len() > MAX_SEGMENTS {
            return Err(WheelError::TooManySegments(MAX_SEGMENTS));
        }
        if !(0..=MAX_FREE_SPIN_CAP).contains(&self.free_spin_cap) {
            return Err(WheelError::InvalidFreeSpinCap(MAX_FREE_SPIN_CAP));
        }
        if self
            .bokya_rate_basis_points
            .is_some_and(|rate| !(0..=WHEEL_PROBABILITY_BASIS_POINTS).contains(&rate))
        {
            return Err(WheelError::InvalidBokyaRate);
        }

        let mut has_terminal = false;
        let mut total_seconds = 0_i64;
        let mut total_points = 0_i64;
        let mut max_seconds = 0_i64;
        let mut max_points = 0_i64;
        for segment in &self.segments {
            if segment.seconds < 0 {
                return Err(WheelError::InvalidSegmentSeconds);
            }
            if segment.points_centi < 0 {
                return Err(WheelError::InvalidPoints);
            }
            total_seconds = total_seconds
                .checked_add(segment.seconds)
                .ok_or(WheelError::ArithmeticOverflow)?;
            // Representability check only, not a points-axis economic cap:
            // every finite i64 prize remains legal under ADR-0046.
            total_points = total_points
                .checked_add(segment.points_centi)
                .ok_or(WheelError::ArithmeticOverflow)?;
            max_seconds = max_seconds.max(segment.seconds);
            max_points = max_points.max(segment.points_centi);
            if !segment.free_spin {
                has_terminal = true;
            }
        }
        let max_landings = self
            .free_spin_cap
            .checked_add(1)
            .ok_or(WheelError::ArithmeticOverflow)?;
        max_seconds
            .checked_mul(max_landings)
            .ok_or(WheelError::ArithmeticOverflow)?;
        max_points
            .checked_mul(max_landings)
            .ok_or(WheelError::ArithmeticOverflow)?;
        if !has_terminal {
            return Err(WheelError::NoTerminalSegment);
        }
        if let Some(rate) = self.bokya_rate_basis_points {
            let has_bokya = self.segments.iter().any(WheelSegment::is_bokya);
            let has_win = self.segments.iter().any(|segment| !segment.is_bokya());
            if rate > 0 && !has_bokya {
                return Err(WheelError::NoBokyaSegment);
            }
            if rate < WHEEL_PROBABILITY_BASIS_POINTS && !has_win {
                return Err(WheelError::NoWinSegment);
            }
        }
        Ok(())
    }

    /// Per-segment landing probabilities in basis points, always summing to
    /// exactly 10000. Drives the biased draw and states the odds to the owner.
    /// Unset is uniform; when the segment count does not divide 10000 this is
    /// the basis-point rounding of an exactly `1/n` draw, so it is a display
    /// and draw contract rather than the input to the expectation math.
    /// When set, Bokya segments evenly share the rate and win segments evenly
    /// share its remainder. Integer leftovers go to each group's lowest indices.
    pub fn segment_probability_basis_points(&self) -> Vec<i64> {
        let Ok(segment_count) = i64::try_from(self.segments.len()) else {
            return Vec::new();
        };
        if segment_count == 0 {
            return Vec::new();
        }
        let Some(bokya_rate) = self.bokya_rate_basis_points else {
            let share = WHEEL_PROBABILITY_BASIS_POINTS / segment_count;
            let remainder = WHEEL_PROBABILITY_BASIS_POINTS % segment_count;
            return (0..segment_count)
                .map(|index| share + i64::from(index < remainder))
                .collect();
        };

        let mut distribution = vec![0_i64; self.segments.len()];
        for (bokya, group_total) in [
            (true, bokya_rate),
            (false, WHEEL_PROBABILITY_BASIS_POINTS - bokya_rate),
        ] {
            let Ok(group_count) = i64::try_from(
                self.segments
                    .iter()
                    .filter(|segment| segment.is_bokya() == bokya)
                    .count(),
            ) else {
                return Vec::new();
            };
            if group_count == 0 {
                continue;
            }
            let share = group_total / group_count;
            let remainder = group_total % group_count;
            let mut group_index = 0_i64;
            for (index, segment) in self.segments.iter().enumerate() {
                if segment.is_bokya() == bokya {
                    distribution[index] = share + i64::from(group_index < remainder);
                    group_index += 1;
                }
            }
        }
        distribution
    }

    /// Exact per-segment landing probabilities for the expectation math.
    ///
    /// An unset Bokya Rate is exactly `1/n` — the pre-ADR-0049 uniform draw,
    /// which integer basis points cannot express when `n` does not divide
    /// 10000. Rounding it would make an upgrade an economic event. When the
    /// rate is set, these are the same basis points the draw walks, so the
    /// guardrail measures the wheel that is actually spun.
    fn segment_probabilities(&self) -> Vec<f64> {
        let total = self.segments.len();
        if total == 0 {
            return Vec::new();
        }
        if self.bokya_rate_basis_points.is_none() {
            return vec![1.0 / total as f64; total];
        }
        self.segment_probability_basis_points()
            .into_iter()
            .map(|basis_points| basis_points as f64 / WHEEL_PROBABILITY_BASIS_POINTS as f64)
            .collect()
    }

    /// Number of segments that grant a free spin.
    pub fn free_spin_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| segment.free_spin)
            .count()
    }

    /// Expected number of landings for one paid spin: the paid landing plus the
    /// capped free-spin chain, `sum_{k=0}^{cap} free_probability^k`.
    fn expected_landings(&self, probabilities: &[f64]) -> f64 {
        let free_probability = self
            .segments
            .iter()
            .zip(probabilities)
            .filter(|(segment, _)| segment.free_spin)
            .map(|(_, probability)| *probability)
            .sum::<f64>();
        let mut landings = 0.0_f64;
        let mut term = 1.0_f64;
        for _ in 0..=self.free_spin_cap.max(0) {
            landings += term;
            term *= free_probability;
        }
        landings
    }

    /// Expected time payout of one paid spin in seconds, including the
    /// capped free-spin chain. This is an expectation, so it is computed in
    /// floating point; the actual time credited per spin is always an integer.
    pub fn expected_payout_seconds(&self) -> f64 {
        let probabilities = self.segment_probabilities();
        let expected_single = self
            .segments
            .iter()
            .zip(&probabilities)
            .map(|(segment, probability)| segment.seconds as f64 * *probability)
            .sum::<f64>();
        expected_single * self.expected_landings(&probabilities)
    }

    /// Expected points payout (centipoints) of one paid spin, including the
    /// capped free-spin chain. Computed in floating point like the seconds
    /// expectation; the actual points credited per spin are always integer
    /// centipoints.
    pub fn expected_payout_points_centi(&self) -> f64 {
        let probabilities = self.segment_probabilities();
        let expected_single = self
            .segments
            .iter()
            .zip(&probabilities)
            .map(|(segment, probability)| segment.points_centi as f64 * *probability)
            .sum::<f64>();
        expected_single * self.expected_landings(&probabilities)
    }

    /// House edge in basis points, measured against the most generous
    /// Redemption Tier's value of the one point a spin costs. Only the
    /// seconds axis is considered — points prizes are deliberately not
    /// folded into this guardrail (ADR-0046).
    pub fn house_edge_basis_points(&self, tier: TierRate) -> Result<i64, WheelError> {
        if tier.points <= 0 || tier.seconds <= 0 {
            return Err(WheelError::InvalidRedemptionTier);
        }
        let point_value = tier.seconds as f64 / tier.points as f64;
        if point_value <= 0.0 {
            return Err(WheelError::InvalidRedemptionTier);
        }
        let edge = (point_value - self.expected_payout_seconds()) / point_value;
        Ok((edge * 10_000.0).round() as i64)
    }

    /// House edge in basis points against the paid-time value of one point
    /// under the active Shop Rate. This is an owner advisory only: the Wheel
    /// save and spin guards continue to use the most generous Redemption Tier.
    pub fn house_edge_basis_points_against_point_value(&self, point_value_seconds: i64) -> i64 {
        if point_value_seconds <= 0 {
            return 0;
        }
        // The established expectation is exposed as whole seconds in the API;
        // keep this advisory's ratio arithmetic in integer units too.
        let expected_payout_seconds = self.expected_payout_seconds().round() as i128;
        let point_value_seconds = i128::from(point_value_seconds);
        let edge = (point_value_seconds - expected_payout_seconds) * 10_000 / point_value_seconds;
        edge.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// Time value of one point in whole seconds at the most generous tier.
    pub fn point_value_seconds(&self, tier: TierRate) -> Result<i64, WheelError> {
        if tier.points <= 0 || tier.seconds <= 0 {
            return Err(WheelError::InvalidRedemptionTier);
        }
        Ok(tier.seconds / tier.points)
    }

    /// Refuses a configuration whose expected payout is not strictly less than
    /// the redemption value of its cost (the money-printer guardrail).
    /// Measured against time-axis only (ADR-0046).
    pub fn ensure_positive_house_edge(&self, tier: TierRate) -> Result<i64, WheelError> {
        if tier.points <= 0 || tier.seconds <= 0 {
            return Err(WheelError::InvalidRedemptionTier);
        }
        let point_value = tier.seconds as f64 / tier.points as f64;
        if self.expected_payout_seconds() >= point_value {
            return Err(WheelError::NegativeHouseEdge);
        }
        self.house_edge_basis_points(tier)
    }

    /// Resolves the complete capped free-spin chain for one paid spin using the
    /// injected RNG. The caller is responsible for the point deduction and the
    /// time/point credit; this only chooses the outcome.
    pub fn resolve<R: SpinRng>(&self, rng: &mut R) -> Result<SpinOutcome, WheelError> {
        self.validate()?;
        // Unset keeps the pre-ADR-0049 draw exactly: `validate` guarantees a
        // non-empty segment list, so the segment count is a positive bound and
        // the draw is the index. Only a set Bokya Rate walks the basis-point
        // table, so an upgrade cannot move an unbiased wheel's odds.
        let distribution = self
            .bokya_rate_basis_points
            .map(|_| self.segment_probability_basis_points());
        let mut steps = Vec::new();
        let mut seconds_won = 0_i64;
        let mut points_won_centi = 0_i64;
        let mut free_spins_triggered = 0_i64;

        loop {
            let index = match &distribution {
                Some(distribution) => {
                    self.pick(rng.gen_below(WHEEL_PROBABILITY_BASIS_POINTS), distribution)
                }
                None => self.pick_uniform(rng.gen_below(self.segments.len() as i64)),
            };
            let segment = &self.segments[index];
            steps.push(SpinStep {
                segment_index: index,
                seconds: segment.seconds,
                points_centi: segment.points_centi,
                free_spin: segment.free_spin,
            });
            seconds_won = seconds_won.saturating_add(segment.seconds);
            points_won_centi = points_won_centi.saturating_add(segment.points_centi);

            if segment.free_spin && free_spins_triggered < self.free_spin_cap {
                free_spins_triggered += 1;
                continue;
            }
            break;
        }

        Ok(SpinOutcome {
            steps,
            seconds_won,
            points_won_centi,
            free_spins_triggered,
        })
    }

    /// Segments are equally likely while Bokya Rate is unset, so the draw *is*
    /// the index. Clamped rather than trusted: a misbehaving `SpinRng` must not
    /// panic the spin path.
    fn pick_uniform(&self, draw: i64) -> usize {
        let last = self.segments.len() - 1;
        usize::try_from(draw).unwrap_or(0).min(last)
    }

    /// Selects through the biased distribution. Clamped rather than
    /// trusted: a misbehaving `SpinRng` must not panic the spin path.
    fn pick(&self, draw: i64, distribution: &[i64]) -> usize {
        let draw = draw.clamp(0, WHEEL_PROBABILITY_BASIS_POINTS - 1);
        let mut cumulative = 0_i64;
        distribution
            .iter()
            .position(|probability| {
                cumulative += probability;
                draw < cumulative
            })
            .unwrap_or(self.segments.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic RNG that replays a fixed sequence of draws, then panics if
    /// the chain asks for more than expected.
    struct QueuedRng {
        draws: Vec<i64>,
        position: usize,
    }

    impl QueuedRng {
        fn new(draws: Vec<i64>) -> Self {
            Self { draws, position: 0 }
        }
    }

    impl SpinRng for QueuedRng {
        fn gen_below(&mut self, bound: i64) -> i64 {
            let value = self.draws[self.position] % bound;
            self.position += 1;
            value
        }
    }

    /// Replays fixed draws and records the bound each landing asked for.
    struct BoundRecordingRng {
        draws: Vec<i64>,
        bounds: Vec<i64>,
        position: usize,
    }

    impl SpinRng for BoundRecordingRng {
        fn gen_below(&mut self, bound: i64) -> i64 {
            self.bounds.push(bound);
            let value = self.draws[self.position];
            self.position += 1;
            value
        }
    }

    /// Simple deterministic linear-congruential RNG for statistical sanity.
    struct OutOfRangeRng(i64);

    impl SpinRng for OutOfRangeRng {
        fn gen_below(&mut self, _bound: i64) -> i64 {
            self.0
        }
    }

    struct LcgRng {
        state: u64,
    }

    impl SpinRng for LcgRng {
        fn gen_below(&mut self, bound: i64) -> i64 {
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.state >> 33) % bound as u64) as i64
        }
    }

    fn segment(label: &str, seconds: i64, points_centi: i64, free_spin: bool) -> WheelSegment {
        WheelSegment {
            label: label.into(),
            seconds,
            points_centi,
            free_spin,
        }
    }

    /// Repeat a prize across `count` equal slices — the only way to express
    /// rarity now that segments carry no weight (ADR-0047).
    fn repeated(
        count: usize,
        label: &str,
        seconds: i64,
        points_centi: i64,
        free_spin: bool,
    ) -> Vec<WheelSegment> {
        vec![segment(label, seconds, points_centi, free_spin); count]
    }

    /// The seeded 12-slice default: Try again ×5, 5 min ×3, 10 min ×2,
    /// 15 min ×1, Free spin ×1.
    fn default_wheel() -> WheelConfig {
        let mut segments = Vec::new();
        segments.extend(repeated(5, "Try again", 0, 0, false));
        segments.extend(repeated(3, "5 minutes", 300, 0, false));
        segments.extend(repeated(2, "10 minutes", 600, 0, false));
        segments.extend(repeated(1, "15 minutes", 900, 0, false));
        segments.extend(repeated(1, "Free spin", 0, 0, true));
        WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 1,
            segments,
        }
    }

    #[test]
    fn uniform_distribution_uses_equal_shares() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: repeated(4, "nothing", 0, 0, false),
        };

        assert_eq!(wheel.segment_probability_basis_points(), vec![2_500; 4]);
    }

    #[test]
    fn uniform_distribution_sums_to_ten_thousand_with_remainder_at_lowest_indices() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: repeated(6, "nothing", 0, 0, false),
        };

        let distribution = wheel.segment_probability_basis_points();
        assert_eq!(distribution, vec![1_667, 1_667, 1_667, 1_667, 1_666, 1_666]);
        assert_eq!(distribution.iter().sum::<i64>(), 10_000);
    }

    #[test]
    fn bokya_distribution_splits_each_group_evenly_by_lowest_index() {
        let wheel = WheelConfig {
            bokya_rate_basis_points: Some(5_000),
            segments: vec![
                segment("bokya 1", 0, 0, false),
                segment("win 1", 60, 0, false),
                segment("bokya 2", 0, 0, false),
                segment("free spin", 0, 0, true),
                segment("bokya 3", 0, 0, false),
            ],
            ..default_wheel()
        };

        let distribution = wheel.segment_probability_basis_points();
        assert_eq!(distribution, vec![1_667, 2_500, 1_667, 2_500, 1_666]);
        assert_eq!(distribution.iter().sum::<i64>(), 10_000);
    }

    #[test]
    fn free_spin_only_segment_is_a_win_in_bokya_distribution() {
        let wheel = WheelConfig {
            bokya_rate_basis_points: Some(4_001),
            segments: vec![
                segment("bokya", 0, 0, false),
                segment("free spin", 0, 0, true),
                segment("time", 60, 0, false),
                segment("points", 0, 100, false),
            ],
            ..default_wheel()
        };

        assert_eq!(
            wheel.segment_probability_basis_points(),
            vec![4_001, 2_000, 2_000, 1_999]
        );
    }

    #[test]
    fn default_wheel_validates_and_keeps_a_positive_house_edge() {
        let wheel = default_wheel();
        assert_eq!(wheel.validate(), Ok(()));
        assert_eq!(wheel.segments.len(), 12);
        // Shared unset fixture with packages/ui/src/wheel.test.ts. Equal
        // slices: expected single = 3000/12 = 250; the free-spin slice is
        // 1 of 12, so landings = 1 + 1/12.
        let expected = wheel.expected_payout_seconds();
        assert!((expected - 270.833_333_333).abs() < 1e-6, "{expected}");
        // The owner-facing odds round the same way on both sides of the mirror.
        let mut probabilities = vec![834_i64; 4];
        probabilities.extend(vec![833_i64; 8]);
        assert_eq!(wheel.segment_probability_basis_points(), probabilities);
        // Point value at default redeem (10 pts = 14400s) = 1440s.
        let tier = TierRate {
            points: 10,
            seconds: 14_400,
        };
        let edge = wheel.house_edge_basis_points(tier).expect("house edge");
        assert_eq!(edge, 8119); // ~81.19%
        assert!(wheel.ensure_positive_house_edge(tier).is_ok());
    }

    #[test]
    fn every_segment_is_equally_likely() {
        // Four distinct slices, drawn uniformly: the draw is the index.
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: vec![
                segment("a", 10, 0, false),
                segment("b", 20, 0, false),
                segment("c", 30, 0, false),
                segment("d", 40, 0, false),
            ],
        };
        for (draw, expected_index) in [(0, 0), (1, 1), (2, 2), (3, 3)] {
            let mut rng = QueuedRng::new(vec![draw]);
            let outcome = wheel.resolve(&mut rng).expect("resolve");
            assert_eq!(outcome.steps[0].segment_index, expected_index);
        }
        // Plain mean of the four prizes.
        assert!((wheel.expected_payout_seconds() - 25.0).abs() < 1e-6);
    }

    #[test]
    fn unset_draw_is_index_based_and_exactly_uniform() {
        // Three slices: 10000 basis points cannot express 1/3, so drawing or
        // valuing an unbiased wheel through the basis-point table would move
        // its odds and its expectation. Unset must stay exactly `1/n`.
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: vec![
                segment("a", 0, 0, false),
                segment("b", 300, 0, false),
                segment("c", 600, 0, false),
            ],
        };

        let mut rng = BoundRecordingRng {
            draws: vec![2],
            bounds: Vec::new(),
            position: 0,
        };
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        assert_eq!(rng.bounds, vec![3]);
        assert_eq!(outcome.steps[0].segment_index, 2);

        // Exactly 900/3, not the 299.97 the 3334/3333/3333 rounding gives.
        assert!((wheel.expected_payout_seconds() - 300.0).abs() < 1e-9);
        // The owner-facing distribution still rounds to whole basis points.
        assert_eq!(
            wheel.segment_probability_basis_points(),
            vec![3_334, 3_333, 3_333]
        );
    }

    #[test]
    fn biased_draw_asks_for_basis_points_rather_than_segment_indices() {
        let wheel = WheelConfig {
            bokya_rate_basis_points: Some(2_000),
            free_spin_cap: 0,
            segments: vec![
                segment("bokya", 0, 0, false),
                segment("prize", 300, 0, false),
            ],
            ..default_wheel()
        };

        let mut rng = BoundRecordingRng {
            draws: vec![1_999],
            bounds: Vec::new(),
            position: 0,
        };
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        assert_eq!(rng.bounds, vec![WHEEL_PROBABILITY_BASIS_POINTS]);
        assert_eq!(outcome.steps[0].segment_index, 0);
    }

    #[test]
    fn duplicating_a_prize_is_how_rarity_is_expressed() {
        // Three "nothing" slices and one 400s slice: a 25% chance of the prize,
        // so the expectation is a quarter of it.
        let mut segments = repeated(3, "Try again", 0, 0, false);
        segments.push(segment("400s", 400, 0, false));
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments,
        };
        assert!((wheel.expected_payout_seconds() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn chained_expectations_include_combination_free_spin_prizes() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 2,
            segments: vec![
                segment("combo respin", 60, 50, true),
                segment("terminal", 120, 100, false),
            ],
        };
        // One landing pays 90s/75cp on average. With p(free)=1/2 and cap 2,
        // expected landings are 1 + 1/2 + 1/4 = 1.75.
        assert!((wheel.expected_payout_seconds() - 157.5).abs() < 1e-6);
        assert!((wheel.expected_payout_points_centi() - 131.25).abs() < 1e-6);
    }

    #[test]
    fn expected_payout_points_separate_from_seconds() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: vec![
                segment("2 pts", 0, 200, false),
                segment("nothing", 0, 0, false),
            ],
        };
        // Expected points = 100 centipoints
        assert!((wheel.expected_payout_points_centi() - 100.0).abs() < 1e-6);
        // Expected seconds = 0
        assert!((wheel.expected_payout_seconds() - 0.0).abs() < 1e-6);
        // A points-only wheel with positive time edge still validates
        let tier = TierRate {
            points: 10,
            seconds: 14_400,
        };
        assert!(wheel.ensure_positive_house_edge(tier).is_ok());
    }

    #[test]
    fn wheel_math_uses_the_selected_most_generous_tier() {
        let wheel = default_wheel();
        // 5 points for 3 hours is 2,160 seconds per point, more generous
        // than the seeded 10 points for 4 hours (1,440 seconds per point).
        let tier = TierRate {
            points: 5,
            seconds: 10_800,
        };
        assert_eq!(wheel.point_value_seconds(tier), Ok(2_160));
        // (2160 - 270.8333) / 2160 = 87.46%
        assert_eq!(wheel.house_edge_basis_points(tier), Ok(8_746));
    }

    #[test]
    fn shop_rate_point_value_advisory_can_become_unsafe_without_changing_tier_guardrail() {
        let wheel = default_wheel();

        assert!(wheel.house_edge_basis_points_against_point_value(500) > 0);
        assert!(wheel.house_edge_basis_points_against_point_value(200) < 0);
        assert!(wheel
            .ensure_positive_house_edge(TierRate {
                points: 10,
                seconds: 14_400,
            })
            .is_ok());
    }

    #[test]
    fn shared_ui_fixture_includes_biased_distribution_and_capped_free_spins() {
        let wheel = WheelConfig {
            bokya_rate_basis_points: Some(5_000),
            free_spin_cap: 2,
            segments: vec![
                segment("bokya", 0, 0, false),
                segment("free spin", 0, 0, true),
                segment("prize", 400, 200, false),
            ],
            ..default_wheel()
        };

        assert_eq!(
            wheel.segment_probability_basis_points(),
            vec![5_000, 2_500, 2_500]
        );
        // Shared with packages/ui/src/wheel.test.ts. One landing pays 100s/50cp.
        // Free-spin probability is 1/4,
        // so expected landings are 1 + 1/4 + 1/16 = 1.3125.
        assert!((wheel.expected_payout_seconds() - 131.25).abs() < 1e-6);
        assert!((wheel.expected_payout_points_centi() - 65.625).abs() < 1e-6);
    }

    #[test]
    fn raising_bokya_rate_raises_house_edge_and_lowering_it_can_be_unsafe() {
        let tier = TierRate {
            points: 10,
            seconds: 14_400,
        };
        let mut wheel = WheelConfig {
            bokya_rate_basis_points: Some(2_000),
            free_spin_cap: 0,
            segments: vec![
                segment("bokya", 0, 0, false),
                segment("prize", 2_000, 0, false),
            ],
            ..default_wheel()
        };
        let lower_edge = wheel.house_edge_basis_points(tier).expect("edge");
        assert_eq!(
            wheel.ensure_positive_house_edge(tier),
            Err(WheelError::NegativeHouseEdge)
        );

        wheel.bokya_rate_basis_points = Some(3_000);
        let higher_edge = wheel.house_edge_basis_points(tier).expect("edge");
        assert!(higher_edge > lower_edge);
        assert!(wheel.ensure_positive_house_edge(tier).is_ok());
    }

    #[test]
    fn negative_house_edge_is_refused() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: vec![
                segment("1 hour", 3_600, 0, false),
                segment("nothing", 0, 0, false),
            ],
        };
        // Expected = 1800s > 1440s point value -> money printer.
        let tier = TierRate {
            points: 10,
            seconds: 14_400,
        };
        assert_eq!(
            wheel.ensure_positive_house_edge(tier),
            Err(WheelError::NegativeHouseEdge)
        );
        let edge = wheel.house_edge_basis_points(tier).expect("edge");
        assert!(edge < 0);
    }

    #[test]
    fn biased_draw_uses_cumulative_boundaries_and_every_free_spin_redraw() {
        let wheel = WheelConfig {
            bokya_rate_basis_points: Some(2_000),
            free_spin_cap: 1,
            segments: vec![
                segment("bokya", 0, 0, false),
                segment("free spin", 0, 0, true),
                segment("prize", 60, 0, false),
            ],
            ..default_wheel()
        };

        for (draw, expected_index) in [(0, 0), (1_999, 0), (2_000, 1), (9_999, 2)] {
            let outcome = wheel
                .resolve(&mut QueuedRng::new(vec![draw, 0]))
                .expect("resolve");
            assert_eq!(outcome.steps[0].segment_index, expected_index);
        }

        // The paid landing and free re-draw both use the 20% Bokya distribution.
        let mut rng = QueuedRng::new(vec![2_000, 0]);
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(rng.position, outcome.steps.len());
        assert_eq!(outcome.steps[0].segment_index, 1);
        assert_eq!(outcome.steps[1].segment_index, 0);
    }

    #[test]
    fn free_spin_chain_is_capped() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 2,
            segments: vec![
                segment("1 min", 60, 0, false),
                segment("Free spin", 0, 0, true),
            ],
        };
        // Always draw the free-spin segment (index 1).
        let mut rng = QueuedRng::new(vec![1, 1, 1, 1, 1]);
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        // Paid spin + 2 free spins = 3 steps, all free spins, forced stop.
        assert_eq!(outcome.free_spins_triggered, 2);
        assert_eq!(outcome.steps.len(), 3);
        assert_eq!(outcome.seconds_won, 0);
        assert_eq!(outcome.points_won_centi, 0);
    }

    #[test]
    fn time_segment_ends_the_chain_and_awards_seconds() {
        let wheel = default_wheel();
        // Draw 5 -> the first "5 minutes" slice, which is terminal.
        let mut rng = QueuedRng::new(vec![5]);
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        assert_eq!(outcome.steps.len(), 1);
        assert_eq!(outcome.seconds_won, 300);
        assert_eq!(outcome.free_spins_triggered, 0);
        assert_eq!(outcome.points_won_centi, 0);
    }

    #[test]
    fn free_spin_then_time_chains_once() {
        let wheel = default_wheel();
        // Draw 11 is the lone free-spin slice; the follow-up draw 5 lands on a
        // "5 minutes" slice and ends the chain.
        let mut rng = QueuedRng::new(vec![11, 5]);
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        assert_eq!(outcome.free_spins_triggered, 1);
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(outcome.seconds_won, 300);
        assert_eq!(outcome.points_won_centi, 0);
    }

    #[test]
    fn out_of_range_draws_are_clamped_without_panicking() {
        let wheel = default_wheel();

        let below = wheel.resolve(&mut OutOfRangeRng(-1)).expect("resolve");
        assert_eq!(below.steps[0].segment_index, 0);
        let above = wheel
            .resolve(&mut OutOfRangeRng(i64::MAX))
            .expect("resolve");
        assert_eq!(above.steps[0].segment_index, 11);

        let biased = WheelConfig {
            bokya_rate_basis_points: Some(5_000),
            ..wheel
        };
        let below = biased.resolve(&mut OutOfRangeRng(-1)).expect("resolve");
        assert_eq!(below.steps[0].segment_index, 0);
        let above = biased
            .resolve(&mut OutOfRangeRng(i64::MAX))
            .expect("resolve");
        assert_eq!(above.steps[0].segment_index, 11);
    }

    #[test]
    fn segment_awards_both_time_and_points_and_free_spin() {
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 1,
            segments: vec![
                segment("10m + 5pts + respin", 600, 500, true),
                segment("nothing", 0, 0, false),
            ],
        };
        assert_eq!(wheel.validate(), Ok(()));
        // Draw 0 -> first segment (600s, 500pts, free spin), then draw 1 -> nothing
        let mut rng = QueuedRng::new(vec![0, 1]);
        let outcome = wheel.resolve(&mut rng).expect("resolve");
        assert_eq!(outcome.seconds_won, 600);
        assert_eq!(outcome.points_won_centi, 500);
        assert_eq!(outcome.free_spins_triggered, 1);
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(outcome.steps[0].seconds, 600);
        assert_eq!(outcome.steps[0].points_centi, 500);
        assert!(outcome.steps[0].free_spin);
        assert!(!outcome.steps[1].free_spin);
    }

    #[test]
    fn validation_rejects_each_impossible_bokya_rate_distinctly() {
        for rate in [-1, 10_001] {
            let wheel = WheelConfig {
                bokya_rate_basis_points: Some(rate),
                ..default_wheel()
            };
            assert_eq!(wheel.validate(), Err(WheelError::InvalidBokyaRate));
        }

        let no_bokya = WheelConfig {
            bokya_rate_basis_points: Some(1),
            segments: vec![segment("win", 60, 0, false)],
            ..default_wheel()
        };
        assert_eq!(no_bokya.validate(), Err(WheelError::NoBokyaSegment));

        let no_win = WheelConfig {
            bokya_rate_basis_points: Some(9_999),
            segments: vec![segment("bokya", 0, 0, false)],
            ..default_wheel()
        };
        assert_eq!(no_win.validate(), Err(WheelError::NoWinSegment));

        let never_bokya = WheelConfig {
            bokya_rate_basis_points: Some(0),
            segments: vec![segment("win", 60, 0, false)],
            ..default_wheel()
        };
        assert_eq!(never_bokya.validate(), Ok(()));
        assert_eq!(never_bokya.segment_probability_basis_points(), vec![10_000]);

        let always_bokya = WheelConfig {
            bokya_rate_basis_points: Some(10_000),
            segments: vec![segment("bokya", 0, 0, false)],
            ..default_wheel()
        };
        assert_eq!(always_bokya.validate(), Ok(()));
        assert_eq!(
            always_bokya.segment_probability_basis_points(),
            vec![10_000]
        );
    }

    #[test]
    fn validation_rejects_bad_segments() {
        let only_free = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 1,
            segments: vec![segment("Free spin", 0, 0, true)],
        };
        assert_eq!(only_free.validate(), Err(WheelError::NoTerminalSegment));

        let negative_points = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 1,
            segments: vec![segment("bad", 0, -1, false)],
        };
        assert_eq!(negative_points.validate(), Err(WheelError::InvalidPoints));

        let bad_seconds = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 1,
            segments: vec![segment("bad", -5, 0, false)],
        };
        assert_eq!(
            bad_seconds.validate(),
            Err(WheelError::InvalidSegmentSeconds)
        );

        for free_spin_cap in [-1, MAX_FREE_SPIN_CAP + 1] {
            let bad_cap = WheelConfig {
                free_spin_cap,
                ..default_wheel()
            };
            assert_eq!(
                bad_cap.validate(),
                Err(WheelError::InvalidFreeSpinCap(MAX_FREE_SPIN_CAP))
            );
        }

        let mut empty = default_wheel();
        empty.segments.clear();
        assert_eq!(empty.validate(), Err(WheelError::NoSegments));

        let mut too_many = default_wheel();
        too_many.segments = vec![too_many.segments[0].clone(); MAX_SEGMENTS + 1];
        assert_eq!(
            too_many.validate(),
            Err(WheelError::TooManySegments(MAX_SEGMENTS))
        );

        let mut overflowing = default_wheel();
        overflowing.segments[0].seconds = i64::MAX;
        overflowing.segments[1].seconds = i64::MAX;
        assert_eq!(overflowing.validate(), Err(WheelError::ArithmeticOverflow));
    }

    #[test]
    fn self_sustaining_points_wheel_is_not_rejected() {
        // A wheel whose expected points payout exceeds the spin cost is
        // deliberately permitted (ADR-0046). No guardrail on the points axis.
        let wheel = WheelConfig {
            enabled: true,
            bokya_rate_basis_points: None,
            free_spin_cap: 0,
            segments: vec![segment("200 cp", 0, 200, false)],
        };
        assert_eq!(wheel.validate(), Ok(()));
        // 200 centipoints expected > 100 centipoint cost — no rejection
        assert!((wheel.expected_payout_points_centi() - 200.0).abs() < 1e-6);
        // Time edge is trivially positive (0s expected payout vs 1440s point value)
        let tier = TierRate {
            points: 10,
            seconds: 14_400,
        };
        assert!(wheel.ensure_positive_house_edge(tier).is_ok());
    }

    #[test]
    fn empirical_bokya_frequency_and_payout_track_the_biased_expectation() {
        let wheel = WheelConfig {
            bokya_rate_basis_points: Some(7_000),
            free_spin_cap: 0,
            segments: vec![
                segment("bokya", 0, 0, false),
                segment("prize", 100, 50, false),
            ],
            ..default_wheel()
        };
        let mut rng = LcgRng {
            state: 0x0ddc_0ffe_e15e_beef,
        };
        let trials = 200_000_i64;
        let mut bokya = 0_i64;
        let mut seconds = 0_i64;
        let mut points_centi = 0_i64;
        for _ in 0..trials {
            let outcome = wheel.resolve(&mut rng).expect("resolve");
            bokya += i64::from(outcome.steps[0].segment_index == 0);
            seconds += outcome.seconds_won;
            points_centi += outcome.points_won_centi;
        }
        let bokya_rate = bokya as f64 / trials as f64;
        let average_seconds = seconds as f64 / trials as f64;
        let average_points_centi = points_centi as f64 / trials as f64;
        assert!((bokya_rate - 0.7).abs() < 0.01, "rate {bokya_rate}");
        assert!(
            (average_seconds - wheel.expected_payout_seconds()).abs() < 1.0,
            "average seconds {average_seconds}"
        );
        assert!(
            (average_points_centi - wheel.expected_payout_points_centi()).abs() < 0.5,
            "average centipoints {average_points_centi}"
        );
    }

    #[test]
    fn empirical_average_payout_tracks_the_expectation() {
        let mut wheel = default_wheel();
        wheel.segments[0].points_centi = 75; // a "Try again" slice
        wheel.segments[11].points_centi = 25; // the free-spin slice
        let expected_secs = wheel.expected_payout_seconds();
        let expected_pts = wheel.expected_payout_points_centi();
        let mut rng = LcgRng {
            state: 0x1234_5678_9abc_def0,
        };
        let trials = 200_000_i64;
        let mut total_secs = 0_i64;
        let mut total_pts = 0_i64;
        for _ in 0..trials {
            let o = wheel.resolve(&mut rng).expect("resolve");
            total_secs += o.seconds_won;
            total_pts += o.points_won_centi;
        }
        let avg_secs = total_secs as f64 / trials as f64;
        let avg_pts = total_pts as f64 / trials as f64;
        // Within ~3% of the analytical expectation.
        assert!(
            (avg_secs - expected_secs).abs() / expected_secs < 0.03,
            "average secs {avg_secs} vs expected {expected_secs}"
        );
        assert!(
            (avg_pts - expected_pts).abs() / expected_pts.max(1.0) < 0.03,
            "average pts {avg_pts} vs expected {expected_pts}"
        );
    }
}

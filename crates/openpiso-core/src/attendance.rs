use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    pricing::PricingError,
    shop_hours::{self, DaySchedule, ShopHoursSchedule, ShopOverride, ShopOverrideMode},
};

/// A half-open interval of absolute instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ScheduleVersion {
    pub effective_at: DateTime<Utc>,
    pub schedule: ShopHoursSchedule,
}

#[derive(Debug, Clone, Copy)]
pub struct OverrideHistory {
    pub effective_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub mode: Option<ShopOverrideMode>,
}

/// The UTC floor is durable; host clock regression cannot move authority backward.
pub fn authoritative_instant(host_utc: DateTime<Utc>, utc_floor: DateTime<Utc>) -> DateTime<Utc> {
    host_utc.max(utc_floor)
}

/// Never assign new activity to a finalized local date after a clock regression.
pub fn authoritative_local_date(
    authority_utc: DateTime<Utc>,
    offset: FixedOffset,
    open_date: NaiveDate,
) -> NaiveDate {
    authority_utc
        .with_timezone(&offset)
        .date_naive()
        .max(open_date)
}

/// Exclusive deadline after the configured number of following calendar months.
pub fn claim_deadline(
    earning_date: NaiveDate,
    months: u32,
    offset: FixedOffset,
) -> Option<DateTime<Utc>> {
    if !(1..=120).contains(&months) {
        return None;
    }
    let first = earning_date.with_day(1)?;
    let deadline = first.checked_add_months(chrono::Months::new(months.checked_add(1)?))?;
    offset
        .from_local_datetime(&deadline.and_hms_opt(0, 0, 0)?)
        .single()
        .map(|date| date.to_utc())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EarningMonthState {
    Earning,
    Claimable,
    Claimed,
    Expired,
    Forfeited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarningMonthBalance {
    pub state: EarningMonthState,
    pub earned_seconds: i64,
    pub claimed_seconds: i64,
    pub outstanding_seconds: i64,
    pub claimable_seconds: i64,
    pub expired_seconds: i64,
    pub forfeited_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttendanceEffectiveBonus {
    Finite {
        base_seconds: i64,
        basis_points: i64,
    },
    Infinite,
}

impl AttendanceEffectiveBonus {
    pub const fn is_unsafe(self) -> bool {
        matches!(self, Self::Infinite)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttendanceSettingsDraft {
    pub enabled: bool,
    pub daily_threshold_cents: i64,
    pub reward_minutes: i64,
    pub claim_window_months: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedAttendanceSettings {
    pub enabled: bool,
    pub daily_threshold_cents: i64,
    pub reward_seconds: i64,
    pub claim_window_months: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttendanceSettingsValidationError {
    #[error("daily threshold must be positive")]
    InvalidDailyThreshold,
    #[error("reward minutes must be positive")]
    InvalidRewardMinutes,
    #[error("reward minutes exceed the supported seconds range")]
    RewardSecondsOverflow,
    #[error("claim window must be between 1 and 120 months")]
    InvalidClaimWindow,
}

/// Validate an owner-authored draft and convert reward minutes to checked seconds.
pub fn validate_attendance_settings(
    draft: AttendanceSettingsDraft,
) -> Result<ValidatedAttendanceSettings, AttendanceSettingsValidationError> {
    if draft.daily_threshold_cents <= 0 {
        return Err(AttendanceSettingsValidationError::InvalidDailyThreshold);
    }
    if draft.reward_minutes <= 0 {
        return Err(AttendanceSettingsValidationError::InvalidRewardMinutes);
    }
    let reward_seconds = draft
        .reward_minutes
        .checked_mul(60)
        .ok_or(AttendanceSettingsValidationError::RewardSecondsOverflow)?;
    if !(1..=120).contains(&draft.claim_window_months) {
        return Err(AttendanceSettingsValidationError::InvalidClaimWindow);
    }

    Ok(ValidatedAttendanceSettings {
        enabled: draft.enabled,
        daily_threshold_cents: draft.daily_threshold_cents,
        reward_seconds,
        claim_window_months: u32::try_from(draft.claim_window_months)
            .map_err(|_| AttendanceSettingsValidationError::InvalidClaimWindow)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EarningMonthBalanceError {
    #[error("attendance claim window is invalid")]
    InvalidWindow,
    #[error("attendance month totals cannot be negative")]
    NegativeTotals,
    #[error("attendance month totals do not reconcile")]
    UnreconciledTotals,
}

/// Derive an Earning Month's state and mutually exclusive liability buckets.
pub fn derive_earning_month_balance(
    now: DateTime<Utc>,
    claim_starts_at: DateTime<Utc>,
    deadline_at_exclusive: DateTime<Utc>,
    earned_seconds: i64,
    claimed_seconds: i64,
    forfeited_seconds: i64,
) -> Result<EarningMonthBalance, EarningMonthBalanceError> {
    if deadline_at_exclusive <= claim_starts_at {
        return Err(EarningMonthBalanceError::InvalidWindow);
    }
    if earned_seconds < 0 || claimed_seconds < 0 || forfeited_seconds < 0 {
        return Err(EarningMonthBalanceError::NegativeTotals);
    }
    let settled_seconds = claimed_seconds
        .checked_add(forfeited_seconds)
        .ok_or(EarningMonthBalanceError::UnreconciledTotals)?;
    let outstanding_before_expiry = earned_seconds
        .checked_sub(settled_seconds)
        .filter(|seconds| *seconds >= 0)
        .ok_or(EarningMonthBalanceError::UnreconciledTotals)?;
    let expired = now >= deadline_at_exclusive;
    let outstanding_seconds = if expired {
        0
    } else {
        outstanding_before_expiry
    };
    let expired_seconds = if expired {
        outstanding_before_expiry
    } else {
        0
    };
    let claimable_seconds = if now >= claim_starts_at && !expired {
        outstanding_seconds
    } else {
        0
    };
    let state = if outstanding_seconds > 0 {
        if claimable_seconds > 0 {
            EarningMonthState::Claimable
        } else {
            EarningMonthState::Earning
        }
    } else if expired_seconds > 0 {
        EarningMonthState::Expired
    } else if forfeited_seconds > 0 {
        EarningMonthState::Forfeited
    } else if claimed_seconds > 0 {
        EarningMonthState::Claimed
    } else {
        EarningMonthState::Earning
    };
    Ok(EarningMonthBalance {
        state,
        earned_seconds,
        claimed_seconds,
        outstanding_seconds,
        claimable_seconds,
        expired_seconds,
        forfeited_seconds,
    })
}

/// Derive the informational Attendance reward bonus at the daily threshold.
/// A zero base duration has no finite percentage representation.
pub fn attendance_effective_bonus(
    daily_threshold_cents: i64,
    reward_seconds: i64,
    rate_amount_cents: i64,
    rate_seconds: i64,
) -> Result<AttendanceEffectiveBonus, PricingError> {
    if daily_threshold_cents <= 0 {
        return Err(PricingError::InvalidAmount);
    }
    if reward_seconds <= 0 {
        return Err(PricingError::InvalidEconomy);
    }
    if rate_amount_cents <= 0 || rate_seconds <= 0 {
        return Err(PricingError::InvalidRate);
    }

    let base_seconds = i128::from(daily_threshold_cents) * i128::from(rate_seconds)
        / i128::from(rate_amount_cents);
    if base_seconds == 0 {
        return Ok(AttendanceEffectiveBonus::Infinite);
    }

    let bonus_numerator = i128::from(reward_seconds) * 10_000;
    let basis_points = (bonus_numerator / base_seconds).min(i128::from(i64::MAX));
    let base_seconds = base_seconds.min(i128::from(i64::MAX));
    let basis_points = i64::try_from(basis_points).map_err(|_| PricingError::Overflow)?;
    let base_seconds = i64::try_from(base_seconds).map_err(|_| PricingError::Overflow)?;
    Ok(AttendanceEffectiveBonus::Finite {
        base_seconds,
        basis_points,
    })
}

/// Resolve completed local-date evidence from immutable schedule and override versions.
/// Versions and override history must be supplied in effective-time order.
pub fn eligible_intervals(
    date: NaiveDate,
    offset: FixedOffset,
    versions: &[ScheduleVersion],
    overrides: &[OverrideHistory],
) -> Vec<Interval> {
    let Some(start) = offset
        .from_local_datetime(&date.and_time(chrono::NaiveTime::MIN))
        .single()
    else {
        return vec![];
    };
    let Some(next) = date.succ_opt().and_then(|day| {
        offset
            .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
            .single()
    }) else {
        return vec![];
    };
    let (start, end) = (start.to_utc(), next.to_utc());
    let mut boundaries = vec![start, end];
    for version in versions {
        boundaries.push(version.effective_at);
    }
    for override_entry in overrides {
        boundaries.push(override_entry.effective_at);
        if let Some(expires_at) = override_entry.expires_at {
            boundaries.push(expires_at);
        }
    }
    for day in [date.pred_opt(), Some(date)].into_iter().flatten() {
        for version in versions {
            let Some(midnight) = offset
                .from_local_datetime(&day.and_time(chrono::NaiveTime::MIN))
                .single()
            else {
                continue;
            };
            let midnight = midnight.to_utc();
            let (opening, closing) = match version.schedule.day(day.weekday()) {
                DaySchedule::ClosedAllDay => continue,
                DaySchedule::Open24Hours => (0, 1_440),
                DaySchedule::OpenPeriod {
                    open_minute,
                    close_minute,
                } => {
                    let open_minute = i64::from(open_minute.value());
                    let close_minute = i64::from(close_minute.value());
                    (
                        open_minute,
                        close_minute
                            + if close_minute <= open_minute {
                                1_440
                            } else {
                                0
                            },
                    )
                }
            };
            boundaries.push((midnight + Duration::minutes(opening)).to_utc());
            boundaries.push((midnight + Duration::minutes(closing)).to_utc());
        }
    }
    boundaries.retain(|instant| *instant >= start && *instant <= end);
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut result: Vec<Interval> = Vec::new();
    for pair in boundaries.windows(2) {
        let at = pair[0];
        let Some(schedule) = versions
            .iter()
            .rev()
            .find(|version| version.effective_at <= at)
            .map(|version| &version.schedule)
        else {
            continue;
        };
        let shop_override = overrides
            .iter()
            .rev()
            .find(|entry| entry.effective_at <= at)
            .and_then(|entry| {
                let mode = entry.mode?;
                let expires_at = entry.expires_at?;
                (at < expires_at).then(|| ShopOverride {
                    mode,
                    expires_at: expires_at.with_timezone(&offset),
                })
            });
        let open =
            shop_hours::resolve(schedule, shop_override.as_ref(), at.with_timezone(&offset)).open;
        if open {
            if let Some(last) = result.last_mut() {
                if last.end == at {
                    last.end = pair[1];
                    continue;
                }
            }
            result.push(Interval {
                start: at,
                end: pair[1],
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, day, hour, 0, 0).unwrap()
    }

    #[test]
    fn overnight_period_tail_is_carried_into_the_next_date() {
        let mut days = [DaySchedule::ClosedAllDay; 7];
        days[0] = DaySchedule::open_period(22 * 60, 2 * 60).unwrap();
        let schedule = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days,
        };
        let versions = [ScheduleVersion {
            effective_at: instant(1, 0),
            schedule,
        }];

        assert_eq!(
            eligible_intervals(
                NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                FixedOffset::east_opt(0).unwrap(),
                &versions,
                &[],
            ),
            vec![Interval {
                start: instant(6, 0),
                end: instant(6, 2),
            }]
        );
    }

    #[test]
    fn overnight_tail_and_override_history_are_normalized() {
        let mut days = [DaySchedule::ClosedAllDay; 7];
        days[0] = DaySchedule::open_period(22 * 60, 2 * 60).unwrap();
        let schedule = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days,
        };
        let versions = [ScheduleVersion {
            effective_at: instant(1, 0),
            schedule,
        }];
        let overrides = [OverrideHistory {
            effective_at: instant(5, 23),
            expires_at: Some(instant(6, 1)),
            mode: Some(ShopOverrideMode::ForceClosed),
        }];
        assert_eq!(
            eligible_intervals(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                FixedOffset::east_opt(0).unwrap(),
                &versions,
                &overrides
            ),
            vec![Interval {
                start: instant(6, 1),
                end: instant(6, 2)
            }]
        );
        assert_eq!(
            eligible_intervals(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                FixedOffset::east_opt(0).unwrap(),
                &versions,
                &overrides
            ),
            vec![Interval {
                start: instant(5, 22),
                end: instant(5, 23)
            }]
        );
    }

    #[test]
    fn midday_override_clear_and_force_open_are_resolved_as_intervals() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut days = [DaySchedule::ClosedAllDay; 7];
        days[0] = DaySchedule::open_period(8 * 60, 18 * 60).unwrap();
        let schedule = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days,
        };
        let versions = [ScheduleVersion {
            effective_at: instant(1, 0),
            schedule,
        }];
        let overrides = [
            OverrideHistory {
                effective_at: instant(5, 10),
                expires_at: Some(instant(5, 12)),
                mode: Some(ShopOverrideMode::ForceClosed),
            },
            OverrideHistory {
                effective_at: instant(5, 11),
                expires_at: None,
                mode: None,
            },
            OverrideHistory {
                effective_at: instant(5, 20),
                expires_at: Some(instant(5, 22)),
                mode: Some(ShopOverrideMode::ForceOpen),
            },
        ];

        assert_eq!(
            eligible_intervals(
                date,
                FixedOffset::east_opt(0).unwrap(),
                &versions,
                &overrides
            ),
            vec![
                Interval {
                    start: instant(5, 8),
                    end: instant(5, 10),
                },
                Interval {
                    start: instant(5, 11),
                    end: instant(5, 18),
                },
                Interval {
                    start: instant(5, 20),
                    end: instant(5, 22),
                },
            ]
        );
    }

    #[test]
    fn disabled_hours_and_force_open_override_create_open_intervals() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let offset = FixedOffset::east_opt(0).unwrap();
        let disabled = ShopHoursSchedule {
            enabled: false,
            last_call_lead_minutes: 0,
            days: [DaySchedule::ClosedAllDay; 7],
        };
        let versions = [ScheduleVersion {
            effective_at: instant(1, 0),
            schedule: disabled,
        }];
        assert_eq!(
            eligible_intervals(date, offset, &versions, &[]),
            vec![Interval {
                start: instant(6, 0),
                end: instant(7, 0)
            }]
        );
        let closed = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days: [DaySchedule::ClosedAllDay; 7],
        };
        let versions = [ScheduleVersion {
            effective_at: instant(1, 0),
            schedule: closed,
        }];
        let overrides = [OverrideHistory {
            effective_at: instant(6, 10),
            expires_at: Some(instant(6, 13)),
            mode: Some(ShopOverrideMode::ForceOpen),
        }];
        assert_eq!(
            eligible_intervals(date, offset, &versions, &overrides),
            vec![Interval {
                start: instant(6, 10),
                end: instant(6, 13)
            }]
        );
    }

    #[test]
    fn midday_schedule_edit_to_closed_truncates_open_period() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut open_days = [DaySchedule::ClosedAllDay; 7];
        open_days[0] = DaySchedule::open_period(8 * 60, 18 * 60).unwrap();
        let open = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days: open_days,
        };
        let closed = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days: [DaySchedule::ClosedAllDay; 7],
        };
        let versions = [
            ScheduleVersion {
                effective_at: instant(1, 0),
                schedule: open,
            },
            ScheduleVersion {
                effective_at: instant(5, 12),
                schedule: closed,
            },
        ];

        assert_eq!(
            eligible_intervals(date, FixedOffset::east_opt(0).unwrap(), &versions, &[]),
            vec![Interval {
                start: instant(5, 8),
                end: instant(5, 12),
            }]
        );
    }

    #[test]
    fn midday_schedule_edit_opens_immediately() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let closed = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days: [DaySchedule::ClosedAllDay; 7],
        };
        let mut days = [DaySchedule::ClosedAllDay; 7];
        days[1] = DaySchedule::open_period(8 * 60, 18 * 60).unwrap();
        let open = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days,
        };
        let versions = [
            ScheduleVersion {
                effective_at: instant(1, 0),
                schedule: closed,
            },
            ScheduleVersion {
                effective_at: instant(6, 12),
                schedule: open,
            },
        ];
        assert_eq!(
            eligible_intervals(date, FixedOffset::east_opt(0).unwrap(), &versions, &[]),
            vec![Interval {
                start: instant(6, 12),
                end: instant(6, 18)
            }]
        );
    }

    #[test]
    fn schedule_edit_at_midnight_ends_prior_overnight_tail() {
        let mut days = [DaySchedule::ClosedAllDay; 7];
        days[0] = DaySchedule::open_period(22 * 60, 2 * 60).unwrap();
        let original = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days,
        };
        let closed = ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 0,
            days: [DaySchedule::ClosedAllDay; 7],
        };
        let versions = [
            ScheduleVersion {
                effective_at: instant(1, 0),
                schedule: original,
            },
            ScheduleVersion {
                effective_at: instant(6, 0),
                schedule: closed,
            },
        ];
        assert_eq!(
            eligible_intervals(
                NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                FixedOffset::east_opt(0).unwrap(),
                &versions,
                &[]
            ),
            vec![]
        );
    }

    #[test]
    fn deadline_crosses_leap_year_and_authority_never_regresses() {
        let jan = NaiveDate::from_ymd_opt(2024, 1, 10).unwrap();
        assert_eq!(
            claim_deadline(jan, 1, FixedOffset::east_opt(0).unwrap()).unwrap(),
            Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap()
        );
        let december = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        assert_eq!(
            claim_deadline(december, 1, offset),
            Some(Utc.with_ymd_and_hms(2026, 1, 31, 16, 0, 0).unwrap())
        );
        assert_eq!(
            claim_deadline(december, 2, offset),
            Some(Utc.with_ymd_and_hms(2026, 2, 28, 16, 0, 0).unwrap())
        );
        assert_eq!(
            authoritative_instant(instant(2, 0), instant(3, 0)),
            instant(3, 0)
        );
    }

    #[test]
    fn claim_deadline_handles_february_and_31_day_earning_dates() {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        for (earning_date, months, expected_utc) in [
            (
                NaiveDate::from_ymd_opt(2023, 2, 28).unwrap(),
                1,
                Utc.with_ymd_and_hms(2023, 3, 31, 16, 0, 0).unwrap(),
            ),
            (
                NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(),
                1,
                Utc.with_ymd_and_hms(2024, 3, 31, 16, 0, 0).unwrap(),
            ),
            (
                NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
                1,
                Utc.with_ymd_and_hms(2026, 2, 28, 16, 0, 0).unwrap(),
            ),
            (
                NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
                2,
                Utc.with_ymd_and_hms(2026, 3, 31, 16, 0, 0).unwrap(),
            ),
        ] {
            assert_eq!(
                claim_deadline(earning_date, months, offset),
                Some(expected_utc)
            );
        }
    }

    #[test]
    fn earning_month_balance_moves_unclaimed_seconds_through_its_window() {
        let claim_starts_at = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let deadline_at_exclusive = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();

        let earning = derive_earning_month_balance(
            Utc.with_ymd_and_hms(2026, 5, 20, 0, 0, 0).unwrap(),
            claim_starts_at,
            deadline_at_exclusive,
            1_800,
            600,
            300,
        )
        .unwrap();
        assert_eq!(earning.state, EarningMonthState::Earning);
        assert_eq!(earning.outstanding_seconds, 900);
        assert_eq!(earning.claimable_seconds, 0);
        assert_eq!(earning.expired_seconds, 0);

        let claimable = derive_earning_month_balance(
            Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap(),
            claim_starts_at,
            deadline_at_exclusive,
            1_800,
            600,
            300,
        )
        .unwrap();
        assert_eq!(claimable.state, EarningMonthState::Claimable);
        assert_eq!(claimable.outstanding_seconds, 900);
        assert_eq!(claimable.claimable_seconds, 900);
        assert_eq!(claimable.expired_seconds, 0);

        let expired = derive_earning_month_balance(
            deadline_at_exclusive,
            claim_starts_at,
            deadline_at_exclusive,
            1_800,
            600,
            300,
        )
        .unwrap();
        assert_eq!(expired.state, EarningMonthState::Expired);
        assert_eq!(expired.outstanding_seconds, 0);
        assert_eq!(expired.claimable_seconds, 0);
        assert_eq!(expired.expired_seconds, 900);
    }

    #[test]
    fn earning_month_balance_distinguishes_fully_claimed_and_forfeited_months() {
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let start = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let deadline = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let claimed = derive_earning_month_balance(now, start, deadline, 600, 600, 0).unwrap();
        assert_eq!(claimed.state, EarningMonthState::Claimed);
        assert_eq!(claimed.expired_seconds, 0);

        let forfeited = derive_earning_month_balance(now, start, deadline, 600, 0, 600).unwrap();
        assert_eq!(forfeited.state, EarningMonthState::Forfeited);
        assert_eq!(forfeited.forfeited_seconds, 600);
    }

    #[test]
    fn earning_month_balance_rejects_negative_or_unreconciled_totals() {
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let deadline = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        assert!(derive_earning_month_balance(now, now, deadline, 600, 700, 0).is_err());
        assert!(derive_earning_month_balance(now, now, deadline, 600, 0, -1).is_err());
        assert!(derive_earning_month_balance(now, deadline, now, 600, 0, 0).is_err());
    }

    #[test]
    fn attendance_effective_bonus_uses_floored_base_and_bonus() {
        assert_eq!(
            attendance_effective_bonus(1_001, 60, 100, 59),
            Ok(AttendanceEffectiveBonus::Finite {
                base_seconds: 590,
                basis_points: 1_016,
            })
        );
    }

    #[test]
    fn attendance_effective_bonus_marks_zero_base_as_unbounded() {
        let bonus = attendance_effective_bonus(1, 60, 100, 59).unwrap();
        assert_eq!(bonus, AttendanceEffectiveBonus::Infinite);
        assert!(bonus.is_unsafe());
    }

    #[test]
    fn attendance_effective_bonus_rejects_invalid_inputs() {
        assert_eq!(
            attendance_effective_bonus(0, 60, 100, 59),
            Err(crate::pricing::PricingError::InvalidAmount)
        );
        assert_eq!(
            attendance_effective_bonus(1_000, 0, 100, 59),
            Err(crate::pricing::PricingError::InvalidEconomy)
        );
    }

    #[test]
    fn attendance_effective_bonus_keeps_large_finite_base_finite() {
        assert_eq!(
            attendance_effective_bonus(i64::MAX, 60, 1, i64::MAX),
            Ok(AttendanceEffectiveBonus::Finite {
                base_seconds: i64::MAX,
                basis_points: 0,
            })
        );
        assert_eq!(
            attendance_effective_bonus(1, i64::MAX, 100, 100),
            Ok(AttendanceEffectiveBonus::Finite {
                base_seconds: 1,
                basis_points: i64::MAX,
            })
        );
    }

    #[test]
    fn attendance_settings_validation_rejects_invalid_drafts() {
        let cases = [
            (
                "zero daily threshold",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: 0,
                    reward_minutes: 60,
                    claim_window_months: 1,
                },
                AttendanceSettingsValidationError::InvalidDailyThreshold,
            ),
            (
                "negative daily threshold",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: -1,
                    reward_minutes: 60,
                    claim_window_months: 1,
                },
                AttendanceSettingsValidationError::InvalidDailyThreshold,
            ),
            (
                "zero reward minutes",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: 1_000,
                    reward_minutes: 0,
                    claim_window_months: 1,
                },
                AttendanceSettingsValidationError::InvalidRewardMinutes,
            ),
            (
                "negative reward minutes",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: 1_000,
                    reward_minutes: -1,
                    claim_window_months: 1,
                },
                AttendanceSettingsValidationError::InvalidRewardMinutes,
            ),
            (
                "reward minutes overflow seconds",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: 1_000,
                    reward_minutes: i64::MAX,
                    claim_window_months: 1,
                },
                AttendanceSettingsValidationError::RewardSecondsOverflow,
            ),
            (
                "claim window below minimum",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: 1_000,
                    reward_minutes: 60,
                    claim_window_months: 0,
                },
                AttendanceSettingsValidationError::InvalidClaimWindow,
            ),
            (
                "claim window above maximum",
                AttendanceSettingsDraft {
                    enabled: false,
                    daily_threshold_cents: 1_000,
                    reward_minutes: 60,
                    claim_window_months: 121,
                },
                AttendanceSettingsValidationError::InvalidClaimWindow,
            ),
        ];

        for (case, draft, expected_error) in cases {
            assert_eq!(
                validate_attendance_settings(draft),
                Err(expected_error),
                "{case}"
            );
        }
    }

    #[test]
    fn attendance_settings_validation_converts_minutes_and_has_no_threshold_cap() {
        assert_eq!(
            validate_attendance_settings(AttendanceSettingsDraft {
                enabled: true,
                daily_threshold_cents: i64::MAX,
                reward_minutes: 153_722_867_280_912_930,
                claim_window_months: 120,
            }),
            Ok(ValidatedAttendanceSettings {
                enabled: true,
                daily_threshold_cents: i64::MAX,
                reward_seconds: 9_223_372_036_854_775_800,
                claim_window_months: 120,
            })
        );
    }
}

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDateTime, NaiveTime, TimeZone, Weekday,
};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

const MINUTES_PER_DAY: u16 = 1_440;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("minute since midnight must be between 0 and 1439")]
pub struct InvalidMinuteOfDay;

/// A validated minute in the local day, from 00:00 through 23:59.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    pub fn new(value: u16) -> Result<Self, InvalidMinuteOfDay> {
        (value < MINUTES_PER_DAY)
            .then_some(Self(value))
            .ok_or(InvalidMinuteOfDay)
    }

    pub const fn value(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for MinuteOfDay {
    type Error = InvalidMinuteOfDay;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<MinuteOfDay> for u16 {
    fn from(value: MinuteOfDay) -> Self {
        value.0
    }
}

impl Serialize for MinuteOfDay {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for MinuteOfDay {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

const DAYS_PER_WEEK: usize = 7;

/// One row in a Shop Hours schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DaySchedule {
    /// An Open Period beginning on this calendar day.
    OpenPeriod {
        open_minute: MinuteOfDay,
        close_minute: MinuteOfDay,
    },
    ClosedAllDay,
    Open24Hours,
}

impl DaySchedule {
    pub fn open_period(open_minute: u16, close_minute: u16) -> Result<Self, InvalidMinuteOfDay> {
        Ok(Self::OpenPeriod {
            open_minute: MinuteOfDay::new(open_minute)?,
            close_minute: MinuteOfDay::new(close_minute)?,
        })
    }

    pub const fn closed_all_day() -> Self {
        Self::ClosedAllDay
    }

    pub const fn open_24_hours() -> Self {
        Self::Open24Hours
    }
}

/// The complete weekly Shop Hours schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopHoursSchedule {
    pub enabled: bool,
    pub last_call_lead_minutes: u16,
    /// Monday through Sunday.
    pub days: [DaySchedule; DAYS_PER_WEEK],
}

/// Short name for the weekly schedule used by the resolver.
pub type Schedule = ShopHoursSchedule;

impl ShopHoursSchedule {
    pub fn day(&self, weekday: Weekday) -> DaySchedule {
        self.days[weekday_index(weekday)]
    }
}

/// The mode of a temporary Shop Override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShopOverrideMode {
    ForceOpen,
    ForceClosed,
}

/// A temporary force with an absolute expiry instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopOverride {
    pub mode: ShopOverrideMode,
    pub expires_at: DateTime<FixedOffset>,
}

/// Why the current Shop Status has its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShopStatusReason {
    Schedule,
    Override,
}

/// The pure, computed Shop Hours result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopStatus {
    pub open: bool,
    pub next_change_at: Option<DateTime<FixedOffset>>,
    pub reason: ShopStatusReason,
    pub last_call_active: bool,
}

/// Resolve Shop Status at `now` without consulting a clock, database, or I/O.
///
/// An override is live strictly before its expiry. At the exact expiry instant
/// it is ignored and the schedule resumes. A disabled schedule keeps the shop
/// open because its schedule rows are inert; a live override still wins.
pub fn resolve(
    schedule: &ShopHoursSchedule,
    shop_override: Option<&ShopOverride>,
    now: DateTime<FixedOffset>,
) -> ShopStatus {
    if let Some(shop_override) = shop_override.filter(|value| value.expires_at > now) {
        return ShopStatus {
            open: matches!(shop_override.mode, ShopOverrideMode::ForceOpen),
            next_change_at: Some(shop_override.expires_at),
            reason: ShopStatusReason::Override,
            last_call_active: false,
        };
    }

    if !schedule.enabled {
        return ShopStatus {
            open: true,
            next_change_at: None,
            reason: ShopStatusReason::Schedule,
            last_call_active: false,
        };
    }

    let open = schedule_is_open(schedule, now);
    ShopStatus {
        open,
        next_change_at: next_scheduled_open_change(schedule, now, open),
        reason: ShopStatusReason::Schedule,
        last_call_active: open && scheduled_last_call_active(schedule, now),
    }
}

fn weekday_index(weekday: Weekday) -> usize {
    match weekday {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    }
}

fn midnight_for(now: DateTime<FixedOffset>, day_offset: i64) -> Option<DateTime<FixedOffset>> {
    let date = now
        .date_naive()
        .checked_add_signed(Duration::days(day_offset))?;
    let local_midnight = NaiveDateTime::new(date, NaiveTime::MIN);
    now.offset().from_local_datetime(&local_midnight).single()
}

fn interval_for(
    day_start: DateTime<FixedOffset>,
    day: DaySchedule,
) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    match day {
        DaySchedule::ClosedAllDay => None,
        DaySchedule::Open24Hours => Some((day_start, day_start + Duration::days(1))),
        DaySchedule::OpenPeriod {
            open_minute,
            close_minute,
        } => {
            let start = day_start + Duration::minutes(i64::from(open_minute.value()));
            let close_day_offset = i64::from(close_minute <= open_minute);
            let end = day_start
                + Duration::days(close_day_offset)
                + Duration::minutes(i64::from(close_minute.value()));
            Some((start, end))
        }
    }
}

fn schedule_is_open(schedule: &ShopHoursSchedule, now: DateTime<FixedOffset>) -> bool {
    (-1..=0).any(|day_offset| {
        let Some(day_start) = midnight_for(now, day_offset) else {
            return false;
        };
        let day = schedule.day(day_start.weekday());
        interval_for(day_start, day).is_some_and(|(start, end)| now >= start && now < end)
    })
}

fn scheduled_close_times(
    schedule: &ShopHoursSchedule,
    now: DateTime<FixedOffset>,
) -> Vec<DateTime<FixedOffset>> {
    (-1..=8)
        .filter_map(|day_offset| {
            let day_start = midnight_for(now, day_offset)?;
            let day = schedule.day(day_start.weekday());
            interval_for(day_start, day).map(|(_, end)| end)
        })
        .collect()
}

fn scheduled_last_call_active(schedule: &ShopHoursSchedule, now: DateTime<FixedOffset>) -> bool {
    if schedule.last_call_lead_minutes == 0 {
        return false;
    }

    let lead = Duration::minutes(i64::from(schedule.last_call_lead_minutes));
    scheduled_close_times(schedule, now)
        .into_iter()
        .filter(|close| *close > now)
        .filter(|close| !schedule_is_open(schedule, *close))
        .any(|close| close - now <= lead)
}

fn scheduled_boundaries(
    schedule: &ShopHoursSchedule,
    now: DateTime<FixedOffset>,
) -> Vec<DateTime<FixedOffset>> {
    let mut boundaries = (-1..=8)
        .filter_map(|day_offset| {
            let day_start = midnight_for(now, day_offset)?;
            let day = schedule.day(day_start.weekday());
            interval_for(day_start, day)
        })
        .flat_map(|(start, end)| [start, end])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}

fn next_scheduled_open_change(
    schedule: &ShopHoursSchedule,
    now: DateTime<FixedOffset>,
    current_open: bool,
) -> Option<DateTime<FixedOffset>> {
    scheduled_boundaries(schedule, now)
        .into_iter()
        .filter(|boundary| *boundary > now)
        .find(|boundary| schedule_is_open(schedule, *boundary) != current_open)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(&format!("{value}+08:00")).unwrap()
    }

    fn schedule(days: [DaySchedule; DAYS_PER_WEEK]) -> ShopHoursSchedule {
        ShopHoursSchedule {
            enabled: true,
            last_call_lead_minutes: 15,
            days,
        }
    }

    fn all_days(day: DaySchedule) -> [DaySchedule; DAYS_PER_WEEK] {
        [day; DAYS_PER_WEEK]
    }

    fn period(open: u16, close: u16) -> DaySchedule {
        DaySchedule::open_period(open, close).unwrap()
    }

    fn override_at(mode: ShopOverrideMode, expires_at: &str) -> ShopOverride {
        ShopOverride {
            mode,
            expires_at: DateTime::parse_from_rfc3339(&format!("{expires_at}+08:00")).unwrap(),
        }
    }

    #[test]
    fn normal_open_period_is_open_inside_and_closed_outside() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));

        assert!(resolve(&schedule, None, now("2026-08-03T12:00:00")).open);
        assert!(!resolve(&schedule, None, now("2026-08-03T07:59:59")).open);
    }

    #[test]
    fn period_boundaries_are_open_at_start_and_closed_at_close() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));

        assert!(resolve(&schedule, None, now("2026-08-03T08:00:00")).open);
        assert!(!resolve(&schedule, None, now("2026-08-03T22:00:00")).open);
        assert_eq!(
            resolve(&schedule, None, now("2026-08-03T21:00:00")).next_change_at,
            Some(now("2026-08-03T22:00:00"))
        );
    }

    #[test]
    fn overnight_period_owns_the_tail_across_midnight() {
        let mut days = all_days(DaySchedule::closed_all_day());
        days[4] = period(20 * 60, 2 * 60);
        let schedule = schedule(days);

        assert!(resolve(&schedule, None, now("2026-08-07T23:00:00")).open);
        assert!(resolve(&schedule, None, now("2026-08-08T01:59:59")).open);
        assert!(!resolve(&schedule, None, now("2026-08-08T02:00:00")).open);
        assert_eq!(
            resolve(&schedule, None, now("2026-08-07T23:00:00")).next_change_at,
            Some(now("2026-08-08T02:00:00"))
        );
    }

    #[test]
    fn overnight_tail_is_not_truncated_by_a_closed_following_day() {
        let mut days = all_days(DaySchedule::closed_all_day());
        days[4] = period(20 * 60, 2 * 60);
        let schedule = schedule(days);

        let status = resolve(&schedule, None, now("2026-08-08T01:00:00"));
        assert!(status.open);
        assert_eq!(status.next_change_at, Some(now("2026-08-08T02:00:00")));
    }

    #[test]
    fn closed_day_without_a_previous_tail_is_shop_closed() {
        let schedule = schedule(all_days(DaySchedule::closed_all_day()));
        let status = resolve(&schedule, None, now("2026-08-08T01:00:00"));

        assert!(!status.open);
        assert_eq!(status.next_change_at, None);
    }

    #[test]
    fn consecutive_open_24_hour_days_have_no_false_midnight_close() {
        let mut days = all_days(DaySchedule::closed_all_day());
        days[0] = DaySchedule::open_24_hours();
        days[1] = DaySchedule::open_24_hours();
        let schedule = schedule(days);

        assert!(resolve(&schedule, None, now("2026-08-03T00:00:00")).open);
        assert!(resolve(&schedule, None, now("2026-08-04T00:00:00")).open);
        assert_eq!(
            resolve(&schedule, None, now("2026-08-03T23:00:00")).next_change_at,
            Some(now("2026-08-05T00:00:00"))
        );
        assert!(!resolve(&schedule, None, now("2026-08-05T00:00:00")).open);
    }

    #[test]
    fn disabled_schedule_keeps_shop_open_and_rows_inert() {
        let schedule = ShopHoursSchedule {
            enabled: false,
            last_call_lead_minutes: 15,
            days: all_days(DaySchedule::closed_all_day()),
        };
        let status = resolve(&schedule, None, now("2026-08-03T12:00:00"));

        assert!(status.open);
        assert_eq!(status.reason, ShopStatusReason::Schedule);
        assert_eq!(status.next_change_at, None);
        assert!(!status.last_call_active);
    }

    #[test]
    fn live_force_open_override_opens_a_scheduled_closed_period() {
        let schedule = schedule(all_days(DaySchedule::closed_all_day()));
        let force_open = override_at(ShopOverrideMode::ForceOpen, "2026-08-03T18:00:00");
        let status = resolve(&schedule, Some(&force_open), now("2026-08-03T12:00:00"));

        assert!(status.open);
        assert_eq!(status.reason, ShopStatusReason::Override);
    }

    #[test]
    fn live_overrides_win_and_expiry_returns_to_schedule() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));
        let force_open = override_at(ShopOverrideMode::ForceOpen, "2026-08-03T18:00:00");
        let force_closed = override_at(ShopOverrideMode::ForceClosed, "2026-08-03T18:00:00");

        let opened = resolve(&schedule, Some(&force_open), now("2026-08-03T17:00:00"));
        assert!(opened.open);
        assert_eq!(opened.reason, ShopStatusReason::Override);
        assert_eq!(opened.next_change_at, Some(now("2026-08-03T18:00:00")));
        assert!(!opened.last_call_active);

        let closed = resolve(&schedule, Some(&force_closed), now("2026-08-03T12:00:00"));
        assert!(!closed.open);
        assert_eq!(closed.reason, ShopStatusReason::Override);

        let resumed = resolve(&schedule, Some(&force_open), now("2026-08-03T18:00:00"));
        assert!(resumed.open);
        assert_eq!(resumed.reason, ShopStatusReason::Schedule);

        let disabled = ShopHoursSchedule {
            enabled: false,
            last_call_lead_minutes: 15,
            days: all_days(DaySchedule::open_24_hours()),
        };
        let disabled_status = resolve(&disabled, Some(&force_closed), now("2026-08-03T17:00:00"));
        assert!(!disabled_status.open);
        assert_eq!(disabled_status.reason, ShopStatusReason::Override);
    }

    #[test]
    fn invalid_open_period_minutes_are_rejected() {
        assert_eq!(DaySchedule::open_period(1_440, 0), Err(InvalidMinuteOfDay));
    }

    #[test]
    fn expired_override_is_ignored_and_exact_expiry_is_expired() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));
        let expired = override_at(ShopOverrideMode::ForceClosed, "2026-08-03T12:00:00");

        let at_expiry = resolve(&schedule, Some(&expired), now("2026-08-03T12:00:00"));
        assert!(at_expiry.open);
        assert_eq!(at_expiry.reason, ShopStatusReason::Schedule);

        let after_expiry = resolve(&schedule, Some(&expired), now("2026-08-03T12:00:01"));
        assert!(after_expiry.open);
        assert_eq!(after_expiry.reason, ShopStatusReason::Schedule);
    }

    #[test]
    fn override_expiry_is_the_next_change_when_nearer_than_schedule_boundary() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));
        let force_open = override_at(ShopOverrideMode::ForceOpen, "2026-08-03T18:00:00");
        let status = resolve(&schedule, Some(&force_open), now("2026-08-03T12:00:00"));

        assert_eq!(status.next_change_at, Some(now("2026-08-03T18:00:00")));
    }

    #[test]
    fn last_call_is_active_before_a_scheduled_close_only_with_a_positive_lead() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));
        assert!(resolve(&schedule, None, now("2026-08-03T21:45:00")).last_call_active);
        assert!(!resolve(&schedule, None, now("2026-08-03T21:44:59")).last_call_active);
        assert!(!resolve(&schedule, None, now("2026-08-03T22:00:00")).last_call_active);

        let mut no_lead = schedule;
        no_lead.last_call_lead_minutes = 0;
        assert!(!resolve(&no_lead, None, now("2026-08-03T21:59:00")).last_call_active);
    }

    #[test]
    fn last_call_is_not_active_during_a_force_close() {
        let schedule = schedule(all_days(period(8 * 60, 22 * 60)));
        let force_closed = override_at(ShopOverrideMode::ForceClosed, "2026-08-03T22:00:00");

        let status = resolve(&schedule, Some(&force_closed), now("2026-08-03T21:50:00"));
        assert!(!status.open);
        assert!(!status.last_call_active);
    }

    #[test]
    fn next_change_finds_the_next_open_period_after_midnight() {
        let mut days = all_days(DaySchedule::closed_all_day());
        days[5] = period(8 * 60, 12 * 60);
        let schedule = schedule(days);

        let status = resolve(&schedule, None, now("2026-08-07T23:00:00"));
        assert!(!status.open);
        assert_eq!(status.next_change_at, Some(now("2026-08-08T08:00:00")));
    }
}

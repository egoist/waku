//! The clock-free schedule core.
//!
//! Every function takes an injected reference time as a local-wall-clock
//! [`NaiveDateTime`], so results are deterministic and independent of the host
//! timezone. Callers convert a stored unix timestamp to local wall clock at the
//! boundary (see the scheduler tick) and back again for storage.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use super::{Schedule, TimeOfDay};

/// How far the monthly search walks before giving up. A year and a bit covers
/// any reachable day-of-month, including a February-only 29th.
const MONTH_SEARCH_LIMIT: u32 = 14;

impl TimeOfDay {
    fn naive(self) -> Option<NaiveTime> {
        NaiveTime::from_hms_opt(u32::from(self.hour), u32::from(self.minute), 0)
    }
}

/// The first occurrence of `schedule` strictly after `after`, in local wall
/// clock.
///
/// Returns `None` when the schedule can never fire (an empty weekly/monthly
/// selection) or on a malformed time — callers treat that as "no next run" and
/// degrade gracefully. A slot whose time already passed on `after`'s day rolls
/// forward to the next valid day; a monthly day a month lacks clamps to that
/// month's last day.
pub fn next_occurrence(schedule: &Schedule, after: NaiveDateTime) -> Option<NaiveDateTime> {
    match schedule {
        // Manual automations never fire on their own.
        Schedule::Manual => None,
        Schedule::Hourly { minute } => {
            let minute = u32::from(*minute).min(59);
            // The `:minute` slot in the current hour, then walk hours forward
            // until it lands strictly after `after` (covers the hour and day
            // rollover in one loop).
            let mut slot = after.date().and_hms_opt(after.hour(), minute, 0)?;
            while slot <= after {
                slot += chrono::Duration::hours(1);
            }
            Some(slot)
        }
        Schedule::Daily { .. } => {
            let time = schedule.time()?.naive()?;
            let today = after.date().and_time(time);
            if today > after {
                Some(today)
            } else {
                Some(after.date().succ_opt()?.and_time(time))
            }
        }
        Schedule::Weekly { weekdays, .. } => {
            let time = schedule.time()?.naive()?;
            if weekdays.is_empty() {
                return None;
            }
            let mut date = after.date();
            // At most eight days: the next matching weekday is within seven, and
            // one extra covers a same-day slot that already passed.
            for _ in 0..8 {
                if weekdays.iter().any(|day| day.chrono() == date.weekday()) {
                    let candidate = date.and_time(time);
                    if candidate > after {
                        return Some(candidate);
                    }
                }
                date = date.succ_opt()?;
            }
            None
        }
        Schedule::Monthly { days, .. } => {
            let time = schedule.time()?.naive()?;
            if days.is_empty() {
                return None;
            }
            let mut year = after.year();
            let mut month = after.month();
            for _ in 0..MONTH_SEARCH_LIMIT {
                let last = last_day_of_month(year, month);
                let mut best: Option<NaiveDateTime> = None;
                for &day in days {
                    let clamped = u32::from(day).clamp(1, last);
                    let Some(date) = NaiveDate::from_ymd_opt(year, month, clamped) else {
                        continue;
                    };
                    let candidate = date.and_time(time);
                    if candidate > after {
                        best = Some(best.map_or(candidate, |current| current.min(candidate)));
                    }
                }
                if let Some(candidate) = best {
                    return Some(candidate);
                }
                (year, month) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
            }
            None
        }
    }
}

/// Whether a due occurrence elapsed at or before `now`, given the reference
/// `marker` (the last run, or the automation's baseline before it has ever run).
///
/// A [`Due`] result carries whether the elapsed occurrence is stale enough — by
/// more than `catch_up_grace` — to count as a catch-up. `catch_up_grace`
/// absorbs the tick cadence so an on-time fire a few seconds late is not
/// mislabeled a catch-up.
pub fn due_state(
    schedule: &Schedule,
    marker: NaiveDateTime,
    now: NaiveDateTime,
    catch_up_grace: chrono::Duration,
) -> Option<Due> {
    let occurrence = next_occurrence(schedule, marker)?;
    if occurrence > now {
        return None;
    }
    let catch_up = now.signed_duration_since(occurrence) > catch_up_grace;
    Some(Due {
        occurrence,
        catch_up,
    })
}

/// A due occurrence and whether firing it counts as a catch-up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Due {
    pub occurrence: NaiveDateTime,
    pub catch_up: bool,
}

/// The last calendar day of `month` in `year` (28-31).
fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first| first.pred_opt())
        .map_or(28, |last| last.day())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::Weekday;

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    fn daily(hour: u8, minute: u8) -> Schedule {
        Schedule::Daily {
            time: TimeOfDay::new(hour, minute),
        }
    }

    #[test]
    fn manual_never_fires() {
        assert_eq!(next_occurrence(&Schedule::Manual, at(2026, 8, 13, 8, 0)), None);
    }

    #[test]
    fn hourly_finds_the_next_minute_slot() {
        let schedule = Schedule::Hourly { minute: 15 };
        // Before this hour's slot: fires this hour.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 9, 0)),
            Some(at(2026, 8, 13, 9, 15))
        );
        // After this hour's slot: rolls to the next hour.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 9, 30)),
            Some(at(2026, 8, 13, 10, 15))
        );
        // Exactly at the slot: strictly-after means the next hour.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 9, 15)),
            Some(at(2026, 8, 13, 10, 15))
        );
    }

    #[test]
    fn hourly_crosses_a_day_boundary() {
        let schedule = Schedule::Hourly { minute: 30 };
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 23, 45)),
            Some(at(2026, 8, 14, 0, 30))
        );
    }

    #[test]
    fn daily_rolls_a_passed_slot_to_the_next_day() {
        let schedule = daily(9, 0);
        // Before today's slot: fires today.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 8, 0)),
            Some(at(2026, 8, 13, 9, 0))
        );
        // After today's slot: rolls to tomorrow.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 9, 30)),
            Some(at(2026, 8, 14, 9, 0))
        );
        // Exactly at the slot: strictly-after means tomorrow.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 9, 0)),
            Some(at(2026, 8, 14, 9, 0))
        );
    }

    #[test]
    fn daily_crosses_a_month_and_year_boundary() {
        let schedule = daily(0, 30);
        assert_eq!(
            next_occurrence(&schedule, at(2026, 1, 31, 23, 0)),
            Some(at(2026, 2, 1, 0, 30))
        );
        assert_eq!(
            next_occurrence(&schedule, at(2026, 12, 31, 23, 0)),
            Some(at(2027, 1, 1, 0, 30))
        );
    }

    #[test]
    fn weekly_single_weekday_finds_the_next_matching_day() {
        // 2026-08-13 is a Thursday. A Monday schedule after Thursday lands on
        // the following Monday.
        let schedule = Schedule::Weekly {
            time: TimeOfDay::new(9, 0),
            weekdays: vec![Weekday::Monday],
        };
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 12, 0)),
            Some(at(2026, 8, 17, 9, 0))
        );
    }

    #[test]
    fn weekly_same_weekday_before_and_after_the_slot() {
        // 2026-08-13 is a Thursday.
        let schedule = Schedule::Weekly {
            time: TimeOfDay::new(9, 0),
            weekdays: vec![Weekday::Thursday],
        };
        // Before the slot on the matching day: today.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 8, 0)),
            Some(at(2026, 8, 13, 9, 0))
        );
        // After the slot on the matching day: a week later.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 10, 0)),
            Some(at(2026, 8, 20, 9, 0))
        );
    }

    #[test]
    fn weekly_multiple_weekdays_pick_the_nearest() {
        // Thursday 2026-08-13. Mon+Wed+Fri: next is Friday the 14th.
        let schedule = Schedule::Weekly {
            time: TimeOfDay::new(9, 0),
            weekdays: vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday],
        };
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 13, 12, 0)),
            Some(at(2026, 8, 14, 9, 0))
        );
    }

    #[test]
    fn weekly_with_no_weekdays_never_fires() {
        let schedule = Schedule::Weekly {
            time: TimeOfDay::new(9, 0),
            weekdays: vec![],
        };
        assert_eq!(next_occurrence(&schedule, at(2026, 8, 13, 12, 0)), None);
    }

    #[test]
    fn monthly_single_day_rolls_to_next_month_when_passed() {
        let schedule = Schedule::Monthly {
            time: TimeOfDay::new(9, 0),
            days: vec![15],
        };
        // Before the 15th: this month.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 10, 0, 0)),
            Some(at(2026, 8, 15, 9, 0))
        );
        // After the 15th: next month.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 8, 20, 0, 0)),
            Some(at(2026, 9, 15, 9, 0))
        );
    }

    #[test]
    fn monthly_clamps_the_31st_to_a_short_month() {
        let schedule = Schedule::Monthly {
            time: TimeOfDay::new(9, 0),
            days: vec![31],
        };
        // From mid-February 2026 (28 days): clamps to the 28th.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 2, 10, 0, 0)),
            Some(at(2026, 2, 28, 9, 0))
        );
        // April has 30 days: clamps to the 30th.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 4, 10, 0, 0)),
            Some(at(2026, 4, 30, 9, 0))
        );
        // A 31-day month keeps the 31st.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 3, 10, 0, 0)),
            Some(at(2026, 3, 31, 9, 0))
        );
    }

    #[test]
    fn monthly_clamps_the_29th_in_a_non_leap_february() {
        let schedule = Schedule::Monthly {
            time: TimeOfDay::new(9, 0),
            days: vec![29],
        };
        // 2026 is not a leap year: the 29th clamps to the 28th.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 2, 1, 0, 0)),
            Some(at(2026, 2, 28, 9, 0))
        );
        // 2028 is a leap year: the 29th stays.
        assert_eq!(
            next_occurrence(&schedule, at(2028, 2, 1, 0, 0)),
            Some(at(2028, 2, 29, 9, 0))
        );
    }

    #[test]
    fn monthly_multiple_days_pick_the_nearest_and_dedupe_clamps() {
        let schedule = Schedule::Monthly {
            time: TimeOfDay::new(9, 0),
            days: vec![1, 30, 31],
        };
        // From Feb 10 2026: both 30 and 31 clamp to the 28th, so the nearest
        // future occurrence is Feb 28.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 2, 10, 0, 0)),
            Some(at(2026, 2, 28, 9, 0))
        );
        // From Feb 28 after the slot: the next is the 1st of March.
        assert_eq!(
            next_occurrence(&schedule, at(2026, 2, 28, 12, 0)),
            Some(at(2026, 3, 1, 9, 0))
        );
    }

    #[test]
    fn due_state_reports_nothing_before_the_first_slot() {
        let schedule = daily(9, 0);
        // Marker just created; now is before the next slot.
        let due = due_state(
            &schedule,
            at(2026, 8, 13, 8, 0),
            at(2026, 8, 13, 8, 30),
            chrono::Duration::minutes(5),
        );
        assert_eq!(due, None);
    }

    #[test]
    fn due_state_reports_an_on_time_fire_without_catch_up() {
        let schedule = daily(9, 0);
        // Marker yesterday; now is one minute past today's slot (within grace).
        let due = due_state(
            &schedule,
            at(2026, 8, 12, 9, 0),
            at(2026, 8, 13, 9, 1),
            chrono::Duration::minutes(5),
        )
        .unwrap();
        assert_eq!(due.occurrence, at(2026, 8, 13, 9, 0));
        assert!(!due.catch_up);
    }

    #[test]
    fn due_state_flags_a_stale_occurrence_as_catch_up() {
        let schedule = daily(9, 0);
        // Marker two days ago; now is well past the slot (app was closed).
        let due = due_state(
            &schedule,
            at(2026, 8, 11, 9, 0),
            at(2026, 8, 13, 14, 0),
            chrono::Duration::minutes(5),
        )
        .unwrap();
        // The first occurrence after the marker is Aug 12 9:00 — coalescing to
        // a single run is the planner's job; here we just report the earliest.
        assert_eq!(due.occurrence, at(2026, 8, 12, 9, 0));
        assert!(due.catch_up);
    }

    #[test]
    fn due_state_is_none_for_a_schedule_that_cannot_fire() {
        let schedule = Schedule::Weekly {
            time: TimeOfDay::new(9, 0),
            weekdays: vec![],
        };
        let due = due_state(
            &schedule,
            at(2026, 8, 1, 9, 0),
            at(2026, 8, 13, 9, 0),
            chrono::Duration::minutes(5),
        );
        assert_eq!(due, None);
    }
}

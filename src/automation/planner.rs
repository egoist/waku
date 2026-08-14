//! The clock-free firing planner.
//!
//! Given a snapshot of automations, the current per-automation active-run state,
//! and an injected `now`, [`plan`] returns concrete fire / skip decisions. It
//! resolves the overlap policy, coalesces catch-up to at most one run per
//! automation (the schedule core reports the earliest missed occurrence; firing
//! advances the marker to `now`, so a burst can never form), and never fires a
//! disabled automation. It holds no clock and no schedule math of its own beyond
//! calling [`super::schedule`].

use chrono::NaiveDateTime;
use uuid::Uuid;

use super::schedule::due_state;
use super::{OverlapPolicy, Schedule};

/// One automation's state, as the planner needs to see it. Decoupled from the
/// stored [`super::Automation`] so the planner stays pure and trivially
/// testable.
#[derive(Clone, Debug)]
pub struct AutomationTick {
    pub id: Uuid,
    pub enabled: bool,
    pub schedule: Schedule,
    /// The scheduling reference: the last run, or the automation's baseline
    /// before it has ever run — already converted to local wall clock.
    pub marker: NaiveDateTime,
    pub overlap: OverlapPolicy,
    /// Whether a prior run of this automation is still active.
    pub active: bool,
}

/// What the planner decided for one automation this tick. An automation with no
/// due occurrence — or one deferred by [`OverlapPolicy::Queue`] while active —
/// produces no decision at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanDecision {
    /// Spawn a run now. `catch_up` marks a coalesced missed occurrence.
    Fire { id: Uuid, catch_up: bool },
    /// Consume the occurrence without running it — the overlap policy skipped it
    /// against an active run. Recorded in history as skipped.
    Skip { id: Uuid, catch_up: bool },
}

impl PlanDecision {
    pub fn id(self) -> Uuid {
        match self {
            Self::Fire { id, .. } | Self::Skip { id, .. } => id,
        }
    }
}

/// Resolve every automation's decision for this tick.
///
/// `catch_up_grace` absorbs the tick cadence when distinguishing an on-time fire
/// from a catch-up (see [`due_state`]).
pub fn plan(
    automations: &[AutomationTick],
    now: NaiveDateTime,
    catch_up_grace: chrono::Duration,
) -> Vec<PlanDecision> {
    automations
        .iter()
        .filter_map(|automation| decide(automation, now, catch_up_grace))
        .collect()
}

fn decide(
    automation: &AutomationTick,
    now: NaiveDateTime,
    catch_up_grace: chrono::Duration,
) -> Option<PlanDecision> {
    if !automation.enabled {
        return None;
    }
    let due = due_state(&automation.schedule, automation.marker, now, catch_up_grace)?;
    let id = automation.id;
    let catch_up = due.catch_up;

    if !automation.active {
        return Some(PlanDecision::Fire { id, catch_up });
    }

    // A prior run is still active: the overlap policy decides.
    match automation.overlap {
        OverlapPolicy::Concurrent => Some(PlanDecision::Fire { id, catch_up }),
        OverlapPolicy::Skip => Some(PlanDecision::Skip { id, catch_up }),
        // Defer: leave the occurrence pending (marker unchanged) so it fires on
        // a later tick once the active run clears.
        OverlapPolicy::Queue => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::TimeOfDay;
    use chrono::NaiveDate;

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

    fn tick(
        id: Uuid,
        marker: NaiveDateTime,
        overlap: OverlapPolicy,
        active: bool,
    ) -> AutomationTick {
        AutomationTick {
            id,
            enabled: true,
            schedule: daily(9, 0),
            marker,
            overlap,
            active,
        }
    }

    const GRACE: chrono::Duration = chrono::Duration::minutes(5);

    #[test]
    fn a_due_idle_automation_fires() {
        let id = Uuid::new_v4();
        let ticks = vec![tick(id, at(2026, 8, 12, 9, 0), OverlapPolicy::Skip, false)];
        let decisions = plan(&ticks, at(2026, 8, 13, 9, 1), GRACE);
        assert_eq!(
            decisions,
            vec![PlanDecision::Fire {
                id,
                catch_up: false
            }]
        );
    }

    #[test]
    fn a_not_yet_due_automation_produces_no_decision() {
        let id = Uuid::new_v4();
        let ticks = vec![tick(id, at(2026, 8, 13, 8, 0), OverlapPolicy::Skip, false)];
        // now is before today's 9:00 slot.
        let decisions = plan(&ticks, at(2026, 8, 13, 8, 30), GRACE);
        assert!(decisions.is_empty());
    }

    #[test]
    fn a_disabled_automation_never_fires_even_when_due() {
        let id = Uuid::new_v4();
        let mut disabled = tick(id, at(2026, 8, 12, 9, 0), OverlapPolicy::Skip, false);
        disabled.enabled = false;
        let decisions = plan(&[disabled], at(2026, 8, 13, 9, 1), GRACE);
        assert!(decisions.is_empty());
    }

    #[test]
    fn skip_policy_skips_against_an_active_run() {
        let id = Uuid::new_v4();
        let ticks = vec![tick(id, at(2026, 8, 12, 9, 0), OverlapPolicy::Skip, true)];
        let decisions = plan(&ticks, at(2026, 8, 13, 9, 1), GRACE);
        assert_eq!(
            decisions,
            vec![PlanDecision::Skip {
                id,
                catch_up: false
            }]
        );
    }

    #[test]
    fn queue_policy_defers_against_an_active_run() {
        let id = Uuid::new_v4();
        let ticks = vec![tick(id, at(2026, 8, 12, 9, 0), OverlapPolicy::Queue, true)];
        let decisions = plan(&ticks, at(2026, 8, 13, 9, 1), GRACE);
        // Deferred: no decision, marker untouched so it re-evaluates next tick.
        assert!(decisions.is_empty());
    }

    #[test]
    fn concurrent_policy_fires_against_an_active_run() {
        let id = Uuid::new_v4();
        let ticks = vec![tick(
            id,
            at(2026, 8, 12, 9, 0),
            OverlapPolicy::Concurrent,
            true,
        )];
        let decisions = plan(&ticks, at(2026, 8, 13, 9, 1), GRACE);
        assert_eq!(
            decisions,
            vec![PlanDecision::Fire {
                id,
                catch_up: false
            }]
        );
    }

    #[test]
    fn any_policy_fires_normally_when_no_run_is_active() {
        for overlap in [
            OverlapPolicy::Skip,
            OverlapPolicy::Queue,
            OverlapPolicy::Concurrent,
        ] {
            let id = Uuid::new_v4();
            let ticks = vec![tick(id, at(2026, 8, 12, 9, 0), overlap, false)];
            let decisions = plan(&ticks, at(2026, 8, 13, 9, 1), GRACE);
            assert_eq!(
                decisions,
                vec![PlanDecision::Fire {
                    id,
                    catch_up: false
                }],
                "overlap {overlap:?} should fire when idle"
            );
        }
    }

    #[test]
    fn a_stale_missed_occurrence_fires_once_as_catch_up() {
        let id = Uuid::new_v4();
        // Marker two days ago, now well past today's slot: the app was closed.
        let ticks = vec![tick(id, at(2026, 8, 11, 9, 0), OverlapPolicy::Skip, false)];
        let decisions = plan(&ticks, at(2026, 8, 13, 14, 0), GRACE);
        // Exactly one fire, flagged catch-up — never a burst.
        assert_eq!(decisions, vec![PlanDecision::Fire { id, catch_up: true }]);
    }

    #[test]
    fn plan_covers_a_mixed_batch_of_automations() {
        let due_idle = Uuid::new_v4();
        let due_active_skip = Uuid::new_v4();
        let not_due = Uuid::new_v4();
        let ticks = vec![
            tick(due_idle, at(2026, 8, 12, 9, 0), OverlapPolicy::Skip, false),
            tick(
                due_active_skip,
                at(2026, 8, 12, 9, 0),
                OverlapPolicy::Skip,
                true,
            ),
            // Already ran at 9:03 today: its next slot is tomorrow.
            tick(not_due, at(2026, 8, 13, 9, 3), OverlapPolicy::Skip, false),
        ];
        // 9:03, three minutes past the 9:00 slot — inside the grace window, so
        // the two due automations fire/skip on time rather than as catch-ups.
        let decisions = plan(&ticks, at(2026, 8, 13, 9, 3), GRACE);
        // not_due already ran today; due_idle fires; due_active_skip skips.
        assert!(!decisions.iter().any(|decision| decision.id() == not_due));
        assert!(decisions.iter().any(|decision| *decision
            == PlanDecision::Fire {
                id: due_idle,
                catch_up: false
            }));
        assert!(decisions.iter().any(|decision| *decision
            == PlanDecision::Skip {
                id: due_active_skip,
                catch_up: false
            }));
    }
}

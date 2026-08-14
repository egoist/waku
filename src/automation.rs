//! Automations: named, saved prompts that spawn agent sessions on a schedule.
//!
//! This module owns the durable domain model (this file) plus two pure,
//! clock-free cores that both the Automations page and the scheduler tick
//! consume:
//!
//! - [`schedule`] computes the next occurrence of a schedule and whether a due
//!   occurrence elapsed while nothing ran (catch-up owed).
//! - [`planner`] maps a set of automations plus the current active-run state to
//!   concrete fire / skip decisions, resolving the overlap policy and coalescing
//!   catch-up to at most one run per automation.
//!
//! Neither core touches GPUI, the database, or a real clock — every input is
//! injected, so the whole scheduling brain is verifiable through unit tests.
//! The domain types here are plain serde structs that round-trip through a JSON
//! blob in the state store (see `persistence::PersistedState::automations`).

pub mod planner;
pub mod schedule;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{InteractionMode, ProviderKind, RuntimeMode, SessionWorkspace};

/// The most run-history entries kept per automation. Appending past this bound
/// drops the oldest entry so the blob a save rewrites stays small.
pub const MAX_HISTORY: usize = 50;

/// A named, saved prompt with an agent configuration and a schedule.
///
/// Persisted whole as a JSON blob keyed by [`Automation::id`]. The schedule is a
/// tagged enum ([`Schedule`]) so a raw-cron variant can be added later without
/// migrating existing rows.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Automation {
    pub id: Uuid,
    pub name: String,
    pub prompt: String,
    pub agent: AutomationAgent,
    /// Project this automation binds its runs to. `None` runs projectless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    /// Filesystem context each run uses. Only [`SessionWorkspace::Local`] and
    /// [`SessionWorkspace::NewWorktree`] are meaningful here — a stored
    /// `NewWorktree` materializes a fresh worktree on every run.
    #[serde(default, skip_serializing_if = "SessionWorkspace::is_local")]
    pub workspace: SessionWorkspace,
    pub schedule: Schedule,
    #[serde(default)]
    pub overlap: OverlapPolicy,
    #[serde(default)]
    pub notification: NotificationConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Creation time, unix seconds. Doubles as the scheduling baseline until the
    /// first run, so a fresh automation never immediately catch-up fires.
    pub created_at: u64,
    /// Any mutation, unix seconds.
    pub updated_at: u64,
    /// When the automation last fired (or was intentionally skipped), unix
    /// seconds. `None` until it has ever run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<u64>,
    /// Newest-first, capped at [`MAX_HISTORY`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<AutomationRun>,
}

impl Automation {
    /// A new automation with sensible defaults, stamped at `now` (unix seconds).
    pub fn new(name: impl Into<String>, provider: ProviderKind, now: u64) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            prompt: String::new(),
            agent: AutomationAgent::new(provider),
            project_id: None,
            workspace: SessionWorkspace::Local,
            schedule: Schedule::default(),
            overlap: OverlapPolicy::default(),
            notification: NotificationConfig::default(),
            enabled: true,
            created_at: now,
            updated_at: now,
            last_run_at: None,
            history: Vec::new(),
        }
    }

    /// Prepend a run to the history and trim to [`MAX_HISTORY`], keeping the
    /// newest entries.
    pub fn record_run(&mut self, run: AutomationRun) {
        self.history.insert(0, run);
        self.history.truncate(MAX_HISTORY);
    }

    /// Update the outcome (and, if newly known, the spawned-task link) of the
    /// history entry for `run_id`. Called when a spawned run completes.
    pub fn resolve_run(&mut self, run_id: Uuid, outcome: RunOutcome, session_id: Option<Uuid>) {
        if let Some(entry) = self.history.iter_mut().find(|entry| entry.id == run_id) {
            entry.outcome = outcome;
            if session_id.is_some() {
                entry.session_id = session_id;
            }
        }
    }
}

/// The agent configuration a run is spawned with. Mirrors the config fields of
/// [`crate::model::AgentSession`] so it drops straight onto a spawned session.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AutomationAgent {
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
    /// Permission mode. Whatever the user configured — no forced auto-approve.
    #[serde(default)]
    pub runtime_mode: RuntimeMode,
    #[serde(default)]
    pub interaction_mode: InteractionMode,
}

impl AutomationAgent {
    pub fn new(provider: ProviderKind) -> Self {
        Self {
            provider,
            model: None,
            reasoning_effort: None,
            service_tier: None,
            agent_preset: None,
            runtime_mode: RuntimeMode::default(),
            interaction_mode: InteractionMode::default(),
        }
    }
}

/// Local-time wall clock, no date. Interpreted in the machine's local timezone
/// at the scheduling boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    pub const fn new(hour: u8, minute: u8) -> Self {
        Self { hour, minute }
    }
}

impl Default for TimeOfDay {
    /// 9:00 AM local — a reasonable default slot for a daily run.
    fn default() -> Self {
        Self::new(9, 0)
    }
}

/// A day of the week, Monday-first. Kept independent of `chrono::Weekday` so its
/// serialized spelling is stable regardless of chrono's serde feature.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    pub fn chrono(self) -> chrono::Weekday {
        match self {
            Self::Monday => chrono::Weekday::Mon,
            Self::Tuesday => chrono::Weekday::Tue,
            Self::Wednesday => chrono::Weekday::Wed,
            Self::Thursday => chrono::Weekday::Thu,
            Self::Friday => chrono::Weekday::Fri,
            Self::Saturday => chrono::Weekday::Sat,
            Self::Sunday => chrono::Weekday::Sun,
        }
    }
}

/// When an automation fires. A tagged enum so a raw-cron variant can be added
/// later without migrating stored rows.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Schedule {
    /// Never fires on its own; runs only when the user starts it (Run-now).
    Manual,
    /// Every hour at `minute` past the hour.
    Hourly { minute: u8 },
    /// Every day at `time`.
    Daily { time: TimeOfDay },
    /// On each selected weekday at `time`.
    Weekly {
        time: TimeOfDay,
        weekdays: Vec<Weekday>,
    },
    /// On each selected day of the month at `time`. A day a given month lacks
    /// (e.g. the 31st) clamps to that month's last day.
    Monthly { time: TimeOfDay, days: Vec<u8> },
}

impl Default for Schedule {
    fn default() -> Self {
        Self::Daily {
            time: TimeOfDay::default(),
        }
    }
}

impl Schedule {
    /// The time-of-day slot for the variants that carry one. `Manual` has no
    /// schedule and `Hourly` only pins a minute, so both return `None`.
    pub fn time(&self) -> Option<TimeOfDay> {
        match self {
            Self::Daily { time } | Self::Weekly { time, .. } | Self::Monthly { time, .. } => {
                Some(*time)
            }
            Self::Manual | Self::Hourly { .. } => None,
        }
    }
}

/// What to do when a schedule comes due while a prior run of the same
/// automation is still active.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlapPolicy {
    /// Skip the due occurrence and record it as skipped.
    #[default]
    Skip,
    /// Defer: leave the occurrence pending so it fires once the active run ends.
    Queue,
    /// Start the new run alongside the active one.
    Concurrent,
}

/// Whether and when a finished run raises a system notification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub trigger: NotificationTrigger,
}

impl Default for NotificationConfig {
    /// Enabled, notifying on failure — an unattended run that fails is the case
    /// most worth surfacing.
    fn default() -> Self {
        Self {
            enabled: true,
            trigger: NotificationTrigger::OnFailure,
        }
    }
}

/// Which outcomes a notification fires for.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationTrigger {
    Always,
    OnSuccess,
    #[default]
    OnFailure,
}

impl NotificationTrigger {
    /// Whether a run finishing with `success` should notify under this trigger.
    pub fn matches(self, success: bool) -> bool {
        match self {
            Self::Always => true,
            Self::OnSuccess => success,
            Self::OnFailure => !success,
        }
    }
}

/// One entry in an automation's bounded run history.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AutomationRun {
    pub id: Uuid,
    /// When the run was initiated (or skipped), unix seconds.
    pub at: u64,
    /// The spawned session, when one was created. `None` for a skipped run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    pub outcome: RunOutcome,
    /// Whether this run coalesced one or more occurrences missed while nothing
    /// ran (app closed or asleep).
    #[serde(default)]
    pub catch_up: bool,
}

impl AutomationRun {
    /// A freshly spawned run linking to its session, pending completion.
    pub fn spawned(session_id: Uuid, at: u64, catch_up: bool) -> Self {
        Self {
            id: Uuid::new_v4(),
            at,
            session_id: Some(session_id),
            outcome: RunOutcome::Running,
            catch_up,
        }
    }

    /// A run that never spawned because the overlap policy skipped it.
    pub fn skipped(at: u64, catch_up: bool) -> Self {
        Self {
            id: Uuid::new_v4(),
            at,
            session_id: None,
            outcome: RunOutcome::Skipped,
            catch_up,
        }
    }
}

/// The lifecycle outcome recorded for a run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOutcome {
    /// Spawned and still in flight.
    #[default]
    Running,
    Succeeded,
    Failed,
    /// Not run: the overlap policy skipped this occurrence.
    Skipped,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_run_prepends_newest_first_and_caps_at_the_bound() {
        let mut automation = Automation::new("Nightly", ProviderKind::Codex, 1_000);
        for index in 0..(MAX_HISTORY as u64 + 10) {
            automation.record_run(AutomationRun::skipped(1_000 + index, false));
        }
        assert_eq!(automation.history.len(), MAX_HISTORY);
        // Newest-first: the most recently recorded entry is at the front.
        assert_eq!(
            automation.history.first().unwrap().at,
            1_000 + MAX_HISTORY as u64 + 9
        );
        // The oldest surviving entry dropped everything before it.
        assert_eq!(automation.history.last().unwrap().at, 1_000 + 10);
    }

    #[test]
    fn resolve_run_updates_outcome_and_links_the_session() {
        let mut automation = Automation::new("Nightly", ProviderKind::Codex, 1_000);
        let session = Uuid::new_v4();
        let run = AutomationRun::spawned(session, 1_100, false);
        let run_id = run.id;
        automation.record_run(run);

        automation.resolve_run(run_id, RunOutcome::Succeeded, None);
        let entry = automation.history.first().unwrap();
        assert_eq!(entry.outcome, RunOutcome::Succeeded);
        // Link preserved even when the resolve call omits it.
        assert_eq!(entry.session_id, Some(session));
    }

    #[test]
    fn notification_trigger_matches_the_configured_outcome() {
        assert!(NotificationTrigger::Always.matches(true));
        assert!(NotificationTrigger::Always.matches(false));
        assert!(NotificationTrigger::OnSuccess.matches(true));
        assert!(!NotificationTrigger::OnSuccess.matches(false));
        assert!(!NotificationTrigger::OnFailure.matches(true));
        assert!(NotificationTrigger::OnFailure.matches(false));
    }

    #[test]
    fn a_default_automation_round_trips_through_json() {
        let automation = Automation::new("Nightly", ProviderKind::Codex, 1_000);
        let json = serde_json::to_string(&automation).unwrap();
        let restored: Automation = serde_json::from_str(&json).unwrap();
        assert_eq!(automation, restored);
    }
}

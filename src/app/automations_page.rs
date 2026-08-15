//! The Automations full-page view: a first-class peer of the transcript, not a
//! settings tab. Lists saved automations with a schedule summary and computed
//! next-run time, and hosts a form to create, edit, and delete them. Firing is
//! handled elsewhere (the scheduler tick and Run-now); this page is management
//! only.

use chrono::{NaiveDateTime, NaiveTime};

use super::composer::AgentControlTarget;
use super::*;
use crate::automation::schedule::next_occurrence;
use crate::automation::{
    Automation, NotificationConfig, NotificationTrigger, OverlapPolicy, Schedule, TimeOfDay,
    Weekday,
};
use crate::model::{InteractionMode, ProviderKind, RuntimeMode, SessionWorkspace};

/// Which view of the Automations page is showing.
pub(super) enum AutomationsPage {
    List,
    Editor(AutomationEditor),
}

/// The schedule presets the picker offers, in display order. Each maps onto a
/// [`Schedule`] value; `Weekdays` is a UI shortcut for `Weekly` with Mon–Fri.
/// A raw-cron `Custom` preset can be added here later.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SchedulePreset {
    Manual,
    Hourly,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
}

impl SchedulePreset {
    /// Every preset, in the order they render across the segmented control.
    const ALL: [Self; 6] = [
        Self::Manual,
        Self::Hourly,
        Self::Daily,
        Self::Weekdays,
        Self::Weekly,
        Self::Monthly,
    ];
}

/// Monday–Friday, the fixed day set behind the `Weekdays` preset.
fn weekdays_mon_fri() -> Vec<Weekday> {
    vec![
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
    ]
}

/// Whether a weekly selection is exactly Mon–Fri (order-independent), so a
/// stored `Weekly` schedule reads back as the `Weekdays` preset.
fn is_weekdays_mon_fri(weekdays: &[Weekday]) -> bool {
    let mut set = weekdays.to_vec();
    set.sort_by_key(|day| day.chrono().number_from_monday());
    set.dedup();
    set == weekdays_mon_fri()
}

/// Live form state for creating or editing one automation. Name and prompt live
/// in the reused `automation_name_input` / `automation_prompt_input` entities;
/// everything else is edited here and assembled into an [`Automation`] on save.
#[derive(Clone)]
pub(super) struct AutomationEditor {
    /// The automation being edited, or `None` when creating a new one.
    id: Option<Uuid>,
    // These agent fields back the shared composer controls (model picker,
    // reasoning traits, access, agent preset, interaction mode), so they are
    // read and written from `composer.rs` via `AgentControlTarget::Automation`.
    pub(super) provider: ProviderKind,
    pub(super) model: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) service_tier: Option<String>,
    pub(super) agent_preset: Option<String>,
    pub(super) runtime_mode: RuntimeMode,
    pub(super) interaction_mode: InteractionMode,
    // These back the shared composer project/workspace chips, so they are read
    // and written from `composer.rs` via `AgentControlTarget::Automation`.
    pub(super) project_id: Option<Uuid>,
    pub(super) fresh_worktree: bool,
    /// Preserved so editing keeps an existing worktree's base branch.
    pub(super) base_branch: Option<String>,
    preset: SchedulePreset,
    hour: u8,
    minute: u8,
    weekdays: Vec<Weekday>,
    monthdays: Vec<u8>,
    overlap: OverlapPolicy,
    notify_enabled: bool,
    notify_trigger: NotificationTrigger,
    enabled: bool,
}

impl AutomationEditor {
    /// Defaults for a brand-new automation.
    fn new(provider: ProviderKind) -> Self {
        Self {
            id: None,
            provider,
            model: None,
            reasoning_effort: None,
            service_tier: None,
            agent_preset: None,
            runtime_mode: RuntimeMode::default(),
            interaction_mode: InteractionMode::default(),
            project_id: None,
            fresh_worktree: false,
            base_branch: None,
            preset: SchedulePreset::Daily,
            hour: 9,
            minute: 0,
            weekdays: vec![Weekday::Monday],
            monthdays: vec![1],
            overlap: OverlapPolicy::default(),
            notify_enabled: true,
            notify_trigger: NotificationTrigger::OnFailure,
            enabled: true,
        }
    }

    /// Seeds the form from an existing automation.
    fn from_automation(automation: &Automation) -> Self {
        let time = automation.schedule.time().unwrap_or_default();
        // `minute` seeds both the time field (Daily/Weekly/Monthly) and the
        // Hourly minute — only one is shown at a time, so they share the field.
        let mut minute = time.minute;
        let (preset, weekdays, monthdays) = match &automation.schedule {
            Schedule::Manual => (SchedulePreset::Manual, vec![Weekday::Monday], vec![1]),
            Schedule::Hourly { minute: hourly } => {
                minute = *hourly;
                (SchedulePreset::Hourly, vec![Weekday::Monday], vec![1])
            }
            Schedule::Daily { .. } => (SchedulePreset::Daily, vec![Weekday::Monday], vec![1]),
            Schedule::Weekly { weekdays, .. } => {
                // Mon–Fri reads back as the Weekdays shortcut; any other set is
                // a general Weekly schedule.
                let preset = if is_weekdays_mon_fri(weekdays) {
                    SchedulePreset::Weekdays
                } else {
                    SchedulePreset::Weekly
                };
                (preset, weekdays.clone(), vec![1])
            }
            Schedule::Monthly { days, .. } => {
                (SchedulePreset::Monthly, vec![Weekday::Monday], days.clone())
            }
        };
        let (fresh_worktree, base_branch) = match &automation.workspace {
            SessionWorkspace::NewWorktree { base_branch } => (true, base_branch.clone()),
            SessionWorkspace::Worktree { branch, .. } => (true, Some(branch.clone())),
            SessionWorkspace::Local => (false, None),
        };
        Self {
            id: Some(automation.id),
            provider: automation.agent.provider,
            model: automation.agent.model.clone(),
            reasoning_effort: automation.agent.reasoning_effort.clone(),
            service_tier: automation.agent.service_tier.clone(),
            agent_preset: automation.agent.agent_preset.clone(),
            runtime_mode: automation.agent.runtime_mode,
            interaction_mode: automation.agent.interaction_mode,
            project_id: automation.project_id,
            fresh_worktree,
            base_branch,
            preset,
            hour: time.hour.min(23),
            minute: minute.min(59),
            weekdays: if weekdays.is_empty() {
                vec![Weekday::Monday]
            } else {
                weekdays
            },
            monthdays: if monthdays.is_empty() {
                vec![1]
            } else {
                monthdays
            },
            overlap: automation.overlap,
            notify_enabled: automation.notification.enabled,
            notify_trigger: automation.notification.trigger,
            enabled: automation.enabled,
        }
    }

    /// The schedule assembled from the current picker state.
    fn schedule(&self) -> Schedule {
        let time = TimeOfDay::new(self.hour, self.minute);
        match self.preset {
            SchedulePreset::Manual => Schedule::Manual,
            SchedulePreset::Hourly => Schedule::Hourly {
                minute: self.minute,
            },
            SchedulePreset::Daily => Schedule::Daily { time },
            SchedulePreset::Weekdays => Schedule::Weekly {
                time,
                weekdays: weekdays_mon_fri(),
            },
            SchedulePreset::Weekly => {
                let mut weekdays = self.weekdays.clone();
                weekdays.sort_by_key(|day| day.chrono().number_from_monday());
                Schedule::Weekly { time, weekdays }
            }
            SchedulePreset::Monthly => {
                let mut days = self.monthdays.clone();
                days.sort_unstable();
                Schedule::Monthly { time, days }
            }
        }
    }

    /// The workspace the runs use.
    fn workspace(&self) -> SessionWorkspace {
        if self.fresh_worktree {
            SessionWorkspace::NewWorktree {
                base_branch: self.base_branch.clone(),
            }
        } else {
            SessionWorkspace::Local
        }
    }

    /// Writes the form onto an automation, preserving its id/created_at/history.
    fn apply_to(&self, automation: &mut Automation, name: String, prompt: String) {
        automation.name = name;
        automation.prompt = prompt;
        automation.agent.provider = self.provider;
        automation.agent.model = self.model.clone();
        automation.agent.reasoning_effort = self.reasoning_effort.clone();
        automation.agent.service_tier = self.service_tier.clone();
        automation.agent.agent_preset = self.agent_preset.clone();
        automation.agent.runtime_mode = self.runtime_mode;
        automation.agent.interaction_mode = self.interaction_mode;
        automation.project_id = self.project_id;
        automation.workspace = self.workspace();
        automation.schedule = self.schedule();
        automation.overlap = self.overlap;
        automation.notification = NotificationConfig {
            enabled: self.notify_enabled,
            trigger: self.notify_trigger,
        };
        automation.enabled = self.enabled;
        automation.updated_at = crate::model::unix_time();
    }
}

impl Waku {
    pub(super) fn open_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_page = None;
        self.automations_page = Some(AutomationsPage::List);
        self.automations_scroll.set_offset(gpui::Point::default());
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    /// Opens the editor for `id`, or a blank one when `None`. Reachable from the
    /// sidebar's automation context menu as well as the list, so it clears any
    /// open settings page to bring the editor to the foreground.
    pub(super) fn open_automation_editor(
        &mut self,
        id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        let (editor, name, prompt) = match id.and_then(|id| self.state.automation(id)) {
            Some(automation) => (
                AutomationEditor::from_automation(automation),
                automation.name.clone(),
                automation.prompt.clone(),
            ),
            None => (
                AutomationEditor::new(self.state.last_provider),
                String::new(),
                String::new(),
            ),
        };
        self.automation_name_input
            .update(cx, |input, cx| input.set_content(name, cx));
        self.automation_prompt_input
            .update(cx, |input, cx| input.set_content(prompt, cx));
        let hour_text = format!("{:02}", editor.hour);
        let minute_text = format!("{:02}", editor.minute);
        self.automation_hour_input
            .update(cx, |input, cx| input.set_content(hour_text, cx));
        self.automation_minute_input
            .update(cx, |input, cx| input.set_content(minute_text, cx));
        self.automations_page = Some(AutomationsPage::Editor(editor));
        self.automations_scroll.set_offset(gpui::Point::default());
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    /// The open automation editor form, if the editor is showing. Lets the
    /// shared agent controls read the current form values.
    pub(super) fn automation_editor(&self) -> Option<&AutomationEditor> {
        match self.automations_page.as_ref() {
            Some(AutomationsPage::Editor(editor)) => Some(editor),
            _ => None,
        }
    }

    /// Mutates the open editor, if any, then repaints.
    pub(super) fn edit_automation_form(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut AutomationEditor),
    ) {
        if let Some(AutomationsPage::Editor(editor)) = self.automations_page.as_mut() {
            change(editor);
            cx.notify();
        }
    }

    /// Backs the freeform hour/minute schedule fields. Keeps only digits (at
    /// most two), clamps to a valid clock value (0–23 / 0–59), and writes it
    /// onto the open editor. The visible text is rewritten only when it diverges
    /// from the sanitized value, so it never fights the caret mid-type.
    pub(super) fn on_automation_time_edited(&mut self, minute_field: bool, cx: &mut Context<Self>) {
        let input = if minute_field {
            self.automation_minute_input.clone()
        } else {
            self.automation_hour_input.clone()
        };
        let raw = input.read(cx).content().to_string();
        let max: u8 = if minute_field { 59 } else { 23 };
        let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).take(2).collect();
        let parsed = digits.parse::<u8>().ok();
        let clamped = parsed.map(|value| value.min(max));
        if let Some(value) = clamped {
            self.edit_automation_form(cx, |editor| {
                if minute_field {
                    editor.minute = value;
                } else {
                    editor.hour = value;
                }
            });
        }
        // Preserve the typed digits (including a leading zero) unless the value
        // was out of range or carried non-digits, in which case snap the text.
        let normalized = match (parsed, clamped) {
            (Some(parsed), Some(clamped)) if parsed != clamped => clamped.to_string(),
            _ => digits,
        };
        if normalized != raw {
            input.update(cx, |input, cx| input.set_content(normalized, cx));
        }
    }

    fn save_automation_editor(&mut self, cx: &mut Context<Self>) {
        let Some(AutomationsPage::Editor(editor)) = self.automations_page.as_ref() else {
            return;
        };
        let editor = editor.clone();
        let name = self
            .automation_name_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let name = if name.is_empty() {
            tr!("automations.default_name")
        } else {
            name
        };
        let prompt = self
            .automation_prompt_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        // A prompt-less automation can never do anything — the spawn path no-ops
        // on empty input — so reject it at save time with a visible message
        // rather than persist a run that silently does nothing forever.
        if prompt.is_empty() {
            self.show_toast(tr!("automations.prompt_required"));
            cx.notify();
            return;
        }

        let saved_id = match editor.id {
            Some(id) => {
                if let Some(automation) = self.state.automation_mut(id) {
                    editor.apply_to(automation, name, prompt);
                }
                id
            }
            None => {
                let mut automation =
                    Automation::new(name.clone(), editor.provider, crate::model::unix_time());
                editor.apply_to(&mut automation, name, prompt);
                let id = automation.id;
                self.state.push_automation(automation);
                id
            }
        };
        self.save();
        // Stay on the editor after saving. Promote a freshly created automation
        // to an existing one so a second save updates it (and Run now becomes
        // available) instead of pushing a duplicate.
        if let Some(AutomationsPage::Editor(editor)) = self.automations_page.as_mut() {
            editor.id = Some(saved_id);
        }
        cx.notify();
    }

    fn delete_automation(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.automation_delete_arming = None;
        if self.state.remove_automation(id) {
            // Deleting the automation whose editor is open returns to the list.
            if matches!(&self.automations_page, Some(AutomationsPage::Editor(editor)) if editor.id == Some(id))
            {
                self.automations_page = Some(AutomationsPage::List);
            }
            self.save();
            cx.notify();
        }
    }

    /// Closes the editor and returns to the automations list.
    fn close_automation_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.automation_delete_arming = None;
        self.automations_page = Some(AutomationsPage::List);
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    pub(super) fn toggle_automation_enabled(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if let Some(automation) = self.state.automation_mut(id) {
            automation.enabled = !automation.enabled;
            automation.updated_at = crate::model::unix_time();
            self.save();
            cx.notify();
        }
    }

    /// Spawns a fresh session from an automation's configuration and records the
    /// run in its history. Shared by Run-now and the scheduler tick.
    ///
    /// Returns the spawned session id. The heavy work — worktree materialization
    /// and the provider process — runs off the UI thread inside
    /// [`Self::submit_submission_for_session`]'s background preparation; only the
    /// in-memory session/history bookkeeping happens here.
    pub(super) fn spawn_automation_run(
        &mut self,
        id: Uuid,
        catch_up: bool,
        cx: &mut Context<Self>,
    ) -> Option<Uuid> {
        let automation = self.state.automation(id)?.clone();
        // The submission path no-ops on empty input, so a prompt-less automation
        // would leave a run entry linked to a session that never starts. Skip it
        // rather than record a run that can never complete.
        if automation.prompt.trim().is_empty() {
            return None;
        }

        let project_id = match automation.project_id {
            Some(project_id)
                if self
                    .state
                    .projects
                    .iter()
                    .any(|project| project.id == project_id) =>
            {
                project_id
            }
            // Unbound (or a project that no longer exists): give this run its own
            // projectless workspace, the way a projectless manual task gets one.
            _ => self.create_automation_projectless_project(cx)?,
        };

        let mut session = self
            .state
            .new_session(project_id, automation.agent.provider);
        session.model = automation.agent.model.clone();
        session.reasoning_effort = automation.agent.reasoning_effort.clone();
        session.service_tier = automation.agent.service_tier.clone();
        session.agent_preset = automation.agent.agent_preset.clone();
        session.runtime_mode = automation.agent.runtime_mode;
        session.interaction_mode = automation.agent.interaction_mode;
        session.workspace = automation.workspace.clone();
        session.originating_automation = Some(id);
        let session_id = session.id;
        self.state.push_session(session);

        let now = crate::model::unix_time();
        if let Some(automation) = self.state.automation_mut(id) {
            automation.record_run(crate::automation::AutomationRun::spawned(
                session_id, now, catch_up,
            ));
            automation.last_run_at = Some(now);
        }

        self.submit_submission_for_session(
            session_id,
            super::ComposerSubmission::plain(automation.prompt.clone()),
            cx,
        );
        Some(session_id)
    }

    /// The scheduler tick: asks the pure planner which automations are due given
    /// the real clock and the current active-run state, then fires or skips each
    /// decision. A thin shell — every schedule and overlap decision is made by
    /// [`crate::automation::planner::plan`], so the same core can drive a future
    /// headless worker.
    pub(super) fn tick_automations(&mut self, cx: &mut Context<Self>) {
        if self.state.automations.is_empty() {
            return;
        }

        // An automation is "active" when one of its spawned sessions is still
        // busy.
        let active: std::collections::HashSet<Uuid> = self
            .state
            .sessions
            .iter()
            .filter(|session| session.is_busy())
            .filter_map(|session| session.originating_automation)
            .collect();

        let now = chrono::Local::now().naive_local();
        let ticks: Vec<crate::automation::planner::AutomationTick> = self
            .state
            .automations
            .iter()
            .map(|automation| crate::automation::planner::AutomationTick {
                id: automation.id,
                enabled: automation.enabled,
                schedule: automation.schedule.clone(),
                // Before an automation has ever run, its creation time is the
                // baseline, so a fresh automation never immediately catch-up
                // fires.
                marker: local_naive(automation.last_run_at.unwrap_or(automation.created_at)),
                overlap: automation.overlap,
                active: active.contains(&automation.id),
            })
            .collect();

        let decisions = crate::automation::planner::plan(
            &ticks,
            now,
            chrono::Duration::seconds(super::AUTOMATION_CATCH_UP_GRACE_SECS),
        );
        if decisions.is_empty() {
            return;
        }

        let mut changed = false;
        for decision in decisions {
            match decision {
                crate::automation::planner::PlanDecision::Fire { id, catch_up } => {
                    if self.spawn_automation_run(id, catch_up, cx).is_some() {
                        changed = true;
                    }
                }
                crate::automation::planner::PlanDecision::Skip { id, catch_up } => {
                    // Consume the occurrence: record the skip and advance the
                    // marker so it is not re-evaluated next tick.
                    let now_unix = crate::model::unix_time();
                    if let Some(automation) = self.state.automation_mut(id) {
                        automation.record_run(crate::automation::AutomationRun::skipped(
                            now_unix, catch_up,
                        ));
                        automation.last_run_at = Some(now_unix);
                        changed = true;
                    }
                }
            }
        }

        if changed {
            self.save();
            cx.notify();
        }
    }

    /// A fresh projectless project for an unbound automation run.
    fn create_automation_projectless_project(&mut self, cx: &mut Context<Self>) -> Option<Uuid> {
        let workspace = match crate::projectless::create_workspace(None) {
            Ok(workspace) => workspace,
            Err(error) => {
                self.show_toast(tr!("errors.create_projectless_task", error = error));
                cx.notify();
                return None;
            }
        };
        let mut project = Project::from_path(workspace.cwd);
        project.name = Project::PROJECTLESS_NAME.to_owned();
        let project_id = project.id;
        self.state.projects.push(project);
        Some(project_id)
    }

    /// Resolves a completed automation run's history outcome and, per that
    /// automation's notification config, raises a system notification.
    ///
    /// A no-op for manual sessions and for follow-up turns on a run that already
    /// resolved — only a run still marked `Running` is acted on, so completion
    /// notifications fire exactly once.
    pub(super) fn complete_automation_run(
        &mut self,
        session_id: Uuid,
        success: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(automation_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.originating_automation)
        else {
            return;
        };
        let outcome = if success {
            crate::automation::RunOutcome::Succeeded
        } else {
            crate::automation::RunOutcome::Failed
        };
        let Some(automation) = self.state.automation_mut(automation_id) else {
            return;
        };
        let Some(run_id) = automation
            .history
            .iter()
            .find(|run| {
                run.session_id == Some(session_id)
                    && run.outcome == crate::automation::RunOutcome::Running
            })
            .map(|run| run.id)
        else {
            return;
        };
        automation.resolve_run(run_id, outcome, None);
        let should_notify =
            automation.notification.enabled && automation.notification.trigger.matches(success);
        let name = automation.name.clone();
        self.save();

        if should_notify {
            let body = if success {
                tr!("automations.notify_succeeded")
            } else {
                tr!("automations.notify_failed")
            };
            crate::platform::show_task_notification(
                &format!("automation-{automation_id}"),
                &name,
                &body,
                cx,
            );
        }
    }

    /// Run-now: spawn the run, then open its transcript so the result is visible.
    pub(super) fn run_automation_now(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(session_id) = self.spawn_automation_run(id, false, cx) else {
            return;
        };
        self.save();
        self.automations_page = None;
        self.select_session(session_id, cx);
        cx.notify();
    }

    pub(super) fn render_automations(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let right_window_controls = self.render_client_window_controls(
            super::window_chrome::WindowControlSide::Right,
            window,
            cx,
        );

        let body = match self.automations_page.as_ref() {
            Some(AutomationsPage::Editor(_)) => self.render_automation_editor(cx),
            _ => self.render_automations_list(cx),
        };

        div()
            .key_context("Waku")
            .track_focus(&self.automations_focus)
            .on_action(cx.listener(Self::cancel_turn_action))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_command_palette_action))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .child(
                self.render_page_drag_region("automations-titlebar", cx)
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(right_window_controls),
            )
            .child(body)
            .into_any_element()
    }

    fn render_automations_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);

        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(20.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("automations.title")),
            )
            .child(
                div()
                    .id("automations-new")
                    .tab_index(0)
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(30.0))
                    .px(px(12.0))
                    .rounded(px(7.0))
                    .bg(theme.inverse)
                    .text_color(theme.on_inverse)
                    .text_size(px(13.0))
                    .cursor_default()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|element| element.opacity(0.9))
                    .child(icon("icons/plus.svg", 14.0, theme.on_inverse))
                    .child(tr!("automations.new"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_automation_editor(None, window, cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.open_automation_editor(None, window, cx);
                            cx.stop_propagation();
                        }
                    })),
            );

        let mut list = div().flex().flex_col().gap(px(8.0)).pb(px(24.0));
        if self.state.automations.is_empty() {
            list = list.child(
                div()
                    .mt(px(40.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .text_color(theme.text_tertiary)
                    .child(icon("icons/zap.svg", 28.0, theme.text_tertiary))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .text_color(theme.text_secondary)
                            .child(tr!("automations.empty_title")),
                    )
                    .child(
                        div()
                            .max_w(px(360.0))
                            .text_center()
                            .text_size(px(13.0))
                            .child(tr!("automations.empty_body")),
                    ),
            );
        } else {
            for automation in &self.state.automations {
                list = list.child(self.render_automation_row(automation, &theme, cx));
            }
        }

        // Fixed header bar mirroring the editor's, so the "Automations" title
        // sits at identical coordinates when switching between list and editor.
        let header_bar = div().flex_none().pt(px(8.0)).pb(px(8.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .px(px(24.0))
                .child(header),
        );

        let scroll = div()
            .id("automations-scroll")
            .track_scroll(&self.automations_scroll)
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .px(px(24.0))
                    .child(list),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header_bar)
            .child(scroll)
            .into_any_element()
    }

    fn render_automation_row(
        &self,
        automation: &Automation,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = automation.id;
        let enabled = automation.enabled;
        let next_run = next_run_label(&automation.schedule);
        let summary = schedule_summary(&automation.schedule);

        // Status is never color alone: an icon and a word carry it too.
        let (status_icon, status_color, status_label) = if enabled {
            ("icons/check.svg", theme.success, tr!("automations.enabled"))
        } else {
            (
                "icons/pause.svg",
                theme.text_tertiary,
                tr!("automations.disabled"),
            )
        };

        div()
            .id(SharedString::from(format!("automation-row-{id}")))
            .tab_index(0)
            .flex()
            .items_center()
            .gap(px(12.0))
            .p(px(14.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .cursor_default()
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.border_color(theme.border_strong))
            // The whole card opens the editor; the inner controls stop
            // propagation so they keep their own actions.
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_automation_editor(Some(id), window, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.open_automation_editor(Some(id), window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(automation.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .text_size(px(11.0))
                                    .text_color(status_color)
                                    .child(icon(status_icon, 11.0, status_color))
                                    .child(status_label),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_secondary)
                            .child(summary),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_tertiary)
                            .child(next_run),
                    ),
            )
            .child(self.render_automation_enable_toggle(id, enabled, theme, cx))
            // A right chevron hints that the card itself opens the editor, where
            // Run now and Delete live.
            .child(icon("icons/chevron-right.svg", 16.0, theme.text_tertiary))
            .into_any_element()
    }

    /// The delete control: a single click arms it (danger styling, "Confirm
    /// delete"), and a second click removes the automation. Clicking away
    /// disarms, so a stray click can never delete. Mirrors the skills page.
    fn render_automation_delete_button(
        &self,
        id: Uuid,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let armed = self.automation_delete_arming == Some(id);
        div()
            .id(SharedString::from(format!("automation-delete-{id}")))
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            // Sized to match Save beside it in the editor header.
            .h(px(30.0))
            .px(px(12.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(if armed { theme.danger } else { theme.border })
            .when(armed, |element| element.bg(theme.danger.opacity(0.12)))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(px(13.0))
            .text_color(if armed {
                theme.danger
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
            .child(icon(
                "icons/trash.svg",
                13.0,
                if armed {
                    theme.danger
                } else {
                    theme.text_tertiary
                },
            ))
            .child(if armed {
                tr!("automations.confirm_delete")
            } else {
                tr!("automations.delete")
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.automation_delete_arming == Some(id) {
                    this.delete_automation(id, cx);
                } else {
                    this.automation_delete_arming = Some(id);
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    if this.automation_delete_arming == Some(id) {
                        this.delete_automation(id, cx);
                    } else {
                        this.automation_delete_arming = Some(id);
                        cx.notify();
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                if this.automation_delete_arming == Some(id) {
                    this.automation_delete_arming = None;
                    cx.notify();
                }
            }))
    }

    fn render_automation_enable_toggle(
        &self,
        id: Uuid,
        enabled: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("automation-toggle-{id}")))
            .tab_index(0)
            .flex_none()
            .focus_visible(|style| style.border_color(theme.accent))
            .w(px(36.0))
            .h(px(20.0))
            .p(px(2.0))
            .rounded_full()
            .cursor_default()
            .bg(if enabled { theme.inverse } else { theme.inset })
            .border_1()
            .border_color(if enabled {
                theme.inverse
            } else {
                theme.border_strong
            })
            .flex()
            .items_center()
            .when(enabled, |element| element.justify_end())
            .child(div().w(px(14.0)).h(px(14.0)).rounded_full().bg(if enabled {
                theme.on_inverse
            } else {
                theme.text_tertiary
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_automation_enabled(id, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.toggle_automation_enabled(id, cx);
                    cx.stop_propagation();
                }
            }))
    }

    fn render_automation_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(AutomationsPage::Editor(editor)) = self.automations_page.as_ref() else {
            return div().into_any_element();
        };
        let editor = editor.clone();
        let id = editor.id;
        let crumb_name = match id.and_then(|id| self.state.automation(id)) {
            Some(automation) => SharedString::from(automation.name.clone()),
            None => SharedString::from(tr!("automations.create_title")),
        };

        // "Automations" returns to the list (replacing Cancel), then the
        // current automation's name.
        let breadcrumb = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w_0()
            .text_size(px(20.0))
            .font_weight(FontWeight::MEDIUM)
            .child(
                div()
                    .id("automation-breadcrumb-home")
                    .tab_index(0)
                    .flex_none()
                    .cursor_default()
                    .text_color(theme.text_tertiary)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|element| element.text_color(theme.text))
                    .child(tr!("automations.title"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_automation_editor(window, cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.close_automation_editor(window, cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .child(icon("icons/chevron-right.svg", 14.0, theme.text_tertiary))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(crumb_name),
            );

        let save = div()
            .id("automation-save")
            .tab_index(0)
            .flex_none()
            .h(px(30.0))
            .px(px(12.0))
            .rounded(px(7.0))
            // A border that is only transparent, not absent: Delete beside it
            // carries one, so this keeps both boxes the same size and stops
            // the button resizing when focus draws its ring.
            .border_1()
            .border_color(gpui::transparent_black())
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(px(13.0))
            .bg(theme.inverse)
            .text_color(theme.on_inverse)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.opacity(0.9))
            .child(tr!("automations.save"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.save_automation_editor(cx);
            }));

        // Delete is only meaningful for an existing automation; Run now now
        // lives in the composer, bottom-right where the send button sits.
        let delete = id.map(|id| self.render_automation_delete_button(id, &theme, cx));

        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(breadcrumb)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(8.0))
                    .children(delete)
                    .child(save),
            );

        // The header stays fixed at the top so the breadcrumb and Save are
        // always reachable.
        let header_bar = div().flex_none().pt(px(8.0)).pb(px(8.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .px(px(24.0))
                .child(header),
        );

        // One scroll flow, top to bottom like a settings page. The composer is
        // the Instructions field of the first card rather than a surface pinned
        // to the bottom, so every part of the automation is one list of fields.
        let body = div()
            .id("automations-scroll")
            .track_scroll(&self.automations_scroll)
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .px(px(24.0))
                    .pb(px(24.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(editor_card(
                        &theme,
                        vec![
                            editor_row(
                                &theme,
                                tr!("automations.field_name"),
                                tr!("automations.field_name_description"),
                                TextField::new(
                                    "automation-name-field",
                                    self.automation_name_input.clone(),
                                )
                                .w(px(260.0))
                                .into_any_element(),
                            )
                            .into_any_element(),
                            editor_row_stacked(
                                &theme,
                                tr!("automations.field_instructions"),
                                tr!("automations.field_instructions_description"),
                                self.editor_agent_section(id, &theme, cx).into_any_element(),
                            )
                            .into_any_element(),
                            editor_row(
                                &theme,
                                tr!("automations.enabled"),
                                tr!("automations.field_enabled_description"),
                                self.editor_toggle(
                                    "automation-enabled",
                                    editor.enabled,
                                    &theme,
                                    cx,
                                    |editor| editor.enabled = !editor.enabled,
                                )
                                .into_any_element(),
                            )
                            .into_any_element(),
                        ],
                    ))
                    .child(self.editor_schedule_section(&editor, &theme, cx))
                    .child(self.editor_behavior_section(&editor, &theme, cx)),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header_bar)
            .child(body)
            .into_any_element()
    }

    /// A labeled dropdown chip. Collapses the shared `MenuChip` + `dropdown_menu`
    /// + handle boilerplate; each field supplies only its `items`. The single
    /// select's per-item behavior stays at the call site.
    fn picker(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        width: f32,
        disabled: bool,
        cx: &mut Context<Self>,
        items: impl Fn(&mut App) -> Vec<MenuItem> + 'static,
    ) -> AnyElement {
        let handle = self.menu_handle(id, cx);
        dropdown_menu(
            MenuChip::new(id)
                .label(label)
                .outlined()
                .selected(handle.is_open())
                .disabled(disabled)
                .w(px(width))
                .justify_between(),
            SharedString::from(format!("{id}-menu")),
            &handle,
            MenuAlign::BelowRight,
            items,
        )
    }

    fn editor_agent_section(&self, id: Option<Uuid>, theme: &Theme, cx: &mut Context<Self>) -> Div {
        // A composer-style card: the prompt textarea with the model/access
        // controls docked beneath it, mirroring the new-task composer so the
        // editor's core reads like the surface a user already knows.
        let prompt_scroll = self.automation_prompt_scroll.clone();
        let prompt_area = div()
            .id("automation-prompt-area")
            .w_full()
            // Sized like the chat composer: one line when empty, growing with
            // the text, capped so a long prompt scrolls inside the field
            // rather than pushing the rest of the form off the page. The field
            // itself is `FieldMode::Code` — it inherits its metrics from here
            // rather than carrying `FieldMode::Composer`'s, so this floor and
            // the type scale below stand in for them.
            .min_h(px(24.0))
            .max_h(px(260.0))
            .overflow_y_scroll()
            .track_scroll(&self.automation_prompt_scroll)
            // Scroll chaining, which GPUI does not do on its own: every hitbox
            // under the pointer gets the wheel, so without this both the prompt
            // and the page move at once. This listener runs after the built-in
            // one for the same element, so the offset it reads is the one the
            // wheel just produced. While that offset is still inside the
            // field's own range the field consumed the scroll and the page must
            // stay put; once it runs past an edge the field is done, so the
            // offset snaps back to that edge and the event goes on to the page.
            // A prompt short enough not to overflow claims nothing.
            .on_scroll_wheel(move |_, _, cx| {
                let max = prompt_scroll.max_offset().y;
                let mut offset = prompt_scroll.offset();
                let clamped = offset.y.clamp(-max, px(0.0));
                if max > px(0.5) && clamped == offset.y {
                    cx.stop_propagation();
                    return;
                }
                offset.y = clamped;
                prompt_scroll.set_offset(offset);
            })
            .px(px(4.0))
            .pt(px(2.0))
            .text_size(px(13.5))
            .line_height(px(22.0))
            .cursor(gpui::CursorStyle::IBeam)
            .child(self.automation_prompt_input.clone())
            // A click in the padding around the text still lands focus in the
            // field rather than falling through to the card.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    let focus = this.automation_prompt_input.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }),
            );

        // Run now sits at the composer's bottom-right, where the chat composer's
        // send button lives. Only an existing automation can be run.
        let run_now = id.map(|id| {
            div()
                .id("automation-editor-run")
                .tab_index(0)
                .flex_none()
                .w(px(26.0))
                .h(px(26.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .bg(theme.inverse)
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .hover(|element| element.opacity(0.9))
                .active(|element| element.opacity(0.8))
                .child(icon("icons/zap.svg", 14.0, theme.on_inverse))
                .tooltip(Tooltip::text(tr!("automations.run_now")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run_automation_now(id, cx);
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.run_automation_now(id, cx);
                        cx.stop_propagation();
                    }
                }))
        });

        // The exact composer controls, one source of truth, editing the open
        // form instead of a live session. The controls wrap in a flexible left
        // cluster so Run now stays pinned to the right, like the send button.
        let toolbar = div()
            .mt(px(8.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(self.render_provider_model_control(AgentControlTarget::Automation, cx))
                    .children(self.render_model_traits_control(AgentControlTarget::Automation, cx))
                    .children(self.render_agent_preset_control(AgentControlTarget::Automation, cx))
                    .child(self.render_access_control(AgentControlTarget::Automation, cx))
                    .child(
                        self.render_interaction_mode_control(AgentControlTarget::Automation, cx),
                    ),
            )
            .children(run_now);

        // An input well, not a composer surface: this field lives inside a
        // raised card, and `theme.composer` is within two shades of
        // `theme.raised` in dark mode, so it would read as an invisible box.
        // `inset` + `border_strong` is what the Name field beside it uses.
        let card = div()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.inset)
            .p(px(10.0))
            .child(prompt_area)
            .child(toolbar);

        // The composer footer's project + workspace chips, docked under the
        // field exactly as they sit under the new-task composer — same chrome,
        // editing the open automation form instead of a live session.
        let workspace_footer = div()
            .mt(px(8.0))
            .pl(px(10.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .text_size(px(11.0))
            .line_height(px(14.0))
            .child(self.render_project_control(AgentControlTarget::Automation, cx))
            .child(self.render_workspace_kind_control(AgentControlTarget::Automation, cx));

        div().flex().flex_col().child(card).child(workspace_footer)
    }

    fn editor_schedule_section(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        // Schedule preset dropdown.
        let preset = editor.preset;
        let preset_weak = cx.entity().downgrade();
        let preset_picker = self.picker(
            "automation-frequency",
            preset_label(preset),
            200.0,
            false,
            cx,
            move |_| {
                let weak = preset_weak.clone();
                SchedulePreset::ALL
                    .into_iter()
                    .map(|option| {
                        let weak = weak.clone();
                        MenuItem::new(preset_label(option), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.edit_automation_form(cx, |editor| editor.preset = option);
                            });
                        })
                        .selected(option == preset)
                    })
                    .collect()
            },
        );

        // Time-of-day: freeform hour and minute fields the user types by hand.
        // The `on_automation_time_edited` subscription clamps and validates.
        // Built lazily since only the presets that carry a full time show it.
        let time_picker = || {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    TextField::new("automation-hour-field", self.automation_hour_input.clone())
                        .w(px(52.0)),
                )
                .child(div().text_color(theme.text_tertiary).child(":"))
                .child(
                    TextField::new(
                        "automation-minute-field",
                        self.automation_minute_input.clone(),
                    )
                    .w(px(52.0)),
                )
                .into_any_element()
        };

        let mut rows = vec![
            editor_row(
                theme,
                tr!("automations.field_schedule_frequency"),
                // Manual has no follow-up row, so its caption explains itself
                // here.
                match editor.preset {
                    SchedulePreset::Manual => tr!("automations.hint_manual"),
                    _ => tr!("automations.field_schedule_frequency_description"),
                },
                preset_picker,
            )
            .into_any_element(),
        ];

        // Each preset shows only the sub-controls it needs.
        match editor.preset {
            SchedulePreset::Manual => {}
            SchedulePreset::Hourly => {
                // Every hour at a chosen minute past the hour: minute only.
                let minute_picker = div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_color(theme.text_tertiary)
                            .child(tr!("automations.at_minute")),
                    )
                    .child(
                        TextField::new(
                            "automation-minute-field",
                            self.automation_minute_input.clone(),
                        )
                        .w(px(52.0)),
                    )
                    .into_any_element();
                rows.push(
                    editor_row(
                        theme,
                        tr!("automations.field_time"),
                        tr!("automations.field_minute_description"),
                        minute_picker,
                    )
                    .into_any_element(),
                );
            }
            SchedulePreset::Daily | SchedulePreset::Weekdays => {
                rows.push(
                    editor_row(
                        theme,
                        tr!("automations.field_time"),
                        tr!("automations.field_time_description"),
                        time_picker(),
                    )
                    .into_any_element(),
                );
            }
            SchedulePreset::Weekly => {
                rows.push(
                    editor_row(
                        theme,
                        tr!("automations.field_time"),
                        tr!("automations.field_time_description"),
                        time_picker(),
                    )
                    .into_any_element(),
                );
                // The chip rows are wider than a right-hand control column, so
                // they stack under their label at full width.
                rows.push(
                    editor_row_stacked(
                        theme,
                        tr!("automations.field_days"),
                        tr!("automations.field_weekdays_description"),
                        self.weekday_chips(editor, theme, cx),
                    )
                    .into_any_element(),
                );
            }
            SchedulePreset::Monthly => {
                rows.push(
                    editor_row(
                        theme,
                        tr!("automations.field_time"),
                        tr!("automations.field_time_description"),
                        time_picker(),
                    )
                    .into_any_element(),
                );
                rows.push(
                    editor_row_stacked(
                        theme,
                        tr!("automations.field_days"),
                        tr!("automations.field_monthdays_description"),
                        self.monthday_chips(editor, theme, cx),
                    )
                    .into_any_element(),
                );
            }
        }

        editor_card(theme, rows)
    }

    fn editor_behavior_section(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let weak = cx.entity().downgrade();

        // Overlap policy.
        let overlap = editor.overlap;
        let overlap_weak = weak.clone();
        let overlap_picker = self.picker(
            "automation-overlap",
            overlap_label(overlap),
            200.0,
            false,
            cx,
            move |_| {
                let weak = overlap_weak.clone();
                [
                    OverlapPolicy::Skip,
                    OverlapPolicy::Queue,
                    OverlapPolicy::Concurrent,
                ]
                .into_iter()
                .map(|option| {
                    let weak = weak.clone();
                    MenuItem::new(overlap_label(option), move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.edit_automation_form(cx, |editor| editor.overlap = option);
                        });
                    })
                    .selected(option == overlap)
                })
                .collect()
            },
        );

        // Notifications: an enable toggle plus a trigger picker.
        let notify_toggle = self.editor_toggle(
            "automation-notify",
            editor.notify_enabled,
            theme,
            cx,
            |editor| editor.notify_enabled = !editor.notify_enabled,
        );

        let trigger = editor.notify_trigger;
        let trigger_weak = weak.clone();
        let trigger_enabled = editor.notify_enabled;
        let trigger_picker = self.picker(
            "automation-trigger",
            trigger_label(trigger),
            200.0,
            !trigger_enabled,
            cx,
            move |_| {
                let weak = trigger_weak.clone();
                [
                    NotificationTrigger::Always,
                    NotificationTrigger::OnSuccess,
                    NotificationTrigger::OnFailure,
                ]
                .into_iter()
                .map(|option| {
                    let weak = weak.clone();
                    MenuItem::new(trigger_label(option), move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.edit_automation_form(cx, |editor| editor.notify_trigger = option);
                        });
                    })
                    .selected(option == trigger)
                })
                .collect()
            },
        );

        editor_card(
            theme,
            vec![
                editor_row(
                    theme,
                    tr!("automations.field_overlap"),
                    tr!("automations.field_overlap_description"),
                    overlap_picker,
                )
                .into_any_element(),
                editor_row(
                    theme,
                    tr!("automations.field_notifications"),
                    tr!("automations.field_notifications_description"),
                    notify_toggle.into_any_element(),
                )
                .into_any_element(),
                editor_row(
                    theme,
                    tr!("automations.field_notify_when"),
                    tr!("automations.field_notify_when_description"),
                    trigger_picker,
                )
                .into_any_element(),
            ],
        )
    }

    /// A small toggle bound to a form mutation. `change` flips the field.
    fn editor_toggle(
        &self,
        id: &'static str,
        on: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        change: impl Fn(&mut AutomationEditor) + 'static,
    ) -> Stateful<Div> {
        // Shared so both the click and keyboard handlers can invoke it.
        let change = std::rc::Rc::new(change);
        let key_change = change.clone();
        div()
            .id(id)
            .tab_index(0)
            .flex_none()
            .focus_visible(|style| style.border_color(theme.accent))
            .w(px(36.0))
            .h(px(20.0))
            .p(px(2.0))
            .rounded_full()
            .cursor_default()
            .bg(if on { theme.inverse } else { theme.inset })
            .border_1()
            .border_color(if on {
                theme.inverse
            } else {
                theme.border_strong
            })
            .flex()
            .items_center()
            .when(on, |element| element.justify_end())
            .child(div().w(px(14.0)).h(px(14.0)).rounded_full().bg(if on {
                theme.on_inverse
            } else {
                theme.text_tertiary
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit_automation_form(cx, |editor| change(editor));
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.edit_automation_form(cx, |editor| key_change(editor));
                    cx.stop_propagation();
                }
            }))
    }

    fn weekday_chips(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = editor.weekdays.clone();
        let mut row = div().flex().flex_wrap().gap(px(6.0));
        for weekday in Weekday::ALL {
            let is_selected = selected.contains(&weekday);
            row = row.child(self.selection_chip(
                SharedString::from(format!("weekday-{}", weekday_short(weekday))),
                weekday_short(weekday),
                is_selected,
                theme,
                cx,
                move |editor| {
                    toggle_membership_min_one(&mut editor.weekdays, weekday);
                },
            ));
        }
        row.into_any_element()
    }

    fn monthday_chips(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = editor.monthdays.clone();
        let mut grid = div().flex().flex_wrap().gap(px(4.0));
        for day in 1u8..=31 {
            let is_selected = selected.contains(&day);
            grid = grid.child(self.selection_chip(
                SharedString::from(format!("monthday-{day}")),
                day.to_string(),
                is_selected,
                theme,
                cx,
                move |editor| {
                    toggle_membership_min_one(&mut editor.monthdays, day);
                },
            ));
        }
        grid.into_any_element()
    }

    fn selection_chip(
        &self,
        id: SharedString,
        label: String,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        change: impl Fn(&mut AutomationEditor) + 'static,
    ) -> Stateful<Div> {
        // Shared so both the click and keyboard handlers can invoke it.
        let change = std::rc::Rc::new(change);
        let key_change = change.clone();
        div()
            .id(id)
            .tab_index(0)
            .min_w(px(30.0))
            .h(px(26.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(px(12.0))
            .border_1()
            .focus_visible(|style| style.border_color(theme.accent))
            .bg(if selected { theme.inverse } else { theme.inset })
            .border_color(if selected {
                theme.inverse
            } else {
                theme.border
            })
            .text_color(if selected {
                theme.on_inverse
            } else {
                theme.text_secondary
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit_automation_form(cx, |editor| change(editor));
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.edit_automation_form(cx, |editor| key_change(editor));
                    cx.stop_propagation();
                }
            }))
    }
}

/// A grouped card, matching the cards the settings pages are built from: a
/// heading and caption on a raised surface, with the group's field rows
/// appended as children.
fn editor_card(theme: &Theme, rows: Vec<AnyElement>) -> Div {
    let mut card = div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(13.0))
        .overflow_hidden()
        .bg(theme.raised);
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            card = card.child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border));
        }
        card = card.child(row);
    }
    card
}

/// One field row inside a card: label and caption on the left, the control
/// flush right.
fn editor_row(theme: &Theme, label: String, description: String, control: AnyElement) -> Div {
    editor_row_base()
        .items_center()
        .gap(px(24.0))
        .child(
            editor_row_label(theme, label, description)
                .flex_1()
                .min_w_0(),
        )
        .child(div().flex_none().child(control))
}

/// A field row whose control is too wide for the right-hand column — the chip
/// pickers — so it stacks beneath the label at full width.
fn editor_row_stacked(
    theme: &Theme,
    label: String,
    description: String,
    control: AnyElement,
) -> Div {
    editor_row_base()
        .flex_col()
        .child(editor_row_label(theme, label, description))
        .child(div().mt(px(10.0)).w_full().child(control))
}

fn editor_row_base() -> Div {
    div()
        .w_full()
        .min_h(px(60.0))
        .px(px(20.0))
        .py(px(12.0))
        .flex()
}

fn editor_row_label(theme: &Theme, label: String, description: String) -> Div {
    div()
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(label),
        )
        .child(
            div()
                .mt(px(5.0))
                .text_size(px(12.5))
                .line_height(px(18.0))
                .text_color(theme.text_secondary)
                .child(description),
        )
}

/// Adds or removes `value`, but never empties the set — the last selected
/// entry can't be removed, so a weekly/monthly schedule always fires.
fn toggle_membership_min_one<T: PartialEq>(items: &mut Vec<T>, value: T) {
    if let Some(index) = items.iter().position(|item| *item == value) {
        if items.len() > 1 {
            items.remove(index);
        }
    } else {
        items.push(value);
    }
}

fn preset_label(preset: SchedulePreset) -> String {
    match preset {
        SchedulePreset::Manual => tr!("automations.preset_manual"),
        SchedulePreset::Hourly => tr!("automations.preset_hourly"),
        SchedulePreset::Daily => tr!("automations.frequency_daily"),
        SchedulePreset::Weekdays => tr!("automations.preset_weekdays"),
        SchedulePreset::Weekly => tr!("automations.frequency_weekly"),
        SchedulePreset::Monthly => tr!("automations.frequency_monthly"),
    }
}

fn overlap_label(overlap: OverlapPolicy) -> String {
    match overlap {
        OverlapPolicy::Skip => tr!("automations.overlap_skip"),
        OverlapPolicy::Queue => tr!("automations.overlap_queue"),
        OverlapPolicy::Concurrent => tr!("automations.overlap_concurrent"),
    }
}

fn trigger_label(trigger: NotificationTrigger) -> String {
    match trigger {
        NotificationTrigger::Always => tr!("automations.notify_always"),
        NotificationTrigger::OnSuccess => tr!("automations.notify_success"),
        NotificationTrigger::OnFailure => tr!("automations.notify_failure"),
    }
}

fn weekday_short(weekday: Weekday) -> String {
    match weekday {
        Weekday::Monday => tr!("automations.weekday_mon"),
        Weekday::Tuesday => tr!("automations.weekday_tue"),
        Weekday::Wednesday => tr!("automations.weekday_wed"),
        Weekday::Thursday => tr!("automations.weekday_thu"),
        Weekday::Friday => tr!("automations.weekday_fri"),
        Weekday::Saturday => tr!("automations.weekday_sat"),
        Weekday::Sunday => tr!("automations.weekday_sun"),
    }
}

/// A human-readable one-line summary of a schedule.
pub(super) fn schedule_summary(schedule: &Schedule) -> String {
    match schedule {
        Schedule::Manual => tr!("automations.summary_manual"),
        Schedule::Hourly { minute } => {
            tr!(
                "automations.summary_hourly",
                minute = format!("{minute:02}")
            )
        }
        Schedule::Daily { time } => tr!("automations.summary_daily", time = format_time(*time)),
        Schedule::Weekly { weekdays, time } => {
            let days = weekdays
                .iter()
                .map(|day| weekday_short(*day))
                .collect::<Vec<_>>()
                .join(", ");
            tr!(
                "automations.summary_weekly",
                days = days,
                time = format_time(*time)
            )
        }
        Schedule::Monthly { days, time } => {
            let days = days
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            tr!(
                "automations.summary_monthly",
                days = days,
                time = format_time(*time)
            )
        }
    }
}

/// The next-run line for the list, computed against the real local clock.
fn next_run_label(schedule: &Schedule) -> String {
    let now = chrono::Local::now().naive_local();
    match next_occurrence(schedule, now) {
        Some(next) => tr!("automations.next_run", time = format_next_run(next)),
        None => tr!("automations.next_run_none"),
    }
}

fn format_time(time: TimeOfDay) -> String {
    NaiveTime::from_hms_opt(u32::from(time.hour), u32::from(time.minute), 0)
        .map(|time| time.format("%-I:%M %p").to_string())
        .unwrap_or_else(|| format!("{:02}:{:02}", time.hour, time.minute))
}

fn format_next_run(when: NaiveDateTime) -> String {
    when.format("%a %b %-d · %-I:%M %p").to_string()
}

/// Converts a stored unix timestamp to local wall clock, the representation the
/// pure schedule core works in. This is the timezone boundary; the core itself
/// stays timezone-free.
fn local_naive(unix: u64) -> NaiveDateTime {
    chrono::DateTime::from_timestamp(unix as i64, 0)
        .map(|when| when.with_timezone(&chrono::Local).naive_local())
        .unwrap_or_default()
}

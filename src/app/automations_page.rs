//! The Automations full-page view: a first-class peer of the transcript, not a
//! settings tab. Lists saved automations with a schedule summary and computed
//! next-run time, and hosts a form to create, edit, and delete them. Scheduled
//! firing is daemon-owned; this page manages automations and requests Run-now.

use chrono::{NaiveDateTime, NaiveTime};
use gpui::{App, KeyBinding, actions};

use super::composer::AgentControlTarget;
use super::*;
use crate::automation::schedule::next_occurrence;
use crate::automation::{
    Automation, AutomationAgent, NotificationConfig, NotificationTrigger, OverlapPolicy, Schedule,
    TimeOfDay, Weekday,
};
use crate::model::{ProviderKind, SessionWorkspace};

const AUTOMATIONS_PAGE_GUTTER: f32 = 24.0;
const AUTOMATIONS_ACTION_HEIGHT: f32 = 30.0;
const AUTOMATIONS_ACTION_PADDING: f32 = 12.0;
const AUTOMATIONS_PICKER_WIDTH: f32 = 200.0;
const AUTOMATIONS_TIME_FIELD_WIDTH: f32 = 52.0;
const DELETE_DIALOG_CONTEXT: &str = "AutomationDeleteDialog";

actions!(
    waku_automation_delete_dialog,
    [DismissAutomationDeleteDialog]
);

/// Installs the key binding owned by the automation-delete confirmation modal.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        DismissAutomationDeleteDialog,
        Some(DELETE_DIALOG_CONTEXT),
    )]);
}

/// The value carried from the confirmation surface into the deletion path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DeleteAutomationRequest {
    automation_id: Uuid,
    delete_sessions: bool,
}

pub(super) struct AutomationDeleteDialogState {
    request: DeleteAutomationRequest,
    name: String,
    run_count: usize,
    session_count: usize,
    cancel_focus: FocusHandle,
    cascade_focus: Option<FocusHandle>,
    delete_focus: FocusHandle,
}

fn automation_session_ids(state: &PersistedState, automation_id: Uuid) -> Vec<Uuid> {
    state
        .sessions
        .iter()
        .filter(|session| session.originating_automation == Some(automation_id))
        .map(|session| session.id)
        .collect()
}

fn automation_delete_session_ids(
    state: &PersistedState,
    request: DeleteAutomationRequest,
) -> Vec<Uuid> {
    if request.delete_sessions {
        automation_session_ids(state, request.automation_id)
    } else {
        Vec::new()
    }
}

#[derive(Debug, Eq, PartialEq)]
struct AutomationDeleteResult {
    automation_deleted: bool,
    removed_session_ids: Vec<Uuid>,
    refused_session_ids: Vec<Uuid>,
}

/// Completes the state-level part of an automation deletion after each target
/// has gone through the ordinary session-removal path. Keeping this small seam
/// separate makes the cascade policy testable without constructing a GPUI
/// window; the real caller has already performed the runtime teardown by the
/// time this function removes the corresponding state rows.
fn finalize_automation_delete(
    state: &mut PersistedState,
    request: DeleteAutomationRequest,
    session_ids: &[Uuid],
    results: impl IntoIterator<Item = (Uuid, super::sessions::SessionRemovalResult)>,
) -> AutomationDeleteResult {
    let targeted = if request.delete_sessions {
        session_ids.iter().copied().collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    let mut result = AutomationDeleteResult {
        automation_deleted: false,
        removed_session_ids: Vec::new(),
        refused_session_ids: Vec::new(),
    };
    for (session_id, removal) in results {
        if !targeted.contains(&session_id) {
            continue;
        }
        match removal {
            super::sessions::SessionRemovalResult::Removed => {
                state.sessions.retain(|session| session.id != session_id);
                result.removed_session_ids.push(session_id);
            }
            super::sessions::SessionRemovalResult::ResponseForkInProgress => {
                result.refused_session_ids.push(session_id);
            }
            super::sessions::SessionRemovalResult::Missing => {}
        }
    }
    result.automation_deleted = state.remove_automation(request.automation_id);
    result
}

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
    pub(super) agent: AutomationAgent,
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
            agent: AutomationAgent::new(provider),
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
    fn from_automation(automation: &Automation, project_exists: bool) -> Self {
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
        let (fresh_worktree, base_branch) = match automation.workspace_for_project(project_exists) {
            SessionWorkspace::NewWorktree { base_branch } => (true, base_branch),
            SessionWorkspace::Worktree { branch, .. } => (true, Some(branch)),
            SessionWorkspace::Local => (false, None),
        };
        Self {
            id: Some(automation.id),
            agent: automation.agent.clone(),
            project_id: automation.project_id.filter(|_| project_exists),
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
        if self.project_id.is_some() && self.fresh_worktree {
            SessionWorkspace::NewWorktree {
                base_branch: self.base_branch.clone(),
            }
        } else {
            SessionWorkspace::Local
        }
    }

    pub(super) fn clear_project_binding(&mut self) {
        self.project_id = None;
        self.fresh_worktree = false;
        self.base_branch = None;
    }

    /// Writes the form onto an automation, preserving its id/created_at/history.
    fn apply_to(&self, automation: &mut Automation, name: String, prompt: String) {
        automation.name = name;
        automation.prompt = prompt;
        automation.agent = self.agent.clone();
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
    fn invalidate_automation_preparations(&mut self, id: Uuid) {
        let generation = self
            .automation_preparation_generations
            .entry(id)
            .or_default();
        *generation = generation.wrapping_add(1);
    }

    /// Switch the full-page view, committing an open automation editor first.
    ///
    /// Every navigation away from the editor routes through here. Text fields
    /// commit on blur, but a click that both blurs the field and changes the
    /// page can deliver the blur after the editor is already gone — so the
    /// commit has to happen before `active_page` moves, not as a side effect of
    /// focus. This is what makes clicking the breadcrumb straight after typing
    /// instructions keep them.
    pub(super) fn set_active_page(&mut self, page: Option<ActivePage>, cx: &mut Context<Self>) {
        if matches!(
            self.active_page,
            Some(ActivePage::Automations(AutomationsPage::Editor(_)))
        ) {
            self.commit_automation_editor(cx);
        }
        self.active_page = page;
    }

    pub(super) fn open_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active_page(Some(ActivePage::Automations(AutomationsPage::List)), cx);
        self.automations_scroll.set_offset(gpui::Point::default());
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    /// Opens the editor for `id`, or a blank one when `None`. Reachable from the
    /// sidebar's automation context menu as well as the list, so it brings the
    /// editor to the foreground.
    pub(super) fn open_automation_editor(
        &mut self,
        id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Commit any editor already open *before* the shared name and prompt
        // fields are reloaded below. Committing afterwards — as routing this
        // through `set_active_page` would — reads the incoming automation's text
        // back onto the outgoing one.
        self.commit_automation_editor(cx);
        let (editor, name, prompt) = match id.and_then(|id| self.state.automation(id)) {
            Some(automation) => {
                let project_exists = automation.project_id.is_some_and(|project_id| {
                    self.state
                        .projects
                        .iter()
                        .any(|project| project.id == project_id)
                });
                (
                    AutomationEditor::from_automation(automation, project_exists),
                    automation.name.clone(),
                    automation.prompt.clone(),
                )
            }
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
        // Assigned directly: the outgoing editor was already committed above.
        self.active_page = Some(ActivePage::Automations(AutomationsPage::Editor(editor)));
        self.automations_scroll.set_offset(gpui::Point::default());
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    /// The open automation editor form, if the editor is showing. Lets the
    /// shared agent controls read the current form values.
    pub(super) fn automation_editor(&self) -> Option<&AutomationEditor> {
        match self.active_page.as_ref() {
            Some(ActivePage::Automations(AutomationsPage::Editor(editor))) => Some(editor),
            _ => None,
        }
    }

    /// Mutates the open editor, if any, writes it through, then repaints.
    ///
    /// The editor commits on change: an existing automation has no Save button,
    /// so the control that moves the form is also the thing that persists it.
    /// Text fields are the exception — they would otherwise write once per
    /// keystroke, so they mutate through
    /// [`Self::edit_automation_form_uncommitted`] and commit on blur.
    pub(super) fn edit_automation_form(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut AutomationEditor),
    ) {
        if !self.edit_automation_form_uncommitted(cx, change) {
            return;
        }
        self.commit_automation_editor(cx);
    }

    /// Mutates the open editor without persisting it. Returns whether an editor
    /// was actually open.
    fn edit_automation_form_uncommitted(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut AutomationEditor),
    ) -> bool {
        let Some(ActivePage::Automations(AutomationsPage::Editor(editor))) =
            self.active_page.as_mut()
        else {
            return false;
        };
        change(editor);
        cx.notify();
        true
    }

    /// The editor's live name and prompt text, trimmed. The name falls back to
    /// a default so an automation is never nameless in the sidebar.
    fn automation_editor_text(&self, cx: &Context<Self>) -> (String, String) {
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
        (name, prompt)
    }

    /// Write the open editor onto its automation and persist it.
    ///
    /// A no-op while creating — an automation that has never been saved is
    /// materialized explicitly, so an incidental control change cannot conjure
    /// a half-filled row. If the automation disappeared underneath the editor
    /// (another client deleted it, and a task-state sync replaced the catalog),
    /// the form reverts to a create and says so rather than dropping the edit.
    pub(super) fn commit_automation_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ActivePage::Automations(AutomationsPage::Editor(editor))) =
            self.active_page.as_ref()
        else {
            return;
        };
        let Some(id) = editor.id else {
            return;
        };
        let editor = editor.clone();
        let (name, prompt) = self.automation_editor_text(cx);
        let Some(updated) = self.state.automation_mut(id).map(|automation| {
            editor.apply_to(automation, name, prompt);
            automation.clone()
        }) else {
            self.demote_automation_editor_to_create(cx);
            return;
        };
        self.state.queue_automation_upsert(updated);
        self.invalidate_automation_preparations(id);
        self.save();
        cx.notify();
    }

    /// Turn an editor whose automation no longer exists back into a create, so
    /// the next Create keeps everything the user has typed so far.
    fn demote_automation_editor_to_create(&mut self, cx: &mut Context<Self>) {
        if let Some(ActivePage::Automations(AutomationsPage::Editor(editor))) =
            self.active_page.as_mut()
        {
            editor.id = None;
        }
        self.show_toast(tr!("automations.save_failed_missing"));
        cx.notify();
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
            // Per-keystroke, so it must not write: these fields commit on blur
            // like the rest of the editor's text.
            self.edit_automation_form_uncommitted(cx, |editor| {
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

    /// Materialize the automation an editor in create mode is describing.
    ///
    /// This is the only explicit write the editor has. Once the row exists,
    /// every later change commits on its own, so the button disappears rather
    /// than becoming a Save the user has to remember to press.
    fn create_automation_from_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ActivePage::Automations(AutomationsPage::Editor(editor))) =
            self.active_page.as_ref()
        else {
            return;
        };
        if editor.id.is_some() {
            return;
        }
        let editor = editor.clone();
        let (name, prompt) = self.automation_editor_text(cx);
        // A prompt-less automation can never do anything — the spawn path bails
        // on empty input — so creating one is refused outright. An automation
        // that already exists is not blocked the same way: its prompt is
        // momentarily empty whenever the user clears the field to retype, and
        // the editor says so inline instead.
        if prompt.is_empty() {
            self.show_toast(tr!("automations.prompt_required"));
            cx.notify();
            return;
        }

        let mut automation = Automation::new(
            name.clone(),
            editor.agent.provider,
            crate::model::unix_time(),
        );
        editor.apply_to(&mut automation, name, prompt);
        let id = automation.id;
        self.state.push_automation(automation.clone());
        self.state.queue_automation_upsert(automation);
        self.invalidate_automation_preparations(id);
        self.save();
        // Stay on the editor, now bound to the row it just created, so the next
        // change commits to it instead of pushing a duplicate.
        if let Some(ActivePage::Automations(AutomationsPage::Editor(editor))) =
            self.active_page.as_mut()
        {
            editor.id = Some(id);
        }
        cx.notify();
    }

    fn open_automation_delete_dialog(
        &mut self,
        id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.automation_delete_dialog.is_some() {
            return;
        }
        let Some(automation) = self.state.automation(id) else {
            return;
        };
        let session_count = automation_session_ids(&self.state, id).len();
        let dialog = AutomationDeleteDialogState {
            request: DeleteAutomationRequest {
                automation_id: id,
                delete_sessions: false,
            },
            name: automation.name.clone(),
            run_count: automation.history.len(),
            session_count,
            cancel_focus: cx.focus_handle(),
            cascade_focus: (session_count > 0).then(|| cx.focus_handle()),
            delete_focus: cx.focus_handle(),
        };
        let cancel_focus = dialog.cancel_focus.clone();
        self.automation_delete_dialog = Some(dialog);
        // The deferred layer is not in the focus tree until after it draws.
        // Focus Cancel only after the second frame so the underlying editor
        // cannot receive the next key press.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&cancel_focus, cx));
        });
        cx.notify();
    }

    fn toggle_automation_delete_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.automation_delete_dialog.as_mut() else {
            return;
        };
        if dialog.session_count == 0 {
            return;
        }
        dialog.request.delete_sessions = !dialog.request.delete_sessions;
        cx.notify();
    }

    fn dismiss_automation_delete_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.automation_delete_dialog.take().is_none() {
            return;
        }
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    fn confirm_automation_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.automation_delete_dialog.take() else {
            return;
        };
        let request = dialog.request;
        let session_ids = automation_delete_session_ids(&self.state, request);
        // Invalidate preparation results before tearing down any spawned
        // session. A completion racing this action must not resurrect the
        // automation's work after the modal confirms deletion.
        self.invalidate_automation_preparations(request.automation_id);

        let removal_results = session_ids
            .iter()
            .copied()
            .map(|session_id| {
                (
                    session_id,
                    self.remove_session_for_automation_cascade(session_id, cx),
                )
            })
            .collect::<Vec<_>>();
        let result =
            finalize_automation_delete(&mut self.state, request, &session_ids, removal_results);

        if result.automation_deleted {
            self.state.queue_automation_removal(
                request.automation_id,
                request.delete_sessions && result.refused_session_ids.is_empty(),
            );
            self.automation_card_focuses
                .borrow_mut()
                .remove(&request.automation_id);
            // Deleting the automation whose editor is open returns to the list.
            // Assigned directly rather than through `set_active_page`: the row
            // is gone on purpose, and committing the editor here would only
            // report it as missing.
            if matches!(
                &self.active_page,
                Some(ActivePage::Automations(AutomationsPage::Editor(editor)))
                    if editor.id == Some(request.automation_id)
            ) {
                self.active_page = Some(ActivePage::Automations(AutomationsPage::List));
            }
            self.save();
        }
        if !result.refused_session_ids.is_empty() {
            self.show_toast(tr!(
                "automations.delete_sessions_partial",
                count = result.refused_session_ids.len()
            ));
        }
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    /// Closes the editor and returns to the automations list.
    fn close_automation_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active_page(Some(ActivePage::Automations(AutomationsPage::List)), cx);
        window.focus(&self.automations_focus, cx);
        cx.notify();
    }

    pub(super) fn toggle_automation_enabled(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.invalidate_automation_preparations(id);
        let updated = self.state.automation_mut(id).map(|automation| {
            automation.enabled = !automation.enabled;
            automation.updated_at = crate::model::unix_time();
            automation.clone()
        });
        if let Some(updated) = updated {
            self.state.queue_automation_upsert(updated);
            self.save();
            cx.notify();
        }
    }

    /// Resolves a completed automation run's history outcome. Notification
    /// policy is owned by the daemon now; the resulting notification event is
    /// rendered by each attached client.
    ///
    /// A no-op for manual sessions and for follow-up turns on a run that already
    /// resolved — only a run still marked `Running` is acted on.
    pub(super) fn settle_automation_run(
        &mut self,
        session_id: Uuid,
        outcome: crate::automation::RunOutcome,
        _cx: &mut Context<Self>,
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
        if let Some(automation) = self.state.automation_mut(automation_id) {
            automation.settle_session_run(session_id, outcome);
        }
    }

    /// Run-now uses the daemon's scheduler/execution path, then opens the
    /// returned session so the result is visible in the desktop transcript.
    pub(super) fn run_automation_now(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let daemon = self.daemon.client();
        let event_wake = self.event_wake_tx.clone();
        cx.spawn(async move |waku, cx| {
            let response = cx
                .background_executor()
                .spawn(async move {
                    let response = daemon.request(
                        Uuid::nil(),
                        Uuid::nil(),
                        waku_client::Command::RunAutomation {
                            automation_id: id,
                            catch_up: false,
                        },
                    )?;
                    let waku_client::ResponsePayload::AutomationRunStarted {
                        automation,
                        session,
                        runtime_id,
                        supports_steer,
                    } = response
                    else {
                        anyhow::bail!("the daemon returned an invalid automation response");
                    };
                    let (event_tx, events) = driver::event_channel(event_wake);
                    let handle = driver::attach_remote(
                        daemon,
                        session.id,
                        runtime_id,
                        supports_steer,
                        None,
                        event_tx,
                    )?;
                    Ok((automation, session, PreparedDriver { handle, events }))
                })
                .await;
            let _ = waku.update(cx, |waku, cx| match response {
                Ok((automation, session, prepared)) => {
                    if let Some(existing) = waku
                        .state
                        .automations
                        .iter_mut()
                        .find(|existing| existing.id == automation.id)
                    {
                        *existing = automation;
                    } else {
                        waku.state.automations.push(automation);
                    }
                    if let Some(existing) = waku
                        .state
                        .sessions
                        .iter_mut()
                        .find(|existing| existing.id == session.id)
                    {
                        *existing = session.clone();
                    } else {
                        waku.state.push_session(session.clone());
                    }
                    waku.install_prepared_driver(session.id, prepared);
                    waku.set_active_page(None, cx);
                    waku.select_session(session.id, cx);
                    cx.notify();
                }
                // The daemon reports an overlap-policy refusal (already running,
                // or queued behind the active run) through this same path, so
                // the label has to describe starting the automation rather than
                // saving state.
                Err(error) => waku.show_toast(tr!("errors.run_automation", error = error)),
            });
        })
        .detach();
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

        let body = match self.active_page.as_ref() {
            Some(ActivePage::Automations(AutomationsPage::Editor(_))) => {
                self.render_automation_editor(cx)
            }
            _ => self.render_automations_list(cx),
        };

        div()
            .key_context("Waku")
            .track_focus(&self.automations_focus)
            .tab_group()
            .tab_stop(false)
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

    fn render_automations_shell(
        &self,
        header: impl IntoElement,
        content: impl IntoElement,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div().flex_none().pt(px(8.0)).pb(px(8.0)).child(
                    div()
                        .w_full()
                        .max_w(px(CONTENT_MAX_WIDTH))
                        .mx_auto()
                        .px(px(AUTOMATIONS_PAGE_GUTTER))
                        .child(header),
                ),
            )
            .child(
                div()
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
                            .px(px(AUTOMATIONS_PAGE_GUTTER))
                            .child(content),
                    ),
            )
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
                    .h(px(AUTOMATIONS_ACTION_HEIGHT))
                    .px(px(AUTOMATIONS_ACTION_PADDING))
                    .rounded(px(7.0))
                    .bg(theme.inverse)
                    .text_color(theme.on_inverse)
                    .text_size(px(13.0))
                    .cursor_default()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|element| element.opacity(0.9))
                    .child(icon("icons/plus.svg", 14.0, theme.on_inverse))
                    .child(tr!("automations.new"))
                    .on_activation(cx, |this, window, cx| {
                        this.open_automation_editor(None, window, cx);
                    }),
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
            // Resolved once for the whole list, not per row: `Local::now` sits
            // behind a cache that re-stats the host timezone, and this runs on
            // every frame the page is up. `render` already schedules a
            // time-label wake, so the next-run text still ticks forward.
            let now = chrono::Local::now().naive_local();
            // Rendered eagerly rather than through `list()`. Virtualization
            // pays for itself on session history, which is unbounded; an
            // automation is a hand-authored record and the realistic ceiling is
            // a few dozen. Each row is a handful of divs over data already in
            // memory — no I/O, no per-row clock — so the whole list costs less
            // than the virtualized row builder's bookkeeping would. Revisit if
            // automations ever become machine-generated.
            for automation in &self.state.automations {
                list = list.child(self.render_automation_row(automation, now, &theme, cx));
            }
        }

        self.render_automations_shell(header, list)
    }

    fn render_automation_row(
        &self,
        automation: &Automation,
        now: NaiveDateTime,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = automation.id;
        let enabled = automation.enabled;
        let next_run = next_run_label(&automation.schedule, now);
        let summary = schedule_summary(&automation.schedule);
        let row_focus = self
            .automation_card_focuses
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| cx.focus_handle())
            .clone();

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
            .track_focus(&row_focus)
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
            .on_activation(cx, move |this, window, cx| {
                this.open_automation_editor(Some(id), window, cx);
            })
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

    pub(super) fn render_automation_delete_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.automation_delete_dialog.as_ref()?;
        let theme = Theme::current(cx);
        let name = dialog.name.clone();
        let run_count = dialog.run_count;
        let session_count = dialog.session_count;
        let delete_sessions = dialog.request.delete_sessions;
        let cancel_focus = dialog.cancel_focus.clone();
        let delete_focus = dialog.delete_focus.clone();

        let cancel = div()
            .id("automation-delete-cancel")
            .track_focus(&cancel_focus)
            .tab_index(0)
            .h(px(32.0))
            .px(px(14.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(px(13.0))
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .child(tr!("automations.cancel"))
            .on_activation(cx, |this, window, cx| {
                this.dismiss_automation_delete_dialog(window, cx);
            });

        let delete = div()
            .id("automation-delete-confirm")
            .track_focus(&delete_focus)
            .tab_index(0)
            .h(px(32.0))
            .px(px(14.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.danger)
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(px(13.0))
            .bg(theme.danger)
            .text_color(theme.on_inverse)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.opacity(0.9))
            .child(tr!("automations.delete"))
            .on_activation(cx, |this, window, cx| {
                this.confirm_automation_delete(window, cx);
            });

        let cascade = dialog.cascade_focus.as_ref().map(|focus| {
            div()
                .id("automation-delete-sessions")
                .track_focus(focus)
                .tab_index(0)
                .w_full()
                .px(px(10.0))
                .py(px(9.0))
                .rounded(px(8.0))
                .flex()
                .items_start()
                .gap(px(10.0))
                .cursor_default()
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .hover(|element| element.bg(theme.overlay))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_automation_delete_sessions(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.toggle_automation_delete_sessions(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .mt(px(1.0))
                        .size(px(16.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(if delete_sessions {
                            theme.accent
                        } else {
                            theme.border_strong
                        })
                        .bg(if delete_sessions {
                            theme.accent
                        } else {
                            gpui::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(delete_sessions, |checkbox| {
                            checkbox.child(icon("icons/check.svg", 12.0, theme.on_inverse))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .text_size(px(13.0))
                        .line_height(px(18.0))
                        .text_color(theme.text)
                        .child(tr!("automations.delete_sessions", count = session_count))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.danger)
                                .child(tr!("automations.delete_irreversible")),
                        ),
                )
        });

        let mut card = div()
            .id("automation-delete-dialog-card")
            .key_context(DELETE_DIALOG_CONTEXT)
            .on_action(
                cx.listener(|waku, _: &DismissAutomationDeleteDialog, window, cx| {
                    waku.dismiss_automation_delete_dialog(window, cx);
                }),
            )
            .tab_group()
            .tab_stop(false)
            .w_full()
            .max_w(px(460.0))
            .p(px(20.0))
            .rounded(px(16.0))
            .bg(theme.composer)
            .shadow_xl()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(17.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("automations.delete_title", name = name)),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .line_height(px(19.0))
                    .text_color(theme.text_secondary)
                    .child(tr!("automations.delete_runs", count = run_count)),
            );

        if let Some(cascade) = cascade {
            card = card.child(cascade);
        }

        card = card.child(
            div()
                .mt(px(4.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(cancel)
                .child(delete),
        );

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("automation-delete-dialog-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|waku, _, window, cx| {
                    waku.dismiss_automation_delete_dialog(window, cx);
                }),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }

    /// The delete control opens a confirmation modal. The consequences belong
    /// in the modal because deleting the automation and deleting its output are
    /// intentionally separate user intents.
    fn render_automation_delete_button(
        &self,
        id: Uuid,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("automation-delete-{id}")))
            .track_focus(&self.automation_delete_focus)
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(AUTOMATIONS_ACTION_HEIGHT))
            .px(px(AUTOMATIONS_ACTION_PADDING))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(px(13.0))
            .text_color(theme.text_secondary)
            .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
            .child(icon("icons/trash.svg", 13.0, theme.text_tertiary))
            .child(tr!("automations.delete"))
            .on_activation(cx, move |this, window, cx| {
                this.open_automation_delete_dialog(id, window, cx);
            })
    }

    fn render_automation_enable_toggle(
        &self,
        id: Uuid,
        enabled: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        toggle_switch(
            SharedString::from(format!("automation-toggle-{id}")),
            enabled,
            false,
            *theme,
            cx,
            move |this, _, cx| this.toggle_automation_enabled(id, cx),
        )
    }

    fn render_automation_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(ActivePage::Automations(AutomationsPage::Editor(editor))) =
            self.active_page.as_ref()
        else {
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
                    .on_activation(cx, |this, window, cx| {
                        this.close_automation_editor(window, cx);
                    }),
            )
            .child(icon("icons/chevron-right.svg", 14.0, theme.text_tertiary))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(crumb_name),
            );

        // Create is the editor's only explicit write, and only while the
        // automation does not exist yet. Once it does, every control commits on
        // change, so leaving a Save button there would imply the rest of the
        // form was still waiting on it.
        let create = id.is_none().then(|| {
            div()
                .id("automation-create")
                .track_focus(&self.automation_save_focus)
                .tab_index(0)
                .flex_none()
                .h(px(AUTOMATIONS_ACTION_HEIGHT))
                .px(px(AUTOMATIONS_ACTION_PADDING))
                .rounded(px(7.0))
                // A border that is only transparent, not absent: Delete beside
                // it carries one, so this keeps both boxes the same size and
                // stops the button resizing when focus draws its ring.
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
                .child(tr!("automations.create"))
                .on_activation(cx, |this, _, cx| {
                    this.create_automation_from_editor(cx);
                })
        });

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
                    .children(create),
            );

        // One scroll flow, top to bottom like a settings page. The composer is
        // the Instructions field of the first card rather than a surface pinned
        // to the bottom, so every part of the automation is one list of fields.
        let content = div()
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
                        TextField::new("automation-name-field", self.automation_name_input.clone())
                            .w(px(260.0)),
                    ),
                    editor_row_stacked(
                        &theme,
                        tr!("automations.field_instructions"),
                        tr!("automations.field_instructions_description"),
                        self.editor_agent_section(id, &theme, cx),
                    ),
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
                        ),
                    ),
                ],
            ))
            .child(self.editor_schedule_section(&editor, &theme, cx))
            .child(self.editor_behavior_section(&editor, &theme, cx));

        self.render_automations_shell(header, content)
    }

    /// A labeled enum dropdown. The value list, label mapping, and mutation are
    /// the only field-specific pieces; menu chrome and selection handling stay
    /// here.
    fn picker<T>(
        &self,
        id: &'static str,
        value: T,
        options: impl IntoIterator<Item = T>,
        width: f32,
        disabled: bool,
        cx: &mut Context<Self>,
        label: impl Fn(T) -> String + 'static,
        apply: impl Fn(&mut AutomationEditor, T) + 'static,
    ) -> AnyElement
    where
        T: Copy + Eq + 'static,
    {
        let options = options.into_iter().collect::<Vec<_>>();
        let labels = std::rc::Rc::new(label);
        let apply = std::rc::Rc::new(apply);
        let selected_label = labels(value);
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle(id, cx);
        dropdown_menu(
            MenuChip::new(id)
                .label(selected_label)
                .outlined()
                .selected(handle.is_open())
                .disabled(disabled)
                .w(px(width))
                .justify_between(),
            SharedString::from(format!("{id}-menu")),
            &handle,
            MenuAlign::BelowRight,
            move |_| {
                let labels = labels.clone();
                let apply = apply.clone();
                let weak = weak.clone();
                options
                    .iter()
                    .copied()
                    .map(move |option| {
                        let labels = labels.clone();
                        let apply = apply.clone();
                        let item_weak = weak.clone();
                        MenuItem::new(labels(option), move |_, cx| {
                            let _ = item_weak.update(cx, |this, cx| {
                                this.edit_automation_form(cx, |editor| apply(editor, option));
                            });
                        })
                        .selected(option == value)
                    })
                    .collect()
            },
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
                .on_activation(cx, move |this, _, cx| {
                    this.run_automation_now(id, cx);
                })
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

        // An existing automation commits on change, so an empty prompt is a
        // state the user can sit in — clearing the field to retype produces it.
        // Say what that costs inline instead of blocking the write, and pair the
        // warning colour with text so it does not rely on colour alone.
        let prompt_missing = (id.is_some()
            && self
                .automation_prompt_input
                .read(cx)
                .content()
                .trim()
                .is_empty())
        .then(|| {
            div()
                .mt(px(6.0))
                .pl(px(10.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(px(11.5))
                .text_color(theme.warning)
                .child(icon("icons/alert.svg", 11.0, theme.warning))
                .child(tr!("automations.prompt_missing"))
        });

        div()
            .flex()
            .flex_col()
            .child(card)
            .children(prompt_missing)
            .child(workspace_footer)
    }

    fn editor_schedule_section(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        // Schedule preset dropdown.
        let preset_picker = self.picker(
            "automation-frequency",
            editor.preset,
            SchedulePreset::ALL,
            AUTOMATIONS_PICKER_WIDTH,
            false,
            cx,
            preset_label,
            |editor, preset| editor.preset = preset,
        );

        // Time-of-day: freeform hour and minute fields the user types by hand.
        // The `on_automation_time_edited` subscription clamps and validates.
        let time_picker = || {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    TextField::new("automation-hour-field", self.automation_hour_input.clone())
                        .w(px(AUTOMATIONS_TIME_FIELD_WIDTH)),
                )
                .child(div().text_color(theme.text_tertiary).child(":"))
                .child(
                    TextField::new(
                        "automation-minute-field",
                        self.automation_minute_input.clone(),
                    )
                    .w(px(AUTOMATIONS_TIME_FIELD_WIDTH)),
                )
        };

        let mut rows = vec![editor_row(
            theme,
            tr!("automations.field_schedule_frequency"),
            // Manual has no follow-up row, so its caption explains itself here.
            match editor.preset {
                SchedulePreset::Manual => tr!("automations.hint_manual"),
                _ => tr!("automations.field_schedule_frequency_description"),
            },
            preset_picker,
        )];

        if matches!(
            editor.preset,
            SchedulePreset::Daily
                | SchedulePreset::Weekdays
                | SchedulePreset::Weekly
                | SchedulePreset::Monthly
        ) {
            rows.push(editor_row(
                theme,
                tr!("automations.field_time"),
                tr!("automations.field_time_description"),
                time_picker(),
            ));
        }

        // Each preset shows only the controls that differ from the shared time
        // row above.
        match editor.preset {
            SchedulePreset::Manual | SchedulePreset::Daily | SchedulePreset::Weekdays => {}
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
                        .w(px(AUTOMATIONS_TIME_FIELD_WIDTH)),
                    );
                rows.push(editor_row(
                    theme,
                    tr!("automations.field_time"),
                    tr!("automations.field_minute_description"),
                    minute_picker,
                ));
            }
            SchedulePreset::Weekly => {
                rows.push(editor_row_stacked(
                    theme,
                    tr!("automations.field_days"),
                    tr!("automations.field_weekdays_description"),
                    self.weekday_chips(editor, theme, cx),
                ));
            }
            SchedulePreset::Monthly => {
                rows.push(editor_row_stacked(
                    theme,
                    tr!("automations.field_days"),
                    tr!("automations.field_monthdays_description"),
                    self.monthday_chips(editor, theme, cx),
                ));
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
        // Overlap policy.
        let overlap_picker = self.picker(
            "automation-overlap",
            editor.overlap,
            OverlapPolicy::ALL,
            AUTOMATIONS_PICKER_WIDTH,
            false,
            cx,
            overlap_label,
            |editor, overlap| editor.overlap = overlap,
        );

        // Notifications: an enable toggle plus a trigger picker.
        let notify_toggle = self.editor_toggle(
            "automation-notify",
            editor.notify_enabled,
            theme,
            cx,
            |editor| editor.notify_enabled = !editor.notify_enabled,
        );

        let trigger_picker = self.picker(
            "automation-trigger",
            editor.notify_trigger,
            NotificationTrigger::ALL,
            AUTOMATIONS_PICKER_WIDTH,
            !editor.notify_enabled,
            cx,
            trigger_label,
            |editor, trigger| editor.notify_trigger = trigger,
        );

        editor_card(
            theme,
            vec![
                editor_row(
                    theme,
                    tr!("automations.field_overlap"),
                    tr!("automations.field_overlap_description"),
                    overlap_picker,
                ),
                editor_row(
                    theme,
                    tr!("automations.field_notifications"),
                    tr!("automations.field_notifications_description"),
                    notify_toggle,
                ),
                editor_row(
                    theme,
                    tr!("automations.field_notify_when"),
                    tr!("automations.field_notify_when_description"),
                    trigger_picker,
                ),
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
        toggle_switch(id, on, false, *theme, cx, move |this, _, cx| {
            this.edit_automation_form(cx, |editor| change(editor))
        })
    }

    fn weekday_chips(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.selection_chips(
            Weekday::ALL,
            editor.weekdays.clone(),
            6.0,
            |weekday| SharedString::from(format!("weekday-{}", weekday_short(weekday))),
            weekday_short,
            theme,
            cx,
            |editor, weekday| toggle_membership_min_one(&mut editor.weekdays, weekday),
        )
    }

    fn monthday_chips(
        &self,
        editor: &AutomationEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.selection_chips(
            1u8..=31,
            editor.monthdays.clone(),
            4.0,
            |day| SharedString::from(format!("monthday-{day}")),
            |day| day.to_string(),
            theme,
            cx,
            |editor, day| toggle_membership_min_one(&mut editor.monthdays, day),
        )
    }

    fn selection_chips<T>(
        &self,
        values: impl IntoIterator<Item = T>,
        selected: Vec<T>,
        gap: f32,
        id: impl Fn(T) -> SharedString,
        label: impl Fn(T) -> String,
        theme: &Theme,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut AutomationEditor, T) + 'static,
    ) -> AnyElement
    where
        T: Copy + PartialEq + 'static,
    {
        let apply = std::rc::Rc::new(apply);
        let mut row = div().flex().flex_wrap().gap(px(gap));
        for value in values {
            let is_selected = selected.contains(&value);
            let apply = apply.clone();
            row = row.child(self.selection_chip(
                id(value),
                label(value),
                is_selected,
                theme,
                cx,
                move |editor| apply(editor, value),
            ));
        }
        row.into_any_element()
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
            .on_activation(cx, move |this, _, cx| {
                this.edit_automation_form(cx, |editor| change(editor));
            })
    }
}

/// A grouped card, matching the cards the settings pages are built from: a
/// heading and caption on a raised surface, with the group's field rows
/// appended as children.
fn editor_card<I, E>(theme: &Theme, rows: I) -> Div
where
    I: IntoIterator<Item = E>,
    E: IntoElement,
{
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
fn editor_row(theme: &Theme, label: String, description: String, control: impl IntoElement) -> Div {
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
    control: impl IntoElement,
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

/// The next-run line for one row, against a reference time the caller resolved
/// once for the whole list.
///
/// `now` is injected rather than read here: this runs for every visible row on
/// every frame, and `Local::now` re-reads the host timezone behind a cache, so
/// per-row calls put a syscall-bearing path in the render loop.
fn next_run_label(schedule: &Schedule, now: NaiveDateTime) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn automation_delete_fixture(session_count: usize) -> (PersistedState, Uuid, Vec<Uuid>) {
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions.clear();
        let project_id = state.projects[0].id;
        let automation = Automation::new("Nightly", ProviderKind::Codex, 1_000);
        let automation_id = automation.id;
        state.push_automation(automation);
        let mut session_ids = Vec::new();
        for _ in 0..session_count {
            let mut session = state.new_session(project_id, ProviderKind::Codex);
            session.originating_automation = Some(automation_id);
            session_ids.push(session.id);
            state.push_session(session);
        }
        (state, automation_id, session_ids)
    }

    #[test]
    fn automation_delete_without_cascade_removes_only_the_automation() {
        let (mut state, automation_id, session_ids) = automation_delete_fixture(1);
        let request = DeleteAutomationRequest {
            automation_id,
            delete_sessions: false,
        };

        let result = finalize_automation_delete(&mut state, request, &session_ids, []);

        assert!(result.automation_deleted);
        assert!(result.removed_session_ids.is_empty());
        assert!(result.refused_session_ids.is_empty());
        assert!(state.automation(automation_id).is_none());
        assert_eq!(
            state
                .sessions
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            session_ids
        );
    }

    #[test]
    fn automation_delete_with_cascade_targets_only_its_sessions() {
        let (mut state, automation_id, session_ids) = automation_delete_fixture(2);
        let other_automation = Automation::new("Weekly", ProviderKind::Codex, 1_000);
        let other_automation_id = other_automation.id;
        state.push_automation(other_automation);
        let project_id = state.projects[0].id;
        let mut unrelated = state.new_session(project_id, ProviderKind::Codex);
        unrelated.originating_automation = Some(other_automation_id);
        let unrelated_id = unrelated.id;
        state.push_session(unrelated);
        let request = DeleteAutomationRequest {
            automation_id,
            delete_sessions: true,
        };

        let targets = automation_delete_session_ids(&state, request);
        let result = finalize_automation_delete(
            &mut state,
            request,
            &targets,
            targets
                .iter()
                .copied()
                .map(|id| (id, super::super::sessions::SessionRemovalResult::Removed)),
        );

        assert_eq!(targets, session_ids);
        assert!(!targets.contains(&unrelated_id));
        assert!(result.automation_deleted);
        assert_eq!(result.removed_session_ids, session_ids);
        assert!(
            state
                .sessions
                .iter()
                .all(|session| session.id == unrelated_id)
        );
    }

    #[test]
    fn automation_delete_with_no_sessions_has_no_cascade_targets() {
        let (mut state, automation_id, session_ids) = automation_delete_fixture(0);
        let request = DeleteAutomationRequest {
            automation_id,
            delete_sessions: true,
        };

        assert!(session_ids.is_empty());
        assert!(automation_delete_session_ids(&state, request).is_empty());
        let result = finalize_automation_delete(&mut state, request, &[], []);
        assert!(result.automation_deleted);
        assert!(result.removed_session_ids.is_empty());
        assert!(result.refused_session_ids.is_empty());
        assert!(state.automation(automation_id).is_none());
    }

    #[test]
    fn automation_delete_cascade_keeps_a_refusing_session_and_aggregates_once() {
        let (mut state, automation_id, session_ids) = automation_delete_fixture(2);
        let request = DeleteAutomationRequest {
            automation_id,
            delete_sessions: true,
        };
        let refusing = session_ids[1];
        let result = finalize_automation_delete(
            &mut state,
            request,
            &session_ids,
            [
                (
                    session_ids[0],
                    super::super::sessions::SessionRemovalResult::Removed,
                ),
                (
                    refusing,
                    super::super::sessions::SessionRemovalResult::ResponseForkInProgress,
                ),
            ],
        );

        assert!(result.automation_deleted);
        assert_eq!(result.removed_session_ids, vec![session_ids[0]]);
        assert_eq!(result.refused_session_ids, vec![refusing]);
        assert_eq!(result.refused_session_ids.len(), 1);
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, refusing);
    }

    #[test]
    fn clearing_an_editor_project_also_clears_worktree_state() {
        let mut editor = AutomationEditor::new(ProviderKind::Codex);
        editor.project_id = Some(Uuid::new_v4());
        editor.fresh_worktree = true;
        editor.base_branch = Some("main".to_owned());

        editor.clear_project_binding();

        assert_eq!(editor.project_id, None);
        assert!(!editor.fresh_worktree);
        assert_eq!(editor.base_branch, None);
        assert_eq!(editor.workspace(), SessionWorkspace::Local);
    }

    #[test]
    fn missing_saved_project_hydrates_as_a_local_editor() {
        let mut automation = Automation::new("Nightly", ProviderKind::Codex, 1_000);
        automation.project_id = Some(Uuid::new_v4());
        automation.workspace = SessionWorkspace::NewWorktree {
            base_branch: Some("main".to_owned()),
        };

        let editor = AutomationEditor::from_automation(&automation, false);

        assert_eq!(editor.project_id, None);
        assert!(!editor.fresh_worktree);
        assert_eq!(editor.base_branch, None);
        assert_eq!(editor.workspace(), SessionWorkspace::Local);
    }
}

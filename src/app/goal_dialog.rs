//! Modal editor for Codex thread goals — the `/goal` command's surface.
//!
//! The dialog reads goal state live from the selected session, so provider
//! notifications (progress accounting, status flips) keep an open dialog
//! current. Mutations go through the session's live runtime and come back as
//! `DriverEvent::GoalUpdated`; the dialog itself holds only the objective
//! draft.

use gpui::{KeyBinding, actions};

use crate::model::{GoalOperation, MessageRole, ThreadGoal, ThreadGoalStatus};
use crate::usage::format_tokens;

use super::*;

actions!(waku_goal_dialog, [ConfirmGoalDialog, DismissGoalDialog]);

const DIALOG_CONTEXT: &str = "GoalDialog";
const DIALOG_INPUT_CONTEXT: &str = "GoalDialog > TextInput";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "secondary-enter",
            ConfirmGoalDialog,
            Some(DIALOG_INPUT_CONTEXT),
        ),
        KeyBinding::new("secondary-enter", ConfirmGoalDialog, Some(DIALOG_CONTEXT)),
        KeyBinding::new("escape", DismissGoalDialog, Some(DIALOG_CONTEXT)),
    ]);
}

/// A deferred open. The objective editor needs a `Window` to exist, which
/// command paths (composer submissions) do not carry, so opening stages a
/// request that the next frame materializes.
pub(super) struct GoalDialogRequest {
    pub session_id: Uuid,
    /// Objective text to start the editor with; `None` prefills the current
    /// goal's objective.
    pub prefill: Option<String>,
    /// Saving replaces the existing goal — clears it first so the new
    /// objective starts with fresh token and time accounting.
    pub replace: bool,
}

pub(super) struct GoalDialogState {
    session_id: Uuid,
    replace: bool,
    objective: Entity<TextInput>,
    save_focus: FocusHandle,
    status_focus: FocusHandle,
    clear_focus: FocusHandle,
}

impl Waku {
    /// Stage the goal dialog for `session_id`; the next frame builds it.
    pub(super) fn request_goal_dialog(
        &mut self,
        session_id: Uuid,
        prefill: Option<String>,
        replace: bool,
        cx: &mut Context<Self>,
    ) {
        self.goal_dialog_request = Some(GoalDialogRequest {
            session_id,
            prefill,
            replace,
        });
        cx.notify();
    }

    fn materialize_goal_dialog(
        &mut self,
        request: GoalDialogRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let content = request.prefill.or_else(|| {
            self.state
                .sessions
                .iter()
                .find(|session| session.id == request.session_id)
                .and_then(|session| session.thread_goal.as_ref())
                .map(|goal| goal.objective.clone())
        });
        let objective = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line()
                .placeholder(tr!("goal.objective_placeholder"))
        });
        if let Some(content) = content {
            objective.update(cx, |input, cx| input.set_content(content, cx));
        }
        let objective_focus = objective.read(cx).focus();
        self.goal_dialog = Some(GoalDialogState {
            session_id: request.session_id,
            replace: request.replace,
            objective,
            save_focus: cx.focus_handle(),
            status_focus: cx.focus_handle(),
            clear_focus: cx.focus_handle(),
        });
        // Like Waku's other deferred surfaces, the modal joins the dispatch
        // tree only after it has drawn. Focus it two frames later so typing
        // cannot fall through to the composer beneath it.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&objective_focus, cx));
        });
        cx.notify();
    }

    fn close_goal_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_dialog_request = None;
        if self.goal_dialog.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Hand a goal operation to the session's runtime, starting one first
    /// when none exists yet. Goals attach to the provider thread, not to any
    /// turn — the Codex CLI opens its thread at launch, so `/goal` works
    /// there before the first message. Waku starts providers lazily, so the
    /// goal path starts the runtime itself and the queued operations drain
    /// the moment it installs.
    pub(super) fn dispatch_goal_operation(
        &mut self,
        session_id: Uuid,
        operation: GoalOperation,
        cx: &mut Context<Self>,
    ) {
        self.record_goal_submission(session_id, &operation, cx);
        self.begin_goal_pursuit_turn(session_id, &operation, cx);
        if let Some(runtime) = self.runtimes.get(&session_id) {
            runtime.driver.goal(operation);
            return;
        }
        self.pending_goal_operations
            .entry(session_id)
            .or_default()
            .push(operation);
        self.start_goal_runtime(session_id, cx);
    }

    /// A submitted objective leaves a persistent transcript record — the
    /// centered pill a system message renders as — the way a submission
    /// leaves its user message. Pushed before the pursuit turn exists so it
    /// stays turn-less and survives an unwound pursuit.
    fn record_goal_submission(
        &mut self,
        session_id: Uuid,
        operation: &GoalOperation,
        cx: &mut Context<Self>,
    ) {
        let GoalOperation::Set {
            objective: Some(objective),
            ..
        } = operation
        else {
            return;
        };
        let Some(session) = self.state.session_mut(session_id) else {
            return;
        };
        session.set_title_from_prompt(objective);
        let notice = tr!("goal.set_notice", objective = notice_objective(objective));
        session.push_message(MessageRole::System, notice);
        session.updated_at = crate::model::unix_time();
        self.state.mark_session_dirty(session_id);
        cx.notify();
    }

    /// Activating a goal on an idle thread makes Codex pursue it right away
    /// (`apply_external_goal_set` → `continue_if_idle`), so begin its turn
    /// optimistically — exactly how a submission's turn begins at accept —
    /// instead of leaving the empty-task page up until the provider's start
    /// report arrives seconds later. The provider's `turn/started` confirms
    /// the turn; errors and a watchdog unwind an unconfirmed one.
    fn begin_goal_pursuit_turn(
        &mut self,
        session_id: Uuid,
        operation: &GoalOperation,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            operation,
            GoalOperation::Set {
                status: Some(ThreadGoalStatus::Active),
                ..
            }
        ) {
            return;
        }
        let Some(session) = self
            .state
            .session_mut(session_id)
            .filter(|session| session.active_turn_id().is_none() && !session.status.is_busy())
        else {
            return;
        };
        let turn_id = session.begin_provider_turn();
        session.status = SessionStatus::Connecting;
        self.state.mark_session_dirty(session_id);
        cx.notify();
        // Continuation can legitimately never come — an inherited deferral,
        // or a goal feature disabled provider-side. Do not let the working
        // indicator outlive that silence.
        cx.spawn(async move |waku, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(30))
                .await;
            let _ = waku.update(cx, |waku, cx| {
                let stale = waku
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .is_some_and(|session| {
                        session.active_turn_id() == Some(turn_id)
                            && session.active_turn_is_unconfirmed_pursuit()
                    });
                if stale {
                    waku.unwind_unconfirmed_pursuit_turn(session_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Remove an optimistic pursuit turn whose provider start never came,
    /// returning the session to rest. Confirmed turns and submissions are
    /// never touched: only a running provider turn without a user message
    /// and without a provider start report qualifies.
    pub(super) fn unwind_unconfirmed_pursuit_turn(&mut self, session_id: Uuid) {
        let Some(session) = self
            .state
            .session_mut(session_id)
            .filter(|session| session.active_turn_is_unconfirmed_pursuit())
        else {
            return;
        };
        if let Some(turn_id) = session.active_turn_id() {
            session.unwind_unstarted_turn(turn_id);
        }
        if session.status.is_busy() {
            session.status = SessionStatus::Idle;
        }
        self.state.mark_session_dirty(session_id);
    }

    /// Flush operations accepted before the runtime existed. Called after
    /// any runtime install so the goal lands on the thread that was started
    /// for it — whether the goal path or a racing submission started it.
    pub(super) fn drain_pending_goal_operations(&mut self, session_id: Uuid) {
        let Some(operations) = self.pending_goal_operations.remove(&session_id) else {
            return;
        };
        if let Some(runtime) = self.runtimes.get(&session_id) {
            for operation in operations {
                runtime.driver.goal(operation);
            }
        }
    }

    fn confirm_goal_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.goal_dialog.as_ref() else {
            return;
        };
        let session_id = dialog.session_id;
        let replace = dialog.replace;
        let objective = dialog.objective.read(cx).content().trim().to_owned();
        if objective.is_empty() {
            return;
        }
        let current_status = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.thread_goal.as_ref())
            .map(|goal| goal.status);
        let (status, replace) = match current_status {
            // Editing keeps a resumable status; finished goals restart.
            Some(status) if !replace => (edited_goal_status(status), false),
            // Replacing always pursues the new objective from scratch. A goal
            // that vanished since the dialog opened degrades to a plain set.
            Some(_) => (ThreadGoalStatus::Active, true),
            None => (ThreadGoalStatus::Active, false),
        };
        self.dispatch_goal_operation(
            session_id,
            GoalOperation::Set {
                objective: Some(objective),
                status: Some(status),
                replace,
            },
            cx,
        );
        self.close_goal_dialog(window, cx);
    }

    fn goal_dialog_set_status(
        &mut self,
        status: ThreadGoalStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.goal_dialog.as_ref().map(|dialog| dialog.session_id) else {
            return;
        };
        self.dispatch_goal_operation(
            session_id,
            GoalOperation::Set {
                objective: None,
                status: Some(status),
                replace: false,
            },
            cx,
        );
        self.close_goal_dialog(window, cx);
    }

    fn goal_dialog_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.goal_dialog.as_ref().map(|dialog| dialog.session_id) else {
            return;
        };
        self.dispatch_goal_operation(session_id, GoalOperation::Clear, cx);
        self.close_goal_dialog(window, cx);
    }

    pub(super) fn render_goal_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(request) = self.goal_dialog_request.take() {
            self.materialize_goal_dialog(request, window, cx);
        }
        let dialog = self.goal_dialog.as_ref()?;
        let theme = Theme::current(cx);
        let current = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == dialog.session_id)
            .and_then(|session| session.thread_goal.as_ref());
        let replace = dialog.replace;
        let can_save = !dialog.objective.read(cx).content().trim().is_empty();
        let save_label = match (&current, replace) {
            (Some(_), true) => tr!("goal.replace"),
            (Some(_), false) => tr!("goal.save"),
            (None, _) => tr!("goal.set"),
        };
        let status_action = current
            .filter(|_| !replace)
            .and_then(|goal| match goal.status {
                ThreadGoalStatus::Active => Some((
                    ThreadGoalStatus::Paused,
                    tr!("goal.pause"),
                    "icons/stop.svg",
                )),
                ThreadGoalStatus::Paused
                | ThreadGoalStatus::Blocked
                | ThreadGoalStatus::UsageLimited => Some((
                    ThreadGoalStatus::Active,
                    tr!("goal.resume"),
                    "icons/arrow-up.svg",
                )),
                ThreadGoalStatus::BudgetLimited | ThreadGoalStatus::Complete => None,
            });
        let status_line = current.map(|goal| {
            (
                goal_status_label(goal.status),
                goal_status_color(goal.status, &theme),
                goal_usage_summary(goal),
            )
        });
        let weak = cx.entity().downgrade();

        let mut card = div()
            .id("goal-dialog-card")
            .key_context(DIALOG_CONTEXT)
            .on_action(cx.listener(|waku, _: &ConfirmGoalDialog, window, cx| {
                waku.confirm_goal_dialog(window, cx);
            }))
            .on_action(cx.listener(|waku, _: &DismissGoalDialog, window, cx| {
                waku.close_goal_dialog(window, cx);
            }))
            .tab_group()
            .tab_stop(false)
            .w_full()
            .max_w(px(420.0))
            .overflow_hidden()
            .rounded(px(18.0))
            .bg(theme.composer)
            .shadow_xl()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(48.0))
                    .px(px(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .text_size(sp(14.0))
                    .text_color(theme.text)
                    .child(icon("icons/target.svg", 15.0, theme.text))
                    .child(div().child(tr!("goal.title")))
                    .when_some(status_line, |header, (label, color, usage)| {
                        header
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(7.0))
                                    .py(px(2.0))
                                    .rounded(px(9.0))
                                    .bg(theme.overlay)
                                    .text_size(sp(12.0))
                                    .text_color(color)
                                    .child(label),
                            )
                            .when_some(usage, |header, usage| {
                                header.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(sp(12.0))
                                        .text_color(theme.text_secondary)
                                        .child(usage),
                                )
                            })
                    }),
            )
            .child(
                div()
                    .h(px(112.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .text_size(sp(14.0))
                    .line_height(sp(21.0))
                    .text_color(theme.text)
                    .child(dialog.objective.clone()),
            );
        if replace && current.is_some() {
            card = card.child(
                div()
                    .px(px(20.0))
                    .pb(px(10.0))
                    .text_size(sp(12.5))
                    .line_height(sp(16.0))
                    .text_color(theme.warning)
                    .child(tr!("goal.replace_notice")),
            );
        }
        let save = render_goal_action_row(
            "goal-dialog-save",
            &dialog.save_focus,
            "icons/check.svg",
            save_label,
            can_save,
            Some(crate::platform::primary_shortcut("⌘↩", "Ctrl+Enter")),
            theme.text,
            weak.clone(),
            &theme,
            |waku, window, cx| waku.confirm_goal_dialog(window, cx),
        );
        let mut actions_column = div().p(px(8.0)).flex().flex_col().gap(px(2.0)).child(save);
        if let Some((status, label, icon_path)) = status_action {
            let toggle_weak = cx.entity().downgrade();
            actions_column = actions_column.child(render_goal_action_row(
                "goal-dialog-status",
                &dialog.status_focus,
                icon_path,
                label,
                true,
                None,
                theme.text,
                toggle_weak,
                &theme,
                move |waku, window, cx| waku.goal_dialog_set_status(status, window, cx),
            ));
        }
        if current.is_some() && !replace {
            let clear_weak = cx.entity().downgrade();
            actions_column = actions_column.child(render_goal_action_row(
                "goal-dialog-clear",
                &dialog.clear_focus,
                "icons/trash.svg",
                tr!("goal.clear"),
                true,
                None,
                theme.danger,
                clear_weak,
                &theme,
                |waku, window, cx| waku.goal_dialog_clear(window, cx),
            ));
        }
        let card = card
            .child(div().mx(px(8.0)).h(px(1.0)).bg(theme.border))
            .child(actions_column);

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("goal-dialog-layer")
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
                cx.listener(|waku, _, window, cx| waku.close_goal_dialog(window, cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }
}

#[allow(clippy::too_many_arguments)]
fn render_goal_action_row(
    id: &'static str,
    focus: &FocusHandle,
    icon_path: &'static str,
    label: String,
    enabled: bool,
    shortcut: Option<&'static str>,
    tint: gpui::Hsla,
    weak: WeakEntity<Waku>,
    theme: &Theme,
    activate: impl Fn(&mut Waku, &mut Window, &mut Context<Waku>) + Clone + 'static,
) -> Stateful<Div> {
    let foreground = if enabled { tint } else { theme.text_ghost };
    let click_activate = activate.clone();
    let click_weak = weak.clone();
    let key_weak = weak;
    div()
        .id(id)
        .track_focus(focus)
        .when(enabled, |row| row.tab_index(0))
        .h(px(38.0))
        .w_full()
        .px(px(10.0))
        .rounded(px(9.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .cursor_default()
        .text_size(sp(14.0))
        .text_color(foreground)
        .focus_visible(|style| style.border_1().border_color(theme.accent))
        .when(enabled, |row| {
            row.hover(|style| style.bg(theme.overlay_strong))
        })
        .child(icon(icon_path, 15.0, foreground))
        .child(div().min_w_0().flex_1().truncate().child(label))
        .when_some(shortcut, |row, shortcut| {
            row.child(
                div()
                    .h(px(22.0))
                    .min_w(px(34.0))
                    .px(px(7.0))
                    .rounded(px(11.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.overlay_strong)
                    .text_size(sp(12.5))
                    .text_color(if enabled {
                        theme.text_secondary
                    } else {
                        theme.text_ghost
                    })
                    .child(shortcut),
            )
        })
        .when(enabled, |row| {
            row.on_click(move |_, window, cx| {
                let _ = click_weak.update(cx, |waku, cx| click_activate(waku, window, cx));
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    let _ = key_weak.update(cx, |waku, cx| activate(waku, window, cx));
                    cx.stop_propagation();
                }
            })
        })
}

/// The status vocabulary Codex's own UI uses, translated.
pub(super) fn goal_status_label(status: ThreadGoalStatus) -> String {
    match status {
        ThreadGoalStatus::Active => tr!("goal.status_active"),
        ThreadGoalStatus::Paused => tr!("goal.status_paused"),
        ThreadGoalStatus::Blocked => tr!("goal.status_stalled"),
        ThreadGoalStatus::UsageLimited => tr!("goal.status_usage_limited"),
        ThreadGoalStatus::BudgetLimited => tr!("goal.status_budget_limited"),
        ThreadGoalStatus::Complete => tr!("goal.status_complete"),
    }
}

/// Status tint, always paired with the label text — never color alone.
pub(super) fn goal_status_color(status: ThreadGoalStatus, theme: &Theme) -> gpui::Hsla {
    match status {
        ThreadGoalStatus::Active => theme.accent,
        ThreadGoalStatus::Paused => theme.text_secondary,
        ThreadGoalStatus::Blocked
        | ThreadGoalStatus::UsageLimited
        | ThreadGoalStatus::BudgetLimited => theme.warning,
        ThreadGoalStatus::Complete => theme.success,
    }
}

/// The composer chip's text: the status phrase plus consumption — token
/// budget when one bounds the goal, elapsed pursuit time otherwise, the
/// Codex CLI's own treatment. `live_elapsed_seconds` extends an active
/// goal's recorded time with the current turn's wall clock.
pub(super) fn goal_chip_label(goal: &ThreadGoal, live_elapsed_seconds: i64) -> String {
    let phrase = match goal.status {
        ThreadGoalStatus::Active => tr!("goal.chip_active"),
        ThreadGoalStatus::Paused => tr!("goal.chip_paused"),
        ThreadGoalStatus::Blocked => tr!("goal.chip_stalled"),
        ThreadGoalStatus::UsageLimited => tr!("goal.chip_usage_limited"),
        ThreadGoalStatus::BudgetLimited => tr!("goal.chip_budget_limited"),
        ThreadGoalStatus::Complete => tr!("goal.chip_complete"),
    };
    let usage = match (goal.status, goal.token_budget) {
        (
            ThreadGoalStatus::Active | ThreadGoalStatus::Complete | ThreadGoalStatus::BudgetLimited,
            Some(budget),
        ) => Some(format!(
            "{} / {}",
            format_tokens(goal.tokens_used.max(0) as u64),
            format_tokens(budget.max(0) as u64)
        )),
        (ThreadGoalStatus::Active, None) => {
            let seconds = goal.time_used_seconds.saturating_add(live_elapsed_seconds);
            (seconds > 0).then(|| format_goal_elapsed(seconds))
        }
        (ThreadGoalStatus::Complete, None) => {
            (goal.time_used_seconds > 0).then(|| format_goal_elapsed(goal.time_used_seconds))
        }
        _ => None,
    };
    match usage {
        Some(usage) => format!("{phrase} ({usage})"),
        None => phrase,
    }
}

/// One line of accounting for the dialog header: elapsed pursuit time and
/// token consumption, whichever the goal has recorded.
pub(super) fn goal_usage_summary(goal: &ThreadGoal) -> Option<String> {
    let mut parts = Vec::new();
    if goal.time_used_seconds > 0 {
        parts.push(format_goal_elapsed(goal.time_used_seconds));
    }
    match goal.token_budget {
        Some(budget) => parts.push(tr!(
            "goal.usage_tokens",
            used = format_tokens(goal.tokens_used.max(0) as u64),
            budget = format_tokens(budget.max(0) as u64)
        )),
        None if goal.tokens_used > 0 => parts.push(tr!(
            "goal.usage_tokens_unbudgeted",
            used = format_tokens(goal.tokens_used.max(0) as u64)
        )),
        None => {}
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Compact elapsed time, matching Codex's own goal display: `45s`, `12m`,
/// `1h 30m`, `2d 3h 15m`.
fn format_goal_elapsed(seconds: i64) -> String {
    let seconds = seconds.max(0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if hours >= 24 {
        let days = hours / 24;
        return format!("{days}d {}h {remaining_minutes}m", hours % 24);
    }
    if remaining_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remaining_minutes}m")
    }
}

/// The objective as a transcript notice: whole when short, elided past 120
/// characters — the chip tooltip and dialog carry the full text.
fn notice_objective(objective: &str) -> String {
    const NOTICE_OBJECTIVE_CHARS: usize = 120;
    if objective.chars().count() <= NOTICE_OBJECTIVE_CHARS {
        return objective.to_owned();
    }
    let clipped: String = objective.chars().take(NOTICE_OBJECTIVE_CHARS - 1).collect();
    format!("{}…", clipped.trim_end())
}

/// Saving an objective keeps a resumable status but restarts a finished one,
/// mirroring Codex's `/goal edit` semantics.
fn edited_goal_status(status: ThreadGoalStatus) -> ThreadGoalStatus {
    if status.is_terminal() {
        ThreadGoalStatus::Active
    } else {
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_time_is_compact() {
        assert_eq!(format_goal_elapsed(0), "0s");
        assert_eq!(format_goal_elapsed(59), "59s");
        assert_eq!(format_goal_elapsed(90 * 60), "1h 30m");
        assert_eq!(format_goal_elapsed(2 * 60 * 60), "2h");
        assert_eq!(format_goal_elapsed(26 * 60 * 60 + 5 * 60), "1d 2h 5m");
        assert_eq!(format_goal_elapsed(-5), "0s");
    }

    #[test]
    fn editing_restarts_only_finished_goals() {
        assert_eq!(
            edited_goal_status(ThreadGoalStatus::Paused),
            ThreadGoalStatus::Paused
        );
        assert_eq!(
            edited_goal_status(ThreadGoalStatus::UsageLimited),
            ThreadGoalStatus::UsageLimited
        );
        assert_eq!(
            edited_goal_status(ThreadGoalStatus::Complete),
            ThreadGoalStatus::Active
        );
        assert_eq!(
            edited_goal_status(ThreadGoalStatus::BudgetLimited),
            ThreadGoalStatus::Active
        );
    }

    #[test]
    fn chip_label_reports_budget_consumption() {
        let goal = ThreadGoal {
            objective: "Ship it".into(),
            status: ThreadGoalStatus::Active,
            token_budget: Some(50_000),
            tokens_used: 12_500,
            time_used_seconds: 90,
        };
        assert_eq!(goal_chip_label(&goal, 0), "Pursuing goal (12.5k / 50.0k)");
    }

    #[test]
    fn unbudgeted_pursuit_reports_live_elapsed_time() {
        let mut goal = ThreadGoal {
            objective: "Ship it".into(),
            status: ThreadGoalStatus::Active,
            token_budget: None,
            tokens_used: 12_500,
            time_used_seconds: 16_500,
        };
        // 4h 35m recorded + 60s of the current turn — Codex CLI's readout.
        assert_eq!(goal_chip_label(&goal, 60), "Pursuing goal (4h 36m)");

        goal.status = ThreadGoalStatus::Complete;
        assert_eq!(goal_chip_label(&goal, 0), "Goal achieved (4h 35m)");

        goal.status = ThreadGoalStatus::Paused;
        assert_eq!(goal_chip_label(&goal, 0), "Goal paused");

        // A brand-new goal has nothing to report yet.
        goal.status = ThreadGoalStatus::Active;
        goal.time_used_seconds = 0;
        assert_eq!(goal_chip_label(&goal, 0), "Pursuing goal");
    }
}

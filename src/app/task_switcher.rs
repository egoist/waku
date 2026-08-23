//! Ctrl-Tab switching across Waku tasks.
//!
//! The task order is snapshotted when Control-Tab opens the overlay. Repeated
//! presses move only the highlight; releasing Control commits once, so a
//! switch never hydrates intermediate transcripts or reshuffles the cards
//! underneath the pointer. The snapshot is capped at ten visited tasks and
//! laid out in no more than two rows.

use super::*;

const CARD_WIDTH: f32 = 194.0;
const CARD_HEIGHT: f32 = 169.0;
const CARD_INSET: f32 = 9.0;
const CARD_BOTTOM_INSET: f32 = 14.0;
const PREVIEW_WIDTH: f32 = 176.0;
const PREVIEW_HEIGHT: f32 = 119.0;
const PREVIEW_TITLE_GAP: f32 = 9.0;
const GRID_INSET: f32 = 8.0;
const CONTAINER_RADIUS: f32 = 26.0;
const MAX_COLUMNS: usize = 5;
const MAX_ROWS: usize = 2;
const MAX_TASKS: usize = MAX_COLUMNS * MAX_ROWS;
const WINDOW_MARGIN: f32 = 44.0;

/// Runtime-only switcher state. Restoration deliberately seeds only the
/// selected task: opening old windows must not masquerade as user recency.
pub(super) struct TaskSwitcherUi {
    open: bool,
    ordered_session_ids: Vec<Uuid>,
    highlighted_session_id: Option<Uuid>,
    original_session_id: Option<Uuid>,
    recent_session_ids: Vec<Uuid>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    list: ListState,
    columns: usize,
    generation: u64,
}

impl TaskSwitcherUi {
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            open: false,
            ordered_session_ids: Vec::new(),
            highlighted_session_id: None,
            original_session_id: None,
            recent_session_ids: Vec::new(),
            focus,
            previous_focus: None,
            list: ListState::new(0, ListAlignment::Top, px(CARD_HEIGHT * 2.0))
                .with_uniform_item_height(px(CARD_HEIGHT)),
            columns: 1,
            generation: 0,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn record_access(&mut self, session_id: Uuid) {
        self.recent_session_ids
            .retain(|recent| *recent != session_id);
        self.recent_session_ids.insert(0, session_id);
    }

    pub(super) fn remove(&mut self, session_id: Uuid) {
        self.recent_session_ids
            .retain(|recent| *recent != session_id);
        let highlighted_index = self
            .ordered_session_ids
            .iter()
            .position(|candidate| *candidate == session_id);
        self.ordered_session_ids
            .retain(|candidate| *candidate != session_id);
        if self.highlighted_session_id == Some(session_id) {
            self.highlighted_session_id = highlighted_index.and_then(|index| {
                self.ordered_session_ids
                    .get(index.min(self.ordered_session_ids.len().saturating_sub(1)))
                    .copied()
            });
        }
        self.reset_list();
    }

    fn reset_list(&self) {
        let rows = self.ordered_session_ids.len().div_ceil(self.columns.max(1));
        self.list.reset_with_uniform_height(rows, px(CARD_HEIGHT));
    }

    fn reveal_highlight(&self) {
        let Some(index) = self.highlighted_session_id.and_then(|highlighted| {
            self.ordered_session_ids
                .iter()
                .position(|candidate| *candidate == highlighted)
        }) else {
            return;
        };
        self.list.scroll_to_reveal_item(index / self.columns.max(1));
    }

    fn configure_columns(&mut self, viewport_width: f32) {
        let columns = task_switcher_column_count(self.ordered_session_ids.len(), viewport_width);
        if columns != self.columns {
            self.columns = columns;
            self.reset_list();
            self.reveal_highlight();
        }
    }

    fn dismiss(&mut self) -> Option<FocusHandle> {
        self.open = false;
        self.ordered_session_ids.clear();
        self.highlighted_session_id = None;
        self.original_session_id = None;
        self.columns = 1;
        self.list.reset(0);
        self.generation = self.generation.wrapping_add(1);
        self.previous_focus.take()
    }
}

fn ordered_task_ids(current: Option<Uuid>, recent: &[Uuid], started_tasks: &[Uuid]) -> Vec<Uuid> {
    let valid = started_tasks.iter().copied().collect::<HashSet<_>>();
    let mut seen = HashSet::with_capacity(started_tasks.len());
    let mut ordered = Vec::with_capacity(started_tasks.len().min(MAX_TASKS));
    let mut push = |id| {
        if ordered.len() < MAX_TASKS && valid.contains(&id) && seen.insert(id) {
            ordered.push(id);
        }
    };

    if let Some(current) = current {
        push(current);
    }
    for recent in recent {
        push(*recent);
    }
    ordered
}

fn initial_highlight_index(
    ordered: &[Uuid],
    current: Option<Uuid>,
    reverse: bool,
) -> Option<usize> {
    if ordered.is_empty() {
        return None;
    }
    if ordered.first().copied() == current {
        if ordered.len() == 1 {
            return Some(0);
        }
        return Some(if reverse { ordered.len() - 1 } else { 1 });
    }
    Some(if reverse { ordered.len() - 1 } else { 0 })
}

fn task_switcher_column_count(count: usize, viewport_width: f32) -> usize {
    let count = count.min(MAX_TASKS);
    let usable = (viewport_width - WINDOW_MARGIN - GRID_INSET * 2.0).max(CARD_WIDTH);
    let fitting = (usable / CARD_WIDTH).floor() as usize;
    let minimum_for_two_rows = count.div_ceil(MAX_ROWS);
    count
        .min(fitting.max(minimum_for_two_rows).max(1))
        .clamp(1, MAX_COLUMNS)
}

fn task_switcher_grid_height(count: usize, columns: usize, viewport_height: f32) -> f32 {
    let rows = count.div_ceil(columns.max(1));
    let content_height = rows as f32 * CARD_HEIGHT + GRID_INSET * 2.0;
    content_height.min((viewport_height - WINDOW_MARGIN).max(CARD_HEIGHT + GRID_INSET * 2.0))
}

fn task_switcher_title(session: &AgentSession) -> String {
    let title = session.display_title();
    if title == AgentSession::DEFAULT_TITLE {
        tr!("session.new_task")
    } else {
        title.to_owned()
    }
}

fn task_switcher_branch(workspace: &SessionWorkspace) -> Option<&str> {
    match workspace {
        SessionWorkspace::Local => None,
        SessionWorkspace::NewWorktree { base_branch } => base_branch.as_deref(),
        SessionWorkspace::Worktree { branch, .. } => Some(branch.as_str()),
    }
    .filter(|branch| !branch.is_empty())
}

fn task_switcher_status_icon(status: SessionStatus) -> Option<&'static str> {
    match status {
        SessionStatus::Idle => None,
        SessionStatus::Connecting | SessionStatus::Working => Some("icons/loader-circle.svg"),
        SessionStatus::Waiting => Some("icons/alert.svg"),
        SessionStatus::Failed => Some("icons/x.svg"),
    }
}

impl Waku {
    pub(super) fn switch_task_forward_action(
        &mut self,
        _: &SwitchTaskForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_task_switcher(false, window, cx);
    }

    pub(super) fn switch_task_backward_action(
        &mut self,
        _: &SwitchTaskBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_task_switcher(true, window, cx);
    }

    pub(super) fn select_first_task_action(
        &mut self,
        _: &SelectFirstTask,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_task_switcher_highlight(0, cx);
    }

    pub(super) fn select_last_task_action(
        &mut self,
        _: &SelectLastTask,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = self
            .task_switcher
            .ordered_session_ids
            .len()
            .saturating_sub(1);
        self.set_task_switcher_highlight(last, cx);
    }

    pub(super) fn confirm_task_switch_action(
        &mut self,
        _: &ConfirmTaskSwitch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_task_switcher(false, window, cx);
    }

    pub(super) fn cancel_task_switch_action(
        &mut self,
        _: &CancelTaskSwitch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_task_switcher(window, cx);
    }

    pub(super) fn task_switcher_modifiers_changed(
        &mut self,
        event: &gpui::ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task_switcher.open && !event.control {
            self.finish_task_switcher(false, window, cx);
        }
    }

    fn cycle_task_switcher(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.task_switcher.open {
            self.open_task_switcher(reverse, window, cx);
            return;
        }

        let Some(current_index) = self
            .task_switcher
            .highlighted_session_id
            .and_then(|current| {
                self.task_switcher
                    .ordered_session_ids
                    .iter()
                    .position(|candidate| *candidate == current)
            })
        else {
            self.cancel_task_switcher(window, cx);
            return;
        };
        let len = self.task_switcher.ordered_session_ids.len();
        if len == 0 {
            self.cancel_task_switcher(window, cx);
            return;
        }
        let next = if reverse {
            (current_index + len - 1) % len
        } else {
            (current_index + 1) % len
        };
        self.set_task_switcher_highlight(next, cx);
    }

    fn open_task_switcher(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let started_tasks = self
            .state
            .sessions
            .iter()
            .filter(|session| session.has_started())
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let ordered = ordered_task_ids(
            self.state.selected_session,
            &self.task_switcher.recent_session_ids,
            &started_tasks,
        );
        let Some(highlighted_index) =
            initial_highlight_index(&ordered, self.state.selected_session, reverse)
        else {
            return;
        };

        if self.command_palette.is_open() {
            self.toggle_command_palette_action(&ToggleCommandPalette, window, cx);
        }
        let open_menus = self
            .menus
            .borrow()
            .values()
            .filter(|menu| menu.is_open())
            .cloned()
            .collect::<Vec<_>>();
        self.task_switcher.previous_focus = if open_menus.is_empty() {
            window.focused(cx)
        } else if self.settings_page.is_some() {
            Some(self.settings_focus.clone())
        } else {
            Some(self.composer_focus(cx))
        };

        self.task_switcher.open = true;
        self.task_switcher.ordered_session_ids = ordered;
        self.task_switcher.highlighted_session_id = self
            .task_switcher
            .ordered_session_ids
            .get(highlighted_index)
            .copied();
        self.task_switcher.original_session_id = self.state.selected_session;
        self.task_switcher.columns = task_switcher_column_count(
            self.task_switcher.ordered_session_ids.len(),
            f32::from(window.viewport_size().width),
        );
        self.task_switcher.reset_list();
        self.task_switcher.reveal_highlight();
        self.task_switcher.generation = self.task_switcher.generation.wrapping_add(1);
        let generation = self.task_switcher.generation;
        let focus = self.task_switcher.focus.clone();
        let weak = cx.entity().downgrade();

        if !open_menus.is_empty() {
            window.defer(cx, move |window, cx| {
                for menu in open_menus {
                    menu.close(window, cx);
                }
            });
        }

        // Deferred overlays join the dispatch tree after their deferred paint.
        // Two frames guarantees the switcher focus can resolve, while the root
        // modifier listener still catches a very quick Control release.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let should_focus = weak
                    .update(cx, |this, _| {
                        this.task_switcher.open && this.task_switcher.generation == generation
                    })
                    .unwrap_or(false);
                if should_focus {
                    window.focus(&focus, cx);
                }
            });
        });
        cx.notify();
    }

    fn set_task_switcher_highlight(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(session_id) = self.task_switcher.ordered_session_ids.get(index).copied() else {
            return;
        };
        if self.task_switcher.highlighted_session_id == Some(session_id) {
            return;
        }
        self.task_switcher.highlighted_session_id = Some(session_id);
        self.task_switcher.reveal_highlight();
        cx.notify();
    }

    fn finish_task_switcher(
        &mut self,
        pointer_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.task_switcher.open {
            return;
        }
        let selected = self.task_switcher.highlighted_session_id;
        let original = self.task_switcher.original_session_id;
        let previous_focus = self.task_switcher.dismiss();
        let may_commit = pointer_selection || self.state.selected_session == original;
        let mut focus_after = previous_focus;
        if may_commit
            && let Some(selected) = selected
            && self
                .state
                .sessions
                .iter()
                .any(|session| session.id == selected && session.has_started())
        {
            let was_in_settings = self.settings_page.is_some();
            self.settings_page = None;
            self.select_session(selected, cx);
            if was_in_settings {
                focus_after = Some(self.composer_focus(cx));
            }
        }
        if let Some(previous_focus) = focus_after {
            window.focus(&previous_focus, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_task_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.task_switcher.open {
            return;
        }
        if let Some(previous_focus) = self.task_switcher.dismiss() {
            window.focus(&previous_focus, cx);
        }
        cx.notify();
    }

    fn render_task_switcher_row(
        &self,
        row_index: usize,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let start = row_index.saturating_mul(columns);
        let end = (start + columns).min(self.task_switcher.ordered_session_ids.len());
        div()
            .h(px(CARD_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .children(
                self.task_switcher.ordered_session_ids[start..end]
                    .iter()
                    .filter_map(|session_id| {
                        self.state
                            .sessions
                            .iter()
                            .find(|session| session.id == *session_id)
                    })
                    .map(|session| self.render_task_switcher_card(session, cx)),
            )
            .into_any_element()
    }

    fn render_task_switcher_card(
        &self,
        session: &AgentSession,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let session_id = session.id;
        let highlighted = self.task_switcher.highlighted_session_id == Some(session_id);
        let project_name = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .map(Project::display_name)
            .unwrap_or_else(|| tr!("sidebar.unknown_project"));
        let branch = task_switcher_branch(&session.workspace).map(SharedString::from);
        let timestamp = session.last_reply_at.unwrap_or(session.created_at);
        let time = super::sidebar::format_time_ago(unix_time().saturating_sub(timestamp));
        let status_icon = task_switcher_status_icon(session.status);
        let provider = session.provider;
        let model = session
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| provider.display_name());

        let preview = div()
            .w(px(PREVIEW_WIDTH))
            .h(px(PREVIEW_HEIGHT))
            .flex_none()
            .p(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.inset)
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon(
                        provider_icon(provider),
                        14.0,
                        provider_color(&theme, provider),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(sp(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child(provider.display_name()),
                    )
                    .when_some(status_icon, |row, icon_path| {
                        row.child(icon(icon_path, 12.0, status_color(&theme, session.status)))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .child(icon("icons/folder.svg", 16.0, theme.text_tertiary))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(sp(14.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(project_name),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(sp(12.5))
                            .text_color(theme.text_tertiary)
                            .child(model.to_owned()),
                    ),
            )
            .child(
                div()
                    .h(px(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .when_some(branch.clone(), |row, branch| {
                        row.child(icon("icons/git-branch.svg", 11.5, theme.text_tertiary))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(sp(12.5))
                                    .text_color(theme.text_tertiary)
                                    .child(branch),
                            )
                    })
                    .when(branch.is_none(), |row| row.child(div().flex_1()))
                    .child(
                        div()
                            .flex_none()
                            .text_size(sp(12.5))
                            .text_color(theme.text_ghost)
                            .child(time),
                    ),
            );

        div()
            .id(SharedString::from(format!(
                "task-switcher-card-{session_id}"
            )))
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .flex_none()
            .px(px(CARD_INSET))
            .pt(px(CARD_INSET))
            .pb(px(CARD_BOTTOM_INSET))
            .rounded(px(16.0))
            .flex()
            .flex_col()
            .gap(px(PREVIEW_TITLE_GAP))
            .cursor_default()
            .when(highlighted, |card| card.bg(theme.overlay_strong))
            .hover(|card| card.bg(theme.overlay))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                let Some(index) = this
                    .task_switcher
                    .ordered_session_ids
                    .iter()
                    .position(|candidate| *candidate == session_id)
                else {
                    return;
                };
                this.set_task_switcher_highlight(index, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.task_switcher.open {
                    this.task_switcher.highlighted_session_id = Some(session_id);
                    this.finish_task_switcher(true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(preview)
            .child(
                div()
                    .h(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(
                        provider_icon(provider),
                        16.0,
                        if highlighted {
                            provider_color(&theme, provider)
                        } else {
                            theme.text_secondary
                        },
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(sp(14.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if highlighted {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(task_switcher_title(session)),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_task_switcher(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.task_switcher.open {
            return None;
        }
        self.task_switcher
            .configure_columns(f32::from(window.viewport_size().width));
        let count = self.task_switcher.ordered_session_ids.len();
        let columns = self.task_switcher.columns;
        let rows = count.div_ceil(columns.max(1));
        if rows == 0 {
            return None;
        }
        let container_width = columns as f32 * CARD_WIDTH + GRID_INSET * 2.0;
        let container_height =
            task_switcher_grid_height(count, columns, f32::from(window.viewport_size().height));
        let list_state = self.task_switcher.list.clone();
        let focus = self.task_switcher.focus.clone();
        let entity = cx.entity().downgrade();
        let task_list = list(list_state, move |row_index, _window, cx| {
            entity
                .upgrade()
                .map(|entity| {
                    entity.update(cx, |this, cx| {
                        this.render_task_switcher_row(row_index, columns, cx)
                    })
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .size_full();

        let theme = Theme::current(cx);
        let card = div()
            .id("task-switcher")
            .key_context("TaskSwitcher")
            .track_focus(&focus)
            .w(px(container_width))
            .h(px(container_height))
            .p(px(GRID_INSET))
            .overflow_hidden()
            .rounded(px(CONTAINER_RADIUS))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.raised)
            .shadow_xl()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(task_list);
        let layer = div()
            .id("task-switcher-layer")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.cancel_task_switcher(window, cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(6).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switcher_order_contains_only_the_ten_most_recently_visited_tasks() {
        let current = Uuid::new_v4();
        let recent = (0..12).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        let unvisited = Uuid::new_v4();
        let missing = Uuid::new_v4();
        let mut started_tasks = vec![unvisited, current];
        started_tasks.extend(recent.iter().copied());
        let mut recorded_recency = vec![missing];
        recorded_recency.extend(recent.iter().copied());
        let mut expected = vec![current];
        expected.extend(recent.iter().take(MAX_TASKS - 1).copied());

        assert_eq!(
            ordered_task_ids(Some(current), &recorded_recency, &started_tasks),
            expected
        );
    }

    #[test]
    fn first_control_tab_targets_previous_task_and_reverse_wraps() {
        let current = Uuid::new_v4();
        let previous = Uuid::new_v4();
        let oldest = Uuid::new_v4();
        let ordered = [current, previous, oldest];

        assert_eq!(
            initial_highlight_index(&ordered, Some(current), false),
            Some(1)
        );
        assert_eq!(
            initial_highlight_index(&ordered, Some(current), true),
            Some(2)
        );
    }

    #[test]
    fn single_current_task_still_opens_the_switcher() {
        let current = Uuid::new_v4();
        assert_eq!(
            initial_highlight_index(&[current], Some(current), false),
            Some(0)
        );
    }

    #[test]
    fn draft_can_switch_to_the_only_visited_started_task() {
        let draft = Uuid::new_v4();
        let started = Uuid::new_v4();
        let ordered = ordered_task_ids(Some(draft), &[draft, started], &[started]);
        assert_eq!(ordered, vec![started]);
        assert_eq!(
            initial_highlight_index(&ordered, Some(draft), false),
            Some(0)
        );
    }

    #[test]
    fn grid_caps_at_ten_tasks_and_two_rows() {
        assert_eq!(task_switcher_column_count(20, 1400.0), 5);
        assert_eq!(task_switcher_column_count(10, 520.0), 5);
        assert_eq!(task_switcher_column_count(4, 520.0), 2);
        assert_eq!(task_switcher_column_count(4, 180.0), 2);
    }
}

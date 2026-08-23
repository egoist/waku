use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};
use gpui::{KeyBinding, actions};

use super::*;

actions!(waku_sidebar, [CancelSessionRename]);

const SESSION_RENAME_PARENT_CONTEXT: &str = "SessionRename";
const SESSION_RENAME_FIELD_CONTEXT: &str = "SessionRename > TextInput";

/// Keep Escape inside the focused inline editor so it cancels the rename,
/// rather than falling through to the window-wide Stop action.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        CancelSessionRename,
        Some(SESSION_RENAME_FIELD_CONTEXT),
    )]);
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SessionDateGroup {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    ThisYear,
    More,
}

impl SessionDateGroup {
    const ALL: [Self; 6] = [
        Self::Today,
        Self::Yesterday,
        Self::ThisWeek,
        Self::ThisMonth,
        Self::ThisYear,
        Self::More,
    ];

    fn index(self) -> usize {
        match self {
            Self::Today => 0,
            Self::Yesterday => 1,
            Self::ThisWeek => 2,
            Self::ThisMonth => 3,
            Self::ThisYear => 4,
            Self::More => 5,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Today => tr!("sidebar.today"),
            Self::Yesterday => tr!("sidebar.yesterday"),
            Self::ThisWeek => tr!("sidebar.this_week"),
            Self::ThisMonth => tr!("sidebar.this_month"),
            Self::ThisYear => tr!("sidebar.this_year"),
            Self::More => tr!("sidebar.more"),
        }
    }
}

/// Stable identity for a collapsible sidebar section. Keeping both variants in
/// one set preserves each view's disclosure state when the user switches
/// between Project and Updated grouping.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SidebarGroup {
    Updated(SessionDateGroup),
    Project(Uuid),
    Projectless,
}

impl SidebarGroup {
    fn element_key(self) -> SharedString {
        match self {
            Self::Updated(group) => format!("updated-{}", group.index()).into(),
            Self::Project(project_id) => format!("project-{project_id}").into(),
            Self::Projectless => "projectless".into(),
        }
    }

    fn mix_fingerprint(self, fingerprint: u64) -> u64 {
        match self {
            Self::Updated(group) => mix(fingerprint, group.index() as u64 + 1),
            Self::Project(project_id) => mix_uuid(mix(fingerprint, 0x100), project_id),
            Self::Projectless => mix(fingerprint, 0x200),
        }
    }
}

fn sidebar_grouping_label(grouping: SidebarGrouping) -> String {
    match grouping {
        SidebarGrouping::Project => tr!("sidebar.grouping_project"),
        SidebarGrouping::Updated => tr!("sidebar.grouping_updated"),
    }
}

fn sidebar_ordering_label(ordering: SidebarOrdering) -> String {
    match ordering {
        SidebarOrdering::Newest => tr!("sidebar.ordering_newest"),
        SidebarOrdering::Oldest => tr!("sidebar.ordering_oldest"),
    }
}

fn session_date_group(timestamp: u64, today: NaiveDate) -> SessionDateGroup {
    let session_date = i64::try_from(timestamp)
        .ok()
        .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
        .map(|timestamp| timestamp.with_timezone(&Local).date_naive())
        .unwrap_or(today);
    session_date_group_for_dates(session_date, today)
}

fn session_date_group_for_dates(session_date: NaiveDate, today: NaiveDate) -> SessionDateGroup {
    if session_date >= today {
        return SessionDateGroup::Today;
    }

    if today.pred_opt() == Some(session_date) {
        return SessionDateGroup::Yesterday;
    }

    let week_start = today
        .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
        .unwrap_or(today);
    if session_date >= week_start {
        return SessionDateGroup::ThisWeek;
    }

    if session_date.year() == today.year() && session_date.month() == today.month() {
        return SessionDateGroup::ThisMonth;
    }

    if session_date.year() == today.year() {
        return SessionDateGroup::ThisYear;
    }

    SessionDateGroup::More
}

fn session_group_header(theme: &Theme) -> Div {
    div()
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .text_size(sp(13.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_secondary)
}

fn append_sidebar_group_rows(
    rows: &mut Vec<SidebarRow>,
    group: SidebarGroup,
    sessions: &[Uuid],
    collapsed: bool,
    show_more: bool,
) {
    if sessions.is_empty() && !show_more {
        return;
    }

    rows.push(SidebarRow::Header(group));
    if !collapsed {
        rows.extend(sessions.iter().copied().map(SidebarRow::Session));
        if show_more {
            rows.push(SidebarRow::ShowMore(group));
        }
    }
    rows.push(SidebarRow::GroupSpacer);
}

fn updater_button_available_content(
    foreground: Hsla,
    label: SharedString,
    label_reveal: f32,
) -> Div {
    div()
        .relative()
        .size_full()
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .opacity(1.0 - label_reveal)
                .child(icon("icons/download.svg", 12.0, foreground)),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .whitespace_nowrap()
                .opacity(label_reveal)
                .child(label),
        )
}

/// Height of a session card plus the separation reserved beneath it in the
/// virtualized sidebar list. Keep the gap inside the list row so measured and
/// estimated heights stay identical for off-screen sessions.
const SIDEBAR_SESSION_CARD_HEIGHT: f32 = 51.0;
const SIDEBAR_SESSION_ROW_GAP: f32 = 1.0;
const SIDEBAR_SESSION_ROW_HEIGHT: f32 = SIDEBAR_SESSION_CARD_HEIGHT + SIDEBAR_SESSION_ROW_GAP;
const SIDEBAR_ACTION_ROW_HEIGHT: f32 = 32.0;
const SIDEBAR_SEARCH_BOTTOM_GAP: f32 = 10.0;
const SIDEBAR_GROUP_GUIDE_X: f32 = 15.0;
const SIDEBAR_GROUP_CHILD_PADDING: f32 = 28.0;
const SIDEBAR_PROJECT_RECENT_WINDOW_SECONDS: u64 = 3 * 24 * 60 * 60;
const SIDEBAR_PROJECT_REVEAL_BATCH: usize = 30;

/// The session row's trailing time: how long the live turn has been working,
/// or how long ago the agent last replied. A session that has never replied
/// shows nothing.
pub(super) fn session_time_label(session: &AgentSession, now: u64) -> Option<String> {
    if session.is_busy()
        && let Some(turn) = session
            .turns
            .last()
            .filter(|turn| turn.status == TurnStatus::Running)
    {
        return Some(tr!(
            "sidebar.working",
            elapsed = format_working_elapsed(now.saturating_sub(turn.started_at))
        ));
    }
    session
        .last_reply_at
        .map(|last_reply_at| format_time_ago(now.saturating_sub(last_reply_at)))
}

/// Recency for sidebar ordering and date groups. A submitted turn promotes the
/// task immediately, while metadata edits such as a rename do not; a task with
/// no turns stays anchored to when it was created.
fn sidebar_session_timestamp(session: &AgentSession) -> u64 {
    session.last_reply_at.unwrap_or(session.created_at)
}

fn sort_sidebar_sessions(sessions: &mut Vec<&AgentSession>, ordering: SidebarOrdering) {
    match ordering {
        SidebarOrdering::Newest => {
            sessions.sort_by_key(|session| std::cmp::Reverse(sidebar_session_timestamp(session)))
        }
        SidebarOrdering::Oldest => {
            sessions.sort_by_key(|session| sidebar_session_timestamp(session))
        }
    }
}

fn project_sidebar_groups(
    sessions: &[&AgentSession],
    projectless_project_ids: &HashSet<Uuid>,
) -> Vec<(SidebarGroup, Vec<Uuid>)> {
    let mut groups: Vec<(SidebarGroup, Vec<Uuid>)> = Vec::new();
    let mut indexes = HashMap::new();
    let mut projectless_sessions = Vec::new();
    for session in sessions {
        if projectless_project_ids.contains(&session.project_id) {
            projectless_sessions.push(session.id);
            continue;
        }
        let index = *indexes.entry(session.project_id).or_insert_with(|| {
            let index = groups.len();
            groups.push((SidebarGroup::Project(session.project_id), Vec::new()));
            index
        });
        groups[index].1.push(session.id);
    }
    if !projectless_sessions.is_empty() {
        groups.push((SidebarGroup::Projectless, projectless_sessions));
    }
    groups
}

fn visible_project_sessions(
    sessions: &[Uuid],
    session_timestamps: &HashMap<Uuid, u64>,
    recent_cutoff: u64,
    revealed_older_sessions: usize,
) -> (Vec<Uuid>, bool) {
    let mut visible = Vec::with_capacity(sessions.len());
    let mut older_seen = 0usize;
    for session_id in sessions {
        let recent = session_timestamps
            .get(session_id)
            .is_some_and(|timestamp| *timestamp >= recent_cutoff);
        if recent || older_seen < revealed_older_sessions {
            visible.push(*session_id);
        }
        if !recent {
            older_seen = older_seen.saturating_add(1);
        }
    }
    (visible, older_seen > revealed_older_sessions)
}

fn sidebar_project_is_projectless(project: &Project, projectless_root: Option<&Path>) -> bool {
    projectless_root.is_some_and(|root| project.path.starts_with(root))
}

fn persisted_sidebar_branch_label(workspace: &SessionWorkspace) -> Option<&str> {
    match workspace {
        SessionWorkspace::Local => None,
        SessionWorkspace::NewWorktree { base_branch } => base_branch.as_deref(),
        SessionWorkspace::Worktree { branch, .. } => Some(branch.as_str()),
    }
    .filter(|branch| !branch.is_empty())
}

/// Compact "how long ago" for the sidebar: "just now", then one coarse unit —
/// "5m", "3h", "420d". Days are the largest unit so a glance still reads as a
/// count rather than a date.
pub(super) fn format_time_ago(seconds: u64) -> String {
    match seconds {
        0..=59 => tr!("sidebar.just_now"),
        60..=3_599 => tr!("sidebar.minutes_ago", count = seconds / 60),
        3_600..=86_399 => tr!("sidebar.hours_ago", count = seconds / 3_600),
        _ => tr!("sidebar.days_ago", count = seconds / 86_400),
    }
}

/// One row of the virtualized sidebar session history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SidebarRow {
    /// Opens the window-wide command palette and scrolls with history.
    Search,
    /// Group header; the first row also carries the sidebar actions.
    Header(SidebarGroup),
    /// A started session.
    Session(Uuid),
    /// Reveals the next batch of older sessions in a project section.
    ShowMore(SidebarGroup),
    /// Spacing between date groups.
    GroupSpacer,
}

impl Waku {
    pub(super) fn window_drag_region(
        &self,
        region: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // Windows drags from the hit test, not from a mouse-move handler:
        // `DefWindowProc` moves the window once the region reports itself as
        // caption, and performs the user's configured double-click action.
        #[cfg(target_os = "windows")]
        let region = region.window_control_area(gpui::WindowControlArea::Drag);

        region
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    crate::platform::titlebar_double_click(window);
                }
            })
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.header_drag_armed = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.header_drag_armed {
                    this.header_drag_armed = false;
                    crate::platform::start_window_move(window);
                }
            }))
    }
    // ── Sidebar ────────────────────────────────────────────────────────────

    fn render_fps_counter(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let fps = self.fps_value;
        let dot = if fps == 0 {
            theme.text_ghost
        } else if fps >= 55 {
            theme.success
        } else if fps >= 30 {
            theme.warning
        } else {
            theme.danger
        };
        div()
            .flex_none()
            .h(px(26.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .text_size(sp(12.5))
            .line_height(sp(0.0))
            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(dot))
            .child(
                div()
                    .text_color(theme.text_tertiary)
                    .font_family(crate::md::render::MONO_FAMILY)
                    .child(SharedString::from(format!("{fps} FPS"))),
            )
    }

    fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("toggle-sidebar")
            .w(px(26.0))
            .h(px(26.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon("icons/panel-left.svg", 14.0, theme.text_tertiary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.set_sidebar_visible(!this.sidebar_visible, cx);
            }))
    }

    pub(super) fn render_history_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        enabled: bool,
        navigate_back: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .w(px(26.0))
            .h(px(26.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .when(!enabled, |element| element.opacity(0.35))
            .when(enabled, |element| {
                element
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if navigate_back {
                            this.navigate_back_action(&NavigateBack, window, cx);
                        } else {
                            this.navigate_forward_action(&NavigateForward, window, cx);
                        }
                    }))
            })
            .child(icon(icon_path, 14.0, theme.text_tertiary))
    }

    fn render_sidebar_titlebar(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-titlebar")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .children(self.render_client_window_controls(
                super::window_chrome::WindowControlSide::Left,
                window,
                cx,
            ))
            .child(
                self.window_drag_region(
                    div()
                        .id("sidebar-traffic-light-drag-region")
                        .w(px(TRAFFIC_LIGHT_CLEARANCE))
                        .h_full()
                        .flex_none(),
                    cx,
                ),
            )
            .child(self.render_sidebar_toggle(cx))
            .child(
                div()
                    .ml(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(self.render_history_button(
                        "navigate-back",
                        "icons/arrow-left.svg",
                        !self.session_navigation.back.is_empty(),
                        true,
                        cx,
                    ))
                    .child(self.render_history_button(
                        "navigate-forward",
                        "icons/arrow-right.svg",
                        !self.session_navigation.forward.is_empty(),
                        false,
                        cx,
                    )),
            )
            .child(self.window_drag_region(
                div().id("sidebar-titlebar-drag-region").h_full().flex_1(),
                cx,
            ))
    }

    fn render_sidebar_header_actions(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let menu = self.menu_handle("sidebar-options", cx);
        let menu_open = menu.is_open();
        let weak = cx.entity().downgrade();
        let grouping = self.state.sidebar_grouping;
        let ordering = self.state.sidebar_ordering;
        let options = dropdown_menu(
            div()
                .id("sidebar-options")
                .w(px(20.0))
                .h(px(20.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .when(menu_open, |element| element.bg(theme.overlay_strong))
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .tooltip(Tooltip::text(tr!("sidebar.options")))
                .child(icon("icons/list-filter.svg", 14.0, theme.text_secondary)),
            "sidebar-options-menu",
            &menu,
            MenuAlign::BelowLeft,
            move |_| {
                let grouping_weak = weak.clone();
                let ordering_weak = weak.clone();
                vec![
                    MenuItem::submenu_with_value(
                        tr!("sidebar.grouping"),
                        sidebar_grouping_label(grouping),
                        move |_| {
                            let project_weak = grouping_weak.clone();
                            let updated_weak = grouping_weak.clone();
                            vec![
                                MenuItem::new(tr!("sidebar.grouping_project"), move |_, cx| {
                                    let _ = project_weak.update(cx, |this, cx| {
                                        this.set_sidebar_grouping(SidebarGrouping::Project, cx);
                                    });
                                })
                                .selected(grouping == SidebarGrouping::Project),
                                MenuItem::new(tr!("sidebar.grouping_updated"), move |_, cx| {
                                    let _ = updated_weak.update(cx, |this, cx| {
                                        this.set_sidebar_grouping(SidebarGrouping::Updated, cx);
                                    });
                                })
                                .selected(grouping == SidebarGrouping::Updated),
                            ]
                        },
                    ),
                    MenuItem::submenu_with_value(
                        tr!("sidebar.ordering"),
                        sidebar_ordering_label(ordering),
                        move |_| {
                            let newest_weak = ordering_weak.clone();
                            let oldest_weak = ordering_weak.clone();
                            vec![
                                MenuItem::new(tr!("sidebar.ordering_newest"), move |_, cx| {
                                    let _ = newest_weak.update(cx, |this, cx| {
                                        this.set_sidebar_ordering(SidebarOrdering::Newest, cx);
                                    });
                                })
                                .selected(ordering == SidebarOrdering::Newest),
                                MenuItem::new(tr!("sidebar.ordering_oldest"), move |_, cx| {
                                    let _ = oldest_weak.update(cx, |this, cx| {
                                        this.set_sidebar_ordering(SidebarOrdering::Oldest, cx);
                                    });
                                })
                                .selected(ordering == SidebarOrdering::Oldest),
                            ]
                        },
                    ),
                ]
            },
        );
        let add_project = div()
            .id("add-project")
            .tab_index(0)
            .w(px(20.0))
            .h(px(22.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tr!("project.new_project")))
            .child(icon("icons/folder-new.svg", 14.0, theme.text_secondary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.add_project(cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.add_project(cx);
                    cx.stop_propagation();
                }
            }));

        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(options)
            .child(add_project)
    }

    fn render_sidebar_action_row(
        &self,
        id: &'static str,
        icon_path: &'static str,
        label: String,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .tab_index(0)
            .w_full()
            .h(px(SIDEBAR_ACTION_ROW_HEIGHT))
            .flex_none()
            .px(px(4.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|element| element.bg(theme.sidebar_item_background))
            .active(|element| element.bg(theme.overlay_strong))
            .child(
                div()
                    .size(px(20.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(icon_path, 14.0, theme.text_secondary)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(sp(13.0))
                    .text_color(theme.text_secondary)
                    .child(label),
            )
    }

    fn render_sidebar_new_session(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.render_sidebar_action_row(
            "sidebar-new-session",
            "icons/compose.svg",
            tr!("menu.new_task"),
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.new_session_action(&NewSession, window, cx);
        }))
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                this.new_session_action(&NewSession, window, cx);
                cx.stop_propagation();
            }
        }))
    }

    fn render_sidebar_search(&self, cx: &mut Context<Self>) -> Div {
        let search = self
            .render_sidebar_action_row(
                "sidebar-search",
                "icons/search.svg",
                tr!("sidebar.search"),
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_command_palette_action(&ToggleCommandPalette, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.toggle_command_palette_action(&ToggleCommandPalette, window, cx);
                    cx.stop_propagation();
                }
            }));
        div()
            .w_full()
            .h(px(SIDEBAR_ACTION_ROW_HEIGHT + SIDEBAR_SEARCH_BOTTOM_GAP))
            .flex_none()
            .child(search)
    }

    fn start_available_update(&mut self, cx: &mut Context<Self>) {
        if self.updater_status != crate::updater::UpdateStatus::Available {
            return;
        }
        let started = cx
            .try_global::<crate::updater::UpdaterState>()
            .and_then(|state| state.0.as_ref())
            .is_some_and(|updater| updater.install_available_update());
        if started {
            self.updater_status = crate::updater::UpdateStatus::Updating;
            self.reset_updater_button_animation();
            cx.notify();
        }
    }

    fn render_updater_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = self.updater_status;
        if status == crate::updater::UpdateStatus::Idle {
            return None;
        }

        let theme = Theme::current(cx);
        let foreground = rgb(0xFFFFFF).into();
        let available = status == crate::updater::UpdateStatus::Available;
        let button = div()
            .id("sidebar-update")
            .track_focus(&self.updater_button_focus)
            .when(available, |button| button.tab_index(0))
            .w(px(UPDATER_BUTTON_COLLAPSED_WIDTH))
            .h(px(20.0))
            .flex_none()
            .overflow_hidden()
            .rounded_full()
            .relative()
            .cursor_default()
            .bg(theme.gauge)
            .text_color(foreground)
            .text_size(sp(12.5))
            .font_weight(FontWeight::MEDIUM)
            .when(available, |button| {
                button
                    .hover(|style| style.opacity(0.92))
                    .focus_visible(|style| style.border_1().border_color(rgb(0xFFFFFF)))
                    .active(|style| style.opacity(0.8))
                    .on_hover(cx.listener(|this, hovering: &bool, _, cx| {
                        this.set_updater_button_hovered(*hovering, cx);
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_available_update(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.start_available_update(cx);
                            cx.stop_propagation();
                        }
                    }))
            });

        if !available {
            let indicator = motion::spin_slow(icon("icons/loader-circle.svg", 14.0, foreground));
            return Some(
                button
                    .tooltip(Tooltip::text(tr!("updater.updating")))
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(indicator),
                    )
                    .into_any_element(),
            );
        }

        let label: SharedString = tr_cow!("updater.update").into();
        let animation_generation = self.updater_button_animation_generation;
        if animation_generation == 0 {
            return Some(
                button
                    .child(updater_button_available_content(foreground, label, 0.0))
                    .into_any_element(),
            );
        }

        let from_width = self.updater_button_animation_from_width;
        let from_reveal = self.updater_button_animation_from_reveal;
        let target_width = if self.updater_button_expanded() {
            UPDATER_BUTTON_EXPANDED_WIDTH
        } else {
            UPDATER_BUTTON_COLLAPSED_WIDTH
        };
        let target_reveal = if self.updater_button_expanded() {
            1.0
        } else {
            0.0
        };
        let current_width = self.updater_button_width.clone();
        let current_reveal = self.updater_button_label_reveal.clone();

        Some(
            button
                .with_animation(
                    SharedString::from(format!("sidebar-updater-expand-{animation_generation}")),
                    Animation::new(Duration::from_millis(150)).with_easing(ease_out_quint()),
                    move |button, delta| {
                        let width = from_width + (target_width - from_width) * delta;
                        let reveal = from_reveal + (target_reveal - from_reveal) * delta;
                        current_width.set(width);
                        current_reveal.set(reveal);
                        button.w(px(width)).child(updater_button_available_content(
                            foreground,
                            label.clone(),
                            reveal,
                        ))
                    },
                )
                .into_any_element(),
        )
    }

    fn render_sidebar_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        div()
            .flex_none()
            .h(px(40.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .child(
                div()
                    .id("open-settings")
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .w(px(26.0))
                    .h(px(26.0))
                    .flex_none()
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .tooltip(Tooltip::text(tr_cow!("common.settings")))
                    .child(icon("icons/settings.svg", 14.0, theme.text_tertiary))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings_action(&OpenSettings, window, cx);
                    })),
            )
            .child(div().flex_1())
            .when_some(self.render_updater_button(cx), |footer, button| {
                footer.child(button)
            })
    }

    /// Resolve every ordinary local project's branch in one background pass.
    /// The render path only computes an allocation-free source fingerprint;
    /// collection building and daemon requests happen once when that moves.
    fn ensure_sidebar_branch_labels(&self, cx: &mut Context<Self>) {
        if self.state.sidebar_grouping != SidebarGrouping::Project {
            return;
        }

        let mut fingerprint = 0xb4a7_c4e5_51de_ba11;
        for session in &self.state.sessions {
            if session.has_started() && matches!(&session.workspace, SessionWorkspace::Local) {
                fingerprint = mix_uuid(fingerprint, session.id);
                fingerprint = mix_uuid(fingerprint, session.project_id);
            }
        }
        for project in &self.state.projects {
            fingerprint = mix_uuid(fingerprint, project.id);
        }
        if self.sidebar_branch_scan_fingerprint.get() == Some(fingerprint) {
            return;
        }
        self.sidebar_branch_scan_fingerprint.set(Some(fingerprint));
        let generation = self.sidebar_branch_scan_generation.get().wrapping_add(1);
        self.sidebar_branch_scan_generation.set(generation);

        let local_project_ids = self
            .state
            .sessions
            .iter()
            .filter(|session| {
                session.has_started() && matches!(&session.workspace, SessionWorkspace::Local)
            })
            .map(|session| session.project_id)
            .collect::<HashSet<_>>();
        let projectless_root = crate::projectless::workspace_root();
        let paths = self
            .state
            .projects
            .iter()
            .filter(|project| local_project_ids.contains(&project.id))
            .filter(|project| !sidebar_project_is_projectless(project, projectless_root.as_deref()))
            .map(|project| project.path.clone())
            .collect::<HashSet<_>>();
        if paths.is_empty() {
            self.sidebar_branch_labels.borrow_mut().clear();
            return;
        }

        let workspace = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |waku, cx| {
            let labels = cx
                .background_executor()
                .spawn(async move {
                    let mut labels = HashMap::new();
                    for path in paths {
                        let branch = match workspace.request(
                            waku_client::WorkspaceOperation::InspectBranches { cwd: path.clone() },
                        ) {
                            Ok(waku_client::WorkspaceResult::Branches {
                                snapshot: Some(snapshot),
                            }) => snapshot.display_branch().map(str::to_owned),
                            _ => None,
                        };
                        if let Some(branch) = branch {
                            labels.insert(path, branch);
                        }
                    }
                    labels
                })
                .await;
            let _ = waku.update(cx, |waku, cx| {
                if waku.sidebar_branch_scan_generation.get() != generation {
                    return;
                }
                *waku.sidebar_branch_labels.borrow_mut() = labels
                    .into_iter()
                    .map(|(path, branch)| (path, SharedString::from(branch)))
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cache_sidebar_branch_label(&self, path: &Path, branch: Option<&str>) {
        let mut labels = self.sidebar_branch_labels.borrow_mut();
        if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
            labels.insert(path.to_path_buf(), SharedString::from(branch.to_owned()));
        } else {
            labels.remove(path);
        }
    }

    pub(super) fn render_sidebar(
        &self,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        self.ensure_sidebar_branch_labels(cx);
        let is_resizing = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == PanelResizeTarget::Sidebar);

        let rows = self.sidebar_rows_cached(Local::now().date_naive(), unix_time());
        self.sync_sidebar_rows(&rows);
        let history_scrolled =
            self.sidebar_list_state.scroll_px_offset_for_scrollbar().y < px(-0.5);
        let entity = cx.entity().downgrade();

        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(if is_resizing {
                theme.sidebar_drag_background
            } else {
                theme.sidebar
            })
            .child(self.render_sidebar_titlebar(window, cx))
            .child(
                div()
                    .flex_none()
                    .px(px(10.0))
                    .child(self.render_sidebar_new_session(cx)),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div().px(px(10.0)).size_full().child(
                            list(
                                self.sidebar_list_state.clone(),
                                move |index, _window, cx| {
                                    entity
                                        .upgrade()
                                        .map(|entity| {
                                            entity.update(cx, |this, cx| {
                                                this.sidebar_row(index, &rows, cx)
                                            })
                                        })
                                        .unwrap_or_else(|| div().into_any_element())
                                },
                            )
                            .size_full(),
                        ),
                    )
                    .child(scrollbar::vertical(
                        &self.sidebar_list_state,
                        &self.sidebar_scrollbar,
                    ))
                    .when(history_scrolled, |scroll| {
                        scroll.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .w_full()
                                .h(px(1.0))
                                .bg(theme.border),
                        )
                    }),
            )
            .child(self.render_sidebar_footer(cx))
    }

    /// The sidebar row snapshot, rebuilt only when its inputs move.
    ///
    /// The sidebar re-renders at pulse cadence whenever one of its session
    /// rows shows a working spinner, and rebuilding the snapshot sorts every
    /// started session and runs calendar math per session — far too much per
    /// tick for values that move at most once per stream commit. The
    /// fingerprint is an allocation-free scan of exactly what
    /// [`Self::sidebar_rows`] reads: started sessions with their project and
    /// recency, the presentation preferences, the collapsed-group set, and
    /// today's date and the moving project-recency boundary.
    fn sidebar_rows_cached(&self, today: NaiveDate, now: u64) -> Rc<Vec<SidebarRow>> {
        let mut fingerprint = mix(0x51de_ba5e_5eed_c0de, today.num_days_from_ce() as u64);
        fingerprint = mix(
            fingerprint,
            match self.state.sidebar_grouping {
                SidebarGrouping::Project => 1,
                SidebarGrouping::Updated => 2,
            },
        );
        fingerprint = mix(
            fingerprint,
            match self.state.sidebar_ordering {
                SidebarOrdering::Newest => 1,
                SidebarOrdering::Oldest => 2,
            },
        );
        for session in &self.state.sessions {
            if !session.has_started() {
                continue;
            }
            fingerprint = mix_uuid(fingerprint, session.id);
            fingerprint = mix_uuid(fingerprint, session.project_id);
            fingerprint = mix(fingerprint, sidebar_session_timestamp(session));
            if self.state.sidebar_grouping == SidebarGrouping::Project {
                fingerprint = mix(
                    fingerprint,
                    u64::from(
                        sidebar_session_timestamp(session)
                            >= now.saturating_sub(SIDEBAR_PROJECT_RECENT_WINDOW_SECONDS),
                    ),
                );
            }
        }
        if self.state.sidebar_grouping == SidebarGrouping::Project {
            for project in &self.state.projects {
                fingerprint = mix_uuid(fingerprint, project.id);
            }
            // A map has no stable iteration order; combine order-independently.
            let revealed =
                self.sidebar_project_reveal_counts
                    .iter()
                    .fold(0u64, |combined, (group, count)| {
                        combined.wrapping_add(group.mix_fingerprint(*count as u64))
                    });
            fingerprint = mix(
                mix(fingerprint, self.sidebar_project_reveal_counts.len() as u64),
                revealed,
            );
        }
        // A set has no stable iteration order; combine order-independently.
        let collapsed = self
            .sidebar_collapsed_groups
            .iter()
            .fold(0u64, |combined, group| {
                combined.wrapping_add(group.mix_fingerprint(0))
            });
        fingerprint = mix(
            mix(fingerprint, self.sidebar_collapsed_groups.len() as u64),
            collapsed,
        );
        if self.sidebar_rows_fingerprint.get() != Some(fingerprint) {
            *self.sidebar_rows_snapshot.borrow_mut() = Rc::new(self.sidebar_rows(today, now));
            self.sidebar_rows_fingerprint.set(Some(fingerprint));
        }
        self.sidebar_rows_snapshot.borrow().clone()
    }

    /// Snapshot the session history as a flat list of lightweight rows under
    /// the current grouping and ordering preferences.
    fn sidebar_rows(&self, today: NaiveDate, now: u64) -> Vec<SidebarRow> {
        let mut sorted_sessions = self
            .state
            .sessions
            .iter()
            .filter(|session| session.has_started())
            .collect::<Vec<_>>();
        sort_sidebar_sessions(&mut sorted_sessions, self.state.sidebar_ordering);

        let mut rows = vec![SidebarRow::Search];
        match self.state.sidebar_grouping {
            SidebarGrouping::Updated => {
                let mut grouped_sessions: [Vec<Uuid>; 6] = std::array::from_fn(|_| Vec::new());
                for session in sorted_sessions {
                    grouped_sessions
                        [session_date_group(sidebar_session_timestamp(session), today).index()]
                    .push(session.id);
                }
                let mut groups = SessionDateGroup::ALL;
                if self.state.sidebar_ordering == SidebarOrdering::Oldest {
                    groups.reverse();
                }
                for date_group in groups {
                    let group = SidebarGroup::Updated(date_group);
                    append_sidebar_group_rows(
                        &mut rows,
                        group,
                        &grouped_sessions[date_group.index()],
                        self.sidebar_collapsed_groups.contains(&group),
                        false,
                    );
                }
            }
            SidebarGrouping::Project => {
                let recent_cutoff = now.saturating_sub(SIDEBAR_PROJECT_RECENT_WINDOW_SECONDS);
                let session_timestamps = sorted_sessions
                    .iter()
                    .map(|session| (session.id, sidebar_session_timestamp(session)))
                    .collect::<HashMap<_, _>>();
                let projectless_root = crate::projectless::workspace_root();
                let projectless_project_ids = self
                    .state
                    .projects
                    .iter()
                    .filter(|project| {
                        sidebar_project_is_projectless(project, projectless_root.as_deref())
                    })
                    .map(|project| project.id)
                    .collect::<HashSet<_>>();
                for (group, sessions) in
                    project_sidebar_groups(&sorted_sessions, &projectless_project_ids)
                {
                    let revealed_older_sessions = self
                        .sidebar_project_reveal_counts
                        .get(&group)
                        .copied()
                        .unwrap_or_default();
                    let (visible_sessions, show_more) = visible_project_sessions(
                        &sessions,
                        &session_timestamps,
                        recent_cutoff,
                        revealed_older_sessions,
                    );
                    append_sidebar_group_rows(
                        &mut rows,
                        group,
                        &visible_sessions,
                        self.sidebar_collapsed_groups.contains(&group),
                        show_more,
                    );
                }
            }
        }
        if rows.len() == 1 {
            // Keep the header actions visible while there is no history.
            let group = match self.state.sidebar_grouping {
                SidebarGrouping::Updated => SidebarGroup::Updated(SessionDateGroup::Today),
                SidebarGrouping::Project => {
                    let projectless_root = crate::projectless::workspace_root();
                    self.state
                        .selected_project
                        .and_then(|project_id| {
                            self.state
                                .projects
                                .iter()
                                .find(|project| project.id == project_id)
                        })
                        .or_else(|| self.state.projects.first())
                        .map(|project| {
                            if sidebar_project_is_projectless(project, projectless_root.as_deref())
                            {
                                SidebarGroup::Projectless
                            } else {
                                SidebarGroup::Project(project.id)
                            }
                        })
                        .unwrap_or(SidebarGroup::Projectless)
                }
            };
            rows.push(SidebarRow::Header(group));
        }
        rows
    }

    /// Keep the virtualized list in sync with the current row snapshot.
    /// Rows are cheap values, so only the minimal changed suffix is spliced,
    /// preserving scroll position and measured heights across unrelated churn
    /// (e.g. the active session's `updated_at` bumping on every stream tick).
    fn sync_sidebar_rows(&self, rows: &[SidebarRow]) {
        let mut cached = self.sidebar_row_cache.borrow_mut();
        if cached.as_slice() == rows {
            return;
        }
        let prefix = cached
            .iter()
            .zip(rows.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let old_count = cached.len();
        *cached = rows.to_vec();
        if old_count == 0 {
            self.sidebar_list_state
                .reset_with_uniform_height(rows.len(), px(SIDEBAR_SESSION_ROW_HEIGHT));
        } else {
            self.sidebar_list_state
                .splice(prefix..old_count, rows.len() - prefix);
            // Newly inserted rows have no measured height yet; give them the
            // uniform hint so the scrollbar keeps a correct total height.
            self.sidebar_list_state
                .clone()
                .with_uniform_item_height(px(SIDEBAR_SESSION_ROW_HEIGHT));
        }
    }

    fn sidebar_row(&self, index: usize, rows: &[SidebarRow], cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = rows.get(index) else {
            return div().into_any_element();
        };
        match *row {
            SidebarRow::Search => self.render_sidebar_search(cx).into_any_element(),
            SidebarRow::Header(group) => {
                let has_expanded_children = rows.get(index + 1).is_some_and(|row| {
                    matches!(row, SidebarRow::Session(_) | SidebarRow::ShowMore(_))
                });
                self.render_sidebar_group_header(group, index == 1, has_expanded_children, cx)
                    .into_any_element()
            }
            SidebarRow::Session(session_id) => self
                .render_sidebar_session_item(session_id, cx)
                .into_any_element(),
            SidebarRow::ShowMore(group) => {
                self.render_sidebar_show_more(group, cx).into_any_element()
            }
            SidebarRow::GroupSpacer => div().w_full().h(px(10.0)).into_any_element(),
        }
    }

    fn render_sidebar_group_header(
        &self,
        group: SidebarGroup,
        first: bool,
        has_expanded_children: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let collapsed = self.sidebar_collapsed_groups.contains(&group);
        let group_key = group.element_key();
        let group_name = SharedString::from(format!("sidebar-group-header-{group_key}"));
        let header_focus = self
            .sidebar_group_header_focuses
            .borrow_mut()
            .entry(group)
            .or_insert_with(|| cx.focus_handle())
            .clone();
        let show_folder_icon =
            matches!(group, SidebarGroup::Project(_) | SidebarGroup::Projectless);
        let folder_icon = if collapsed {
            "icons/folder.svg"
        } else {
            "icons/folder-open.svg"
        };
        let label = match group {
            SidebarGroup::Updated(group) => group.label(),
            SidebarGroup::Project(project_id) => self
                .state
                .projects
                .iter()
                .find(|project| project.id == project_id)
                .map(Project::display_name)
                .unwrap_or_else(|| tr!("project.no_project_name")),
            SidebarGroup::Projectless => tr!("project.no_project_name"),
        };
        let updated_chevron = matches!(group, SidebarGroup::Updated(_)).then(|| {
            icon("icons/chevron-down.svg", 14.0, theme.text_secondary)
                .when(collapsed, |icon| {
                    icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
                })
                .invisible()
                .group_hover(group_name.clone(), |icon| icon.visible())
        });
        let compose = show_folder_icon.then(|| {
            let compose_focus = self
                .sidebar_group_compose_focuses
                .borrow_mut()
                .entry(group)
                .or_insert_with(|| cx.focus_handle())
                .clone();
            div()
                .w(px(20.0))
                .h(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "sidebar-group-compose-{group_key}"
                        )))
                        .track_focus(&compose_focus)
                        .tab_index(0)
                        .tab_stop(true)
                        .w_0()
                        .h(px(22.0))
                        .overflow_hidden()
                        .rounded(px(4.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_default()
                        .opacity(0.0)
                        .group_hover(group_name.clone(), |style| style.w(px(20.0)).opacity(1.0))
                        .focus_visible(|style| {
                            style
                                .w(px(20.0))
                                .opacity(1.0)
                                .border_1()
                                .border_color(theme.accent)
                        })
                        .hover(|style| style.bg(theme.overlay))
                        .active(|style| style.bg(theme.overlay_strong))
                        .tooltip(Tooltip::text(tr!("menu.new_task")))
                        .child(icon("icons/compose.svg", 14.0, theme.text_secondary))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_new_task_for_sidebar_group(group, window, cx);
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.open_new_task_for_sidebar_group(group, window, cx);
                                cx.stop_propagation();
                            }
                        })),
                )
        });

        let header = session_group_header(&theme)
            .id(SharedString::from(format!(
                "sidebar-group-toggle-{group_key}"
            )))
            .track_focus(&header_focus)
            .tab_index(0)
            .tab_group()
            .tab_stop(true)
            .group(group_name)
            .relative()
            .w_full()
            .rounded(px(6.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.sidebar_item_background))
            .active(|style| style.bg(theme.overlay_strong))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .when(show_folder_icon, |element| {
                        element.child(icon(folder_icon, 14.0, theme.text_secondary))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .child(div().min_w_0().truncate().child(label))
                            .when_some(updated_chevron, |element, chevron| element.child(chevron)),
                    )
                    .child(div().flex_1()),
            )
            .when_some(compose, |element, compose| element.child(compose))
            .when(first, |element| {
                element.child(self.render_sidebar_header_actions(cx))
            })
            .when(
                show_folder_icon && has_expanded_children,
                |element| {
                    element.child(
                        div()
                            .absolute()
                            .left(px(SIDEBAR_GROUP_GUIDE_X))
                            .top(px(19.0))
                            .bottom(px(-2.0))
                            .w(px(1.0))
                            .bg(theme.border),
                    )
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_group(group, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => {
                        this.toggle_sidebar_group(group, cx);
                        cx.stop_propagation();
                    }
                    "left" if !collapsed => {
                        this.set_sidebar_group_collapsed(group, true, cx);
                        cx.stop_propagation();
                    }
                    "right" if collapsed => {
                        this.set_sidebar_group_collapsed(group, false, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }));

        div().w_full().pb(px(2.0)).child(header)
    }

    fn open_new_task_for_sidebar_group(
        &mut self,
        group: SidebarGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        match group {
            SidebarGroup::Project(project_id) => self.select_project(project_id, cx),
            SidebarGroup::Projectless => self.create_projectless_session(cx),
            SidebarGroup::Updated(_) => return,
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
    }

    fn render_sidebar_show_more(
        &self,
        group: SidebarGroup,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let group_key = group.element_key();
        let focus = self
            .sidebar_show_more_focuses
            .borrow_mut()
            .entry(group)
            .or_insert_with(|| cx.focus_handle())
            .clone();
        let button = div()
            .id(SharedString::from(format!("sidebar-show-more-{group_key}")))
            .track_focus(&focus)
            .tab_index(0)
            .tab_stop(true)
            .flex_none()
            .cursor_default()
            .text_size(sp(12.5))
            .text_color(theme.text_tertiary)
            .focus_visible(|style| style.text_color(theme.text))
            .hover(|style| style.text_color(theme.text))
            .child(tr!("sidebar.show_more"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show_more_project_sessions(group, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.show_more_project_sessions(group, cx);
                    cx.stop_propagation();
                }
            }));

        div()
            .relative()
            .w_full()
            .h(px(30.0))
            .pl(px(SIDEBAR_GROUP_CHILD_PADDING))
            .flex()
            .items_center()
            .child(button)
            .child(
                div()
                    .absolute()
                    .left(px(SIDEBAR_GROUP_GUIDE_X))
                    .top_0()
                    .w(px(
                        SIDEBAR_GROUP_CHILD_PADDING - SIDEBAR_GROUP_GUIDE_X - 4.0,
                    ))
                    .h(px(15.0))
                    .border_l_1()
                    .border_b_1()
                    .rounded_bl(px(4.0))
                    .border_color(theme.border),
            )
    }

    fn show_more_project_sessions(&mut self, group: SidebarGroup, cx: &mut Context<Self>) {
        let revealed = self.sidebar_project_reveal_counts.entry(group).or_default();
        *revealed = revealed.saturating_add(SIDEBAR_PROJECT_REVEAL_BATCH);
        self.sidebar_rows_fingerprint.set(None);
        cx.notify();
    }

    fn toggle_sidebar_group(&mut self, group: SidebarGroup, cx: &mut Context<Self>) {
        let collapsed = !self.sidebar_collapsed_groups.contains(&group);
        self.set_sidebar_group_collapsed(group, collapsed, cx);
    }

    pub(super) fn collapse_all_sidebar_groups(&mut self, cx: &mut Context<Self>) {
        let groups = self
            .sidebar_rows_cached(Local::now().date_naive(), unix_time())
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Header(group) => Some(*group),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for group in groups {
            changed |= self.sidebar_collapsed_groups.insert(group);
            changed |= self.sidebar_project_reveal_counts.remove(&group).is_some();
        }
        if changed {
            self.sidebar_rows_fingerprint.set(None);
            cx.notify();
        }
    }

    fn set_sidebar_group_collapsed(
        &mut self,
        group: SidebarGroup,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let collapse_changed = if collapsed {
            self.sidebar_collapsed_groups.insert(group)
        } else {
            self.sidebar_collapsed_groups.remove(&group)
        };
        let reveal_reset = collapsed
            && self
                .sidebar_project_reveal_counts
                .remove(&group)
                .is_some();
        if collapse_changed || reveal_reset {
            self.sidebar_rows_fingerprint.set(None);
            cx.notify();
        }
    }

    fn set_sidebar_grouping(&mut self, grouping: SidebarGrouping, cx: &mut Context<Self>) {
        if self.state.sidebar_grouping == grouping {
            return;
        }
        self.state.sidebar_grouping = grouping;
        self.sidebar_rows_fingerprint.set(None);
        self.sidebar_branch_scan_fingerprint.set(None);
        self.sidebar_branch_scan_generation
            .set(self.sidebar_branch_scan_generation.get().wrapping_add(1));
        self.sidebar_list_state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: Pixels::ZERO,
        });
        self.save();
        cx.notify();
    }

    fn set_sidebar_ordering(&mut self, ordering: SidebarOrdering, cx: &mut Context<Self>) {
        if self.state.sidebar_ordering == ordering {
            return;
        }
        self.state.sidebar_ordering = ordering;
        self.sidebar_rows_fingerprint.set(None);
        self.sidebar_list_state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: Pixels::ZERO,
        });
        self.save();
        cx.notify();
    }

    fn begin_session_rename(
        &mut self,
        session_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(title) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(localized_session_title)
        else {
            return;
        };

        self.session_rename = Some(session_id);
        self.session_rename_input.update(cx, |input, cx| {
            input.set_content(title, cx);
            input.select_all_text(cx);
        });
        let focus = self.session_rename_input.read(cx).focus();
        window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        cx.notify();
    }

    pub(super) fn commit_session_rename(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_rename.take() else {
            return;
        };
        let title = self
            .session_rename_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let should_update = !title.is_empty()
            && self
                .state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| session.title != title);
        if should_update
            && self
                .state
                .session_mut(session_id)
                .is_some_and(|session| session.set_title(&title))
        {
            self.save();
        }
        cx.notify();
    }

    fn cancel_session_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_rename.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn render_sidebar_session_item(&self, session_id: Uuid, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return div().into_any_element();
        };
        let selected = sidebar_session_selected(
            self.state.selected_session,
            self.pending_session_activation
                .map(|pending| pending.session_id),
            session_id,
        );
        let working = matches!(
            session.status,
            SessionStatus::Connecting | SessionStatus::Working
        );
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id);
        let grouped_by_project = self.state.sidebar_grouping == SidebarGrouping::Project;
        let left_padding = if grouped_by_project {
            SIDEBAR_GROUP_CHILD_PADDING
        } else {
            8.0
        };
        let detail_label = if grouped_by_project {
            persisted_sidebar_branch_label(&session.workspace)
                .map(|branch| SharedString::from(branch.to_owned()))
                .or_else(|| {
                    if !matches!(&session.workspace, SessionWorkspace::Local) {
                        return None;
                    }
                    project.and_then(|project| {
                        self.sidebar_branch_labels
                            .borrow()
                            .get(&project.path)
                            .cloned()
                    })
                })
        } else {
            Some(SharedString::from(
                project
                    .map(Project::display_name)
                    .unwrap_or_else(|| tr!("sidebar.unknown_project")),
            ))
        };
        let has_detail_label = detail_label.is_some();
        let detail_icon = if grouped_by_project {
            "icons/git-branch.svg"
        } else {
            "icons/folder.svg"
        };
        let rename_input =
            (self.session_rename == Some(session_id)).then(|| self.session_rename_input.clone());
        let renaming = rename_input.is_some();
        let title = if let Some(rename_input) = rename_input {
            div()
                .id(SharedString::from(format!(
                    "session-rename-field-{session_id}"
                )))
                .key_context(SESSION_RENAME_PARENT_CONTEXT)
                .on_action(cx.listener(|this, _: &CancelSessionRename, window, cx| {
                    this.cancel_session_rename(window, cx);
                }))
                .h(px(18.0))
                .flex_1()
                .min_w_0()
                .px(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.accent)
                .bg(theme.inset)
                .flex()
                .items_center()
                .text_size(sp(13.5))
                .text_color(theme.text)
                .child(rename_input)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .line_clamp(1)
                .text_overflow(gpui::TextOverflow::Truncate("...".into()))
                .text_size(sp(13.5))
                .text_color(theme.text)
                .child(SharedString::from(localized_session_title(session)))
                .into_any_element()
        };
        let waku = cx.entity().downgrade();
        let menu = self.menu_handle(format!("session-{session_id}"), cx);
        let row_focus = menu.trigger_focus_handle().clone();
        let keyboard_menu = menu.clone();
        let row = div()
            .id(SharedString::from(format!("session-{}", session.id)))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .pl(px(left_padding))
            .pr(px(8.0))
            .py(px(7.0))
            .rounded(px(7.0))
            .cursor_default()
            .when(selected, |element| {
                element.bg(theme.sidebar_item_background)
            })
            .hover(|element| element.bg(theme.sidebar_item_background))
            .active(|element| element.bg(theme.sidebar_item_background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .overflow_hidden()
                    .line_height(sp(18.0))
                    .child(title)
                    .when(working, |element| {
                        element.child(motion::spin_slow(icon(
                            "icons/loader-circle.svg",
                            12.0,
                            status_color(&theme, session.status),
                        )))
                    })
                    .when(session.status == SessionStatus::Waiting, |element| {
                        element.child(icon(
                            "icons/alert.svg",
                            12.0,
                            status_color(&theme, session.status),
                        ))
                    })
                    .when(session.status == SessionStatus::Failed, |element| {
                        element.child(icon(
                            "icons/x.svg",
                            12.0,
                            status_color(&theme, session.status),
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .text_size(sp(if grouped_by_project { 12.5 } else { 13.0 }))
                    .line_height(sp(15.0))
                    .when_some(detail_label, |element, label| {
                        element
                            .child(icon(detail_icon, 12.5, theme.text_tertiary))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_tertiary)
                                    .child(label),
                            )
                    })
                    .when(!has_detail_label, |element| element.child(div().flex_1()))
                    .when_some(
                        session_time_label(session, unix_time()),
                        |element, label| {
                            element.child(
                                div()
                                    .flex_none()
                                    .text_size(sp(12.5))
                                    .text_color(if session.is_busy() {
                                        theme.text_tertiary
                                    } else {
                                        theme.text_ghost
                                    })
                                    .child(SharedString::from(label)),
                            )
                        },
                    ),
            )
            .when(!renaming, |element| {
                element
                    .track_focus(&row_focus)
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let key = event.keystroke.key.as_str();
                        if matches!(key, "enter" | "space") {
                            this.select_session(session_id, cx);
                            cx.stop_propagation();
                        } else if key == "f10" && event.keystroke.modifiers.shift {
                            keyboard_menu.open_context_menu(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_session(session_id, cx);
                    }))
            });
        let row = if renaming {
            div()
                .w_full()
                .child(row)
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    if this.session_rename == Some(session_id) {
                        this.commit_session_rename(cx);
                    }
                }))
                .into_any_element()
        } else {
            context_menu(
                div().w_full().child(row),
                SharedString::from(format!("session-menu-{session_id}")),
                &menu,
                move |_| {
                    let rename_waku = waku.clone();
                    let remove_waku = waku.clone();
                    vec![
                        MenuItem::new(tr!("common.rename"), move |window, cx| {
                            let _ = rename_waku.update(cx, |waku, cx| {
                                waku.begin_session_rename(session_id, window, cx);
                            });
                        }),
                        MenuItem::Separator,
                        MenuItem::new(tr!("common.remove"), move |_, cx| {
                            let _ = remove_waku
                                .update(cx, |waku, cx| waku.remove_session(session_id, cx));
                        }),
                    ]
                },
            )
        };

        div()
            .relative()
            .w_full()
            .pb(px(SIDEBAR_SESSION_ROW_GAP))
            .child(row)
            .when(grouped_by_project, |element| {
                element.child(
                    div()
                        .absolute()
                        .left(px(SIDEBAR_GROUP_GUIDE_X))
                        .top_0()
                        .bottom_0()
                        .w(px(1.0))
                        .bg(theme.border),
                )
            })
            .into_any_element()
    }

    // ── Header ─────────────────────────────────────────────────────────────

    pub(super) fn render_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let title = session
            .map(localized_session_title)
            .unwrap_or_else(|| tr!("session.new_task"));
        let agent_preset_label = session
            .filter(|session| session.provider == ProviderKind::DeepSeek && session.has_started())
            .and_then(|session| self.agent_preset_label_for_session(session));
        let left_window_controls = (!self.sidebar_visible)
            .then(|| {
                self.render_client_window_controls(
                    super::window_chrome::WindowControlSide::Left,
                    window,
                    cx,
                )
            })
            .flatten();
        let right_window_controls = (!self.right_panel_visible)
            .then(|| {
                self.render_client_window_controls(
                    super::window_chrome::WindowControlSide::Right,
                    window,
                    cx,
                )
            })
            .flatten();
        div()
            .id("window-header")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .children(left_window_controls)
            // The header starts where the sidebar ends, so until the sidebar
            // is wide enough to host the traffic lights itself the header has
            // to clear them. Steady state with the sidebar open adds nothing;
            // a sidebar sliding in shrinks the inset as it takes the lights
            // over, which is what keeps the title from passing under them.
            .pl(if self.sidebar_visible {
                px(14.0 + (TRAFFIC_LIGHT_CLEARANCE - self.sidebar_rendered_width).max(0.0))
            } else {
                px(0.0)
            })
            .pr(px(14.0))
            .when(!self.sidebar_visible, |element| {
                element
                    .child(
                        self.window_drag_region(
                            div()
                                .id("header-traffic-light-drag-region")
                                .w(px(TRAFFIC_LIGHT_CLEARANCE - 8.0))
                                .h_full()
                                .flex_none(),
                            cx,
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(self.render_sidebar_toggle(cx))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .child(self.render_history_button(
                                        "navigate-back",
                                        "icons/arrow-left.svg",
                                        !self.session_navigation.back.is_empty(),
                                        true,
                                        cx,
                                    ))
                                    .child(self.render_history_button(
                                        "navigate-forward",
                                        "icons/arrow-right.svg",
                                        !self.session_navigation.forward.is_empty(),
                                        false,
                                        cx,
                                    )),
                            ),
                    )
            })
            .child(
                self.window_drag_region(
                    div()
                        .id("header-title-drag-region")
                        .h_full()
                        .min_w_0()
                        .flex_shrink(1.0)
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(sp(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(title)),
                        )
                        .children(agent_preset_label.map(|label| {
                            div()
                                .h(px(22.0))
                                .max_w(px(180.0))
                                .px(px(6.0))
                                .rounded(px(6.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .bg(theme.overlay)
                                .text_size(sp(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text_secondary)
                                .child(icon("icons/bot.svg", 10.5, theme.text_tertiary))
                                .child(div().min_w_0().truncate().child(SharedString::from(label)))
                        })),
                    cx,
                ),
            )
            .child(
                self.window_drag_region(
                    div().id("header-center-drag-region").h_full().flex_1(),
                    cx,
                ),
            )
            .child(self.render_background_work_summary(cx))
            .when(!self.right_panel_visible, |element| {
                element
                    .when(self.fps_counter_visible, |element| {
                        element.child(self.render_fps_counter(cx))
                    })
                    .child(self.render_right_panel_toggle(cx))
            })
            .children(right_window_controls)
    }

    // ── Empty states ───────────────────────────────────────────────────────

    pub(super) fn render_empty_state(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        if self.selected_project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_8()
                .pb(px(46.0))
                .child(icon("icons/sparkle.svg", 24.0, theme.accent))
                .child(
                    div()
                        .mt(px(16.0))
                        .text_size(sp(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tr_cow!("onboarding.open_project_to_begin")),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(sp(12.5))
                        .line_height(sp(19.0))
                        .text_color(theme.text_tertiary)
                        .child(tr_cow!("onboarding.description")),
                )
                .child(
                    div()
                        .mt(px(20.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.0))
                        .tab_index(0)
                        .tab_group()
                        .tab_stop(false)
                        .child(
                            div()
                                .id("onboarding-add-project")
                                .track_focus(&self.onboarding_add_project_focus)
                                .tab_index(0)
                                .focus_visible(|style| style.border_1().border_color(theme.accent))
                                .h(px(32.0))
                                .px(px(14.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .cursor_default()
                                .bg(theme.inverse)
                                .text_color(theme.on_inverse)
                                .text_size(sp(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .hover(|element| element.opacity(0.9))
                                .active(|element| element.opacity(0.8))
                                .child(tr_cow!("onboarding.open_project_folder"))
                                .on_click(cx.listener(|this, _, _, cx| this.add_project(cx)))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        this.add_project(cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        )
                        .child(
                            div()
                                .id("onboarding-projectless")
                                .track_focus(&self.onboarding_projectless_focus)
                                .tab_index(1)
                                .focus_visible(|style| style.border_1().border_color(theme.accent))
                                .h(px(30.0))
                                .px(px(12.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .cursor_default()
                                .text_color(theme.text_secondary)
                                .text_size(sp(12.5))
                                .hover(|element| element.bg(theme.overlay))
                                .active(|element| element.bg(theme.overlay_strong))
                                .child(icon("icons/x.svg", 11.0, theme.text_tertiary))
                                .child(tr_cow!("project.no_project"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.create_projectless_session(cx);
                                }))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        this.create_projectless_session(cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        ),
                );
        }
        let selected_project_id = self.state.selected_project;
        let projectless_selected = self.selected_project().is_some_and(Project::is_projectless);
        let project_name = self
            .selected_project()
            .map(|project| {
                if project.is_projectless() {
                    tr!("project.without_a_project")
                } else {
                    project.display_name()
                }
            })
            .unwrap_or_else(|| tr!("project.your_project"));
        let project_options = self
            .state
            .projects
            .iter()
            .filter(|project| !project.is_projectless())
            .filter(|project| Some(project.id) == selected_project_id)
            .chain(
                self.state
                    .projects
                    .iter()
                    .filter(|project| !project.is_projectless())
                    .filter(|project| Some(project.id) != selected_project_id),
            )
            .map(|project| (project.id, project.display_name()))
            .collect::<Vec<_>>();
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("empty-state-project", cx);
        let project_selector = dropdown_menu(
            ProjectNameSelector::new("empty-state-project", project_name)
                .selected(handle.is_open()),
            "empty-state-project-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = project_options
                    .clone()
                    .into_iter()
                    .map(|(project_id, project_name)| {
                        let weak = weak.clone();
                        MenuItem::new(project_name, move |_, cx| {
                            if Some(project_id) == selected_project_id {
                                return;
                            }
                            let _ = weak.update(cx, |this, cx| this.select_project(project_id, cx));
                        })
                        .selected(Some(project_id) == selected_project_id)
                    })
                    .collect::<Vec<_>>();
                if !items.is_empty() {
                    items.push(MenuItem::Separator);
                }
                let add_project_weak = weak.clone();
                items.push(
                    MenuItem::new(tr!("project.new_project"), move |_, cx| {
                        let _ = add_project_weak.update(cx, |this, cx| this.add_project(cx));
                    })
                    .icon("icons/folder-new.svg"),
                );
                let projectless_weak = weak.clone();
                items.push(
                    MenuItem::new(tr!("project.no_project"), move |_, cx| {
                        let _ = projectless_weak.update(cx, |this, cx| {
                            if !this.selected_project().is_some_and(Project::is_projectless) {
                                this.create_projectless_session(cx);
                            }
                        });
                    })
                    .icon("icons/x.svg")
                    .selected(projectless_selected),
                );
                items
            },
        );
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_8()
            .pb(px(52.0))
            .child(icon("icons/sparkle.svg", 20.0, theme.accent))
            .child(
                div()
                    .mt(px(14.0))
                    .flex()
                    .items_baseline()
                    .text_size(sp(20.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .when(projectless_selected, |element| {
                        element.child(tr_cow!("onboarding.what_should_we_build"))
                    })
                    .when(!projectless_selected, |element| {
                        element
                            .child(tr_cow!("onboarding.what_should_we_build_in"))
                            .child(project_selector)
                            .child(tr_cow!("onboarding.question_mark"))
                    }),
            )
    }
}

fn localized_session_title(session: &AgentSession) -> String {
    let title = session.display_title();
    if title == AgentSession::DEFAULT_TITLE {
        tr!("session.new_task")
    } else {
        title.to_owned()
    }
}

fn sidebar_session_selected(
    selected_session: Option<Uuid>,
    pending_session: Option<Uuid>,
    session_id: Uuid,
) -> bool {
    pending_session.map_or(selected_session == Some(session_id), |pending| {
        pending == session_id
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_sessions_by_calendar_period() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
        let cases = [
            ((2026, 8, 12), SessionDateGroup::Today),
            ((2026, 8, 11), SessionDateGroup::Yesterday),
            ((2026, 8, 10), SessionDateGroup::ThisWeek),
            ((2026, 8, 1), SessionDateGroup::ThisMonth),
            ((2026, 1, 1), SessionDateGroup::ThisYear),
            ((2025, 12, 31), SessionDateGroup::More),
        ];

        for ((year, month, day), expected) in cases {
            let session_date = NaiveDate::from_ymd_opt(year, month, day).unwrap();
            assert_eq!(session_date_group_for_dates(session_date, today), expected);
        }
    }

    #[test]
    fn future_sessions_stay_in_today() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
        let tomorrow = NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        assert_eq!(
            session_date_group_for_dates(tomorrow, today),
            SessionDateGroup::Today
        );
    }

    #[test]
    fn collapsed_sidebar_group_keeps_only_its_header_and_spacer() {
        let sessions = [Uuid::from_u128(1), Uuid::from_u128(2)];
        let group = SidebarGroup::Updated(SessionDateGroup::Today);
        let mut expanded = Vec::new();
        append_sidebar_group_rows(&mut expanded, group, &sessions, false, false);
        assert_eq!(
            expanded,
            vec![
                SidebarRow::Header(group),
                SidebarRow::Session(sessions[0]),
                SidebarRow::Session(sessions[1]),
                SidebarRow::GroupSpacer,
            ]
        );

        let mut collapsed = Vec::new();
        append_sidebar_group_rows(&mut collapsed, group, &sessions, true, false);
        assert_eq!(
            collapsed,
            vec![SidebarRow::Header(group), SidebarRow::GroupSpacer,]
        );
    }

    #[test]
    fn hidden_project_sessions_keep_a_keyboard_reveal_row() {
        let group = SidebarGroup::Project(Uuid::from_u128(1));
        let mut expanded = Vec::new();
        append_sidebar_group_rows(&mut expanded, group, &[], false, true);
        assert_eq!(
            expanded,
            vec![
                SidebarRow::Header(group),
                SidebarRow::ShowMore(group),
                SidebarRow::GroupSpacer,
            ]
        );

        let mut collapsed = Vec::new();
        append_sidebar_group_rows(&mut collapsed, group, &[], true, true);
        assert_eq!(
            collapsed,
            vec![SidebarRow::Header(group), SidebarRow::GroupSpacer]
        );
    }

    #[test]
    fn project_sessions_reveal_older_history_in_thirty_item_batches() {
        let sessions = (1..=36).map(Uuid::from_u128).collect::<Vec<_>>();
        let recent_cutoff = 100;
        let timestamps = sessions
            .iter()
            .enumerate()
            .map(|(index, session_id)| {
                (
                    *session_id,
                    if index == 0 {
                        recent_cutoff
                    } else {
                        recent_cutoff - 1
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        let (initial, show_more) =
            visible_project_sessions(&sessions, &timestamps, recent_cutoff, 0);
        assert_eq!(initial, vec![sessions[0]]);
        assert!(show_more);

        let (first_batch, show_more) = visible_project_sessions(
            &sessions,
            &timestamps,
            recent_cutoff,
            SIDEBAR_PROJECT_REVEAL_BATCH,
        );
        assert_eq!(first_batch, sessions[..31]);
        assert!(show_more);

        let (all_sessions, show_more) = visible_project_sessions(
            &sessions,
            &timestamps,
            recent_cutoff,
            SIDEBAR_PROJECT_REVEAL_BATCH * 2,
        );
        assert_eq!(all_sessions, sessions);
        assert!(!show_more);
    }

    #[test]
    fn sidebar_recency_uses_last_reply_with_creation_fallback() {
        let project_id = Uuid::new_v4();
        let mut renamed_old_session = AgentSession::new(project_id, ProviderKind::Codex);
        renamed_old_session.created_at = 10;
        renamed_old_session.last_reply_at = Some(20);
        renamed_old_session.updated_at = 1_000;

        let mut newer_unanswered_session = AgentSession::new(project_id, ProviderKind::Codex);
        newer_unanswered_session.created_at = 30;
        newer_unanswered_session.last_reply_at = None;
        newer_unanswered_session.updated_at = 30;

        assert_eq!(sidebar_session_timestamp(&renamed_old_session), 20);
        assert_eq!(sidebar_session_timestamp(&newer_unanswered_session), 30);

        let mut sessions = vec![&renamed_old_session, &newer_unanswered_session];
        sort_sidebar_sessions(&mut sessions, SidebarOrdering::Newest);
        assert_eq!(sessions[0].id, newer_unanswered_session.id);

        sort_sidebar_sessions(&mut sessions, SidebarOrdering::Oldest);
        assert_eq!(sessions[0].id, renamed_old_session.id);
    }

    #[test]
    fn project_grouping_preserves_global_group_and_session_order() {
        let first_project = Uuid::from_u128(1);
        let second_project = Uuid::from_u128(2);
        let first = AgentSession::new(first_project, ProviderKind::Codex);
        let second = AgentSession::new(second_project, ProviderKind::Codex);
        let third = AgentSession::new(first_project, ProviderKind::Codex);

        let groups = project_sidebar_groups(&[&second, &first, &third], &HashSet::new());

        assert_eq!(
            groups,
            vec![
                (SidebarGroup::Project(second_project), vec![second.id]),
                (
                    SidebarGroup::Project(first_project),
                    vec![first.id, third.id]
                ),
            ]
        );
    }

    #[test]
    fn projectless_sessions_share_one_trailing_group() {
        let ordinary_project = Uuid::from_u128(1);
        let first_projectless_project = Uuid::from_u128(2);
        let second_projectless_project = Uuid::from_u128(3);
        let first_projectless = AgentSession::new(first_projectless_project, ProviderKind::Codex);
        let ordinary = AgentSession::new(ordinary_project, ProviderKind::Codex);
        let second_projectless = AgentSession::new(second_projectless_project, ProviderKind::Codex);

        let groups = project_sidebar_groups(
            &[&first_projectless, &ordinary, &second_projectless],
            &HashSet::from([first_projectless_project, second_projectless_project]),
        );

        assert_eq!(
            groups,
            vec![
                (SidebarGroup::Project(ordinary_project), vec![ordinary.id]),
                (
                    SidebarGroup::Projectless,
                    vec![first_projectless.id, second_projectless.id]
                ),
            ]
        );
    }

    #[test]
    fn projectless_sidebar_projects_are_paths_under_the_workspace_root() {
        let root = Path::new("/tmp/.waku/projects");
        let projectless = Project {
            id: Uuid::from_u128(1),
            name: "Task".to_owned(),
            path: root.join("2026-08-23/task"),
            created_at: 0,
        };
        let ordinary = Project {
            id: Uuid::from_u128(2),
            name: "Ordinary".to_owned(),
            path: PathBuf::from("/tmp/dev/ordinary"),
            created_at: 0,
        };

        assert!(sidebar_project_is_projectless(&projectless, Some(root)));
        assert!(!sidebar_project_is_projectless(&ordinary, Some(root)));
        assert!(!sidebar_project_is_projectless(&projectless, None));
    }

    #[test]
    fn persisted_worktree_branches_supply_sidebar_labels() {
        let local = SessionWorkspace::Local;
        let planned = SessionWorkspace::NewWorktree {
            base_branch: Some("develop".to_owned()),
        };
        let worktree = SessionWorkspace::Worktree {
            path: PathBuf::from("/tmp/worktree"),
            branch: "feature/sidebar".to_owned(),
        };

        assert_eq!(persisted_sidebar_branch_label(&local), None);
        assert_eq!(persisted_sidebar_branch_label(&planned), Some("develop"));
        assert_eq!(
            persisted_sidebar_branch_label(&worktree),
            Some("feature/sidebar")
        );
    }

    #[test]
    fn pending_session_replaces_sidebar_selection_immediately() {
        let current = Uuid::from_u128(1);
        let pending = Uuid::from_u128(2);

        assert!(!sidebar_session_selected(
            Some(current),
            Some(pending),
            current
        ));
        assert!(sidebar_session_selected(
            Some(current),
            Some(pending),
            pending
        ));
        assert!(sidebar_session_selected(Some(current), None, current));
    }
}

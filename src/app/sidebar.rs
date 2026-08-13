use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};
use gpui::{KeyBinding, actions};

use super::*;

actions!(waku_sidebar, [CancelSessionRename]);

const SESSION_RENAME_PARENT_CONTEXT: &str = "SessionRename";
const SESSION_RENAME_FIELD_CONTEXT: &str = "SessionRename > ComposerInput";

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

/// One section of the sidebar list.
///
/// Pinned and archived bracket the list whatever the grouping mode is; the
/// groups between them are whichever kind the current [`SidebarGrouping`]
/// produces.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SidebarGroup {
    Pinned,
    Date(SessionDateGroup),
    Project(Uuid),
    Folder(Uuid),
    /// Sessions filed under no folder, shown while grouping by folder.
    Unfiled,
    Archived,
}

impl SidebarGroup {
    /// Stable identity for the collapsed set and for element ids. Keys from
    /// other grouping modes survive a switch, so folding is remembered per
    /// group rather than per mode.
    fn key(self) -> String {
        match self {
            Self::Pinned => "pinned".to_owned(),
            Self::Date(group) => format!("date:{}", group.index()),
            Self::Project(id) => format!("project:{id}"),
            Self::Folder(id) => format!("folder:{id}"),
            Self::Unfiled => "unfiled".to_owned(),
            Self::Archived => "archived".to_owned(),
        }
    }

    /// Only a folder earns a glyph: it is a thing the user made and can act
    /// on. The rest are captions, and captions that all look alike are what
    /// make a list scannable.
    fn icon_path(self) -> Option<&'static str> {
        match self {
            Self::Folder(_) => Some("icons/folder.svg"),
            Self::Pinned | Self::Unfiled | Self::Archived | Self::Date(_) | Self::Project(_) => {
                None
            }
        }
    }

    /// What an empty section says while a card is held over the sidebar.
    fn drop_hint(self) -> Option<String> {
        match self {
            Self::Pinned => Some(tr!("sidebar.drop_to_pin")),
            Self::Archived => Some(tr!("sidebar.drop_to_archive")),
            Self::Folder(_) => Some(tr!("sidebar.drop_to_file")),
            Self::Unfiled | Self::Date(_) | Self::Project(_) => None,
        }
    }
}

fn session_group_header(theme: &Theme) -> Div {
    div()
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_tertiary)
}

/// Orders one group's sessions. Recency is the default because the sidebar is
/// a work queue; title order exists for people who name their tasks.
fn sort_sidebar_sessions(sessions: &mut [&AgentSession], sort: SidebarSort) {
    match sort {
        SidebarSort::Recent => {
            sessions.sort_by_key(|session| std::cmp::Reverse(sidebar_session_timestamp(session)));
        }
        SidebarSort::Oldest => sessions.sort_by_key(|session| sidebar_session_timestamp(session)),
        SidebarSort::Title => sessions.sort_by_key(|session| {
            (
                localized_session_title(session).to_lowercase(),
                std::cmp::Reverse(sidebar_session_timestamp(session)),
            )
        }),
        // Never-moved sessions sort after every hand-placed one, and keep
        // recency order among themselves, so one drag does not scramble the
        // rest of the list.
        SidebarSort::Manual => sessions.sort_by_key(|session| {
            (
                session.position.unwrap_or(i64::MAX),
                std::cmp::Reverse(sidebar_session_timestamp(session)),
            )
        }),
        SidebarSort::Activity => sessions.sort_by_key(|session| {
            (
                !session.is_busy(),
                // Within the active block, whoever needs the user first.
                session.status != SessionStatus::Waiting,
                std::cmp::Reverse(sidebar_session_timestamp(session)),
            )
        }),
    }
}

fn append_sidebar_group_rows(
    rows: &mut Vec<SidebarRow>,
    group: SidebarGroup,
    sessions: &[Uuid],
    collapsed: bool,
) {
    if sessions.is_empty() {
        return;
    }

    rows.push(SidebarRow::Header(group));
    if !collapsed {
        rows.extend(
            sessions
                .iter()
                .map(|id| SidebarRow::Session(*id, Some(group))),
        );
    }
    rows.push(SidebarRow::GroupSpacer);
}

/// Where a dragged session would land: the row the insertion line is drawn
/// above, and the group that row belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SidebarDropTarget {
    pub row: usize,
    pub group: SidebarGroup,
}

/// The session being dragged, and the section it started in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SidebarDrag {
    pub session: Uuid,
    /// `None` while grouping is off, where rows belong to no section.
    pub group: Option<SidebarGroup>,
    /// Whether this gesture has already spent its one haptic tick.
    pub ticked: bool,
}

/// The payload a dragged sidebar row carries. Sections that accept a drop
/// listen for this type, so nothing else dragged over the sidebar can file a
/// session by accident.
pub(super) struct DraggedSessionId(pub Uuid);

/// The card that follows the pointer during a drag.
///
/// It is the session row itself — same size, same two lines — rather than a
/// token standing in for it, so what is being moved is never in question. It
/// is lifted off the list with a shadow and drawn slightly transparent so the
/// insertion line underneath stays readable.
pub(super) struct DraggedSession {
    title: SharedString,
    project_name: SharedString,
    time_label: Option<SharedString>,
    width: f32,
}

impl Render for DraggedSession {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        div()
            .w(px(self.width))
            .px(px(8.0))
            .py(px(7.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .rounded(px(7.0))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .opacity(0.95)
            .child(
                div()
                    .w_full()
                    .truncate()
                    .line_height(px(18.0))
                    .text_size(px(13.5))
                    .text_color(theme.text)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .text_size(px(11.5))
                    .line_height(px(15.0))
                    .child(icon("icons/folder.svg", 11.0, theme.text_tertiary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_tertiary)
                            .child(self.project_name.clone()),
                    )
                    .when_some(self.time_label.clone(), |element, label| {
                        element.child(div().flex_none().text_color(theme.text_ghost).child(label))
                    }),
            )
    }
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
const SIDEBAR_CONTROLS_ROW_HEIGHT: f32 = 28.0;
/// Padding around the virtualized list, and therefore the width a dragged
/// card has to match.
const SIDEBAR_LIST_HORIZONTAL_PADDING: f32 = 10.0;
/// Space the list opens above the row a drop is aimed at, tall enough to read
/// as a parting rather than as a nudge.
const SIDEBAR_DROP_INDICATOR_GAP: f32 = 12.0;
/// Height of the dashed well an empty section offers mid-drag. Shorter than a
/// session card, so an empty section never outweighs a full one.
const SIDEBAR_DROP_ZONE_HEIGHT: f32 = 36.0;

/// Opens a sidebar dropdown from the keyboard, anchored to its trigger the way
/// a click would. Deferred because the handle's toggle observers update the
/// entity whose lease this listener holds.
fn sidebar_menu_key_handler(
    handle: &ContextMenuHandle,
    align: MenuAlign,
) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    let handle = handle.clone();
    move |event, window, cx| {
        if !matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
            return;
        }
        let handle = handle.clone();
        window.defer(cx, move |window, cx| {
            crate::ui::menu::toggle_popover(&handle, align, window, cx);
        });
        cx.stop_propagation();
    }
}

fn sidebar_grouping_label(grouping: SidebarGrouping) -> String {
    match grouping {
        SidebarGrouping::Date => tr!("sidebar.group_by_date"),
        SidebarGrouping::Project => tr!("sidebar.group_by_project"),
        SidebarGrouping::Folder => tr!("sidebar.group_by_folder"),
        SidebarGrouping::Flat => tr!("sidebar.group_by_none"),
    }
}

fn sidebar_sort_label(sort: SidebarSort) -> String {
    match sort {
        SidebarSort::Recent => tr!("sidebar.sort_recent"),
        SidebarSort::Oldest => tr!("sidebar.sort_oldest"),
        SidebarSort::Title => tr!("sidebar.sort_title"),
        SidebarSort::Activity => tr!("sidebar.sort_activity"),
        SidebarSort::Manual => tr!("sidebar.sort_manual"),
    }
}

/// The label for one age cutoff. Days are spelled as the periods people
/// actually mean by them rather than as a raw count.
fn sidebar_age_label(days: u32) -> String {
    match days {
        1 => tr!("sidebar.age_day"),
        7 => tr!("sidebar.age_week"),
        30 => tr!("sidebar.age_month"),
        days => tr!("sidebar.age_days", count = days),
    }
}

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
    /// Grouping, sorting and filtering controls, plus the folder and project
    /// actions. Scrolls with history so it never crowds the fixed header.
    Controls,
    /// Group header.
    Header(SidebarGroup),
    /// A started session, and the group it is currently listed under. The
    /// group travels with the row so a drop onto it knows where it landed
    /// without walking back up the list for the nearest header.
    Session(Uuid, Option<SidebarGroup>),
    /// Spacing between groups.
    GroupSpacer,
    /// Shown when every session was filtered out, so an active filter never
    /// reads as an empty history.
    NoMatches,
    /// The landing area an empty section offers mid-drag.
    DropZone(SidebarGroup),
}

impl Waku {
    pub(super) fn window_drag_region(
        &self,
        region: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        region
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
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
            .text_size(px(11.0))
            .line_height(px(0.0))
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

    fn render_sidebar_titlebar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-titlebar")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
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
                    .child(icon(icon_path, 16.0, theme.text_secondary)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
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

    /// The row under search: how the list is organized on the left, and the
    /// actions that add to it on the right.
    fn render_sidebar_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .w_full()
            .h(px(SIDEBAR_CONTROLS_ROW_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(4.0))
            .child(self.render_sidebar_grouping_control(cx))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(self.render_sidebar_filter_control(cx))
                    .child(self.render_sidebar_icon_button(
                        "sidebar-add-project",
                        "icons/folder-new.svg",
                        tr!("sidebar.add_project"),
                        false,
                        cx.listener(|this, _, _, cx| this.add_project(cx)),
                        cx,
                    )),
            )
            .text_color(theme.text_tertiary)
            .into_any_element()
    }

    /// A 20pt square control. The hit area is the square rather than the
    /// glyph, so the target stays comfortable at every icon size.
    fn render_sidebar_icon_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        label: String,
        active: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let label = SharedString::from(label);
        div()
            .id(id)
            .tab_index(0)
            .size(px(20.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .when(active, |element| element.bg(theme.overlay_strong))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(label))
            .child(icon(
                icon_path,
                14.0,
                if active {
                    theme.text_secondary
                } else {
                    theme.text_ghost
                },
            ))
            .on_click(on_click)
    }

    fn render_sidebar_grouping_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let handle = self.menu_handle("sidebar-grouping", cx);
        let grouping = self.state.sidebar_grouping;
        let sort = self.state.sidebar_sort;
        let waku = cx.entity().downgrade();
        dropdown_menu(
            div()
                .id("sidebar-grouping-trigger")
                .tab_index(0)
                .h(px(20.0))
                .min_w_0()
                .px(px(5.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .cursor_default()
                .text_size(px(12.0))
                .text_color(theme.text_tertiary)
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .hover(|element| element.bg(theme.overlay))
                .when(handle.is_open(), |element| element.bg(theme.overlay_strong))
                .child(icon("icons/list.svg", 12.0, theme.text_ghost))
                .child(SharedString::from(sidebar_grouping_label(grouping)))
                .child(icon("icons/chevron-down.svg", 10.0, theme.text_ghost))
                .on_key_down(sidebar_menu_key_handler(&handle, MenuAlign::BelowLeft)),
            "sidebar-grouping-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = vec![MenuItem::Header(SharedString::from(tr!(
                    "sidebar.group_by"
                )))];
                items.extend(SidebarGrouping::ALL.into_iter().map(|option| {
                    let waku = waku.clone();
                    MenuItem::new(sidebar_grouping_label(option), move |_, cx| {
                        let _ = waku.update(cx, |waku, cx| waku.set_sidebar_grouping(option, cx));
                    })
                    .selected(option == grouping)
                }));
                items.push(MenuItem::Separator);
                items.push(MenuItem::Header(SharedString::from(tr!("sidebar.sort_by"))));
                items.extend(SidebarSort::ALL.into_iter().map(|option| {
                    let waku = waku.clone();
                    MenuItem::new(sidebar_sort_label(option), move |_, cx| {
                        let _ = waku.update(cx, |waku, cx| waku.set_sidebar_sort(option, cx));
                    })
                    .selected(option == sort)
                }));
                items.push(MenuItem::Separator);
                let folder_waku = waku.clone();
                items.push(MenuItem::new(
                    tr!("sidebar.new_folder"),
                    move |window, cx| {
                        let _ =
                            folder_waku.update(cx, |waku, cx| waku.create_folder(None, window, cx));
                    },
                ));
                items
            },
        )
    }

    fn render_sidebar_filter_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let handle = self.menu_handle("sidebar-filter", cx);
        let filter = self.state.sidebar_filter.clone();
        // The icon alone would encode "filtered" in shape only, so an active
        // filter also names itself in the tooltip.
        let active = filter.is_narrowed();
        let label = if active {
            tr!("sidebar.filter_active")
        } else {
            tr!("sidebar.filter")
        };
        let waku = cx.entity().downgrade();
        dropdown_menu(
            self.render_sidebar_icon_button(
                "sidebar-filter-trigger",
                "icons/filter.svg",
                label,
                active || handle.is_open(),
                |_, _, _| {},
                cx,
            )
            .on_key_down(sidebar_menu_key_handler(&handle, MenuAlign::BelowRight)),
            "sidebar-filter-menu",
            &handle,
            MenuAlign::BelowRight,
            move |cx| {
                // Walked when the menu opens rather than on every frame the
                // control is on screen.
                let providers = waku
                    .upgrade()
                    .map(|waku| waku.read(cx).available_sidebar_providers())
                    .unwrap_or_default();
                let mut items = vec![MenuItem::Header(SharedString::from(tr!(
                    "sidebar.provider"
                )))];
                let all_waku = waku.clone();
                items.push(
                    MenuItem::new(tr!("sidebar.all_providers"), move |_, cx| {
                        let _ = all_waku.update(cx, |waku, cx| {
                            waku.update_sidebar_filter(|filter| filter.provider = None, cx);
                        });
                    })
                    .selected(filter.provider.is_none()),
                );
                items.extend(providers.iter().copied().map(|provider| {
                    let waku = waku.clone();
                    MenuItem::new(provider.display_name(), move |_, cx| {
                        let _ = waku.update(cx, |waku, cx| {
                            waku.update_sidebar_filter(
                                |filter| {
                                    // Choosing the provider already selected
                                    // clears it, so the menu is its own undo.
                                    filter.provider =
                                        (filter.provider != Some(provider)).then_some(provider);
                                },
                                cx,
                            );
                        });
                    })
                    .selected(filter.provider == Some(provider))
                }));
                items.push(MenuItem::Separator);
                items.push(MenuItem::Header(SharedString::from(tr!(
                    "sidebar.last_active"
                ))));
                let any_age_waku = waku.clone();
                items.push(
                    MenuItem::new(tr!("sidebar.any_age"), move |_, cx| {
                        let _ = any_age_waku.update(cx, |waku, cx| {
                            waku.update_sidebar_filter(|filter| filter.max_age_days = None, cx);
                        });
                    })
                    .selected(filter.max_age_days.is_none()),
                );
                items.extend(SidebarFilter::AGE_OPTIONS.into_iter().map(|days| {
                    let waku = waku.clone();
                    MenuItem::new(sidebar_age_label(days), move |_, cx| {
                        let _ = waku.update(cx, |waku, cx| {
                            waku.update_sidebar_filter(
                                |filter| {
                                    filter.max_age_days =
                                        (filter.max_age_days != Some(days)).then_some(days);
                                },
                                cx,
                            );
                        });
                    })
                    .selected(filter.max_age_days == Some(days))
                }));
                items.push(MenuItem::Separator);
                let active_waku = waku.clone();
                items.push(
                    MenuItem::new(tr!("sidebar.active_only"), move |_, cx| {
                        let _ = active_waku.update(cx, |waku, cx| {
                            waku.update_sidebar_filter(
                                |filter| filter.active_only = !filter.active_only,
                                cx,
                            );
                        });
                    })
                    .selected(filter.active_only),
                );
                let archived_waku = waku.clone();
                items.push(
                    MenuItem::new(tr!("sidebar.show_archived"), move |_, cx| {
                        let _ = archived_waku.update(cx, |waku, cx| {
                            waku.update_sidebar_filter(
                                |filter| filter.show_archived = !filter.show_archived,
                                cx,
                            );
                        });
                    })
                    .selected(filter.show_archived),
                );
                items
            },
        )
    }

    /// Providers that actually appear in the history. Offering a filter that
    /// can only ever empty the list would be a dead control.
    fn available_sidebar_providers(&self) -> Vec<ProviderKind> {
        let mut providers = Vec::new();
        for session in self
            .state
            .sessions
            .iter()
            .filter(|session| session.has_started() && session.is_task())
        {
            if !providers.contains(&session.provider) {
                providers.push(session.provider);
            }
        }
        providers
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
        let foreground = theme.on_accent;
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
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .when(available, |button| {
                button
                    .hover(|style| style.opacity(0.92))
                    .focus_visible(|style| style.border_1().border_color(foreground))
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
            let indicator = icon("icons/loader-circle.svg", 14.0, foreground)
                .with_animation(
                    "sidebar-updater-spinner",
                    Animation::new(Duration::from_millis(900))
                        .repeat()
                        .with_easing(gpui::linear),
                    |icon, delta| {
                        icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                            delta,
                        )))
                    },
                )
                .into_any_element();
            return Some(
                button
                    .tooltip(Tooltip::text(
                        if status == crate::updater::UpdateStatus::Checking {
                            tr!("updater.checking")
                        } else {
                            tr!("updater.updating")
                        },
                    ))
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

    pub(super) fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let is_resizing = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == PanelResizeTarget::Sidebar);

        // Building the row snapshot is cheap (a few bytes per session); the
        // heavy element construction happens only for rows the list can see.
        let rows = Rc::new(self.sidebar_rows(Local::now().date_naive()));
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
            .child(self.render_sidebar_titlebar(cx))
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
                    // A drag released over empty list space, or over a group
                    // that cannot take it, leaves everything where it was.
                    .on_drop(cx.listener(|this, _: &DraggedSessionId, _, cx| {
                        this.cancel_sidebar_drop(cx);
                    }))
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.cancel_sidebar_drop(cx)),
                    )
                    .child(
                        div()
                            .px(px(SIDEBAR_LIST_HORIZONTAL_PADDING))
                            .size_full()
                            .child(
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

    /// Snapshot the session history as a flat list of lightweight rows: the
    /// controls, a pinned section, the groups the current mode produces, then
    /// archived sessions when they are shown.
    fn sidebar_rows(&self, today: NaiveDate) -> Vec<SidebarRow> {
        let now = unix_time();
        let mut sessions = self
            .state
            .sessions
            .iter()
            // Subagents are conversations too, but they belong to the task
            // that launched them, not beside it.
            .filter(|session| session.has_started() && session.is_task())
            .filter(|session| self.state.sidebar_filter.matches(session, now))
            .collect::<Vec<_>>();
        sort_sidebar_sessions(&mut sessions, self.state.sidebar_sort);

        let mut rows = vec![SidebarRow::Search, SidebarRow::Controls];
        let session_count = sessions.len();
        // Pinned and archived are hidden while empty, which is right at rest
        // and wrong mid-drag: they are the two destinations every session has,
        // so a drag has to be able to see them even on a sidebar where nothing
        // has been pinned or archived yet.
        let dragging = self.sidebar_drag.is_some();

        // Pinned sits above every group; archiving a pinned session still
        // files it away, so the two sections never claim the same row.
        let pinned = sessions
            .iter()
            .filter(|session| session.pinned && !session.archived)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        self.append_group_with_header(&mut rows, SidebarGroup::Pinned, &pinned, dragging);

        let filed = sessions
            .iter()
            .copied()
            .filter(|session| !session.pinned && !session.archived)
            .collect::<Vec<_>>();
        match self.state.sidebar_grouping {
            SidebarGrouping::Date => {
                let mut buckets: [Vec<Uuid>; 6] = std::array::from_fn(|_| Vec::new());
                for session in &filed {
                    buckets[session_date_group(sidebar_session_timestamp(session), today).index()]
                        .push(session.id);
                }
                for group in SessionDateGroup::ALL {
                    let bucket = &buckets[group.index()];
                    self.append_group(&mut rows, SidebarGroup::Date(group), bucket);
                }
            }
            SidebarGrouping::Project => {
                // One pass fills every bucket. Scanning the sessions once per
                // project would make an ordinary frame cost projects × history.
                let known = self
                    .state
                    .projects
                    .iter()
                    .map(|project| project.id)
                    .collect::<HashSet<_>>();
                let mut buckets: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
                let mut orphaned = Vec::new();
                for session in &filed {
                    if known.contains(&session.project_id) {
                        buckets
                            .entry(session.project_id)
                            .or_default()
                            .push(session.id);
                    } else {
                        // A session whose project was removed still belongs
                        // somewhere.
                        orphaned.push(session.id);
                    }
                }
                // Project order comes from the sidebar's own project list, so
                // groups keep the order the user arranged them in.
                for project in &self.state.projects {
                    let bucket = buckets.remove(&project.id).unwrap_or_default();
                    self.append_group(&mut rows, SidebarGroup::Project(project.id), &bucket);
                }
                self.append_group(&mut rows, SidebarGroup::Unfiled, &orphaned);
            }
            SidebarGrouping::Folder => {
                let known = self
                    .state
                    .folders
                    .iter()
                    .map(|folder| folder.id)
                    .collect::<HashSet<_>>();
                let mut buckets: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
                let mut unfiled = Vec::new();
                for session in &filed {
                    match session.folder_id.filter(|id| known.contains(id)) {
                        Some(folder_id) => {
                            buckets.entry(folder_id).or_default().push(session.id);
                        }
                        None => unfiled.push(session.id),
                    }
                }
                for folder in &self.state.folders {
                    let bucket = buckets.remove(&folder.id).unwrap_or_default();
                    // Unlike the other groups, an empty folder still shows its
                    // header — otherwise a folder just made would vanish.
                    if bucket.is_empty() {
                        rows.push(SidebarRow::Header(SidebarGroup::Folder(folder.id)));
                        rows.push(SidebarRow::GroupSpacer);
                    } else {
                        self.append_group(&mut rows, SidebarGroup::Folder(folder.id), &bucket);
                    }
                }
                self.append_group(&mut rows, SidebarGroup::Unfiled, &unfiled);
            }
            SidebarGrouping::Flat => {
                // No headers in this mode, so these rows belong to no group and
                // cannot be a drop target.
                rows.extend(
                    filed
                        .iter()
                        .map(|session| SidebarRow::Session(session.id, None)),
                );
                if !filed.is_empty() {
                    rows.push(SidebarRow::GroupSpacer);
                }
            }
        }

        let archived = sessions
            .iter()
            .filter(|session| session.archived)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        self.append_group_with_header(&mut rows, SidebarGroup::Archived, &archived, dragging);

        if session_count == 0 && self.has_sidebar_history() {
            rows.push(SidebarRow::NoMatches);
        }
        rows
    }

    fn append_group(&self, rows: &mut Vec<SidebarRow>, group: SidebarGroup, sessions: &[Uuid]) {
        append_sidebar_group_rows(rows, group, sessions, self.sidebar_group_collapsed(group));
    }

    /// [`Self::append_group`], but `keep_header` emits the header even for an
    /// empty group — which is what turns it into a visible drop zone.
    fn append_group_with_header(
        &self,
        rows: &mut Vec<SidebarRow>,
        group: SidebarGroup,
        sessions: &[Uuid],
        keep_header: bool,
    ) {
        if sessions.is_empty() && keep_header {
            rows.push(SidebarRow::Header(group));
            rows.push(SidebarRow::DropZone(group));
            rows.push(SidebarRow::GroupSpacer);
            return;
        }
        self.append_group(rows, group, sessions);
    }

    fn sidebar_group_collapsed(&self, group: SidebarGroup) -> bool {
        self.sidebar_collapsed_groups.contains(&group.key())
    }

    /// Whether anything would be listed with no filter, so an empty sidebar can
    /// tell "nothing yet" apart from "nothing matches".
    fn has_sidebar_history(&self) -> bool {
        self.state
            .sessions
            .iter()
            .any(|session| session.has_started() && session.is_task())
    }

    fn sidebar_group_label(&self, group: SidebarGroup) -> String {
        match group {
            SidebarGroup::Pinned => tr!("sidebar.pinned"),
            SidebarGroup::Date(group) => group.label(),
            SidebarGroup::Project(id) => self
                .state
                .projects
                .iter()
                .find(|project| project.id == id)
                .map(Project::display_name)
                .unwrap_or_else(|| tr!("sidebar.unknown_project")),
            SidebarGroup::Folder(id) => self
                .state
                .folders
                .iter()
                .find(|folder| folder.id == id)
                .map(|folder| folder.name.clone())
                .unwrap_or_else(|| tr!("sidebar.unfiled")),
            SidebarGroup::Unfiled => tr!("sidebar.unfiled"),
            SidebarGroup::Archived => tr!("sidebar.archived"),
        }
    }

    /// Folded groups are app state, not session state, so they ride along with
    /// the sidebar's width and visibility rather than any session row.
    fn persist_sidebar_layout(&mut self) {
        // Sorted so an unordered set cannot make an unchanged layout look
        // dirty and rewrite the state file on every fold.
        let mut collapsed = self
            .sidebar_collapsed_groups
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        collapsed.sort();
        self.state.sidebar_collapsed_groups = collapsed;
        self.save();
    }

    fn set_sidebar_grouping(&mut self, grouping: SidebarGrouping, cx: &mut Context<Self>) {
        if self.state.sidebar_grouping == grouping {
            return;
        }
        self.state.sidebar_grouping = grouping;
        self.save();
        cx.notify();
    }

    fn set_sidebar_sort(&mut self, sort: SidebarSort, cx: &mut Context<Self>) {
        if self.state.sidebar_sort == sort {
            return;
        }
        self.state.sidebar_sort = sort;
        self.save();
        cx.notify();
    }

    fn update_sidebar_filter(
        &mut self,
        update: impl FnOnce(&mut SidebarFilter),
        cx: &mut Context<Self>,
    ) {
        let mut filter = self.state.sidebar_filter.clone();
        update(&mut filter);
        if filter == self.state.sidebar_filter {
            return;
        }
        self.state.sidebar_filter = filter;
        self.save();
        cx.notify();
    }

    fn set_session_pinned(&mut self, session_id: Uuid, pinned: bool, cx: &mut Context<Self>) {
        let Some(session) = self.state.session_mut(session_id) else {
            return;
        };
        if session.pinned == pinned {
            return;
        }
        session.pinned = pinned;
        // Pinning a session brings it back into view, so it cannot stay filed
        // away in the archive at the same time.
        if pinned {
            session.archived = false;
        }
        self.save();
        cx.notify();
    }

    fn set_session_archived(&mut self, session_id: Uuid, archived: bool, cx: &mut Context<Self>) {
        let Some(session) = self.state.session_mut(session_id) else {
            return;
        };
        if session.archived == archived {
            return;
        }
        session.archived = archived;
        if archived {
            session.pinned = false;
        }
        self.save();
        cx.notify();
    }

    fn move_session_to_folder(
        &mut self,
        session_id: Uuid,
        folder_id: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.state.session_mut(session_id) else {
            return;
        };
        if session.folder_id == folder_id {
            return;
        }
        session.folder_id = folder_id;
        self.save();
        cx.notify();
    }

    /// Whether the drag in progress can land in `group`.
    ///
    /// Every section takes a drop, because a drop always means at least "put
    /// it here" — the order is the point. The two derived groupings are the
    /// exception in one direction only: a calendar period and a project are
    /// facts about a session, not places to put one, so they accept a reorder
    /// within themselves and refuse anything arriving from elsewhere.
    fn can_drop_into(&self, drag: SidebarDrag, group: SidebarGroup) -> bool {
        match group {
            SidebarGroup::Folder(_)
            | SidebarGroup::Unfiled
            | SidebarGroup::Pinned
            | SidebarGroup::Archived => true,
            SidebarGroup::Date(_) | SidebarGroup::Project(_) => drag.group == Some(group),
        }
    }

    /// Opens the drop zones as the card lifts.
    fn begin_sidebar_drag(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if self
            .sidebar_drag
            .is_some_and(|drag| drag.session == session_id)
        {
            return;
        }
        // The row snapshot already knows which section the card came from;
        // remembering it now means the derived groups can tell a reorder from
        // an arrival without re-deriving anything per frame.
        let group = self
            .sidebar_row_cache
            .borrow()
            .iter()
            .find_map(|row| match row {
                SidebarRow::Session(id, group) if *id == session_id => Some(*group),
                _ => None,
            })
            .flatten();
        self.sidebar_drag = Some(SidebarDrag {
            session: session_id,
            group,
            ticked: false,
        });
        self.sidebar_drop_target = None;
        cx.notify();
    }

    /// Moves the insertion line, ticking the trackpad when it lands somewhere
    /// new. The tick is the same `Alignment` pattern macOS uses for snapping,
    /// so a drag feels like it is catching on each target rather than sliding
    /// over an inert list.
    fn set_sidebar_drop_target(
        &mut self,
        target: Option<SidebarDropTarget>,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_drop_target == target {
            return;
        }
        // One tap per gesture, at the first moment the drag has somewhere to
        // go. Ticking per row crossed, or again on the drop, turns a two
        // second drag into a burst — the tap is meant to confirm that this is
        // a real target, and it only has to say that once.
        let entered_new_group = target.map(|target| target.group)
            != self.sidebar_drop_target.map(|target| target.group);
        if target.is_some()
            && entered_new_group
            && let Some(drag) = self.sidebar_drag.as_mut()
            && !drag.ticked
        {
            drag.ticked = true;
            crate::platform::haptic_alignment_tick();
        }
        self.sidebar_drop_target = target;
        cx.notify();
    }

    /// Called while a session is dragged over a row: the pointer's half of the
    /// row decides whether the line sits above it or below it.
    fn sidebar_drag_over_row(
        &mut self,
        group: Option<SidebarGroup>,
        index: usize,
        event: &gpui::DragMoveEvent<DraggedSessionId>,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.sidebar_drag else {
            return;
        };
        let target = group
            .filter(|group| self.can_drop_into(drag, *group))
            .map(|group| {
                let midpoint = event.bounds.origin.y + event.bounds.size.height / 2.0;
                let row = if event.event.position.y < midpoint {
                    index
                } else {
                    index + 1
                };
                SidebarDropTarget { row, group }
            });
        self.set_sidebar_drop_target(target, cx);
    }

    /// Lands the drag: files the session where it was dropped, then puts it at
    /// the exact position the insertion line promised.
    fn commit_sidebar_drop(
        &mut self,
        session_id: Uuid,
        group: SidebarGroup,
        cx: &mut Context<Self>,
    ) {
        let target = self.sidebar_drop_target.take();
        let drag = self.sidebar_drag.take();
        let Some(drag) = drag.filter(|drag| self.can_drop_into(*drag, group)) else {
            cx.notify();
            return;
        };
        // Silent if the snap already spoke for this gesture — which it has,
        // unless the card never left the section it started in.
        if !drag.ticked {
            crate::platform::haptic_drop_tick();
        }
        self.drop_session_on_group(session_id, group, cx);
        // A drop with no line — onto a header, or onto a collapsed section —
        // means the top of that group.
        let row = target.filter(|target| target.group == group).map(|t| t.row);
        self.place_session_in_group(session_id, group, row, cx);
    }

    /// Writes the hand-picked order the drop implies.
    ///
    /// The whole group is renumbered from the order it is currently showing,
    /// with the dropped session spliced in. Renumbering a group beats trying
    /// to squeeze a value between two neighbours: it cannot run out of room,
    /// and the numbers stay readable in the database.
    fn place_session_in_group(
        &mut self,
        session_id: Uuid,
        group: SidebarGroup,
        row: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let rows = self.sidebar_row_cache.borrow().clone();
        let mut ordered = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Session(id, Some(row_group)) if *row_group == group => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        // How many of this group's rows sit above the line.
        let mut insert_at = row.map_or(0, |row| {
            rows.iter()
                .take(row)
                .filter(|candidate| {
                    matches!(
                        candidate,
                        SidebarRow::Session(_, Some(row_group)) if *row_group == group
                    )
                })
                .count()
        });
        // Pulling the session out from above the line shifts the line up one.
        if let Some(from) = ordered.iter().position(|id| *id == session_id) {
            ordered.remove(from);
            if from < insert_at {
                insert_at -= 1;
            }
        }
        ordered.insert(insert_at.min(ordered.len()), session_id);

        for (position, id) in ordered.into_iter().enumerate() {
            let position = position as i64;
            if let Some(session) = self.state.session_mut(id)
                && session.position != Some(position)
            {
                session.position = Some(position);
            }
        }
        // A hand-placed order is only visible under the sort that honors it,
        // so the drop selects it rather than silently doing nothing.
        self.state.sidebar_sort = SidebarSort::Manual;
        self.save();
        cx.notify();
    }

    /// A drag that ends anywhere other than a target leaves the list as it was.
    pub(super) fn cancel_sidebar_drop(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_drop_target.take().is_some() || self.sidebar_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Drops onto a section mean the obvious thing for that section: file into
    /// a folder, unfile, pin, or archive.
    fn drop_session_on_group(
        &mut self,
        session_id: Uuid,
        group: SidebarGroup,
        cx: &mut Context<Self>,
    ) {
        match group {
            SidebarGroup::Folder(folder_id) => {
                self.move_session_to_folder(session_id, Some(folder_id), cx);
            }
            SidebarGroup::Unfiled => self.move_session_to_folder(session_id, None, cx),
            SidebarGroup::Pinned => self.set_session_pinned(session_id, true, cx),
            SidebarGroup::Archived => self.set_session_archived(session_id, true, cx),
            SidebarGroup::Date(_) | SidebarGroup::Project(_) => {}
        }
    }

    /// Creates a folder and immediately opens its inline rename, so naming one
    /// is a single gesture rather than a create-then-rename round trip.
    fn create_folder(
        &mut self,
        session_id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = SessionFolder::new(
            tr!("sidebar.new_folder_name"),
            self.state.folders.len() as u32,
        );
        let folder_id = folder.id;
        self.state.folders.push(folder);
        if let Some(session_id) = session_id
            && let Some(session) = self.state.session_mut(session_id)
        {
            session.folder_id = Some(folder_id);
        }
        // A folder is only visible in folder grouping, so switch to it rather
        // than creating something the user cannot see.
        self.state.sidebar_grouping = SidebarGrouping::Folder;
        self.sidebar_collapsed_groups
            .remove(&SidebarGroup::Folder(folder_id).key());
        self.save();
        self.begin_folder_rename(folder_id, window, cx);
    }

    fn remove_folder(&mut self, folder_id: Uuid, cx: &mut Context<Self>) {
        if !self
            .state
            .folders
            .iter()
            .any(|folder| folder.id == folder_id)
        {
            return;
        }
        self.state.folders.retain(|folder| folder.id != folder_id);
        // Its sessions are unfiled rather than removed; a folder is a label,
        // not a container.
        let filed = self
            .state
            .sessions
            .iter()
            .filter(|session| session.folder_id == Some(folder_id))
            .map(|session| session.id)
            .collect::<Vec<_>>();
        for session_id in filed {
            if let Some(session) = self.state.session_mut(session_id) {
                session.folder_id = None;
            }
        }
        if self.folder_rename == Some(folder_id) {
            self.folder_rename = None;
        }
        self.sidebar_collapsed_groups
            .remove(&SidebarGroup::Folder(folder_id).key());
        self.persist_sidebar_layout();
        cx.notify();
    }

    fn begin_folder_rename(
        &mut self,
        folder_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self
            .state
            .folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .map(|folder| folder.name.clone())
        else {
            return;
        };
        self.folder_rename = Some(folder_id);
        self.folder_rename_input.update(cx, |input, cx| {
            input.set_content(name, cx);
            input.select_all_text(cx);
        });
        let focus = self.folder_rename_input.read(cx).focus();
        window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        cx.notify();
    }

    pub(super) fn commit_folder_rename(&mut self, cx: &mut Context<Self>) {
        let Some(folder_id) = self.folder_rename.take() else {
            return;
        };
        let name = self
            .folder_rename_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        if !name.is_empty()
            && let Some(folder) = self
                .state
                .folders
                .iter_mut()
                .find(|folder| folder.id == folder_id)
            && folder.name != name
        {
            folder.name = name;
            self.save();
        }
        cx.notify();
    }

    fn cancel_folder_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder_rename.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
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
            SidebarRow::Controls => self.render_sidebar_controls(cx),
            SidebarRow::Header(group) => self.render_sidebar_group_header(group, index, cx),
            SidebarRow::Session(session_id, group) => self
                .render_sidebar_session_item(session_id, group, index, cx)
                .into_any_element(),
            // The spacer is also the position after a group's last row, so it
            // carries the insertion line when a drop is aimed at the end.
            SidebarRow::GroupSpacer => div()
                .w_full()
                .h(px(10.0))
                .child(self.sidebar_insertion_gap(index, cx))
                .into_any_element(),
            SidebarRow::NoMatches => self.render_sidebar_no_matches(cx),
            SidebarRow::DropZone(group) => self.render_sidebar_drop_zone(group, cx),
        }
    }

    /// The landing area an empty section shows while a card is in the air.
    ///
    /// A dashed well that names what dropping here does, rather than an outline
    /// drawn around a caption — a section with nothing in it has no shape of
    /// its own to decorate, so it has to grow one.
    fn render_sidebar_drop_zone(&self, group: SidebarGroup, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(hint) = group.drop_hint() else {
            return div().into_any_element();
        };
        div()
            .id(SharedString::from(format!(
                "sidebar-drop-zone-{}",
                group.key()
            )))
            .w_full()
            .h(px(SIDEBAR_DROP_ZONE_HEIGHT))
            .mb(px(2.0))
            .rounded(px(7.0))
            .border_1()
            .border_dashed()
            .border_color(theme.border)
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.0))
            .text_color(theme.text_tertiary)
            .child(SharedString::from(hint))
            .drag_over::<DraggedSessionId>(move |style, _, _, cx| {
                let theme = Theme::current(cx);
                style
                    .border_color(theme.accent)
                    .bg(theme.accent.opacity(0.08))
            })
            .on_drag_move(cx.listener(
                move |this, _: &gpui::DragMoveEvent<DraggedSessionId>, _, cx| {
                    // The well is the whole target, so there is no line to
                    // place inside it — only the section it belongs to.
                    let target = this
                        .sidebar_drag
                        .filter(|drag| this.can_drop_into(*drag, group))
                        .map(|_| SidebarDropTarget {
                            row: usize::MAX,
                            group,
                        });
                    this.set_sidebar_drop_target(target, cx);
                },
            ))
            .on_drop(cx.listener(move |this, dragged: &DraggedSessionId, _, cx| {
                this.commit_sidebar_drop(dragged.0, group, cx);
            }))
            .into_any_element()
    }

    fn render_sidebar_no_matches(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .w_full()
            .px(px(8.0))
            .py(px(10.0))
            .text_size(px(12.5))
            .text_color(theme.text_tertiary)
            .child(tr!("sidebar.no_matching_sessions"))
            .into_any_element()
    }

    fn render_sidebar_group_header(
        &self,
        group: SidebarGroup,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let collapsed = self.sidebar_group_collapsed(group);
        let key = group.key();
        let renaming_folder =
            matches!(group, SidebarGroup::Folder(id) if self.folder_rename == Some(id));
        let group_name = SharedString::from(format!("sidebar-group-header-{key}"));
        let chevron = icon("icons/chevron-down.svg", 11.0, theme.text_ghost)
            .when(collapsed, |icon| {
                icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
            })
            .invisible()
            .group_hover(group_name.clone(), |icon| icon.visible());

        let label: AnyElement = if renaming_folder {
            div()
                .id(SharedString::from(format!("folder-rename-field-{key}")))
                .key_context(SESSION_RENAME_PARENT_CONTEXT)
                .on_action(cx.listener(|this, _: &CancelSessionRename, window, cx| {
                    this.cancel_folder_rename(window, cx);
                }))
                .h(px(20.0))
                .flex_1()
                .min_w_0()
                .px(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.accent)
                .bg(theme.inset)
                .flex()
                .items_center()
                .text_color(theme.text)
                .child(self.folder_rename_input.clone())
                .into_any_element()
        } else {
            div()
                .min_w_0()
                .truncate()
                .child(SharedString::from(self.sidebar_group_label(group)))
                .into_any_element()
        };

        let toggle = div()
            .id(SharedString::from(format!("sidebar-group-toggle-{key}")))
            .tab_index(0)
            .h(px(22.0))
            .min_w_0()
            .flex_1()
            .rounded(px(4.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .when_some(group.icon_path(), |element, path| {
                element.child(icon(path, 11.0, theme.text_ghost))
            })
            .child(label)
            .when(!renaming_folder, |element| {
                element
                    .child(chevron)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_sidebar_group(group, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
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
                            "f2" => {
                                if let SidebarGroup::Folder(id) = group {
                                    this.begin_folder_rename(id, window, cx);
                                    cx.stop_propagation();
                                }
                            }
                            _ => {}
                        }
                    }))
            });

        let header = session_group_header(&theme)
            .id(SharedString::from(format!("sidebar-group-{key}")))
            .group(group_name)
            .w_full()
            .gap(px(4.0))
            .child(toggle)
            // Dropping a session onto a section is how it is filed by pointer;
            // the same moves are on the row's context menu for the keyboard.
            // The whole header is the target, not just its label.
            .drag_over::<DraggedSessionId>(move |style, _, _, cx| {
                style
                    .bg(Theme::current(cx).sidebar_item_background)
                    .rounded(px(6.0))
            })
            .on_drag_move(cx.listener(
                move |this, _: &gpui::DragMoveEvent<DraggedSessionId>, _, cx| {
                    // Landing on a header means the top of its group.
                    let target = this
                        .sidebar_drag
                        .filter(|drag| this.can_drop_into(*drag, group))
                        .map(|_| SidebarDropTarget {
                            row: index + 1,
                            group,
                        });
                    this.set_sidebar_drop_target(target, cx);
                },
            ))
            .on_drop(cx.listener(move |this, dragged: &DraggedSessionId, _, cx| {
                this.commit_sidebar_drop(dragged.0, group, cx);
            }));

        if renaming_folder {
            return header
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.commit_folder_rename(cx);
                }))
                .into_any_element();
        }

        match group {
            SidebarGroup::Folder(folder_id) => {
                let waku = cx.entity().downgrade();
                let menu = self.menu_handle(format!("folder-{folder_id}"), cx);
                context_menu(
                    div().w_full().child(header),
                    SharedString::from(format!("folder-menu-{folder_id}")),
                    &menu,
                    move |_| {
                        let rename_waku = waku.clone();
                        let remove_waku = waku.clone();
                        vec![
                            MenuItem::new(tr!("common.rename"), move |window, cx| {
                                let _ = rename_waku.update(cx, |waku, cx| {
                                    waku.begin_folder_rename(folder_id, window, cx);
                                });
                            }),
                            MenuItem::Separator,
                            MenuItem::new(tr!("sidebar.delete_folder"), move |_, cx| {
                                let _ = remove_waku
                                    .update(cx, |waku, cx| waku.remove_folder(folder_id, cx));
                            }),
                        ]
                    },
                )
            }
            _ => header.into_any_element(),
        }
    }

    fn toggle_sidebar_group(&mut self, group: SidebarGroup, cx: &mut Context<Self>) {
        self.set_sidebar_group_collapsed(group, !self.sidebar_group_collapsed(group), cx);
    }

    fn set_sidebar_group_collapsed(
        &mut self,
        group: SidebarGroup,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let key = group.key();
        let changed = if collapsed {
            self.sidebar_collapsed_groups.insert(key)
        } else {
            self.sidebar_collapsed_groups.remove(&key)
        };
        if changed {
            self.persist_sidebar_layout();
            cx.notify();
        }
    }

    fn begin_session_rename(
        &mut self,
        session_id: Uuid,
        in_header: bool,
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
        self.session_rename_in_header = in_header;
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
        self.session_rename_in_header = false;
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
        self.session_rename_in_header = false;
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn render_sidebar_session_item(
        &self,
        session_id: Uuid,
        group: Option<SidebarGroup>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return div().into_any_element();
        };
        let selected = self.state.selected_session == Some(session_id);
        let working = matches!(
            session.status,
            SessionStatus::Connecting | SessionStatus::Working
        );
        let pinned = session.pinned;
        let archived = session.archived;
        let folder_id = session.folder_id;
        let drag_title = SharedString::from(localized_session_title(session));
        let drag_time_label = session_time_label(session, unix_time()).map(SharedString::from);
        // The preview matches the row it came from, so it has to know how wide
        // the sidebar is right now rather than assume a default.
        let drag_width = self.sidebar_width - SIDEBAR_LIST_HORIZONTAL_PADDING * 2.0;
        let folders = self
            .state
            .folders
            .iter()
            .map(|folder| (folder.id, SharedString::from(folder.name.clone())))
            .collect::<Vec<_>>();
        let project_name = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .map(Project::display_name)
            .unwrap_or_else(|| tr!("sidebar.unknown_project"));
        let drag_project_name = SharedString::from(project_name.clone());
        let rename_input = (self.session_rename == Some(session_id)
            && !self.session_rename_in_header)
            .then(|| self.session_rename_input.clone());
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
                .text_size(px(13.5))
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
                .text_size(px(13.5))
                // Archived tasks stay legible but visibly set aside, so the
                // section reads as storage rather than as more of the list.
                .text_color(if archived {
                    theme.text_secondary
                } else {
                    theme.text
                })
                .child(SharedString::from(localized_session_title(session)))
                .into_any_element()
        };
        let waku = cx.entity().downgrade();
        let drag_waku = waku.clone();
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
            .px(px(8.0))
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
                    .line_height(px(18.0))
                    .child(title)
                    .when(working, |element| {
                        element.child(
                            icon(
                                "icons/loader-circle.svg",
                                12.0,
                                status_color(&theme, session.status),
                            )
                            .with_animation(
                                SharedString::from(format!("session-spinner-{session_id}")),
                                Animation::new(Duration::from_millis(900))
                                    .repeat()
                                    .with_easing(gpui::linear),
                                |icon, delta| {
                                    icon.with_transformation(gpui::Transformation::rotate(
                                        gpui::percentage(delta),
                                    ))
                                },
                            ),
                        )
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
                    .text_size(px(11.5))
                    .line_height(px(15.0))
                    .child(icon("icons/folder.svg", 11.0, theme.text_tertiary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(project_name)),
                    )
                    .when_some(
                        session_time_label(session, unix_time()),
                        |element, label| {
                            element.child(
                                div()
                                    .flex_none()
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
                    .on_drag(DraggedSessionId(session_id), move |_, _, _, cx| {
                        // Announcing the drag here rather than on mouse-down
                        // means the drop zones appear exactly when the card
                        // lifts, not on every click of a row.
                        let _ = drag_waku.update(cx, |this, cx| {
                            this.begin_sidebar_drag(session_id, cx);
                        });
                        let title = drag_title.clone();
                        let project_name = drag_project_name.clone();
                        let time_label = drag_time_label.clone();
                        cx.new(|_| DraggedSession {
                            title,
                            project_name,
                            time_label,
                            width: drag_width,
                        })
                    })
                    // The whole card is the drop target, not the header strip
                    // above it, so a drop lands wherever the pointer already is.
                    .on_drag_move(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<DraggedSessionId>, _, cx| {
                            this.sidebar_drag_over_row(group, index, event, cx);
                        },
                    ))
                    .on_drop(cx.listener(
                        move |this, dragged: &DraggedSessionId, _, cx| match group {
                            Some(group) => this.commit_sidebar_drop(dragged.0, group, cx),
                            None => this.cancel_sidebar_drop(cx),
                        },
                    ))
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
                    let pin_waku = waku.clone();
                    let archive_waku = waku.clone();
                    let unfile_waku = waku.clone();
                    let new_folder_waku = waku.clone();
                    let remove_waku = waku.clone();
                    let mut items = vec![
                        MenuItem::new(tr!("common.rename"), move |window, cx| {
                            let _ = rename_waku.update(cx, |waku, cx| {
                                waku.begin_session_rename(session_id, false, window, cx);
                            });
                        }),
                        MenuItem::new(
                            if pinned {
                                tr!("sidebar.unpin")
                            } else {
                                tr!("sidebar.pin")
                            },
                            move |_, cx| {
                                let _ = pin_waku.update(cx, |waku, cx| {
                                    waku.set_session_pinned(session_id, !pinned, cx);
                                });
                            },
                        )
                        .icon("icons/pin.svg"),
                        MenuItem::new(
                            if archived {
                                tr!("sidebar.unarchive")
                            } else {
                                tr!("sidebar.archive")
                            },
                            move |_, cx| {
                                let _ = archive_waku.update(cx, |waku, cx| {
                                    waku.set_session_archived(session_id, !archived, cx);
                                });
                            },
                        )
                        .icon("icons/archive.svg"),
                        MenuItem::Separator,
                        MenuItem::Header(SharedString::from(tr!("sidebar.move_to_folder"))),
                    ];
                    items.extend(folders.iter().cloned().map(|(id, name)| {
                        let waku = waku.clone();
                        MenuItem::new(name, move |_, cx| {
                            let _ = waku.update(cx, |waku, cx| {
                                // Choosing the current folder takes the session
                                // out of it, so the menu is its own undo.
                                let target = (folder_id != Some(id)).then_some(id);
                                waku.move_session_to_folder(session_id, target, cx);
                            });
                        })
                        .selected(folder_id == Some(id))
                    }));
                    items.push(
                        MenuItem::new(tr!("sidebar.no_folder"), move |_, cx| {
                            let _ = unfile_waku.update(cx, |waku, cx| {
                                waku.move_session_to_folder(session_id, None, cx);
                            });
                        })
                        .selected(folder_id.is_none()),
                    );
                    items.push(MenuItem::new(
                        tr!("sidebar.new_folder"),
                        move |window, cx| {
                            let _ = new_folder_waku.update(cx, |waku, cx| {
                                waku.create_folder(Some(session_id), window, cx);
                            });
                        },
                    ));
                    items.push(MenuItem::Separator);
                    items.push(MenuItem::new(tr!("common.remove"), move |_, cx| {
                        let _ =
                            remove_waku.update(cx, |waku, cx| waku.remove_session(session_id, cx));
                    }));
                    items
                },
            )
        };

        div()
            .w_full()
            .pb(px(SIDEBAR_SESSION_ROW_GAP))
            .child(self.sidebar_insertion_gap(index, cx))
            .child(row)
            .into_any_element()
    }

    /// The gap and line drawn above a row while a drop is aimed at it.
    ///
    /// Opening real space rather than overlaying a line is what makes the
    /// neighbours part around the insertion point, and it keeps the line from
    /// covering the row beneath. Rendered as its own child so the row's own
    /// layout is untouched.
    fn sidebar_insertion_gap(&self, index: usize, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let aimed_here = self
            .sidebar_drop_target
            .is_some_and(|target| target.row == index);
        if !aimed_here {
            return div();
        }
        div()
            .w_full()
            .h(px(SIDEBAR_DROP_INDICATOR_GAP))
            .flex()
            .items_center()
            .child(div().w_full().h(px(2.0)).rounded(px(1.0)).bg(theme.accent))
    }

    // ── Header ─────────────────────────────────────────────────────────────

    /// The task title in the window header: a double-click target that turns
    /// into the same rename field the sidebar uses, and reads as one by
    /// drawing its box on hover and on focus.
    fn render_header_title(&self, title: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(session_id) = self.selected_session().map(|session| session.id) else {
            // Padded and bordered like the editable title, so the label does
            // not shift when a task is selected.
            return div()
                .min_w_0()
                .h(px(24.0))
                .px(px(6.0))
                .border_1()
                .border_color(gpui::transparent_black())
                .flex()
                .items_center()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(title)),
                )
                .into_any_element();
        };

        if self.session_rename == Some(session_id) && self.session_rename_in_header {
            return div()
                .id("header-title-rename-field")
                .key_context(SESSION_RENAME_PARENT_CONTEXT)
                .on_action(cx.listener(|this, _: &CancelSessionRename, window, cx| {
                    this.cancel_session_rename(window, cx);
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if this.session_rename.is_some() {
                        this.commit_session_rename(cx);
                    }
                }))
                .h(px(24.0))
                // Sized by its text, so a short name gets a short box, with a
                // floor that keeps an emptied field clickable and a ceiling
                // that leaves the rest of the header alone.
                .min_w(px(90.0))
                .max_w(px(460.0))
                .px(px(6.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.accent)
                .bg(theme.inset)
                .flex()
                .items_center()
                .overflow_hidden()
                .text_size(px(13.0))
                .text_color(theme.text)
                .child(
                    div()
                        .relative()
                        .flex_none()
                        // The field itself is percentage-width, which
                        // contributes nothing to its parent's content size, so
                        // this mirror of the same text is what makes the box
                        // follow what the user types. The trailing space keeps
                        // room for the caret at the end.
                        .child(
                            div()
                                .whitespace_nowrap()
                                .text_color(gpui::transparent_black())
                                .child(SharedString::from(format!(
                                    "{} ",
                                    self.session_rename_input.read(cx).content()
                                ))),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .child(self.session_rename_input.clone()),
                        ),
                )
                .into_any_element();
        }

        div()
            .id("header-title")
            .track_focus(&self.header_title_focus)
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .min_w_0()
            .h(px(24.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(gpui::transparent_black())
            .flex()
            .items_center()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay).border_color(theme.border))
            .tooltip(Tooltip::text(tr!("common.rename")))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(title)),
            )
            // The header is also the window's drag region; a press here belongs
            // to the title, not to a window move or a titlebar zoom.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.begin_session_rename(session_id, true, window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.begin_session_rename(session_id, true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let title = session
            .map(localized_session_title)
            .unwrap_or_else(|| tr!("session.new_task"));
        let agent_preset_label = session
            .filter(|session| session.provider == ProviderKind::DeepSeek && session.has_started())
            .and_then(|session| self.agent_preset_label_for_session(session));
        // A subagent is deliberately absent from the sidebar, so its own header
        // has to carry the way back to the task that launched it.
        let parent_crumb = session
            .and_then(|session| session.subagent.as_ref())
            .and_then(|origin| {
                let parent = self
                    .state
                    .sessions
                    .iter()
                    .find(|candidate| candidate.id == origin.parent_session_id)?;
                Some((
                    origin.parent_session_id,
                    localized_session_title(parent),
                    origin.role.clone(),
                ))
            });
        div()
            .id("window-header")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pl(if self.sidebar_visible {
                px(14.0)
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
                        .children(parent_crumb.map(|(parent_id, parent_title, role)| {
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .id("subagent-parent-crumb")
                                        .track_focus(&self.transcript_control_focus(
                                            "subagent-parent-crumb".to_owned(),
                                            cx,
                                        ))
                                        .tab_index(0)
                                        .h(px(22.0))
                                        .max_w(px(160.0))
                                        .px(px(6.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .cursor_default()
                                        .text_size(px(11.5))
                                        .text_color(theme.text_tertiary)
                                        .focus_visible(|style| {
                                            style.border_1().border_color(theme.accent)
                                        })
                                        .hover(|style| style.bg(theme.overlay))
                                        .tooltip(Tooltip::text(tr!("session.back_to_task")))
                                        .child(icon("icons/arrow-left.svg", 11.0, theme.text_ghost))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .child(SharedString::from(parent_title)),
                                        )
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.select_session(parent_id, cx);
                                        }))
                                        .on_key_down(cx.listener(
                                            move |this, event: &KeyDownEvent, _, cx| {
                                                if matches!(
                                                    event.keystroke.key.as_str(),
                                                    "enter" | "space"
                                                ) {
                                                    this.select_session(parent_id, cx);
                                                    cx.stop_propagation();
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_ghost)
                                        .child("/"),
                                )
                                .children(role.map(|role| {
                                    div()
                                        .flex_none()
                                        .h(px(18.0))
                                        .px(px(5.0))
                                        .rounded(px(5.0))
                                        .flex()
                                        .items_center()
                                        .bg(theme.overlay)
                                        .text_size(px(10.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text_tertiary)
                                        .child(role)
                                }))
                        }))
                        .child(self.render_header_title(title, cx))
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
                                .text_size(px(11.0))
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
                        .text_size(px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tr_cow!("onboarding.open_project_to_begin")),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(px(12.5))
                        .line_height(px(19.0))
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
                                .text_size(px(12.5))
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
                                .text_size(px(12.0))
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
                    .text_size(px(20.0))
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
        let group = SidebarGroup::Date(SessionDateGroup::Today);
        let sessions = [Uuid::from_u128(1), Uuid::from_u128(2)];
        let mut expanded = Vec::new();
        append_sidebar_group_rows(&mut expanded, group, &sessions, false);
        assert_eq!(
            expanded,
            vec![
                SidebarRow::Header(group),
                SidebarRow::Session(sessions[0], Some(group)),
                SidebarRow::Session(sessions[1], Some(group)),
                SidebarRow::GroupSpacer,
            ]
        );

        let mut collapsed = Vec::new();
        append_sidebar_group_rows(&mut collapsed, group, &sessions, true);
        assert_eq!(
            collapsed,
            vec![SidebarRow::Header(group), SidebarRow::GroupSpacer]
        );
    }

    #[test]
    fn group_keys_are_stable_and_distinct_across_grouping_modes() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let keys = [
            SidebarGroup::Pinned,
            SidebarGroup::Date(SessionDateGroup::Today),
            SidebarGroup::Date(SessionDateGroup::More),
            SidebarGroup::Project(first),
            SidebarGroup::Folder(first),
            SidebarGroup::Folder(second),
            SidebarGroup::Unfiled,
            SidebarGroup::Archived,
        ]
        .map(SidebarGroup::key);
        let unique = keys.iter().collect::<HashSet<_>>();
        assert_eq!(unique.len(), keys.len());
        // A project and a folder sharing an id must not share a fold state.
        assert_ne!(
            SidebarGroup::Project(first).key(),
            SidebarGroup::Folder(first).key()
        );
    }

    /// A fixed "now" so age assertions do not drift with the clock.
    const TEST_NOW: u64 = 1_000 * 86_400;

    #[test]
    fn filter_hides_archived_sessions_until_they_are_asked_for() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.archived = true;
        let mut filter = SidebarFilter::default();
        assert!(!filter.matches(&session, TEST_NOW));
        filter.show_archived = true;
        assert!(filter.matches(&session, TEST_NOW));
    }

    #[test]
    fn provider_and_activity_filters_compose() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.status = SessionStatus::Idle;
        let mut filter = SidebarFilter {
            provider: Some(ProviderKind::Codex),
            ..SidebarFilter::default()
        };
        assert!(filter.matches(&session, TEST_NOW));
        assert!(filter.is_narrowed());

        filter.active_only = true;
        assert!(!filter.matches(&session, TEST_NOW));
        session.status = SessionStatus::Working;
        assert!(filter.matches(&session, TEST_NOW));

        filter.provider = Some(ProviderKind::Claude);
        assert!(!filter.matches(&session, TEST_NOW));
    }

    #[test]
    fn age_filter_hides_stale_sessions_but_never_live_ones() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.created_at = TEST_NOW - 10 * 86_400;
        session.last_reply_at = Some(TEST_NOW - 10 * 86_400);
        session.status = SessionStatus::Idle;

        let mut filter = SidebarFilter {
            max_age_days: Some(30),
            ..SidebarFilter::default()
        };
        assert!(filter.matches(&session, TEST_NOW));
        assert!(filter.is_narrowed());

        filter.max_age_days = Some(7);
        assert!(!filter.matches(&session, TEST_NOW));

        // Still working, so the cutoff must not hide it out from under the user.
        session.status = SessionStatus::Working;
        assert!(filter.matches(&session, TEST_NOW));
    }

    #[test]
    fn age_filter_falls_back_to_creation_for_a_session_that_never_replied() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.created_at = TEST_NOW - 3 * 86_400;
        session.last_reply_at = None;
        session.status = SessionStatus::Idle;

        let filter = SidebarFilter {
            max_age_days: Some(7),
            ..SidebarFilter::default()
        };
        assert!(filter.matches(&session, TEST_NOW));

        session.created_at = TEST_NOW - 30 * 86_400;
        assert!(!filter.matches(&session, TEST_NOW));
    }

    #[test]
    fn activity_sort_puts_waiting_first_then_working_then_the_rest() {
        let project_id = Uuid::new_v4();
        let mut idle = AgentSession::new(project_id, ProviderKind::Codex);
        idle.status = SessionStatus::Idle;
        idle.last_reply_at = Some(300);
        let mut working = AgentSession::new(project_id, ProviderKind::Codex);
        working.status = SessionStatus::Working;
        working.last_reply_at = Some(200);
        let mut waiting = AgentSession::new(project_id, ProviderKind::Codex);
        waiting.status = SessionStatus::Waiting;
        waiting.last_reply_at = Some(100);

        let mut sessions = [&idle, &working, &waiting];
        sort_sidebar_sessions(&mut sessions, SidebarSort::Activity);
        assert_eq!(
            sessions.map(|session| session.id),
            [waiting.id, working.id, idle.id]
        );

        // Recency still wins when nothing is active, so the sort degrades to
        // the default rather than to an arbitrary order.
        sort_sidebar_sessions(&mut sessions, SidebarSort::Recent);
        assert_eq!(sessions[0].id, idle.id);
    }

    #[test]
    fn sorting_orders_by_recency_then_by_title() {
        let project_id = Uuid::new_v4();
        let mut older = AgentSession::new(project_id, ProviderKind::Codex);
        older.set_title("Alpha");
        older.last_reply_at = Some(10);
        let mut newer = AgentSession::new(project_id, ProviderKind::Codex);
        newer.set_title("Zulu");
        newer.last_reply_at = Some(20);

        let mut sessions = [&older, &newer];
        sort_sidebar_sessions(&mut sessions, SidebarSort::Recent);
        assert_eq!(sessions[0].id, newer.id);

        sort_sidebar_sessions(&mut sessions, SidebarSort::Oldest);
        assert_eq!(sessions[0].id, older.id);

        sort_sidebar_sessions(&mut sessions, SidebarSort::Title);
        assert_eq!(sessions[0].id, older.id);
    }

    #[test]
    fn manual_sort_keeps_unmoved_sessions_below_hand_placed_ones() {
        let project_id = Uuid::new_v4();
        let mut placed = AgentSession::new(project_id, ProviderKind::Codex);
        placed.position = Some(3);
        placed.last_reply_at = Some(10);
        let mut newer_unplaced = AgentSession::new(project_id, ProviderKind::Codex);
        newer_unplaced.position = None;
        newer_unplaced.last_reply_at = Some(500);
        let mut older_unplaced = AgentSession::new(project_id, ProviderKind::Codex);
        older_unplaced.position = None;
        older_unplaced.last_reply_at = Some(100);

        let mut sessions = [&newer_unplaced, &placed, &older_unplaced];
        sort_sidebar_sessions(&mut sessions, SidebarSort::Manual);
        assert_eq!(
            sessions.map(|session| session.id),
            // The placed one leads; the rest keep recency order behind it.
            [placed.id, newer_unplaced.id, older_unplaced.id]
        );
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

        let mut sessions = [&renamed_old_session, &newer_unanswered_session];
        sessions.sort_by_key(|session| std::cmp::Reverse(sidebar_session_timestamp(session)));
        assert_eq!(sessions[0].id, newer_unanswered_session.id);
    }
}

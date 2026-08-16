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

fn append_sidebar_group_rows(
    rows: &mut Vec<SidebarRow>,
    group: SessionDateGroup,
    sessions: &[Uuid],
    collapsed: bool,
) {
    if sessions.is_empty() {
        return;
    }

    rows.push(SidebarRow::Header(group));
    if !collapsed {
        rows.extend(sessions.iter().copied().map(SidebarRow::Session));
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
/// How many of an expanded automation's runs show before the rest fold behind a
/// "Show more" toggle.
const AUTOMATION_RUN_PREVIEW: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AutomationSidebarMetadata {
    name: Option<String>,
    enabled: bool,
    time_label: Option<String>,
    running_status: Option<SessionStatus>,
    owns_selection: bool,
    run_count: usize,
}

impl Default for AutomationSidebarMetadata {
    fn default() -> Self {
        Self {
            name: None,
            enabled: true,
            time_label: None,
            running_status: None,
            owns_selection: false,
            run_count: 0,
        }
    }
}

fn automation_sidebar_metadata<'a>(
    sessions: impl IntoIterator<Item = &'a AgentSession>,
    automations: impl IntoIterator<Item = &'a crate::automation::Automation>,
    selected_session: Option<Uuid>,
    now: u64,
) -> HashMap<Uuid, AutomationSidebarMetadata> {
    let mut metadata: HashMap<Uuid, AutomationSidebarMetadata> = HashMap::new();
    for session in sessions {
        let Some(automation_id) = session.originating_automation else {
            continue;
        };
        let entry = metadata.entry(automation_id).or_default();
        entry.run_count += 1;
        entry.owns_selection |= selected_session == Some(session.id);
        if entry.running_status.is_none()
            && matches!(
                session.status,
                SessionStatus::Connecting | SessionStatus::Working
            )
        {
            entry.running_status = Some(session.status);
            entry.time_label = session_time_label(session, now);
        } else if entry.running_status.is_none() && entry.time_label.is_none() {
            // Sessions arrive newest first, so the first idle session supplies
            // the same recency label as the former `latest_run` timestamp.
            entry.time_label = Some(format_time_ago(
                now.saturating_sub(sidebar_session_timestamp(session)),
            ));
        }
    }
    for automation in automations {
        let entry = metadata.entry(automation.id).or_default();
        entry.name = Some(automation.name.clone());
        entry.enabled = automation.enabled;
    }
    metadata
}
const SIDEBAR_SEARCH_BOTTOM_GAP: f32 = 10.0;

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

/// Absolute local date and time for an automation run row, e.g.
/// "Aug 14 · 3:25 PM". Runs share the prompt as their title, so the time is
/// what distinguishes them under the group.
fn format_run_timestamp(unix: u64) -> String {
    DateTime::<Utc>::from_timestamp(unix as i64, 0)
        .map(|when| {
            when.with_timezone(&Local)
                .format("%b %-d · %-I:%M %p")
                .to_string()
        })
        .unwrap_or_default()
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
    /// Collapsible header for the automation-spawned conversations section,
    /// pinned above the date groups so automation runs do not bloat them.
    AutomationsHeader,
    /// A collapsible group for one automation's runs, labeled with the
    /// automation's name. Its runs render as `AutomationRun` rows when expanded.
    AutomationGroup(Uuid),
    /// One run under an expanded `AutomationGroup`: the same session row as
    /// `Session`, indented so it reads as nested under the automation name.
    AutomationRun(Uuid),
    /// Toggle below an expanded group with more than the preview count of runs:
    /// reveals the remaining runs or collapses back to the preview.
    AutomationRunsToggle(Uuid),
    /// Date-group header; the first date header also carries the project action.
    Header(SessionDateGroup),
    /// A started session.
    Session(Uuid),
    /// Spacing between date groups.
    GroupSpacer,
}

pub(super) struct SidebarRows {
    rows: Vec<SidebarRow>,
    first_date_header: Option<usize>,
}

impl Default for SidebarRows {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            first_date_header: None,
        }
    }
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

    fn render_sidebar_project_action(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        div().flex().items_center().child(
            div()
                .id("add-project")
                .w(px(20.0))
                .h(px(20.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .child(icon("icons/folder-new.svg", 15.0, theme.text_ghost))
                .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
        )
    }

    fn render_sidebar_action_row(
        &self,
        id: &'static str,
        icon_path: &'static str,
        label: String,
        selected: bool,
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
            .when(selected, |element| {
                element.bg(theme.sidebar_item_background)
            })
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
            false,
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

    fn render_sidebar_automations(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.render_sidebar_action_row(
            "sidebar-automations",
            "icons/zap.svg",
            tr!("sidebar.automations"),
            matches!(self.active_page.as_ref(), Some(ActivePage::Automations(_))),
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_automations(window, cx);
        }))
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                this.open_automations(window, cx);
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
                false,
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
            .text_size(px(11.0))
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

    pub(super) fn render_sidebar(
        &self,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let is_resizing = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == PanelResizeTarget::Sidebar);

        let rows = self.sidebar_rows_cached(Local::now().date_naive());
        self.sync_sidebar_rows(&rows.rows);
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
                    .flex_none()
                    .px(px(10.0))
                    .child(self.render_sidebar_automations(cx)),
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
                                                this.sidebar_row(
                                                    index,
                                                    &rows.rows,
                                                    rows.first_date_header,
                                                    cx,
                                                )
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
    /// [`Self::sidebar_rows`] reads.
    fn sidebar_rows_cached(&self, today: NaiveDate) -> Rc<SidebarRows> {
        let fingerprint = sidebar_rows_fingerprint(
            &self.state.sessions,
            &self.state.automations,
            self.state.selected_session,
            today,
            &self.sidebar_collapsed_groups,
            self.sidebar_automations_collapsed,
            &self.sidebar_expanded_automations,
            &self.sidebar_expanded_automation_runs,
        );
        if self.sidebar_rows_fingerprint.get() != Some(fingerprint) {
            *self.sidebar_rows_snapshot.borrow_mut() = Rc::new(self.sidebar_rows(today));
            self.sidebar_rows_fingerprint.set(Some(fingerprint));
        }
        self.sidebar_rows_snapshot.borrow().clone()
    }

    /// Snapshot the session history as a flat list of lightweight rows, newest
    /// first, grouped by calendar period like the previous eager render.
    fn sidebar_rows(&self, today: NaiveDate) -> SidebarRows {
        let mut grouped_sessions: [Vec<Uuid>; 6] = std::array::from_fn(|_| Vec::new());
        // Automation runs fold under one collapsible section, and within it
        // under a per-automation group named after the automation, so a busy
        // automation is a single row rather than a run per line. Groups keep
        // first-seen order (most recent run first) since sessions are sorted by
        // recency below.
        let mut automation_group_index: HashMap<Uuid, usize> = HashMap::new();
        let mut automation_groups: Vec<(Uuid, Vec<Uuid>)> = Vec::new();
        let mut sorted_sessions = self
            .state
            .sessions
            .iter()
            .filter(|session| session.has_started())
            .collect::<Vec<_>>();
        sorted_sessions
            .sort_by_key(|session| std::cmp::Reverse(sidebar_session_timestamp(session)));
        *self.sidebar_automation_metadata.borrow_mut() = automation_sidebar_metadata(
            sorted_sessions.iter().copied(),
            self.state.automations.iter(),
            self.state.selected_session,
            unix_time(),
        );
        for session in sorted_sessions {
            if let Some(automation_id) = session.originating_automation {
                match automation_group_index.get(&automation_id) {
                    Some(&index) => automation_groups[index].1.push(session.id),
                    None => {
                        automation_group_index.insert(automation_id, automation_groups.len());
                        automation_groups.push((automation_id, vec![session.id]));
                    }
                }
                continue;
            }
            grouped_sessions[session_date_group(sidebar_session_timestamp(session), today).index()]
                .push(session.id);
        }

        let mut rows = vec![SidebarRow::Search];
        if !automation_groups.is_empty() {
            rows.push(SidebarRow::AutomationsHeader);
            if !self.sidebar_automations_collapsed {
                for (automation_id, session_ids) in automation_groups {
                    rows.push(SidebarRow::AutomationGroup(automation_id));
                    if self.sidebar_expanded_automations.contains(&automation_id) {
                        // Preview only the most recent runs; the rest hide behind
                        // a toggle so a busy automation does not flood the sidebar.
                        let show_all = self
                            .sidebar_expanded_automation_runs
                            .contains(&automation_id);
                        let visible = if show_all {
                            session_ids.len()
                        } else {
                            session_ids.len().min(AUTOMATION_RUN_PREVIEW)
                        };
                        rows.extend(
                            session_ids[..visible]
                                .iter()
                                .copied()
                                .map(SidebarRow::AutomationRun),
                        );
                        if session_ids.len() > AUTOMATION_RUN_PREVIEW {
                            rows.push(SidebarRow::AutomationRunsToggle(automation_id));
                        }
                    }
                }
            }
            rows.push(SidebarRow::GroupSpacer);
        }
        let has_date_sessions = grouped_sessions.iter().any(|group| !group.is_empty());
        let mut first_date_header = None;
        for group in SessionDateGroup::ALL {
            let group_sessions = &grouped_sessions[group.index()];
            if !group_sessions.is_empty() && first_date_header.is_none() {
                first_date_header = Some(rows.len());
            }
            append_sidebar_group_rows(
                &mut rows,
                group,
                group_sessions,
                self.sidebar_collapsed_groups.contains(&group),
            );
        }
        if !has_date_sessions {
            // Keep the project action visible while there is no manual history,
            // even when automation runs already occupy the sidebar above.
            first_date_header = Some(rows.len());
            rows.push(SidebarRow::Header(SessionDateGroup::Today));
        }
        SidebarRows {
            rows,
            first_date_header,
        }
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

    fn sidebar_row(
        &self,
        index: usize,
        rows: &[SidebarRow],
        first_date_header: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = rows.get(index) else {
            return div().into_any_element();
        };
        match *row {
            SidebarRow::Search => self.render_sidebar_search(cx).into_any_element(),
            SidebarRow::AutomationsHeader => self
                .render_sidebar_automations_header(cx)
                .into_any_element(),
            SidebarRow::AutomationGroup(automation_id) => {
                self.render_sidebar_automation_group(automation_id, cx)
            }
            SidebarRow::AutomationRun(session_id) => div()
                .pl(px(16.0))
                .child(self.render_sidebar_session_item(session_id, true, cx))
                .into_any_element(),
            SidebarRow::AutomationRunsToggle(automation_id) => {
                self.render_sidebar_automation_runs_toggle(automation_id, cx)
            }
            SidebarRow::Header(group) => self
                .render_sidebar_group_header(group, first_date_header == Some(index), cx)
                .into_any_element(),
            SidebarRow::Session(session_id) => self
                .render_sidebar_session_item(session_id, false, cx)
                .into_any_element(),
            SidebarRow::GroupSpacer => div().w_full().h(px(10.0)).into_any_element(),
        }
    }

    fn render_sidebar_group_header(
        &self,
        group: SessionDateGroup,
        first: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let collapsed = self.sidebar_collapsed_groups.contains(&group);
        let group_name = SharedString::from(format!("sidebar-group-header-{}", group.index()));
        let chevron = icon("icons/chevron-down.svg", 11.0, theme.text_ghost)
            .when(collapsed, |icon| {
                icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
            })
            .invisible()
            .group_hover(group_name.clone(), |icon| icon.visible());

        session_group_header(&theme)
            .group(group_name)
            .w_full()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "sidebar-group-toggle-{}",
                        group.index()
                    )))
                    .tab_index(0)
                    .h(px(22.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_default()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .child(group.label())
                    .child(chevron)
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
                    })),
            )
            .when(first, |element| {
                element
                    .justify_between()
                    .child(self.render_sidebar_project_action(cx))
            })
    }

    fn toggle_sidebar_group(&mut self, group: SessionDateGroup, cx: &mut Context<Self>) {
        let collapsed = !self.sidebar_collapsed_groups.contains(&group);
        self.set_sidebar_group_collapsed(group, collapsed, cx);
    }

    fn set_sidebar_group_collapsed(
        &mut self,
        group: SessionDateGroup,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.sidebar_collapsed_groups.insert(group)
        } else {
            self.sidebar_collapsed_groups.remove(&group)
        };
        if changed {
            cx.notify();
        }
    }

    fn render_sidebar_automations_header(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let collapsed = self.sidebar_automations_collapsed;
        let group_name = SharedString::from("sidebar-automations-group-header");
        let chevron = icon("icons/chevron-down.svg", 11.0, theme.text_ghost)
            .when(collapsed, |icon| {
                icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
            })
            .invisible()
            .group_hover(group_name.clone(), |icon| icon.visible());

        session_group_header(&theme)
            .group(group_name)
            .w_full()
            .child(
                div()
                    .id("sidebar-automations-group-toggle")
                    .tab_index(0)
                    .h(px(22.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_default()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .child(tr!("sidebar.automations"))
                    .child(chevron)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_sidebar_automations(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        match event.keystroke.key.as_str() {
                            "enter" | "space" => {
                                this.toggle_sidebar_automations(cx);
                                cx.stop_propagation();
                            }
                            "left" if !this.sidebar_automations_collapsed => {
                                this.set_sidebar_automations_collapsed(true, cx);
                                cx.stop_propagation();
                            }
                            "right" if this.sidebar_automations_collapsed => {
                                this.set_sidebar_automations_collapsed(false, cx);
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    })),
            )
    }

    fn toggle_sidebar_automations(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar_automations_collapsed(!self.sidebar_automations_collapsed, cx);
    }

    fn set_sidebar_automations_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.sidebar_automations_collapsed != collapsed {
            self.sidebar_automations_collapsed = collapsed;
            cx.notify();
        }
    }

    /// Whether a specific automation has a run connecting or working right now.
    /// One automation's collapsible run group: its name, the time since its
    /// most recent run, and a chevron. Collapsed by default; expanding reveals
    /// its runs. Falls back to a generic label if the automation was deleted.
    fn render_sidebar_automation_group(
        &self,
        automation_id: Uuid,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let expanded = self.sidebar_expanded_automations.contains(&automation_id);
        let metadata = self
            .sidebar_automation_metadata
            .borrow()
            .get(&automation_id)
            .cloned()
            .unwrap_or_default();
        let name = metadata
            .name
            .clone()
            .unwrap_or_else(|| tr!("sidebar.automation_deleted"));
        // A disabled automation reads muted, paired with a block glyph so the
        // state is not carried by color alone.
        let enabled = metadata.enabled;
        // A currently-working run turns the trailing metadata into the same
        // spinner + "Working…" treatment the run rows use. The finished label
        // is prepared with the session row's shared time-label routine while
        // the snapshot is built.
        // When collapsed, mirror the selected run so the active automation reads
        // as selected without expanding. The Automations page owns the
        // highlight while it is open, so defer to it as session rows do.
        let owns_selection = !matches!(self.active_page.as_ref(), Some(ActivePage::Automations(_)))
            && metadata.owns_selection;
        let highlight = owns_selection && !expanded;

        // Reveal the chevron only on hover, matching the section headers.
        let group_name = SharedString::from(format!("automation-group-hover-{automation_id}"));
        let chevron = icon("icons/chevron-down.svg", 11.0, theme.text_ghost)
            .when(!expanded, |icon| {
                icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
            })
            .invisible()
            .group_hover(group_name.clone(), |icon| icon.visible());

        // The right-hand metadata. Expanding the group reveals the runs below,
        // whose first row already carries this, so drop it to avoid a duplicate.
        let trailing = if expanded {
            None
        } else if let Some(status) = metadata.running_status {
            Some(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        icon(
                            "icons/loader-circle.svg",
                            12.0,
                            status_color(&theme, status),
                        )
                        .with_animation(
                            SharedString::from(format!("automation-group-spinner-{automation_id}")),
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
                    .when_some(metadata.time_label.clone(), |element, label| {
                        element.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_ghost)
                                .child(SharedString::from(label)),
                        )
                    })
                    .into_any_element(),
            )
        } else if !enabled {
            // Disabled: a pause glyph replaces the time so the muted row still
            // states its status without relying on color.
            Some(icon("icons/pause.svg", 11.0, theme.text_tertiary).into_any_element())
        } else {
            metadata.time_label.map(|label| {
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme.text_ghost)
                    .child(SharedString::from(label))
                    .into_any_element()
            })
        };

        let waku = cx.entity().downgrade();
        let menu = self.menu_handle(format!("automation-menu-{automation_id}"), cx);
        let keyboard_menu = menu.clone();

        let row = div()
            .id(SharedString::from(format!(
                "automation-group-{automation_id}"
            )))
            .group(group_name)
            .tab_index(0)
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(7.0))
            .rounded(px(7.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .when(highlight, |element| {
                element.bg(theme.sidebar_item_background)
            })
            .hover(|element| element.bg(theme.sidebar_item_background))
            .child(
                // Name + chevron form the left cluster so the chevron sits just
                // right of the (truncating) name rather than pinned far right.
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .min_w_0()
                            .whitespace_normal()
                            .line_clamp(1)
                            .text_overflow(gpui::TextOverflow::Truncate("...".into()))
                            .text_size(px(13.5))
                            .text_color(if enabled {
                                theme.text
                            } else {
                                theme.text_tertiary
                            })
                            .child(SharedString::from(name)),
                    )
                    .child(chevron),
            )
            .when_some(trailing, |element, trailing| element.child(trailing))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_automation(automation_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => {
                        this.toggle_sidebar_automation(automation_id, cx);
                        cx.stop_propagation();
                    }
                    "left" if this.sidebar_expanded_automations.contains(&automation_id) => {
                        this.set_sidebar_automation_expanded(automation_id, false, cx);
                        cx.stop_propagation();
                    }
                    "right" if !this.sidebar_expanded_automations.contains(&automation_id) => {
                        this.set_sidebar_automation_expanded(automation_id, true, cx);
                        cx.stop_propagation();
                    }
                    "f10" if event.keystroke.modifiers.shift => {
                        keyboard_menu.open_context_menu(window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }));

        // Right-click (or Shift+F10) to run now, edit, or toggle enabled.
        // The bottom gap separates the group row from its runs, matching the
        // spacing between the run rows themselves.
        context_menu(
            div().w_full().pb(px(SIDEBAR_SESSION_ROW_GAP)).child(row),
            SharedString::from(format!("automation-menu-{automation_id}")),
            &menu,
            move |_| {
                let run_waku = waku.clone();
                let edit_waku = waku.clone();
                let toggle_waku = waku.clone();
                vec![
                    MenuItem::new(tr!("automations.run_now"), move |_, cx| {
                        let _ = run_waku.update(cx, |waku, cx| {
                            waku.run_automation_now(automation_id, cx);
                        });
                    }),
                    MenuItem::new(tr!("automations.edit"), move |window, cx| {
                        let _ = edit_waku.update(cx, |waku, cx| {
                            waku.open_automation_editor(Some(automation_id), window, cx);
                        });
                    }),
                    MenuItem::Separator,
                    MenuItem::new(
                        if enabled {
                            tr!("automations.menu_disable")
                        } else {
                            tr!("automations.menu_enable")
                        },
                        move |_, cx| {
                            let _ = toggle_waku.update(cx, |waku, cx| {
                                waku.toggle_automation_enabled(automation_id, cx);
                            });
                        },
                    ),
                ]
            },
        )
    }

    fn toggle_sidebar_automation(&mut self, automation_id: Uuid, cx: &mut Context<Self>) {
        let expanded = self.sidebar_expanded_automations.contains(&automation_id);
        self.set_sidebar_automation_expanded(automation_id, !expanded, cx);
    }

    fn set_sidebar_automation_expanded(
        &mut self,
        automation_id: Uuid,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if expanded {
            self.sidebar_expanded_automations.insert(automation_id)
        } else {
            self.sidebar_expanded_automations.remove(&automation_id)
        };
        if changed {
            cx.notify();
        }
    }

    /// The "Show more"/"Show less" row beneath an expanded group whose runs
    /// exceed the preview. Indented under the runs and toggles the full list.
    fn render_sidebar_automation_runs_toggle(
        &self,
        automation_id: Uuid,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let expanded = self
            .sidebar_expanded_automation_runs
            .contains(&automation_id);
        let total = self
            .sidebar_automation_metadata
            .borrow()
            .get(&automation_id)
            .map_or(0, |metadata| metadata.run_count);
        let label = if expanded {
            tr!("sidebar.show_fewer_runs")
        } else {
            tr!(
                "sidebar.show_more_runs",
                count = total.saturating_sub(AUTOMATION_RUN_PREVIEW)
            )
        };
        let group_name =
            SharedString::from(format!("automation-runs-toggle-hover-{automation_id}"));
        // Down chevron reads as "reveal more"; flip it up once expanded.
        let chevron = icon("icons/chevron-down.svg", 11.0, theme.text_ghost)
            .when(expanded, |icon| {
                icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.5)))
            })
            .group_hover(group_name.clone(), |icon| {
                icon.text_color(theme.text_secondary)
            });

        div()
            .id(SharedString::from(format!(
                "automation-runs-toggle-{automation_id}"
            )))
            .group(group_name.clone())
            .tab_index(0)
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(5.0))
            .pl(px(24.0))
            .pr(px(8.0))
            .py(px(5.0))
            .rounded(px(7.0))
            .cursor_default()
            .text_color(theme.text_ghost)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.5))
                    .group_hover(group_name, |label| label.text_color(theme.text_secondary))
                    .child(SharedString::from(label)),
            )
            .child(chevron)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_automation_runs(automation_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.toggle_sidebar_automation_runs(automation_id, cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    fn toggle_sidebar_automation_runs(&mut self, automation_id: Uuid, cx: &mut Context<Self>) {
        if !self.sidebar_expanded_automation_runs.remove(&automation_id) {
            self.sidebar_expanded_automation_runs.insert(automation_id);
        }
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

    fn render_sidebar_session_item(
        &self,
        session_id: Uuid,
        show_run_time: bool,
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
        // While the Automations page is open it owns the selection highlight, so
        // the underlying selected session row does not also appear selected.
        let selected = !matches!(self.active_page.as_ref(), Some(ActivePage::Automations(_)))
            && sidebar_session_selected(
                self.state.selected_session,
                self.pending_session_activation
                    .map(|pending| pending.session_id),
                session_id,
            );
        let working = matches!(
            session.status,
            SessionStatus::Connecting | SessionStatus::Working
        );
        let project_name = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .map(Project::display_name)
            .unwrap_or_else(|| tr!("sidebar.unknown_project"));
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
                // Automation runs all share the prompt as their title, so the
                // run time is what tells them apart under the group. Style it
                // like the folder subtitle — small and muted — since it reads
                // more as metadata than a title.
                .text_size(if show_run_time { px(11.5) } else { px(13.5) })
                .text_color(if show_run_time {
                    theme.text_tertiary
                } else {
                    theme.text
                })
                .child(SharedString::from(if show_run_time {
                    format_run_timestamp(session.created_at)
                } else {
                    localized_session_title(session)
                }))
                .into_any_element()
        };
        let time_label = session_time_label(session, unix_time());
        let time_label_color = if session.is_busy() {
            theme.text_tertiary
        } else {
            theme.text_ghost
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
                    })
                    // Automation runs collapse to a single row, so the run time
                    // rides inline at the end of the title line rather than in
                    // the folder subtitle below (which they omit entirely).
                    .when(show_run_time, |element| {
                        element.when_some(time_label.clone(), |element, label| {
                            element.child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.5))
                                    .text_color(time_label_color)
                                    .child(SharedString::from(label)),
                            )
                        })
                    }),
            )
            .when(!show_run_time, |element| {
                element.child(
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
                        .when_some(time_label.clone(), |element, label| {
                            element.child(
                                div()
                                    .flex_none()
                                    .text_color(time_label_color)
                                    .child(SharedString::from(label)),
                            )
                        }),
                )
            })
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
            .w_full()
            .pb(px(SIDEBAR_SESSION_ROW_GAP))
            .child(row)
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
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.0))
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

fn sidebar_rows_fingerprint(
    sessions: &[AgentSession],
    automations: &[crate::automation::Automation],
    selected_session: Option<Uuid>,
    today: NaiveDate,
    collapsed_groups: &HashSet<SessionDateGroup>,
    automations_collapsed: bool,
    expanded_automations: &HashSet<Uuid>,
    expanded_automation_runs: &HashSet<Uuid>,
) -> u64 {
    let mut fingerprint = mix(0x51de_ba5e_5eed_c0de, today.num_days_from_ce() as u64);
    for session in sessions {
        if !session.has_started() {
            continue;
        }
        fingerprint = mix_uuid(fingerprint, session.id);
        fingerprint = mix(fingerprint, sidebar_session_timestamp(session));
        // `sidebar_rows` also caches each automation group's spinner state, and
        // a run can go Connecting -> Working -> Failed without ever landing a
        // reply, so the recency timestamp above would not move.
        if session.originating_automation.is_some() {
            fingerprint = mix(fingerprint, session.status as u64 + 1);
        }
    }
    // The group rows cache each automation's name and enabled flag, so a rename
    // or a toggle has to invalidate the snapshot even though no session moved.
    for automation in automations {
        fingerprint = mix_uuid(fingerprint, automation.id);
        fingerprint = mix(fingerprint, automation.updated_at);
        fingerprint = mix(fingerprint, u64::from(automation.enabled));
    }
    // A collapsed group mirrors its selected run's highlight, which is cached
    // alongside the rest of the metadata.
    fingerprint = match selected_session {
        Some(session_id) => mix_uuid(fingerprint, session_id),
        None => mix(fingerprint, 0x5e1e_c7ed_0000_0000),
    };
    // Sets have no stable iteration order; combine each one order-independently
    // and use distinct seeds so moving an id between disclosures changes the key.
    let collapsed = collapsed_groups.iter().fold(0u64, |combined, group| {
        combined.wrapping_add(mix(0, group.index() as u64 + 1))
    });
    fingerprint = mix(mix(fingerprint, collapsed_groups.len() as u64), collapsed);
    fingerprint = mix(fingerprint, u64::from(automations_collapsed));

    let expanded = expanded_automations.iter().fold(0u64, |combined, id| {
        combined.wrapping_add(mix_uuid(0xa710_5eed, *id))
    });
    fingerprint = mix(
        mix(fingerprint, expanded_automations.len() as u64),
        expanded,
    );

    let expanded_runs = expanded_automation_runs.iter().fold(0u64, |combined, id| {
        combined.wrapping_add(mix_uuid(0x7a11_5eed, *id))
    });
    mix(
        mix(fingerprint, expanded_automation_runs.len() as u64),
        expanded_runs,
    )
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
        let mut expanded = Vec::new();
        append_sidebar_group_rows(&mut expanded, SessionDateGroup::Today, &sessions, false);
        assert_eq!(
            expanded,
            vec![
                SidebarRow::Header(SessionDateGroup::Today),
                SidebarRow::Session(sessions[0]),
                SidebarRow::Session(sessions[1]),
                SidebarRow::GroupSpacer,
            ]
        );

        let mut collapsed = Vec::new();
        append_sidebar_group_rows(&mut collapsed, SessionDateGroup::Today, &sessions, true);
        assert_eq!(
            collapsed,
            vec![
                SidebarRow::Header(SessionDateGroup::Today),
                SidebarRow::GroupSpacer,
            ]
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

    #[test]
    fn automation_disclosures_invalidate_sidebar_rows() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
        let automation_id = Uuid::from_u128(1);
        let sessions = Vec::new();
        let automations = Vec::new();
        let collapsed_groups = HashSet::new();
        let collapsed = HashSet::new();
        let expanded = HashSet::from([automation_id]);

        let baseline = sidebar_rows_fingerprint(
            &sessions,
            &automations,
            None,
            today,
            &collapsed_groups,
            false,
            &collapsed,
            &collapsed,
        );
        assert_ne!(
            baseline,
            sidebar_rows_fingerprint(
                &sessions,
                &automations,
                None,
                today,
                &collapsed_groups,
                true,
                &collapsed,
                &collapsed,
            ),
            "collapsing the Automations section must invalidate its row snapshot"
        );
        assert_ne!(
            baseline,
            sidebar_rows_fingerprint(
                &sessions,
                &automations,
                None,
                today,
                &collapsed_groups,
                false,
                &expanded,
                &collapsed,
            ),
            "expanding an automation must invalidate its run rows"
        );
        assert_ne!(
            baseline,
            sidebar_rows_fingerprint(
                &sessions,
                &automations,
                None,
                today,
                &collapsed_groups,
                false,
                &collapsed,
                &expanded,
            ),
            "showing all automation runs must invalidate its run rows"
        );
    }

    /// The group rows cache the automation's name, enabled flag, spinner, and
    /// selection highlight. None of those move a session's recency timestamp,
    /// so each one needs its own fingerprint input or the sidebar renders the
    /// last-cached value indefinitely.
    #[test]
    fn automation_metadata_changes_invalidate_sidebar_rows() {
        use crate::automation::Automation;

        let today = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
        let collapsed_groups = HashSet::new();
        let collapsed = HashSet::new();
        let automation = Automation::new("Nightly", ProviderKind::Codex, 1_000);
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.originating_automation = Some(automation.id);
        session.begin_turn("run");
        session.status = SessionStatus::Connecting;
        let session_id = session.id;
        let sessions = vec![session];
        let automations = vec![automation.clone()];

        let fingerprint =
            |sessions: &[AgentSession], automations: &[Automation], selected: Option<Uuid>| {
                sidebar_rows_fingerprint(
                    sessions,
                    automations,
                    selected,
                    today,
                    &collapsed_groups,
                    false,
                    &collapsed,
                    &collapsed,
                )
            };
        let baseline = fingerprint(&sessions, &automations, None);

        let mut renamed = automation.clone();
        renamed.name = "Renamed".to_owned();
        renamed.updated_at = automation.updated_at + 1;
        assert_ne!(
            baseline,
            fingerprint(&sessions, &[renamed], None),
            "renaming an automation must invalidate its cached group name"
        );

        let mut disabled = automation.clone();
        disabled.enabled = false;
        assert_ne!(
            baseline,
            fingerprint(&sessions, &[disabled], None),
            "toggling an automation must invalidate its cached enabled state"
        );

        assert_ne!(
            baseline,
            fingerprint(&sessions, &automations, Some(session_id)),
            "selecting a run must invalidate the group's cached highlight"
        );

        let mut working = sessions.clone();
        working[0].status = SessionStatus::Failed;
        assert_ne!(
            baseline,
            fingerprint(&working, &automations, None),
            "a run settling without a reply must invalidate the group's spinner"
        );
    }

    #[test]
    fn automation_metadata_is_built_for_all_groups_in_one_session_pass() {
        let project_id = Uuid::new_v4();
        let first_automation = Uuid::new_v4();
        let second_automation = Uuid::new_v4();
        let mut first = AgentSession::new(project_id, ProviderKind::Codex);
        first.originating_automation = Some(first_automation);
        first.created_at = 10;
        first.status = SessionStatus::Working;
        let mut second = AgentSession::new(project_id, ProviderKind::Codex);
        second.originating_automation = Some(first_automation);
        second.created_at = 20;
        let mut third = AgentSession::new(project_id, ProviderKind::Codex);
        third.originating_automation = Some(second_automation);
        third.created_at = 30;

        let metadata = automation_sidebar_metadata(
            [&first, &second, &third],
            std::iter::empty(),
            Some(second.id),
            1_000,
        );
        assert_eq!(
            metadata[&first_automation],
            AutomationSidebarMetadata {
                name: None,
                enabled: true,
                time_label: None,
                running_status: Some(SessionStatus::Working),
                owns_selection: true,
                run_count: 2,
            }
        );
        assert_eq!(metadata[&second_automation].run_count, 1);
    }
}

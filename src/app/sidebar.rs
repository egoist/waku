use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionDateGroup {
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

    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This Week",
            Self::ThisMonth => "This Month",
            Self::ThisYear => "This Year",
            Self::More => "More",
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

fn session_group_label(theme: &Theme, group: SessionDateGroup) -> Div {
    div()
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_tertiary)
        .child(group.label())
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

    fn render_sidebar_session_actions(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(
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
            .child(
                div()
                    .id("new-session")
                    .w(px(20.0))
                    .h(px(20.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .child(icon("icons/plus.svg", 15.0, theme.text_ghost))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(project_id) = this.state.selected_project {
                            this.create_session_for(project_id, this.state.last_provider, cx);
                        }
                    })),
            )
    }

    pub(super) fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let is_resizing = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == PanelResizeTarget::Sidebar);
        let selected_session = self.state.selected_session;

        let today = Local::now().date_naive();
        let mut grouped_sessions: [Vec<&AgentSession>; 6] = std::array::from_fn(|_| Vec::new());
        let mut sorted_sessions = self
            .state
            .sessions
            .iter()
            .filter(|session| session.has_started())
            .collect::<Vec<_>>();
        sorted_sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        for session in sorted_sessions {
            grouped_sessions[session_date_group(session.updated_at, today).index()].push(session);
        }

        let mut sessions = div().flex().flex_col();
        let mut is_first_group = true;
        for group in SessionDateGroup::ALL {
            let group_sessions = &grouped_sessions[group.index()];
            if group_sessions.is_empty() {
                continue;
            }

            let group_header = session_group_label(&theme, group).when(is_first_group, |element| {
                element
                    .justify_between()
                    .child(self.render_sidebar_session_actions(cx))
            });
            is_first_group = false;
            let mut group_element = div().flex().flex_col().child(group_header);
            for session in group_sessions {
                let session_id = session.id;
                let selected = selected_session == Some(session.id);
                let active = !matches!(session.status, SessionStatus::Idle);
                let project_name = self
                    .state
                    .projects
                    .iter()
                    .find(|project| project.id == session.project_id)
                    .map(|project| project.name.clone())
                    .unwrap_or_else(|| "Unknown project".to_owned());
                let waku = cx.entity().downgrade();
                let composer = self.composer.clone();
                group_element = group_element.child(
                    div()
                        .id(SharedString::from(format!("session-{}", session.id)))
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
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .line_clamp(1)
                                        .text_overflow(gpui::TextOverflow::Truncate("...".into()))
                                        .text_size(px(13.5))
                                        .text_color(theme.text)
                                        .child(SharedString::from(session.title.clone())),
                                )
                                .when(active, |element| {
                                    element.child(pulse_dot(
                                        format!("session-pulse-{session_id}"),
                                        5.0,
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
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.text_tertiary)
                                        .child(SharedString::from(project_name)),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_session(session_id, cx);
                        }))
                        .context_menu_with_id(
                            SharedString::from(format!("session-context-menu-{session_id}")),
                            move |menu, window, cx| {
                                let waku = waku.clone();
                                preserve_composer_focus_for_context_menu(
                                    &composer, menu, window, cx,
                                )
                                .min_w(px(140.0))
                                .item(
                                    PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                                        let _ = waku.update(cx, |waku, cx| {
                                            waku.remove_session(session_id, cx);
                                        });
                                    }),
                                )
                            },
                        ),
                );
            }
            sessions = sessions.child(group_element).child(div().h(px(10.0)));
        }
        if is_first_group {
            sessions = sessions.child(
                session_group_label(&theme, SessionDateGroup::Today)
                    .justify_between()
                    .child(self.render_sidebar_session_actions(cx)),
            );
        }

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
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .child(div().px(px(10.0)).pt(px(2.0)).child(sessions))
                    .overflow_y_scrollbar(),
            )
    }

    // ── Header ─────────────────────────────────────────────────────────────

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let status = session.map(|session| session.status).unwrap_or_default();
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
                        .flex()
                        .items_center()
                        .truncate()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(
                            session
                                .map(|session| session.title.clone())
                                .unwrap_or_else(|| "New task".into()),
                        )),
                    cx,
                ),
            )
            .child(
                self.window_drag_region(
                    div().id("header-center-drag-region").h_full().flex_1(),
                    cx,
                ),
            )
            .when(status != SessionStatus::Idle, |element| {
                element.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .child(match status {
                            SessionStatus::Connecting | SessionStatus::Working => {
                                pulse_dot("header-status-pulse", 5.0, status_color(&theme, status))
                            }
                            _ => div()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(status_color(&theme, status))
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .text_color(status_color(&theme, status))
                                .child(status_label(status)),
                        ),
                )
            })
            .when(!self.right_panel_visible, |element| {
                element.child(self.render_right_panel_toggle(cx))
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
                        .child("Open a project to begin"),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(px(12.5))
                        .line_height(px(19.0))
                        .text_color(theme.text_tertiary)
                        .child(
                            "Waku runs coding agents in folders you choose. Your code, sessions, and history stay on this Mac.",
                        ),
                )
                .child(
                    div()
                        .id("onboarding-add-project")
                        .mt(px(20.0))
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
                        .child("Open project folder…")
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                );
        }
        let selected_project_id = self.state.selected_project;
        let project_name = self
            .selected_project()
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "your project".to_owned());
        let project_options = self
            .state
            .projects
            .iter()
            .filter(|project| Some(project.id) == selected_project_id)
            .chain(
                self.state
                    .projects
                    .iter()
                    .filter(|project| Some(project.id) != selected_project_id),
            )
            .map(|project| (project.id, project.name.clone()))
            .collect::<Vec<_>>();
        let weak = cx.entity().downgrade();
        let composer = self.composer.clone();
        let project_selector = ProjectNameSelector::new("empty-state-project", project_name)
            .dropdown_menu(move |mut menu, _window, cx| {
                menu = menu
                    .action_context(composer.read(cx).focus())
                    .min_w(px(160.0))
                    .max_w(px(256.0))
                    .max_h(px(320.0))
                    .scrollable(true);
                for (project_id, project_name) in project_options.clone() {
                    let item_weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(project_name)
                            .checked(Some(project_id) == selected_project_id)
                            .on_click(move |_, _, cx| {
                                if Some(project_id) == selected_project_id {
                                    return;
                                }
                                let _ = item_weak.update(cx, |this, cx| {
                                    this.select_project(project_id, cx);
                                });
                            }),
                    );
                }
                let add_project_weak = weak.clone();
                menu.separator().item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/folder-new.svg", 13.0, theme.text_tertiary))
                            .child("New project…")
                    })
                    .on_click(move |_, _, cx| {
                        let _ = add_project_weak.update(cx, |this, cx| this.add_project(cx));
                    }),
                )
            });
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
                    .child("What should we build in\u{00a0}")
                    .child(project_selector)
                    .child("?"),
            )
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
}

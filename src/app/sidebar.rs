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

/// Height of a compact, single-line task row plus its measured separation.
/// Keeping both inside the virtualized item makes off-screen estimates exact.
const SIDEBAR_SESSION_CARD_HEIGHT: f32 = 32.0;
const SIDEBAR_SESSION_ROW_GAP: f32 = 2.0;
const SIDEBAR_SESSION_ROW_HEIGHT: f32 = SIDEBAR_SESSION_CARD_HEIGHT + SIDEBAR_SESSION_ROW_GAP;

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

/// Recency for sidebar ordering within a project. A submitted turn promotes the
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

/// One row of the virtualized sidebar project hierarchy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SidebarRow {
    /// A persisted project folder. Projectless folders use the first persisted
    /// project id as the stable identity for their consolidated group.
    Project(Uuid),
    /// Started sessions whose project was removed remotely.
    UnknownProject,
    /// A started session.
    Session(Uuid),
    /// Spacing between project folders.
    ProjectSpacer,
}

fn append_sidebar_project_rows(
    rows: &mut Vec<SidebarRow>,
    project_id: Option<Uuid>,
    sessions: &mut [Uuid],
    collapsed: bool,
    timestamps: &HashMap<Uuid, u64>,
) {
    sessions.sort_by_key(|session_id| std::cmp::Reverse(timestamps[session_id]));
    rows.push(project_id.map_or(SidebarRow::UnknownProject, SidebarRow::Project));
    if !collapsed {
        rows.extend(sessions.iter().copied().map(SidebarRow::Session));
    }
    rows.push(SidebarRow::ProjectSpacer);
}

fn build_sidebar_rows(
    projects: &[(Uuid, bool)],
    sessions: &[AgentSession],
    collapsed_projects: &HashSet<Uuid>,
) -> Vec<SidebarRow> {
    let mut groups: Vec<(Uuid, Vec<Uuid>)> = Vec::with_capacity(projects.len());
    let mut project_groups = HashMap::with_capacity(projects.len());
    let mut projectless_group = None;
    for &(project_id, projectless) in projects {
        let group_index = if projectless {
            *projectless_group.get_or_insert_with(|| {
                groups.push((project_id, Vec::new()));
                groups.len() - 1
            })
        } else {
            groups.push((project_id, Vec::new()));
            groups.len() - 1
        };
        project_groups.insert(project_id, group_index);
    }

    let mut timestamps = HashMap::with_capacity(sessions.len());
    let mut orphans = Vec::new();
    for session in sessions.iter().filter(|session| session.has_started()) {
        timestamps.insert(session.id, sidebar_session_timestamp(session));
        if let Some(&group_index) = project_groups.get(&session.project_id) {
            groups[group_index].1.push(session.id);
        } else {
            orphans.push(session.id);
        }
    }

    let mut rows = Vec::with_capacity(projects.len() + timestamps.len() + projects.len());
    for (project_id, mut sessions) in groups {
        append_sidebar_project_rows(
            &mut rows,
            Some(project_id),
            &mut sessions,
            collapsed_projects.contains(&project_id),
            &timestamps,
        );
    }
    if !orphans.is_empty() {
        append_sidebar_project_rows(
            &mut rows,
            None,
            &mut orphans,
            collapsed_projects.contains(&Uuid::nil()),
            &timestamps,
        );
    }
    rows
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
            .text_size(px(11.0))
            .line_height(px(0.0))
            .child(div().w(px(6.0)).h(px(6.0)).rounded(px(RADIUS_LG)).bg(dot))
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
            .tab_index(0)
            .w(px(26.0))
            .h(px(26.0))
            .flex_none()
            .rounded(px(RADIUS_DF))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .focus_visible(|style| style.border_1().border_color(theme.ring))
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.set_sidebar_visible(!this.sidebar_visible, cx);
                    cx.stop_propagation();
                }
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
            .rounded(px(RADIUS_DF))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
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
        let traffic_light_clearance = if window.is_fullscreen() {
            0.0
        } else {
            (TRAFFIC_LIGHT_CLEARANCE - 8.0).max(0.0)
        };
        div()
            .id("sidebar-titlebar")
            .h(px(48.0))
            .pl(px(8.0))
            .pr(px(8.0))
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
                        .w(px(traffic_light_clearance))
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
                        "icons/history-back.svg",
                        !self.session_navigation.back.is_empty(),
                        true,
                        cx,
                    ))
                    .child(self.render_history_button(
                        "navigate-forward",
                        "icons/history-forward.svg",
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
        let foreground = theme.on_inverse;
        let available = status == crate::updater::UpdateStatus::Available;
        let button = div()
            .id("sidebar-update")
            .track_focus(&self.updater_button_focus)
            .when(available, |button| button.tab_index(0))
            .w(px(UPDATER_BUTTON_COLLAPSED_WIDTH))
            .h(px(20.0))
            .flex_none()
            .overflow_hidden()
            .rounded(px(RADIUS_LG))
            .relative()
            .cursor_pointer()
            .bg(theme.gauge)
            .text_color(foreground)
            .text_size(px(11.0))
            .font_weight(FontWeight::NORMAL)
            .when(available, |button| {
                button
                    .hover(|style| style.bg(theme.primary_hover))
                    .focus_visible(|style| style.border_1().border_color(theme.ring))
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

    pub(super) fn profile_display_name(&self) -> SharedString {
        let name = self.state.profile_name.trim();
        if name.is_empty() {
            tr_cow!("profile.default_name").into()
        } else {
            SharedString::from(name.to_owned())
        }
    }

    pub(super) fn render_profile_avatar(&self, size: f32, theme: &Theme) -> AnyElement {
        if let Some(path) = self.state.profile_avatar_path.clone() {
            return img(path)
                .size(px(size))
                .flex_none()
                .rounded(px(RADIUS_LG))
                .object_fit(ObjectFit::Cover)
                .into_any_element();
        }
        let initial = self
            .profile_display_name()
            .graphemes(true)
            .next()
            .unwrap_or("W")
            .to_uppercase();
        div()
            .size(px(size))
            .flex_none()
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(theme.sidebar_border)
            .bg(theme.overlay)
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.42))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text)
            .child(SharedString::from(initial))
            .into_any_element()
    }

    fn render_sidebar_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let display_name = self.profile_display_name();
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("sidebar-profile", cx);
        let profile_trigger = div()
            .id("sidebar-profile-trigger")
            .h(px(34.0))
            .min_w_0()
            .max_w(px(190.0))
            .px(px(5.0))
            .rounded(px(RADIUS_DF))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .focus_visible(|style| style.border_1().border_color(theme.ring))
            .child(self.render_profile_avatar(26.0, &theme))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(display_name),
            );
        let profile_menu = dropdown_menu(
            profile_trigger,
            "sidebar-profile-menu",
            &handle,
            MenuAlign::AboveLeft,
            move |_| {
                let settings_weak = weak.clone();
                let usage_weak = weak.clone();
                vec![
                    MenuItem::new(tr!("common.settings"), move |window, cx| {
                        let _ = settings_weak.update(cx, |this, cx| {
                            this.open_settings_action(&OpenSettings, window, cx);
                            this.open_settings_page(SettingsPage::Profile, cx);
                        });
                    })
                    .icon("icons/settings.svg"),
                    MenuItem::new(tr!("settings.usage"), move |window, cx| {
                        let _ = usage_weak.update(cx, |this, cx| {
                            this.open_settings_action(&OpenSettings, window, cx);
                            this.open_settings_page(SettingsPage::Usage, cx);
                        });
                    })
                    .icon("icons/chart-column.svg"),
                ]
            },
        );
        div()
            .flex_none()
            .h(px(48.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .child(profile_menu)
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

        let rows = self.sidebar_rows_cached();
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
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div().px(px(8.0)).size_full().child(
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

    /// The sidebar row snapshot, rebuilt only when its topology inputs move.
    /// Pulse renders can then read the same flat virtualized list without
    /// regrouping or sorting every started session.
    fn sidebar_rows_cached(&self) -> Rc<Vec<SidebarRow>> {
        let mut fingerprint = 0x51de_ba5e_5eed_c0de;
        for project in &self.state.projects {
            fingerprint = mix_uuid(fingerprint, project.id);
            fingerprint = mix(
                fingerprint,
                (project.name == Project::PROJECTLESS_NAME) as u64,
            );
        }
        for session in &self.state.sessions {
            if !session.has_started() {
                continue;
            }
            fingerprint = mix_uuid(fingerprint, session.id);
            fingerprint = mix_uuid(fingerprint, session.project_id);
            fingerprint = mix(fingerprint, sidebar_session_timestamp(session));
        }
        // A set has no stable iteration order; combine order-independently.
        let collapsed = self
            .sidebar_collapsed_projects
            .iter()
            .fold(0u64, |combined, project_id| {
                combined.wrapping_add(mix_uuid(0, *project_id))
            });
        fingerprint = mix(
            mix(fingerprint, self.sidebar_collapsed_projects.len() as u64),
            collapsed,
        );
        if self.sidebar_rows_fingerprint.get() != Some(fingerprint) {
            *self.sidebar_rows_snapshot.borrow_mut() = Rc::new(self.sidebar_rows());
            self.sidebar_rows_fingerprint.set(Some(fingerprint));

            let mut project_ids = self
                .state
                .projects
                .iter()
                .map(|project| project.id)
                .collect::<HashSet<_>>();
            if self
                .sidebar_rows_snapshot
                .borrow()
                .contains(&SidebarRow::UnknownProject)
            {
                project_ids.insert(Uuid::nil());
            }
            self.sidebar_project_focuses
                .borrow_mut()
                .retain(|project_id, _| project_ids.contains(project_id));
            self.sidebar_project_plus_focuses
                .borrow_mut()
                .retain(|project_id, _| project_ids.contains(project_id));
        }
        self.sidebar_rows_snapshot.borrow().clone()
    }

    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let projects = self
            .state
            .projects
            .iter()
            .map(|project| (project.id, project.name == Project::PROJECTLESS_NAME))
            .collect::<Vec<_>>();
        build_sidebar_rows(
            &projects,
            &self.state.sessions,
            &self.sidebar_collapsed_projects,
        )
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
            SidebarRow::Project(project_id) => self
                .render_sidebar_project_row(Some(project_id), cx)
                .into_any_element(),
            SidebarRow::UnknownProject => {
                self.render_sidebar_project_row(None, cx).into_any_element()
            }
            SidebarRow::Session(session_id) => self
                .render_sidebar_session_item(session_id, cx)
                .into_any_element(),
            SidebarRow::ProjectSpacer => div().w_full().h(px(6.0)).into_any_element(),
        }
    }

    fn sidebar_project_focus(
        &self,
        project_id: Uuid,
        plus: bool,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        let focuses = if plus {
            &self.sidebar_project_plus_focuses
        } else {
            &self.sidebar_project_focuses
        };
        focuses
            .borrow_mut()
            .entry(project_id)
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    fn render_sidebar_project_row(
        &self,
        project_id: Option<Uuid>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let focus_id = project_id.unwrap_or_else(Uuid::nil);
        let collapsed = self.sidebar_collapsed_projects.contains(&focus_id);
        let project = project_id.and_then(|project_id| {
            self.state
                .projects
                .iter()
                .find(|project| project.id == project_id)
        });
        let project_name = project
            .map(Project::display_name)
            .unwrap_or_else(|| tr!("sidebar.unknown_project"));
        let row_focus = self.sidebar_project_focus(focus_id, false, cx);
        let plus_focus =
            project_id.map(|project_id| self.sidebar_project_focus(project_id, true, cx));
        let project_toggle = div()
            .id(SharedString::from(format!(
                "sidebar-project-toggle-{focus_id}"
            )))
            .track_focus(&row_focus)
            .tab_index(0)
            .h(px(30.0))
            .flex_1()
            .min_w_0()
            .rounded(px(RADIUS_DF))
            .flex()
            .items_center()
            .cursor_pointer()
            .focus_visible(|style| style.border_1().border_color(theme.ring))
            .hover(|element| element.bg(theme.sidebar_item_background))
            .active(|element| element.bg(theme.overlay_strong))
            // Match the titlebar toggle's 26 px control slot so the folder
            // glyph, navigation controls, and project title form one clean
            // vertical grid instead of drifting by a few pixels.
            .child(
                div()
                    .size(px(26.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("icons/folder.svg", 15.0, theme.text_tertiary)),
            )
            .child(
                div()
                    .ml(px(4.0))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(project_name)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_project(focus_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => {
                        this.toggle_sidebar_project(focus_id, cx);
                        cx.stop_propagation();
                    }
                    "left" if !collapsed => {
                        this.set_sidebar_project_collapsed(focus_id, true, cx);
                        cx.stop_propagation();
                    }
                    "right" if collapsed => {
                        this.set_sidebar_project_collapsed(focus_id, false, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }));

        div()
            .id(SharedString::from(format!("sidebar-project-{focus_id}")))
            .w_full()
            // Give the 30 px folder control its own measured layout slot. A
            // margin on a fixed-height virtual row is absorbed into that box;
            // this explicit slot leaves a real 3 px edge above and below it.
            .h(px(36.0))
            .rounded(px(RADIUS_DF))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(project_toggle)
            .when_some(
                project_id.zip(plus_focus),
                |row, (project_id, plus_focus)| {
                    let tooltip_name = project
                        .map(Project::display_name)
                        .unwrap_or_else(|| tr!("sidebar.unknown_project"));
                    row.child(
                        div()
                            .id(SharedString::from(format!(
                                "sidebar-project-plus-{project_id}"
                            )))
                            .track_focus(&plus_focus)
                            .tab_index(0)
                            .size(px(22.0))
                            .flex_none()
                            .rounded(px(RADIUS_DF))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .focus_visible(|style| style.border_1().border_color(theme.ring))
                            .hover(|element| element.bg(theme.overlay_strong))
                            .active(|element| element.bg(theme.overlay_strong))
                            .tooltip(Tooltip::text(tr!(
                                "sidebar.new_task_in_project",
                                project = tooltip_name
                            )))
                            .child(icon("icons/plus.svg", 12.0, theme.text_tertiary))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.create_sidebar_session_for(project_id, window, cx);
                            }))
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        this.create_sidebar_session_for(project_id, window, cx);
                                        cx.stop_propagation();
                                    }
                                },
                            )),
                    )
                },
            )
    }

    fn create_sidebar_session_for(
        &mut self,
        project_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(projectless) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(Project::is_projectless)
        else {
            return;
        };
        self.settings_page = None;
        if projectless {
            self.create_projectless_session(cx);
        } else {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
        window.focus(&self.composer_focus(cx), cx);
    }

    fn toggle_sidebar_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        let collapsed = !self.sidebar_collapsed_projects.contains(&project_id);
        self.set_sidebar_project_collapsed(project_id, collapsed, cx);
    }

    fn set_sidebar_project_collapsed(
        &mut self,
        project_id: Uuid,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.sidebar_collapsed_projects.insert(project_id)
        } else {
            self.sidebar_collapsed_projects.remove(&project_id)
        };
        if changed {
            cx.notify();
        }
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
                .h(px(20.0))
                .flex_1()
                .min_w_0()
                .px(px(4.0))
                .rounded(px(RADIUS_SM))
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
                .truncate()
                .text_size(px(13.0))
                .line_height(px(18.0))
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
            .h(px(SIDEBAR_SESSION_CARD_HEIGHT))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pl(px(12.0))
            .pr(px(8.0))
            .rounded(px(RADIUS_DF))
            .cursor_pointer()
            .when(selected, |element| {
                element.bg(theme.sidebar_item_background)
            })
            .hover(|element| element.bg(theme.sidebar_item_background))
            .active(|element| element.bg(theme.sidebar_item_background))
            .child(
                div()
                    .size(px(7.0))
                    .flex_none()
                    .rounded(px(RADIUS_LG))
                    .border_1()
                    .border_color(if working {
                        theme.success
                    } else {
                        status_color(&theme, session.status)
                    })
                    .when(working, |dot| dot.bg(theme.success))
                    .when(session.status == SessionStatus::Waiting, |dot| {
                        dot.bg(theme.warning)
                    })
                    .when(session.status == SessionStatus::Failed, |dot| {
                        dot.bg(theme.danger)
                    }),
            )
            .child(title)
            .when_some(
                session_time_label(session, unix_time()),
                |element, label| {
                    element.child(
                        div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(if session.is_busy() {
                                theme.success
                            } else {
                                theme.text_ghost
                            })
                            .child(SharedString::from(label)),
                    )
                },
            )
            .when(!renaming, |element| {
                element
                    .track_focus(&row_focus)
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.ring))
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
        let traffic_light_clearance = if window.is_fullscreen() {
            0.0
        } else {
            TRAFFIC_LIGHT_CLEARANCE
        };
        let session = self.selected_session();
        let title = session
            .map(localized_session_title)
            .unwrap_or_else(|| tr!("session.new_task"));
        let agent_preset_label = session
            .filter(|session| session.provider == ProviderKind::DeepSeek && session.has_started())
            .and_then(|session| self.agent_preset_label_for_session(session));
        let header_project_id = session
            .map(|session| session.project_id)
            .or(self.state.selected_project);
        let new_task_focus = self.transcript_control_focus("header-new-task", cx);
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
                px(14.0 + (traffic_light_clearance - self.sidebar_rendered_width).max(0.0))
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
                                .w(px((traffic_light_clearance - 8.0).max(0.0)))
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
                                        "icons/history-back.svg",
                                        !self.session_navigation.back.is_empty(),
                                        true,
                                        cx,
                                    ))
                                    .child(self.render_history_button(
                                        "navigate-forward",
                                        "icons/history-forward.svg",
                                        !self.session_navigation.forward.is_empty(),
                                        false,
                                        cx,
                                    )),
                            ),
                    )
            })
            .when_some(header_project_id, |element, project_id| {
                let project_name = self
                    .state
                    .projects
                    .iter()
                    .find(|project| project.id == project_id)
                    .map(Project::display_name)
                    .unwrap_or_else(|| tr!("sidebar.unknown_project"));
                element.child(
                    div()
                        .id("header-new-task")
                        .track_focus(&new_task_focus)
                        .tab_index(0)
                        .tab_stop(true)
                        .size(px(28.0))
                        .flex_none()
                        .rounded(px(RADIUS_DF))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .focus_visible(|style| style.border_1().border_color(theme.ring))
                        .hover(|style| style.bg(theme.overlay))
                        .active(|style| style.bg(theme.overlay_strong))
                        .tooltip(Tooltip::text(tr!(
                            "sidebar.new_task_in_project",
                            project = project_name
                        )))
                        .child(icon("icons/plus.svg", 14.0, theme.text_secondary))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.create_sidebar_session_for(project_id, window, cx);
                            cx.stop_propagation();
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.create_sidebar_session_for(project_id, window, cx);
                                cx.stop_propagation();
                            }
                        })),
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
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme.text)
                                .child(SharedString::from(title)),
                        )
                        .children(agent_preset_label.map(|label| {
                            div()
                                .h(px(22.0))
                                .max_w(px(180.0))
                                .px(px(6.0))
                                .rounded(px(RADIUS_DF))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .bg(theme.overlay)
                                .text_size(px(11.0))
                                .font_weight(FontWeight::NORMAL)
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
                        .font_weight(FontWeight::NORMAL)
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
                                .focus_visible(|style| style.border_1().border_color(theme.ring))
                                .h(px(32.0))
                                .px(px(14.0))
                                .rounded(px(RADIUS_LG))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .bg(theme.inverse)
                                .text_color(theme.on_inverse)
                                .text_size(px(12.5))
                                .font_weight(FontWeight::NORMAL)
                                .hover(|element| element.bg(theme.primary_hover))
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
                                .focus_visible(|style| style.border_1().border_color(theme.ring))
                                .h(px(30.0))
                                .px(px(12.0))
                                .rounded(px(RADIUS_LG))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .cursor_pointer()
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
                    .font_weight(FontWeight::NORMAL)
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

    fn started_session(
        project_id: Uuid,
        created_at: u64,
        last_reply_at: Option<u64>,
    ) -> AgentSession {
        let mut session = AgentSession::new(project_id, ProviderKind::Codex);
        session.created_at = created_at;
        session.last_reply_at = last_reply_at;
        session.begin_turn("Started");
        session.created_at = created_at;
        session.last_reply_at = last_reply_at;
        session
    }

    #[test]
    fn sidebar_projects_follow_persisted_order_and_include_empty_projects() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let empty = Uuid::from_u128(3);
        let first_session = started_session(first, 10, None);
        let second_session = started_session(second, 20, None);
        let rows = build_sidebar_rows(
            &[(second, false), (first, false), (empty, false)],
            &[first_session.clone(), second_session.clone()],
            &HashSet::new(),
        );

        assert_eq!(
            rows,
            vec![
                SidebarRow::Project(second),
                SidebarRow::Session(second_session.id),
                SidebarRow::ProjectSpacer,
                SidebarRow::Project(first),
                SidebarRow::Session(first_session.id),
                SidebarRow::ProjectSpacer,
                SidebarRow::Project(empty),
                SidebarRow::ProjectSpacer,
            ]
        );
    }

    #[test]
    fn collapsed_project_keeps_only_its_folder_row_and_spacer() {
        let project_id = Uuid::from_u128(1);
        let session = started_session(project_id, 10, None);
        let rows = build_sidebar_rows(
            &[(project_id, false)],
            &[session],
            &HashSet::from([project_id]),
        );

        assert_eq!(
            rows,
            vec![SidebarRow::Project(project_id), SidebarRow::ProjectSpacer,]
        );
    }

    #[test]
    fn project_sessions_are_newest_first() {
        let project_id = Uuid::from_u128(1);
        let older = started_session(project_id, 10, Some(20));
        let newer = started_session(project_id, 30, None);
        let rows = build_sidebar_rows(
            &[(project_id, false)],
            &[older.clone(), newer.clone()],
            &HashSet::new(),
        );

        assert_eq!(
            rows,
            vec![
                SidebarRow::Project(project_id),
                SidebarRow::Session(newer.id),
                SidebarRow::Session(older.id),
                SidebarRow::ProjectSpacer,
            ]
        );
    }

    #[test]
    fn orphan_started_sessions_are_preserved_under_unknown_project() {
        let orphan = started_session(Uuid::from_u128(99), 10, None);
        let rows = build_sidebar_rows(&[], &[orphan.clone()], &HashSet::new());

        assert_eq!(
            rows,
            vec![
                SidebarRow::UnknownProject,
                SidebarRow::Session(orphan.id),
                SidebarRow::ProjectSpacer,
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
}

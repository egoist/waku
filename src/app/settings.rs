use super::*;

const SETTINGS_CONTENT_MAX_WIDTH: f32 = 760.0;

impl Waku {
    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);

        div()
            .key_context("Waku")
            .on_action(|_: &CloseWindow, window, _| crate::platform::hide_window(window))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(cx.listener(Self::navigate_back_action))
            .on_action(cx.listener(Self::navigate_forward_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .capture_any_mouse_down(cx.listener(Self::navigation_mouse_down))
            .size_full()
            .flex()
            .bg(theme.canvas)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .child(self.render_settings_sidebar(cx))
            .child(self.render_settings_content(cx))
            .into_any_element()
    }

    fn render_settings_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let current_page = self.settings_page.unwrap_or(SettingsPage::Appearance);
        let query = self
            .settings_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let mut navigation = div().flex().flex_col().gap(px(3.0));

        for (page, label, icon_path, _keywords) in [
            (
                SettingsPage::General,
                "General",
                "icons/settings.svg",
                "general local projects conversations privacy",
            ),
            (
                SettingsPage::Appearance,
                "Appearance",
                "icons/appearance.svg",
                "appearance theme system light dark",
            ),
            (
                SettingsPage::ComputerUse,
                "Computer Use",
                "icons/cursor-spark.svg",
                "computer use screen recording accessibility apps control codex",
            ),
        ]
        .into_iter()
        .filter(|(_, _, _, keywords)| query.is_empty() || keywords.contains(query.as_str()))
        {
            let selected = current_page == page;
            navigation = navigation.child(
                div()
                    .id(SharedString::from(format!(
                        "settings-tab-{}",
                        label.to_ascii_lowercase()
                    )))
                    .h(px(36.0))
                    .px(px(11.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .cursor_default()
                    .text_size(px(13.0))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| {
                        element.bg(theme.sidebar_item_background)
                    })
                    .hover(|element| element.bg(theme.sidebar_item_background))
                    .active(|element| element.bg(theme.sidebar_item_background))
                    .child(icon(
                        icon_path,
                        15.0,
                        if selected {
                            theme.text_secondary
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_page = Some(page);
                        cx.notify();
                    })),
            );
        }

        div()
            .w(px(DEFAULT_SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(self.render_settings_sidebar_titlebar(cx))
            .child(
                div().px(px(12.0)).child(
                    div()
                        .id("settings-back")
                        .h(px(34.0))
                        .px(px(9.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .cursor_default()
                        .text_size(px(13.0))
                        .text_color(theme.text_secondary)
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(icon("icons/arrow-left.svg", 15.0, theme.text_tertiary))
                        .child("Back")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.settings_page = None;
                            let focus_handle = this.composer_focus(cx);
                            window.focus(&focus_handle, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                div().px(px(12.0)).pt(px(8.0)).child(
                    Input::new(&self.settings_search)
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .h(px(29.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .rounded(px(7.0))
                        .prefix(icon("icons/search.svg", 13.0, theme.text_tertiary)),
                ),
            )
            .child(div().h(px(18.0)))
            .child(div().px(px(12.0)).child(navigation))
    }

    fn render_settings_sidebar_titlebar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("settings-sidebar-titlebar")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .child(
                self.window_drag_region(
                    div()
                        .id("settings-sidebar-traffic-light-drag-region")
                        .w(px(TRAFFIC_LIGHT_CLEARANCE))
                        .h_full()
                        .flex_none(),
                    cx,
                ),
            )
            .child(
                self.render_settings_drag_region("settings-sidebar-titlebar-drag-region", cx)
                    .flex_1(),
            )
    }

    fn render_settings_content(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let page = self.settings_page.unwrap_or(SettingsPage::Appearance);

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.sidebar_border)
            .bg(theme.surface)
            .child(self.render_settings_drag_region("settings-content-titlebar", cx))
            .child(
                div()
                    .id("settings-content-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(32.0))
                    .pb(px(48.0))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(SETTINGS_CONTENT_MAX_WIDTH))
                            .mx_auto()
                            .child(
                                div()
                                    .pt(px(2.0))
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(match page {
                                        SettingsPage::General => "General",
                                        SettingsPage::ComputerUse => "Computer Use",
                                        SettingsPage::Appearance => "Appearance",
                                    }),
                            )
                            .child(match page {
                                SettingsPage::General => self.render_general_settings(cx),
                                SettingsPage::ComputerUse => self.render_computer_use_settings(cx),
                                SettingsPage::Appearance => self.render_appearance_settings(cx),
                            }),
                    ),
            )
    }

    fn render_general_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .mt(px(15.0))
            .w_full()
            .px(px(20.0))
            .py(px(14.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Local by default"),
            )
            .child(
                div()
                    .mt(px(5.0))
                    .text_size(px(12.5))
                    .line_height(px(18.0))
                    .text_color(theme.text_secondary)
                    .child("Projects, conversations, and settings are stored on this Mac."),
            )
            .into_any_element()
    }

    fn render_appearance_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected = self.state.theme;
        let weak = cx.entity().downgrade();
        let selector = MenuChip::new("theme-selector")
            .label(selected.label())
            .outlined()
            .w(px(116.0))
            .justify_between()
            .dropdown_menu(move |mut menu, _window, _cx| {
                menu = menu.min_w(px(148.0)).max_w(px(148.0));
                for preference in ThemePreference::ALL {
                    let weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(preference.label())
                            .selected(preference == selected)
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_theme_preference(preference, window, cx);
                                });
                            }),
                    );
                }
                menu
            })
            .anchor(Anchor::TopRight);

        div()
            .mt(px(15.0))
            .w_full()
            .min_h(px(60.0))
            .px(px(20.0))
            .py(px(12.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .flex()
            .items_center()
            .gap(px(24.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Theme"),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(px(12.5))
                            .line_height(px(18.0))
                            .text_color(theme.text_secondary)
                            .child("Choose between system, light, or dark themes."),
                    ),
            )
            .child(selector)
            .into_any_element()
    }

    fn render_computer_use_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let enabled = self.state.computer_use_enabled;
        let permissions = self.computer_permissions.clone();
        let pending = self.computer_permission_request_pending;
        let helper_name = crate::computer_use::helper_display_name();
        let mut allowed_apps = div().flex().flex_col().gap(px(1.0));
        if self.state.computer_use_allowed_apps.is_empty() {
            allowed_apps = allowed_apps.child(
                div()
                    .py(px(12.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_tertiary)
                    .child("No apps are always allowed. Task-scoped grants stay in memory only."),
            );
        } else {
            for (index, grant) in self.state.computer_use_allowed_apps.iter().enumerate() {
                let key = grant.key();
                let is_last = index + 1 == self.state.computer_use_allowed_apps.len();
                let app_icon = self.computer_use_app_icon(&grant.bundle_id, cx);
                allowed_apps = allowed_apps.child(
                    div()
                        .py(px(9.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .when(!is_last, |element| {
                            element.border_b_1().border_color(theme.border)
                        })
                        .child(
                            div()
                                .w(px(32.0))
                                .h(px(32.0))
                                .flex_none()
                                .rounded(px(7.0))
                                .when_some(app_icon, |element, app_icon| {
                                    element.child(img(app_icon).size_full().rounded(px(7.0)))
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(SharedString::from(grant.app_name.clone())),
                                )
                                .child(
                                    div()
                                        .mt(px(2.0))
                                        .text_size(px(9.5))
                                        .text_color(theme.text_tertiary)
                                        .truncate()
                                        .child(SharedString::from(grant.bundle_id.clone())),
                                ),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("revoke-computer-app-{key}")))
                                .h(px(25.0))
                                .px(px(9.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(theme.border_strong)
                                .flex()
                                .items_center()
                                .cursor_default()
                                .text_size(px(10.5))
                                .text_color(theme.text_secondary)
                                .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
                                .child("Revoke")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.revoke_computer_app(&key, cx);
                                })),
                        ),
                );
            }
        }

        div()
            .mt(px(15.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .flex()
                    .items_center()
                    .gap(px(20.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child("Let Waku use apps"),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(px(12.0))
                                    .line_height(px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(
                                        "Computer Use is available with Codex, OpenCode, Grok, and Pi.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("computer-use-enabled")
                            .w(px(36.0))
                            .h(px(20.0))
                            .p(px(2.0))
                            .rounded_full()
                            .cursor_default()
                            .bg(if enabled { theme.inverse } else { theme.inset })
                            .border_1()
                            .border_color(if enabled { theme.inverse } else { theme.border_strong })
                            .flex()
                            .items_center()
                            .when(enabled, |element| element.justify_end())
                            .child(
                                div()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .rounded_full()
                                    .bg(if enabled { theme.on_inverse } else { theme.text_tertiary }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_computer_use_enabled(!enabled, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("macOS access"),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(11.5))
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(format!(
                                "Grant access to {helper_name}, Waku's isolated control helper."
                            ))),
                    )
                    .child(permission_status_row(
                        "Screen Recording",
                        "Captures only the approved app window.",
                        permissions.screen_recording,
                        "screen-recording-settings",
                        theme,
                        cx,
                    ))
                    .child(permission_status_row(
                        "Accessibility",
                        "Posts pointer and keyboard events after approval.",
                        permissions.accessibility,
                        "accessibility-settings",
                        theme,
                        cx,
                    ))
                    .child(
                        div()
                            .mt(px(11.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("recheck-computer-permissions")
                                    .h(px(28.0))
                                    .px(px(11.0))
                                    .rounded(px(7.0))
                                    .border_1()
                                    .border_color(theme.border_strong)
                                    .text_color(theme.text_secondary)
                                    .flex()
                                    .items_center()
                                    .cursor_default()
                                    .text_size(px(10.5))
                                    .opacity(if pending { 0.6 } else { 1.0 })
                                    .child(if pending { "Checking…" } else { "Recheck" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request_computer_permissions(false, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Always allowed apps"),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(11.5))
                            .text_color(theme.text_secondary)
                            .child("Waku pins these grants to the app's bundle ID and signing team."),
                    )
                    .child(allowed_apps),
            )
            .into_any_element()
    }

    fn set_computer_use_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.state.computer_use_enabled = enabled;
        self.save();
        if enabled {
            self.request_computer_permissions(true, cx);
        }
        cx.notify();
    }

    pub(super) fn request_computer_permissions(&mut self, prompt: bool, cx: &mut Context<Self>) {
        if self.computer_permission_request_pending {
            return;
        }
        self.computer_permission_request_pending = true;
        let tx = self.computer_permission_tx.clone();
        std::thread::Builder::new()
            .name("waku-computer-permission-request".into())
            .spawn(move || {
                let result = crate::computer_use::probe_permissions(prompt)
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
            })
            .ok();
        cx.notify();
    }

    fn revoke_computer_app(&mut self, key: &str, cx: &mut Context<Self>) {
        self.state
            .computer_use_allowed_apps
            .retain(|grant| grant.key() != key);
        self.save();
        cx.notify();
    }

    fn computer_use_app_icon(
        &self,
        bundle_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<gpui::Image>> {
        if let Some(icon) = self.computer_use_app_icons.borrow().get(bundle_id) {
            return icon.clone();
        }

        let bundle_id = bundle_id.to_owned();
        if self
            .computer_use_app_icon_loads
            .borrow_mut()
            .insert(bundle_id.clone())
        {
            cx.spawn(async move |this, cx| {
                let load_bundle_id = bundle_id.clone();
                let icon =
                    cx.background_executor()
                        .spawn(async move {
                            crate::platform::load_app_icon_for_bundle_id(&load_bundle_id)
                        })
                        .await;
                let _ = this.update(cx, |this, cx| {
                    this.computer_use_app_icon_loads
                        .borrow_mut()
                        .remove(&bundle_id);
                    this.computer_use_app_icons
                        .borrow_mut()
                        .insert(bundle_id, icon);
                    cx.notify();
                });
            })
            .detach();
        }
        None
    }

    fn render_settings_drag_region(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(48.0))
            .flex_none()
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

    fn set_theme_preference(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.theme == preference {
            return;
        }
        self.state.theme = preference;
        crate::theme::apply_theme_preference(preference, window, cx);
        self.save();
        cx.notify();
    }
}

fn permission_status_row(
    name: &'static str,
    description: &'static str,
    granted: bool,
    id: &'static str,
    theme: Theme,
    cx: &mut Context<Waku>,
) -> Div {
    let status = if granted {
        div()
            .id(id)
            .h(px(25.0))
            .px(px(4.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(px(10.0))
            .text_color(theme.success)
            .child(icon("icons/check.svg", 12.0, theme.success))
            .child("Access Granted")
    } else {
        div()
            .id(id)
            .h(px(25.0))
            .px(px(9.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .cursor_default()
            .text_size(px(10.0))
            .text_color(theme.text_secondary)
            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
            .child("Grant Access")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request_computer_permissions(true, cx);
            }))
    };

    div()
        .mt(px(10.0))
        .pt(px(10.0))
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(name),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_tertiary)
                        .child(description),
                ),
        )
        .child(status)
}

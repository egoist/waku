use super::*;

fn should_render_empty_state(session: Option<&AgentSession>) -> bool {
    session
        .map(|session| session.detail_loaded && session.messages.is_empty())
        .unwrap_or(true)
}

impl Waku {
    pub(super) fn render_panel_resize_handle(
        &self,
        id: &'static str,
        target: PanelResizeTarget,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let active = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == target);
        // The right panel's left edge abuts the browser webview, a native view
        // that composites above every base-scene pixel at or beyond the edge.
        // Its bar and hover strip therefore sit entirely left of the edge,
        // where GPUI still owns rendering and input; the other edges keep the
        // conventional straddle.
        let (strip_left, strip_width) = match target {
            PanelResizeTarget::RightPanel => (-7.0, 8.0),
            PanelResizeTarget::Sidebar | PanelResizeTarget::FileTree => (-5.0, 10.0),
        };
        div()
            .id(id)
            .absolute()
            .top_0()
            .left(px(strip_left))
            .w(px(strip_width))
            .h_full()
            .group("panel-resize-handle")
            .cursor_col_resize()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(5.0))
                    .w(px(2.0))
                    .h_full()
                    .bg(if active {
                        theme.resize_handle
                    } else {
                        gpui::transparent_black()
                    })
                    .group_hover("panel-resize-handle", |element| {
                        element.bg(theme.resize_handle)
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    this.begin_panel_resize(target, event, window, cx);
                }),
            )
    }
}

impl Waku {
    /// Width left for the chat column once the visible panels take theirs.
    fn chat_viewport_width(&self, window: &Window) -> f32 {
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        f32::from(window.viewport_size().width)
            - if self.sidebar_visible {
                sidebar_width
            } else {
                0.0
            }
            - if self.right_panel_visible {
                right_panel_width
            } else {
                0.0
            }
    }

    /// [`WakuPane`] delegate for the sidebar island.
    pub(super) fn sidebar_pane_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (sidebar_width, _) = self.effective_panel_widths(window);
        self.render_sidebar(sidebar_width, window, cx)
            .into_any_element()
    }

    /// [`WakuPane`] delegate for the transcript island.
    pub(super) fn transcript_pane_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat_viewport_width = self.chat_viewport_width(window);
        // The transcript's own element sizes itself with `flex_1`, which only
        // stretches inside a flex parent. A cached pane lays its content out
        // as a root, so give it that parent here or its height collapses to
        // the zero flex basis.
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(self.render_transcript(window, chat_viewport_width, cx))
            .into_any_element()
    }

    /// [`WakuPane`] delegate for the right-panel island.
    pub(super) fn right_panel_pane_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (_, right_panel_width) = self.effective_panel_widths(window);
        self.render_right_panel(right_panel_width, window, cx)
            .into_any_element()
    }

    /// Measure live frame rate by counting renders over a sliding one-second
    /// window and keep requesting animation frames so the counter stays current.
    fn tick_fps(&mut self, window: &Window) {
        let now = Instant::now();
        self.fps_frame_count = self.fps_frame_count.saturating_add(1);
        if now.duration_since(self.fps_last_frame) >= Duration::from_secs(1) {
            self.fps_value = self.fps_frame_count as u32;
            self.fps_frame_count = 0;
            self.fps_last_frame = now;
        }
        window.request_animation_frame();
    }
}

impl Render for Waku {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything can early-return (the settings page below), settle
        // whether each native browser webview belongs on screen this frame —
        // it floats above everything GPUI paints.
        self.sync_browser_webviews(cx);
        if self.fps_counter_visible {
            self.tick_fps(window);
        }
        let image_preview = self.render_image_preview(cx);
        if self.settings_page.is_some() {
            let command_palette = self.render_command_palette(window, cx);
            let commit_dialog = self.render_commit_dialog(cx);
            let content = div()
                .relative()
                .size_full()
                .on_action(cx.listener(Self::toggle_command_palette_action))
                .child(self.render_settings(window, cx))
                .children(command_palette)
                .children(commit_dialog)
                .children(image_preview)
                .into_any_element();
            return self.render_window_frame(content, window, cx);
        }
        // Re-armed every frame this window shows time labels; parks while
        // settings covers them and while the window isn't drawing at all.
        self.schedule_time_label_wake(cx);

        let theme = Theme::current(cx);
        let empty = should_render_empty_state(self.selected_session());
        let permission = self.render_permission(cx);
        let computer_use = self.render_computer_use_overlay(cx);
        let command_palette = self.render_command_palette(window, cx);
        let commit_dialog = self.render_commit_dialog(cx);
        self.start_toast_dismiss_timer(cx);
        let toast = self
            .toast
            .as_ref()
            .map(|toast| (toast.message.clone(), toast.tone, toast.id));
        let toast = toast.map(|(message, tone, generation)| {
            self.render_toast(message, tone, generation, cx)
                .into_any_element()
        });
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let content = div()
            .key_context("Waku")
            .on_action(cx.listener(Self::close_window_or_right_panel_tab_action))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::new_project_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(cx.listener(Self::toggle_command_palette_action))
            .on_action(cx.listener(Self::toggle_fps_counter_action))
            .on_action(cx.listener(Self::navigate_back_action))
            .on_action(cx.listener(Self::navigate_forward_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::toggle_model_picker_action))
            .on_action(cx.listener(Self::toggle_usage_panel_action))
            .on_action(cx.listener(Self::save_right_panel_file_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .on_action(cx.listener(Self::copy_selection_action))
            .on_action(cx.listener(Self::open_find_action))
            .on_action(cx.listener(Self::open_find_replace_action))
            .on_action(cx.listener(Self::close_find_action))
            .on_action(cx.listener(Self::find_next_action))
            .on_action(cx.listener(Self::find_previous_action))
            .on_action(cx.listener(Self::toggle_find_case_action))
            .on_action(cx.listener(Self::toggle_find_whole_word_action))
            .on_action(cx.listener(Self::toggle_find_regex_action))
            .on_action(cx.listener(Self::replace_all_matches_action))
            .capture_any_mouse_down(cx.listener(Self::navigation_mouse_down))
            .on_mouse_move(cx.listener(Self::resize_panel_mouse_move))
            .capture_any_mouse_up(cx.listener(Self::finish_panel_resize))
            .size_full()
            .relative()
            .flex()
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .when(self.sidebar_visible, |root| {
                root.child(self.sidebar_pane.clone().cached(
                    StyleRefinement::default()
                        .w(px(sidebar_width))
                        .h_full()
                        .flex_none(),
                ))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .bg(theme.surface)
                    .when(self.sidebar_visible, |element| {
                        element.border_l_1().border_color(theme.sidebar_border)
                    })
                    .child(self.render_header(window, cx))
                    .child(if empty {
                        self.render_empty_state(cx).into_any_element()
                    } else {
                        self.transcript_pane
                            .clone()
                            .cached(
                                StyleRefinement::default().flex_1().min_h(px(0.0)).w_full(),
                            )
                            .into_any_element()
                    })
                    .children(permission)
                    .when(self.selected_project().is_some(), |element| {
                        element
                            .children(self.render_queued_messages(cx))
                            .child(self.render_composer(window, cx))
                            .child(self.render_workspace_footer(cx))
                    })
                    .relative()
                    .children(toast)
                    .children(computer_use)
                    .when(self.sidebar_visible, |element| {
                        element.child(self.render_panel_resize_handle(
                            "sidebar-resize-handle",
                            PanelResizeTarget::Sidebar,
                            cx,
                        ))
                    }),
            )
            .when(self.right_panel_visible, |root| {
                root.child(self.right_panel_pane.clone().cached(
                    StyleRefinement::default()
                        .w(px(right_panel_width))
                        .h_full()
                        .flex_none(),
                ))
            })
            .children(command_palette)
            .children(commit_dialog)
            .children(image_preview)
            .into_any_element();

        self.render_window_frame(content, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloaded_history_never_renders_the_new_task_prompt() {
        let mut stored = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        stored.detail_loaded = false;

        assert!(!should_render_empty_state(Some(&stored)));

        let draft = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        assert!(should_render_empty_state(Some(&draft)));
        assert!(should_render_empty_state(None));
    }
}

impl Waku {
    fn render_toast(
        &self,
        message: String,
        tone: ToastTone,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let (status_icon, status_color) = match tone {
            ToastTone::Alert => ("icons/alert.svg", theme.danger),
            ToastTone::Success => ("icons/check.svg", theme.success),
        };
        let palette = MarkdownPalette::from_theme(&theme);
        let text_ctx = MarkdownCtx::new(
            format!("toast-{generation}"),
            &palette,
            MarkdownMetrics::COMPACT,
            self.toast_selection.clone(),
        );
        let message = md::render::plain_text(
            message,
            md::render::SANS_FAMILY,
            FontWeight::NORMAL,
            theme.text,
            &text_ctx,
        );
        let dismiss = div()
            .id(SharedString::from(format!("dismiss-toast-{generation}")))
            .tab_index(0)
            .size(px(26.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tr!("common.dismiss_notification")))
            .child(icon("icons/x.svg", 12.0, theme.text_tertiary))
            .on_click(cx.listener(|this, _, _, cx| {
                this.hide_toast();
                cx.notify();
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space" | "escape") {
                    this.hide_toast();
                    cx.notify();
                    cx.stop_propagation();
                }
            }));

        div()
            .id(SharedString::from(format!("toast-layer-{generation}")))
            .absolute()
            .left_0()
            .top(px(56.0))
            .w_full()
            .px(px(20.0))
            .flex()
            .justify_center()
            .child(
                div()
                    .id(SharedString::from(format!("toast-{generation}")))
                    .occlude()
                    .max_w(px(560.0))
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(theme.text)
                    .on_hover(cx.listener(|this, hovering: &bool, _, cx| {
                        this.set_toast_hovered(*hovering, cx);
                    }))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(md::render::frame_reset(self.toast_selection.clone()))
                    .child(icon(status_icon, 14.0, status_color))
                    .child(div().flex_1().min_w_0().whitespace_normal().child(message))
                    .child(dismiss)
                    .child(self.toast_selection_input()),
            )
            // Keep the toast top-centered just beneath Waku's 48px header.
            // GPUI's animation path honors the system reduce-motion preference
            // and resolves immediately.
            .with_animation(
                SharedString::from(format!("toast-enter-{generation}")),
                Animation::new(TOAST_ANIMATION_DURATION).with_easing(ease_out_quint()),
                |element, delta| {
                    element
                        .top(px(48.0 + 8.0 * delta))
                        .opacity(0.4 + 0.6 * delta)
                },
            )
    }
}

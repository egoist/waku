use super::*;

impl Waku {
    // ── Permission ─────────────────────────────────────────────────────────

    pub(super) fn render_permission(&self, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(permission) = self.selected_runtime()?.pending_computer_approval.as_ref() {
            return Some(self.render_computer_permission(permission, cx));
        }
        let permission = self.selected_runtime()?.pending_permission.as_ref()?;
        let theme = Theme::current(cx);
        let request_id = permission.request_id.clone();
        let mut buttons = div().flex().items_center().gap(px(8.0)).mt(px(10.0));
        for option in &permission.options {
            let request_id = request_id.clone();
            let option_id = option.id.clone();
            let allow = option.allow;
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "permission-{}-{}",
                        permission.request_id, option.id
                    )))
                    .h(px(28.0))
                    .px(px(13.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(allow, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!allow, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(SharedString::from(option.label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_permission(request_id.clone(), option_id.clone(), cx);
                    })),
            );
        }
        Some(
            div().px(px(20.0)).pb(px(8.0)).child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_md()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/alert.svg", 13.0, theme.warning))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(permission.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .id("permission-detail")
                            .mt(px(8.0))
                            .max_h(px(92.0))
                            .overflow_y_scroll()
                            .p(px(8.0))
                            .rounded(px(7.0))
                            .bg(theme.inset)
                            .font_family("SF Mono")
                            .text_size(px(10.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_secondary)
                            .whitespace_normal()
                            .child(SharedString::from(permission.detail.clone())),
                    )
                    .child(buttons),
            ),
        )
    }

    fn render_computer_permission(
        &self,
        permission: &PendingComputerApproval,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let target = &permission.target;
        let mut buttons = div().mt(px(12.0)).flex().items_center().gap(px(8.0));
        let mut options = vec![("task", "Allow for task", true), ("deny", "Deny", false)];
        if target.persistable() {
            options.insert(1, ("always", "Always allow app", false));
        }
        for (decision, label, primary) in options {
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "computer-permission-{}-{decision}",
                        permission.request.call_id
                    )))
                    .h(px(29.0))
                    .px(px(13.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(primary, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!primary, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_computer_permission(decision, cx);
                    })),
            );
        }

        div().px(px(20.0)).pb(px(8.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .p(px(13.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.warning.opacity(0.5))
                .bg(theme.raised)
                .shadow_md()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .child(icon("icons/globe.svg", 14.0, theme.warning))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(format!("Allow Waku to control {}?", target.app_name)),
                        ),
                )
                .child(
                    div()
                        .mt(px(7.0))
                        .text_size(px(10.0))
                        .line_height(px(14.0))
                        .text_color(theme.text_secondary)
                        .child("A screenshot of this window will be shared with the active Codex model."),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .p(px(9.0))
                        .rounded(px(8.0))
                        .bg(theme.inset)
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .truncate()
                                .child(SharedString::from(target.window_title.clone())),
                        )
                        .child(
                            div()
                                .mt(px(4.0))
                                .text_size(px(10.5))
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(permission.request.summary())),
                        )
                        .when(permission.sensitive, |element| {
                            element.child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(px(10.5))
                                    .text_color(theme.warning)
                                    .child("This action can type text or press keys."),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(7.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_tertiary)
                        .child(if target.persistable() {
                            format!("Bundle ID: {}", target.bundle_id)
                        } else {
                            "This app has no bundle identifier, so it cannot be always allowed."
                                .into()
                        }),
                )
                .child(buttons),
        )
    }

    pub(super) fn render_computer_use_overlay(&self, cx: &mut Context<Self>) -> Option<Div> {
        let previews = self
            .selected_runtime()?
            .computer_use_previews
            .iter()
            .filter(|state| state.visible && state.phase != ComputerUsePhase::AwaitingApproval)
            .collect::<Vec<_>>();
        if previews.is_empty() {
            return None;
        }
        let theme = Theme::current(cx);
        let stack_x_offset = 14.0;
        let stack_y_offset = 24.0;
        let deepest_x_offset = (previews.len().saturating_sub(1) as f32) * stack_x_offset;
        let deepest_y_offset = (previews.len().saturating_sub(1) as f32) * stack_y_offset;
        let top_index = previews.len() - 1;
        let cards = previews
            .into_iter()
            .enumerate()
            .filter_map(|(index, state)| {
                let target = state.target.as_ref()?;
                let window_id = target.window_id;
                let app_name = target.app_name.clone();
                let app_initial = app_name.chars().next().unwrap_or('W').to_string();
                let title = target.window_title.clone();
                let screenshot = state.screenshot.clone();
                let active = state.phase == ComputerUsePhase::Running;
                let failed = state.phase == ComputerUsePhase::Failed;
                let is_top = index == top_index;
                let depth = (top_index - index) as f32;
                let x_offset = depth * stack_x_offset;
                let y_offset = depth * stack_y_offset;
                let status_color = if failed {
                    theme.danger
                } else if active {
                    theme.warning
                } else {
                    theme.accent
                };
                let status = if failed {
                    "Stopped"
                } else if active {
                    "Controlling"
                } else {
                    "Captured"
                };

                Some(
                    div()
                        .id(SharedString::from(format!(
                            "computer-use-preview-{window_id}"
                        )))
                        .absolute()
                        .right(px(x_offset))
                        .bottom(px(y_offset))
                        .w(px(304.0))
                        .h(px(220.0))
                        .p(px(6.0))
                        .rounded(px(16.0))
                        .overflow_hidden()
                        .border_1()
                        .border_color(if is_top {
                            theme.border_strong
                        } else {
                            theme.border
                        })
                        .bg(theme.raised)
                        .shadow_lg()
                        .cursor_default()
                        .when(!is_top, |element| element.opacity(0.96))
                        .child(
                            div()
                                .h(px(38.0))
                                .px(px(5.0))
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .w(px(27.0))
                                        .h(px(27.0))
                                        .rounded(px(7.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(theme.overlay_strong)
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text_secondary)
                                        .child(SharedString::from(app_initial)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text)
                                                .truncate()
                                                .child(SharedString::from(app_name)),
                                        )
                                        .child(
                                            div()
                                                .mt(px(1.0))
                                                .text_size(px(9.5))
                                                .text_color(theme.text_tertiary)
                                                .truncate()
                                                .child(SharedString::from(title)),
                                        ),
                                )
                                .when(is_top, |element| {
                                    element.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "computer-use-preview-action-{window_id}"
                                            )))
                                            .h(px(27.0))
                                            .px(px(10.0))
                                            .rounded(px(7.0))
                                            .border_1()
                                            .border_color(theme.border_strong)
                                            .flex()
                                            .items_center()
                                            .cursor_default()
                                            .text_size(px(10.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if active {
                                                theme.danger
                                            } else {
                                                theme.text_secondary
                                            })
                                            .hover(|element| element.bg(theme.overlay))
                                            .active(|element| element.opacity(0.8))
                                            .child(if active { "Take Control" } else { "Close" })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                if active {
                                                    this.cancel_turn(cx);
                                                } else {
                                                    this.dismiss_computer_use(window_id, cx);
                                                }
                                            })),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .relative()
                                .h(px(170.0))
                                .w_full()
                                .rounded(px(11.0))
                                .overflow_hidden()
                                .bg(rgb(0x101010))
                                .when_some(screenshot, |element, screenshot| {
                                    element.child(
                                        img(screenshot)
                                            .w_full()
                                            .h_full()
                                            .object_fit(ObjectFit::Contain),
                                    )
                                })
                                .when(state.screenshot.is_none(), |element| {
                                    element.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .justify_center()
                                            .gap(px(9.0))
                                            .child(
                                                div()
                                                    .w(px(34.0))
                                                    .h(px(23.0))
                                                    .rounded(px(5.0))
                                                    .border_1()
                                                    .border_color(theme.text_tertiary)
                                                    .child(
                                                        div()
                                                            .mt(px(4.0))
                                                            .ml(px(25.0))
                                                            .w(px(3.0))
                                                            .h(px(3.0))
                                                            .rounded_full()
                                                            .bg(status_color),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.0))
                                                    .text_color(theme.text_tertiary)
                                                    .child("Preparing preview…"),
                                            ),
                                    )
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .top(px(8.0))
                                        .left(px(8.0))
                                        .h(px(24.0))
                                        .px(px(8.0))
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .bg(theme.canvas.opacity(0.86))
                                        .border_1()
                                        .border_color(theme.border)
                                        .child(
                                            div()
                                                .w(px(6.0))
                                                .h(px(6.0))
                                                .rounded_full()
                                                .bg(status_color),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(9.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text)
                                                .child(status),
                                        ),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.bring_computer_use_to_front(window_id, cx);
                        })),
                )
            })
            .collect::<Vec<_>>();

        Some(
            div()
                .absolute()
                .right(px(16.0))
                .bottom(px(82.0))
                .w(px(304.0 + deepest_x_offset))
                .h(px(220.0 + deepest_y_offset))
                .children(cards),
        )
    }

    // ── Composer ───────────────────────────────────────────────────────────

    pub(super) fn render_provider_model_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let selected_model = session.and_then(|session| self.model_for_session(session));
        let selected_model_name = self.model_display_name(provider, selected_model);
        let locked_provider = session
            .filter(|session| !session.messages.is_empty())
            .map(|session| session.provider);
        let picker_enabled = session.is_some_and(|session| session.can_choose_model(provider));

        if !picker_enabled {
            return div()
                .h(px(24.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(icon(
                    provider_icon(provider),
                    10.5,
                    provider_color(&theme, provider).opacity(0.9),
                ))
                .child(
                    div()
                        .max_w(px(210.0))
                        .truncate()
                        .text_color(theme.text_secondary)
                        .child(SharedString::from(selected_model_name)),
                )
                .into_any_element();
        }

        let search_query = self.model_search.read(cx).value().to_string();
        let normalized_query = search_query.trim().to_ascii_lowercase();
        let searching = !normalized_query.is_empty();
        let selected_tab = self.model_picker_tab;
        let selected_model = selected_model.map(str::to_owned);
        let probes = self.probes.clone();
        let pending_discoveries = self.provider_model_discoveries_pending.clone();
        let favorites = self.state.favorite_models.clone();
        let weak = cx.entity().downgrade();
        let search = self.model_search.clone();
        let search_focus = search.read(cx).focus_handle(cx);

        let trigger = MenuChip::new("composer-provider-model")
            .icon(
                provider_icon(provider),
                provider_color(&theme, provider).opacity(0.9),
            )
            .label(selected_model_name);

        Popover::new("provider-model-picker")
            .anchor(Anchor::BottomLeft)
            .appearance(false)
            .track_focus(&search_focus)
            .on_open_change({
                let weak = weak.clone();
                let search = search.clone();
                move |open, window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if *open {
                            let provider = this
                                .selected_session()
                                .map(|session| session.provider)
                                .unwrap_or_default();
                            this.model_picker_tab = ModelPickerTab::Provider(provider);
                            this.request_provider_model_discovery(provider);
                            search.update(cx, |search, cx| {
                                search.set_value("", window, cx);
                            });
                        } else {
                            let focus_handle = this.composer.read(cx).focus();
                            window.focus(&focus_handle, cx);
                        }
                        cx.notify();
                    });
                }
            })
            .trigger(trigger)
            .content(move |_state, _window, popover_cx| {
                let popover = popover_cx.entity();
                let mut available_models = probes
                    .iter()
                    .filter(|probe| probe.installed)
                    .flat_map(|probe| {
                        probe
                            .models
                            .iter()
                            .cloned()
                            .map(move |model| (probe.provider, model))
                    })
                    .filter(|(kind, _)| locked_provider.is_none() || locked_provider == Some(*kind))
                    .filter(|(kind, model)| {
                        if searching {
                            let searchable = format!(
                                "{} {} {} {}",
                                model.name,
                                model.id,
                                kind.short_name(),
                                model.sub_provider.as_deref().unwrap_or("")
                            )
                            .to_ascii_lowercase();
                            return normalized_query
                                .split_whitespace()
                                .all(|token| searchable.contains(token));
                        }
                        match selected_tab {
                            ModelPickerTab::Favorites => favorites.iter().any(|favorite| {
                                favorite.provider == *kind && favorite.model == model.id
                            }),
                            ModelPickerTab::Provider(provider) => provider == *kind,
                        }
                    })
                    .collect::<Vec<_>>();
                if !searching && selected_tab == ModelPickerTab::Favorites {
                    available_models.sort_by_key(|(kind, model)| {
                        favorites
                            .iter()
                            .position(|favorite| {
                                favorite.provider == *kind && favorite.model == model.id
                            })
                            .unwrap_or(usize::MAX)
                    });
                }

                let mut sidebar = div()
                    .w(px(50.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .p(px(5.0))
                    .rounded_tl(px(12.0))
                    .rounded_bl(px(12.0))
                    .bg(theme.canvas)
                    .border_r_1()
                    .border_color(theme.border);

                let favorites_selected = selected_tab == ModelPickerTab::Favorites && !searching;
                let favorite_weak = weak.clone();
                sidebar = sidebar
                    .child(
                        div()
                            .id("model-tab-favorites")
                            .w(px(38.0))
                            .h(px(38.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(favorites_selected, |element| {
                                element.bg(theme.overlay_strong)
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .child(icon(
                                "icons/star.svg",
                                17.0,
                                if favorites_selected {
                                    theme.text
                                } else {
                                    theme.text_tertiary
                                },
                            ))
                            .on_click(move |_, _, cx| {
                                let _ = favorite_weak.update(cx, |this, cx| {
                                    this.select_model_picker_tab(ModelPickerTab::Favorites, cx);
                                });
                            }),
                    )
                    .child(div().w(px(34.0)).h(px(1.0)).my(px(3.0)).bg(theme.border));

                for kind in ProviderKind::ALL {
                    let installed = probes
                        .iter()
                        .find(|probe| probe.provider == kind)
                        .map(|probe| probe.installed)
                        .unwrap_or(false);
                    let allowed = locked_provider.is_none() || locked_provider == Some(kind);
                    let selected = selected_tab == ModelPickerTab::Provider(kind) && !searching;
                    let tab_weak = weak.clone();
                    sidebar = sidebar.child(
                        div()
                            .id(SharedString::from(format!("model-tab-{}", kind.id())))
                            .w(px(38.0))
                            .h(px(38.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(selected, |element| element.bg(theme.overlay_strong))
                            .when(!installed || !allowed, |element| element.opacity(0.35))
                            .when(installed && allowed, |element| {
                                element.hover(|element| element.bg(theme.overlay)).on_click(
                                    move |_, _, cx| {
                                        let _ = tab_weak.update(cx, |this, cx| {
                                            this.select_model_picker_tab(
                                                ModelPickerTab::Provider(kind),
                                                cx,
                                            );
                                        });
                                    },
                                )
                            })
                            .child(icon(
                                provider_icon(kind),
                                18.0,
                                provider_color(&theme, kind).opacity(if selected {
                                    1.0
                                } else {
                                    0.82
                                }),
                            )),
                    );
                }

                let search_input = div()
                    .h(px(52.0))
                    .px(px(12.0))
                    .pt(px(10.0))
                    .pb(px(8.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        Input::new(&search)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .bg(theme.raised)
                            .rounded(px(9.0))
                            .prefix(icon("icons/search.svg", 15.0, theme.text_secondary)),
                    );

                let mut rows = div()
                    .id("model-picker-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(9.0));
                if available_models.is_empty() {
                    let label = if searching {
                        "No models found"
                    } else if selected_tab == ModelPickerTab::Favorites {
                        "Star a model to keep it here"
                    } else if matches!(
                        selected_tab,
                        ModelPickerTab::Provider(provider)
                            if pending_discoveries.contains(&provider)
                    ) {
                        "Loading models…"
                    } else {
                        "No models reported by this provider"
                    };
                    rows = rows.child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.5))
                            .text_color(theme.text_ghost)
                            .child(label),
                    );
                }

                for (kind, model) in available_models {
                    let is_selected =
                        kind == provider && selected_model.as_deref() == Some(model.id.as_str());
                    let is_favorite = favorites
                        .iter()
                        .any(|favorite| favorite.provider == kind && favorite.model == model.id);
                    let model_id = model.id.clone();
                    let select_weak = weak.clone();
                    let select_popover = popover.clone();
                    let favorite_model_id = model.id.clone();
                    let favorite_weak = weak.clone();
                    let subtitle = model.sub_provider.as_deref().map_or_else(
                        || kind.short_name().to_owned(),
                        |sub_provider| format!("{sub_provider} · {}", kind.short_name()),
                    );
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!(
                                "model-row-{}-{}",
                                kind.id(),
                                model.id
                            )))
                            .h(px(58.0))
                            .px(px(12.0))
                            .rounded(px(9.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_default()
                            .when(is_selected, |element| element.bg(theme.overlay_strong))
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.opacity(0.85))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(SharedString::from(model.name)),
                                    )
                                    .child(
                                        div()
                                            .mt(px(4.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .child(icon(
                                                provider_icon(kind),
                                                10.5,
                                                provider_color(&theme, kind).opacity(0.85),
                                            ))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(11.0))
                                                    .text_color(theme.text_tertiary)
                                                    .child(SharedString::from(subtitle)),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "favorite-model-{}-{}",
                                        kind.id(),
                                        model.id
                                    )))
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .child(icon(
                                        if is_favorite {
                                            "icons/star-filled.svg"
                                        } else {
                                            "icons/star.svg"
                                        },
                                        14.0,
                                        if is_favorite {
                                            theme.favorite
                                        } else {
                                            theme.text_ghost
                                        },
                                    ))
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        let _ = favorite_weak.update(cx, |this, cx| {
                                            this.toggle_favorite_model(
                                                kind,
                                                favorite_model_id.clone(),
                                                cx,
                                            );
                                        });
                                    }),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = select_weak.update(cx, |this, cx| {
                                    this.choose_model(kind, model_id.clone(), cx);
                                });
                                select_popover.update(cx, |popover, cx| {
                                    popover.dismiss(window, cx);
                                });
                            }),
                    );
                }

                div()
                    .w(px(460.0))
                    .h(px(390.0))
                    .rounded(px(13.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_lg()
                    .flex()
                    .child(sidebar)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .rounded_tr(px(12.0))
                            .rounded_br(px(12.0))
                            .bg(theme.surface)
                            .child(search_input)
                            .child(rows),
                    )
            })
            .into_any_element()
    }

    pub(super) fn render_model_traits_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::current(cx);
        let session = self.selected_session()?;
        let model = self.model_metadata_for_session(session)?;
        if model.reasoning_efforts.is_empty() && model.service_tiers.is_empty() {
            return None;
        }

        let selected_effort = session
            .reasoning_effort
            .as_deref()
            .filter(|selected| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *selected)
            })
            .or(model.default_reasoning_effort.as_deref())
            .or_else(|| {
                model
                    .reasoning_efforts
                    .first()
                    .map(|option| option.id.as_str())
            })
            .map(str::to_owned);
        let effort_label = selected_effort.as_deref().and_then(|selected| {
            model
                .reasoning_efforts
                .iter()
                .find(|option| option.id == selected)
                .map(|option| option.label.clone())
        });

        let selected_tier = session
            .service_tier
            .as_deref()
            .filter(|selected| {
                *selected == "default"
                    || model
                        .service_tiers
                        .iter()
                        .any(|option| option.id == *selected)
            })
            .or(model.default_service_tier.as_deref())
            .unwrap_or("default")
            .to_owned();
        let tier_label = if selected_tier == "default" {
            "Standard".to_owned()
        } else {
            model
                .service_tiers
                .iter()
                .find(|option| option.id == selected_tier)
                .map(|option| option.label.clone())
                .unwrap_or_else(|| selected_tier.clone())
        };
        let fast = selected_tier == "fast" || tier_label.eq_ignore_ascii_case("fast");
        let trigger_label = effort_label.unwrap_or_else(|| tier_label.clone());
        let reasoning_efforts = model.reasoning_efforts.clone();
        let default_effort = model.default_reasoning_effort.clone();
        let service_tiers = model.service_tiers.clone();
        let default_tier = model
            .default_service_tier
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        let weak = cx.entity().downgrade();
        let composer = self.composer.clone();
        let trigger = MenuChip::new("model-traits")
            .when(fast, |trigger| {
                trigger.icon("icons/zap.svg", theme.text_secondary)
            })
            .label(trigger_label);

        Some(
            trigger
                .dropdown_menu(move |mut menu, _window, cx| {
                    menu = menu
                        .action_context(composer.read(cx).focus())
                        .min_w(px(208.0))
                        .max_w(px(208.0));
                    if !reasoning_efforts.is_empty() {
                        menu = menu.item(traits_menu_label(theme, "Reasoning"));
                        for option in reasoning_efforts.clone() {
                            let checked = selected_effort.as_deref() == Some(option.id.as_str());
                            let is_default = default_effort.as_deref() == Some(option.id.as_str());
                            let effort = option.id;
                            let item_weak = weak.clone();
                            menu = menu.item(
                                traits_menu_choice(theme, option.label, is_default, checked)
                                    .on_click(move |_, _, cx| {
                                        let _ = item_weak.update(cx, |this, cx| {
                                            this.set_reasoning_effort(effort.clone(), cx);
                                        });
                                    }),
                            );
                        }
                    }
                    if !service_tiers.is_empty() {
                        if !reasoning_efforts.is_empty() {
                            menu = menu.separator();
                        }
                        menu = menu.item(traits_menu_label(theme, "Service Tier"));
                        let standard_weak = weak.clone();
                        menu = menu.item(
                            traits_menu_choice(
                                theme,
                                "Standard".to_owned(),
                                default_tier == "default",
                                selected_tier == "default",
                            )
                            .on_click(move |_, _, cx| {
                                let _ = standard_weak.update(cx, |this, cx| {
                                    this.set_service_tier("default".to_owned(), cx);
                                });
                            }),
                        );
                        for option in service_tiers.clone() {
                            let checked = selected_tier == option.id;
                            let is_default = default_tier == option.id;
                            let tier = option.id;
                            let item_weak = weak.clone();
                            menu = menu.item(
                                traits_menu_choice(theme, option.label, is_default, checked)
                                    .on_click(move |_, _, cx| {
                                        let _ = item_weak.update(cx, |this, cx| {
                                            this.set_service_tier(tier.clone(), cx);
                                        });
                                    }),
                            );
                        }
                    }
                    menu
                })
                .anchor(Anchor::BottomLeft)
                .into_any_element(),
        )
    }

    pub(super) fn render_access_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected_mode = self
            .selected_session()
            .map(|session| session.runtime_mode)
            .filter(|mode| *mode != RuntimeMode::Plan)
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let composer = self.composer.clone();
        MenuChip::new("runtime-mode")
            .icon(selected_mode.icon(), theme.text_tertiary)
            .label(selected_mode.label())
            .dropdown_menu(move |mut menu, _window, cx| {
                menu = menu
                    .action_context(composer.read(cx).focus())
                    .min_w(px(320.0))
                    .max_w(px(320.0));
                for option in RuntimeMode::ACCESS_OPTIONS {
                    let item_weak = weak.clone();
                    let item_theme = theme;
                    let selected = option == selected_mode;
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .w_full()
                                .px(px(4.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(icon(option.icon(), 14.0, item_theme.text_tertiary))
                                .child(
                                    div()
                                        .w(px(272.0))
                                        .flex_none()
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_size(px(12.0))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(item_theme.text)
                                                .child(option.label()),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .mt(px(2.0))
                                                .text_size(px(10.5))
                                                .line_height(px(14.0))
                                                .whitespace_normal()
                                                .text_color(item_theme.text_tertiary)
                                                .child(option.description()),
                                        ),
                                )
                        })
                        .selected(selected)
                        .on_click(move |_, _, cx| {
                            let _ = item_weak.update(cx, |this, cx| {
                                this.set_runtime_mode(option, cx);
                            });
                        }),
                    );
                }
                menu
            })
            .anchor(Anchor::BottomLeft)
            .into_any_element()
    }

    pub(super) fn render_interaction_mode_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let mode = self
            .selected_session()
            .map(|session| session.interaction_mode)
            .unwrap_or_default();
        let next_mode = if mode == InteractionMode::Plan {
            InteractionMode::Build
        } else {
            InteractionMode::Plan
        };
        let weak = cx.entity().downgrade();
        div()
            .id("interaction-mode")
            .h(px(24.0))
            .px(px(7.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(px(11.5))
            .line_height(px(14.0))
            .text_color(if mode == InteractionMode::Plan {
                theme.accent
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay))
            .child(icon(
                if mode == InteractionMode::Plan {
                    "icons/list.svg"
                } else {
                    "icons/wrench.svg"
                },
                10.5,
                if mode == InteractionMode::Plan {
                    theme.accent
                } else {
                    theme.text_tertiary
                },
            ))
            .child(mode.label())
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.set_interaction_mode(next_mode, cx);
                });
            })
            .into_any_element()
    }

    pub(super) fn render_composer(&self, _window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let working = session
            .map(|session| {
                matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
                )
            })
            .unwrap_or(false);
        let has_draft = !self.composer.read(cx).content().trim().is_empty();
        div().flex_none().px(px(20.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .rounded(px(13.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.composer)
                .p(px(10.0))
                .child(div().px(px(4.0)).pt(px(2.0)).child(self.composer.clone()))
                .child(
                    div()
                        .mt(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(px(11.5))
                        .line_height(px(14.0))
                        .child(self.render_provider_model_control(cx))
                        .children(self.render_model_traits_control(cx))
                        .child(self.render_access_control(cx))
                        .child(self.render_interaction_mode_control(cx))
                        .child(div().flex_1())
                        .child(if working {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_default()
                                .bg(theme.overlay_strong)
                                .hover(|element| element.bg(theme.danger_soft))
                                .active(|element| element.opacity(0.8))
                                .child(icon("icons/stop.svg", 18.0, theme.text))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_turn(cx);
                                }))
                        } else {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if has_draft {
                                    theme.inverse
                                } else {
                                    theme.overlay_strong
                                })
                                .when(has_draft, |element| {
                                    element
                                        .cursor_default()
                                        .hover(|element| element.opacity(0.9))
                                        .active(|element| element.opacity(0.8))
                                })
                                .child(icon(
                                    "icons/arrow-up.svg",
                                    16.0,
                                    if has_draft {
                                        theme.on_inverse
                                    } else {
                                        theme.text_ghost
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let prompt = this.composer.read(cx).content().trim().to_owned();
                                    if !prompt.is_empty() {
                                        this.composer.update(cx, |input, cx| input.clear(cx));
                                        this.submit_prompt(prompt, cx);
                                    }
                                }))
                        }),
                ),
        )
    }

    pub(super) fn render_workspace_footer(&self, cx: &App) -> Div {
        let theme = Theme::current(cx);
        let path = self
            .selected_project()
            .map(|project| compact_path(&project.path))
            .unwrap_or_default();
        div()
            .flex_none()
            .px(px(20.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .h(px(20.0))
                    // Left edge lines up with the composer card's inner icon
                    // column (10px card padding + 7px chip padding).
                    .pl(px(17.0))
                    .pr(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .when_some(self.branch.clone(), |element, branch| {
                        element
                            .child(icon("icons/git-branch.svg", 10.5, theme.text_tertiary))
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(branch)),
                            )
                            .child(
                                div()
                                    .w(px(2.5))
                                    .h(px(2.5))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(theme.text_ghost),
                            )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(theme.text_ghost)
                            .child(SharedString::from(path)),
                    ),
            )
    }
}

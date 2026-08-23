use super::*;

use chrono::{Datelike, Days};
use std::path::Path;

pub(super) fn pulse_dot(size: f32, color: Hsla) -> AnyElement {
    motion::pulse(Duration::from_millis(1600), move |phase| {
        div()
            .w(px(size))
            .h(px(size))
            .flex_none()
            .rounded_full()
            .bg(color)
            .opacity(pulsating_between(0.3, 1.0)(phase))
            .into_any_element()
    })
    // Mounted for whole activities; its pane must not tick at full rate.
    .every(2)
    .into_any_element()
}

/// Three dots chasing a brightness wave, the transcript's "still working"
/// signal. Each dot rides the shared pulse clock with a phase offset, so the
/// bright spot travels left to right. Under reduce-motion the clock holds the
/// cycle's first frame — the lead dot bright, the tail dim — which reads as a
/// static ellipsis.
pub(super) fn working_wave_dots(color: Hsla) -> AnyElement {
    const DOT_PHASE_STEP: f32 = 0.18;
    motion::pulse(Duration::from_millis(1400), move |phase| {
        div()
            .flex()
            .items_center()
            .gap(px(3.5))
            .children((0..3).map(|index| {
                let dot_phase = (phase + 1.0 - index as f32 * DOT_PHASE_STEP) % 1.0;
                let wave = ((dot_phase * std::f32::consts::TAU).sin() + 1.0) / 2.0;
                div()
                    .size(px(4.5))
                    .flex_none()
                    .rounded_full()
                    .bg(color)
                    .opacity(0.25 + 0.75 * wave)
            }))
            .into_any_element()
    })
    // Mounted for the whole turn: this is what sets the transcript pane's
    // tick floor, and every tick rebuilds each visible row. The 1400 ms wave
    // reads identically at half cadence.
    .every(2)
    .into_any_element()
}

pub(super) fn format_message_time(created_at: u64) -> String {
    format_message_time_at(created_at, Local::now())
}

fn format_message_time_at(created_at: u64, now: DateTime<Local>) -> String {
    let Ok(seconds) = i64::try_from(created_at) else {
        return String::new();
    };
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|timestamp| {
            let timestamp = timestamp.with_timezone(&Local);
            let message_date = timestamp.date_naive();
            let today = now.date_naive();
            if crate::i18n::uses_east_asian_date_format() {
                let time = timestamp.format("%H:%M").to_string();
                if message_date >= today {
                    return time;
                }
                if today.pred_opt() == Some(message_date) {
                    return tr!("time.yesterday_at", time = time);
                }
                let week_start = today
                    .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
                    .unwrap_or(today);
                if message_date >= week_start {
                    let weekday = match timestamp.weekday() {
                        chrono::Weekday::Mon => tr!("time.monday"),
                        chrono::Weekday::Tue => tr!("time.tuesday"),
                        chrono::Weekday::Wed => tr!("time.wednesday"),
                        chrono::Weekday::Thu => tr!("time.thursday"),
                        chrono::Weekday::Fri => tr!("time.friday"),
                        chrono::Weekday::Sat => tr!("time.saturday"),
                        chrono::Weekday::Sun => tr!("time.sunday"),
                    };
                    return tr!("time.weekday_at", weekday = weekday, time = time);
                }
                if message_date.year() == today.year() {
                    return tr!(
                        "time.date_at",
                        month = timestamp.month(),
                        day = timestamp.day(),
                        time = time
                    );
                }
                return tr!(
                    "time.full_date_at",
                    year = timestamp.year(),
                    month = timestamp.month(),
                    day = timestamp.day(),
                    time = time
                );
            }
            let time = timestamp
                .format("%I:%M %p")
                .to_string()
                .trim_start_matches('0')
                .to_owned();

            if message_date >= today {
                return time;
            }

            if today.pred_opt() == Some(message_date) {
                return tr!("time.yesterday_at", time = time);
            }

            let week_start = today
                .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
                .unwrap_or(today);
            if message_date >= week_start {
                return format!("{} {time}", timestamp.format("%A"));
            }

            let day = timestamp.day();
            let ordinal_suffix = match day % 100 {
                11..=13 => "th",
                _ => match day % 10 {
                    1 => "st",
                    2 => "nd",
                    3 => "rd",
                    _ => "th",
                },
            };
            let date = if message_date.year() == today.year() {
                format!("{} {day}{ordinal_suffix}", timestamp.format("%b"))
            } else {
                format!(
                    "{} {day}{ordinal_suffix} {}",
                    timestamp.format("%b"),
                    timestamp.year()
                )
            };
            format!("{date}, {time}")
        })
        .unwrap_or_default()
}

impl Waku {
    pub(super) fn control_was_copied(&self, control_id: &str) -> bool {
        self.copied_control_feedback.contains_key(control_id)
    }

    pub(super) fn show_control_copied(
        &mut self,
        control_id: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let control_id = control_id.into();
        self.copied_control_generation = self.copied_control_generation.wrapping_add(1);
        let generation = self.copied_control_generation;
        self.copied_control_feedback
            .insert(control_id.clone(), generation);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_control_feedback.get(&control_id) == Some(&generation) {
                    this.copied_control_feedback.remove(&control_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn show_message_copied(&mut self, message_id: Uuid, cx: &mut Context<Self>) {
        self.copied_message_generation = self.copied_message_generation.wrapping_add(1);
        let generation = self.copied_message_generation;
        self.copied_message_feedback.insert(message_id, generation);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_message_feedback.get(&message_id) == Some(&generation) {
                    this.copied_message_feedback.remove(&message_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[allow(clippy::too_many_arguments)]
fn render_message_footer(
    theme: &Theme,
    message: &Message,
    footer_time: u64,
    copy_content: SharedString,
    copied: bool,
    group_name: SharedString,
    align_right: bool,
    assistant_message_action: Option<AssistantMessageAction>,
    user_message_action: Option<UserMessageAction>,
    waku: gpui::WeakEntity<Waku>,
) -> AnyElement {
    let theme = *theme;
    let message_id = message.id;
    let copy_waku = waku.clone();
    let footer_color = if theme.is_dark {
        gpui::hsla(126.93 / 360.0, 0.000_000_1, 0.543_95, 1.0)
    } else {
        theme.text_ghost
    };
    let timestamp = div()
        .h(px(27.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .text_size(sp(12.5))
        .line_height(sp(14.0))
        .text_color(footer_color)
        .child(format_message_time(footer_time));
    let copy_button = div()
        .id(SharedString::from(format!("copy-message-{message_id}")))
        .w(px(27.0))
        .h(px(27.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .hover(|element| element.bg(theme.overlay_strong))
        .child(icon(
            if copied {
                "icons/check.svg"
            } else {
                "icons/copy.svg"
            },
            14.0,
            footer_color,
        ))
        .tooltip(Tooltip::text(if copied {
            tr!("common.copied")
        } else {
            tr!("common.copy_message")
        }))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.to_string()));
            let _ = copy_waku.update(cx, |this, cx| {
                this.show_message_copied(message_id, cx);
            });
        });
    let mut footer = div()
        .w_full()
        .h(px(27.0))
        .flex()
        .items_center()
        .gap(px(1.0))
        .invisible()
        .group_hover(group_name, |element| element.visible())
        .when(!align_right, |element| element.ml(-px(7.0)))
        .when(align_right, |element| element.justify_end());

    if align_right {
        footer = footer.child(timestamp).child(copy_button);
    } else {
        footer = footer.child(copy_button);
        if let Some(action) = assistant_message_action {
            let fork_waku = waku.clone();
            let fork_icon = if action.preparing {
                motion::spin(icon("icons/loader-circle.svg", 14.0, footer_color))
            } else {
                icon("icons/fork.svg", 14.0, footer_color).into_any_element()
            };
            let fork_button = div()
                .id(SharedString::from(format!("fork-response-{message_id}")))
                .w(px(27.0))
                .h(px(27.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .when(!action.enabled && !action.preparing, |element| {
                    element.opacity(0.45)
                })
                .child(fork_icon)
                .tooltip(Tooltip::text(if action.enabled {
                    tr_cow!("session.fork_task")
                } else {
                    tr_cow!("session.forking_task")
                }));
            footer = footer.child(if action.enabled {
                fork_button
                    .hover(|element| element.bg(theme.overlay_strong))
                    .on_click(move |_, _, cx| {
                        let _ = fork_waku.update(cx, |this, cx| {
                            this.fork_session_from_response(
                                action.session_id,
                                action.turn_count,
                                cx,
                            );
                        });
                    })
            } else {
                fork_button
            });
        }
        footer = footer.child(timestamp);
    }

    if let Some(action) = user_message_action {
        let edit_waku = waku;
        footer = footer.child(
            div()
                .id(SharedString::from(format!(
                    "user-message-action-{message_id}"
                )))
                .w(px(27.0))
                .h(px(27.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .hover(|element| element.bg(theme.overlay_strong))
                .child(icon("icons/rewind.svg", 14.0, footer_color))
                .tooltip(Tooltip::text(tr_cow!("session.revert_to_here")))
                .on_click(move |_, window, cx| {
                    let _ = edit_waku.update(cx, |this, cx| {
                        this.begin_message_edit(action, window, cx);
                    });
                }),
        );
    }

    footer.into_any_element()
}

/// Everything one transcript message row needs to render itself. Bundled
/// because these travel together from `transcript_row` and nowhere else.
pub(super) struct MessageRender<'a> {
    pub(super) theme: &'a Theme,
    pub(super) message: &'a Message,
    pub(super) assistant_footer_copy_content: Option<SharedString>,
    pub(super) assistant_footer_time: Option<u64>,
    pub(super) assistant_before_footer: Option<AnyElement>,
    pub(super) copied: bool,
    pub(super) assistant_message_action: Option<AssistantMessageAction>,
    pub(super) user_message_action: Option<UserMessageAction>,
    pub(super) message_edit_input: Option<Entity<ComposerInput>>,
    pub(super) attachment_menus: Vec<ContextMenuHandle>,
    pub(super) attachment_images: Vec<Option<Arc<gpui::Image>>>,
    /// Captured from the selected daemon before the virtualized row is built.
    /// A row is laid out while the root `Waku` entity is already updating, so
    /// it must not read that entity again just to decide whether Finder reveal
    /// is available.
    pub(super) attachments_can_reveal: bool,
    /// The parsed human or assistant body. System messages remain verbatim.
    pub(super) markdown: Option<&'a MarkdownView>,
    pub(super) ctx: &'a MarkdownCtx<'a>,
    pub(super) menu: ContextMenuHandle,
    pub(super) waku: gpui::WeakEntity<Waku>,
    pub(super) composer: Entity<ComposerInput>,
}

fn render_sent_message_attachments(
    message_id: Uuid,
    attachments: &[MessageAttachment],
    attachment_menus: &[ContextMenuHandle],
    attachment_images: &[Option<Arc<gpui::Image>>],
    can_reveal: bool,
    waku: &gpui::WeakEntity<Waku>,
    theme: &Theme,
) -> Option<AnyElement> {
    if attachments.is_empty() {
        return None;
    }
    let mut row = div()
        .max_w(px(540.0))
        .flex()
        .flex_wrap()
        .justify_end()
        .gap(px(8.0));
    for (index, attachment) in attachments.iter().enumerate() {
        let Some(menu) = attachment_menus.get(index) else {
            continue;
        };
        let icon_path = if attachment.is_dir {
            "icons/folder.svg"
        } else {
            right_panel::file_icon_for_path(&attachment.mention)
        };
        let attachment_image = attachment_images.get(index).and_then(|image| image.clone());
        let mut tile = div()
            .id(SharedString::from(format!(
                "message-{message_id}-attachment-{index}"
            )))
            .w(px(96.0))
            .h(px(80.0))
            .rounded(px(9.0))
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(theme.inset)
            .track_focus(menu.trigger_focus_handle())
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .tooltip(Tooltip::text(attachment.name.clone()));
        if attachment.is_image {
            let key_menu = menu.clone();
            if let Some(attachment_image) = attachment_image.as_ref() {
                let preview_waku = waku.clone();
                let key_waku = waku.clone();
                let preview_image = attachment_image.clone();
                let key_image = attachment_image.clone();
                let preview_name = SharedString::from(attachment.name.clone());
                let key_name = preview_name.clone();
                tile = tile.child(
                    div()
                        .id(SharedString::from(format!(
                            "message-{message_id}-attachment-{index}-preview"
                        )))
                        .size_full()
                        .cursor_default()
                        .on_click(move |_, window, cx| {
                            let _ = preview_waku.update(cx, |this, cx| {
                                this.open_image_preview(
                                    preview_image.clone(),
                                    preview_name.clone(),
                                    window,
                                    cx,
                                );
                            });
                            cx.stop_propagation();
                        })
                        .child(
                            img(attachment_image.clone())
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        ),
                );
                tile = tile.on_key_down(move |event: &KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    if matches!(key, "enter" | "space") {
                        let _ = key_waku.update(cx, |this, cx| {
                            this.open_image_preview(
                                key_image.clone(),
                                key_name.clone(),
                                window,
                                cx,
                            );
                        });
                        cx.stop_propagation();
                    } else if key == "f10" && event.keystroke.modifiers.shift {
                        key_menu.open_context_menu(window, cx);
                        cx.stop_propagation();
                    }
                });
            } else {
                tile = tile
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("icons/file-types/image.svg", 18.0, theme.text_ghost)),
                    )
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "f10" && event.keystroke.modifiers.shift {
                            key_menu.open_context_menu(window, cx);
                            cx.stop_propagation();
                        }
                    });
            }
        } else {
            let key_menu = menu.clone();
            tile = tile.child(
                div()
                    .size_full()
                    .px(px(7.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(7.0))
                    .child(icon(icon_path, 18.0, theme.text_tertiary))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_center()
                            .text_size(sp(12.5))
                            .text_color(theme.text_secondary)
                            .child(attachment.name.clone()),
                    ),
            );
            tile = tile.on_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "f10" && event.keystroke.modifiers.shift {
                    key_menu.open_context_menu(window, cx);
                    cx.stop_propagation();
                }
            });
        }
        let reveal_path = attachment.path.clone();
        row = row.child(context_menu(
            tile,
            SharedString::from(format!("message-{message_id}-attachment-{index}-menu")),
            menu,
            move |_| image_preview::attachment_menu_items(reveal_path.clone(), can_reveal),
        ));
    }
    Some(row.into_any_element())
}

fn render_markdown_message_body<'a>(
    content: &str,
    markdown: Option<&'a MarkdownView>,
    theme: &Theme,
    ctx: &MarkdownCtx<'a>,
) -> AnyElement {
    markdown
        .and_then(|markdown| md::render::markdown(markdown, ctx))
        // Empty or not-yet-parsed content still needs a selectable fallback.
        .unwrap_or_else(|| {
            md::render::plain_text(
                content.to_owned(),
                md::render::SANS_FAMILY,
                FontWeight::NORMAL,
                theme.text,
                ctx,
            )
        })
}

pub(super) fn render_message(params: MessageRender, cx: &mut App) -> AnyElement {
    let MessageRender {
        theme,
        message,
        assistant_footer_copy_content,
        assistant_footer_time,
        assistant_before_footer,
        copied,
        assistant_message_action,
        user_message_action,
        message_edit_input,
        attachment_menus,
        attachment_images,
        attachments_can_reveal,
        markdown,
        ctx,
        menu,
        waku,
        composer,
    } = params;

    let content = message.visible_content().to_owned();
    // "Copy Message" must match what the row presents. The terminal part of a
    // settled response stands in for the whole visible answer, so its menu
    // shares the footer's copy content — parts hidden behind "Worked for X"
    // stay out — rather than copying the final part alone.
    let menu_copy_content = assistant_footer_copy_content
        .clone()
        .unwrap_or_else(|| SharedString::from(content.clone()));
    let message_id = message.id;
    let role = message.role;
    let element = match role {
        MessageRole::User => {
            let group_name = SharedString::from(format!("user-message-{message_id}"));
            let mut column = div()
                .w_full()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(3.0))
                .group(group_name.clone());
            if let Some(attachments) = render_sent_message_attachments(
                message_id,
                &message.attachments,
                &attachment_menus,
                &attachment_images,
                attachments_can_reveal,
                &waku,
                theme,
            ) {
                column = column.child(attachments);
            }
            if let Some(edit_input) = message_edit_input {
                let can_submit = !edit_input.read(cx).content(cx).trim().is_empty()
                    || !message.attachments.is_empty();
                let cancel_waku = waku.clone();
                let submit_waku = waku.clone();
                column = column.child(
                    div()
                        .w_full()
                        .max_w(px(540.0))
                        .rounded(px(12.0))
                        .bg(theme.raised)
                        .pt(px(9.0))
                        .pb(px(8.0))
                        .child(edit_input)
                        .child(
                            div()
                                .mt(px(7.0))
                                .px(px(12.0))
                                .flex()
                                .justify_end()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "cancel-message-edit-{message_id}"
                                        )))
                                        .h(px(26.0))
                                        .px(px(10.0))
                                        .rounded(px(7.0))
                                        .border_1()
                                        .border_color(theme.border)
                                        .bg(theme.overlay)
                                        .flex()
                                        .items_center()
                                        .text_size(sp(12.5))
                                        .text_color(theme.text_secondary)
                                        .cursor_default()
                                        .hover(|element| element.bg(theme.overlay_strong))
                                        .child(tr_cow!("common.cancel"))
                                        .on_click(move |_, window, cx| {
                                            let _ = cancel_waku.update(cx, |this, cx| {
                                                this.cancel_message_edit(window, cx);
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "submit-message-edit-{message_id}"
                                        )))
                                        .h(px(26.0))
                                        .px(px(11.0))
                                        .rounded(px(7.0))
                                        .bg(if can_submit {
                                            theme.inverse
                                        } else {
                                            theme.overlay_strong
                                        })
                                        .flex()
                                        .items_center()
                                        .text_size(sp(12.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if can_submit {
                                            theme.on_inverse
                                        } else {
                                            theme.text_ghost
                                        })
                                        .when(can_submit, |element| {
                                            element
                                                .cursor_default()
                                                .hover(|element| element.opacity(0.9))
                                        })
                                        .child(tr_cow!("common.send"))
                                        .on_click(move |_, _, cx| {
                                            if can_submit {
                                                let _ = submit_waku.update(cx, |this, cx| {
                                                    this.submit_message_edit(cx);
                                                });
                                            }
                                        }),
                                ),
                        ),
                );
            } else {
                if !content.trim().is_empty() {
                    let body = render_markdown_message_body(&content, markdown, theme, ctx);
                    column = column.child(
                        div()
                            .max_w(px(540.0))
                            .min_w_0()
                            .rounded(px(12.0))
                            .bg(theme.raised)
                            .px(px(12.0))
                            .py(px(8.0))
                            .text_size(sp(14.0))
                            .line_height(sp(20.0))
                            .child(body),
                    );
                }
                column = column.child(render_message_footer(
                    theme,
                    message,
                    message.created_at,
                    SharedString::from(content.clone()),
                    copied,
                    group_name,
                    true,
                    None,
                    user_message_action,
                    waku.clone(),
                ));
            }
            column
        }
        MessageRole::Assistant => {
            let group_name = SharedString::from(format!("assistant-message-{message_id}"));
            let body = render_markdown_message_body(&content, markdown, theme, ctx);
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .gap(px(3.0))
                .group(group_name.clone())
                .child(body);
            if let Some(before_footer) = assistant_before_footer {
                column = column.child(div().w_full().mt(px(12.0)).mb(px(3.0)).child(before_footer));
            }
            if let Some(copy_content) = assistant_footer_copy_content {
                column = column.child(render_message_footer(
                    theme,
                    message,
                    assistant_footer_time.unwrap_or(message.created_at),
                    copy_content,
                    copied,
                    group_name,
                    false,
                    assistant_message_action,
                    None,
                    waku.clone(),
                ));
            }
            column
        }
        MessageRole::System => div().w_full().flex().justify_center().child(
            div()
                .px(px(10.0))
                .py(px(4.0))
                .rounded_full()
                .bg(theme.overlay)
                .text_size(sp(12.5))
                .line_height(sp(16.0))
                .child(md::render::plain_text(
                    content.clone(),
                    md::render::SANS_FAMILY,
                    FontWeight::NORMAL,
                    theme.text_tertiary,
                    ctx,
                )),
        ),
    };

    let selection = ctx.selection().clone();
    context_menu(
        element.id(message_id),
        SharedString::from(format!("message-menu-{message_id}")),
        &menu,
        move |cx| {
            message_menu_items(
                &menu_copy_content,
                role,
                user_message_action,
                assistant_message_action,
                &selection,
                &composer,
                &waku,
                cx,
            )
        },
    )
}

/// The message row's context menu. Rebuilt on each open, so availability checks
/// here always reflect the current session state.
#[allow(clippy::too_many_arguments)]
fn message_menu_items(
    content: &str,
    role: MessageRole,
    user_message_action: Option<UserMessageAction>,
    assistant_message_action: Option<AssistantMessageAction>,
    selection: &TranscriptSelection,
    composer: &Entity<ComposerInput>,
    waku: &gpui::WeakEntity<Waku>,
    _cx: &mut App,
) -> Vec<MenuItem> {
    let mut items = Vec::new();

    if let Some(selected) = selection.selection.borrow().selected_text() {
        items.push(MenuItem::new(tr!("common.copy_selection"), move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.clone()));
        }));
    }

    let copy_content = content.to_owned();
    items.push(MenuItem::new(
        tr!("common.copy_message_title"),
        move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.clone()));
        },
    ));

    if role == MessageRole::User && user_message_action.is_none() {
        let composer = composer.clone();
        let edit_content = content.to_owned();
        items.push(MenuItem::new(
            tr!("common.copy_to_composer"),
            move |window, cx| {
                composer.update(cx, |composer, cx| {
                    composer.set_content(edit_content.clone(), cx);
                });
                let focus_handle = composer.read(cx).focus();
                window.focus(&focus_handle, cx);
            },
        ));
    }

    if let Some(code) = fenced_code(content) {
        items.push(MenuItem::new(tr!("common.copy_code"), move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
        }));
    }

    if let Some(action) = user_message_action {
        let waku = waku.clone();
        items.push(MenuItem::Separator);
        items.push(
            MenuItem::new(tr!("session.revert_to_here_title"), move |window, cx| {
                let _ = waku.update(cx, |this, cx| {
                    this.begin_message_edit(action, window, cx);
                });
            })
            .icon("icons/rewind.svg"),
        );
    }

    if let Some(action) = assistant_message_action {
        let waku = waku.clone();
        items.push(MenuItem::Separator);
        items.push(
            MenuItem::new(
                if action.enabled {
                    tr!("session.fork_task_title")
                } else {
                    tr!("session.forking_task_title")
                },
                move |_, cx| {
                    let _ = waku.update(cx, |this, cx| {
                        this.fork_session_from_response(action.session_id, action.turn_count, cx);
                    });
                },
            )
            .icon("icons/fork.svg")
            .disabled(!action.enabled),
        );
    }

    items
}

pub(super) fn fenced_code(content: &str) -> Option<String> {
    let mut code_blocks = Vec::new();
    let mut segments = content.split("```");
    let _ = segments.next();
    while let Some(fenced) = segments.next() {
        let (language, code) = fenced
            .split_once('\n')
            .map(|(language, code)| (language.trim(), code))
            .unwrap_or(("", fenced));
        let code = if language.is_empty() && !fenced.contains('\n') {
            fenced
        } else {
            code
        };
        if !code.trim().is_empty() {
            code_blocks.push(code.trim_end().to_owned());
        }
        let _ = segments.next();
    }
    (!code_blocks.is_empty()).then(|| code_blocks.join("\n\n"))
}

pub(super) fn activity_summary(activities: &[ActivityItem]) -> String {
    let mut counts: Vec<(crate::model::ActivityKind, usize)> = Vec::new();
    for activity in activities {
        if let Some(entry) = counts.iter_mut().find(|(kind, _)| *kind == activity.kind) {
            entry.1 += 1;
        } else {
            counts.push((activity.kind, 1));
        }
    }
    let parts = counts
        .into_iter()
        .map(|(kind, count)| {
            let (singular, plural) = activity_noun(kind);
            tr!(
                "activity.count",
                count = count,
                activity = if count == 1 { singular } else { plural }
            )
        })
        .collect::<Vec<_>>();
    let running = activities.iter().any(|activity| !activity.complete);
    if running {
        tr!("activity.running", activities = parts.join(" · "))
    } else {
        tr!("activity.ran", activities = parts.join(" · "))
    }
}

pub(super) fn activity_group_is_live(
    live_turn: bool,
    latest_block: bool,
    after_message: usize,
    message_count: usize,
) -> bool {
    live_turn && latest_block && after_message == message_count
}

pub(super) fn activity_header_title(
    activities: &[ActivityItem],
    live_group: bool,
    live_reasoning_id: Option<Uuid>,
) -> String {
    if live_group && let Some(activity) = activities.last() {
        return activity.reasoning.as_ref().map_or_else(
            || activity_display_title(activity),
            |reasoning| reasoning_activity_title(reasoning, live_reasoning_id == Some(activity.id)),
        );
    }

    activity_summary(activities)
}

fn tool_name_leaf(name: &str) -> &str {
    let name = name.trim();
    let leaf = name.rsplit("__").next().unwrap_or(name);
    leaf.rsplit([':', '.', '/']).next().unwrap_or(leaf)
}

fn is_ask_user_question(activity: &ActivityItem) -> bool {
    activity.kind == crate::model::ActivityKind::Tool
        && tool_name_leaf(&activity.title)
            .chars()
            .filter(|character| !matches!(*character, '_' | '-' | ' '))
            .flat_map(char::to_lowercase)
            .collect::<String>()
            == "askuserquestion"
}

fn humanize_tool_name(name: &str) -> String {
    let name = name.trim();
    if name.chars().any(char::is_whitespace) {
        return name.to_owned();
    }

    let leaf = tool_name_leaf(name);
    let characters = leaf.chars().collect::<Vec<_>>();
    let mut display = String::with_capacity(leaf.len() + 4);
    for (index, character) in characters.iter().copied().enumerate() {
        if matches!(character, '_' | '-') {
            if !display.ends_with(' ') {
                display.push(' ');
            }
            continue;
        }
        let previous = index.checked_sub(1).and_then(|index| characters.get(index));
        let next = characters.get(index + 1);
        let starts_word = character.is_ascii_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_ascii_uppercase()
                        && next.is_some_and(|next| next.is_ascii_lowercase()))
            });
        if starts_word && !display.ends_with(' ') {
            display.push(' ');
        }
        display.push(character);
    }

    let display = display.trim();
    let mut characters = display.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_else(|| tr!("activity.tool"))
}

fn activity_tool_display_name(activity: &ActivityItem) -> String {
    if is_ask_user_question(activity) {
        return tr!("activity.ask_questions");
    }
    if let Some(target) = activity
        .display_target
        .as_deref()
        .map(str::trim)
        .filter(|target| !target.is_empty())
    {
        return target.to_owned();
    }
    if !crate::model::is_generic_activity_title(activity.kind, &activity.title) {
        return humanize_tool_name(&activity.title);
    }
    tr!("activity.tool")
}

pub(super) fn activity_display_title(activity: &ActivityItem) -> String {
    use crate::model::ActivityKind;

    match activity.kind {
        ActivityKind::FileChange => {
            let subject = match activity.file_changes.as_slice() {
                [change] => Some(change.display_name().to_owned()),
                changes if !changes.is_empty() => {
                    Some(tr!("activity.file_count", count = changes.len()))
                }
                _ => None,
            };
            if subject.is_none()
                && !crate::model::is_generic_activity_title(activity.kind, &activity.title)
            {
                return activity.title.clone();
            }
            match (activity.complete, activity.failed, subject) {
                (false, _, Some(file)) => tr!("activity.editing_named_file", file = file),
                (true, false, Some(file)) => tr!("activity.edited_named_file", file = file),
                (true, true, Some(file)) => tr!("activity.edit_failed_named_file", file = file),
                (false, _, None) => tr!("activity.editing_files"),
                (true, false, None) => tr!("activity.edited_files"),
                (true, true, None) => tr!("activity.edit_failed"),
            }
        }
        ActivityKind::FileRead => {
            let file = activity.display_target.as_deref().map(activity_path_name);
            if file.is_none()
                && !crate::model::is_generic_activity_title(activity.kind, &activity.title)
            {
                return activity.title.clone();
            }
            match (activity.complete, activity.failed, file) {
                (false, _, Some(file)) => tr!("activity.reading_named_file", file = file),
                (true, false, Some(file)) => tr!("activity.read_named_file", file = file),
                (true, true, Some(file)) => tr!("activity.read_named_file_failed", file = file),
                (false, _, None) => tr!("activity.reading_file"),
                (true, false, None) => tr!("activity.read_file_completed"),
                (true, true, None) => tr!("activity.read_file_failed"),
            }
        }
        ActivityKind::FileSearch => {
            let query = activity.display_target.as_deref();
            if query.is_none()
                && !crate::model::is_generic_activity_title(activity.kind, &activity.title)
            {
                return activity.title.clone();
            }
            match (activity.complete, activity.failed, query) {
                (false, _, Some(query)) => tr!("activity.searching_files_for", query = query),
                (true, false, Some(query)) => tr!("activity.searched_files_for", query = query),
                (true, true, Some(query)) => tr!("activity.file_search_failed_for", query = query),
                (false, _, None) => tr!("activity.searching_files"),
                (true, false, None) => tr!("activity.searched_files"),
                (true, true, None) => tr!("activity.file_search_failed"),
            }
        }
        ActivityKind::FileList => {
            let directory = activity.display_target.as_deref().map(activity_path_name);
            if directory.is_none()
                && !crate::model::is_generic_activity_title(activity.kind, &activity.title)
            {
                return activity.title.clone();
            }
            match (activity.complete, activity.failed, directory) {
                (false, _, Some(directory)) => {
                    tr!("activity.listing_files_in", directory = directory)
                }
                (true, false, Some(directory)) => {
                    tr!("activity.listed_files_in", directory = directory)
                }
                (true, true, Some(directory)) => {
                    tr!("activity.file_list_failed_in", directory = directory)
                }
                (false, _, None) => tr!("activity.listing_files"),
                (true, false, None) => tr!("activity.listed_files"),
                (true, true, None) => tr!("activity.file_list_failed"),
            }
        }
        ActivityKind::Command => {
            if let Some(description) = activity.display_description.as_deref() {
                return match (activity.complete, activity.failed) {
                    (false, _) => {
                        tr!(
                            "activity.running_described_command",
                            description = description
                        )
                    }
                    (true, false) => {
                        tr!("activity.ran_described_command", description = description)
                    }
                    (true, true) => {
                        tr!(
                            "activity.described_command_failed",
                            description = description
                        )
                    }
                };
            }
            if let Some(command) = activity.display_target.as_deref() {
                return match (activity.complete, activity.failed) {
                    (false, _) => tr!("activity.running_named_command", command = command),
                    (true, false) => tr!("activity.ran_named_command", command = command),
                    (true, true) => tr!("activity.named_command_failed", command = command),
                };
            }
            if !crate::model::is_generic_activity_title(activity.kind, &activity.title) {
                return activity.title.clone();
            }
            match (activity.complete, activity.failed) {
                (false, _) => tr!("activity.running_command"),
                (true, false) => tr!("activity.ran_command"),
                (true, true) => tr!("activity.command_failed"),
            }
        }
        ActivityKind::Search => {
            if let Some(query) = activity.display_target.as_deref() {
                return match (activity.complete, activity.failed) {
                    (false, _) => tr!("activity.searching_web_for", query = query),
                    (true, false) => tr!("activity.searched_web_for", query = query),
                    (true, true) => tr!("activity.web_search_failed_for", query = query),
                };
            }
            if ActivityKind::from_tool_name(&activity.title) == ActivityKind::Search {
                return match (activity.complete, activity.failed) {
                    (false, _) => tr!("activity.searching_web"),
                    (true, false) => tr!("activity.searched_the_web"),
                    (true, true) => tr!("activity.web_search_failed"),
                };
            }
            activity.title.clone()
        }
        ActivityKind::Plan => {
            if !crate::model::is_generic_activity_title(activity.kind, &activity.title) {
                return activity.title.clone();
            }
            match (activity.complete, activity.failed) {
                (false, _) => tr!("activity.updating_plan"),
                (true, false) => tr!("activity.updated_plan"),
                (true, true) => tr!("activity.plan_update_failed"),
            }
        }
        ActivityKind::Tool => activity_tool_display_name(activity),
        ActivityKind::Reasoning => activity.title.clone(),
    }
}

pub(super) fn activity_action_label(activity: &ActivityItem) -> String {
    use crate::model::ActivityKind;

    match activity.kind {
        ActivityKind::Reasoning => tr!("activity.action_think"),
        ActivityKind::Command => tr!("activity.action_run"),
        ActivityKind::FileChange => tr!("activity.action_edit"),
        ActivityKind::FileRead => tr!("activity.action_read"),
        ActivityKind::FileSearch | ActivityKind::Search => tr!("activity.action_search"),
        ActivityKind::FileList => tr!("activity.action_list"),
        ActivityKind::Plan => tr!("activity.action_plan"),
        ActivityKind::Tool if is_ask_user_question(activity) => tr!("activity.ask_questions"),
        ActivityKind::Tool => tr!("activity.tool"),
    }
}

pub(super) fn activity_row_detail(activity: &ActivityItem, reasoning_live: bool) -> String {
    use crate::model::ActivityKind;

    let custom_title = || {
        (!crate::model::is_generic_activity_title(activity.kind, &activity.title))
            .then(|| activity.title.clone())
    };
    match activity.kind {
        ActivityKind::Reasoning => activity.reasoning.as_ref().map_or_else(
            || activity.title.clone(),
            |reasoning| reasoning_activity_title(reasoning, reasoning_live),
        ),
        ActivityKind::Command => activity
            .display_description
            .clone()
            .or_else(|| activity.display_target.clone())
            .or_else(custom_title)
            .unwrap_or_default(),
        ActivityKind::FileChange => match activity.file_changes.as_slice() {
            [change] => change.display_name().to_owned(),
            changes if !changes.is_empty() => {
                tr!("activity.file_count", count = changes.len())
            }
            _ => custom_title().unwrap_or_default(),
        },
        ActivityKind::FileRead | ActivityKind::FileList => activity
            .display_target
            .as_deref()
            .map(activity_path_name)
            .or_else(custom_title)
            .unwrap_or_default(),
        ActivityKind::FileSearch => activity_display_title(activity),
        ActivityKind::Search => activity.display_target.as_deref().map_or_else(
            || custom_title().unwrap_or_default(),
            |query| tr!("activity.search_for", query = query),
        ),
        ActivityKind::Plan => custom_title().unwrap_or_default(),
        ActivityKind::Tool if is_ask_user_question(activity) => String::new(),
        ActivityKind::Tool => {
            let has_name = activity
                .display_target
                .as_deref()
                .is_some_and(|target| !target.trim().is_empty())
                || !crate::model::is_generic_activity_title(activity.kind, &activity.title);
            has_name
                .then(|| activity_tool_display_name(activity))
                .unwrap_or_default()
        }
    }
}

pub(super) fn reasoning_activity_title(reasoning: &ReasoningBlock, live: bool) -> String {
    if live {
        tr!("transcript.thinking")
    } else {
        tr!(
            "transcript.thought_for",
            duration = format_worked_duration(
                reasoning
                    .finished_at_ms
                    .saturating_sub(reasoning.started_at_ms)
                    .div_ceil(1000)
                    .max(1)
            )
        )
    }
}

fn activity_path_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// Whether this activity's expanded view shows a diff instead of the tool
/// arguments that produced it.
pub(super) fn activity_shows_diff(activity: &ActivityItem) -> bool {
    activity.kind == ActivityKind::FileChange
        && activity
            .file_changes
            .iter()
            .any(|change| change.diff.is_some())
}

pub(super) fn activity_file_change_stats(activity: &ActivityItem) -> Option<(u64, u64)> {
    if activity.kind != crate::model::ActivityKind::FileChange
        || !activity.complete
        || activity.failed
        || activity.file_changes.is_empty()
    {
        return None;
    }
    let additions = activity
        .file_changes
        .iter()
        .map(|change| change.additions)
        .sum::<Option<u64>>()?;
    let deletions = activity
        .file_changes
        .iter()
        .map(|change| change.deletions)
        .sum::<Option<u64>>()?;
    Some((additions, deletions))
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ActivityDisclosureSectionKind {
    Command,
    Arguments,
    Output,
    Detail,
}

impl ActivityDisclosureSectionKind {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Arguments => "arguments",
            Self::Output => "output",
            Self::Detail => "detail",
        }
    }

    pub(super) fn label(self) -> Option<String> {
        match self {
            Self::Command => Some(tr!("activity.command_detail")),
            Self::Arguments => Some(tr!("activity.arguments")),
            Self::Output => Some(tr!("activity.output")),
            Self::Detail => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityDisclosureSection {
    pub(super) kind: ActivityDisclosureSectionKind,
    pub(super) content: String,
}

pub(super) fn activity_disclosure_sections(
    activity: &ActivityItem,
) -> Vec<ActivityDisclosureSection> {
    let mut sections = Vec::new();
    if activity.kind == ActivityKind::Command {
        if let Some(command) = activity
            .arguments
            .as_deref()
            .or(activity.display_target.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            sections.push(ActivityDisclosureSection {
                kind: ActivityDisclosureSectionKind::Command,
                content: command.to_owned(),
            });
        }
        if let Some(output) = activity
            .output
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            sections.push(ActivityDisclosureSection {
                kind: ActivityDisclosureSectionKind::Output,
                content: output.to_owned(),
            });
        } else if !activity.image_urls.is_empty() {
            sections.push(ActivityDisclosureSection {
                kind: ActivityDisclosureSectionKind::Output,
                content: String::new(),
            });
        }
        return sections;
    }
    // An edit renders as a diff, which says everything the raw arguments would
    // and reads. What the tool replied is only worth the room when it failed.
    let shows_diff = activity_shows_diff(activity);
    if let Some(arguments) = activity
        .arguments
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|_| !shows_diff)
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Arguments,
            content: arguments.to_owned(),
        });
    }
    if let Some(output) = activity
        .output
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|_| !shows_diff || activity.failed)
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Output,
            content: output.to_owned(),
        });
    } else if !activity.image_urls.is_empty() {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Output,
            content: String::new(),
        });
    }
    if sections.is_empty()
        && let Some(detail) = activity
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Detail,
            content: detail.to_owned(),
        });
    }
    sections
}

pub(super) fn activity_preview(activity: &ActivityItem) -> String {
    let detail = activity.detail.as_deref().unwrap_or_default().trim();
    if detail.eq_ignore_ascii_case("failed")
        && let Some(output) = activity.output.as_deref()
        && let Some(first_line) = output.lines().find(|line| !line.trim().is_empty())
    {
        return first_line.trim().to_owned();
    }
    if (detail.is_empty() || detail.eq_ignore_ascii_case("failed"))
        && !activity.image_urls.is_empty()
    {
        return tr!("activity.image_output");
    }
    detail.to_owned()
}

#[cfg(test)]
mod message_time_tests {
    use super::*;
    use chrono::TimeZone;

    /// Test-only rendering of disclosure sections into plain text; production
    /// renders them interactively via [`activity_disclosure_sections`].
    fn activity_disclosure_text(activity: &ActivityItem) -> Option<String> {
        let sections = activity_disclosure_sections(activity);
        (!sections.is_empty()).then(|| {
            sections
                .into_iter()
                .map(
                    |section| match (section.kind.label(), section.content.is_empty()) {
                        (Some(label), false) => format!("{label}\n{}", section.content),
                        (Some(label), true) => label.to_owned(),
                        (None, _) => section.content,
                    },
                )
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }

    fn local_datetime(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("test date should be valid in the local timezone")
    }

    fn unix_seconds(timestamp: DateTime<Local>) -> u64 {
        timestamp
            .timestamp()
            .try_into()
            .expect("test date should have a positive Unix timestamp")
    }

    #[test]
    fn message_time_includes_calendar_context_for_older_messages() {
        let now = local_datetime(2026, 8, 9, 16, 0); // Sunday

        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 9, 9, 5)), now),
            "9:05 AM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 8, 17, 0)), now),
            "Yesterday 5:00 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 7, 13, 12)), now),
            "Friday 1:12 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 5, 12, 23, 0)), now),
            "May 12th, 11:00 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2024, 8, 4, 11, 0)), now),
            "Aug 4th 2024, 11:00 AM"
        );
    }

    #[test]
    fn message_time_uses_correct_ordinal_suffixes() {
        let now = local_datetime(2026, 8, 9, 16, 0);

        for (day, suffix) in [
            (1, "st"),
            (2, "nd"),
            (3, "rd"),
            (11, "th"),
            (12, "th"),
            (13, "th"),
            (21, "st"),
        ] {
            let formatted =
                format_message_time_at(unix_seconds(local_datetime(2026, 5, day, 9, 0)), now);
            assert!(formatted.starts_with(&format!("May {day}{suffix},")));
        }
    }

    #[test]
    fn activity_disclosure_keeps_arguments_and_output() {
        let activity = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "Use Helium",
            Some("failed".into()),
            true,
        )
        .with_arguments(Some("{\n  \"actions\": []\n}".into()))
        .with_output(Some("Computer Use helper closed its session".into()))
        .with_failed(true);

        assert_eq!(
            activity_disclosure_sections(&activity),
            vec![
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Arguments,
                    content: "{\n  \"actions\": []\n}".into(),
                },
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Output,
                    content: "Computer Use helper closed its session".into(),
                },
            ]
        );
        assert_eq!(
            activity_disclosure_text(&activity).as_deref(),
            Some(
                "Arguments\n{\n  \"actions\": []\n}\n\nOutput\nComputer Use helper closed its session"
            )
        );
        assert_eq!(
            activity_preview(&activity),
            "Computer Use helper closed its session"
        );

        let image_only = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Screenshot",
            None,
            true,
        )
        .with_image_urls(vec!["data:image/png;base64,aGVsbG8=".into()]);
        assert_eq!(
            activity_disclosure_text(&image_only).as_deref(),
            Some("Output")
        );
        assert_eq!(activity_preview(&image_only), "Image output");
    }

    #[test]
    fn command_disclosure_shows_only_the_command_and_output() {
        let activity = ActivityItem::new(
            Some("command-1".into()),
            crate::model::ActivityKind::Command,
            "bash",
            Some("Completed".into()),
            true,
        )
        .with_arguments(Some(
            r#"{"command":"git status --short","description":"Check status"}"#.into(),
        ))
        .with_output(Some("clean".into()));

        assert_eq!(
            activity_disclosure_sections(&activity),
            vec![
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Command,
                    content: "git status --short".into(),
                },
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Output,
                    content: "clean".into(),
                },
            ]
        );
        assert_eq!(
            activity_disclosure_text(&activity).as_deref(),
            Some("Command\ngit status --short\n\nOutput\nclean")
        );
    }

    #[test]
    fn activity_display_title_prefers_the_human_facing_tool_argument() {
        let titled = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "Js",
            None,
            true,
        )
        .with_arguments(Some(
            r#"{"title":"Inspect Helium browser","code":"sky.get_app_state()"}"#.into(),
        ));
        let untitled = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Js",
            None,
            true,
        )
        .with_arguments(Some(r#"{"code":"sky.list_apps()"}"#.into()));

        assert_eq!(activity_display_title(&titled), "Inspect Helium browser");
        assert_eq!(activity_display_title(&untitled), "Js");
    }

    #[test]
    fn generic_tool_rows_keep_a_humanized_provider_name() {
        let named = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "mcp__threads__create_thread",
            None,
            true,
        );
        let unnamed = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Tool",
            None,
            true,
        );

        assert_eq!(activity_action_label(&named), "Tool");
        assert_eq!(activity_row_detail(&named, false), "Create thread");
        assert_eq!(activity_display_title(&named), "Create thread");
        assert_eq!(activity_action_label(&unnamed), "Tool");
        assert_eq!(activity_row_detail(&unnamed, false), "");
    }

    #[test]
    fn ask_user_question_has_a_purpose_specific_label() {
        let activity = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "AskUserQuestion",
            None,
            true,
        )
        .with_arguments(Some(r#"{"questions":[]}"#.into()));

        assert_eq!(activity_action_label(&activity), "Ask questions");
        assert_eq!(activity_row_detail(&activity, false), "");
        assert_eq!(activity_display_title(&activity), "Ask questions");
    }

    #[test]
    fn activity_header_summarizes_only_after_the_group_leaves_the_live_tail() {
        let reasoning = ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Inspecting history".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        );
        let command = ActivityItem::new(
            Some("command-1".into()),
            crate::model::ActivityKind::Command,
            "bash",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({"command": "git log --oneline -15"}).to_string(),
        ));
        let mut activities = vec![reasoning, command];

        assert_eq!(
            activity_header_title(&activities, true, None),
            "Running git log --oneline -15"
        );
        assert!(activity_group_is_live(true, true, 1, 1));
        activities[1].complete = true;
        assert_eq!(
            activity_header_title(&activities, true, None),
            "Ran git log --oneline -15"
        );
        assert!(!activity_group_is_live(true, true, 1, 2));
        assert_eq!(
            activity_header_title(&activities, false, None),
            "Ran 1 thought · 1 command"
        );
        assert!(!activity_group_is_live(true, false, 1, 1));
        assert!(!activity_group_is_live(false, true, 1, 1));
        assert_eq!(activity_action_label(&activities[1]), "Run");
        assert_eq!(
            activity_row_detail(&activities[1], false),
            "git log --oneline -15"
        );
    }

    #[test]
    fn file_edit_title_and_stats_follow_the_activity_state() {
        let mut activity = ActivityItem::new(
            Some("edit-1".into()),
            crate::model::ActivityKind::FileChange,
            "apply_patch",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({
                "patch": "*** Begin Patch\n*** Update File: /tmp/waku/src/app.rs\n@@\n-old\n+new\n+more\n*** End Patch"
            })
            .to_string(),
        ));

        assert_eq!(activity_display_title(&activity), "Editing app.rs");
        assert_eq!(activity_file_change_stats(&activity), None);

        activity.complete = true;
        assert_eq!(activity_display_title(&activity), "Edited app.rs");
        assert_eq!(activity_file_change_stats(&activity), Some((2, 1)));

        activity.failed = true;
        assert_eq!(activity_display_title(&activity), "Failed to edit app.rs");
        assert_eq!(activity_file_change_stats(&activity), None);
    }

    #[test]
    fn multi_file_edits_use_a_compact_count() {
        let activity = ActivityItem::new(
            Some("edit-2".into()),
            crate::model::ActivityKind::FileChange,
            "apply_patch",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({
                "patch": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** Update File: src/b.rs\n@@\n-c\n+d\n*** End Patch"
            })
            .to_string(),
        ));

        assert_eq!(activity_display_title(&activity), "Edited 2 files");
        assert_eq!(activity_file_change_stats(&activity), Some((2, 2)));
    }

    #[test]
    fn file_tool_titles_include_the_target_and_state() {
        let mut read = ActivityItem::new(
            Some("read-1".into()),
            crate::model::ActivityKind::FileRead,
            "read",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({"filePath": "/tmp/waku/src/model.rs"}).to_string(),
        ));
        assert_eq!(activity_display_title(&read), "Reading model.rs");
        read.complete = true;
        assert_eq!(activity_display_title(&read), "Read model.rs");
        read.failed = true;
        assert_eq!(activity_display_title(&read), "Failed to read model.rs");

        let search = ActivityItem::new(
            Some("grep-1".into()),
            crate::model::ActivityKind::FileSearch,
            "grep",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({"pattern": "ActivityKind"}).to_string(),
        ));
        assert_eq!(
            activity_display_title(&search),
            "Searched files for ActivityKind"
        );

        let list = ActivityItem::new(
            Some("list-1".into()),
            crate::model::ActivityKind::FileList,
            "ls",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({"path": "/tmp/waku/src"}).to_string(),
        ));
        assert_eq!(activity_display_title(&list), "Listing files in src");

        let custom = ActivityItem::new(
            Some("read-2".into()),
            crate::model::ActivityKind::FileRead,
            "Inspect generated manifest",
            None,
            true,
        );
        assert_eq!(
            activity_display_title(&custom),
            "Inspect generated manifest"
        );
    }

    #[test]
    fn command_web_search_and_plan_titles_include_their_state() {
        let mut command = ActivityItem::new(
            Some("command-1".into()),
            crate::model::ActivityKind::Command,
            "bash",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({
                "description": "Run focused tests",
                "command": "cargo test activity"
            })
            .to_string(),
        ));
        assert_eq!(
            activity_display_title(&command),
            "Ran command: Run focused tests"
        );
        command.complete = false;
        assert_eq!(
            activity_display_title(&command),
            "Running command: Run focused tests"
        );

        let web_search = ActivityItem::new(
            Some("search-1".into()),
            crate::model::ActivityKind::Search,
            "web_search",
            None,
            true,
        )
        .with_arguments(Some(serde_json::json!({"query": "Waku GPUI"}).to_string()));
        assert_eq!(
            activity_display_title(&web_search),
            "Searched the web for Waku GPUI"
        );

        let plan = ActivityItem::new(
            Some("plan-1".into()),
            crate::model::ActivityKind::Plan,
            "update_plan",
            None,
            false,
        );
        assert_eq!(activity_display_title(&plan), "Updating plan");
    }
}

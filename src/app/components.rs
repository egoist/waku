use super::*;

use chrono::{Datelike, Days};

pub(super) fn pulse_dot(id: impl Into<SharedString>, size: f32, color: Hsla) -> AnyElement {
    div()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .rounded_full()
        .bg(color)
        .with_animation(
            id.into(),
            Animation::new(Duration::from_millis(1600))
                .repeat()
                .with_easing(pulsating_between(0.3, 1.0)),
            |element, delta| element.opacity(delta),
        )
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
            let time = timestamp
                .format("%I:%M %p")
                .to_string()
                .trim_start_matches('0')
                .to_owned();

            if message_date >= today {
                return time;
            }

            if today.pred_opt() == Some(message_date) {
                return format!("Yesterday {time}");
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
    copy_content: String,
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
        .text_size(px(11.5))
        .line_height(px(14.0))
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
        .tooltip(move |window, cx| {
            Tooltip::new(if copied { "Copied" } else { "Copy message" }).build(window, cx)
        })
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.clone()));
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
            footer = footer.child(
                div()
                    .id(SharedString::from(format!("fork-response-{message_id}")))
                    .w(px(27.0))
                    .h(px(27.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay_strong))
                    .child(icon("icons/fork.svg", 14.0, footer_color))
                    .tooltip(|window, cx| Tooltip::new("Fork task").build(window, cx))
                    .on_click(move |_, _, cx| {
                        let _ = fork_waku.update(cx, |this, cx| {
                            this.fork_session_from_response(
                                action.session_id,
                                action.turn_count,
                                cx,
                            );
                        });
                    }),
            );
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
                .tooltip(|window, cx| Tooltip::new("Revert to here").build(window, cx))
                .on_click(move |_, window, cx| {
                    let _ = edit_waku.update(cx, |this, cx| {
                        this.begin_message_edit(action.session_id, action.turn_count, window, cx);
                    });
                }),
        );
    }

    footer.into_any_element()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_message(
    theme: &Theme,
    message: &Message,
    assistant_footer_copy_content: Option<String>,
    assistant_footer_time: Option<u64>,
    copied: bool,
    assistant_message_action: Option<AssistantMessageAction>,
    user_message_action: Option<UserMessageAction>,
    message_edit_input: Option<Entity<ComposerInput>>,
    session_id: Uuid,
    transcript_resize_tx: crossbeam_channel::Sender<TranscriptMarkdownResize>,
    transcript_layout_width: Pixels,
    transcript_rows: ListState,
    transcript_viewport: TextViewScrollViewport,
    text_state: Entity<TextViewState>,
    waku: gpui::WeakEntity<Waku>,
    composer: Entity<ComposerInput>,
    cx: &mut App,
) -> AnyElement {
    let content = message.content.clone();
    let message_id = message.id;
    let role = message.role;
    let code = fenced_code(&content);
    let menu_content = content.clone();
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
            if let Some(edit_input) = message_edit_input {
                let can_submit = !edit_input.read(cx).content().trim().is_empty();
                let cancel_waku = waku.clone();
                let submit_waku = waku.clone();
                column = column.child(
                    div()
                        .w_full()
                        .max_w(px(540.0))
                        .rounded(px(12.0))
                        .bg(theme.raised)
                        .px(px(12.0))
                        .pt(px(9.0))
                        .pb(px(8.0))
                        .child(edit_input)
                        .child(
                            div()
                                .mt(px(7.0))
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
                                        .text_size(px(11.5))
                                        .text_color(theme.text_secondary)
                                        .cursor_default()
                                        .hover(|element| element.bg(theme.overlay_strong))
                                        .child("Cancel")
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
                                        .text_size(px(11.5))
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
                                        .child("Send")
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
                column = column.child(
                    div()
                        .max_w(px(540.0))
                        .rounded(px(12.0))
                        .bg(theme.raised)
                        .px(px(12.0))
                        .py(px(8.0))
                        .text_size(px(14.0))
                        .line_height(px(20.0))
                        .text_color(theme.text)
                        .whitespace_normal()
                        .child(
                            selectable_plain_text(
                                SharedString::from(format!("message-{message_id}-user")),
                                &content,
                                text_state,
                                cx,
                            )
                            .selection_scroll_handle(&transcript_rows)
                            .block_viewport(transcript_viewport),
                        ),
                );
                column = column.child(render_message_footer(
                    theme,
                    message,
                    message.created_at,
                    message.content.clone(),
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
            let resize_tx = transcript_resize_tx.clone();
            let resize_waku = waku.clone();
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .gap(px(3.0))
                .group(group_name.clone())
                .text_size(px(13.5))
                .line_height(px(21.0))
                .text_color(theme.text)
                .child(
                    TextView::markdown_with_state(
                        SharedString::from(format!("message-{message_id}-assistant")),
                        content,
                        text_state,
                        cx,
                    )
                    .update_delay(STREAM_MARKDOWN_DELAY)
                    .style(assistant_markdown_style(theme))
                    .selectable(true)
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport)
                    .block_layout_width(transcript_layout_width)
                    .on_block_resize(move |resize: TextViewBlockResize, cx| {
                        let _ = resize_tx.send(TranscriptMarkdownResize {
                            session_id,
                            message_id,
                            delta: resize.delta,
                            anchor_delta: if resize.above_viewport {
                                resize.delta
                            } else {
                                Pixels::ZERO
                            },
                        });
                        if let Some(waku) = resize_waku.upgrade() {
                            cx.notify(waku.entity_id());
                        }
                    })
                    .w_full()
                    .cursor_text(),
                );
            if message.streaming {
                column = column.child(pulse_dot(
                    format!("stream-{}", message.id),
                    6.0,
                    theme.accent,
                ));
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
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(theme.text_tertiary)
                .child(
                    selectable_plain_text(
                        SharedString::from(format!("message-{message_id}-system")),
                        &content,
                        text_state,
                        cx,
                    )
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport),
                ),
        ),
    };

    element
        .id(message_id)
        .context_menu_with_id(
            SharedString::from(format!("message-context-menu-{message_id}")),
            move |menu, window, cx| {
                let copy_content = menu_content.clone();
                let mut menu =
                    preserve_composer_focus_for_context_menu(&composer, menu, window, cx)
                        .min_w(px(170.0))
                        .item(
                            PopupMenuItem::new("Copy Message").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_content.clone(),
                                ));
                            }),
                        );

                if role == MessageRole::User && user_message_action.is_none() {
                    let composer = composer.clone();
                    let edit_content = menu_content.clone();
                    menu = menu.item(PopupMenuItem::new("Copy to Composer").on_click(
                        move |_, window, cx| {
                            composer.update(cx, |composer, cx| {
                                composer.set_content(edit_content.clone(), cx);
                            });
                            let focus_handle = composer.read(cx).focus();
                            window.focus(&focus_handle, cx);
                        },
                    ));
                }

                if let Some(code) = code.clone() {
                    menu = menu.item(PopupMenuItem::new("Copy Code").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                    }));
                }

                if let Some(action) = user_message_action {
                    let action_waku = waku.clone();
                    menu = menu.item(PopupMenuItem::new("Revert to Here").on_click(
                        move |_, window, cx| {
                            let _ = action_waku.update(cx, |this, cx| {
                                this.begin_message_edit(
                                    action.session_id,
                                    action.turn_count,
                                    window,
                                    cx,
                                );
                            });
                        },
                    ));
                }

                if let Some(action) = assistant_message_action {
                    let action_waku = waku.clone();
                    menu = menu.item(PopupMenuItem::new("Fork Task").on_click(move |_, _, cx| {
                        let _ = action_waku.update(cx, |this, cx| {
                            this.fork_session_from_response(
                                action.session_id,
                                action.turn_count,
                                cx,
                            );
                        });
                    }));
                }

                menu
            },
        )
        .into_any_element()
}

pub(super) fn assistant_markdown_style(theme: &Theme) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.75))
        .heading_font_size(|level, base| match level {
            1 => base * 1.5,
            2 => base * 1.3,
            3 => base * 1.15,
            4 => base * 1.05,
            _ => base,
        })
        .code_block(
            StyleRefinement::default()
                .bg(theme.inset)
                .border_1()
                .border_color(theme.border_strong)
                .rounded(px(8.0))
                .p(px(12.0))
                .text_size(px(12.0)),
        )
}

pub(super) fn selectable_plain_text(
    id: impl Into<gpui::ElementId>,
    content: &str,
    state: Entity<TextViewState>,
    cx: &mut App,
) -> TextView {
    let html = if content.is_empty() {
        "<p></p>".to_owned()
    } else {
        content
            .split('\n')
            .map(|line| format!("<p>{}</p>", escape_html(line)))
            .collect::<String>()
    };
    TextView::html_with_state(id, html, state, cx)
        .style(TextViewStyle::default().paragraph_gap(rems(0.0)))
        .selectable(true)
        .w_full()
        .cursor_text()
}

pub(super) fn escape_html(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
            format!("{count} {}", if count == 1 { singular } else { plural })
        })
        .collect::<Vec<_>>();
    let running = activities.iter().any(|activity| !activity.complete);
    format!(
        "{} {}",
        if running { "Running" } else { "Ran" },
        parts.join(" · ")
    )
}

pub(super) fn activity_display_title(activity: &ActivityItem) -> String {
    if activity.kind == crate::model::ActivityKind::Tool
        && let Some(arguments) = activity.arguments.as_deref()
        && let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(title) = arguments
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
    {
        return title.to_owned();
    }
    activity.title.clone()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ActivityDisclosureSectionKind {
    Arguments,
    Output,
    Detail,
}

impl ActivityDisclosureSectionKind {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::Output => "output",
            Self::Detail => "detail",
        }
    }

    pub(super) fn label(self) -> Option<&'static str> {
        match self {
            Self::Arguments => Some("Arguments"),
            Self::Output => Some("Output"),
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
    if let Some(arguments) = activity
        .arguments
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
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

pub(super) fn activity_disclosure_text(activity: &ActivityItem) -> Option<String> {
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
        return "Image output".to_owned();
    }
    detail.to_owned()
}

pub(super) fn git_branch(path: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(path)
        .output()
        .ok()?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}

#[cfg(test)]
mod message_time_tests {
    use super::*;
    use chrono::TimeZone;

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
}

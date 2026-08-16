use super::*;

#[derive(Clone)]
pub(super) struct MessageAnnotationRender {
    pub(super) message_id: Uuid,
    pub(super) annotations: Vec<ComposerAnnotation>,
    pub(super) active: Option<Uuid>,
    pub(super) editor: Entity<ComposerInput>,
    pub(super) handle: ContextMenuHandle,
}

/// Serialize pending notes after the ordinary user prompt. XML escaping keeps
/// quoted provider text from forging annotation boundaries of its own.
pub(super) fn append_annotations_to_prompt(
    prompt: &mut String,
    annotations: &[ComposerAnnotation],
) {
    let annotations = annotations
        .iter()
        .filter(|annotation| !annotation.comment.trim().is_empty())
        .collect::<Vec<_>>();
    if annotations.is_empty() {
        return;
    }
    prompt.push_str("\n\n<conversation_annotations>\n");
    for annotation in annotations {
        prompt.push_str("  <annotation>\n    <selection>");
        prompt.push_str(&escape_annotation_xml(annotation.quote.trim()));
        prompt.push_str("</selection>\n    <comment>");
        prompt.push_str(&escape_annotation_xml(annotation.comment.trim()));
        prompt.push_str("</comment>\n  </annotation>\n");
    }
    prompt.push_str("</conversation_annotations>");
}

/// Add the same completed notes to the user-visible message as restrained
/// Markdown. The provider still receives the machine-readable XML above.
pub(super) fn append_annotations_to_display(
    display: &mut String,
    annotations: &[ComposerAnnotation],
    heading: &str,
) {
    let annotations = annotations
        .iter()
        .filter(|annotation| !annotation.comment.trim().is_empty())
        .collect::<Vec<_>>();
    if annotations.is_empty() {
        return;
    }
    if !display.trim().is_empty() {
        display.push_str("\n\n");
    }
    display.push_str("**");
    display.push_str(heading);
    display.push_str("**\n\n");
    for (index, annotation) in annotations.iter().enumerate() {
        if index > 0 {
            display.push_str("\n\n");
        }
        display.push_str(&format!("**{}.**\n", index + 1));
        for line in annotation.quote.trim().lines() {
            display.push_str("> ");
            display.push_str(line);
            display.push('\n');
        }
        display.push('\n');
        display.push_str(annotation.comment.trim());
    }
}

fn escape_annotation_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Waku {
    pub(super) fn begin_annotation(
        &mut self,
        message_id: Uuid,
        quote: String,
        ranges: Vec<ComposerAnnotationRange>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let quote = quote.trim().to_owned();
        if quote.is_empty() {
            return;
        }
        let id = Uuid::new_v4();
        self.composer_annotations.push(ComposerAnnotation {
            id,
            message_id,
            quote,
            comment: String::new(),
            ranges,
        });
        self.active_annotation = Some(id);
        self.annotation_editor
            .update(cx, |input, cx| input.clear(cx));
        self.transcript_selection.clear();
        self.schedule_composer_draft_save(cx);
        let text_index = self
            .composer_annotations
            .last()
            .and_then(|annotation| annotation.ranges.first())
            .map_or(0, |range| range.text_index);
        let handle = self.menu_handle(format!("message-annotations-{message_id}-{text_index}"), cx);
        let focus = self.annotation_editor.read(cx).focus();
        window.defer(cx, move |window, cx| {
            crate::ui::menu::open_popover_at_mouse(&handle, window, cx);
            window.focus(&focus, cx);
        });
        cx.notify();
    }

    pub(super) fn edit_annotation(
        &mut self,
        annotation_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(comment) = self
            .composer_annotations
            .iter()
            .find(|annotation| annotation.id == annotation_id)
            .map(|annotation| annotation.comment.clone())
        else {
            return;
        };
        self.active_annotation = Some(annotation_id);
        self.annotation_editor
            .update(cx, |input, cx| input.set_content(comment, cx));
        let focus = self.annotation_editor.read(cx).focus();
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn finish_annotation_edit(
        &mut self,
        comment: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(annotation_id) = self.active_annotation else {
            return;
        };
        let Some((message_id, text_index)) = self
            .composer_annotations
            .iter_mut()
            .find(|annotation| annotation.id == annotation_id)
            .map(|annotation| {
                if let Some(comment) = comment {
                    annotation.comment = comment;
                }
                (
                    annotation.message_id,
                    annotation
                        .ranges
                        .first()
                        .map_or(0, |range| range.text_index),
                )
            })
        else {
            return;
        };
        self.active_annotation = None;
        self.schedule_composer_draft_save(cx);
        self.menu_handle(format!("message-annotations-{message_id}-{text_index}"), cx)
            .close(window, cx);
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn remove_annotation(&mut self, annotation_id: Uuid, cx: &mut Context<Self>) {
        self.composer_annotations
            .retain(|annotation| annotation.id != annotation_id);
        if self.active_annotation == Some(annotation_id) {
            self.active_annotation = None;
            self.annotation_editor
                .update(cx, |input, cx| input.clear(cx));
        }
        self.schedule_composer_draft_save(cx);
        cx.notify();
    }

    fn go_to_annotation(&mut self, annotation_id: Uuid, cx: &mut Context<Self>) {
        let Some(message_id) = self
            .composer_annotations
            .iter()
            .find(|annotation| annotation.id == annotation_id)
            .map(|annotation| annotation.message_id)
        else {
            return;
        };
        let Some(message_index) = self.selected_session().and_then(|session| {
            session
                .messages
                .iter()
                .position(|message| message.id == message_id)
        }) else {
            return;
        };
        self.refresh_transcript_row_kinds();
        let row_index = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::Message(message_index));
        let Some(row_index) = row_index else {
            return;
        };
        self.transcript_anchor_following.set(false);
        self.active_transcript_rows().scroll_to(ListOffset {
            item_ix: row_index,
            offset_in_item: Pixels::ZERO,
        });
        self.transcript_is_scrolled.set(true);
        cx.notify();
    }

    pub(super) fn render_composer_annotation_pill(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let count = self.composer_annotations.len();
        let annotations = self.composer_annotations.clone();
        let handle = self.menu_handle("composer-annotations", cx);
        let waku = cx.entity().downgrade();
        let trigger = div()
            .id("composer-annotations-trigger")
            .h(px(24.0))
            .px(px(9.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded_full()
            .border_1()
            .border_color(theme.accent.opacity(0.28))
            .bg(theme
                .accent
                .opacity(if theme.is_dark { 0.10 } else { 0.07 }))
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text_secondary)
            .cursor_default()
            .hover(|element| element.bg(theme.accent.opacity(0.14)))
            .child(icon("icons/comment.svg", 12.0, theme.accent))
            .child(tr!("annotations.count", count = count))
            .child(icon("icons/chevron-down.svg", 10.0, theme.text_tertiary));
        let popover_theme = theme;
        let popover_handle = handle.clone();
        let overview = popover(trigger, &handle, MenuAlign::AboveLeft, move |_, _, _| {
            let mut list = div()
                .id("composer-annotations-list")
                .max_h(px(300.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(2.0));
            for (index, annotation) in annotations.iter().cloned().enumerate() {
                let annotation_id = annotation.id;
                let go_waku = waku.clone();
                let go_handle = popover_handle.clone();
                let delete_waku = waku.clone();
                let delete_handle = popover_handle.clone();
                let comment = annotation.comment.trim().to_owned();
                list = list.child(
                    div()
                        .min_h(px(38.0))
                        .rounded(px(7.0))
                        .px(px(6.0))
                        .py(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .hover(|element| element.bg(popover_theme.overlay))
                        .child(
                            div()
                                .size(px(20.0))
                                .flex_none()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(popover_theme.accent.opacity(if popover_theme.is_dark {
                                    0.14
                                } else {
                                    0.09
                                }))
                                .text_size(px(10.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(popover_theme.accent)
                                .child((index + 1).to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .line_clamp(1)
                                .text_ellipsis()
                                .text_size(px(11.5))
                                .line_height(px(15.0))
                                .text_color(if comment.is_empty() {
                                    popover_theme.text_ghost
                                } else {
                                    popover_theme.text
                                })
                                .child(if comment.is_empty() {
                                    tr!("annotations.comment_placeholder")
                                } else {
                                    comment
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "composer-annotation-{annotation_id}-goto"
                                        )))
                                        .size(px(24.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_default()
                                        .hover(|element| element.bg(popover_theme.overlay_strong))
                                        .child(icon(
                                            "icons/arrow-right.svg",
                                            11.0,
                                            popover_theme.text_tertiary,
                                        ))
                                        .tooltip(Tooltip::text(tr!("annotations.goto")))
                                        .on_click(move |_, window, cx| {
                                            go_handle.close(window, cx);
                                            let _ = go_waku.update(cx, |this, cx| {
                                                this.go_to_annotation(annotation_id, cx);
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "composer-annotation-{annotation_id}-remove"
                                        )))
                                        .size(px(24.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_default()
                                        .hover(|element| element.bg(popover_theme.danger_soft))
                                        .child(icon(
                                            "icons/trash.svg",
                                            11.0,
                                            popover_theme.text_tertiary,
                                        ))
                                        .tooltip(Tooltip::text(tr!("annotations.remove")))
                                        .on_click(move |_, window, cx| {
                                            let _ = delete_waku.update(cx, |this, cx| {
                                                this.remove_annotation(annotation_id, cx);
                                                if this.composer_annotations.is_empty() {
                                                    delete_handle.close(window, cx);
                                                }
                                            });
                                        }),
                                ),
                        ),
                );
            }
            div()
                .w(px(310.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(popover_theme.border_strong)
                .bg(popover_theme.raised)
                .shadow_lg()
                .p(px(10.0))
                .child(
                    div()
                        .mb(px(7.0))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(popover_theme.text)
                        .child(tr!("annotations.count", count = count)),
                )
                .child(list)
                .into_any_element()
        });
        div()
            .px(px(4.0))
            .pt(px(2.0))
            .pb(px(7.0))
            .flex()
            .items_start()
            .child(overview)
    }
}

pub(super) fn render_message_annotation_indicator(
    annotation: MessageAnnotationRender,
    theme: &Theme,
    waku: gpui::WeakEntity<Waku>,
) -> AnyElement {
    let theme = *theme;
    let MessageAnnotationRender {
        message_id,
        annotations,
        active,
        editor,
        handle,
    } = annotation;
    let count = annotations.len();
    let first_id = annotations.first().map(|annotation| annotation.id);
    let activate_waku = waku.clone();
    let trigger = div()
        .id(SharedString::from(format!(
            "message-annotations-{message_id}-trigger"
        )))
        .h(px(23.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded_full()
        .border_1()
        .border_color(theme.accent.opacity(0.24))
        .bg(theme
            .accent
            .opacity(if theme.is_dark { 0.10 } else { 0.07 }))
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.accent)
        .cursor_default()
        .hover(|element| element.bg(theme.accent.opacity(0.16)))
        .child(icon("icons/comment.svg", 11.5, theme.accent))
        .child(count.to_string())
        .tooltip(Tooltip::text(tr!("annotations.show")))
        .when_some(first_id, move |element, annotation_id| {
            element.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let _ = activate_waku.update(cx, |this, cx| {
                    if this.active_annotation != Some(annotation_id) {
                        this.edit_annotation(annotation_id, window, cx);
                    }
                });
            })
        });

    let card_annotations = annotations.clone();
    let anchored = popover(
        trigger,
        &handle,
        MenuAlign::AboveLeft,
        move |card_handle, _window, _cx| {
            let confirm_handle = card_handle.clone();
            let confirm_waku = waku.clone();
            let mut list = div().flex().flex_col().gap(px(10.0));
            for annotation in card_annotations.iter().cloned() {
                let annotation_id = annotation.id;
                let editing = active == Some(annotation_id);
                let edit_waku = waku.clone();
                let remove_waku = waku.clone();
                let comment = annotation.comment.clone();
                let quote = annotation.quote.clone();
                let body = if editing {
                    div()
                        .min_h(px(56.0))
                        .px(px(9.0))
                        .py(px(7.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.surface)
                        .child(editor.clone())
                        .into_any_element()
                } else {
                    div()
                        .id(SharedString::from(format!(
                            "annotation-{annotation_id}-edit"
                        )))
                        .min_h(px(32.0))
                        .px(px(9.0))
                        .py(px(7.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface)
                        .cursor_default()
                        .hover(|element| element.border_color(theme.border_strong))
                        .text_size(px(12.5))
                        .line_height(px(17.0))
                        .text_color(if comment.trim().is_empty() {
                            theme.text_ghost
                        } else {
                            theme.text
                        })
                        .child(if comment.trim().is_empty() {
                            tr!("annotations.comment_placeholder")
                        } else {
                            comment
                        })
                        .on_click(move |_, window, cx| {
                            let _ = edit_waku.update(cx, |this, cx| {
                                this.edit_annotation(annotation_id, window, cx);
                            });
                        })
                        .into_any_element()
                };
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(7.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .max_h(px(38.0))
                                        .overflow_hidden()
                                        .text_size(px(11.0))
                                        .line_height(px(15.0))
                                        .text_color(theme.text_tertiary)
                                        .child(format!("“{}”", quote.trim())),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "annotation-{annotation_id}-remove"
                                        )))
                                        .size(px(20.0))
                                        .flex_none()
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_default()
                                        .hover(|element| element.bg(theme.danger_soft))
                                        .child(icon("icons/trash.svg", 11.0, theme.text_tertiary))
                                        .tooltip(Tooltip::text(tr!("annotations.remove")))
                                        .on_click(move |_, _, cx| {
                                            let _ = remove_waku.update(cx, |this, cx| {
                                                this.remove_annotation(annotation_id, cx);
                                            });
                                        }),
                                ),
                        )
                        .child(body),
                );
            }
            div()
                .w(px(340.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.raised)
                .shadow_lg()
                .p(px(12.0))
                .on_action(move |_: &ConfirmEntry, window, cx| {
                    let _ = confirm_waku.update(cx, |this, cx| {
                        this.finish_annotation_edit(None, window, cx);
                    });
                    confirm_handle.close(window, cx);
                    window.refresh();
                })
                .child(
                    div()
                        .mb(px(10.0))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(icon("icons/comment.svg", 13.0, theme.accent))
                        .child(tr!("annotations.count", count = count)),
                )
                .child(list)
                .into_any_element()
        },
    );
    div()
        .absolute()
        .right(px(-30.0))
        .top(px(-1.0))
        .child(anchored)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_only_completed_annotations_and_escapes_boundaries() {
        let mut prompt = "Please revise this.".to_owned();
        append_annotations_to_prompt(
            &mut prompt,
            &[
                ComposerAnnotation {
                    id: Uuid::nil(),
                    message_id: Uuid::nil(),
                    quote: "Use <unsafe> & hope".into(),
                    comment: "Explain > replace".into(),
                    ranges: Vec::new(),
                },
                ComposerAnnotation {
                    id: Uuid::nil(),
                    message_id: Uuid::nil(),
                    quote: "ignored".into(),
                    comment: "  ".into(),
                    ranges: Vec::new(),
                },
            ],
        );
        assert!(prompt.contains("Use &lt;unsafe&gt; &amp; hope"));
        assert!(prompt.contains("Explain &gt; replace"));
        assert!(!prompt.contains("ignored"));
    }

    #[test]
    fn displays_completed_annotations_as_readable_markdown() {
        let annotations = vec![
            ComposerAnnotation {
                id: Uuid::nil(),
                message_id: Uuid::nil(),
                quote: "Use the older API".into(),
                comment: "Replace this with the current API.".into(),
                ranges: Vec::new(),
            },
            ComposerAnnotation {
                id: Uuid::nil(),
                message_id: Uuid::nil(),
                quote: "unfinished".into(),
                comment: " ".into(),
                ranges: Vec::new(),
            },
        ];
        let mut display = "Please update the example.".to_owned();
        append_annotations_to_display(&mut display, &annotations, "Annotations");

        assert_eq!(
            display,
            "Please update the example.\n\n**Annotations**\n\n**1.**\n> Use the older API\n\nReplace this with the current API."
        );
        assert!(!display.contains("unfinished"));
    }
}

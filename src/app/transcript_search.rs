//! Find-in-page for the visible transcript.
//!
//! The query is scanned only when it changes. Results carry the virtualized
//! row plus the markdown renderer's stable element ordinal and byte range, so
//! navigation can mount an off-screen message, highlight the exact glyphs,
//! and then reveal the wrapped line without doing search work in a frame.

use regex::{Regex, RegexBuilder};

use super::*;
use crate::md::render::{SearchHighlights, TextSearchMatch};
use crate::md::selection::TextKey;
use crate::ui::ActivationExt;

const MAX_TRANSCRIPT_SEARCH_MATCHES: usize = 20_000;

pub(super) struct TranscriptSearch {
    open: bool,
    query: Entity<TextInput>,
    matches: Vec<TranscriptSearchMatch>,
    matches_by_message: HashMap<usize, Rc<Vec<TextSearchMatch>>>,
    current: Option<usize>,
    limited: bool,
    previous_focus: Option<FocusHandle>,
    generation: u64,
    pending_reveal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TranscriptSearchMatch {
    message_index: usize,
    row_index: usize,
    message_id: Uuid,
    text: TextSearchMatch,
}

#[derive(Clone)]
struct TranscriptSearchTarget {
    generation: u64,
    current: usize,
    row_index: usize,
    key: TextKey,
    range: Range<usize>,
}

impl TranscriptSearch {
    fn target(&self) -> Option<TranscriptSearchTarget> {
        let current = self.current?;
        let found = self.matches.get(current)?;
        Some(TranscriptSearchTarget {
            generation: self.generation,
            current,
            row_index: found.row_index,
            key: TextKey::new(format!("message-{}", found.message_id), found.text.ordinal),
            range: found.text.range.clone(),
        })
    }
}

impl Waku {
    pub(super) fn refresh_transcript_search_localized_text(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &self.transcript_search else {
            return;
        };
        search
            .query
            .update(cx, |input, cx| input.set_placeholder(tr!("input.find"), cx));
    }

    pub(super) fn transcript_search_open(&self) -> bool {
        self.transcript_search
            .as_ref()
            .is_some_and(|search| search.open)
    }

    fn ensure_transcript_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transcript_search.is_some() {
            return;
        }
        let query = cx.new(|cx| TextInput::new(window, cx).placeholder(tr!("input.find")));
        cx.subscribe(
            &query,
            |this: &mut Self, _, event: &InputEvent, cx| match event {
                InputEvent::Edited => this.refresh_transcript_search(cx),
                InputEvent::Submit(_) => this.navigate_transcript_search(false, cx),
                InputEvent::Focus | InputEvent::BackspaceOnEmpty => {}
            },
        )
        .detach();
        self.transcript_search = Some(TranscriptSearch {
            open: false,
            query,
            matches: Vec::new(),
            matches_by_message: HashMap::new(),
            current: None,
            limited: false,
            previous_focus: None,
            generation: 0,
            pending_reveal: false,
        });
    }

    pub(super) fn open_transcript_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .selected_session()
            .is_none_or(|session| session.messages.is_empty())
        {
            cx.propagate();
            return;
        }
        let previous_focus = window.focused(cx);
        let seed = self
            .transcript_selection
            .selection
            .borrow()
            .selected_text()
            .filter(|text| !text.is_empty() && !text.contains('\n'));
        self.ensure_transcript_search(window, cx);

        let search = self
            .transcript_search
            .as_mut()
            .expect("ensure_transcript_search just created it");
        if !search.open {
            search.previous_focus = previous_focus;
        }
        search.open = true;
        let query = search.query.clone();
        if let Some(seed) = seed {
            query.update(cx, |query, cx| query.set_content(seed, cx));
        }
        query.update(cx, |query, cx| query.select_all_text(cx));
        window.focus(&query.read(cx).focus(), cx);
        self.refresh_transcript_search(cx);
        cx.notify();
    }

    pub(super) fn close_transcript_search(
        &mut self,
        restore_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.transcript_search.as_mut().filter(|search| search.open) else {
            return;
        };
        search.open = false;
        search.matches.clear();
        search.matches_by_message.clear();
        search.current = None;
        search.limited = false;
        search.pending_reveal = false;
        search.generation = search.generation.wrapping_add(1);
        let previous_focus = search.previous_focus.take();
        if restore_focus && let Some(focus) = previous_focus {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(super) fn reset_transcript_search_for_session(&mut self) {
        let Some(search) = self.transcript_search.as_mut() else {
            return;
        };
        search.open = false;
        search.matches.clear();
        search.matches_by_message.clear();
        search.current = None;
        search.limited = false;
        search.previous_focus = None;
        search.pending_reveal = false;
        search.generation = search.generation.wrapping_add(1);
    }

    fn refresh_transcript_search(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.transcript_search.as_ref().filter(|search| search.open) else {
            return;
        };
        let query = search.query.read(cx).content().to_owned();
        self.sync_transcript_rows();
        let origin_row = self.active_transcript_rows().logical_scroll_top().item_ix;
        let row_kinds = self.transcript_row_kinds.borrow().clone();

        let mut matches = Vec::new();
        let mut matches_by_message = HashMap::new();
        let mut limited = false;
        if !query.is_empty() {
            let regex = literal_find_regex(&query);
            if let Some(session) = self.selected_session() {
                for (row_index, kind) in row_kinds.iter().copied().enumerate() {
                    let TranscriptRowKind::Message(message_index) = kind else {
                        continue;
                    };
                    let Some(message) = session.messages.get(message_index) else {
                        continue;
                    };
                    let remaining = MAX_TRANSCRIPT_SEARCH_MATCHES.saturating_sub(matches.len());
                    let (message_matches, message_limited) =
                        if matches!(message.role, MessageRole::User | MessageRole::Assistant) {
                            md::render::markdown_search_matches(
                                message.visible_content(),
                                &regex,
                                remaining,
                            )
                        } else {
                            md::render::plain_search_matches(
                                message.visible_content(),
                                0,
                                &regex,
                                remaining,
                            )
                        };
                    if !message_matches.is_empty() {
                        for text in &message_matches {
                            matches.push(TranscriptSearchMatch {
                                message_index,
                                row_index,
                                message_id: message.id,
                                text: text.clone(),
                            });
                        }
                        matches_by_message.insert(message_index, Rc::new(message_matches));
                    }
                    if message_limited {
                        limited = true;
                        break;
                    }
                }
            }
        }

        let current = (!matches.is_empty()).then(|| {
            matches
                .iter()
                .position(|found| found.row_index >= origin_row)
                .unwrap_or(0)
        });
        let search = self
            .transcript_search
            .as_mut()
            .expect("search remains present while it is refreshed");
        search.matches = matches;
        search.matches_by_message = matches_by_message;
        search.current = current;
        search.limited = limited;
        search.generation = search.generation.wrapping_add(1);
        search.pending_reveal = current.is_some();
        cx.notify();
    }

    pub(super) fn navigate_transcript_search(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let Some(search) = self
            .transcript_search
            .as_mut()
            .filter(|search| search.open && !search.matches.is_empty())
        else {
            return;
        };
        let count = search.matches.len();
        search.current = Some(match search.current {
            Some(current) if backwards => (current + count - 1) % count,
            Some(current) => (current + 1) % count,
            None if backwards => count - 1,
            None => 0,
        });
        search.generation = search.generation.wrapping_add(1);
        search.pending_reveal = true;
        cx.notify();
    }

    pub(super) fn transcript_search_highlights(
        &self,
        message_index: usize,
    ) -> Option<SearchHighlights> {
        let search = self
            .transcript_search
            .as_ref()
            .filter(|search| search.open)?;
        let matches = search.matches_by_message.get(&message_index)?.clone();
        let active = search
            .current
            .and_then(|current| search.matches.get(current))
            .filter(|found| found.message_index == message_index)
            .map(|found| found.text.clone());
        Some(SearchHighlights { matches, active })
    }

    /// Reveal a pending result without disturbing an already-mounted row.
    ///
    /// `ListState::scroll_to_reveal_item` resets the intra-item offset when the
    /// target is the current row. Transcript messages can be taller than the
    /// viewport, so doing that before the exact glyph reveal produces a
    /// visible one-frame jump to the top of the message. Reuse the previous
    /// frame's registered text geometry whenever it exists; only mount a row
    /// first when the result is genuinely off screen.
    pub(super) fn apply_pending_transcript_search_reveal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.transcript_search.as_mut().and_then(|search| {
            if !search.open || !search.pending_reveal {
                return None;
            }
            search.pending_reveal = false;
            search.target()
        });
        let Some(target) = target else {
            return;
        };

        if self.transcript_search_match_bounds(&target).is_some() {
            self.reveal_transcript_search_geometry(target, 1, window, cx);
            return;
        }

        self.detach_transcript_search_from_tail();
        self.active_transcript_rows()
            .scroll_to_reveal_item(target.row_index);
        cx.on_next_frame(window, move |this, window, cx| {
            this.reveal_transcript_search_geometry(target, 0, window, cx)
        });
    }

    fn transcript_search_match_bounds(
        &self,
        target: &TranscriptSearchTarget,
    ) -> Option<Bounds<Pixels>> {
        let registry = self.transcript_selection.registry.borrow();
        registry
            .entries()
            .iter()
            .find(|entry| entry.key == target.key)
            .and_then(|entry| {
                md::render::text_range_bounds(&entry.geometry, &target.range)
                    .into_iter()
                    .next()
            })
    }

    fn detach_transcript_search_from_tail(&self) {
        self.transcript_anchor_following.set(false);
        self.transcript_tail_recheck.set(false);
        self.transcript_is_scrolled.set(true);
    }

    fn reveal_transcript_search_geometry(
        &mut self,
        target: TranscriptSearchTarget,
        attempt: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let still_current = self.transcript_search.as_ref().is_some_and(|search| {
            search.open
                && search.generation == target.generation
                && search.current == Some(target.current)
        });
        if !still_current {
            return;
        }
        let match_bounds = self.transcript_search_match_bounds(&target);
        let Some(match_bounds) = match_bounds else {
            if attempt < 1 {
                cx.on_next_frame(window, move |this, window, cx| {
                    this.reveal_transcript_search_geometry(target, attempt + 1, window, cx)
                });
            }
            return;
        };

        let rows = self.active_transcript_rows();
        let viewport = rows.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return;
        }
        let margin = px(24.0);
        if match_bounds.top() >= viewport.top() + margin
            && match_bounds.bottom() <= viewport.bottom() - margin
        {
            return;
        }
        let target_y = viewport.top() + viewport.size.height * 0.35;
        let current = rows.scroll_px_offset_for_scrollbar().y;
        let max_offset = rows.max_offset_for_scrollbar().y;
        let next = (current + (target_y - match_bounds.top())).clamp(-max_offset, Pixels::ZERO);
        if next != current {
            self.detach_transcript_search_from_tail();
            rows.set_offset_from_scrollbar(point(Pixels::ZERO, next));
            cx.notify();
        }
    }

    pub(super) fn render_transcript_search_bar(
        &self,
        chat_viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let search = self
            .transcript_search
            .as_ref()
            .filter(|search| search.open)?;
        let theme = Theme::current(cx);
        let query_empty = search.query.read(cx).content().is_empty();
        let has_matches = !search.matches.is_empty();
        let count_label: Option<SharedString> = if query_empty {
            None
        } else if !has_matches {
            Some(tr!("find.no_results").into())
        } else {
            let current = search.current.map(|index| index + 1).unwrap_or(0);
            let suffix = if search.limited { "+" } else { "" };
            Some(SharedString::from(tr!(
                "find.result_count",
                current = current,
                total = format!("{}{suffix}", search.matches.len())
            )))
        };
        let bar_width = (chat_viewport_width - 24.0).clamp(260.0, 430.0);
        let input_width = (bar_width - 154.0).max(104.0);
        let query = search.query.clone();
        let previous_focus = self.transcript_control_focus("transcript-find-previous", cx);
        let next_focus = self.transcript_control_focus("transcript-find-next", cx);
        let close_focus = self.transcript_control_focus("transcript-find-close", cx);

        let previous = icon_button("transcript-find-previous", "icons/arrow-up.svg", theme)
            .track_focus(&previous_focus)
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .opacity(if has_matches { 1.0 } else { 0.45 })
            .tooltip(|window, cx| Tooltip::new(tr!("find.previous_match")).build(window, cx))
            .when(has_matches, |button| {
                button.on_activation(cx, |this, _, cx| this.navigate_transcript_search(true, cx))
            });
        let next = icon_button("transcript-find-next", "icons/arrow-down.svg", theme)
            .track_focus(&next_focus)
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .opacity(if has_matches { 1.0 } else { 0.45 })
            .tooltip(|window, cx| Tooltip::new(tr!("find.next_match")).build(window, cx))
            .when(has_matches, |button| {
                button.on_activation(cx, |this, _, cx| this.navigate_transcript_search(false, cx))
            });
        let close = icon_button("transcript-find-close", "icons/x.svg", theme)
            .track_focus(&close_focus)
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .tooltip(|window, cx| Tooltip::new(tr!("find.close")).build(window, cx))
            .on_activation(cx, |this, window, cx| {
                this.close_transcript_search(true, window, cx)
            });

        Some(
            div()
                .id("transcript-search-bar")
                .key_context("FindBar")
                .absolute()
                .top(px(8.0))
                .right(px(12.0))
                .w(px(bar_width))
                .max_w_full()
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.raised)
                .shadow_xs()
                .px(px(6.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                // The transcript canvas takes focus on mouse down so Cmd/Ctrl-F
                // routes here instead of to a stale editor focus. Keep that
                // parent listener from stealing focus back from this field.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    TextField::new("transcript-find-query", query)
                        .w(px(input_width))
                        .flex_none(),
                )
                .child(
                    div()
                        .min_w(px(62.0))
                        .flex_1()
                        .text_size(sp(12.5))
                        .whitespace_nowrap()
                        .text_color(if !query_empty && !has_matches {
                            theme.danger
                        } else {
                            theme.text_tertiary
                        })
                        .children(count_label),
                )
                .child(previous)
                .child(next)
                .child(close)
                .into_any_element(),
        )
    }
}

fn literal_find_regex(query: &str) -> Regex {
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .expect("an escaped literal is always a valid regex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_find_is_case_insensitive_and_unicode_safe() {
        let regex = literal_find_regex("waku");
        let (matches, limited) =
            md::render::markdown_search_matches("Waku **waku** WAKU", &regex, 20);
        assert!(!limited);
        assert_eq!(
            matches
                .iter()
                .map(|found| found.range.clone())
                .collect::<Vec<_>>(),
            vec![0..4, 5..9, 10..14]
        );

        let regex = literal_find_regex("界");
        let (matches, _) = md::render::markdown_search_matches("世**界** 世界", &regex, 20);
        assert_eq!(matches.len(), 2);
        assert!(
            matches
                .iter()
                .all(|found| found.range.start < found.range.end)
        );
    }

    #[test]
    fn markdown_matches_use_the_renderers_element_ordinals() {
        let regex = literal_find_regex("needle");
        let source = "first\n\n> needle\n\n```txt\nneedle\n```";
        let (matches, limited) = md::render::markdown_search_matches(source, &regex, 20);
        assert!(!limited);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].ordinal, 1 << 16);
        assert_eq!(matches[1].ordinal, 2 << 16);
    }

    #[test]
    fn transcript_search_cap_reports_more_results() {
        let regex = literal_find_regex("x");
        let (matches, limited) = md::render::markdown_search_matches("xxxx", &regex, 3);
        assert_eq!(matches.len(), 3);
        assert!(limited);
    }
}

use super::right_panel::{DiffRowStyle, render_diff_code_row};
use super::*;
use base64::Engine as _;

const CHANGED_FILES_PREVIEW_LIMIT: usize = 3;
/// Keep one virtualized transcript row bounded even when a generator touches
/// hundreds of files. The full immutable list remains one click away in the
/// right panel.
const CHANGED_FILES_EXPANDED_LIMIT: usize = 12;
/// An expanded edit stays one transcript row tall; past this the diff scrolls
/// in place, the same as long command output.
const ACTIVITY_DIFF_MAX_HEIGHT: f32 = 400.0;
/// Aligns a hunk separator with the line numbers in the rows below it; see
/// `DiffRowStyle::ACTIVITY`.
const ACTIVITY_DIFF_GUTTER_WIDTH: f32 = 52.0;

#[derive(Clone, Debug)]
struct ConversationNavigationRailSnapshot {
    visible: bool,
    /// Shared with the `Waku` cache: the turns only change when the row-kinds
    /// fingerprint moves, so the per-frame equality check here is a pointer
    /// comparison rather than a walk over every turn's snippets.
    turns: Rc<Vec<TranscriptNavigationTurn>>,
    viewport_height: f32,
    active_turn: Option<Uuid>,
    reset_generation: u64,
    theme_is_dark: bool,
}

impl PartialEq for ConversationNavigationRailSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.visible == other.visible
            && Rc::ptr_eq(&self.turns, &other.turns)
            && self.viewport_height == other.viewport_height
            && self.active_turn == other.active_turn
            && self.reset_generation == other.reset_generation
            && self.theme_is_dark == other.theme_is_dark
    }
}

impl Default for ConversationNavigationRailSnapshot {
    fn default() -> Self {
        Self {
            visible: false,
            turns: Rc::new(Vec::new()),
            viewport_height: 0.0,
            active_turn: None,
            reset_generation: 0,
            theme_is_dark: true,
        }
    }
}

pub(super) struct ConversationNavigationRail {
    waku: Option<WeakEntity<Waku>>,
    snapshot: ConversationNavigationRailSnapshot,
    turn_list_state: ListState,
    turn_indexes: HashMap<Uuid, usize>,
    hovered_turn: Option<Uuid>,
    focused_turn: Option<Uuid>,
    focus_handles: HashMap<Uuid, FocusHandle>,
    visual_state: NavigationRailVisualState,
    transition_from: NavigationRailVisualState,
    animation_generation: u64,
}

impl ConversationNavigationRail {
    pub(super) fn new() -> Self {
        let turn_list_state = ListState::new(0, ListAlignment::Top, px(48.0))
            .with_uniform_item_height(px(NAVIGATION_RAIL_TURN_HEIGHT));
        turn_list_state.set_scroll_handler(|_, window, _| window.refresh());
        Self {
            waku: None,
            snapshot: ConversationNavigationRailSnapshot::default(),
            turn_list_state,
            turn_indexes: HashMap::new(),
            hovered_turn: None,
            focused_turn: None,
            focus_handles: HashMap::new(),
            visual_state: NavigationRailVisualState::default(),
            transition_from: NavigationRailVisualState::default(),
            animation_generation: 0,
        }
    }

    pub(super) fn set_waku(&mut self, waku: WeakEntity<Waku>) {
        self.waku = Some(waku);
    }

    fn set_snapshot(
        &mut self,
        snapshot: ConversationNavigationRailSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.snapshot == snapshot {
            return;
        }
        let reset = self.snapshot.reset_generation != snapshot.reset_generation;
        let turn_identity_changed = self.snapshot.turns.len() != snapshot.turns.len()
            || self
                .snapshot
                .turns
                .iter()
                .zip(snapshot.turns.iter())
                .any(|(previous, next)| previous.message_id != next.message_id);
        let active_turn_changed = self.snapshot.active_turn != snapshot.active_turn;
        if reset {
            self.hovered_turn = None;
            self.focused_turn = None;
            self.focus_handles.clear();
            self.visual_state = NavigationRailVisualState::default();
            self.transition_from = NavigationRailVisualState::default();
            self.animation_generation = self.animation_generation.wrapping_add(1);
        } else if turn_identity_changed {
            self.focus_handles.retain(|message_id, _| {
                snapshot
                    .turns
                    .iter()
                    .any(|turn| turn.message_id == *message_id)
            });
            if self
                .focused_turn
                .is_some_and(|message_id| !self.focus_handles.contains_key(&message_id))
            {
                self.focused_turn = None;
            }
        }
        if reset || turn_identity_changed {
            self.turn_list_state
                .reset_with_uniform_height(snapshot.turns.len(), px(NAVIGATION_RAIL_TURN_HEIGHT));
            self.turn_indexes.clear();
            self.turn_indexes.extend(
                snapshot
                    .turns
                    .iter()
                    .enumerate()
                    .map(|(index, turn)| (turn.message_id, index)),
            );
        }
        self.snapshot = snapshot;
        if (reset || turn_identity_changed || active_turn_changed)
            && let Some(active_index) = self
                .snapshot
                .active_turn
                .and_then(|message_id| self.turn_indexes.get(&message_id).copied())
        {
            self.turn_list_state.scroll_to_reveal_item(active_index);
        }
        cx.notify();
    }
}

impl Waku {
    // ── Transcript ─────────────────────────────────────────────────────────

    pub(super) fn transcript_control_focus(
        &self,
        key: impl Into<String>,
        cx: &mut App,
    ) -> FocusHandle {
        self.transcript_control_focuses
            .borrow_mut()
            .entry(key.into())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    pub(super) fn render_transcript(
        &mut self,
        window: &mut Window,
        chat_viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.prefetch_checkpoint_refs(cx);
        self.sync_transcript_rows();
        self.sync_transcript_layout_width(window);
        self.apply_pending_transcript_search_reveal(window, cx);
        let search_bar = self.render_transcript_search_bar(chat_viewport_width, cx);
        let transcript_rows = self.active_transcript_rows().clone();
        // A scrollbar drag owns the position for as long as it lasts, and the
        // bar writes offsets straight into the list rather than through its
        // scroll handler, so nothing else reports one. Release following as the
        // thumb is taken — otherwise the pins below haul the view back on every
        // frame of the drag — and hand back the same question a wheel scroll
        // asks when the thumb is let go: did the reader come to rest on the
        // tail?
        let scrollbar_dragging = self.transcript_scrollbar.is_grabbed();
        if self
            .transcript_scrollbar_dragging
            .replace(scrollbar_dragging)
            != scrollbar_dragging
        {
            if scrollbar_dragging {
                self.transcript_anchor_following.set(false);
                self.transcript_is_scrolled.set(true);
                self.transcript_tail_recheck.set(false);
            } else {
                self.transcript_tail_recheck.set(true);
            }
        }
        let anchor_end_space = self.update_transcript_anchor_end_space(window);
        if self.transcript_anchor_following.get()
            && anchor_end_space <= Pixels::ZERO
            && self
                .selected_transcript_anchor_row()
                .is_some_and(|anchor_row| anchor_row + 1 < transcript_rows.item_count())
        {
            transcript_rows.scroll_to(ListOffset {
                item_ix: transcript_rows.item_count(),
                offset_in_item: Pixels::ZERO,
            });
            self.transcript_is_scrolled.set(false);
        }
        let entity = cx.entity().downgrade();
        let scrollbar_handle = transcript_rows.clone();
        let viewport_bounds = transcript_rows.viewport_bounds();
        let transcript_scrollable = viewport_bounds.size.height > Pixels::ZERO
            && transcript_rows.max_offset_for_scrollbar().y > px(0.5);
        let viewport_bottom = viewport_bounds.bottom();
        let tail_bottom = transcript_rows
            .item_count()
            .checked_sub(1)
            .and_then(|last_row| transcript_rows.bounds_for_item(last_row))
            .map(|bounds| bounds.bottom());
        // Scrolling back down onto the tail by hand re-engages following, just
        // as the affordance below does. GPUI re-engages its own tail pin when a
        // bottom-aligned list reaches the end, but a turn renders through the
        // top-aligned anchored list and the pin there is ours, so without this
        // the reader is stranded mid-stream after a round trip up and back —
        // watching the reply grow past the bottom edge with no way but the
        // button to rejoin it.
        if self.transcript_tail_recheck.get()
            && let Some(rests_at_tail) =
                transcript_rests_at_tail(viewport_bottom, tail_bottom, anchor_end_space)
        {
            self.transcript_tail_recheck.set(false);
            if rests_at_tail {
                self.pin_transcript_to_tail();
            }
        }
        let scroll_to_bottom_visible = should_show_scroll_to_bottom(
            self.transcript_is_scrolled.get(),
            self.transcript_anchor_following.get(),
            transcript_scrollable,
            viewport_bottom,
            tail_bottom,
            anchor_end_space,
        )
        .unwrap_or_else(|| self.transcript_scroll_to_bottom_visible.get());
        self.transcript_scroll_to_bottom_visible
            .set(scroll_to_bottom_visible);
        let scroll_to_bottom = scroll_to_bottom_visible.then(|| {
            let theme = Theme::current(cx);
            let focus = self.transcript_control_focus("transcript-scroll-to-bottom", cx);
            div()
                .id("transcript-scroll-to-bottom-layer")
                .absolute()
                .left_0()
                .bottom(px(8.0))
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("transcript-scroll-to-bottom")
                        .track_focus(&focus)
                        .tab_index(0)
                        .size(px(32.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.composer)
                        .shadow_xs()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_default()
                        .focus_visible(|style| style.border_color(theme.accent))
                        .hover(|style| style.bg(theme.raised))
                        .active(|style| style.bg(theme.overlay_strong))
                        .child(icon("icons/arrow-down.svg", 16.0, theme.text))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.scroll_transcript_to_bottom(cx);
                            cx.stop_propagation();
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.scroll_transcript_to_bottom(cx);
                                cx.stop_propagation();
                            }
                        })),
                )
        });
        const NAVIGATION_RAIL_ENABLED: bool = true;
        let navigation_rail = NAVIGATION_RAIL_ENABLED.then(|| {
            let viewport_size = viewport_bounds.size;
            let navigation_turns = self.navigation_turns();
            let navigation_rail_visible = should_show_navigation_rail(
                transcript_scrollable,
                navigation_turns.len(),
                chat_viewport_width,
            );
            let scroll_top_row = transcript_rows.logical_scroll_top().item_ix;
            let turn_rows = navigation_turns
                .iter()
                .map(|turn| turn.row_index)
                .collect::<Vec<_>>();
            let active_turn = active_navigation_turn_index(
                &turn_rows,
                scroll_top_row,
                !self.transcript_is_scrolled.get(),
            )
            .map(|index| navigation_turns[index].message_id);
            let navigation_rail_snapshot = ConversationNavigationRailSnapshot {
                visible: navigation_rail_visible,
                turns: navigation_turns,
                viewport_height: f32::from(viewport_size.height),
                active_turn,
                reset_generation: self.navigation_rail_reset_generation.get(),
                theme_is_dark: Theme::current(cx).is_dark,
            };
            if self.navigation_rail.read(cx).snapshot != navigation_rail_snapshot {
                self.navigation_rail.update(cx, |rail, cx| {
                    rail.set_snapshot(navigation_rail_snapshot, cx)
                });
            }
            self.navigation_rail.clone().cached(
                StyleRefinement::default()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
        });
        let transcript_focus = self.transcript_focus.clone();
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .key_context("Transcript")
            .track_focus(&transcript_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.transcript_focus, cx);
                }),
            )
            // Painted before any row, so the frame's selection registry holds
            // exactly the text elements this frame put on screen, in order.
            .child(md::render::frame_reset(self.transcript_selection.clone()))
            .child(
                list(transcript_rows, move |index, window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| {
                            entity.update(cx, |this, cx| this.transcript_row(index, window, cx))
                        })
                        .unwrap_or_else(|| div().into_any_element())
                })
                .size_full()
                .pb(anchor_end_space),
            )
            .children(navigation_rail)
            .children(scroll_to_bottom)
            .child(scrollbar::vertical(
                &scrollbar_handle,
                &self.transcript_scrollbar,
            ))
            .child(self.transcript_selection_input())
            .children(search_bar)
            .into_any_element()
    }

    fn scroll_transcript_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.sync_transcript_rows();
        self.pin_transcript_to_tail();
        cx.notify();
    }

    /// Re-engage tail following from wherever the transcript sits now.
    ///
    /// What actually survives a growing reply is GPUI's past-the-end anchor,
    /// not the follow flag: `scroll_to_end` parks the list on `item_count`, and
    /// the layout walks backwards from there, so the last row's bottom stays
    /// against the viewport as that row grows. The flag alone only re-pins in
    /// the phase where the anchor's end space has already collapsed to zero.
    fn pin_transcript_to_tail(&self) {
        self.transcript_anchor_following
            .set(self.transcript_anchor.get().is_some());
        self.active_transcript_rows().scroll_to_end();
        self.transcript_is_scrolled.set(false);
    }

    /// Copy the transcript's text selection.
    ///
    /// This is the fallback leg of the copy shortcut: the composer holds focus almost
    /// always, so it handles the keystroke first and propagates when it has
    /// nothing selected of its own.
    pub(super) fn copy_selection_action(
        &mut self,
        _: &CopySelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reviewing_diff = self.right_panel_visible
            && self
                .right_panel_active_surface
                .and_then(|index| self.right_panel_surfaces.get(index))
                .is_some_and(|surface| matches!(surface, RightPanelSurface::Diff));
        let reviewing_background_work = self.right_panel_visible
            && self
                .right_panel_active_surface
                .and_then(|index| self.right_panel_surfaces.get(index))
                .is_some_and(|surface| matches!(surface, RightPanelSurface::BackgroundWork { .. }));
        let selected = reviewing_diff
            .then(|| {
                self.right_panel_diff_selection
                    .selection
                    .borrow()
                    .selected_text()
            })
            .flatten()
            .or_else(|| {
                reviewing_background_work
                    .then(|| {
                        self.state
                            .selected_session
                            .and_then(|session_id| self.background_work.get(&session_id))
                            .and_then(BackgroundWorkRegistry::selected_text)
                    })
                    .flatten()
            })
            .or_else(|| self.toast_selection.selection.borrow().selected_text())
            .or_else(|| self.skills_selection.selection.borrow().selected_text())
            .or_else(|| self.transcript_selection.selection.borrow().selected_text());
        match selected {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            None => cx.propagate(),
        }
    }

    /// A zero-size canvas that installs the frame's selection mouse listeners.
    /// One set for the whole transcript: the registry already knows every
    /// painted element's geometry, so per-element listeners would be redundant.
    fn transcript_selection_input(&self) -> impl IntoElement {
        let selection = self.transcript_selection.clone();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| md::render::install_selection_input(window, &selection),
        )
        .absolute()
        .w(px(0.0))
        .h(px(0.0))
    }

    pub(super) fn toast_selection_input(&self) -> impl IntoElement {
        let selection = self.toast_selection.clone();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| md::render::install_selection_input(window, &selection),
        )
        .absolute()
        .w(px(0.0))
        .h(px(0.0))
    }
}

impl Render for ConversationNavigationRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.snapshot.visible {
            return div().into_any_element();
        }
        let theme = Theme::current(cx);
        let turns = self.snapshot.turns.clone();
        let turn_count = turns.len();
        let viewport_height = self.snapshot.viewport_height;
        if turn_count == 0 {
            return div().into_any_element();
        }
        let rail_height = navigation_rail_height(turn_count, viewport_height);
        if rail_height <= 0.0 {
            return div().into_any_element();
        }
        let rail_top = (viewport_height - rail_height).max(0.0) / 2.0;
        // The rail keeps a true one-to-one scroll position for every turn. Its
        // `ListState` only asks the builder for visible ticks plus overdraw, so
        // hover and scroll work remain bounded by the viewport even for a very
        // long conversation.
        let emphasized_turn = self.hovered_turn.or_else(|| {
            window
                .last_input_was_keyboard()
                .then_some(self.focused_turn)
                .flatten()
        });
        let emphasized_turn_index =
            emphasized_turn.and_then(|message_id| self.turn_indexes.get(&message_id).copied());
        let active_turn_index = self
            .snapshot
            .active_turn
            .and_then(|message_id| self.turn_indexes.get(&message_id).copied());
        let visual_state = NavigationRailVisualState { emphasized_turn };
        let previous_visual_state = self.visual_state;
        if previous_visual_state != visual_state {
            self.transition_from = previous_visual_state;
            self.visual_state = visual_state;
            self.animation_generation = self.animation_generation.wrapping_add(1);
        }
        let transition_from = self.transition_from;
        let from_emphasized_turn_index = transition_from
            .emphasized_turn
            .and_then(|message_id| self.turn_indexes.get(&message_id).copied());
        let animation_generation = self.animation_generation;
        let entity = cx.entity().downgrade();
        let turn_list_state = self.turn_list_state.clone();
        let tick_list = list(turn_list_state.clone(), move |turn_index, window, cx| {
            entity
                .upgrade()
                .map(|entity| {
                    entity.update(cx, |this, cx| {
                        this.render_navigation_rail_tick(
                            turn_index,
                            from_emphasized_turn_index,
                            emphasized_turn_index,
                            active_turn_index,
                            animation_generation,
                            window,
                            cx,
                        )
                    })
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .size_full();

        let (show_top_fade, show_bottom_fade) = navigation_rail_fade_visibility(
            self.turn_list_state.scroll_px_offset_for_scrollbar().y,
            self.turn_list_state.max_offset_for_scrollbar().y,
        );
        let transparent_surface = theme.surface.opacity(0.0);

        let rail = div()
            .id("conversation-navigation-rail")
            .absolute()
            .left(px(NAVIGATION_RAIL_LEFT))
            .top(px(rail_top))
            .w(px(NAVIGATION_RAIL_WIDTH))
            .h(px(rail_height))
            .relative()
            .overflow_hidden()
            .tab_index(0)
            .tab_group()
            .tab_stop(false)
            .child(tick_list)
            .when(show_top_fade, |rail| {
                rail.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w_full()
                        .h(px(NAVIGATION_RAIL_FADE_HEIGHT))
                        .bg(linear_gradient(
                            180.0,
                            linear_color_stop(theme.surface, 0.0),
                            linear_color_stop(transparent_surface, 1.0),
                        )),
                )
            })
            .when(show_bottom_fade, |rail| {
                rail.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(NAVIGATION_RAIL_FADE_HEIGHT))
                        .bg(linear_gradient(
                            180.0,
                            linear_color_stop(transparent_surface, 0.0),
                            linear_color_stop(theme.surface, 1.0),
                        )),
                )
            });

        let preview = emphasized_turn_index.map(|turn_index| {
            let turn = &turns[turn_index];
            let scroll_top = -self.turn_list_state.scroll_px_offset_for_scrollbar().y;
            let preview_height = 126.0;
            let max_preview_top = (viewport_height - preview_height - 12.0).max(12.0);
            let preview_top = (rail_top + turn_index as f32 * NAVIGATION_RAIL_TURN_HEIGHT
                - f32::from(scroll_top)
                + NAVIGATION_RAIL_TURN_HEIGHT / 2.0
                - preview_height / 2.0)
                .clamp(12.0, max_preview_top);
            div()
                .absolute()
                .left(px(NAVIGATION_RAIL_LEFT
                    + NAVIGATION_RAIL_WIDTH
                    + NAVIGATION_RAIL_CONTENT_GAP))
                .top(px(preview_top))
                .w(px(320.0))
                .max_h(px(preview_height))
                .overflow_hidden()
                .rounded(px(14.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.raised)
                .shadow_lg()
                .px(px(15.0))
                .py(px(12.0))
                .flex()
                .flex_col()
                .gap(px(7.0))
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(sp(14.0))
                        .line_height(sp(20.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from(turn.prompt.clone())),
                )
                .when(!turn.response.is_empty(), |preview| {
                    preview.child(
                        div()
                            .w_full()
                            .max_h(px(60.0))
                            .overflow_hidden()
                            .whitespace_normal()
                            .text_size(sp(13.0))
                            .line_height(sp(20.0))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(turn.response.clone())),
                    )
                })
        });

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(rail)
            .children(preview)
            .into_any_element()
    }
}

impl ConversationNavigationRail {
    #[allow(clippy::too_many_arguments)]
    fn render_navigation_rail_tick(
        &mut self,
        turn_index: usize,
        from_emphasized_turn_index: Option<usize>,
        emphasized_turn_index: Option<usize>,
        active_turn_index: Option<usize>,
        animation_generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(turn) = self.snapshot.turns.get(turn_index) else {
            return div().into_any_element();
        };
        let message_id = turn.message_id;
        let theme = Theme::current(cx);
        let focus_handle = self.navigation_rail_focus_handle(message_id, window, cx);
        let from_width = NAVIGATION_RAIL_TICK_WIDTH
            * navigation_rail_scale(turn_index, from_emphasized_turn_index);
        let to_width =
            NAVIGATION_RAIL_TICK_WIDTH * navigation_rail_scale(turn_index, emphasized_turn_index);
        let prominent =
            active_turn_index == Some(turn_index) || emphasized_turn_index == Some(turn_index);
        let tick_color = if prominent {
            if theme.is_dark {
                rgb(0xFFFFFF).into()
            } else {
                theme.text
            }
        } else {
            theme.text_ghost.opacity(NAVIGATION_RAIL_INACTIVE_OPACITY)
        };
        let click_focus = focus_handle.clone();
        let animation_id = SharedString::from(format!(
            "conversation-navigation-tick-animation-{message_id}-{animation_generation}"
        ));
        let tick = div()
            .h(px(NAVIGATION_RAIL_TICK_HEIGHT))
            .rounded_full()
            .bg(tick_color)
            .with_animation(
                animation_id,
                Animation::new(NAVIGATION_RAIL_ANIMATION_DURATION).with_easing(ease_out_quint()),
                move |element, delta| element.w(px(from_width + (to_width - from_width) * delta)),
            );

        div()
            .id(SharedString::from(format!(
                "conversation-navigation-turn-hit-{message_id}"
            )))
            .w(px(NAVIGATION_RAIL_WIDTH))
            .h(px(NAVIGATION_RAIL_TURN_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .cursor_default()
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if *hovering {
                    this.hovered_turn = Some(message_id);
                } else if this.hovered_turn == Some(message_id) {
                    this.hovered_turn = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                click_focus.focus(window, cx);
                this.activate_turn(message_id, cx);
            }))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "conversation-navigation-turn-focus-{message_id}"
                    )))
                    .w(px(NAVIGATION_RAIL_TICK_WIDTH + 4.0))
                    .h(px(8.0))
                    .ml(px(-2.0))
                    .pl(px(2.0))
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .track_focus(&focus_handle)
                    .tab_index(turn_index as isize)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_key_down(cx.listener(move |this, event, window, cx| {
                        this.navigation_rail_key_down(message_id, event, window, cx);
                    }))
                    .child(tick),
            )
            .into_any_element()
    }

    fn navigation_rail_focus_handle(
        &mut self,
        message_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        if let Some(focus_handle) = self.focus_handles.get(&message_id).cloned() {
            return focus_handle;
        }

        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, move |this: &mut Self, _, cx| {
            this.focused_turn = Some(message_id);
            cx.notify();
        })
        .detach();
        cx.on_blur(&focus_handle, window, move |this: &mut Self, _, cx| {
            if this.focused_turn == Some(message_id) {
                this.focused_turn = None;
            }
            cx.notify();
        })
        .detach();
        self.focus_handles.insert(message_id, focus_handle.clone());
        focus_handle
    }

    fn navigation_rail_key_down(
        &mut self,
        message_id: Uuid,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let turns = &self.snapshot.turns;
        let turn_count = turns.len();
        if turn_count == 0 {
            return;
        }
        let Some(turn_index) = self.turn_indexes.get(&message_id).copied() else {
            return;
        };

        let target_turn = match event.keystroke.key.as_str() {
            "up" => Some(turn_index.saturating_sub(1)),
            "down" => Some((turn_index + 1).min(turn_count - 1)),
            "home" => Some(0),
            "end" => Some(turn_count - 1),
            "enter" | "space" => {
                self.activate_turn(message_id, cx);
                cx.stop_propagation();
                return;
            }
            _ => None,
        };
        let Some(target_turn) = target_turn else {
            return;
        };
        self.turn_list_state.scroll_to_reveal_item(target_turn);
        let target_message_id = turns[target_turn].message_id;
        let focus_handle = self.navigation_rail_focus_handle(target_message_id, window, cx);
        focus_handle.focus(window, cx);
        cx.notify();
        cx.stop_propagation();
    }

    fn activate_turn(&self, message_id: Uuid, cx: &mut Context<Self>) {
        if let Some(waku) = &self.waku {
            let _ = waku.update(cx, |waku, cx| {
                waku.scroll_to_navigation_turn(message_id, cx)
            });
        }
    }
}

impl Waku {
    fn scroll_to_navigation_turn(&mut self, message_id: Uuid, cx: &mut Context<Self>) {
        let row_index = self
            .navigation_turns()
            .iter()
            .find(|turn| turn.message_id == message_id)
            .map(|turn| turn.row_index);
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

    /// Open or close one turn-block disclosure, holding the reader's place.
    ///
    /// Re-measuring the row changes its height, and a bottom-aligned list keeps
    /// its pixel offset across that change — which lands the viewport past the
    /// end of the content and shows nothing until you scroll. Capturing the
    /// logical position before the change and restoring it after keeps the row
    /// exactly where it was.
    fn toggle_block_disclosure(
        &mut self,
        block_index: usize,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Self),
    ) {
        self.pin_transcript_for_disclosure();
        apply(self);
        // `remeasure_transcript_block` preserves the reader's scroll position
        // across the row's height change on its own.
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    pub(super) fn toggle_activities(
        &mut self,
        block_index: usize,
        current: bool,
        cx: &mut Context<Self>,
    ) {
        self.toggle_block_disclosure(block_index, cx, |this| {
            this.activities_expanded.insert(block_index, !current);
        });
    }

    pub(super) fn toggle_activity_item(&mut self, id: Uuid, current: bool, cx: &mut Context<Self>) {
        let block_index = self
            .selected_transcript_blocks()
            .iter()
            .position(|block| block.activities.iter().any(|activity| activity.id == id));
        let Some(block_index) = block_index else {
            return;
        };
        self.toggle_block_disclosure(block_index, cx, |this| {
            this.expanded_activity_items.insert(id, !current);
            if current {
                // Collapsed: the rows would only be rebuilt from the same
                // changes if it reopens, so do not keep them alive for every
                // edit the session ever made.
                this.activity_diffs.borrow_mut().remove(&id);
                this.activity_diff_viewports.borrow_mut().remove(&id);
            }
        });
    }

    /// Opens a file a tool changed in the right panel's viewer.
    ///
    /// Sits inside a row that toggles on click, so it stops the press from
    /// reaching that toggle, and answers the keyboard on its own.
    fn render_activity_open_file_button(
        &self,
        id: String,
        path: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = self.transcript_control_focus(id.clone(), cx);
        let key_path = path.clone();
        div()
            .id(SharedString::from(id))
            .track_focus(&focus)
            .tab_index(0)
            .size(px(20.0))
            .flex_none()
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|button| button.bg(theme.overlay_strong))
            .hover(|button| button.bg(theme.overlay_strong))
            .child(icon(
                "icons/file-bottom-left-arrow.svg",
                14.0,
                theme.text_ghost,
            ))
            .tooltip(Tooltip::text(tr!("activity.open_file")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.open_activity_file(&path, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.open_activity_file(&key_path, cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    /// Diff rows for an expanded file-change activity, built on first sight and
    /// held until the activity collapses or its changes are replaced.
    fn activity_diff_rows(&self, activity: &ActivityItem) -> Rc<activity_diff::Diff> {
        if let Some(diff) = self.activity_diffs.borrow().get(&activity.id) {
            return diff.clone();
        }
        let diff = Rc::new(activity_diff::build(activity));
        self.activity_diffs
            .borrow_mut()
            .insert(activity.id, diff.clone());
        diff
    }

    pub(super) fn toggle_turn_fold(
        &mut self,
        turn_id: Uuid,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        let scroll_top = self.active_transcript_rows().logical_scroll_top();
        let previous_kinds = self.transcript_row_kinds.borrow().clone();
        let anchor_kind = previous_kinds.get(scroll_top.item_ix).copied();
        if expanded {
            self.expanded_turns.remove(&turn_id);
        } else {
            self.expanded_turns.insert(turn_id);
        }
        self.transcript_anchor_following.set(false);
        self.splice_transcript_rows_after_visibility_change(&previous_kinds);

        let next_kinds = self.transcript_row_kinds.borrow();
        let anchored_target =
            anchor_kind.and_then(|kind| next_kinds.iter().position(|candidate| *candidate == kind));
        let target = anchored_target.or_else(|| {
            next_kinds
                .iter()
                .position(|kind| *kind == TranscriptRowKind::TurnFold(turn_id))
        });
        drop(next_kinds);
        if let Some(item_ix) = target {
            self.active_transcript_rows().scroll_to(ListOffset {
                item_ix,
                offset_in_item: if anchored_target.is_some() {
                    scroll_top.offset_in_item
                } else {
                    Pixels::ZERO
                },
            });
            self.transcript_is_scrolled.set(true);
        }
        cx.notify();
    }

    /// A single transcript row, self-centered to the content column so the
    /// list can measure it at its true wrap width. Current-turn reasoning and
    /// activity blocks are anchored at the exact boundary between assistant
    /// text segments where their provider events arrived.
    pub(super) fn user_message_action_for_message(
        &self,
        message_index: usize,
    ) -> Option<UserMessageAction> {
        let session = self.selected_session()?;
        let message = session.messages.get(message_index)?;
        if message.role != MessageRole::User
            || !matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
        {
            return None;
        }
        let turn_id = message.turn_id?;
        let turn = session.turns.iter().find(|turn| turn.id == turn_id)?;
        if !session.provider.supports_conversation_rollback() {
            return None;
        }
        // A steer joins the running turn as another user message. Rewinding
        // restores the turn's checkpoint and resubmits the prompt that opened
        // it, so only that prompt can carry the affordance.
        if !message_opens_turn(&session.messages, message_index) {
            return None;
        }
        let retained_turn_count = turn.turn_count.saturating_sub(1);
        // Cache only — the ref lives in git, and this runs for every visible
        // user message on every frame. `prefetch_checkpoint_refs` fills the
        // cache off-thread and notifies.
        if !self
            .checkpoint_ref_cache
            .borrow()
            .get(&(session.id, retained_turn_count))
            .copied()
            .unwrap_or(false)
        {
            return None;
        }
        let rollback_turns = session.provider_turns_after(retained_turn_count);
        if rollback_turns > 0 && session.provider_cursor.is_none() {
            return None;
        }
        Some(UserMessageAction {
            session_id: session.id,
            message_id: message.id,
            turn_count: turn.turn_count,
        })
    }

    /// Forget cached checkpoint-ref existence after refs changed. The next
    /// transcript frame schedules a fresh background prefetch.
    pub(super) fn invalidate_checkpoint_refs(&self) {
        self.checkpoint_ref_cache.borrow_mut().clear();
        self.checkpoint_ref_generation
            .set(self.checkpoint_ref_generation.get().wrapping_add(1));
    }

    /// Resolve the selected session's checkpoint refs on the background
    /// executor — one `git for-each-ref` per session per invalidation — and
    /// cache which retained turn counts have one. The rewind affordance
    /// appears once the result lands and notifies.
    fn prefetch_checkpoint_refs(&self, cx: &mut Context<Self>) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let generation = self.checkpoint_ref_generation.get();
        if self.checkpoint_ref_prefetch.get() == Some((session.id, generation)) {
            return;
        }
        let Some(project_path) = self
            .workspace_path_for_session(session)
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        let session_id = session.id;
        let retained_turn_counts = session
            .turns
            .iter()
            .map(|turn| turn.turn_count.saturating_sub(1))
            .collect::<Vec<_>>();
        self.checkpoint_ref_prefetch
            .set(Some((session_id, generation)));
        let workspace = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let existing = cx
                .background_executor()
                .spawn(async move {
                    match workspace.request(waku_client::WorkspaceOperation::SessionTurnRefs {
                        cwd: project_path,
                        session_id,
                    }) {
                        Ok(waku_client::WorkspaceResult::TurnRefs { turn_counts }) => {
                            turn_counts.into_iter().collect::<HashSet<_>>()
                        }
                        Ok(_) | Err(_) => HashSet::new(),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.checkpoint_ref_generation.get() != generation {
                    return;
                }
                let mut cache = this.checkpoint_ref_cache.borrow_mut();
                for turn_count in retained_turn_counts {
                    cache.insert((session_id, turn_count), existing.contains(&turn_count));
                }
                drop(cache);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn assistant_message_action_for_message(
        &self,
        message_index: usize,
    ) -> Option<AssistantMessageAction> {
        let session = self.selected_session()?;
        let message = session.messages.get(message_index)?;
        if message.role != MessageRole::Assistant
            || assistant_response_footer_index(session, message_index) != Some(message_index)
            || !matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
            || !session.provider.supports_conversation_fork()
            || session
                .provider_cursor
                .as_ref()
                .is_none_or(|cursor| cursor.provider() != session.provider)
        {
            return None;
        }
        let turn_id = message.turn_id?;
        let turn = session
            .turns
            .iter()
            .find(|turn| turn.id == turn_id && turn.provider_turn_started)?;
        let pending_turn = self.response_fork_preparations.get(&session.id).copied();
        Some(AssistantMessageAction {
            session_id: session.id,
            turn_count: turn.turn_count,
            enabled: pending_turn.is_none(),
            preparing: pending_turn == Some(turn.turn_count),
        })
    }

    /// `metrics` rescaled to the user's UI and code font size settings.
    pub(super) fn scaled_markdown_metrics(&self, metrics: MarkdownMetrics) -> MarkdownMetrics {
        metrics.scaled(self.state.ui_font_size, self.state.code_font_size)
    }

    /// Mono text scale for plain tool-output sections: the code font size
    /// itself, the way every dedicated code surface takes it.
    fn activity_mono_text(&self) -> (f32, f32) {
        let size = self.state.code_font_size;
        (size, (size * 1.5 * 2.0).round() / 2.0)
    }

    /// The markdown render context for one transcript row. Element keys are
    /// scoped to the row, so a virtualized remount recreates the same keys and
    /// an in-progress selection survives scrolling.
    fn markdown_ctx<'a>(
        &self,
        row: String,
        palette: &'a MarkdownPalette,
        metrics: MarkdownMetrics,
        animate_streaming: bool,
    ) -> MarkdownCtx<'a> {
        MarkdownCtx::new(row, palette, metrics, self.transcript_selection.clone())
            .with_link_handler(self.markdown_link_handler.clone())
            .with_streaming_animation(animate_streaming)
    }

    /// The menu handle for `id`, created on first use.
    ///
    /// Every menu holds the composer's *visual* focus while it is open, so
    /// opening one never looks like it defocused the input — the composer owns
    /// real focus almost all the time, and the menu has to take it to see keys.
    pub(super) fn menu_handle(
        &self,
        id: impl Into<SharedString>,
        cx: &mut App,
    ) -> ContextMenuHandle {
        self.menu_handle_with(id, cx, |_, _, _| {})
    }

    /// [`Self::menu_handle`] with an extra toggle observer, run after the
    /// composer's. `extra` is only consulted the first time a given id is seen.
    pub(super) fn menu_handle_with(
        &self,
        id: impl Into<SharedString>,
        cx: &mut App,
        extra: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> ContextMenuHandle {
        let id = id.into();
        if let Some(handle) = self.menus.borrow().get(&id) {
            return handle.clone();
        }
        let composer = self.composer.clone();
        let handle = ContextMenuHandle::new(cx)
            .on_toggle(move |open, window, cx| {
                composer.update(cx, |composer, cx| {
                    if open {
                        composer.preserve_visual_focus_for_context_menu(window, cx);
                    } else {
                        composer.release_visual_focus_for_context_menu(window, cx);
                    }
                });
            })
            .on_toggle(extra);
        self.menus.borrow_mut().insert(id, handle.clone());
        handle
    }

    pub(super) fn transcript_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let palette = MarkdownPalette::from_theme(&theme);
        let composer = self.composer.clone();
        let waku = cx.entity().downgrade();
        // Both from the cache `sync_transcript_rows` refreshed at the top of
        // this frame. Recomputing the row list here would rebuild the whole
        // transcript's row kinds — several allocations proportional to the
        // session — once for every visible row, every frame.
        let (row_count, kind) = {
            let kinds = self.transcript_row_kinds.borrow();
            let kind = kinds
                .get(index)
                .copied()
                .unwrap_or(TranscriptRowKind::Message(index));
            (kinds.len(), kind)
        };
        let starts_followup_turn = match kind {
            TranscriptRowKind::Message(message_index) => {
                self.selected_session().is_some_and(|session| {
                    message_starts_followup_turn(&session.messages, message_index)
                })
            }
            TranscriptRowKind::TurnBlock(_)
            | TranscriptRowKind::TurnFold(_)
            | TranscriptRowKind::ChangedFiles(_)
            | TranscriptRowKind::WorkingIndicator => false,
        };
        let inner = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| session.messages.get(message_index))
                .cloned()
                .map(|message| {
                    let copied = self.copied_message_feedback.contains_key(&message.id);
                    let (assistant_footer_copy_content, assistant_footer_time) =
                        self.assistant_response_footer_cached(message_index);
                    let assistant_before_footer = assistant_footer_copy_content
                        .as_ref()
                        .and(message.turn_id)
                        .filter(|_| !message.content.trim().is_empty())
                        .and_then(|turn_id| self.render_changed_files_row(turn_id, &theme, cx));
                    let assistant_message_action =
                        self.assistant_message_action_for_message(message_index);
                    let user_message_action = self.user_message_action_for_message(message_index);
                    let message_edit_input = user_message_action.and_then(|action| {
                        self.message_edit
                            .as_ref()
                            .filter(|edit| {
                                edit.session_id == action.session_id
                                    && edit.message_id == action.message_id
                            })
                            .map(|edit| edit.input.clone())
                    });
                    let attachment_menus = (0..message.attachments.len())
                        .map(|index| {
                            self.menu_handle(
                                format!("message-{}-attachment-{index}", message.id),
                                cx,
                            )
                        })
                        .collect();
                    let attachment_images = message
                        .attachments
                        .iter()
                        .map(|attachment| {
                            if !attachment.is_image {
                                return None;
                            }
                            let Some(reference) = attachment.blob_reference.as_deref() else {
                                return None;
                            };
                            self.image_for_reference(
                                reference,
                                Some(&attachment.path),
                                Some(&attachment.name),
                                cx,
                            )
                        })
                        .collect();
                    let attachments_can_reveal = !self.daemon.is_remote();
                    let menu = self.menu_handle(format!("message-{}", message.id), cx);
                    let metrics = self.scaled_markdown_metrics(if message.role == MessageRole::User
                    {
                        MarkdownMetrics::USER_MESSAGE
                    } else {
                        MarkdownMetrics::BODY
                    });
                    let animate_streaming = message.streaming && !cx.reduce_motion();
                    let mut ctx = self.markdown_ctx(
                        format!("message-{}", message.id),
                        &palette,
                        metrics,
                        animate_streaming,
                    );
                    if let Some(highlights) = self.transcript_search_highlights(message_index) {
                        ctx = ctx.with_search_highlights(highlights);
                    }
                    // Human and assistant messages share the Markdown path.
                    // Parse only visible rows rather than doing work for every
                    // driver delta or every off-screen prompt.
                    let mut markdown = self.message_markdown.borrow_mut();
                    let view = matches!(message.role, MessageRole::User | MessageRole::Assistant)
                        .then(|| {
                            let view = markdown.entry(message.id).or_default();
                            view.set_text(message.visible_content(), message.streaming);
                            &*view
                        });
                    let rendered = render_message(
                        MessageRender {
                            theme: &theme,
                            message: &message,
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
                            markdown: view,
                            ctx: &ctx,
                            menu,
                            waku,
                            composer,
                        },
                        cx,
                    );
                    if animate_streaming && view.is_some_and(MarkdownView::is_fading) {
                        // Advance the dissolve from the shared pulse clock,
                        // not `request_animation_frame`: chunks land every
                        // stream commit, so a fade is active for essentially
                        // the whole response and a display-rate re-arm held
                        // the window at 120 Hz — and every one of those
                        // frames rebuilds each visible row. ~30 fps across a
                        // 120-400 ms dissolve is visually equivalent at a
                        // quarter of the redraws, the same trade the loaders
                        // make, and the lease parks once the last chunk
                        // settles. Leasing `current_view` (the transcript
                        // pane) keeps the tick from busting sibling islands.
                        motion::pulse_lease(window.current_view(), cx);
                    }
                    rendered
                })
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| {
                    self.render_activities_row(&block.activities, block_index, &theme, window, cx)
                })
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnFold(turn_id) => self.render_turn_fold_row(turn_id, &theme, cx),
            TranscriptRowKind::ChangedFiles(turn_id) => self
                .render_changed_files_row(turn_id, &theme, cx)
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::WorkingIndicator => self.render_working_indicator_row(&theme),
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(20.0))
            .py(px(8.0))
            .when(index == 0, |element| element.pt(px(22.0)))
            .when(starts_followup_turn, |element| {
                element.pt(px(FOLLOWUP_TURN_TOP_GAP))
            })
            .when(index + 1 == row_count, |element| element.pb(px(22.0)))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .min_w_0()
                    .child(inner),
            )
            .into_any_element()
    }

    pub(super) fn toggle_changed_files(
        &mut self,
        turn_id: Uuid,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        if expanded {
            self.expanded_changed_files.remove(&turn_id);
        } else {
            self.expanded_changed_files.insert(turn_id);
        }
        self.remeasure_changed_files(turn_id);
        cx.notify();
    }

    /// The immutable file delta captured when a response settles. Small
    /// summaries stay useful at a glance; larger ones disclose in place and
    /// always offer the complete per-turn list in the right panel.
    fn render_changed_files_row(
        &self,
        turn_id: Uuid,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(checkpoint) = self
            .selected_session()
            .and_then(|session| session.turns.iter().find(|turn| turn.id == turn_id))
            .and_then(|turn| turn.checkpoint.as_ref())
            .filter(|checkpoint| checkpoint.status == CheckpointStatus::Ready)
            .filter(|checkpoint| !checkpoint.files.is_empty())
        else {
            return None;
        };

        let files = checkpoint.files.as_slice();
        let additions = checkpoint.additions;
        let deletions = checkpoint.deletions;
        let expanded = self.expanded_changed_files.contains(&turn_id);
        let visible_limit = if expanded {
            CHANGED_FILES_EXPANDED_LIMIT
        } else {
            CHANGED_FILES_PREVIEW_LIMIT
        };
        let visible_count = files.len().min(visible_limit);
        let can_expand = files.len() > CHANGED_FILES_PREVIEW_LIMIT;
        let clipped = expanded && files.len() > CHANGED_FILES_EXPANDED_LIMIT;

        let review_focus =
            self.transcript_control_focus(format!("changed-files-review-{turn_id}"), cx);
        let review = div()
            .id(SharedString::from(format!(
                "changed-files-review-{turn_id}"
            )))
            .track_focus(&review_focus)
            .tab_index(0)
            .h(px(28.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(sp(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay_strong).text_color(theme.text))
            .active(|style| style.bg(theme.overlay))
            .child(icon("icons/file-diff.svg", 12.0, theme.text_tertiary))
            .child(tr_cow!("transcript.review_changes"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_turn_diff(turn_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.open_turn_diff(turn_id, cx);
                    cx.stop_propagation();
                }
            }));

        let title = if files.len() == 1 {
            tr!("transcript.changed_file", count = files.len())
        } else {
            tr!("transcript.changed_files", count = files.len())
        };
        let mut card = div()
            .id(SharedString::from(format!("changed-files-card-{turn_id}")))
            .w_full()
            .min_w_0()
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.overlay)
            .tab_index(0)
            .tab_group()
            .tab_stop(false)
            .overflow_hidden()
            .child(
                div()
                    .min_h(px(58.0))
                    .px(px(12.0))
                    .py(px(9.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .size(px(36.0))
                            .flex_none()
                            .rounded(px(9.0))
                            .bg(theme.overlay_strong)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("icons/file-diff.svg", 16.0, theme.text_tertiary)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(sp(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_size(sp(12.5))
                                    .line_height(sp(14.0))
                                    .child(
                                        div()
                                            .text_color(theme.success)
                                            .child(format!("+{additions}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.danger)
                                            .child(format!("-{deletions}")),
                                    ),
                            ),
                    )
                    .child(review),
            );

        let mut file_rows = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border);
        for file in files.iter().take(visible_count) {
            file_rows = file_rows.child(
                div()
                    .h(px(31.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "changed-file-path-{turn_id}-{}",
                                file.path
                            )))
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(sp(12.5))
                            .text_color(theme.text_secondary)
                            .tooltip(Tooltip::text(file.path.clone()))
                            .child(file.path.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(sp(12.5))
                            .text_color(theme.success)
                            .child(format!("+{}", file.additions)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(sp(12.5))
                            .text_color(theme.danger)
                            .child(format!("-{}", file.deletions)),
                    ),
            );
        }
        card = card.child(file_rows);

        if can_expand {
            let toggle_focus =
                self.transcript_control_focus(format!("changed-files-toggle-{turn_id}"), cx);
            let label = if expanded {
                tr!("transcript.show_fewer_files")
            } else {
                tr!(
                    "transcript.show_more_files",
                    count = files.len() - CHANGED_FILES_PREVIEW_LIMIT
                )
            };
            card = card.child(
                div()
                    .id(SharedString::from(format!(
                        "changed-files-toggle-{turn_id}"
                    )))
                    .track_focus(&toggle_focus)
                    .tab_index(0)
                    .h(px(34.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_default()
                    .text_size(sp(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_secondary)
                    .focus_visible(|style| style.bg(theme.overlay_strong))
                    .hover(|style| style.bg(theme.overlay_strong).text_color(theme.text))
                    .active(|style| style.bg(theme.overlay))
                    .child(SharedString::from(label))
                    .when(clipped, |row| {
                        row.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme.text_ghost)
                                .child(tr!(
                                    "transcript.showing_first_files",
                                    count = CHANGED_FILES_EXPANDED_LIMIT,
                                    total = files.len()
                                )),
                        )
                    })
                    .child(div().flex_1())
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        11.0,
                        theme.text_tertiary,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_changed_files(turn_id, expanded, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.toggle_changed_files(turn_id, expanded, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }

        Some(card.into_any_element())
    }

    /// Settled reasoning, tool activity, and interim assistant commentary are
    /// folded into a compact divider while the terminal response stays visible.
    pub(super) fn render_turn_fold_row(
        &self,
        turn_id: Uuid,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_turns.contains(&turn_id);
        let label = self
            .selected_session()
            .map(|session| turn_fold_label(session, turn_id))
            .unwrap_or_else(|| tr!("transcript.worked"));
        div()
            .w_full()
            .h(px(24.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(div().h(px(1.0)).flex_1().bg(theme.border))
            .child(
                div()
                    .id(SharedString::from(format!("turn-fold-{turn_id}")))
                    .h(px(24.0))
                    .px(px(2.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_default()
                    .text_size(sp(13.5))
                    .line_height(sp(18.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(label))
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        11.5,
                        theme.text_tertiary,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_turn_fold(turn_id, expanded, cx);
                    })),
            )
            .child(div().h(px(1.0)).flex_1().bg(theme.border))
            .into_any_element()
    }

    /// The live turn's closing row: pulsing dots and "Working for Ns". It is
    /// on screen from the moment the prompt lands — before the provider has
    /// produced a single chunk — and stays below whatever streams in until
    /// the turn settles into its "Worked for N" fold.
    fn render_working_indicator_row(&self, theme: &Theme) -> AnyElement {
        let elapsed = self
            .selected_session()
            .and_then(|session| session.turns.last())
            .filter(|turn| turn.status == TurnStatus::Running)
            .map(|turn| unix_time().saturating_sub(turn.started_at))
            .unwrap_or(0);
        div()
            .h(px(22.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(working_wave_dots(theme.text_tertiary))
            .child(
                div()
                    .text_size(sp(13.5))
                    .line_height(sp(18.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(tr!(
                        "transcript.working_for",
                        duration = format_working_elapsed(elapsed)
                    ))),
            )
            .into_any_element()
    }

    /// The turn's tool activity as a disclosure: the summary line toggles the
    /// row list, and each row with detail expands to its full content.
    fn show_activity_section_copied(
        &mut self,
        activity_id: Uuid,
        section_kind: ActivityDisclosureSectionKind,
        cx: &mut Context<Self>,
    ) {
        self.copied_activity_generation = self.copied_activity_generation.wrapping_add(1);
        let generation = self.copied_activity_generation;
        let key = (activity_id, section_kind);
        self.copied_activity_feedback.insert(key, generation);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_activity_feedback.get(&key) == Some(&generation) {
                    this.copied_activity_feedback.remove(&key);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn render_activities_row(
        &self,
        activities: &[ActivityItem],
        block_index: usize,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A completed child is not a group boundary: providers commonly emit
        // the next tool after the previous result. The group leaves the live
        // tail only when answer text is appended (so `after_message` falls
        // behind) or when the turn itself settles.
        let live_group = self.selected_session().is_some_and(|session| {
            session
                .transcript_blocks
                .get(block_index)
                .is_some_and(|block| {
                    activity_group_is_live(
                        session
                            .active_turn_id()
                            .is_some_and(|turn_id| block.turn_id == Some(turn_id)),
                        block_index + 1 == session.transcript_blocks.len(),
                        block.after_message,
                        session.messages.len(),
                    )
                })
        });
        let expanded = self
            .activities_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(live_group);
        let live_reasoning_id = (self
            .selected_runtime()
            .is_some_and(|runtime| runtime.stream_phase == Some(StreamPhase::Reasoning))
            && self
                .selected_session()
                .is_some_and(|session| session.status == SessionStatus::Working)
            && block_index + 1 == self.selected_transcript_blocks().len())
        .then(|| {
            activities
                .iter()
                .rev()
                .find(|activity| activity.reasoning.is_some())
                .map(|activity| activity.id)
        })
        .flatten();
        let header_title = activity_header_title(activities, live_group, live_reasoning_id);
        let header_focus =
            self.transcript_control_focus(format!("activity-toggle-{block_index}"), cx);
        let cluster = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .id(SharedString::from(format!("activity-toggle-{block_index}")))
                    .track_focus(&header_focus)
                    .tab_index(0)
                    .w_full()
                    .min_w_0()
                    .h(px(26.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(sp(12.5))
                    .line_height(sp(16.0))
                    .cursor_default()
                    .focus_visible(|style| style.text_color(theme.text))
                    .hover(|style| style.text_color(theme.text))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(header_title)),
                    )
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        10.0,
                        theme.text_tertiary,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_activities(block_index, expanded, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.toggle_activities(block_index, expanded, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        if !expanded {
            return cluster.into_any_element();
        }
        // `Theme::overlay` is 5% alpha and GPUI's `opacity` multiplies it.
        let activity_surface = theme.surface.blend(theme.overlay.opacity(0.7));
        let activity_hover_surface = theme.surface.blend(theme.overlay);
        let activity_active_surface = theme.surface.blend(theme.overlay_strong.opacity(0.72));
        let mut items = div()
            .w_full()
            .min_w_0()
            .ml(px(6.0))
            .pl(px(12.0))
            .pb(px(2.0))
            .border_l_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap(px(8.0));
        for activity in activities {
            let id = activity.id;
            let background_work = self
                .state
                .selected_session
                .zip(activity.source_id.as_deref())
                .and_then(|(session_id, source_id)| {
                    self.background_work_for_activity(session_id, source_id)
                        .map(|item| (session_id, item.key.clone(), item.status))
                });
            let background_badge = background_work.map(|(session_id, key, status)| {
                let click_key = key.clone();
                let focus = self.transcript_control_focus(format!("activity-background-{id}"), cx);
                let color = work_status_color(status, *theme);
                div()
                    .id(SharedString::from(format!("activity-background-{id}")))
                    .track_focus(&focus)
                    .tab_index(0)
                    .h(px(20.0))
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .flex_none()
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(sp(12.5))
                    .text_color(color)
                    .focus_visible(|style| style.border_color(theme.accent))
                    .hover(|style| style.bg(theme.overlay_strong))
                    .child(work_status_label(status))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_background_work_surface(session_id, click_key.clone(), cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.open_background_work_surface(session_id, key.clone(), cx);
                            cx.stop_propagation();
                        }
                    }))
            });
            let reasoning = activity.reasoning.as_ref();
            let reasoning_live = live_reasoning_id == Some(id);
            let sections = if reasoning.is_some() {
                Vec::new()
            } else {
                activity_disclosure_sections(activity)
            };
            let preview = if reasoning.is_some() {
                String::new()
            } else {
                activity_preview(activity)
            };
            let action_label = activity_action_label(activity);
            let mut row_detail = activity_row_detail(activity, reasoning_live);
            if row_detail.trim().is_empty() {
                row_detail = preview;
            }
            let file_change_stats = activity_file_change_stats(activity);
            // One changed file is unambiguous, so the row itself can offer to
            // open it. A change touching several names each file in the diff
            // below, and each of those rows opens its own.
            let open_file_button = match activity.file_changes.as_slice() {
                [change] if activity.kind == ActivityKind::FileChange => {
                    Some(self.render_activity_open_file_button(
                        format!("activity-open-{id}"),
                        change.path.clone(),
                        theme,
                        cx,
                    ))
                }
                _ => None,
            };
            let shows_diff = reasoning.is_none() && activity_shows_diff(activity);
            let has_detail = reasoning
                .is_some_and(|reasoning| !reasoning.content.trim().is_empty())
                || !sections.is_empty()
                || shows_diff;
            let item_expanded = has_detail
                && self
                    .expanded_activity_items
                    .get(&id)
                    .copied()
                    .unwrap_or(reasoning_live);
            let item_focus = self.transcript_control_focus(format!("activity-item-{id}"), cx);
            let mut item = div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .rounded(px(9.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(activity_surface)
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(SharedString::from(format!("activity-item-{id}")))
                        // The parent owns a 1px border on each edge, so a
                        // 28px row makes the visible activity header 30px.
                        .h(px(28.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .rounded_tl(px(8.0))
                        .rounded_tr(px(8.0))
                        .when(!item_expanded, |element| {
                            element.rounded_bl(px(8.0)).rounded_br(px(8.0))
                        })
                        .text_size(sp(12.5))
                        .line_height(sp(16.0))
                        .when(has_detail, |element| {
                            element
                                .track_focus(&item_focus)
                                .tab_index(0)
                                .cursor_default()
                                .focus_visible(|element| element.bg(activity_hover_surface))
                                .hover(|element| element.bg(activity_hover_surface))
                                .active(|element| element.bg(activity_active_surface))
                        })
                        .child(icon(
                            activity_icon(activity.kind),
                            12.0,
                            theme.text_tertiary,
                        ))
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(action_label)),
                        )
                        .when(!row_detail.is_empty(), |element| {
                            element.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(row_detail)),
                            )
                        })
                        .when_some(file_change_stats, |row, (additions, deletions)| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_color(theme.success)
                                    .child(SharedString::from(format!("+{additions}"))),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.danger)
                                    .child(SharedString::from(format!("-{deletions}"))),
                            )
                        })
                        .children(background_badge)
                        .children(open_file_button)
                        .when(has_detail, |element| {
                            element.child(icon(
                                if item_expanded {
                                    "icons/chevron-down.svg"
                                } else {
                                    "icons/chevron-right.svg"
                                },
                                10.0,
                                theme.text_tertiary,
                            ))
                        })
                        .when(!has_detail && reasoning.is_none(), |element| {
                            element
                                .when(activity.failed, |element| {
                                    element.child(
                                        icon("icons/x.svg", 10.0, theme.danger).into_any_element(),
                                    )
                                })
                                .when(!activity.complete && !activity.failed, |element| {
                                    element.child(pulse_dot(5.0, theme.accent))
                                })
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if has_detail {
                                this.toggle_activity_item(id, item_expanded, cx);
                            }
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                            if has_detail
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                this.toggle_activity_item(id, item_expanded, cx);
                                cx.stop_propagation();
                            }
                        })),
                );
            if item_expanded && let Some(reasoning) = reasoning {
                // Reasoning remains model prose even though it now shares the
                // activity stream, so keep selectable markdown rather than
                // presenting it as monospace tool output.
                let mut palette = MarkdownPalette::from_theme(theme);
                palette.text = theme.text_secondary;
                palette.secondary = theme.text_tertiary;
                let ctx = self.markdown_ctx(
                    format!("reasoning-{id}"),
                    &palette,
                    self.scaled_markdown_metrics(MarkdownMetrics::COMPACT),
                    reasoning_live && !cx.reduce_motion(),
                );
                let reasoning_viewport = self
                    .activity_scroll_viewports
                    .borrow_mut()
                    .entry(id)
                    .or_default()
                    .clone();
                let mut views = self.activity_markdown.borrow_mut();
                let view = views.entry(id).or_default();
                if reasoning_live {
                    let start = self.live_reasoning_window_start(id, &reasoning.content, view);
                    view.set_text(&reasoning.content[start..], true);
                } else {
                    self.reasoning_window_starts.borrow_mut().remove(&id);
                    view.set_text(&reasoning.content, false);
                }
                let wheel_scroll = reasoning_viewport.scroll_handle.clone();
                let wheel_follow_tail = reasoning_viewport.follow_tail.clone();
                let markdown = if reasoning_live {
                    // The live peek pins to the tail of a growing document;
                    // building every block of a long think per pulse tick was
                    // the remaining 40%-CPU streaming path.
                    md::render::markdown_tail(view, &ctx, LIVE_REASONING_TAIL_BLOCKS)
                } else {
                    md::render::markdown(view, &ctx)
                };
                if reasoning_live && !cx.reduce_motion() && view.is_fading() {
                    // The reasoning dissolve rides the half-rate lease: fast
                    // thinking keeps a fade active for the whole phase, every
                    // tick rebuilds each visible transcript row, and 15 fps
                    // alpha on the dim compact peek is indistinguishable. The
                    // answer text keeps the full-rate dissolve.
                    motion::pulse_lease_slow(window.current_view(), cx);
                }
                item = item.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .relative()
                        .max_h(px(400.0))
                        .overflow_hidden()
                        .border_t_1()
                        .border_color(theme.border_strong)
                        .child(
                            div()
                                .id(SharedString::from(format!("reasoning-scroll-{id}")))
                                .w_full()
                                .min_w_0()
                                .max_h(px(400.0))
                                .overflow_y_scroll()
                                .track_scroll(&reasoning_viewport.scroll_handle)
                                .px(px(12.0))
                                .py(px(8.0))
                                .children(markdown)
                                .on_scroll_wheel(move |_, window, cx| {
                                    contain_scroll(&wheel_scroll, cx);
                                    let scroll = wheel_scroll.clone();
                                    let follow_tail = wheel_follow_tail.clone();
                                    window.defer(cx, move |_, _| {
                                        follow_tail.set(activity_scroll_at_bottom(&scroll));
                                    });
                                }),
                        )
                        .child(activity_scroll_fade(
                            reasoning_viewport.scroll_handle.clone(),
                            ActivityScrollFadeSide::Top,
                            activity_surface,
                        ))
                        .child(activity_scroll_fade(
                            reasoning_viewport.scroll_handle.clone(),
                            ActivityScrollFadeSide::Bottom,
                            activity_surface,
                        ))
                        .child(scrollbar::vertical(
                            &reasoning_viewport.scroll_handle,
                            &reasoning_viewport.scrollbar,
                        ))
                        .child(activity_scroll_guard(reasoning_viewport, reasoning_live)),
                );
            }
            if item_expanded && shows_diff {
                let diff = self.activity_diff_rows(activity);
                if !diff.is_empty() {
                    item = item.child(self.render_activity_diff(
                        id,
                        &diff,
                        activity_surface,
                        theme,
                        cx,
                    ));
                }
            }
            if item_expanded
                && reasoning.is_none()
                && (!sections.is_empty() || !activity.image_urls.is_empty())
            {
                let palette = MarkdownPalette::from_theme(theme);
                let ctx = self.markdown_ctx(
                    format!("activity-{id}"),
                    &palette,
                    self.scaled_markdown_metrics(MarkdownMetrics::COMPACT),
                    false,
                );
                let (mono_size, mono_line) = self.activity_mono_text();
                let mut detail_card = div()
                    .w_full()
                    .min_w_0()
                    .border_t_1()
                    .border_color(theme.border_strong)
                    .px(px(12.0))
                    .py(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .font_family(md::render::MONO_FAMILY)
                    .text_size(px(mono_size))
                    .line_height(px(mono_line))
                    .text_color(theme.text_secondary)
                    .whitespace_normal()
                    .overflow_hidden();
                for section in sections {
                    let section_kind = section.kind;
                    let content = section.content;
                    let mut section_view = div().w_full().min_w_0().flex().flex_col().gap(px(3.0));
                    if let Some(label) = section_kind.label() {
                        let copy_content = content.clone();
                        let copied = self
                            .copied_activity_feedback
                            .contains_key(&(id, section_kind));
                        let copy_waku = cx.entity().downgrade();
                        let copy_tooltip = SharedString::from(if copied {
                            tr!("common.copied")
                        } else {
                            tr!("common.copy_named", name = label.to_lowercase())
                        });
                        section_view = section_view.child(
                            div()
                                .h(px(20.0))
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text_secondary)
                                        .child(label),
                                )
                                .when(!content.is_empty(), |header| {
                                    header.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "copy-activity-{}-{}",
                                                id,
                                                section_kind.id()
                                            )))
                                            .size(px(20.0))
                                            .rounded(px(5.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_default()
                                            .hover(|button| button.bg(theme.overlay_strong))
                                            .child(icon(
                                                if copied {
                                                    "icons/check.svg"
                                                } else {
                                                    "icons/copy.svg"
                                                },
                                                11.0,
                                                theme.text_ghost,
                                            ))
                                            .tooltip(Tooltip::text(copy_tooltip.clone()))
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_content.clone(),
                                                ));
                                                let _ = copy_waku.update(cx, |this, cx| {
                                                    this.show_activity_section_copied(
                                                        id,
                                                        section_kind,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                }),
                        );
                    }
                    if !content.is_empty() {
                        if activity.kind == ActivityKind::Command
                            && section_kind == ActivityDisclosureSectionKind::Output
                        {
                            let output_viewport = self
                                .activity_scroll_viewports
                                .borrow_mut()
                                .entry(id)
                                .or_default()
                                .clone();
                            let wheel_scroll = output_viewport.scroll_handle.clone();
                            let wheel_follow_tail = output_viewport.follow_tail.clone();
                            section_view = section_view.child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .relative()
                                    .max_h(px(400.0))
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "activity-output-scroll-{id}"
                                            )))
                                            .w_full()
                                            .min_w_0()
                                            .max_h(px(400.0))
                                            .overflow_y_scroll()
                                            .track_scroll(&output_viewport.scroll_handle)
                                            .py(px(4.0))
                                            .pr(px(8.0))
                                            .child(md::render::plain_text(
                                                content.clone(),
                                                md::render::MONO_FAMILY,
                                                FontWeight::NORMAL,
                                                theme.text_secondary,
                                                &ctx,
                                            ))
                                            .on_scroll_wheel(move |_, window, cx| {
                                                contain_scroll(&wheel_scroll, cx);
                                                let scroll = wheel_scroll.clone();
                                                let follow_tail = wheel_follow_tail.clone();
                                                window.defer(cx, move |_, _| {
                                                    follow_tail
                                                        .set(activity_scroll_at_bottom(&scroll));
                                                });
                                            }),
                                    )
                                    .child(activity_scroll_fade(
                                        output_viewport.scroll_handle.clone(),
                                        ActivityScrollFadeSide::Top,
                                        activity_surface,
                                    ))
                                    .child(activity_scroll_fade(
                                        output_viewport.scroll_handle.clone(),
                                        ActivityScrollFadeSide::Bottom,
                                        activity_surface,
                                    ))
                                    .child(scrollbar::vertical(
                                        &output_viewport.scroll_handle,
                                        &output_viewport.scrollbar,
                                    ))
                                    .child(activity_scroll_guard(
                                        output_viewport,
                                        !activity.complete,
                                    )),
                            );
                        } else {
                            section_view = section_view.child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .child(md::render::plain_text(
                                        content.clone(),
                                        md::render::MONO_FAMILY,
                                        FontWeight::NORMAL,
                                        theme.text_secondary,
                                        &ctx,
                                    )),
                            );
                        }
                    }
                    detail_card = detail_card.child(section_view);
                }
                for (image_index, image_url) in activity.image_urls.iter().enumerate() {
                    let image = self.image_for_reference(image_url, None, None, cx);
                    detail_card = detail_card.child(render_activity_image(
                        image_url,
                        image,
                        id,
                        image_index,
                        theme,
                    ));
                }
                item = item.child(detail_card);
            }
            items = items.child(item);
        }
        cluster.child(items).into_any_element()
    }

    /// The diff for an expanded file-change activity.
    ///
    /// Rows bleed to the card's edges so a changed line reads as a band, and
    /// the whole diff sits in the same capped, faded viewport as command
    /// output — a large edit stays one transcript row tall.
    fn render_activity_diff(
        &self,
        id: Uuid,
        diff: &activity_diff::Diff,
        surface: Hsla,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let viewport = self
            .activity_diff_viewports
            .borrow_mut()
            .entry(id)
            .or_default()
            .clone();
        let wheel_scroll = viewport.scroll_handle.clone();
        let (diff_mono_size, diff_mono_line) = self.activity_mono_text();
        let mut rows = div()
            .id(SharedString::from(format!("activity-diff-scroll-{id}")))
            .w_full()
            .min_w_0()
            .max_h(px(ACTIVITY_DIFF_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&viewport.scroll_handle)
            .flex()
            .flex_col()
            .font_family(md::render::MONO_FAMILY)
            .text_size(px(diff_mono_size))
            .line_height(px(diff_mono_line))
            .on_scroll_wheel(move |_, _, cx| contain_scroll(&wheel_scroll, cx));
        for (index, line) in diff.snapshot.lines.iter().enumerate() {
            rows = rows.child(self.render_activity_diff_row(
                id,
                index,
                line,
                &diff.snapshot,
                theme,
                cx,
            ));
        }
        if diff.hidden_rows > 0 {
            let note = if diff.hidden_rows == 1 {
                tr!("diff.rows_hidden_one")
            } else {
                tr!("diff.rows_hidden", count = diff.hidden_rows)
            };
            rows = rows.child(
                div()
                    .w_full()
                    .min_w_0()
                    .px(px(12.0))
                    .py(px(4.0))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(note)),
            );
        }
        div()
            .w_full()
            .min_w_0()
            .relative()
            .max_h(px(ACTIVITY_DIFF_MAX_HEIGHT))
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border_strong)
            .child(rows)
            .child(activity_scroll_fade(
                viewport.scroll_handle.clone(),
                ActivityScrollFadeSide::Top,
                surface,
            ))
            .child(activity_scroll_fade(
                viewport.scroll_handle.clone(),
                ActivityScrollFadeSide::Bottom,
                surface,
            ))
            .child(scrollbar::vertical(
                &viewport.scroll_handle,
                &viewport.scrollbar,
            ))
            .into_any_element()
    }

    fn render_activity_diff_row(
        &self,
        id: Uuid,
        index: usize,
        line: &crate::review_diff::Line,
        snapshot: &crate::review_diff::Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::review_diff::LineKind;

        match &line.kind {
            LineKind::FileHeader => {
                let Some(file) = snapshot.files.get(line.file_index) else {
                    return div().into_any_element();
                };
                div()
                    .w_full()
                    .min_w_0()
                    .h(px(24.0))
                    .pl(px(10.0))
                    .pr(px(6.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .bg(theme.overlay)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_color(theme.text_secondary)
                    .font_weight(FontWeight::MEDIUM)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(file.path.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.success)
                            .child(SharedString::from(format!("+{}", file.additions))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.danger)
                            .child(SharedString::from(format!("-{}", file.deletions))),
                    )
                    .child(self.render_activity_open_file_button(
                        format!("activity-diff-open-{id}-{}", line.file_index),
                        file.path.clone(),
                        theme,
                        cx,
                    ))
                    .into_any_element()
            }
            // Unchanged lines the provider left out of its patch. Nothing was
            // withheld locally, so this marks the break rather than offering
            // to expand it the way the Review panel does.
            LineKind::Gap(gap) => activity_diff_break_row(
                Some(tr!("diff.unmodified_lines", count = gap.count())),
                theme,
            ),
            LineKind::HunkHeader | LineKind::Meta => activity_diff_break_row(
                (!line.content.is_empty()).then(|| line.content.clone()),
                theme,
            ),
            LineKind::Context | LineKind::Addition | LineKind::Deletion => render_diff_code_row(
                line,
                index,
                &format!("activity-diff-{id}"),
                &self.transcript_selection,
                DiffRowStyle::activity(self.state.code_font_size),
                theme,
            ),
        }
    }
}

/// The separator between two hunks of the same file.
fn activity_diff_break_row(label: Option<String>, theme: &Theme) -> AnyElement {
    div()
        .w_full()
        .min_w_0()
        .h(px(20.0))
        .flex_none()
        .flex()
        .items_center()
        .font_family(md::render::MONO_FAMILY)
        // Fixed like the Review panel's gap and hunk captions: a caption in
        // the code surface follows neither font setting.
        .text_size(px(12.5))
        .bg(theme.overlay)
        .text_color(theme.text_ghost)
        .child(
            div()
                .w(px(ACTIVITY_DIFF_GUTTER_WIDTH))
                .flex_none()
                .self_stretch()
                .flex()
                .items_center()
                .justify_center()
                .border_r_1()
                .border_color(theme.border)
                .child("⋯"),
        )
        .children(label.map(|label| {
            div()
                .min_w_0()
                .pl(px(12.0))
                .truncate()
                .child(SharedString::from(label))
        }))
        .into_any_element()
}

#[derive(Clone, Copy)]
enum ActivityScrollFadeSide {
    Top,
    Bottom,
}

fn activity_scroll_at_bottom(scroll: &ScrollHandle) -> bool {
    let scrolled = -scroll.offset().y;
    scroll.max_offset().y - scrolled <= px(0.5)
}

fn activity_scroll_follow_state(
    following: bool,
    previous_scrolled: Option<Pixels>,
    previous_max_offset: Option<Pixels>,
    scrolled: Pixels,
    max_offset: Pixels,
) -> bool {
    let at_bottom = max_offset - scrolled <= px(0.5);
    let user_moved = previous_scrolled.zip(previous_max_offset).is_some_and(
        |(previous_scrolled, previous_max_offset)| {
            (scrolled - previous_scrolled).abs() > px(0.5)
                && (max_offset - previous_max_offset).abs() <= px(0.5)
        },
    );
    if user_moved || at_bottom {
        at_bottom
    } else {
        following
    }
}

/// Pure window arithmetic behind [`Waku::live_reasoning_window_start`]:
/// given the cached start and the current content, the byte offset the
/// window should render from. Every returned offset is a character boundary
/// of `content`, so callers may slice with it directly.
fn live_reasoning_window_anchor(cached: usize, content: &str) -> usize {
    // A restarted block can leave the cached start past the end of the new
    // content or inside a multibyte character; either way the window is
    // stale (`is_char_boundary` is false past the end too), so restart it.
    let cached = if content.is_char_boundary(cached) {
        cached
    } else {
        0
    };
    if content.len() - cached <= LIVE_REASONING_WINDOW_MAX {
        return cached;
    }
    // Slide: re-anchor near the tail, preferring a block boundary so the
    // window opens on whole markdown. The raw cut is an arbitrary byte
    // offset, so advance it to a character boundary before slicing; the
    // end of the string is always a boundary, so this terminates.
    let mut cut = content.len() - LIVE_REASONING_WINDOW_TARGET;
    while !content.is_char_boundary(cut) {
        cut += 1;
    }
    content[cut..]
        .find("\n\n")
        .map(|found| cut + found + 2)
        .unwrap_or(cut)
}

impl Waku {
    /// Byte offset the live reasoning peek renders from, slid forward as the
    /// thought grows. The peek pins a 400 px viewport to the tail, but
    /// markdown cost is O(rendered source) per pulse tick regardless of block
    /// shape, so the window keeps parse, flatten, elements, and veil all
    /// O(window); the full trace renders once the turn settles. A slide
    /// re-anchors at a block boundary and reseeds the view so already-shown
    /// text never re-dissolves.
    fn live_reasoning_window_start(
        &self,
        id: Uuid,
        content: &str,
        view: &mut MarkdownView,
    ) -> usize {
        let mut starts = self.reasoning_window_starts.borrow_mut();
        let start = starts.entry(id).or_insert(0);
        let next = live_reasoning_window_anchor(*start, content);
        if next != *start {
            *start = next;
            *view = MarkdownView::seeded();
        }
        *start
    }
}

fn activity_scroll_guard(viewport: ActivityScrollViewport, live: bool) -> impl IntoElement {
    canvas(
        move |_, window, cx| {
            let scrolled = -viewport.scroll_handle.offset().y;
            let max_offset = viewport.scroll_handle.max_offset().y;
            let following = activity_scroll_follow_state(
                viewport.follow_tail.get(),
                viewport.last_scrolled.get(),
                viewport.last_max_offset.get(),
                scrolled,
                max_offset,
            );
            viewport.follow_tail.set(following);
            viewport.last_scrolled.set(Some(scrolled));
            viewport.last_max_offset.set(Some(max_offset));
            if live && following && max_offset - scrolled > px(0.5) {
                viewport.scroll_handle.scroll_to_bottom();
                // Notify the enclosing island rather than `window.refresh()`:
                // a refresh busts every pane cache, and this fires on each
                // stream commit while a live viewport follows its tail.
                cx.notify(window.current_view());
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .w(px(0.0))
    .h(px(0.0))
}

fn activity_scroll_fade(
    scroll: ScrollHandle,
    side: ActivityScrollFadeSide,
    surface: Hsla,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            let scrolled = -scroll.offset().y;
            let max_offset = scroll.max_offset().y;
            let visible = match side {
                ActivityScrollFadeSide::Top => scrolled > px(0.5),
                ActivityScrollFadeSide::Bottom => max_offset - scrolled > px(0.5),
            };
            visible.then(|| {
                let transparent = surface.opacity(0.0);
                let background = match side {
                    ActivityScrollFadeSide::Top => linear_gradient(
                        180.0,
                        linear_color_stop(surface, 0.0),
                        linear_color_stop(transparent, 1.0),
                    ),
                    ActivityScrollFadeSide::Bottom => linear_gradient(
                        180.0,
                        linear_color_stop(transparent, 0.0),
                        linear_color_stop(surface, 1.0),
                    ),
                };
                fill(bounds, background)
            })
        },
        |_, fade, window, _| {
            if let Some(fade) = fade {
                window.paint_quad(fade);
            }
        },
    )
    .absolute()
    .left_0()
    .w_full()
    .h(px(18.0))
    .when(matches!(side, ActivityScrollFadeSide::Top), |element| {
        element.top_0()
    })
    .when(matches!(side, ActivityScrollFadeSide::Bottom), |element| {
        element.bottom_0()
    })
}

fn render_activity_image(
    image_url: &str,
    image: Option<Arc<gpui::Image>>,
    activity_id: Uuid,
    image_index: usize,
    theme: &Theme,
) -> AnyElement {
    // Daemon blobs arrive only when a visible row requests them and GPUI keeps
    // their decoded form in memory. Only legacy inline data URLs still pay a
    // per-render base64 decode.
    let id = SharedString::from(format!("activity-image-{activity_id}-{image_index}"));
    if let Some(image) = image {
        return img(image)
            .id(id)
            .w(px(ACTIVITY_IMAGE_WIDTH))
            .max_w(gpui::relative(1.0))
            .max_h(px(ACTIVITY_IMAGE_HEIGHT))
            .mt(px(8.0))
            .rounded(px(4.0))
            .object_fit(ObjectFit::Contain)
            .into_any_element();
    }
    if waku_protocol::blob::is_reference(image_url)
        || image_url.starts_with(waku_protocol::attachments::ATTACHMENT_SCHEME)
    {
        return div()
            .id(id)
            .w(px(ACTIVITY_IMAGE_WIDTH))
            .max_w(gpui::relative(1.0))
            .h(px(80.0))
            .mt(px(8.0))
            .rounded(px(4.0))
            .bg(theme.inset)
            .flex()
            .items_center()
            .justify_center()
            .child(icon("icons/file-types/image.svg", 18.0, theme.text_ghost))
            .into_any_element();
    };

    match decode_activity_image(image_url) {
        Some(image) => img(image),
        None => img(image_url.to_owned()),
    }
    .id(id)
    .w(px(ACTIVITY_IMAGE_WIDTH))
    .max_w(gpui::relative(1.0))
    .max_h(px(ACTIVITY_IMAGE_HEIGHT))
    .mt(px(8.0))
    .rounded(px(4.0))
    .object_fit(ObjectFit::Contain)
    .into_any_element()
}

fn decode_activity_image(image_url: &str) -> Option<std::sync::Arc<gpui::Image>> {
    let (header, encoded) = image_url.split_once(",")?;
    let mime_type = header.strip_prefix("data:")?.split(';').next()?;
    let format = gpui::ImageFormat::from_mime_type(mime_type)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    (!bytes.is_empty()).then(|| std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

#[cfg(test)]
mod activity_scroll_tests {
    use super::*;

    #[test]
    fn user_scroll_pauses_following_until_the_tail_is_reached_again() {
        assert!(!activity_scroll_follow_state(
            true,
            Some(px(100.0)),
            Some(px(200.0)),
            px(70.0),
            px(200.0),
        ));
        assert!(!activity_scroll_follow_state(
            false,
            Some(px(70.0)),
            Some(px(200.0)),
            px(120.0),
            px(200.0),
        ));
        assert!(activity_scroll_follow_state(
            false,
            Some(px(120.0)),
            Some(px(200.0)),
            px(200.0),
            px(200.0),
        ));
    }

    #[test]
    fn growing_content_does_not_cancel_tail_following() {
        assert!(activity_scroll_follow_state(
            true,
            Some(px(200.0)),
            Some(px(200.0)),
            px(200.0),
            px(240.0),
        ));
        assert!(!activity_scroll_follow_state(
            false,
            Some(px(120.0)),
            Some(px(200.0)),
            px(120.0),
            px(240.0),
        ));
    }
}

#[cfg(test)]
mod live_reasoning_window_tests {
    use super::*;

    #[test]
    fn slide_lands_on_a_character_boundary_in_multibyte_content() {
        // A body of 3-byte chars with two ASCII bytes at the end leaves the
        // naive cut mid-character while both tuning consts are KiB multiples;
        // the guard assert keeps the test honest if they are ever retuned.
        let content = "界".repeat(LIVE_REASONING_WINDOW_MAX / 3 + 1) + "zz";
        assert!(content.len() > LIVE_REASONING_WINDOW_MAX);
        assert!(
            !content.is_char_boundary(content.len() - LIVE_REASONING_WINDOW_TARGET),
            "setup must place the naive cut mid-character to cover the panic",
        );
        let start = live_reasoning_window_anchor(0, &content);
        assert!(content.is_char_boundary(start));
        assert!(content.len() - start <= LIVE_REASONING_WINDOW_TARGET);
        assert!(!content[start..].is_empty());
    }

    #[test]
    fn stale_start_from_a_restarted_block_resets_to_zero() {
        let content = "思".repeat(64);
        assert!(!content.is_char_boundary(4));
        assert_eq!(live_reasoning_window_anchor(4, &content), 0);
        assert_eq!(live_reasoning_window_anchor(content.len() + 1, &content), 0);
    }

    #[test]
    fn stale_start_still_slides_when_the_new_content_is_long() {
        let content = "界".repeat(LIVE_REASONING_WINDOW_MAX);
        assert!(!content.is_char_boundary(5));
        let start = live_reasoning_window_anchor(5, &content);
        assert!(content.is_char_boundary(start));
        assert!(content.len() - start <= LIVE_REASONING_WINDOW_TARGET);
    }

    #[test]
    fn slide_reanchors_after_a_block_boundary_when_one_is_near() {
        let content = format!("{}\n\ntail", "a".repeat(LIVE_REASONING_WINDOW_MAX));
        let start = live_reasoning_window_anchor(0, &content);
        assert_eq!(&content[start..], "tail");
    }

    #[test]
    fn window_below_the_threshold_keeps_the_cached_start() {
        let content = "a".repeat(LIVE_REASONING_WINDOW_MAX);
        assert_eq!(live_reasoning_window_anchor(7, &content), 7);
    }
}

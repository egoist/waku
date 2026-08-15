use super::*;
use base64::Engine as _;

const CHANGED_FILES_PREVIEW_LIMIT: usize = 3;
/// Keep one virtualized transcript row bounded even when a generator touches
/// hundreds of files. The full immutable list remains one click away in the
/// right panel.
const CHANGED_FILES_EXPANDED_LIMIT: usize = 12;

#[derive(Clone, Debug, PartialEq)]
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
    hovered_turn: Option<Uuid>,
    focus_handles: HashMap<Uuid, FocusHandle>,
    visual_state: NavigationRailVisualState,
    transition_from: NavigationRailVisualState,
    animation_generation: u64,
}

impl ConversationNavigationRail {
    pub(super) fn new() -> Self {
        Self {
            waku: None,
            snapshot: ConversationNavigationRailSnapshot::default(),
            hovered_turn: None,
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
        if self.snapshot.reset_generation != snapshot.reset_generation {
            self.hovered_turn = None;
            self.focus_handles.clear();
            self.visual_state = NavigationRailVisualState::default();
            self.transition_from = NavigationRailVisualState::default();
            self.animation_generation = self.animation_generation.wrapping_add(1);
        } else {
            self.focus_handles.retain(|message_id, _| {
                snapshot
                    .turns
                    .iter()
                    .any(|turn| turn.message_id == *message_id)
            });
        }
        self.snapshot = snapshot;
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
        &self,
        window: &mut Window,
        chat_viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.prefetch_checkpoint_refs(cx);
        self.sync_transcript_rows();
        self.sync_transcript_layout_width(window);
        let transcript_rows = self.active_transcript_rows().clone();
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
        let scroll_to_bottom = should_show_scroll_to_bottom(
            self.transcript_is_scrolled.get(),
            self.transcript_anchor_following.get(),
            transcript_scrollable,
            viewport_bottom,
            tail_bottom,
            anchor_end_space,
        )
        .then(|| {
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
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            // Painted before any row, so the frame's selection registry holds
            // exactly the text elements this frame put on screen, in order.
            .child(md::render::frame_reset(self.transcript_selection.clone()))
            .child(
                list(transcript_rows, move |index, _window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| entity.update(cx, |this, cx| this.transcript_row(index, cx)))
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
            .into_any_element()
    }

    fn scroll_transcript_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.sync_transcript_rows();
        self.transcript_anchor_following
            .set(self.transcript_anchor.get().is_some());
        self.active_transcript_rows().scroll_to_end();
        self.transcript_is_scrolled.set(false);
        cx.notify();
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
        // Hundreds of turns cannot each keep a legible tick, so ticks sample
        // the turns: every tick keeps the rail's full pitch and stands for a
        // contiguous bucket of turns, represented by the bucket's first. While
        // everything fits this is the identity and each turn has its own tick.
        // Bounding the tick count also bounds what a hover re-render builds —
        // per-frame work stays proportional to the viewport, not the session.
        let tick_count = navigation_rail_tick_count(turn_count, viewport_height);
        if tick_count == 0 {
            return div().into_any_element();
        }
        let rail_height = navigation_rail_height(turn_count, viewport_height);
        let rail_top = (viewport_height - rail_height).max(0.0) / 2.0;
        let tick_turn_indexes = (0..tick_count)
            .map(|tick_index| navigation_rail_tick_turn(tick_index, tick_count, turn_count))
            .collect::<Vec<_>>();
        let tick_message_ids = tick_turn_indexes
            .iter()
            .map(|&turn_index| turns[turn_index].message_id)
            .collect::<Vec<_>>();
        let focus_handles = tick_message_ids
            .iter()
            .map(|&message_id| self.navigation_rail_focus_handle(message_id, window, cx))
            .collect::<Vec<_>>();
        // Focus emphasizes a tick only while focus is keyboard-driven, matching
        // the `focus_visible` ring: a click also focuses the tick it hit, and
        // ungated focus would pin the preview card open after the cursor left.
        let focused_tick_index = window
            .last_input_was_keyboard()
            .then(|| {
                focus_handles
                    .iter()
                    .position(|focus_handle| focus_handle.is_focused(window))
            })
            .flatten();
        let hovered_tick_index = self
            .hovered_turn
            .and_then(|message_id| tick_message_ids.iter().position(|&id| id == message_id));
        let emphasized_tick_index = hovered_tick_index.or(focused_tick_index);
        let active_tick_index = self.snapshot.active_turn.and_then(|message_id| {
            turns
                .iter()
                .position(|turn| turn.message_id == message_id)
                .map(|turn_index| navigation_rail_turn_tick(turn_index, tick_count, turn_count))
        });
        let visual_state = NavigationRailVisualState {
            emphasized_turn: emphasized_tick_index.map(|index| tick_message_ids[index]),
        };
        let previous_visual_state = self.visual_state;
        if previous_visual_state != visual_state {
            self.transition_from = previous_visual_state;
            self.visual_state = visual_state;
            self.animation_generation = self.animation_generation.wrapping_add(1);
        }
        let transition_from = self.transition_from;
        let tick_for_message = |message_id: Option<Uuid>| {
            message_id
                .and_then(|message_id| tick_message_ids.iter().position(|&id| id == message_id))
        };
        let from_emphasized_tick_index = tick_for_message(transition_from.emphasized_turn);
        let animation_generation = self.animation_generation;

        let tick_rows = tick_message_ids
            .iter()
            .zip(focus_handles)
            .enumerate()
            .map(|(tick_index, (&message_id, focus_handle))| {
                let from_width = NAVIGATION_RAIL_TICK_WIDTH
                    * navigation_rail_scale(tick_index, from_emphasized_tick_index);
                let to_width = NAVIGATION_RAIL_TICK_WIDTH
                    * navigation_rail_scale(tick_index, emphasized_tick_index);
                let prominent = active_tick_index == Some(tick_index)
                    || emphasized_tick_index == Some(tick_index);
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
                        Animation::new(NAVIGATION_RAIL_ANIMATION_DURATION)
                            .with_easing(ease_out_quint()),
                        move |element, delta| {
                            element.w(px(from_width + (to_width - from_width) * delta))
                        },
                    );

                div()
                    .id(SharedString::from(format!(
                        "conversation-navigation-turn-hit-{message_id}"
                    )))
                    .w(px(NAVIGATION_RAIL_WIDTH))
                    .flex_1()
                    .min_h_0()
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
                            .tab_index(tick_index as isize)
                            .focus_visible(|style| style.border_1().border_color(theme.accent))
                            .on_key_down(cx.listener(move |this, event, window, cx| {
                                this.navigation_rail_key_down(message_id, event, window, cx);
                            }))
                            .child(tick),
                    )
            })
            .collect::<Vec<_>>();

        let rail = div()
            .id("conversation-navigation-rail")
            .absolute()
            .left(px(NAVIGATION_RAIL_LEFT))
            .top(px(rail_top))
            .w(px(NAVIGATION_RAIL_WIDTH))
            .h(px(rail_height))
            .flex()
            .flex_col()
            .tab_index(0)
            .tab_group()
            .tab_stop(false)
            .children(tick_rows);

        let preview = emphasized_tick_index.map(|tick_index| {
            let turn = &turns[tick_turn_indexes[tick_index]];
            let hit_height = rail_height / tick_count as f32;
            let preview_height = 126.0;
            let max_preview_top = (viewport_height - preview_height - 12.0).max(12.0);
            let preview_top = (rail_top + (tick_index as f32 + 0.5) * hit_height
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
                        .text_size(px(14.0))
                        .line_height(px(20.0))
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
                            .text_size(px(13.0))
                            .line_height(px(20.0))
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
        cx.on_focus(&focus_handle, window, |_: &mut Self, _, cx| {
            cx.notify();
        })
        .detach();
        cx.on_blur(&focus_handle, window, |_: &mut Self, _, cx| {
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
        // Arrows walk the rendered ticks, not the underlying turns: on a
        // sampled rail only tick representatives have focus handles.
        let turns = &self.snapshot.turns;
        let turn_count = turns.len();
        let tick_count = navigation_rail_tick_count(turn_count, self.snapshot.viewport_height);
        if tick_count == 0 {
            return;
        }
        let tick_turn =
            |tick_index: usize| navigation_rail_tick_turn(tick_index, tick_count, turn_count);
        let Some(tick_index) =
            (0..tick_count).position(|tick| turns[tick_turn(tick)].message_id == message_id)
        else {
            return;
        };

        let target_tick = match event.keystroke.key.as_str() {
            "up" => Some(tick_index.saturating_sub(1)),
            "down" => Some((tick_index + 1).min(tick_count - 1)),
            "home" => Some(0),
            "end" => Some(tick_count - 1),
            "enter" | "space" => {
                self.activate_turn(message_id, cx);
                cx.stop_propagation();
                return;
            }
            _ => None,
        };
        let Some(target_tick) = target_tick else {
            return;
        };
        if let Some(focus_handle) = self
            .focus_handles
            .get(&turns[tick_turn(target_tick)].message_id)
            .cloned()
        {
            focus_handle.focus(window, cx);
            cx.stop_propagation();
        }
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
        });
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
        cx.spawn(async move |this, cx| {
            let existing = cx
                .background_executor()
                .spawn(async move { checkpoint::session_turn_refs(&project_path, session_id) })
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

    /// The markdown render context for one transcript row. Element keys are
    /// scoped to the row, so a virtualized remount recreates the same keys and
    /// an in-progress selection survives scrolling.
    fn markdown_ctx<'a>(
        &self,
        row: String,
        palette: &'a MarkdownPalette,
        metrics: MarkdownMetrics,
    ) -> MarkdownCtx<'a> {
        MarkdownCtx::new(row, palette, metrics, self.transcript_selection.clone())
            .with_link_handler(self.markdown_link_handler.clone())
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

    pub(super) fn transcript_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
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
                    let assistant_footer_copy_content = self
                        .selected_session()
                        .and_then(|session| assistant_response_footer(session, message_index));
                    let assistant_footer_time = self
                        .selected_session()
                        .and_then(|session| assistant_response_footer_time(session, message_index));
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
                                    && edit.turn_count == action.turn_count
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
                    let menu = self.menu_handle(format!("message-{}", message.id), cx);
                    let metrics = if message.role == MessageRole::User {
                        MarkdownMetrics::USER_MESSAGE
                    } else {
                        MarkdownMetrics::BODY
                    };
                    let ctx =
                        self.markdown_ctx(format!("message-{}", message.id), &palette, metrics);
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
                    render_message(
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
                            markdown: view,
                            ctx: &ctx,
                            menu,
                            waku,
                            composer,
                        },
                        cx,
                    )
                })
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| self.render_activities_row(&block.activities, block_index, &theme, cx))
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
            .text_size(px(11.5))
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
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_size(px(11.0))
                                    .line_height(px(14.0))
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
                            .text_size(px(11.5))
                            .text_color(theme.text_secondary)
                            .tooltip(Tooltip::text(file.path.clone()))
                            .child(file.path.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme.success)
                            .child(format!("+{}", file.additions)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
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
                    .text_size(px(11.5))
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
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(label))
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
                    .text_size(px(11.5))
                    .line_height(px(16.0))
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
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = activities.iter().any(|activity| !activity.complete);
        let live_turn = self
            .selected_session()
            .and_then(AgentSession::active_turn_id)
            .is_some_and(|turn_id| {
                self.selected_transcript_blocks()
                    .get(block_index)
                    .is_some_and(|block| block.turn_id == Some(turn_id))
            });
        let expanded = self
            .activities_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(live_turn);
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
        let cluster = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .id(SharedString::from(format!("activity-toggle-{block_index}")))
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .cursor_default()
                    .when(running, |element| {
                        element.child(pulse_dot(
                            format!("activity-running-{block_index}"),
                            5.0,
                            theme.accent,
                        ))
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(activity_summary(activities))),
                    )
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        9.0,
                        theme.text_ghost,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_activities(block_index, expanded, cx);
                    })),
            );
        if !expanded {
            return cluster.into_any_element();
        }
        let mut items = div().w_full().min_w_0().flex().flex_col().pl(px(15.0));
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
                    .text_size(px(9.5))
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
            let display_title = reasoning.map_or_else(
                || activity_display_title(activity),
                |reasoning| reasoning_activity_title(reasoning, reasoning_live),
            );
            let file_change_stats = activity_file_change_stats(activity);
            let has_detail = reasoning
                .is_some_and(|reasoning| !reasoning.content.trim().is_empty())
                || !sections.is_empty();
            let item_expanded = has_detail
                && self
                    .expanded_activity_items
                    .get(&id)
                    .copied()
                    .unwrap_or(reasoning_live);
            let mut item = div().flex().flex_col().child(
                div()
                    .id(SharedString::from(format!("activity-item-{id}")))
                    .min_h(px(24.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.5))
                    .line_height(px(14.0))
                    .when(has_detail, |element| {
                        element
                            .cursor_default()
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .child(icon(
                        activity_icon(activity.kind),
                        11.0,
                        theme.text_tertiary,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(300.0))
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(display_title)),
                            )
                            .when_some(file_change_stats, |title, (additions, deletions)| {
                                title
                                    .child(
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
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_ghost)
                            .when(item_expanded, |element| element.invisible())
                            .child(SharedString::from(preview)),
                    )
                    .children(background_badge)
                    .when(reasoning.is_none(), |element| {
                        element.child(if activity.failed {
                            icon("icons/x.svg", 10.0, theme.danger).into_any_element()
                        } else if activity.complete {
                            icon("icons/check.svg", 10.0, theme.text_ghost).into_any_element()
                        } else {
                            pulse_dot(format!("activity-pulse-{id}"), 5.0, theme.accent)
                        })
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if has_detail {
                            this.toggle_activity_item(id, item_expanded, cx);
                        }
                    })),
            );
            if item_expanded && let Some(reasoning) = reasoning {
                // Reasoning remains model prose even though it now shares the
                // activity stream, so keep selectable markdown rather than
                // presenting it as monospace tool output.
                let mut palette = MarkdownPalette::from_theme(theme);
                palette.text = theme.text_tertiary;
                palette.secondary = theme.text_tertiary;
                let ctx = self.markdown_ctx(
                    format!("reasoning-{id}"),
                    &palette,
                    MarkdownMetrics::COMPACT,
                );
                let mut views = self.activity_markdown.borrow_mut();
                let view = views.entry(id).or_default();
                view.set_text(&reasoning.content, reasoning_live);
                item = item.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .px(px(4.0))
                        .children(md::render::markdown(view, &ctx)),
                );
            }
            if item_expanded && reasoning.is_none() {
                let palette = MarkdownPalette::from_theme(theme);
                let ctx =
                    self.markdown_ctx(format!("activity-{id}"), &palette, MarkdownMetrics::COMPACT);
                let mut detail_card = div()
                    .ml(px(21.0))
                    .mr(px(4.0))
                    .min_w_0()
                    .mt(px(2.0))
                    .mb(px(4.0))
                    .p(px(8.0))
                    .rounded(px(7.0))
                    .bg(theme.inset)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .font_family(md::render::MONO_FAMILY)
                    .text_size(px(10.5))
                    .line_height(px(16.0))
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
                        section_view = section_view.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_size(px(10.5))
                                .line_height(px(16.0))
                                .child(md::render::plain_text(
                                    content.clone(),
                                    md::render::MONO_FAMILY,
                                    FontWeight::NORMAL,
                                    theme.text_secondary,
                                    &ctx,
                                )),
                        );
                    }
                    detail_card = detail_card.child(section_view);
                }
                for (image_index, image_url) in activity.image_urls.iter().enumerate() {
                    detail_card =
                        detail_card.child(render_activity_image(image_url, id, image_index));
                }
                item = item.child(detail_card);
            }
            items = items.child(item);
        }
        cluster.child(items).into_any_element()
    }
}

fn reasoning_activity_title(reasoning: &ReasoningBlock, live: bool) -> String {
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

fn render_activity_image(image_url: &str, activity_id: Uuid, image_index: usize) -> AnyElement {
    // Stored blobs go through GPUI's asset cache, which reads and decodes the
    // file once off the UI thread. Only legacy inline data URLs still pay a
    // per-render base64 decode.
    let element = match crate::blob_store::shared_path_for(image_url) {
        Some(path) => img(path),
        None => match decode_activity_image(image_url) {
            Some(image) => img(image),
            None => img(image_url.to_owned()),
        },
    };
    element
        .id(SharedString::from(format!(
            "activity-image-{activity_id}-{image_index}"
        )))
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

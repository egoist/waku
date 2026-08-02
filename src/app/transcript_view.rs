use super::*;
use base64::Engine as _;

#[derive(Clone, Debug, PartialEq)]
struct ConversationNavigationRailSnapshot {
    visible: bool,
    turns: Vec<TranscriptNavigationTurn>,
    viewport_height: f32,
    active_turn: Option<Uuid>,
    active_scale_enabled: bool,
    reset_generation: u64,
    theme_is_dark: bool,
}

impl Default for ConversationNavigationRailSnapshot {
    fn default() -> Self {
        Self {
            visible: false,
            turns: Vec::new(),
            viewport_height: 0.0,
            active_turn: None,
            active_scale_enabled: false,
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

    pub(super) fn render_transcript(
        &self,
        window: &mut Window,
        chat_viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_transcript_rows();
        if self.sync_transcript_layout_width(window) {
            while self.transcript_resize_rx.try_recv().is_ok() {}
        } else {
            self.drain_transcript_resize_events();
        }
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
        let transcript_viewport = TextViewScrollViewport::from_list(&transcript_rows);
        let initial_measurement_pending = !self.transcript_provisional_rows.borrow().is_empty()
            || !self.transcript_exact_measurement_rows.borrow().is_empty();
        let scrollbar_handle = StableListScrollbarHandle::new(
            &transcript_rows,
            &self.transcript_estimated_height,
            &self.transcript_anchor_end_space,
            &self.transcript_anchor_following,
            &self.transcript_drag_estimated_height,
            &self.transcript_is_scrolled,
            initial_measurement_pending,
        );
        let viewport_size = transcript_rows.viewport_bounds().size;
        let transcript_scrollable = viewport_size.height > Pixels::ZERO
            && transcript_rows.max_offset_for_scrollbar().y > px(0.5);
        let navigation_turns = self
            .selected_session()
            .map(|session| {
                transcript_navigation_turns(session, &self.transcript_row_kinds.borrow())
            })
            .unwrap_or_default();
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
            active_scale_enabled: self.navigation_rail_active_scale_enabled.get(),
            reset_generation: self.navigation_rail_reset_generation.get(),
            theme_is_dark: Theme::current(cx).is_dark,
        };
        if self.navigation_rail.read(cx).snapshot != navigation_rail_snapshot {
            self.navigation_rail.update(cx, |rail, cx| {
                rail.set_snapshot(navigation_rail_snapshot, cx)
            });
        }
        let navigation_rail = self.navigation_rail.clone().cached(
            StyleRefinement::default()
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        );
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .child(
                list(transcript_rows, move |index, _window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| {
                            entity.update(cx, |this, cx| {
                                this.transcript_row(index, transcript_viewport, cx)
                            })
                        })
                        .unwrap_or_else(|| div().into_any_element())
                })
                .size_full()
                .pb(anchor_end_space),
            )
            .child(navigation_rail)
            .vertical_scrollbar(&scrollbar_handle)
            .into_any_element()
    }
}

impl Render for ConversationNavigationRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.snapshot.visible {
            return div().into_any_element();
        }
        let theme = Theme::current(cx);
        let turns = self.snapshot.turns.clone();
        let viewport_height = self.snapshot.viewport_height;
        let rail_height = navigation_rail_height(turns.len(), viewport_height);
        let rail_top = (viewport_height - rail_height).max(0.0) / 2.0;
        let focus_handles = turns
            .iter()
            .map(|turn| self.navigation_rail_focus_handle(turn.message_id, window, cx))
            .collect::<Vec<_>>();
        let focused_turn_index = focus_handles
            .iter()
            .position(|focus_handle| focus_handle.is_focused(window));
        let hovered_turn_index = self
            .hovered_turn
            .and_then(|message_id| turns.iter().position(|turn| turn.message_id == message_id));
        let emphasized_turn_index = hovered_turn_index.or(focused_turn_index);
        let active_turn_index = self
            .snapshot
            .active_turn
            .and_then(|message_id| turns.iter().position(|turn| turn.message_id == message_id));
        let scaled_active_turn_index = self
            .snapshot
            .active_scale_enabled
            .then_some(active_turn_index)
            .flatten();
        let visual_state = NavigationRailVisualState {
            active_turn: scaled_active_turn_index.map(|index| turns[index].message_id),
            emphasized_turn: emphasized_turn_index.map(|index| turns[index].message_id),
        };
        let previous_visual_state = self.visual_state;
        if previous_visual_state != visual_state {
            self.transition_from = previous_visual_state;
            self.visual_state = visual_state;
            self.animation_generation = self.animation_generation.wrapping_add(1);
        }
        let transition_from = self.transition_from;
        let index_for_message = |message_id: Option<Uuid>| {
            message_id
                .and_then(|message_id| turns.iter().position(|turn| turn.message_id == message_id))
        };
        let from_active_turn_index = index_for_message(transition_from.active_turn);
        let from_emphasized_turn_index = index_for_message(transition_from.emphasized_turn);
        let animation_generation = self.animation_generation;

        let tick_rows = turns
            .iter()
            .zip(focus_handles)
            .enumerate()
            .map(|(turn_index, (turn, focus_handle))| {
                let from_width = NAVIGATION_RAIL_TICK_WIDTH
                    * navigation_rail_scale(
                        turn_index,
                        from_active_turn_index,
                        from_emphasized_turn_index,
                    );
                let to_width = NAVIGATION_RAIL_TICK_WIDTH
                    * navigation_rail_scale(
                        turn_index,
                        scaled_active_turn_index,
                        emphasized_turn_index,
                    );
                let prominent = active_turn_index == Some(turn_index)
                    || emphasized_turn_index == Some(turn_index);
                let tick_color = if prominent {
                    if theme.is_dark {
                        rgb(0xFFFFFF).into()
                    } else {
                        theme.text
                    }
                } else {
                    theme.text_ghost.opacity(NAVIGATION_RAIL_INACTIVE_OPACITY)
                };
                let message_id = turn.message_id;
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
                            .tab_index(turn_index as isize)
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

        let preview = emphasized_turn_index.map(|turn_index| {
            let turn = &turns[turn_index];
            let hit_height = rail_height / turns.len() as f32;
            let preview_height = 126.0;
            let max_preview_top = (viewport_height - preview_height - 12.0).max(12.0);
            let preview_top = (rail_top + (turn_index as f32 + 0.5) * hit_height
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
        let turns = &self.snapshot.turns;
        let Some(turn_index) = turns.iter().position(|turn| turn.message_id == message_id) else {
            return;
        };

        let target_index = match event.keystroke.key.as_str() {
            "up" => Some(turn_index.saturating_sub(1)),
            "down" => Some((turn_index + 1).min(turns.len() - 1)),
            "home" => Some(0),
            "end" => Some(turns.len() - 1),
            "enter" | "space" => {
                self.activate_turn(message_id, cx);
                cx.stop_propagation();
                return;
            }
            _ => None,
        };
        let Some(target_index) = target_index else {
            return;
        };
        if let Some(focus_handle) = self
            .focus_handles
            .get(&turns[target_index].message_id)
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
        let row_index = self.selected_session().and_then(|session| {
            transcript_navigation_turns(session, &self.transcript_row_kinds.borrow())
                .into_iter()
                .find(|turn| turn.message_id == message_id)
                .map(|turn| turn.row_index)
        });
        let Some(row_index) = row_index else {
            return;
        };

        self.transcript_anchor_following.set(false);
        self.navigation_rail_active_scale_enabled.set(true);
        self.active_transcript_rows().scroll_to(ListOffset {
            item_ix: row_index,
            offset_in_item: Pixels::ZERO,
        });
        self.transcript_is_scrolled.set(true);
        cx.notify();
    }

    /// The provider's latest ordered block is still reasoning.
    pub(super) fn reasoning_live(&self) -> bool {
        self.selected_runtime()
            .is_some_and(|runtime| runtime.stream_phase == Some(StreamPhase::Reasoning))
            && self
                .selected_session()
                .is_some_and(|session| session.status == SessionStatus::Working)
    }

    pub(super) fn toggle_reasoning(
        &mut self,
        block_index: usize,
        current: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        self.reasoning_expanded.insert(block_index, !current);
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    pub(super) fn toggle_activities(
        &mut self,
        block_index: usize,
        current: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        self.activities_expanded.insert(block_index, !current);
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    pub(super) fn toggle_activity_item(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.pin_transcript_for_disclosure();
        if !self.expanded_activity_items.remove(&id) {
            self.expanded_activity_items.insert(id);
        }
        if let Some(block_index) = self.selected_transcript_blocks().iter().position(|block| {
            matches!(
                &block.content,
                TranscriptBlockContent::Activities(activities)
                    if activities.iter().any(|activity| activity.id == id)
            )
        }) {
            self.remeasure_transcript_block(block_index);
        }
        cx.notify();
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
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .map(|project| project.path.as_path())?;
        if !checkpoint::has_ref(
            project_path,
            &checkpoint::checkpoint_ref(session.id, retained_turn_count),
        ) {
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
        Some(AssistantMessageAction {
            session_id: session.id,
            turn_count: turn.turn_count,
        })
    }

    pub(super) fn transcript_row(
        &mut self,
        index: usize,
        transcript_viewport: TextViewScrollViewport,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.transcript_provisional_rows.borrow_mut().remove(&index) {
            // Keep the scrollbar suppressed for the pass that replaces this
            // estimated-height row with its exact content. The following
            // render can then trust ListState's measured scroll range.
            self.transcript_exact_measurement_rows
                .borrow_mut()
                .insert(index);
            cx.notify();
            let estimated_height = self
                .transcript_row_estimates
                .borrow()
                .get(index)
                .copied()
                .unwrap_or(px(44.0));
            return div()
                .w_full()
                .h(estimated_height)
                .flex_none()
                .into_any_element();
        }
        if self
            .transcript_exact_measurement_rows
            .borrow_mut()
            .remove(&index)
        {
            // This render replaces the provisional element. Schedule one more
            // pass so the anchor reservation reads the exact post-layout row
            // bounds instead of leaving the estimate in place indefinitely.
            cx.notify();
        }

        let theme = Theme::current(cx);
        let composer = self.composer.clone();
        let waku = cx.entity().downgrade();
        let row_count = self.transcript_row_count();
        let kind = self
            .transcript_row_kinds
            .borrow()
            .get(index)
            .copied()
            .unwrap_or(TranscriptRowKind::Message(index));
        let starts_followup_turn = match kind {
            TranscriptRowKind::Message(message_index) => {
                self.selected_session().is_some_and(|session| {
                    message_starts_followup_turn(&session.messages, message_index)
                })
            }
            TranscriptRowKind::TurnBlock(_) | TranscriptRowKind::TurnFold(_) => false,
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
                    let text_state = self
                        .message_text_states
                        .entry(message.id)
                        .or_insert_with(|| cx.new(TextViewState::new))
                        .clone();
                    render_message(
                        &theme,
                        &message,
                        assistant_footer_copy_content,
                        assistant_footer_time,
                        copied,
                        assistant_message_action,
                        user_message_action,
                        message_edit_input,
                        self.state.selected_session.unwrap_or_default(),
                        self.transcript_resize_tx.clone(),
                        self.transcript_layout_width.get(),
                        self.active_transcript_rows().clone(),
                        transcript_viewport,
                        text_state,
                        waku,
                        composer,
                        cx,
                    )
                })
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| match &block.content {
                    TranscriptBlockContent::Reasoning(reasoning) => {
                        self.render_reasoning_row(reasoning, block_index, &theme, cx)
                    }
                    TranscriptBlockContent::Activities(activities) => self.render_activities_row(
                        activities,
                        block_index,
                        &theme,
                        transcript_viewport,
                        cx,
                    ),
                })
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnFold(turn_id) => self.render_turn_fold_row(turn_id, &theme, cx),
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
            .unwrap_or_else(|| "Worked".into());
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

    /// The turn's reasoning as a disclosure: open while the provider is
    /// thinking, collapsing to "Thought for Ns" once the answer starts, and
    /// clickable either way.
    pub(super) fn render_reasoning_row(
        &self,
        reasoning: &ReasoningBlock,
        block_index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let live =
            self.reasoning_live()
                && self.selected_transcript_blocks().iter().rposition(|block| {
                    matches!(block.content, TranscriptBlockContent::Reasoning(_))
                }) == Some(block_index);
        let expanded = self
            .reasoning_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(live);
        let label = if live {
            "Thinking".to_owned()
        } else {
            format!(
                "Thought for {}s",
                reasoning
                    .finished_at_ms
                    .saturating_sub(reasoning.started_at_ms)
                    .div_ceil(1000)
                    .max(1)
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .id(SharedString::from(format!("thinking-toggle-{block_index}")))
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .cursor_default()
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        9.0,
                        theme.text_ghost,
                    ))
                    .child(if live {
                        icon("icons/sparkle.svg", 11.0, theme.text_tertiary)
                            .with_animation(
                                SharedString::from(format!("thinking-pulse-{block_index}")),
                                Animation::new(Duration::from_millis(1800))
                                    .repeat()
                                    .with_easing(pulsating_between(0.4, 1.0)),
                                |element, delta| element.opacity(delta),
                            )
                            .into_any_element()
                    } else {
                        icon("icons/sparkle.svg", 11.0, theme.text_ghost).into_any_element()
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(label)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_reasoning(block_index, expanded, cx);
                    })),
            )
            .when(expanded, |element| {
                element.child(
                    div()
                        .pl(px(15.0))
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(theme.text_tertiary)
                        .whitespace_normal()
                        .child(SharedString::from(reasoning.content.clone())),
                )
            })
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
        transcript_viewport: TextViewScrollViewport,
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
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        9.0,
                        theme.text_ghost,
                    ))
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
            let sections = activity_disclosure_sections(activity);
            let preview = activity_preview(activity);
            let has_detail = !sections.is_empty();
            let item_expanded = has_detail && self.expanded_activity_items.contains(&id);
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
                    .child(if has_detail {
                        icon(
                            if item_expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            },
                            9.0,
                            theme.text_ghost,
                        )
                        .into_any_element()
                    } else {
                        div().w(px(9.0)).flex_none().into_any_element()
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
                            .truncate()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(activity_display_title(activity))),
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
                    .child(if activity.failed {
                        icon("icons/x.svg", 10.0, theme.danger).into_any_element()
                    } else if activity.complete {
                        icon("icons/check.svg", 10.0, theme.text_ghost).into_any_element()
                    } else {
                        pulse_dot(format!("activity-pulse-{id}"), 5.0, theme.accent)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if has_detail {
                            this.toggle_activity_item(id, cx);
                        }
                    })),
            );
            if item_expanded {
                let transcript_rows = self.active_transcript_rows().clone();
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
                    .font_family("SF Mono")
                    .text_size(px(10.5))
                    .line_height(px(16.0))
                    .text_color(theme.text_secondary)
                    .whitespace_normal()
                    .overflow_hidden();
                for section in sections {
                    let section_kind = section.kind;
                    let content = section.content;
                    let text_state = {
                        let mut states = self.activity_text_states.borrow_mut();
                        states
                            .entry((id, section_kind))
                            .or_insert_with(|| cx.new(TextViewState::new))
                            .clone()
                    };
                    let mut section_view = div().w_full().min_w_0().flex().flex_col().gap(px(3.0));
                    if let Some(label) = section_kind.label() {
                        let copy_content = content.clone();
                        let copied = self
                            .copied_activity_feedback
                            .contains_key(&(id, section_kind));
                        let copy_waku = cx.entity().downgrade();
                        let copy_tooltip = SharedString::from(if copied {
                            "Copied".to_owned()
                        } else {
                            format!("Copy {}", label.to_ascii_lowercase())
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
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(copy_tooltip.clone()).build(window, cx)
                                            })
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
                            selectable_plain_text(
                                SharedString::from(format!(
                                    "activity-detail-{}-{}",
                                    id,
                                    section_kind.id()
                                )),
                                &content,
                                text_state,
                                cx,
                            )
                            .selection_scroll_handle(&transcript_rows)
                            .block_viewport(transcript_viewport)
                            .font_family("SF Mono")
                            .text_size(px(10.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_secondary),
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

fn render_activity_image(image_url: &str, activity_id: Uuid, image_index: usize) -> AnyElement {
    let element = match decode_activity_image(image_url) {
        Some(image) => img(image),
        None => img(image_url.to_owned()),
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

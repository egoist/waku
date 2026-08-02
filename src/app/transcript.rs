use super::*;

impl Waku {
    /// One list row per message plus each ordered non-message turn block.
    pub(super) fn transcript_row_count(&self) -> usize {
        self.selected_transcript_row_kinds().len()
    }

    pub(super) fn selected_transcript_row_kinds(&self) -> Vec<TranscriptRowKind> {
        self.selected_session().map_or_else(Vec::new, |session| {
            folded_transcript_row_kinds(session, &self.expanded_turns)
        })
    }

    pub(super) fn estimated_transcript_row_height(
        &self,
        row_index: usize,
        kind: TranscriptRowKind,
        row_count: usize,
    ) -> Pixels {
        let inner_height = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| {
                    let message = session.messages.get(message_index)?;
                    let message_footer_visible = match message.role {
                        MessageRole::User => true,
                        MessageRole::Assistant => {
                            assistant_response_footer_index(session, message_index)
                                == Some(message_index)
                        }
                        MessageRole::System => false,
                    };
                    Some(estimated_message_height(
                        message,
                        self.transcript_layout_width.get(),
                        message_footer_visible,
                    ))
                })
                .unwrap_or(px(36.0)),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| match &block.content {
                    TranscriptBlockContent::Reasoning(reasoning) => {
                        let live = self.reasoning_live()
                            && self.selected_transcript_blocks().iter().rposition(|block| {
                                matches!(block.content, TranscriptBlockContent::Reasoning(_))
                            }) == Some(block_index);
                        let expanded = self
                            .reasoning_expanded
                            .get(&block_index)
                            .copied()
                            .unwrap_or(live);
                        px(22.0)
                            + if expanded {
                                px(6.0) + estimated_text_height(&reasoning.content, 88, 18.0)
                            } else {
                                Pixels::ZERO
                            }
                    }
                    TranscriptBlockContent::Activities(activities) => {
                        let live_turn = self
                            .selected_session()
                            .and_then(AgentSession::active_turn_id)
                            .is_some_and(|turn_id| block.turn_id == Some(turn_id));
                        let expanded = self
                            .activities_expanded
                            .get(&block_index)
                            .copied()
                            .unwrap_or(live_turn);
                        if !expanded {
                            px(22.0)
                        } else {
                            activities.iter().fold(px(22.0), |height, activity| {
                                let detail_height =
                                    if self.expanded_activity_items.contains(&activity.id) {
                                        activity_disclosure_text(activity).map_or(
                                            Pixels::ZERO,
                                            |detail| {
                                                px(14.0)
                                                    + estimated_text_height(&detail, 76, 17.0)
                                                    + activity_image_height(activity)
                                            },
                                        )
                                    } else {
                                        Pixels::ZERO
                                    };
                                height + px(24.0) + detail_height
                            })
                        }
                    }
                })
                .unwrap_or(px(22.0)),
            TranscriptRowKind::TurnFold(_) => px(24.0),
        };
        let starts_followup_turn = matches!(kind, TranscriptRowKind::Message(message_index)
            if message_index > 0
                && self.selected_session().and_then(|session| session.messages.get(message_index)).is_some_and(|message| message.role == MessageRole::User));
        inner_height
            + estimated_transcript_row_padding(row_index, row_count, starts_followup_turn)
            + self
                .transcript_row_height_adjustments
                .borrow()
                .get(&kind)
                .copied()
                .unwrap_or(Pixels::ZERO)
    }

    pub(super) fn rebuild_transcript_estimates(&self) {
        let kinds = self.selected_transcript_row_kinds();
        let valid_kinds = kinds.iter().copied().collect::<HashSet<_>>();
        self.transcript_row_height_adjustments
            .borrow_mut()
            .retain(|kind, _| valid_kinds.contains(kind));
        let row_count = kinds.len();
        let estimates = kinds
            .iter()
            .copied()
            .enumerate()
            .map(|(index, kind)| self.estimated_transcript_row_height(index, kind, row_count))
            .collect::<Vec<_>>();
        let total = estimates
            .iter()
            .copied()
            .fold(Pixels::ZERO, |total, height| total + height);
        *self.transcript_row_kinds.borrow_mut() = kinds;
        *self.transcript_row_estimates.borrow_mut() = estimates;
        self.transcript_estimated_height.set(total);
    }

    pub(super) fn append_transcript_estimates(&self, current: usize, count: usize) -> bool {
        let kinds = self.selected_transcript_row_kinds();
        if kinds.len() != count {
            return false;
        }
        {
            let previous = self.transcript_row_kinds.borrow();
            if previous.len() != current || previous.as_slice() != &kinds[..current] {
                return false;
            }
        }

        let mut estimates = self.transcript_row_estimates.borrow_mut();
        if estimates.len() != current {
            return false;
        }
        let mut total = self.transcript_estimated_height.get();

        // The old tail loses its special bottom padding once another row is
        // appended. Recompute that one row, then estimate only the new tail.
        if current > 0 {
            let index = current - 1;
            let next = self.estimated_transcript_row_height(index, kinds[index], count);
            total += next - estimates[index];
            estimates[index] = next;
        }
        for (index, kind) in kinds.iter().copied().enumerate().skip(current) {
            let height = self.estimated_transcript_row_height(index, kind, count);
            estimates.push(height);
            total += height;
        }

        *self.transcript_row_kinds.borrow_mut() = kinds;
        self.transcript_estimated_height
            .set(total.max(Pixels::ZERO));
        true
    }

    pub(super) fn update_transcript_estimates(&self, range: Range<usize>) {
        let kinds = self.transcript_row_kinds.borrow();
        if self.transcript_row_estimates.borrow().len() != kinds.len() {
            drop(kinds);
            self.rebuild_transcript_estimates();
            return;
        }

        let mut estimates = self.transcript_row_estimates.borrow_mut();
        let mut total = self.transcript_estimated_height.get();
        for index in range.start.min(kinds.len())..range.end.min(kinds.len()) {
            let next = self.estimated_transcript_row_height(index, kinds[index], kinds.len());
            total += next - estimates[index];
            estimates[index] = next;
        }
        self.transcript_estimated_height
            .set(total.max(Pixels::ZERO));
    }

    pub(super) fn active_transcript_rows(&self) -> &ListState {
        if self.transcript_anchor.get().is_some() {
            &self.anchored_transcript_rows
        } else {
            &self.transcript_rows
        }
    }

    /// Turn a tail-pinned list into an explicit scroll position before a
    /// disclosure changes the document height. Otherwise GPUI keeps the
    /// bottom edge fixed and makes the disclosure header jump upward while
    /// its newly visible content is inserted.
    pub(super) fn pin_transcript_for_disclosure(&self) {
        self.sync_transcript_rows();
        let transcript_rows = self.active_transcript_rows();
        let count = transcript_rows.item_count();
        let scroll_top = transcript_rows.logical_scroll_top();

        if scroll_top.item_ix >= count && count > 0 {
            let viewport_height = transcript_rows.viewport_bounds().size.height;
            let actual_max = transcript_rows.max_offset_for_scrollbar().y;
            if actual_max > px(0.5) {
                // GPUI represents the exact bottom as an implicit tail anchor.
                // Resolve the corresponding item just above the bottom, then
                // restore the final half pixel with scroll_to so the same
                // position remains explicit while rows below it grow.
                transcript_rows
                    .set_offset_from_scrollbar(point(Pixels::ZERO, -(actual_max - px(0.5))));
                let mut explicit_bottom = transcript_rows.logical_scroll_top();
                explicit_bottom.offset_in_item += px(0.5);
                transcript_rows.scroll_to(explicit_bottom);
            } else if viewport_height > Pixels::ZERO {
                // A short bottom-aligned transcript has leading empty space.
                // A negative item offset preserves that space so expanding a
                // row still grows downward from its current screen position.
                // `scroll_px_offset_for_scrollbar` is zero for a short list in
                // Zed's GPUI, so derive the actual content height from its
                // rendered row bounds instead of treating the list as empty.
                let measured_content_height = transcript_rows
                    .bounds_for_item(0)
                    .zip(transcript_rows.bounds_for_item(count - 1))
                    .map(|(first, last)| (last.bottom() - first.top()).max(Pixels::ZERO))
                    .unwrap_or_else(|| self.transcript_estimated_height.get());
                let leading_space = (viewport_height - measured_content_height).max(Pixels::ZERO);
                transcript_rows.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: -leading_space,
                });
            }
        }

        self.transcript_anchor_following.set(false);
        self.transcript_is_scrolled.set(true);
    }

    /// Bulk-reset the transcript through cheap height-only rows. This is for
    /// session/document replacement, never for an in-place disclosure toggle.
    pub(super) fn reset_transcript_rows_with_placeholders(&self, count: usize) {
        // Row keys contain indices, so dynamic corrections from another
        // session must never bleed into the newly selected transcript.
        self.transcript_row_height_adjustments.borrow_mut().clear();
        self.rebuild_transcript_estimates();
        *self.transcript_provisional_rows.borrow_mut() = (0..count).collect();
        self.transcript_exact_measurement_rows.borrow_mut().clear();
        let _ = self.transcript_rows.clone().measure_all();
        self.transcript_is_scrolled.set(false);
        self.transcript_rows.reset(count);
        let _ = self.anchored_transcript_rows.clone().measure_all();
        self.anchored_transcript_rows.reset(count);
    }

    /// Recover an unexpected local row-count mismatch without hiding content.
    /// This can cost more than a placeholder reset, but it is the safe fallback
    /// for message and transcript features running inside the current document.
    fn reset_transcript_rows_exact(&self, count: usize) {
        self.rebuild_transcript_estimates();
        self.transcript_provisional_rows.borrow_mut().clear();
        *self.transcript_exact_measurement_rows.borrow_mut() = (0..count).collect();
        self.transcript_rows.reset(count);
        self.anchored_transcript_rows.reset(count);
    }

    /// Apply a local disclosure change without replacing unchanged transcript
    /// rows. A full reset intentionally renders height-only placeholders for
    /// one pass; using it for a turn fold makes the surrounding markdown flash
    /// blank even though those messages did not change.
    pub(super) fn splice_transcript_rows_after_visibility_change(
        &self,
        previous_kinds: &[TranscriptRowKind],
    ) {
        let next_kinds = self.selected_transcript_row_kinds();
        let splice = transcript_row_splice(previous_kinds, &next_kinds);

        self.rebuild_transcript_estimates();
        apply_transcript_visibility_splice(
            [&self.transcript_rows, &self.anchored_transcript_rows],
            previous_kinds.len(),
            next_kinds.len(),
            splice,
            &self.transcript_provisional_rows,
            &self.transcript_exact_measurement_rows,
        );
    }

    pub(super) fn selected_transcript_anchor_row(&self) -> Option<usize> {
        let anchor = self.transcript_anchor.get()?;
        let session = self.selected_session()?;
        if session.id != anchor.session_id {
            return None;
        }
        let message_index = session.messages.iter().position(|message| {
            message.role == MessageRole::User && message.turn_id == Some(anchor.turn_id)
        })?;
        self.transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::Message(message_index))
    }

    pub(super) fn scroll_transcript_to_anchor(&self) {
        let Some(item_ix) = self.selected_transcript_anchor_row() else {
            return;
        };
        self.active_transcript_rows().scroll_to(ListOffset {
            item_ix,
            offset_in_item: Pixels::ZERO,
        });
        self.transcript_is_scrolled.set(true);
    }

    pub(super) fn update_transcript_anchor_end_space(&self, window: &Window) -> Pixels {
        let Some(anchor_row) = self.selected_transcript_anchor_row() else {
            self.transcript_anchor_end_space.set(Pixels::ZERO);
            self.transcript_anchor_following.set(false);
            return Pixels::ZERO;
        };

        let viewport_height = {
            let measured = self.active_transcript_rows().viewport_bounds().size.height;
            if measured > Pixels::ZERO {
                measured
            } else {
                // The first sent message replaces the empty state, so the list
                // has no prior bounds yet. The full window is a conservative
                // first-frame fallback that still guarantees a top anchor.
                window.viewport_size().height
            }
        };
        let estimated_tail_height = self
            .transcript_row_estimates
            .borrow()
            .iter()
            .skip(anchor_row)
            .copied()
            .fold(Pixels::ZERO, |height, row| height + row);
        let transcript_rows = self.active_transcript_rows();
        let last_row = transcript_rows.item_count().checked_sub(1);
        let measured_tail_height = last_row.and_then(|last_row| {
            let anchor = transcript_rows.bounds_for_item(anchor_row)?;
            let last = transcript_rows.bounds_for_item(last_row)?;
            Some((last.bottom() - anchor.top()).max(Pixels::ZERO))
        });
        let tail_is_unmeasured = self
            .transcript_provisional_rows
            .borrow()
            .iter()
            .any(|row| *row >= anchor_row)
            || self
                .transcript_exact_measurement_rows
                .borrow()
                .iter()
                .any(|row| *row >= anchor_row);
        let anchored_tail_height = if tail_is_unmeasured {
            // Bounds still describe the element that was just invalidated.
            // The estimate already reflects the requested expanded/collapsed
            // state, so use it to prevent a one-frame underfill on collapse.
            estimated_tail_height
        } else {
            measured_tail_height.unwrap_or(estimated_tail_height)
        };
        let end_space = stabilized_transcript_anchor_end_space(
            viewport_height,
            anchored_tail_height,
            self.transcript_anchor_end_space.get(),
            tail_is_unmeasured,
        );
        self.transcript_anchor_end_space.set(end_space);
        if maintain_transcript_anchor(
            transcript_rows,
            anchor_row,
            self.transcript_anchor_following.get(),
            end_space,
        ) {
            self.transcript_is_scrolled.set(true);
        }
        end_space
    }

    /// Remeasure rows whose content changed in place. This path always renders
    /// the real row on its next pass so message features cannot flash blank.
    pub(super) fn remeasure_transcript_rows(&self, range: Range<usize>, anchor_delta: Pixels) {
        self.splice_remeasured_transcript_rows(range, anchor_delta, false);
    }

    /// Bulk width reflow may use cheap height-only rows while every transcript
    /// row is being remeasured. Never use this for a local content interaction.
    fn reflow_transcript_rows_with_placeholders(&self, range: Range<usize>, anchor_delta: Pixels) {
        self.splice_remeasured_transcript_rows(range, anchor_delta, true);
    }

    fn splice_remeasured_transcript_rows(
        &self,
        range: Range<usize>,
        anchor_delta: Pixels,
        use_placeholders: bool,
    ) {
        let transcript_rows = self.active_transcript_rows();
        let count = transcript_rows.item_count();
        let range = range.start.min(count)..range.end.min(count);
        if range.is_empty() {
            return;
        }

        let preserve_scroll = self.transcript_is_scrolled.get();
        let previous_scroll_top = preserve_scroll.then(|| transcript_rows.logical_scroll_top());
        prepare_transcript_row_remeasurement(
            &self.transcript_provisional_rows,
            &self.transcript_exact_measurement_rows,
            range.clone(),
            use_placeholders,
        );
        if use_placeholders {
            let _ = transcript_rows.clone().measure_all();
        }
        transcript_rows.splice(range.clone(), range.len());

        if let Some(scroll_top) = previous_scroll_top.and_then(|scroll_top| {
            scroll_top_after_row_invalidation(scroll_top, range, anchor_delta)
        }) {
            transcript_rows.scroll_to(scroll_top);
        }
    }

    pub(super) fn sync_transcript_layout_width(&self, window: &Window) -> bool {
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let sidebar_width = px(sidebar_width);
        let right_panel_width = px(right_panel_width);
        let content_width =
            (window.viewport_size().width - sidebar_width - right_panel_width - px(40.0))
                .clamp(px(1.0), px(CONTENT_MAX_WIDTH));
        let previous = self.transcript_layout_width.replace(content_width);
        if previous > Pixels::ZERO && (previous - content_width).abs() < px(1.0) {
            return false;
        }

        // A width change is a real document reflow. Rebase the stable estimate
        // for the new wrap width and let GPUI bulk-measure cheap placeholders
        // before it lays out the visible rows exactly.
        self.rebuild_transcript_estimates();
        let count = self.active_transcript_rows().item_count();
        self.reflow_transcript_rows_with_placeholders(0..count, Pixels::ZERO);
        true
    }

    pub(super) fn drain_transcript_resize_events(&self) {
        let Some(session_id) = self.state.selected_session else {
            while self.transcript_resize_rx.try_recv().is_ok() {}
            return;
        };
        let mut by_row = HashMap::<usize, (TranscriptRowKind, Pixels, Pixels)>::new();

        while let Ok(event) = self.transcript_resize_rx.try_recv() {
            if event.session_id != session_id {
                continue;
            }
            let Some(message_index) = self.selected_session().and_then(|session| {
                session
                    .messages
                    .iter()
                    .position(|message| message.id == event.message_id)
            }) else {
                continue;
            };
            let kind = TranscriptRowKind::Message(message_index);
            let Some(row_index) = self
                .transcript_row_kinds
                .borrow()
                .iter()
                .position(|candidate| *candidate == kind)
            else {
                continue;
            };
            let entry = by_row
                .entry(row_index)
                .or_insert((kind, Pixels::ZERO, Pixels::ZERO));
            entry.1 += event.delta;
            entry.2 += event.anchor_delta;
        }

        for (row_index, (kind, delta, anchor_delta)) in by_row {
            *self
                .transcript_row_height_adjustments
                .borrow_mut()
                .entry(kind)
                .or_insert(Pixels::ZERO) += delta;
            let row_count = self.transcript_row_kinds.borrow().len();
            let next_estimate = self
                .estimated_transcript_row_height(row_index, kind, row_count)
                .max(px(1.0));
            let applied_delta = {
                let mut estimates = self.transcript_row_estimates.borrow_mut();
                let Some(current) = estimates.get_mut(row_index) else {
                    continue;
                };
                let applied_delta = next_estimate - *current;
                *current = next_estimate;
                applied_delta
            };
            self.transcript_estimated_height
                .set((self.transcript_estimated_height.get() + applied_delta).max(Pixels::ZERO));
            self.remeasure_transcript_rows(row_index..row_index + 1, anchor_delta);
        }
    }

    /// Keep the list's row count in sync with the transcript. Appends keep
    /// the reader's place (or the pinned tail); shrinking resets the view.
    pub(super) fn sync_transcript_rows(&self) {
        let count = self.transcript_row_count();
        let transcript_rows = self.active_transcript_rows();
        let current = transcript_rows.item_count();
        if count > current {
            if !self.append_transcript_estimates(current, count) {
                self.reset_transcript_rows_exact(count);
                return;
            }
            // Appended prompt/stream rows are a local update and must render
            // immediately; the stable scrollbar already uses our estimates.
            prepare_transcript_row_remeasurement(
                &self.transcript_provisional_rows,
                &self.transcript_exact_measurement_rows,
                current..count,
                false,
            );
            transcript_rows.splice(current..current, count - current);
        } else if count < current {
            self.reset_transcript_rows_exact(count);
        }
    }

    /// Provider events arrive in transcript order, so only the active tail can
    /// change height. Keeping earlier row measurements intact is critical
    /// for responsive scrolling while a long answer is still growing.
    pub(super) fn remeasure_transcript_tail(&self) {
        self.sync_transcript_rows();
        let count = self.active_transcript_rows().item_count();
        let from = count.saturating_sub(STREAM_REMEASURE_TAIL_ROWS);
        if from < count {
            self.update_transcript_estimates(from..count);
            self.remeasure_transcript_rows(from..count, Pixels::ZERO);
        }
    }

    pub(super) fn remeasure_transcript_block(&self, block_index: usize) {
        self.sync_transcript_rows();
        let row_index = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::TurnBlock(block_index));
        if let Some(row_index) = row_index {
            self.update_transcript_estimates(row_index..row_index + 1);
            self.remeasure_transcript_rows(row_index..row_index + 1, Pixels::ZERO);
        }
    }

    pub(super) fn remeasure_transcript_message(&self, message_index: usize) {
        self.sync_transcript_rows();
        let row_index = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::Message(message_index));
        if let Some(row_index) = row_index {
            self.update_transcript_estimates(row_index..row_index + 1);
            self.remeasure_transcript_rows(row_index..row_index + 1, Pixels::ZERO);
        }
    }
}

// ── Shared pieces ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum TranscriptRowKind {
    Message(usize),
    TurnBlock(usize),
    TurnFold(Uuid),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TranscriptNavigationTurn {
    pub message_id: Uuid,
    pub message_index: usize,
    pub row_index: usize,
    pub prompt: String,
    pub response: String,
}

pub(super) fn transcript_navigation_turns(
    session: &AgentSession,
    row_kinds: &[TranscriptRowKind],
) -> Vec<TranscriptNavigationTurn> {
    let user_message_indexes = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::User).then_some(index))
        .collect::<Vec<_>>();

    user_message_indexes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(turn_index, message_index)| {
            let message = session.messages.get(message_index)?;
            let row_index = row_kinds
                .iter()
                .position(|kind| *kind == TranscriptRowKind::Message(message_index))?;
            let next_user_index = user_message_indexes
                .get(turn_index + 1)
                .copied()
                .unwrap_or(session.messages.len());
            let turn_running = message.turn_id.is_some_and(|turn_id| {
                session
                    .turns
                    .iter()
                    .any(|turn| turn.id == turn_id && turn.status == TurnStatus::Running)
            });
            let response = (!turn_running)
                .then(|| {
                    session.messages[message_index + 1..next_user_index]
                        .iter()
                        .rev()
                        .find(|candidate| {
                            candidate.role == MessageRole::Assistant
                                && !candidate.content.trim().is_empty()
                        })
                        .map(|candidate| navigation_preview_snippet(&candidate.content, 240))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            Some(TranscriptNavigationTurn {
                message_id: message.id,
                message_index,
                row_index,
                prompt: navigation_preview_snippet(&message.content, 100),
                response,
            })
        })
        .collect()
}

pub(super) fn navigation_preview_snippet(content: &str, max_graphemes: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut graphemes = normalized.graphemes(true);
    let snippet = graphemes.by_ref().take(max_graphemes).collect::<String>();
    if graphemes.next().is_some() {
        format!("{snippet}…")
    } else {
        snippet
    }
}

pub(super) fn active_navigation_turn_index(
    turn_rows: &[usize],
    scroll_top_row: usize,
    at_transcript_end: bool,
) -> Option<usize> {
    if turn_rows.is_empty() {
        return None;
    }
    if at_transcript_end {
        return Some(turn_rows.len() - 1);
    }
    Some(
        turn_rows
            .partition_point(|row| *row <= scroll_top_row)
            .saturating_sub(1),
    )
}

pub(super) fn navigation_rail_scale(
    turn_index: usize,
    active_turn_index: Option<usize>,
    emphasized_turn_index: Option<usize>,
) -> f32 {
    let active_scale: f32 = if active_turn_index == Some(turn_index) {
        0.50
    } else {
        0.25
    };
    let emphasis_scale =
        emphasized_turn_index.map_or(0.25, |emphasized| match turn_index.abs_diff(emphasized) {
            0 => 1.0,
            1 => 0.68,
            2 => 0.44,
            _ => 0.25,
        });
    active_scale.max(emphasis_scale)
}

pub(super) fn navigation_rail_height(turn_count: usize, viewport_height: f32) -> f32 {
    (turn_count as f32 * NAVIGATION_RAIL_TURN_HEIGHT)
        .min(viewport_height * NAVIGATION_RAIL_VIEWPORT_HEIGHT_RATIO)
}

pub(super) fn should_show_navigation_rail(
    transcript_scrollable: bool,
    turn_count: usize,
    chat_viewport_width: f32,
) -> bool {
    let content_left = ((chat_viewport_width - CONTENT_MAX_WIDTH) / 2.0).max(20.0);
    let rail_right = NAVIGATION_RAIL_LEFT + NAVIGATION_RAIL_WIDTH;
    transcript_scrollable
        && turn_count >= 2
        && content_left >= rail_right + NAVIGATION_RAIL_CONTENT_GAP
}

/// A provider can split one assistant response into several ordered text
/// messages around reasoning and tool activity. The response footer belongs
/// only to the terminal text part, once the turn has settled.
pub(super) fn assistant_response_footer_index(
    session: &AgentSession,
    message_index: usize,
) -> Option<usize> {
    let message = session.messages.get(message_index)?;
    if message.role != MessageRole::Assistant || message.streaming {
        return None;
    }
    let Some(turn_id) = message.turn_id else {
        return Some(message_index);
    };
    if session
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .is_some_and(|turn| turn.status == TurnStatus::Running)
    {
        return None;
    }
    session.messages.iter().rposition(|candidate| {
        candidate.role == MessageRole::Assistant && candidate.turn_id == Some(turn_id)
    })
}

pub(super) fn assistant_response_footer(
    session: &AgentSession,
    message_index: usize,
) -> Option<String> {
    if assistant_response_footer_index(session, message_index) != Some(message_index) {
        return None;
    }
    let message = &session.messages[message_index];
    let Some(turn_id) = message.turn_id else {
        return Some(message.content.clone());
    };
    Some(
        session
            .messages
            .iter()
            .filter(|candidate| {
                candidate.role == MessageRole::Assistant
                    && candidate.turn_id == Some(turn_id)
                    && !candidate.content.trim().is_empty()
            })
            .map(|candidate| candidate.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

pub(super) fn assistant_response_footer_time(
    session: &AgentSession,
    message_index: usize,
) -> Option<u64> {
    if assistant_response_footer_index(session, message_index) != Some(message_index) {
        return None;
    }
    let message = &session.messages[message_index];
    let completed_at = message.turn_id.and_then(|turn_id| {
        session
            .turns
            .iter()
            .find(|turn| turn.id == turn_id)
            .and_then(|turn| turn.completed_at)
    });
    Some(completed_at.unwrap_or(message.created_at))
}

pub(super) fn transcript_row_splice(
    previous: &[TranscriptRowKind],
    next: &[TranscriptRowKind],
) -> Option<(Range<usize>, usize)> {
    let prefix = previous
        .iter()
        .zip(next)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = previous[prefix..]
        .iter()
        .rev()
        .zip(next[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_end = previous.len() - suffix;
    let new_count = next.len() - prefix - suffix;

    (prefix != old_end || new_count != 0).then_some((prefix..old_end, new_count))
}

pub(super) fn apply_transcript_visibility_splice(
    row_lists: [&ListState; 2],
    previous_count: usize,
    next_count: usize,
    splice: Option<(Range<usize>, usize)>,
    provisional_rows: &RefCell<HashSet<usize>>,
    exact_measurement_rows: &RefCell<HashSet<usize>>,
) {
    // A disclosure update must render its newly visible rows immediately. In
    // particular, never carry the bulk-reset placeholders into this path.
    provisional_rows.borrow_mut().clear();
    exact_measurement_rows.borrow_mut().clear();

    let Some((old_range, new_count)) = splice else {
        return;
    };
    exact_measurement_rows
        .borrow_mut()
        .extend(old_range.start..old_range.start + new_count);
    for rows in row_lists {
        if rows.item_count() == previous_count {
            rows.splice(old_range.clone(), new_count);
        } else {
            // The inactive list can be stale after an alignment switch. Keep
            // its count valid without introducing blank placeholders.
            rows.reset(next_count);
        }
    }
}

pub(super) fn prepare_transcript_row_remeasurement(
    provisional_rows: &RefCell<HashSet<usize>>,
    exact_measurement_rows: &RefCell<HashSet<usize>>,
    range: Range<usize>,
    use_placeholders: bool,
) {
    let mut provisional_rows = provisional_rows.borrow_mut();
    if use_placeholders {
        exact_measurement_rows
            .borrow_mut()
            .retain(|row| !range.contains(row));
        provisional_rows.extend(range);
    } else {
        provisional_rows.retain(|row| !range.contains(row));
        exact_measurement_rows.borrow_mut().extend(range);
    }
}

pub(super) fn transcript_anchor_end_space(
    viewport_height: Pixels,
    anchored_tail_height: Pixels,
) -> Pixels {
    (viewport_height - anchored_tail_height).max(Pixels::ZERO)
}

pub(super) fn stabilized_transcript_anchor_end_space(
    viewport_height: Pixels,
    anchored_tail_height: Pixels,
    previous_end_space: Pixels,
    tail_is_unmeasured: bool,
) -> Pixels {
    let candidate = transcript_anchor_end_space(viewport_height, anchored_tail_height);
    if tail_is_unmeasured && previous_end_space > Pixels::ZERO {
        // Expansion needs the old (larger) spacer until its exact height is
        // known; collapse needs the new estimated (larger) spacer immediately.
        // Taking the maximum prevents GPUI's bottom-aligned list from ever
        // seeing an underfilled frame in either direction.
        previous_end_space.max(candidate)
    } else {
        candidate
    }
}

pub(super) fn maintain_transcript_anchor(
    transcript_rows: &ListState,
    anchor_row: usize,
    anchor_following: bool,
    end_space: Pixels,
) -> bool {
    if !anchor_following || end_space <= Pixels::ZERO {
        return false;
    }

    // A bottom-aligned GPUI list represents its pinned tail as no logical
    // scroll offset. While a response row is being remeasured, the retained
    // end spacer and the newly expanded content briefly overflow together;
    // without an explicit item offset that overflow is taken from the top of
    // the user row. Reassert the turn anchor in the same layout pass.
    transcript_rows.scroll_to(ListOffset {
        item_ix: anchor_row,
        offset_in_item: Pixels::ZERO,
    });
    true
}

pub(super) fn estimated_text_height(
    text: &str,
    characters_per_line: usize,
    line_height: f32,
) -> Pixels {
    let visual_lines = text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                0.35
            } else {
                line.chars().count().max(1).div_ceil(characters_per_line) as f32
            }
        })
        .sum::<f32>()
        .max(1.0);
    px(visual_lines * line_height)
}

pub(super) const ACTIVITY_IMAGE_WIDTH: f32 = 300.0;
pub(super) const ACTIVITY_IMAGE_HEIGHT: f32 = 200.0;

pub(super) fn activity_image_height(activity: &ActivityItem) -> Pixels {
    px(ACTIVITY_IMAGE_HEIGHT * activity.image_urls.len() as f32)
}

pub(super) fn markdown_estimation_source(text: &str) -> (String, Pixels) {
    const DEFAULT_IMAGE_HEIGHT: f32 = 160.0;
    // Image sources can be multi-megabyte data URLs. The visible estimate is
    // usually much smaller, so do not duplicate that capacity or allocate a
    // second lowercase copy of the entire response.
    let mut visible = String::with_capacity(text.len().min(16 * 1024));
    let mut media_height = Pixels::ZERO;
    let mut offset = 0;

    while offset < text.len() {
        let remainder = &text[offset..];
        if starts_html_tag(remainder, "<details")
            && let Some(open_end) = remainder.find('>')
            && let Some((body_end, details_end)) = matching_details_end(remainder, open_end + 1)
        {
            let opening_tag = &remainder[..=open_end];
            let body = &remainder[open_end + 1..body_end];
            let source = if html_has_attribute(opening_tag, "open") {
                body
            } else if let Some(summary_start) = find_html_tag(body, "<summary") {
                let summary = &body[summary_start..];
                if let Some(tag_end) = summary.find('>') {
                    let content = &summary[tag_end + 1..];
                    find_ascii_case_insensitive(content, "</summary>")
                        .map_or("Details", |end| &content[..end])
                } else {
                    "Details"
                }
            } else {
                "Details"
            };
            let (details_text, details_media) = markdown_estimation_source(source);
            visible.push_str(&details_text);
            visible.push('\n');
            media_height += details_media;
            offset += details_end;
            continue;
        }
        if let Some(after_alt) = remainder.strip_prefix("![")
            && let Some(label_end) = after_alt.find("](")
        {
            let target = &after_alt[label_end + 2..];
            if let Some(target_end) = target.find(')') {
                media_height += px(DEFAULT_IMAGE_HEIGHT);
                visible.push('\n');
                offset += 2 + label_end + 2 + target_end + 1;
                continue;
            }
        }
        if starts_html_tag(remainder, "<img")
            && let Some(tag_end) = remainder.find('>')
        {
            let tag = &remainder[..=tag_end];
            let explicit_height = html_numeric_attribute(tag, "height")
                .map(|height| height.clamp(1.0, 720.0))
                .unwrap_or(DEFAULT_IMAGE_HEIGHT);
            media_height += px(explicit_height);
            visible.push('\n');
            offset += tag_end + 1;
            continue;
        }

        let Some(character) = remainder.chars().next() else {
            break;
        };
        visible.push(character);
        offset += character.len_utf8();
    }

    (visible, media_height)
}

pub(super) fn find_ascii_case_insensitive(source: &str, needle: &str) -> Option<usize> {
    source
        .as_bytes()
        .windows(needle.len())
        .position(|candidate| candidate.eq_ignore_ascii_case(needle.as_bytes()))
}

pub(super) fn starts_html_tag(source: &str, name: &str) -> bool {
    source
        .get(..name.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
        && source[name.len()..]
            .chars()
            .next()
            .is_some_and(|character| {
                character.is_ascii_whitespace() || character == '>' || character == '/'
            })
}

pub(super) fn find_html_tag(source: &str, name: &str) -> Option<usize> {
    let mut cursor = 0;
    loop {
        let index = cursor + find_ascii_case_insensitive(&source[cursor..], name)?;
        if starts_html_tag(&source[index..], name) {
            return Some(index);
        }
        cursor = index + name.len();
    }
}

pub(super) fn matching_details_end(source: &str, body_start: usize) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut cursor = body_start;
    while cursor < source.len() {
        let next_open = find_html_tag(&source[cursor..], "<details").map(|index| cursor + index);
        let next_close = find_ascii_case_insensitive(&source[cursor..], "</details>")
            .map(|index| cursor + index);
        match (next_open, next_close) {
            (_, None) => return None,
            (Some(open), Some(close)) if open < close => {
                depth += 1;
                cursor = open + "<details".len();
            }
            (_, Some(close)) => {
                depth -= 1;
                if depth == 0 {
                    return Some((close, close + "</details>".len()));
                }
                cursor = close + "</details>".len();
            }
        }
    }
    None
}

pub(super) fn html_has_attribute(tag: &str, name: &str) -> bool {
    tag.split(|character: char| {
        character.is_ascii_whitespace() || character == '>' || character == '/'
    })
    .any(|attribute| {
        attribute.eq_ignore_ascii_case(name)
            || attribute
                .split_once('=')
                .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
    })
}

pub(super) fn html_numeric_attribute(tag: &str, name: &str) -> Option<f32> {
    let mut cursor = 0;
    let start = loop {
        let index = cursor + find_ascii_case_insensitive(&tag[cursor..], name)?;
        let before = tag[..index].chars().next_back();
        let after = tag[index + name.len()..].chars().next();
        if before.is_some_and(|character| character.is_ascii_whitespace())
            && after.is_some_and(|character| character.is_ascii_whitespace() || character == '=')
        {
            break index + name.len();
        }
        cursor = index + name.len();
    };
    let value = tag[start..].trim_start().strip_prefix('=')?.trim_start();
    let value = value
        .strip_prefix(['\'', '"'])
        .unwrap_or(value)
        .split(|character: char| {
            character == '\''
                || character == '"'
                || character.is_ascii_whitespace()
                || character == '>'
        })
        .next()?;
    value.trim_end_matches("px").parse().ok()
}

pub(super) fn estimated_message_height(
    message: &Message,
    content_width: Pixels,
    message_footer_visible: bool,
) -> Pixels {
    let assistant_columns = if content_width > Pixels::ZERO {
        (content_width / px(7.25)).max(20.0) as usize
    } else {
        88
    };
    let user_width = content_width.min(px(540.0));
    let user_columns = if user_width > Pixels::ZERO {
        (user_width / px(7.5)).max(20.0) as usize
    } else {
        72
    };
    match message.role {
        MessageRole::User => {
            let action_height = if message_footer_visible {
                px(30.0)
            } else {
                Pixels::ZERO
            };
            estimated_text_height(&message.content, user_columns, 20.0) + px(16.0) + action_height
        }
        MessageRole::Assistant => {
            let (visible_source, media_height) = markdown_estimation_source(&message.content);
            let footer_height = if message_footer_visible {
                px(30.0)
            } else {
                Pixels::ZERO
            };
            estimated_text_height(&visible_source, assistant_columns, 21.0)
                + media_height
                + px(8.0)
                + footer_height
        }
        MessageRole::System => {
            estimated_text_height(&message.content, assistant_columns, 16.0) + px(8.0)
        }
    }
}

pub(super) fn estimated_transcript_row_padding(
    row_index: usize,
    row_count: usize,
    starts_followup_turn: bool,
) -> Pixels {
    let top = if row_index == 0 {
        px(22.0)
    } else if starts_followup_turn {
        px(FOLLOWUP_TURN_TOP_GAP)
    } else {
        px(8.0)
    };
    let bottom = if row_index + 1 == row_count {
        px(22.0)
    } else {
        px(8.0)
    };
    top + bottom
}

/// Interleave live turn blocks at the exact message boundary where their
/// provider events arrived. `anchors[n] == 2` means block `n` renders after
/// messages 0 and 1, before message 2.
pub(super) fn transcript_row_kinds(
    message_count: usize,
    anchors: &[usize],
) -> Vec<TranscriptRowKind> {
    let mut blocks_after = vec![Vec::new(); message_count + 1];
    for (block_index, anchor) in anchors.iter().copied().enumerate() {
        blocks_after[anchor.min(message_count)].push(block_index);
    }
    let mut rows = Vec::with_capacity(message_count + anchors.len());
    rows.extend(
        blocks_after[0]
            .iter()
            .copied()
            .map(TranscriptRowKind::TurnBlock),
    );
    for message_index in 0..message_count {
        rows.push(TranscriptRowKind::Message(message_index));
        rows.extend(
            blocks_after[message_index + 1]
                .iter()
                .copied()
                .map(TranscriptRowKind::TurnBlock),
        );
    }
    rows
}

/// A settled turn presents only its terminal assistant message by default.
/// Earlier assistant commentary and ordered reasoning/tool blocks remain in
/// the transcript, but move behind one expandable work summary row.
pub(super) fn folded_transcript_row_kinds(
    session: &AgentSession,
    expanded_turns: &HashSet<Uuid>,
) -> Vec<TranscriptRowKind> {
    let anchors = session
        .transcript_blocks
        .iter()
        .map(|block| block.after_message)
        .collect::<Vec<_>>();
    let raw_rows = transcript_row_kinds(session.messages.len(), &anchors);
    let mut hidden_rows = HashSet::new();
    let mut fold_anchors = HashMap::new();

    for turn in &session.turns {
        if turn.status == TurnStatus::Running {
            continue;
        }
        let terminal_message = session.messages.iter().rposition(|message| {
            message.role == MessageRole::Assistant && message.turn_id == Some(turn.id)
        });
        let hidden = raw_rows
            .iter()
            .copied()
            .filter(|row| match *row {
                TranscriptRowKind::Message(message_index) => {
                    Some(message_index) != terminal_message
                        && session.messages.get(message_index).is_some_and(|message| {
                            message.role == MessageRole::Assistant
                                && message.turn_id == Some(turn.id)
                        })
                }
                TranscriptRowKind::TurnBlock(block_index) => session
                    .transcript_blocks
                    .get(block_index)
                    .is_some_and(|block| block.turn_id == Some(turn.id)),
                TranscriptRowKind::TurnFold(_) => false,
            })
            .collect::<Vec<_>>();
        let Some(anchor) = hidden.first().copied() else {
            continue;
        };
        fold_anchors.insert(anchor, turn.id);
        hidden_rows.extend(hidden);
    }

    let mut rows = Vec::with_capacity(raw_rows.len() + fold_anchors.len());
    for row in raw_rows {
        if let Some(turn_id) = fold_anchors.get(&row).copied() {
            rows.push(TranscriptRowKind::TurnFold(turn_id));
        }
        let expanded =
            row_turn_id(session, row).is_some_and(|turn_id| expanded_turns.contains(&turn_id));
        if expanded || !hidden_rows.contains(&row) {
            rows.push(row);
        }
    }
    rows
}

fn row_turn_id(session: &AgentSession, row: TranscriptRowKind) -> Option<Uuid> {
    match row {
        TranscriptRowKind::Message(index) => session.messages.get(index)?.turn_id,
        TranscriptRowKind::TurnBlock(index) => session.transcript_blocks.get(index)?.turn_id,
        TranscriptRowKind::TurnFold(turn_id) => Some(turn_id),
    }
}

pub(super) fn turn_fold_label(session: &AgentSession, turn_id: Uuid) -> String {
    let Some(turn) = session.turns.iter().find(|turn| turn.id == turn_id) else {
        return "Worked".into();
    };
    let seconds = turn
        .completed_at
        .unwrap_or_else(unix_time)
        .saturating_sub(turn.started_at)
        .max(1);
    let duration = format_worked_duration(seconds);
    if turn.status == TurnStatus::Interrupted {
        format!("You stopped after {duration}")
    } else {
        format!("Worked for {duration}")
    }
}

pub(super) fn format_worked_duration(seconds: u64) -> String {
    fn unit(value: u64, singular: &str) -> String {
        format!("{value} {singular}{}", if value == 1 { "" } else { "s" })
    }

    match seconds {
        0..=59 => unit(seconds, "second"),
        60..=3599 => {
            let minutes = seconds / 60;
            let seconds = seconds % 60;
            if seconds == 0 {
                unit(minutes, "minute")
            } else {
                format!("{} {}", unit(minutes, "minute"), unit(seconds, "second"))
            }
        }
        _ => {
            let hours = seconds / 3600;
            let minutes = (seconds % 3600) / 60;
            if minutes == 0 {
                unit(hours, "hour")
            } else {
                format!("{} {}", unit(hours, "hour"), unit(minutes, "minute"))
            }
        }
    }
}

pub(super) fn message_starts_followup_turn(messages: &[Message], message_index: usize) -> bool {
    messages
        .get(message_index)
        .is_some_and(|message| message.role == MessageRole::User)
        && messages[..message_index]
            .iter()
            .any(|message| message.role == MessageRole::User)
}

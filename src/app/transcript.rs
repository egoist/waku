use super::*;

impl Waku {
    /// One list row per message plus each ordered non-message turn block.
    pub(super) fn transcript_row_count(&self) -> usize {
        self.refresh_transcript_row_kinds()
    }

    /// Refold the cached row kinds, but only when the transcript they were
    /// folded from actually changed. Returns the row count.
    ///
    /// `render` calls this on every frame and folding is O(turns × messages) —
    /// a long session pays tens of thousands of operations plus a handful of
    /// allocations to rebuild rows that are almost always identical to the ones
    /// already cached. The fingerprint is one allocation-free linear pass, so a
    /// settled transcript now costs a scan instead of a fold.
    pub(super) fn refresh_transcript_row_kinds(&self) -> usize {
        let fingerprint = self
            .selected_session()
            .map_or(EMPTY_TRANSCRIPT_FINGERPRINT, |session| {
                transcript_rows_fingerprint(session, &self.expanded_turns)
            });
        if self.transcript_row_kinds_fingerprint.get() != Some(fingerprint) {
            let next_kinds = self.selected_transcript_row_kinds();
            *self.transcript_row_kinds.borrow_mut() = next_kinds;
            self.transcript_row_kinds_fingerprint.set(Some(fingerprint));
        }
        self.transcript_row_kinds.borrow().len()
    }

    pub(super) fn selected_transcript_row_kinds(&self) -> Vec<TranscriptRowKind> {
        self.selected_session().map_or_else(Vec::new, |session| {
            folded_transcript_row_kinds(session, &self.expanded_turns)
        })
    }

    /// The response footer's copy content and timestamp for `message_index`,
    /// cached under the row-kinds fingerprint.
    ///
    /// The row builder asks for every visible row on every frame, and
    /// [`assistant_response_footer`] walks the whole session and joins the
    /// turn's answer into a fresh `String` — far too much to redo per frame
    /// for values that only move when the fingerprint does: footers exist
    /// only for settled turns ([`assistant_response_footer_index`] returns
    /// `None` while the message streams or its turn runs), settled parts are
    /// immutable, and settling flips a turn status the fingerprint hashes.
    pub(super) fn assistant_response_footer_cached(
        &self,
        message_index: usize,
    ) -> (Option<SharedString>, Option<u64>) {
        self.refresh_transcript_row_kinds();
        let fingerprint = self.transcript_row_kinds_fingerprint.get();
        if self.assistant_footer_fingerprint.get() != fingerprint {
            self.assistant_footer_cache.borrow_mut().clear();
            self.assistant_footer_fingerprint.set(fingerprint);
        }
        if let Some(cached) = self.assistant_footer_cache.borrow().get(&message_index) {
            return cached.clone();
        }
        let value = self.selected_session().map_or((None, None), |session| {
            (
                assistant_response_footer(session, message_index).map(SharedString::from),
                assistant_response_footer_time(session, message_index),
            )
        });
        self.assistant_footer_cache
            .borrow_mut()
            .insert(message_index, value.clone());
        value
    }

    /// The navigation rail's turn list, rebuilt only when the row-kinds
    /// fingerprint moves.
    ///
    /// `render_transcript` needs this every frame, and extracting prompt and
    /// response snippets for every turn of a long session is far too much to
    /// redo per frame. Every input the extraction reads is settled under an
    /// unchanged [`transcript_rows_fingerprint`]: prompts are immutable once
    /// sent, a response snippet is read only after its turn stopped running,
    /// and completions, rewinds, and refolds all move the fingerprint.
    pub(super) fn navigation_turns(&self) -> Rc<Vec<TranscriptNavigationTurn>> {
        self.refresh_transcript_row_kinds();
        let fingerprint = self.transcript_row_kinds_fingerprint.get();
        if self.transcript_navigation_turns_fingerprint.get() != fingerprint {
            let turns = self
                .selected_session()
                .map(|session| {
                    transcript_navigation_turns(session, &self.transcript_row_kinds.borrow())
                })
                .unwrap_or_default();
            *self.transcript_navigation_turns.borrow_mut() = Rc::new(turns);
            self.transcript_navigation_turns_fingerprint
                .set(fingerprint);
        }
        self.transcript_navigation_turns.borrow().clone()
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
                //
                // Only when those bounds actually exist. Rows that have not
                // been measured yet report `None`, and treating that as a
                // zero-height document asks for a leading space of the whole
                // viewport — which pushes every row off screen and leaves the
                // transcript blank until the reader scrolls it back.
                let measured_content_height = transcript_rows
                    .bounds_for_item(0)
                    .zip(transcript_rows.bounds_for_item(count - 1))
                    .map(|(first, last)| (last.bottom() - first.top()).max(Pixels::ZERO));
                if let Some(leading_space) =
                    disclosure_leading_space(viewport_height, measured_content_height)
                {
                    transcript_rows.scroll_to(ListOffset {
                        item_ix: 0,
                        offset_in_item: -leading_space,
                    });
                }
            }
        }

        self.transcript_anchor_following.set(false);
        self.transcript_is_scrolled.set(true);
        // The position above is deliberate, so a wheel scroll still waiting to
        // be classified must not re-engage following on top of it.
        self.transcript_tail_recheck.set(false);
    }

    /// Bulk-reset the transcript. Used for session/document replacement.
    pub(super) fn reset_transcript_rows(&self, count: usize) {
        self.transcript_is_scrolled.set(false);
        self.transcript_rows.reset(count);
        self.anchored_transcript_rows.reset(count);
    }

    /// Apply a local disclosure change without replacing unchanged transcript
    /// rows.
    pub(super) fn splice_transcript_rows_after_visibility_change(
        &self,
        previous_kinds: &[TranscriptRowKind],
    ) {
        self.refresh_transcript_row_kinds();
        let splice = {
            let next_kinds = self.transcript_row_kinds.borrow();
            transcript_row_splice(previous_kinds, &next_kinds)
        };
        self.splice_transcript_rows(splice);
    }

    /// Snapshot the rows currently shown for `session_id` before a local state
    /// transition changes their visibility. Synchronizing first makes the
    /// snapshot describe the active list even when several provider events
    /// arrived in the same drain pass.
    pub(super) fn snapshot_selected_transcript_rows(
        &self,
        session_id: Uuid,
    ) -> Option<Vec<TranscriptRowKind>> {
        if self.state.selected_session != Some(session_id) {
            return None;
        }
        self.sync_transcript_rows();
        Some(self.transcript_row_kinds.borrow().clone())
    }

    /// Reconcile a visibility change against only the list on screen.
    ///
    /// Settling a turn folds its live work and removes the working indicator,
    /// so the visible row count usually shrinks. The generic count-based sync
    /// handles a shrink with `ListState::reset`, which clears the logical
    /// scroll position and exposes row zero for one frame before the sent-row
    /// anchor is restored. An exact splice retains the unchanged measurements
    /// and GPUI's logical scroll anchor throughout the fold.
    pub(super) fn splice_active_transcript_rows_after_visibility_change(
        &self,
        previous_kinds: &[TranscriptRowKind],
    ) {
        self.refresh_transcript_row_kinds();
        let splice = {
            let next_kinds = self.transcript_row_kinds.borrow();
            transcript_row_splice(previous_kinds, &next_kinds)
        };
        if let Some((range, new_count)) = splice {
            self.active_transcript_rows().splice(range, new_count);
        }
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
        let transcript_rows = self.active_transcript_rows();
        let last_row = transcript_rows.item_count().checked_sub(1);
        let anchored_tail_height = last_row.and_then(|last_row| {
            let anchor = transcript_rows.bounds_for_item(anchor_row)?;
            let last = transcript_rows.bounds_for_item(last_row)?;
            Some((last.bottom() - anchor.top()).max(Pixels::ZERO))
        });
        // Tail rows report no bounds for a frame whenever they are remeasured —
        // which the stream pump does on every commit — and report none at all
        // before the anchored list's first paint. Missing bounds mean unknown,
        // not zero: a zero end space reads as "the reply filled the viewport",
        // so the render's follow branch pins the list to its end, the next
        // measured frame snaps back to the anchor, and the two alternate at
        // stream cadence for the whole turn. Let the previous end space stand
        // (the send path seeds a provisional full-viewport reservation) and
        // keep asserting the anchor straight through the unmeasured frame —
        // scroll_to is bounds-independent.
        let end_space = match anchored_tail_height {
            Some(height) => transcript_anchor_end_space(viewport_height, height),
            None => self.transcript_anchor_end_space.get(),
        };
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

    /// Invalidate the measurement of rows whose *content* changed, without
    /// touching the row structure.
    ///
    /// `splice` would also work, but it re-arms GPUI's whole-list measuring
    /// behaviour, so every disclosure toggle and every streamed batch would
    /// re-measure the entire transcript. `remeasure_items` invalidates just the
    /// range — and restores the reader's scroll position across the height
    /// change on its own. This is what Zed's own agent chat uses.
    pub(super) fn remeasure_transcript_rows(&self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        self.transcript_rows.remeasure_items(range.clone());
        if self.transcript_anchor.get().is_some() {
            self.anchored_transcript_rows.remeasure_items(range);
        }
    }

    /// Re-render rows whose content changed in place so GPUI re-measures them.
    pub(super) fn splice_transcript_rows(&self, splice: Option<(Range<usize>, usize)>) {
        let Some((range, new_count)) = splice else {
            return;
        };
        self.transcript_rows.splice(range.clone(), new_count);
        if self.transcript_anchor.get().is_some() {
            self.anchored_transcript_rows.splice(range, new_count);
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

        // Reflow every row at the new wrap width. The row set is unchanged, so
        // this is a re-measure, not a splice.
        let count = self.active_transcript_rows().item_count();
        self.remeasure_transcript_rows(0..count);
        true
    }

    /// Keep the list's row count *and its row kinds* in sync with the
    /// transcript.
    ///
    /// The kinds cache is what tells `transcript_row` whether row `n` is a
    /// message, a reasoning block, a tool-activity cluster or a turn fold.
    /// Leaving it stale makes every row fall back to `Message(n)`, which
    /// silently drops all reasoning and activity from the transcript.
    ///
    /// Appends keep the reader's place (or the pinned tail); shrinking resets
    /// the view.
    pub(super) fn sync_transcript_rows(&self) {
        // The fold is cached; the list-state reconciliation below is not. It
        // has to run every time because `active_transcript_rows` can switch
        // lists under an unchanged transcript.
        let count = self.refresh_transcript_row_kinds();

        let transcript_rows = self.active_transcript_rows();
        let current = transcript_rows.item_count();
        if count > current {
            transcript_rows.splice(current..current, count - current);
        } else if count < current {
            self.reset_transcript_rows(count);
        }
    }

    pub(super) fn remeasure_transcript_tail(&self) {
        self.sync_transcript_rows();
        let count = self.active_transcript_rows().item_count();
        let from = count.saturating_sub(STREAM_REMEASURE_TAIL_ROWS);
        self.remeasure_transcript_rows(from..count);
    }

    pub(super) fn remeasure_transcript_block(&self, block_index: usize) {
        self.remeasure_transcript_row(TranscriptRowKind::TurnBlock(block_index));
    }

    pub(super) fn remeasure_transcript_message(&self, message_index: usize) {
        self.remeasure_transcript_row(TranscriptRowKind::Message(message_index));
    }

    pub(super) fn remeasure_changed_files(&self, turn_id: Uuid) {
        let target = self
            .selected_session()
            .and_then(|session| changed_files_inline_message_index(session, turn_id))
            .map(TranscriptRowKind::Message)
            .unwrap_or(TranscriptRowKind::ChangedFiles(turn_id));
        self.remeasure_transcript_row(target);
    }

    fn remeasure_transcript_row(&self, target: TranscriptRowKind) {
        self.sync_transcript_rows();
        let row = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == target);
        if let Some(row) = row {
            self.remeasure_transcript_rows(row..row + 1);
        }
    }
}

// ── Shared pieces ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum TranscriptRowKind {
    Message(usize),
    TurnBlock(usize),
    TurnFold(Uuid),
    /// Immutable file stats captured between this turn's pre- and post-turn
    /// checkpoints. Responses with a visible answer render this inside their
    /// terminal message; the row remains for interrupted/tool-only turns whose
    /// entire assistant output folds away.
    ChangedFiles(Uuid),
    /// The live turn's footer — pulsing dots plus "Working for Ns". Present
    /// from the moment the prompt lands until the turn settles, so a provider
    /// that has not produced a chunk yet still shows visible progress, and a
    /// streaming one shows it below whatever content has arrived.
    WorkingIndicator,
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

    // Message rows keep ascending message order through folding, so one cursor
    // walk resolves every prompt's row. A `position` scan from the top per
    // turn made this quadratic over a long session.
    let mut row_cursor = 0;
    let mut turns = Vec::with_capacity(user_message_indexes.len());
    for (turn_index, message_index) in user_message_indexes.iter().copied().enumerate() {
        let Some(message) = session.messages.get(message_index) else {
            continue;
        };
        let row_index = loop {
            match row_kinds.get(row_cursor) {
                None => break None,
                Some(TranscriptRowKind::Message(row_message)) if *row_message >= message_index => {
                    break (*row_message == message_index).then_some(row_cursor);
                }
                Some(_) => row_cursor += 1,
            }
        };
        let Some(row_index) = row_index else {
            continue;
        };
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
        turns.push(TranscriptNavigationTurn {
            message_id: message.id,
            message_index,
            row_index,
            prompt: if message.visible_content().trim().is_empty() {
                message
                    .attachments
                    .iter()
                    .map(|attachment| attachment.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                navigation_preview_snippet(message.visible_content(), 100)
            },
            response,
        });
    }
    turns
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
    emphasized_turn_index: Option<usize>,
) -> f32 {
    emphasized_turn_index.map_or(0.25, |emphasized| match turn_index.abs_diff(emphasized) {
        0 => 1.0,
        1 => 0.68,
        2 => 0.44,
        _ => 0.25,
    })
}

pub(super) fn navigation_rail_height(turn_count: usize, viewport_height: f32) -> f32 {
    (turn_count as f32 * NAVIGATION_RAIL_TURN_HEIGHT)
        .min(viewport_height * NAVIGATION_RAIL_VIEWPORT_HEIGHT_RATIO)
}

pub(super) fn navigation_rail_fade_visibility(
    offset_y: Pixels,
    max_offset: Pixels,
) -> (bool, bool) {
    let scrolled = -offset_y;
    let threshold = px(0.5);
    (scrolled > threshold, max_offset - scrolled > threshold)
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

/// The copy content for the response footer: the turn's *visible* answer.
///
/// Everything before the trailing run of text parts hides behind the turn's
/// "Worked for X" fold, and copying must match what the transcript presents
/// as the message — interim commentary the fold hides stays out, even while
/// the fold is expanded for inspection.
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
    let rows = turn_rows(session, turn_id);
    Some(
        rows[turn_answer_start(session, &rows)..]
            .iter()
            .filter_map(|row| match *row {
                TranscriptRowKind::Message(index) => session.messages.get(index),
                TranscriptRowKind::TurnBlock(_)
                | TranscriptRowKind::TurnFold(_)
                | TranscriptRowKind::ChangedFiles(_)
                | TranscriptRowKind::WorkingIndicator => None,
            })
            .filter(|part| !part.content.trim().is_empty())
            .map(|part| part.content.as_str())
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

/// The visible terminal answer row that owns both the changed-files card and
/// the response footer. A turn with no visible answer returns `None`, leaving
/// its card as a standalone row after the collapsed work disclosure.
pub(super) fn changed_files_inline_message_index(
    session: &AgentSession,
    turn_id: Uuid,
) -> Option<usize> {
    let turn = session.turns.iter().find(|turn| turn.id == turn_id)?;
    if turn.status == TurnStatus::Running
        || !turn.checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.status == CheckpointStatus::Ready && !checkpoint.files.is_empty()
        })
    {
        return None;
    }

    let message_index = session.messages.iter().rposition(|message| {
        message.role == MessageRole::Assistant && message.turn_id == Some(turn_id)
    })?;
    let message = &session.messages[message_index];
    (!message.content.trim().is_empty()
        && assistant_response_footer_index(session, message_index) == Some(message_index))
    .then_some(message_index)
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

/// Leading space above a short, bottom-aligned transcript, or `None` when the
/// content height is unknown.
///
/// `None` matters more than the arithmetic: rows that have not been measured
/// yet report no bounds, and treating that as a zero-height document asks for a
/// leading space of the entire viewport — which pushes every row off screen and
/// leaves the transcript blank until the reader scrolls it back.
pub(super) fn disclosure_leading_space(
    viewport_height: Pixels,
    content_height: Option<Pixels>,
) -> Option<Pixels> {
    content_height.map(|height| (viewport_height - height).max(Pixels::ZERO))
}

pub(super) fn transcript_anchor_end_space(
    viewport_height: Pixels,
    anchored_tail_height: Pixels,
) -> Pixels {
    (viewport_height - anchored_tail_height).max(Pixels::ZERO)
}

/// Whether the transcript needs an explicit affordance for returning to its
/// tail, or `None` when the tail's position is unknowable this frame. A
/// measured scroll range is required because disclosure pinning can leave
/// `is_scrolled` set after a collapse removes all overflow. The final row's
/// bounds then distinguish the tail position for both list alignments:
/// `ListScrollEvent::is_scrolled` alone stays true at the bottom of the
/// top-aligned list used while a turn is anchored.
///
/// The caller holds the previous answer through `None` rather than resolving
/// it. Every stream commit remeasures the tail rows, so the frame after each
/// one has no bounds to read, and answering "show" into that silence blinks the
/// button against the measured frames in between — at commit cadence, for as
/// long as the reader sits at the tail without following it.
pub(super) fn should_show_scroll_to_bottom(
    is_scrolled: bool,
    anchor_following: bool,
    transcript_scrollable: bool,
    viewport_bottom: Pixels,
    tail_bottom: Option<Pixels>,
    end_space: Pixels,
) -> Option<bool> {
    if !is_scrolled || anchor_following || !transcript_scrollable {
        return Some(false);
    }

    Some(!transcript_rests_at_tail(
        viewport_bottom,
        tail_bottom,
        end_space,
    )?)
}

/// Whether the transcript currently sits at the end of its content, or `None`
/// while the last row is unmeasured and its position is unknowable.
///
/// `None` is not `false`. The stream remeasures the tail on every commit, so a
/// wheel scroll can easily settle on a frame that cannot answer; a caller
/// waiting to re-engage tail following has to ask again rather than conclude
/// the reader stopped short of the tail.
pub(super) fn transcript_rests_at_tail(
    viewport_bottom: Pixels,
    tail_bottom: Option<Pixels>,
    end_space: Pixels,
) -> Option<bool> {
    Some(tail_bottom? + end_space <= viewport_bottom + px(0.5))
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

pub(super) const ACTIVITY_IMAGE_WIDTH: f32 = 300.0;
pub(super) const ACTIVITY_IMAGE_HEIGHT: f32 = 200.0;

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

/// Fingerprint of every field [`folded_transcript_row_kinds`] reads, so a frame
/// can tell a settled transcript from a changed one without refolding it.
///
/// Keep this in step with that function and with [`row_turn_id`]. A field they
/// consult but this one misses leaves the cached rows stale, and stale rows
/// fall back to `Message(n)` — silently dropping every reasoning block and tool
/// activity from the transcript. Cheap mixing, not a real hash: this runs on
/// the frame path, and the values it folds in are already well distributed.
pub(super) fn transcript_rows_fingerprint(
    session: &AgentSession,
    expanded_turns: &HashSet<Uuid>,
) -> u64 {
    let mut hash = mix_uuid(EMPTY_TRANSCRIPT_FINGERPRINT, session.id);

    // The working indicator row exists only while the session is busy, and a
    // driver error can drop the busy status without touching any turn — the
    // turn statuses below would hold still while the rows moved.
    hash = mix(hash, session.status.is_busy() as u64);

    hash = mix(hash, session.messages.len() as u64);
    for message in &session.messages {
        hash = mix(hash, message.role as u64);
        hash = mix_turn_id(hash, message.turn_id);
        // The fold counts a blank text part as work, so a part crossing that
        // line moves rows. `trim` stops at the first non-space character, so
        // this stays a per-message constant on the frame path.
        hash = mix(hash, message.content.trim().is_empty() as u64);
    }

    hash = mix(hash, session.transcript_blocks.len() as u64);
    for block in &session.transcript_blocks {
        // An in-flight activity block re-anchors itself, so the anchor has to
        // be folded in rather than assumed fixed at insertion.
        hash = mix(hash, block.after_message as u64);
        hash = mix_turn_id(hash, block.turn_id);
    }

    hash = mix(hash, session.turns.len() as u64);
    for turn in &session.turns {
        hash = mix_uuid(hash, turn.id);
        hash = mix(hash, turn.status as u64);
        hash = mix(
            hash,
            turn.checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.status == CheckpointStatus::Ready && !checkpoint.files.is_empty()
            }) as u64,
        );
    }

    // A set has no stable iteration order, so combine its members with an
    // order-independent sum instead of folding them in sequence.
    let expanded = expanded_turns.iter().fold(0u64, |combined, turn_id| {
        combined.wrapping_add(mix_uuid(EMPTY_TRANSCRIPT_FINGERPRINT, *turn_id))
    });
    mix(mix(hash, expanded_turns.len() as u64), expanded)
}

/// The fingerprint of "no session selected", and the seed everything else
/// mixes its session id into.
pub(super) const EMPTY_TRANSCRIPT_FINGERPRINT: u64 = 0xcbf2_9ce4_8422_2325;

const FINGERPRINT_PRIME: u64 = 0x0000_0100_0000_01b3;

pub(super) fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(FINGERPRINT_PRIME)
}

pub(super) fn mix_uuid(hash: u64, id: Uuid) -> u64 {
    let bits = id.as_u128();
    mix(mix(hash, bits as u64), (bits >> 64) as u64)
}

fn mix_turn_id(hash: u64, turn_id: Option<Uuid>) -> u64 {
    match turn_id {
        Some(turn_id) => mix_uuid(hash, turn_id),
        None => mix(hash, u64::MAX),
    }
}

/// A settled turn presents its answer — the trailing run of assistant text —
/// under a single work summary row standing in for everything that came
/// before it: reasoning, tool activity and interim commentary alike.
///
/// That summary belongs at the *top* of the turn, where the work began. It
/// used to be anchored at the first row it hid, which left it stranded mid-turn
/// whenever the agent thought before it spoke — a divider reading "Worked for
/// 19 seconds" with more work listed below it, as if the response had been cut
/// in half. Expanding it restores the turn's full order in place.
///
/// Every field this reads is fingerprinted by [`transcript_rows_fingerprint`]
/// so frames can skip the fold; consult a new one and that must learn it too.
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
        let turn_rows = turn_rows(session, turn.id);
        let hidden = &turn_rows[..turn_answer_start(session, &turn_rows)];
        let Some(anchor) = hidden.first().copied() else {
            continue;
        };
        fold_anchors.insert(anchor, turn.id);
        hidden_rows.extend(hidden.iter().copied());
    }

    let mut rows = Vec::with_capacity(raw_rows.len() + fold_anchors.len() + 1);
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
    // A busy session with a live turn closes with the working indicator. The
    // busy check matters on its own: a driver error can fail the session while
    // its turn is still marked running, and "Working" over a failure misleads.
    if session.status.is_busy() && session.active_turn_id().is_some() {
        rows.push(TranscriptRowKind::WorkingIndicator);
    }

    // A normal response renders its file summary inside the terminal answer,
    // immediately before that message's footer actions. Only turns without a
    // visible answer need a standalone row; derive its insertion point from
    // the already-folded rows so it lands after the visible `Worked for …`
    // disclosure and before the next user prompt.
    let changed_turns = session
        .turns
        .iter()
        .filter(|turn| {
            turn.status != TurnStatus::Running
                && turn.checkpoint.as_ref().is_some_and(|checkpoint| {
                    checkpoint.status == CheckpointStatus::Ready && !checkpoint.files.is_empty()
                })
                && changed_files_inline_message_index(session, turn.id).is_none()
        })
        .map(|turn| turn.id)
        .collect::<HashSet<_>>();
    if changed_turns.is_empty() {
        return rows;
    }

    let mut last_row_by_turn = HashMap::new();
    for (index, row) in rows.iter().copied().enumerate() {
        if let Some(turn_id) = row_turn_id(session, row)
            && changed_turns.contains(&turn_id)
        {
            last_row_by_turn.insert(turn_id, index);
        }
    }
    let changed_after_row = last_row_by_turn
        .into_iter()
        .map(|(turn_id, index)| (index, turn_id))
        .collect::<HashMap<_, _>>();
    let mut with_changes = Vec::with_capacity(rows.len() + changed_after_row.len());
    for (index, row) in rows.into_iter().enumerate() {
        with_changes.push(row);
        if let Some(turn_id) = changed_after_row.get(&index).copied() {
            with_changes.push(TranscriptRowKind::ChangedFiles(turn_id));
        }
    }
    with_changes
}

/// The rows the turn *produced*, in transcript order: its assistant text parts
/// merged with its blocks at their anchors, exactly the turn's subsequence of
/// [`transcript_row_kinds`]. The user's prompt opens a turn and carries its id,
/// but it is the heading the fold sits under, never part of what folds away —
/// so it is not one of the turn's rows.
///
/// Both the fold and the response footer's copy content derive from this, so
/// what gets copied is by construction what the fold leaves visible.
fn turn_rows(session: &AgentSession, turn_id: Uuid) -> Vec<TranscriptRowKind> {
    let message_count = session.messages.len();
    // A block with (clamped) anchor `n` renders before message `n`, and blocks
    // sharing an anchor keep their insertion order — the stable sort mirrors
    // the bucket traversal in `transcript_row_kinds`.
    let mut blocks = session
        .transcript_blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.turn_id == Some(turn_id))
        .map(|(block_index, block)| (block.after_message.min(message_count), block_index))
        .collect::<Vec<_>>();
    blocks.sort_by_key(|(anchor, _)| *anchor);
    let mut blocks = blocks.into_iter().peekable();

    let mut rows = Vec::new();
    for (message_index, message) in session.messages.iter().enumerate() {
        while let Some((_, block_index)) = blocks.next_if(|(anchor, _)| *anchor <= message_index) {
            rows.push(TranscriptRowKind::TurnBlock(block_index));
        }
        if message.role == MessageRole::Assistant && message.turn_id == Some(turn_id) {
            rows.push(TranscriptRowKind::Message(message_index));
        }
    }
    rows.extend(blocks.map(|(_, block_index)| TranscriptRowKind::TurnBlock(block_index)));
    rows
}

/// Where the turn's answer begins within its own rows: the trailing run of
/// assistant text parts. Everything before that is work and folds.
///
/// A blank part renders nothing, so it counts as work rather than extending the
/// run backwards over the reasoning and tool calls between it and the answer.
/// A turn that produced no text at all — interrupted, or pure tool output — has
/// no answer, and all of it folds.
fn turn_answer_start(session: &AgentSession, turn_rows: &[TranscriptRowKind]) -> usize {
    let is_answer_text = |row: &TranscriptRowKind| match *row {
        TranscriptRowKind::Message(message_index) => session
            .messages
            .get(message_index)
            .is_some_and(|message| !message.content.trim().is_empty()),
        TranscriptRowKind::TurnBlock(_)
        | TranscriptRowKind::TurnFold(_)
        | TranscriptRowKind::ChangedFiles(_)
        | TranscriptRowKind::WorkingIndicator => false,
    };
    let Some(last_text) = turn_rows.iter().rposition(is_answer_text) else {
        return turn_rows.len();
    };
    turn_rows[..last_text]
        .iter()
        .rposition(|row| !is_answer_text(row))
        .map_or(0, |index| index + 1)
}

fn row_turn_id(session: &AgentSession, row: TranscriptRowKind) -> Option<Uuid> {
    match row {
        TranscriptRowKind::Message(index) => session.messages.get(index)?.turn_id,
        TranscriptRowKind::TurnBlock(index) => session.transcript_blocks.get(index)?.turn_id,
        TranscriptRowKind::TurnFold(turn_id) => Some(turn_id),
        TranscriptRowKind::ChangedFiles(turn_id) => Some(turn_id),
        TranscriptRowKind::WorkingIndicator => None,
    }
}

pub(super) fn turn_fold_label(session: &AgentSession, turn_id: Uuid) -> String {
    let Some(turn) = session.turns.iter().find(|turn| turn.id == turn_id) else {
        return tr!("transcript.worked");
    };
    let seconds = turn
        .completed_at
        .unwrap_or_else(unix_time)
        .saturating_sub(turn.started_at)
        .max(1);
    let duration = format_worked_duration(seconds);
    if turn.status == TurnStatus::Interrupted {
        tr!("transcript.you_stopped_after", duration = duration)
    } else {
        tr!("transcript.worked_for", duration = duration)
    }
}

pub(super) fn format_worked_duration(seconds: u64) -> String {
    fn unit(value: u64, singular_key: &str, plural_key: &str) -> String {
        if value == 1 {
            tr!(singular_key, count = value)
        } else {
            tr!(plural_key, count = value)
        }
    }

    match seconds {
        0..=59 => unit(seconds, "duration.second", "duration.seconds"),
        60..=3599 => {
            let minutes = seconds / 60;
            let seconds = seconds % 60;
            if seconds == 0 {
                unit(minutes, "duration.minute", "duration.minutes")
            } else {
                tr!(
                    "duration.two_units",
                    first = unit(minutes, "duration.minute", "duration.minutes"),
                    second = unit(seconds, "duration.second", "duration.seconds")
                )
            }
        }
        _ => {
            let hours = seconds / 3600;
            let minutes = (seconds % 3600) / 60;
            if minutes == 0 {
                unit(hours, "duration.hour", "duration.hours")
            } else {
                tr!(
                    "duration.two_units",
                    first = unit(hours, "duration.hour", "duration.hours"),
                    second = unit(minutes, "duration.minute", "duration.minutes")
                )
            }
        }
    }
}

/// The live indicator's elapsed label: "9s", "1m 5s", "1h 2m". Compact where
/// [`format_worked_duration`] is prose — the settled fold reads as a sentence,
/// while this one ticks every second beside the pulsing dots.
pub(super) fn format_working_elapsed(seconds: u64) -> String {
    match seconds {
        0..=59 => tr!("duration.seconds_short", count = seconds),
        60..=3599 => {
            let minutes = seconds / 60;
            match seconds % 60 {
                0 => tr!("duration.minutes_short", count = minutes),
                seconds => tr!(
                    "duration.two_units",
                    first = tr!("duration.minutes_short", count = minutes),
                    second = tr!("duration.seconds_short", count = seconds)
                ),
            }
        }
        _ => {
            let hours = seconds / 3600;
            match (seconds % 3600) / 60 {
                0 => tr!("duration.hours_short", count = hours),
                minutes => tr!(
                    "duration.two_units",
                    first = tr!("duration.hours_short", count = hours),
                    second = tr!("duration.minutes_short", count = minutes)
                ),
            }
        }
    }
}

/// Whether this user message is the prompt that opened its turn.
///
/// A steer accepted mid-turn joins the running turn as another user message,
/// so a turn can hold several. Only the opening prompt is a rewind boundary —
/// checkpoints and provider rollback are per turn, not per message. Messages
/// of a turn are contiguous, so this walks back only while the turn id holds
/// rather than scanning the session, which a per-frame row builder must avoid.
pub(super) fn message_opens_turn(messages: &[Message], message_index: usize) -> bool {
    let Some(turn_id) = messages
        .get(message_index)
        .filter(|message| message.role == MessageRole::User)
        .and_then(|message| message.turn_id)
    else {
        return false;
    };
    !messages[..message_index]
        .iter()
        .rev()
        .take_while(|earlier| earlier.turn_id == Some(turn_id))
        .any(|earlier| earlier.role == MessageRole::User)
}

pub(super) fn message_starts_followup_turn(messages: &[Message], message_index: usize) -> bool {
    messages
        .get(message_index)
        .is_some_and(|message| message.role == MessageRole::User)
        && messages[..message_index]
            .iter()
            .any(|message| message.role == MessageRole::User)
}

use super::*;

impl Waku {
    pub(super) fn finish_streaming_assistant(&mut self, session_id: Uuid) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            for message in &mut session.messages {
                if message.role == MessageRole::Assistant && message.streaming {
                    message.streaming = false;
                }
            }
        }
    }

    pub(super) fn append_text_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        delta: String,
    ) {
        let continuing = runtime.stream_phase == Some(StreamPhase::Text);
        append_text_delta_to_session(&mut self.state.sessions, session_id, continuing, delta);
        runtime.stream_phase = Some(StreamPhase::Text);
    }

    pub(super) fn append_reasoning_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        delta: String,
    ) {
        let continuing = runtime.stream_phase == Some(StreamPhase::Reasoning);
        if !continuing && delta.trim().is_empty() {
            return;
        }
        let now = unix_time_millis();
        if !continuing {
            self.finish_streaming_assistant(session_id);
        }
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if continuing
                && let Some(TranscriptBlock {
                    content: TranscriptBlockContent::Reasoning(reasoning),
                    ..
                }) = session.transcript_blocks.last_mut()
            {
                reasoning.content.push_str(&delta);
                reasoning.finished_at_ms = now;
            } else {
                session.transcript_blocks.push(TranscriptBlock {
                    after_message: session.messages.len(),
                    turn_id: session.active_turn_id(),
                    content: TranscriptBlockContent::Reasoning(ReasoningBlock {
                        content: delta,
                        started_at_ms: now,
                        finished_at_ms: now,
                    }),
                });
            }
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Reasoning);
    }

    pub(super) fn update_activity(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        item: ActivityItem,
    ) {
        if runtime.stream_phase == Some(StreamPhase::Text) {
            self.finish_streaming_assistant(session_id);
        }

        let continuing = runtime.stream_phase == Some(StreamPhase::Activity);
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            for block in session.transcript_blocks.iter_mut().rev() {
                let TranscriptBlockContent::Activities(activities) = &mut block.content else {
                    continue;
                };
                let matching = activities.iter_mut().rev().find(|activity| {
                    item.source_id
                        .as_ref()
                        .is_some_and(|id| activity.source_id.as_ref() == Some(id))
                        || (item.source_id.is_none()
                            && activity.title == item.title
                            && !activity.complete)
                });
                if let Some(activity) = matching {
                    activity.kind = item.kind;
                    activity.title = item.title;
                    activity.complete = item.complete;
                    if item.detail.is_some() {
                        activity.detail = item.detail;
                    }
                    session.updated_at = unix_time();
                    runtime.stream_phase = Some(StreamPhase::Activity);
                    return;
                }
            }

            let after_message = session.messages.len();
            if continuing
                && let Some(TranscriptBlock {
                    after_message: anchor,
                    content: TranscriptBlockContent::Activities(activities),
                    ..
                }) = session.transcript_blocks.last_mut()
                && *anchor == after_message
            {
                activities.push(item);
            } else {
                session.transcript_blocks.push(TranscriptBlock {
                    after_message,
                    turn_id: session.active_turn_id(),
                    content: TranscriptBlockContent::Activities(vec![item]),
                });
            }
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Activity);
    }

    pub(super) fn complete_turn_blocks(&mut self, session_id: Uuid) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            for block in &mut session.transcript_blocks {
                if let TranscriptBlockContent::Activities(activities) = &mut block.content {
                    for activity in activities {
                        activity.complete = true;
                    }
                }
            }
        }
    }

    pub(super) fn turn_has_assistant_message(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                let Some(turn_id) = session.active_turn_id() else {
                    return false;
                };
                session.messages.iter().any(|message| {
                    message.role == MessageRole::Assistant && message.turn_id == Some(turn_id)
                })
            })
    }

    pub(super) fn accepts_turn_output(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                session.active_turn_id().is_some()
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    )
            })
    }

    /// Returns whether the runtime should remain attached after this event.
    pub(super) fn handle_driver_event(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        event: DriverEvent,
    ) -> bool {
        match event {
            DriverEvent::Connected { provider_cursor } => {
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    if let Some(ProviderResumeCursor::Claude {
                        resume_at: Some(message_id),
                        ..
                    }) = &provider_cursor
                    {
                        session.mark_active_turn_provider_resume_at(message_id.clone());
                    }
                    session.provider_cursor = provider_cursor;
                    if session.status == SessionStatus::Connecting {
                        session.status = SessionStatus::Working;
                    }
                }
            }
            DriverEvent::TurnStarted => {
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && session.active_turn_id().is_some()
                {
                    session.mark_active_turn_provider_started();
                    session.status = SessionStatus::Working;
                }
            }
            DriverEvent::TextDelta(delta) => {
                if self.accepts_turn_output(session_id) {
                    self.append_text_delta(session_id, runtime, delta);
                }
            }
            DriverEvent::ReasoningDelta(delta) => {
                if self.accepts_turn_output(session_id) {
                    self.append_reasoning_delta(session_id, runtime, delta);
                }
            }
            DriverEvent::Activity {
                id,
                kind,
                title,
                detail,
                complete,
            } => {
                if self.accepts_turn_output(session_id) {
                    self.update_activity(
                        session_id,
                        runtime,
                        ActivityItem::new(id, kind, title, detail, complete),
                    );
                }
            }
            DriverEvent::Permission {
                request_id,
                title,
                detail,
                options,
            } => {
                if self.accepts_turn_output(session_id) {
                    runtime.pending_permission = Some(PendingPermission {
                        request_id,
                        title,
                        detail,
                        options,
                    });
                    if let Some(session) = self
                        .state
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == session_id)
                    {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::ComputerToolRequested(request) => {
                if !self.state.computer_use_enabled {
                    runtime.driver.reject_computer_tool(
                        request,
                        "Computer Use is disabled in Waku Settings.".into(),
                    );
                } else if let Err(error) = crate::computer_use::validate_request(&request) {
                    runtime
                        .driver
                        .reject_computer_tool(request, error.to_string());
                } else if request.tool != "use" {
                    runtime.driver.run_computer_tool(request);
                } else if runtime.pending_computer_approval.is_some() {
                    runtime.driver.reject_computer_tool(
                        request,
                        "Another app-control request is awaiting approval. Retry after it resolves."
                            .into(),
                    );
                } else if let Some(target) = request.target() {
                    let key = target.grant_key();
                    let globally_allowed = target.persistable()
                        && self
                            .state
                            .computer_use_allowed_apps
                            .iter()
                            .any(|grant| grant.key() == key);
                    let already_allowed =
                        globally_allowed || runtime.computer_session_grants.contains(&key);
                    let sensitive = request.requires_sensitive_confirmation();
                    if already_allowed && !sensitive {
                        runtime.driver.run_computer_tool(request);
                    } else {
                        Self::upsert_computer_use_preview(
                            runtime,
                            ComputerUseState {
                                call_id: request.call_id.clone(),
                                target: Some(target.clone()),
                                summary: request.summary(),
                                phase: ComputerUsePhase::AwaitingApproval,
                                visible: true,
                                screenshot: None,
                                error: None,
                            },
                        );
                        runtime.pending_computer_approval = Some(PendingComputerApproval {
                            request,
                            target,
                            sensitive,
                        });
                        if let Some(session) = self
                            .state
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id)
                        {
                            session.status = SessionStatus::Waiting;
                        }
                    }
                } else {
                    runtime.driver.reject_computer_tool(
                        request,
                        "Computer use requires a target returned by list_targets.".into(),
                    );
                }
            }
            DriverEvent::ComputerUseUpdated(state) => {
                let complete = matches!(
                    state.phase,
                    ComputerUsePhase::Completed | ComputerUsePhase::Failed
                );
                let app_name = state
                    .target
                    .as_ref()
                    .map(|target| target.app_name.as_str())
                    .unwrap_or("the computer");
                let title = match (state.target.is_some(), state.phase) {
                    (false, _) => state.summary.clone(),
                    (true, ComputerUsePhase::AwaitingApproval) => {
                        format!("Waiting to use {app_name}")
                    }
                    (true, ComputerUsePhase::Running) => format!("Using {app_name}"),
                    (true, ComputerUsePhase::Completed) => format!("Used {app_name}"),
                    (true, ComputerUsePhase::Failed) => format!("Could not use {app_name}"),
                };
                let detail = state.error.clone().or_else(|| Some(state.summary.clone()));
                self.update_activity(
                    session_id,
                    runtime,
                    ActivityItem::new(
                        Some(state.call_id.clone()),
                        ActivityKind::Tool,
                        title,
                        detail,
                        complete,
                    ),
                );
                Self::upsert_computer_use_preview(runtime, state);
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && session.status == SessionStatus::Waiting
                    && runtime.pending_computer_approval.is_none()
                {
                    session.status = SessionStatus::Working;
                }
            }
            DriverEvent::ComputerPermissions(permissions) => {
                self.computer_permissions = permissions;
            }
            DriverEvent::TurnFinished { success, summary } => {
                if self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(AgentSession::active_turn_id)
                    .is_none()
                {
                    return true;
                }
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                let needs_fallback = !self.turn_has_assistant_message(session_id);
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    session.status = if success {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Failed
                    };
                    if needs_fallback {
                        session.push_message(
                            MessageRole::Assistant,
                            summary.unwrap_or_else(|| {
                                if success {
                                    "Turn completed.".into()
                                } else {
                                    "The agent stopped before returning a response.".into()
                                }
                            }),
                        );
                    }
                    session.finish_active_turn(if success {
                        TurnStatus::Completed
                    } else {
                        TurnStatus::Failed
                    });
                }
                runtime.pending_permission = None;
                runtime.pending_computer_approval = None;
                runtime.driver.cancel_computer_use();
                runtime.computer_use_previews.clear();
                self.capture_latest_turn_checkpoint_for(session_id);
            }
            DriverEvent::Error(error) => {
                if self.state.selected_session == Some(session_id) {
                    self.toast = Some(error.clone());
                }
                let has_active_turn = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(AgentSession::active_turn_id)
                    .is_some();
                let should_append = has_active_turn
                    && !self.turn_has_assistant_message(session_id)
                    && self
                        .state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .is_some_and(|session| session.status != SessionStatus::Working);
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && has_active_turn
                {
                    if session.status != SessionStatus::Working {
                        session.status = SessionStatus::Failed;
                    }
                    if should_append {
                        session.push_message(MessageRole::Assistant, error);
                    }
                }
            }
            DriverEvent::ProcessExited => {
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                runtime.pending_permission = None;
                runtime.pending_computer_approval = None;
                runtime.driver.cancel_computer_use();
                runtime.computer_use_previews.clear();
                let mut finished_turn = false;
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    )
                {
                    session.status = SessionStatus::Failed;
                    session.updated_at = unix_time();
                    finished_turn = session.finish_active_turn(TurnStatus::Failed).is_some();
                }
                if finished_turn {
                    self.capture_latest_turn_checkpoint_for(session_id);
                }
                return false;
            }
        }
        true
    }

    fn upsert_computer_use_preview(runtime: &mut SessionRuntime, mut state: ComputerUseState) {
        if !state.visible {
            return;
        }
        let Some(window_id) = state.target.as_ref().map(|target| target.window_id) else {
            return;
        };
        if let Some(index) = runtime.computer_use_previews.iter().position(|preview| {
            preview
                .target
                .as_ref()
                .is_some_and(|target| target.window_id == window_id)
        }) {
            let previous = runtime.computer_use_previews.remove(index);
            if state.screenshot.is_none() {
                state.screenshot = previous.screenshot;
            }
        }
        runtime.computer_use_previews.push(state);
    }
}

pub(super) fn stream_delta_kind(event: &DriverEvent) -> Option<StreamDeltaKind> {
    match event {
        DriverEvent::TextDelta(_) => Some(StreamDeltaKind::Text),
        DriverEvent::ReasoningDelta(_) => Some(StreamDeltaKind::Reasoning),
        _ => None,
    }
}

pub(super) fn stream_delta_text(event: &DriverEvent, kind: StreamDeltaKind) -> Option<&str> {
    match (kind, event) {
        (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
        | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => Some(text),
        _ => None,
    }
}

pub(super) fn stream_frame_budget(backlog: usize) -> usize {
    backlog
        .div_ceil(STREAM_CATCH_UP_FRAMES)
        .clamp(
            STREAM_MIN_GRAPHEMES_PER_FRAME,
            STREAM_MAX_GRAPHEMES_PER_FRAME,
        )
        .min(backlog)
}

/// Pop one display-sized chunk while retaining the provider's event order.
///
/// Adjacent deltas of the same kind are coalesced. Large deltas are split on
/// grapheme and line boundaries, so a provider that emits its whole answer in
/// one event still gets the same progressive presentation as token streams.
pub(super) fn pop_stream_chunk(
    events: &mut VecDeque<DriverEvent>,
    kind: StreamDeltaKind,
) -> Option<DriverEvent> {
    let backlog = events
        .iter()
        .map_while(|event| stream_delta_text(event, kind))
        .map(|text| text.graphemes(true).count())
        .sum();
    if backlog == 0 {
        return events.pop_front();
    }

    let mut remaining_budget = stream_frame_budget(backlog);
    let mut chunk = String::new();
    while remaining_budget > 0 {
        let Some(text) = events.front_mut().and_then(|event| match (kind, event) {
            (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
            | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => Some(text),
            _ => None,
        }) else {
            break;
        };

        let (prefix, graphemes) = take_stream_prefix(text, remaining_budget);
        let reached_line_boundary = prefix.ends_with('\n');
        chunk.push_str(&prefix);
        remaining_budget = remaining_budget.saturating_sub(graphemes);
        if text.is_empty() {
            events.pop_front();
        }
        if reached_line_boundary {
            break;
        }
    }

    match kind {
        StreamDeltaKind::Text => Some(DriverEvent::TextDelta(chunk)),
        StreamDeltaKind::Reasoning => Some(DriverEvent::ReasoningDelta(chunk)),
    }
}

pub(super) fn take_stream_prefix(text: &mut String, budget: usize) -> (String, usize) {
    if text.is_empty() || budget == 0 {
        return (String::new(), 0);
    }

    let mut count = 0;
    let mut end = text.len();
    for (start, grapheme) in text.grapheme_indices(true) {
        count += 1;
        end = start + grapheme.len();
        if grapheme == "\n" || count == budget {
            break;
        }
    }

    let remainder = text.split_off(end);
    (std::mem::replace(text, remainder), count)
}

pub(super) fn append_text_delta_to_session(
    sessions: &mut [AgentSession],
    session_id: Uuid,
    continuing: bool,
    delta: String,
) {
    let Some(session) = sessions.iter_mut().find(|session| session.id == session_id) else {
        return;
    };
    if !continuing {
        for message in &mut session.messages {
            if message.role == MessageRole::Assistant && message.streaming {
                message.streaming = false;
            }
        }
    }
    let existing = continuing.then(|| {
        session
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.role == MessageRole::Assistant && message.streaming)
    });
    if let Some(Some(message)) = existing {
        message.content.push_str(&delta);
    } else {
        let mut message = session
            .active_turn_id()
            .map(|turn_id| Message::new_for_turn(MessageRole::Assistant, delta.clone(), turn_id))
            .unwrap_or_else(|| Message::new(MessageRole::Assistant, delta));
        message.streaming = true;
        session.messages.push(message);
    }
    session.updated_at = unix_time();
}

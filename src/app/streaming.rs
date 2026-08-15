use super::*;

impl Waku {
    pub(super) fn finish_streaming_assistant(&mut self, session_id: Uuid) {
        if let Some(session) = self.state.session_mut(session_id) {
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
        let previous_phase = runtime.stream_phase;
        if previous_phase == Some(StreamPhase::Reasoning) {
            self.complete_reasoning_activity(session_id);
        }
        let continuing = previous_phase == Some(StreamPhase::Text);
        append_text_delta_to_session(&mut self.state.sessions, session_id, continuing, delta);
        self.state.mark_session_dirty(session_id);
        runtime.stream_phase = Some(StreamPhase::Text);
    }

    fn complete_reasoning_activity(&mut self, session_id: Uuid) {
        let Some(session) = self.state.session_mut(session_id) else {
            return;
        };
        let reasoning = session
            .transcript_blocks
            .iter_mut()
            .rev()
            .flat_map(|block| block.activities.iter_mut().rev())
            .find(|activity| activity.reasoning.is_some() && !activity.complete);
        if let Some(reasoning) = reasoning {
            reasoning.complete = true;
            session.updated_at = unix_time();
        }
    }

    pub(super) fn append_reasoning_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        delta: String,
    ) {
        let previous_phase = runtime.stream_phase;
        let continuing = previous_phase == Some(StreamPhase::Reasoning);
        if !continuing && delta.trim().is_empty() {
            return;
        }
        let now = unix_time_millis();
        if !continuing {
            self.finish_streaming_assistant(session_id);
        }
        if let Some(session) = self.state.session_mut(session_id) {
            if continuing
                && let Some(reasoning) = session
                    .transcript_blocks
                    .last_mut()
                    .and_then(|block| block.activities.last_mut())
                    .and_then(|activity| activity.reasoning.as_mut())
            {
                reasoning.content.push_str(&delta);
                reasoning.finished_at_ms = now;
            } else {
                push_transcript_activity(
                    session,
                    ActivityItem::from_reasoning(
                        ReasoningBlock {
                            content: delta,
                            started_at_ms: now,
                            finished_at_ms: now,
                        },
                        false,
                    ),
                    matches!(
                        previous_phase,
                        Some(StreamPhase::Reasoning | StreamPhase::Activity)
                    ),
                );
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
        let previous_phase = runtime.stream_phase;
        if previous_phase == Some(StreamPhase::Text) {
            self.finish_streaming_assistant(session_id);
        }
        if previous_phase == Some(StreamPhase::Reasoning) {
            self.complete_reasoning_activity(session_id);
        }

        let continuing_work = matches!(
            previous_phase,
            Some(StreamPhase::Reasoning | StreamPhase::Activity)
        );
        if let Some(session) = self.state.session_mut(session_id) {
            for block in session.transcript_blocks.iter_mut().rev() {
                let matching = block.activities.iter_mut().rev().find(|activity| {
                    item.source_id
                        .as_ref()
                        .is_some_and(|id| activity.source_id.as_ref() == Some(id))
                        || (item.source_id.is_none()
                            && activity.title == item.title
                            && !activity.complete)
                });
                if let Some(activity) = matching {
                    let has_arguments = item.arguments.is_some();
                    activity.kind = item.kind;
                    activity.title = item.title;
                    activity.complete = item.complete;
                    activity.failed = item.failed;
                    if item.detail.is_some() {
                        activity.detail = item.detail;
                    }
                    if item.arguments.is_some() {
                        activity.arguments = item.arguments;
                    }
                    if item.output.is_some() {
                        activity.output = item.output;
                    }
                    if !item.image_urls.is_empty() {
                        activity.image_urls = item.image_urls;
                    }
                    if !item.file_changes.is_empty() {
                        activity.file_changes = item.file_changes;
                    }
                    if item.display_target.is_some()
                        && (activity.display_target.is_none() || has_arguments)
                    {
                        activity.display_target = item.display_target;
                    }
                    if item.display_description.is_some()
                        && (activity.display_description.is_none() || has_arguments)
                    {
                        activity.display_description = item.display_description;
                    }
                    if item.reasoning.is_some() {
                        activity.reasoning = item.reasoning;
                    }
                    session.updated_at = unix_time();
                    runtime.stream_phase = Some(StreamPhase::Activity);
                    return;
                }
            }

            push_transcript_activity(session, item, continuing_work);
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Activity);
    }

    pub(super) fn complete_turn_blocks(&mut self, session_id: Uuid) {
        if let Some(session) = self.state.session_mut(session_id) {
            for block in &mut session.transcript_blocks {
                for activity in &mut block.activities {
                    activity.complete = true;
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
        // The turn begins at submission accept, before its prompt has reached
        // any provider. While preparation is still running, a reused runtime
        // could only be draining leftovers of a settled turn — output landing
        // in the new turn then would attribute stale text to it.
        !self.submission_preparations.contains(&session_id)
            && self
                .state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    session.active_turn_id().is_some()
                        && matches!(
                            session.status,
                            SessionStatus::Connecting
                                | SessionStatus::Working
                                | SessionStatus::Waiting
                        )
                })
    }

    /// Returns whether the runtime should remain attached after this event.
    ///
    /// `allow_queue_drain` is false when the caller is flushing buffered
    /// events for a turn the user just stopped: a settling event must not
    /// start queued follow-ups then, because the user asked to stop, not to
    /// continue.
    pub(super) fn handle_driver_event(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        event: DriverEvent,
        allow_queue_drain: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        runtime.last_active_at = Instant::now();
        match event {
            DriverEvent::RuntimeEventCursorAdvanced(cursor) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.runtime_event_cursor = Some(cursor);
                }
            }
            DriverEvent::Connected { provider_cursor } => {
                runtime.last_driver_error = None;
                runtime.last_background_refresh_at = Instant::now();
                runtime.driver.refresh_background_work();
                if let Some(session) = self.state.session_mut(session_id) {
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
            DriverEvent::AgentPresetSelected(agent_preset) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.agent_preset = agent_preset;
                }
            }
            DriverEvent::AutoTitleUpdated(title) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.set_auto_title(title);
                }
            }
            DriverEvent::AvailableCommands(names) => {
                if let Some(session) = self
                    .state
                    .session_mut(session_id)
                    .filter(|session| session.available_commands != names)
                {
                    session.available_commands = names;
                    // The drain has no `Context`; the frame loop rebuilds the
                    // drawn index when it sees this.
                    self.composer_sources_stale = true;
                }
            }
            DriverEvent::TurnStarted => {
                runtime.last_driver_error = None;
                if let Some(session) = self.state.session_mut(session_id)
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
                    let refresh_branch = should_refresh_branch_after_activity(kind, complete)
                        && self.state.selected_session == Some(session_id);
                    let item = ActivityItem::new(id, kind, title, detail, complete);
                    self.observe_foreground_command_activity(session_id, &item);
                    self.update_activity(session_id, runtime, item);
                    if refresh_branch {
                        self.refresh_selected_branch_snapshot(cx);
                    }
                }
            }
            DriverEvent::RichActivity(item) => {
                if self.accepts_turn_output(session_id) {
                    let refresh_branch =
                        should_refresh_branch_after_activity(item.kind, item.complete)
                            && self.state.selected_session == Some(session_id);
                    self.observe_foreground_command_activity(session_id, &item);
                    self.update_activity(session_id, runtime, item);
                    if refresh_branch {
                        self.refresh_selected_branch_snapshot(cx);
                    }
                }
            }
            DriverEvent::BackgroundWork(event) => {
                // Background work is session state, not turn output. It must
                // survive a settled or rewound turn and therefore bypasses
                // `accepts_turn_output` deliberately.
                self.handle_background_work_event(session_id, event);
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
                    if let Some(session) = self.state.session_mut(session_id) {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::UserInputRequested {
                request_id,
                questions,
            } => {
                if self.accepts_turn_output(session_id) && !questions.is_empty() {
                    runtime.pending_user_input = Some(PendingUserInput::new(request_id, questions));
                    if self.state.selected_session == Some(session_id) {
                        self.user_input_answer
                            .update(cx, |input, cx| input.clear(cx));
                    }
                    if let Some(session) = self.state.session_mut(session_id) {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::ComputerUseUpdated(state) => {
                if self.accepts_turn_output(session_id) {
                    Self::upsert_computer_use_preview(runtime, state);
                }
            }
            DriverEvent::SteerAccepted { message } => {
                let submission = runtime
                    .pending_steers
                    .iter()
                    .position(|submission| submission.prompt == message)
                    .and_then(|index| runtime.pending_steers.remove(index))
                    // Providers normally echo the exact transport text, but a
                    // normalized echo still acknowledges the oldest pending
                    // steer. Preserve its attachment presentation metadata.
                    .or_else(|| runtime.pending_steers.pop_front())
                    .unwrap_or_else(|| ComposerSubmission::plain(message.clone()));
                // The provider folded the message into the live turn. Append
                // it to the same turn so the transcript mirrors the provider
                // conversation (no new turn boundary).
                if let Some(session) = self.state.session_mut(session_id) {
                    session.push_user_message_with_presentation(
                        message,
                        submission.display_content,
                        submission.attachments,
                    );
                    session.updated_at = unix_time();
                }
            }
            DriverEvent::SteerRejected { message, reason } => {
                let submission = runtime
                    .pending_steers
                    .iter()
                    .position(|submission| submission.prompt == message)
                    .and_then(|index| runtime.pending_steers.remove(index))
                    .or_else(|| runtime.pending_steers.pop_front())
                    .unwrap_or_else(|| ComposerSubmission::plain(message));
                let (busy, settled_cleanly) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| {
                        let settled_cleanly = session
                            .turns
                            .last()
                            .is_some_and(|turn| turn.status == TurnStatus::Completed);
                        (session.is_busy(), settled_cleanly)
                    })
                    .unwrap_or((false, false));
                if busy {
                    self.enqueue_follow_up_submission(session_id, submission, cx);
                    if self.state.selected_session == Some(session_id) {
                        self.show_toast(tr!(
                            "session.steer_rejected",
                            error = compact_driver_error(&reason)
                        ));
                    }
                } else if settled_cleanly {
                    // The turn settled before the steer arrived; run the
                    // message as a fresh turn instead of losing it. Submission
                    // is deferred through the queue-drain pass because this
                    // session's runtime is detached from the map while its
                    // events are handled — an inline submit would spawn a
                    // second driver process only to have it clobbered when the
                    // drain re-inserts the detached runtime.
                    if let Some(session) = self.state.session_mut(session_id) {
                        session
                            .queued_messages
                            .insert(0, submission.into_queued_message());
                    }
                    if allow_queue_drain {
                        self.pending_queue_drains.push(session_id);
                    }
                } else {
                    // The user stopped the turn (or the provider died) before
                    // the steer landed. Keep the message visible and
                    // user-controlled instead of auto-running it.
                    self.enqueue_follow_up_submission(session_id, submission, cx);
                }
            }
            DriverEvent::PlanUsageUpdated(usage) => {
                if let Some(provider) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| session.provider)
                {
                    self.plan_usage.insert(provider, usage);
                }
            }
            DriverEvent::UsageUpdated {
                context_tokens,
                context_window,
            } => {
                // Meta about the conversation, not turn output: it applies
                // even while a rewound or cancelled turn's tail drains.
                if let Some(session) = self.state.session_mut(session_id) {
                    let usage = session.context_usage.get_or_insert(ContextUsage::default());
                    if let Some(tokens) = context_tokens {
                        usage.tokens = tokens;
                    }
                    if let Some(window) = context_window {
                        usage.window = Some(window);
                    }
                    self.state.mark_session_dirty(session_id);
                }
            }
            DriverEvent::TurnFinished { success, summary } => {
                self.settle_foreground_work(
                    session_id,
                    if success {
                        BackgroundWorkStatus::Completed
                    } else {
                        BackgroundWorkStatus::Failed
                    },
                );
                let previous_kinds = self.snapshot_selected_transcript_rows(session_id);
                runtime.last_driver_error = None;
                // A settled turn moved the account's rate-limit needles; ask
                // that provider's plan meter to refresh once its backoff
                // allows.
                if let Some(provider) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| session.provider)
                    .filter(|provider| usage_meter::PLAN_USAGE_PROVIDERS.contains(provider))
                {
                    self.plan_usage_stale.insert(provider);
                }
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
                let task_notification = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .filter(|session| {
                        cx.active_window().is_none() && session.originating_automation.is_none()
                    })
                    .map(|session| {
                        let title = if session.display_title() == AgentSession::DEFAULT_TITLE {
                            tr!("session.new_task")
                        } else {
                            session.display_title().to_owned()
                        };
                        let body = if success {
                            tr!("session.turn_completed")
                        } else {
                            tr!("session.stopped")
                        };
                        (title, body)
                    });
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                let needs_fallback = !self.turn_has_assistant_message(session_id);
                if let Some(session) = self.state.session_mut(session_id) {
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
                                    tr!("session.turn_completed")
                                } else {
                                    tr!("session.stopped_before_response")
                                }
                            }),
                        );
                    }
                }
                self.finish_active_turn_with_analytics(
                    session_id,
                    if success {
                        TurnStatus::Completed
                    } else {
                        TurnStatus::Failed
                    },
                    if success {
                        crate::analytics::TurnOutcome::Completed
                    } else {
                        crate::analytics::TurnOutcome::Failed
                    },
                );
                runtime.pending_permission = None;
                runtime.pending_user_input = None;
                runtime.pending_computer_approval = None;
                runtime.driver.cancel_computer_use();
                // The agent may have edited files or switched branches, so the
                // cached view of the workspace is no longer trustworthy. This
                // handler has no `Context`, so the drain loop acts on the flag.
                if self.state.selected_session == Some(session_id) {
                    self.workspace_queries_stale = true;
                }
                runtime.computer_use_previews.clear();
                runtime.driver.refresh_background_work();
                self.capture_latest_turn_checkpoint_for(session_id);
                if allow_queue_drain && success {
                    // Start the next queued follow-up once the runtime has
                    // been re-inserted so the same process is reused.
                    self.pending_queue_drains.push(session_id);
                }
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
                if let Some((title, body)) = task_notification {
                    crate::platform::show_task_notification(
                        &task_notification_tag(session_id),
                        &title,
                        &body,
                        cx,
                    );
                }
                // Keep the local automation projection aligned with the
                // daemon's durable run outcome. Notification policy is emitted
                // by the daemon; this path only settles local state.
                self.settle_automation_run(
                    session_id,
                    if success {
                        crate::automation::RunOutcome::Succeeded
                    } else {
                        crate::automation::RunOutcome::Failed
                    },
                    cx,
                );
            }
            DriverEvent::Error(error) => {
                let error = compact_driver_error(&error);
                runtime.last_driver_error = Some(error.clone());
                if self.state.selected_session == Some(session_id) {
                    self.show_toast(error.clone());
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
                if let Some(session) = self.state.session_mut(session_id)
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
                self.mark_background_work_lost(session_id);
                let previous_kinds = self.snapshot_selected_transcript_rows(session_id);
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                runtime.pending_permission = None;
                runtime.pending_user_input = None;
                runtime.pending_computer_approval = None;
                runtime.driver.cancel_computer_use();
                runtime.computer_use_previews.clear();
                let needs_fallback = !self.turn_has_assistant_message(session_id);
                let failure_message = runtime
                    .last_driver_error
                    .take()
                    .unwrap_or_else(|| tr!("session.codex_exited_before_response"));
                let should_finish_turn = if let Some(session) = self.state.session_mut(session_id)
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    ) {
                    session.status = SessionStatus::Failed;
                    session.updated_at = unix_time();
                    if needs_fallback {
                        session.push_message(MessageRole::Assistant, failure_message);
                    }
                    true
                } else {
                    false
                };
                let finished_turn = should_finish_turn
                    && self
                        .finish_active_turn_with_analytics(
                            session_id,
                            TurnStatus::Failed,
                            crate::analytics::TurnOutcome::ProcessExited,
                        )
                        .is_some();
                if finished_turn {
                    self.capture_latest_turn_checkpoint_for(session_id);
                    // A crashed provider is a failed automation run.
                    self.settle_automation_run(
                        session_id,
                        crate::automation::RunOutcome::Failed,
                        cx,
                    );
                }
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
                return false;
            }
        }
        true
    }

    fn upsert_computer_use_preview(runtime: &mut SessionRuntime, state: ComputerUseState) {
        if !state.visible {
            return;
        }
        let Some(window_id) = state.target.as_ref().map(|target| target.window_id) else {
            return;
        };
        let mut preview = ComputerUsePreview {
            target: state.target,
            phase: state.phase,
            visible: state.visible,
            screenshot: state.image_url.as_deref().and_then(|image_url| {
                crate::computer_use::decode_preview_image_url(image_url).ok()
            }),
        };
        if let Some(index) = runtime.computer_use_previews.iter().position(|preview| {
            preview
                .target
                .as_ref()
                .is_some_and(|target| target.window_id == window_id)
        }) {
            let previous = runtime.computer_use_previews.remove(index);
            if preview.screenshot.is_none() {
                preview.screenshot = previous.screenshot;
            }
        }
        runtime.computer_use_previews.push(preview);
    }
}

/// A completed edit or shell command is the earliest provider-neutral point at
/// which its filesystem effects are stable enough to re-read. The actual Git
/// work remains behind the branch cache's background fetch.
pub(super) fn should_refresh_branch_after_activity(
    kind: crate::model::ActivityKind,
    complete: bool,
) -> bool {
    complete
        && matches!(
            kind,
            crate::model::ActivityKind::Command | crate::model::ActivityKind::FileChange
        )
}

pub(super) fn push_transcript_activity(
    session: &mut AgentSession,
    item: ActivityItem,
    continuing_work: bool,
) {
    let after_message = session.messages.len();
    let turn_id = session.active_turn_id();
    if continuing_work
        && let Some(block) = session.transcript_blocks.last_mut()
        && block.after_message == after_message
        && block.turn_id == turn_id
    {
        block.activities.push(item);
    } else {
        session.transcript_blocks.push(TranscriptBlock {
            after_message,
            turn_id,
            activities: vec![item],
        });
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

pub(super) fn compact_driver_error(error: &str) -> String {
    const MAX_LINES: usize = 6;
    const MAX_CHARS: usize = 800;

    let lines = error.lines().collect::<Vec<_>>();
    let mut compact = lines
        .iter()
        .take(MAX_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > MAX_LINES {
        compact.push_str("\n…");
    }
    if compact.chars().count() > MAX_CHARS {
        compact = compact.chars().take(MAX_CHARS - 1).collect();
        compact.push('…');
    }
    compact
}

/// Coalesce every adjacent delta of one kind while retaining provider order.
/// Runtime cursors are acknowledgements rather than visible boundaries, so the
/// newest cursor follows the combined delta. The full text enters layout in
/// this pass; Markdown's paint-only veil provides the progressive dissolve.
pub(super) fn pop_stream_batch(
    events: &mut VecDeque<DriverEvent>,
    kind: StreamDeltaKind,
) -> Option<DriverEvent> {
    let mut chunk = String::new();
    let mut latest_cursor = None;
    loop {
        match events.front() {
            Some(DriverEvent::RuntimeEventCursorAdvanced(_)) => {
                latest_cursor = events.pop_front();
            }
            Some(event) if stream_delta_text(event, kind).is_some() => {
                let event = events.pop_front()?;
                match (kind, event) {
                    (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
                    | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => {
                        chunk.push_str(&text);
                    }
                    _ => unreachable!("the stream kind was checked before removing the event"),
                }
            }
            _ => break,
        }
    }
    if let Some(cursor) = latest_cursor {
        events.push_front(cursor);
    }
    match kind {
        StreamDeltaKind::Text => Some(DriverEvent::TextDelta(chunk)),
        StreamDeltaKind::Reasoning => Some(DriverEvent::ReasoningDelta(chunk)),
    }
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

use super::*;

impl Waku {
    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus()
    }

    pub(super) fn selected_project(&self) -> Option<&Project> {
        let id = self.state.selected_project?;
        self.state.projects.iter().find(|project| project.id == id)
    }

    pub(super) fn selected_session(&self) -> Option<&AgentSession> {
        let id = self.state.selected_session?;
        self.state.sessions.iter().find(|session| session.id == id)
    }

    pub(super) fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
    }

    pub(super) fn selected_runtime(&self) -> Option<&SessionRuntime> {
        self.runtimes.get(&self.state.selected_session?)
    }

    pub(super) fn provider_probe(&self, provider: ProviderKind) -> Option<&ProviderProbe> {
        self.probes.iter().find(|probe| probe.provider == provider)
    }

    pub(super) fn request_provider_model_discovery(&mut self, provider: ProviderKind) {
        if !provider.supports_model_discovery()
            || self.provider_model_discoveries.contains(&provider)
        {
            return;
        }
        let Some(probe) = self
            .provider_probe(provider)
            .filter(|probe| probe.installed)
            .cloned()
        else {
            return;
        };
        self.provider_model_discoveries.insert(provider);
        self.provider_model_discoveries_pending.insert(provider);
        let provider_probe_tx = self.provider_probe_tx.clone();
        if std::thread::Builder::new()
            .name(format!("waku-{}-model-discovery", provider.id()))
            .spawn(move || {
                let _ = provider_probe_tx.send(probe.discover_models());
            })
            .is_err()
        {
            self.provider_model_discoveries.remove(&provider);
            self.provider_model_discoveries_pending.remove(&provider);
        }
    }

    pub(super) fn model_for_session<'a>(&'a self, session: &'a AgentSession) -> Option<&'a str> {
        session.model.as_deref().or_else(|| {
            self.provider_probe(session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.as_str())
        })
    }

    pub(super) fn model_display_name(&self, provider: ProviderKind, model: Option<&str>) -> String {
        let Some(model) = model else {
            return provider.short_name().to_owned();
        };
        self.provider_probe(provider)
            .and_then(|probe| probe.models.iter().find(|candidate| candidate.id == model))
            .map(|candidate| candidate.name.clone())
            .unwrap_or_else(|| model.to_owned())
    }

    pub(super) fn model_metadata_for_session(
        &self,
        session: &AgentSession,
    ) -> Option<&ProviderModel> {
        let model = self.model_for_session(session)?;
        self.provider_probe(session.provider)?
            .models
            .iter()
            .find(|candidate| candidate.id == model)
    }

    pub(super) fn selected_transcript_blocks(&self) -> &[TranscriptBlock] {
        self.selected_session()
            .map(|session| session.transcript_blocks.as_slice())
            .unwrap_or(&[])
    }

    pub(super) fn save(&mut self) {
        self.last_stream_save = Instant::now();
        if let Err(error) = self.store.save(&self.state) {
            self.toast = Some(format!("Could not save local state: {error}"));
        } else {
            self.stream_state_dirty = false;
        }
    }

    pub(super) fn capture_latest_turn_checkpoint(&mut self) {
        if let Some(session_id) = self.state.selected_session {
            self.capture_latest_turn_checkpoint_for(session_id);
        }
    }

    pub(super) fn capture_latest_turn_checkpoint_for(&mut self, session_id: Uuid) {
        let Some((project_id, turn_count)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .last()
                    .filter(|turn| turn.status != TurnStatus::Running)
                    .map(|turn| (session.project_id, turn.turn_count))
            })
        else {
            return;
        };
        let Some(project_path) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return;
        };

        let checkpoint = match checkpoint::capture_turn(&project_path, session_id, turn_count) {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.toast = Some(format!("Could not capture the turn checkpoint: {error}"));
                Checkpoint {
                    turn_count,
                    git_ref: checkpoint::checkpoint_ref(session_id, turn_count),
                    status: CheckpointStatus::Error,
                    files: Vec::new(),
                    created_at: unix_time(),
                }
            }
        };
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && let Some(turn) = session
                .turns
                .iter_mut()
                .find(|turn| turn.turn_count == turn_count)
        {
            turn.checkpoint = Some(checkpoint);
        }
    }

    pub(super) fn fork_session_from_response(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            self.toast = Some("That response is no longer available.".into());
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id)
            || !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed)
            || !source.provider.supports_conversation_fork()
            || source
                .turns
                .get(turn_count.saturating_sub(1))
                .is_none_or(|turn| turn.turn_count != turn_count || !turn.provider_turn_started)
        {
            self.toast = Some("That response cannot be forked right now.".into());
            cx.notify();
            return;
        }

        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let turns_to_remove = source.provider_turns_after(turn_count);
        let native_fork = (|| -> anyhow::Result<(
            ProviderResumeCursor,
            Option<std::collections::HashMap<String, String>>,
        )> {
            match source.provider {
                ProviderKind::Claude => {
                    let ProviderResumeCursor::Claude {
                        session_id: native_session_id,
                        ..
                    } = source
                        .provider_cursor
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Claude's native session is unavailable"))?
                    else {
                        anyhow::bail!("Claude's native session is unavailable");
                    };
                    let resume_at = source.turns[turn_count - 1]
                        .provider_resume_at
                        .clone()
                        .map(Ok)
                        .unwrap_or_else(|| {
                            crate::claude_session::message_id_for_turn(
                                native_session_id,
                                provider_turn_count,
                            )
                        })?;
                    let fork = crate::claude_session::fork_session_at(
                        native_session_id,
                        &resume_at,
                        &format!("{} (fork)", source.title),
                    )?;
                    let fork_resume_at = fork
                        .message_ids
                        .get(&resume_at)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Claude omitted the fork checkpoint"))?;
                    Ok((
                        ProviderResumeCursor::Claude {
                            session_id: fork.session_id,
                            resume_at: Some(fork_resume_at),
                        },
                        Some(fork.message_ids),
                    ))
                }
                ProviderKind::Codex => {
                    if !matches!(
                        source.provider_cursor.as_ref(),
                        Some(ProviderResumeCursor::Codex { .. })
                    ) {
                        anyhow::bail!("Codex's native thread is unavailable");
                    }
                    Ok((self.ensure_driver()?.fork(turns_to_remove)?, None))
                }
                ProviderKind::Cursor => Ok((
                    crate::cursor_session::fork_session_at_turn(&source, turn_count)?,
                    None,
                )),
                ProviderKind::Amp => {
                    let Some(ProviderResumeCursor::Amp {
                        thread_id: native_thread_id,
                        fork_context,
                    }) = source.provider_cursor.as_ref()
                    else {
                        anyhow::bail!("Amp's native thread is unavailable");
                    };
                    let project_path = self
                        .state
                        .projects
                        .iter()
                        .find(|project| project.id == source.project_id)
                        .map(|project| project.path.as_path())
                        .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
                    let binary = self
                        .probes
                        .iter()
                        .find(|probe| probe.provider == ProviderKind::Amp)
                        .and_then(|probe| probe.path.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("Amp is not installed"))?;
                    Ok((
                        crate::amp_session::fork_session_at_turn(
                            binary,
                            project_path,
                            native_thread_id,
                            fork_context.as_deref(),
                            provider_turn_count,
                        )?,
                        None,
                    ))
                }
                ProviderKind::OpenCode => {
                    let Some(ProviderResumeCursor::OpenCode {
                        session_id: native_session_id,
                    }) = source.provider_cursor.as_ref()
                    else {
                        anyhow::bail!("OpenCode's native session is unavailable");
                    };
                    let project_path = self
                        .state
                        .projects
                        .iter()
                        .find(|project| project.id == source.project_id)
                        .map(|project| project.path.as_path())
                        .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
                    let binary = self
                        .probes
                        .iter()
                        .find(|probe| probe.provider == ProviderKind::OpenCode)
                        .and_then(|probe| probe.path.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("OpenCode is not installed"))?;
                    Ok((
                        crate::opencode_session::fork_session_at_turn(
                            binary,
                            project_path,
                            native_session_id,
                            provider_turn_count,
                        )?,
                        None,
                    ))
                }
                ProviderKind::Grok => {
                    let Some(ProviderResumeCursor::Grok {
                        session_id: native_session_id,
                    }) = source.provider_cursor.as_ref()
                    else {
                        anyhow::bail!("Grok's native session is unavailable");
                    };
                    let project_path = self
                        .state
                        .projects
                        .iter()
                        .find(|project| project.id == source.project_id)
                        .map(|project| project.path.as_path())
                        .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
                    let binary = self
                        .probes
                        .iter()
                        .find(|probe| probe.provider == ProviderKind::Grok)
                        .and_then(|probe| probe.path.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("Grok Build is not installed"))?;
                    Ok((
                        crate::grok_session::fork_session_at_turn(
                            binary,
                            project_path,
                            native_session_id,
                            provider_turn_count,
                        )?,
                        None,
                    ))
                }
                ProviderKind::Pi => {
                    if !matches!(
                        source.provider_cursor.as_ref(),
                        Some(ProviderResumeCursor::Pi {
                            session_file: Some(_),
                            ..
                        })
                    ) {
                        anyhow::bail!("Pi's native session file is unavailable");
                    }
                    Ok((self.ensure_driver()?.fork(turns_to_remove)?, None))
                }
            }
        })();

        let (provider_cursor, claude_message_ids) = match native_fork {
            Ok(fork) => fork,
            Err(error) => {
                if source.provider == ProviderKind::Pi {
                    // A failed restore after Pi creates a fork can leave the RPC
                    // process on that fork. Recreate it from the source cursor.
                    self.runtimes.remove(&session_id);
                }
                self.toast = Some(format!("Could not fork the task: {error}"));
                cx.notify();
                return;
            }
        };
        let Some(mut forked) = source.fork_through_turn(turn_count, provider_cursor) else {
            self.toast = Some("That response could not be copied into a new task.".into());
            cx.notify();
            return;
        };
        if let Some(message_ids) = claude_message_ids {
            for turn in &mut forked.turns {
                if let Some(message_id) = turn.provider_resume_at.as_mut()
                    && let Some(remapped) = message_ids.get(message_id)
                {
                    *message_id = remapped.clone();
                }
            }
        }

        let fork_id = forked.id;
        for turn in &mut forked.turns {
            if let Some(checkpoint) = turn.checkpoint.as_mut() {
                checkpoint.git_ref = checkpoint::checkpoint_ref(fork_id, checkpoint.turn_count);
            }
        }
        let checkpoint_warning = self
            .state
            .projects
            .iter()
            .find(|project| project.id == source.project_id)
            .and_then(|project| {
                checkpoint::copy_session_refs(&project.path, source.id, fork_id, turn_count).err()
            });

        self.state.sessions.push(forked);
        self.select_session(fork_id, cx);
        self.toast = Some(match checkpoint_warning {
            Some(error) => {
                format!("Forked task; some Git checkpoints could not be copied: {error}")
            }
            None => "Forked task from this response.".into(),
        });
        self.save();
        cx.notify();
    }

    pub(super) fn begin_message_edit(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((message_index, initial_message)) = self
            .state
            .sessions
            .iter()
            .find(|session| {
                session.id == session_id
                    && session.provider.supports_conversation_rollback()
                    && matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
            })
            .and_then(|session| {
                let turn = session
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)?;
                session
                    .messages
                    .iter()
                    .enumerate()
                    .find_map(|(index, message)| {
                        (message.turn_id == Some(turn.id) && message.role == MessageRole::User)
                            .then(|| (index, message.content.clone()))
                    })
            })
        else {
            self.toast = Some("That message is not editable right now.".into());
            cx.notify();
            return;
        };

        let input = cx.new(|cx| ComposerInput::new(window, cx));
        input.update(cx, |input, cx| input.set_content(initial_message, cx));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        cx.subscribe(
            &input,
            |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Submit(_) => this.submit_message_edit(cx),
            },
        )
        .detach();
        self.message_edit = Some(MessageEdit {
            session_id,
            turn_count,
            input: input.clone(),
        });
        self.toast = None;
        self.remeasure_transcript_message(message_index);
        let focus_handle = input.read(cx).focus();
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.take() else {
            return;
        };
        let message_index = self.selected_session().and_then(|session| {
            let turn_id = session
                .turns
                .iter()
                .find(|turn| turn.turn_count == edit.turn_count)?
                .id;
            session.messages.iter().position(|message| {
                message.turn_id == Some(turn_id) && message.role == MessageRole::User
            })
        });
        if let Some(message_index) = message_index {
            self.remeasure_transcript_message(message_index);
        }
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn submit_message_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.clone() else {
            return;
        };
        let prompt = edit.input.read(cx).content().trim().to_owned();
        if prompt.is_empty() {
            self.toast = Some("The edited message cannot be empty.".into());
            cx.notify();
            return;
        }
        if !self.rewind_before_turn(edit.session_id, edit.turn_count, cx) {
            return;
        }
        self.message_edit = None;
        self.submit_prompt(prompt, cx);
    }

    fn rewind_before_turn(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let retained_turn_count = turn_count.saturating_sub(1);
        let Some((
            project_id,
            provider,
            status,
            provider_cursor,
            previous_turn_count,
            rollback_turns,
            provider_turn_count,
            provider_resume_at,
            session_title,
        )) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)
                    .map(|_| {
                        (
                            session.project_id,
                            session.provider,
                            session.status,
                            session.provider_cursor.clone(),
                            session.turns.len(),
                            session.provider_turns_after(retained_turn_count),
                            session
                                .turns
                                .iter()
                                .take(retained_turn_count)
                                .filter(|turn| turn.provider_turn_started)
                                .count(),
                            retained_turn_count
                                .checked_sub(1)
                                .and_then(|index| session.turns.get(index))
                                .and_then(|turn| turn.provider_resume_at.clone()),
                            session.title.clone(),
                        )
                    })
            })
        else {
            self.toast = Some("That message is no longer available.".into());
            cx.notify();
            return false;
        };
        if self.state.selected_session != Some(session_id) {
            self.toast = Some("Select the task before rewinding its conversation.".into());
            cx.notify();
            return false;
        }
        if !matches!(status, SessionStatus::Idle | SessionStatus::Failed) {
            self.toast = Some("Stop the current turn before rewinding the conversation.".into());
            cx.notify();
            return false;
        }
        if !provider.supports_conversation_rollback()
            || (rollback_turns > 0 && provider_cursor.is_none())
        {
            self.toast = Some(format!(
                "{} cannot safely roll back its native conversation yet.",
                provider.display_name()
            ));
            cx.notify();
            return false;
        }
        let Some(project_path) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            self.toast = Some("The task's project could not be found.".into());
            cx.notify();
            return false;
        };
        let checkpoint_ref = checkpoint::checkpoint_ref(session_id, retained_turn_count);
        if !checkpoint::has_ref(&project_path, &checkpoint_ref) {
            self.toast = Some("The message's pre-turn Git checkpoint is missing.".into());
            cx.notify();
            return false;
        }

        let claude_reset =
            provider == ProviderKind::Claude && rollback_turns > 0 && retained_turn_count == 0;
        let cursor_reset =
            provider == ProviderKind::Cursor && rollback_turns > 0 && retained_turn_count == 0;
        let grok_reset =
            provider == ProviderKind::Grok && rollback_turns > 0 && retained_turn_count == 0;
        let claude_rollback =
            if provider == ProviderKind::Claude && rollback_turns > 0 && retained_turn_count > 0 {
                let Some(ProviderResumeCursor::Claude {
                    session_id: native_session_id,
                    ..
                }) = provider_cursor.as_ref()
                else {
                    self.toast = Some("Claude's native session cursor is unavailable.".into());
                    cx.notify();
                    return false;
                };
                let resume_at = match provider_resume_at {
                    Some(message_id) => message_id,
                    None => match crate::claude_session::message_id_for_turn(
                        native_session_id,
                        provider_turn_count,
                    ) {
                        Ok(message_id) => message_id,
                        Err(error) => {
                            self.toast = Some(format!(
                                "Claude's native checkpoint for that turn is unavailable: {error}"
                            ));
                            cx.notify();
                            return false;
                        }
                    },
                };
                Some((native_session_id.clone(), resume_at))
            } else {
                None
            };

        let safety_ref = format!("refs/waku/revert-backup-{session_id}-{}", Uuid::new_v4());
        if let Err(error) = checkpoint::capture_ref(&project_path, &safety_ref) {
            self.toast = Some(format!(
                "Could not create a rewind safety snapshot: {error}"
            ));
            cx.notify();
            return false;
        }
        if let Err(error) = checkpoint::restore_ref(&project_path, &checkpoint_ref) {
            self.toast = Some(match checkpoint::restore_ref(&project_path, &safety_ref) {
                Ok(()) => {
                    let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                    format!("Could not restore the checkpoint: {error}")
                }
                Err(restore_error) => format!(
                    "Checkpoint restore failed ({error}); safety restore also failed ({restore_error}). Recovery ref retained at {safety_ref}."
                ),
            });
            cx.notify();
            return false;
        }

        let mut claude_fork = None;
        let mut provider_rewind_cursor = None;
        if rollback_turns > 0 && !claude_reset && !cursor_reset && !grok_reset {
            let rollback_result = if let Some((native_session_id, resume_at)) = &claude_rollback {
                crate::claude_session::fork_session_at(
                    native_session_id,
                    resume_at,
                    &format!("{session_title} (rewind)"),
                )
                .map(|fork| {
                    claude_fork = Some((fork, resume_at.to_owned()));
                })
            } else if provider == ProviderKind::OpenCode {
                let Some(ProviderResumeCursor::OpenCode {
                    session_id: native_session_id,
                }) = provider_cursor.as_ref()
                else {
                    self.toast = Some("OpenCode's native session cursor is unavailable.".into());
                    cx.notify();
                    return false;
                };
                let Some(binary) = self
                    .probes
                    .iter()
                    .find(|probe| probe.provider == ProviderKind::OpenCode)
                    .and_then(|probe| probe.path.as_deref())
                else {
                    self.toast = Some("OpenCode is not installed or could not be found.".into());
                    cx.notify();
                    return false;
                };
                crate::opencode_session::fork_session_at_turn(
                    binary,
                    &project_path,
                    native_session_id,
                    provider_turn_count,
                )
                .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else if provider == ProviderKind::Amp {
                let Some(ProviderResumeCursor::Amp {
                    thread_id: native_thread_id,
                    fork_context,
                }) = provider_cursor.as_ref()
                else {
                    self.toast = Some("Amp's native thread cursor is unavailable.".into());
                    cx.notify();
                    return false;
                };
                let Some(binary) = self
                    .probes
                    .iter()
                    .find(|probe| probe.provider == ProviderKind::Amp)
                    .and_then(|probe| probe.path.as_deref())
                else {
                    self.toast = Some("Amp is not installed or could not be found.".into());
                    cx.notify();
                    return false;
                };
                crate::amp_session::fork_session_at_turn(
                    binary,
                    &project_path,
                    native_thread_id,
                    fork_context.as_deref(),
                    provider_turn_count,
                )
                .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else if provider == ProviderKind::Cursor {
                let Some(source) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                else {
                    self.toast = Some("Cursor's Waku task is unavailable.".into());
                    cx.notify();
                    return false;
                };
                crate::cursor_session::fork_session_at_turn(source, retained_turn_count)
                    .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else if provider == ProviderKind::Grok {
                let Some(ProviderResumeCursor::Grok {
                    session_id: native_session_id,
                }) = provider_cursor.as_ref()
                else {
                    self.toast = Some("Grok's native session cursor is unavailable.".into());
                    cx.notify();
                    return false;
                };
                let Some(binary) = self
                    .probes
                    .iter()
                    .find(|probe| probe.provider == ProviderKind::Grok)
                    .and_then(|probe| probe.path.as_deref())
                else {
                    self.toast = Some("Grok Build is not installed or could not be found.".into());
                    cx.notify();
                    return false;
                };
                crate::grok_session::fork_session_at_turn(
                    binary,
                    &project_path,
                    native_session_id,
                    provider_turn_count,
                )
                .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else {
                self.ensure_driver()
                    .and_then(|driver| driver.rollback(rollback_turns))
                    .map(|cursor| provider_rewind_cursor = cursor)
            };
            if let Err(error) = rollback_result {
                let restore_result = checkpoint::restore_ref(&project_path, &safety_ref);
                self.toast = Some(match restore_result {
                    Ok(()) => {
                        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                        format!(
                            "The provider rejected the rollback, so the workspace was restored: {error}"
                        )
                    }
                    Err(restore_error) => format!(
                        "Provider rollback failed ({error}) and the safety snapshot could not be restored ({restore_error}). Recovery ref retained at {safety_ref}."
                    ),
                });
                cx.notify();
                return false;
            }
        }

        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
        let cleanup_result = checkpoint::delete_turn_refs_after(
            &project_path,
            session_id,
            retained_turn_count,
            previous_turn_count,
        );
        self.sync_transcript_rows();
        let previous_kinds = self.transcript_row_kinds.borrow().clone();
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some((fork, source_resume_at)) = &claude_fork {
                for turn in session.turns.iter_mut().take(retained_turn_count) {
                    if let Some(remapped) = turn
                        .provider_resume_at
                        .as_ref()
                        .and_then(|message_id| fork.message_ids.get(message_id))
                        .cloned()
                    {
                        turn.provider_resume_at = Some(remapped);
                    }
                }
                let remapped_resume_at = fork
                    .message_ids
                    .get(source_resume_at)
                    .cloned()
                    .expect("the Claude fork includes its target message");
                session.provider_cursor = Some(ProviderResumeCursor::Claude {
                    session_id: fork.session_id.clone(),
                    resume_at: Some(remapped_resume_at),
                });
            } else if claude_reset || cursor_reset || grok_reset {
                session.provider_cursor = None;
            } else if let Some(cursor) = provider_rewind_cursor.clone() {
                session.provider_cursor = Some(cursor);
            }
            session.truncate_after_turn(retained_turn_count);
            session.status = SessionStatus::Idle;
        }
        if claude_fork.is_some()
            || claude_reset
            || cursor_reset
            || grok_reset
            || (matches!(
                provider,
                ProviderKind::Amp
                    | ProviderKind::Cursor
                    | ProviderKind::OpenCode
                    | ProviderKind::Grok
            ) && provider_rewind_cursor.is_some())
        {
            // Headless drivers retain their original native session ID. Recreate
            // them lazily so the next prompt resumes the fork instead.
            self.runtimes.remove(&session_id);
        } else if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
            runtime.pending_computer_approval = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.splice_transcript_rows_after_visibility_change(&previous_kinds);
        self.toast = Some(match cleanup_result {
            Ok(()) => format!("Rewound to before turn {turn_count}."),
            Err(error) => {
                format!("Rewound to before turn {turn_count}; stale refs remain: {error}")
            }
        });
        self.save();
        cx.notify();
        true
    }

    pub(super) fn ensure_driver(&mut self) -> anyhow::Result<DriverHandle> {
        let session = self
            .selected_session()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No session selected"))?;
        if let Some(runtime) = self.runtimes.get(&session.id) {
            return Ok(runtime.driver.clone());
        }
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
        let binary = self
            .probes
            .iter()
            .find(|probe| probe.provider == session.provider)
            .and_then(|probe| probe.path.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is not installed or could not be found",
                    session.provider.display_name()
                )
            })?;
        let model = session.model.clone().or_else(|| {
            self.probes
                .iter()
                .find(|probe| probe.provider == session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.clone())
        });
        let model_metadata = self.model_metadata_for_session(&session);
        let reasoning_effort = session.reasoning_effort.clone().filter(|effort| {
            model_metadata.is_some_and(|model| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *effort)
            })
        });
        let service_tier = session.service_tier.clone().filter(|tier| {
            tier == "default"
                || model_metadata.is_some_and(|model| {
                    model.service_tiers.iter().any(|option| option.id == *tier)
                })
        });
        let (event_tx, event_rx) = unbounded();
        let handle = driver::start(
            session.provider,
            DriverStartOptions {
                binary,
                cwd: project.path.clone(),
                mode: session.runtime_mode,
                interaction_mode: session.interaction_mode,
                model,
                reasoning_effort,
                service_tier,
                computer_use_enabled: self.state.computer_use_enabled,
                provider_cursor: session.provider_cursor.clone(),
            },
            event_tx,
        )?;
        self.runtimes.insert(
            session.id,
            SessionRuntime {
                driver: handle.clone(),
                events: event_rx,
                pending_events: VecDeque::new(),
                stream_phase: None,
                stream_remeasure_pending: false,
                pending_permission: None,
                pending_computer_approval: None,
                computer_use_previews: Vec::new(),
                computer_session_grants: HashSet::new(),
                last_driver_error: None,
            },
        );
        Ok(handle)
    }

    pub(super) fn submit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some((session_id, project_id, status, next_turn_count)) =
            self.selected_session().map(|session| {
                (
                    session.id,
                    session.project_id,
                    session.status,
                    session.turns.len() + 1,
                )
            })
        else {
            return;
        };
        if matches!(
            status,
            SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
        ) {
            self.toast = Some("The agent is already working. Stop it before sending again.".into());
            cx.notify();
            return;
        }
        self.sync_transcript_rows();
        let previous_kinds = self.transcript_row_kinds.borrow().clone();
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone());
        let checkpoint_warning = project_path.as_deref().and_then(|path| {
            let baseline_count = next_turn_count - 1;
            let git_ref = checkpoint::checkpoint_ref(session_id, baseline_count);
            (!checkpoint::has_ref(path, &git_ref))
                .then(|| checkpoint::capture_turn(path, session_id, baseline_count).err())
                .flatten()
                .map(|error| format!("Could not capture the pre-turn checkpoint: {error}"))
        });
        let transcript_anchor = if let Some(session) = self.selected_session_mut() {
            session.set_title_from_prompt(&prompt);
            let turn_id = session.begin_turn(&prompt);
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            Some(TranscriptAnchor {
                session_id,
                turn_id,
            })
        } else {
            None
        };
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
            runtime.pending_computer_approval = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.message_edit = None;
        self.toast = checkpoint_warning;
        self.transcript_anchor.set(transcript_anchor);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(true);
        self.splice_transcript_rows_after_visibility_change(&previous_kinds);
        self.scroll_transcript_to_anchor();
        let mut failed_to_start = false;
        match self.ensure_driver() {
            Ok(driver) => driver.prompt(prompt),
            Err(error) => {
                failed_to_start = true;
                let message = format!("Could not start the agent: {error}");
                if let Some(session) = self.selected_session_mut() {
                    session.status = SessionStatus::Failed;
                    session.push_message(MessageRole::Assistant, message);
                    session.finish_active_turn(TurnStatus::Failed);
                }
            }
        }
        if failed_to_start {
            self.capture_latest_turn_checkpoint();
        }
        self.save();
        cx.notify();
    }

    pub(super) fn collect_runtime_events(runtime: &mut SessionRuntime) {
        while let Ok(event) = runtime.events.try_recv() {
            runtime.pending_events.push_back(event);
        }
    }

    pub(super) fn drain_provider_probe_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(probe) = self.provider_probe_events.try_recv() {
            self.provider_model_discoveries_pending
                .remove(&probe.provider);
            if let Some(existing) = self
                .probes
                .iter_mut()
                .find(|existing| existing.provider == probe.provider)
            {
                *existing = probe;
            } else {
                self.probes.push(probe);
            }
            changed = true;
        }
        changed
    }

    pub(super) fn drain_computer_permission_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.computer_permission_events.try_recv() {
            self.computer_permission_request_pending = false;
            match result {
                Ok(permissions) => self.computer_permissions = permissions,
                Err(error) => self.toast = Some(error),
            }
            changed = true;
        }
        changed
    }

    pub(super) fn drain_driver_events(&mut self) -> bool {
        let session_ids = self.runtimes.keys().copied().collect::<Vec<_>>();
        let mut changed = false;
        let mut force_save = false;
        let mut selected_changed = false;
        for session_id in session_ids {
            let Some(mut runtime) = self.runtimes.remove(&session_id) else {
                continue;
            };
            let follow_up_remeasure = std::mem::take(&mut runtime.stream_remeasure_pending);
            Self::collect_runtime_events(&mut runtime);
            let mut runtime_changed = false;
            let mut markdown_changed = false;
            let mut revealed_stream_chunk = false;
            let mut keep_runtime = true;
            while let Some(event) = runtime.pending_events.front() {
                let kind = stream_delta_kind(event);
                if kind.is_some() && revealed_stream_chunk {
                    break;
                }

                let event = if let Some(kind) = kind {
                    revealed_stream_chunk = true;
                    pop_stream_chunk(&mut runtime.pending_events, kind)
                } else {
                    runtime.pending_events.pop_front()
                };
                let Some(event) = event else {
                    break;
                };
                force_save |= matches!(
                    event,
                    DriverEvent::Connected { .. }
                        | DriverEvent::Permission { .. }
                        | DriverEvent::TurnFinished { .. }
                        | DriverEvent::Error(_)
                        | DriverEvent::ProcessExited
                );
                markdown_changed |= matches!(event, DriverEvent::TextDelta(_));
                runtime_changed = true;
                keep_runtime &= self.handle_driver_event(session_id, &mut runtime, event);
                if !keep_runtime {
                    break;
                }
            }
            runtime.stream_remeasure_pending = markdown_changed;
            if keep_runtime {
                self.runtimes.insert(session_id, runtime);
            }
            changed |= runtime_changed;
            if self.state.selected_session == Some(session_id)
                && (runtime_changed || follow_up_remeasure)
            {
                selected_changed = true;
            }
        }

        if changed {
            self.stream_state_dirty = true;
        }
        if selected_changed {
            self.remeasure_transcript_tail();
        }
        if self.stream_state_dirty
            && (force_save || self.last_stream_save.elapsed() >= STREAM_SAVE_INTERVAL)
        {
            self.save();
        }
        changed || selected_changed
    }
}

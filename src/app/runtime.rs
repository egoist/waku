use super::*;

fn start_driver(mut request: DriverStartRequest, cwd: PathBuf) -> anyhow::Result<PreparedDriver> {
    request.options.cwd = cwd;
    let (event_tx, events) = driver::event_channel(request.event_wake);
    let handle = driver::start(request.provider, request.options, event_tx)?;
    Ok(PreparedDriver { handle, events })
}

/// Perform every blocking operation between accepting a submission and
/// starting its provider. This function is called only from the background
/// executor; the UI thread owns applying the returned workspace afterward.
fn prepare_submission(
    project: Project,
    workspace: SessionWorkspace,
    driver_start: Option<anyhow::Result<DriverStartRequest>>,
    session_id: Uuid,
    prompt: &str,
    turn_count: usize,
) -> anyhow::Result<PreparedSubmission> {
    let workspace = match workspace {
        SessionWorkspace::NewWorktree { base_branch } => {
            if project.is_projectless() {
                anyhow::bail!("a projectless task cannot create a Git worktree");
            }
            let created = crate::worktree::create(
                &project.path,
                project.id,
                session_id,
                prompt,
                base_branch.as_deref(),
            )?;
            SessionWorkspace::Worktree {
                path: created.path,
                branch: created.branch,
            }
        }
        workspace => workspace,
    };
    let project_path = workspace.path().unwrap_or(&project.path);

    // Every turn gets its own immutable starting snapshot. Reusing the prior
    // response's ending ref would attribute branch switches or terminal edits
    // made between turns to the next response.
    let checkpoint_warning = checkpoint::capture_turn_start(project_path, session_id, turn_count)
        .err()
        .map(|error| tr!("errors.capture_pre_turn_checkpoint", error = error));

    // Process startup can synchronously resolve executables, bind sockets,
    // and spawn children. It belongs behind the same animated preparation
    // boundary as Git work, otherwise the last spinner frame visibly freezes
    // just before Stop appears.
    let driver = driver_start.map(|request| {
        request.and_then(|request| start_driver(request, project_path.to_path_buf()))
    });

    Ok(PreparedSubmission {
        workspace,
        checkpoint_warning,
        driver,
    })
}

/// Everything a past-message resend needs after the UI accepts it.
///
/// The request owns only thread-safe snapshots. Git, provider RPCs, process
/// startup, and native transcript reads all happen in
/// [`perform_message_rewind`] on the background executor.
struct MessageRewindRequest {
    session_id: Uuid,
    provider: ProviderKind,
    provider_cursor: Option<ProviderResumeCursor>,
    session_title: String,
    /// Cursor has no native branch API, so its background helper needs the
    /// retained visible transcript. Other providers avoid cloning a long task
    /// on the click path entirely.
    cursor_source: Option<AgentSession>,
    project_path: PathBuf,
    retained_turn_count: usize,
    previous_turn_count: usize,
    rollback_turns: usize,
    provider_turn_count: usize,
    provider_resume_at: Option<String>,
    binary: Option<PathBuf>,
    driver: Option<DriverHandle>,
    driver_start: Option<DriverStartRequest>,
}

struct PreparedMessageRewind {
    provider_rewind_cursor: Option<ProviderResumeCursor>,
    claude_fork: Option<(crate::claude_session::ForkedClaudeSession, String)>,
    prepared_driver: Option<PreparedDriver>,
    reset_native_session: bool,
    cleanup_error: Option<String>,
}

fn perform_message_rewind(
    mut request: MessageRewindRequest,
) -> Result<PreparedMessageRewind, String> {
    let session_id = request.session_id;
    let turn_start_ref =
        checkpoint::turn_start_ref(session_id, request.retained_turn_count.saturating_add(1));
    let retained_ref = checkpoint::checkpoint_ref(session_id, request.retained_turn_count);
    let restore_ref = if checkpoint::has_ref(&request.project_path, &turn_start_ref) {
        turn_start_ref
    } else {
        retained_ref
    };
    if !checkpoint::has_ref(&request.project_path, &restore_ref) {
        return Err(tr!("session.pre_turn_checkpoint_missing"));
    }

    let safety_ref = format!("refs/waku/revert-backup-{session_id}-{}", Uuid::new_v4());
    checkpoint::capture_ref(&request.project_path, &safety_ref)
        .map_err(|error| tr!("errors.create_rewind_snapshot", error = error))?;
    if let Err(error) = checkpoint::restore_ref(&request.project_path, &restore_ref) {
        return Err(
            match checkpoint::restore_ref(&request.project_path, &safety_ref) {
                Ok(()) => {
                    let _ = checkpoint::delete_ref(&request.project_path, &safety_ref);
                    tr!("errors.restore_checkpoint", error = error)
                }
                Err(restore_error) => tr!(
                    "errors.restore_checkpoint_and_safety",
                    error = error,
                    restore_error = restore_error,
                    safety_ref = safety_ref
                ),
            },
        );
    }

    let provider_rewind = perform_provider_rewind(&mut request);
    let (provider_rewind_cursor, claude_fork, prepared_driver) = match provider_rewind {
        Ok(rewind) => rewind,
        Err(error) => {
            return Err(
                match checkpoint::restore_ref(&request.project_path, &safety_ref) {
                    Ok(()) => {
                        let _ = checkpoint::delete_ref(&request.project_path, &safety_ref);
                        tr!("errors.rollback_rejected_workspace_restored", error = error)
                    }
                    Err(restore_error) => tr!(
                        "errors.rollback_and_safety_failed",
                        error = error,
                        restore_error = restore_error,
                        safety_ref = safety_ref
                    ),
                },
            );
        }
    };

    let _ = checkpoint::delete_ref(&request.project_path, &safety_ref);
    let cleanup_error = checkpoint::delete_turn_refs_after(
        &request.project_path,
        session_id,
        request.retained_turn_count,
        request.previous_turn_count,
    )
    .err()
    .map(|error| error.to_string());

    Ok(PreparedMessageRewind {
        provider_rewind_cursor,
        claude_fork,
        prepared_driver,
        reset_native_session: request.rollback_turns > 0
            && request.retained_turn_count == 0
            && matches!(
                request.provider,
                ProviderKind::Claude | ProviderKind::Cursor | ProviderKind::Grok
            ),
        cleanup_error,
    })
}

type ProviderRewindResult = (
    Option<ProviderResumeCursor>,
    Option<(crate::claude_session::ForkedClaudeSession, String)>,
    Option<PreparedDriver>,
);

fn perform_provider_rewind(
    request: &mut MessageRewindRequest,
) -> anyhow::Result<ProviderRewindResult> {
    let provider = request.provider;
    let reset_native_session = request.rollback_turns > 0
        && request.retained_turn_count == 0
        && matches!(
            provider,
            ProviderKind::Claude | ProviderKind::Cursor | ProviderKind::Grok
        );
    if request.rollback_turns == 0 || reset_native_session {
        return Ok((None, None, None));
    }

    match provider {
        ProviderKind::Claude => {
            let Some(ProviderResumeCursor::Claude {
                session_id: native_session_id,
                ..
            }) = request.provider_cursor.as_ref()
            else {
                anyhow::bail!(tr!(
                    "errors.provider_native_cursor_unavailable",
                    provider = "Claude"
                ));
            };
            let resume_at = request
                .provider_resume_at
                .clone()
                .map(Ok)
                .unwrap_or_else(|| {
                    crate::claude_session::message_id_for_turn(
                        native_session_id,
                        request.provider_turn_count,
                    )
                })?;
            let fork = crate::claude_session::fork_session_at(
                native_session_id,
                &resume_at,
                &tr!(
                    "session.rewind_title",
                    title = request.session_title.as_str()
                ),
            )?;
            Ok((None, Some((fork, resume_at)), None))
        }
        ProviderKind::OpenCode => {
            let cursor = if let Some(driver) = request.driver.as_ref() {
                driver.rollback(request.rollback_turns)?.ok_or_else(|| {
                    anyhow::anyhow!("OpenCode returned no cursor for the rewound session")
                })?
            } else {
                let Some(ProviderResumeCursor::OpenCode {
                    session_id: native_session_id,
                }) = request.provider_cursor.as_ref()
                else {
                    anyhow::bail!(tr!(
                        "errors.provider_native_cursor_unavailable",
                        provider = "OpenCode"
                    ));
                };
                let binary = request.binary.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(tr!("errors.provider_not_found", provider = "OpenCode"))
                })?;
                crate::opencode_session::fork_session_at_turn(
                    binary,
                    &request.project_path,
                    native_session_id,
                    request.provider_turn_count,
                )?
            };
            Ok((Some(cursor), None, None))
        }
        ProviderKind::Amp => {
            let Some(ProviderResumeCursor::Amp {
                thread_id: native_thread_id,
                fork_context,
            }) = request.provider_cursor.as_ref()
            else {
                anyhow::bail!(tr!(
                    "errors.provider_native_thread_cursor_unavailable",
                    provider = "Amp"
                ));
            };
            let binary = request.binary.as_deref().ok_or_else(|| {
                anyhow::anyhow!(tr!("errors.provider_not_found", provider = "Amp"))
            })?;
            let cursor = crate::amp_session::fork_session_at_turn(
                binary,
                &request.project_path,
                native_thread_id,
                fork_context.as_deref(),
                request.provider_turn_count,
            )?;
            Ok((Some(cursor), None, None))
        }
        ProviderKind::Cursor => {
            let source = request.cursor_source.as_ref().ok_or_else(|| {
                anyhow::anyhow!(tr!(
                    "errors.provider_waku_task_unavailable",
                    provider = "Cursor"
                ))
            })?;
            Ok((
                Some(crate::cursor_session::fork_session_at_turn(
                    source,
                    request.retained_turn_count,
                )?),
                None,
                None,
            ))
        }
        ProviderKind::Grok => {
            let Some(ProviderResumeCursor::Grok {
                session_id: native_session_id,
            }) = request.provider_cursor.as_ref()
            else {
                anyhow::bail!(tr!(
                    "errors.provider_native_cursor_unavailable",
                    provider = "Grok"
                ));
            };
            let binary = request.binary.as_deref().ok_or_else(|| {
                anyhow::anyhow!(tr!("errors.provider_not_found", provider = "Grok Build"))
            })?;
            let cursor = crate::grok_session::fork_session_at_turn(
                binary,
                &request.project_path,
                native_session_id,
                request.provider_turn_count,
            )?;
            Ok((Some(cursor), None, None))
        }
        ProviderKind::Codex | ProviderKind::Pi => {
            let mut prepared_driver = None;
            let driver = if let Some(driver) = request.driver.as_ref() {
                driver.clone()
            } else {
                let start = request.driver_start.take().ok_or_else(|| {
                    anyhow::anyhow!(tr!(
                        "errors.provider_not_found",
                        provider = provider.display_name()
                    ))
                })?;
                let prepared = start_driver(start, request.project_path.clone())?;
                let driver = prepared.handle.clone();
                prepared_driver = Some(prepared);
                driver
            };
            let cursor = driver.rollback(request.rollback_turns)?;
            Ok((cursor, None, prepared_driver))
        }
    }
}

/// Everything a response fork needs after the click has been accepted.
///
/// The session is a point-in-time snapshot: provider branching may take long
/// enough for the user to navigate elsewhere, but the resulting task must
/// still end at the response they chose. Provider RPCs, process startup,
/// native transcript I/O, and Git ref copying are all performed by
/// [`perform_response_fork`] on the background executor.
struct ResponseForkRequest {
    source: AgentSession,
    source_workspace_path: PathBuf,
    fork_title: String,
    turn_count: usize,
    provider_turn_count: usize,
    turns_to_remove: usize,
    binary: Option<PathBuf>,
    driver: Option<DriverHandle>,
    driver_start: Option<DriverStartRequest>,
}

fn numbered_title_suffix(title: &str) -> Option<(&str, usize)> {
    let (base, suffix) = title.rsplit_once(" (")?;
    let number = suffix.strip_suffix(')')?.parse().ok()?;
    (!base.is_empty() && number >= 2).then_some((base, number))
}

fn next_response_fork_title<'a>(
    source_title: &str,
    existing_titles: impl IntoIterator<Item = &'a str>,
) -> String {
    let existing_titles = existing_titles.into_iter().collect::<Vec<_>>();
    let base = numbered_title_suffix(source_title)
        .filter(|(base, _)| existing_titles.iter().any(|title| title == base))
        .map_or(source_title, |(base, _)| base);
    let highest_number = existing_titles
        .iter()
        .filter_map(|title| {
            if *title == base {
                Some(1)
            } else {
                numbered_title_suffix(title)
                    .filter(|(candidate_base, _)| *candidate_base == base)
                    .map(|(_, number)| number)
            }
        })
        .max()
        .unwrap_or(1);
    format!("{base} ({})", highest_number.saturating_add(1).max(2))
}

struct PreparedResponseFork {
    forked: AgentSession,
    prepared_driver: Option<PreparedDriver>,
    checkpoint_warning: Option<String>,
}

type ProviderForkResult = (
    ProviderResumeCursor,
    Option<HashMap<String, String>>,
    Option<PreparedDriver>,
);

fn fork_response_with_driver(
    request: &mut ResponseForkRequest,
) -> anyhow::Result<(ProviderResumeCursor, Option<PreparedDriver>)> {
    let provider = request.source.provider;
    let mut prepared_driver = None;
    let driver = if let Some(driver) = request.driver.as_ref() {
        driver.clone()
    } else {
        let start = request.driver_start.take().ok_or_else(|| {
            anyhow::anyhow!(tr!(
                "errors.provider_not_found",
                provider = provider.display_name()
            ))
        })?;
        let prepared = start_driver(start, request.source_workspace_path.clone())?;
        let driver = prepared.handle.clone();
        prepared_driver = Some(prepared);
        driver
    };
    Ok((driver.fork(request.turns_to_remove)?, prepared_driver))
}

fn perform_response_fork(mut request: ResponseForkRequest) -> Result<PreparedResponseFork, String> {
    let provider = request.source.provider;
    let native_fork = (|| -> anyhow::Result<ProviderForkResult> {
        match provider {
            ProviderKind::Claude => {
                let ProviderResumeCursor::Claude {
                    session_id: native_session_id,
                    ..
                } = request.source.provider_cursor.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(tr!(
                        "errors.provider_native_session_unavailable",
                        provider = "Claude"
                    ))
                })?
                else {
                    anyhow::bail!(tr!(
                        "errors.provider_native_session_unavailable",
                        provider = "Claude"
                    ));
                };
                let resume_at = request
                    .source
                    .turns
                    .get(request.turn_count.saturating_sub(1))
                    .and_then(|turn| turn.provider_resume_at.clone())
                    .map(Ok)
                    .unwrap_or_else(|| {
                        crate::claude_session::message_id_for_turn(
                            native_session_id,
                            request.provider_turn_count,
                        )
                    })?;
                let fork = crate::claude_session::fork_session_at(
                    native_session_id,
                    &resume_at,
                    &request.fork_title,
                )?;
                let fork_resume_at =
                    fork.message_ids.get(&resume_at).cloned().ok_or_else(|| {
                        anyhow::anyhow!(tr!("errors.claude_fork_checkpoint_missing"))
                    })?;
                Ok((
                    ProviderResumeCursor::Claude {
                        session_id: fork.session_id,
                        resume_at: Some(fork_resume_at),
                    },
                    Some(fork.message_ids),
                    None,
                ))
            }
            ProviderKind::Codex => {
                if !matches!(
                    request.source.provider_cursor.as_ref(),
                    Some(ProviderResumeCursor::Codex { .. })
                ) {
                    anyhow::bail!(tr!(
                        "errors.provider_native_thread_unavailable",
                        provider = "Codex"
                    ));
                }
                let (cursor, prepared_driver) = fork_response_with_driver(&mut request)?;
                Ok((cursor, None, prepared_driver))
            }
            ProviderKind::Cursor => Ok((
                crate::cursor_session::fork_session_at_turn(&request.source, request.turn_count)?,
                None,
                None,
            )),
            ProviderKind::Amp => {
                let Some(ProviderResumeCursor::Amp {
                    thread_id: native_thread_id,
                    fork_context,
                }) = request.source.provider_cursor.as_ref()
                else {
                    anyhow::bail!(tr!(
                        "errors.provider_native_thread_unavailable",
                        provider = "Amp"
                    ));
                };
                let binary = request.binary.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(tr!("errors.provider_not_installed", provider = "Amp"))
                })?;
                Ok((
                    crate::amp_session::fork_session_at_turn(
                        binary,
                        &request.source_workspace_path,
                        native_thread_id,
                        fork_context.as_deref(),
                        request.provider_turn_count,
                    )?,
                    None,
                    None,
                ))
            }
            ProviderKind::OpenCode => {
                let Some(ProviderResumeCursor::OpenCode {
                    session_id: native_session_id,
                }) = request.source.provider_cursor.as_ref()
                else {
                    anyhow::bail!(tr!(
                        "errors.provider_native_session_unavailable",
                        provider = "OpenCode"
                    ));
                };
                let binary = request.binary.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(tr!("errors.provider_not_installed", provider = "OpenCode"))
                })?;
                Ok((
                    crate::opencode_session::fork_session_at_turn(
                        binary,
                        &request.source_workspace_path,
                        native_session_id,
                        request.provider_turn_count,
                    )?,
                    None,
                    None,
                ))
            }
            ProviderKind::Grok => {
                let Some(ProviderResumeCursor::Grok {
                    session_id: native_session_id,
                }) = request.source.provider_cursor.as_ref()
                else {
                    anyhow::bail!(tr!(
                        "errors.provider_native_session_unavailable",
                        provider = "Grok"
                    ));
                };
                let binary = request.binary.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(tr!(
                        "errors.provider_not_installed",
                        provider = "Grok Build"
                    ))
                })?;
                Ok((
                    crate::grok_session::fork_session_at_turn(
                        binary,
                        &request.source_workspace_path,
                        native_session_id,
                        request.provider_turn_count,
                    )?,
                    None,
                    None,
                ))
            }
            ProviderKind::Pi => {
                if !matches!(
                    request.source.provider_cursor.as_ref(),
                    Some(ProviderResumeCursor::Pi {
                        session_file: Some(_),
                        ..
                    })
                ) {
                    anyhow::bail!(tr!("errors.pi_session_file_unavailable"));
                }
                let (cursor, prepared_driver) = fork_response_with_driver(&mut request)?;
                Ok((cursor, None, prepared_driver))
            }
        }
    })();

    let (provider_cursor, claude_message_ids, prepared_driver) =
        native_fork.map_err(|error| tr!("errors.fork_task", error = error))?;
    let Some(mut forked) =
        request
            .source
            .fork_through_turn(request.turn_count, provider_cursor, &request.fork_title)
    else {
        return Err(tr!("session.response_cannot_copy"));
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
    let checkpoint_warning = checkpoint::copy_session_refs(
        &request.source_workspace_path,
        request.source.id,
        fork_id,
        request.turn_count,
    )
    .err()
    .map(|error| error.to_string());

    Ok(PreparedResponseFork {
        forked,
        prepared_driver,
        checkpoint_warning,
    })
}

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

    fn active_turn_finished_event(
        &self,
        session_id: Uuid,
        outcome: crate::analytics::TurnOutcome,
    ) -> Option<crate::analytics::Event> {
        let session = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;
        let turn = session
            .turns
            .last()
            .filter(|turn| turn.status == TurnStatus::Running)?;
        Some(crate::analytics::Event::TurnFinished {
            provider: session.provider.id(),
            turn_number: turn.turn_count,
            outcome,
            duration_seconds: unix_time().saturating_sub(turn.started_at),
        })
    }

    /// Completes a persisted turn and emits its anonymous outcome exactly
    /// once. All production turn-settlement paths go through this seam.
    pub(super) fn finish_active_turn_with_analytics(
        &mut self,
        session_id: Uuid,
        status: TurnStatus,
        outcome: crate::analytics::TurnOutcome,
    ) -> Option<(Uuid, usize)> {
        let event = self.active_turn_finished_event(session_id, outcome);
        let result = self
            .state
            .session_mut(session_id)?
            .finish_active_turn(status);
        if result.is_some()
            && let Some(event) = event
        {
            self.analytics.track(event);
        }
        result
    }

    /// Records a failed submission that is about to be unwound and therefore
    /// will not remain as a persisted turn.
    fn track_active_turn_outcome(&self, session_id: Uuid, outcome: crate::analytics::TurnOutcome) {
        if let Some(event) = self.active_turn_finished_event(session_id, outcome) {
            self.analytics.track(event);
        }
    }

    /// The directory every filesystem and provider operation for `session`
    /// must use. A not-yet-materialized worktree draft deliberately reads the
    /// local checkout until its first submission creates the isolated copy.
    pub(super) fn workspace_path_for_session<'a>(
        &'a self,
        session: &'a AgentSession,
    ) -> Option<&'a std::path::Path> {
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)?;
        Some(session.workspace.path().unwrap_or(&project.path))
    }

    pub(super) fn selected_workspace_path(&self) -> Option<&std::path::Path> {
        let session = self.selected_session()?;
        self.workspace_path_for_session(session)
    }

    /// Marks the session for the next save; see `PersistedState::session_mut`.
    pub(super) fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state.session_mut(id)
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
        let event_wake = self.event_wake_tx.clone();
        if std::thread::Builder::new()
            .name(format!("waku-{}-model-discovery", provider.id()))
            .spawn(move || {
                // Stale-while-revalidate: the catalog cached by the last
                // successful discovery renders right away, and the CLI's
                // answer replaces it (and the cache) whenever it lands.
                if let Some(models) = crate::model_catalog::cached_models(provider) {
                    let mut cached = probe.clone();
                    cached.models = models;
                    if provider_probe_tx.send(cached).is_ok() {
                        signal_event_pump(&event_wake);
                    }
                }
                if provider_probe_tx.send(probe.discover_models()).is_ok() {
                    signal_event_pump(&event_wake);
                }
            })
            .is_err()
        {
            self.provider_model_discoveries.remove(&provider);
            self.provider_model_discoveries_pending.remove(&provider);
        }
    }

    /// Ask every installed CLI for its version, one short-lived subprocess per
    /// provider on its own thread. Answers land in `provider_versions` through
    /// the drain loop; render reads only that map.
    pub(super) fn request_provider_version_probes(&mut self) {
        let targets = self
            .probes
            .iter()
            .filter(|probe| probe.installed)
            .filter_map(|probe| probe.path.clone().map(|path| (probe.provider, path)))
            .collect::<Vec<_>>();
        for (provider, path) in targets {
            if !self.provider_version_probes_pending.insert(provider) {
                continue;
            }
            let provider_version_tx = self.provider_version_tx.clone();
            let event_wake = self.event_wake_tx.clone();
            if std::thread::Builder::new()
                .name(format!("waku-{}-version-probe", provider.id()))
                .spawn(move || {
                    let version = probe_provider_version(&path);
                    if provider_version_tx.send((provider, version)).is_ok() {
                        signal_event_pump(&event_wake);
                    }
                })
                .is_err()
            {
                self.provider_version_probes_pending.remove(&provider);
            }
        }
    }

    pub(super) fn drain_provider_version_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok((provider, version)) = self.provider_version_events.try_recv() {
            self.provider_version_probes_pending.remove(&provider);
            self.provider_versions.insert(provider, version);
            changed = true;
        }
        changed
    }

    /// Re-detect provider CLIs off-thread — every provider for the Providers
    /// page's refresh, or one whose binary override just changed. Also re-runs
    /// model discovery and version probes for whatever the detection finds
    /// installed.
    pub(super) fn refresh_provider_detection(&mut self, scope: Option<ProviderKind>) {
        if self.provider_detection_remaining > 0 {
            return;
        }
        let providers = match scope {
            Some(provider) => vec![provider],
            None => ProviderKind::ALL.to_vec(),
        };
        self.provider_detection_remaining = providers.len();
        let overrides = self.state.provider_binary_overrides.clone();
        let provider_detection_tx = self.provider_detection_tx.clone();
        let event_wake = self.event_wake_tx.clone();
        let detect_providers = providers.clone();
        if std::thread::Builder::new()
            .name("waku-provider-detection".into())
            .spawn(move || {
                // Finder/Dock launches do not inherit the PATH assembled by
                // the user's interactive shell. Resolve it here, away from
                // the UI thread, before looking for nvm/fnm-managed CLIs.
                crate::command_env::refresh_from_default_shell();
                for provider in detect_providers {
                    let path = match overrides.get(&provider) {
                        Some(binary) => crate::command_env::resolve_binary_override(binary),
                        None => crate::command_env::find_executable(provider.command()),
                    };
                    if provider_detection_tx
                        .send((provider, path.is_some(), path))
                        .is_ok()
                    {
                        signal_event_pump(&event_wake);
                    }
                }
            })
            .is_err()
        {
            self.provider_detection_remaining = 0;
            return;
        }
        // A refresh means "re-check everything about these providers":
        // clearing the per-launch guard lets each one's catalog discovery run
        // again as its detection lands below.
        for provider in providers {
            self.provider_model_discoveries.remove(&provider);
        }
    }

    pub(super) fn drain_provider_detection_events(&mut self) -> bool {
        let mut changed = false;
        let mut installed_providers = Vec::new();
        while let Ok((provider, installed, path)) = self.provider_detection_events.try_recv() {
            self.provider_detection_remaining = self.provider_detection_remaining.saturating_sub(1);
            if self.provider_detection_remaining == 0 {
                self.provider_detection_checked_at = Some(Instant::now());
            }
            // Merge detection fields only: a probe's model catalog belongs to
            // model discovery, and overwriting it here would race a discovery
            // still in flight.
            if let Some(existing) = self
                .probes
                .iter_mut()
                .find(|existing| existing.provider == provider)
            {
                existing.installed = installed;
                existing.path = path;
            } else {
                self.probes.push(ProviderProbe {
                    provider,
                    installed,
                    path,
                    models: crate::model_catalog::fallback_models(provider),
                });
            }
            if installed {
                installed_providers.push(provider);
            } else {
                self.provider_versions.remove(&provider);
            }
            changed = true;
        }
        for provider in installed_providers {
            self.request_provider_model_discovery(provider);
        }
        if changed {
            self.request_provider_version_probes();
        }
        changed
    }

    /// Whether the provider can back a new session: installed and not switched
    /// off in the Providers settings.
    pub(super) fn provider_enabled(&self, provider: ProviderKind) -> bool {
        !self.state.disabled_providers.contains(&provider)
            && self
                .provider_probe(provider)
                .is_some_and(|probe| probe.installed)
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
        if let Err(error) = self.store.save(&mut self.state) {
            self.show_toast(tr!("errors.save_local_state", error = error));
        } else {
            self.stream_state_dirty = false;
        }
    }

    fn checkpoint_capture_pending(&self, session_id: Uuid, turn_count: usize) -> bool {
        self.checkpoint_captures_in_flight
            .contains(&(session_id, turn_count))
            || self
                .pending_checkpoint_captures
                .iter()
                .any(|capture| capture.session_id == session_id && capture.turn_count == turn_count)
    }

    fn ending_checkpoint_pending(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.turns.last())
            .filter(|turn| turn.status != TurnStatus::Running)
            .is_some_and(|turn| self.checkpoint_capture_pending(session_id, turn.turn_count))
    }

    fn defer_queue_drain(&mut self, session_id: Uuid) {
        if !self.pending_queue_drains.contains(&session_id) {
            self.pending_queue_drains.push(session_id);
        }
    }

    /// Queues the newest finished turn's checkpoint for capture.
    ///
    /// Bookkeeping only. The capture itself is upwards of ten `git`
    /// invocations, one of them a `git add -A` over the whole worktree, and the
    /// hottest caller is the driver-event drain that shares the UI thread with
    /// rendering — so the work belongs to
    /// [`Self::start_pending_checkpoint_captures`], which every caller that
    /// holds a `Context` runs straight after queueing.
    pub(super) fn capture_latest_turn_checkpoint_for(&mut self, session_id: Uuid) {
        let Some((session, turn_count)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .last()
                    .filter(|turn| turn.status != TurnStatus::Running)
                    .map(|turn| (session, turn.turn_count))
            })
        else {
            return;
        };
        if self.checkpoint_capture_pending(session_id, turn_count) {
            return;
        }
        let Some(project_path) = self
            .workspace_path_for_session(session)
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        self.pending_checkpoint_captures
            .push(PendingCheckpointCapture {
                session_id,
                turn_count,
                project_path,
            });
    }

    /// Runs queued turn checkpoints on the background executor.
    ///
    /// A capture lands a frame or many later, and the turn it belongs to may be
    /// gone by then, so the result is matched back by turn count rather than
    /// position. Nothing on screen waits for it: the transcript's rewind
    /// affordance appears when `invalidate_checkpoint_refs` prompts the next
    /// prefetch to notice the new ref.
    pub(super) fn start_pending_checkpoint_captures(&mut self, cx: &mut Context<Self>) {
        for request in std::mem::take(&mut self.pending_checkpoint_captures) {
            let PendingCheckpointCapture {
                session_id,
                turn_count,
                project_path,
            } = request;
            if !self
                .checkpoint_captures_in_flight
                .insert((session_id, turn_count))
            {
                continue;
            }
            cx.spawn(async move |waku, cx| {
                let captured =
                    cx.background_executor()
                        .spawn({
                            let project_path = project_path.clone();
                            async move {
                                checkpoint::capture_turn(&project_path, session_id, turn_count)
                            }
                        })
                        .await;
                waku.update(cx, |waku, cx| {
                    waku.checkpoint_captures_in_flight
                        .remove(&(session_id, turn_count));
                    let selected = waku.state.selected_session == Some(session_id);
                    if selected {
                        waku.sync_transcript_rows();
                    }
                    let previous_kinds = if selected {
                        waku.transcript_row_kinds.borrow().clone()
                    } else {
                        Vec::new()
                    };
                    let checkpoint = match captured {
                        Ok(checkpoint) => checkpoint,
                        Err(error) => {
                            waku.show_toast(tr!("errors.capture_turn_checkpoint", error = error));
                            Checkpoint {
                                turn_count,
                                git_ref: checkpoint::checkpoint_ref(session_id, turn_count),
                                status: CheckpointStatus::Error,
                                files: Vec::new(),
                                additions: 0,
                                deletions: 0,
                                created_at: unix_time(),
                            }
                        }
                    };
                    waku.invalidate_checkpoint_refs();
                    let mut attached_turn_id = None;
                    if let Some(session) = waku.state.session_mut(session_id)
                        && let Some(turn) = session
                            .turns
                            .iter_mut()
                            .find(|turn| turn.turn_count == turn_count)
                    {
                        turn.checkpoint = Some(checkpoint);
                        attached_turn_id = Some(turn.id);
                    }
                    if let Some(turn_id) = attached_turn_id
                        && selected
                    {
                        // Reconcile a standalone card by row identity, then
                        // remeasure the terminal response when the card is
                        // hosted inline before its footer.
                        waku.splice_transcript_rows_after_visibility_change(&previous_kinds);
                        waku.remeasure_changed_files(turn_id);
                    }
                    let resume_queue = waku.pending_queue_drains.contains(&session_id);
                    if resume_queue {
                        waku.pending_queue_drains.retain(|id| *id != session_id);
                        waku.drain_queued_message(session_id, cx);
                    }
                    cx.notify();
                    if attached_turn_id.is_some() {
                        // Let the new transcript row paint before SQLite work.
                        // Without this save, a checkpoint that lands after the
                        // turn's final stream save can disappear on relaunch.
                        cx.spawn(async move |waku, cx| {
                            cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
                            let _ = waku.update(cx, |waku, _| waku.save());
                        })
                        .detach();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    pub(super) fn fork_session_from_response(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.response_fork_preparations.contains_key(&session_id)
            || self.submission_preparations.contains(&session_id)
        {
            self.show_toast(tr!("session.response_cannot_fork"));
            cx.notify();
            return;
        }
        let Some(source) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            self.show_toast(tr!("session.response_unavailable"));
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
            self.show_toast(tr!("session.response_cannot_fork"));
            cx.notify();
            return;
        }
        let Some(source_workspace_path) = self
            .workspace_path_for_session(&source)
            .map(std::path::Path::to_path_buf)
        else {
            self.show_toast(tr!("errors.task_project_not_found"));
            cx.notify();
            return;
        };

        let provider = source.provider;
        let project_id = source.project_id;
        let fork_title = next_response_fork_title(
            source.display_title(),
            self.state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .map(AgentSession::display_title),
        );
        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let turns_to_remove = source.provider_turns_after(turn_count);
        let driver = self
            .runtimes
            .get(&session_id)
            .map(|runtime| runtime.driver.clone());
        let binary_provider = match provider {
            ProviderKind::Amp => Some("Amp"),
            ProviderKind::OpenCode => Some("OpenCode"),
            ProviderKind::Grok => Some("Grok Build"),
            _ => None,
        };
        let binary = binary_provider.and_then(|_| {
            self.probes
                .iter()
                .find(|probe| probe.provider == provider)
                .and_then(|probe| probe.path.clone())
        });
        if let Some(provider_name) = binary_provider
            && binary.is_none()
        {
            self.show_toast(tr!(
                "errors.provider_not_installed",
                provider = provider_name
            ));
            cx.notify();
            return;
        }
        let driver_start = if matches!(provider, ProviderKind::Codex | ProviderKind::Pi)
            && driver.is_none()
        {
            match self.driver_start_request_for_session(&source, source_workspace_path.clone()) {
                Ok(request) => Some(request),
                Err(error) => {
                    self.show_toast(tr!("errors.fork_task", error = error));
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        let request = ResponseForkRequest {
            source,
            source_workspace_path,
            fork_title,
            turn_count,
            provider_turn_count,
            turns_to_remove,
            binary,
            driver,
            driver_start,
        };

        self.response_fork_preparations
            .insert(session_id, turn_count);
        self.hide_toast();
        cx.notify();

        cx.spawn(async move |waku, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { perform_response_fork(request) })
                .await;
            let _ = waku.update(cx, move |waku, cx| {
                waku.finish_response_fork(session_id, turn_count, provider, result, cx);
            });
        })
        .detach();
    }

    fn finish_response_fork(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        provider: ProviderKind,
        result: Result<PreparedResponseFork, String>,
        cx: &mut Context<Self>,
    ) {
        if self.response_fork_preparations.get(&session_id) != Some(&turn_count) {
            return;
        }
        self.response_fork_preparations.remove(&session_id);

        let PreparedResponseFork {
            forked,
            prepared_driver,
            checkpoint_warning,
        } = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if provider == ProviderKind::Pi {
                    // A failed restore after Pi creates a fork can leave the
                    // resident RPC process on that fork. Recreate it lazily
                    // from the source cursor on its next prompt.
                    self.runtimes.remove(&session_id);
                }
                self.drain_queued_message(session_id, cx);
                self.show_toast(error);
                cx.notify();
                return;
            }
        };

        if let Some(prepared) = prepared_driver
            && !self.runtimes.contains_key(&session_id)
        {
            self.install_prepared_driver(session_id, prepared);
        }
        self.invalidate_checkpoint_refs();

        let fork_id = forked.id;
        self.state.push_session(forked);
        self.analytics
            .track(crate::analytics::Event::ResponseForked {
                provider: provider.id(),
                turn_number: turn_count,
            });
        self.select_session(fork_id, cx);
        self.drain_queued_message(session_id, cx);
        match checkpoint_warning {
            Some(error) => {
                self.show_toast(tr!("session.forked_with_checkpoint_warning", error = error))
            }
            None => self.show_success_toast(tr!("session.forked_from_response")),
        }
        cx.notify();
    }

    /// Composer Enter clears the field after emitting its event. A response
    /// fork temporarily owns the source provider, so restore a keyboard
    /// submission on the next task turn instead of racing it against the fork.
    pub(super) fn defer_restore_composer_after_fork(
        &self,
        session_id: Uuid,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let composer = self.composer.clone();
        cx.spawn(async move |waku, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
            let _ = waku.update(cx, |waku, cx| {
                if waku.state.selected_session == Some(session_id) {
                    composer.update(cx, |input, cx| {
                        if input.content().is_empty() {
                            input.set_content(prompt, cx);
                        }
                    });
                }
            });
        })
        .detach();
    }

    pub(super) fn begin_message_edit(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((message_index, initial_message, attachments)) = self
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
                            .then(|| {
                                (
                                    index,
                                    message.visible_content().to_owned(),
                                    message.attachments.clone(),
                                )
                            })
                    })
            })
        else {
            self.show_toast(tr!("session.message_not_editable"));
            cx.notify();
            return;
        };

        let input = cx.new(|cx| ComposerInput::new(window, cx));
        input.update(cx, |input, cx| input.set_content(initial_message, cx));
        cx.subscribe(
            &input,
            |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Submit(prompt) => {
                    this.submit_message_edit_prompt(prompt.clone(), cx)
                }
                // An edited past message resubmits from that point; there is
                // no running turn for it to steer.
                ComposerEvent::SubmitSteer(prompt) => {
                    this.submit_message_edit_prompt(prompt.clone(), cx)
                }
                ComposerEvent::Edited => cx.notify(),
                ComposerEvent::Focus => {}
                ComposerEvent::BackspaceOnEmpty => {}
            },
        )
        .detach();
        self.message_edit = Some(MessageEdit {
            session_id,
            turn_count,
            input: input.clone(),
            attachments,
        });
        self.hide_toast();
        self.remeasure_transcript_message(message_index);
        let focus_handle = input.read(cx).focus();
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .message_edit
            .as_ref()
            .is_some_and(|edit| self.submission_preparations.contains(&edit.session_id))
        {
            return;
        }
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
        let prompt = self
            .message_edit
            .as_ref()
            .map(|edit| edit.input.read(cx).content().to_owned())
            .unwrap_or_default();
        self.submit_message_edit_prompt(prompt, cx);
    }

    fn submit_message_edit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.clone() else {
            return;
        };
        if self.submission_preparations.contains(&edit.session_id) {
            return;
        }
        // Keyboard submission clears ComposerInput after emitting its event.
        // Use the event's captured value rather than rereading the field; the
        // button path enters here with its own pre-clear content as well.
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() && edit.attachments.is_empty() {
            self.show_toast(tr!("session.edited_message_empty"));
            cx.notify();
            return;
        }
        let mentions = edit
            .attachments
            .iter()
            .map(|attachment| attachment.mention.clone())
            .collect::<Vec<_>>();
        let provider_prompt = composer::merged_submission(&prompt, &mentions)
            .expect("edited text or retained attachments always form a submission");
        let display_content = (!edit.attachments.is_empty()).then_some(prompt);
        self.start_message_rewind(
            edit.clone(),
            ComposerSubmission {
                prompt: provider_prompt,
                display_content,
                attachments: edit.attachments,
            },
            cx,
        );
    }

    fn start_message_rewind(
        &mut self,
        edit: MessageEdit,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        let session_id = edit.session_id;
        let turn_count = edit.turn_count;
        let retained_turn_count = turn_count.saturating_sub(1);
        let Some(source) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .filter(|session| {
                session
                    .turns
                    .iter()
                    .any(|turn| turn.turn_count == turn_count)
            })
        else {
            self.show_toast(tr!("session.message_unavailable"));
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id) {
            self.show_toast(tr!("session.select_before_rewind"));
            cx.notify();
            return;
        }
        if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
            self.show_toast(tr!("session.stop_before_rewind"));
            cx.notify();
            return;
        }
        let rollback_turns = source.provider_turns_after(retained_turn_count);
        if !source.provider.supports_conversation_rollback()
            || (rollback_turns > 0 && source.provider_cursor.is_none())
        {
            self.show_toast(tr!(
                "session.provider_cannot_rewind",
                provider = source.provider.display_name()
            ));
            cx.notify();
            return;
        }
        let Some(project_path) = self
            .workspace_path_for_session(&source)
            .map(std::path::Path::to_path_buf)
        else {
            self.show_toast(tr!("errors.task_project_not_found"));
            cx.notify();
            return;
        };
        let provider_turn_count = source
            .turns
            .iter()
            .take(retained_turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let provider_resume_at = retained_turn_count
            .checked_sub(1)
            .and_then(|index| source.turns.get(index))
            .and_then(|turn| turn.provider_resume_at.clone());
        let driver = self
            .runtimes
            .get(&session_id)
            .map(|runtime| runtime.driver.clone());
        let needs_binary = rollback_turns > 0
            && (matches!(source.provider, ProviderKind::Amp)
                || (source.provider == ProviderKind::OpenCode && driver.is_none())
                || (source.provider == ProviderKind::Grok && retained_turn_count > 0));
        let binary = needs_binary
            .then(|| {
                self.probes
                    .iter()
                    .find(|probe| probe.provider == source.provider)
                    .and_then(|probe| probe.path.clone())
            })
            .flatten();
        if needs_binary && binary.is_none() {
            self.show_toast(tr!(
                "errors.provider_not_found",
                provider = source.provider.display_name()
            ));
            cx.notify();
            return;
        }
        let driver_start = if rollback_turns > 0
            && matches!(source.provider, ProviderKind::Codex | ProviderKind::Pi)
            && driver.is_none()
        {
            match self.driver_start_request_for_session(&source, project_path.clone()) {
                Ok(request) => Some(request),
                Err(error) => {
                    self.show_toast(error.to_string());
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        let previous_status = source.status;
        let previous_turn_count = source.turns.len();
        let provider = source.provider;
        let provider_cursor = source.provider_cursor.clone();
        let session_title = source.display_title().to_owned();
        let cursor_source = (provider == ProviderKind::Cursor).then(|| source.clone());
        let Some((edited_message_index, edited_message_id)) = source
            .turns
            .iter()
            .find(|turn| turn.turn_count == turn_count)
            .and_then(|turn| {
                source
                    .messages
                    .iter()
                    .enumerate()
                    .find_map(|(index, message)| {
                        (message.turn_id == Some(turn.id) && message.role == MessageRole::User)
                            .then_some((index, message.id))
                    })
            })
        else {
            self.show_toast(tr!("session.message_unavailable"));
            cx.notify();
            return;
        };
        let request = MessageRewindRequest {
            session_id,
            provider,
            provider_cursor,
            session_title,
            cursor_source,
            previous_turn_count,
            project_path,
            retained_turn_count,
            rollback_turns,
            provider_turn_count,
            provider_resume_at,
            binary,
            driver,
            driver_start,
        };

        // Optimistically leave edit mode and show the replacement bubble at
        // accept time. The main composer switches to its non-cancellable
        // spinner while every Git, process, native transcript, and provider
        // operation runs off the UI thread. Failure restores both the original
        // bubble and this edit input.
        let original_message = self.state.session_mut(session_id).and_then(|session| {
            let message = session
                .messages
                .iter_mut()
                .find(|message| message.id == edited_message_id)?;
            let original = message.clone();
            message.content = submission.prompt.clone();
            message.display_content = submission.display_content.clone();
            message.attachments = submission.attachments.clone();
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            Some(original)
        });
        let Some(original_message) = original_message else {
            self.show_toast(tr!("session.message_unavailable"));
            cx.notify();
            return;
        };
        self.message_edit = None;
        self.submission_preparations.insert(session_id);
        self.hide_toast();
        self.remeasure_transcript_message(edited_message_index);
        cx.notify();

        cx.spawn(async move |waku, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { perform_message_rewind(request) })
                .await;
            let _ = waku.update(cx, move |waku, cx| {
                waku.finish_message_rewind(
                    edit,
                    submission,
                    edited_message_id,
                    original_message,
                    previous_status,
                    result,
                    cx,
                );
            });
        })
        .detach();
    }

    fn finish_message_rewind(
        &mut self,
        edit: MessageEdit,
        submission: ComposerSubmission,
        edited_message_id: Uuid,
        original_message: Message,
        previous_status: SessionStatus,
        result: Result<PreparedMessageRewind, String>,
        cx: &mut Context<Self>,
    ) {
        let session_id = edit.session_id;
        let turn_count = edit.turn_count;
        if !self.submission_preparations.remove(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    if let Some(message) = session
                        .messages
                        .iter_mut()
                        .find(|message| message.id == edited_message_id)
                    {
                        *message = original_message;
                    }
                    if session.status == SessionStatus::Connecting {
                        session.status = previous_status;
                    }
                }
                if selected && self.message_edit.is_none() {
                    self.message_edit = Some(edit.clone());
                }
                if selected
                    && let Some(message_index) = self.selected_session().and_then(|session| {
                        let turn = session
                            .turns
                            .iter()
                            .find(|turn| turn.turn_count == turn_count)?;
                        session.messages.iter().position(|message| {
                            message.turn_id == Some(turn.id) && message.role == MessageRole::User
                        })
                    })
                {
                    self.remeasure_transcript_message(message_index);
                }
                self.show_toast(error);
                cx.notify();
                return;
            }
        };
        let PreparedMessageRewind {
            provider_rewind_cursor,
            claude_fork,
            mut prepared_driver,
            reset_native_session,
            cleanup_error,
        } = prepared;
        let retained_turn_count = turn_count.saturating_sub(1);
        let provider_and_removed_turns = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| {
                (
                    session.provider,
                    session.turns.len().saturating_sub(retained_turn_count),
                )
            });
        let Some((provider, removed_turns)) = provider_and_removed_turns else {
            return;
        };
        if selected {
            self.sync_transcript_rows();
        }
        let previous_kinds = if selected {
            self.transcript_row_kinds.borrow().clone()
        } else {
            Vec::new()
        };
        if let Some(session) = self.state.session_mut(session_id) {
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
            } else if reset_native_session {
                session.provider_cursor = None;
            } else if let Some(cursor) = provider_rewind_cursor.clone() {
                session.provider_cursor = Some(cursor);
            }
            session.truncate_after_turn(retained_turn_count);
            session.status = SessionStatus::Idle;
        }

        if let Some(prepared) = prepared_driver.as_mut() {
            // Startup announces the source cursor before a cold Codex/Pi
            // rollback finishes. It is stale now; do not let it overwrite the
            // rewound cursor after this driver is installed.
            while prepared.events.try_recv().is_ok() {}
        }
        if let Some(prepared) = prepared_driver {
            self.install_prepared_driver(session_id, prepared);
        }
        if claude_fork.is_some()
            || reset_native_session
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
            self.mark_background_work_lost(session_id);
        } else if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime
                .pending_events
                .retain(|event| matches!(event, DriverEvent::BackgroundWork(_)));
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
            runtime.pending_computer_approval = None;
        }
        self.invalidate_checkpoint_refs();
        if self
            .message_edit
            .as_ref()
            .is_some_and(|current| current.session_id == session_id)
        {
            self.message_edit = None;
        }
        if selected {
            self.activities_expanded.clear();
            self.expanded_activity_items.clear();
            self.expanded_turns.clear();
            self.expanded_changed_files.clear();
            self.transcript_control_focuses.borrow_mut().clear();
            self.splice_transcript_rows_after_visibility_change(&previous_kinds);
            self.show_toast(match cleanup_error {
                None => tr!("session.rewound", turn = turn_count),
                Some(error) => tr!(
                    "session.rewound_with_stale_refs",
                    turn = turn_count,
                    error = error
                ),
            });
        }
        self.analytics
            .track(crate::analytics::Event::ConversationRolledBack {
                provider: provider.id(),
                turns: removed_turns,
            });
        cx.notify();
        self.submit_submission_for_session(session_id, submission, cx);
    }

    /// Resolves the turn options a driver should run with, dropping a reasoning
    /// effort or service tier the resolved model does not offer. Driver start
    /// and in-session option changes both go through this so they cannot
    /// disagree about what the session is currently set to.
    pub(super) fn session_options(&self, session: &AgentSession) -> SessionOptions {
        let model = session.model.clone().or_else(|| {
            self.provider_probe(session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.clone())
        });
        let model_metadata = self.model_metadata_for_session(session);
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
        SessionOptions {
            mode: session.runtime_mode,
            interaction_mode: session.interaction_mode,
            model,
            reasoning_effort,
            service_tier,
        }
    }

    /// Releases provider processes for sessions nobody has touched in a while.
    ///
    /// Codex and Pi keep a process resident between turns, so an abandoned task
    /// otherwise holds an agent — and, with Computer Use on, a whole process
    /// tree — for as long as the app runs. Recreating a runtime is exactly the
    /// work the next prompt already does after Stop, and the resume cursor is
    /// persisted, so the conversation survives.
    pub(super) fn reap_idle_sessions(&mut self) {
        if self.last_idle_session_sweep.elapsed() < IDLE_SESSION_SWEEP_INTERVAL {
            return;
        }
        self.last_idle_session_sweep = Instant::now();
        let idle = self
            .runtimes
            .iter()
            .filter(|(session_id, runtime)| {
                let session = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == **session_id);
                session_is_reapable(
                    session,
                    runtime.last_active_at.elapsed(),
                    self.session_has_live_background_work(**session_id),
                )
            })
            .map(|(session_id, _)| *session_id)
            .collect::<Vec<_>>();
        for session_id in idle {
            // Dropping the runtime is the release: it closes the transport, and
            // the driver's own `Drop` takes the process tree with it. No cancel,
            // because there is no turn to cancel.
            self.runtimes.remove(&session_id);
        }
    }

    /// Applies a changed model, effort, tier, or mode to a session. Transports
    /// that carry these per turn absorb the change and keep running; the rest
    /// are torn down so the next prompt starts with the new options.
    pub(super) fn apply_session_options(&mut self, session_id: Uuid) {
        let Some(options) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| self.session_options(session))
        else {
            return;
        };
        let applied = self
            .runtimes
            .get(&session_id)
            .is_none_or(|runtime| runtime.driver.apply_options(options));
        if !applied {
            self.reset_session_runtime(session_id);
        }
    }

    fn driver_start_request_for_session(
        &self,
        session: &AgentSession,
        cwd: PathBuf,
    ) -> anyhow::Result<DriverStartRequest> {
        let binary = self
            .probes
            .iter()
            .find(|probe| probe.provider == session.provider)
            .and_then(|probe| probe.path.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(tr!(
                    "errors.provider_not_found",
                    provider = session.provider.display_name()
                ))
            })?;
        let SessionOptions {
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
        } = self.session_options(&session);
        Ok(DriverStartRequest {
            provider: session.provider,
            options: DriverStartOptions {
                binary,
                cwd,
                mode,
                interaction_mode,
                model,
                reasoning_effort,
                service_tier,
                computer_use_enabled: self.state.computer_use_enabled,
                provider_cursor: session.provider_cursor.clone(),
            },
            event_wake: self.event_wake_tx.clone(),
        })
    }

    fn install_prepared_driver(
        &mut self,
        session_id: Uuid,
        prepared: PreparedDriver,
    ) -> DriverHandle {
        let handle = prepared.handle.clone();
        self.runtimes.insert(
            session_id,
            SessionRuntime {
                driver: prepared.handle,
                events: prepared.events,
                pending_events: VecDeque::new(),
                pending_steers: VecDeque::new(),
                stream_phase: None,
                stream_remeasure_pending: false,
                pending_permission: None,
                pending_computer_approval: None,
                computer_use_previews: Vec::new(),
                computer_session_grants: HashSet::new(),
                last_driver_error: None,
                last_active_at: Instant::now(),
                last_background_refresh_at: Instant::now()
                    .checked_sub(BACKGROUND_WORK_REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
            },
        );
        // Startup can emit before the background task hands this receiver to
        // the runtime map. Wake once after installation so those buffered
        // events cannot be stranded behind an already-consumed edge.
        signal_event_pump(&self.event_wake_tx);
        handle
    }

    pub(super) fn submit_composer_submission(
        &mut self,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if self.response_fork_preparations.contains_key(&session.id) {
            return;
        }
        if session.is_busy() {
            // While the agent is working, Enter queues a follow-up instead of
            // refusing the message. The queue drains once the turn settles.
            self.enqueue_follow_up_submission(session.id, submission, cx);
            return;
        }
        self.submit_submission_for_session(session.id, submission, cx);
    }

    /// Deliver a steering message into the running turn. Providers without a
    /// live-turn transport (or a session that is not actively working) fall
    /// back to queueing a follow-up.
    pub(super) fn steer_composer_submission(
        &mut self,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        if !session.is_busy() {
            self.submit_composer_submission(submission, cx);
            return;
        }
        // A turn that has not reached the provider yet cannot be steered; the
        // driver reports the outcome asynchronously via SteerAccepted or
        // SteerRejected once it is handed off.
        let steerable = session.status != SessionStatus::Connecting
            && self
                .runtimes
                .get(&session.id)
                .is_some_and(|runtime| runtime.driver.supports_steer());
        if !steerable {
            self.enqueue_follow_up_submission(session.id, submission, cx);
            return;
        }
        if let Some(runtime) = self.runtimes.get_mut(&session.id) {
            runtime.driver.steer(submission.prompt.clone());
            runtime.pending_steers.push_back(submission);
        } else {
            self.enqueue_follow_up_submission(session.id, submission, cx);
        }
        cx.notify();
    }

    pub(super) fn enqueue_follow_up_submission(
        &mut self,
        session_id: Uuid,
        mut submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        submission.prompt = submission.prompt.trim().to_owned();
        if submission.prompt.is_empty() {
            return;
        }
        if let Some(session) = self.state.session_mut(session_id) {
            session
                .queued_messages
                .push(submission.into_queued_message());
            session.updated_at = unix_time();
        }
        self.save();
        cx.notify();
    }

    pub(super) fn remove_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.state.session_mut(session_id) {
            session
                .queued_messages
                .retain(|message| message.id != message_id);
        }
        self.save();
        cx.notify();
    }

    /// Pop a queued message back into the composer so the user can edit and
    /// resubmit it.
    pub(super) fn edit_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.state.session_mut(session_id).and_then(|session| {
            let index = session
                .queued_messages
                .iter()
                .position(|message| message.id == message_id)?;
            Some(session.queued_messages.remove(index))
        }) else {
            return;
        };
        self.restore_composer_submission(ComposerSubmission::from_queued_message(message), cx);
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        self.save();
        cx.notify();
    }

    /// Start the next queued follow-up as a fresh turn. Only called once a
    /// settled turn has been fully closed, so the session is Idle.
    fn drain_queued_message(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if self.response_fork_preparations.contains_key(&session_id) {
            return;
        }
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if session.is_busy()
            || session.queued_messages.is_empty()
            || self.ending_checkpoint_pending(session_id)
        {
            return;
        }
        let Some(message) = self
            .state
            .session_mut(session_id)
            .map(|session| session.queued_messages.remove(0))
        else {
            return;
        };
        self.submit_submission_for_session(
            session_id,
            ComposerSubmission::from_queued_message(message),
            cx,
        );
    }

    fn submit_submission_for_session(
        &mut self,
        session_id: Uuid,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        if self.response_fork_preparations.contains_key(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if self.ending_checkpoint_pending(session_id) {
            self.enqueue_follow_up_submission(session_id, submission, cx);
            self.defer_queue_drain(session_id);
            return;
        }
        if session.status.is_busy() {
            self.enqueue_follow_up_submission(session_id, submission, cx);
            return;
        }
        let prompt = submission.prompt.clone();
        let human_prompt = submission.human_prompt();
        let has_input = !submission
            .display_content
            .as_deref()
            .unwrap_or(&submission.prompt)
            .trim()
            .is_empty();
        let next_turn_count = session.turns.len() + 1;
        let provider = session.provider.id();
        let model = self
            .session_options(session)
            .model
            .unwrap_or_else(|| "default".into());
        let workspace_kind = if session.workspace.is_worktree() {
            "worktree"
        } else {
            "local"
        };
        let attachment_count = submission.attachments.len();
        let project_id = session.project_id;
        let workspace = session.workspace.clone();
        let driver_start = (!self.runtimes.contains_key(&session_id)).then(|| {
            let provisional_cwd = self
                .workspace_path_for_session(session)
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            self.driver_start_request_for_session(session, provisional_cwd)
        });
        let Some(project) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            if selected {
                self.restore_composer_submission(submission, cx);
                self.show_toast(tr!("errors.prepare_task_project_not_found"));
            }
            cx.notify();
            return;
        };
        let projectless = project.is_projectless();
        // Busy is visible before any Git work begins. The separate transient
        // set keeps this non-cancellable phase visually distinct from a
        // connecting provider, whose runtime already has a working Stop path.
        //
        // The turn also begins now, not once preparation settles: the sent
        // message and its working indicator belong in the transcript the
        // moment the submission is accepted — a first prompt otherwise leaves
        // the empty state on screen for as long as a `git add -A` takes.
        // Preparation failure unwinds the turn and restores the prompt.
        if selected {
            self.sync_transcript_rows();
        }
        let previous_kinds = if selected {
            self.transcript_row_kinds.borrow().clone()
        } else {
            Vec::new()
        };
        let transcript_anchor = if let Some(session) = self.state.session_mut(session_id) {
            session.set_title_from_prompt(&human_prompt);
            let turn_id = session.begin_turn_with_presentation(
                &prompt,
                submission.display_content.clone(),
                submission.attachments.clone(),
            );
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            selected.then_some(TranscriptAnchor {
                session_id,
                turn_id,
            })
        } else {
            None
        };
        self.analytics
            .track(crate::analytics::Event::TurnSubmitted {
                provider,
                model,
                turn_number: next_turn_count,
                workspace: workspace_kind,
                projectless,
                attachment_count,
                has_input,
            });
        self.submission_preparations.insert(session_id);
        if selected {
            self.activities_expanded.clear();
            self.expanded_activity_items.clear();
            self.expanded_turns.clear();
            self.expanded_changed_files.clear();
            self.transcript_control_focuses.borrow_mut().clear();
            self.message_edit = None;
            self.hide_toast();
            self.transcript_anchor.set(transcript_anchor);
            self.transcript_anchor_end_space.set(Pixels::ZERO);
            self.transcript_anchor_following.set(true);
            self.splice_transcript_rows_after_visibility_change(&previous_kinds);
            self.scroll_transcript_to_anchor();
        }
        cx.notify();

        let preparation_prompt = human_prompt;
        cx.spawn(async move |waku, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    prepare_submission(
                        project,
                        workspace,
                        driver_start,
                        session_id,
                        &preparation_prompt,
                        next_turn_count,
                    )
                })
                .await;
            let _ = waku.update(cx, move |waku, cx| {
                waku.finish_submission_preparation(session_id, submission, prepared, cx);
            });
        })
        .detach();
    }

    fn finish_submission_preparation(
        &mut self,
        session_id: Uuid,
        submission: ComposerSubmission,
        prepared: anyhow::Result<PreparedSubmission>,
        cx: &mut Context<Self>,
    ) {
        if !self.submission_preparations.contains(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.submission_preparations.remove(&session_id);
                self.track_active_turn_outcome(
                    session_id,
                    crate::analytics::TurnOutcome::PreparationFailed,
                );
                if selected {
                    self.sync_transcript_rows();
                }
                let previous_kinds = if selected {
                    self.transcript_row_kinds.borrow().clone()
                } else {
                    Vec::new()
                };
                if let Some(session) = self.state.session_mut(session_id)
                    && session.status == SessionStatus::Connecting
                {
                    // The submission never reached a provider and its prompt
                    // returns to the composer, so the eagerly-begun turn and
                    // its message leave the transcript with it.
                    if let Some(turn_id) = session.active_turn_id() {
                        session.unwind_unstarted_turn(turn_id);
                    }
                    session.status = SessionStatus::Idle;
                }
                if selected {
                    if self
                        .transcript_anchor
                        .get()
                        .is_some_and(|anchor| anchor.session_id == session_id)
                    {
                        self.transcript_anchor.set(None);
                        self.transcript_anchor_following.set(false);
                    }
                    self.splice_transcript_rows_after_visibility_change(&previous_kinds);
                    self.restore_composer_submission(submission, cx);
                    self.show_toast(tr!("errors.create_worktree", error = error));
                }
                cx.notify();
                return;
            }
        };
        let PreparedSubmission {
            workspace,
            checkpoint_warning,
            driver: prepared_driver,
        } = prepared;
        // The turn began at accept time; it must still be the untouched one
        // this preparation belongs to. Cancellation is blocked while the
        // preparation set holds the session, so a mismatch means the session
        // was replaced under the preparation rather than a user action.
        let can_start = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                session.status == SessionStatus::Connecting
                    && session.turns.last().is_some_and(|turn| {
                        turn.status == TurnStatus::Running && !turn.provider_turn_started
                    })
            });
        if !can_start {
            self.submission_preparations.remove(&session_id);
            cx.notify();
            return;
        }

        let workspace_changed = self.state.session_mut(session_id).is_some_and(|session| {
            let changed = session.workspace != workspace;
            session.workspace = workspace;
            changed
        });
        if selected && workspace_changed {
            self.invalidate_workspace_queries(cx);
            self.reload_clean_right_panel_file_editors(cx);
            self.ensure_right_panel_terminals(cx);
        }
        let driver = match prepared_driver {
            None => self
                .runtimes
                .get(&session_id)
                .map(|runtime| runtime.driver.clone())
                .ok_or_else(|| anyhow::anyhow!(tr!("errors.prepared_runtime_unavailable"))),
            Some(Ok(prepared)) => Ok(self.install_prepared_driver(session_id, prepared)),
            Some(Err(error)) => Err(error),
        };
        self.invalidate_checkpoint_refs();
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime
                .pending_events
                .retain(|event| matches!(event, DriverEvent::BackgroundWork(_)));
            runtime.pending_steers.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
            runtime.pending_computer_approval = None;
            runtime.last_active_at = Instant::now();
        }
        // The transcript already shows the turn — the prompt message, its
        // anchor, and the working indicator all landed at accept time. Only
        // preparation's own output surfaces here.
        if selected && let Some(warning) = checkpoint_warning {
            self.show_toast(warning);
        }
        // Template commands expand here, at the seam between the transcript
        // and the transport: the user message keeps the typed `/name …` —
        // the same echo the CLIs show — while the provider receives the
        // rendered prompt. Claude's commands pass through untouched; its CLI
        // owns their expansion.
        let prompt = submission.prompt;
        let driver_prompt =
            crate::composer_complete::expanded_submission(&prompt, &self.slash_command_index)
                .unwrap_or(prompt);
        let mut failed_to_start = false;
        match driver {
            Ok(driver) => driver.prompt(driver_prompt),
            Err(error) => {
                failed_to_start = true;
                let message = tr!("errors.start_agent", error = error);
                if let Some(session) = self.state.session_mut(session_id) {
                    session.status = SessionStatus::Failed;
                    session.push_message(MessageRole::Assistant, message);
                }
                self.finish_active_turn_with_analytics(
                    session_id,
                    TurnStatus::Failed,
                    crate::analytics::TurnOutcome::StartFailed,
                );
            }
        }
        // From this point onward `cancel_turn` has either a live driver to
        // cancel or a settled startup failure. The next frame must therefore
        // show Stop (or Send after failure), never the preparation spinner.
        self.submission_preparations.remove(&session_id);
        if failed_to_start {
            self.capture_latest_turn_checkpoint_for(session_id);
            self.start_pending_checkpoint_captures(cx);
        }
        cx.notify();
        // Persist on the next frame boundary. Saving is intentionally after
        // the spinner-to-Stop paint: SQLite or blob externalization must not
        // hold the final preparation frame motionless.
        cx.spawn(async move |waku, cx| {
            cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
            let _ = waku.update(cx, |waku, _| waku.save());
        })
        .detach();
    }

    pub(super) fn collect_runtime_events(runtime: &mut SessionRuntime) {
        while let Ok(event) = runtime.events.try_recv() {
            runtime.pending_events.push_back(event);
        }
    }

    pub(super) fn drain_event_pump(&mut self, cx: &mut Context<Self>) -> EventPumpSchedule {
        // `|` on purpose: a busy provider must not starve the other result
        // queues just because its own drain reported a change first.
        if self.drain_driver_events(cx)
            | self.drain_provider_probe_events()
            | self.drain_provider_version_events()
            | self.drain_provider_detection_events()
            | self.drain_computer_permission_events()
            | self.drain_plan_usage_events()
        {
            cx.notify();
        }
        if std::mem::take(&mut self.workspace_queries_stale) {
            self.invalidate_workspace_queries(cx);
        }
        if std::mem::take(&mut self.composer_sources_stale) {
            self.refresh_composer_sources(cx);
        }
        self.maybe_refresh_background_work(cx);
        // A finished turn asks for a checkpoint from a handler with no
        // `Context`; this is where that `git` work leaves the UI thread.
        self.start_pending_checkpoint_captures(cx);

        if self
            .runtimes
            .values()
            .any(|runtime| !runtime.pending_events.is_empty() || runtime.stream_remeasure_pending)
        {
            EventPumpSchedule::StreamFrame
        } else if let Some(delay) = self.background_output_refresh_delay() {
            EventPumpSchedule::BackgroundOutput(delay)
        } else {
            EventPumpSchedule::Idle
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
                Err(error) => self.show_toast(error),
            }
            changed = true;
        }
        changed
    }

    pub(super) fn drain_driver_events(&mut self, cx: &mut Context<Self>) -> bool {
        let session_ids = self.runtimes.keys().copied().collect::<Vec<_>>();
        let mut changed = false;
        let mut persisted_state_changed = false;
        let mut force_save = false;
        let mut selected_changed = false;
        for session_id in session_ids {
            let Some(mut runtime) = self.runtimes.remove(&session_id) else {
                continue;
            };
            let follow_up_remeasure = std::mem::take(&mut runtime.stream_remeasure_pending);
            Self::collect_runtime_events(&mut runtime);
            let mut runtime_changed = false;
            let mut background_changed = false;
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
                let background_event = matches!(event, DriverEvent::BackgroundWork(_));
                let background_output_delta = matches!(
                    event,
                    DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { .. })
                );
                force_save |= matches!(
                    event,
                    DriverEvent::Connected { .. }
                        | DriverEvent::AutoTitleUpdated(_)
                        | DriverEvent::Permission { .. }
                        | DriverEvent::SteerAccepted { .. }
                        | DriverEvent::SteerRejected { .. }
                        | DriverEvent::TurnFinished { .. }
                        | DriverEvent::Error(_)
                        | DriverEvent::ProcessExited
                );
                markdown_changed |= matches!(event, DriverEvent::TextDelta(_));
                if background_output_delta {
                    // The registry batches log text into SharedString at 10Hz;
                    // repainting and saving for every provider chunk would
                    // turn a noisy command into UI-thread work.
                } else if background_event {
                    background_changed = true;
                } else {
                    runtime_changed = true;
                }
                keep_runtime &= self.handle_driver_event(session_id, &mut runtime, event, true, cx);
                if !keep_runtime {
                    break;
                }
            }
            runtime.stream_remeasure_pending = markdown_changed;
            if keep_runtime {
                self.runtimes.insert(session_id, runtime);
            }
            changed |= runtime_changed || background_changed;
            persisted_state_changed |= runtime_changed;
            if self.state.selected_session == Some(session_id)
                && (runtime_changed || follow_up_remeasure)
            {
                selected_changed = true;
            }
        }

        if !self.pending_queue_drains.is_empty() {
            let drains = std::mem::take(&mut self.pending_queue_drains);
            for session_id in drains {
                if self.ending_checkpoint_pending(session_id) {
                    self.defer_queue_drain(session_id);
                } else {
                    self.drain_queued_message(session_id, cx);
                }
            }
            changed = true;
        }

        if persisted_state_changed {
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

#[cfg(test)]
mod response_fork_title_tests {
    use super::next_response_fork_title;

    #[test]
    fn response_fork_titles_advance_one_numbered_sequence() {
        assert_eq!(
            next_response_fork_title("Fix the bug", ["Fix the bug"]),
            "Fix the bug (2)"
        );
        assert_eq!(
            next_response_fork_title(
                "Fix the bug",
                ["Fix the bug", "Fix the bug (2)", "Fix the bug (4)"]
            ),
            "Fix the bug (5)"
        );
        assert_eq!(
            next_response_fork_title("Fix the bug (2)", ["Fix the bug", "Fix the bug (2)"]),
            "Fix the bug (3)"
        );
        assert_eq!(
            next_response_fork_title("Plan (2026)", ["Plan (2026)"]),
            "Plan (2026) (2)"
        );
    }
}

/// Run `<cli> --version` and pull a version number out of whatever it prints.
/// Blocking; runs on a version-probe thread, never on the UI thread.
fn probe_provider_version(binary: &std::path::Path) -> Option<String> {
    let mut command = crate::command_env::command(binary);
    let command = command.arg("--version").stdin(std::process::Stdio::null());
    let output = crate::command_env::output(command).ok()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_cli_version(&combined)
}

/// The first token that reads as a version number — digits and dots, an
/// optional leading `v`, optional pre-release tail — from the first non-empty
/// line. CLIs decorate this differently ("codex-cli 0.45.0", "2.1.24 (Claude
/// Code)", "v1.3.0-beta"); the number is the part worth showing.
fn parse_cli_version(output: &str) -> Option<String> {
    let line = output.lines().find(|line| !line.trim().is_empty())?;
    line.split_whitespace()
        .map(|token| {
            token
                .trim_start_matches('v')
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        })
        .find(|token| {
            let mut parts = token.split('.');
            let leading_number = parts
                .next()
                .is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
            leading_number
                && parts
                    .next()
                    .is_some_and(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod version_tests {
    use super::parse_cli_version;

    #[test]
    fn parses_common_cli_version_banners() {
        assert_eq!(
            parse_cli_version("codex-cli 0.45.0\n"),
            Some("0.45.0".to_owned())
        );
        assert_eq!(
            parse_cli_version("2.1.24 (Claude Code)\n"),
            Some("2.1.24".to_owned())
        );
        assert_eq!(
            parse_cli_version("v1.3.0-beta.2"),
            Some("1.3.0-beta.2".to_owned())
        );
        assert_eq!(
            parse_cli_version("\nAmp CLI version 0.9.12\n"),
            Some("0.9.12".to_owned())
        );
        assert_eq!(parse_cli_version("not a version"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    #[test]
    fn version_requires_a_dotted_number_not_a_bare_digit() {
        // "2024" alone or a hash must not read as a version.
        assert_eq!(parse_cli_version("build 2024 f3a9c1"), None);
        assert_eq!(
            parse_cli_version("cursor-agent 2025.09.12-4f8d8e2"),
            Some("2025.09.12-4f8d8e2".to_owned())
        );
    }
}

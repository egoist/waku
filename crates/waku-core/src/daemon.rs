//! Provider backend and driver-event wire translation for `waku-daemon`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::{
    Backend, Command, EventSink, Request, ResponsePayload, WireDriverEvent, WorkspaceOperation,
    WorkspaceResult,
};
use anyhow::{Context as _, anyhow, bail};
use chrono::Local;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::attachments::AttachmentStore;
use crate::automation::{AutomationRun, RunOutcome};
use crate::computer_use::{ComputerTarget, ComputerUsePhase, ComputerUseState};
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::model::{
    ActivityKind, AgentSession, Checkpoint, CheckpointStatus, DriverEvent, MessageRole,
    PermissionOption, Project, ProviderKind, ProviderResumeCursor, SessionStatus, TurnStatus,
};
use crate::persistence::{ComposerDraftStore, PersistedState, StateStore};
use crate::settings::DaemonSettingsStore;
use waku_protocol::provider_session::{ProviderSessionFork, ProviderSessionForkRequest};

const AUTOMATION_CATCH_UP_GRACE_SECS: i64 = 5 * 60;

#[derive(Debug)]
enum AutomationStartResult {
    Started {
        automation: waku_protocol::automation::Automation,
        session: AgentSession,
        runtime_id: Uuid,
        supports_steer: bool,
    },
    Skipped,
    Deferred,
}

pub struct WakuBackend {
    sessions: Arc<Mutex<HashMap<Uuid, (Uuid, DriverHandle)>>>,
    terminals: Mutex<HashMap<Uuid, (Uuid, crate::terminal::DaemonTerminal)>>,
    settings: DaemonSettingsStore,
    task_store: Arc<StateStore>,
    task_state: Arc<Mutex<PersistedState>>,
    removed_session_ids: Arc<Mutex<HashSet<Uuid>>>,
    composer_drafts: ComposerDraftStore,
    attachments: AttachmentStore,
    usage_scan_cache: Mutex<crate::usage_history::ScanCache>,
    checkpoint_capture_locks: Mutex<HashMap<(PathBuf, Uuid, usize), Arc<Mutex<()>>>>,
    automation_locks: Mutex<HashMap<Uuid, Arc<Mutex<()>>>>,
    /// Automations whose start is running off the scheduler thread but has not
    /// written its durable claim yet. Planning skips them so an occurrence
    /// cannot be resolved twice in that window.
    automation_starts_in_flight: Mutex<HashSet<Uuid>>,
    /// Handles for the work automations push off the request and scheduler
    /// threads — starting a run, and sweeping blobs after a cascade delete.
    /// Joined by [`Backend::shutdown`] so a run still spawning a provider
    /// cannot install it after the session map has been swept, and so a sweep
    /// cannot still be walking the data directory after the daemon exits.
    automation_worker_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    usage_rates_dir: std::path::PathBuf,
    default_cwd: std::path::PathBuf,
}

impl WakuBackend {
    pub fn new(settings: DaemonSettingsStore, task_store: StateStore) -> anyhow::Result<Self> {
        let mut task_state = task_store
            .load()
            .context("could not load Waku task database")?;
        migrate_projectless_state(&task_store, &mut task_state)?;
        if recover_interrupted_automation_runs(&mut task_state) {
            task_store
                .save(&mut task_state)
                .context("could not persist interrupted automation recovery")?;
        }
        let composer_drafts = ComposerDraftStore::for_state_path(task_store.path());
        let attachments = AttachmentStore::new(
            task_store
                .path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("attachments"),
        );
        let usage_rates_dir = task_store
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_owned();
        Ok(Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            terminals: Mutex::new(HashMap::new()),
            settings,
            task_store: Arc::new(task_store),
            task_state: Arc::new(Mutex::new(task_state)),
            removed_session_ids: Arc::new(Mutex::new(HashSet::new())),
            composer_drafts,
            attachments,
            usage_scan_cache: Mutex::new(HashMap::new()),
            checkpoint_capture_locks: Mutex::new(HashMap::new()),
            automation_locks: Mutex::new(HashMap::new()),
            automation_starts_in_flight: Mutex::new(HashSet::new()),
            automation_worker_threads: Mutex::new(Vec::new()),
            usage_rates_dir,
            default_cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        })
    }

    /// Capture and persist one ending checkpoint exactly once per daemon.
    /// Desktop and Web may observe the same turn completion concurrently; a
    /// per-turn lock prevents both clients from running the expensive Git
    /// snapshot while leaving unrelated tasks independent.
    fn capture_turn_checkpoint(
        &self,
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<Checkpoint> {
        let key = (cwd.clone(), session_id, turn_count);
        let capture_lock = self
            .checkpoint_capture_locks
            .lock()
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _capture = capture_lock.lock();

        {
            let mut state = self.task_state.lock();
            if let Some(index) = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
            {
                self.task_store.hydrate(&mut state.sessions[index])?;
                if let Some(checkpoint) = state.sessions[index]
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)
                    .and_then(|turn| turn.checkpoint.as_ref())
                    .filter(|checkpoint| {
                        matches!(
                            checkpoint.status,
                            CheckpointStatus::Ready | CheckpointStatus::Unavailable
                        )
                    })
                {
                    return Ok(checkpoint.clone());
                }
            }
        }

        let checkpoint = crate::checkpoint::capture_turn(&cwd, session_id, turn_count)?;
        let mut state = self.task_state.lock();
        if let Some(index) = state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        {
            self.task_store.hydrate(&mut state.sessions[index])?;
            if let Some(turn) = state.sessions[index]
                .turns
                .iter_mut()
                .find(|turn| turn.turn_count == turn_count)
            {
                turn.checkpoint = Some(checkpoint.clone());
                state.mark_session_dirty(session_id);
                self.task_store.save(&mut state)?;
            }
        }
        Ok(checkpoint)
    }
}

/// A daemon restart cannot retain any provider runtime. Resolve the durable
/// running markers before the scheduler starts so a stale run never blocks
/// overlap checks forever. The marker remains at its last occurrence, which
/// lets the normal planner coalesce one catch-up run if the schedule is due.
fn recover_interrupted_automation_runs(task_state: &mut PersistedState) -> bool {
    let mut changed = false;
    for automation in &mut task_state.automations {
        for run in &mut automation.history {
            if run.outcome == RunOutcome::Running {
                run.outcome = RunOutcome::Cancelled;
                changed = true;
            }
        }
    }
    changed
}

/// Storage-layout migrations belong to the daemon because both the database
/// rows and the directories name paths on its host. Persist after each move
/// so a later failure cannot leave an earlier project pointing at its old
/// location in SQLite.
fn migrate_projectless_state(
    task_store: &StateStore,
    task_state: &mut PersistedState,
) -> anyhow::Result<()> {
    let indices = task_state
        .projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            crate::projectless::needs_migration(&project.path).then_some(index)
        })
        .collect::<Vec<_>>();
    for index in indices {
        let old_path = task_state.projects[index].path.clone();
        let workspace = crate::projectless::migrate_workspace(&old_path).with_context(|| {
            format!(
                "could not move projectless workspace {} under ~/.waku/projects",
                old_path.display()
            )
        })?;
        task_state.projects[index].name = crate::model::Project::PROJECTLESS_NAME.to_owned();
        task_state.projects[index].path = workspace.cwd;
        task_store
            .save(task_state)
            .context("could not persist migrated projectless workspace")?;
    }
    Ok(())
}

impl Backend for WakuBackend {
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload> {
        let session_id = request.session_id;
        let runtime_id = request.runtime_id;
        match request.command {
            Command::AttachSession => {
                let sessions = self.sessions.lock();
                let Some((runtime_id, driver)) = sessions.get(&session_id) else {
                    return Ok(ResponsePayload::SessionRuntime {
                        runtime_id: None,
                        supports_steer: false,
                    });
                };
                Ok(ResponsePayload::SessionRuntime {
                    runtime_id: Some(*runtime_id),
                    supports_steer: driver.supports_steer(),
                })
            }
            Command::GetSettings => Ok(ResponsePayload::Settings {
                settings: self.settings.get(),
            }),
            Command::UpdateSettings { settings } => {
                self.settings.replace(settings)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ProbeProvider {
                provider,
                binary_override,
                discover_models,
                probe_version,
            } => {
                ensure_shell_environment();
                let mut probe = match binary_override.as_deref() {
                    override_value if discover_models || probe_version => {
                        crate::model::provider_probe(provider, override_value)
                    }
                    override_value => crate::model::cached_provider_probe(provider, override_value),
                };
                let version = probe_version
                    .then(|| {
                        probe
                            .path
                            .as_deref()
                            .and_then(crate::model::probe_provider_version)
                    })
                    .flatten();
                if discover_models {
                    probe = crate::model::discover_provider_models(probe);
                }
                Ok(ResponsePayload::ProviderProbe { probe, version })
            }
            Command::FetchPlanUsage {
                provider,
                binary_override,
                cli_version,
            } => {
                let usage = match provider {
                    crate::model::ProviderKind::Claude => Some(
                        crate::usage::fetch_claude_plan_usage(cli_version.as_deref())?,
                    ),
                    crate::model::ProviderKind::Codex => {
                        Some(crate::usage::fetch_codex_plan_usage()?)
                    }
                    crate::model::ProviderKind::OpenCode => {
                        crate::usage::fetch_opencode_go_plan_usage()?
                    }
                    crate::model::ProviderKind::Grok => {
                        ensure_shell_environment();
                        let probe = match binary_override.as_deref() {
                            override_value => {
                                crate::model::provider_probe(provider, override_value)
                            }
                        };
                        let binary = probe.path.ok_or_else(|| anyhow!("grok is not installed"))?;
                        Some(crate::usage::fetch_grok_plan_usage(&binary)?)
                    }
                    _ => bail!("provider has no plan usage fetcher"),
                };
                Ok(ResponsePayload::PlanUsage { usage })
            }
            Command::ProbeComputerPermissions { prompt } => {
                Ok(ResponsePayload::ComputerPermissions {
                    permissions: crate::computer_use::probe_permissions(prompt)?,
                })
            }
            Command::LoadUsageHistory {
                window,
                project_roots,
            } => {
                let rates = crate::usage_history::load_rate_table(&self.usage_rates_dir);
                let history = crate::usage_history::scan(
                    &mut self.usage_scan_cache.lock(),
                    &rates,
                    window,
                    &project_roots,
                );
                Ok(ResponsePayload::UsageHistory { history })
            }
            Command::LoadSkills { projects } => {
                let locations = crate::skills::skill_locations(&projects);
                Ok(ResponsePayload::SkillsCatalog {
                    catalog: crate::skills::scan_skills(&locations),
                })
            }
            Command::SetSkillsEnabled { dirs, enabled } => {
                for dir in dirs {
                    crate::skills::set_skill_enabled(&dir, enabled)
                        .map_err(|error| anyhow!(error))?;
                }
                Ok(ResponsePayload::Ack)
            }
            Command::TrashSkills { dirs } => {
                crate::skills::trash_skills(&dirs).map_err(|error| anyhow!(error))?;
                Ok(ResponsePayload::Ack)
            }
            Command::LoadTaskState => {
                let state = self.task_state.lock();
                Ok(ResponsePayload::TaskState {
                    projects: state.projects.clone(),
                    sessions: state
                        .sessions
                        .iter()
                        .map(AgentSession::list_projection)
                        .collect(),
                    automations: state.automations.clone(),
                    default_cwd: self.default_cwd.clone(),
                    projectless_root: crate::projectless::workspace_root(),
                })
            }
            Command::RunAutomation {
                automation_id,
                catch_up,
            } => match self.start_automation_run(automation_id, catch_up, events.clone())? {
                AutomationStartResult::Started {
                    automation,
                    session,
                    runtime_id,
                    supports_steer,
                } => Ok(ResponsePayload::AutomationRunStarted {
                    automation,
                    session,
                    runtime_id,
                    supports_steer,
                }),
                AutomationStartResult::Skipped => {
                    bail!("automation {automation_id} is already running")
                }
                AutomationStartResult::Deferred => {
                    bail!("automation {automation_id} is queued until its current run finishes")
                }
            },
            Command::ApplyAutomationChanges { changes } => self.apply_automation_changes(changes),
            Command::SaveTaskState {
                projects,
                live_session_ids: _,
                sessions,
            } => {
                let active_runtimes = self
                    .sessions
                    .lock()
                    .iter()
                    .map(|(session_id, (runtime_id, _))| (*session_id, *runtime_id))
                    .collect::<HashMap<_, _>>();
                let mut state = self.task_state.lock();
                let removed_session_ids = self.removed_session_ids.lock();
                for project in projects {
                    if let Some(existing) = state
                        .projects
                        .iter_mut()
                        .find(|existing| existing.id == project.id)
                    {
                        *existing = project;
                    } else {
                        state.projects.push(project);
                    }
                }
                let sessions = sessions
                    .into_iter()
                    .filter(|session| !removed_session_ids.contains(&session.id))
                    .collect::<Vec<_>>();
                drop(removed_session_ids);
                let saved_ids = sessions
                    .iter()
                    .map(|session| session.id)
                    .collect::<Vec<_>>();
                for mut session in sessions {
                    if let Some(existing) = state
                        .sessions
                        .iter_mut()
                        .find(|existing| existing.id == session.id)
                    {
                        if session_projection_precedes(
                            existing,
                            &session,
                            active_runtimes.get(&session.id).copied(),
                        ) {
                            merge_stale_session_metadata(existing, session);
                        } else {
                            preserve_daemon_checkpoints(existing, &mut session);
                            *existing = session;
                        }
                    } else {
                        state.sessions.push(session);
                    }
                }
                let used_project_ids = state
                    .sessions
                    .iter()
                    .map(|session| session.project_id)
                    .collect::<std::collections::HashSet<_>>();
                state.projects.retain(|project| {
                    !project.is_projectless() || used_project_ids.contains(&project.id)
                });
                for session_id in &saved_ids {
                    state.mark_session_dirty(*session_id);
                }
                self.task_store.save(&mut state)?;
                let sessions = saved_ids
                    .into_iter()
                    .filter_map(|session_id| {
                        state
                            .sessions
                            .iter()
                            .find(|session| session.id == session_id)
                            .cloned()
                    })
                    .collect();
                Ok(ResponsePayload::TaskStateSaved { sessions })
            }
            Command::RemoveSession => {
                let originating_automation = {
                    let state = self.task_state.lock();
                    state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .and_then(|session| session.originating_automation)
                };
                let automation_lock =
                    originating_automation.map(|automation_id| self.automation_lock(automation_id));
                let automation_guard = automation_lock.as_ref().map(|lock| lock.lock());
                let automation_cancelled;
                {
                    let mut state = self.task_state.lock();
                    self.removed_session_ids.lock().insert(session_id);
                    automation_cancelled = cancel_automation_run_for_removed_session(
                        &mut state,
                        session_id,
                        originating_automation,
                    );
                    let project_id = state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.project_id);
                    state.sessions.retain(|session| session.id != session_id);
                    if let Some(project_id) = project_id {
                        let remove_project = state
                            .projects
                            .iter()
                            .find(|project| project.id == project_id)
                            .is_some_and(Project::is_projectless)
                            && !state
                                .sessions
                                .iter()
                                .any(|session| session.project_id == project_id);
                        if remove_project {
                            state.projects.retain(|project| project.id != project_id);
                        }
                    }
                    self.task_store.save(&mut state)?;
                }
                drop(automation_guard);
                drop(automation_lock);
                if automation_cancelled {
                    events.task_state_changed();
                }
                let removed = self.sessions.lock().remove(&session_id);
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            Command::HydrateSession { session_id } => {
                let mut state = self.task_state.lock();
                let session = if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    self.task_store.hydrate(session)?;
                    Some(session.clone())
                } else {
                    None
                };
                Ok(ResponsePayload::Session { session })
            }
            Command::SearchSessionMessages { query, limit } => {
                let matches = self.task_store.session_message_search(query, limit)()?;
                Ok(ResponsePayload::SessionMessageMatches { matches })
            }
            Command::LoadComposerDrafts => Ok(ResponsePayload::ComposerDrafts {
                drafts: self.composer_drafts.load()?,
            }),
            Command::SaveComposerDrafts { drafts, generation } => {
                self.composer_drafts.save(drafts, generation)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ApplyComposerDraftChanges { changes } => {
                self.composer_drafts.apply_changes(changes)?;
                Ok(ResponsePayload::Ack)
            }
            Command::StoreBlob { mime_type, bytes } => {
                let reference = self
                    .task_store
                    .blobs()
                    .store_image_bytes(&mime_type, &bytes)?;
                let path = self
                    .task_store
                    .blobs()
                    .path_for(&reference)
                    .ok_or_else(|| anyhow!("stored blob has no daemon path"))?;
                Ok(ResponsePayload::BlobStored { reference, path })
            }
            Command::ImportAttachment { name, upload } => Ok(ResponsePayload::AttachmentStored {
                attachment: self.attachments.import(&name, upload)?,
            }),
            Command::ImportPathAttachment { path } => Ok(ResponsePayload::AttachmentStored {
                attachment: self.attachments.import_path(&path)?,
            }),
            Command::ReadBlob { reference } => {
                let path = self
                    .task_store
                    .blobs()
                    .path_for(&reference)
                    .ok_or_else(|| anyhow!("invalid blob reference"))?;
                Ok(ResponsePayload::BlobData {
                    bytes: std::fs::read(path)?,
                })
            }
            Command::ReadAttachment { reference, path } => Ok(ResponsePayload::BlobData {
                bytes: self.attachments.read_file(&reference, &path)?,
            }),
            Command::SweepBlobs => {
                self.task_store.blob_sweep()();
                Ok(ResponsePayload::Ack)
            }
            Command::ForkSessionFromResponse { turn_count } => {
                let (session, checkpoint_warning) =
                    self.fork_session_from_response(session_id, turn_count)?;
                Ok(ResponsePayload::SessionForked {
                    session,
                    checkpoint_warning,
                })
            }
            Command::RewindSessionToMessage { turn_count } => {
                let (session, cleanup_warning) =
                    self.rewind_session_to_message(session_id, turn_count)?;
                Ok(ResponsePayload::SessionRewound {
                    session,
                    cleanup_warning,
                })
            }
            Command::ForkProviderSession { request } => {
                Ok(ResponsePayload::ProviderSessionForked {
                    result: fork_provider_session(request)?,
                })
            }
            Command::Workspace {
                operation:
                    WorkspaceOperation::CaptureTurn {
                        cwd,
                        session_id,
                        turn_count,
                    },
            } => Ok(ResponsePayload::Workspace {
                result: WorkspaceResult::Checkpoint {
                    checkpoint: self.capture_turn_checkpoint(cwd, session_id, turn_count)?,
                },
            }),
            Command::Workspace { operation } => Ok(ResponsePayload::Workspace {
                result: crate::workspace::execute(operation)?,
            }),
            Command::OpenTerminal { cwd, cols, rows } => {
                ensure_shell_environment();
                let terminal = crate::terminal::DaemonTerminal::open(&cwd, cols, rows, events)?;
                let previous = self
                    .terminals
                    .lock()
                    .insert(session_id, (runtime_id, terminal));
                drop(previous);
                Ok(ResponsePayload::Ack)
            }
            Command::WriteTerminal { data } => {
                let terminals = self.terminals.lock();
                let (active_runtime_id, terminal) = terminals
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("daemon terminal {session_id} is not running"))?;
                if *active_runtime_id != runtime_id {
                    bail!(
                        "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                    );
                }
                terminal.write(data)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ResizeTerminal { cols, rows } => {
                let terminals = self.terminals.lock();
                let (active_runtime_id, terminal) = terminals
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("daemon terminal {session_id} is not running"))?;
                if *active_runtime_id != runtime_id {
                    bail!(
                        "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                    );
                }
                terminal.resize(cols, rows);
                Ok(ResponsePayload::Ack)
            }
            Command::CloseTerminal => {
                let removed = {
                    let mut terminals = self.terminals.lock();
                    if let Some((active_runtime_id, _)) = terminals.get(&session_id) {
                        if *active_runtime_id != runtime_id {
                            bail!(
                                "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                            );
                        }
                    }
                    terminals.remove(&session_id)
                };
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            Command::Start { options } => {
                let previous = self.sessions.lock().remove(&session_id);
                drop(previous);
                let provider = decode_enum(&options.provider)?;
                let options = DriverStartOptions {
                    binary: options.binary,
                    cwd: options.cwd,
                    mode: decode_enum(&options.mode)?,
                    interaction_mode: decode_enum(&options.interaction_mode)?,
                    model: options.model,
                    reasoning_effort: options.reasoning_effort,
                    service_tier: options.service_tier,
                    context_window: options.context_window,
                    agent_preset: options.agent_preset,
                    computer_use_enabled: options.computer_use_enabled,
                    provider_cursor: options
                        .provider_cursor
                        .map(serde_json::from_value)
                        .transpose()
                        .context("daemon received an invalid provider cursor")?,
                };
                let (wake, _wake_events) = smol::channel::bounded(1);
                let (event_sender, event_receiver) = driver::event_channel(wake);
                let handle = driver::start_local(provider, options, event_sender)?;
                let supports_steer = handle.supports_steer();
                std::thread::Builder::new()
                    .name(format!("waku-daemon-events-{session_id}"))
                    .spawn(move || {
                        while let Ok(event) = event_receiver.recv() {
                            let wire = event_to_wire(event).unwrap_or_else(|error| {
                                WireDriverEvent::new(
                                    "error",
                                    Value::String(format!(
                                        "could not encode daemon event: {error}"
                                    )),
                                )
                            });
                            if events.send(wire).is_err() {
                                break;
                            }
                        }
                    })
                    .context("could not start daemon event forwarding thread")?;
                self.sessions
                    .lock()
                    .insert(session_id, (runtime_id, handle));
                Ok(ResponsePayload::Started { supports_steer })
            }
            Command::CloseSession => {
                let removed = {
                    let mut sessions = self.sessions.lock();
                    sessions
                        .get(&session_id)
                        .is_some_and(|(active_runtime_id, _)| *active_runtime_id == runtime_id)
                        .then(|| sessions.remove(&session_id))
                        .flatten()
                };
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            command => {
                let driver = {
                    let sessions = self.sessions.lock();
                    let (active_runtime_id, driver) = sessions
                        .get(&session_id)
                        .ok_or_else(|| anyhow!("daemon session {session_id} is not running"))?;
                    if *active_runtime_id != runtime_id {
                        bail!(
                            "daemon session {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                        );
                    }
                    driver.clone()
                };
                handle_driver_command(&driver, command)
            }
        }
    }

    fn tick(self: Arc<Self>, events: EventSink) {
        self.tick_automations(events);
    }

    fn shutdown(&self) {
        // Drain the off-thread starts first. Each one may still be about to
        // install a provider into `sessions`, and anything installed after the
        // sweep below would outlive the daemon as an orphaned process.
        self.join_automation_workers();
        let sessions = std::mem::take(&mut *self.sessions.lock());
        drop(sessions);
        let terminals = std::mem::take(&mut *self.terminals.lock());
        drop(terminals);
    }
}

impl WakuBackend {
    fn automation_lock(&self, automation_id: Uuid) -> Arc<Mutex<()>> {
        self.automation_locks
            .lock()
            .entry(automation_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn automation_snapshot(
        &self,
        automation_id: Uuid,
        fallback: waku_protocol::automation::Automation,
    ) -> waku_protocol::automation::Automation {
        self.task_state
            .lock()
            .automations
            .iter()
            .find(|candidate| candidate.id == automation_id)
            .cloned()
            .unwrap_or(fallback)
    }

    fn apply_automation_changes(
        &self,
        changes: Vec<waku_protocol::automation::AutomationChange>,
    ) -> anyhow::Result<ResponsePayload> {
        for change in changes {
            let automation_id = change.automation_id();
            let lock = self.automation_lock(automation_id);
            let mut removed_handles = Vec::new();
            let mut removed_automation = None;
            let mut swept_sessions = false;
            {
                let _automation_guard = lock.lock();
                match change {
                    waku_protocol::automation::AutomationChange::Upsert { automation } => {
                        let mut state = self.task_state.lock();
                        if let Some(existing) = state
                            .automations
                            .iter_mut()
                            .find(|existing| existing.id == automation.id)
                        {
                            merge_automation_update(existing, automation);
                        } else {
                            state.automations.push(automation);
                        }
                        self.task_store.save(&mut state)?;
                    }
                    waku_protocol::automation::AutomationChange::Remove {
                        automation_id,
                        cascade_sessions,
                    } => {
                        removed_automation = Some(automation_id);
                        let mut state = self.task_state.lock();
                        if cascade_sessions {
                            swept_sessions = true;
                            let session_ids = state
                                .sessions
                                .iter()
                                .filter(|session| {
                                    session.originating_automation == Some(automation_id)
                                })
                                .map(|session| session.id)
                                .collect::<HashSet<_>>();
                            {
                                let mut sessions = self.sessions.lock();
                                removed_handles.extend(
                                    session_ids
                                        .iter()
                                        .filter_map(|session_id| sessions.remove(session_id)),
                                );
                            }
                            self.removed_session_ids
                                .lock()
                                .extend(session_ids.iter().copied());
                            state
                                .sessions
                                .retain(|session| !session_ids.contains(&session.id));
                            let used_project_ids = state
                                .sessions
                                .iter()
                                .map(|session| session.project_id)
                                .collect::<HashSet<_>>();
                            state.projects.retain(|project| {
                                !project.is_projectless() || used_project_ids.contains(&project.id)
                            });
                        }
                        state
                            .automations
                            .retain(|automation| automation.id != automation_id);
                        self.task_store.save(&mut state)?;
                    }
                }
            }
            // Runtime teardown may join its event forwarder, which also takes
            // this automation lock. Drop handles only after releasing it.
            drop(removed_handles);
            if let Some(automation_id) = removed_automation {
                self.release_automation_lock(automation_id, &lock);
            }
            if swept_sessions {
                // A client that deletes one session follows up with
                // `SweepBlobs`, but a cascade removes an unknown number of them
                // inside a single command, so the caller has nothing to react
                // to. Sweeping here is what keeps those sessions' attachments
                // from leaking. It walks the database and the attachment
                // directory, so it does not belong on the connection thread
                // holding up the delete response.
                let task_store = self.task_store.clone();
                match std::thread::Builder::new()
                    .name("waku-daemon-automation-cascade-sweep".into())
                    .spawn(move || task_store.blob_sweep()())
                {
                    // Tracked, not detached: the sweep walks the data directory,
                    // so it has to be joined at teardown rather than left racing
                    // whatever tears that directory down.
                    Ok(handle) => self.automation_worker_threads.lock().push(handle),
                    Err(error) => eprintln!(
                        "could not start the blob sweep for an automation cascade: {error}"
                    ),
                }
            }
        }
        let automations = self.task_state.lock().automations.clone();
        Ok(ResponsePayload::AutomationChangesApplied { automations })
    }

    /// Drop a deleted automation's lock entry, but only while this call holds
    /// the sole remaining handle.
    ///
    /// The map lock is held across the check so no one can clone the entry in
    /// between. A strong count of two means the map and `held` — any waiter
    /// would raise it, and handing a waiter a lock that no longer guards the
    /// same entry is exactly how two callers end up running concurrently for
    /// one id.
    fn release_automation_lock(&self, automation_id: Uuid, held: &Arc<Mutex<()>>) {
        let mut locks = self.automation_locks.lock();
        if locks
            .get(&automation_id)
            .is_some_and(|existing| Arc::ptr_eq(existing, held) && Arc::strong_count(existing) == 2)
        {
            locks.remove(&automation_id);
        }
    }

    fn tick_automations(self: &Arc<Self>, events: EventSink) {
        self.automation_worker_threads
            .lock()
            .retain(|start| !start.is_finished());
        // Cloned rather than held, so the planning block never nests this lock
        // inside the task-state lock.
        let in_flight = self.automation_starts_in_flight.lock().clone();
        let now = Local::now().naive_local();
        let decisions = {
            let state = self.task_state.lock();
            let ticks = state
                .automations
                .iter()
                // A start already running off-thread has not persisted its
                // claim yet. Leaving the marker untouched re-evaluates the
                // occurrence next tick, by which point the claim makes the
                // automation read as active and the overlap policy applies
                // normally — rather than racing a second run past the lock.
                .filter(|automation| !in_flight.contains(&automation.id))
                .map(|automation| crate::automation::planner::AutomationTick {
                    automation,
                    marker: local_naive(automation.last_run_at.unwrap_or(automation.created_at)),
                    active: automation
                        .history
                        .iter()
                        .any(|run| run.outcome == RunOutcome::Running),
                })
                .collect::<Vec<_>>();
            crate::automation::planner::plan(
                &ticks,
                now,
                chrono::Duration::seconds(AUTOMATION_CATCH_UP_GRACE_SECS),
            )
        };

        let mut changed = false;
        let mut firing = Vec::new();
        for decision in decisions {
            match decision {
                crate::automation::planner::PlanDecision::Fire { id, catch_up } => {
                    firing.push((id, catch_up));
                }
                crate::automation::planner::PlanDecision::Skip { id, catch_up } => {
                    if self.record_scheduled_skip(id, catch_up) {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            events.task_state_changed();
        }
        for (id, catch_up) in firing {
            self.spawn_automation_start(id, catch_up, events.clone());
        }
    }

    /// Wait for every off-thread automation worker to finish. The guard lock is
    /// released before joining so a start that is still claiming its occurrence
    /// cannot deadlock against this drain.
    fn join_automation_workers(&self) {
        let starts = std::mem::take(&mut *self.automation_worker_threads.lock());
        for start in starts {
            let _ = start.join();
        }
    }

    /// Start one planned occurrence off the scheduler thread.
    ///
    /// Claiming the occurrence, materializing a worktree, and spawning the
    /// provider all block — the worktree path shells out to `git` and the
    /// provider path spawns a process — so none of it may run on the scheduler
    /// loop. Serially, one slow start delays every other automation due in the
    /// same minute; one hung start stalls scheduling for the daemon's lifetime.
    fn spawn_automation_start(self: &Arc<Self>, id: Uuid, catch_up: bool, events: EventSink) {
        if !self.automation_starts_in_flight.lock().insert(id) {
            return;
        }
        let backend = self.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("waku-daemon-automation-start-{id}"))
            .spawn(move || {
                let result = backend.start_automation_run(id, catch_up, events.clone());
                backend.automation_starts_in_flight.lock().remove(&id);
                match result {
                    Ok(AutomationStartResult::Started { .. })
                    | Ok(AutomationStartResult::Skipped) => events.task_state_changed(),
                    Ok(AutomationStartResult::Deferred) => {}
                    Err(error) => {
                        eprintln!("could not start scheduled automation {id}: {error:#}")
                    }
                }
            });
        match spawned {
            Ok(handle) => self.automation_worker_threads.lock().push(handle),
            Err(error) => {
                self.automation_starts_in_flight.lock().remove(&id);
                eprintln!("could not start a thread for scheduled automation {id}: {error}");
            }
        }
    }

    fn record_scheduled_skip(&self, automation_id: Uuid, catch_up: bool) -> bool {
        let lock = self.automation_lock(automation_id);
        let _automation_lock = lock.lock();
        let mut state = self.task_state.lock();
        let Some(automation) = state
            .automations
            .iter_mut()
            .find(|automation| automation.id == automation_id)
        else {
            return false;
        };
        // The planner works from a snapshot. Re-check the overlap state at the
        // mutation boundary so a run that started after planning is not
        // incorrectly consumed as a skipped occurrence.
        if !automation.enabled
            || automation.overlap != waku_protocol::automation::OverlapPolicy::Skip
            || !automation
                .history
                .iter()
                .any(|run| run.outcome == RunOutcome::Running)
        {
            return false;
        }
        let now = crate::model::unix_time();
        automation.record_run(AutomationRun::skipped(now, catch_up));
        automation.last_run_at = Some(now);
        if let Err(error) = self.task_store.save(&mut state) {
            eprintln!("could not persist skipped automation {automation_id}: {error}");
            return false;
        }
        true
    }

    fn start_automation_run(
        &self,
        automation_id: Uuid,
        catch_up: bool,
        events: EventSink,
    ) -> anyhow::Result<AutomationStartResult> {
        // Planning and on-demand requests race by design. The same lock covers
        // the active-run check, the history append, and provider startup so a
        // scheduled occurrence can never double-fire with Run-now or another
        // scheduler tick.
        let lock = self.automation_lock(automation_id);
        let _automation_lock = lock.lock();
        let (mut automation, project, project_exists) = {
            let mut state = self.task_state.lock();
            let Some(automation) = state
                .automations
                .iter()
                .find(|automation| automation.id == automation_id)
                .cloned()
            else {
                bail!("automation {automation_id} was not found");
            };
            if automation.prompt.trim().is_empty() {
                bail!("automation {automation_id} has no prompt");
            }
            let project = automation
                .project_id
                .and_then(|project_id| {
                    state
                        .projects
                        .iter()
                        .find(|project| project.id == project_id)
                })
                .cloned();
            let project_exists = project.is_some();
            let active = automation
                .history
                .iter()
                .any(|run| run.outcome == RunOutcome::Running);
            if active {
                match automation.overlap {
                    waku_protocol::automation::OverlapPolicy::Concurrent => {}
                    waku_protocol::automation::OverlapPolicy::Skip => {
                        let now = crate::model::unix_time();
                        if let Some(current) = state
                            .automations
                            .iter_mut()
                            .find(|current| current.id == automation_id)
                        {
                            current.record_run(AutomationRun::skipped(now, catch_up));
                            current.last_run_at = Some(now);
                            self.task_store.save(&mut state)?;
                            return Ok(AutomationStartResult::Skipped);
                        }
                        bail!("automation {automation_id} was deleted before it could be skipped");
                    }
                    waku_protocol::automation::OverlapPolicy::Queue => {
                        return Ok(AutomationStartResult::Deferred);
                    }
                }
            }
            (automation, project, project_exists)
        };

        if automation.normalize_project_binding(project_exists) {
            automation.updated_at = crate::model::unix_time();
        }
        let project = match project {
            Some(project) => project,
            None => {
                let root = crate::projectless::workspace_root()
                    .ok_or_else(|| anyhow!("projectless workspace is unavailable"))?;
                let mut project = Project::from_path(root);
                project.name = Project::PROJECTLESS_NAME.to_owned();
                project
            }
        };
        let project_exists = project_exists && automation.project_id == Some(project.id);
        let mut session = AgentSession::new(project.id, automation.agent.provider);
        session.model = automation.agent.model.clone();
        session.reasoning_effort = automation.agent.reasoning_effort.clone();
        session.service_tier = automation.agent.service_tier.clone();
        session.agent_preset = automation.agent.agent_preset.clone();
        session.runtime_mode = automation.agent.runtime_mode;
        session.interaction_mode = automation.agent.interaction_mode;
        session.workspace = automation.workspace_for_project(project_exists);
        session.originating_automation = Some(automation_id);
        session.begin_turn(automation.prompt.clone());
        session.status = SessionStatus::Connecting;
        let runtime_id = Uuid::new_v4();
        let session_id = session.id;

        // Claim and persist the occurrence before materializing its workspace.
        // A worktree failure is still a real attempted run: it advances the
        // schedule marker, settles durably, and cannot retry every scheduler
        // tick.
        {
            let mut state = self.task_state.lock();
            if let Some(current) = state
                .automations
                .iter_mut()
                .find(|automation| automation.id == automation_id)
            {
                // The execution lock makes this snapshot current for
                // user-authored fields; the merge still protects history if a
                // terminal event was persisted just before this boundary.
                merge_automation_update(current, automation.clone());
                let now = crate::model::unix_time();
                current.record_run(AutomationRun::spawned(session.id, now, catch_up));
                current.last_run_at = Some(now);
            } else {
                bail!("automation {automation_id} was deleted before it could run");
            }
            if !state
                .projects
                .iter()
                .any(|candidate| candidate.id == project.id)
            {
                state.projects.push(project.clone());
            }
            state.push_session(session.clone());
            self.task_store
                .save(&mut state)
                .context("could not persist automation run start")?;
        }

        if let crate::model::SessionWorkspace::NewWorktree { base_branch } = &session.workspace {
            let worktree = match crate::workspace::execute(WorkspaceOperation::CreateWorktree {
                project_path: project.path.clone(),
                project_id: project.id,
                session_id: session.id,
                prompt: automation.prompt.clone(),
                base_branch: base_branch.clone(),
            }) {
                Ok(WorkspaceResult::WorktreeCreated { worktree }) => worktree,
                Ok(_) => {
                    let error = "daemon returned an invalid worktree result".to_owned();
                    self.finish_automation_start_failure(
                        &mut session,
                        automation_id,
                        error,
                        events.for_runtime(session_id, runtime_id),
                    );
                    return Ok(AutomationStartResult::Started {
                        automation: self.automation_snapshot(automation_id, automation),
                        session,
                        runtime_id,
                        supports_steer: false,
                    });
                }
                Err(error) => {
                    self.finish_automation_start_failure(
                        &mut session,
                        automation_id,
                        error.to_string(),
                        events.for_runtime(session_id, runtime_id),
                    );
                    return Ok(AutomationStartResult::Started {
                        automation: self.automation_snapshot(automation_id, automation),
                        session,
                        runtime_id,
                        supports_steer: false,
                    });
                }
            };
            session.workspace = crate::model::SessionWorkspace::Worktree {
                path: worktree.path,
                branch: worktree.branch,
            };
            let mut state = self.task_state.lock();
            if let Some(stored) = state
                .sessions
                .iter_mut()
                .find(|stored| stored.id == session.id)
            {
                stored.workspace = session.workspace.clone();
                state.mark_session_dirty(session.id);
                self.task_store
                    .save(&mut state)
                    .context("could not persist automation worktree")?;
            }
        }

        let cwd = session
            .workspace
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| project.path.clone());

        let runtime_events = events.for_runtime(session.id, runtime_id);
        let binary = match self.provider_binary(automation.agent.provider) {
            Ok(binary) => binary,
            Err(error) => {
                self.finish_automation_start_failure(
                    &mut session,
                    automation_id,
                    error.to_string(),
                    runtime_events,
                );
                return Ok(AutomationStartResult::Started {
                    automation: self.automation_snapshot(automation_id, automation),
                    session,
                    runtime_id,
                    supports_steer: false,
                });
            }
        };
        let (wake, _wake_events) = smol::channel::bounded(1);
        let (event_sender, event_receiver) = driver::event_channel(wake);
        let driver = match driver::start_local(
            automation.agent.provider,
            DriverStartOptions {
                binary,
                cwd,
                mode: automation.agent.runtime_mode,
                interaction_mode: automation.agent.interaction_mode,
                model: automation.agent.model.clone(),
                reasoning_effort: automation.agent.reasoning_effort.clone(),
                service_tier: automation.agent.service_tier.clone(),
                context_window: None,
                agent_preset: automation.agent.agent_preset.clone(),
                computer_use_enabled: self.settings.get().computer_use_enabled,
                provider_cursor: None,
            },
            event_sender,
        ) {
            Ok(driver) => driver,
            Err(error) => {
                self.finish_automation_start_failure(
                    &mut session,
                    automation_id,
                    error.to_string(),
                    runtime_events,
                );
                return Ok(AutomationStartResult::Started {
                    automation: self.automation_snapshot(automation_id, automation),
                    session,
                    runtime_id,
                    supports_steer: false,
                });
            }
        };
        let supports_steer = driver.supports_steer();
        self.sessions
            .lock()
            .insert(session.id, (runtime_id, driver.clone()));
        self.spawn_automation_event_forwarder(
            session.id,
            runtime_id,
            automation_id,
            driver,
            event_receiver,
            runtime_events,
        );
        // The event thread is installed before the first prompt so a provider
        // that answers synchronously cannot race the subscription/replay path.
        if let Some((_, driver)) = self.sessions.lock().get(&session.id) {
            driver.prompt(automation.prompt.clone());
        }
        let persisted_automation = self
            .task_state
            .lock()
            .automations
            .iter()
            .find(|candidate| candidate.id == automation_id)
            .cloned()
            .unwrap_or(automation);
        Ok(AutomationStartResult::Started {
            automation: persisted_automation,
            session,
            runtime_id,
            supports_steer,
        })
    }

    fn finish_automation_start_failure(
        &self,
        session: &mut AgentSession,
        automation_id: Uuid,
        error: String,
        events: EventSink,
    ) {
        let runtime_events = if events.runtime_id() == Uuid::nil() {
            events.for_runtime(session.id, Uuid::new_v4())
        } else {
            events
        };
        let _ = runtime_events.send(
            event_to_wire(DriverEvent::Error(error.clone()))
                .unwrap_or_else(|_| WireDriverEvent::new("error", Value::String(error.clone()))),
        );
        let _ = runtime_events.send(
            event_to_wire(DriverEvent::TurnFinished {
                success: false,
                summary: Some(error),
            })
            .unwrap_or_else(|_| WireDriverEvent::new("turnFinished", Value::Null)),
        );
        let mut notification = None;
        {
            let mut state = self.task_state.lock();
            if let Some(stored) = state
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == session.id)
            {
                stored.status = SessionStatus::Failed;
                stored.push_message(MessageRole::Assistant, "The automation could not start.");
                stored.finish_active_turn(TurnStatus::Failed);
                *session = stored.clone();
                state.mark_session_dirty(session.id);
            }
            if let Some(automation) = state
                .automations
                .iter_mut()
                .find(|automation| automation.id == automation_id)
                && automation.settle_session_run(session.id, RunOutcome::Failed)
                && automation.notification.matches_outcome(RunOutcome::Failed)
            {
                notification = Some(automation.name.clone());
            }
            if let Err(save_error) = self.task_store.save(&mut state) {
                eprintln!("could not persist failed automation start: {save_error}");
                notification = None;
            }
        }
        // Notification intent is emitted only after the terminal outcome is
        // durable. If persistence fails, clients still receive the runtime
        // failure events but must not present an outcome the daemon did not
        // commit.
        if let Some(name) = notification {
            runtime_events.automation_notification(crate::protocol::AutomationNotification {
                session_id: session.id,
                name,
                outcome: RunOutcome::Failed,
            });
        }
        runtime_events.task_state_changed();
        runtime_events.end_runtime();
    }

    fn spawn_automation_event_forwarder(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        automation_id: Uuid,
        _driver: DriverHandle,
        event_receiver: crossbeam_channel::Receiver<DriverEvent>,
        events: EventSink,
    ) {
        let sessions = self.sessions.clone();
        let task_store = self.task_store.clone();
        let task_state = self.task_state.clone();
        let automation_lock = self.automation_lock(automation_id);
        std::thread::Builder::new()
            .name(format!("waku-daemon-automation-{session_id}"))
            .spawn(move || {
                while let Ok(event) = event_receiver.recv() {
                    let terminal = matches!(
                        event,
                        DriverEvent::TurnFinished { .. } | DriverEvent::ProcessExited
                    );
                    if let Ok(wire) = event_to_wire(event.clone()) {
                        let _ = events.send(wire);
                    }
                    let notification = apply_automation_driver_event(
                        &task_state,
                        &task_store,
                        &automation_lock,
                        session_id,
                        automation_id,
                        &event,
                    );
                    if let Some(name) = notification {
                        events.automation_notification(crate::protocol::AutomationNotification {
                            session_id,
                            name,
                            outcome: match event {
                                DriverEvent::TurnFinished { success: true, .. } => {
                                    RunOutcome::Succeeded
                                }
                                _ => RunOutcome::Failed,
                            },
                        });
                    }
                    // The automation history is part of task state, not the
                    // runtime transcript. Wake catalog subscribers for every
                    // terminal outcome, even when notification policy is off.
                    if terminal {
                        events.task_state_changed();
                    }
                    if terminal {
                        if matches!(event, DriverEvent::ProcessExited) {
                            let should_remove = sessions.lock().get(&session_id).is_some_and(
                                |(active_runtime_id, _)| *active_runtime_id == runtime_id,
                            );
                            if should_remove {
                                sessions.lock().remove(&session_id);
                                events.end_runtime();
                            }
                        }
                    }
                }
            })
            .ok();
    }
}

/// Merge client-authored fields without allowing a stale automation snapshot to
/// erase daemon-owned run history or the marker used by the scheduler.
fn merge_automation_update(
    existing: &mut waku_protocol::automation::Automation,
    incoming: waku_protocol::automation::Automation,
) {
    let history = std::mem::take(&mut existing.history);
    let last_run_at = existing.last_run_at;
    let created_at = existing.created_at;
    let updated_at = existing.updated_at.max(incoming.updated_at);
    *existing = incoming;
    existing.history = history;
    existing.last_run_at = last_run_at;
    existing.created_at = created_at;
    existing.updated_at = updated_at;
}

fn cancel_automation_run_for_removed_session(
    state: &mut PersistedState,
    session_id: Uuid,
    automation_id: Option<Uuid>,
) -> bool {
    let Some(automation_id) = automation_id else {
        return false;
    };
    state
        .automations
        .iter_mut()
        .find(|automation| automation.id == automation_id)
        .is_some_and(|automation| automation.settle_session_run(session_id, RunOutcome::Cancelled))
}

fn local_naive(timestamp: u64) -> chrono::NaiveDateTime {
    chrono::DateTime::from_timestamp(timestamp as i64, 0)
        .map(|instant| instant.with_timezone(&Local).naive_local())
        .unwrap_or_else(|| Local::now().naive_local())
}

/// Apply the small daemon-owned projection needed when no client is attached.
/// Attached clients still receive every provider event and persist their richer
/// transcript projection; the daemon must at least retain the prompt, terminal
/// status, and automation outcome on its own.
fn apply_automation_driver_event(
    task_state: &Arc<Mutex<PersistedState>>,
    task_store: &Arc<StateStore>,
    automation_lock: &Arc<Mutex<()>>,
    session_id: Uuid,
    automation_id: Uuid,
    event: &DriverEvent,
) -> Option<String> {
    let terminal = matches!(
        event,
        DriverEvent::TurnFinished { .. } | DriverEvent::ProcessExited
    );
    let _automation_guard = automation_lock.lock();
    let mut notification = None;
    let mut changed = false;
    let mut state = task_state.lock();
    if let Some(session) = state
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        match event {
            DriverEvent::Connected { provider_cursor } => {
                session.provider_cursor = provider_cursor.clone();
                if session.status == SessionStatus::Connecting {
                    session.status = SessionStatus::Working;
                }
                changed = true;
            }
            DriverEvent::AgentPresetSelected(preset) => {
                session.agent_preset = preset.clone();
                changed = true;
            }
            DriverEvent::AutoTitleUpdated(title) => {
                changed = session.set_auto_title(title.clone()) || changed;
            }
            DriverEvent::TurnStarted => {
                session.mark_active_turn_provider_started();
                session.status = SessionStatus::Working;
                changed = true;
            }
            DriverEvent::TextDelta(delta) => {
                if !delta.is_empty() && session.active_turn_id().is_some() {
                    let turn_id = session.active_turn_id();
                    let message = session.messages.last_mut().filter(|message| {
                        message.role == MessageRole::Assistant && message.turn_id == turn_id
                    });
                    if let Some(message) = message {
                        message.content.push_str(delta);
                        message.streaming = true;
                    } else {
                        let id = session.push_message(MessageRole::Assistant, delta.clone());
                        if let Some(message) =
                            session.messages.iter_mut().find(|message| message.id == id)
                        {
                            message.streaming = true;
                        }
                    }
                    changed = true;
                }
            }
            DriverEvent::Error(error) => {
                session.status = SessionStatus::Failed;
                if session.active_turn_id().is_some()
                    && !session.messages.iter().any(|message| {
                        message.role == MessageRole::Assistant
                            && message.turn_id == session.active_turn_id()
                    })
                {
                    session.push_message(MessageRole::Assistant, error.clone());
                }
                changed = true;
            }
            DriverEvent::TurnFinished { success, summary } => {
                for message in &mut session.messages {
                    if message.role == MessageRole::Assistant {
                        message.streaming = false;
                    }
                }
                if session.active_turn_id().is_some()
                    && !session.messages.iter().any(|message| {
                        message.role == MessageRole::Assistant
                            && message.turn_id == session.active_turn_id()
                    })
                {
                    session.push_message(
                        MessageRole::Assistant,
                        summary.clone().unwrap_or_else(|| {
                            if *success {
                                "The turn completed without a text response.".to_owned()
                            } else {
                                "The turn stopped before a response.".to_owned()
                            }
                        }),
                    );
                }
                session.status = if *success {
                    SessionStatus::Idle
                } else {
                    SessionStatus::Failed
                };
                session.finish_active_turn(if *success {
                    TurnStatus::Completed
                } else {
                    TurnStatus::Failed
                });
                changed = true;
            }
            DriverEvent::ProcessExited => {
                // Providers may exit after a successful turn. Do not turn an
                // already-settled automation back into a failed session just
                // because its resident process was released.
                if session.active_turn_id().is_some() || session.status.is_busy() {
                    for message in &mut session.messages {
                        if message.role == MessageRole::Assistant {
                            message.streaming = false;
                        }
                    }
                    if session.active_turn_id().is_some()
                        && !session.messages.iter().any(|message| {
                            message.role == MessageRole::Assistant
                                && message.turn_id == session.active_turn_id()
                        })
                    {
                        session.push_message(
                            MessageRole::Assistant,
                            "The agent exited before responding.",
                        );
                    }
                    session.status = SessionStatus::Failed;
                    session.finish_active_turn(TurnStatus::Failed);
                    changed = true;
                }
            }
            DriverEvent::ReasoningDelta(_)
            | DriverEvent::AvailableCommands(_)
            | DriverEvent::Activity { .. }
            | DriverEvent::RichActivity(_)
            | DriverEvent::BackgroundWork(_)
            | DriverEvent::Permission { .. }
            | DriverEvent::ComputerUseUpdated(_)
            | DriverEvent::SteerAccepted { .. }
            | DriverEvent::SteerRejected { .. }
            | DriverEvent::UsageUpdated { .. }
            | DriverEvent::PlanUsageUpdated(_)
            | DriverEvent::RuntimeEventCursorAdvanced(_)
            | DriverEvent::UserInputRequested { .. } => {}
        }
        if changed {
            session.updated_at = crate::model::unix_time();
            state.mark_session_dirty(session_id);
        }
    }

    if terminal {
        let outcome = match event {
            DriverEvent::TurnFinished { success: true, .. } => RunOutcome::Succeeded,
            _ => RunOutcome::Failed,
        };
        if let Some(automation) = state
            .automations
            .iter_mut()
            .find(|automation| automation.id == automation_id)
            && automation.settle_session_run(session_id, outcome)
        {
            if automation.notification.matches_outcome(outcome) {
                notification = Some(automation.name.clone());
            }
            changed = true;
        }
    }
    if changed && let Err(error) = task_store.save(&mut state) {
        eprintln!("could not persist daemon automation event: {error}");
        // Never emit notification intent ahead of durable settlement.
        notification = None;
    }
    notification
}

fn session_projection_precedes(
    existing: &AgentSession,
    incoming: &AgentSession,
    active_runtime_id: Option<Uuid>,
) -> bool {
    let existing_cursor = existing.runtime_event_cursor;
    let incoming_cursor = incoming.runtime_event_cursor;
    if let Some(active_runtime_id) = active_runtime_id {
        let existing_is_active =
            existing_cursor.is_some_and(|cursor| cursor.runtime_id == active_runtime_id);
        let incoming_is_active =
            incoming_cursor.is_some_and(|cursor| cursor.runtime_id == active_runtime_id);
        if existing_is_active != incoming_is_active {
            return existing_is_active;
        }
    }
    match (existing_cursor, incoming_cursor) {
        (Some(existing), Some(incoming))
            if existing.runtime_id == incoming.runtime_id && existing.epoch == incoming.epoch =>
        {
            incoming.sequence < existing.sequence
        }
        (Some(_), None) if existing.status.is_busy() => true,
        _ => incoming.updated_at < existing.updated_at,
    }
}

fn merge_stale_session_metadata(existing: &mut AgentSession, incoming: AgentSession) {
    if incoming.updated_at >= existing.updated_at {
        existing.title = incoming.title;
        existing.project_id = incoming.project_id;
        existing.workspace = incoming.workspace;
        existing.provider = incoming.provider;
        existing.model = incoming.model;
        existing.runtime_mode = incoming.runtime_mode;
        existing.interaction_mode = incoming.interaction_mode;
        existing.reasoning_effort = incoming.reasoning_effort;
        existing.service_tier = incoming.service_tier;
        existing.context_window = incoming.context_window;
        existing.agent_preset = incoming.agent_preset;
        existing.updated_at = incoming.updated_at;
        existing.last_reply_at = incoming.last_reply_at.or(existing.last_reply_at);
    }
    for queued in incoming.queued_messages {
        if !existing
            .queued_messages
            .iter()
            .any(|candidate| candidate.id == queued.id)
        {
            existing.queued_messages.push(queued);
        }
    }
}

/// Ending checkpoints are produced and stored by the daemon. A second client
/// may still save a projection created just before capture completed; never
/// let that stale projection erase the canonical Git snapshot.
fn preserve_daemon_checkpoints(existing: &AgentSession, incoming: &mut AgentSession) {
    for turn in &mut incoming.turns {
        let Some(checkpoint) = existing
            .turns
            .iter()
            .find(|candidate| candidate.turn_count == turn.turn_count)
            .and_then(|candidate| candidate.checkpoint.as_ref())
            .filter(|checkpoint| {
                matches!(
                    checkpoint.status,
                    CheckpointStatus::Ready | CheckpointStatus::Unavailable
                )
            })
        else {
            continue;
        };
        turn.checkpoint = Some(checkpoint.clone());
    }
}

impl WakuBackend {
    /// Fork a response using only daemon-host state.
    ///
    /// A browser must never reconstruct or persist this operation itself:
    /// provider-native sessions, checkpoint refs, and the task database all
    /// belong to the daemon and may be on another machine.
    fn fork_session_from_response(
        &self,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<(AgentSession, Option<String>)> {
        let (source, cwd, fork_title) = {
            let mut state = self.task_state.lock();
            let source_index = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("the source task is unavailable"))?;
            self.task_store
                .hydrate(&mut state.sessions[source_index])
                .context("could not load the source task")?;
            let source = state.sessions[source_index].clone();
            let project = state
                .projects
                .iter()
                .find(|project| project.id == source.project_id)
                .ok_or_else(|| anyhow!("the source task project is unavailable"))?;
            let cwd = source.workspace.path().unwrap_or(&project.path).to_owned();
            let fork_title = next_response_fork_title(
                source.display_title(),
                state
                    .sessions
                    .iter()
                    .filter(|session| session.project_id == source.project_id)
                    .map(AgentSession::display_title),
            );
            (source, cwd, fork_title)
        };

        validate_response_fork(&source, turn_count)?;
        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let turns_to_remove = source.provider_turns_after(turn_count);
        let (provider_cursor, message_ids) = self.fork_provider_response(
            &source,
            &cwd,
            &fork_title,
            turn_count,
            provider_turn_count,
            turns_to_remove,
        )?;
        let mut forked = source
            .fork_through_turn(turn_count, provider_cursor, &fork_title)
            .ok_or_else(|| anyhow!("the selected response cannot be copied"))?;
        if !message_ids.is_empty() {
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
                checkpoint.git_ref =
                    crate::checkpoint::checkpoint_ref(fork_id, checkpoint.turn_count);
            }
        }
        let checkpoint_warning =
            crate::checkpoint::copy_session_refs(&cwd, source.id, fork_id, turn_count)
                .err()
                .map(|error| error.to_string());

        let mut state = self.task_state.lock();
        state.push_session(forked.clone());
        if let Err(error) = self.task_store.save(&mut state) {
            state.sessions.retain(|session| session.id != fork_id);
            let _ = crate::checkpoint::delete_all_session_refs(&cwd, fork_id);
            return Err(error).context("could not save the forked task");
        }
        Ok((forked, checkpoint_warning))
    }

    /// Restore the daemon-host worktree, provider conversation, and stored
    /// transcript to immediately before one user turn.
    fn rewind_session_to_message(
        &self,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<(AgentSession, Option<String>)> {
        let (source, cwd) = {
            let mut state = self.task_state.lock();
            let source_index = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("the task is unavailable"))?;
            self.task_store
                .hydrate(&mut state.sessions[source_index])
                .context("could not load the task")?;
            let source = state.sessions[source_index].clone();
            let project = state
                .projects
                .iter()
                .find(|project| project.id == source.project_id)
                .ok_or_else(|| anyhow!("the task project is unavailable"))?;
            let cwd = source.workspace.path().unwrap_or(&project.path).to_owned();
            (source, cwd)
        };
        validate_message_rewind(&source, turn_count)?;

        // Resolve the executable before touching the worktree. Even native
        // transcript operations are immediately followed by a replacement
        // prompt, so accepting a rewind that cannot resume would strand the
        // user at a provider state the UI cannot continue.
        let binary = self.provider_binary(source.provider)?;
        let retained_turn_count = turn_count.saturating_sub(1);
        let previous_turn_count = source.turns.len();
        let rollback_turns = source.provider_turns_after(retained_turn_count);
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

        let turn_start_ref = crate::checkpoint::turn_start_ref(session_id, turn_count);
        let retained_ref = crate::checkpoint::checkpoint_ref(session_id, retained_turn_count);
        let restore_ref = if crate::checkpoint::has_ref(&cwd, &turn_start_ref) {
            turn_start_ref
        } else {
            retained_ref
        };
        if !crate::checkpoint::has_ref(&cwd, &restore_ref) {
            bail!("the checkpoint before this message is unavailable");
        }

        let safety_ref = format!("refs/waku/revert-backup-{session_id}-{}", Uuid::new_v4());
        crate::checkpoint::capture_ref(&cwd, &safety_ref)
            .context("could not create a rewind safety snapshot")?;
        if let Err(error) = crate::checkpoint::restore_ref(&cwd, &restore_ref) {
            return Err(restore_rewind_safety(
                &cwd,
                &safety_ref,
                "could not restore the selected checkpoint",
                error,
            ));
        }

        let provider_rewind = self.rewind_provider_response(
            &source,
            &cwd,
            &binary,
            retained_turn_count,
            rollback_turns,
            provider_turn_count,
            provider_resume_at,
        );
        let (provider_cursor, message_ids, reset_native_session) = match provider_rewind {
            Ok(result) => result,
            Err(error) => {
                return Err(restore_rewind_safety(
                    &cwd,
                    &safety_ref,
                    "the provider rejected the rewind",
                    error,
                ));
            }
        };

        let _ = crate::checkpoint::delete_ref(&cwd, &safety_ref);
        let cleanup_warning = crate::checkpoint::delete_turn_refs_after(
            &cwd,
            session_id,
            retained_turn_count,
            previous_turn_count,
        )
        .err()
        .map(|error| error.to_string());

        // Every provider resumes from the newly stored cursor on the next
        // prompt. Dropping a resident source driver also prevents its late
        // events from racing the rewound transcript.
        let removed = self.sessions.lock().remove(&session_id);
        drop(removed);

        let mut rewound = source.clone();
        if !message_ids.is_empty() {
            for turn in rewound.turns.iter_mut().take(retained_turn_count) {
                if let Some(remapped) = turn
                    .provider_resume_at
                    .as_ref()
                    .and_then(|message_id| message_ids.get(message_id))
                    .cloned()
                {
                    turn.provider_resume_at = Some(remapped);
                }
            }
        }
        if reset_native_session {
            rewound.provider_cursor = None;
        } else if let Some(cursor) = provider_cursor {
            rewound.provider_cursor = Some(cursor);
        }
        rewound.truncate_after_turn(retained_turn_count);
        rewound.status = SessionStatus::Idle;

        let mut state = self.task_state.lock();
        let existing = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("the task was removed while it was being rewound"))?;
        *existing = rewound.clone();
        state.mark_session_dirty(session_id);
        self.task_store
            .save(&mut state)
            .context("could not save the rewound task")?;
        Ok((rewound, cleanup_warning))
    }

    fn fork_provider_response(
        &self,
        source: &AgentSession,
        cwd: &Path,
        fork_title: &str,
        turn_count: usize,
        provider_turn_count: usize,
        turns_to_remove: usize,
    ) -> anyhow::Result<(ProviderResumeCursor, HashMap<String, String>)> {
        match source.provider {
            ProviderKind::Claude => {
                let Some(ProviderResumeCursor::Claude { session_id, .. }) =
                    source.provider_cursor.as_ref()
                else {
                    bail!("Claude's native session is unavailable");
                };
                let resume_at = source
                    .turns
                    .get(turn_count.saturating_sub(1))
                    .and_then(|turn| turn.provider_resume_at.clone());
                let fork = fork_provider_session(ProviderSessionForkRequest::Claude {
                    session_id: session_id.clone(),
                    resume_at,
                    turn_count: provider_turn_count,
                    title: fork_title.to_owned(),
                })?;
                Ok((fork.cursor, fork.message_ids))
            }
            ProviderKind::Codex | ProviderKind::DeepSeek | ProviderKind::Pi => Ok((
                self.fork_response_with_driver(source, cwd, turns_to_remove)?,
                HashMap::new(),
            )),
            ProviderKind::Cursor => {
                let fork = fork_provider_session(ProviderSessionForkRequest::Cursor {
                    source: source.clone(),
                    turn_count,
                })?;
                Ok((fork.cursor, HashMap::new()))
            }
            ProviderKind::Amp => {
                let Some(ProviderResumeCursor::Amp {
                    thread_id,
                    fork_context,
                }) = source.provider_cursor.as_ref()
                else {
                    bail!("Amp's native thread is unavailable");
                };
                let fork = fork_provider_session(ProviderSessionForkRequest::Amp {
                    binary: self.provider_binary(ProviderKind::Amp)?,
                    cwd: cwd.to_owned(),
                    thread_id: thread_id.clone(),
                    fork_context: fork_context.clone(),
                    turn_count: provider_turn_count,
                })?;
                Ok((fork.cursor, HashMap::new()))
            }
            ProviderKind::OpenCode => {
                let Some(ProviderResumeCursor::OpenCode { session_id }) =
                    source.provider_cursor.as_ref()
                else {
                    bail!("OpenCode's native session is unavailable");
                };
                let fork = fork_provider_session(ProviderSessionForkRequest::OpenCode {
                    binary: self.provider_binary(ProviderKind::OpenCode)?,
                    cwd: cwd.to_owned(),
                    session_id: session_id.clone(),
                    turn_count: provider_turn_count,
                })?;
                Ok((fork.cursor, HashMap::new()))
            }
            ProviderKind::Grok => {
                let Some(ProviderResumeCursor::Grok { session_id }) =
                    source.provider_cursor.as_ref()
                else {
                    bail!("Grok Build's native session is unavailable");
                };
                let fork = fork_provider_session(ProviderSessionForkRequest::Grok {
                    binary: self.provider_binary(ProviderKind::Grok)?,
                    cwd: cwd.to_owned(),
                    session_id: session_id.clone(),
                    turn_count: provider_turn_count,
                })?;
                Ok((fork.cursor, HashMap::new()))
            }
        }
    }

    fn fork_response_with_driver(
        &self,
        source: &AgentSession,
        cwd: &Path,
        turns_to_remove: usize,
    ) -> anyhow::Result<ProviderResumeCursor> {
        if let Some(driver) = self
            .sessions
            .lock()
            .get(&source.id)
            .map(|(_, driver)| driver.clone())
        {
            return driver.fork(turns_to_remove);
        }

        match source.provider {
            ProviderKind::Codex
                if !matches!(
                    source.provider_cursor.as_ref(),
                    Some(ProviderResumeCursor::Codex { .. })
                ) =>
            {
                bail!("Codex's native thread is unavailable");
            }
            ProviderKind::DeepSeek
                if !matches!(
                    source.provider_cursor.as_ref(),
                    Some(ProviderResumeCursor::DeepSeek { .. })
                ) =>
            {
                bail!("DeepSeek Harness's native session is unavailable");
            }
            ProviderKind::Pi
                if !matches!(
                    source.provider_cursor.as_ref(),
                    Some(ProviderResumeCursor::Pi {
                        session_file: Some(_),
                        ..
                    })
                ) =>
            {
                bail!("Pi's native session file is unavailable");
            }
            _ => {}
        }

        let (wake, _wake_events) = smol::channel::bounded(1);
        let (event_sender, _event_receiver) = driver::event_channel(wake);
        let driver = driver::start_local(
            source.provider,
            DriverStartOptions {
                binary: self.provider_binary(source.provider)?,
                cwd: cwd.to_owned(),
                mode: source.runtime_mode,
                interaction_mode: source.interaction_mode,
                model: source.model.clone(),
                reasoning_effort: source.reasoning_effort.clone(),
                service_tier: source.service_tier.clone(),
                context_window: source.context_window.clone(),
                agent_preset: source.agent_preset.clone(),
                computer_use_enabled: false,
                provider_cursor: source.provider_cursor.clone(),
            },
            event_sender,
        )?;
        driver.fork(turns_to_remove)
    }

    #[allow(clippy::too_many_arguments)]
    fn rewind_provider_response(
        &self,
        source: &AgentSession,
        cwd: &Path,
        binary: &Path,
        retained_turn_count: usize,
        rollback_turns: usize,
        provider_turn_count: usize,
        provider_resume_at: Option<String>,
    ) -> anyhow::Result<(Option<ProviderResumeCursor>, HashMap<String, String>, bool)> {
        if rollback_turns == 0 {
            return Ok((None, HashMap::new(), false));
        }
        let reset_native_session = retained_turn_count == 0
            && matches!(
                source.provider,
                ProviderKind::Claude | ProviderKind::Cursor | ProviderKind::Grok
            );
        if reset_native_session {
            return Ok((None, HashMap::new(), true));
        }

        match source.provider {
            ProviderKind::Claude => {
                let Some(ProviderResumeCursor::Claude { session_id, .. }) =
                    source.provider_cursor.as_ref()
                else {
                    bail!("Claude's native session is unavailable");
                };
                let fork = fork_provider_session(ProviderSessionForkRequest::Claude {
                    session_id: session_id.clone(),
                    resume_at: provider_resume_at,
                    turn_count: provider_turn_count,
                    title: format!("{} (rewind)", source.display_title()),
                })?;
                Ok((Some(fork.cursor), fork.message_ids, false))
            }
            ProviderKind::OpenCode => {
                let cursor = if let Some(driver) = self
                    .sessions
                    .lock()
                    .get(&source.id)
                    .map(|(_, driver)| driver.clone())
                {
                    driver
                        .rollback(rollback_turns)?
                        .ok_or_else(|| anyhow!("OpenCode returned no rewound-session cursor"))?
                } else {
                    let Some(ProviderResumeCursor::OpenCode { session_id }) =
                        source.provider_cursor.as_ref()
                    else {
                        bail!("OpenCode's native session is unavailable");
                    };
                    fork_provider_session(ProviderSessionForkRequest::OpenCode {
                        binary: binary.to_owned(),
                        cwd: cwd.to_owned(),
                        session_id: session_id.clone(),
                        turn_count: provider_turn_count,
                    })?
                    .cursor
                };
                Ok((Some(cursor), HashMap::new(), false))
            }
            ProviderKind::Amp => {
                let Some(ProviderResumeCursor::Amp {
                    thread_id,
                    fork_context,
                }) = source.provider_cursor.as_ref()
                else {
                    bail!("Amp's native thread is unavailable");
                };
                let cursor = fork_provider_session(ProviderSessionForkRequest::Amp {
                    binary: binary.to_owned(),
                    cwd: cwd.to_owned(),
                    thread_id: thread_id.clone(),
                    fork_context: fork_context.clone(),
                    turn_count: provider_turn_count,
                })?
                .cursor;
                Ok((Some(cursor), HashMap::new(), false))
            }
            ProviderKind::Cursor => {
                let cursor = fork_provider_session(ProviderSessionForkRequest::Cursor {
                    source: source.clone(),
                    turn_count: retained_turn_count,
                })?
                .cursor;
                Ok((Some(cursor), HashMap::new(), false))
            }
            ProviderKind::Grok => {
                let Some(ProviderResumeCursor::Grok { session_id }) =
                    source.provider_cursor.as_ref()
                else {
                    bail!("Grok Build's native session is unavailable");
                };
                let cursor = fork_provider_session(ProviderSessionForkRequest::Grok {
                    binary: binary.to_owned(),
                    cwd: cwd.to_owned(),
                    session_id: session_id.clone(),
                    turn_count: provider_turn_count,
                })?
                .cursor;
                Ok((Some(cursor), HashMap::new(), false))
            }
            ProviderKind::Codex | ProviderKind::DeepSeek | ProviderKind::Pi => Ok((
                self.rollback_response_with_driver(source, cwd, binary, rollback_turns)?,
                HashMap::new(),
                false,
            )),
        }
    }

    fn rollback_response_with_driver(
        &self,
        source: &AgentSession,
        cwd: &Path,
        binary: &Path,
        rollback_turns: usize,
    ) -> anyhow::Result<Option<ProviderResumeCursor>> {
        if let Some(driver) = self
            .sessions
            .lock()
            .get(&source.id)
            .map(|(_, driver)| driver.clone())
        {
            return driver.rollback(rollback_turns);
        }

        let (wake, _wake_events) = smol::channel::bounded(1);
        let (event_sender, _event_receiver) = driver::event_channel(wake);
        let driver = driver::start_local(
            source.provider,
            DriverStartOptions {
                binary: binary.to_owned(),
                cwd: cwd.to_owned(),
                mode: source.runtime_mode,
                interaction_mode: source.interaction_mode,
                model: source.model.clone(),
                reasoning_effort: source.reasoning_effort.clone(),
                service_tier: source.service_tier.clone(),
                context_window: source.context_window.clone(),
                agent_preset: source.agent_preset.clone(),
                computer_use_enabled: false,
                provider_cursor: source.provider_cursor.clone(),
            },
            event_sender,
        )?;
        driver.rollback(rollback_turns)
    }

    fn provider_binary(&self, provider: ProviderKind) -> anyhow::Result<PathBuf> {
        ensure_shell_environment();
        let settings = self.settings.get();
        let binary_override = settings
            .provider_binary_overrides
            .get(&provider)
            .map(String::as_str);
        crate::model::provider_probe(provider, binary_override)
            .path
            .ok_or_else(|| anyhow!("{} is not installed on the daemon", provider.display_name()))
    }
}

fn validate_message_rewind(source: &AgentSession, turn_count: usize) -> anyhow::Result<()> {
    if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
        bail!("stop the task before editing a prior message");
    }
    let Some(turn) = source
        .turns
        .iter()
        .find(|turn| turn.turn_count == turn_count)
    else {
        bail!("the selected message is unavailable");
    };
    if !source.messages.iter().any(|message| {
        message.turn_id == Some(turn.id) && message.role == crate::model::MessageRole::User
    }) {
        bail!("the selected user message is unavailable");
    }
    let rollback_turns = source.provider_turns_after(turn_count.saturating_sub(1));
    if rollback_turns > 0 && source.provider_cursor.is_none() {
        bail!("the provider conversation is unavailable");
    }
    Ok(())
}

fn restore_rewind_safety(
    cwd: &Path,
    safety_ref: &str,
    context: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match crate::checkpoint::restore_ref(cwd, safety_ref) {
        Ok(()) => {
            let _ = crate::checkpoint::delete_ref(cwd, safety_ref);
            anyhow!("{context}: {error}; the original worktree was restored")
        }
        Err(restore_error) => anyhow!(
            "{context}: {error}; restoring the safety snapshot also failed: {restore_error}; snapshot: {safety_ref}"
        ),
    }
}

fn validate_response_fork(source: &AgentSession, turn_count: usize) -> anyhow::Result<()> {
    if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
        bail!("stop the task before forking a response");
    }
    let cursor = source
        .provider_cursor
        .as_ref()
        .ok_or_else(|| anyhow!("the provider conversation is unavailable"))?;
    if cursor.provider() != source.provider {
        bail!("the provider conversation does not match this task");
    }
    if source
        .turns
        .get(turn_count.saturating_sub(1))
        .is_none_or(|turn| turn.turn_count != turn_count || !turn.provider_turn_started)
    {
        bail!("the selected response cannot be forked");
    }
    Ok(())
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

fn fork_provider_session(
    request: ProviderSessionForkRequest,
) -> anyhow::Result<ProviderSessionFork> {
    use crate::model::ProviderResumeCursor;

    let (cursor, message_ids, source_resume_at) = match request {
        ProviderSessionForkRequest::Claude {
            session_id,
            resume_at,
            turn_count,
            title,
        } => {
            let source_resume_at = resume_at.map(Ok).unwrap_or_else(|| {
                crate::claude_session::message_id_for_turn(&session_id, turn_count)
            })?;
            let fork =
                crate::claude_session::fork_session_at(&session_id, &source_resume_at, &title)?;
            let fork_resume_at = fork
                .message_ids
                .get(&source_resume_at)
                .cloned()
                .ok_or_else(|| anyhow!("Claude fork did not include its target message"))?;
            (
                ProviderResumeCursor::Claude {
                    session_id: fork.session_id,
                    resume_at: Some(fork_resume_at),
                },
                fork.message_ids,
                Some(source_resume_at),
            )
        }
        ProviderSessionForkRequest::Amp {
            binary,
            cwd,
            thread_id,
            fork_context,
            turn_count,
        } => (
            crate::amp_session::fork_session_at_turn(
                &binary,
                &cwd,
                &thread_id,
                fork_context.as_deref(),
                turn_count,
            )?,
            HashMap::new(),
            None,
        ),
        ProviderSessionForkRequest::Cursor { source, turn_count } => (
            crate::cursor_session::fork_session_at_turn(&source, turn_count)?,
            HashMap::new(),
            None,
        ),
        ProviderSessionForkRequest::OpenCode {
            binary,
            cwd,
            session_id,
            turn_count,
        } => (
            crate::opencode_session::fork_session_at_turn(&binary, &cwd, &session_id, turn_count)?,
            HashMap::new(),
            None,
        ),
        ProviderSessionForkRequest::Grok {
            binary,
            cwd,
            session_id,
            turn_count,
        } => (
            crate::grok_session::fork_session_at_turn(&binary, &cwd, &session_id, turn_count)?,
            HashMap::new(),
            None,
        ),
    };
    Ok(ProviderSessionFork {
        cursor,
        message_ids,
        source_resume_at,
    })
}

fn handle_driver_command(
    driver: &DriverHandle,
    command: Command,
) -> anyhow::Result<ResponsePayload> {
    match command {
        Command::Prompt { prompt } => driver.prompt(prompt),
        Command::Steer { prompt } => driver.steer(prompt),
        Command::Cancel => driver.cancel(),
        Command::CancelComputerUse => driver.cancel_computer_use(),
        Command::RefreshBackgroundWork => driver.refresh_background_work(),
        Command::StopBackgroundWork { key, control_id } => {
            driver.stop_background_work(
                serde_json::from_value(key).context("invalid background-work key")?,
                control_id,
            );
        }
        Command::Respond {
            request_id,
            option_id,
        } => driver.respond(request_id, option_id),
        Command::RespondUserInput {
            request_id,
            answers,
        } => driver.respond_user_input(request_id, answers),
        Command::RunComputerTool { request } => {
            driver.run_computer_tool(crate::computer_use::ComputerToolRequest {
                call_id: request.call_id,
                tool: request.tool,
                arguments: request.arguments,
            });
        }
        Command::RejectComputerTool { request, reason } => {
            driver.reject_computer_tool(
                crate::computer_use::ComputerToolRequest {
                    call_id: request.call_id,
                    tool: request.tool,
                    arguments: request.arguments,
                },
                reason,
            );
        }
        Command::ApplyOptions { options } => {
            return Ok(ResponsePayload::OptionsApplied {
                applied: driver.apply_options(SessionOptions {
                    mode: decode_enum(&options.mode)?,
                    interaction_mode: decode_enum(&options.interaction_mode)?,
                    model: options.model,
                    reasoning_effort: options.reasoning_effort,
                    service_tier: options.service_tier,
                    context_window: options.context_window,
                }),
            });
        }
        Command::Rollback { turns } => {
            let cursor = driver
                .rollback(turns)?
                .map(serde_json::to_value)
                .transpose()?;
            return Ok(ResponsePayload::Cursor { cursor });
        }
        Command::Fork { turns_to_remove } => {
            let cursor = Some(serde_json::to_value(driver.fork(turns_to_remove)?)?);
            return Ok(ResponsePayload::Cursor { cursor });
        }
        Command::AttachSession
        | Command::Start { .. }
        | Command::GetSettings
        | Command::UpdateSettings { .. }
        | Command::ProbeProvider { .. }
        | Command::FetchPlanUsage { .. }
        | Command::ProbeComputerPermissions { .. }
        | Command::LoadUsageHistory { .. }
        | Command::LoadSkills { .. }
        | Command::SetSkillsEnabled { .. }
        | Command::TrashSkills { .. }
        | Command::LoadTaskState
        | Command::RunAutomation { .. }
        | Command::ApplyAutomationChanges { .. }
        | Command::SaveTaskState { .. }
        | Command::RemoveSession
        | Command::HydrateSession { .. }
        | Command::SearchSessionMessages { .. }
        | Command::LoadComposerDrafts
        | Command::SaveComposerDrafts { .. }
        | Command::ApplyComposerDraftChanges { .. }
        | Command::StoreBlob { .. }
        | Command::ImportAttachment { .. }
        | Command::ImportPathAttachment { .. }
        | Command::ReadBlob { .. }
        | Command::ReadAttachment { .. }
        | Command::SweepBlobs
        | Command::ForkSessionFromResponse { .. }
        | Command::RewindSessionToMessage { .. }
        | Command::ForkProviderSession { .. }
        | Command::Workspace { .. }
        | Command::OpenTerminal { .. }
        | Command::WriteTerminal { .. }
        | Command::ResizeTerminal { .. }
        | Command::CloseTerminal
        | Command::CloseSession => {
            bail!("daemon received a command in the wrong dispatch path")
        }
    }
    Ok(ResponsePayload::Ack)
}

fn ensure_shell_environment() {
    static REFRESHED: OnceLock<()> = OnceLock::new();
    REFRESHED.get_or_init(|| {
        crate::command_env::refresh_from_default_shell();
    });
}

fn decode_enum<T: DeserializeOwned>(value: &str) -> anyhow::Result<T> {
    serde_json::from_value(Value::String(value.to_owned()))
        .with_context(|| format!("invalid protocol enum value {value:?}"))
}

pub fn encode_enum<T: Serialize>(value: T) -> anyhow::Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("protocol enum did not serialize as a string"))
}

fn event_to_wire(event: DriverEvent) -> anyhow::Result<WireDriverEvent> {
    let (kind, payload) = match event {
        DriverEvent::RuntimeEventCursorAdvanced(_) => {
            bail!("client-only runtime cursors cannot be sent by the daemon")
        }
        DriverEvent::Connected { provider_cursor } => {
            ("connected", serde_json::to_value(provider_cursor)?)
        }
        DriverEvent::AgentPresetSelected(preset) => {
            ("agentPresetSelected", serde_json::to_value(preset)?)
        }
        DriverEvent::AutoTitleUpdated(title) => ("autoTitleUpdated", serde_json::to_value(title)?),
        DriverEvent::AvailableCommands(commands) => {
            ("availableCommands", serde_json::to_value(commands)?)
        }
        DriverEvent::TurnStarted => ("turnStarted", Value::Null),
        DriverEvent::TextDelta(text) => ("textDelta", Value::String(text)),
        DriverEvent::ReasoningDelta(text) => ("reasoningDelta", Value::String(text)),
        DriverEvent::Activity {
            id,
            kind,
            title,
            detail,
            complete,
        } => (
            "activity",
            json!({
                "id": id,
                "kind": kind,
                "title": title,
                "detail": detail,
                "complete": complete,
            }),
        ),
        DriverEvent::RichActivity(activity) => ("richActivity", serde_json::to_value(activity)?),
        DriverEvent::BackgroundWork(work) => ("backgroundWork", serde_json::to_value(work)?),
        DriverEvent::Permission {
            request_id,
            title,
            detail,
            options,
        } => (
            "permission",
            json!({
                "requestId": request_id,
                "title": title,
                "detail": detail,
                "options": options,
            }),
        ),
        DriverEvent::UserInputRequested {
            request_id,
            questions,
        } => (
            "userInputRequested",
            json!({
                "requestId": request_id,
                "questions": questions,
            }),
        ),
        DriverEvent::ComputerUseUpdated(state) => (
            "computerUseUpdated",
            serde_json::to_value(ComputerUseWire {
                target: state.target,
                phase: state.phase,
                visible: state.visible,
                image_url: state.image_url,
            })?,
        ),
        DriverEvent::SteerAccepted { message } => ("steerAccepted", json!({ "message": message })),
        DriverEvent::SteerRejected { message, reason } => (
            "steerRejected",
            json!({ "message": message, "reason": reason }),
        ),
        DriverEvent::UsageUpdated {
            context_tokens,
            context_window,
        } => (
            "usageUpdated",
            json!({
                "contextTokens": context_tokens,
                "contextWindow": context_window,
            }),
        ),
        DriverEvent::PlanUsageUpdated(usage) => ("planUsageUpdated", serde_json::to_value(usage)?),
        DriverEvent::TurnFinished { success, summary } => (
            "turnFinished",
            json!({ "success": success, "summary": summary }),
        ),
        DriverEvent::Error(error) => ("error", Value::String(error)),
        DriverEvent::ProcessExited => ("processExited", Value::Null),
    };
    Ok(WireDriverEvent::new(kind, payload))
}

pub fn event_from_wire(event: WireDriverEvent) -> anyhow::Result<DriverEvent> {
    let payload = event.payload;
    Ok(match event.kind.as_str() {
        "connected" => DriverEvent::Connected {
            provider_cursor: serde_json::from_value(payload)?,
        },
        "agentPresetSelected" => DriverEvent::AgentPresetSelected(serde_json::from_value(payload)?),
        "autoTitleUpdated" => DriverEvent::AutoTitleUpdated(serde_json::from_value(payload)?),
        "availableCommands" => DriverEvent::AvailableCommands(serde_json::from_value(payload)?),
        "turnStarted" => DriverEvent::TurnStarted,
        "textDelta" => DriverEvent::TextDelta(serde_json::from_value(payload)?),
        "reasoningDelta" => DriverEvent::ReasoningDelta(serde_json::from_value(payload)?),
        "activity" => {
            let activity: ActivityWire = serde_json::from_value(payload)?;
            DriverEvent::Activity {
                id: activity.id,
                kind: activity.kind,
                title: activity.title,
                detail: activity.detail,
                complete: activity.complete,
            }
        }
        "richActivity" => DriverEvent::RichActivity(serde_json::from_value(payload)?),
        "backgroundWork" => DriverEvent::BackgroundWork(serde_json::from_value(payload)?),
        "permission" => {
            let permission: PermissionWire = serde_json::from_value(payload)?;
            DriverEvent::Permission {
                request_id: permission.request_id,
                title: permission.title,
                detail: permission.detail,
                options: permission.options,
            }
        }
        "userInputRequested" => {
            let request: UserInputWire = serde_json::from_value(payload)?;
            DriverEvent::UserInputRequested {
                request_id: request.request_id,
                questions: request.questions,
            }
        }
        "computerUseUpdated" => {
            let state: ComputerUseWire = serde_json::from_value(payload)?;
            DriverEvent::ComputerUseUpdated(ComputerUseState {
                target: state.target,
                phase: state.phase,
                visible: state.visible,
                image_url: state.image_url,
            })
        }
        "steerAccepted" => {
            let steer: AcceptedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerAccepted {
                message: steer.message,
            }
        }
        "steerRejected" => {
            let steer: RejectedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerRejected {
                message: steer.message,
                reason: steer.reason,
            }
        }
        "usageUpdated" => {
            let usage: UsageWire = serde_json::from_value(payload)?;
            DriverEvent::UsageUpdated {
                context_tokens: usage.context_tokens,
                context_window: usage.context_window,
            }
        }
        "planUsageUpdated" => DriverEvent::PlanUsageUpdated(serde_json::from_value(payload)?),
        "turnFinished" => {
            let finished: TurnFinishedWire = serde_json::from_value(payload)?;
            DriverEvent::TurnFinished {
                success: finished.success,
                summary: finished.summary,
            }
        }
        "error" => DriverEvent::Error(serde_json::from_value(payload)?),
        "processExited" => DriverEvent::ProcessExited,
        kind => bail!("daemon sent an unsupported driver event {kind:?}"),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityWire {
    id: Option<String>,
    kind: ActivityKind,
    title: String,
    detail: Option<String>,
    complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionWire {
    request_id: String,
    title: String,
    detail: String,
    options: Vec<PermissionOption>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputWire {
    request_id: String,
    questions: Vec<crate::model::UserInputQuestion>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComputerUseWire {
    target: Option<ComputerTarget>,
    phase: ComputerUsePhase,
    visible: bool,
    image_url: Option<String>,
}

#[derive(Deserialize)]
struct AcceptedSteerWire {
    message: String,
}

#[derive(Deserialize)]
struct RejectedSteerWire {
    message: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageWire {
    context_tokens: Option<u64>,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
struct TurnFinishedWire {
    success: bool,
    summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_runtime_projection_keeps_newer_transcript_cursor() {
        let runtime_id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let mut existing = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        existing.status = SessionStatus::Working;
        existing.runtime_event_cursor = Some(crate::model::RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 10,
        });
        existing.push_message(crate::model::MessageRole::Assistant, "complete so far");

        let mut stale = existing.clone();
        stale.title = "Renamed elsewhere".into();
        stale.messages.clear();
        stale.runtime_event_cursor = Some(crate::model::RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 7,
        });

        assert!(session_projection_precedes(
            &existing,
            &stale,
            Some(runtime_id)
        ));
        merge_stale_session_metadata(&mut existing, stale);
        assert_eq!(existing.title, "Renamed elsewhere");
        assert_eq!(existing.messages.len(), 1);
        assert_eq!(existing.runtime_event_cursor.unwrap().sequence, 10);
    }

    #[test]
    fn client_projection_cannot_replace_a_daemon_checkpoint() {
        let mut existing = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        existing.begin_turn("change it");
        existing.finish_active_turn(crate::model::TurnStatus::Completed);
        let checkpoint = Checkpoint {
            turn_count: 1,
            git_ref: "refs/waku/canonical".into(),
            status: CheckpointStatus::Ready,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            created_at: 1,
        };
        existing.turns[0].checkpoint = Some(checkpoint.clone());

        let mut incoming = existing.clone();
        incoming.turns[0].checkpoint = Some(Checkpoint {
            git_ref: "refs/waku/stale-client".into(),
            ..checkpoint.clone()
        });
        preserve_daemon_checkpoints(&existing, &mut incoming);

        assert_eq!(incoming.turns[0].checkpoint.as_ref(), Some(&checkpoint));
    }

    #[test]
    fn response_fork_titles_follow_one_numbered_sequence() {
        assert_eq!(
            next_response_fork_title("Fix the bug", ["Fix the bug"]),
            "Fix the bug (2)"
        );
        assert_eq!(
            next_response_fork_title(
                "Fix the bug (2)",
                ["Fix the bug", "Fix the bug (2)", "Fix the bug (4)"]
            ),
            "Fix the bug (5)"
        );
        assert_eq!(
            next_response_fork_title("Plan (2026)", ["Plan (2026)"]),
            "Plan (2026) (2)"
        );
    }

    #[test]
    fn message_rewind_requires_a_settled_user_turn_and_provider_cursor() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.begin_turn("change it");
        session.mark_active_turn_provider_started();
        session.provider_cursor = Some(ProviderResumeCursor::Codex {
            thread_id: "thread".into(),
        });
        session.finish_active_turn(crate::model::TurnStatus::Completed);

        assert!(validate_message_rewind(&session, 1).is_ok());

        let mut busy = session.clone();
        busy.status = SessionStatus::Working;
        assert!(validate_message_rewind(&busy, 1).is_err());

        let mut missing_cursor = session.clone();
        missing_cursor.provider_cursor = None;
        assert!(validate_message_rewind(&missing_cursor, 1).is_err());

        let mut missing_message = session;
        missing_message.messages.clear();
        assert!(validate_message_rewind(&missing_message, 1).is_err());
    }

    #[test]
    fn wire_event_round_trip_preserves_ordered_delta_payload() {
        let wire = event_to_wire(DriverEvent::TextDelta("hello".into())).unwrap();
        assert_eq!(wire.kind, "textDelta");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::TextDelta(text) if text == "hello"
        ));
    }

    #[test]
    fn removing_an_automation_session_settles_its_run() {
        let mut state = PersistedState::empty();
        let mut automation = crate::automation::Automation::new("Nightly", ProviderKind::Codex, 1);
        let session_id = Uuid::new_v4();
        automation.record_run(crate::automation::AutomationRun::spawned(
            session_id, 2, false,
        ));
        state.automations.push(automation);
        let automation_id = state.automations[0].id;

        assert!(cancel_automation_run_for_removed_session(
            &mut state,
            session_id,
            Some(automation_id),
        ));
        assert_eq!(
            state.automations[0].history[0].outcome,
            RunOutcome::Cancelled
        );
        assert!(!cancel_automation_run_for_removed_session(
            &mut state, session_id, None,
        ));
    }

    #[test]
    fn daemon_restart_resolves_durable_running_automation_markers() {
        let mut state = PersistedState::empty();
        let mut automation = crate::automation::Automation::new("Nightly", ProviderKind::Codex, 10);
        automation.record_run(crate::automation::AutomationRun::spawned(
            Uuid::new_v4(),
            20,
            false,
        ));
        state.automations.push(automation);

        assert!(recover_interrupted_automation_runs(&mut state));
        assert_eq!(
            state.automations[0].history[0].outcome,
            RunOutcome::Cancelled
        );
        assert!(!recover_interrupted_automation_runs(&mut state));
    }

    #[test]
    fn stale_automation_upsert_preserves_daemon_run_history() {
        let mut existing = crate::automation::Automation::new("Nightly", ProviderKind::Codex, 10);
        let session_id = Uuid::new_v4();
        existing.record_run(crate::automation::AutomationRun::spawned(
            session_id, 20, false,
        ));
        existing.last_run_at = Some(20);
        let run_id = existing.history[0].id;

        let mut stale = existing.clone();
        stale.name = "Renamed elsewhere".into();
        stale.history.clear();
        stale.last_run_at = None;
        stale.created_at = 1;
        stale.updated_at = 5;
        merge_automation_update(&mut existing, stale);

        assert_eq!(existing.name, "Renamed elsewhere");
        assert_eq!(existing.created_at, 10);
        assert_eq!(existing.updated_at, 10);
        assert_eq!(existing.last_run_at, Some(20));
        assert_eq!(existing.history[0].id, run_id);
        assert_eq!(existing.history[0].session_id, Some(session_id));
    }

    #[test]
    fn concurrent_deltas_for_different_automations_both_survive() {
        let root = std::env::temp_dir().join(format!("waku-automation-deltas-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = StateStore::daemon(root.join("app.db"));
        let mut state = PersistedState::empty();
        let first = crate::automation::Automation::new("First", ProviderKind::Codex, 1);
        let second = crate::automation::Automation::new("Second", ProviderKind::Codex, 1);
        let first_id = first.id;
        let second_id = second.id;
        state.automations.extend([first.clone(), second.clone()]);
        store.save(&mut state).unwrap();
        drop(store);

        let backend = Arc::new(
            WakuBackend::new(
                DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
                StateStore::daemon(root.join("app.db")),
            )
            .unwrap(),
        );
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles = [(first, "First renamed"), (second, "Second renamed")]
            .into_iter()
            .map(|(mut automation, name)| {
                automation.name = name.to_owned();
                let backend = backend.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    backend
                        .apply_automation_changes(vec![
                            waku_protocol::automation::AutomationChange::Upsert { automation },
                        ])
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        let state = backend.task_state.lock();
        assert_eq!(state.automations.len(), 2);
        assert_eq!(
            state
                .automations
                .iter()
                .find(|automation| automation.id == first_id)
                .unwrap()
                .name,
            "First renamed"
        );
        assert_eq!(
            state
                .automations
                .iter()
                .find(|automation| automation.id == second_id)
                .unwrap()
                .name,
            "Second renamed"
        );
        drop(state);
        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_delta_waits_for_daemon_history_and_preserves_it() {
        let root = std::env::temp_dir().join(format!("waku-automation-history-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = StateStore::daemon(root.join("app.db"));
        let mut state = PersistedState::empty();
        let automation = crate::automation::Automation::new("Nightly", ProviderKind::Codex, 1);
        let automation_id = automation.id;
        let mut stale = automation.clone();
        stale.name = "Renamed from stale client".to_owned();
        state.automations.push(automation);
        store.save(&mut state).unwrap();
        drop(store);

        let backend = Arc::new(
            WakuBackend::new(
                DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
                StateStore::daemon(root.join("app.db")),
            )
            .unwrap(),
        );
        let lock = backend.automation_lock(automation_id);
        let guard = lock.lock();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let worker = {
            let backend = backend.clone();
            std::thread::spawn(move || {
                let result = backend.apply_automation_changes(vec![
                    waku_protocol::automation::AutomationChange::Upsert { automation: stale },
                ]);
                finished_tx.send(result).unwrap();
            })
        };
        assert!(matches!(
            finished_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        let session_id = Uuid::new_v4();
        let run_id = {
            let mut state = backend.task_state.lock();
            let automation = state
                .automations
                .iter_mut()
                .find(|automation| automation.id == automation_id)
                .unwrap();
            automation.record_run(AutomationRun::spawned(session_id, 2, false));
            let run_id = automation.history[0].id;
            backend.task_store.save(&mut state).unwrap();
            run_id
        };
        drop(guard);
        finished_rx.recv().unwrap().unwrap();
        worker.join().unwrap();

        let state = backend.task_state.lock();
        let automation = state
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .unwrap();
        assert_eq!(automation.name, "Renamed from stale client");
        assert_eq!(automation.history[0].id, run_id);
        assert_eq!(automation.history[0].session_id, Some(session_id));
        drop(state);
        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_deleted_automations_lock_is_released_only_when_unheld() {
        let root = std::env::temp_dir().join(format!("waku-automation-lock-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let backend = WakuBackend::new(
            DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
            StateStore::daemon(root.join("app.db")),
        )
        .unwrap();

        let automation_id = Uuid::new_v4();
        let lock = backend.automation_lock(automation_id);
        // A second caller is mid-flight holding the same Arc, so dropping the
        // map entry now would hand the next caller a lock guarding nothing.
        let waiter = lock.clone();
        backend.release_automation_lock(automation_id, &lock);
        assert!(backend.automation_locks.lock().contains_key(&automation_id));

        drop(waiter);
        backend.release_automation_lock(automation_id, &lock);
        assert!(!backend.automation_locks.lock().contains_key(&automation_id));

        // A stale handle must never evict the entry a later caller installed.
        let replacement = backend.automation_lock(automation_id);
        backend.release_automation_lock(automation_id, &lock);
        assert!(backend.automation_locks.lock().contains_key(&automation_id));
        drop(replacement);

        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automation_removal_applies_its_cascade_choice() {
        let root = std::env::temp_dir().join(format!("waku-automation-remove-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = StateStore::daemon(root.join("app.db"));
        let mut state = PersistedState::empty();
        let project = Project::from_path(root.join("project"));
        let automation = crate::automation::Automation::new("Nightly", ProviderKind::Codex, 1);
        let automation_id = automation.id;
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.originating_automation = Some(automation_id);
        let session_id = session.id;
        state.projects.push(project);
        state.automations.push(automation);
        state.push_session(session);
        store.save(&mut state).unwrap();
        drop(store);

        let backend = WakuBackend::new(
            DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
            StateStore::daemon(root.join("app.db")),
        )
        .unwrap();
        backend
            .apply_automation_changes(vec![waku_protocol::automation::AutomationChange::Remove {
                automation_id,
                cascade_sessions: true,
            }])
            .unwrap();
        // The cascade sweeps blobs off-thread; that sweep walks this directory,
        // so it has to finish before the test tears the directory down.
        backend.join_automation_workers();
        let state = backend.task_state.lock();
        assert!(
            state
                .automations
                .iter()
                .all(|automation| automation.id != automation_id)
        );
        assert!(
            state
                .sessions
                .iter()
                .all(|session| session.id != session_id)
        );
        drop(state);
        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execution_path_enforces_skip_and_queue_for_active_runs() {
        let root = std::env::temp_dir().join(format!("waku-automation-overlap-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = StateStore::daemon(root.join("app.db"));
        let mut state = PersistedState::empty();
        let mut skip = crate::automation::Automation::new("Skip", ProviderKind::Codex, 10);
        skip.prompt = "run".into();
        skip.overlap = crate::automation::OverlapPolicy::Skip;
        let skip_id = skip.id;
        let mut queue = crate::automation::Automation::new("Queue", ProviderKind::Codex, 10);
        queue.prompt = "run".into();
        queue.overlap = crate::automation::OverlapPolicy::Queue;
        let queue_id = queue.id;
        state.automations.extend([skip, queue]);
        store.save(&mut state).unwrap();
        drop(store);

        let backend = WakuBackend::new(
            DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
            StateStore::daemon(root.join("app.db")),
        )
        .unwrap();
        {
            let mut state = backend.task_state.lock();
            state
                .automations
                .iter_mut()
                .find(|item| item.id == skip_id)
                .unwrap()
                .record_run(AutomationRun::spawned(Uuid::new_v4(), 20, false));
            state
                .automations
                .iter_mut()
                .find(|item| item.id == queue_id)
                .unwrap()
                .record_run(AutomationRun::spawned(Uuid::new_v4(), 20, false));
            backend.task_store.save(&mut state).unwrap();
        }
        assert!(matches!(
            backend
                .start_automation_run(skip_id, false, EventSink::for_test())
                .unwrap(),
            AutomationStartResult::Skipped
        ));
        assert!(matches!(
            backend
                .start_automation_run(queue_id, false, EventSink::for_test())
                .unwrap(),
            AutomationStartResult::Deferred
        ));

        let state = backend.task_state.lock();
        let skip = state
            .automations
            .iter()
            .find(|item| item.id == skip_id)
            .unwrap();
        let queue = state
            .automations
            .iter()
            .find(|item| item.id == queue_id)
            .unwrap();
        assert_eq!(skip.history.len(), 2);
        assert!(
            skip.history
                .iter()
                .any(|run| run.outcome == RunOutcome::Skipped)
        );
        assert_eq!(queue.history.len(), 1);
        drop(state);
        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_tick_coalesces_catch_up_and_persists_without_a_client() {
        use chrono::Timelike as _;

        let root = std::env::temp_dir().join(format!("waku-automation-tick-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let settings = DaemonSettingsStore::open(root.join("settings.json")).unwrap();
        let mut daemon_settings = crate::settings::DaemonSettings::default();
        daemon_settings.provider_binary_overrides.insert(
            ProviderKind::DeepSeek,
            root.join("missing-provider").display().to_string(),
        );
        settings.replace(daemon_settings).unwrap();

        let store = StateStore::daemon(root.join("app.db"));
        let mut state = PersistedState::empty();
        let project = Project::from_path(root.join("repo"));
        std::fs::create_dir_all(&project.path).unwrap();
        let scheduled = Local::now() - chrono::Duration::hours(1);
        let mut automation = crate::automation::Automation::new(
            "Catch up once",
            ProviderKind::DeepSeek,
            crate::model::unix_time().saturating_sub(3 * 24 * 60 * 60),
        );
        automation.prompt = "run once".into();
        automation.enabled = true;
        automation.project_id = Some(project.id);
        automation.schedule = crate::automation::Schedule::Daily {
            time: crate::automation::TimeOfDay::new(
                scheduled.hour() as u8,
                scheduled.minute() as u8,
            ),
        };
        automation.last_run_at = Some(crate::model::unix_time().saturating_sub(3 * 24 * 60 * 60));
        let automation_id = automation.id;
        state.projects.push(project);
        state.automations.push(automation);
        store.save(&mut state).unwrap();
        drop(store);

        let backend =
            Arc::new(WakuBackend::new(settings, StateStore::daemon(root.join("app.db"))).unwrap());
        // Starts run off the scheduler thread, so each tick is drained before
        // the next one plans against the state it produced.
        backend.tick_automations(EventSink::for_test());
        backend.join_automation_workers();
        backend.tick_automations(EventSink::for_test());
        backend.join_automation_workers();
        drop(backend);

        let reopened = StateStore::daemon(root.join("app.db"));
        let reloaded = reopened.load().unwrap();
        let automation = reloaded
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .unwrap();
        assert_eq!(automation.history.len(), 1);
        assert!(automation.history[0].catch_up);
        assert_eq!(automation.history[0].outcome, RunOutcome::Failed);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn process_exit_after_success_does_not_overwrite_automation_outcome() {
        let root = std::env::temp_dir().join(format!("waku-automation-event-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(crate::persistence::StateStore::daemon(root.join("app.db")));
        let state = Arc::new(Mutex::new(crate::persistence::PersistedState::empty()));
        let mut automation = crate::automation::Automation::new("Nightly", ProviderKind::Codex, 10);
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.begin_turn("run");
        session.status = SessionStatus::Working;
        let session_id = session.id;
        automation.record_run(crate::automation::AutomationRun::spawned(
            session_id, 20, false,
        ));
        state.lock().automations.push(automation);
        state.lock().push_session(session);
        store.save(&mut state.lock()).unwrap();

        let automation_lock = Arc::new(Mutex::new(()));
        let finished = DriverEvent::TurnFinished {
            success: true,
            summary: Some("done".into()),
        };
        let automation_id = state.lock().automations[0].id;
        apply_automation_driver_event(
            &state,
            &store,
            &automation_lock,
            session_id,
            automation_id,
            &finished,
        );
        apply_automation_driver_event(
            &state,
            &store,
            &automation_lock,
            session_id,
            automation_id,
            &DriverEvent::ProcessExited,
        );

        let state_guard = state.lock();
        assert_eq!(state_guard.sessions[0].status, SessionStatus::Idle);
        assert_eq!(
            state_guard.automations[0].history[0].outcome,
            RunOutcome::Succeeded
        );
        drop(state_guard);
        drop(state);
        drop(store);

        let reopened = crate::persistence::StateStore::daemon(root.join("app.db"));
        let reloaded = reopened.load().unwrap();
        assert_eq!(reloaded.sessions[0].status, SessionStatus::Idle);
        assert_eq!(
            reloaded.automations[0].history[0].outcome,
            RunOutcome::Succeeded
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}

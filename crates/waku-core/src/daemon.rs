//! Provider backend and driver-event wire translation for `waku-daemon`.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::{Backend, Command, EventSink, Request, ResponsePayload, WireDriverEvent};
use anyhow::{Context as _, anyhow, bail};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::attachments::AttachmentStore;
use crate::computer_use::{ComputerTarget, ComputerUsePhase, ComputerUseState};
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::model::{ActivityKind, DriverEvent, PermissionOption};
use crate::persistence::{ComposerDraftStore, PersistedState, StateStore};
use crate::settings::DaemonSettingsStore;
use waku_protocol::provider_session::{ProviderSessionFork, ProviderSessionForkRequest};

pub struct WakuBackend {
    sessions: Mutex<HashMap<Uuid, (Uuid, DriverHandle)>>,
    settings: DaemonSettingsStore,
    task_store: StateStore,
    task_state: Mutex<PersistedState>,
    composer_drafts: ComposerDraftStore,
    attachments: AttachmentStore,
    usage_scan_cache: Mutex<crate::usage_history::ScanCache>,
    usage_rates_dir: std::path::PathBuf,
    default_cwd: std::path::PathBuf,
}

impl WakuBackend {
    pub fn new(settings: DaemonSettingsStore, task_store: StateStore) -> anyhow::Result<Self> {
        let mut task_state = task_store
            .load()
            .context("could not load Waku task database")?;
        migrate_projectless_state(&task_store, &mut task_state)?;
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
            sessions: Mutex::new(HashMap::new()),
            settings,
            task_store,
            task_state: Mutex::new(task_state),
            composer_drafts,
            attachments,
            usage_scan_cache: Mutex::new(HashMap::new()),
            usage_rates_dir,
            default_cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        })
    }
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
                    override_value => crate::model::provider_probe(provider, override_value),
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
                    sessions: state.sessions.clone(),
                    default_cwd: self.default_cwd.clone(),
                    projectless_root: crate::projectless::workspace_root(),
                })
            }
            Command::SaveTaskState {
                projects,
                live_session_ids,
                sessions,
            } => {
                let mut state = self.task_state.lock();
                state.projects = projects;
                state
                    .sessions
                    .retain(|session| live_session_ids.contains(&session.id));
                let saved_ids = sessions
                    .iter()
                    .map(|session| session.id)
                    .collect::<Vec<_>>();
                for session in sessions {
                    if let Some(existing) = state
                        .sessions
                        .iter_mut()
                        .find(|existing| existing.id == session.id)
                    {
                        *existing = session;
                    } else {
                        state.sessions.push(session);
                    }
                }
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
            Command::ForkProviderSession { request } => {
                Ok(ResponsePayload::ProviderSessionForked {
                    result: fork_provider_session(request)?,
                })
            }
            Command::Workspace { operation } => Ok(ResponsePayload::Workspace {
                result: crate::workspace::execute(operation)?,
            }),
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

    fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock());
        drop(sessions);
    }
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
        Command::Start { .. }
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
        | Command::SaveTaskState { .. }
        | Command::HydrateSession { .. }
        | Command::SearchSessionMessages { .. }
        | Command::LoadComposerDrafts
        | Command::SaveComposerDrafts { .. }
        | Command::StoreBlob { .. }
        | Command::ImportAttachment { .. }
        | Command::ReadBlob { .. }
        | Command::ReadAttachment { .. }
        | Command::SweepBlobs
        | Command::ForkProviderSession { .. }
        | Command::Workspace { .. }
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
    fn wire_event_round_trip_preserves_ordered_delta_payload() {
        let wire = event_to_wire(DriverEvent::TextDelta("hello".into())).unwrap();
        assert_eq!(wire.kind, "textDelta");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::TextDelta(text) if text == "hello"
        ));
    }
}

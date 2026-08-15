//! Agent Client Protocol transport backed by the official Rust SDK.
//!
//! The SDK owns JSON-RPC framing, request IDs, response routing, cancellation,
//! unknown-method errors, stdio lifetime, and protocol type validation. Waku
//! only adapts typed ACP messages to its provider-neutral [`DriverEvent`]s.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, Implementation, InitializeRequest,
    InitializeResponse, LoadSessionRequest, NewSessionRequest, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestId, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelect,
    SessionConfigSelectOptions, SessionId, SessionModeId, SessionModeState, SessionNotification,
    SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest, StopReason, TextContent,
};
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo, LineDirection, Responder, UntypedMessage,
};
use anyhow::{Context as _, anyhow};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, PermissionOption, ProviderKind, ProviderModel,
    ProviderResumeCursor, RuntimeMode,
};

enum CommandMessage {
    Prompt(String),
    Steer(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    Options(SessionOptions),
    Shutdown,
}

pub struct AcpDriver {
    commands: smol::channel::Sender<CommandMessage>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    computer_use: Option<super::support::HeadlessComputerUseRuntime>,
}

/// Per-provider launch details. Everything after process launch is ACP.
struct AcpLaunch {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

fn launch_for(provider: ProviderKind) -> anyhow::Result<AcpLaunch> {
    match provider {
        ProviderKind::Cursor => Ok(AcpLaunch {
            args: vec!["acp".into()],
            env: Vec::new(),
        }),
        ProviderKind::Grok => Ok(AcpLaunch {
            args: vec!["agent".into(), "stdio".into()],
            env: vec![("GROK_OAUTH2_REFERRER".into(), "waku".into())],
        }),
        ProviderKind::OpenCode => Ok(AcpLaunch {
            args: vec!["acp".into()],
            env: Vec::new(),
        }),
        ProviderKind::DeerFlow => Ok(AcpLaunch {
            args: Vec::new(),
            env: Vec::new(),
        }),
        _ => Err(anyhow!(
            "{} does not speak the Agent Client Protocol",
            provider.display_name()
        )),
    }
}

impl AcpDriver {
    pub fn start(
        provider: ProviderKind,
        options: DriverStartOptions,
        events: DriverEventSender,
    ) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier: _,
            agent_preset: _,
            computer_use_enabled,
            provider_cursor,
        } = options;
        let fork_context = match &provider_cursor {
            Some(ProviderResumeCursor::Cursor { fork_context, .. }) => fork_context.clone(),
            _ => None,
        };
        let resume_session_id = match provider_cursor {
            Some(cursor) if cursor.provider() == provider => {
                let id = cursor.native_id();
                (!id.is_empty()).then(|| id.to_owned())
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume {} from a {} cursor",
                    provider.display_name(),
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };

        let launch = launch_for(provider)?;
        let computer_use = (provider == ProviderKind::Grok && computer_use_enabled)
            .then(|| super::support::HeadlessComputerUseRuntime::start(provider, events.clone()))
            .transpose()?;
        let grok_title_home = computer_use
            .as_ref()
            .and_then(super::support::HeadlessComputerUseRuntime::grok_home)
            .map(ToOwned::to_owned);
        let stderr_lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let agent = sdk_agent(
            &binary,
            &cwd,
            launch,
            computer_use.as_ref().map(|runtime| &runtime.config),
            stderr_lines.clone(),
        )?;
        let (commands, command_rx) = smol::channel::unbounded();
        let provider_name = provider.display_name();
        let thread_events = events.clone();

        thread::Builder::new()
            .name(format!("waku-{}-acp", provider.id()))
            .spawn(move || {
                if let Err(error) = crate::command_env::unblock_sigchld_for_current_thread() {
                    let _ = thread_events.send(DriverEvent::Error(format!(
                        "{provider_name}: failed to normalize the provider signal mask: {error}"
                    )));
                    let _ = thread_events.send(DriverEvent::ProcessExited);
                    return;
                }
                let result = smol::block_on(run_sdk_connection(
                    agent,
                    provider,
                    cwd,
                    mode,
                    interaction_mode,
                    model,
                    reasoning_effort,
                    resume_session_id,
                    fork_context,
                    grok_title_home,
                    command_rx,
                    thread_events.clone(),
                ));
                if let Err(error) = result {
                    let stderr = super::support::provider_stderr_error(stderr_lines.lock().clone());
                    let detail = stderr.unwrap_or_else(|| error.to_string());
                    let _ = thread_events
                        .send(DriverEvent::Error(format!("{provider_name}: {detail}")));
                }
                let _ = thread_events.send(DriverEvent::ProcessExited);
            })
            .with_context(|| format!("failed to start {provider_name} ACP runtime"))?;

        Ok(Self {
            commands,
            mode,
            interaction_mode,
            computer_use,
        })
    }
}

fn sdk_agent(
    binary: &Path,
    cwd: &Path,
    mut launch: AcpLaunch,
    computer_use: Option<&super::support::HeadlessComputerUseConfig>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<AcpAgent> {
    let binary = binary
        .to_str()
        .ok_or_else(|| anyhow!("the ACP executable path is not valid UTF-8"))?;
    let cwd = cwd
        .to_str()
        .ok_or_else(|| anyhow!("the ACP working directory is not valid UTF-8"))?;
    let (computer_args, computer_env) =
        super::support::grok_computer_use_launch_configuration(computer_use);
    launch.args.extend(computer_args);
    let mut environment = crate::command_env::shell_environment()
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<Vec<_>>();
    environment.append(&mut launch.env);
    environment.extend(computer_env);

    // `AcpAgentConfig` deliberately contains only argv and environment. Unix
    // `env -C` supplies the session cwd without a shell, preserving exact
    // argument boundaries and the SDK's process-group lifecycle management.
    #[cfg(not(windows))]
    let config = {
        let mut args = vec!["-C".to_owned(), cwd.to_owned(), binary.to_owned()];
        args.extend(launch.args);
        AcpAgentConfig::new("/usr/bin/env")
            .args(args)
            .envs(environment)
    };
    // Windows has no `/usr/bin/env`, and ACP agents are commonly installed as
    // `.cmd` or PowerShell wrappers. The protocol still receives the requested
    // session cwd; this layer only preserves executable dispatch and argument
    // boundaries. The SDK itself applies CREATE_NO_WINDOW to the child.
    #[cfg(windows)]
    let config = {
        let extension = Path::new(binary)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        let _ = cwd;
        match extension.as_deref() {
            Some("cmd" | "bat") => {
                let mut args = vec![
                    "/d".to_owned(),
                    "/s".to_owned(),
                    "/c".to_owned(),
                    "call".to_owned(),
                    binary.to_owned(),
                ];
                args.extend(launch.args);
                AcpAgentConfig::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()))
                    .args(args)
                    .envs(environment)
            }
            Some("ps1") => {
                let mut args = vec![
                    "-NoLogo".to_owned(),
                    "-NoProfile".to_owned(),
                    "-ExecutionPolicy".to_owned(),
                    "Bypass".to_owned(),
                    "-File".to_owned(),
                    binary.to_owned(),
                ];
                args.extend(launch.args);
                AcpAgentConfig::new("powershell.exe")
                    .args(args)
                    .envs(environment)
            }
            _ => AcpAgentConfig::new(binary)
                .args(launch.args)
                .envs(environment),
        }
    };
    Ok(AcpAgent::new(config).with_debug(move |line, direction| {
        if direction != LineDirection::Stderr || line.trim().is_empty() {
            return;
        }
        let mut lines = stderr_lines.lock();
        if lines.len() == 128 {
            lines.remove(0);
        }
        lines.push(line.to_owned());
    }))
}

type PermissionResponder = Responder<RequestPermissionResponse>;
type PendingPermissions = Arc<Mutex<HashMap<String, PermissionResponder>>>;

#[derive(Default)]
struct PendingPrompts(Vec<PendingPrompt>);

struct PendingPrompt {
    request_id: RequestId,
    extension_id: Option<String>,
    session_id: String,
}

impl PendingPrompts {
    fn insert(&mut self, request_id: RequestId, extension_id: Option<String>, session_id: String) {
        self.0.push(PendingPrompt {
            request_id,
            extension_id,
            session_id,
        });
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn settle_request(&mut self, request_id: &RequestId) -> bool {
        let Some(index) = self
            .0
            .iter()
            .position(|prompt| &prompt.request_id == request_id)
        else {
            return false;
        };
        self.0.remove(index);
        self.0.is_empty()
    }

    fn settle_extension(&mut self, session_id: &str, extension_id: Option<&str>) -> bool {
        let Some(index) = self.0.iter().position(|prompt| {
            prompt.session_id == session_id
                && extension_id
                    .is_none_or(|extension_id| prompt.extension_id.as_deref() == Some(extension_id))
        }) else {
            return false;
        };
        self.0.remove(index);
        self.0.is_empty()
    }
}

type PendingPromptRequests = Arc<Mutex<PendingPrompts>>;

#[allow(clippy::too_many_arguments)]
async fn run_sdk_connection(
    agent: AcpAgent,
    provider: ProviderKind,
    cwd: std::path::PathBuf,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: Option<String>,
    reasoning_effort: Option<String>,
    resume_session_id: Option<String>,
    fork_context: Option<String>,
    grok_title_home: Option<std::path::PathBuf>,
    commands: smol::channel::Receiver<CommandMessage>,
    events: DriverEventSender,
) -> agent_client_protocol::Result<()> {
    let suppress_session_updates = Arc::new(AtomicBool::new(false));
    let stream_state = Arc::new(Mutex::new(AcpStreamState::default()));
    let config_state = Arc::new(Mutex::new(AcpConfigState::default()));
    let pending_permissions: PendingPermissions = Arc::new(Mutex::new(HashMap::new()));
    let prompt_requests = Arc::new(Mutex::new(PendingPrompts::default()));
    let title_refresh = super::title_refresh::NativeTitleRefresh::default();
    let auto_approve = mode != RuntimeMode::Ask;

    Client
        .builder()
        .name("waku")
        .on_receive_notification(
            {
                let events = events.clone();
                let suppress_session_updates = suppress_session_updates.clone();
                let stream_state = stream_state.clone();
                let config_state = config_state.clone();
                async move |notification: SessionNotification, _connection| {
                    if !suppress_session_updates.load(Ordering::Acquire) {
                        handle_session_update(
                            notification,
                            &events,
                            &mut stream_state.lock(),
                            &config_state,
                        )?;
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            {
                let events = events.clone();
                let prompt_requests = prompt_requests.clone();
                let grok_title_home = grok_title_home.clone();
                let title_refresh = title_refresh.clone();
                async move |notification: UntypedMessage, _connection| {
                    if notification.method() == "_x.ai/session/prompt_complete" {
                        if let Some(session_id) = finish_xai_prompt_complete(
                            notification.params(),
                            &prompt_requests,
                            &events,
                        ) {
                            start_grok_title_refresh(
                                grok_title_home.as_deref(),
                                &session_id,
                                &title_refresh,
                                events.clone(),
                            );
                        }
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let events = events.clone();
                let pending_permissions = pending_permissions.clone();
                async move |request: RequestPermissionRequest, responder, _connection| {
                    handle_permission_request(
                        request,
                        responder,
                        auto_approve,
                        &pending_permissions,
                        &events,
                    )
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let initialize = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(ClientCapabilities::new().terminal(false))
                        .client_info(Implementation::new("waku", env!("CARGO_PKG_VERSION"))),
                )
                .block_task()
                .await?;
            let established = establish_session(
                &connection,
                &initialize,
                resume_session_id.as_deref(),
                &cwd,
                &suppress_session_updates,
            )
            .await?;
            let session_id = established.session_id;
            let modes = established.modes;
            {
                let mut state = config_state.lock();
                state.replace_initial(established.config_options);
                state.emit_models(&events);
            }

            if let Some(mode_id) = desired_mode(modes.as_ref(), mode, interaction_mode) {
                // Mode selection is opportunistic: an agent can advertise a
                // mode but reject a later transition without invalidating the
                // session itself.
                let _ = connection
                    .send_request(SetSessionModeRequest::new(session_id.clone(), mode_id))
                    .block_task()
                    .await;
            }
            let native_session_id = session_id.to_string();
            let _ = events.send(DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::from_session_id(
                    provider,
                    native_session_id.clone(),
                )),
            });

            apply_model(
                &connection,
                &session_id,
                provider,
                model.as_deref(),
                reasoning_effort.as_deref(),
                &config_state,
                &events,
            )
            .await;
            let mut fork_context = fork_context;

            while let Ok(command) = commands.recv().await {
                match command {
                    CommandMessage::Prompt(text) => {
                        let text = fork_context
                            .take()
                            .map(|context| {
                                crate::cursor_session::prompt_with_fork_context(&context, &text)
                            })
                            .unwrap_or(text);
                        let _ = events.send(DriverEvent::TurnStarted);
                        if let Err(error) = send_prompt(
                            &connection,
                            &session_id,
                            text,
                            &prompt_requests,
                            &events,
                            provider,
                            &native_session_id,
                            grok_title_home.clone(),
                            title_refresh.clone(),
                        ) {
                            let _ = events.send(DriverEvent::Error(error.to_string()));
                            let _ = events.send(DriverEvent::TurnFinished {
                                success: false,
                                summary: None,
                            });
                        }
                    }
                    CommandMessage::Steer(text) => {
                        if prompt_requests.lock().is_empty() {
                            let _ = events.send(DriverEvent::SteerRejected {
                                message: text,
                                reason: format!(
                                    "{} has no active turn to steer.",
                                    provider.display_name()
                                ),
                            });
                            continue;
                        }
                        match send_prompt(
                            &connection,
                            &session_id,
                            text.clone(),
                            &prompt_requests,
                            &events,
                            provider,
                            &native_session_id,
                            grok_title_home.clone(),
                            title_refresh.clone(),
                        ) {
                            Ok(()) => {
                                let _ = events.send(DriverEvent::SteerAccepted { message: text });
                            }
                            Err(error) => {
                                let _ = events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: error.to_string(),
                                });
                            }
                        }
                    }
                    CommandMessage::Cancel => {
                        let _ = connection
                            .send_notification(CancelNotification::new(session_id.clone()));
                        cancel_pending_permissions(&pending_permissions);
                    }
                    CommandMessage::Respond {
                        request_id,
                        option_id,
                    } => {
                        if let Some(responder) = pending_permissions.lock().remove(&request_id) {
                            let _ = responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                    option_id,
                                )),
                            ));
                        }
                    }
                    CommandMessage::Options(options) => {
                        // Compare against the latest full ACP config state in
                        // `apply_model`, not a remembered client request: the
                        // agent may update the current model independently.
                        apply_model(
                            &connection,
                            &session_id,
                            provider,
                            options.model.as_deref(),
                            options.reasoning_effort.as_deref(),
                            &config_state,
                            &events,
                        )
                        .await;
                    }
                    CommandMessage::Shutdown => break,
                }
            }
            cancel_pending_permissions(&pending_permissions);
            Ok(())
        })
        .await
}

struct EstablishedSession {
    session_id: SessionId,
    modes: Option<SessionModeState>,
    config_options: Vec<SessionConfigOption>,
}

async fn establish_session(
    connection: &ConnectionTo<Agent>,
    initialize: &InitializeResponse,
    resume_session_id: Option<&str>,
    cwd: &Path,
    suppress_session_updates: &AtomicBool,
) -> agent_client_protocol::Result<EstablishedSession> {
    if let Some(existing) = resume_session_id {
        if initialize
            .agent_capabilities
            .session_capabilities
            .resume
            .is_some()
            && let Ok(response) = connection
                .send_request(ResumeSessionRequest::new(existing.to_owned(), cwd))
                .block_task()
                .await
        {
            return Ok(EstablishedSession {
                session_id: SessionId::new(existing.to_owned()),
                modes: response.modes,
                config_options: response.config_options.unwrap_or_default(),
            });
        }

        if initialize.agent_capabilities.load_session {
            suppress_session_updates.store(true, Ordering::Release);
            let response = connection
                .send_request(LoadSessionRequest::new(existing.to_owned(), cwd))
                .block_task()
                .await;
            suppress_session_updates.store(false, Ordering::Release);
            if let Ok(response) = response {
                return Ok(EstablishedSession {
                    session_id: SessionId::new(existing.to_owned()),
                    modes: response.modes,
                    config_options: response.config_options.unwrap_or_default(),
                });
            }
        }
    }

    let response = connection
        .send_request(NewSessionRequest::new(cwd))
        .block_task()
        .await?;
    Ok(EstablishedSession {
        session_id: response.session_id,
        modes: response.modes,
        config_options: response.config_options.unwrap_or_default(),
    })
}

fn desired_mode(
    modes: Option<&SessionModeState>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) -> Option<SessionModeId> {
    if interaction_mode != InteractionMode::Plan && mode != RuntimeMode::Plan {
        return None;
    }
    let modes = modes?;
    let plan = modes
        .available_modes
        .iter()
        .find(|mode| mode.id.to_string().eq_ignore_ascii_case("plan"))?
        .id
        .clone();
    (modes.current_mode_id != plan).then_some(plan)
}

async fn apply_model(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    provider: ProviderKind,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    config_state: &Mutex<AcpConfigState>,
    events: &DriverEventSender,
) {
    if let Some(model) = model {
        let config_id = {
            let state = config_state.lock();
            state
                .model_config()
                .filter(|(_, select)| {
                    select.current_value.to_string() != model
                        && select_contains_value(select, model)
                })
                .map(|(config, _)| config.id.clone())
        };
        if let Some(config_id) = config_id {
            match connection
                .send_request(SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    config_id,
                    model,
                ))
                .block_task()
                .await
            {
                Ok(response) => {
                    let mut state = config_state.lock();
                    state.replace(response.config_options);
                    state.emit_models(events);
                }
                Err(error) => {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.select_model",
                        error = error
                    )));
                    return;
                }
            }
        } else if !config_state.lock().has_model_config()
            && matches!(provider, ProviderKind::Cursor | ProviderKind::Grok)
        {
            // Cursor and Grok shipped this extension before ACP standardized
            // model selection as a session config option. Keep the extension
            // only as their compatibility fallback.
            let request = match UntypedMessage::new(
                "session/set_model",
                json!({"sessionId": session_id, "modelId": model}),
            ) {
                Ok(request) => request,
                Err(error) => {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.select_model",
                        error = error
                    )));
                    return;
                }
            };
            if let Err(error) = connection.send_request(request).block_task().await {
                let _ = events.send(DriverEvent::Error(tr!(
                    "errors.select_model",
                    error = error
                )));
                return;
            }
        }
    }
    if let Some(effort) = reasoning_effort {
        let config_id = {
            let state = config_state.lock();
            state
                .thought_config()
                .filter(|(_, select)| {
                    select.current_value.to_string() != effort
                        && select_contains_value(select, effort)
                })
                .map(|(config, _)| config.id.clone())
        };
        if let Some(config_id) = config_id
            && let Ok(response) = connection
                .send_request(SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    config_id,
                    effort,
                ))
                .block_task()
                .await
        {
            // Setter responses are complete snapshots, even when the changed
            // option was not the model selector.
            let mut state = config_state.lock();
            state.replace(response.config_options);
            state.emit_models(events);
        }
    }
}

#[derive(Default)]
struct AcpConfigState {
    options: Vec<SessionConfigOption>,
    /// The first model reported by the agent is its session default. Later
    /// setter responses report the current model and must not redefine it.
    default_model: Option<String>,
}

impl AcpConfigState {
    fn replace_initial(&mut self, options: Vec<SessionConfigOption>) {
        self.options = options;
        self.default_model = self.current_model();
    }

    fn replace(&mut self, options: Vec<SessionConfigOption>) {
        self.options = options;
    }

    fn model_config(&self) -> Option<(&SessionConfigOption, &SessionConfigSelect)> {
        self.options
            .iter()
            .find_map(|config| match (&config.category, &config.kind) {
                (Some(SessionConfigOptionCategory::Model), SessionConfigKind::Select(select)) => {
                    Some((config, select))
                }
                _ => None,
            })
            .or_else(|| {
                self.options.iter().find_map(|config| match &config.kind {
                    SessionConfigKind::Select(select)
                        if config.id.to_string().eq_ignore_ascii_case("model") =>
                    {
                        Some((config, select))
                    }
                    _ => None,
                })
            })
    }

    fn has_model_config(&self) -> bool {
        self.model_config().is_some()
    }

    fn thought_config(&self) -> Option<(&SessionConfigOption, &SessionConfigSelect)> {
        self.options
            .iter()
            .find_map(|config| match (&config.category, &config.kind) {
                (
                    Some(SessionConfigOptionCategory::ThoughtLevel),
                    SessionConfigKind::Select(select),
                ) => Some((config, select)),
                _ => None,
            })
            .or_else(|| {
                self.options.iter().find_map(|config| match &config.kind {
                    SessionConfigKind::Select(select)
                        if config.id.to_string().eq_ignore_ascii_case("mode") =>
                    {
                        Some((config, select))
                    }
                    _ => None,
                })
            })
    }

    fn current_model(&self) -> Option<String> {
        self.model_config()
            .map(|(_, select)| select.current_value.to_string())
    }

    fn emit_models(&self, events: &impl DriverEventSink) {
        let Some((_, select)) = self.model_config() else {
            return;
        };
        let current_model = select.current_value.to_string();
        let models = models_from_select(select, self.default_model.as_deref());
        let _ = events.send(DriverEvent::ModelsUpdated {
            models,
            current_model: Some(current_model),
        });
    }
}

fn select_contains_value(select: &SessionConfigSelect, value: &str) -> bool {
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .any(|option| option.value.to_string() == value),
        SessionConfigSelectOptions::Grouped(groups) => groups.iter().any(|group| {
            group
                .options
                .iter()
                .any(|option| option.value.to_string() == value)
        }),
        _ => false,
    }
}

fn models_from_select(
    select: &SessionConfigSelect,
    default_model: Option<&str>,
) -> Vec<ProviderModel> {
    let build = |option: &agent_client_protocol::schema::v1::SessionConfigSelectOption,
                 group: Option<&str>| {
        let id = option.value.to_string();
        let mut model = ProviderModel::new(&id, &option.name);
        if let Some(group) = group {
            model = model.sub_provider(group);
        }
        if default_model == Some(id.as_str()) {
            model = model.default();
        }
        model
    };
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => {
            options.iter().map(|option| build(option, None)).collect()
        }
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| {
                group
                    .options
                    .iter()
                    .map(|option| build(option, Some(&group.name)))
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn send_prompt(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    text: String,
    prompt_requests: &PendingPromptRequests,
    events: &DriverEventSender,
    provider: ProviderKind,
    native_session_id: &str,
    grok_title_home: Option<std::path::PathBuf>,
    title_refresh: super::title_refresh::NativeTitleRefresh,
) -> agent_client_protocol::Result<()> {
    let extension_id =
        (provider == ProviderKind::Grok).then(|| format!("waku-{}", uuid::Uuid::new_v4()));
    let mut request = PromptRequest::new(
        session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(text))],
    );
    if let Some(extension_id) = extension_id.as_ref() {
        let mut meta = serde_json::Map::new();
        meta.insert("promptId".into(), Value::String(extension_id.clone()));
        meta.insert("requestId".into(), Value::String(extension_id.clone()));
        request = request.meta(meta);
    }
    let sent = connection.send_request(request);
    let request_id = sent.id().clone();
    prompt_requests.lock().insert(
        request_id.clone(),
        extension_id,
        native_session_id.to_owned(),
    );
    let callback_request_id = request_id.clone();
    let callback_requests = prompt_requests.clone();
    let callback_events = events.clone();
    let native_session_id = native_session_id.to_owned();
    let registered = sent.on_receiving_result(async move |result| {
        if settle_prompt_request(&callback_requests, &callback_request_id) {
            let success = finish_prompt(result, &callback_events);
            if provider == ProviderKind::Grok && success {
                start_grok_title_refresh(
                    grok_title_home.as_deref(),
                    &native_session_id,
                    &title_refresh,
                    callback_events,
                );
            }
        }
        Ok(())
    });
    if registered.is_err() {
        prompt_requests.lock().settle_request(&request_id);
    }
    registered
}

fn settle_prompt_request(prompt_requests: &Mutex<PendingPrompts>, request_id: &RequestId) -> bool {
    prompt_requests.lock().settle_request(request_id)
}

fn finish_xai_prompt_complete(
    params: &Value,
    prompt_requests: &Mutex<PendingPrompts>,
    events: &DriverEventSender,
) -> Option<String> {
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        return None;
    };
    let prompt_id = params.get("promptId").and_then(Value::as_str);
    if !prompt_requests
        .lock()
        .settle_extension(session_id, prompt_id)
    {
        return None;
    }

    let stop_reason = match params.get("stopReason").and_then(Value::as_str) {
        Some("cancelled") => StopReason::Cancelled,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("max_turn_requests") => StopReason::MaxTurnRequests,
        Some("refusal") => StopReason::Refusal,
        _ => StopReason::EndTurn,
    };
    finish_prompt(Ok(PromptResponse::new(stop_reason)), events).then(|| session_id.to_owned())
}

fn start_grok_title_refresh(
    grok_title_home: Option<&Path>,
    native_session_id: &str,
    title_refresh: &super::title_refresh::NativeTitleRefresh,
    events: DriverEventSender,
) {
    let grok_title_home = grok_title_home.map(ToOwned::to_owned);
    let native_session_id = native_session_id.to_owned();
    title_refresh.start(
        "waku-grok-title",
        vec![
            Duration::ZERO,
            Duration::from_millis(250),
            Duration::from_millis(750),
            Duration::from_millis(1_500),
            Duration::from_secs(3),
            Duration::from_secs(5),
            Duration::from_millis(7_500),
            Duration::from_secs(10),
        ],
        events,
        move || match grok_title_home.as_deref() {
            Some(home) => crate::grok_session::generated_title_in(home, &native_session_id),
            None => crate::grok_session::generated_title(&native_session_id),
        },
    );
}

fn finish_prompt(
    result: agent_client_protocol::Result<PromptResponse>,
    events: &impl DriverEventSink,
) -> bool {
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            let _ = events.send(DriverEvent::Error(error.to_string()));
            let _ = events.send(DriverEvent::TurnFinished {
                success: false,
                summary: None,
            });
            return false;
        }
    };
    let (success, summary) = match response.stop_reason {
        StopReason::EndTurn | StopReason::Cancelled => (true, None),
        StopReason::MaxTokens => (false, Some(tr!("session.agent_ran_out_of_context"))),
        StopReason::Refusal => (false, Some(tr!("session.agent_declined_turn"))),
        StopReason::MaxTurnRequests => (
            false,
            Some(tr!(
                "session.agent_stopped_reason",
                reason = "max_turn_requests"
            )),
        ),
        _ => (
            false,
            Some(tr!("session.agent_stopped_reason", reason = "unknown")),
        ),
    };
    let _ = events.send(DriverEvent::TurnFinished { success, summary });
    success
}

fn cancel_pending_permissions(pending: &PendingPermissions) {
    for (_, responder) in pending.lock().drain() {
        let _ = responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ));
    }
}

fn handle_permission_request(
    request: RequestPermissionRequest,
    responder: PermissionResponder,
    auto_approve: bool,
    pending: &PendingPermissions,
    events: &impl DriverEventSink,
) -> agent_client_protocol::Result<()> {
    let request_id = responder.id().to_string();
    let params = serde_json::to_value(&request)?;
    let options = request
        .options
        .iter()
        .map(|option| PermissionOption {
            id: option.option_id.to_string(),
            label: option.name.clone(),
            allow: matches!(
                option.kind,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            ),
        })
        .collect::<Vec<_>>();

    if auto_approve {
        let choice = request
            .options
            .iter()
            .find(|option| option.kind == PermissionOptionKind::AllowAlways)
            .or_else(|| {
                request
                    .options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            });
        return match choice {
            Some(choice) => responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    choice.option_id.clone(),
                )),
            )),
            None => responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            )),
        };
    }

    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("permission.run_a_tool"));
    let detail = permission_reason(&params).unwrap_or_else(|| {
        params
            .pointer("/toolCall/kind")
            .and_then(Value::as_str)
            .map(|kind| tr!("permission.agent_wants_to", action = kind))
            .unwrap_or_else(|| tr!("permission.agent_asks_for_permission"))
    });
    pending.lock().insert(request_id.clone(), responder);
    if events
        .send(DriverEvent::Permission {
            request_id: request_id.clone(),
            title,
            detail,
            options,
        })
        .is_err()
        && let Some(responder) = pending.lock().remove(&request_id)
    {
        let _ = responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ));
    }
    Ok(())
}

fn handle_session_update(
    notification: SessionNotification,
    events: &impl DriverEventSink,
    state: &mut AcpStreamState,
    config_state: &Mutex<AcpConfigState>,
) -> agent_client_protocol::Result<()> {
    if let SessionUpdate::ConfigOptionUpdate(update) = &notification.update {
        let mut config_state = config_state.lock();
        config_state.replace(update.config_options.clone());
        config_state.emit_models(events);
        return Ok(());
    }
    let update = serde_json::to_value(notification.update)?;
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => {
            if let Some(text) = update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
            }
        }
        Some("agent_thought_chunk") => {
            if let Some(text) = update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
            }
        }
        Some("tool_call" | "tool_call_update") => tool_activity(&update, events, state),
        Some("plan") => {
            let _ = events.send(DriverEvent::Activity {
                id: Some("acp-plan".into()),
                kind: ActivityKind::Plan,
                title: tr!("activity.plan_updated"),
                detail: None,
                complete: false,
            });
        }
        Some("available_commands_update") => {
            let commands = update
                .get("availableCommands")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|command| {
                            let name = command.get("name").and_then(Value::as_str)?;
                            Some(crate::model::ReportedCommand {
                                name: name.to_owned(),
                                description: command
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !commands.is_empty() {
                let _ = events.send(DriverEvent::AvailableCommands(commands));
            }
        }
        Some("session_info_update") => {
            if update.get("title").is_some() {
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let _ = events.send(DriverEvent::AutoTitleUpdated(title));
            }
        }
        Some("usage_update") => {
            let used = update
                .get("used")
                .and_then(Value::as_u64)
                .filter(|used| *used > 0);
            let window = ["max", "limit", "size", "contextWindow", "context_window"]
                .into_iter()
                .find_map(|key| update.get(key).and_then(Value::as_u64))
                .filter(|window| *window > 0);
            if used.is_some() || window.is_some() {
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens: used,
                    context_window: window,
                });
            }
        }
        // `user_message_chunk` is Waku's own prompt echoed back. Other typed
        // updates currently have no transcript representation.
        _ => {}
    }
    Ok(())
}

#[derive(Default)]
struct AcpStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
}

/// Pull the agent's explanation out of a permission request's tool call.
fn permission_reason(params: &Value) -> Option<String> {
    let content = params
        .pointer("/toolCall/content")
        .and_then(Value::as_array)?;
    let reason = content
        .iter()
        .filter_map(|entry| {
            entry
                .pointer("/content/text")
                .or_else(|| entry.get("text"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!reason.is_empty()).then(|| truncate(&reason, 400))
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars()
        .take(max_chars)
        .chain(std::iter::once('…'))
        .collect()
}

fn tool_activity(update: &Value, events: &impl DriverEventSink, state: &mut AcpStreamState) {
    let id = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let status = update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let complete = matches!(status, "completed" | "failed");
    let failed = status == "failed";

    let wire_kind = update.get("kind").and_then(Value::as_str);
    let wire_title = update.get("title").and_then(Value::as_str);
    let stored = id.as_ref().and_then(|id| {
        if complete {
            state.tools.remove(id)
        } else {
            state.tools.get(id).cloned()
        }
    });
    let mut kind = wire_kind
        .map(classify)
        .or_else(|| stored.as_ref().map(|(kind, _)| *kind))
        .unwrap_or(ActivityKind::Tool);
    if matches!(kind, ActivityKind::Search | ActivityKind::Tool)
        && let Some(wire_title) = wire_title
    {
        let named_kind = ActivityKind::from_tool_name(wire_title);
        if named_kind != ActivityKind::Tool {
            kind = named_kind;
        }
    }
    let arguments = update.get("rawInput").filter(|value| !value.is_null());
    let title = activity::input_title(arguments)
        .or_else(|| {
            wire_title
                .filter(|title| !title.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| stored.map(|(_, title)| title))
        .unwrap_or_else(|| "Tool".to_owned());
    if !complete && let Some(id) = id.as_ref() {
        state.tools.insert(id.clone(), (kind, title.clone()));
    }

    let output = update
        .get("content")
        .filter(|value| !value.is_null())
        .or_else(|| update.get("rawOutput").filter(|value| !value.is_null()));
    let item =
        activity::tool_activity(id, kind, title, arguments, output, output, failed, complete);
    let _ = events.send(DriverEvent::RichActivity(item));
}

fn classify(kind: &str) -> ActivityKind {
    match kind {
        "execute" => ActivityKind::Command,
        "edit" | "delete" | "move" => ActivityKind::FileChange,
        "read" => ActivityKind::FileRead,
        "search" | "fetch" => ActivityKind::Search,
        "think" => ActivityKind::Reasoning,
        _ => ActivityKind::Tool,
    }
}

impl DriverControl for AcpDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.try_send(CommandMessage::Prompt(prompt));
    }

    fn supports_steer(&self) -> bool {
        true
    }

    fn steer(&self, prompt: String) {
        let _ = self.commands.try_send(CommandMessage::Steer(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.try_send(CommandMessage::Cancel);
    }

    fn cancel_computer_use(&self) {
        if let Some(computer_use) = self.computer_use.as_ref() {
            computer_use.stop();
        }
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.try_send(CommandMessage::Respond {
            request_id,
            option_id,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        if options.mode != self.mode || options.interaction_mode != self.interaction_mode {
            return false;
        }
        self.commands
            .try_send(CommandMessage::Options(options))
            .is_ok()
    }

    fn rollback(&self, _turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        Err(anyhow!(
            "conversation rollback is not supported by this provider transport"
        ))
    }
}

impl Drop for AcpDriver {
    fn drop(&mut self) {
        self.cancel_computer_use();
        let _ = self.commands.try_send(CommandMessage::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        SessionMode, SessionModeState, ToolCallUpdate, ToolCallUpdateFields,
    };

    #[test]
    fn deerflow_launch_has_no_arguments_or_environment() {
        let launch = launch_for(ProviderKind::DeerFlow).unwrap();
        assert!(launch.args.is_empty());
        assert!(launch.env.is_empty());
    }

    #[test]
    fn plan_mode_selects_the_advertised_plan_mode() {
        let modes = SessionModeState::new(
            "agent",
            vec![
                SessionMode::new("agent", "Agent"),
                SessionMode::new("plan", "Plan"),
            ],
        );
        assert_eq!(
            desired_mode(Some(&modes), RuntimeMode::FullAccess, InteractionMode::Plan)
                .map(|mode| mode.to_string()),
            Some("plan".to_owned())
        );
        assert!(
            desired_mode(
                Some(&modes),
                RuntimeMode::FullAccess,
                InteractionMode::Build
            )
            .is_none()
        );
    }

    #[test]
    fn a_steer_only_settles_when_the_last_sdk_request_finishes() {
        let requests = Mutex::new(PendingPrompts::default());
        requests
            .lock()
            .insert(RequestId::Str("first".into()), None, "session".into());
        requests
            .lock()
            .insert(RequestId::Str("steer".into()), None, "session".into());
        assert!(!settle_prompt_request(
            &requests,
            &RequestId::Str("first".into())
        ));
        assert!(settle_prompt_request(
            &requests,
            &RequestId::Str("steer".into())
        ));
        assert!(!settle_prompt_request(
            &requests,
            &RequestId::Str("steer".into())
        ));
    }

    #[test]
    fn xai_prompt_complete_settles_a_missing_standard_response_once() {
        let requests = Mutex::new(PendingPrompts::default());
        let request_id = RequestId::Str("sdk-request".into());
        requests.lock().insert(
            request_id.clone(),
            Some("waku-prompt".into()),
            "grok-session".into(),
        );
        let (events, event_rx) = crate::driver::test_event_channel();

        assert_eq!(
            finish_xai_prompt_complete(
                &json!({
                    "sessionId": "grok-session",
                    "promptId": "waku-prompt",
                    "stopReason": "end_turn"
                }),
                &requests,
                &events,
            ),
            Some("grok-session".into())
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished {
                success: true,
                summary: None
            }
        ));
        assert!(!settle_prompt_request(&requests, &request_id));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn typed_prompt_response_settles_the_turn() {
        let (events, event_rx) = crossbeam_channel::unbounded();
        assert!(finish_prompt(
            Ok(PromptResponse::new(StopReason::EndTurn)),
            &events
        ));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished {
                success: true,
                summary: None
            }
        ));
    }

    #[test]
    fn typed_updates_preserve_text_reasoning_and_correlated_tools() {
        let (events, event_rx) = crossbeam_channel::unbounded();
        let mut state = AcpStreamState::default();
        let config_state = Mutex::new(AcpConfigState::default());
        let updates = [
            json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}),
            json!({"sessionUpdate":"tool_call","toolCallId":"call_1","title":"read","kind":"read","status":"pending","rawInput":{}}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed","title":"fixture.txt","content":[{"type":"content","content":{"type":"text","text":"waku probe fixture"}}]}),
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"OK"}}),
            json!({"sessionUpdate":"usage_update","used":9677,"size":500000}),
        ];
        for update in updates {
            let update = serde_json::from_value(update).unwrap();
            handle_session_update(
                SessionNotification::new("s", update),
                &events,
                &mut state,
                &config_state,
            )
            .unwrap();
        }

        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[1], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::FileRead && !item.complete));
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.complete
                    && item.title == "fixture.txt"
                    && item.output.as_deref().is_some_and(|output| output.contains("waku probe fixture"))));
        assert!(matches!(&seen[3], DriverEvent::TextDelta(text) if text == "OK"));
        assert!(matches!(
            &seen[4],
            DriverEvent::UsageUpdated {
                context_tokens: Some(9677),
                context_window: Some(500000),
            }
        ));
    }

    #[test]
    fn acp_model_config_supports_category_fallback_groups_and_stable_default() {
        let initial = serde_json::from_value::<Vec<SessionConfigOption>>(json!([
            {
                "id": "model",
                "name": "Model",
                "type": "select",
                "currentValue": "sonnet",
                "options": [
                    {
                        "group": "anthropic",
                        "name": "Anthropic",
                        "options": [
                            {"value": "sonnet", "name": "Claude Sonnet"},
                            {"value": "opus", "name": "Claude Opus"}
                        ]
                    },
                    {
                        "group": "openai",
                        "name": "OpenAI",
                        "options": [
                            {"value": "gpt", "name": "GPT"}
                        ]
                    }
                ]
            }
        ]))
        .unwrap();
        let mut state = AcpConfigState::default();
        state.replace_initial(initial);
        let (_, select) = state.model_config().expect("id=model is the ACP fallback");
        let models = models_from_select(select, state.default_model.as_deref());
        assert_eq!(state.current_model().as_deref(), Some("sonnet"));
        assert_eq!(models.len(), 3);
        assert!(models[0].is_default);
        assert_eq!(models[0].sub_provider.as_deref(), Some("Anthropic"));
        assert_eq!(models[2].sub_provider.as_deref(), Some("OpenAI"));

        let changed = serde_json::from_value::<Vec<SessionConfigOption>>(json!([
            {
                "id": "runtime-model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "opus",
                "options": [
                    {"value": "sonnet", "name": "Claude Sonnet"},
                    {"value": "opus", "name": "Claude Opus"}
                ]
            }
        ]))
        .unwrap();
        state.replace(changed);
        let (_, select) = state
            .model_config()
            .expect("the standard category takes precedence");
        let models = models_from_select(select, state.default_model.as_deref());
        assert_eq!(state.current_model().as_deref(), Some("opus"));
        assert!(models[0].is_default);
        assert!(!models[1].is_default);
    }

    #[test]
    fn config_option_update_replaces_the_complete_model_state() {
        let (events, event_rx) = crossbeam_channel::unbounded();
        let mut stream_state = AcpStreamState::default();
        let config_state = Mutex::new(AcpConfigState::default());
        let update = serde_json::from_value(json!({
            "sessionUpdate": "config_option_update",
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "m2",
                "options": [
                    {"value": "m1", "name": "Model One"},
                    {"value": "m2", "name": "Model Two"}
                ]
            }]
        }))
        .unwrap();
        handle_session_update(
            SessionNotification::new("s", update),
            &events,
            &mut stream_state,
            &config_state,
        )
        .unwrap();

        let event = event_rx.try_recv().unwrap();
        assert!(matches!(
            event,
            DriverEvent::ModelsUpdated {
                models,
                current_model: Some(current)
            } if models.len() == 2 && current == "m2"
        ));
        assert_eq!(config_state.lock().options.len(), 1);
    }

    #[test]
    fn permission_reason_preserves_the_agents_explanation() {
        let tool_call = ToolCallUpdate::new(
            "tool-1",
            serde_json::from_value::<ToolCallUpdateFields>(json!({
                "title": "rm -rf build",
                "kind": "execute",
                "content": [
                    {"type":"content","content":{"type":"text","text":"Not in allowlist: rm"}}
                ]
            }))
            .unwrap(),
        );
        let request = RequestPermissionRequest::new("s", tool_call, Vec::new());
        let params = serde_json::to_value(request).unwrap();
        assert_eq!(
            permission_reason(&params).as_deref(),
            Some("Not in allowlist: rm")
        );
    }

    /// Exercises the session-scoped ACP catalog and standard model setter
    /// against a real DeerFlow binary. Ignored by default because it depends
    /// on the user's configured DeerFlow installation.
    #[test]
    #[ignore = "requires WAKU_TEST_DEERFLOW_ACP"]
    fn deerflow_reports_and_switches_models_through_config_options() {
        let binary = std::env::var_os("WAKU_TEST_DEERFLOW_ACP")
            .map(std::path::PathBuf::from)
            .expect("set WAKU_TEST_DEERFLOW_ACP to deerflow-acp");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = AcpDriver::start(
            ProviderKind::DeerFlow,
            DriverStartOptions {
                binary,
                cwd: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the DeerFlow ACP session should open");

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let (models, current) = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let event = event_rx
                .recv_timeout(remaining)
                .expect("DeerFlow should advertise its model configOption");
            match event {
                DriverEvent::ModelsUpdated {
                    models,
                    current_model: Some(current),
                } => break (models, current),
                DriverEvent::Error(error) => panic!("DeerFlow reported: {error}"),
                DriverEvent::ProcessExited => panic!("DeerFlow exited before model discovery"),
                _ => {}
            }
        };
        let target = models
            .iter()
            .find(|model| model.id != current)
            .expect("the integration test needs at least two DeerFlow models")
            .id
            .clone();
        assert!(driver.apply_options(SessionOptions {
            mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Build,
            model: Some(target.clone()),
            reasoning_effort: None,
            service_tier: None,
        }));

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let event = event_rx
                .recv_timeout(remaining)
                .expect("DeerFlow should confirm the selected ACP model config");
            match event {
                DriverEvent::ModelsUpdated {
                    current_model: Some(current),
                    ..
                } if current == target => break,
                DriverEvent::Error(error) => panic!("DeerFlow reported: {error}"),
                DriverEvent::ProcessExited => panic!("DeerFlow exited during model selection"),
                _ => {}
            }
        }
    }

    /// Drives a real agent through the SDK-backed driver. Ignored by default:
    /// it needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated grok"]
    fn grok_prompt_response_from_the_sdk_finishes_the_turn() {
        let binary = crate::command_env::find_executable("grok").expect("grok is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = AcpDriver::start(
            ProviderKind::Grok,
            DriverStartOptions {
                binary,
                cwd: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some("grok-4.5".into()),
                reasoning_effort: None,
                service_tier: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the ACP session should open");

        loop {
            let event = event_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the agent should report its session");
            match event {
                DriverEvent::Connected {
                    provider_cursor: Some(ProviderResumeCursor::Grok { .. }),
                } => break,
                DriverEvent::Error(error) => panic!("the agent reported: {error}"),
                _ => {}
            }
        }
        driver.prompt("hi".into());
        let mut finished = None;
        while let Ok(event) = event_rx.recv_timeout(Duration::from_secs(120)) {
            match event {
                DriverEvent::TurnFinished { success, .. } => {
                    finished = Some(success);
                    break;
                }
                DriverEvent::Error(error) => panic!("the agent reported: {error}"),
                _ => {}
            }
        }
        assert_eq!(finished, Some(true));
    }
}

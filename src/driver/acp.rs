//! Agent Client Protocol transport.
//!
//! ACP is a bidirectional JSON-RPC session over stdio: one agent process serves
//! the whole conversation, streams `session/update` notifications, and asks the
//! client for tool permission with a real request it expects an answer to. That
//! makes it the only transport besides Codex's app-server where Waku's
//! Supervised mode means what it says.
//!
//! The payload shapes here were read off live agents (`opencode acp`,
//! `grok agent stdio`), not the specification alone.
//!
//! A second `session/prompt` sent while one is open is ACP's steer: the agent
//! continues the same conversation under the newer request, and only the last
//! open prompt's response settles the merged turn. How the agent absorbs the
//! message differs — probed against both: Cursor resolves the superseded
//! request `cancelled` and re-plans with the message in context; Grok finishes
//! the current work first and answers the message before settling.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, PermissionOption, ProviderKind,
    ProviderResumeCursor, RuntimeMode,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const PROTOCOL_VERSION: u64 = 1;

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

struct PendingResponse {
    sender: Sender<Result<Value, String>>,
    suppress_session_updates: bool,
}

#[derive(Default)]
struct PendingRequests {
    responses: Mutex<HashMap<u64, PendingResponse>>,
    suppress_session_updates: AtomicBool,
}

impl PendingRequests {
    fn insert(
        &self,
        id: u64,
        sender: Sender<Result<Value, String>>,
        suppress_session_updates: bool,
    ) {
        if suppress_session_updates {
            self.suppress_session_updates.store(true, Ordering::Release);
        }
        self.responses.lock().insert(
            id,
            PendingResponse {
                sender,
                suppress_session_updates,
            },
        );
    }

    fn take(&self, id: u64) -> Option<PendingResponse> {
        let response = self.responses.lock().remove(&id);
        if response
            .as_ref()
            .is_some_and(|response| response.suppress_session_updates)
        {
            // Clear this before waking the writer. The reader owns stdout
            // ordering, so any update after the load response is live again.
            self.suppress_session_updates
                .store(false, Ordering::Release);
        }
        response
    }

    fn session_updates_are_suppressed(&self) -> bool {
        self.suppress_session_updates.load(Ordering::Acquire)
    }
}

type PendingResponses = Arc<PendingRequests>;

pub struct AcpDriver {
    commands: Sender<CommandMessage>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    computer_use: Option<super::support::HeadlessComputerUseRuntime>,
}

/// Per-provider launch details. Everything below this is protocol, not provider.
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
            computer_use_enabled,
            provider_cursor,
        } = options;
        // A Cursor branch has no native session yet: its retained history is
        // carried in the cursor and replayed in the first prompt.
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

        // Grok's Computer Use support is an isolated GROK_HOME plus a rules
        // argument, and it is transport-independent — the ACP session needs the
        // same setup the headless turns had.
        let computer_use = (provider == ProviderKind::Grok && computer_use_enabled)
            .then(|| super::support::HeadlessComputerUseRuntime::start(provider, events.clone()))
            .transpose()?;
        let grok_title_home = computer_use
            .as_ref()
            .and_then(super::support::HeadlessComputerUseRuntime::grok_home)
            .map(ToOwned::to_owned);

        let mut command = crate::command_env::command(&binary);
        command.args(&launch.args).current_dir(&cwd);
        for (name, value) in &launch.env {
            command.env(name, value);
        }
        super::support::configure_grok_computer_use_command(
            &mut command,
            computer_use.as_ref().map(|runtime| &runtime.config),
        );
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start {} in ACP mode", provider.display_name()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("{} stdin unavailable", provider.display_name()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("{} stdout unavailable", provider.display_name()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("{} stderr unavailable", provider.display_name()))?;

        let (commands, command_rx) = unbounded();
        let pending: PendingResponses = Arc::new(PendingRequests::default());
        // Prompt requests stay open for the whole turn, so they are tracked
        // apart from the blocking request table: the writer must stay free to
        // send a cancel while they are outstanding. More than one is open
        // exactly while a steer is in flight, and only the last one to resolve
        // settles the merged turn.
        let prompt_requests = Arc::new(Mutex::new(Vec::<u64>::new()));
        let native_session_id = Arc::new(Mutex::new(None::<String>));
        let auto_approve = mode != RuntimeMode::Ask;

        let reader_pending = pending.clone();
        let reader_prompt = prompt_requests.clone();
        let reader_commands = commands.clone();
        let reader_events = events.clone();
        let reader_session_id = native_session_id.clone();
        let reader_thread = thread::Builder::new()
            .name(format!("waku-{}-acp-reader", provider.id()))
            .spawn(move || {
                let mut state = AcpStreamState::default();
                let title_refresh = super::title_refresh::NativeTitleRefresh::default();
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let turn_result = handle_message(
                        value,
                        &reader_pending,
                        &reader_prompt,
                        &reader_commands,
                        &reader_events,
                        auto_approve,
                        &mut state,
                    );
                    if provider == ProviderKind::Grok
                        && turn_result == Some(true)
                        && let Some(session_id) = reader_session_id.lock().clone()
                    {
                        let grok_home = grok_title_home.clone();
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
                            reader_events.clone(),
                            move || match grok_home.as_deref() {
                                Some(home) => {
                                    crate::grok_session::generated_title_in(home, &session_id)
                                }
                                None => crate::grok_session::generated_title(&session_id),
                            },
                        );
                    }
                }
                fail_pending(&reader_pending, "the ACP agent exited");
            })?;

        let writer_pending = pending.clone();
        let writer_prompt = prompt_requests;
        let writer_session_id = native_session_id;
        let writer_events = events.clone();
        let provider_name = provider.display_name();
        thread::Builder::new()
            .name(format!("waku-{}-acp-writer", provider.id()))
            .spawn(move || {
                let mut stdin = stdin;
                let mut next_id = 0_u64;
                let session = (|| -> Result<String, String> {
                    let initialize = request(
                        &mut stdin,
                        &writer_pending,
                        &mut next_id,
                        "initialize",
                        json!({
                            "protocolVersion": PROTOCOL_VERSION,
                            "clientCapabilities": {
                                // Waku does not proxy the agent's file or
                                // terminal access, so it claims neither. An
                                // advertised capability the client cannot honor
                                // strands the agent mid-tool-call.
                                "fs": {"readTextFile": false, "writeTextFile": false},
                                "terminal": false
                            }
                        }),
                    )?;
                    establish_session(
                        &initialize,
                        resume_session_id.as_deref(),
                        &cwd,
                        mode,
                        interaction_mode,
                        |method, params, suppress_session_updates| {
                            if suppress_session_updates {
                                request_suppressing_session_updates(
                                    &mut stdin,
                                    &writer_pending,
                                    &mut next_id,
                                    method,
                                    params,
                                )
                            } else {
                                request(&mut stdin, &writer_pending, &mut next_id, method, params)
                            }
                        },
                    )
                })();

                let session_id = match session {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        let _ = writer_events.send(DriverEvent::Error(tr!(
                            "errors.open_provider_session",
                            provider = provider_name,
                            error = error
                        )));
                        let _ = writer_events.send(DriverEvent::TurnFinished {
                            success: false,
                            summary: Some(format!("{provider_name} could not start a session.")),
                        });
                        return;
                    }
                };
                *writer_session_id.lock() = Some(session_id.clone());
                let _ = writer_events.send(DriverEvent::Connected {
                    provider_cursor: Some(ProviderResumeCursor::from_session_id(
                        provider,
                        session_id.clone(),
                    )),
                });

                let mut fork_context = fork_context;
                let mut current_model = model;
                apply_model(
                    &mut stdin,
                    &writer_pending,
                    &mut next_id,
                    &session_id,
                    current_model.as_deref(),
                    reasoning_effort.as_deref(),
                    &writer_events,
                );

                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(text) => {
                            let text = fork_context
                                .take()
                                .map(|context| {
                                    crate::cursor_session::prompt_with_fork_context(&context, &text)
                                })
                                .unwrap_or(text);
                            let _ = writer_events.send(DriverEvent::TurnStarted);
                            next_id += 1;
                            writer_prompt.lock().push(next_id);
                            let sent = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": next_id,
                                    "method": "session/prompt",
                                    "params": {
                                        "sessionId": session_id,
                                        "prompt": [{"type": "text", "text": text}]
                                    }
                                }),
                            );
                            if let Err(error) = sent {
                                writer_prompt.lock().retain(|id| *id != next_id);
                                let _ = writer_events.send(DriverEvent::Error(format!(
                                    "{provider_name} transport write failed: {error}"
                                )));
                                let _ = writer_events.send(DriverEvent::TurnFinished {
                                    success: false,
                                    summary: Some(format!(
                                        "{provider_name} could not receive the prompt."
                                    )),
                                });
                            }
                        }
                        CommandMessage::Steer(text) => {
                            // A second `session/prompt` while one is open: the
                            // agent continues the conversation under this newer
                            // request and the superseded one resolves without
                            // settling the merged turn (see the module doc for
                            // the per-agent absorption policies).
                            if writer_prompt.lock().is_empty() {
                                let _ = writer_events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: format!("{provider_name} has no active turn to steer."),
                                });
                                continue;
                            }
                            next_id += 1;
                            writer_prompt.lock().push(next_id);
                            let sent = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": next_id,
                                    "method": "session/prompt",
                                    "params": {
                                        "sessionId": session_id,
                                        "prompt": [{"type": "text", "text": text.clone()}]
                                    }
                                }),
                            );
                            match sent {
                                Ok(()) => {
                                    let _ = writer_events
                                        .send(DriverEvent::SteerAccepted { message: text });
                                }
                                Err(error) => {
                                    writer_prompt.lock().retain(|id| *id != next_id);
                                    let _ = writer_events.send(DriverEvent::SteerRejected {
                                        message: text,
                                        reason: format!(
                                            "{provider_name} transport write failed: {error}"
                                        ),
                                    });
                                }
                            }
                        }
                        CommandMessage::Cancel => {
                            // A notification, not a request: the outstanding
                            // `session/prompt` is what reports the cancellation.
                            let _ = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "method": "session/cancel",
                                    "params": {"sessionId": session_id}
                                }),
                            );
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let Ok(id) = request_id.parse::<u64>() else {
                                continue;
                            };
                            let _ = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "outcome": {
                                            "outcome": "selected",
                                            "optionId": option_id
                                        }
                                    }
                                }),
                            );
                        }
                        CommandMessage::Options(options) => {
                            if options.model != current_model {
                                current_model = options.model;
                                apply_model(
                                    &mut stdin,
                                    &writer_pending,
                                    &mut next_id,
                                    &session_id,
                                    current_model.as_deref(),
                                    options.reasoning_effort.as_deref(),
                                    &writer_events,
                                );
                            }
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })?;

        let last_visible_stderr = Arc::new(Mutex::new(None::<String>));
        let stderr_last_error = last_visible_stderr.clone();
        let stderr_events = events.clone();
        let stderr_thread = thread::Builder::new()
            .name(format!("waku-{}-acp-stderr", provider.id()))
            .spawn(move || {
                let lines = BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                    .filter(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>();
                if let Some(message) = super::support::provider_stderr_error(lines) {
                    let error = format!("{provider_name}: {message}");
                    *stderr_last_error.lock() = Some(error.clone());
                    let _ = stderr_events.send(DriverEvent::Error(error));
                }
            })?;

        thread::Builder::new()
            .name(format!("waku-{}-acp-process", provider.id()))
            .spawn(move || {
                let status = child.wait();
                let _ = reader_thread.join();
                let _ = stderr_thread.join();
                if let Ok(status) = status
                    && !status.success()
                    && last_visible_stderr.lock().is_none()
                {
                    let _ = events.send(DriverEvent::Error(format!(
                        "{provider_name} exited with {status}"
                    )));
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            mode,
            interaction_mode,
            computer_use,
        })
    }
}

impl DriverControl for AcpDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Prompt(prompt));
    }

    fn supports_steer(&self) -> bool {
        true
    }

    fn steer(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Steer(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.send(CommandMessage::Cancel);
    }

    fn cancel_computer_use(&self) {
        if let Some(computer_use) = self.computer_use.as_ref() {
            computer_use.stop();
        }
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.send(CommandMessage::Respond {
            request_id,
            option_id,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // The permission posture is decided when the session opens, because it
        // is what decides whether approvals reach the user at all.
        if options.mode != self.mode || options.interaction_mode != self.interaction_mode {
            return false;
        }
        self.commands.send(CommandMessage::Options(options)).is_ok()
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
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

/// Picks the agent's own mode id for Waku's interaction mode.
///
/// Only Plan maps here. Supervised deliberately stays in the agent mode: ACP's
/// read-only "ask" mode answers questions instead of asking permission, whereas
/// Supervised means the agent still acts — it just checks first, which is what
/// `session/request_permission` already does.
fn desired_mode(
    opened: &Value,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) -> Option<String> {
    if interaction_mode != InteractionMode::Plan && mode != RuntimeMode::Plan {
        return None;
    }
    let modes = opened.pointer("/result/modes")?;
    let available = modes.get("availableModes").and_then(Value::as_array)?;
    let plan = available.iter().find_map(|entry| {
        let id = entry.get("id").and_then(Value::as_str)?;
        id.eq_ignore_ascii_case("plan").then(|| id.to_owned())
    })?;
    (modes.get("currentModeId").and_then(Value::as_str) != Some(plan.as_str())).then_some(plan)
}

/// Opens a fresh session or restores the provider cursor negotiated during
/// `initialize`. `session/resume` is preferred because ACP forbids history
/// replay there. Older agents fall back to `session/load`, whose replay is
/// suppressed until its response because Waku already restored the transcript
/// from SQLite.
fn establish_session(
    initialize: &Value,
    resume_session_id: Option<&str>,
    cwd: &std::path::Path,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    mut send_request: impl FnMut(&str, Value, bool) -> Result<Value, String>,
) -> Result<String, String> {
    let can_resume = initialize
        .pointer("/result/agentCapabilities/sessionCapabilities/resume")
        .is_some_and(Value::is_object);
    let can_load = initialize
        .pointer("/result/agentCapabilities/loadSession")
        .and_then(Value::as_bool)
        == Some(true);

    let restored = resume_session_id.and_then(|session_id| {
        let (method, suppress_session_updates) = if can_resume {
            ("session/resume", false)
        } else if can_load {
            ("session/load", true)
        } else {
            return None;
        };
        send_request(
            method,
            json!({
                "sessionId": session_id,
                "cwd": cwd,
                "mcpServers": []
            }),
            suppress_session_updates,
        )
        .ok()
        .map(|response| (session_id.to_owned(), response))
    });

    let (session_id, opened) = match restored {
        Some(restored) => restored,
        None => {
            // A cursor the agent no longer recognizes must not strand the
            // task: start fresh and let this id replace the stale cursor.
            let opened = send_request("session/new", json!({"cwd": cwd, "mcpServers": []}), false)?;
            let session_id = opened
                .pointer("/result/sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "the ACP agent returned no session id".to_owned())?;
            (session_id, opened)
        }
    };

    if let Some(mode) = desired_mode(&opened, mode, interaction_mode) {
        // Not fatal: an agent without the mode still runs, it just runs with
        // its default posture. Restoration responses carry the same optional
        // mode state as new-session responses, so this must run for both.
        let _ = send_request(
            "session/set_mode",
            json!({"sessionId": session_id, "modeId": mode}),
            false,
        );
    }
    Ok(session_id)
}

fn apply_model(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_id: &mut u64,
    session_id: &str,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    events: &impl DriverEventSink,
) {
    let Some(model) = model else {
        return;
    };
    if let Err(error) = request(
        stdin,
        pending,
        next_id,
        "session/set_model",
        json!({"sessionId": session_id, "modelId": model}),
    ) {
        let _ = events.send(DriverEvent::Error(tr!(
            "errors.select_model",
            error = error
        )));
        return;
    }
    // Reasoning effort is a session config option rather than a first-class
    // field, and not every agent offers one; a rejection is not turn-fatal.
    if let Some(effort) = reasoning_effort {
        let _ = request(
            stdin,
            pending,
            next_id,
            "session/set_config_option",
            json!({"sessionId": session_id, "configId": "mode", "value": effort}),
        );
    }
}

fn request(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    request_with_update_policy(stdin, pending, next_id, method, params, false)
}

fn request_suppressing_session_updates(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    request_with_update_policy(stdin, pending, next_id, method, params, true)
}

fn request_with_update_policy(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_id: &mut u64,
    method: &str,
    params: Value,
    suppress_session_updates: bool,
) -> Result<Value, String> {
    *next_id += 1;
    let id = *next_id;
    let (response_tx, response_rx) = bounded(1);
    pending.insert(id, response_tx, suppress_session_updates);
    let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    if let Err(error) = write_line(stdin, &message) {
        pending.take(id);
        return Err(format!("transport write failed: {error}"));
    }
    match response_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(response) => response,
        Err(_) => {
            pending.take(id);
            Err(format!("{method} timed out"))
        }
    }
}

fn write_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn fail_pending(pending: &PendingResponses, message: &str) {
    pending
        .suppress_session_updates
        .store(false, Ordering::Release);
    for (_, response) in pending.responses.lock().drain() {
        let _ = response.sender.send(Err(message.to_owned()));
    }
}

#[derive(Default)]
struct AcpStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
}

#[allow(clippy::too_many_arguments)]
fn handle_message(
    value: Value,
    pending: &PendingResponses,
    prompt_requests: &Mutex<Vec<u64>>,
    commands: &Sender<CommandMessage>,
    events: &impl DriverEventSink,
    auto_approve: bool,
    state: &mut AcpStreamState,
) -> Option<bool> {
    let id = value.get("id").and_then(Value::as_u64);
    let method = value.get("method").and_then(Value::as_str);

    // A message with an id and no method is a reply to something Waku sent.
    if let Some(id) = id
        && method.is_none()
    {
        {
            let mut prompts = prompt_requests.lock();
            if let Some(position) = prompts.iter().position(|prompt| *prompt == id) {
                prompts.remove(position);
                let settles = prompts.is_empty();
                drop(prompts);
                // A steer-superseded prompt resolves early — Cursor answers it
                // `cancelled` the moment the newer prompt lands — while the
                // merged turn keeps running under that newer request. Only the
                // last open prompt settles the turn.
                if settles {
                    return Some(finish_turn(&value, events));
                }
                return None;
            }
        }
        let result = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map_or_else(|| Ok(value.clone()), |error| Err(error.to_owned()));
        if let Some(response) = pending.take(id) {
            let _ = response.sender.send(result);
        }
        return None;
    }

    let Some(method) = method else {
        return None;
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    // An id alongside a method makes this the agent asking Waku for something.
    if let Some(id) = id {
        if method == "session/request_permission" {
            request_permission(id, &params, commands, events, auto_approve);
        }
        return None;
    }

    if method != "session/update" {
        // Everything else on this channel is agent-private control traffic
        // (`_x.ai/*` and friends). It must never reach the transcript.
        return None;
    }
    if pending.session_updates_are_suppressed() {
        return None;
    }
    let update = params.get("update").unwrap_or(&Value::Null);
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
        Some("tool_call" | "tool_call_update") => {
            tool_activity(update, events, state);
        }
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
            // Grok reports the tokens occupying the model's context; a bound
            // arrives under provider-specific names when it arrives at all.
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
        // `user_message_chunk` is Waku's own prompt echoed back.
        _ => {}
    }
    None
}

fn finish_turn(value: &Value, events: &impl DriverEventSink) -> bool {
    if let Some(error) = value
        .pointer("/error/data/message")
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str)
    {
        let _ = events.send(DriverEvent::Error(error.to_owned()));
        let _ = events.send(DriverEvent::TurnFinished {
            success: false,
            summary: None,
        });
        return false;
    }
    let stop_reason = value
        .pointer("/result/stopReason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let success = matches!(stop_reason, "end_turn" | "cancelled");
    let _ = events.send(DriverEvent::TurnFinished {
        success,
        summary: match stop_reason {
            "end_turn" | "cancelled" => None,
            "max_tokens" => Some(tr!("session.agent_ran_out_of_context")),
            "refusal" => Some(tr!("session.agent_declined_turn")),
            other => Some(tr!("session.agent_stopped_reason", reason = other)),
        },
    });
    success
}

fn request_permission(
    id: u64,
    params: &Value,
    commands: &Sender<CommandMessage>,
    events: &impl DriverEventSink,
    auto_approve: bool,
) {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = option.get("optionId").and_then(Value::as_str)?;
                    let kind = option
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(PermissionOption {
                        id: id.to_owned(),
                        label: option
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_owned(),
                        allow: kind.starts_with("allow"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if auto_approve {
        // Outside Supervised the user has already answered this question once,
        // for the whole session. Prefer the durable allow so the agent stops
        // asking for the same tool.
        let choice = options
            .iter()
            .find(|option| option.allow && option.id.contains("always"))
            .or_else(|| options.iter().find(|option| option.allow));
        if let Some(choice) = choice {
            let _ = commands.send(CommandMessage::Respond {
                request_id: id.to_string(),
                option_id: choice.id.clone(),
            });
        }
        return;
    }

    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("permission.run_a_tool"));
    // The agent explains why it is asking — "Not in allowlist: cat, pwd" — and
    // that reason is the whole basis for the user's decision. Only fall back to
    // the tool kind when it says nothing.
    let detail = permission_reason(params).unwrap_or_else(|| {
        params
            .pointer("/toolCall/kind")
            .and_then(Value::as_str)
            .map(|kind| tr!("permission.agent_wants_to", action = kind))
            .unwrap_or_else(|| tr!("permission.agent_asks_for_permission"))
    });
    let _ = events.send(DriverEvent::Permission {
        request_id: id.to_string(),
        title,
        detail,
        options,
    });
}

/// Pulls the agent's own explanation out of a permission request's tool call.
///
/// ACP nests it as `content: [{type: "content", content: {type: "text", …}}]`,
/// and agents also emit the inner shape directly, so both are accepted.
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

/// ACP names tool kinds in the same vocabulary Grok's headless stream already
/// used, which is why this mapping is shared with it.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> (
        PendingResponses,
        Mutex<Vec<u64>>,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        Sender<DriverEvent>,
        crossbeam_channel::Receiver<DriverEvent>,
        AcpStreamState,
    ) {
        let (commands, command_rx) = unbounded();
        let (events, event_rx) = unbounded();
        (
            Arc::new(PendingRequests::default()),
            Mutex::new(Vec::new()),
            commands,
            command_rx,
            events,
            event_rx,
            AcpStreamState::default(),
        )
    }

    #[test]
    fn resume_is_preferred_when_the_agent_advertises_it() {
        let initialize = json!({
            "result": {
                "agentCapabilities": {
                    "loadSession": true,
                    "sessionCapabilities": {"resume": {}}
                }
            }
        });
        let mut calls = Vec::new();
        let session_id = establish_session(
            &initialize,
            Some("existing-session"),
            std::path::Path::new("/tmp/project"),
            RuntimeMode::FullAccess,
            InteractionMode::Build,
            |method, params, suppress_session_updates| {
                calls.push((method.to_owned(), params, suppress_session_updates));
                Ok(json!({"result": {}}))
            },
        )
        .unwrap();

        assert_eq!(session_id, "existing-session");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "session/resume");
        assert!(!calls[0].2);
        assert_eq!(calls[0].1["sessionId"], "existing-session");
    }

    #[test]
    fn legacy_load_suppresses_replay_and_still_applies_plan_mode() {
        let initialize = json!({
            "result": {"agentCapabilities": {"loadSession": true}}
        });
        let mut calls = Vec::new();
        let session_id = establish_session(
            &initialize,
            Some("existing-session"),
            std::path::Path::new("/tmp/project"),
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            |method, params, suppress_session_updates| {
                calls.push((method.to_owned(), params, suppress_session_updates));
                match method {
                    "session/load" => Ok(json!({
                        "result": {
                            "modes": {
                                "currentModeId": "agent",
                                "availableModes": [
                                    {"id": "agent"},
                                    {"id": "plan"}
                                ]
                            }
                        }
                    })),
                    "session/set_mode" => Ok(json!({"result": {}})),
                    other => panic!("unexpected ACP request: {other}"),
                }
            },
        )
        .unwrap();

        assert_eq!(session_id, "existing-session");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "session/load");
        assert!(calls[0].2, "history replay must be suppressed");
        assert_eq!(calls[1].0, "session/set_mode");
        assert!(!calls[1].2);
        assert_eq!(calls[1].1["sessionId"], "existing-session");
        assert_eq!(calls[1].1["modeId"], "plan");
    }

    #[test]
    fn session_load_replay_is_dropped_until_the_response_arrives() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        let (response_tx, response_rx) = bounded(1);
        pending.insert(7, response_tx, true);

        let update = |text| {
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "s",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": text}
                    }
                }
            })
        };
        handle_message(
            update("old answer"),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert!(event_rx.try_recv().is_err());

        handle_message(
            json!({"jsonrpc": "2.0", "id": 7, "result": {}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert!(response_rx.try_recv().is_ok());
        assert!(!pending.session_updates_are_suppressed());

        handle_message(
            update("new answer"),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "new answer"
        ));
    }

    /// Drives a real agent through the actual driver. Ignored by default: it
    /// needs the CLI installed, credentials, and the network. Run with
    /// `cargo test --bin waku acp_session_against_a_real_agent -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated cursor-agent"]
    fn acp_session_against_a_real_agent() {
        let binary = crate::command_env::find_executable("cursor-agent")
            .expect("cursor-agent is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = AcpDriver::start(
            ProviderKind::Cursor,
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the ACP session should open");

        let connected = event_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the agent should report its session");
        assert!(matches!(
            connected,
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Cursor { .. })
            }
        ));

        driver.prompt("Reply with exactly: OK. Do not use any tools.".into());
        let mut text = String::new();
        let mut finished = None;
        while let Ok(event) = event_rx.recv_timeout(Duration::from_secs(120)) {
            match event {
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
                DriverEvent::TurnFinished { success, .. } => {
                    finished = Some(success);
                    break;
                }
                DriverEvent::Error(error) => panic!("the agent reported: {error}"),
                _ => {}
            }
        }
        assert_eq!(finished, Some(true), "the turn should settle successfully");
        assert!(
            text.contains("OK"),
            "expected the reply to stream through, got {text:?}"
        );
    }

    #[test]
    fn streams_text_reasoning_and_correlated_tools_in_wire_order() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        // Payloads copied from a live `opencode acp` session.
        let wire = [
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call_1","title":"read","kind":"read","status":"pending","rawInput":{}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed","title":"fixture.txt","content":[{"type":"content","content":{"type":"text","text":"waku probe fixture"}}]}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"OK"}}}}),
            // Agent-private control traffic must not reach the transcript.
            json!({"jsonrpc":"2.0","method":"_x.ai/models/update","params":{"currentModelId":"grok-4.5"}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"usage_update","used":9677}}}),
        ];
        for message in wire {
            handle_message(
                message, &pending, &prompt, &commands, &events, true, &mut state,
            );
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[1], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::FileRead && !item.complete));
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.complete
                    && item.title == "fixture.txt"
                    && item.display_target.as_deref() == Some("fixture.txt")
                    && item.output.as_deref().is_some_and(|output| output.contains("waku probe fixture"))));
        assert!(matches!(&seen[3], DriverEvent::TextDelta(text) if text == "OK"));
        // `usage_update` feeds the context meter rather than the transcript.
        assert!(matches!(
            &seen[4],
            DriverEvent::UsageUpdated {
                context_tokens: Some(9677),
                context_window: None,
            }
        ));
        assert_eq!(seen.len(), 5, "control traffic leaked into the transcript");
    }

    #[test]
    fn session_info_titles_follow_acp_optional_nullable_semantics() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        let mut send = |update| {
            handle_message(
                json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {"sessionId": "s", "update": update}
                }),
                &pending,
                &prompt,
                &commands,
                &events,
                true,
                &mut state,
            );
        };

        send(json!({"sessionUpdate": "session_info_update"}));
        assert!(
            event_rx.try_recv().is_err(),
            "an omitted title is no update"
        );

        send(json!({
            "sessionUpdate": "session_info_update",
            "title": "Provider session title"
        }));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Provider session title"
        ));

        send(json!({"sessionUpdate": "session_info_update", "title": null}));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(None)
        ));
    }

    #[test]
    fn supervised_mode_asks_the_user_and_auto_modes_answer_themselves() {
        let (pending, prompt, commands, command_rx, events, event_rx, mut state) = harness();
        let permission = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/request_permission",
            "params": {
                "sessionId": "s",
                // Shape observed from a live `cursor-agent acp` approval.
                "toolCall": {
                    "title": "rm -rf build",
                    "kind": "execute",
                    "content": [
                        {"type": "content", "content": {"type": "text", "text": "Not in allowlist: rm"}}
                    ]
                },
                "options": [
                    {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "allow-always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
                ]
            }
        });

        let outcome = handle_message(
            permission.clone(),
            &pending,
            &prompt,
            &commands,
            &events,
            false,
            &mut state,
        );
        assert_eq!(outcome, None);
        let DriverEvent::Permission {
            request_id,
            options,
            detail,
            title,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "7");
        assert_eq!(options.iter().filter(|option| option.allow).count(), 2);
        assert_eq!(title, "rm -rf build");
        // The agent's own reason is what the user is actually deciding on.
        assert_eq!(detail, "Not in allowlist: rm");
        assert!(command_rx.try_recv().is_err());

        let outcome = handle_message(
            permission, &pending, &prompt, &commands, &events, true, &mut state,
        );
        assert_eq!(outcome, None);
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "allow-always");
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn the_open_prompt_request_settles_the_turn_exactly_once() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        prompt.lock().push(3);

        let outcome = handle_message(
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert_eq!(outcome, Some(true));

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert!(prompt.lock().is_empty());
        // A late duplicate must not settle a turn that already ended.
        let duplicate = handle_message(
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert_eq!(duplicate, None);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn a_steer_superseded_prompt_resolves_without_settling_the_merged_turn() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        prompt.lock().push(3);
        prompt.lock().push(4);

        // Cursor answers the superseded request `cancelled` the moment the
        // steering prompt lands; the merged turn must keep running.
        let superseded = handle_message(
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert_eq!(superseded, None);
        assert!(
            event_rx.try_recv().is_err(),
            "a superseded prompt must not settle the merged turn"
        );
        assert_eq!(*prompt.lock(), vec![4]);

        // The last open prompt is what settles it.
        let outcome = handle_message(
            json!({"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert_eq!(outcome, Some(true));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert!(prompt.lock().is_empty());
    }

    /// Proves steering through the actual driver against a real ACP agent: the
    /// message injected while the shell tool sleeps lands inside the same
    /// merged turn — one SteerAccepted, one TurnFinished, and a reply that
    /// honors the injected instruction. Ignored by default: needs the CLI
    /// installed, credentials, and the network.
    fn acp_steering_against_a_real_agent(provider: ProviderKind, binary_name: &str) {
        let binary = crate::command_env::find_executable(binary_name)
            .unwrap_or_else(|| panic!("{binary_name} is not installed"));
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = AcpDriver::start(
            provider,
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the ACP session should open");

        driver.prompt(
            "Run exactly `sleep 6` in the shell (nothing else). \
             After the command completes, reply with exactly: FIRST DONE"
                .into(),
        );

        let mut text = String::new();
        let mut steered = false;
        let mut steer_accepted = false;
        let mut turns_finished = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(Duration::from_secs(5)) else {
                // Quiet after the turn settled means no second turn is coming.
                if turns_finished == 1 {
                    break;
                }
                continue;
            };
            match event {
                DriverEvent::RichActivity(item) if !steered && !item.complete => {
                    // The tool is running: the turn is unambiguously live.
                    steered = true;
                    driver.steer(
                        "ADDITIONAL INSTRUCTION: end your very next reply \
                         with the word BANANA."
                            .into(),
                    );
                }
                DriverEvent::SteerAccepted { message } => {
                    assert!(message.contains("BANANA"));
                    steer_accepted = true;
                }
                DriverEvent::SteerRejected { reason, .. } => {
                    panic!("the steer should be accepted, got rejection: {reason}");
                }
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
                DriverEvent::TurnFinished { success, .. } => {
                    assert!(success, "the turn should settle successfully");
                    turns_finished += 1;
                }
                DriverEvent::Error(error) => panic!("the agent reported: {error}"),
                _ => {}
            }
        }

        assert!(steered, "the probe never saw the tool start");
        assert!(steer_accepted, "the driver should acknowledge the steer");
        assert_eq!(
            turns_finished, 1,
            "a steered message must not settle a second turn"
        );
        assert!(
            text.contains("BANANA"),
            "the steered instruction should shape the same turn's reply, got {text:?}"
        );
    }

    #[test]
    #[ignore = "requires an installed, authenticated cursor-agent"]
    fn cursor_steering_folds_a_mid_turn_message_into_the_running_turn() {
        acp_steering_against_a_real_agent(ProviderKind::Cursor, "cursor-agent");
    }

    #[test]
    #[ignore = "requires an installed, authenticated grok"]
    fn grok_steering_folds_a_mid_turn_message_into_the_running_turn() {
        acp_steering_against_a_real_agent(ProviderKind::Grok, "grok");
    }

    #[test]
    fn a_failed_prompt_reports_the_agent_error_and_fails_the_turn() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        prompt.lock().push(3);

        // Shape observed from `grok agent stdio` with an exhausted balance.
        let outcome = handle_message(
            json!({"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"Internal error","data":{"message":"API error (status 402 Payment Required): Grok Build usage balance exhausted","http_status":402}}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert_eq!(outcome, Some(false));

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Error(message) if message.contains("402")
        ));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished { success: false, .. }
        ));
    }
}

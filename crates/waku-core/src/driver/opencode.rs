//! `opencode serve` is OpenCode's real API: one resident process serves
//! every session in a workspace, streams server-sent events, and answers
//! permission requests the user can actually be asked. Waku already started
//! this server for a side-quest — forking a session — while running
//! conversations through one-shot `opencode run` invocations; this drives
//! everything through it, pooled per workspace via `opencode_pool` so
//! sessions share the process instead of starting one each. A prompt posted
//! into a busy session is folded into the running turn rather than queued
//! behind it, which is what makes steering a plain post.
//!
//! Routes and payload shapes here were read off a live server's OpenAPI
//! document and event stream, not guessed.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use super::support::{OpenCodePermissionRequest, OpenCodePermissionState, permission_responses};
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::http_wire::{Endpoint, StreamControl, open_event_stream};
use crate::model::{
    ActivityKind, DriverEvent, PermissionOption, ProviderResumeCursor, ReportedCommand,
    RuntimeMode, UserInputAnswer, UserInputOption, UserInputQuestion,
};
use crate::opencode_pool::PooledServer;
use crate::opencode_session::{
    OpenCodeServer, encode_path_segment, fork_session_removing_turns_on_server,
};

enum CommandMessage {
    Prompt(String),
    Steer(String),
    NativeCommandFinished {
        generation: u64,
        steer: Option<String>,
        result: Result<(), String>,
    },
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    RespondUserInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
    Shutdown,
}

/// The prompt body both turn starts and steers post; the model rides on every
/// prompt because the server has no session-level model setting.
///
/// `variant` is how OpenCode expresses reasoning effort — the same ids the
/// model catalogue advertises from each model's `variants` map — and it is a
/// sibling of `model`, not a field inside it.
fn prompt_body(text: &str, model: Option<&str>, variant: Option<&str>, agent: &str) -> Value {
    let mut body = json!({
        "agent": agent,
        "parts": [{"type": "text", "text": text}]
    });
    if let Some((provider_id, model_id)) = model.and_then(|model| model.split_once('/')) {
        body["model"] = json!({"providerID": provider_id, "modelID": model_id});
    }
    if let Some(variant) = variant.map(str::trim).filter(|variant| !variant.is_empty()) {
        body["variant"] = json!(variant);
    }
    body
}

fn native_command_body(
    text: &str,
    commands: &HashSet<String>,
    model: Option<&str>,
    variant: Option<&str>,
    agent: &str,
) -> Option<Value> {
    let invocation = text.strip_prefix('/')?;
    let (name, arguments) = invocation
        .split_once(char::is_whitespace)
        .unwrap_or((invocation, ""));
    if !commands.contains(name) {
        return None;
    }
    let mut body = json!({"command": name, "arguments": arguments.trim(), "agent": agent});
    // The command route takes a provider/model string, unlike prompt_async.
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    if let Some(variant) = variant.map(str::trim).filter(|variant| !variant.is_empty()) {
        body["variant"] = json!(variant);
    }
    Some(body)
}

fn start_native_command(
    port: u16,
    session_id: &str,
    body: Value,
    commands: Sender<CommandMessage>,
    generation: u64,
    steer: Option<String>,
) -> std::io::Result<()> {
    let path = format!("/session/{}/command", encode_path_segment(session_id));
    // Unlike prompt_async, /command holds its response until the turn ends.
    // Keep the control worker free to answer permissions, stop, and steer.
    // A port alone cannot keep the pooled server alive after driver teardown.
    thread::Builder::new()
        .name("waku-opencode-command".into())
        .spawn(move || {
            let result = crate::opencode_session::request_json_on_port(
                port,
                "POST",
                &path,
                Some(&body),
                Duration::from_secs(30 * 60),
            )
            .map(|_| ())
            .map_err(|error| error.to_string());
            let _ = commands.send(CommandMessage::NativeCommandFinished {
                generation,
                steer,
                result,
            });
        })?;
    Ok(())
}

fn reject_prompt(error: impl std::fmt::Display, events: &impl DriverEventSink, turn: &Mutex<bool>) {
    let _ = events.send(DriverEvent::Error(tr!(
        "errors.provider_rejected_prompt_detail",
        provider = "OpenCode",
        error = error
    )));
    // session.idle never arrives for a request that failed to start.
    if std::mem::take(&mut *turn.lock()) {
        let _ = events.send(DriverEvent::TurnFinished {
            success: false,
            summary: Some(tr!("errors.provider_start_turn", provider = "OpenCode")),
        });
    }
}

fn opencode_permission_rules(mode: RuntimeMode) -> Value {
    let rule = |permission: &str, action: &str| {
        json!({
            "permission": permission,
            "pattern": "*",
            "action": action,
        })
    };

    match mode {
        RuntimeMode::Ask => Value::Array(vec![rule("bash", "ask"), rule("edit", "ask")]),
        RuntimeMode::AutoAcceptEdits => {
            Value::Array(vec![rule("bash", "ask"), rule("edit", "allow")])
        }
        RuntimeMode::Auto | RuntimeMode::FullAccess => Value::Array(vec![rule("*", "allow")]),
    }
}

pub struct OpenCodeDriver {
    // `Drop` releases this lease before waking the worker, guaranteeing that
    // final process teardown runs on the worker rather than the UI thread.
    server: Option<PooledServer>,
    session_id: String,
    commands: Sender<CommandMessage>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
    event_stream: Arc<StreamControl>,
    mode: RuntimeMode,
    computer_use: Option<super::support::HeadlessComputerUseRuntime>,
}

impl OpenCodeDriver {
    pub fn start(options: DriverStartOptions, events: DriverEventSender) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            model,
            reasoning_effort,
            service_tier: _,
            context_window: _,
            agent_preset,
            computer_use_enabled,
            provider_cursor,
        } = options;
        let resume_session_id = match provider_cursor {
            Some(ProviderResumeCursor::OpenCode { session_id }) => {
                (!session_id.is_empty()).then_some(session_id)
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume OpenCode from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };

        let computer_use = computer_use_enabled
            .then(|| {
                super::support::HeadlessComputerUseRuntime::start(
                    crate::model::ProviderKind::OpenCode,
                    events.clone(),
                )
            })
            .transpose()?;
        // The one-shot path handed Computer Use to OpenCode through the
        // environment; the resident server takes it exactly the same way.
        let environment = computer_use
            .as_ref()
            .map(|runtime| super::support::opencode_computer_use_environment(&runtime.config))
            .unwrap_or_default();
        // Computer Use bakes per-session configuration into the server's
        // environment, so it keeps a dedicated server. Every other session
        // shares the workspace's one resident server — OpenCode hosts many
        // sessions per process, and a second `opencode serve` in the same
        // workspace contends with the live one.
        let server = if computer_use.is_some() {
            PooledServer::dedicated(OpenCodeServer::start_with_env(&binary, &cwd, &environment)?)
        } else {
            crate::opencode_pool::acquire(&binary, &cwd)?
        };

        // `build` is the server's default agent; an explicit selection rides
        // the session create and every prompt, steer, and command body. Waku
        // changes the model per prompt because the server has no session-level
        // model setting, and the picker only ever asks before a task starts,
        // so one resolved string covers the session's lifetime.
        let agent = agent_preset
            .map(|preset| preset.trim().to_owned())
            .filter(|agent| !agent.is_empty())
            .unwrap_or_else(|| "build".to_owned());

        // Reuse the native session when resuming so the conversation, and the
        // cursor already persisted for it, stay the same.
        let session_id = match resume_session_id {
            Some(session_id) => session_id,
            None => {
                let created = server
                    .request("POST", "/session", Some(&json!({"agent": agent})))
                    .context("could not open an OpenCode session")?;
                created
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("OpenCode returned no session ID"))?
            }
        };

        // OpenCode's build agent allows ordinary shell commands by default.
        // Waku must therefore install the selected access policy on this
        // native session; listening for permission events alone cannot make
        // Supervised mode ask. Session-local rules are also safe on the shared
        // per-workspace server and replace a previous mode when resuming.
        server
            .request(
                "PATCH",
                &format!("/session/{}", encode_path_segment(&session_id)),
                Some(&json!({
                    "permission": opencode_permission_rules(mode)
                })),
            )
            .context("could not configure OpenCode session permissions")?;
        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::OpenCode {
                session_id: session_id.clone(),
            }),
        });

        let catalog = server
            .request_with_timeout("GET", "/command", None, Duration::from_secs(10))
            .ok()
            .map(|value| crate::slash_command_catalog::parse_opencode_commands(&value))
            .unwrap_or_default();
        let command_names = catalog
            .iter()
            .map(|command| command.name.clone())
            .collect::<HashSet<_>>();
        let _ = events.send(DriverEvent::AvailableCommands(
            catalog
                .into_iter()
                .map(|command| ReportedCommand {
                    name: command.name,
                    description: command.description,
                })
                .collect(),
        ));

        let usage_metadata = Arc::new(OpenCodeUsageMetadata::default());
        let previous_usage_path = format!(
            "/session/{}/message?limit=20",
            encode_path_segment(&session_id)
        );
        let previous_info = server
            .request("GET", &previous_usage_path, None)
            .ok()
            .and_then(|messages| latest_opencode_usage_info(&messages).cloned());
        if let Some(info) = previous_info.as_ref() {
            if let Some(model) = opencode_model_key(info) {
                *usage_metadata.last_model.lock() = Some(model);
            }
            if let Some(tokens) = opencode_context_tokens(info) {
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens: Some(tokens),
                    context_window: None,
                });
            }
        } else if let Some(model) = model.as_ref() {
            *usage_metadata.last_model.lock() = Some(model.clone());
        }

        // `/api/model` can be cold on the first server in a directory. Resolve
        // it off the driver-start path so a slow catalog never delays the
        // transcript or turns an otherwise healthy provider into a 0% meter.
        // The stream records the actual provider/model key in parallel; when
        // the catalog lands, publish the matching window as a separate merge.
        // The thread holds only the port: a handle would delay the pooled
        // server's teardown behind this request's timeout.
        let metadata_port = server.port;
        let metadata_events = events.clone();
        let background_usage_metadata = usage_metadata.clone();
        thread::Builder::new()
            .name("waku-opencode-usage-metadata".into())
            .spawn(move || {
                // `/api/model` answers with an empty catalog until the server
                // warms it up. The first session of a workspace starts a cold
                // server, so poll until models land or the budget runs out;
                // later sessions share an already-warm server.
                let started = std::time::Instant::now();
                let budget = Duration::from_secs(30);
                let response = loop {
                    let request = crate::opencode_session::request_json_on_port(
                        metadata_port,
                        "GET",
                        "/api/model",
                        None,
                        Duration::from_secs(30),
                    );
                    let landed = request.as_ref().is_ok_and(|response| {
                        response
                            .pointer("/data")
                            .and_then(Value::as_array)
                            .is_some_and(|data| !data.is_empty())
                    });
                    if landed || started.elapsed() >= budget {
                        break request;
                    }
                    thread::sleep(Duration::from_secs(2));
                };
                let Ok(response) = response else {
                    return;
                };
                let windows = opencode_model_context_windows(&response);
                *background_usage_metadata.model_context_windows.lock() = windows;
                let window = background_usage_metadata.current_context_window();
                if let Some(window) = window {
                    let _ = metadata_events.send(DriverEvent::UsageUpdated {
                        context_tokens: None,
                        context_window: Some(window),
                    });
                }
            })?;

        let auto_approve = matches!(mode, RuntimeMode::Auto | RuntimeMode::FullAccess);
        let (commands, command_rx) = unbounded();
        let turn_active = Arc::new(Mutex::new(false));
        let permissions = Arc::new(Mutex::new(OpenCodePermissionState::default()));
        let event_stream = Arc::new(StreamControl::default());

        // The reader holds only the port, never a server handle: the stream
        // closes exactly when the process exits, so a handle held here would
        // keep the pooled server from ever being killed.
        let stream_port = server.port;
        let stream_session = session_id.clone();
        let stream_events = events.clone();
        let stream_commands = commands.clone();
        let stream_turn = turn_active.clone();
        let stream_usage_metadata = usage_metadata;
        let stream_permissions = Arc::clone(&permissions);
        let stream_control = Arc::clone(&event_stream);
        thread::Builder::new()
            .name("waku-opencode-events".into())
            .spawn(move || {
                let mut state = OpenCodeStreamState {
                    usage_metadata: stream_usage_metadata,
                    permissions: stream_permissions,
                    ..OpenCodeStreamState::default()
                };
                // The server-wide stream, not a per-session one: the scoped
                // route exists only under `/api`, and the workspace server
                // may carry other sessions' traffic, so filter by session id.
                match open_event_stream(&Endpoint::local(stream_port), "/event", &stream_control) {
                    Ok(Some(stream)) => {
                        // The stream is live before this snapshot is read, so
                        // a request can neither fall between the two nor be
                        // lost when Waku reconnects after it was asked. Events
                        // that arrive during the snapshot wait in the socket;
                        // request-level de-duplication handles the overlap.
                        if let Ok(pending) = crate::opencode_session::request_json_on_port(
                            stream_port,
                            "GET",
                            "/permission",
                            None,
                            Duration::from_secs(5),
                        ) {
                            rehydrate_pending_permissions(
                                &pending,
                                &stream_session,
                                &stream_events,
                                &stream_commands,
                                &stream_turn,
                                auto_approve,
                                &state.permissions,
                            );
                        }
                        for line in BufReader::new(stream).lines().map_while(Result::ok) {
                            if stream_control.is_cancelled() {
                                break;
                            }
                            let Some(payload) = line.strip_prefix("data:") else {
                                continue;
                            };
                            let Ok(value) = serde_json::from_str::<Value>(payload.trim()) else {
                                continue;
                            };
                            // Another session's traffic must not reach this
                            // task's transcript.
                            let session = value
                                .pointer("/properties/sessionID")
                                .and_then(Value::as_str);
                            if session.is_some_and(|session| session != stream_session) {
                                continue;
                            }
                            handle_event(
                                &value,
                                &stream_events,
                                &stream_commands,
                                &stream_turn,
                                auto_approve,
                                &mut state,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if !stream_control.is_cancelled() {
                            let _ = stream_events.send(DriverEvent::Error(tr!(
                                "errors.read_provider_event_stream",
                                provider = "OpenCode",
                                error = error
                            )));
                        }
                    }
                }
                stream_control.clear();
                if !stream_control.is_cancelled() {
                    let _ = stream_events.send(DriverEvent::ProcessExited);
                }
            })?;

        let worker_server = server.clone();
        let worker_session = session_id.clone();
        let variant = reasoning_effort;
        let worker_events = events;
        let worker_turn = turn_active;
        let worker_commands = commands.clone();
        thread::Builder::new()
            .name("waku-opencode-driver".into())
            .spawn(move || {
                let mut generation = 0_u64;
                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(text) => {
                            generation = generation.wrapping_add(1);
                            *worker_turn.lock() = true;
                            let _ = worker_events.send(DriverEvent::TurnStarted);
                            if let Some(body) = native_command_body(
                                &text,
                                &command_names,
                                model.as_deref(),
                                variant.as_deref(),
                                agent.as_str(),
                            ) {
                                if let Err(error) = start_native_command(
                                    worker_server.port,
                                    &worker_session,
                                    body,
                                    worker_commands.clone(),
                                    generation,
                                    None,
                                ) {
                                    reject_prompt(error, &worker_events, &worker_turn);
                                }
                                continue;
                            }
                            // `prompt_async` acknowledges as soon as the prompt
                            // is accepted; completion arrives as `session.idle`
                            // on the event stream. The blocking message route
                            // holds its response for the whole turn, which no
                            // sane read timeout survives — a turn longer than
                            // the HTTP timeout would be falsely failed.
                            let path = format!(
                                "/session/{}/prompt_async",
                                encode_path_segment(&worker_session)
                            );
                            let body = prompt_body(
                                &text,
                                model.as_deref(),
                                variant.as_deref(),
                                agent.as_str(),
                            );
                            if let Err(error) = worker_server.request("POST", &path, Some(&body)) {
                                reject_prompt(error, &worker_events, &worker_turn);
                            }
                        }
                        CommandMessage::Steer(text) => {
                            // A prompt posted into a busy session is a steer:
                            // the server folds it into the running turn and one
                            // `session.idle` still settles everything —
                            // OpenCode's own UI calls this "queued", but it is
                            // the live turn absorbing the message, not a
                            // follow-up turn. `prompt_async` acknowledges as
                            // soon as the prompt is accepted, unlike the
                            // message route, which blocks until the merged turn
                            // ends — which is what makes it the steer vehicle.
                            if !*worker_turn.lock() {
                                let _ = worker_events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: tr!(
                                        "errors.provider_no_active_turn",
                                        provider = "OpenCode"
                                    ),
                                });
                                continue;
                            }
                            if let Some(body) = native_command_body(
                                &text,
                                &command_names,
                                model.as_deref(),
                                variant.as_deref(),
                                agent.as_str(),
                            ) {
                                if let Err(error) = start_native_command(
                                    worker_server.port,
                                    &worker_session,
                                    body,
                                    worker_commands.clone(),
                                    generation,
                                    Some(text.clone()),
                                ) {
                                    let _ = worker_events.send(DriverEvent::SteerRejected {
                                        message: text,
                                        reason: error.to_string(),
                                    });
                                }
                                continue;
                            }
                            let path = format!(
                                "/session/{}/prompt_async",
                                encode_path_segment(&worker_session)
                            );
                            let body = prompt_body(
                                &text,
                                model.as_deref(),
                                variant.as_deref(),
                                agent.as_str(),
                            );
                            match worker_server.request("POST", &path, Some(&body)) {
                                Ok(_) => {
                                    let _ = worker_events
                                        .send(DriverEvent::SteerAccepted { message: text });
                                }
                                Err(error) => {
                                    let _ = worker_events.send(DriverEvent::SteerRejected {
                                        message: text,
                                        reason: tr!(
                                            "errors.provider_rejected_steer",
                                            provider = "OpenCode",
                                            error = error
                                        ),
                                    });
                                }
                            }
                        }
                        CommandMessage::NativeCommandFinished {
                            generation: completed,
                            steer,
                            result,
                        } => {
                            if let Some(message) = steer {
                                let event = match result {
                                    Ok(()) => DriverEvent::SteerAccepted { message },
                                    Err(reason) => DriverEvent::SteerRejected { message, reason },
                                };
                                let _ = worker_events.send(event);
                            } else if completed == generation
                                && *worker_turn.lock()
                                && let Err(error) = result
                            {
                                // A late HTTP failure must not fail a newer
                                // turn or one the event stream already settled.
                                reject_prompt(error, &worker_events, &worker_turn);
                            }
                        }
                        CommandMessage::Cancel => {
                            let path =
                                format!("/session/{}/abort", encode_path_segment(&worker_session));
                            if let Err(error) = worker_server.request("POST", &path, None) {
                                let _ = worker_events.send(DriverEvent::Error(tr!(
                                    "errors.stop_provider",
                                    provider = "OpenCode",
                                    error = error
                                )));
                            }
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let path =
                                format!("/permission/{}/reply", encode_path_segment(&request_id));
                            if let Err(error) = worker_server.request(
                                "POST",
                                &path,
                                Some(&json!({"reply": option_id})),
                            ) {
                                let _ = worker_events.send(DriverEvent::Error(tr!(
                                    "errors.answer_provider_permission",
                                    provider = "OpenCode",
                                    error = error
                                )));
                            }
                        }
                        CommandMessage::RespondUserInput {
                            request_id,
                            answers,
                        } => {
                            let path =
                                format!("/question/{}/reply", encode_path_segment(&request_id));
                            let answers = answers
                                .into_iter()
                                .map(|answer| answer.answers)
                                .collect::<Vec<_>>();
                            if let Err(error) = worker_server.request(
                                "POST",
                                &path,
                                Some(&json!({"answers": answers})),
                            ) {
                                let _ = worker_events.send(DriverEvent::Error(tr!(
                                    "errors.answer_provider_question",
                                    provider = "OpenCode",
                                    error = error
                                )));
                            }
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })
            .inspect_err(|_| {
                event_stream.cancel();
            })?;

        Ok(Self {
            server: Some(server),
            session_id,
            commands,
            permissions,
            event_stream,
            mode,
            computer_use,
        })
    }
}

impl DriverControl for OpenCodeDriver {
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
        for (request_id, option_id) in
            permission_responses(&self.permissions, &request_id, &option_id)
        {
            let _ = self.commands.send(CommandMessage::Respond {
                request_id,
                option_id,
            });
        }
    }

    fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        let _ = self.commands.send(CommandMessage::RespondUserInput {
            request_id,
            answers,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // The model rides on each prompt, but access is installed when the
        // driver starts, so changing it restarts the driver.
        options.mode == self.mode
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        if turns == 0 {
            return Ok(None);
        }
        self.fork(turns).map(Some)
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        let server = self
            .server
            .as_deref()
            .ok_or_else(|| anyhow!("OpenCode driver is shutting down"))?;
        fork_session_removing_turns_on_server(server, &self.session_id, turns_to_remove)
    }
}

impl Drop for OpenCodeDriver {
    fn drop(&mut self) {
        self.cancel_computer_use();
        self.event_stream.cancel();
        // The worker owns the other server lease. Release the UI-owned lease
        // first, then wake the worker so any final terminate/wait happens there.
        drop(self.server.take());
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

/// Reads the human-readable text out of a `session.error` event.
///
/// Every OpenCode error is wrapped as `{name, data: {message}}` — verified
/// against the live 1.18 `/doc` schema, where all eight `session.error`
/// members (`ProviderAuthError`, `APIError`, `ContextOverflowError`,
/// `ContentFilterError`, `MessageAbortedError`, `MessageOutputLengthError`,
/// `StructuredOutputError`, `UnknownError`) share that shape. Reading
/// `/error/message` therefore never matched, and every failure — an expired
/// login, a billing stop, a context overflow — surfaced as the same bare
/// "OpenCode reported an error" with the real cause discarded.
fn session_error_message(properties: &Value) -> String {
    let error = properties.get("error");
    error
        .and_then(|error| {
            error
                .pointer("/data/message")
                .or_else(|| error.get("message"))
                .or_else(|| properties.get("message"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
        // `MessageOutputLengthError` carries an empty `data`, so its name is
        // the only thing that says what went wrong.
        .or_else(|| {
            error
                .and_then(|error| error.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| tr!("errors.provider_reported_error", provider = "OpenCode"))
}

#[derive(Default)]
struct OpenCodeStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
    reasoning_parts: HashSet<String>,
    usage_metadata: Arc<OpenCodeUsageMetadata>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
}

#[derive(Default)]
struct OpenCodeUsageMetadata {
    model_context_windows: Mutex<HashMap<String, u64>>,
    last_model: Mutex<Option<String>>,
}

impl OpenCodeUsageMetadata {
    fn current_context_window(&self) -> Option<u64> {
        let model = self.last_model.lock().clone()?;
        self.model_context_windows.lock().get(&model).copied()
    }
}

fn opencode_model_context_windows(response: &Value) -> HashMap<String, u64> {
    response
        .pointer("/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let provider = model.get("providerID").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            let window = model
                .pointer("/limit/context")
                .and_then(Value::as_u64)
                .filter(|window| *window > 0)?;
            Some((format!("{provider}/{id}"), window))
        })
        .collect()
}

fn opencode_context_tokens(info: &Value) -> Option<u64> {
    let tokens = info.get("tokens")?;
    tokens
        .get("total")
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0)
        .or_else(|| {
            let total = [
                tokens.get("input"),
                tokens.get("output"),
                tokens.pointer("/cache/read"),
                tokens.pointer("/cache/write"),
            ]
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .fold(0_u64, u64::saturating_add);
            (total > 0).then_some(total)
        })
}

fn opencode_model_key(info: &Value) -> Option<String> {
    info.get("providerID")
        .and_then(Value::as_str)
        .zip(info.get("modelID").and_then(Value::as_str))
        .map(|(provider, model)| format!("{provider}/{model}"))
}

fn opencode_context_usage(
    info: &Value,
    model_context_windows: &HashMap<String, u64>,
) -> Option<(Option<u64>, Option<u64>)> {
    if info.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tokens = opencode_context_tokens(info);
    let window = opencode_model_key(info)
        .as_ref()
        .and_then(|model| model_context_windows.get(model).copied());
    (tokens.is_some() || window.is_some()).then_some((tokens, window))
}

fn latest_opencode_usage_info(messages: &Value) -> Option<&Value> {
    let mut assistant = None;
    for message in messages.as_array()?.iter().rev() {
        let Some(info) = message
            .get("info")
            .filter(|info| info.get("role").and_then(Value::as_str) == Some("assistant"))
        else {
            continue;
        };
        if opencode_context_tokens(info).is_some() {
            return Some(info);
        }
        assistant.get_or_insert(info);
    }
    assistant
}

fn handle_event(
    value: &Value,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    auto_approve: bool,
    state: &mut OpenCodeStreamState,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let properties = value.get("properties").unwrap_or(&Value::Null);

    match kind {
        "message.part.delta" => {
            let Some(delta) = properties.get("delta").and_then(Value::as_str) else {
                return;
            };
            if delta.is_empty() {
                return;
            }
            let part_id = properties.get("partID").and_then(Value::as_str);
            let native_reasoning_part =
                part_id.is_some_and(|part_id| state.reasoning_parts.contains(part_id));
            match properties.get("field").and_then(Value::as_str) {
                // OpenCode streams a reasoning part's own `text` property as
                // `field: "text"`. The preceding part update is therefore the
                // authoritative distinction between answer and thought text.
                Some("text") if native_reasoning_part => {
                    let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
                }
                Some("text") => {
                    let _ = events.send(DriverEvent::TextDelta(delta.to_owned()));
                }
                Some("reasoning" | "thinking") => {
                    if let Some(part_id) = part_id {
                        state.reasoning_parts.insert(part_id.to_owned());
                    }
                    let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
                }
                _ => {}
            }
        }
        "message.part.updated" => {
            let part = properties.get("part").unwrap_or(&Value::Null);
            let part_type = part.get("type").and_then(Value::as_str);
            if let Some(part_id) = part.get("id").and_then(Value::as_str) {
                if matches!(part_type, Some("reasoning" | "thinking")) {
                    state.reasoning_parts.insert(part_id.to_owned());
                } else {
                    state.reasoning_parts.remove(part_id);
                }
            }
            if part_type == Some("tool") {
                tool_activity(part, events, state);
            }
        }
        "message.updated" => {
            if let Some(info) = properties.get("info") {
                if let Some(model) = opencode_model_key(info) {
                    *state.usage_metadata.last_model.lock() = Some(model);
                }
                let usage = {
                    let windows = state.usage_metadata.model_context_windows.lock();
                    opencode_context_usage(info, &windows)
                };
                if let Some((context_tokens, context_window)) = usage {
                    let _ = events.send(DriverEvent::UsageUpdated {
                        context_tokens,
                        context_window,
                    });
                }
            }
        }
        "session.idle" => {
            state.reasoning_parts.clear();
            let mut permissions = state.permissions.lock();
            permissions.pending.clear();
            permissions.responding.clear();
            drop(permissions);
            if std::mem::take(&mut *turn_active.lock()) {
                let _ = events.send(DriverEvent::TurnFinished {
                    success: true,
                    summary: None,
                });
            }
        }
        "session.error" => {
            let _ = events.send(DriverEvent::Error(session_error_message(properties)));
        }
        "session.updated" => {
            let title = properties
                .pointer("/info/title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty() && !title.starts_with("New session - "));
            if let Some(title) = title {
                let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title.to_owned())));
            }
        }
        "permission.replied" => {
            if let Some(request_id) = properties
                .get("requestID")
                .or_else(|| properties.get("id"))
                .and_then(Value::as_str)
            {
                let mut permissions = state.permissions.lock();
                permissions.pending.remove(request_id);
                permissions.responding.remove(request_id);
            }
        }
        _ if kind.starts_with("permission.") => {
            request_permission(
                properties,
                events,
                commands,
                auto_approve,
                &state.permissions,
            );
        }
        "question.asked" => request_user_input(properties, events),
        "question.replied" | "question.rejected" => {}
        // `session.created`, `session.diff`, and the plugin/catalog/reference
        // chatter are not transcript content.
        _ => {}
    }
}

fn rehydrate_pending_permissions(
    response: &Value,
    session_id: &str,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    auto_approve: bool,
    permissions: &Mutex<OpenCodePermissionState>,
) {
    let requests = response
        .as_array()
        .into_iter()
        .flatten()
        .filter(|request| {
            request.get("sessionID").and_then(Value::as_str) == Some(session_id)
                && request.get("id").and_then(Value::as_str).is_some()
        })
        .collect::<Vec<_>>();
    if requests.is_empty() {
        return;
    }

    // A pending native permission proves that the resumed provider turn is
    // still live. Restore this driver-local edge so the eventual
    // `session.idle` settles Waku's persisted running turn exactly once.
    *turn_active.lock() = true;
    for request in requests {
        request_permission(request, events, commands, auto_approve, permissions);
    }
}

fn request_user_input(properties: &Value, events: &impl DriverEventSink) {
    let Some(request_id) = properties.get("id").and_then(Value::as_str) else {
        return;
    };
    let questions = properties
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, question)| {
            let text = question.get("question").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            let header = question
                .get("header")
                .and_then(Value::as_str)
                .filter(|header| !header.trim().is_empty())
                .unwrap_or("Question");
            let slug = header
                .trim()
                .to_ascii_lowercase()
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                        character
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            let slug = slug.trim_matches('-');
            let id = if slug.is_empty() {
                format!("question-{index}")
            } else {
                format!("question-{index}-{slug}")
            };
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let label = option.get("label").and_then(Value::as_str)?.trim();
                    (!label.is_empty()).then(|| UserInputOption {
                        label: label.to_owned(),
                        description: option
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|description| !description.is_empty())
                            .map(str::to_owned),
                    })
                })
                .collect();
            Some(UserInputQuestion {
                id,
                header: header.to_owned(),
                question: text.to_owned(),
                options,
                multi_select: question
                    .get("multiple")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    if !questions.is_empty() {
        let _ = events.send(DriverEvent::UserInputRequested {
            request_id: request_id.to_owned(),
            questions,
        });
    }
}

fn request_permission(
    properties: &Value,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    auto_approve: bool,
    permissions: &Mutex<OpenCodePermissionState>,
) {
    // The request is either the properties themselves or nested under a key,
    // and it is identified by its `per`-prefixed ID.
    let request = ["permission", "request", "info"]
        .iter()
        .find_map(|key| properties.get(*key))
        .filter(|value| value.get("id").is_some())
        .unwrap_or(properties);
    let Some(request_id) = request.get("id").and_then(Value::as_str) else {
        return;
    };
    let permission_request = OpenCodePermissionRequest {
        permission: request
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        patterns: request
            .get("patterns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        always: request
            .get("always")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    };

    // OpenCode's `always` response updates a process-wide approval cache. A
    // pooled Full Access task must never suppress prompts in a Supervised task,
    // so Waku sends only one-shot provider replies and retains durable choices
    // in this driver's session-local state.
    let mut permission_state = permissions.lock();
    if permission_state.pending.contains_key(request_id)
        || permission_state.responding.contains(request_id)
    {
        return;
    }
    if auto_approve || permission_state.is_approved(&permission_request) {
        permission_state.responding.insert(request_id.to_owned());
        drop(permission_state);
        let _ = commands.send(CommandMessage::Respond {
            request_id: request_id.to_owned(),
            option_id: "once".into(),
        });
        return;
    }

    permission_state
        .pending
        .insert(request_id.to_owned(), permission_request.clone());
    drop(permission_state);

    let permission = if permission_request.permission.is_empty() {
        tr!("permission.run_a_tool_lower")
    } else {
        permission_request.permission.clone()
    };
    let patterns = (!permission_request.patterns.is_empty())
        .then(|| permission_request.patterns.join(", "))
        .filter(|patterns| !patterns.is_empty());
    let _ = events.send(DriverEvent::Permission {
        request_id: request_id.to_owned(),
        title: patterns.clone().unwrap_or_else(|| {
            tr!(
                "permission.allow_named_permission",
                permission = permission.as_str()
            )
        }),
        detail: match patterns {
            Some(_) => tr!(
                "permission.agent_asks_for_named_permission",
                permission = permission.as_str()
            ),
            None => tr!("permission.agent_asks_for_permission"),
        },
        options: vec![
            PermissionOption {
                id: "once".into(),
                label: tr!("permission.allow_once"),
                allow: true,
            },
            PermissionOption {
                id: "always".into(),
                label: tr!("permission.always_allow"),
                allow: true,
            },
            PermissionOption {
                id: "reject".into(),
                label: tr!("common.deny"),
                allow: false,
            },
        ],
    });
}

fn tool_activity(part: &Value, events: &impl DriverEventSink, state: &mut OpenCodeStreamState) {
    let wire_title = part
        .get("tool")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("activity.tool"));
    let id = part
        .get("callID")
        .or_else(|| part.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let arguments = part.pointer("/state/input");
    let complete = matches!(
        part.pointer("/state/status").and_then(Value::as_str),
        Some("completed" | "error")
    );
    let stored = id.as_ref().and_then(|id| {
        if complete {
            state.tools.remove(id)
        } else {
            state.tools.get(id).cloned()
        }
    });
    let kind = stored
        .as_ref()
        .map(|(kind, _)| *kind)
        .unwrap_or_else(|| super::support::classify_tool(&wire_title));
    let title = activity::input_title(arguments)
        .or_else(|| stored.map(|(_, title)| title))
        .unwrap_or(wire_title);
    if !complete && let Some(id) = id.as_ref() {
        state.tools.insert(id.clone(), (kind, title.clone()));
    }
    let failed = part.pointer("/state/status").and_then(Value::as_str) == Some("error")
        || part
            .pointer("/state/error")
            .is_some_and(|error| !error.is_null());
    let output = part
        .pointer("/state/error")
        .filter(|value| !value.is_null())
        .or_else(|| {
            part.pointer("/state/output")
                .filter(|value| !value.is_null())
        });
    let item = activity::tool_activity(
        id,
        kind,
        title,
        arguments,
        output,
        part.get("state"),
        failed,
        complete,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_commands_preserve_provider_arguments_and_model_options() {
        let commands = ["init".to_owned(), "review".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            native_command_body(
                "/review main\nfocus on tests",
                &commands,
                Some("openai/gpt-5"),
                Some("high"),
                "build"
            ),
            Some(json!({
                "command": "review", "arguments": "main\nfocus on tests", "model": "openai/gpt-5", "variant": "high", "agent": "build"
            }))
        );
        assert_eq!(
            native_command_body("/init", &commands, None, Some(" "), "build"),
            Some(json!({
                "command": "init", "arguments": "", "agent": "build"
            }))
        );
        for text in [
            "ordinary prompt",
            "/unknown args",
            "/reviewer",
            "Discuss /review",
        ] {
            assert!(native_command_body(text, &commands, None, None, "build").is_none());
        }
    }

    #[test]
    fn native_command_http_wait_keeps_control_delivery_unblocked() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (received, requests) = unbounded();
        let (finish, finish_rx) = unbounded();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let route = line.trim().to_owned();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            received
                .send((route, serde_json::from_slice::<Value>(&body).unwrap()))
                .unwrap();
            finish_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        });
        let (commands, command_rx) = unbounded();
        let body = json!({"command": "review", "arguments": "main"});
        start_native_command(port, "ses_test", body.clone(), commands.clone(), 7, None).unwrap();
        let (route, sent) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(route, "POST /session/ses_test/command HTTP/1.1");
        assert_eq!(sent, body);
        commands.send(CommandMessage::Cancel).unwrap();
        assert!(matches!(
            command_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            CommandMessage::Cancel
        ));
        finish.send(()).unwrap();
        assert!(matches!(
            command_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            CommandMessage::NativeCommandFinished {
                generation: 7,
                steer: None,
                result: Ok(())
            }
        ));
        server.join().unwrap();
    }

    fn harness() -> (
        Sender<DriverEvent>,
        crossbeam_channel::Receiver<DriverEvent>,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        Mutex<bool>,
        OpenCodeStreamState,
    ) {
        let (events, event_rx) = unbounded();
        let (commands, command_rx) = unbounded();
        (
            events,
            event_rx,
            commands,
            command_rx,
            Mutex::new(true),
            OpenCodeStreamState::default(),
        )
    }

    #[test]
    fn prompts_select_the_agent_and_model_for_the_turn() {
        assert_eq!(
            prompt_body(
                "Inspect the failure",
                Some("opencode-go/deepseek-v4-flash"),
                None,
                "build",
            ),
            json!({
                "agent": "build",
                "model": {
                    "providerID": "opencode-go",
                    "modelID": "deepseek-v4-flash",
                },
                "parts": [{"type": "text", "text": "Inspect the failure"}],
            })
        );
    }

    /// Reasoning effort is OpenCode's per-model `variant`, and it sits beside
    /// `model` rather than inside it. The driver used to discard the chosen
    /// effort entirely, so every turn ran at the model's default.
    #[test]
    fn prompts_carry_the_selected_reasoning_effort_as_a_variant() {
        let body = prompt_body(
            "Inspect the failure",
            Some("opencode-go/deepseek-v4-flash"),
            Some("max"),
            "build",
        );
        assert_eq!(body["variant"], json!("max"));
        assert_eq!(body["model"]["modelID"], json!("deepseek-v4-flash"));
    }

    /// A model with no effort ladder must not send an empty variant, which the
    /// server would reject as an unknown one.
    #[test]
    fn prompts_omit_a_blank_variant() {
        let body = prompt_body("hi", Some("opencode/big-pickle"), Some("   "), "build");
        assert!(body.get("variant").is_none(), "{body}");
    }

    #[test]
    fn access_modes_install_session_local_opencode_permissions() {
        let rule = |permission: &str, action: &str| {
            json!({
                "permission": permission,
                "pattern": "*",
                "action": action,
            })
        };

        assert_eq!(
            opencode_permission_rules(RuntimeMode::Ask),
            json!([rule("bash", "ask"), rule("edit", "ask")])
        );
        assert_eq!(
            opencode_permission_rules(RuntimeMode::AutoAcceptEdits),
            json!([rule("bash", "ask"), rule("edit", "allow")])
        );
        assert_eq!(
            opencode_permission_rules(RuntimeMode::FullAccess),
            json!([rule("*", "allow")])
        );
    }

    #[test]
    fn question_events_preserve_multiple_selection_and_option_copy() {
        let (events, event_rx) = unbounded();
        request_user_input(
            &json!({
                "id": "question-request",
                "sessionID": "session-1",
                "questions": [{
                    "header": "Files",
                    "question": "Which files should change?",
                    "multiple": true,
                    "options": [{
                        "label": "Source",
                        "description": "Update implementation files"
                    }]
                }]
            }),
            &events,
        );

        let DriverEvent::UserInputRequested {
            request_id,
            questions,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("OpenCode question.asked must use the structured question event");
        };
        assert_eq!(request_id, "question-request");
        assert_eq!(questions[0].id, "question-0-files");
        assert!(questions[0].multi_select);
        assert_eq!(questions[0].options[0].label, "Source");
    }

    /// Drives a real `opencode serve` through the actual driver. Ignored by
    /// default: needs the CLI installed, credentials, and the network. Run with
    /// `cargo test --bin waku opencode_session_against_a_real_server -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn opencode_session_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the server should start and open a session");

        let connected = event_rx
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("the server should report its session");
        let source_session_id = match connected {
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::OpenCode { session_id }),
            } => session_id,
            event => panic!("expected an OpenCode cursor, got {event:?}"),
        };

        driver.prompt("Reply with exactly: OK. Do not use any tools.".into());
        let mut text = String::new();
        let mut finished = None;
        let mut context_tokens = None;
        let mut context_window = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                continue;
            };
            match event {
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
                DriverEvent::UsageUpdated {
                    context_tokens: tokens,
                    context_window: window,
                } => {
                    context_tokens = tokens.or(context_tokens);
                    context_window = window.or(context_window);
                }
                DriverEvent::TurnFinished { success, .. } => {
                    finished = Some(success);
                }
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
                _ => {}
            }
            if finished.is_some()
                && context_tokens.is_some_and(|tokens| tokens > 0)
                && context_window.is_some_and(|window| window > 0)
            {
                break;
            }
        }
        assert_eq!(finished, Some(true), "the turn should settle successfully");
        assert!(
            text.contains("OK"),
            "expected the reply to stream through, got {text:?}"
        );
        assert!(context_tokens.is_some_and(|tokens| tokens > 0));
        assert!(context_window.is_some_and(|window| window > 0));

        let ProviderResumeCursor::OpenCode {
            session_id: fork_session_id,
        } = driver
            .fork(1)
            .expect("the resident server should fork away the completed turn")
        else {
            panic!("expected an OpenCode fork cursor");
        };
        assert_ne!(fork_session_id, source_session_id);
    }

    /// Proves steering through the actual driver: the message injected while
    /// the bash tool sleeps lands inside the same turn — one SteerAccepted,
    /// one TurnFinished, and a reply that honors both instructions. Ignored by
    /// default: needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn opencode_steering_folds_a_mid_turn_message_into_the_running_turn() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the server should start and open a session");

        driver.prompt(
            "Use the bash tool to run exactly `sleep 6` (nothing else). \
             After the command completes, reply with exactly: FIRST DONE"
                .into(),
        );

        let mut text = String::new();
        let mut steered = false;
        let mut steer_accepted = false;
        let mut turns_finished = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
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
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
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
    fn streams_text_and_correlated_tools_and_settles_on_idle() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // Payloads copied from a live `opencode serve` event stream.
        let wire = [
            json!({"type":"message.part.delta","properties":{"sessionID":"ses_1","messageID":"msg_1","partID":"prt_1","field":"text","delta":"OK"}}),
            json!({"type":"message.part.delta","properties":{"field":"reasoning","delta":"thinking"}}),
            json!({"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"read","callID":"call_1","state":{"status":"running","input":{"filePath":"a.txt"}}}}}),
            json!({"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"read","callID":"call_1","state":{"status":"completed","output":"contents"}}}}),
            // Not transcript content.
            json!({"type":"session.diff","properties":{"diff":[]}}),
            json!({"type":"message.updated","properties":{"info":{"role":"assistant"}}}),
            json!({"type":"session.idle","properties":{"sessionID":"ses_1"}}),
        ];
        for event in wire {
            handle_event(&event, &events, &commands, &turn, true, &mut state);
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::TextDelta(text) if text == "OK"));
        assert!(matches!(&seen[1], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::FileRead
                    && !item.complete
                    && item.display_target.as_deref() == Some("a.txt")));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
                if item.complete && item.title == "read"));
        assert!(matches!(
            &seen[4],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 5, "non-transcript events leaked");
        assert!(!*turn.lock(), "the turn should be settled exactly once");
    }

    /// The exact `session.error` payload a live opencode 1.18 server emits when
    /// the account is out of credit. Every error is `{name, data:{message}}`,
    /// so reading `/error/message` reported "OpenCode reported an error" and
    /// threw the actionable billing URL away.
    #[test]
    fn session_errors_surface_the_provider_message_not_a_generic_sentence() {
        let properties = json!({
            "sessionID": "ses_1",
            "error": {
                "name": "APIError",
                "data": {
                    "message": "Insufficient balance. Manage your billing here: https://opencode.ai/workspace/wrk_1/billing",
                    "statusCode": 401,
                    "isRetryable": false
                }
            }
        });
        assert_eq!(
            session_error_message(&properties),
            "Insufficient balance. Manage your billing here: https://opencode.ai/workspace/wrk_1/billing"
        );
    }

    /// `MessageOutputLengthError` is the one member with an empty `data`, so
    /// its name is the only thing that says what happened.
    #[test]
    fn session_errors_without_a_message_fall_back_to_the_error_name() {
        let properties = json!({
            "sessionID": "ses_1",
            "error": {"name": "MessageOutputLengthError", "data": {}}
        });
        assert_eq!(
            session_error_message(&properties),
            "MessageOutputLengthError"
        );
    }

    /// A flatter shape still works, so a future server rename cannot silently
    /// regress this back to the generic sentence.
    #[test]
    fn session_errors_accept_a_flat_message() {
        let properties = json!({"error": {"message": "boom"}});
        assert_eq!(session_error_message(&properties), "boom");
    }

    #[test]
    fn classifies_text_deltas_by_their_native_part_type() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // DeepSeek V4 Flash is stored by OpenCode as a reasoning part, but the
        // part's content still streams through the generic `text` field.
        let wire = [
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_reason","type":"reasoning","text":""}}}),
            json!({"type":"message.part.delta","properties":{"partID":"prt_reason","field":"text","delta":"thinking"}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_answer","type":"text","text":""}}}),
            json!({"type":"message.part.delta","properties":{"partID":"prt_answer","field":"text","delta":"answer"}}),
            json!({"type":"message.part.delta","properties":{"partID":"prt_unknown","field":"text","delta":" fallback"}}),
            json!({"type":"session.idle","properties":{"sessionID":"ses_1"}}),
        ];
        for event in wire {
            handle_event(&event, &events, &commands, &turn, true, &mut state);
        }

        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[1], DriverEvent::TextDelta(text) if text == "answer"));
        assert!(matches!(&seen[2], DriverEvent::TextDelta(text) if text == " fallback"));
        assert!(matches!(
            &seen[3],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 4);
        assert!(
            state.reasoning_parts.is_empty(),
            "settled turns should release their part classification state"
        );
    }

    #[test]
    fn assistant_updates_feed_opencode_context_usage() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        state
            .usage_metadata
            .model_context_windows
            .lock()
            .insert("opencode/deepseek-v4-flash-free".into(), 200_000);

        handle_event(
            &json!({
                "type": "message.updated",
                "properties": {
                    "sessionID": "ses_1",
                    "info": {
                        "role": "assistant",
                        "providerID": "opencode",
                        "modelID": "deepseek-v4-flash-free",
                        "tokens": {
                            "total": 0,
                            "input": 13_399,
                            "output": 10,
                            "reasoning": 0,
                            "cache": {"read": 1792, "write": 0}
                        }
                    }
                }
            }),
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UsageUpdated {
                context_tokens: Some(15_201),
                context_window: Some(200_000)
            }
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn model_metadata_and_last_message_restore_opencode_usage() {
        let models = json!({
            "data": [{
                "providerID": "opencode-go",
                "id": "deepseek-v4-flash",
                "limit": {"context": 1_000_000, "output": 384_000}
            }]
        });
        let windows = opencode_model_context_windows(&models);
        let messages = json!([
            {"info": {"role": "user"}},
            {"info": {
                "role": "assistant",
                "providerID": "opencode-go",
                "modelID": "deepseek-v4-flash",
                "tokens": {
                    "total": 15_467,
                    "input": 15_450,
                    "output": 17,
                    "reasoning": 0,
                    "cache": {"read": 0, "write": 0}
                }
            }}
        ]);

        let latest = latest_opencode_usage_info(&messages).expect("latest assistant usage");
        assert_eq!(
            opencode_context_usage(latest, &windows),
            Some((Some(15_467), Some(1_000_000)))
        );
    }

    #[test]
    fn generated_session_titles_replace_the_local_fallback() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // OpenCode emits this placeholder before its title-generation model call.
        handle_event(
            &json!({
                "type": "session.updated",
                "properties": {
                    "sessionID": "ses_1",
                    "info": {"title": "New session - 2026-08-08T18:33:35.122Z"}
                }
            }),
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        assert!(event_rx.try_recv().is_err());

        // Exact envelope captured from a live isolated `opencode serve` stream.
        handle_event(
            &json!({
                "type": "session.updated",
                "properties": {
                    "sessionID": "ses_1",
                    "info": {"title": "Generated provider title"}
                }
            }),
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Generated provider title"
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn pending_permissions_rehydrate_only_their_session_without_duplicate_prompts() {
        let (events, event_rx, commands, command_rx, turn, state) = harness();
        *turn.lock() = false;
        let response = json!([
            {
                "id": "per_current",
                "sessionID": "ses_current",
                "permission": "bash",
                "patterns": ["ps aux", "sort -rk3", "head -25"],
                "metadata": {},
                "always": ["ps *", "sort *", "head *"]
            },
            {
                "id": "per_other",
                "sessionID": "ses_other",
                "permission": "bash",
                "patterns": ["cargo test"],
                "metadata": {},
                "always": ["cargo *"]
            }
        ]);

        rehydrate_pending_permissions(
            &response,
            "ses_current",
            &events,
            &commands,
            &turn,
            false,
            &state.permissions,
        );
        assert!(
            *turn.lock(),
            "a restored request proves the native turn is still active"
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Permission { request_id, .. } if request_id == "per_current"
        ));
        assert!(event_rx.try_recv().is_err());
        assert!(command_rx.try_recv().is_err());

        // The live SSE event can already be buffered while the snapshot is
        // read. Replaying the same request must not replace the approval card
        // or send a second provider response.
        rehydrate_pending_permissions(
            &response,
            "ses_current",
            &events,
            &commands,
            &turn,
            false,
            &state.permissions,
        );
        assert!(event_rx.try_recv().is_err());
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn auto_approval_deduplicates_snapshot_and_stream_overlap() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        let response = json!([{
            "id": "per_auto",
            "sessionID": "ses_current",
            "permission": "bash",
            "patterns": ["cargo test"],
            "always": ["cargo *"]
        }]);

        for _ in 0..2 {
            rehydrate_pending_permissions(
                &response,
                "ses_current",
                &events,
                &commands,
                &turn,
                true,
                &state.permissions,
            );
        }

        assert!(matches!(
            command_rx.try_recv().unwrap(),
            CommandMessage::Respond { request_id, option_id }
                if request_id == "per_auto" && option_id == "once"
        ));
        assert!(command_rx.try_recv().is_err());
        assert!(event_rx.try_recv().is_err());

        handle_event(
            &json!({
                "type": "permission.replied",
                "properties": {
                    "sessionID": "ses_current",
                    "requestID": "per_auto",
                    "reply": "once"
                }
            }),
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        assert!(state.permissions.lock().responding.is_empty());
    }

    #[test]
    fn permission_approvals_stay_driver_local() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        // Shape from the server's OpenAPI PermissionRequest schema.
        let permission = json!({
            "type": "permission.requested",
            "properties": {
                "id": "per_abc",
                "sessionID": "ses_1",
                "permission": "bash",
                "patterns": ["rm -rf *"],
                "metadata": {},
                "always": ["rm -rf *"]
            }
        });

        handle_event(&permission, &events, &commands, &turn, false, &mut state);
        let DriverEvent::Permission {
            request_id,
            options,
            title,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "per_abc");
        assert_eq!(title, "rm -rf *");
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["once", "always", "reject"]
        );
        assert!(command_rx.try_recv().is_err());

        assert_eq!(
            permission_responses(&state.permissions, "per_abc", "always"),
            [("per_abc".into(), "once".into())],
            "provider-wide durable approval must be translated to one-shot"
        );
        let repeated = json!({
            "type": "permission.requested",
            "properties": {
                "id": "per_def",
                "sessionID": "ses_1",
                "permission": "bash",
                "patterns": ["rm -rf /tmp/waku-cache"],
                "metadata": {},
                "always": ["rm -rf *"]
            }
        });
        handle_event(&repeated, &events, &commands, &turn, false, &mut state);
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("the driver's remembered rule should answer without asking again");
        };
        assert_eq!(option_id, "once");
        assert!(event_rx.try_recv().is_err());

        let mut isolated = OpenCodeStreamState::default();
        handle_event(&repeated, &events, &commands, &turn, false, &mut isolated);
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Permission { request_id, .. } if request_id == "per_def"
        ));
        assert!(
            command_rx.try_recv().is_err(),
            "another driver must not inherit the approval"
        );
    }

    #[test]
    fn auto_modes_use_one_shot_provider_approval() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        handle_event(
            &json!({
                "type": "permission.requested",
                "properties": {
                    "id": "per_auto",
                    "sessionID": "ses_1",
                    "permission": "bash",
                    "patterns": ["cargo test"],
                    "always": ["cargo *"]
                }
            }),
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "once");
        assert!(event_rx.try_recv().is_err());
        assert!(state.permissions.lock().approved.is_empty());
    }
}

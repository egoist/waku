use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::{activity, computer_use as computer_use_runtime};
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{ActivityKind, DriverEvent, InteractionMode, ProviderResumeCursor, RuntimeMode};

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

enum CommandMessage {
    Prompt(String),
    Steer(String),
    Cancel,
    CancelExtensionRequest(String),
    Options(SessionOptions),
    Rollback {
        turns: usize,
        response: Sender<Result<ProviderResumeCursor, String>>,
    },
    Fork {
        turns_to_remove: usize,
        response: Sender<Result<ProviderResumeCursor, String>>,
    },
    Shutdown,
}

type PendingResponses = Arc<Mutex<HashMap<String, Sender<Result<Value, String>>>>>;

pub struct PiDriver {
    commands: Sender<CommandMessage>,
    computer_use: Option<computer_use_runtime::ComputerUseRuntime>,
}

fn configure_pi_computer_use_command(
    command: &mut std::process::Command,
    config: Option<(&computer_use_runtime::ComputerUseConfig, &Path)>,
) {
    if let Some((config, extension)) = config {
        command
            .arg("--extension")
            .arg(extension)
            .arg("--skill")
            .arg(&config.skill_path)
            .env("WAKU_JS_REPL_SERVER", &config.repl_path)
            .env("WAKU_COMPUTER_USE_SERVER", &config.server_path)
            .env(
                "WAKU_COMPUTER_USE_PROCESS_DIRECTORY",
                &config.process_directory,
            );
    }
}

impl PiDriver {
    pub fn start(options: DriverStartOptions, events: DriverEventSender) -> anyhow::Result<Self> {
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
        if mode != RuntimeMode::FullAccess || interaction_mode != InteractionMode::Build {
            return Err(anyhow!("Pi currently supports Build with Full access only"));
        }
        let resume_session_file = match provider_cursor {
            Some(ProviderResumeCursor::Pi {
                session_file: Some(session_file),
                ..
            }) => Some(session_file),
            Some(ProviderResumeCursor::Pi { .. }) => {
                return Err(anyhow!(
                    "cannot resume Pi because its native session file is missing"
                ));
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume Pi from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };
        if let Some(model) = model.as_deref() {
            parse_model_slug(model)?;
        }

        let computer_use = computer_use_enabled
            .then(|| computer_use_runtime::ComputerUseRuntime::start(events.clone()))
            .transpose()?;
        let pi_extension = computer_use
            .as_ref()
            .map(|_| crate::computer_use::pi_extension_path())
            .transpose()?;
        let mut command = crate::command_env::command(binary);
        command
            .args(["--mode", "rpc", "--approve"])
            .env("PI_SKIP_VERSION_CHECK", "1");
        configure_pi_computer_use_command(
            &mut command,
            computer_use
                .as_ref()
                .zip(pi_extension.as_deref())
                .map(|(runtime, extension)| (&runtime.config, extension)),
        );
        let command = command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            crate::command_env::spawn(command).context("failed to start `pi --mode rpc`")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Pi stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Pi stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Pi stderr unavailable"))?;

        let (commands, command_rx) = unbounded();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reader_commands = commands.clone();
        let reader_events = events.clone();
        let reader_thread =
            thread::Builder::new()
                .name("waku-pi-reader".into())
                .spawn(move || {
                    let mut stream_state = PiStreamState::default();
                    for line in BufReader::new(stdout).lines() {
                        match line {
                            Ok(line) if !line.trim().is_empty() => {
                                match serde_json::from_str::<Value>(&line) {
                                    Ok(value) => handle_pi_message(
                                        value,
                                        &reader_pending,
                                        &reader_commands,
                                        &reader_events,
                                        &mut stream_state,
                                    ),
                                    Err(error) => {
                                        let _ = reader_events.send(DriverEvent::Error(tr!(
                                            "errors.provider_invalid_json",
                                            provider = "Pi",
                                            error = error
                                        )));
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                let _ = reader_events.send(DriverEvent::Error(tr!(
                                    "errors.provider_transport_read",
                                    provider = "Pi",
                                    error = error
                                )));
                                break;
                            }
                        }
                    }
                    // Unblock anything waiting on an RPC reply immediately; the
                    // process thread owns the `ProcessExited` announcement so a
                    // non-zero exit can be reported before the runtime is torn down.
                    fail_pending(&reader_pending, "Pi RPC process exited");
                })?;

        let writer_pending = pending;
        let writer_events = events.clone();
        thread::Builder::new()
            .name("waku-pi-writer".into())
            .spawn(move || {
                let mut stdin = stdin;
                let mut next_request_id = 0_u64;
                let initialize = (|| -> Result<Value, String> {
                    let _ = send_request(
                        &mut stdin,
                        &writer_pending,
                        &mut next_request_id,
                        json!({"type": "get_state"}),
                    )?;
                    if let Some(session_file) = resume_session_file {
                        let response = send_request(
                            &mut stdin,
                            &writer_pending,
                            &mut next_request_id,
                            json!({
                                "type": "switch_session",
                                "sessionPath": session_file
                            }),
                        )?;
                        if response.pointer("/data/cancelled").and_then(Value::as_bool)
                            == Some(true)
                        {
                            return Err("Pi session switch was cancelled".into());
                        }
                    }
                    if let Some(model) = model.as_deref() {
                        let (provider, model_id) =
                            parse_model_slug(model).map_err(|error| error.to_string())?;
                        let _ = send_request(
                            &mut stdin,
                            &writer_pending,
                            &mut next_request_id,
                            json!({
                                "type": "set_model",
                                "provider": provider,
                                "modelId": model_id
                            }),
                        )?;
                    }
                    if let Some(level) = reasoning_effort.as_deref() {
                        let _ = send_request(
                            &mut stdin,
                            &writer_pending,
                            &mut next_request_id,
                            json!({"type": "set_thinking_level", "level": level}),
                        )?;
                    }
                    send_request(
                        &mut stdin,
                        &writer_pending,
                        &mut next_request_id,
                        json!({"type": "get_state"}),
                    )
                })();

                let state = match initialize {
                    Ok(state) => state,
                    Err(error) => {
                        let _ = writer_events.send(DriverEvent::Error(tr!(
                            "errors.initialize_provider",
                            provider = "Pi",
                            error = error
                        )));
                        let _ = writer_events.send(DriverEvent::TurnFinished {
                            success: false,
                            summary: Some(tr!(
                                "errors.provider_initialize_session",
                                provider = "Pi"
                            )),
                        });
                        return;
                    }
                };
                let Some(mut cursor) = cursor_from_state(&state) else {
                    let _ = writer_events.send(DriverEvent::Error(tr!(
                        "errors.provider_no_session_id",
                        provider = "Pi"
                    )));
                    let _ = writer_events.send(DriverEvent::TurnFinished {
                        success: false,
                        summary: Some(tr!("errors.provider_initialize_session", provider = "Pi")),
                    });
                    return;
                };
                let initial_usage = send_request(
                    &mut stdin,
                    &writer_pending,
                    &mut next_request_id,
                    json!({"type": "get_session_stats"}),
                )
                .ok()
                .and_then(|stats| pi_context_usage(&state, Some(&stats)))
                .or_else(|| pi_context_usage(&state, None));
                let _ = writer_events.send(DriverEvent::Connected {
                    provider_cursor: Some(cursor.clone()),
                });
                if let Some((context_tokens, context_window)) = initial_usage {
                    let _ = writer_events.send(DriverEvent::UsageUpdated {
                        context_tokens,
                        context_window,
                    });
                }
                if let Some(title) = state
                    .pointer("/data/sessionName")
                    .and_then(Value::as_str)
                    .filter(|title| !title.trim().is_empty())
                {
                    let _ =
                        writer_events.send(DriverEvent::AutoTitleUpdated(Some(title.to_owned())));
                }

                // Pi exposes setters for both, so changing either is an RPC on
                // the live session rather than a restart.
                let mut current_model = model;
                let mut current_effort = reasoning_effort;
                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(prompt) => {
                            let result = send_request(
                                &mut stdin,
                                &writer_pending,
                                &mut next_request_id,
                                json!({"type": "prompt", "message": prompt}),
                            );
                            if let Err(error) = result {
                                let _ = writer_events.send(DriverEvent::Error(tr!(
                                    "errors.provider_rejected_prompt_detail",
                                    provider = "Pi",
                                    error = error
                                )));
                                let _ = writer_events.send(DriverEvent::TurnFinished {
                                    success: false,
                                    summary: Some(tr!(
                                        "errors.provider_rejected_prompt",
                                        provider = "Pi"
                                    )),
                                });
                            }
                        }
                        CommandMessage::Steer(prompt) => {
                            let result = send_request(
                                &mut stdin,
                                &writer_pending,
                                &mut next_request_id,
                                json!({"type": "steer", "message": prompt}),
                            );
                            match result {
                                Ok(_) => {
                                    let _ = writer_events
                                        .send(DriverEvent::SteerAccepted { message: prompt });
                                }
                                Err(error) => {
                                    let _ = writer_events.send(DriverEvent::SteerRejected {
                                        message: prompt,
                                        reason: error,
                                    });
                                }
                            }
                        }
                        CommandMessage::Cancel => {
                            if let Err(error) = send_request(
                                &mut stdin,
                                &writer_pending,
                                &mut next_request_id,
                                json!({"type": "abort"}),
                            ) {
                                let _ = writer_events.send(DriverEvent::Error(tr!(
                                    "errors.stop_provider",
                                    provider = "Pi",
                                    error = error
                                )));
                            }
                        }
                        CommandMessage::Options(options) => {
                            if options.model != current_model {
                                match options.model.as_deref().map(parse_model_slug).transpose() {
                                    Ok(Some((provider, model_id))) => {
                                        match send_request(
                                            &mut stdin,
                                            &writer_pending,
                                            &mut next_request_id,
                                            json!({
                                                "type": "set_model",
                                                "provider": provider,
                                                "modelId": model_id
                                            }),
                                        ) {
                                            Ok(response) => {
                                                if let Some(window) = response
                                                    .pointer("/data/contextWindow")
                                                    .and_then(Value::as_u64)
                                                    .filter(|window| *window > 0)
                                                {
                                                    let _ = writer_events.send(
                                                        DriverEvent::UsageUpdated {
                                                            context_tokens: None,
                                                            context_window: Some(window),
                                                        },
                                                    );
                                                }
                                            }
                                            Err(error) => {
                                                let _ =
                                                    writer_events.send(DriverEvent::Error(tr!(
                                                        "errors.switch_provider_model",
                                                        provider = "Pi",
                                                        error = error
                                                    )));
                                            }
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        let _ = writer_events
                                            .send(DriverEvent::Error(error.to_string()));
                                    }
                                }
                                current_model = options.model;
                            }
                            if options.reasoning_effort != current_effort {
                                if let Some(level) = options.reasoning_effort.as_deref()
                                    && let Err(error) = send_request(
                                        &mut stdin,
                                        &writer_pending,
                                        &mut next_request_id,
                                        json!({"type": "set_thinking_level", "level": level}),
                                    )
                                {
                                    let _ = writer_events.send(DriverEvent::Error(tr!(
                                        "errors.change_provider_thinking",
                                        provider = "Pi",
                                        error = error
                                    )));
                                }
                                current_effort = options.reasoning_effort;
                            }
                        }
                        CommandMessage::CancelExtensionRequest(id) => {
                            if write_json_line(
                                &mut stdin,
                                &json!({
                                    "type": "extension_ui_response",
                                    "id": id,
                                    "cancelled": true
                                }),
                            )
                            .is_err()
                            {
                                break;
                            }
                        }
                        CommandMessage::Rollback { turns, response } => {
                            let result = fork_pi_session(
                                &mut stdin,
                                &writer_pending,
                                &mut next_request_id,
                                &cursor,
                                turns,
                                false,
                            );
                            if let Ok(next_cursor) = &result {
                                cursor = next_cursor.clone();
                            }
                            let _ = response.send(result);
                        }
                        CommandMessage::Fork {
                            turns_to_remove,
                            response,
                        } => {
                            let result = fork_pi_session(
                                &mut stdin,
                                &writer_pending,
                                &mut next_request_id,
                                &cursor,
                                turns_to_remove,
                                true,
                            );
                            let _ = response.send(result);
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })?;

        let last_visible_stderr = Arc::new(Mutex::new(None::<String>));
        let stderr_last_error = last_visible_stderr.clone();
        let stderr_events = events.clone();
        let stderr_thread =
            thread::Builder::new()
                .name("waku-pi-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if line.to_ascii_lowercase().contains("error") {
                            let error = format!("Pi: {}", line.trim());
                            *stderr_last_error.lock() = Some(error.clone());
                            let _ = stderr_events.send(DriverEvent::Error(error));
                        }
                    }
                })?;

        // Nothing signals or kills the Pi process: it exits when the writer
        // thread drops its stdin. Something still has to reap it, or every
        // session that ever ran leaves a zombie behind for the life of the app.
        thread::Builder::new()
            .name("waku-pi-process".into())
            .spawn(move || {
                let status = child.wait();
                let _ = reader_thread.join();
                let _ = stderr_thread.join();
                match status {
                    Ok(status) if !status.success() && last_visible_stderr.lock().is_none() => {
                        let _ = events.send(DriverEvent::Error(tr!(
                            "errors.provider_rpc_exited",
                            provider = "Pi",
                            status = status
                        )));
                    }
                    Err(error) => {
                        let _ = events.send(DriverEvent::Error(tr!(
                            "errors.read_provider_exit_status",
                            provider = "Pi RPC",
                            error = error
                        )));
                    }
                    _ => {}
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            computer_use,
        })
    }
}

impl DriverControl for PiDriver {
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

    fn respond(&self, _request_id: String, _option_id: String) {}

    fn apply_options(&self, options: SessionOptions) -> bool {
        // Pi has setters for the model and thinking level, so those apply to the
        // live session. It has none for permissions — and only runs Build with
        // Full access anyway — so a mode change asks for a fresh start, which is
        // where that constraint is reported.
        if options.mode != RuntimeMode::FullAccess
            || options.interaction_mode != InteractionMode::Build
        {
            return false;
        }
        self.commands.send(CommandMessage::Options(options)).is_ok()
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        if turns == 0 {
            return Ok(None);
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(CommandMessage::Rollback {
                turns,
                response: response_tx,
            })
            .context("Pi driver stopped before rollback")?;
        response_rx
            .recv_timeout(Duration::from_secs(45))
            .context("timed out waiting for Pi conversation rollback")?
            .map(Some)
            .map_err(anyhow::Error::msg)
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(CommandMessage::Fork {
                turns_to_remove,
                response: response_tx,
            })
            .context("Pi driver stopped before forking")?;
        response_rx
            .recv_timeout(Duration::from_secs(45))
            .context("timed out waiting for Pi conversation fork")?
            .map_err(anyhow::Error::msg)
    }
}

impl Drop for PiDriver {
    fn drop(&mut self) {
        self.cancel_computer_use();
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

fn send_request(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_request_id: &mut u64,
    mut request: Value,
) -> Result<Value, String> {
    *next_request_id += 1;
    let id = format!("waku-{}", next_request_id);
    request["id"] = Value::String(id.clone());
    let (response_tx, response_rx) = bounded(1);
    pending.lock().insert(id.clone(), response_tx);
    if let Err(error) = write_json_line(stdin, &request) {
        pending.lock().remove(&id);
        return Err(format!("transport write failed: {error}"));
    }
    match response_rx.recv_timeout(RPC_TIMEOUT) {
        Ok(response) => response,
        Err(_) => {
            pending.lock().remove(&id);
            Err(format!(
                "{} timed out",
                request["type"].as_str().unwrap_or("request")
            ))
        }
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn fail_pending(pending: &PendingResponses, message: &str) {
    for (_, response) in pending.lock().drain() {
        let _ = response.send(Err(message.to_owned()));
    }
}

fn parse_model_slug(model: &str) -> anyhow::Result<(&str, &str)> {
    let Some((provider, model_id)) = model.trim().split_once('/') else {
        return Err(anyhow!(
            "Pi models must use provider/model format; received `{model}`"
        ));
    };
    if provider.is_empty() || model_id.is_empty() {
        return Err(anyhow!(
            "Pi models must use provider/model format; received `{model}`"
        ));
    }
    Ok((provider, model_id))
}

fn cursor_from_state(response: &Value) -> Option<ProviderResumeCursor> {
    let session_id = response
        .pointer("/data/sessionId")
        .and_then(Value::as_str)?;
    let session_file = response
        .pointer("/data/sessionFile")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    Some(ProviderResumeCursor::Pi {
        session_id: session_id.to_owned(),
        session_file,
    })
}

/// Pi already computes context occupancy for its own footer. Prefer that
/// native value when session stats are available, and use the active model in
/// `get_state` for the window before the first assistant message arrives.
fn pi_context_usage(state: &Value, stats: Option<&Value>) -> Option<(Option<u64>, Option<u64>)> {
    let context = stats.and_then(|stats| stats.pointer("/data/contextUsage"));
    let tokens = context
        .and_then(|context| context.get("tokens"))
        .and_then(Value::as_u64);
    let window = context
        .and_then(|context| context.get("contextWindow"))
        .and_then(Value::as_u64)
        .or_else(|| {
            state
                .pointer("/data/model/contextWindow")
                .and_then(Value::as_u64)
        })
        .filter(|window| *window > 0);
    (tokens.is_some() || window.is_some()).then_some((tokens, window))
}

/// Pi's providers normally fill `totalTokens`, but Pi itself deliberately
/// falls back to the four component counters when a provider leaves it zero.
/// Keep Waku's meter aligned with that provider-native calculation.
fn pi_message_context_tokens(message: &Value) -> Option<u64> {
    let usage = message.get("usage")?;
    usage
        .get("totalTokens")
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0)
        .or_else(|| {
            let total = ["input", "output", "cacheRead", "cacheWrite"]
                .into_iter()
                .filter_map(|field| usage.get(field).and_then(Value::as_u64))
                .fold(0_u64, u64::saturating_add);
            (total > 0).then_some(total)
        })
}

fn fork_pi_session(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_request_id: &mut u64,
    original_cursor: &ProviderResumeCursor,
    turns_to_remove: usize,
    restore_original: bool,
) -> Result<ProviderResumeCursor, String> {
    let messages = send_request(
        stdin,
        pending,
        next_request_id,
        json!({"type": "get_fork_messages"}),
    )?;
    let messages = messages
        .pointer("/data/messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Pi returned an invalid fork-message list".to_owned())?;
    let request = pi_fork_request(messages, turns_to_remove)?;
    let fork = send_request(stdin, pending, next_request_id, request)?;
    if fork.pointer("/data/cancelled").and_then(Value::as_bool) == Some(true) {
        return Err("Pi session fork was cancelled".into());
    }
    let fork_state = send_request(
        stdin,
        pending,
        next_request_id,
        json!({"type": "get_state"}),
    )?;
    let fork_cursor = cursor_from_state(&fork_state)
        .ok_or_else(|| "Pi did not report the forked session cursor".to_owned())?;

    if restore_original {
        let ProviderResumeCursor::Pi {
            session_file: Some(session_file),
            ..
        } = original_cursor
        else {
            return Err("Pi's original session file is unavailable".into());
        };
        let switched = send_request(
            stdin,
            pending,
            next_request_id,
            json!({
                "type": "switch_session",
                "sessionPath": session_file
            }),
        )?;
        if switched.pointer("/data/cancelled").and_then(Value::as_bool) == Some(true) {
            return Err("Pi could not return to the source session after forking".into());
        }
        let restored_state = send_request(
            stdin,
            pending,
            next_request_id,
            json!({"type": "get_state"}),
        )?;
        let restored_cursor = cursor_from_state(&restored_state)
            .ok_or_else(|| "Pi did not report the restored source session".to_owned())?;
        if restored_cursor.native_id() != original_cursor.native_id() {
            return Err("Pi returned to the wrong source session after forking".into());
        }
    }

    Ok(fork_cursor)
}

fn pi_fork_request(messages: &[Value], turns_to_remove: usize) -> Result<Value, String> {
    if turns_to_remove > messages.len() {
        return Err(format!(
            "Pi has only {} native turns, but Waku needs to remove {turns_to_remove}",
            messages.len()
        ));
    }
    let retained_turns = messages.len() - turns_to_remove;
    if turns_to_remove == 0 {
        Ok(json!({"type": "clone"}))
    } else {
        let entry_id = messages
            .get(retained_turns)
            .and_then(|message| message.get("entryId"))
            .and_then(Value::as_str)
            .ok_or_else(|| "Pi returned a fork message without an entry ID".to_owned())?;
        Ok(json!({"type": "fork", "entryId": entry_id}))
    }
}

#[derive(Default)]
struct PiStreamState {
    run_started: bool,
    message_saw_text: bool,
    message_saw_reasoning: bool,
    failed: bool,
    tools: HashMap<String, (ActivityKind, String)>,
}

fn handle_pi_message(
    value: Value,
    pending: &PendingResponses,
    commands: &Sender<CommandMessage>,
    events: &impl DriverEventSink,
    state: &mut PiStreamState,
) {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "response" {
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(response) = pending.lock().remove(id) else {
            return;
        };
        if value.get("success").and_then(Value::as_bool) == Some(true) {
            let _ = response.send(Ok(value));
        } else {
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Pi RPC command failed")
                .to_owned();
            let _ = response.send(Err(error));
        }
        return;
    }

    match event_type {
        "session_info_changed" => {
            let title = value
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
            let _ = events.send(DriverEvent::AutoTitleUpdated(title));
        }
        "agent_start" | "turn_start" => {
            if !state.run_started {
                state.run_started = true;
                state.failed = false;
                let _ = events.send(DriverEvent::TurnStarted);
            }
        }
        "message_start" => {
            if value.pointer("/message/role").and_then(Value::as_str) == Some("assistant") {
                state.message_saw_text = false;
                state.message_saw_reasoning = false;
            }
        }
        "message_update" => {
            let update = value.get("assistantMessageEvent").unwrap_or(&Value::Null);
            match update.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(delta) = update
                        .get("delta")
                        .and_then(Value::as_str)
                        .filter(|delta| !delta.is_empty())
                    {
                        state.message_saw_text = true;
                        let _ = events.send(DriverEvent::TextDelta(delta.to_owned()));
                    }
                }
                Some("thinking_delta") => {
                    if let Some(delta) = update
                        .get("delta")
                        .and_then(Value::as_str)
                        .filter(|delta| !delta.is_empty())
                    {
                        state.message_saw_reasoning = true;
                        let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
                    }
                }
                Some("error") => {
                    state.failed = true;
                    let _ = events.send(DriverEvent::Error(pi_error_message(update)));
                }
                _ => {}
            }
        }
        "message_end" => {
            if value.pointer("/message/role").and_then(Value::as_str) == Some("assistant") {
                // This is the context the next call starts from, not the
                // cumulative billed total for the whole session.
                if let Some(tokens) = value.get("message").and_then(pi_message_context_tokens) {
                    let _ = events.send(DriverEvent::UsageUpdated {
                        context_tokens: Some(tokens),
                        context_window: None,
                    });
                }
                emit_completed_message_fallback(value.get("message"), events, state);
            }
        }
        "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => {
            let id = value
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let tool_name = value.get("toolName").and_then(Value::as_str);
            let (kind, mut title) = id
                .as_ref()
                .and_then(|id| state.tools.get(id))
                .cloned()
                .unwrap_or_else(|| {
                    tool_name
                        .map(|tool_name| (classify_tool(tool_name), tool_title(tool_name)))
                        .unwrap_or_else(|| (ActivityKind::Tool, tr!("activity.tool")))
                });
            if event_type == "tool_execution_start"
                && let Some(input_title) = activity::input_title(value.get("args"))
            {
                title = input_title;
            }
            if event_type == "tool_execution_start"
                && let Some(id) = id.as_ref()
            {
                state.tools.insert(id.clone(), (kind, title.clone()));
            }
            let arguments = (event_type == "tool_execution_start")
                .then(|| value.get("args"))
                .flatten();
            let output = match event_type {
                "tool_execution_update" => value.get("partialResult"),
                "tool_execution_end" => value.get("result"),
                _ => None,
            };
            let complete = event_type == "tool_execution_end";
            let failed = value.get("isError").and_then(Value::as_bool) == Some(true);
            let item = activity::tool_activity(
                id.clone(),
                kind,
                title,
                arguments,
                output,
                output,
                failed,
                complete,
            );
            let _ = events.send(DriverEvent::RichActivity(item));
            if complete && let Some(id) = id {
                state.tools.remove(&id);
            }
        }
        "auto_retry_end" => {
            if value.get("success").and_then(Value::as_bool) == Some(true) {
                state.failed = false;
            } else {
                state.failed = true;
                let message = value
                    .get("finalError")
                    .and_then(Value::as_str)
                    .unwrap_or("Pi exhausted its automatic retries");
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
        }
        "agent_settled" => {
            if state.run_started {
                let success = !state.failed;
                let _ = events.send(DriverEvent::TurnFinished {
                    success,
                    summary: (!success)
                        .then(|| tr!("errors.provider_complete_turn", provider = "Pi")),
                });
            }
            *state = PiStreamState::default();
        }
        "extension_ui_request" => {
            let method = value.get("method").and_then(Value::as_str);
            let id = value.get("id").and_then(Value::as_str);
            if matches!(method, Some("select" | "confirm" | "input" | "editor"))
                && let Some(id) = id
            {
                let _ = commands.send(CommandMessage::CancelExtensionRequest(id.to_owned()));
            }
        }
        "extension_error" => {
            let _ = events.send(DriverEvent::Error(pi_error_message(&value)));
        }
        _ => {}
    }
}

fn emit_completed_message_fallback(
    message: Option<&Value>,
    events: &impl DriverEventSink,
    state: &mut PiStreamState,
) {
    let Some(content) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") if !state.message_saw_text => {
                if let Some(text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    state.message_saw_text = true;
                    let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                }
            }
            Some("thinking") if !state.message_saw_reasoning => {
                if let Some(thinking) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|thinking| !thinking.is_empty())
                {
                    state.message_saw_reasoning = true;
                    let _ = events.send(DriverEvent::ReasoningDelta(thinking.to_owned()));
                }
            }
            _ => {}
        }
    }
}

fn pi_error_message(value: &Value) -> String {
    value
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| value.get("errorMessage").and_then(Value::as_str))
        .or_else(|| value.get("reason").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("errors.pi_reported_error"))
}

fn classify_tool(name: &str) -> ActivityKind {
    ActivityKind::from_tool_name(name)
}

fn tool_title(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "bash" => tr!("activity.run_command"),
        "edit" => tr!("activity.edit_file"),
        "write" => tr!("activity.write_file"),
        "read" => tr!("activity.read_file"),
        "grep" => tr!("activity.search_files"),
        "find" => tr!("activity.find_files"),
        "ls" => tr!("activity.list_files"),
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::TryRecvError;

    fn harness() -> (
        PendingResponses,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        PiStreamState,
    ) {
        let (commands, receiver) = unbounded();
        (
            Arc::new(Mutex::new(HashMap::new())),
            commands,
            receiver,
            PiStreamState::default(),
        )
    }

    /// Drives the installed Pi RPC through one real provider turn. Ignored by
    /// default because it needs the CLI, credentials, and network access.
    #[test]
    #[ignore = "requires an installed, authenticated pi"]
    fn pi_context_usage_against_the_real_rpc() {
        let binary = crate::command_env::find_executable("pi").expect("pi is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = PiDriver::start(
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
        .expect("the Pi RPC session should start");

        let mut connected = false;
        let mut context_tokens = None;
        let mut context_window = None;
        while let Ok(event) = event_rx.recv_timeout(Duration::from_secs(30)) {
            match event {
                DriverEvent::Connected { .. } => {
                    connected = true;
                    break;
                }
                DriverEvent::Error(error) => panic!("Pi failed to initialize: {error}"),
                _ => {}
            }
        }
        assert!(connected, "Pi never reported its native session");

        driver.prompt("Reply with exactly: OK. Do not use any tools.".into());
        let mut finished = false;
        while let Ok(event) = event_rx.recv_timeout(Duration::from_secs(180)) {
            match event {
                DriverEvent::UsageUpdated {
                    context_tokens: tokens,
                    context_window: window,
                } => {
                    context_tokens = tokens.or(context_tokens);
                    context_window = window.or(context_window);
                }
                DriverEvent::TurnFinished { success, .. } => {
                    assert!(success, "Pi should finish the probe turn");
                    finished = true;
                    break;
                }
                DriverEvent::Error(error) => panic!("Pi reported: {error}"),
                _ => {}
            }
        }

        assert!(finished, "Pi never settled the probe turn");
        assert!(context_tokens.is_some_and(|tokens| tokens > 0));
        assert!(context_window.is_some_and(|window| window > 0));
    }

    #[test]
    fn model_and_thinking_changes_reach_the_running_session_but_mode_changes_do_not() {
        let (commands, command_rx) = unbounded();
        let driver = PiDriver {
            commands,
            computer_use: None,
        };
        let options = |mode, interaction_mode| SessionOptions {
            mode,
            interaction_mode,
            model: Some("anthropic/claude-opus-5".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            service_tier: None,
        };

        assert!(driver.apply_options(options(RuntimeMode::FullAccess, InteractionMode::Build)));
        assert!(matches!(
            command_rx.try_recv(),
            Ok(CommandMessage::Options(_))
        ));

        // Pi has no permission setter, and only runs Build with Full access.
        assert!(!driver.apply_options(options(RuntimeMode::Ask, InteractionMode::Build)));
        assert!(!driver.apply_options(options(RuntimeMode::FullAccess, InteractionMode::Plan)));
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn pi_computer_use_uses_only_session_scoped_extension_and_skill_arguments() {
        let config = computer_use_runtime::ComputerUseConfig {
            server_path: PathBuf::from("/tmp/Waku Computer Use"),
            repl_path: PathBuf::from("/Applications/Waku.app/Resources/waku_js_repl"),
            skill_path: PathBuf::from("/Applications/Waku.app/Resources/skills/SKILL.md"),
            process_directory: PathBuf::from("/tmp/waku-computer-use/session"),
        };
        let mut command = std::process::Command::new("pi");

        configure_pi_computer_use_command(
            &mut command,
            Some((
                &config,
                Path::new("/Applications/Waku.app/Resources/computer-use/pi-extension.ts"),
            )),
        );

        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "--extension",
                "/Applications/Waku.app/Resources/computer-use/pi-extension.ts",
                "--skill",
                "/Applications/Waku.app/Resources/skills/SKILL.md",
            ]
        );
        let environment = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(
            environment.get("WAKU_JS_REPL_SERVER"),
            Some(&Some(
                "/Applications/Waku.app/Resources/waku_js_repl".into()
            ))
        );
        assert_eq!(
            environment.get("WAKU_COMPUTER_USE_PROCESS_DIRECTORY"),
            Some(&Some("/tmp/waku-computer-use/session".into()))
        );
    }

    #[test]
    fn pi_fork_selects_the_first_removed_user_turn_or_clones_the_tip() {
        let messages = [
            json!({"entryId": "turn-1"}),
            json!({"entryId": "turn-2"}),
            json!({"entryId": "turn-3"}),
        ];
        assert_eq!(
            pi_fork_request(&messages, 0).unwrap(),
            json!({"type": "clone"})
        );
        assert_eq!(
            pi_fork_request(&messages, 2).unwrap(),
            json!({"type": "fork", "entryId": "turn-2"})
        );
        assert_eq!(
            pi_fork_request(&messages, 3).unwrap(),
            json!({"type": "fork", "entryId": "turn-1"})
        );
        assert!(pi_fork_request(&messages, 4).is_err());
    }

    #[test]
    fn streams_pi_text_reasoning_tools_and_settles_once() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();
        for value in [
            json!({"type": "agent_start"}),
            json!({"type": "turn_start"}),
            json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "thinking_delta", "delta": "checking"}
            }),
            json!({
                "type": "tool_execution_start",
                "toolCallId": "tool-1",
                "toolName": "read",
                "args": {"path": "src/main.rs", "title": "Inspect Pi source"}
            }),
            json!({
                "type": "tool_execution_end",
                "toolCallId": "tool-1",
                "toolName": "read",
                "result": {"content": "..."},
                "isError": false
            }),
            json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "delta": "done"}
            }),
            json!({"type": "agent_end", "willRetry": false}),
            json!({"type": "agent_settled"}),
        ] {
            handle_pi_message(value, &pending, &commands, &events, &mut state);
        }

        assert!(matches!(event_rx.recv().unwrap(), DriverEvent::TurnStarted));
        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::ReasoningDelta(value) if value == "checking"
        ));
        let DriverEvent::RichActivity(started) = event_rx.recv().unwrap() else {
            panic!("expected a rich Pi tool activity");
        };
        assert_eq!(started.title, "Inspect Pi source");
        assert_eq!(started.kind, ActivityKind::FileRead);
        assert_eq!(started.display_target.as_deref(), Some("src/main.rs"));
        assert!(
            started
                .arguments
                .as_deref()
                .is_some_and(|arguments| arguments.contains("src/main.rs"))
        );
        assert!(!started.complete);
        let DriverEvent::RichActivity(completed) = event_rx.recv().unwrap() else {
            panic!("expected a completed rich Pi tool activity");
        };
        assert!(
            completed
                .output
                .as_deref()
                .is_some_and(|output| output.contains("..."))
        );
        assert!(completed.complete);
        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::TextDelta(value) if value == "done"
        ));
        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert!(matches!(event_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn session_name_changes_are_forwarded_as_automatic_titles() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();

        handle_pi_message(
            json!({"type": "session_info_changed", "name": "Named by Pi"}),
            &pending,
            &commands,
            &events,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Named by Pi"
        ));

        handle_pi_message(
            json!({"type": "session_info_changed", "name": null}),
            &pending,
            &commands,
            &events,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(None)
        ));
    }

    #[test]
    fn tool_only_intermediate_message_does_not_emit_empty_text() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();
        handle_pi_message(
            json!({
                "type": "message_end",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "toolCall", "id": "tool-1", "name": "read"}]
                }
            }),
            &pending,
            &commands,
            &events,
            &mut state,
        );

        assert!(matches!(event_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn completed_message_is_used_when_deltas_were_not_streamed() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();
        handle_pi_message(
            json!({
                "type": "message_end",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "reason"},
                        {"type": "text", "text": "answer"}
                    ]
                }
            }),
            &pending,
            &commands,
            &events,
            &mut state,
        );

        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::ReasoningDelta(value) if value == "reason"
        ));
        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::TextDelta(value) if value == "answer"
        ));
    }

    #[test]
    fn context_usage_uses_pi_components_when_total_is_zero() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();
        handle_pi_message(
            json!({
                "type": "message_end",
                "message": {
                    "role": "assistant",
                    "content": [],
                    "usage": {
                        "input": 33,
                        "output": 27,
                        "cacheRead": 5888,
                        "cacheWrite": 4,
                        "totalTokens": 0
                    }
                }
            }),
            &pending,
            &commands,
            &events,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UsageUpdated {
                context_tokens: Some(5952),
                context_window: None
            }
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn session_stats_supply_pi_context_tokens_and_window() {
        let state = json!({"data": {"model": {"contextWindow": 200_000}}});
        let stats = json!({
            "data": {
                "contextUsage": {
                    "tokens": 6109,
                    "contextWindow": 1_000_000,
                    "percent": 0.6109
                }
            }
        });

        assert_eq!(
            pi_context_usage(&state, Some(&stats)),
            Some((Some(6109), Some(1_000_000)))
        );
        assert_eq!(pi_context_usage(&state, None), Some((None, Some(200_000))));
    }

    #[test]
    fn recoverable_tool_error_does_not_fail_the_turn() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();
        for value in [
            json!({"type": "agent_start"}),
            json!({
                "type": "tool_execution_end",
                "toolCallId": "tool-1",
                "toolName": "read",
                "result": {"error": "missing"},
                "isError": true
            }),
            json!({"type": "agent_settled"}),
        ] {
            handle_pi_message(value, &pending, &commands, &events, &mut state);
        }

        assert!(matches!(event_rx.recv().unwrap(), DriverEvent::TurnStarted));
        let DriverEvent::RichActivity(completed) = event_rx.recv().unwrap() else {
            panic!("expected a completed rich Pi tool activity");
        };
        assert!(completed.failed);
        assert!(
            completed
                .output
                .as_deref()
                .is_some_and(|output| output.contains("missing"))
        );
        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::TurnFinished { success: true, .. }
        ));
    }

    #[test]
    fn successful_auto_retry_recovers_the_turn() {
        let (pending, commands, _command_rx, mut state) = harness();
        let (events, event_rx) = unbounded();
        for value in [
            json!({"type": "agent_start"}),
            json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "error", "error": "temporary"}
            }),
            json!({"type": "auto_retry_end", "success": true}),
            json!({"type": "agent_settled"}),
        ] {
            handle_pi_message(value, &pending, &commands, &events, &mut state);
        }

        assert!(matches!(event_rx.recv().unwrap(), DriverEvent::TurnStarted));
        assert!(matches!(event_rx.recv().unwrap(), DriverEvent::Error(_)));
        assert!(matches!(
            event_rx.recv().unwrap(),
            DriverEvent::TurnFinished { success: true, .. }
        ));
    }
}

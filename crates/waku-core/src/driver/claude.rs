//! Claude Code's streaming-input session.
//!
//! `claude` accepts a realtime stream of user messages on stdin and answers on
//! stdout, which is the same transport the Claude Agent SDK's `query()` drives —
//! the SDK is a wrapper around these flags, not a separate capability. One
//! process serves the whole conversation, and `--permission-prompt-tool stdio`
//! makes it ask the host before running a tool instead of deciding alone.
//! A user message written mid-turn is folded into the running turn at the next
//! model call rather than queued as a turn of its own, which is what makes
//! steering a plain write.
//!
//! Flags and payloads here were read off the real CLI and the SDK's own
//! invocation, not guessed. `--permission-prompt-tool` in particular is absent
//! from `claude --help`.

use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};
use uuid::Uuid;

use super::activity;
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{
    ActivityKind, BackgroundWorkEvent, BackgroundWorkItem, BackgroundWorkKey, BackgroundWorkKind,
    BackgroundWorkStatus, DriverEvent, InteractionMode, PermissionOption, ProviderResumeCursor,
    RuntimeMode, UserInputAnswer, UserInputOption, UserInputQuestion, unix_time_millis,
};

enum CommandMessage {
    Prompt(String),
    Steer(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    RespondUserInput {
        request_id: String,
        input: Value,
        answers: Vec<UserInputAnswer>,
    },
    Options(SessionOptions),
    StopBackgroundWork {
        key: BackgroundWorkKey,
        control_id: String,
    },
    Shutdown,
}

/// The stream-json user message both prompts and steers are delivered as.
fn user_message_payload(text: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        },
        "parent_tool_use_id": null
    })
}

fn stop_task_request(request_id: u64, task_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": format!("waku-{request_id}"),
        "request": {"subtype": "stop_task", "task_id": task_id}
    })
}

pub struct ClaudeDriver {
    commands: Sender<CommandMessage>,
    pending_user_inputs: Arc<Mutex<HashMap<String, Value>>>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
}

/// The permission posture Claude is launched with.
fn permission_mode(mode: RuntimeMode, interaction_mode: InteractionMode) -> &'static str {
    if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
        return "plan";
    }
    match mode {
        RuntimeMode::Ask => "default",
        RuntimeMode::AutoAcceptEdits => "acceptEdits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "bypassPermissions",
        RuntimeMode::Plan => unreachable!("handled above"),
    }
}

/// The model id to hand the CLI for a session's context-window choice.
///
/// Claude Code serves 200K by default and reaches its 1M window through a
/// `[1m]` suffix on the model id rather than a flag, so the window travels with
/// `--model` and with mid-session `set_model` requests.
fn wire_model(model: Option<&str>, context_window: Option<&str>) -> Option<String> {
    let model = model?;
    if context_window == Some("1m") && !model.ends_with("[1m]") {
        Some(format!("{model}[1m]"))
    } else {
        Some(model.to_owned())
    }
}

/// Claude Code titles the session itself, on Haiku, in a request it fires
/// alongside the first turn's own first model call, so the title reaches the
/// native transcript about three seconds in — long before the turn it belongs
/// to settles. Reading it only when `result` arrives, as the turn-end pass
/// does, leaves an agentic first turn showing the truncated prompt for its
/// whole run and an interrupted one showing it forever. One look after five
/// seconds catches it, with a second as insurance; a schedule that runs dry
/// re-arms on the next prompt, and the turn-end pass still owns later
/// retitles and the rewind cursor.
fn start_claude_title_refresh(
    title_refresh: &super::title_refresh::NativeTitleRefresh,
    session_id: &str,
    events: &DriverEventSender,
) {
    let session_id = session_id.to_owned();
    title_refresh.start(
        "waku-claude-title",
        vec![Duration::from_secs(5), Duration::from_secs(10)],
        events.clone(),
        move || crate::claude_session::session_metadata(&session_id).map(|native| native.title),
    );
}

fn configure_stream_command(
    command: &mut Command,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) {
    command.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Newer Claude models omit readable thinking text unless the caller
        // explicitly asks for a summary. Raw reasoning remains provider-private.
        "--thinking-display",
        "summarized",
        // Echoes each user message back, which is how a queued prompt is
        // distinguished from one the agent has started on.
        "--replay-user-messages",
        // Undocumented, and the whole reason Supervised can mean supervised:
        // without it the CLI decides permissions itself and only reports
        // denials after the fact.
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        permission_mode(mode, interaction_mode),
    ]);
    if mode == RuntimeMode::FullAccess && interaction_mode != InteractionMode::Plan {
        command.arg("--dangerously-skip-permissions");
    }
}

impl ClaudeDriver {
    pub fn start(options: DriverStartOptions, events: DriverEventSender) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier: _,
            context_window,
            agent_preset: _,
            computer_use_enabled: _,
            provider_cursor,
        } = options;
        let (resume_session_id, resume_at) = match provider_cursor {
            Some(ProviderResumeCursor::Claude {
                session_id,
                resume_at,
            }) => ((!session_id.is_empty()).then_some(session_id), resume_at),
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume Claude Code from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => (None, None),
        };
        // Claude accepts a caller-chosen session id, so the cursor exists before
        // the first turn does and a rewind has something to point at.
        let session_id = resume_session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let mut command = crate::command_env::command(&binary);
        command.current_dir(&cwd);
        configure_stream_command(&mut command, mode, interaction_mode);
        let launch_model = wire_model(model.as_deref(), context_window.as_deref());
        if let Some(model) = launch_model.as_deref() {
            command.args(["--model", model]);
        }
        if let Some(effort) = reasoning_effort.as_deref() {
            command.args(["--effort", effort]);
        }
        if resume_session_id.is_some() {
            command.args(["--resume", &session_id]);
        } else {
            command.args(["--session-id", &session_id]);
        }

        let command = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::command_env::spawn(command)
            .context("failed to start `claude` in streaming-input mode")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Claude stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Claude stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Claude stderr unavailable"))?;

        // The cursor is known up front, so a rewind can address this session
        // before it has produced anything.
        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::Claude {
                session_id: session_id.clone(),
                resume_at,
            }),
        });

        let (commands, command_rx) = unbounded();
        let auto_approve = mode != RuntimeMode::Ask;
        let turn_active = Arc::new(Mutex::new(false));
        let pending_task_stops = Arc::new(Mutex::new(HashMap::<String, BackgroundWorkKey>::new()));
        let pending_user_inputs = Arc::new(Mutex::new(HashMap::<String, Value>::new()));

        let reader_events = events.clone();
        let reader_commands = commands.clone();
        let reader_turn = turn_active.clone();
        let reader_session = session_id.clone();
        let reader_pending_task_stops = pending_task_stops.clone();
        let reader_pending_user_inputs = pending_user_inputs.clone();
        let reader_thread = thread::Builder::new()
            .name("waku-claude-reader".into())
            .spawn(move || {
                let mut state = ClaudeStreamState {
                    pending_task_stops: reader_pending_task_stops,
                    pending_user_inputs: reader_pending_user_inputs,
                    ..ClaudeStreamState::default()
                };
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    handle_message(
                        &value,
                        &reader_session,
                        &reader_events,
                        &reader_commands,
                        &reader_turn,
                        auto_approve,
                        &mut state,
                    );
                }
            })?;

        let writer_events = events.clone();
        let writer_turn = turn_active;
        let writer_pending_task_stops = pending_task_stops;
        let writer_title_refresh = super::title_refresh::NativeTitleRefresh::default();
        let title_session_id = session_id;
        thread::Builder::new()
            .name("waku-claude-writer".into())
            .spawn(move || {
                let mut stdin = stdin;
                let mut next_request_id = 0_u64;
                let mut current_model = launch_model;
                while let Ok(message) = command_rx.recv() {
                    let written = match message {
                        CommandMessage::Prompt(text) => {
                            *writer_turn.lock() = true;
                            start_claude_title_refresh(
                                &writer_title_refresh,
                                &title_session_id,
                                &writer_events,
                            );
                            let _ = writer_events.send(DriverEvent::TurnStarted);
                            write_line(&mut stdin, &user_message_payload(&text))
                        }
                        CommandMessage::Steer(text) => {
                            // A mid-turn user message is held by the CLI and
                            // folded into the running turn at the next model
                            // call — one `result` settles everything, and the
                            // isReplay echo marks the moment it is absorbed.
                            // Verified against the real CLI (2.1.223). Unlike
                            // Amp, no marker is needed: folding is the default.
                            if !*writer_turn.lock() {
                                let _ = writer_events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: tr!(
                                        "errors.provider_no_active_turn",
                                        provider = "Claude"
                                    ),
                                });
                                continue;
                            }
                            // No TurnStarted and no turn re-arm: the turn the
                            // message joins is already running.
                            let written = write_line(&mut stdin, &user_message_payload(&text));
                            match &written {
                                Ok(()) => {
                                    let _ = writer_events
                                        .send(DriverEvent::SteerAccepted { message: text });
                                }
                                Err(error) => {
                                    let _ = writer_events.send(DriverEvent::SteerRejected {
                                        message: text,
                                        reason: tr!(
                                            "errors.provider_transport_write",
                                            provider = "Claude",
                                            error = error
                                        ),
                                    });
                                }
                            }
                            // A failed write still falls through to the shared
                            // transport-failure path: the running turn cannot
                            // settle once stdin is gone.
                            written
                        }
                        CommandMessage::Cancel => {
                            next_request_id += 1;
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_request",
                                    "request_id": format!("waku-{next_request_id}"),
                                    "request": {"subtype": "interrupt"}
                                }),
                            )
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let decision = if option_id == "deny" {
                                json!({
                                    "behavior": "deny",
                                    "message": "The user denied this tool call."
                                })
                            } else {
                                json!({"behavior": "allow"})
                            };
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_response",
                                    "response": {
                                        "subtype": "success",
                                        "request_id": request_id,
                                        "response": decision
                                    }
                                }),
                            )
                        }
                        CommandMessage::RespondUserInput {
                            request_id,
                            input,
                            answers,
                        } => {
                            let mut answer_values = serde_json::Map::new();
                            for answer in answers {
                                let multi_select = input
                                    .get("questions")
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .find(|question| {
                                        question.get("question").and_then(Value::as_str)
                                            == Some(answer.question_id.as_str())
                                    })
                                    .and_then(|question| question.get("multiSelect"))
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false);
                                let value = if multi_select {
                                    json!(answer.answers)
                                } else {
                                    Value::String(
                                        answer.answers.into_iter().next().unwrap_or_default(),
                                    )
                                };
                                answer_values.insert(answer.question_id, value);
                            }
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_response",
                                    "response": {
                                        "subtype": "success",
                                        "request_id": request_id,
                                        "response": {
                                            "behavior": "allow",
                                            "updatedInput": {
                                                "questions": input.get("questions").cloned().unwrap_or(Value::Array(Vec::new())),
                                                "answers": answer_values
                                            }
                                        }
                                    }
                                }),
                            )
                        }
                        CommandMessage::Options(options) => {
                            // The window rides on the model id, so switching it
                            // is the same `set_model` round trip as switching
                            // models — verified accepted by the CLI (2.1.228).
                            let next_model = wire_model(
                                options.model.as_deref(),
                                options.context_window.as_deref(),
                            );
                            if next_model == current_model {
                                continue;
                            }
                            current_model = next_model;
                            let Some(model) = current_model.as_deref() else {
                                continue;
                            };
                            next_request_id += 1;
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_request",
                                    "request_id": format!("waku-{next_request_id}"),
                                    "request": {"subtype": "set_model", "model": model}
                                }),
                            )
                        }
                        CommandMessage::StopBackgroundWork { key, control_id } => {
                            next_request_id += 1;
                            let request_id = format!("waku-{next_request_id}");
                            writer_pending_task_stops
                                .lock()
                                .insert(request_id, key.clone());
                            write_line(&mut stdin, &stop_task_request(next_request_id, &control_id))
                        }
                        CommandMessage::Shutdown => break,
                    };
                    if let Err(error) = written {
                        let _ = writer_events.send(DriverEvent::Error(tr!(
                            "errors.provider_transport_write",
                            provider = "Claude",
                            error = error
                        )));
                        // Nothing will settle a turn whose prompt never landed.
                        if std::mem::take(&mut *writer_turn.lock()) {
                            let _ = writer_events.send(DriverEvent::TurnFinished {
                                success: false,
                                summary: Some(tr!(
                                    "errors.provider_receive_prompt",
                                    provider = "Claude"
                                )),
                            });
                        }
                        break;
                    }
                }
            })?;

        let last_visible_stderr = Arc::new(Mutex::new(None::<String>));
        let stderr_last_error = last_visible_stderr.clone();
        let stderr_events = events.clone();
        let stderr_thread = thread::Builder::new()
            .name("waku-claude-stderr".into())
            .spawn(move || {
                let lines = BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                    .filter(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>();
                if let Some(message) = super::support::provider_stderr_error(lines) {
                    let error = format!("Claude Code: {message}");
                    *stderr_last_error.lock() = Some(error.clone());
                    let _ = stderr_events.send(DriverEvent::Error(error));
                }
            })?;

        thread::Builder::new()
            .name("waku-claude-process".into())
            .spawn(move || {
                let status = child.wait();
                let _ = reader_thread.join();
                let _ = stderr_thread.join();
                if let Ok(status) = status
                    && !status.success()
                    && last_visible_stderr.lock().is_none()
                {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.provider_exited",
                        provider = "Claude Code",
                        status = status
                    )));
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            pending_user_inputs,
            mode,
            interaction_mode,
        })
    }
}

impl DriverControl for ClaudeDriver {
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

    fn stop_background_work(&self, key: BackgroundWorkKey, control_id: String) {
        let _ = self
            .commands
            .send(CommandMessage::StopBackgroundWork { key, control_id });
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.send(CommandMessage::Respond {
            request_id,
            option_id,
        });
    }

    fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        let Some(input) = self.pending_user_inputs.lock().remove(&request_id) else {
            return;
        };
        let _ = self.commands.send(CommandMessage::RespondUserInput {
            request_id,
            input,
            answers,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // The model has a setter; the permission posture is a launch flag, and
        // changing what a running agent may touch deserves a fresh session.
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

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

fn write_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[derive(Default)]
struct ClaudeStreamState {
    saw_text_delta: bool,
    saw_reasoning_delta: bool,
    /// Tool-use id → transcript presentation plus the full shell command.
    /// Claude's later `task_started` event links back by id but does not repeat
    /// the command, so keep it here until the matching tool result arrives.
    tools: HashMap<String, (ActivityKind, String, String, Option<String>)>,
    background_task_kinds: HashMap<String, BackgroundWorkKind>,
    /// Task tool-use id → task id, so a subagent's own messages — they arrive
    /// on the main channel with `parent_tool_use_id` set — can be routed into
    /// that task's output pane instead of this session's transcript.
    subagent_tasks: HashMap<String, String>,
    /// The description each task started with. Progress events reuse the
    /// `description` field for the agent's current activity line, which
    /// belongs in the detail row, never the title.
    task_descriptions: HashMap<String, String>,
    /// Tasks whose output pane already carries streamed transcript; the
    /// settle notification's summary would only duplicate it.
    streamed_task_output: HashSet<String>,
    /// Stop handles for native Bash output files currently being tailed.
    task_output_tails: ClaudeTaskOutputTails,
    pending_task_stops: Arc<Mutex<HashMap<String, BackgroundWorkKey>>>,
    pending_user_inputs: Arc<Mutex<HashMap<String, Value>>>,
    /// Model of the latest main-thread assistant message, so the settled
    /// turn's `modelUsage` map can be read for that model's context window
    /// rather than a subagent's.
    last_assistant_model: Option<String>,
    /// Last title copied from Claude's native transcript metadata.
    last_auto_title: Option<String>,
}

#[derive(Default)]
struct ClaudeTaskOutputTails(HashMap<String, Arc<AtomicBool>>);

impl Drop for ClaudeTaskOutputTails {
    fn drop(&mut self) {
        for stop in self.0.values() {
            stop.store(true, Ordering::Release);
        }
    }
}

#[cfg(unix)]
const CLAUDE_TASK_OUTPUT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Claude keeps live Bash output outside its JSON stream. The installed CLI
/// writes it under `/tmp/claude-<uid>/<workspace>/<session>/tasks/<id>.output`;
/// locate the workspace component instead of reproducing Claude's private cwd
/// escaping rules.
#[cfg(unix)]
fn claude_task_output_path(session_id: &str, task_id: &str) -> Option<PathBuf> {
    // SAFETY: `geteuid` has no preconditions and does not retain pointers.
    let uid = unsafe { libc::geteuid() };
    claude_task_output_path_in(
        &Path::new("/tmp").join(format!("claude-{uid}")),
        session_id,
        task_id,
    )
}

#[cfg(unix)]
fn claude_task_output_path_in(root: &Path, session_id: &str, task_id: &str) -> Option<PathBuf> {
    if task_id.is_empty()
        || task_id.contains('/')
        || task_id.contains('\\')
        || session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
    {
        return None;
    }
    std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .path()
                .join(session_id)
                .join("tasks")
                .join(format!("{task_id}.output"))
        })
        .find(|path| path.is_file())
}

#[cfg(unix)]
fn drain_utf8_output(bytes: &mut Vec<u8>, final_read: bool) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => String::from_utf8(std::mem::take(bytes)).ok(),
        Err(error) => {
            let valid = error.valid_up_to();
            let invalid = error.error_len().is_some();
            if invalid || final_read {
                let text = String::from_utf8_lossy(bytes).into_owned();
                bytes.clear();
                return Some(text);
            }
            (valid > 0).then(|| {
                String::from_utf8(bytes.drain(..valid).collect())
                    .expect("the UTF-8 validator marked this prefix valid")
            })
        }
    }
}

#[cfg(unix)]
fn stream_claude_task_output(
    path: PathBuf,
    key: BackgroundWorkKey,
    events: DriverEventSender,
    stop: Arc<AtomicBool>,
) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let mut pending_utf8 = Vec::new();
    let mut final_pass = false;
    loop {
        let mut chunk = Vec::new();
        if file.read_to_end(&mut chunk).is_err() {
            break;
        }
        pending_utf8.extend_from_slice(&chunk);
        let stopping = stop.load(Ordering::Acquire);
        if let Some(delta) = drain_utf8_output(&mut pending_utf8, stopping && final_pass)
            && !delta.is_empty()
        {
            let _ = events.send(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::OutputDelta {
                    key: key.clone(),
                    delta,
                },
            ));
        }
        if stopping {
            if final_pass {
                break;
            }
            // The notification and the output-file close are adjacent but
            // originate on different tasks. One final poll closes that race.
            final_pass = true;
        }
        thread::sleep(CLAUDE_TASK_OUTPUT_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn stream_claude_task_output_when_ready(
    session_id: String,
    task_id: String,
    key: BackgroundWorkKey,
    events: DriverEventSender,
    stop: Arc<AtomicBool>,
) {
    loop {
        // Check for the file before honoring stop: if Claude creates and
        // completes a short task between polls, the final pass still captures
        // its output for the retained background-process entry.
        if let Some(path) = claude_task_output_path(&session_id, &task_id) {
            stream_claude_task_output(path, key, events, stop);
            return;
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(CLAUDE_TASK_OUTPUT_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn start_claude_task_output_tail(
    session_id: &str,
    item: &BackgroundWorkItem,
    events: &DriverEventSender,
    state: &mut ClaudeStreamState,
) {
    if item.key.kind != BackgroundWorkKind::Process
        || state
            .task_output_tails
            .0
            .contains_key(&item.key.provider_id)
    {
        return;
    }
    let session_id = session_id.to_owned();
    let task_id = item.key.provider_id.clone();
    let key = item.key.clone();
    let events = events.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_task_id = task_id.clone();
    let spawned = thread::Builder::new()
        .name("waku-claude-task-output".into())
        .spawn(move || {
            stream_claude_task_output_when_ready(
                session_id,
                thread_task_id,
                key,
                events,
                thread_stop,
            )
        });
    if spawned.is_ok() {
        state.task_output_tails.0.insert(task_id, stop);
    }
}

#[cfg(not(unix))]
fn start_claude_task_output_tail(
    _session_id: &str,
    _item: &BackgroundWorkItem,
    _events: &DriverEventSender,
    _state: &mut ClaudeStreamState,
) {
}

/// The context window of the model that served this turn, from the result
/// message's per-model usage map. Falls back to the largest reported window
/// when the model key does not line up.
fn context_window_from_result(value: &Value, last_model: Option<&str>) -> Option<u64> {
    let models = value.get("modelUsage")?.as_object()?;
    if let Some(model) = last_model {
        for (key, entry) in models {
            if key == model || entry.get("canonicalModel").and_then(Value::as_str) == Some(model) {
                return entry.get("contextWindow").and_then(Value::as_u64);
            }
        }
    }
    models
        .values()
        .filter_map(|entry| entry.get("contextWindow").and_then(Value::as_u64))
        .max()
}

fn claude_task_id(value: &Value) -> Option<&str> {
    value
        .get("task_id")
        .or_else(|| value.get("taskId"))
        .or_else(|| value.pointer("/task/id"))
        .and_then(Value::as_str)
}

fn claude_task_status(value: &Value) -> BackgroundWorkStatus {
    let status = value
        .get("status")
        .or_else(|| value.pointer("/task/status"))
        .or_else(|| value.pointer("/patch/status"))
        .and_then(Value::as_str)
        .unwrap_or("running")
        .to_ascii_lowercase();
    match status.as_str() {
        "pending" | "starting" => BackgroundWorkStatus::Starting,
        "completed" | "complete" | "succeeded" | "success" => BackgroundWorkStatus::Completed,
        "failed" | "errored" | "error" => BackgroundWorkStatus::Failed,
        "killed" | "cancelled" | "canceled" | "stopped" | "interrupted" => {
            BackgroundWorkStatus::Stopped
        }
        _ => BackgroundWorkStatus::Running,
    }
}

fn claude_task_kind(value: &Value, state: &ClaudeStreamState) -> BackgroundWorkKind {
    if let Some(task_id) = claude_task_id(value)
        && let Some(kind) = state.background_task_kinds.get(task_id)
    {
        return *kind;
    }
    let task_type = value
        .get("task_type")
        .or_else(|| value.get("taskType"))
        .or_else(|| value.get("subagent_type"))
        .or_else(|| value.get("subagentType"))
        .or_else(|| value.get("agentType"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if task_type == "monitor" {
        return BackgroundWorkKind::Monitor;
    }
    if task_type.contains("agent")
        || task_type.contains("teammate")
        || task_type.contains("workflow")
        || task_type == "remote"
    {
        return BackgroundWorkKind::Subagent;
    }
    let tool_use_id = value
        .get("tool_use_id")
        .or_else(|| value.get("toolUseId"))
        .and_then(Value::as_str);
    if tool_use_id
        .and_then(|id| state.tools.get(id))
        .is_some_and(|(_, _, wire_name, _)| wire_name.eq_ignore_ascii_case("monitor"))
    {
        BackgroundWorkKind::Monitor
    } else {
        BackgroundWorkKind::Process
    }
}

/// Progress summaries and errors may span paragraphs; the detail row under a
/// task is a one-liner.
fn one_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut short = line.chars().take(160).collect::<String>();
    if short.len() < line.len() {
        short.push('…');
    }
    Some(short)
}

fn claude_task_item(
    subtype: &str,
    value: &Value,
    state: &ClaudeStreamState,
) -> Option<BackgroundWorkItem> {
    let task_id = claude_task_id(value)?.to_owned();
    let kind = claude_task_kind(value, state);
    let wire_description = value
        .get("description")
        .or_else(|| value.get("subject"))
        .or_else(|| value.get("workflow_name"))
        .or_else(|| value.get("workflowName"))
        .or_else(|| value.pointer("/task/description"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    // Only the start names a task; on later events `description` is the
    // agent's current activity line ("Running …") and `summary` can be the
    // whole final report. An empty title keeps the stored one on upsert.
    let title = if subtype == "task_started" {
        wire_description
            .map(str::to_owned)
            .unwrap_or_else(|| match kind {
                BackgroundWorkKind::Subagent => tr!("background.subagent"),
                BackgroundWorkKind::Monitor => tr!("background.monitor"),
                BackgroundWorkKind::Process => tr!("background.process"),
            })
    } else {
        value
            .pointer("/patch/description")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .unwrap_or_default()
    };
    let mut status = claude_task_status(value);
    if status == BackgroundWorkStatus::Running && kind == BackgroundWorkKind::Monitor {
        status = BackgroundWorkStatus::Monitoring;
    }
    let mut item = BackgroundWorkItem::new(kind, task_id.clone(), title, status);
    item.background = value
        .get("is_backgrounded")
        .or_else(|| value.get("isBackgrounded"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    item.can_stop = status.is_live();
    item.origin_activity_id = value
        .get("tool_use_id")
        .or_else(|| value.get("toolUseId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    item.role = value
        .get("subagent_type")
        .or_else(|| value.get("subagentType"))
        .or_else(|| value.get("agentType"))
        .or_else(|| value.get("task_type"))
        .or_else(|| value.get("taskType"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    item.model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Subagent prompts ride `task_started`, but detached Bash tasks only link
    // back to the originating tool call by id. Preserve either as the detail
    // surface's Prompt/Command value.
    item.command = value
        .get("prompt")
        .or_else(|| value.get("command"))
        .or_else(|| value.pointer("/task/command"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            item.origin_activity_id
                .as_deref()
                .and_then(|id| state.tools.get(id))
                .and_then(|(_, _, _, command)| command.clone())
        });
    // The settle notification's summary is the subagent's final report and
    // belongs in the output pane — unless the live transcript already
    // streamed there.
    if subtype == "task_notification"
        && kind == BackgroundWorkKind::Subagent
        && !state.streamed_task_output.contains(&task_id)
    {
        item.output = value
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
    }
    let progress_line = (subtype != "task_notification")
        .then(|| value.get("summary").and_then(Value::as_str))
        .flatten()
        .filter(|text| !text.is_empty())
        .or_else(|| {
            (subtype != "task_started")
                .then_some(wire_description)
                .flatten()
                .filter(|text| {
                    state
                        .task_descriptions
                        .get(&task_id)
                        .is_none_or(|original| original != text)
                })
        });
    item.detail = progress_line
        .or_else(|| {
            value
                .get("last_tool_name")
                .or_else(|| value.get("lastToolName"))
                .or_else(|| value.get("error"))
                .or_else(|| value.pointer("/patch/error"))
                .or_else(|| value.get("output_file"))
                .or_else(|| value.get("outputFile"))
                .and_then(Value::as_str)
        })
        .and_then(one_line);
    item.control_id = Some(task_id);
    item.duration_ms = value
        .pointer("/usage/duration_ms")
        .or_else(|| value.get("duration_ms"))
        .and_then(Value::as_u64);
    item.updated_at_ms = unix_time_millis();
    Some(item)
}

fn handle_claude_system(
    value: &Value,
    session_id: &str,
    events: &DriverEventSender,
    state: &mut ClaudeStreamState,
) {
    let subtype = value.get("subtype").and_then(Value::as_str);
    if subtype == Some("background_tasks_changed") {
        let items = value
            .get("background_tasks")
            .or_else(|| value.get("backgroundTasks"))
            .or_else(|| value.get("tasks"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let bare_id = entry.as_str();
                let task_id = bare_id
                    .map(str::to_owned)
                    .or_else(|| claude_task_id(entry).map(str::to_owned))?;
                let known_kind = state.background_task_kinds.get(&task_id).copied();
                // The level signal normally precedes task_started. Bare IDs
                // carry no kind, so wait for that edge instead of briefly
                // inventing a Process entry beside the real Subagent entry.
                let kind = known_kind
                    .or_else(|| bare_id.is_none().then(|| claude_task_kind(entry, state)))?;
                let mut item = BackgroundWorkItem::new(
                    kind,
                    task_id.clone(),
                    match kind {
                        BackgroundWorkKind::Subagent => tr!("background.subagent"),
                        BackgroundWorkKind::Monitor => tr!("background.monitor"),
                        BackgroundWorkKind::Process => tr!("background.process"),
                    },
                    match kind {
                        BackgroundWorkKind::Monitor => BackgroundWorkStatus::Monitoring,
                        BackgroundWorkKind::Process | BackgroundWorkKind::Subagent => {
                            BackgroundWorkStatus::Running
                        }
                    },
                );
                item.background = true;
                item.can_stop = true;
                item.control_id = Some(task_id);
                Some(item)
            })
            .collect();
        let _ = events.send(DriverEvent::BackgroundWork(
            BackgroundWorkEvent::ReconcileLive { items },
        ));
        return;
    }
    if let Some(subtype @ ("task_started" | "task_progress" | "task_updated" | "task_notification")) =
        subtype
        && let Some(item) = claude_task_item(subtype, value, state)
    {
        let task_id = item.key.provider_id.clone();
        state
            .background_task_kinds
            .insert(task_id.clone(), item.key.kind);
        if subtype == "task_started"
            && let Some(tool_use_id) = item.origin_activity_id.clone()
        {
            state.subagent_tasks.insert(tool_use_id, task_id.clone());
        }
        let output_tail_item = (subtype == "task_started"
            && item.key.kind == BackgroundWorkKind::Process)
            .then(|| item.clone());
        let stop_output_tail = !item.status.is_live();
        // Remember what the task is called: `task_started` names it, and a
        // `task_updated` patch is an explicit rename. Later activity lines
        // that merely repeat this stay out of the detail row.
        let described = match subtype {
            "task_started" => value.get("description"),
            _ => value.pointer("/patch/description"),
        };
        if let Some(description) = described
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            state
                .task_descriptions
                .insert(task_id.clone(), description.to_owned());
        }
        let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
            item,
        )));
        // Establish the registry item before its tail thread can enqueue the
        // first delta; an output-before-start race would otherwise discard it.
        if let Some(item) = output_tail_item {
            start_claude_task_output_tail(session_id, &item, events, state);
        } else if stop_output_tail && let Some(stop) = state.task_output_tails.0.remove(&task_id) {
            stop.store(true, Ordering::Release);
        }
    }
}

/// Renders a subagent message into its task's output pane: narrative text
/// as-is, tool calls as single `› tool · subject` lines.
fn forward_subagent_transcript(
    parent_tool_use_id: &str,
    value: &Value,
    events: &impl DriverEventSink,
    state: &mut ClaudeStreamState,
) {
    let Some(task_id) = state.subagent_tasks.get(parent_tool_use_id).cloned() else {
        return;
    };
    let Some(content) = value.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    let mut delta = String::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    delta.push_str(text);
                    delta.push_str("\n\n");
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let input = block.get("input");
                let subject = activity::input_title(input).or_else(|| {
                    input.and_then(|input| {
                        [
                            "command",
                            "file_path",
                            "path",
                            "pattern",
                            "query",
                            "url",
                            "description",
                        ]
                        .into_iter()
                        .find_map(|key| input.get(key).and_then(Value::as_str))
                        .and_then(one_line)
                    })
                });
                match subject {
                    Some(subject) => delta.push_str(&format!("› {name} · {subject}\n")),
                    None => delta.push_str(&format!("› {name}\n")),
                }
            }
            _ => {}
        }
    }
    if delta.is_empty() {
        return;
    }
    let kind = state
        .background_task_kinds
        .get(&task_id)
        .copied()
        .unwrap_or(BackgroundWorkKind::Subagent);
    state.streamed_task_output.insert(task_id.clone());
    let _ = events.send(DriverEvent::BackgroundWork(
        BackgroundWorkEvent::OutputDelta {
            key: BackgroundWorkKey::new(kind, task_id),
            delta,
        },
    ));
}

#[allow(clippy::too_many_arguments)]
fn handle_message(
    value: &Value,
    session_id: &str,
    events: &DriverEventSender,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    auto_approve: bool,
    state: &mut ClaudeStreamState,
) {
    match value.get("type").and_then(Value::as_str) {
        Some("system") => {
            // The init handshake carries the CLI's own command registry —
            // built-ins, custom commands, plugins and skills alike.
            if value.get("subtype").and_then(Value::as_str) == Some("init")
                && let Some(commands) = value.get("slash_commands").and_then(Value::as_array)
            {
                let commands = commands
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| crate::model::ReportedCommand {
                        name: name.to_owned(),
                        description: String::new(),
                    })
                    .collect::<Vec<_>>();
                if !commands.is_empty() {
                    let _ = events.send(DriverEvent::AvailableCommands(commands));
                }
            }
            handle_claude_system(value, session_id, events, state);
        }
        Some("control_request") => {
            if value.pointer("/request/subtype").and_then(Value::as_str) == Some("can_use_tool") {
                if !request_user_input(value, events, state) {
                    request_permission(value, events, commands, auto_approve);
                }
            }
        }
        Some("control_response") => {
            let Some(request_id) = value
                .pointer("/response/request_id")
                .or_else(|| value.get("request_id"))
                .and_then(Value::as_str)
            else {
                return;
            };
            let Some(key) = state.pending_task_stops.lock().remove(request_id) else {
                return;
            };
            let subtype = value.pointer("/response/subtype").and_then(Value::as_str);
            let stop_status = value
                .pointer("/response/response/status")
                .or_else(|| value.pointer("/response/status"))
                .or_else(|| value.pointer("/response/response"))
                .and_then(Value::as_str);
            if subtype == Some("error") || matches!(stop_status, Some("not_found" | "not_running"))
            {
                let message = value
                    .pointer("/response/error")
                    .or_else(|| value.pointer("/response/response/message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| tr!("background.stop_not_running"));
                let _ = events.send(DriverEvent::BackgroundWork(
                    BackgroundWorkEvent::StopFailed { key, message },
                ));
            } else {
                let mut item = BackgroundWorkItem::new(
                    key.kind,
                    key.provider_id.clone(),
                    "",
                    BackgroundWorkStatus::Stopped,
                );
                item.key = key;
                item.background = true;
                let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
                    item,
                )));
            }
        }
        Some("stream_event") => {
            // A subagent's partial stream must not feed — or re-arm — the
            // main message; its transcript lands via the completed messages.
            if value
                .get("parent_tool_use_id")
                .and_then(Value::as_str)
                .is_some()
            {
                return;
            }
            let event = value.get("event").unwrap_or(&Value::Null);
            // Each assistant message re-arms the delta fallback.
            if event.get("type").and_then(Value::as_str) == Some("message_start") {
                state.saw_text_delta = false;
                state.saw_reasoning_delta = false;
            }
            let delta = event.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        state.saw_text_delta = true;
                        let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                    }
                }
                Some("thinking_delta") => {
                    if let Some(text) = delta
                        .get("thinking")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        state.saw_reasoning_delta = true;
                        let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                    }
                }
                _ => {}
            }
        }
        Some("assistant") => {
            // A subagent message (a set `parent_tool_use_id`) runs in its own
            // context and belongs to its task's output pane, not this
            // session's transcript or usage meter.
            if let Some(parent) = value.get("parent_tool_use_id").and_then(Value::as_str) {
                forward_subagent_transcript(parent, value, events, state);
                return;
            }
            if let Some(usage) = value.pointer("/message/usage") {
                if let Some(model) = value.pointer("/message/model").and_then(Value::as_str) {
                    state.last_assistant_model = Some(model.to_owned());
                }
                if let Some(tokens) = super::support::claude_context_tokens(usage) {
                    let _ = events.send(DriverEvent::UsageUpdated {
                        context_tokens: Some(tokens),
                        context_window: None,
                    });
                }
            }
            let Some(content) = value.pointer("/message/content").and_then(Value::as_array) else {
                return;
            };
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") if !state.saw_text_delta => {
                        if let Some(text) = block
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                        }
                    }
                    Some("thinking") if !state.saw_reasoning_delta => {
                        if let Some(text) = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                        }
                    }
                    Some("tool_use") => {
                        let id = block.get("id").and_then(Value::as_str).map(str::to_owned);
                        let wire_title = block
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| tr!("activity.tool"));
                        let kind = super::support::classify_tool(&wire_title);
                        // The Agent tool names its work in `description`;
                        // without this the row reads as a bare "Agent".
                        let title = activity::input_title(block.get("input"))
                            .or_else(|| {
                                (wire_title.eq_ignore_ascii_case("task")
                                    || wire_title.eq_ignore_ascii_case("agent"))
                                .then(|| {
                                    block
                                        .pointer("/input/description")
                                        .and_then(Value::as_str)
                                        .map(str::trim)
                                        .filter(|text| !text.is_empty())
                                        .map(str::to_owned)
                                })
                                .flatten()
                            })
                            .unwrap_or_else(|| wire_title.clone());
                        if let Some(id) = &id {
                            let command = block
                                .pointer("/input/command")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|command| !command.is_empty())
                                .map(str::to_owned);
                            state.tools.insert(
                                id.clone(),
                                (kind, title.clone(), wire_title.clone(), command),
                            );
                        }
                        let _ = events.send(DriverEvent::RichActivity(activity::tool_activity(
                            id,
                            kind,
                            title,
                            block.get("input"),
                            None,
                            None,
                            false,
                            false,
                        )));
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            // A subagent's tool results echo on the main channel too; its
            // transcript lives in the task's output pane, not here.
            if value
                .get("parent_tool_use_id")
                .and_then(Value::as_str)
                .is_some()
            {
                return;
            }
            // `--replay-user-messages` echoes Waku's own prompts back; they are
            // an acknowledgement, not transcript content.
            if value.get("isReplay").and_then(Value::as_bool) == Some(true) {
                return;
            }
            let Some(content) = value.pointer("/message/content").and_then(Value::as_array) else {
                return;
            };
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                let id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (kind, title, _, _) = id
                    .as_ref()
                    .and_then(|id| state.tools.remove(id))
                    .unwrap_or((
                        ActivityKind::Tool,
                        "Tool".to_owned(),
                        "Tool".to_owned(),
                        None,
                    ));
                let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
                // The result text of an edit is only a confirmation sentence.
                // The positioned hunks Claude actually applied ride alongside
                // it, so hand them over as the activity's source and the diff
                // lands in the transcript with real line numbers.
                let patch = (kind == ActivityKind::FileChange)
                    .then(|| value.get("tool_use_result"))
                    .flatten()
                    .filter(|result| !result.is_null());
                let item = activity::tool_activity(
                    id,
                    kind,
                    title,
                    None,
                    block.get("content"),
                    block.get("content"),
                    failed,
                    true,
                )
                .with_activity_source(patch);
                let _ = events.send(DriverEvent::RichActivity(item));
            }
        }
        Some("result") => {
            let failed = value.get("is_error").and_then(Value::as_bool) == Some(true);
            if failed && let Some(result) = value.get("result").and_then(Value::as_str) {
                let _ = events.send(DriverEvent::Error(result.to_owned()));
            }
            // Ahead of TurnFinished, whose forced save should include it.
            if let Some(window) =
                context_window_from_result(value, state.last_assistant_model.as_deref())
            {
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens: None,
                    context_window: Some(window),
                });
            }
            if !std::mem::take(&mut *turn_active.lock()) {
                return;
            }
            // Claude writes its generated title and rewind checkpoint to the
            // same native transcript as the turn settles. Read it once, ahead
            // of TurnFinished, so that event's forced save includes both.
            if let Ok(metadata) = crate::claude_session::session_metadata(session_id) {
                if let Some(title) = metadata.title
                    && state.last_auto_title.as_deref() != Some(title.as_str())
                {
                    state.last_auto_title = Some(title.clone());
                    let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title)));
                }
                if let Some(message_id) = metadata.latest_message_id {
                    let _ = events.send(DriverEvent::Connected {
                        provider_cursor: Some(ProviderResumeCursor::Claude {
                            session_id: session_id.to_owned(),
                            resume_at: Some(message_id),
                        }),
                    });
                }
            }
            let _ = events.send(DriverEvent::TurnFinished {
                success: !failed,
                summary: None,
            });
        }
        // `system` status/thinking-token notices and `rate_limit_event` are not
        // transcript content.
        _ => {}
    }
}

fn request_user_input(
    value: &Value,
    events: &impl DriverEventSink,
    state: &ClaudeStreamState,
) -> bool {
    let request = value.get("request").unwrap_or(&Value::Null);
    if request.get("tool_name").and_then(Value::as_str) != Some("AskUserQuestion") {
        return false;
    }
    let Some(request_id) = value.get("request_id").and_then(Value::as_str) else {
        return true;
    };
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, question)| {
            let text = question
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if text.is_empty() {
                return None;
            }
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
                // Claude's SDK resolves answers by the complete question text.
                id: text.to_owned(),
                header: question
                    .get("header")
                    .and_then(Value::as_str)
                    .filter(|header| !header.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Question {}", index + 1)),
                question: text.to_owned(),
                options,
                multi_select: question
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    if questions.is_empty() {
        return true;
    }
    state
        .pending_user_inputs
        .lock()
        .insert(request_id.to_owned(), input);
    let _ = events.send(DriverEvent::UserInputRequested {
        request_id: request_id.to_owned(),
        questions,
    });
    true
}

fn request_permission(
    value: &Value,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    auto_approve: bool,
) {
    let Some(request_id) = value.get("request_id").and_then(Value::as_str) else {
        return;
    };
    if auto_approve {
        let _ = commands.send(CommandMessage::Respond {
            request_id: request_id.to_owned(),
            option_id: "allow".into(),
        });
        return;
    }

    let request = value.get("request").unwrap_or(&Value::Null);
    let tool = request
        .get("display_name")
        .or_else(|| request.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("permission.a_tool"));
    // The agent says why it is asking; that reason is what the answer rests on.
    let detail = request
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            request
                .get("blocked_path")
                .and_then(Value::as_str)
                .map(|path| tr!("permission.blocked_path", path = path))
        })
        .unwrap_or_else(|| tr!("permission.agent_wants_to_run", tool = tool.as_str()));
    let _ = events.send(DriverEvent::Permission {
        request_id: request_id.to_owned(),
        title: activity::input_title(request.get("input"))
            .unwrap_or_else(|| tr!("permission.run_tool", tool = tool.as_str())),
        detail,
        options: vec![
            PermissionOption {
                id: "allow".into(),
                label: tr!("permission.allow_once"),
                allow: true,
            },
            PermissionOption {
                id: "deny".into(),
                label: tr!("common.deny"),
                allow: false,
            },
        ],
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_command_requests_readable_reasoning_summary() {
        let mut command = Command::new("/usr/bin/true");
        configure_stream_command(
            &mut command,
            RuntimeMode::AutoAcceptEdits,
            InteractionMode::Build,
        );
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(
            arguments
                .windows(2)
                .any(|arguments| { arguments == ["--thinking-display", "summarized"] })
        );
    }

    fn harness() -> (
        DriverEventSender,
        crossbeam_channel::Receiver<DriverEvent>,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        Mutex<bool>,
        ClaudeStreamState,
    ) {
        let (events, event_rx) = crate::driver::test_event_channel();
        let (commands, command_rx) = unbounded();
        (
            events,
            event_rx,
            commands,
            command_rx,
            Mutex::new(true),
            ClaudeStreamState::default(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn locates_claudes_native_task_output_across_workspace_slugs() {
        let root = std::env::temp_dir().join(format!("waku-claude-output-test-{}", Uuid::new_v4()));
        let output = root
            .join("-Users-egoist-dev-waku")
            .join("session-live")
            .join("tasks")
            .join("task-live.output");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(&output, "first\n").unwrap();

        assert_eq!(
            claude_task_output_path_in(&root, "session-live", "task-live"),
            Some(output)
        );
        assert_eq!(
            claude_task_output_path_in(&root, "session-live", "../escape"),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn live_task_output_preserves_split_utf8() {
        let bytes = "first 界".as_bytes();
        let split = bytes.len() - 1;
        let mut pending = bytes[..split].to_vec();

        assert_eq!(
            drain_utf8_output(&mut pending, false).as_deref(),
            Some("first ")
        );
        pending.extend_from_slice(&bytes[split..]);
        assert_eq!(
            drain_utf8_output(&mut pending, false).as_deref(),
            Some("界")
        );
        assert!(pending.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn native_task_output_streams_before_completion() {
        let root = std::env::temp_dir().join(format!("waku-claude-tail-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("task.output");
        std::fs::write(&output, "first\n").unwrap();
        let (events, event_rx) = crate::driver::test_event_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Process, "task-live");
        let tail_stop = stop.clone();
        let tail = thread::spawn(move || {
            stream_claude_task_output(output, key, events, tail_stop);
        });

        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { delta, .. })
                if delta == "first\n"
        ));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("task.output"))
            .unwrap();
        file.write_all("second 界\n".as_bytes()).unwrap();
        file.flush().unwrap();
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { delta, .. })
                if delta == "second 界\n"
        ));

        stop.store(true, Ordering::Release);
        tail.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Drives the real CLI through the actual driver, including a second turn
    /// on the same process — the whole point of the transport. Ignored by
    /// default: needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated claude"]
    fn claude_streaming_session_against_the_real_cli() {
        let binary =
            crate::command_env::find_executable("claude").expect("claude is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = ClaudeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
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
        .expect("the streaming session should start");

        assert!(matches!(
            event_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("the driver should report its session"),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Claude { .. })
            }
        ));

        let collect = |driver: &ClaudeDriver, prompt: &str| -> String {
            driver.prompt(prompt.to_owned());
            let mut text = String::new();
            while let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(180)) {
                match event {
                    DriverEvent::TextDelta(delta) => text.push_str(&delta),
                    DriverEvent::TurnFinished { success, .. } => {
                        assert!(success, "the turn should settle successfully");
                        return text;
                    }
                    DriverEvent::Error(error) => panic!("the CLI reported: {error}"),
                    _ => {}
                }
            }
            panic!("the turn never settled");
        };

        let first = collect(&driver, "Reply with exactly: BANANA. Use no tools.");
        assert!(first.contains("BANANA"), "expected a reply, got {first:?}");

        // The second turn proves one process is serving the conversation and
        // kept its context — a per-turn spawn could not answer this.
        let second = collect(
            &driver,
            "What word did I just ask you to reply with? Answer with that word only.",
        );
        assert!(
            second.contains("BANANA"),
            "the session should retain context across turns, got {second:?}"
        );
    }

    /// Proves steering through the actual driver: the message injected while
    /// the Bash tool sleeps lands inside the same turn — one SteerAccepted,
    /// one TurnFinished, and a reply that honors both instructions. Ignored by
    /// default: needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated claude"]
    fn claude_steering_folds_a_mid_turn_message_into_the_running_turn() {
        let binary =
            crate::command_env::find_executable("claude").expect("claude is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = ClaudeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some("claude-haiku-4-5-20251001".into()),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the streaming session should start");

        driver.prompt(
            "Use the Bash tool to run exactly `sleep 6` (nothing else). \
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
                DriverEvent::Error(error) => panic!("the CLI reported: {error}"),
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
    fn steering_is_advertised_and_rides_the_command_channel() {
        let (commands, command_rx) = unbounded();
        let driver = ClaudeDriver {
            commands,
            pending_user_inputs: Arc::new(Mutex::new(HashMap::new())),
            mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Build,
        };

        assert!(driver.supports_steer());
        driver.steer("Focus on the failing tests first".into());
        match command_rx.try_recv() {
            Ok(CommandMessage::Steer(text)) => {
                assert_eq!(text, "Focus on the failing tests first");
            }
            Ok(_) => panic!("expected a steer command"),
            Err(_) => panic!("no command was sent"),
        }
    }

    #[test]
    fn stop_task_uses_claudes_native_control_request() {
        assert_eq!(
            stop_task_request(7, "agent-42"),
            json!({
                "type": "control_request",
                "request_id": "waku-7",
                "request": {"subtype": "stop_task", "task_id": "agent-42"}
            })
        );
    }

    #[test]
    fn stop_task_control_errors_restore_the_live_item() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "agent-42");
        state
            .pending_task_stops
            .lock()
            .insert("waku-8".into(), key.clone());
        handle_message(
            &json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": "waku-8",
                    "response": {"status": "not_running"}
                }
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(DriverEvent::BackgroundWork(BackgroundWorkEvent::StopFailed {
                key: failed_key,
                ..
            })) if failed_key == key
        ));
    }

    #[test]
    fn background_bash_task_keeps_its_originating_command() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        handle_message(
            &json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "id": "toolu-bash",
                    "name": "Bash",
                    "input": {
                        "command": "cargo check && cargo test --bin waku",
                        "description": "Type-check and run full suite"
                    }
                }]}
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(DriverEvent::RichActivity(_))
        ));

        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_started",
                "task_id": "bash-42",
                "tool_use_id": "toolu-bash",
                "task_type": "local_bash",
                "description": "Type-check and run full suite",
                "is_backgrounded": true
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(started)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_started should surface a background item");
        };
        assert_eq!(started.key.kind, BackgroundWorkKind::Process);
        assert_eq!(
            started.command.as_deref(),
            Some("cargo check && cargo test --bin waku")
        );
    }

    #[test]
    fn task_lifecycle_surfaces_subagents_independently_of_the_turn() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let tool = json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use",
                "id": "toolu-agent",
                "name": "Task",
                "input": {"description": "Inspect the parser"}
            }]}
        });
        handle_message(&tool, "s", &events, &commands, &turn, true, &mut state);
        let _ = event_rx.try_recv().unwrap();

        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_started",
                "task_id": "agent-42",
                "tool_use_id": "toolu-agent",
                "task_type": "local_agent",
                "subagent_type": "Explore",
                "description": "Inspect the parser"
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(started)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_started should surface a background item");
        };
        assert_eq!(started.key.kind, BackgroundWorkKind::Subagent);
        assert_eq!(started.key.provider_id, "agent-42");
        assert_eq!(started.origin_activity_id.as_deref(), Some("toolu-agent"));
        assert_eq!(started.status, BackgroundWorkStatus::Running);

        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_updated",
                "task_id": "agent-42",
                "patch": {"status": "completed"}
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(completed)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_updated should settle the background item");
        };
        assert_eq!(completed.key.kind, BackgroundWorkKind::Subagent);
        assert_eq!(completed.status, BackgroundWorkStatus::Completed);
    }

    /// Wire shapes read off CLI 2.1.226: progress events reuse `description`
    /// for the agent's current activity line, and the settle notification's
    /// `summary` is the whole final report. Neither may retitle the task —
    /// the activity line is the detail row, the report is the output pane.
    #[test]
    fn task_progress_and_final_report_never_retitle_the_task() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_started",
                "task_id": "agent-42",
                "tool_use_id": "toolu-agent",
                "task_type": "local_agent",
                "subagent_type": "Explore",
                "description": "Map right panel + UI stack",
                "prompt": "Dig through src/app/right_panel.rs and report the stack."
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(started)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_started should surface a background item");
        };
        assert_eq!(started.title, "Map right panel + UI stack");
        assert_eq!(
            started.command.as_deref(),
            Some("Dig through src/app/right_panel.rs and report the stack."),
            "the launch prompt should ride into the detail surface"
        );

        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_progress",
                "task_id": "agent-42",
                "tool_use_id": "toolu-agent",
                "description": "Running Terminal/Browser entity patterns",
                "subagent_type": "Explore",
                "last_tool_name": "Bash",
                "usage": {"total_tokens": 900, "tool_uses": 12, "duration_ms": 61_000}
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(progress)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_progress should update the background item");
        };
        assert!(
            progress.title.is_empty(),
            "the activity line must not replace the stored title, got {:?}",
            progress.title
        );
        assert_eq!(
            progress.detail.as_deref(),
            Some("Running Terminal/Browser entity patterns")
        );

        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_notification",
                "task_id": "agent-42",
                "tool_use_id": "toolu-agent",
                "status": "completed",
                "output_file": "",
                "summary": "I have a complete map.\n\n# Waku Right Panel\nDetails…"
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(settled)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_notification should settle the background item");
        };
        assert_eq!(settled.status, BackgroundWorkStatus::Completed);
        assert!(
            settled.title.is_empty(),
            "the report must not become the title"
        );
        assert!(
            settled.detail.is_none(),
            "the report must not become the detail row"
        );
        assert_eq!(
            settled.output.as_deref(),
            Some("I have a complete map.\n\n# Waku Right Panel\nDetails…"),
            "the final report belongs in the output pane"
        );
    }

    #[test]
    fn subagent_messages_stream_into_the_tasks_output_pane() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_started",
                "task_id": "agent-42",
                "tool_use_id": "toolu-agent",
                "task_type": "local_agent",
                "description": "Map right panel + UI stack"
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let _ = event_rx.try_recv().unwrap();

        // The subagent's own turn: narrative text, a tool call, its result.
        handle_message(
            &json!({"type": "assistant", "parent_tool_use_id": "toolu-agent", "message": {
                "usage": {"input_tokens": 999_999, "output_tokens": 1},
                "content": [
                    {"type": "text", "text": "Scanning the panel."},
                    {"type": "tool_use", "id": "toolu-sub-1", "name": "Bash",
                     "input": {"command": "rg overlay src"}}
                ]
            }}),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        handle_message(
            &json!({"type": "user", "parent_tool_use_id": "toolu-agent", "message": {
                "content": [{"type": "tool_result", "tool_use_id": "toolu-sub-1", "content": "hits"}]
            }}),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert_eq!(
            seen.len(),
            1,
            "subagent content must not reach the transcript or usage meter"
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { key, delta }) = &seen[0]
        else {
            panic!("the subagent message should stream into its task output");
        };
        assert_eq!(key.provider_id, "agent-42");
        assert_eq!(key.kind, BackgroundWorkKind::Subagent);
        assert_eq!(delta, "Scanning the panel.\n\n› Bash · rg overlay src\n");

        // The settle notification's summary would duplicate the streamed
        // transcript; the pane keeps what it already has.
        handle_message(
            &json!({
                "type": "system",
                "subtype": "task_notification",
                "task_id": "agent-42",
                "status": "completed",
                "summary": "Scanning the panel."
            }),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(settled)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("task_notification should settle the background item");
        };
        assert!(settled.output.is_none());
    }

    #[test]
    fn subagent_partials_do_not_rearm_the_main_delta_fallback() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let wire = [
            json!({"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}}),
            // A concurrent subagent's partial stream interleaves mid-message.
            json!({"type":"stream_event","parent_tool_use_id":"toolu-agent","event":{"type":"message_start","message":{"role":"assistant"}}}),
            json!({"type":"stream_event","parent_tool_use_id":"toolu-agent","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"sub text"}}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}),
        ];
        for message in wire {
            handle_message(&message, "s", &events, &commands, &turn, true, &mut state);
        }
        let deltas = std::iter::from_fn(|| event_rx.try_recv().ok())
            .filter_map(|event| match event {
                DriverEvent::TextDelta(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            deltas,
            ["Hello"],
            "subagent partials leaked or re-armed the fallback"
        );
    }

    #[test]
    fn the_agent_tool_row_is_titled_by_its_description() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        handle_message(
            &json!({"type": "assistant", "message": {"content": [{
                "type": "tool_use",
                "id": "toolu-agent",
                "name": "Agent",
                "input": {"description": "Map right panel + UI stack", "prompt": "Dig in."}
            }]}}),
            "s",
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );
        let DriverEvent::RichActivity(item) = event_rx.try_recv().unwrap() else {
            panic!("the tool call should surface an activity");
        };
        assert_eq!(item.title, "Map right panel + UI stack");
    }

    #[test]
    fn access_modes_map_to_claude_permission_modes() {
        assert_eq!(
            permission_mode(RuntimeMode::Ask, InteractionMode::Build),
            "default"
        );
        assert_eq!(
            permission_mode(RuntimeMode::AutoAcceptEdits, InteractionMode::Build),
            "acceptEdits"
        );
        assert_eq!(
            permission_mode(RuntimeMode::FullAccess, InteractionMode::Build),
            "bypassPermissions"
        );
        assert_eq!(
            permission_mode(RuntimeMode::FullAccess, InteractionMode::Plan),
            "plan"
        );
    }

    #[test]
    fn streams_text_and_tools_and_ignores_its_own_replayed_prompt() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // Payloads copied from a live streaming-input session.
        let wire = [
            json!({"type":"system","subtype":"init","session_id":"s","tools":[]}),
            // Waku's own prompt, echoed by --replay-user-messages.
            json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"isReplay":true}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"pondering"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"I'll run that."}}}),
            json!({"type":"assistant","message":{"content":[
                {"type":"text","text":"I'll run that."},
                {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}
            ]}}),
            json!({"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu_1","content":"hi","is_error":false}
            ]}}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}),
        ];
        for message in wire {
            handle_message(&message, "s", &events, &commands, &turn, true, &mut state);
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(t) if t == "pondering"));
        assert!(matches!(&seen[1], DriverEvent::TextDelta(t) if t == "I'll run that."));
        // The completed assistant block must not repeat the streamed text.
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::Command && !item.complete));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
                if item.complete && item.output.as_deref() == Some("hi")));
        assert_eq!(seen.len(), 4, "replayed prompt or control noise leaked");
    }

    #[test]
    fn supervised_mode_asks_the_user_and_auto_modes_answer_themselves() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        // Shape observed from a live `--permission-prompt-tool stdio` session.
        let request = json!({
            "type": "control_request",
            "request_id": "fa01120e",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "display_name": "Bash",
                "input": {"command": "echo hi", "description": "Write probe file"},
                "description": "Write probe file",
                "blocked_path": "/tmp/probe.txt",
                "tool_use_id": "toolu_1"
            }
        });

        handle_message(&request, "s", &events, &commands, &turn, false, &mut state);
        let DriverEvent::Permission {
            request_id,
            detail,
            options,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "fa01120e");
        assert_eq!(detail, "Write probe file");
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["allow", "deny"]
        );
        assert!(command_rx.try_recv().is_err());

        handle_message(&request, "s", &events, &commands, &turn, true, &mut state);
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "allow");
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn ask_user_question_is_never_treated_as_an_auto_approvable_permission() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        let request = json!({
            "type": "control_request",
            "request_id": "ask-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "AskUserQuestion",
                "input": {
                    "questions": [{
                        "header": "Environment",
                        "question": "Where should this deploy?",
                        "options": [{
                            "label": "Preview",
                            "description": "Create a preview deployment"
                        }],
                        "multiSelect": false
                    }]
                }
            }
        });

        // Even Full Access cannot invent an answer to a content question.
        handle_message(&request, "s", &events, &commands, &turn, true, &mut state);
        let DriverEvent::UserInputRequested {
            request_id,
            questions,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("AskUserQuestion must reach the structured question UI");
        };
        assert_eq!(request_id, "ask-1");
        assert_eq!(questions[0].id, "Where should this deploy?");
        assert_eq!(questions[0].options[0].label, "Preview");
        assert!(command_rx.try_recv().is_err());
        assert!(state.pending_user_inputs.lock().contains_key("ask-1"));
    }

    #[test]
    fn the_result_message_settles_the_turn_exactly_once() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let result = json!({"type":"result","is_error":false,"stop_reason":"end_turn"});

        handle_message(&result, "s", &events, &commands, &turn, true, &mut state);
        // The checkpoint read is best-effort; the turn must settle regardless.
        let settled = std::iter::from_fn(|| event_rx.try_recv().ok())
            .filter(|event| matches!(event, DriverEvent::TurnFinished { .. }))
            .count();
        assert_eq!(settled, 1);
        assert!(!*turn.lock());

        handle_message(&result, "s", &events, &commands, &turn, true, &mut state);
        assert!(
            !std::iter::from_fn(|| event_rx.try_recv().ok())
                .any(|event| matches!(event, DriverEvent::TurnFinished { .. })),
            "a second result must not settle an already-finished turn"
        );
    }

    #[test]
    fn usage_flows_from_main_thread_messages_and_the_settled_result() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // Shapes captured from a live 2.1.223 stream.
        let wire = [
            // Main-thread call: the last iteration is the live context.
            json!({"type":"assistant","parent_tool_use_id":null,"message":{
                "model":"claude-fable-5",
                "usage":{
                    "input_tokens":900,"cache_read_input_tokens":70,
                    "cache_creation_input_tokens":20,"output_tokens":50,
                    "iterations":[{"input_tokens":2,"cache_read_input_tokens":100,
                        "cache_creation_input_tokens":10,"output_tokens":8}]
                },
                "content":[]}}),
            // A subagent runs its own context; it must not touch this meter.
            json!({"type":"assistant","parent_tool_use_id":"toolu_9","message":{
                "model":"claude-haiku-4-5-20251001",
                "usage":{"input_tokens":999999,"output_tokens":1},
                "content":[]}}),
            json!({"type":"result","is_error":false,"modelUsage":{
                "claude-haiku-4-5-20251001":{"contextWindow":200000,"canonicalModel":"claude-haiku-4-5"},
                "claude-fable-5":{"contextWindow":1000000,"canonicalModel":"claude-fable-5"}
            }}),
        ];
        for message in wire {
            handle_message(&message, "s", &events, &commands, &turn, true, &mut state);
        }

        let usage: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok())
            .filter_map(|event| match event {
                DriverEvent::UsageUpdated {
                    context_tokens,
                    context_window,
                } => Some((context_tokens, context_window)),
                _ => None,
            })
            .collect();
        // 2+100+10+8 from the last iteration, then the main model's window —
        // not the subagent's tokens, not the smaller subagent window.
        assert_eq!(usage, [(Some(120), None), (None, Some(1_000_000))]);
    }
}

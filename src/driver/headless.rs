use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use serde_json::Value;
use uuid::Uuid;

use crate::driver::{DriverControl, DriverStartOptions};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, ProviderKind, ProviderResumeCursor, RuntimeMode,
};

enum CommandMessage {
    Prompt(String),
    Shutdown,
}

pub struct HeadlessDriver {
    commands: Sender<CommandMessage>,
    active_pid: Arc<AtomicU32>,
}

impl HeadlessDriver {
    pub fn start(
        provider: ProviderKind,
        options: DriverStartOptions,
        events: Sender<DriverEvent>,
    ) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
            computer_use_enabled: _,
            provider_cursor: existing_cursor,
        } = options;
        if provider == ProviderKind::Amp
            && (mode != RuntimeMode::FullAccess || interaction_mode != InteractionMode::Build)
        {
            return Err(anyhow!(
                "Amp currently supports Build with Full access only"
            ));
        }
        let amp_fork_context = match existing_cursor.as_ref() {
            Some(ProviderResumeCursor::Amp { fork_context, .. })
                if provider == ProviderKind::Amp =>
            {
                fork_context.clone()
            }
            _ => None,
        };
        let cursor_fork_context = match existing_cursor.as_ref() {
            Some(ProviderResumeCursor::Cursor { fork_context, .. })
                if provider == ProviderKind::Cursor =>
            {
                fork_context.clone()
            }
            _ => None,
        };
        let existing_session_id = match existing_cursor.as_ref() {
            Some(cursor) if cursor.provider() == provider => {
                (!cursor.native_id().is_empty()).then(|| cursor.native_id().to_owned())
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
        let (commands, command_rx) = unbounded();
        let active_pid = Arc::new(AtomicU32::new(0));
        let worker_pid = active_pid.clone();

        thread::Builder::new()
            .name(format!("waku-{}-driver", provider.id()))
            .spawn(move || {
                let had_existing_session = existing_session_id.is_some();
                let mut provider_session_id = match provider {
                    ProviderKind::Claude | ProviderKind::Grok => {
                        Some(existing_session_id.unwrap_or_else(|| Uuid::new_v4().to_string()))
                    }
                    ProviderKind::Amp | ProviderKind::Cursor | ProviderKind::OpenCode => {
                        existing_session_id
                    }
                    _ => None,
                };
                let mut can_resume = had_existing_session;
                if provider_session_id.is_some() {
                    let _ = events.send(DriverEvent::Connected {
                        provider_cursor: provider_session_id.clone().map(|id| {
                            if provider == ProviderKind::Amp {
                                ProviderResumeCursor::Amp {
                                    thread_id: id,
                                    fork_context: amp_fork_context.clone(),
                                }
                            } else {
                                ProviderResumeCursor::from_session_id(provider, id)
                            }
                        }),
                    });
                }
                let mut amp_fork_context = amp_fork_context;
                let mut cursor_fork_context = cursor_fork_context;
                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(prompt) => {
                            let prompt = if provider == ProviderKind::Amp {
                                amp_fork_context
                                    .as_deref()
                                    .map(|context| {
                                        crate::amp_session::prompt_with_fork_context(
                                            context, &prompt,
                                        )
                                    })
                                    .unwrap_or(prompt)
                            } else if provider == ProviderKind::Cursor {
                                cursor_fork_context
                                    .as_deref()
                                    .map(|context| {
                                        crate::cursor_session::prompt_with_fork_context(
                                            context, &prompt,
                                        )
                                    })
                                    .unwrap_or(prompt)
                            } else {
                                prompt
                            };
                            if let Some(session_id) = run_prompt(
                                provider,
                                &binary,
                                &cwd,
                                mode,
                                interaction_mode,
                                model.as_deref(),
                                reasoning_effort.as_deref(),
                                service_tier.as_deref(),
                                provider_session_id.as_deref(),
                                can_resume,
                                prompt,
                                &events,
                                &worker_pid,
                            ) {
                                provider_session_id = Some(session_id);
                                can_resume = true;
                                amp_fork_context = None;
                                cursor_fork_context = None;
                            }
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })
            .context("failed to start provider driver thread")?;

        Ok(Self {
            commands,
            active_pid,
        })
    }
}

impl DriverControl for HeadlessDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Prompt(prompt));
    }

    fn cancel(&self) {
        let pid = self.active_pid.load(Ordering::Relaxed);
        if pid != 0 {
            #[cfg(unix)]
            {
                let _ = Command::new("/bin/kill")
                    .args(["-INT", &pid.to_string()])
                    .status();
            }
        }
    }

    fn respond(&self, _request_id: String, _option_id: String) {}

    fn rollback(&self, _turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        Err(anyhow!(
            "conversation rollback is not supported by this provider transport"
        ))
    }
}

impl Drop for HeadlessDriver {
    fn drop(&mut self) {
        self.cancel();
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_prompt(
    provider: ProviderKind,
    binary: &PathBuf,
    cwd: &PathBuf,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
    provider_session_id: Option<&str>,
    resume: bool,
    prompt: String,
    events: &Sender<DriverEvent>,
    active_pid: &AtomicU32,
) -> Option<String> {
    let _ = events.send(DriverEvent::TurnStarted);
    let previous_claude_message = (provider == ProviderKind::Claude)
        .then(|| {
            provider_session_id
                .and_then(|session_id| crate::claude_session::latest_message_id(session_id).ok())
                .flatten()
        })
        .flatten();
    let mut command = crate::command_env::command(binary);
    command.current_dir(cwd);
    let mut prompt_via_stdin = false;
    match provider {
        ProviderKind::Amp => {
            command.args(amp_args(
                model,
                reasoning_effort,
                service_tier,
                provider_session_id.filter(|_| resume),
            ));
            command.stdin(Stdio::piped());
            prompt_via_stdin = true;
        }
        ProviderKind::Claude => {
            command.args([
                "-p",
                &prompt,
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
                    "plan"
                } else {
                    match mode {
                        RuntimeMode::Ask => "default",
                        RuntimeMode::AutoAcceptEdits => "acceptEdits",
                        RuntimeMode::Auto => "auto",
                        RuntimeMode::FullAccess => "bypassPermissions",
                        RuntimeMode::Plan => unreachable!("handled above"),
                    }
                },
            ]);
            if mode == RuntimeMode::FullAccess && interaction_mode != InteractionMode::Plan {
                command.arg("--dangerously-skip-permissions");
            }
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            if let Some(reasoning_effort) = reasoning_effort {
                command.args(["--effort", reasoning_effort]);
            }
            if let Some(session_id) = provider_session_id {
                if resume {
                    command.args(["--resume", session_id]);
                } else {
                    command.args(["--session-id", session_id]);
                }
            }
        }
        ProviderKind::Cursor => {
            command.args(cursor_args(
                mode,
                interaction_mode,
                model,
                provider_session_id.filter(|_| resume),
            ));
            command.arg(&prompt);
        }
        ProviderKind::OpenCode => {
            command.args(["run", "--format", "json", "--thinking"]);
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
                command.args(["--agent", "plan"]);
            } else {
                match mode {
                    RuntimeMode::AutoAcceptEdits | RuntimeMode::Auto | RuntimeMode::FullAccess => {
                        command.arg("--auto");
                    }
                    RuntimeMode::Ask => {}
                    RuntimeMode::Plan => unreachable!("handled above"),
                }
            }
            if let Some(session_id) = provider_session_id {
                command.args(["--session", session_id]);
            }
            command.arg(&prompt);
        }
        ProviderKind::Grok => {
            command.args([
                "--no-auto-update",
                "-p",
                &prompt,
                "--output-format",
                "streaming-json",
            ]);
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
                command.args(["--permission-mode", "plan"]);
            } else {
                match mode {
                    RuntimeMode::Ask => {
                        // Grok's headless stream has no interactive permission
                        // response channel. Deny unapproved tools instead of
                        // blocking on a terminal prompt that Waku cannot answer.
                        command.args(["--permission-mode", "dontAsk"]);
                    }
                    RuntimeMode::AutoAcceptEdits | RuntimeMode::Auto | RuntimeMode::FullAccess => {
                        command.arg("--always-approve");
                    }
                    RuntimeMode::Plan => unreachable!("handled above"),
                }
            }
            if let Some(session_id) = provider_session_id {
                if resume {
                    command.args(["--resume", session_id]);
                } else {
                    command.args(["--session-id", session_id]);
                }
            }
        }
        ProviderKind::Codex | ProviderKind::Pi => return None,
    }
    let result = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match result {
        Ok(child) => child,
        Err(error) => {
            let _ = events.send(DriverEvent::Error(format!(
                "Failed to start {}: {error}",
                provider.display_name()
            )));
            let _ = events.send(DriverEvent::TurnFinished {
                success: false,
                summary: None,
            });
            return None;
        }
    };
    active_pid.store(child.id(), Ordering::Relaxed);
    if prompt_via_stdin {
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Amp stdin unavailable"))
            .and_then(|mut stdin| {
                stdin
                    .write_all(prompt.as_bytes())
                    .context("failed to send the prompt to Amp")
            });
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            active_pid.store(0, Ordering::Relaxed);
            let _ = events.send(DriverEvent::Error(error.to_string()));
            let _ = events.send(DriverEvent::TurnFinished {
                success: false,
                summary: Some("Amp could not receive the prompt.".into()),
            });
            return None;
        }
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stderr_events = events.clone();
    let provider_name = provider.display_name().to_owned();
    let stderr_thread = stderr.map(|stderr| {
        thread::spawn(move || {
            let lines = BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>();
            if !lines.is_empty() {
                let message = lines.join("\n");
                if message.to_ascii_lowercase().contains("error") {
                    let _ = stderr_events
                        .send(DriverEvent::Error(format!("{provider_name}: {message}")));
                }
            }
        })
    });

    let mut parser = StreamParser::default();
    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                match provider {
                    ProviderKind::Amp => parser.parse_amp(value, events),
                    ProviderKind::Claude => parser.parse_claude(value, events),
                    ProviderKind::Cursor => parser.parse_cursor(value, events),
                    ProviderKind::OpenCode => parser.parse_opencode(value, events),
                    ProviderKind::Grok => parser.parse_grok(value, events),
                    ProviderKind::Codex | ProviderKind::Pi => {}
                }
            }
        }
    }
    let status = child.wait();
    active_pid.store(0, Ordering::Relaxed);
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }
    let success = status.map(|status| status.success()).unwrap_or(false);
    if provider == ProviderKind::Claude
        && let Some(session_id) = parser
            .provider_session_id
            .as_deref()
            .or(provider_session_id)
        && let Ok(Some(message_id)) = crate::claude_session::latest_message_id(session_id)
        && previous_claude_message.as_deref() != Some(message_id.as_str())
    {
        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::Claude {
                session_id: session_id.to_owned(),
                resume_at: Some(message_id),
            }),
        });
    }
    let _ = events.send(DriverEvent::TurnFinished {
        success,
        summary: (!success).then(|| {
            format!(
                "{} exited before completing the turn.",
                provider.short_name()
            )
        }),
    });
    parser.provider_session_id
}

#[derive(Default)]
struct StreamParser {
    saw_text_delta: bool,
    saw_reasoning_delta: bool,
    provider_session_id: Option<String>,
    amp_tools: HashMap<String, (ActivityKind, String)>,
    claude_tools: HashMap<String, (ActivityKind, String)>,
    cursor_tools: HashMap<String, (ActivityKind, String)>,
    grok_tools: HashMap<String, (ActivityKind, String)>,
}

impl StreamParser {
    fn parse_amp(&mut self, value: Value, events: &Sender<DriverEvent>) {
        match value.get("type").and_then(Value::as_str) {
            Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
                if let Some(id) = value.get("session_id").and_then(Value::as_str)
                    && self.provider_session_id.as_deref() != Some(id)
                {
                    self.provider_session_id = Some(id.to_owned());
                    let _ = events.send(DriverEvent::Connected {
                        provider_cursor: Some(ProviderResumeCursor::Amp {
                            thread_id: id.to_owned(),
                            fork_context: None,
                        }),
                    });
                }
            }
            Some("assistant") => {
                if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
                    for block in content {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(text) = block
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.is_empty())
                                {
                                    let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                                }
                            }
                            Some("thinking") => {
                                if let Some(text) = block
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.is_empty())
                                {
                                    let _ =
                                        events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                                }
                            }
                            Some("tool_use") => {
                                let id = block.get("id").and_then(Value::as_str).map(str::to_owned);
                                let title = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Tool")
                                    .to_owned();
                                let kind = classify_tool(&title);
                                if let Some(id) = &id {
                                    self.amp_tools.insert(id.clone(), (kind, title.clone()));
                                }
                                let detail = block
                                    .get("input")
                                    .map(compact_json)
                                    .filter(|value| !value.is_empty());
                                let _ = events.send(DriverEvent::Activity {
                                    id,
                                    kind,
                                    title,
                                    detail,
                                    complete: false,
                                });
                            }
                            // Redacted thinking is provider-private control data.
                            _ => {}
                        }
                    }
                }
            }
            Some("user") => {
                if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
                    for block in content {
                        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                            continue;
                        }
                        let id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        let (kind, title) = id
                            .as_ref()
                            .and_then(|id| self.amp_tools.remove(id))
                            .unwrap_or((ActivityKind::Tool, "Tool".to_owned()));
                        let _ = events.send(DriverEvent::Activity {
                            id,
                            kind,
                            title,
                            detail: None,
                            complete: true,
                        });
                    }
                }
            }
            Some("result") if value.get("is_error").and_then(Value::as_bool) == Some(true) => {
                let message = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Amp reported an error");
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
            Some("system") => {
                if let Some(message) = value.get("error").and_then(Value::as_str) {
                    let _ = events.send(DriverEvent::Error(message.to_owned()));
                }
            }
            _ => {}
        }
    }

    fn parse_claude(&mut self, value: Value, events: &Sender<DriverEvent>) {
        match value.get("type").and_then(Value::as_str) {
            Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
                if let Some(id) = value.get("session_id").and_then(Value::as_str) {
                    self.provider_session_id = Some(id.to_owned());
                    let _ = events.send(DriverEvent::Connected {
                        provider_cursor: Some(ProviderResumeCursor::Claude {
                            session_id: id.to_owned(),
                            resume_at: None,
                        }),
                    });
                }
            }
            Some("stream_event") => {
                let event = value.get("event").unwrap_or(&Value::Null);
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            self.saw_text_delta = true;
                            let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            self.saw_reasoning_delta = true;
                            let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
                    for block in content {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") if !self.saw_text_delta => {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                                }
                            }
                            Some("thinking") if !self.saw_reasoning_delta => {
                                if let Some(text) = block
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.is_empty())
                                {
                                    self.saw_reasoning_delta = true;
                                    let _ =
                                        events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                                }
                            }
                            Some("tool_use") => {
                                let id = block.get("id").and_then(Value::as_str).map(str::to_owned);
                                let title = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Tool")
                                    .to_owned();
                                let kind = classify_tool(&title);
                                if let Some(id) = &id {
                                    self.claude_tools.insert(id.clone(), (kind, title.clone()));
                                }
                                let detail = block
                                    .get("input")
                                    .map(compact_json)
                                    .filter(|value| !value.is_empty());
                                let _ = events.send(DriverEvent::Activity {
                                    id,
                                    kind,
                                    title,
                                    detail,
                                    complete: false,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("user") => {
                if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
                    for block in content {
                        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                            continue;
                        }
                        let id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        let (kind, title) = id
                            .as_ref()
                            .and_then(|id| self.claude_tools.remove(id))
                            .unwrap_or((ActivityKind::Tool, "Tool".to_owned()));
                        let _ = events.send(DriverEvent::Activity {
                            id,
                            kind,
                            title,
                            // Keep the command input already shown in the row.
                            detail: None,
                            complete: true,
                        });
                    }
                }
            }
            Some("result") if value.get("is_error").and_then(Value::as_bool) == Some(true) => {
                if let Some(result) = value.get("result").and_then(Value::as_str) {
                    let _ = events.send(DriverEvent::Error(result.to_owned()));
                }
            }
            _ => {}
        }
    }

    fn parse_cursor(&mut self, value: Value, events: &Sender<DriverEvent>) {
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(id) = value.get("session_id").and_then(Value::as_str)
            && !id.is_empty()
            && self.provider_session_id.as_deref() != Some(id)
        {
            self.provider_session_id = Some(id.to_owned());
            let _ = events.send(DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Cursor {
                    session_id: id.to_owned(),
                    fork_context: None,
                }),
            });
        }
        match event_type {
            "assistant" => {
                if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
                    for block in content {
                        if block.get("type").and_then(Value::as_str) == Some("text")
                            && let Some(text) = block
                                .get("text")
                                .and_then(Value::as_str)
                                .filter(|text| !text.is_empty())
                        {
                            let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                        }
                    }
                }
            }
            "tool_call" => {
                let id = value
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let tool_call = value.get("tool_call").unwrap_or(&Value::Null);
                let (wire_name, payload) = tool_call
                    .as_object()
                    .and_then(|tools| tools.iter().next())
                    .map(|(name, payload)| (name.as_str(), payload))
                    .unwrap_or(("toolCall", tool_call));
                let title = cursor_tool_title(wire_name);
                let kind = classify_tool(wire_name);
                let complete = value.get("subtype").and_then(Value::as_str) == Some("completed");
                if !complete && let Some(id) = &id {
                    self.cursor_tools.insert(id.clone(), (kind, title.clone()));
                }
                let (kind, title) = if complete {
                    id.as_ref()
                        .and_then(|id| self.cursor_tools.remove(id))
                        .unwrap_or((kind, title))
                } else {
                    (kind, title)
                };
                let detail = if complete {
                    payload.get("result")
                } else {
                    payload.get("args")
                }
                .filter(|value| !value.is_null())
                .map(compact_json);
                let _ = events.send(DriverEvent::Activity {
                    id,
                    kind,
                    title,
                    detail,
                    complete,
                });
            }
            "result" if value.get("is_error").and_then(Value::as_bool) == Some(true) => {
                let message = value
                    .get("result")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("error").and_then(Value::as_str))
                    .unwrap_or("Cursor reported an error");
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
            _ => {}
        }
    }

    fn parse_opencode(&mut self, value: Value, events: &Sender<DriverEvent>) {
        if let Some(id) = value
            .get("sessionID")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/part/sessionID").and_then(Value::as_str))
            && self.provider_session_id.as_deref() != Some(id)
        {
            self.provider_session_id = Some(id.to_owned());
            let _ = events.send(DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::OpenCode {
                    session_id: id.to_owned(),
                }),
            });
        }
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let part = value.get("part").unwrap_or(&value);
        match event_type {
            "text" | "text.delta" | "message.part.updated" => {
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("delta").and_then(Value::as_str));
                if let Some(text) = text {
                    let delta = if self.saw_text_delta {
                        text
                    } else {
                        self.saw_text_delta = true;
                        text
                    };
                    let _ = events.send(DriverEvent::TextDelta(delta.to_owned()));
                }
            }
            "reasoning" | "thinking" => {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                }
            }
            "tool_use" | "tool" | "tool.updated" => {
                let title = part
                    .get("tool")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("name").and_then(Value::as_str))
                    .unwrap_or("Tool")
                    .to_owned();
                let detail = part
                    .pointer("/state/input")
                    .or_else(|| part.get("input"))
                    .map(compact_json);
                let complete = matches!(
                    part.pointer("/state/status").and_then(Value::as_str),
                    Some("completed" | "error")
                );
                let _ = events.send(DriverEvent::Activity {
                    id: part
                        .get("callID")
                        .or_else(|| part.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    kind: classify_tool(&title),
                    title,
                    detail,
                    complete,
                });
            }
            "error" => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("message").and_then(Value::as_str))
                    .unwrap_or("OpenCode reported an error");
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
            _ => {}
        }
    }

    fn parse_grok(&mut self, value: Value, events: &Sender<DriverEvent>) {
        match value.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = value.get("data").and_then(Value::as_str) {
                    let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                }
            }
            Some("thought") => {
                if let Some(text) = value
                    .get("data")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                }
            }
            Some("tool_call") => {
                let id = value
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|title| !title.is_empty())
                    .or_else(|| value.get("toolName").and_then(Value::as_str))
                    .unwrap_or("Tool")
                    .to_owned();
                let kind = classify_grok_tool(
                    value.get("kind").and_then(Value::as_str),
                    value.get("toolName").and_then(Value::as_str),
                    &title,
                );
                if let Some(id) = &id {
                    self.grok_tools.insert(id.clone(), (kind, title.clone()));
                }
                let detail = value
                    .get("rawInput")
                    .filter(|input| !input.is_null())
                    .map(compact_json);
                let complete = matches!(
                    value.get("status").and_then(Value::as_str),
                    Some("completed" | "failed")
                );
                let _ = events.send(DriverEvent::Activity {
                    id,
                    kind,
                    title,
                    detail,
                    complete,
                });
            }
            Some("tool_call_update") => {
                let id = value
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (kind, title) = id
                    .as_ref()
                    .and_then(|id| self.grok_tools.get(id))
                    .cloned()
                    .unwrap_or((ActivityKind::Tool, "Tool".to_owned()));
                let detail = value
                    .get("rawOutput")
                    .filter(|output| !output.is_null())
                    .map(compact_json);
                let complete = matches!(
                    value.get("status").and_then(Value::as_str),
                    Some("completed" | "failed")
                );
                let _ = events.send(DriverEvent::Activity {
                    id,
                    kind,
                    title,
                    detail,
                    complete,
                });
            }
            Some("plan") => {
                let _ = events.send(DriverEvent::Activity {
                    id: Some("grok-plan".into()),
                    kind: ActivityKind::Plan,
                    title: "Plan updated".into(),
                    detail: value.get("entries").map(compact_json),
                    complete: false,
                });
            }
            Some("end") => {
                if let Some(id) = value.get("sessionId").and_then(Value::as_str)
                    && self.provider_session_id.as_deref() != Some(id)
                {
                    self.provider_session_id = Some(id.to_owned());
                    let _ = events.send(DriverEvent::Connected {
                        provider_cursor: Some(ProviderResumeCursor::Grok {
                            session_id: id.to_owned(),
                        }),
                    });
                }
            }
            Some("error") => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Grok reported an error");
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
            _ => {}
        }
    }
}

fn compact_json(value: &Value) -> String {
    let value = serde_json::to_string(value).unwrap_or_default();
    if value.chars().count() > 180 {
        format!("{}…", value.chars().take(179).collect::<String>())
    } else {
        value
    }
}

fn amp_args(
    mode: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
    thread_id: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(thread_id) = thread_id {
        args.extend([
            "threads".to_owned(),
            "continue".to_owned(),
            thread_id.to_owned(),
        ]);
    }
    args.extend([
        "--execute".to_owned(),
        "--stream-json-thinking".to_owned(),
        "--dangerously-allow-all".to_owned(),
    ]);
    if let Some(mode) = mode {
        args.extend(["--mode".to_owned(), mode.to_owned()]);
    }
    if let Some(reasoning_effort) = reasoning_effort {
        args.extend(["--effort".to_owned(), reasoning_effort.to_owned()]);
    }
    if service_tier == Some("fast") {
        args.push("--fast".to_owned());
    }
    args
}

fn cursor_args(
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: Option<&str>,
    session_id: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
    ];
    if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
        args.push("--mode=plan".to_owned());
    } else if mode == RuntimeMode::Ask {
        args.push("--mode=ask".to_owned());
    } else {
        // Cursor's print transport cannot relay approval prompts back to Waku.
        // Its force flag is the documented non-interactive write path.
        args.push("--force".to_owned());
    }
    if let Some(model) = model {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    if let Some(session_id) = session_id {
        args.extend(["--resume".to_owned(), session_id.to_owned()]);
    }
    args
}

fn cursor_tool_title(wire_name: &str) -> String {
    let name = wire_name
        .strip_suffix("ToolCall")
        .unwrap_or(wire_name)
        .replace('_', " ");
    let mut title = String::new();
    for (index, character) in name.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            title.push(' ');
        }
        if index == 0 {
            title.extend(character.to_uppercase());
        } else {
            title.push(character);
        }
    }
    if title.is_empty() {
        "Tool".into()
    } else {
        title
    }
}

fn classify_tool(name: &str) -> ActivityKind {
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("bash")
        || normalized.contains("command")
        || normalized.contains("shell")
        || normalized.contains("terminal")
    {
        ActivityKind::Command
    } else if normalized.contains("edit")
        || normalized.contains("write")
        || normalized.contains("patch")
        || normalized.contains("create")
    {
        ActivityKind::FileChange
    } else if normalized.contains("search")
        || normalized.contains("grep")
        || normalized.contains("read")
        || normalized.contains("find")
    {
        ActivityKind::Search
    } else if normalized.contains("todo") || normalized.contains("plan") {
        ActivityKind::Plan
    } else {
        ActivityKind::Tool
    }
}

fn classify_grok_tool(kind: Option<&str>, name: Option<&str>, title: &str) -> ActivityKind {
    match kind.unwrap_or_default() {
        "execute" => ActivityKind::Command,
        "edit" | "delete" | "move" => ActivityKind::FileChange,
        "search" | "fetch" | "read" => ActivityKind::Search,
        "think" => ActivityKind::Reasoning,
        _ => classify_tool(name.unwrap_or(title)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amp_cli_args_create_and_resume_the_exact_thread() {
        assert_eq!(
            amp_args(Some("medium"), None, None, None),
            [
                "--execute",
                "--stream-json-thinking",
                "--dangerously-allow-all",
                "--mode",
                "medium",
            ]
        );
        assert_eq!(
            amp_args(
                Some("high"),
                Some("xhigh"),
                Some("fast"),
                Some("T-thread-123"),
            ),
            [
                "threads",
                "continue",
                "T-thread-123",
                "--execute",
                "--stream-json-thinking",
                "--dangerously-allow-all",
                "--mode",
                "high",
                "--effort",
                "xhigh",
                "--fast",
            ]
        );
    }

    #[test]
    fn cursor_cli_args_map_waku_modes_and_resume_exact_session() {
        assert_eq!(
            cursor_args(
                RuntimeMode::FullAccess,
                InteractionMode::Build,
                Some("composer-2.5"),
                Some("cursor-session-1"),
            ),
            [
                "--print",
                "--output-format",
                "stream-json",
                "--force",
                "--model",
                "composer-2.5",
                "--resume",
                "cursor-session-1",
            ]
        );
        assert!(
            cursor_args(RuntimeMode::Ask, InteractionMode::Build, None, None)
                .contains(&"--mode=ask".to_owned())
        );
        assert!(
            cursor_args(RuntimeMode::FullAccess, InteractionMode::Plan, None, None)
                .contains(&"--mode=plan".to_owned())
        );
    }

    #[test]
    fn cursor_stream_emits_session_text_and_correlated_tools_in_wire_order() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_cursor(
            serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "cursor-session-1"
            }),
            &events,
        );
        parser.parse_cursor(
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": "Before"}]},
                "session_id": "cursor-session-1"
            }),
            &events,
        );
        parser.parse_cursor(
            serde_json::json!({
                "type": "tool_call",
                "subtype": "started",
                "call_id": "tool-1",
                "tool_call": {"readToolCall": {"args": {"path": "src/main.rs"}}},
                "session_id": "cursor-session-1"
            }),
            &events,
        );
        parser.parse_cursor(
            serde_json::json!({
                "type": "tool_call",
                "subtype": "completed",
                "call_id": "tool-1",
                "tool_call": {"readToolCall": {"result": {"success": {"totalLines": 12}}}},
                "session_id": "cursor-session-1"
            }),
            &events,
        );

        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Cursor {
                    session_id,
                    fork_context: None,
                })
            } if session_id == "cursor-session-1"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "Before"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                kind: ActivityKind::Search,
                title,
                complete: false,
                ..
            } if id == "tool-1" && title == "Read"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                kind: ActivityKind::Search,
                title,
                complete: true,
                ..
            } if id == "tool-1" && title == "Read"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn amp_stream_preserves_thinking_text_and_tool_order() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_amp(
            serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "T-thread-123"
            }),
            &events,
        );
        parser.parse_amp(
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "thinking", "thinking": "Inspecting"},
                    {"type": "redacted_thinking", "data": "private"},
                    {"type": "text", "text": "Before"},
                    {
                        "type": "tool_use",
                        "id": "toolu_amp_1",
                        "name": "create_file",
                        "input": {"path": "src/new.rs"}
                    }
                ]}
            }),
            &events,
        );
        parser.parse_amp(
            serde_json::json!({
                "type": "user",
                "message": {"content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_amp_1",
                    "content": "created",
                    "is_error": false
                }]}
            }),
            &events,
        );
        parser.parse_amp(
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": "After"}]}
            }),
            &events,
        );

        assert_eq!(parser.provider_session_id.as_deref(), Some("T-thread-123"));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Amp {
                    thread_id,
                    fork_context: None,
                })
            } if thread_id == "T-thread-123"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::ReasoningDelta(text) if text == "Inspecting"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "Before"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                kind: ActivityKind::FileChange,
                complete: false,
                ..
            } if id == "toolu_amp_1"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                kind: ActivityKind::FileChange,
                complete: true,
                ..
            } if id == "toolu_amp_1"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "After"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn amp_stream_surfaces_structured_errors() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_amp(
            serde_json::json!({
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": true,
                "error": "Authentication required"
            }),
            &events,
        );

        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Error(message) if message == "Authentication required"
        ));
    }

    #[test]
    fn opencode_stream_captures_native_session_and_text() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_opencode(
            serde_json::json!({
                "type": "text",
                "sessionID": "ses_native",
                "part": {"text": "hello"}
            }),
            &events,
        );

        assert_eq!(parser.provider_session_id.as_deref(), Some("ses_native"));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::OpenCode { session_id })
            } if session_id == "ses_native"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "hello"
        ));
    }

    #[test]
    fn claude_stream_captures_session_and_partial_delta() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_claude(
            serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "98f012ee-537c-40bd-817c-e6496030973b"
            }),
            &events,
        );
        parser.parse_claude(
            serde_json::json!({
                "type": "stream_event",
                "event": {"delta": {"type": "text_delta", "text": "hi"}}
            }),
            &events,
        );

        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Connected { .. }
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "hi"
        ));
    }

    #[test]
    fn claude_empty_thinking_delta_does_not_hide_final_thinking() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_claude(
            serde_json::json!({
                "type": "stream_event",
                "event": {
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": ""
                    }
                }
            }),
            &events,
        );
        parser.parse_claude(
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "thinking",
                        "thinking": "Visible reasoning"
                    }]
                }
            }),
            &events,
        );

        assert!(parser.saw_reasoning_delta);
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::ReasoningDelta(text) if text == "Visible reasoning"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn claude_tool_result_completes_the_matching_activity() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_claude(
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_123",
                        "name": "Bash",
                        "input": {"command": "cargo test"}
                    }]
                }
            }),
            &events,
        );
        parser.parse_claude(
            serde_json::json!({
                "type": "user",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_123",
                        "content": "test result: ok"
                    }]
                }
            }),
            &events,
        );

        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                title,
                complete: false,
                ..
            } if id == "toolu_123" && title == "Bash"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                title,
                detail: None,
                complete: true,
                ..
            } if id == "toolu_123" && title == "Bash"
        ));
        assert!(parser.claude_tools.is_empty());
    }

    #[test]
    fn grok_native_stream_emits_text_and_tools_in_wire_order() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_grok(
            serde_json::json!({"type": "text", "data": "Before"}),
            &events,
        );
        parser.parse_grok(
            serde_json::json!({
                "type": "tool_call",
                "toolCallId": "tool-1",
                "title": "Read src/main.rs",
                "kind": "read",
                "toolName": "read_file",
                "status": "in_progress",
                "rawInput": {"path": "src/main.rs"}
            }),
            &events,
        );
        parser.parse_grok(
            serde_json::json!({
                "type": "tool_call_update",
                "toolCallId": "tool-1",
                "status": "completed",
                "rawOutput": {"lines": 12}
            }),
            &events,
        );
        parser.parse_grok(
            serde_json::json!({"type": "text", "data": "After"}),
            &events,
        );

        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "Before"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                title,
                complete: false,
                ..
            } if id == "tool-1" && title == "Read src/main.rs"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Activity {
                id: Some(id),
                title,
                complete: true,
                ..
            } if id == "tool-1" && title == "Read src/main.rs"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "After"
        ));
    }

    #[test]
    fn grok_native_end_captures_the_resumable_session() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_grok(
            serde_json::json!({
                "type": "end",
                "stopReason": "end_turn",
                "sessionId": "c26f9cf7-dc11-4075-b0f4-544e65105469"
            }),
            &events,
        );

        assert_eq!(
            parser.provider_session_id.as_deref(),
            Some("c26f9cf7-dc11-4075-b0f4-544e65105469")
        );
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Grok { session_id })
            } if session_id == "c26f9cf7-dc11-4075-b0f4-544e65105469"
        ));
    }
}

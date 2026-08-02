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

use crate::computer_use::{
    self, ComputerToolRequest, ComputerUsePhase, ComputerUseSession, ComputerUseState,
};
use crate::driver::{DriverControl, DriverStartOptions};
use crate::model::{
    ActivityItem, ActivityKind, DriverEvent, InteractionMode, PermissionOption,
    ProviderResumeCursor, RuntimeMode,
};

const DISABLE_EXTERNAL_COMPUTER_USE_PLUGIN: &str =
    "plugins.computer-use@openai-bundled.enabled=false";
// Codex 0.146 only resolves plugin enablement from user/profile config layers, so a
// process-local `-c` plugin override does not yet suppress its bundled capabilities.
const DISABLE_EXTERNAL_COMPUTER_USE_MCP: &str = "mcp_servers.computer-use.enabled=false";
const DISABLE_EXTERNAL_COMPUTER_USE_SKILL: &str =
    r#"skills.config=[{name="computer-use:computer-use",enabled=false}]"#;

enum CommandMessage {
    Prompt(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    RunComputerTool(ComputerToolRequest),
    DynamicToolResponse {
        rpc_id: String,
        success: bool,
        content_items: Vec<Value>,
    },
    RejectComputerTool {
        request: ComputerToolRequest,
        reason: String,
    },
    Rollback {
        turns: usize,
        response: Sender<Result<(), String>>,
    },
    Fork {
        turns_to_remove: usize,
        response: Sender<Result<String, String>>,
    },
    Shutdown,
}

pub struct CodexDriver {
    commands: Sender<CommandMessage>,
    computer_use_session: Arc<ComputerUseSession>,
}

impl CodexDriver {
    pub fn start(options: DriverStartOptions, events: Sender<DriverEvent>) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
            provider_cursor,
        } = options;
        let provider_session_id = match provider_cursor {
            Some(ProviderResumeCursor::Codex { thread_id }) => Some(thread_id),
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume Codex from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };
        let mut child = crate::command_env::command(binary)
            .args([
                "app-server",
                "--stdio",
                "-c",
                DISABLE_EXTERNAL_COMPUTER_USE_PLUGIN,
                "-c",
                DISABLE_EXTERNAL_COMPUTER_USE_MCP,
                "-c",
                DISABLE_EXTERNAL_COMPUTER_USE_SKILL,
            ])
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start `codex app-server`")?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Codex stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Codex stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Codex stderr unavailable"))?;
        let (commands, command_rx) = unbounded();
        let computer_use_session = Arc::new(ComputerUseSession::new());
        let computer_tool_in_flight = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(Mutex::new(None::<String>));
        let turn_id = Arc::new(Mutex::new(None::<String>));
        let turn_ids = Arc::new(Mutex::new(Vec::<String>::new()));
        let pending_rollbacks = Arc::new(Mutex::new(HashMap::<
            u64,
            (usize, Sender<Result<(), String>>),
        >::new()));
        let pending_forks = Arc::new(Mutex::new(
            HashMap::<u64, Sender<Result<String, String>>>::new(),
        ));

        let writer_thread_id = thread_id.clone();
        let writer_turn_id = turn_id.clone();
        let writer_turn_ids = turn_ids.clone();
        let writer_pending_rollbacks = pending_rollbacks.clone();
        let writer_pending_forks = pending_forks.clone();
        let writer_events = events.clone();
        let writer_commands = commands.clone();
        let writer_computer_use_session = computer_use_session.clone();
        let writer_computer_tool_in_flight = computer_tool_in_flight.clone();
        let cwd_string = cwd.display().to_string();
        thread::Builder::new()
            .name("waku-codex-writer".into())
            .spawn(move || {
                let mut stdin = stdin;
                let initialize = json!({
                    "method": "initialize",
                    "id": 0,
                    "params": {
                        "clientInfo": {
                            "name": "waku",
                            "title": "Waku",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "capabilities": {
                            "experimentalApi": true
                        }
                    }
                });
                if write_json_line(&mut stdin, &initialize).is_err()
                    || write_json_line(
                        &mut stdin,
                        &json!({
                            "method": "initialized",
                            "params": {}
                        }),
                    )
                    .is_err()
                {
                    let _ = writer_events.send(DriverEvent::Error(
                        "Failed to initialize Codex app-server".into(),
                    ));
                    return;
                }

                let (approval_policy, sandbox, approvals_reviewer) =
                    codex_permissions(mode, interaction_mode);
                let open_thread = if let Some(thread_id) = provider_session_id {
                    let mut params = json!({
                        "threadId": thread_id,
                        "cwd": cwd_string,
                        "approvalPolicy": approval_policy,
                        "sandbox": sandbox,
                        "approvalsReviewer": approvals_reviewer
                    });
                    if let Some(model) = model.as_deref() {
                        params["model"] = json!(model);
                    }
                    if let Some(service_tier) = service_tier.as_deref() {
                        params["serviceTier"] = json!(service_tier);
                    }
                    json!({
                        "method": "thread/resume",
                        "id": 1,
                        "params": params
                    })
                } else {
                    let mut params = json!({
                        "cwd": cwd_string,
                        "approvalPolicy": approval_policy,
                        "sandbox": sandbox,
                        "approvalsReviewer": approvals_reviewer,
                        "serviceName": "waku",
                        "dynamicTools": computer_use::dynamic_tools()
                    });
                    if let Some(model) = model.as_deref() {
                        params["model"] = json!(model);
                    }
                    if let Some(service_tier) = service_tier.as_deref() {
                        params["serviceTier"] = json!(service_tier);
                    }
                    json!({
                        "method": "thread/start",
                        "id": 1,
                        "params": params
                    })
                };
                let _ = write_json_line(&mut stdin, &open_thread);

                let mut next_request_id = 10_u64;
                while let Ok(command) = command_rx.recv() {
                    let message = match command {
                        CommandMessage::Prompt(text) => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                let _ = writer_events.send(DriverEvent::Error(
                                    "Codex did not finish opening its thread.".into(),
                                ));
                                continue;
                            };
                            next_request_id += 1;
                            let mut params = json!({
                                "threadId": thread_id,
                                "input": [{"type": "text", "text": text}],
                                "approvalPolicy": approval_policy,
                                "approvalsReviewer": approvals_reviewer,
                                "sandboxPolicy": codex_sandbox_policy(sandbox)
                            });
                            if let Some(model) = model.as_deref() {
                                params["model"] = json!(model);
                            }
                            if let Some(reasoning_effort) = reasoning_effort.as_deref() {
                                params["effort"] = json!(reasoning_effort);
                            }
                            if let Some(service_tier) = service_tier.as_deref() {
                                params["serviceTier"] = json!(service_tier);
                            }
                            json!({
                                "method": "turn/start",
                                "id": next_request_id,
                                "params": params
                            })
                        }
                        CommandMessage::Cancel => {
                            let (Some(thread_id), Some(turn_id)) = (
                                writer_thread_id.lock().clone(),
                                writer_turn_id.lock().clone(),
                            ) else {
                                continue;
                            };
                            next_request_id += 1;
                            json!({
                                "method": "turn/interrupt",
                                "id": next_request_id,
                                "params": {"threadId": thread_id, "turnId": turn_id}
                            })
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let id = parse_rpc_id(&request_id);
                            json!({
                                "id": id,
                                "result": {"decision": option_id}
                            })
                        }
                        CommandMessage::RunComputerTool(request) => {
                            if writer_computer_tool_in_flight.swap(true, Ordering::SeqCst) {
                                let reason = "Another computer-use call is still running. Retry after it completes.".to_owned();
                                let _ = writer_events.send(DriverEvent::ComputerUseUpdated(
                                    ComputerUseState {
                                        target: request.target(),
                                        phase: ComputerUsePhase::Failed,
                                        visible: request.tool == "use",
                                        screenshot: None,
                                    },
                                ));
                                let _ = writer_commands.send(
                                    CommandMessage::DynamicToolResponse {
                                        rpc_id: request.rpc_id,
                                        success: false,
                                        content_items: vec![json!({
                                            "type": "inputText",
                                            "text": reason
                                        })],
                                    },
                                );
                                continue;
                            }
                            let worker_events = writer_events.clone();
                            let worker_commands = writer_commands.clone();
                            let computer_use_session = writer_computer_use_session.clone();
                            let worker_computer_tool_in_flight =
                                writer_computer_tool_in_flight.clone();
                            let failed_request = request.clone();
                            let spawn_result = thread::Builder::new()
                                .name("waku-computer-use".into())
                                .spawn(move || {
                                    let _ = worker_events.send(DriverEvent::ComputerUseUpdated(
                                        ComputerUseState {
                                            target: request.target(),
                                            phase: ComputerUsePhase::Running,
                                            visible: request.tool == "use",
                                            screenshot: None,
                                        },
                                    ));
                                    let output = computer_use::execute_tool(
                                        request.clone(),
                                        computer_use_session,
                                    );
                                    worker_computer_tool_in_flight.store(false, Ordering::SeqCst);
                                    if let Some(permissions) = output.permissions.clone() {
                                        let _ = worker_events
                                            .send(DriverEvent::ComputerPermissions(permissions));
                                    }
                                    let _ = worker_events
                                        .send(DriverEvent::ComputerUseUpdated(output.state));
                                    let _ =
                                        worker_commands.send(CommandMessage::DynamicToolResponse {
                                            rpc_id: request.rpc_id,
                                            success: output.success,
                                            content_items: output.content_items,
                                        });
                                });
                            if let Err(error) = spawn_result {
                                writer_computer_tool_in_flight.store(false, Ordering::SeqCst);
                                let reason = format!("Could not start computer use: {error}");
                                let _ = writer_commands.send(
                                    CommandMessage::DynamicToolResponse {
                                        rpc_id: failed_request.rpc_id,
                                        success: false,
                                        content_items: vec![json!({
                                            "type": "inputText",
                                            "text": reason
                                        })],
                                    },
                                );
                            }
                            continue;
                        }
                        CommandMessage::DynamicToolResponse {
                            rpc_id,
                            success,
                            content_items,
                        } => json!({
                            "id": parse_rpc_id(&rpc_id),
                            "result": {
                                "success": success,
                                "contentItems": content_items
                            }
                        }),
                        CommandMessage::RejectComputerTool { request, reason } => {
                            let target = request.target();
                            let _ = writer_events.send(DriverEvent::ComputerUseUpdated(
                                ComputerUseState {
                                    target,
                                    phase: ComputerUsePhase::Failed,
                                    visible: request.tool == "use",
                                    screenshot: None,
                                },
                            ));
                            json!({
                                "id": parse_rpc_id(&request.rpc_id),
                                "result": {
                                    "success": false,
                                    "contentItems": [{"type": "inputText", "text": reason}]
                                }
                            })
                        }
                        CommandMessage::Rollback { turns, response } => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                let _ = response
                                    .send(Err("Codex did not finish opening its thread.".into()));
                                continue;
                            };
                            next_request_id += 1;
                            let request_id = next_request_id;
                            writer_pending_rollbacks
                                .lock()
                                .insert(request_id, (turns, response));
                            let message = json!({
                                "method": "thread/rollback",
                                "id": request_id,
                                "params": {
                                    "threadId": thread_id,
                                    "numTurns": turns
                                }
                            });
                            if let Err(error) = write_json_line(&mut stdin, &message)
                                && let Some((_, response)) =
                                    writer_pending_rollbacks.lock().remove(&request_id)
                            {
                                let _ = response
                                    .send(Err(format!("Codex transport write failed: {error}")));
                            }
                            continue;
                        }
                        CommandMessage::Fork {
                            turns_to_remove,
                            response,
                        } => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                let _ = response
                                    .send(Err("Codex did not finish opening its thread.".into()));
                                continue;
                            };
                            let last_turn_id = {
                                let turn_ids = writer_turn_ids.lock();
                                match fork_last_turn_id(&turn_ids, turns_to_remove) {
                                    Ok(turn_id) => turn_id,
                                    Err(error) => {
                                        let _ = response.send(Err(error));
                                        continue;
                                    }
                                }
                            };
                            next_request_id += 1;
                            let request_id = next_request_id;
                            writer_pending_forks.lock().insert(request_id, response);
                            let message = json!({
                                "method": "thread/fork",
                                "id": request_id,
                                "params": {
                                    "threadId": thread_id,
                                    "lastTurnId": last_turn_id
                                }
                            });
                            if let Err(error) = write_json_line(&mut stdin, &message)
                                && let Some(response) =
                                    writer_pending_forks.lock().remove(&request_id)
                            {
                                let _ = response
                                    .send(Err(format!("Codex transport write failed: {error}")));
                            }
                            continue;
                        }
                        CommandMessage::Shutdown => break,
                    };
                    if let Err(error) = write_json_line(&mut stdin, &message) {
                        let _ = writer_events.send(DriverEvent::Error(format!(
                            "Codex transport write failed: {error}"
                        )));
                        break;
                    }
                }
            })?;

        let reader_thread_id = thread_id.clone();
        let reader_turn_id = turn_id.clone();
        let reader_turn_ids = turn_ids.clone();
        let reader_pending_rollbacks = pending_rollbacks.clone();
        let reader_pending_forks = pending_forks.clone();
        let reader_events = events.clone();
        let reader_thread = thread::Builder::new()
            .name("waku-codex-reader".into())
            .spawn(move || {
                let mut stream_state = CodexStreamState::default();
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if !line.trim().is_empty() => {
                            match serde_json::from_str::<Value>(&line) {
                                Ok(value) => handle_codex_message(
                                    value,
                                    &reader_thread_id,
                                    &reader_turn_id,
                                    &reader_turn_ids,
                                    &reader_pending_rollbacks,
                                    &reader_pending_forks,
                                    &reader_events,
                                    &mut stream_state,
                                ),
                                Err(error) => {
                                    let _ = reader_events.send(DriverEvent::Error(format!(
                                        "Codex sent invalid JSON: {error}"
                                    )));
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let _ = reader_events.send(DriverEvent::Error(format!(
                                "Codex transport read failed: {error}"
                            )));
                            break;
                        }
                    }
                }
            })?;

        let last_visible_stderr = Arc::new(Mutex::new(None::<String>));
        let stderr_last_error = last_visible_stderr.clone();
        let stderr_events = events.clone();
        let stderr_thread = thread::Builder::new()
            .name("waku-codex-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if is_visible_stderr_notice(&line) {
                        let error = clean_stderr(&line);
                        *stderr_last_error.lock() = Some(error.clone());
                        let _ = stderr_events.send(DriverEvent::Error(error));
                    }
                }
            })?;

        thread::Builder::new()
            .name("waku-codex-process".into())
            .spawn(move || {
                let status = child.wait();
                let _ = reader_thread.join();
                let _ = stderr_thread.join();
                match status {
                    Ok(status) if !status.success() && last_visible_stderr.lock().is_none() => {
                        let _ = events.send(DriverEvent::Error(format!(
                            "Codex app-server exited with {status}"
                        )));
                    }
                    Err(error) => {
                        let _ = events.send(DriverEvent::Error(format!(
                            "Could not read Codex app-server exit status: {error}"
                        )));
                    }
                    _ => {}
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            computer_use_session,
        })
    }
}

fn codex_permissions(
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) -> (&'static str, &'static str, &'static str) {
    if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
        return ("never", "read-only", "user");
    }
    match mode {
        RuntimeMode::Ask => ("untrusted", "read-only", "user"),
        RuntimeMode::AutoAcceptEdits => ("on-request", "workspace-write", "user"),
        RuntimeMode::Auto => ("on-request", "workspace-write", "auto_review"),
        RuntimeMode::FullAccess => ("never", "danger-full-access", "user"),
        RuntimeMode::Plan => unreachable!("handled above"),
    }
}

fn codex_sandbox_policy(sandbox: &str) -> Value {
    match sandbox {
        "read-only" => json!({"type": "readOnly"}),
        "danger-full-access" => json!({"type": "dangerFullAccess"}),
        _ => json!({"type": "workspaceWrite"}),
    }
}

impl DriverControl for CodexDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Prompt(prompt));
    }

    fn cancel(&self) {
        self.computer_use_session.cancel();
        let _ = self.commands.send(CommandMessage::Cancel);
    }

    fn cancel_computer_use(&self) {
        self.computer_use_session.cancel();
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.send(CommandMessage::Respond {
            request_id,
            option_id,
        });
    }

    fn run_computer_tool(&self, request: ComputerToolRequest) {
        let _ = self.commands.send(CommandMessage::RunComputerTool(request));
    }

    fn reject_computer_tool(&self, request: ComputerToolRequest, reason: String) {
        let _ = self
            .commands
            .send(CommandMessage::RejectComputerTool { request, reason });
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
            .context("Codex driver stopped before rollback")?;
        response_rx
            .recv_timeout(Duration::from_secs(15))
            .context("timed out waiting for Codex conversation rollback")?
            .map_err(anyhow::Error::msg)?;
        Ok(None)
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(CommandMessage::Fork {
                turns_to_remove,
                response: response_tx,
            })
            .context("Codex driver stopped before forking")?;
        let thread_id = response_rx
            .recv_timeout(Duration::from_secs(15))
            .context("timed out waiting for Codex conversation fork")?
            .map_err(anyhow::Error::msg)?;
        Ok(ProviderResumeCursor::Codex { thread_id })
    }
}

impl Drop for CodexDriver {
    fn drop(&mut self) {
        self.computer_use_session.cancel();
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn wait_for_thread_id(thread_id: &Mutex<Option<String>>) -> Option<String> {
    for _ in 0..500 {
        if let Some(thread_id) = thread_id.lock().clone() {
            return Some(thread_id);
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}

fn fork_last_turn_id(turn_ids: &[String], turns_to_remove: usize) -> Result<String, String> {
    turn_ids
        .len()
        .checked_sub(turns_to_remove + 1)
        .and_then(|index| turn_ids.get(index))
        .cloned()
        .ok_or_else(|| "Codex has no completed turn at that response.".to_owned())
}

const CODEX_CITATION_START: char = '\u{e200}';
const CODEX_CITATION_END: char = '\u{e201}';
const CODEX_CITATION_SEPARATOR: char = '\u{e202}';

#[derive(Default)]
struct CodexStreamState {
    citations: HashMap<String, String>,
    citation_numbers: HashMap<String, usize>,
    citation_buffer: String,
    next_citation_number: usize,
}

impl CodexStreamState {
    fn begin_turn(&mut self) {
        self.citations.clear();
        self.citation_numbers.clear();
        self.citation_buffer.clear();
        self.next_citation_number = 1;
    }

    fn capture_citations(&mut self, item: &Value) {
        if item.get("type").and_then(Value::as_str) != Some("webSearch") {
            return;
        }
        let Some(results) = item.get("results").and_then(Value::as_array) else {
            return;
        };
        for result in results {
            let reference = result
                .get("ref_id")
                .or_else(|| result.get("refId"))
                .and_then(Value::as_str)
                .filter(|reference| !reference.is_empty());
            let url = result
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty());
            if let (Some(reference), Some(url)) = (reference, url) {
                self.citations.insert(reference.into(), url.into());
            }
        }
    }

    fn rewrite_citation_delta(&mut self, delta: &str) -> String {
        self.citation_buffer.push_str(delta);
        let mut input = std::mem::take(&mut self.citation_buffer);
        let mut output = String::with_capacity(input.len());

        loop {
            let Some(start) = input.find(CODEX_CITATION_START) else {
                output.push_str(&input);
                break;
            };
            output.push_str(&input[..start]);
            let marker_start = start + CODEX_CITATION_START.len_utf8();
            let Some(end_offset) = input[marker_start..].find(CODEX_CITATION_END) else {
                self.citation_buffer.push_str(&input[start..]);
                break;
            };
            let marker_end = marker_start + end_offset;
            let marker = &input[marker_start..marker_end];
            let citation = self.render_citation(marker);
            if !citation.is_empty() {
                if output
                    .chars()
                    .last()
                    .is_some_and(|character| !character.is_whitespace())
                {
                    output.push(' ');
                }
                output.push_str(&citation);
            }
            input.drain(..marker_end + CODEX_CITATION_END.len_utf8());
        }

        output
    }

    fn render_citation(&mut self, marker: &str) -> String {
        let mut parts = marker.split(CODEX_CITATION_SEPARATOR);
        if parts.next() != Some("cite") {
            return String::new();
        }

        let mut links = Vec::new();
        for reference in parts.filter(|part| !part.is_empty()) {
            let Some(url) = self.citations.get(reference).cloned() else {
                continue;
            };
            let number = *self
                .citation_numbers
                .entry(reference.into())
                .or_insert_with(|| {
                    let number = self.next_citation_number;
                    self.next_citation_number += 1;
                    number
                });
            links.push(format!("[{number}]({})", markdown_link_destination(&url)));
        }
        links.join(" ")
    }
}

fn markdown_link_destination(url: &str) -> String {
    url.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn handle_codex_message(
    value: Value,
    thread_id: &Mutex<Option<String>>,
    turn_id: &Mutex<Option<String>>,
    turn_ids: &Mutex<Vec<String>>,
    pending_rollbacks: &Mutex<HashMap<u64, (usize, Sender<Result<(), String>>)>>,
    pending_forks: &Mutex<HashMap<u64, Sender<Result<String, String>>>>,
    events: &Sender<DriverEvent>,
    stream_state: &mut CodexStreamState,
) {
    // JSON-RPC IDs are scoped to each peer, so an app-server request may use
    // the same numeric ID as one of Waku's earlier requests. Only messages
    // without a method are responses to Waku-originated requests.
    let is_response = value.get("method").is_none();
    if is_response
        && let Some(id) = value.get("id").and_then(Value::as_u64)
        && id != 1
        && let Some((turns, response)) = pending_rollbacks.lock().remove(&id)
    {
        let result = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map_or_else(|| Ok(()), |error| Err(error.to_owned()));
        if result.is_ok() {
            let retained = turn_ids.lock().len().saturating_sub(turns);
            turn_ids.lock().truncate(retained);
        }
        let _ = response.send(result);
        return;
    }

    if is_response
        && let Some(id) = value.get("id").and_then(Value::as_u64)
        && id != 1
        && let Some(response) = pending_forks.lock().remove(&id)
    {
        let result = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map_or_else(
                || {
                    value
                        .pointer("/result/thread/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "Codex returned no forked thread ID.".to_owned())
                },
                |error| Err(error.to_owned()),
            );
        let _ = response.send(result);
        return;
    }

    if is_response && value.get("id").and_then(Value::as_u64) == Some(1) {
        if let Some(id) = value.pointer("/result/thread/id").and_then(Value::as_str) {
            *thread_id.lock() = Some(id.to_owned());
            *turn_ids.lock() = value
                .pointer("/result/thread/turns")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|turn| turn.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect();
            let _ = events.send(DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Codex {
                    thread_id: id.to_owned(),
                }),
            });
        } else if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
            let _ = events.send(DriverEvent::Error(error.to_owned()));
        }
        return;
    }

    let Some(method) = value.get("method").and_then(Value::as_str) else {
        if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
            let _ = events.send(DriverEvent::Error(error.to_owned()));
        }
        return;
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "item/tool/call" if value.get("id").is_some() => {
            if params.get("namespace").and_then(Value::as_str) == Some(computer_use::NAMESPACE)
                && let (Some(call_id), Some(tool)) = (
                    params.get("callId").and_then(Value::as_str),
                    params.get("tool").and_then(Value::as_str),
                )
            {
                let _ = events.send(DriverEvent::ComputerToolRequested(ComputerToolRequest {
                    rpc_id: rpc_id_string(value.get("id").unwrap()),
                    call_id: call_id.to_owned(),
                    tool: tool.to_owned(),
                    arguments: params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                }));
            }
        }
        "turn/started" => {
            stream_state.begin_turn();
            if let Some(id) = params.pointer("/turn/id").and_then(Value::as_str) {
                *turn_id.lock() = Some(id.to_owned());
                let mut turn_ids = turn_ids.lock();
                if turn_ids.last().is_none_or(|last| last != id) {
                    turn_ids.push(id.to_owned());
                }
            }
            let _ = events.send(DriverEvent::TurnStarted);
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                let delta = stream_state.rewrite_citation_delta(delta);
                if !delta.is_empty() {
                    let _ = events.send(DriverEvent::TextDelta(delta));
                }
            }
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            if let Some(delta) = params
                .get("delta")
                .and_then(Value::as_str)
                .filter(|delta| !delta.is_empty())
            {
                let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
            }
        }
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item") {
                stream_state.capture_citations(item);
                let complete = method == "item/completed";
                let kind = codex_activity_kind(item);
                if let Some(kind) = kind {
                    let title = codex_item_title(item);
                    let output = codex_item_output(item);
                    let image_urls = codex_item_image_urls(item);
                    let detail = codex_item_detail(item, output.as_deref());
                    let activity = ActivityItem::new(
                        item.get("id").and_then(Value::as_str).map(str::to_owned),
                        kind,
                        title,
                        detail,
                        complete,
                    )
                    .with_arguments(codex_item_arguments(item))
                    .with_output(output)
                    .with_image_urls(image_urls)
                    .with_failed(codex_item_failed(item));
                    let _ = events.send(DriverEvent::RichActivity(activity));
                }
            }
        }
        "turn/completed" => {
            stream_state.citation_buffer.clear();
            *turn_id.lock() = None;
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let error = params
                .pointer("/turn/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let _ = events.send(DriverEvent::TurnFinished {
                success: status == "completed",
                summary: error,
            });
        }
        "error" => {
            if let Some(message) = params.get("message").and_then(Value::as_str) {
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
        }
        "mcpServer/startupStatus/updated"
            if params.get("status").and_then(Value::as_str) == Some("failed") =>
        {
            if let Some(message) = params.get("error").and_then(Value::as_str) {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("MCP");
                let _ = events.send(DriverEvent::Error(format!("{name}: {message}")));
            }
        }
        method if value.get("id").is_some() && method.contains("requestApproval") => {
            let request_id = rpc_id_string(value.get("id").unwrap());
            let (title, detail) = approval_copy(method, &params);
            let _ = events.send(DriverEvent::Permission {
                request_id,
                title,
                detail,
                options: vec![
                    PermissionOption {
                        id: "accept".into(),
                        label: "Allow once".into(),
                        allow: true,
                    },
                    PermissionOption {
                        id: "acceptForSession".into(),
                        label: "Allow for session".into(),
                        allow: true,
                    },
                    PermissionOption {
                        id: "decline".into(),
                        label: "Deny".into(),
                        allow: false,
                    },
                ],
            });
        }
        _ => {}
    }
}

fn codex_activity_kind(item: &Value) -> Option<ActivityKind> {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    if item_type.contains("command") {
        Some(ActivityKind::Command)
    } else if item_type.contains("filechange") || item_type.contains("patch") {
        Some(ActivityKind::FileChange)
    } else if item_type.contains("websearch") {
        Some(ActivityKind::Search)
    } else if item_type.contains("plan") || item_type.contains("todo") {
        Some(ActivityKind::Plan)
    } else if item_type.contains("tool") || item_type.contains("collab") {
        Some(ActivityKind::Tool)
    } else {
        None
    }
}

fn codex_item_title(item: &Value) -> String {
    if let Some(command) = item.get("command").and_then(Value::as_str) {
        return command.to_owned();
    }
    if let Some(query) = non_empty_string(item.get("query")) {
        return format!("Search for {query}");
    }
    if item.get("type").and_then(Value::as_str) == Some("webSearch") {
        return codex_web_search_title(item);
    }
    if item.get("type").and_then(Value::as_str) == Some("dynamicToolCall")
        && item.get("namespace").and_then(Value::as_str) == Some(computer_use::NAMESPACE)
        && let Some(tool) = item.get("tool").and_then(Value::as_str)
    {
        return match tool {
            "status" => "Check Computer Use access".into(),
            "list_targets" => "List controllable windows".into(),
            "use" => item
                .pointer("/arguments/target/appName")
                .and_then(Value::as_str)
                .map(|app| format!("Use {app}"))
                .unwrap_or_else(|| "Use app window".into()),
            _ => split_camel_case(tool),
        };
    }
    if let Some(name) = item.get("tool").and_then(Value::as_str) {
        return split_camel_case(name);
    }
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Activity");
    split_camel_case(item_type)
}

fn codex_web_search_title(item: &Value) -> String {
    let Some(action) = item.get("action") else {
        return "Searched the web".into();
    };

    match action.get("type").and_then(Value::as_str) {
        Some("search") => {
            if let Some(query) = non_empty_string(action.get("query")) {
                return format!("Search for {query}");
            }
            if let Some(query) =
                action
                    .get("queries")
                    .and_then(Value::as_array)
                    .and_then(|queries| {
                        queries
                            .iter()
                            .find_map(|query| non_empty_string(Some(query)))
                    })
            {
                return format!("Search for {query}");
            }
            "Searched the web".into()
        }
        Some("openPage") => non_empty_string(action.get("url"))
            .map(|url| format!("Open {url}"))
            .unwrap_or_else(|| "Opened a web page".into()),
        Some("findInPage") => match (
            non_empty_string(action.get("pattern")),
            non_empty_string(action.get("url")),
        ) {
            (Some(pattern), Some(url)) => format!("Find {pattern} in {url}"),
            (Some(pattern), None) => format!("Find {pattern} on the page"),
            (None, Some(url)) => format!("Search within {url}"),
            (None, None) => "Searched within a web page".into(),
        },
        _ => item
            .get("results")
            .and_then(Value::as_array)
            .filter(|results| !results.is_empty())
            .map(|results| {
                let noun = if results.len() == 1 { "page" } else { "pages" };
                format!("Browsed {} {noun}", results.len())
            })
            .unwrap_or_else(|| "Browsed the web".into()),
    }
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn codex_item_detail(item: &Value, output: Option<&str>) -> Option<String> {
    if codex_item_failed(item)
        && let Some(first_line) =
            output.and_then(|output| output.lines().map(str::trim).find(|line| !line.is_empty()))
    {
        return Some(first_line.to_owned());
    }
    item.get("cwd")
        .and_then(Value::as_str)
        .or_else(|| item.get("path").and_then(Value::as_str))
        .or_else(|| item.get("status").and_then(Value::as_str))
        .map(str::to_owned)
}

fn codex_item_arguments(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("dynamicToolCall" | "mcpToolCall") => item
            .get("arguments")
            .filter(|value| !value.is_null())
            .and_then(format_activity_json),
        Some("commandExecution") => {
            let mut arguments = serde_json::Map::new();
            if let Some(command) = item.get("command") {
                arguments.insert("command".into(), command.clone());
            }
            if let Some(cwd) = item.get("cwd") {
                arguments.insert("cwd".into(), cwd.clone());
            }
            (!arguments.is_empty())
                .then(|| Value::Object(arguments))
                .as_ref()
                .and_then(format_activity_json)
        }
        _ => None,
    }
}

fn codex_item_output(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("dynamicToolCall") => {
            let items = item.get("contentItems").and_then(Value::as_array)?;
            let output = items
                .iter()
                .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                    Some("inputText") => {
                        item.get("text").and_then(Value::as_str).map(str::to_owned)
                    }
                    Some("inputImage") => None,
                    Some("inputAudio") => Some("[Audio output]".into()),
                    _ => format_activity_json(item),
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            non_empty_activity_text(output)
        }
        Some("mcpToolCall") => {
            if let Some(message) = item.pointer("/error/message").and_then(Value::as_str) {
                return non_empty_activity_text(message.to_owned());
            }
            let result = item.get("result").filter(|value| !value.is_null())?;
            if let Some(structured) = result
                .get("structuredContent")
                .filter(|value| !value.is_null())
            {
                return format_activity_json(structured);
            }
            result
                .get("content")
                .filter(|value| !value.is_null())
                .and_then(|content| {
                    let text_items = content
                        .as_array()?
                        .iter()
                        .filter(|item| !is_image_content_item(item));
                    let output = text_items
                        .filter_map(|item| {
                            if item.get("type").and_then(Value::as_str) == Some("text") {
                                item.get("text").and_then(Value::as_str).map(str::to_owned)
                            } else {
                                format_activity_json(item)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    non_empty_activity_text(output)
                })
        }
        Some("commandExecution") => {
            let output = item
                .get("aggregatedOutput")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let exit = item.get("exitCode").and_then(Value::as_i64);
            let text = match (output.trim().is_empty(), exit) {
                (false, Some(exit)) => format!("{}\n\nExit code: {exit}", output.trim_end()),
                (false, None) => output.trim_end().to_owned(),
                (true, Some(exit)) => format!("Exit code: {exit}"),
                (true, None) => return None,
            };
            non_empty_activity_text(text)
        }
        _ => None,
    }
}

fn codex_item_image_urls(item: &Value) -> Vec<String> {
    let content = match item.get("type").and_then(Value::as_str) {
        Some("dynamicToolCall") => item.get("contentItems"),
        Some("mcpToolCall") => item.pointer("/result/content"),
        _ => None,
    };
    content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(image_content_url)
        .collect()
}

fn image_content_url(item: &Value) -> Option<String> {
    let item_type = item.get("type").and_then(Value::as_str);
    if !matches!(item_type, Some("inputImage" | "image")) {
        return None;
    }
    item.get("imageUrl")
        .or_else(|| item.get("image_url"))
        .or_else(|| item.get("url"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let data = item.get("data").and_then(Value::as_str)?;
            let mime_type = item
                .get("mimeType")
                .or_else(|| item.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            Some(format!("data:{mime_type};base64,{data}"))
        })
}

fn is_image_content_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("inputImage" | "image")
    )
}

fn codex_item_failed(item: &Value) -> bool {
    item.get("status").and_then(Value::as_str) == Some("failed")
        || item.get("success").and_then(Value::as_bool) == Some(false)
        || item.get("error").is_some_and(|error| !error.is_null())
        || item
            .get("exitCode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
}

fn format_activity_json(value: &Value) -> Option<String> {
    serde_json::to_string_pretty(value)
        .ok()
        .and_then(non_empty_activity_text)
}

fn non_empty_activity_text(value: String) -> Option<String> {
    const MAX_CHARS: usize = 16_000;
    let value = value.trim().to_owned();
    if value.is_empty() {
        return None;
    }
    if value.chars().count() <= MAX_CHARS {
        return Some(value);
    }
    let mut truncated = value.chars().take(MAX_CHARS).collect::<String>();
    truncated.push_str("\n\n… output truncated");
    Some(truncated)
}

fn approval_copy(method: &str, params: &Value) -> (String, String) {
    if method.contains("commandExecution") {
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("Run a command");
        ("Command approval".into(), command.into())
    } else {
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("Apply the proposed file changes");
        ("File change approval".into(), reason.into())
    }
}

fn rpc_id_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn parse_rpc_id(value: &str) -> Value {
    value
        .parse::<u64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(value.to_owned()))
}

fn split_camel_case(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if character == '_' || character == '-' {
            if !output.ends_with(' ') {
                output.push(' ');
            }
            continue;
        }
        if index > 0 && character.is_ascii_uppercase() {
            output.push(' ');
        }
        output.push(character);
    }
    let mut characters = output.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_else(|| "Activity".into())
}

fn clean_stderr(line: &str) -> String {
    line.split_once(": ")
        .map(|(_, message)| message.to_owned())
        .unwrap_or_else(|| line.to_owned())
}

fn is_visible_stderr_notice(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    if lowercase.contains("transport channel closed")
        || lowercase.contains("missing authorization header")
    {
        return false;
    }
    line.contains(" ERROR ")
        || lowercase.starts_with("error:")
        || line.contains('⚠')
        || lowercase.contains("fatal")
        || lowercase.contains("warning")
        || lowercase.contains("no such file or directory")
        || lowercase.contains("mcp startup incomplete")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_use_is_pinned_to_wakus_native_runtime() {
        assert_eq!(
            DISABLE_EXTERNAL_COMPUTER_USE_PLUGIN,
            "plugins.computer-use@openai-bundled.enabled=false"
        );
        assert_eq!(
            DISABLE_EXTERNAL_COMPUTER_USE_MCP,
            "mcp_servers.computer-use.enabled=false"
        );
        assert_eq!(
            DISABLE_EXTERNAL_COMPUTER_USE_SKILL,
            r#"skills.config=[{name="computer-use:computer-use",enabled=false}]"#
        );
    }

    #[test]
    fn access_modes_match_codex_permission_profiles() {
        assert_eq!(
            codex_permissions(RuntimeMode::Ask, InteractionMode::Build),
            ("untrusted", "read-only", "user")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::AutoAcceptEdits, InteractionMode::Build),
            ("on-request", "workspace-write", "user")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::Auto, InteractionMode::Build),
            ("on-request", "workspace-write", "auto_review")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::FullAccess, InteractionMode::Build),
            ("never", "danger-full-access", "user")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::FullAccess, InteractionMode::Plan),
            ("never", "read-only", "user")
        );
    }

    #[test]
    fn missing_launcher_dependencies_are_visible() {
        assert!(is_visible_stderr_notice(
            "env: node: No such file or directory"
        ));
    }

    #[test]
    fn startup_configuration_errors_are_visible() {
        assert!(is_visible_stderr_notice(
            "Error: error loading default config after config error: invalid transport"
        ));
    }

    #[test]
    fn rollback_rpc_responses_are_routed_to_the_waiting_request() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let turn_ids = Mutex::new(vec!["turn-1".to_owned(), "turn-2".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let (response_tx, response_rx) = bounded(1);
        pending_rollbacks.lock().insert(42, (1, response_tx));
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({"id": 42, "result": {}}),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &event_tx,
            &mut stream_state,
        );

        assert_eq!(response_rx.recv().unwrap(), Ok(()));
        assert!(pending_rollbacks.lock().is_empty());
        assert_eq!(*turn_ids.lock(), vec!["turn-1".to_owned()]);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn rollback_rpc_errors_are_returned_without_becoming_stream_errors() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let turn_ids = Mutex::new(vec!["turn-1".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let (response_tx, response_rx) = bounded(1);
        pending_rollbacks.lock().insert(43, (1, response_tx));
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({"id": 43, "error": {"message": "cannot roll back"}}),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &event_tx,
            &mut stream_state,
        );

        assert_eq!(
            response_rx.recv().unwrap(),
            Err("cannot roll back".to_owned())
        );
        assert_eq!(*turn_ids.lock(), vec!["turn-1".to_owned()]);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn fork_rpc_returns_the_new_native_thread() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let turn_ids = Mutex::new(vec!["turn-1".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let (response_tx, response_rx) = bounded(1);
        pending_forks.lock().insert(44, response_tx);
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({"id": 44, "result": {"thread": {"id": "thread-fork"}}}),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &event_tx,
            &mut stream_state,
        );

        assert_eq!(response_rx.recv().unwrap(), Ok("thread-fork".to_owned()));
        assert!(pending_forks.lock().is_empty());
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn fork_turn_selection_keeps_the_requested_completed_prefix() {
        let turns = vec!["turn-1".into(), "turn-2".into(), "turn-3".into()];

        assert_eq!(fork_last_turn_id(&turns, 0), Ok("turn-3".into()));
        assert_eq!(fork_last_turn_id(&turns, 2), Ok("turn-1".into()));
        assert!(fork_last_turn_id(&turns, 3).is_err());
    }

    #[test]
    fn web_search_titles_never_end_with_an_empty_query() {
        let batch_open = json!({
            "type": "webSearch",
            "query": "",
            "action": { "type": "other" },
            "results": [{ "url": "https://openai.com" }, { "url": "https://deepseek.com" }]
        });
        let nested_query = json!({
            "type": "webSearch",
            "query": "",
            "action": { "type": "search", "queries": ["GPT-5.6 Luna official"] }
        });

        assert_eq!(codex_item_title(&batch_open), "Browsed 2 pages");
        assert_eq!(
            codex_item_title(&nested_query),
            "Search for GPT-5.6 Luna official"
        );
    }

    #[test]
    fn web_search_titles_describe_open_and_find_actions() {
        let open = json!({
            "type": "webSearch",
            "query": "",
            "action": { "type": "openPage", "url": "https://openai.com" }
        });
        let find = json!({
            "type": "webSearch",
            "query": "",
            "action": {
                "type": "findInPage",
                "pattern": "pricing",
                "url": "https://openai.com"
            }
        });

        assert_eq!(codex_item_title(&open), "Open https://openai.com");
        assert_eq!(
            codex_item_title(&find),
            "Find pricing in https://openai.com"
        );
    }

    #[test]
    fn citation_markers_become_stable_markdown_links_across_deltas() {
        let mut state = CodexStreamState::default();
        state.begin_turn();
        state.capture_citations(&json!({
            "type": "webSearch",
            "results": [
                { "ref_id": "turn3view0", "url": "https://openai.com/model" },
                { "ref_id": "turn2view2", "url": "https://deepseek.com/model" }
            ]
        }));

        assert_eq!(state.rewrite_citation_delta("Claim.\u{e200}ci"), "Claim.");
        assert_eq!(
            state.rewrite_citation_delta(
                "te\u{e202}turn3view0\u{e202}turn2view2\u{e201}\nNext. \u{e200}cite\u{e202}turn2view2\u{e201}"
            ),
            "[1](https://openai.com/model) [2](https://deepseek.com/model)\nNext. [2](https://deepseek.com/model)"
        );
        assert!(state.citation_buffer.is_empty());
    }

    #[test]
    fn unknown_citation_markers_are_removed() {
        let mut state = CodexStreamState::default();

        assert_eq!(
            state.rewrite_citation_delta("Claim.\u{e200}cite\u{e202}turn9search0\u{e201} After."),
            "Claim. After."
        );
    }

    #[test]
    fn native_dynamic_tool_requests_are_forwarded_even_when_the_rpc_id_is_one() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(Some("turn-1".to_owned()));
        let turn_ids = Mutex::new(vec!["turn-1".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({
                "id": 1,
                "method": "item/tool/call",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "callId": "computer-1",
                    "namespace": computer_use::NAMESPACE,
                    "tool": "list_targets",
                    "arguments": {}
                }
            }),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &event_tx,
            &mut stream_state,
        );

        let DriverEvent::ComputerToolRequested(request) = event_rx.recv().unwrap() else {
            panic!("expected a computer tool request");
        };
        assert_eq!(request.rpc_id, "1");
        assert_eq!(request.call_id, "computer-1");
        assert_eq!(request.tool, "list_targets");
    }

    #[test]
    fn dynamic_tool_activity_keeps_arguments_output_and_failure() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(Some("turn-1".to_owned()));
        let turn_ids = Mutex::new(vec!["turn-1".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();
        let arguments = json!({
            "target": {
                "windowId": 42,
                "bundleId": "net.imput.helium",
                "appName": "Helium",
                "windowTitle": "Window",
                "width": 1440,
                "height": 823
            },
            "actions": []
        });

        handle_codex_message(
            json!({
                "method": "item/completed",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": {
                        "type": "dynamicToolCall",
                        "id": "exec-1",
                        "namespace": computer_use::NAMESPACE,
                        "tool": "use",
                        "arguments": arguments,
                        "status": "failed",
                        "contentItems": [{
                            "type": "inputText",
                            "text": "Computer Use helper closed its session"
                        }],
                        "success": false,
                        "durationMs": 80
                    },
                    "completedAtMs": 1
                }
            }),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &event_tx,
            &mut stream_state,
        );

        let DriverEvent::RichActivity(activity) = event_rx.recv().unwrap() else {
            panic!("expected a rich activity");
        };
        assert_eq!(activity.source_id.as_deref(), Some("exec-1"));
        assert_eq!(activity.title, "Use Helium");
        assert!(activity.arguments.as_deref().unwrap().contains("windowId"));
        assert_eq!(
            activity.output.as_deref(),
            Some("Computer Use helper closed its session")
        );
        assert_eq!(
            activity.detail.as_deref(),
            Some("Computer Use helper closed its session")
        );
        assert!(activity.failed);
        assert!(activity.complete);
    }

    #[test]
    fn dynamic_tool_image_output_is_preserved_for_native_rendering() {
        let image_url = "data:image/png;base64,aGVsbG8=";
        let item = json!({
            "type": "dynamicToolCall",
            "contentItems": [
                {"type": "inputText", "text": "Completed 1 action."},
                {"type": "inputImage", "imageUrl": image_url}
            ]
        });

        assert_eq!(
            codex_item_output(&item).as_deref(),
            Some("Completed 1 action.")
        );
        assert_eq!(codex_item_image_urls(&item), vec![image_url]);
        assert!(!codex_item_output(&item).unwrap().contains("[Image output]"));
    }

    #[test]
    fn mcp_image_content_is_removed_from_text_and_kept_as_an_image() {
        let item = json!({
            "type": "mcpToolCall",
            "result": {
                "content": [
                    {"type": "text", "text": "Screenshot captured."},
                    {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}
                ]
            }
        });

        assert_eq!(
            codex_item_output(&item).as_deref(),
            Some("Screenshot captured.")
        );
        assert_eq!(
            codex_item_image_urls(&item),
            vec!["data:image/png;base64,aGVsbG8=".to_owned()]
        );
    }
}

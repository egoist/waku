use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};
#[cfg(test)]
use uuid::Uuid;

use super::computer_use as computer_use_runtime;
use crate::computer_use;
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{
    ActivityItem, ActivityKind, BackgroundWorkEvent, BackgroundWorkItem, BackgroundWorkKey,
    BackgroundWorkKind, BackgroundWorkStatus, DriverEvent, InteractionMode, PermissionOption,
    ProviderResumeCursor, RuntimeMode, unix_time_millis,
};

const DISABLE_EXTERNAL_COMPUTER_USE_PLUGIN: &str =
    "plugins.computer-use@openai-bundled.enabled=false";
// Codex 0.146 only resolves plugin enablement from user/profile config layers, so a
// process-local `-c` plugin override does not yet suppress its bundled capabilities.
const DISABLE_EXTERNAL_COMPUTER_USE_MCP_COMMAND: &str =
    "mcp_servers.computer-use.command=\"/usr/bin/true\"";
const DISABLE_EXTERNAL_COMPUTER_USE_MCP: &str = "mcp_servers.computer-use.enabled=false";
const DISABLE_EXTERNAL_COMPUTER_USE_SKILL: &str =
    r#"skills.config=[{name="computer-use:computer-use",enabled=false}]"#;
const DISABLE_CODEX_NODE_REPL: &str = "mcp_servers.node_repl.enabled=false";

enum CommandMessage {
    Prompt(String),
    Steer(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    Rollback {
        turns: usize,
        response: Sender<Result<(), String>>,
    },
    Fork {
        turns_to_remove: usize,
        response: Sender<Result<String, String>>,
    },
    Options(SessionOptions),
    RefreshBackgroundWork,
    StopBackgroundWork {
        key: BackgroundWorkKey,
        control_id: String,
    },
    GeneratedTitle(String),
    Shutdown,
}

struct CodexTitleRequest {
    prompt: String,
}

struct CodexTitleGeneration {
    enabled: bool,
    launched: bool,
    pending: Option<CodexTitleRequest>,
}

#[derive(Default)]
struct BackgroundRpcState {
    lists: HashSet<u64>,
    stops: HashMap<u64, BackgroundWorkKey>,
    unsupported: bool,
}

enum PendingBackgroundRpc {
    List,
    Stop(BackgroundWorkKey),
}

impl BackgroundRpcState {
    fn take(&mut self, id: u64) -> Option<PendingBackgroundRpc> {
        if self.lists.remove(&id) {
            Some(PendingBackgroundRpc::List)
        } else {
            self.stops.remove(&id).map(PendingBackgroundRpc::Stop)
        }
    }
}

pub struct CodexDriver {
    commands: Sender<CommandMessage>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    computer_use_process_directory: Option<PathBuf>,
    computer_use_server_path: Option<PathBuf>,
    computer_use_preview_monitor: Option<computer_use_runtime::ComputerUsePreviewMonitor>,
}

struct CodexComputerUseConfig {
    server_path: PathBuf,
    server: String,
    repl: String,
    skill_root: PathBuf,
    process_directory: PathBuf,
    process_directory_config: String,
}

impl CodexComputerUseConfig {
    fn load() -> anyhow::Result<Self> {
        let server_path = computer_use::mcp_server_command()?;
        let server = toml_string(&server_path.display().to_string());
        let repl_path = computer_use::js_repl_server_path()?;
        let repl = toml_string(&repl_path.display().to_string());
        let skill_root = computer_use::skill_root_path()?;
        let process_directory = computer_use_runtime::create_process_directory()?;
        let process_directory_config = toml_string(&process_directory.display().to_string());
        Ok(Self {
            server_path,
            server,
            repl,
            skill_root,
            process_directory,
            process_directory_config,
        })
    }
}

/// Register Waku's long-lived QuickJS MCP server and keep the raw native helper
/// private behind its built-in `sky` object. Codex sees only the compact
/// `js` / `js_reset` execution surface.
fn configure_computer_use_command(command: &mut Command, config: Option<&CodexComputerUseConfig>) {
    if let Some(config) = config {
        command
            .arg("-c")
            .arg(DISABLE_EXTERNAL_COMPUTER_USE_PLUGIN)
            .arg("-c")
            .arg(DISABLE_EXTERNAL_COMPUTER_USE_MCP_COMMAND)
            .arg("-c")
            .arg(DISABLE_EXTERNAL_COMPUTER_USE_MCP)
            .arg("-c")
            .arg(DISABLE_EXTERNAL_COMPUTER_USE_SKILL)
            .arg("-c")
            .arg(DISABLE_CODEX_NODE_REPL)
            .env("WAKU_COMPUTER_USE_SERVER", &config.server_path)
            .env(
                "WAKU_COMPUTER_USE_PROCESS_DIRECTORY",
                &config.process_directory,
            )
            .arg("-c")
            .arg(format!("mcp_servers.waku_js_repl.command={}", config.repl))
            .arg("-c")
            .arg("mcp_servers.waku_js_repl.args=[]")
            .arg("-c")
            .arg(format!(
                "mcp_servers.waku_js_repl.env.WAKU_COMPUTER_USE_SERVER={}",
                config.server
            ))
            .arg("-c")
            .arg(format!(
                "mcp_servers.waku_js_repl.env.WAKU_COMPUTER_USE_PROCESS_DIRECTORY={}",
                config.process_directory_config
            ));
    }
}

impl CodexDriver {
    pub fn start(options: DriverStartOptions, events: DriverEventSender) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
            agent_preset: _,
            computer_use_enabled,
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
        let computer_use = computer_use_enabled
            .then(CodexComputerUseConfig::load)
            .transpose()?;
        let computer_use_skill_root = computer_use
            .as_ref()
            .map(|config| config.skill_root.clone());
        let computer_use_process_directory = computer_use
            .as_ref()
            .map(|config| config.process_directory.clone());
        let computer_use_server_path = computer_use
            .as_ref()
            .map(|config| config.server_path.clone());
        let title_binary = binary.clone();
        let title_cwd = cwd.clone();
        let mut command = crate::command_env::command(&binary);
        command.args(["app-server", "--stdio"]);
        configure_computer_use_command(&mut command, computer_use.as_ref());
        let command = command
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            crate::command_env::spawn(command).context("failed to start `codex app-server`")?;

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
        let computer_use_preview_monitor = computer_use_process_directory
            .as_ref()
            .map(|directory| {
                computer_use_runtime::ComputerUsePreviewMonitor::start(
                    directory.clone(),
                    events.clone(),
                )
            })
            .transpose()?;
        let (commands, command_rx) = unbounded();
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
        let pending_steers = Arc::new(Mutex::new(HashMap::<u64, String>::new()));
        let background_rpcs = Arc::new(Mutex::new(BackgroundRpcState::default()));
        let title_generation = Arc::new(Mutex::new(CodexTitleGeneration {
            enabled: provider_session_id.is_none(),
            launched: false,
            pending: None,
        }));

        let writer_thread_id = thread_id.clone();
        let writer_turn_id = turn_id.clone();
        let writer_turn_ids = turn_ids.clone();
        let writer_pending_rollbacks = pending_rollbacks.clone();
        let writer_pending_forks = pending_forks.clone();
        let writer_pending_steers = pending_steers.clone();
        let writer_background_rpcs = background_rpcs.clone();
        let writer_title_generation = title_generation.clone();
        let writer_events = events.clone();
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
                    let _ = writer_events.send(DriverEvent::Error(tr!(
                        "errors.initialize_codex_app_server"
                    )));
                    return;
                }

                if let Some(computer_use_skill_root) = computer_use_skill_root {
                    // Register Waku's bundled skill through Codex's discoverable-skill
                    // mechanism. Keep the skill out of developerInstructions so it is
                    // loaded and displayed like Codex's own bundled skills.
                    if write_json_line(
                        &mut stdin,
                        &json!({
                            "method": "skills/extraRoots/set",
                            "id": "waku-computer-use-skill",
                            "params": {
                                "extraRoots": [computer_use_skill_root.display().to_string()]
                            }
                        }),
                    )
                    .is_err()
                    {
                        let _ = writer_events.send(DriverEvent::Error(tr!(
                            "errors.register_computer_use_skill"
                        )));
                        return;
                    }
                }

                // Every turn carries its own model, effort and tier, so changing
                // one of those is a new value here rather than a new process.
                // The permission policy is deliberately not in that set — see
                // `apply_options`.
                let (approval_policy, sandbox, approvals_reviewer) =
                    codex_permissions(mode, interaction_mode);
                let mut model = model;
                let mut reasoning_effort = reasoning_effort;
                let mut service_tier = service_tier;
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
                        "serviceName": "waku"
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
                                let _ = writer_events.send(DriverEvent::Error(tr!(
                                    "errors.codex_thread_open_incomplete"
                                )));
                                continue;
                            };
                            {
                                let mut title = writer_title_generation.lock();
                                if title.enabled && !title.launched && title.pending.is_none() {
                                    title.pending = Some(CodexTitleRequest {
                                        prompt: text.clone(),
                                    });
                                }
                            }
                            next_request_id += 1;
                            let mut params = turn_start_params(
                                &thread_id,
                                text,
                                approval_policy,
                                approvals_reviewer,
                                sandbox,
                            );
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
                        CommandMessage::Steer(text) => {
                            let (thread_id, active_turn_id) = (
                                writer_thread_id.lock().clone(),
                                writer_turn_id.lock().clone(),
                            );
                            let (Some(thread_id), Some(expected_turn_id)) =
                                (thread_id, active_turn_id)
                            else {
                                let _ = writer_events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: tr!(
                                        "errors.provider_no_active_turn",
                                        provider = "Codex"
                                    ),
                                });
                                continue;
                            };
                            next_request_id += 1;
                            let request_id = next_request_id;
                            writer_pending_steers
                                .lock()
                                .insert(request_id, text.clone());
                            let message = json!({
                                "method": "turn/steer",
                                "id": request_id,
                                "params": {
                                    "threadId": thread_id,
                                    "expectedTurnId": expected_turn_id,
                                    "input": [{"type": "text", "text": text}]
                                }
                            });
                            if let Err(error) = write_json_line(&mut stdin, &message)
                                && let Some(text) = writer_pending_steers.lock().remove(&request_id)
                            {
                                let _ = writer_events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: tr!(
                                        "errors.provider_transport_write",
                                        provider = "Codex",
                                        error = error
                                    ),
                                });
                            }
                            continue;
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
                        CommandMessage::Options(options) => {
                            model = options.model;
                            reasoning_effort = options.reasoning_effort;
                            service_tier = options.service_tier;
                            continue;
                        }
                        CommandMessage::RefreshBackgroundWork => {
                            if writer_background_rpcs.lock().unsupported {
                                continue;
                            }
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                continue;
                            };
                            next_request_id += 1;
                            writer_background_rpcs.lock().lists.insert(next_request_id);
                            json!({
                                "method": "thread/backgroundTerminals/list",
                                "id": next_request_id,
                                "params": {
                                    "threadId": thread_id,
                                    "cursor": null,
                                    "limit": 100
                                }
                            })
                        }
                        CommandMessage::StopBackgroundWork { key, control_id } => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                let _ = writer_events.send(DriverEvent::BackgroundWork(
                                    BackgroundWorkEvent::StopFailed {
                                        key,
                                        message: tr!("errors.codex_thread_open_incomplete"),
                                    },
                                ));
                                continue;
                            };
                            next_request_id += 1;
                            writer_background_rpcs
                                .lock()
                                .stops
                                .insert(next_request_id, key);
                            json!({
                                "method": "thread/backgroundTerminals/terminate",
                                "id": next_request_id,
                                "params": {
                                    "threadId": thread_id,
                                    "processId": control_id
                                }
                            })
                        }
                        CommandMessage::GeneratedTitle(title) => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                continue;
                            };
                            // Update Waku even if persisting the name back to
                            // Codex fails; the app-server notification will
                            // echo the same value when the write succeeds.
                            let _ = writer_events
                                .send(DriverEvent::AutoTitleUpdated(Some(title.clone())));
                            next_request_id += 1;
                            json!({
                                "method": "thread/name/set",
                                "id": next_request_id,
                                "params": {"threadId": thread_id, "name": title}
                            })
                        }
                        CommandMessage::Shutdown => break,
                    };
                    if let Err(error) = write_json_line(&mut stdin, &message) {
                        let _ = writer_events.send(DriverEvent::Error(tr!(
                            "errors.provider_transport_write",
                            provider = "Codex",
                            error = error
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
        let reader_pending_steers = pending_steers.clone();
        let reader_background_rpcs = background_rpcs.clone();
        let reader_title_generation = title_generation;
        let reader_commands = commands.clone();
        let reader_events = events.clone();
        let reader_thread = thread::Builder::new()
            .name("waku-codex-reader".into())
            .spawn(move || {
                let mut stream_state = CodexStreamState::default();
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if !line.trim().is_empty() => {
                            match serde_json::from_str::<Value>(&line) {
                                Ok(value) => {
                                    let main_turn_started = is_codex_turn_started(
                                        &value,
                                        reader_thread_id.lock().as_deref(),
                                    );
                                    handle_codex_message(
                                        value,
                                        &reader_thread_id,
                                        &reader_turn_id,
                                        &reader_turn_ids,
                                        &reader_pending_rollbacks,
                                        &reader_pending_forks,
                                        &reader_pending_steers,
                                        &reader_background_rpcs,
                                        &reader_events,
                                        &mut stream_state,
                                    );
                                    if main_turn_started {
                                        let request = {
                                            let mut title = reader_title_generation.lock();
                                            if title.enabled && !title.launched {
                                                let request = title.pending.take();
                                                title.launched = request.is_some();
                                                request
                                            } else {
                                                None
                                            }
                                        };
                                        if let Some(request) = request {
                                            let binary = title_binary.clone();
                                            let cwd = title_cwd.clone();
                                            let commands = reader_commands.clone();
                                            let _ = thread::Builder::new()
                                                .name("waku-codex-title".into())
                                                .spawn(move || {
                                                    if let Ok(title) = generate_codex_title(
                                                        &binary,
                                                        &cwd,
                                                        &request.prompt,
                                                    ) {
                                                        let _ = commands.send(
                                                            CommandMessage::GeneratedTitle(title),
                                                        );
                                                    }
                                                });
                                        }
                                    }
                                }
                                Err(error) => {
                                    let _ = reader_events.send(DriverEvent::Error(tr!(
                                        "errors.provider_invalid_json",
                                        provider = "Codex",
                                        error = error
                                    )));
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let _ = reader_events.send(DriverEvent::Error(tr!(
                                "errors.provider_transport_read",
                                provider = "Codex",
                                error = error
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
                        let _ = events.send(DriverEvent::Error(tr!(
                            "errors.provider_exited",
                            provider = "Codex app-server",
                            status = status
                        )));
                    }
                    Err(error) => {
                        let _ = events.send(DriverEvent::Error(tr!(
                            "errors.read_provider_exit_status",
                            provider = "Codex app-server",
                            error = error
                        )));
                    }
                    _ => {}
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            mode,
            interaction_mode,
            computer_use_process_directory,
            computer_use_server_path,
            computer_use_preview_monitor,
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

fn turn_start_params(
    thread_id: &str,
    text: String,
    approval_policy: &str,
    approvals_reviewer: &str,
    sandbox: &str,
) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": text}],
        "approvalPolicy": approval_policy,
        "approvalsReviewer": approvals_reviewer,
        "sandboxPolicy": codex_sandbox_policy(sandbox),
        // Some current models default reasoning summaries to `none`. Waku has
        // a native reasoning disclosure, so explicitly request readable text.
        "summary": "auto"
    })
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("a filesystem path is always valid JSON")
}

impl DriverControl for CodexDriver {
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
        if let (Some(directory), Some(server_path)) = (
            self.computer_use_process_directory.as_deref(),
            self.computer_use_server_path.as_deref(),
        ) {
            computer_use_runtime::stop_registered_processes(directory, server_path);
        }
    }

    fn refresh_background_work(&self) {
        let _ = self.commands.send(CommandMessage::RefreshBackgroundWork);
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

    fn apply_options(&self, options: SessionOptions) -> bool {
        // Model, effort and tier ride on every `turn/start`, so the next turn
        // picks them up. The approval policy and sandbox are not treated the
        // same way even though they are also per-turn fields: loosening or
        // tightening what an already-running agent may touch deserves a fresh
        // thread, so a mode change asks to be restarted instead.
        if options.mode != self.mode || options.interaction_mode != self.interaction_mode {
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
        self.cancel_computer_use();
        drop(self.computer_use_preview_monitor.take());
        if let Some(directory) = self.computer_use_process_directory.as_deref() {
            let _ = fs::remove_dir_all(directory);
        }
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

const CODEX_TITLE_INSTRUCTIONS: &str = "Generate a concise title for the user's coding task. Return only a plain title of at most six words. Do not use tools, quotes, markdown, labels, or ending punctuation.";
const CODEX_TITLE_MODEL: &str = "gpt-5.6-luna";
const CODEX_TITLE_TIMEOUT: Duration = Duration::from_secs(60);

fn is_codex_turn_started(value: &Value, thread_id: Option<&str>) -> bool {
    let Some(thread_id) = thread_id else {
        return false;
    };
    value.get("method").and_then(Value::as_str) == Some("turn/started")
        && value.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
}

fn codex_title_thread_params(cwd: &Path) -> Value {
    json!({
        "cwd": cwd.display().to_string(),
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandbox": "read-only",
        "model": CODEX_TITLE_MODEL,
        "serviceName": "waku-title",
        "baseInstructions": CODEX_TITLE_INSTRUCTIONS,
        "developerInstructions": CODEX_TITLE_INSTRUCTIONS,
        "ephemeral": true
    })
}

fn codex_title_turn_params(thread_id: &str, user_message: &str) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": user_message}],
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandboxPolicy": {"type": "readOnly"},
        "effort": "low",
        "summary": "none",
        "outputSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "minLength": 1, "maxLength": 80}
            },
            "required": ["title"],
            "additionalProperties": false
        }
    })
}

/// Codex app-server can persist a thread name but does not generate one. Match
/// Codex Desktop's client-owned behavior with an isolated ephemeral turn, then
/// hand the result back to the main writer for `thread/name/set`.
fn generate_codex_title(binary: &Path, cwd: &Path, prompt: &str) -> anyhow::Result<String> {
    let mut command = crate::command_env::command(binary);
    let command = command
        .args(["app-server", "--stdio"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child =
        crate::command_env::spawn(command).context("failed to start Codex title generation")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Codex title generation stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Codex title generation stdout is unavailable"))?;
    let (messages, message_rx) = unbounded::<Value>();
    let reader = thread::Builder::new()
        .name("waku-codex-title-reader".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    let _ = messages.send(value);
                }
            }
        })?;

    let result = (|| -> anyhow::Result<String> {
        write_json_line(
            &mut stdin,
            &json!({
                "method": "initialize",
                "id": 0,
                "params": {
                    "clientInfo": {
                        "name": "waku-title",
                        "title": "Waku Title",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {"experimentalApi": true}
                }
            }),
        )?;
        write_json_line(&mut stdin, &json!({"method": "initialized", "params": {}}))?;

        write_json_line(
            &mut stdin,
            &json!({
                "method": "thread/start",
                "id": 1,
                "params": codex_title_thread_params(cwd)
            }),
        )?;

        let deadline = Instant::now() + CODEX_TITLE_TIMEOUT;
        let mut title_thread_id = None::<String>;
        let mut generated = None::<String>;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| anyhow!("Codex title generation timed out"))?;
            let value = message_rx
                .recv_timeout(remaining)
                .context("Codex title generation stopped before completion")?;

            if value.get("id").and_then(Value::as_u64) == Some(1) && value.get("method").is_none() {
                if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
                    return Err(anyhow!("Codex could not open a title thread: {error}"));
                }
                let thread_id = value
                    .pointer("/result/thread/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Codex returned no title thread ID"))?
                    .to_owned();
                title_thread_id = Some(thread_id.clone());
                write_json_line(
                    &mut stdin,
                    &json!({
                        "method": "turn/start",
                        "id": 2,
                        "params": codex_title_turn_params(&thread_id, prompt)
                    }),
                )?;
                continue;
            }

            if value.get("method").and_then(Value::as_str) == Some("item/completed")
                && value.pointer("/params/threadId").and_then(Value::as_str)
                    == title_thread_id.as_deref()
                && value.pointer("/params/item/type").and_then(Value::as_str)
                    == Some("agentMessage")
                && let Some(text) = value.pointer("/params/item/text").and_then(Value::as_str)
            {
                generated = Some(text.to_owned());
                continue;
            }

            if value.get("method").and_then(Value::as_str) == Some("turn/completed")
                && value.pointer("/params/threadId").and_then(Value::as_str)
                    == title_thread_id.as_deref()
            {
                let status = value
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                if status != "completed" {
                    let message = value
                        .pointer("/params/turn/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("the title turn did not complete");
                    return Err(anyhow!("Codex title generation failed: {message}"));
                }
                return generated
                    .as_deref()
                    .and_then(normalize_codex_title)
                    .ok_or_else(|| anyhow!("Codex returned an empty task title"));
            }
        }
    })();

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    result
}

fn normalize_codex_title(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let decoded = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| match value {
            Value::String(title) => Some(title),
            Value::Object(object) => object
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        });
    let candidate = decoded.as_deref().unwrap_or(raw);
    let line = candidate
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```") && *line != "json")?;
    let line = line
        .strip_prefix("Title:")
        .or_else(|| line.strip_prefix("title:"))
        .unwrap_or(line)
        .trim();
    let title = line
        .trim_matches(|character| matches!(character, '"' | '\'' | '`' | '#' | '*' | '_'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim_end_matches(['.', ',', ':', ';']).trim();
    if title.is_empty() {
        return None;
    }
    let title = title.chars().take(80).collect::<String>();
    let title = title.trim_end().to_owned();
    (!title.is_empty()).then_some(title)
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

fn json_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn codex_background_terminal(item: &Value) -> Option<BackgroundWorkItem> {
    let item_id = json_id(item.get("itemId").or_else(|| item.get("id")))?;
    let process_id = json_id(item.get("processId"))?;
    let command = item
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("background.process"));
    let mut work = BackgroundWorkItem::new(
        BackgroundWorkKind::Process,
        item_id,
        command.clone(),
        BackgroundWorkStatus::Running,
    );
    work.command = Some(command);
    work.cwd = item.get("cwd").and_then(Value::as_str).map(str::to_owned);
    let mut telemetry = Vec::new();
    if let Some(pid) = item.get("osPid").and_then(Value::as_u64) {
        telemetry.push(format!("PID {pid}"));
    }
    if let Some(cpu) = item.get("cpuPercent").and_then(Value::as_f64) {
        telemetry.push(format!("CPU {cpu:.1}%"));
    }
    if let Some(rss_kb) = item.get("rssKb").and_then(Value::as_u64) {
        telemetry.push(format!("{:.1} MB", (rss_kb as f64 / 1024.0).max(0.1)));
    }
    if !telemetry.is_empty() {
        work.detail = Some(telemetry.join("  ·  "));
    }
    work.background = true;
    work.can_stop = true;
    work.control_id = Some(process_id);
    work.updated_at_ms = unix_time_millis();
    Some(work)
}

fn codex_command_work(item: &Value, complete: bool) -> Option<BackgroundWorkItem> {
    if item.get("type").and_then(Value::as_str) != Some("commandExecution") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str)?.to_owned();
    let command = item
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("background.command"));
    let exit_code = item
        .get("exitCode")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    let status = if !complete {
        BackgroundWorkStatus::Running
    } else if codex_item_failed(item) || exit_code.is_some_and(|code| code != 0) {
        BackgroundWorkStatus::Failed
    } else {
        BackgroundWorkStatus::Completed
    };
    let mut work = BackgroundWorkItem::new(
        BackgroundWorkKind::Process,
        item_id.clone(),
        command.clone(),
        status,
    );
    work.command = Some(command);
    work.cwd = item.get("cwd").and_then(Value::as_str).map(str::to_owned);
    work.output = item
        .get("aggregatedOutput")
        .and_then(Value::as_str)
        .filter(|output| !output.is_empty())
        .map(str::to_owned);
    work.duration_ms = item
        .get("durationMs")
        .or_else(|| item.get("duration_ms"))
        .and_then(Value::as_u64);
    work.exit_code = exit_code;
    work.control_id = json_id(item.get("processId"));
    work.origin_activity_id = Some(item_id);
    work.updated_at_ms = unix_time_millis();
    Some(work)
}

fn codex_agent_status(status: &str) -> BackgroundWorkStatus {
    match status {
        "pendingInit" => BackgroundWorkStatus::Starting,
        "running" => BackgroundWorkStatus::Running,
        "completed" | "shutdown" => BackgroundWorkStatus::Completed,
        "interrupted" => BackgroundWorkStatus::Stopped,
        "errored" | "notFound" => BackgroundWorkStatus::Failed,
        _ => BackgroundWorkStatus::Running,
    }
}

fn codex_subagent_work(item: &Value) -> Vec<BackgroundWorkItem> {
    if item.get("type").and_then(Value::as_str) != Some("collabAgentToolCall") {
        return Vec::new();
    }
    let origin = item.get("id").and_then(Value::as_str).map(str::to_owned);
    let prompt = item
        .get("prompt")
        .and_then(Value::as_str)
        .and_then(|prompt| prompt.lines().find(|line| !line.trim().is_empty()))
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(|prompt| {
            let mut title = prompt.chars().take(96).collect::<String>();
            if prompt.chars().count() > 96 {
                title.push('…');
            }
            title
        })
        .unwrap_or_else(|| tr!("background.subagent"));
    let model = item.get("model").and_then(Value::as_str).map(str::to_owned);
    let parent_id = item
        .get("senderThreadId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let role = item
        .get("agentType")
        .or_else(|| item.get("role"))
        .or_else(|| item.get("tool"))
        .and_then(Value::as_str)
        .map(split_camel_case);
    item.get("agentsStates")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(thread_id, state)| {
            let status = codex_agent_status(
                state
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("running"),
            );
            let mut work = BackgroundWorkItem::new(
                BackgroundWorkKind::Subagent,
                thread_id,
                prompt.clone(),
                status,
            );
            work.detail = state
                .get("message")
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .map(str::to_owned);
            work.background = true;
            work.origin_activity_id = origin.clone();
            work.role = role.clone();
            work.model = model.clone();
            work.parent_id = parent_id.clone();
            work.updated_at_ms = unix_time_millis();
            work
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn handle_codex_message(
    value: Value,
    thread_id: &Mutex<Option<String>>,
    turn_id: &Mutex<Option<String>>,
    turn_ids: &Mutex<Vec<String>>,
    pending_rollbacks: &Mutex<HashMap<u64, (usize, Sender<Result<(), String>>)>>,
    pending_forks: &Mutex<HashMap<u64, Sender<Result<String, String>>>>,
    pending_steers: &Mutex<HashMap<u64, String>>,
    background_rpcs: &Mutex<BackgroundRpcState>,
    events: &impl DriverEventSink,
    stream_state: &mut CodexStreamState,
) {
    // JSON-RPC IDs are scoped to each peer, so an app-server request may use
    // the same numeric ID as one of Waku's earlier requests. Only messages
    // without a method are responses to Waku-originated requests.
    let is_response = value.get("method").is_none();
    let pending_background = is_response
        .then(|| value.get("id").and_then(Value::as_u64))
        .flatten()
        .and_then(|id| background_rpcs.lock().take(id));
    if let Some(pending) = pending_background {
        match pending {
            PendingBackgroundRpc::List => {
                if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
                    if error.to_ascii_lowercase().contains("method not found")
                        || error.to_ascii_lowercase().contains("unsupported")
                    {
                        background_rpcs.lock().unsupported = true;
                    }
                } else {
                    let items = value
                        .pointer("/result/data")
                        .or_else(|| value.pointer("/result/items"))
                        .or_else(|| value.get("result"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(codex_background_terminal)
                        .collect();
                    let _ = events.send(DriverEvent::BackgroundWork(
                        BackgroundWorkEvent::ReconcileProcesses(items),
                    ));
                }
            }
            PendingBackgroundRpc::Stop(key) => {
                if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
                    let _ = events.send(DriverEvent::BackgroundWork(
                        BackgroundWorkEvent::StopFailed {
                            key,
                            message: error.to_owned(),
                        },
                    ));
                } else if value.pointer("/result/terminated").and_then(Value::as_bool)
                    == Some(false)
                {
                    let _ = events.send(DriverEvent::BackgroundWork(
                        BackgroundWorkEvent::StopFailed {
                            key,
                            message: tr!("background.stop_not_running"),
                        },
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
                    item.updated_at_ms = unix_time_millis();
                    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
                        item,
                    )));
                }
            }
        }
        return;
    }
    if is_response
        && let Some(id) = value.get("id").and_then(Value::as_u64)
        && id != 1
        && let Some(message) = pending_steers.lock().remove(&id)
    {
        if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
            let _ = events.send(DriverEvent::SteerRejected {
                message,
                reason: error.to_owned(),
            });
        } else {
            let _ = events.send(DriverEvent::SteerAccepted { message });
        }
        return;
    }

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
            if let Some(title) = value
                .pointer("/result/thread/name")
                .and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty())
            {
                let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title.to_owned())));
            }
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
        "item/commandExecution/outputDelta" => {
            if let (Some(item_id), Some(delta)) = (
                params.get("itemId").and_then(Value::as_str),
                params.get("delta").and_then(Value::as_str),
            ) && !delta.is_empty()
            {
                let _ = events.send(DriverEvent::BackgroundWork(
                    BackgroundWorkEvent::OutputDelta {
                        key: BackgroundWorkKey::new(BackgroundWorkKind::Process, item_id),
                        delta: delta.to_owned(),
                    },
                ));
            }
        }
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item") {
                stream_state.capture_citations(item);
                let complete = method == "item/completed";
                if let Some(work) = codex_command_work(item, complete) {
                    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
                        work,
                    )));
                }
                for work in codex_subagent_work(item) {
                    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
                        work,
                    )));
                }
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
                    .with_activity_source(Some(item))
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
        "thread/name/updated" => {
            let current_thread_id = thread_id.lock().clone();
            if params.get("threadId").and_then(Value::as_str) == current_thread_id.as_deref() {
                let title = params
                    .get("threadName")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let _ = events.send(DriverEvent::AutoTitleUpdated(title));
            }
        }
        "thread/tokenUsage/updated" => {
            // `last` is the newest model call, so its total is what occupies
            // the context window right now; `total` sums the whole thread.
            let usage = params.get("tokenUsage");
            let tokens = usage
                .and_then(|usage| usage.pointer("/last/totalTokens"))
                .and_then(Value::as_u64)
                .filter(|tokens| *tokens > 0);
            let window = usage
                .and_then(|usage| usage.get("modelContextWindow"))
                .and_then(Value::as_u64)
                .filter(|window| *window > 0);
            if tokens.is_some() || window.is_some() {
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens: tokens,
                    context_window: window,
                });
            }
        }
        "account/rateLimits/updated" => {
            if let Some(plan) = codex_plan_usage(params.get("rateLimits")) {
                let _ = events.send(DriverEvent::PlanUsageUpdated(plan));
            }
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
                        label: tr!("permission.allow_once"),
                        allow: true,
                    },
                    PermissionOption {
                        id: "acceptForSession".into(),
                        label: tr!("permission.allow_for_session"),
                        allow: true,
                    },
                    PermissionOption {
                        id: "decline".into(),
                        label: tr!("common.deny"),
                        allow: false,
                    },
                ],
            });
        }
        _ => {}
    }
}

/// Map an `account/rateLimits/updated` snapshot into the panel's plan rows.
/// The primary window is the short lane (300 min on ChatGPT plans), the
/// secondary the weekly one; both carry a percent and a unix reset time.
fn codex_plan_usage(snapshot: Option<&Value>) -> Option<crate::usage::PlanUsage> {
    let snapshot = snapshot?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(window) = snapshot.get(key).filter(|window| !window.is_null()) else {
            continue;
        };
        let Some(percent) = window.get("usedPercent").and_then(Value::as_f64) else {
            continue;
        };
        windows.push(crate::usage::PlanWindow {
            label: crate::usage::window_label_from_minutes(
                window.get("windowDurationMins").and_then(Value::as_i64),
            ),
            percent: percent.clamp(0.0, 100.0),
            resets_at: window.get("resetsAt").and_then(Value::as_i64),
        });
    }
    if windows.is_empty() {
        return None;
    }
    Some(crate::usage::PlanUsage {
        plan_label: crate::usage::openai_plan_label(
            snapshot.get("planType").and_then(Value::as_str),
        ),
        windows,
    })
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
        Some(
            item.get("tool")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(ActivityKind::from_tool_name)
                .unwrap_or(ActivityKind::Tool),
        )
    } else {
        None
    }
}

fn codex_item_title(item: &Value) -> String {
    if let Some(command) = item.get("command").and_then(Value::as_str) {
        return command.to_owned();
    }
    if let Some(query) = non_empty_string(item.get("query")) {
        return tr!("activity.search_for", query = query);
    }
    if item.get("type").and_then(Value::as_str) == Some("webSearch") {
        return codex_web_search_title(item);
    }
    if item.get("type").and_then(Value::as_str) == Some("mcpToolCall")
        && let Some(title) = item
            .pointer("/arguments/title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
    {
        return title.to_owned();
    }
    if let Some(name) = item.get("tool").and_then(Value::as_str) {
        return split_camel_case(name);
    }
    item.get("type")
        .and_then(Value::as_str)
        .map(split_camel_case)
        .unwrap_or_else(|| tr!("activity.activity"))
}

fn codex_web_search_title(item: &Value) -> String {
    let Some(action) = item.get("action") else {
        return tr!("activity.searched_web");
    };

    match action.get("type").and_then(Value::as_str) {
        Some("search") => {
            if let Some(query) = non_empty_string(action.get("query")) {
                return tr!("activity.search_for", query = query);
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
                return tr!("activity.search_for", query = query);
            }
            tr!("activity.searched_web")
        }
        Some("openPage") => non_empty_string(action.get("url"))
            .map(|url| tr!("activity.open_url", url = url))
            .unwrap_or_else(|| tr!("activity.opened_web_page")),
        Some("findInPage") => match (
            non_empty_string(action.get("pattern")),
            non_empty_string(action.get("url")),
        ) {
            (Some(pattern), Some(url)) => {
                tr!("activity.find_in_url", pattern = pattern, url = url)
            }
            (Some(pattern), None) => tr!("activity.find_on_page", pattern = pattern),
            (None, Some(url)) => tr!("activity.search_within_url", url = url),
            (None, None) => tr!("activity.searched_within_page"),
        },
        _ => item
            .get("results")
            .and_then(Value::as_array)
            .filter(|results| !results.is_empty())
            .map(|results| {
                if results.len() == 1 {
                    tr!("activity.browsed_page", count = results.len())
                } else {
                    tr!("activity.browsed_pages", count = results.len())
                }
            })
            .unwrap_or_else(|| tr!("activity.browsed_web")),
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
    if item.get("type").and_then(Value::as_str) == Some("fileChange") {
        return None;
    }
    item.get("cwd")
        .and_then(Value::as_str)
        .or_else(|| item.get("path").and_then(Value::as_str))
        .or_else(|| item.get("status").and_then(Value::as_str))
        .map(str::to_owned)
}

fn codex_item_arguments(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("mcpToolCall") => item
            .get("arguments")
            .filter(|value| !value.is_null())
            .and_then(format_activity_json),
        Some("fileChange") => item
            .get("changes")
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
                (false, Some(exit)) => tr!(
                    "activity.output_with_exit_code",
                    output = output.trim_end(),
                    code = exit
                ),
                (false, None) => output.trim_end().to_owned(),
                (true, Some(exit)) => tr!("activity.exit_code", code = exit),
                (true, None) => return None,
            };
            non_empty_activity_text(text)
        }
        _ => None,
    }
}

fn codex_item_image_urls(item: &Value) -> Vec<String> {
    let content = match item.get("type").and_then(Value::as_str) {
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
    fn normalizes_structured_and_plain_codex_titles() {
        assert_eq!(
            normalize_codex_title("\"Fix Provider Task Titles\"").as_deref(),
            Some("Fix Provider Task Titles")
        );
        assert_eq!(
            normalize_codex_title("Title: **Repair title updates.**").as_deref(),
            Some("Repair title updates")
        );
        assert_eq!(normalize_codex_title("```\n\n```"), None);
    }

    #[test]
    fn recognizes_only_the_started_main_codex_turn() {
        let started = json!({
            "method":"turn/started",
            "params":{"threadId":"thread-1","turn":{"status":"inProgress"}}
        });
        assert!(is_codex_turn_started(&started, Some("thread-1")));
        assert!(!is_codex_turn_started(&started, Some("thread-2")));
        assert!(!is_codex_turn_started(&started, None));
    }

    #[test]
    fn title_generation_uses_luna_low_and_only_the_user_message() {
        let thread = codex_title_thread_params(Path::new("/tmp/project"));
        let turn = codex_title_turn_params("title-thread", "Fix task titles");

        assert_eq!(thread["model"], "gpt-5.6-luna");
        assert_eq!(turn["effort"], "low");
        assert_eq!(
            turn["input"],
            json!([{"type":"text","text":"Fix task titles"}])
        );
    }

    #[test]
    #[ignore = "requires an installed, authenticated codex"]
    fn codex_title_generation_against_the_real_cli() {
        let binary = crate::command_env::find_executable("codex").expect("codex is not installed");
        let title = generate_codex_title(
            &binary,
            &std::env::temp_dir(),
            "Find why provider-generated task titles never replace the first prompt and fix it.",
        )
        .expect("Codex should generate a task title");
        assert!(!title.is_empty());
        assert!(title.chars().count() <= 80);
    }

    #[test]
    fn turns_request_readable_reasoning_summaries() {
        let params = turn_start_params(
            "thread-1",
            "Inspect the failure".into(),
            "never",
            "user",
            "danger-full-access",
        );

        assert_eq!(params["summary"], "auto");
        assert_eq!(params["threadId"], "thread-1");
        assert_eq!(params["input"][0]["text"], "Inspect the failure");
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

    fn session_options(
        mode: RuntimeMode,
        interaction_mode: InteractionMode,
        model: &str,
    ) -> SessionOptions {
        SessionOptions {
            mode,
            interaction_mode,
            model: Some(model.to_owned()),
            reasoning_effort: None,
            service_tier: None,
        }
    }

    #[test]
    fn model_changes_reach_the_running_thread_but_mode_changes_ask_for_a_restart() {
        let (commands, command_rx) = unbounded();
        let driver = CodexDriver {
            commands,
            mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Build,
            computer_use_process_directory: None,
            computer_use_server_path: None,
            computer_use_preview_monitor: None,
        };

        assert!(driver.apply_options(session_options(
            RuntimeMode::FullAccess,
            InteractionMode::Build,
            "gpt-5-codex"
        )));
        assert!(matches!(
            command_rx.try_recv(),
            Ok(CommandMessage::Options(_))
        ));

        // Both of these change the sandbox the running thread was opened with.
        assert!(!driver.apply_options(session_options(
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            "gpt-5-codex"
        )));
        assert!(!driver.apply_options(session_options(
            RuntimeMode::Ask,
            InteractionMode::Build,
            "gpt-5-codex"
        )));
        assert!(command_rx.try_recv().is_err());
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
    fn computer_use_command_configuration_follows_the_setting() {
        let mut disabled = Command::new("/usr/bin/true");
        configure_computer_use_command(&mut disabled, None);
        let disabled_arguments = disabled
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(disabled_arguments.is_empty());
        assert!(
            disabled
                .get_envs()
                .all(|(name, _)| { !name.to_string_lossy().starts_with("WAKU_COMPUTER_USE_") })
        );

        let config = CodexComputerUseConfig {
            server_path: PathBuf::from("/tmp/waku-computer-use-server"),
            server: toml_string("/tmp/waku-computer-use-server"),
            repl: toml_string("/tmp/waku"),
            skill_root: PathBuf::from("/tmp/waku-computer-use-skill"),
            process_directory: PathBuf::from("/tmp/waku-computer-use-processes"),
            process_directory_config: toml_string("/tmp/waku-computer-use-processes"),
        };
        let mut enabled = Command::new("/usr/bin/true");
        configure_computer_use_command(&mut enabled, Some(&config));
        let enabled_arguments = enabled
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        // The raw helper must never be registered as a Codex MCP server: the
        // Waku REPL owns it and exposes only `sky` inside JavaScript.
        assert!(
            !enabled_arguments
                .iter()
                .any(|argument| argument.contains("mcp_servers.waku_computer_use"))
        );
        assert!(
            enabled_arguments
                .iter()
                .any(|argument| argument.contains("mcp_servers.waku_js_repl.command"))
        );
        assert!(
            enabled_arguments
                .iter()
                .any(|argument| { argument == "mcp_servers.waku_js_repl.args=[]" })
        );
        assert!(
            enabled_arguments
                .iter()
                .any(|argument| argument == DISABLE_EXTERNAL_COMPUTER_USE_PLUGIN)
        );
        assert!(
            enabled_arguments
                .iter()
                .any(|argument| argument == DISABLE_EXTERNAL_COMPUTER_USE_MCP)
        );
        assert!(
            enabled_arguments
                .iter()
                .any(|argument| argument == DISABLE_EXTERNAL_COMPUTER_USE_SKILL)
        );
        assert!(
            enabled_arguments
                .iter()
                .any(|argument| argument == DISABLE_CODEX_NODE_REPL)
        );
        assert!(
            enabled
                .get_envs()
                .any(|(name, _)| { name.to_string_lossy() == "WAKU_COMPUTER_USE_SERVER" })
        );
    }

    #[test]
    fn computer_use_process_registry_accepts_only_pid_files() {
        let directory = std::env::temp_dir().join(format!(
            "waku-computer-use-process-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(directory.join("456")).unwrap();
        fs::write(directory.join("123"), []).unwrap();
        fs::write(directory.join("not-a-pid"), []).unwrap();

        let processes = computer_use_runtime::registered_processes(&directory);
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].0, 123);
        assert_eq!(processes[0].1, directory.join("123"));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn computer_use_cleanup_verifies_the_registered_executable() {
        let current = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(
            computer_use_runtime::process_executable(std::process::id() as i32),
            Some(current)
        );
    }

    #[test]
    fn provider_thread_names_are_forwarded_only_for_the_current_thread() {
        let thread_id = Mutex::new(None);
        let turn_id = Mutex::new(None);
        let turn_ids = Mutex::new(Vec::new());
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();
        let mut send = |value| {
            handle_codex_message(
                value,
                &thread_id,
                &turn_id,
                &turn_ids,
                &pending_rollbacks,
                &pending_forks,
                &pending_steers,
                &background_rpcs,
                &event_tx,
                &mut stream_state,
            );
        };

        // `thread/start` and `thread/resume` both return the current name.
        send(json!({
            "id": 1,
            "result": {
                "thread": {
                    "id": "thread-1",
                    "name": "Initial provider title",
                    "turns": []
                }
            }
        }));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Initial provider title"
        ));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Codex { thread_id })
            } if thread_id == "thread-1"
        ));

        // Exact notification shape from Codex's generated app-server protocol.
        send(json!({
            "method": "thread/name/updated",
            "params": {"threadId": "thread-1", "threadName": "Updated provider title"}
        }));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Updated provider title"
        ));

        send(json!({
            "method": "thread/name/updated",
            "params": {"threadId": "another-thread", "threadName": "Not ours"}
        }));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn rollback_rpc_responses_are_routed_to_the_waiting_request() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let turn_ids = Mutex::new(vec!["turn-1".to_owned(), "turn-2".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
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
            &pending_steers,
            &background_rpcs,
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
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
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
            &pending_steers,
            &background_rpcs,
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
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
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
            &pending_steers,
            &background_rpcs,
            &event_tx,
            &mut stream_state,
        );

        assert_eq!(response_rx.recv().unwrap(), Ok("thread-fork".to_owned()));
        assert!(pending_forks.lock().is_empty());
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn steer_rpc_success_is_reported_as_an_accepted_steer() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(Some("turn-9".to_owned()));
        let turn_ids = Mutex::new(vec!["turn-9".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
        pending_steers
            .lock()
            .insert(50, "Focus on the failing tests first".to_owned());
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({"id": 50, "result": {"turnId": "turn-9"}}),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &pending_steers,
            &background_rpcs,
            &event_tx,
            &mut stream_state,
        );

        assert!(pending_steers.lock().is_empty());
        match event_rx.try_recv().unwrap() {
            DriverEvent::SteerAccepted { message } => {
                assert_eq!(message, "Focus on the failing tests first");
            }
            other => panic!("expected an accepted steer, got {other:?}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn steer_rpc_errors_fall_back_to_a_rejected_steer() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(Some("turn-9".to_owned()));
        let turn_ids = Mutex::new(vec!["turn-9".to_owned()]);
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
        pending_steers.lock().insert(51, "Steer me".to_owned());
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({"id": 51, "error": {"message": "expected turn mismatch"}}),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &pending_steers,
            &background_rpcs,
            &event_tx,
            &mut stream_state,
        );

        assert!(pending_steers.lock().is_empty());
        match event_rx.try_recv().unwrap() {
            DriverEvent::SteerRejected { message, reason } => {
                assert_eq!(message, "Steer me");
                assert_eq!(reason, "expected turn mismatch");
            }
            other => panic!("expected a rejected steer, got {other:?}"),
        }
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
    fn background_terminal_snapshot_preserves_native_control_ids() {
        let item = codex_background_terminal(&json!({
            "itemId": "item-7",
            "processId": "process-9",
            "command": "bun run dev",
            "cwd": "/tmp/project",
            "osPid": 123,
            "cpuPercent": 1.5,
            "rssKb": 4096
        }))
        .unwrap();
        assert_eq!(item.key.provider_id, "item-7");
        assert_eq!(item.control_id.as_deref(), Some("process-9"));
        assert_eq!(item.status, BackgroundWorkStatus::Running);
        assert!(item.background && item.can_stop);
    }

    #[test]
    fn background_terminal_list_responses_reconcile_the_registry() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let turn_ids = Mutex::new(Vec::new());
        let pending_rollbacks = Mutex::new(HashMap::new());
        let pending_forks = Mutex::new(HashMap::new());
        let pending_steers = Mutex::new(HashMap::new());
        let background_rpcs = Mutex::new(BackgroundRpcState::default());
        background_rpcs.lock().lists.insert(70);
        let (event_tx, event_rx) = unbounded();
        let mut stream_state = CodexStreamState::default();

        handle_codex_message(
            json!({
                "id": 70,
                "result": {"data": [{
                    "itemId": "item-7",
                    "processId": "process-9",
                    "command": "bun run dev",
                    "cwd": "/tmp/project"
                }]}
            }),
            &thread_id,
            &turn_id,
            &turn_ids,
            &pending_rollbacks,
            &pending_forks,
            &pending_steers,
            &background_rpcs,
            &event_tx,
            &mut stream_state,
        );

        let DriverEvent::BackgroundWork(BackgroundWorkEvent::ReconcileProcesses(items)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("the terminal snapshot should be reconciled");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].control_id.as_deref(), Some("process-9"));
        assert!(background_rpcs.lock().lists.is_empty());
    }

    #[test]
    fn collab_agent_states_become_subagent_work() {
        let items = codex_subagent_work(&json!({
            "type": "collabAgentToolCall",
            "id": "collab-1",
            "tool": "spawnAgent",
            "prompt": "Inspect the parser\nand report back",
            "senderThreadId": "root-thread",
            "agentsStates": {
                "agent-thread": {"status": "running", "message": "Reading source"}
            }
        }));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key.kind, BackgroundWorkKind::Subagent);
        assert_eq!(items[0].key.provider_id, "agent-thread");
        assert_eq!(items[0].origin_activity_id.as_deref(), Some("collab-1"));
        assert_eq!(items[0].status, BackgroundWorkStatus::Running);
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
    fn mcp_tool_title_prefers_the_human_facing_argument() {
        let titled = json!({
            "type": "mcpToolCall",
            "server": "waku_js_repl",
            "tool": "js",
            "arguments": {
                "title": "Inspect Helium browser",
                "code": "sky.get_app_state({ app: 'Helium' })"
            }
        });
        let untitled = json!({
            "type": "mcpToolCall",
            "server": "waku_js_repl",
            "tool": "js",
            "arguments": { "code": "sky.list_apps()" }
        });

        assert_eq!(codex_item_title(&titled), "Inspect Helium browser");
        assert_eq!(codex_item_title(&untitled), "Js");
    }

    #[test]
    fn mcp_file_tools_use_the_shared_file_presentation_metadata() {
        let item = json!({
            "id": "read-1",
            "type": "mcpToolCall",
            "server": "filesystem",
            "tool": "read_file",
            "arguments": {"path": "/tmp/waku/src/model.rs"},
            "status": "inProgress"
        });

        let kind = codex_activity_kind(&item).expect("MCP call should be an activity");
        let activity = ActivityItem::new(
            Some("read-1".into()),
            kind,
            codex_item_title(&item),
            None,
            false,
        )
        .with_arguments(codex_item_arguments(&item))
        .with_activity_source(Some(&item));

        assert_eq!(activity.kind, ActivityKind::FileRead);
        assert_eq!(
            activity.display_target.as_deref(),
            Some("/tmp/waku/src/model.rs")
        );
    }

    #[test]
    fn file_change_arguments_preserve_provider_diff_metadata() {
        let item = json!({
            "id": "patch-1",
            "type": "fileChange",
            "changes": [{
                "path": "src/app.rs",
                "kind": {"type": "update"},
                "diff": "@@ -1 +1,2 @@\n-old\n+new\n+more"
            }],
            "status": "completed"
        });

        let arguments = codex_item_arguments(&item).expect("file changes should be retained");
        let activity = ActivityItem::new(
            Some("patch-1".into()),
            ActivityKind::FileChange,
            codex_item_title(&item),
            None,
            true,
        )
        .with_arguments(Some(arguments));

        assert_eq!(activity.file_changes.len(), 1);
        assert_eq!(activity.file_changes[0].path, "src/app.rs");
        assert_eq!(activity.file_changes[0].additions, Some(2));
        assert_eq!(activity.file_changes[0].deletions, Some(1));
        assert_eq!(codex_item_detail(&item, None), None);
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

    #[test]
    fn rate_limit_snapshots_become_plan_rows() {
        // Shape from the app-server protocol schema (v2): RateLimitSnapshot.
        let snapshot = json!({
            "planType": "plus",
            "primary": {"usedPercent": 37, "windowDurationMins": 300, "resetsAt": 1_800_000_000},
            "secondary": {"usedPercent": 8, "windowDurationMins": 10080, "resetsAt": 1_800_500_000}
        });

        let plan = codex_plan_usage(Some(&snapshot)).expect("both windows should map");
        assert_eq!(plan.plan_label.as_deref(), Some("Plus"));
        assert_eq!(
            plan.windows
                .iter()
                .map(|window| (window.label.as_str(), window.percent, window.resets_at))
                .collect::<Vec<_>>(),
            [
                ("5-hour limit", 37.0, Some(1_800_000_000)),
                ("Weekly limit", 8.0, Some(1_800_500_000)),
            ]
        );

        assert!(codex_plan_usage(Some(&json!({"planType": "plus"}))).is_none());
        assert!(codex_plan_usage(None).is_none());
    }
}

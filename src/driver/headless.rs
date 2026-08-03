use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use serde_json::Value;
use uuid::Uuid;

use super::{activity, computer_use as computer_use_runtime};
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
    computer_use: Option<HeadlessComputerUseRuntime>,
}

#[derive(Clone)]
enum HeadlessComputerUseConfig {
    OpenCode {
        base: computer_use_runtime::ComputerUseConfig,
        config_content: String,
    },
    Grok {
        base: computer_use_runtime::ComputerUseConfig,
        grok_home: PathBuf,
        auth_path: Option<PathBuf>,
        rules: String,
    },
}

struct HeadlessComputerUseRuntime {
    runtime: computer_use_runtime::ComputerUseRuntime,
    config: HeadlessComputerUseConfig,
}

impl HeadlessComputerUseRuntime {
    fn start(provider: ProviderKind, events: Sender<DriverEvent>) -> anyhow::Result<Self> {
        let runtime = computer_use_runtime::ComputerUseRuntime::start(events)?;
        let config = match provider {
            ProviderKind::OpenCode => {
                let existing = match std::env::var("OPENCODE_CONFIG_CONTENT") {
                    Ok(content) => Some(content),
                    Err(std::env::VarError::NotPresent) => None,
                    Err(std::env::VarError::NotUnicode(_)) => {
                        return Err(anyhow!("OPENCODE_CONFIG_CONTENT is not valid UTF-8"));
                    }
                };
                let base = runtime.config.clone();
                let config_content = build_opencode_computer_use_config(
                    existing.as_deref(),
                    &base.server_path,
                    &base.repl_path,
                    &base.skill_path,
                    &base.process_directory,
                )?;
                HeadlessComputerUseConfig::OpenCode {
                    base,
                    config_content,
                }
            }
            ProviderKind::Grok => build_grok_computer_use_config(runtime.config.clone())?,
            _ => return Err(anyhow!("Computer Use is not supported by this driver")),
        };
        Ok(Self { runtime, config })
    }

    fn stop(&self) {
        self.runtime.stop();
    }
}

fn build_opencode_computer_use_config(
    existing: Option<&str>,
    server_path: &Path,
    repl_path: &Path,
    skill_path: &Path,
    process_directory: &Path,
) -> anyhow::Result<String> {
    let mut config = existing
        .map(serde_json::from_str::<Value>)
        .transpose()
        .context("OPENCODE_CONFIG_CONTENT is invalid JSON")?
        .unwrap_or_else(|| serde_json::json!({}));
    let root = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT must contain a JSON object"))?;
    let mcp = root
        .entry("mcp")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT.mcp must be a JSON object"))?;
    mcp.insert(
        "waku_js_repl".into(),
        serde_json::json!({
            "type": "local",
            "command": [repl_path.display().to_string()],
            "enabled": true,
            "environment": {
                "WAKU_COMPUTER_USE_SERVER": server_path.display().to_string(),
                "WAKU_COMPUTER_USE_PROCESS_DIRECTORY": process_directory.display().to_string(),
            },
        }),
    );
    let instructions = root
        .entry("instructions")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT.instructions must be a JSON array"))?;
    let skill_path = skill_path.display().to_string();
    if !instructions
        .iter()
        .any(|instruction| instruction.as_str() == Some(&skill_path))
    {
        instructions.push(Value::String(skill_path));
    }
    serde_json::to_string(&config).context("could not encode OpenCode Computer Use configuration")
}

fn configure_opencode_computer_use_command(
    command: &mut Command,
    config: Option<&HeadlessComputerUseConfig>,
) {
    if let Some(HeadlessComputerUseConfig::OpenCode {
        base,
        config_content,
    }) = config
    {
        command
            .env("OPENCODE_CONFIG_CONTENT", config_content)
            .env("WAKU_COMPUTER_USE_SERVER", &base.server_path)
            .env(
                "WAKU_COMPUTER_USE_PROCESS_DIRECTORY",
                &base.process_directory,
            );
    }
}

fn build_grok_computer_use_config(
    base: computer_use_runtime::ComputerUseConfig,
) -> anyhow::Result<HeadlessComputerUseConfig> {
    let source_home = match std::env::var_os("GROK_HOME") {
        Some(home) => PathBuf::from(home),
        None => dirs::home_dir()
            .ok_or_else(|| anyhow!("home directory is unavailable"))?
            .join(".grok"),
    };
    let grok_home = base.process_directory.join("grok-home");
    fs::create_dir(&grok_home).with_context(|| {
        format!(
            "could not create isolated Grok home {}",
            grok_home.display()
        )
    })?;
    fs::set_permissions(&grok_home, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "could not secure isolated Grok home {}",
            grok_home.display()
        )
    })?;
    if source_home.is_dir() {
        for entry in fs::read_dir(&source_home)
            .with_context(|| format!("could not read Grok home {}", source_home.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("config.toml" | "auth.json" | "auth.json.lock")
            ) {
                continue;
            }
            symlink(entry.path(), grok_home.join(name)).with_context(|| {
                format!(
                    "could not mirror Grok runtime resource {}",
                    entry.path().display()
                )
            })?;
        }
    }
    let existing = match fs::read_to_string(source_home.join("config.toml")) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "could not read {}",
                    source_home.join("config.toml").display()
                )
            });
        }
    };
    let config_content = build_grok_computer_use_toml(existing.as_deref(), &base)?;
    fs::write(grok_home.join("config.toml"), config_content).with_context(|| {
        format!(
            "could not write isolated Grok config {}",
            grok_home.join("config.toml").display()
        )
    })?;
    let auth_path = std::env::var_os("GROK_AUTH_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            let path = source_home.join("auth.json");
            path.is_file().then_some(path)
        });
    let rules = fs::read_to_string(&base.skill_path).with_context(|| {
        format!(
            "could not read Waku Computer Use skill {}",
            base.skill_path.display()
        )
    })?;
    Ok(HeadlessComputerUseConfig::Grok {
        base,
        grok_home,
        auth_path,
        rules,
    })
}

fn build_grok_computer_use_toml(
    existing: Option<&str>,
    base: &computer_use_runtime::ComputerUseConfig,
) -> anyhow::Result<String> {
    let mut root = match existing.filter(|content| !content.trim().is_empty()) {
        Some(content) => {
            toml::from_str::<toml::Table>(content).context("Grok config.toml is invalid TOML")?
        }
        None => toml::Table::new(),
    };
    let mcp_servers = root
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("Grok config.toml mcp_servers must be a table"))?;
    let mut environment = toml::Table::new();
    environment.insert(
        "WAKU_COMPUTER_USE_SERVER".into(),
        toml::Value::String(base.server_path.display().to_string()),
    );
    environment.insert(
        "WAKU_COMPUTER_USE_PROCESS_DIRECTORY".into(),
        toml::Value::String(base.process_directory.display().to_string()),
    );
    let mut server = toml::Table::new();
    server.insert(
        "command".into(),
        toml::Value::String(base.repl_path.display().to_string()),
    );
    server.insert("args".into(), toml::Value::Array(Vec::new()));
    server.insert("env".into(), toml::Value::Table(environment));
    server.insert("enabled".into(), toml::Value::Boolean(true));
    mcp_servers.insert("waku_js_repl".into(), toml::Value::Table(server));
    toml::to_string(&root).context("could not encode Grok Computer Use configuration")
}

fn configure_grok_computer_use_command(
    command: &mut Command,
    config: Option<&HeadlessComputerUseConfig>,
) {
    if let Some(HeadlessComputerUseConfig::Grok {
        base,
        grok_home,
        auth_path,
        rules,
    }) = config
    {
        command
            .env("GROK_HOME", grok_home)
            .env("WAKU_COMPUTER_USE_SERVER", &base.server_path)
            .env(
                "WAKU_COMPUTER_USE_PROCESS_DIRECTORY",
                &base.process_directory,
            )
            .arg(format!("--rules={rules}"));
        if let Some(auth_path) = auth_path {
            command.env("GROK_AUTH_PATH", auth_path);
        }
    }
}

fn provider_stderr_error(lines: Vec<String>) -> Option<String> {
    let first_error = lines
        .iter()
        .find(|line| line.to_ascii_lowercase().contains("error"))?
        .trim();

    // CLI parsers can echo a rejected multi-line argument in full. The first
    // diagnostic already identifies the failure; forwarding the rest would
    // turn provider stderr into an enormous assistant message.
    if first_error.to_ascii_lowercase().starts_with("error:") {
        return Some(truncate_error(first_error, 400));
    }

    let mut message = String::new();
    let first_error_index = lines
        .iter()
        .position(|line| line.trim() == first_error)
        .unwrap_or_default();
    for line in lines.iter().skip(first_error_index).take(6) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !message.is_empty() {
            message.push('\n');
        }
        message.push_str(line);
        if message.chars().count() >= 800 {
            break;
        }
    }
    Some(truncate_error(&message, 800))
}

fn truncate_error(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_owned();
    }
    let mut truncated = message.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
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
            computer_use_enabled,
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
        let computer_use = (matches!(provider, ProviderKind::OpenCode | ProviderKind::Grok)
            && computer_use_enabled)
            .then(|| HeadlessComputerUseRuntime::start(provider, events.clone()))
            .transpose()?;
        let worker_computer_use = computer_use.as_ref().map(|runtime| runtime.config.clone());

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
                                worker_computer_use.as_ref(),
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
            computer_use,
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

    fn cancel_computer_use(&self) {
        if let Some(computer_use) = self.computer_use.as_ref() {
            computer_use.stop();
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
        self.cancel_computer_use();
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
    computer_use: Option<&HeadlessComputerUseConfig>,
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
            configure_opencode_computer_use_command(&mut command, computer_use);
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
            configure_grok_computer_use_command(&mut command, computer_use);
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
            if let Some(message) = provider_stderr_error(lines) {
                let _ =
                    stderr_events.send(DriverEvent::Error(format!("{provider_name}: {message}")));
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
    opencode_tools: HashMap<String, (ActivityKind, String)>,
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
                                let wire_title = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Tool")
                                    .to_owned();
                                let kind = classify_tool(&wire_title);
                                let title =
                                    activity::input_title(block.get("input")).unwrap_or(wire_title);
                                if let Some(id) = &id {
                                    self.amp_tools.insert(id.clone(), (kind, title.clone()));
                                }
                                let item = activity::tool_activity(
                                    id,
                                    kind,
                                    title,
                                    block.get("input"),
                                    None,
                                    None,
                                    false,
                                    false,
                                );
                                let _ = events.send(DriverEvent::RichActivity(item));
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
                        let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
                        let item = activity::tool_activity(
                            id,
                            kind,
                            title,
                            None,
                            block.get("content"),
                            block.get("content"),
                            failed,
                            true,
                        );
                        let _ = events.send(DriverEvent::RichActivity(item));
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
                                let wire_title = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Tool")
                                    .to_owned();
                                let kind = classify_tool(&wire_title);
                                let title =
                                    activity::input_title(block.get("input")).unwrap_or(wire_title);
                                if let Some(id) = &id {
                                    self.claude_tools.insert(id.clone(), (kind, title.clone()));
                                }
                                let item = activity::tool_activity(
                                    id,
                                    kind,
                                    title,
                                    block.get("input"),
                                    None,
                                    None,
                                    false,
                                    false,
                                );
                                let _ = events.send(DriverEvent::RichActivity(item));
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
                        let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
                        let item = activity::tool_activity(
                            id,
                            kind,
                            title,
                            None,
                            block.get("content"),
                            block.get("content"),
                            failed,
                            true,
                        );
                        let _ = events.send(DriverEvent::RichActivity(item));
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
                let title = activity::input_title(payload.get("args"))
                    .unwrap_or_else(|| cursor_tool_title(wire_name));
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
                let output = payload.get("result");
                let failed = value.get("is_error").and_then(Value::as_bool) == Some(true)
                    || payload.get("isError").and_then(Value::as_bool) == Some(true);
                let item = activity::tool_activity(
                    id,
                    kind,
                    title,
                    payload.get("args"),
                    output,
                    output,
                    failed,
                    complete,
                );
                let _ = events.send(DriverEvent::RichActivity(item));
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
                let wire_title = part
                    .get("tool")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("name").and_then(Value::as_str))
                    .unwrap_or("Tool")
                    .to_owned();
                let id = part
                    .get("callID")
                    .or_else(|| part.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let arguments = part.pointer("/state/input").or_else(|| part.get("input"));
                let requested_title = activity::input_title(arguments);
                let complete = matches!(
                    part.pointer("/state/status").and_then(Value::as_str),
                    Some("completed" | "error")
                );
                let stored = id.as_ref().and_then(|id| {
                    if complete {
                        self.opencode_tools.remove(id)
                    } else {
                        self.opencode_tools.get(id).cloned()
                    }
                });
                let kind = stored
                    .as_ref()
                    .map(|(kind, _)| *kind)
                    .unwrap_or_else(|| classify_tool(&wire_title));
                let title = requested_title
                    .or_else(|| stored.map(|(_, title)| title))
                    .unwrap_or(wire_title);
                if !complete && let Some(id) = id.as_ref() {
                    self.opencode_tools
                        .insert(id.clone(), (kind, title.clone()));
                }
                let failed = opencode_tool_failed(part);
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
                let raw_input = value.get("rawInput").filter(|input| !input.is_null());
                let title = activity::input_title(raw_input)
                    .or_else(|| {
                        value
                            .get("title")
                            .and_then(Value::as_str)
                            .filter(|title| !title.is_empty())
                            .map(str::to_owned)
                    })
                    .or_else(|| {
                        value
                            .get("toolName")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "Tool".to_owned());
                let kind = classify_grok_tool(
                    value.get("kind").and_then(Value::as_str),
                    value.get("toolName").and_then(Value::as_str),
                    &title,
                );
                let complete = matches!(
                    value.get("status").and_then(Value::as_str),
                    Some("completed" | "failed")
                );
                if !complete && let Some(id) = &id {
                    self.grok_tools.insert(id.clone(), (kind, title.clone()));
                }
                let output = value.get("rawOutput").filter(|output| !output.is_null());
                let failed = value.get("status").and_then(Value::as_str) == Some("failed");
                let item = activity::tool_activity(
                    id, kind, title, raw_input, output, output, failed, complete,
                );
                let _ = events.send(DriverEvent::RichActivity(item));
            }
            Some("tool_call_update") => {
                let id = value
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let complete = matches!(
                    value.get("status").and_then(Value::as_str),
                    Some("completed" | "failed")
                );
                let (kind, title) = id
                    .as_ref()
                    .and_then(|id| {
                        if complete {
                            self.grok_tools.remove(id)
                        } else {
                            self.grok_tools.get(id).cloned()
                        }
                    })
                    .unwrap_or((ActivityKind::Tool, "Tool".to_owned()));
                let output = value.get("rawOutput").filter(|output| !output.is_null());
                let failed = value.get("status").and_then(Value::as_str) == Some("failed");
                let item = activity::tool_activity(
                    id, kind, title, None, output, output, failed, complete,
                );
                let _ = events.send(DriverEvent::RichActivity(item));
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

fn opencode_tool_failed(part: &Value) -> bool {
    part.pointer("/state/status").and_then(Value::as_str) == Some("error")
        || part
            .pointer("/state/error")
            .is_some_and(|error| !error.is_null())
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

    fn computer_use_config() -> computer_use_runtime::ComputerUseConfig {
        computer_use_runtime::ComputerUseConfig {
            server_path: PathBuf::from("/tmp/Waku Computer Use"),
            repl_path: PathBuf::from("/Applications/Waku.app/Contents/Resources/waku_js_repl"),
            skill_path: PathBuf::from(
                "/Applications/Waku.app/Contents/Resources/skills/waku-computer-use/SKILL.md",
            ),
            process_directory: PathBuf::from("/tmp/waku-computer-use/session"),
        }
    }

    #[test]
    fn opencode_computer_use_config_preserves_existing_inline_config() {
        let content = build_opencode_computer_use_config(
            Some(
                r#"{
                    "mcp": {
                        "existing": {
                            "type": "local",
                            "command": ["existing-server"],
                            "enabled": true
                        }
                    },
                    "instructions": ["existing.md"],
                    "plugin": ["existing-plugin"]
                }"#,
            ),
            Path::new("/Applications/Waku Computer Use"),
            Path::new("/Applications/Waku.app/Contents/Resources/waku_js_repl"),
            Path::new(
                "/Applications/Waku.app/Contents/Resources/skills/waku-computer-use/SKILL.md",
            ),
            Path::new("/tmp/waku computer use/session"),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            value
                .pointer("/mcp/existing/command/0")
                .and_then(Value::as_str),
            Some("existing-server")
        );
        assert_eq!(
            value
                .pointer("/mcp/waku_js_repl/command/0")
                .and_then(Value::as_str),
            Some("/Applications/Waku.app/Contents/Resources/waku_js_repl")
        );
        assert_eq!(
            value
                .pointer("/mcp/waku_js_repl/environment/WAKU_COMPUTER_USE_SERVER")
                .and_then(Value::as_str),
            Some("/Applications/Waku Computer Use")
        );
        assert_eq!(
            value.get("instructions").and_then(Value::as_array).unwrap(),
            &[
                Value::String("existing.md".into()),
                Value::String(
                    "/Applications/Waku.app/Contents/Resources/skills/waku-computer-use/SKILL.md"
                        .into(),
                ),
            ]
        );
        assert_eq!(
            value.pointer("/plugin/0").and_then(Value::as_str),
            Some("existing-plugin")
        );
        assert!(value.pointer("/mcp/waku_computer_use").is_none());
    }

    #[test]
    fn opencode_computer_use_command_is_process_scoped() {
        let config_content = r#"{"mcp":{"waku_js_repl":{}}}"#.to_owned();
        let config = HeadlessComputerUseConfig::OpenCode {
            base: computer_use_config(),
            config_content: config_content.clone(),
        };
        let mut command = Command::new("opencode");

        configure_opencode_computer_use_command(&mut command, Some(&config));

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
            environment.get("OPENCODE_CONFIG_CONTENT"),
            Some(&Some(config_content))
        );
        assert_eq!(
            environment.get("WAKU_COMPUTER_USE_SERVER"),
            Some(&Some("/tmp/Waku Computer Use".into()))
        );
        assert_eq!(
            environment.get("WAKU_COMPUTER_USE_PROCESS_DIRECTORY"),
            Some(&Some("/tmp/waku-computer-use/session".into()))
        );
    }

    #[test]
    fn grok_computer_use_config_preserves_existing_config_and_replaces_waku_server() {
        let content = build_grok_computer_use_toml(
            Some(
                r#"
                    default_model = "grok-code-fast"

                    [mcp_servers.existing]
                    command = "existing-server"

                    [mcp_servers.waku_js_repl]
                    command = "stale-server"
                "#,
            ),
            &computer_use_config(),
        )
        .unwrap();
        let value: toml::Value = toml::from_str(&content).unwrap();

        assert_eq!(
            value.get("default_model").and_then(toml::Value::as_str),
            Some("grok-code-fast")
        );
        assert_eq!(
            value
                .get("mcp_servers")
                .and_then(|mcp| mcp.get("existing"))
                .and_then(|server| server.get("command"))
                .and_then(toml::Value::as_str),
            Some("existing-server")
        );
        let server = value
            .get("mcp_servers")
            .and_then(|mcp| mcp.get("waku_js_repl"))
            .unwrap();
        assert_eq!(
            server.get("command").and_then(toml::Value::as_str),
            Some("/Applications/Waku.app/Contents/Resources/waku_js_repl")
        );
        assert_eq!(
            server
                .get("env")
                .and_then(|env| env.get("WAKU_COMPUTER_USE_SERVER"))
                .and_then(toml::Value::as_str),
            Some("/tmp/Waku Computer Use")
        );
    }

    #[test]
    fn grok_computer_use_command_is_process_scoped_and_loads_rules() {
        let config = HeadlessComputerUseConfig::Grok {
            base: computer_use_config(),
            grok_home: PathBuf::from("/tmp/waku-computer-use/session/grok-home"),
            auth_path: Some(PathBuf::from("/Users/test/.grok/auth.json")),
            rules: "Waku Computer Use rules".into(),
        };
        let mut command = Command::new("grok");

        configure_grok_computer_use_command(&mut command, Some(&config));

        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(arguments, ["--rules=Waku Computer Use rules"]);
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
            environment.get("GROK_HOME"),
            Some(&Some("/tmp/waku-computer-use/session/grok-home".into()))
        );
        assert_eq!(
            environment.get("GROK_AUTH_PATH"),
            Some(&Some("/Users/test/.grok/auth.json".into()))
        );
    }

    #[test]
    fn provider_stderr_keeps_cli_argument_errors_compact() {
        let message = provider_stderr_error(vec![
            "error: unexpected argument '---".into(),
            "name: waku-computer-use".into(),
            "description: a very long bundled skill".into(),
            "---' found".into(),
            "tip: to pass it as a value, use '-- ---'".into(),
        ]);

        assert_eq!(message.as_deref(), Some("error: unexpected argument '---"));
    }

    #[test]
    fn provider_stderr_ignores_non_error_diagnostics() {
        assert_eq!(
            provider_stderr_error(vec!["warning: optional integration unavailable".into()]),
            None
        );
    }

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
        let DriverEvent::RichActivity(started) = receiver.recv().unwrap() else {
            panic!("expected a rich Cursor tool activity");
        };
        assert_eq!(started.source_id.as_deref(), Some("tool-1"));
        assert_eq!(started.kind, ActivityKind::Search);
        assert_eq!(started.title, "Read");
        assert_eq!(
            started.arguments.as_deref(),
            Some("{\n  \"path\": \"src/main.rs\"\n}")
        );
        assert!(!started.complete);
        let DriverEvent::RichActivity(completed) = receiver.recv().unwrap() else {
            panic!("expected a completed rich Cursor tool activity");
        };
        assert_eq!(completed.source_id.as_deref(), Some("tool-1"));
        assert_eq!(completed.title, "Read");
        assert!(
            completed
                .output
                .as_deref()
                .is_some_and(|output| output.contains("totalLines"))
        );
        assert!(completed.complete);
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
        let DriverEvent::RichActivity(started) = receiver.recv().unwrap() else {
            panic!("expected a rich Amp tool activity");
        };
        assert_eq!(started.source_id.as_deref(), Some("toolu_amp_1"));
        assert_eq!(started.kind, ActivityKind::FileChange);
        assert!(
            started
                .arguments
                .as_deref()
                .is_some_and(|arguments| arguments.contains("path"))
        );
        assert!(!started.complete);
        let DriverEvent::RichActivity(completed) = receiver.recv().unwrap() else {
            panic!("expected a completed rich Amp tool activity");
        };
        assert_eq!(completed.source_id.as_deref(), Some("toolu_amp_1"));
        assert_eq!(completed.output.as_deref(), Some("created"));
        assert!(completed.complete);
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
    fn opencode_stream_uses_and_retains_the_js_title_argument() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        let code = format!(
            "await sky.list_apps(); nodeRepl.write(\"{}\");",
            "full-input-".repeat(30)
        );
        parser.parse_opencode(
            serde_json::json!({
                "type": "tool",
                "part": {
                    "callID": "call-js-1",
                    "tool": "waku_js_repl_js",
                    "state": {
                        "status": "running",
                        "input": {
                            "code": code,
                            "title": "Inspect available Mac apps"
                        }
                    }
                }
            }),
            &events,
        );
        parser.parse_opencode(
            serde_json::json!({
                "type": "tool.updated",
                "part": {
                    "callID": "call-js-1",
                    "tool": "waku_js_repl_js",
                    "state": {
                        "status": "completed",
                        "output": "complete output",
                        "attachments": [{
                            "type": "file",
                            "mime": "image/png",
                            "url": "data:image/png;base64,aGVsbG8="
                        }]
                    }
                }
            }),
            &events,
        );

        let DriverEvent::RichActivity(started) = receiver.recv().unwrap() else {
            panic!("expected a rich OpenCode tool activity");
        };
        assert_eq!(started.source_id.as_deref(), Some("call-js-1"));
        assert_eq!(started.title, "Inspect available Mac apps");
        assert!(
            started
                .arguments
                .as_deref()
                .is_some_and(|arguments| arguments.contains(&"full-input-".repeat(30)))
        );
        assert!(!started.complete);
        let DriverEvent::RichActivity(completed) = receiver.recv().unwrap() else {
            panic!("expected a completed rich OpenCode tool activity");
        };
        assert_eq!(completed.source_id.as_deref(), Some("call-js-1"));
        assert_eq!(completed.title, "Inspect available Mac apps");
        assert_eq!(completed.output.as_deref(), Some("complete output"));
        assert_eq!(
            completed.image_urls,
            ["data:image/png;base64,aGVsbG8=".to_owned()]
        );
        assert!(completed.complete);
        assert!(parser.opencode_tools.is_empty());
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

        let DriverEvent::RichActivity(started) = receiver.recv().unwrap() else {
            panic!("expected a rich Claude tool activity");
        };
        assert_eq!(started.source_id.as_deref(), Some("toolu_123"));
        assert_eq!(started.title, "Bash");
        assert!(
            started
                .arguments
                .as_deref()
                .is_some_and(|arguments| arguments.contains("cargo test"))
        );
        assert!(!started.complete);
        let DriverEvent::RichActivity(completed) = receiver.recv().unwrap() else {
            panic!("expected a completed rich Claude tool activity");
        };
        assert_eq!(completed.source_id.as_deref(), Some("toolu_123"));
        assert_eq!(completed.title, "Bash");
        assert_eq!(completed.output.as_deref(), Some("test result: ok"));
        assert!(completed.complete);
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
        let DriverEvent::RichActivity(started) = receiver.recv().unwrap() else {
            panic!("expected a rich Grok tool activity");
        };
        assert_eq!(started.source_id.as_deref(), Some("tool-1"));
        assert_eq!(started.title, "Read src/main.rs");
        assert!(
            started
                .arguments
                .as_deref()
                .is_some_and(|arguments| arguments.contains("src/main.rs"))
        );
        assert!(!started.complete);
        let DriverEvent::RichActivity(completed) = receiver.recv().unwrap() else {
            panic!("expected a completed rich Grok tool activity");
        };
        assert_eq!(completed.source_id.as_deref(), Some("tool-1"));
        assert_eq!(completed.title, "Read src/main.rs");
        assert!(
            completed
                .output
                .as_deref()
                .is_some_and(|output| output.contains("12"))
        );
        assert!(completed.complete);
        assert!(matches!(
            receiver.recv().unwrap(),
            DriverEvent::TextDelta(text) if text == "After"
        ));
    }

    #[test]
    fn grok_mcp_use_tool_prefers_the_nested_js_title() {
        let (events, receiver) = unbounded();
        let mut parser = StreamParser::default();
        parser.parse_grok(
            serde_json::json!({
                "type": "tool_call",
                "toolCallId": "tool-1",
                "title": "use_tool",
                "kind": "use_tool",
                "toolName": "use_tool",
                "status": "pending",
                "rawInput": {
                    "tool_name": "waku_js_repl__js",
                    "tool_input": {
                        "code": "nodeRepl.write(\"ok\");",
                        "title": "Verify Grok bridge"
                    }
                }
            }),
            &events,
        );

        let DriverEvent::RichActivity(item) = receiver.recv().unwrap() else {
            panic!("expected a rich Grok MCP activity");
        };
        assert_eq!(item.title, "Verify Grok bridge");
        assert!(
            item.arguments
                .as_deref()
                .is_some_and(|arguments| arguments.contains("nodeRepl.write"))
        );
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

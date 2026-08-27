//! Provider-owned slash-command discovery.
//!
//! The composer must not guess a CLI's built-ins. Providers with a safe,
//! sessionless catalog surface are probed here; providers whose command list
//! is session-scoped publish it through their live driver instead. Every probe
//! runs in the daemon's background workspace operation, never on a render path.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use waku_protocol::composer::{CommandScope, SlashCommand};
use waku_protocol::model::ProviderKind;

const CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
// Claude initializes account-backed configurations such as Bedrock before it
// returns its local command registry. Match the upstream SDK probe's budget.
const CLAUDE_PROBE_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
const COMMAND_CATALOG_CAP: usize = 500;

/// Discover the command surface the installed CLI can expose without creating
/// a provider session. `None` means either that the provider only reports
/// commands from a real session or that its probe failed; filesystem-defined
/// commands remain available as the caller's fallback in both cases.
pub(crate) fn discover(
    provider: ProviderKind,
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    match provider {
        ProviderKind::Amp => discover_amp(binary, project_root, environment),
        ProviderKind::Claude => discover_claude(binary, project_root, environment),
        ProviderKind::Codex => discover_codex(binary, project_root, environment),
        ProviderKind::OpenCode => discover_opencode(binary, project_root, environment),
        ProviderKind::OhMyPi => discover_oh_my_pi(binary, project_root, environment),
        ProviderKind::Pi => discover_pi(binary, project_root, environment),
        // ACP advertises commands only after session/new. Harness likewise
        // requires an agent id for commands/list. Creating throwaway sessions
        // merely to seed autocomplete would pollute provider history, so their
        // live DriverEvent::AvailableCommands update is the catalog surface.
        ProviderKind::Cursor
        | ProviderKind::DeepSeek
        | ProviderKind::Fx
        | ProviderKind::Grok
        | ProviderKind::Kimi => None,
    }
}

fn discover_amp(
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    let value = capture_json(
        binary,
        &["skill", "list", "--json"],
        project_root,
        environment,
    )?;
    Some(parse_amp_skills(&value))
}

fn discover_claude(
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    // This is the initialization request used by the Claude Agent SDK. No user
    // message follows it, so the CLI loads local commands without making a
    // model request or persisting a conversation.
    let request = json!({
        "type": "control_request",
        "request_id": "waku-command-catalog",
        "request": {"subtype": "initialize"}
    });
    let input = format!("{request}\n");
    let value = probe_json_lines(
        binary,
        &[
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--no-session-persistence",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--strict-mcp-config",
            "--setting-sources=user,project,local",
        ],
        project_root,
        input.as_bytes(),
        CLAUDE_PROBE_TIMEOUT,
        |value| {
            value.get("type").and_then(Value::as_str) == Some("control_response")
                && value
                    .pointer("/response/request_id")
                    .and_then(Value::as_str)
                    == Some("waku-command-catalog")
        },
        environment,
        &[("ENABLE_CLAUDEAI_MCP_SERVERS", "false")],
    )?;
    Some(parse_claude_commands(&value))
}

fn discover_codex(
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    let mut command = crate::command_env::command_with_environment(binary, environment);
    command
        .args(["app-server", "--stdio"])
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate_process_group(&mut command);
    let mut child = crate::command_env::spawn(&mut command).ok()?;
    let Some(mut stdin) = child.stdin.take() else {
        terminate_child(&mut child);
        return None;
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return None;
    };
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || read_json_lines(stdout, tx));

    let initialize = json!({
        "method": "initialize",
        "id": 0,
        "params": {
            "clientInfo": {
                "name": "waku",
                "title": "Waku",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {"experimentalApi": true}
        }
    });
    let commands = if write_json_line(&mut stdin, &initialize).is_ok()
        && recv_rpc_response(&rx, 0, CLI_PROBE_TIMEOUT).is_some()
        && write_json_line(&mut stdin, &json!({"method": "initialized", "params": {}})).is_ok()
    {
        let mut commands = Vec::new();
        // Codex does not publish a TUI command registry, but it does publish
        // the effective feature set backing the native commands Waku bridges.
        // Availability therefore comes from the installed CLI/config rather
        // than from an unconditional Codex list in the composer.
        let feature_request = json!({
            "method": "experimentalFeature/list",
            "id": 1,
            "params": {"cursor": null, "limit": COMMAND_CATALOG_CAP, "threadId": null}
        });
        if write_json_line(&mut stdin, &feature_request).is_ok()
            && let Some(value) = recv_rpc_response(&rx, 1, CLI_PROBE_TIMEOUT)
        {
            commands.extend(parse_codex_command_features(&value));
        }

        let cwd = project_root.to_string_lossy();
        let request = json!({
            "method": "skills/list",
            "id": 2,
            "params": {"cwds": [cwd.as_ref()]}
        });
        if write_json_line(&mut stdin, &request).is_ok() {
            if let Some(value) = recv_rpc_response(&rx, 2, CLI_PROBE_TIMEOUT) {
                commands.extend(parse_codex_skills(&value, project_root));
            }
        }
        Some(commands)
    } else {
        None
    };
    terminate_child(&mut child);
    drop(stdin);
    let _ = reader.join();
    commands
}

fn discover_opencode(
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    // `debug config` is the effective, plugin-mutated configuration. Unlike a
    // directory walk, it sees plural command roots, inline config, and commands
    // registered by plugins exactly as the installed CLI resolves them.
    let value = capture_json(binary, &["debug", "config"], project_root, environment)?;
    Some(parse_opencode_commands(&value))
}

fn discover_pi(
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    // Catalog reads neither run an agent turn nor need the runtime driver's
    // full-access flag.
    let request = json!({"id": "waku-command-catalog", "type": "get_commands"});
    let input = format!("{request}\n");
    let value = probe_json_lines(
        binary,
        &["--mode", "rpc", "--no-session", "--offline"],
        project_root,
        input.as_bytes(),
        CLI_PROBE_TIMEOUT,
        |value| {
            value.get("id").and_then(Value::as_str) == Some("waku-command-catalog")
                && value.get("success").and_then(Value::as_bool) == Some(true)
        },
        environment,
        &[("PI_SKIP_VERSION_CHECK", "1")],
    )?;
    Some(parse_pi_commands(&value))
}

fn discover_oh_my_pi(
    binary: &Path,
    project_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Vec<SlashCommand>> {
    // OMP publishes its full registry at RPC startup; no request or provider
    // session is needed. A blank frame lets stdin close cleanly on older builds.
    let value = probe_json_lines(
        binary,
        &["--mode", "rpc", "--no-session"],
        project_root,
        b"\n",
        CLI_PROBE_TIMEOUT,
        |value| value.get("type").and_then(Value::as_str) == Some("available_commands_update"),
        environment,
        &[],
    )?;
    Some(parse_oh_my_pi_commands(&value))
}

fn parse_amp_skills(value: &Value) -> Vec<SlashCommand> {
    value
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|skill| {
            command(
                skill.get("name")?.as_str()?,
                skill.get("description").and_then(Value::as_str),
                CommandScope::Skill,
                None,
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn parse_claude_commands(value: &Value) -> Vec<SlashCommand> {
    value
        .pointer("/response/response/commands")
        .or_else(|| value.get("commands"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            command(
                entry.get("name")?.as_str()?,
                entry.get("description").and_then(Value::as_str),
                CommandScope::Builtin,
                entry.get("argumentHint").and_then(Value::as_str),
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn parse_codex_command_features(value: &Value) -> Vec<SlashCommand> {
    value
        .pointer("/result/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|feature| feature.get("enabled").and_then(Value::as_bool) == Some(true))
        .filter_map(
            |feature| match feature.get("name").and_then(Value::as_str)? {
                "fast_mode" => Some(SlashCommand {
                    name: "fast".to_owned(),
                    description: crate::i18n::translate("commands.fast_description"),
                    scope: CommandScope::Builtin,
                    argument_hint: None,
                    template: None,
                }),
                "goals" => Some(SlashCommand {
                    name: "goal".to_owned(),
                    description: crate::i18n::translate("commands.goal_description"),
                    scope: CommandScope::Builtin,
                    argument_hint: Some("[<objective>|clear|edit|pause|resume]".to_owned()),
                    template: None,
                }),
                _ => None,
            },
        )
        .collect()
}

fn parse_codex_skills(value: &Value, project_root: &Path) -> Vec<SlashCommand> {
    let data = value.pointer("/result/data").and_then(Value::as_array);
    let cwd = project_root.to_string_lossy();
    let entries = data.into_iter().flatten();
    let matching = entries
        .clone()
        .find(|entry| entry.get("cwd").and_then(Value::as_str) == Some(cwd.as_ref()));
    let skills = if let Some(entry) = matching {
        entry
            .get("skills")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
    } else {
        entries
            .flat_map(|entry| {
                entry
                    .get("skills")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .collect()
    };
    skills
        .into_iter()
        .filter(|skill| skill.get("enabled").and_then(Value::as_bool) != Some(false))
        .filter_map(|skill| {
            let description = skill
                .get("shortDescription")
                .or_else(|| skill.pointer("/interface/shortDescription"))
                .or_else(|| skill.get("description"))
                .and_then(Value::as_str);
            command(
                skill.get("name")?.as_str()?,
                description,
                CommandScope::Skill,
                None,
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn parse_opencode_commands(value: &Value) -> Vec<SlashCommand> {
    value
        .get("command")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, entry)| {
            command(
                name,
                entry.get("description").and_then(Value::as_str),
                CommandScope::Project,
                None,
                entry.get("template").and_then(Value::as_str),
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn parse_pi_commands(value: &Value) -> Vec<SlashCommand> {
    value
        .pointer("/data/commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let source = entry.get("source").and_then(Value::as_str);
            let raw_name = entry.get("name")?.as_str()?;
            let name = source
                .filter(|source| *source == "skill")
                .and_then(|_| raw_name.strip_prefix("skill:"))
                .unwrap_or(raw_name);
            let scope = match source {
                Some("skill") => CommandScope::Skill,
                Some("prompt") => {
                    match entry.pointer("/sourceInfo/scope").and_then(Value::as_str) {
                        Some("project") => CommandScope::Project,
                        _ => CommandScope::User,
                    }
                }
                _ => CommandScope::Builtin,
            };
            command(
                name,
                entry.get("description").and_then(Value::as_str),
                scope,
                entry.pointer("/input/hint").and_then(Value::as_str),
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn parse_oh_my_pi_commands(value: &Value) -> Vec<SlashCommand> {
    value
        .get("commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let source = entry.get("source").and_then(Value::as_str);
            let raw_name = entry.get("name")?.as_str()?;
            let name = source
                .filter(|source| *source == "skill")
                .and_then(|_| raw_name.strip_prefix("skill:"))
                .unwrap_or(raw_name);
            command(
                name,
                entry.get("description").and_then(Value::as_str),
                if source == Some("skill") {
                    CommandScope::Skill
                } else {
                    CommandScope::Builtin
                },
                entry.pointer("/input/hint").and_then(Value::as_str),
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn command(
    raw_name: &str,
    description: Option<&str>,
    scope: CommandScope,
    argument_hint: Option<&str>,
    template: Option<&str>,
) -> Option<SlashCommand> {
    let name = raw_name.trim().trim_start_matches('/');
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return None;
    }
    Some(SlashCommand {
        name: name.to_owned(),
        description: description.unwrap_or_default().trim().to_owned(),
        scope,
        argument_hint: argument_hint
            .map(str::trim)
            .filter(|hint| !hint.is_empty())
            .map(str::to_owned),
        template: template
            .map(str::trim)
            .filter(|template| !template.is_empty())
            .map(str::to_owned),
    })
}

fn capture_json(
    binary: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<Value> {
    let mut command = crate::command_env::command_with_environment(binary, environment);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate_process_group(&mut command);
    let mut child = crate::command_env::spawn(&mut command).ok()?;
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return None;
    };
    let reader = thread::spawn(move || read_bounded(stdout, MAX_CAPTURE_BYTES));
    let success = wait_for_child(&mut child, CLI_PROBE_TIMEOUT);
    let output = reader.join().ok().flatten()?;
    success
        .then(|| serde_json::from_slice(&output).ok())
        .flatten()
}

#[allow(clippy::too_many_arguments)]
fn probe_json_lines(
    binary: &Path,
    args: &[&str],
    cwd: &Path,
    input: &[u8],
    timeout: Duration,
    matches: impl Fn(&Value) -> bool,
    environment: &BTreeMap<String, String>,
    env: &[(&str, &str)],
) -> Option<Value> {
    let mut command = crate::command_env::command_with_environment(binary, environment);
    command
        .args(args)
        .current_dir(cwd)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate_process_group(&mut command);
    let mut child = crate::command_env::spawn(&mut command).ok()?;
    let Some(mut stdin) = child.stdin.take() else {
        terminate_child(&mut child);
        return None;
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return None;
    };
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || read_json_lines(stdout, tx));
    let result = if stdin.write_all(input).is_ok() && stdin.flush().is_ok() {
        drop(stdin);
        recv_matching(&rx, timeout, matches)
    } else {
        None
    };
    terminate_child(&mut child);
    let _ = reader.join();
    result
}

fn read_json_lines(reader: impl Read, tx: mpsc::Sender<Value>) {
    for line in BufReader::new(reader).lines().map_while(Result::ok) {
        if line.len() > MAX_CAPTURE_BYTES {
            continue;
        }
        if let Ok(value) = serde_json::from_str(&line)
            && tx.send(value).is_err()
        {
            break;
        }
    }
}

fn recv_matching(
    rx: &Receiver<Value>,
    timeout: Duration,
    matches: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let value = rx.recv_timeout(remaining).ok()?;
        if matches(&value) {
            return Some(value);
        }
    }
}

fn recv_rpc_response(rx: &Receiver<Value>, id: u64, timeout: Duration) -> Option<Value> {
    recv_matching(rx, timeout, |value| {
        value.get("id").and_then(Value::as_u64) == Some(id)
    })
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut exceeded = false;
    loop {
        let read = reader.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    (!exceeded).then_some(output)
}

fn wait_for_child(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                terminate_child(child);
                return false;
            }
        }
    }
}

fn isolate_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_initialization_metadata() {
        let commands = parse_claude_commands(&json!({
            "type": "control_response",
            "response": {
                "request_id": "waku-command-catalog",
                "response": {"commands": [
                    {"name": "compact", "description": "Compact context", "argumentHint": "[focus]"},
                    {"name": "  ", "description": "ignored"}
                ]}
            }
        }));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "compact");
        assert_eq!(commands[0].description, "Compact context");
        assert_eq!(commands[0].argument_hint.as_deref(), Some("[focus]"));
        assert_eq!(commands[0].scope, CommandScope::Builtin);
    }

    #[test]
    fn parses_codex_enabled_skills_for_the_requested_cwd() {
        let root = Path::new("/repo");
        let commands = parse_codex_skills(
            &json!({"result": {"data": [
                {"cwd": "/other", "skills": [{"name": "other", "enabled": true, "description": "Other"}]},
                {"cwd": "/repo", "skills": [
                    {"name": "review", "enabled": true, "description": "Long", "shortDescription": "Review changes"},
                    {"name": "off", "enabled": false, "description": "Disabled"}
                ]}
            ]}}),
            root,
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "review");
        assert_eq!(commands[0].description, "Review changes");
        assert_eq!(commands[0].scope, CommandScope::Skill);
    }

    #[test]
    fn derives_codex_native_commands_from_enabled_cli_features() {
        let commands = parse_codex_command_features(&json!({"result": {"data": [
            {"name": "fast_mode", "enabled": true},
            {"name": "goals", "enabled": true},
            {"name": "unrelated", "enabled": true}
        ]}}));
        let fast = commands
            .iter()
            .find(|command| command.name == "fast")
            .expect("enabled fast_mode must expose /fast");
        assert_eq!(fast.scope, CommandScope::Builtin);
        assert!(fast.template.is_none());
        let goal = commands
            .iter()
            .find(|command| command.name == "goal")
            .expect("enabled goals must expose /goal");
        assert_eq!(goal.scope, CommandScope::Builtin);
        assert!(goal.template.is_none());
        assert!(goal.argument_hint.is_some());

        let disabled = parse_codex_command_features(&json!({"result": {"data": [
            {"name": "fast_mode", "enabled": false},
            {"name": "goals", "enabled": false}
        ]}}));
        assert!(disabled.is_empty());
    }

    #[test]
    fn parses_pi_and_oh_my_pi_native_shapes() {
        let pi = parse_pi_commands(&json!({"data": {"commands": [
            {"name": "deploy", "description": "Deploy", "source": "prompt", "sourceInfo": {"scope": "project"}},
            {"name": "skill:verify", "description": "Verify", "source": "skill"},
            {"name": "websearch", "description": "Search", "source": "extension"}
        ]}}));
        assert_eq!(pi[0].scope, CommandScope::Project);
        assert_eq!(pi[1].name, "verify");
        assert_eq!(pi[1].scope, CommandScope::Skill);
        assert_eq!(pi[2].scope, CommandScope::Builtin);

        let omp = parse_oh_my_pi_commands(&json!({
            "type": "available_commands_update",
            "commands": [
                {"name": "compact", "description": "Compact", "source": "builtin", "input": {"hint": "[focus]"}},
                {"name": "skill:verify", "description": "Verify", "source": "skill"}
            ]
        }));
        assert_eq!(omp[0].argument_hint.as_deref(), Some("[focus]"));
        assert_eq!(omp[1].name, "verify");
        assert_eq!(omp[1].scope, CommandScope::Skill);
    }

    #[test]
    fn parses_effective_opencode_commands_and_amp_skills() {
        let opencode = parse_opencode_commands(&json!({"command": {
            "review": {"description": "Review changes", "template": "Review $ARGUMENTS"}
        }}));
        assert_eq!(opencode.len(), 1);
        assert_eq!(opencode[0].template.as_deref(), Some("Review $ARGUMENTS"));

        let amp = parse_amp_skills(&json!({"skills": [
            {"name": "building-skills", "description": "Build skills", "source": "builtin"}
        ]}));
        assert_eq!(amp.len(), 1);
        assert_eq!(amp[0].scope, CommandScope::Skill);
    }
}

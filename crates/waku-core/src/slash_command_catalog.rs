//! Provider-owned slash-command discovery.
//!
//! The composer must not guess a CLI's built-ins. Providers with a safe,
//! sessionless catalog surface are probed here; providers whose command list
//! is session-scoped publish it through their live driver instead. Every probe
//! runs in the daemon's background workspace operation, never on a render path.

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
) -> Option<Vec<SlashCommand>> {
    match provider {
        ProviderKind::Amp => discover_amp(binary, project_root),
        ProviderKind::Claude => discover_claude(binary, project_root),
        ProviderKind::Codex => discover_codex(binary, project_root),
        ProviderKind::OpenCode => discover_opencode(binary, project_root),
        ProviderKind::OpenCode2 => discover_opencode2(binary, project_root),
        ProviderKind::OhMyPi => discover_oh_my_pi(binary, project_root),
        ProviderKind::Pi => discover_pi(binary, project_root),
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

fn discover_amp(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
    let value = capture_json(binary, &["skill", "list", "--json"], project_root)?;
    Some(parse_amp_skills(&value))
}

fn discover_claude(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
    // This is the initialization request used by the Claude Agent SDK. No user
    // message follows it, so the CLI loads local commands without making a
    // model request or persisting a conversation.
    let value =
        crate::claude_metadata::initialize(binary, Some(project_root), "user,project,local")?;
    Some(parse_claude_commands(&value))
}

fn discover_codex(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
    let mut command = crate::command_env::command(binary);
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

fn discover_opencode(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
    // The command registry includes built-ins and MCP prompts that never
    // appear in `debug config`. Reading it needs a workspace server, but does
    // not create a session or run a turn. Reuse the resident server when one
    // already exists instead of starting a competing plugin/MCP stack.
    if let Ok(server) = crate::opencode_pool::acquire(binary, project_root)
        && let Ok(value) = server.request_with_timeout("GET", "/command", None, CLI_PROBE_TIMEOUT)
        && value.is_array()
    {
        return Some(parse_opencode_commands(&value));
    }
    // Older CLI builds can still report configured commands without a server.
    let value = capture_json(binary, &["debug", "config"], project_root)?;
    Some(parse_opencode_config_commands(&value))
}

fn discover_opencode2(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
    // These catalogues are directory-scoped, not session-scoped. Seed the
    // new-task composer too, without creating a disposable provider session.
    let directory = std::fs::canonicalize(project_root).ok()?;
    let directory = directory.to_string_lossy();
    let service = crate::opencode2_service::shared(binary).ok()?;
    let endpoint = service.endpoint();
    let commands = crate::opencode2_api::list_commands(&endpoint, Some(&directory)).ok()?;
    let skills = crate::opencode2_api::list_skills(&endpoint, Some(&directory)).unwrap_or_default();
    Some(opencode2_commands(commands, skills))
}

fn discover_pi(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
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
        &[("PI_SKIP_VERSION_CHECK", "1")],
    )?;
    Some(parse_pi_commands(&value))
}

fn discover_oh_my_pi(binary: &Path, project_root: &Path) -> Option<Vec<SlashCommand>> {
    // OMP publishes its full registry at RPC startup; no request or provider
    // session is needed. A blank frame lets stdin close cleanly on older builds.
    let value = probe_json_lines(
        binary,
        &["--mode", "rpc", "--no-session"],
        project_root,
        b"\n",
        CLI_PROBE_TIMEOUT,
        |value| value.get("type").and_then(Value::as_str) == Some("available_commands_update"),
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

pub(crate) fn parse_opencode_commands(value: &Value) -> Vec<SlashCommand> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let hints = entry.get("hints").and_then(Value::as_array).map(|hints| {
                hints
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            command(
                entry.get("name")?.as_str()?,
                entry.get("description").and_then(Value::as_str),
                if entry.get("source").and_then(Value::as_str) == Some("skill") {
                    CommandScope::Skill
                } else {
                    CommandScope::Builtin
                },
                hints.as_deref(),
                // The server owns template expansion, agent/model overrides,
                // subtask dispatch, file references, and shell interpolation.
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn parse_opencode_config_commands(value: &Value) -> Vec<SlashCommand> {
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
                None,
            )
        })
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

fn opencode2_commands(
    commands: Vec<crate::opencode2_api::CommandInfo>,
    skills: Vec<crate::opencode2_api::SkillInfo>,
) -> Vec<SlashCommand> {
    commands
        .into_iter()
        .filter_map(|entry| {
            command(
                &entry.name,
                entry.description.as_deref(),
                CommandScope::Builtin,
                None,
                None,
            )
        })
        .chain(
            skills
                .into_iter()
                .filter(|skill| skill.slash == Some(true))
                .filter_map(|entry| {
                    command(
                        &entry.name,
                        entry.description.as_deref(),
                        CommandScope::Skill,
                        None,
                        None,
                    )
                }),
        )
        .take(COMMAND_CATALOG_CAP)
        .collect()
}

pub(crate) fn parse_pi_commands(value: &Value) -> Vec<SlashCommand> {
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

pub(crate) fn parse_oh_my_pi_commands(value: &Value) -> Vec<SlashCommand> {
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

fn capture_json(binary: &Path, args: &[&str], cwd: &Path) -> Option<Value> {
    let mut command = crate::command_env::command(binary);
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
    env: &[(&str, &str)],
) -> Option<Value> {
    let mut command = crate::command_env::command(binary);
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
    fn parses_opencode_registry_builtins_and_skills_without_expanding_templates() {
        let commands = parse_opencode_commands(&json!([
            {"name": "init", "source": "command", "description": "guided AGENTS.md setup", "template": "Native init", "hints": []},
            {"name": "review", "source": "command", "subtask": true, "template": "Review $ARGUMENTS", "hints": ["$ARGUMENTS"]},
            {"name": "deploy", "source": "skill", "description": "Deploy", "hints": []},
            {"name": "mcp:lookup", "source": "mcp", "template": {}, "hints": ["$1", "$2"]},
            {"name": "invalid command"}
        ]));
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["init", "review", "deploy", "mcp:lookup"]
        );
        assert_eq!(commands[0].scope, CommandScope::Builtin);
        assert_eq!(commands[1].argument_hint.as_deref(), Some("$ARGUMENTS"));
        assert_eq!(commands[2].scope, CommandScope::Skill);
        assert_eq!(commands[3].argument_hint.as_deref(), Some("$1 $2"));
        assert!(commands.iter().all(|command| command.template.is_none()));
    }

    #[test]
    fn opencode2_catalog_keeps_only_user_invocable_skills() {
        let commands = opencode2_commands(
            vec![crate::opencode2_api::CommandInfo {
                name: "review".into(),
                description: Some("Review changes".into()),
            }],
            serde_json::from_value(json!([
                {"id": "hidden", "name": "hidden", "slash": false, "location": "builtin"},
                {"id": "deploy", "name": "deploy", "slash": true, "location": "skills/deploy.md"}
            ]))
            .unwrap(),
        );
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["review", "deploy"]
        );
        assert_eq!(commands[0].scope, CommandScope::Builtin);
        assert_eq!(commands[1].scope, CommandScope::Skill);
        assert!(commands.iter().all(|command| command.template.is_none()));
    }

    #[test]
    fn parses_effective_opencode_config_and_amp_skills() {
        let opencode = parse_opencode_config_commands(&json!({"command": {
            "review": {"description": "Review changes", "template": "Review $ARGUMENTS"}
        }}));
        assert_eq!(opencode.len(), 1);
        assert!(opencode[0].template.is_none());

        let amp = parse_amp_skills(&json!({"skills": [
            {"name": "building-skills", "description": "Build skills", "source": "builtin"}
        ]}));
        assert_eq!(amp.len(), 1);
        assert_eq!(amp[0].scope, CommandScope::Skill);
    }

    #[test]
    #[ignore = "requires an installed opencode; reads its catalogue without creating sessions"]
    fn opencode_builtin_catalog_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let root =
            std::env::temp_dir().join(format!("waku-command-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let server = crate::opencode_pool::acquire(&binary, &root).unwrap();
        let before = server.request("GET", "/session", None).unwrap();
        let commands = discover(ProviderKind::OpenCode, &binary, &root).unwrap();
        for name in ["init", "review"] {
            let command = commands
                .iter()
                .find(|command| command.name == name)
                .expect("missing OpenCode built-in");
            assert_eq!(command.scope, CommandScope::Builtin);
            assert!(command.template.is_none());
        }
        assert_eq!(before, server.request("GET", "/session", None).unwrap());
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires an installed opencode2; reads a cold workspace catalogue without creating sessions"]
    fn opencode2_builtin_catalog_against_a_real_service() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode2 is not installed");
        let root =
            std::env::temp_dir().join(format!("waku-command-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".opencode/commands")).unwrap();
        std::fs::write(
            root.join(".opencode/commands/waku-catalog-smoke.md"),
            "---\ndescription: Catalog test\n---\nReply OK",
        )
        .unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let commands = discover(ProviderKind::OpenCode2, &binary, &root).unwrap();
        for name in ["init", "review", "waku-catalog-smoke"] {
            assert!(
                commands
                    .iter()
                    .any(|command| command.name == name && command.template.is_none()),
                "missing {name}"
            );
        }
        let service = crate::opencode2_service::shared(&binary).unwrap();
        let (sessions, _) = crate::opencode2_api::list_sessions(
            &service.endpoint(),
            Some(&root.to_string_lossy()),
            None,
            crate::opencode2_api::Order::Desc,
            10,
            None,
        )
        .unwrap();
        assert!(
            sessions.is_empty(),
            "catalogue discovery must not create sessions"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

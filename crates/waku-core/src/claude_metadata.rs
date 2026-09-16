//! Sessionless Claude Code initialization metadata.
//!
//! Claude's stream-json transport exposes the same account- and
//! configuration-aware command and model catalogs that back its interactive
//! pickers. The probe sends only the SDK initialization control request, so it
//! neither runs a model turn nor persists a conversation.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROBE_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_ID: &str = "waku-initialize-catalog";

/// Return Claude Code's initialization response for the requested settings
/// scope. A project-aware command probe supplies its cwd and all setting
/// sources; the global model probe intentionally reads only user settings.
pub(crate) fn initialize(
    binary: &Path,
    cwd: Option<&Path>,
    setting_sources: &str,
) -> Option<Value> {
    let request = json!({
        "type": "control_request",
        "request_id": REQUEST_ID,
        "request": {"subtype": "initialize"}
    });
    let mut input = serde_json::to_vec(&request).ok()?;
    input.push(b'\n');

    let mut command = crate::command_env::command(binary);
    command
        .args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--no-session-persistence",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--strict-mcp-config",
            "--setting-sources",
            setting_sources,
        ])
        .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
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
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.len() > MAX_LINE_BYTES {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && tx.send(value).is_err()
            {
                break;
            }
        }
    });

    let response = if stdin.write_all(&input).is_ok() && stdin.flush().is_ok() {
        drop(stdin);
        let deadline = Instant::now() + PROBE_TIMEOUT;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break None;
            };
            let Ok(value) = rx.recv_timeout(remaining) else {
                break None;
            };
            if value.get("type").and_then(Value::as_str) == Some("control_response")
                && value
                    .pointer("/response/request_id")
                    .and_then(Value::as_str)
                    == Some(REQUEST_ID)
            {
                break Some(value);
            }
        }
    } else {
        None
    };
    terminate_child(&mut child);
    let _ = reader.join();
    response
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

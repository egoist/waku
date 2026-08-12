use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::model::ProviderResumeCursor;

const RPC_TIMEOUT: Duration = Duration::from_secs(30);

pub fn generated_title(session_id: &str) -> anyhow::Result<Option<String>> {
    generated_title_in(&grok_home_directory()?, session_id)
}

pub fn generated_title_in(grok_home: &Path, session_id: &str) -> anyhow::Result<Option<String>> {
    Uuid::parse_str(session_id).context("Grok returned an invalid session ID")?;
    let summary_path = find_session_directory_in(grok_home, session_id)?.join("summary.json");
    let summary: Value = serde_json::from_slice(
        &fs::read(&summary_path)
            .with_context(|| format!("could not read {}", summary_path.display()))?,
    )
    .context("Grok's session summary is invalid JSON")?;
    Ok(summary
        .get("generated_title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned))
}

pub fn fork_session_at_turn(
    binary: &Path,
    cwd: &Path,
    source_session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    Uuid::parse_str(source_session_id).context("Grok returned an invalid source session ID")?;
    let source_dir = find_session_directory(source_session_id)?;
    let fork_id = fork_native_session(binary, cwd, source_session_id)?;
    let fork_dir = source_dir
        .parent()
        .ok_or_else(|| anyhow!("Grok's source session directory has no parent"))?
        .join(&fork_id);
    let result = truncate_fork(&fork_dir, retained_turns);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&fork_dir);
        return Err(error);
    }
    Ok(ProviderResumeCursor::Grok {
        session_id: fork_id,
    })
}

fn fork_native_session(
    binary: &Path,
    cwd: &Path,
    source_session_id: &str,
) -> anyhow::Result<String> {
    let mut client = GrokRpc::start(binary)?;
    client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": {"readTextFile": true, "writeTextFile": true},
                "terminal": true
            }
        }),
    )?;
    let response = client.request(
        2,
        "_x.ai/session/fork",
        json!({
            "sourceSessionId": source_session_id,
            "sourceCwd": cwd,
            "newCwd": cwd
        }),
    )?;
    let session_id = response
        .pointer("/result/newSessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("Grok returned no forked session ID"))?;
    Uuid::parse_str(session_id).context("Grok returned an invalid forked session ID")?;
    Ok(session_id.to_owned())
}

fn find_session_directory(session_id: &str) -> anyhow::Result<PathBuf> {
    find_session_directory_in(&grok_home_directory()?, session_id)
}

fn grok_home_directory() -> anyhow::Result<PathBuf> {
    std::env::var_os("GROK_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".grok")))
        .ok_or_else(|| anyhow!("Grok's home directory could not be located"))
}

fn find_session_directory_in(grok_home: &Path, session_id: &str) -> anyhow::Result<PathBuf> {
    let sessions = grok_home.join("sessions");
    for entry in fs::read_dir(&sessions)
        .with_context(|| format!("could not read Grok sessions at {}", sessions.display()))?
    {
        let Ok(entry) = entry else {
            continue;
        };
        let candidate = entry.path().join(session_id);
        if candidate.join("summary.json").is_file() {
            return Ok(candidate);
        }
    }
    bail!("Grok session {session_id} was not found on disk")
}

fn truncate_fork(session_dir: &Path, retained_turns: usize) -> anyhow::Result<()> {
    let chat_path = session_dir.join("chat_history.jsonl");
    let updates_path = session_dir.join("updates.jsonl");
    let chat = read_json_lines(&chat_path)?;
    let updates = read_json_lines(&updates_path)?;
    let chat = truncate_at_turn(&chat, retained_turns, is_chat_prompt)?;
    let updates = truncate_at_turn(&updates, retained_turns, is_update_prompt)?;
    write_json_lines(&chat_path, &chat)?;
    write_json_lines(&updates_path, &updates)?;

    let summary_path = session_dir.join("summary.json");
    let mut summary: Value = serde_json::from_slice(
        &fs::read(&summary_path)
            .with_context(|| format!("could not read {}", summary_path.display()))?,
    )
    .context("Grok's fork summary is invalid JSON")?;
    summary["num_messages"] = json!(updates.len());
    summary["num_chat_messages"] = json!(chat.len());
    summary["updated_at"] = json!(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true));
    write_json(&summary_path, &summary)
}

fn truncate_at_turn(
    values: &[Value],
    retained_turns: usize,
    is_prompt: fn(&Value) -> bool,
) -> anyhow::Result<Vec<Value>> {
    let mut turns = 0;
    let cutoff = values.iter().position(|value| {
        if !is_prompt(value) {
            return false;
        }
        if turns == retained_turns {
            true
        } else {
            turns += 1;
            false
        }
    });
    if cutoff.is_none() {
        turns = values.iter().filter(|value| is_prompt(value)).count();
        if turns < retained_turns {
            bail!("Grok has only {turns} native turns, but Waku needs {retained_turns}");
        }
    }
    Ok(values[..cutoff.unwrap_or(values.len())].to_vec())
}

fn is_chat_prompt(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("user")
        && value.get("prompt_index").and_then(Value::as_u64).is_some()
}

fn is_update_prompt(value: &Value) -> bool {
    value
        .pointer("/params/update/sessionUpdate")
        .and_then(Value::as_str)
        == Some("user_message_chunk")
}

fn read_json_lines(path: &Path) -> anyhow::Result<Vec<Value>> {
    let file = fs::File::open(path)
        .with_context(|| format!("could not open Grok history at {}", path.display()))?;
    BufReader::new(file)
        .lines()
        .map(|line| {
            let line = line?;
            serde_json::from_str(&line).context("Grok's history contains invalid JSON")
        })
        .collect()
}

fn write_json_lines(path: &Path, values: &[Value]) -> anyhow::Result<()> {
    write_atomic(path, |writer| {
        for value in values {
            serde_json::to_writer(&mut *writer, value)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    })
}

fn write_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    write_atomic(path, |writer| {
        serde_json::to_writer_pretty(&mut *writer, value)?;
        writer.write_all(b"\n")?;
        Ok(())
    })
}

fn write_atomic(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Grok history path has no parent"))?;
    let temp = parent.join(format!(".waku-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(&temp)
        .with_context(|| format!("could not create {}", temp.display()))?;
    let mut writer = BufWriter::new(file);
    if let Err(error) = write(&mut writer).and_then(|()| {
        writer.flush()?;
        writer.get_ref().sync_all()?;
        Ok(())
    }) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    fs::rename(&temp, path)
        .with_context(|| format!("could not replace Grok history at {}", path.display()))
}

struct GrokRpc {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<Value>,
}

impl GrokRpc {
    fn start(binary: &Path) -> anyhow::Result<Self> {
        let mut command = crate::command_env::command(binary);
        let command = command
            .args(["agent", "--always-approve", "--no-leader", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child =
            crate::command_env::spawn(command).context("failed to start Grok's ACP server")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Grok ACP stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Grok ACP stdout is unavailable"))?;
        let (tx, responses) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str(&line) {
                    let _ = tx.send(value);
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            responses,
        })
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> anyhow::Result<Value> {
        serde_json::to_writer(
            &mut self.stdin,
            &json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        )?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        loop {
            let response = self
                .responses
                .recv_timeout(RPC_TIMEOUT)
                .with_context(|| format!("timed out waiting for Grok {method}"))?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!("Grok {method} failed: {error}");
            }
            return Ok(response);
        }
    }
}

impl Drop for GrokRpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn truncates_chat_history_before_the_first_excluded_prompt() {
        let history = vec![
            json!({"type":"system"}),
            json!({"type":"user","prompt_index":0}),
            json!({"type":"assistant"}),
            json!({"type":"user","synthetic_reason":"reminder"}),
            json!({"type":"user","prompt_index":1}),
            json!({"type":"assistant"}),
        ];
        let truncated = truncate_at_turn(&history, 1, is_chat_prompt).unwrap();
        assert_eq!(truncated.len(), 4);
        assert_eq!(truncated.last().unwrap()["synthetic_reason"], "reminder");
    }

    #[test]
    fn truncates_native_updates_at_the_matching_user_chunk() {
        let update = |kind| json!({"params":{"update":{"sessionUpdate":kind}}});
        let history = vec![
            update("hook_execution"),
            update("user_message_chunk"),
            update("agent_message_chunk"),
            update("turn_completed"),
            update("user_message_chunk"),
            update("agent_message_chunk"),
        ];
        let truncated = truncate_at_turn(&history, 1, is_update_prompt).unwrap();
        assert_eq!(truncated.len(), 4);
    }

    #[test]
    fn rejects_a_checkpoint_beyond_groks_history() {
        let error = truncate_at_turn(
            &[json!({"type":"user","prompt_index":0})],
            2,
            is_chat_prompt,
        )
        .unwrap_err();
        assert!(error.to_string().contains("only 1 native turns"));
    }

    #[test]
    fn reads_groks_generated_title_from_native_metadata() {
        let root = std::env::temp_dir().join(format!("waku-grok-title-{}", Uuid::new_v4()));
        let session_id = Uuid::new_v4().to_string();
        let session = root
            .join("sessions")
            .join("%2Ftmp%2Fproject")
            .join(&session_id);
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("summary.json"),
            serde_json::to_vec(&json!({
                "generated_title": "  Fix provider task titles  ",
                "session_summary": "A much longer summary"
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            generated_title_in(&root, &session_id).unwrap().as_deref(),
            Some("Fix provider task titles")
        );
        fs::remove_dir_all(root).ok();
    }
}

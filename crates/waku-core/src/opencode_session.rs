//! OpenCode server lifecycle and native-session helpers.

use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::model::{
    AgentTurn, Message, MessageRole, ProviderKind, ProviderResumeCursor,
    ProviderSessionHistory, ProviderSessionSummary, TurnStatus,
};
use uuid::Uuid;

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Forking copies every retained message and part into a new native session.
/// A long task can legitimately take longer than the ordinary request budget;
/// this operation already runs off the UI thread.
const FORK_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// The server binds its port about a second before the app behind it starts
/// answering, and a request accepted in that window is never answered at all.
/// A startup probe caught there must give up quickly and retry — at the full
/// `HTTP_TIMEOUT` one hung probe would eat the whole start budget.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Lists OpenCode's root sessions across every project, newest first.
///
/// ACP `session/list` is project-scoped: OpenCode resolves the request `cwd`,
/// or the process cwd without one, to a project and lists only that project's
/// sessions, so a catalog launched from Waku's isolated temp directory saw
/// nothing but the "global" project. The server's `/experimental/session`
/// route is the one cross-project listing OpenCode exposes, and each entry
/// carries the directory the session was started in.
pub fn list_provider_sessions(
    binary: &Path,
    limit: usize,
) -> anyhow::Result<Vec<ProviderSessionSummary>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let server = match crate::opencode_pool::any_live(binary) {
        Some(server) => server,
        None => crate::opencode_pool::acquire(
            binary,
            &crate::acp_session::catalog_working_directory()?,
        )?,
    };
    let response = server.request(
        "GET",
        &format!("/experimental/session?roots=true&limit={limit}"),
        None,
    )?;
    Ok(session_summaries(&response))
}

fn session_summaries(response: &Value) -> Vec<ProviderSessionSummary> {
    response
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(session_summary)
        .collect()
}

fn session_summary(session: &Value) -> Option<ProviderSessionSummary> {
    let session_id = session.get("id")?.as_str()?.trim();
    if session_id.is_empty() {
        return None;
    }
    let cwd = PathBuf::from(session.get("directory")?.as_str()?);
    if !cwd.is_absolute() {
        return None;
    }
    let time = session.get("time");
    let created_at = unix_seconds(time.and_then(|time| time.get("created")));
    let updated_at = unix_seconds(time.and_then(|time| time.get("updated"))).max(created_at);
    Some(ProviderSessionSummary {
        cursor: ProviderResumeCursor::OpenCode {
            session_id: session_id.to_owned(),
        },
        title: crate::acp_session::session_title(
            ProviderKind::OpenCode,
            session.get("title").and_then(Value::as_str),
            session_id,
        ),
        cwd,
        created_at,
        updated_at,
    })
}

/// OpenCode stamps sessions in Unix milliseconds; the catalog sorts in seconds.
fn unix_seconds(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or_default() / 1000
}

/// Loads one session's user-visible transcript through OpenCode's own HTTP
/// API instead of an ACP `session/load` replay.
///
/// The ACP route advertises `loadSession` but fails with an internal service
/// error on real sessions (observed on OpenCode 1.18.30), so the resume picker
/// cannot replay through it. The message list the fork path already reads is
/// the same transcript, fetched from the resident server.
pub fn provider_session_history(
    binary: &Path,
    cwd: &Path,
    session_id: &str,
    visible_turn_limit: usize,
) -> anyhow::Result<ProviderSessionHistory> {
    if session_id.trim().is_empty() || visible_turn_limit == 0 {
        return Ok(ProviderSessionHistory::default());
    }
    let server = crate::opencode_pool::acquire(binary, cwd)?;
    let path = format!("/session/{}/message", encode_path_segment(session_id));
    let messages = server
        .request_with_timeout("GET", &path, None, FORK_HTTP_TIMEOUT)
        .with_context(|| format!("OpenCode could not export session {session_id}"))?;
    let entries = messages
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode returned an invalid message list"))?;
    let mut history = history_from_native_messages(entries);
    retain_recent_turns(&mut history, visible_turn_limit);
    Ok(history)
}

/// Converts OpenCode's native `{info, parts}` entries into Waku's transcript
/// model. Each non-synthetic user text part opens a turn; the assistant reply
/// folds into it until the next user turn. Tool and reasoning parts are
/// dropped: `ProviderSessionHistory` carries messages and turns only, the
/// same shape every other import path produces.
fn history_from_native_messages(entries: &[Value]) -> ProviderSessionHistory {
    let mut history = ProviderSessionHistory::default();
    for entry in entries {
        let info = entry.get("info").unwrap_or(&Value::Null);
        let role = info.get("role").and_then(Value::as_str).unwrap_or_default();
        let completed_at = unix_seconds(info.pointer("/time/created"));
        let parts = entry.get("parts").and_then(Value::as_array);
        let Some(parts) = parts else { continue };

        if role == "user" {
            // Synthetic prompts (compaction summaries, queued system text)
            // are provider plumbing, not user turns.
            let text = parts
                .iter()
                .filter(|part| {
                    part.get("type").and_then(Value::as_str) == Some("text")
                        && part.get("synthetic").and_then(Value::as_bool) != Some(true)
                })
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                continue;
            }
            let turn_id = Uuid::new_v4();
            history.turns.push(AgentTurn {
                id: turn_id,
                turn_count: history.turns.len() + 1,
                status: TurnStatus::Completed,
                provider_turn_started: true,
                provider_resume_at: None,
                started_at: completed_at,
                completed_at: Some(completed_at),
                checkpoint: None,
            });
            let mut message = Message::new_for_turn(MessageRole::User, text, turn_id);
            message.created_at = completed_at;
            history.messages.push(message);
            continue;
        }

        if role != "assistant" {
            continue;
        }
        let Some(turn_id) = history.turns.last().map(|turn| turn.id) else {
            continue;
        };
        for part in parts {
            let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
            if part_type != "text" {
                continue;
            }
            let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            else {
                continue;
            };
            if let Some(message) = history.messages.last_mut().filter(|message| {
                message.role == MessageRole::Assistant && message.turn_id == Some(turn_id)
            }) {
                message.content.push('\n');
                message.content.push_str(text);
            } else {
                let mut message = Message::new_for_turn(MessageRole::Assistant, text, turn_id);
                message.created_at = completed_at;
                history.messages.push(message);
            }
        }
    }
    history
}

fn retain_recent_turns(history: &mut ProviderSessionHistory, limit: usize) {
    let retained = history
        .turns
        .iter()
        .rev()
        .take(limit)
        .map(|turn| turn.id)
        .collect::<HashSet<_>>();
    history
        .messages
        .retain(|message| message.turn_id.is_some_and(|id| retained.contains(&id)));
}

pub fn fork_session_at_turn(
    binary: &Path,
    cwd: &Path,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    // Shares the workspace's resident server when one is live; a transient
    // one is started and killed with the handle otherwise.
    let server = crate::opencode_pool::acquire(binary, cwd)?;
    fork_session_at_turn_on_server(&server, session_id, retained_turns)
}

/// Forks through the task's resident OpenCode server.
///
/// Starting a second `opencode serve` against the same workspace can contend
/// with the live process for OpenCode's local resources. Rewinds with a live
/// driver use this path instead, while cold sessions still use the standalone
/// helper above.
pub(crate) fn fork_session_at_turn_on_server(
    server: &OpenCodeServer,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let message_ids = native_user_message_ids(server, session_id)?;
    fork_session_with_message_ids(server, session_id, &message_ids, retained_turns)
}

pub(crate) fn fork_session_removing_turns_on_server(
    server: &OpenCodeServer,
    session_id: &str,
    turns_to_remove: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let message_ids = native_user_message_ids(server, session_id)?;
    let retained_turns = retained_turn_count(message_ids.len(), turns_to_remove)?;
    fork_session_with_message_ids(server, session_id, &message_ids, retained_turns)
}

fn retained_turn_count(total_turns: usize, turns_to_remove: usize) -> anyhow::Result<usize> {
    total_turns.checked_sub(turns_to_remove).ok_or_else(|| {
        anyhow!(
            "OpenCode has only {total_turns} native turns, but Waku needs to remove {turns_to_remove}"
        )
    })
}

fn native_user_message_ids(
    server: &OpenCodeServer,
    session_id: &str,
) -> anyhow::Result<Vec<String>> {
    let session_path = format!("/session/{}/message", encode_path_segment(session_id));
    let messages = server.request_with_timeout("GET", &session_path, None, FORK_HTTP_TIMEOUT)?;
    Ok(messages
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode returned an invalid message list"))?
        .iter()
        .filter_map(|message| {
            (is_native_user_turn(message))
                .then(|| message.pointer("/info/id").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .collect())
}

fn fork_session_with_message_ids(
    server: &OpenCodeServer,
    session_id: &str,
    message_ids: &[String],
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let fork_at = fork_message_id(&message_ids, retained_turns)?;
    let body = fork_at.map_or_else(|| json!({}), |message_id| json!({"messageID": message_id}));
    let fork_path = format!("/session/{}/fork", encode_path_segment(session_id));
    let fork = server.request_with_timeout("POST", &fork_path, Some(&body), FORK_HTTP_TIMEOUT)?;
    let fork_id = fork
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| fork.pointer("/data/id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("OpenCode returned no forked session ID"))?;
    Ok(ProviderResumeCursor::OpenCode {
        session_id: fork_id.to_owned(),
    })
}

fn fork_message_id(message_ids: &[String], retained_turns: usize) -> anyhow::Result<Option<&str>> {
    if retained_turns > message_ids.len() {
        bail!(
            "OpenCode has only {} native turns, but Waku needs {retained_turns}",
            message_ids.len()
        );
    }
    Ok(message_ids.get(retained_turns).map(String::as_str))
}

pub(crate) struct OpenCodeServer {
    child: Mutex<Child>,
    pub(crate) port: u16,
}

impl OpenCodeServer {
    pub(crate) fn start(binary: &Path, cwd: &Path) -> anyhow::Result<Self> {
        Self::start_with_env(binary, cwd, &[])
    }

    /// Starts the server with extra environment, so a caller can hand it the
    /// Computer Use configuration the same way a one-shot invocation got it.
    pub(crate) fn start_with_env(
        binary: &Path,
        cwd: &Path,
        environment: &[(String, String)],
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("could not reserve a local port for OpenCode")?;
        let port = listener.local_addr()?.port();
        drop(listener);

        let mut command = crate::command_env::command(binary);
        for (name, value) in environment {
            command.env(name, value);
        }
        let command = command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("OPENCODE_SERVER_PASSWORD", "")
            .env("OPENCODE_SERVER_USERNAME", "opencode")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child =
            crate::command_env::spawn(command).context("failed to start `opencode serve`")?;
        let server = Self {
            child: Mutex::new(child),
            port,
        };
        let started_at = Instant::now();
        loop {
            if server
                .request_with_timeout("GET", "/global/health", None, HEALTH_PROBE_TIMEOUT)
                .is_ok()
            {
                return Ok(server);
            }
            if let Some(status) = server.child.lock().try_wait()? {
                bail!("OpenCode session server exited during startup ({status})");
            }
            if started_at.elapsed() >= SERVER_START_TIMEOUT {
                bail!("timed out starting the OpenCode session server");
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        self.request_with_timeout(method, path, body, HTTP_TIMEOUT)
    }

    pub(crate) fn request_with_timeout(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        request_json_on_port(self.port, method, path, body, timeout)
    }

    /// Whether the server process is still running. `Child::try_wait` both
    /// observes and reaps an exited child; `kill(pid, 0)` cannot distinguish a
    /// running process from the unreaped zombie owned by this process.
    pub(crate) fn is_alive(&self) -> bool {
        self.child
            .lock()
            .try_wait()
            .is_ok_and(|status| status.is_none())
    }
}

fn is_native_user_turn(message: &Value) -> bool {
    message.pointer("/info/role").and_then(Value::as_str) == Some("user")
        && message
            .get("parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("text")
                        && part.get("synthetic").and_then(Value::as_bool) != Some(true)
                })
            })
}

impl OpenCodeServer {
    /// Terminates and reaps the owned child. The timeout is a graceful-exit
    /// budget; a server that ignores TERM is killed afterward.
    pub(crate) fn shutdown(&self, timeout: Duration) {
        let mut child = self.child.lock();
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }

        #[cfg(unix)]
        {
            let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        }
        #[cfg(not(unix))]
        {
            let _ = child.kill();
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }

        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for OpenCodeServer {
    fn drop(&mut self) {
        let child = self.child.get_mut();
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Sends one request to a server identified by port alone. Readers that must
/// not keep the server alive (they only unblock when it exits) hold the port
/// instead of a handle and request through this.
/// Sends one request to a server identified by port alone. Readers that must
/// not keep the server alive (they only unblock when it exits) hold the port
/// instead of a handle and request through this.
pub(crate) fn request_json_on_port(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> anyhow::Result<Value> {
    crate::http_wire::request_json(
        &crate::http_wire::Endpoint::local(port),
        method,
        path,
        body,
        timeout,
    )
}

pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_fork_message_excludes_the_next_user_turn() {
        let messages = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
        assert_eq!(fork_message_id(&messages, 0).unwrap(), Some("one"));
        assert_eq!(fork_message_id(&messages, 2).unwrap(), Some("three"));
        assert_eq!(fork_message_id(&messages, 3).unwrap(), None);
        assert!(fork_message_id(&messages, 4).is_err());
    }

    #[test]
    fn rollback_count_is_converted_to_the_retained_native_prefix() {
        assert_eq!(retained_turn_count(4, 1).unwrap(), 3);
        assert_eq!(retained_turn_count(4, 4).unwrap(), 0);
        assert!(retained_turn_count(4, 5).is_err());
    }

    #[test]
    fn native_turn_filter_ignores_compaction_and_synthetic_user_messages() {
        assert!(is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "hello"}]
        })));
        assert!(!is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "compaction", "auto": true}]
        })));
        assert!(!is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "continue", "synthetic": true}]
        })));
    }

    #[test]
    fn native_messages_import_into_user_and_assistant_turns() {
        let entries = vec![
            json!({
                "info": {"role": "user", "time": {"created": 1_000_000}},
                "parts": [{"type": "text", "text": "fix the bug"}]
            }),
            json!({
                "info": {"role": "assistant", "time": {"created": 1_000_050}},
                "parts": [
                    {"type": "reasoning", "text": "looking"},
                    {"type": "tool", "tool": "edit", "state": {"status": "completed", "title": "main.rs"}},
                    {"type": "text", "text": "fixed it"}
                ]
            }),
            // Synthetic provider plumbing must not open a turn.
            json!({
                "info": {"role": "user", "time": {"created": 1_000_060}},
                "parts": [{"type": "text", "text": "compaction", "synthetic": true}]
            }),
            json!({
                "info": {"role": "user", "time": {"created": 1_000_100}},
                "parts": [{"type": "text", "text": "now add tests"}]
            }),
            json!({
                "info": {"role": "assistant", "time": {"created": 1_000_200}},
                "parts": [{"type": "text", "text": "done"}]
            }),
        ];

        let history = history_from_native_messages(&entries);
        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.messages.len(), 4);
        assert_eq!(history.messages[0].role, MessageRole::User);
        assert_eq!(history.messages[0].content, "fix the bug");
        assert_eq!(history.messages[1].role, MessageRole::Assistant);
        assert_eq!(history.messages[1].content, "fixed it");
        assert_eq!(history.messages[2].role, MessageRole::User);
        assert_eq!(history.messages[2].content, "now add tests");
        assert_eq!(history.messages[3].content, "done");
        // Each assistant reply attaches to its own turn.
        assert_eq!(history.messages[1].turn_id, Some(history.turns[0].id));
        assert_eq!(history.messages[3].turn_id, Some(history.turns[1].id));
        // Timestamps come from the native payload in seconds.
        assert_eq!(history.turns[0].completed_at, Some(1_000));
    }

    #[test]
    fn assistant_text_before_any_user_turn_is_dropped() {
        let entries = vec![json!({
            "info": {"role": "assistant", "time": {"created": 5}},
            "parts": [{"type": "text", "text": "orphan"}]
        })];
        let history = history_from_native_messages(&entries);
        assert!(history.turns.is_empty());
        assert!(history.messages.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn liveness_probe_reaps_an_exited_child() {
        let child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("the probe child should start");
        let server = OpenCodeServer {
            child: Mutex::new(child),
            port: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && server.is_alive() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!server.is_alive(), "the exited child should be reaped");
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_waits_for_and_reaps_the_owned_child() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("the probe child should start");
        let server = OpenCodeServer {
            child: Mutex::new(child),
            port: 0,
        };
        let started = Instant::now();
        server.shutdown(Duration::from_secs(3));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a TERM-responsive child should not consume the shutdown budget"
        );
        assert!(!server.is_alive());
    }

    #[test]
    fn global_session_list_maps_root_sessions_across_projects() {
        let catalog_root = std::env::temp_dir().join("waku-opencode-session-catalog");
        let waku_directory = catalog_root.join("dev").join("waku");
        let response = json!([
            {
                "id": "ses_waku",
                "title": "Review and merge Waku PR #113",
                "directory": waku_directory,
                "time": { "created": 1_787_000_000_123_u64, "updated": 1_787_000_100_999_u64 },
                "project": { "id": "prj_waku", "worktree": waku_directory }
            },
            {
                "id": " ses_untitled ",
                "directory": catalog_root,
                "time": { "created": 1_786_000_000_000_u64 },
                "project": { "id": "global", "worktree": catalog_root }
            },
            { "id": "ses_relative", "title": "skipped", "directory": "relative/dir" },
            { "title": "no id", "directory": catalog_root },
            { "id": "", "directory": catalog_root }
        ]);

        let sessions = session_summaries(&response);

        assert_eq!(sessions.len(), 2, "{sessions:#?}");
        assert_eq!(
            sessions[0].cursor,
            ProviderResumeCursor::OpenCode {
                session_id: "ses_waku".into()
            }
        );
        assert_eq!(sessions[0].title, "Review and merge Waku PR #113");
        assert_eq!(sessions[0].cwd, waku_directory);
        assert_eq!(sessions[0].created_at, 1_787_000_000);
        assert_eq!(sessions[0].updated_at, 1_787_000_100);
        assert_eq!(sessions[1].title, "OpenCode session ses_unti");
        assert_eq!(sessions[1].cwd, catalog_root);
        assert_eq!(sessions[1].updated_at, sessions[1].created_at);
    }

    #[test]
    fn global_session_list_tolerates_a_non_array_response() {
        assert!(session_summaries(&Value::Null).is_empty());
        assert!(session_summaries(&json!({ "error": "nope" })).is_empty());
    }

    /// The catalog must come from OpenCode's cross-project store, not the
    /// project the server happens to run in: every entry keeps its own
    /// directory, and nothing is a subagent child.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn lists_sessions_across_projects_on_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let sessions = list_provider_sessions(&binary, 50).expect("the catalog should load");
        assert!(sessions.iter().all(|session| session.cwd.is_absolute()));
        assert!(
            sessions
                .windows(2)
                .all(|pair| pair[0].updated_at >= pair[1].updated_at),
            "OpenCode lists newest first"
        );
    }

    /// Exercises the same cold-session path used when an edited message is
    /// submitted after Waku has relaunched. The source session is supplied by
    /// the caller so this never creates provider traffic; it only forks the
    /// already-completed native transcript and removes the test fork again.
    #[test]
    #[ignore = "requires an installed opencode and WAKU_OPENCODE_TEST_SESSION_ID"]
    fn forks_away_a_real_single_turn_session() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let session_id = std::env::var("WAKU_OPENCODE_TEST_SESSION_ID")
            .expect("set WAKU_OPENCODE_TEST_SESSION_ID to a completed one-turn session");
        let cwd = std::env::current_dir().expect("the test working directory should exist");
        let server = OpenCodeServer::start(&binary, &cwd).expect("the server should start");
        let ProviderResumeCursor::OpenCode {
            session_id: fork_id,
        } = fork_session_at_turn_on_server(&server, &session_id, 0)
            .expect("the first turn should be excluded from the fork")
        else {
            panic!("expected an OpenCode cursor");
        };
        let messages = server
            .request(
                "GET",
                &format!("/session/{}/message", encode_path_segment(&fork_id)),
                None,
            )
            .expect("the fork should be readable");
        assert_eq!(messages.as_array().map(Vec::len), Some(0));
        server
            .request(
                "DELETE",
                &format!("/session/{}", encode_path_segment(&fork_id)),
                None,
            )
            .expect("the test fork should be removed");
    }
}

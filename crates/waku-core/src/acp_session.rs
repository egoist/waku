//! Provider-neutral ACP session discovery and transcript replay.
//!
//! ACP agents own their storage migrations and visible-history projection. A
//! one-shot `session/list` or `session/load` therefore stays more accurate than
//! reading their private stores, while typed updates let Waku discard private
//! reasoning and provider-only tool records by construction.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ClientCapabilities, Implementation, InitializeRequest, ListSessionsRequest, LoadSessionRequest,
    SessionNotification,
};
use agent_client_protocol::{Agent, Client, ConnectionTo};
use anyhow::{Context as _, anyhow};
use parking_lot::Mutex;
use serde_json::Value;
use uuid::Uuid;

use crate::model::{
    AgentTurn, Message, MessageRole, ProviderKind, ProviderResumeCursor, ProviderSessionHistory,
    ProviderSessionSummary, TurnStatus,
};

const MAX_CATALOG_PAGES: usize = 20;
const MAX_CATALOG_WORKSPACES: usize = 100;
const ACP_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Empty, app-owned cwd for provider catalog subprocesses.
///
/// Agent CLIs may index their process cwd during startup. Launching a catalog
/// from `$HOME` therefore makes a harmless session-list request recursively
/// touch Desktop, Documents, Downloads, Photos, and other macOS TCC locations.
/// Keeping discovery in an isolated temp directory prevents that implicit
/// workspace scan; actual session loading still receives the selected cwd.
pub(crate) fn catalog_working_directory() -> anyhow::Result<PathBuf> {
    let directory = std::env::temp_dir().join(format!(
        "waku-provider-session-catalog-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "could not create provider-session catalog directory {}",
            directory.display()
        )
    })?;
    Ok(directory)
}

fn initialize_request() -> InitializeRequest {
    InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(ClientCapabilities::new().terminal(false))
        .client_info(Implementation::new("waku", env!("CARGO_PKG_VERSION")))
}

fn timestamp(value: Option<&str>) -> u64 {
    value
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .and_then(|value| u64::try_from(value.timestamp()).ok())
        .unwrap_or_default()
}

pub(crate) fn session_title(
    provider: ProviderKind,
    title: Option<&str>,
    session_id: &str,
) -> String {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let short_id = session_id.chars().take(8).collect::<String>();
            format!("{} session {short_id}", provider.short_name())
        })
}

/// Lists every session exposed by an ACP agent. An empty `cwd_filters` list
/// requests the provider's global catalog; providers such as Kimi that require
/// a cwd pass their native workspace index here instead.
pub fn list_provider_sessions(
    provider: ProviderKind,
    binary: &Path,
    cwd_filters: &[PathBuf],
    limit: usize,
) -> anyhow::Result<Vec<ProviderSessionSummary>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let agent = crate::driver::catalog_agent(provider, binary, &catalog_working_directory()?)?;
    let filters = if cwd_filters.is_empty() {
        vec![None]
    } else {
        cwd_filters
            .iter()
            .take(MAX_CATALOG_WORKSPACES)
            .cloned()
            .map(Some)
            .collect()
    };

    let request = Client.builder().name("waku-session-catalog").connect_with(
        agent,
        async move |connection: ConnectionTo<Agent>| {
            let initialize = connection
                .send_request(initialize_request())
                .block_task()
                .await?;
            if initialize
                .agent_capabilities
                .session_capabilities
                .list
                .is_none()
            {
                return Ok(Vec::new());
            }

            let mut found = Vec::new();
            let mut seen = HashSet::new();
            for cwd in filters {
                let mut cursor = None;
                let mut seen_cursors = HashSet::new();
                for _ in 0..MAX_CATALOG_PAGES {
                    let mut request = ListSessionsRequest::new().cursor(cursor.clone());
                    if let Some(cwd) = cwd.clone() {
                        request = request.cwd(cwd);
                    }
                    let response = connection.send_request(request).block_task().await?;
                    for session in response.sessions {
                        let session_id = session.session_id.to_string();
                        if session_id.trim().is_empty()
                            || !session.cwd.is_absolute()
                            || !seen.insert(session_id.clone())
                        {
                            continue;
                        }
                        let updated_at = timestamp(session.updated_at.as_deref());
                        found.push(ProviderSessionSummary {
                            cursor: ProviderResumeCursor::from_session_id(
                                provider,
                                session_id.clone(),
                            ),
                            title: session_title(provider, session.title.as_deref(), &session_id),
                            cwd: session.cwd,
                            created_at: updated_at,
                            updated_at,
                        });
                    }
                    if found.len() >= limit {
                        break;
                    }
                    let Some(next) = response.next_cursor.filter(|next| !next.is_empty()) else {
                        break;
                    };
                    if !seen_cursors.insert(next.clone()) {
                        break;
                    }
                    cursor = Some(next);
                }
                if found.len() >= limit {
                    break;
                }
            }
            found.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            found.truncate(limit);
            Ok(found)
        },
    );
    smol::block_on(smol::future::race(
        async move { request.await.map_err(anyhow::Error::new) },
        async move {
            smol::Timer::after(ACP_REQUEST_TIMEOUT).await;
            Err(anyhow!(
                "{} ACP session catalog timed out",
                provider.display_name()
            ))
        },
    ))
    .with_context(|| format!("{} could not list ACP sessions", provider.display_name()))
}

/// Replays the provider's user-visible transcript through ACP `session/load`.
pub fn provider_session_history(
    provider: ProviderKind,
    binary: &Path,
    cwd: &Path,
    session_id: &str,
    visible_turn_limit: usize,
) -> anyhow::Result<ProviderSessionHistory> {
    if session_id.trim().is_empty() || visible_turn_limit == 0 {
        return Ok(ProviderSessionHistory::default());
    }
    let agent = crate::driver::catalog_agent(provider, binary, cwd)?;
    let updates = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = Arc::clone(&updates);
    let cwd = cwd.to_path_buf();
    let session_id = session_id.to_owned();

    let request = Client
        .builder()
        .name("waku-session-import")
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                if let Ok(update) = serde_json::to_value(notification.update) {
                    captured.lock().push(update);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let initialize = connection
                .send_request(initialize_request())
                .block_task()
                .await?;
            if !initialize.agent_capabilities.load_session {
                return Err(agent_client_protocol::Error::new(
                    agent_client_protocol::ErrorCode::MethodNotFound.into(),
                    "the agent does not support session/load",
                ));
            }
            connection
                .send_request(LoadSessionRequest::new(session_id, cwd))
                .block_task()
                .await?;
            // ACP ordering places replay before the load response. A short
            // grace still covers providers that flush their final queued
            // notification immediately after that response.
            smol::Timer::after(Duration::from_millis(50)).await;
            Ok(())
        });
    smol::block_on(smol::future::race(
        async move { request.await.map_err(anyhow::Error::new) },
        async move {
            smol::Timer::after(ACP_REQUEST_TIMEOUT).await;
            Err(anyhow!(
                "{} ACP session replay timed out",
                provider.display_name()
            ))
        },
    ))
    .with_context(|| format!("{} could not replay the session", provider.display_name()))?;

    let updates = std::mem::take(&mut *updates.lock());
    let mut history = history_from_updates(provider, &updates);
    retain_recent_messages(&mut history, visible_turn_limit);
    Ok(history)
}

fn history_from_updates(provider: ProviderKind, updates: &[Value]) -> ProviderSessionHistory {
    let mut history = ProviderSessionHistory::default();
    let mut saw_agent_activity = true;

    for update in updates {
        let kind = update.get("sessionUpdate").and_then(Value::as_str);
        if kind == Some("user_message_chunk") {
            let Some(text) = update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            if history.turns.is_empty() || saw_agent_activity {
                let turn_id = Uuid::new_v4();
                history.turns.push(AgentTurn {
                    id: turn_id,
                    turn_count: history.turns.len() + 1,
                    status: TurnStatus::Completed,
                    provider_turn_started: true,
                    provider_resume_at: None,
                    started_at: 0,
                    completed_at: Some(0),
                    checkpoint: None,
                });
                history
                    .messages
                    .push(Message::new_for_turn(MessageRole::User, text, turn_id));
                saw_agent_activity = false;
            } else if let Some(message) = history.messages.last_mut().filter(|message| {
                message.role == MessageRole::User
                    && message.turn_id == history.turns.last().map(|turn| turn.id)
            }) {
                message.content.push_str(text);
            }
            continue;
        }

        if matches!(
            kind,
            Some(
                "agent_message_chunk"
                    | "agent_thought_chunk"
                    | "tool_call"
                    | "tool_call_update"
                    | "plan"
            )
        ) {
            saw_agent_activity = true;
        }
        if kind != Some("agent_message_chunk") {
            continue;
        }
        let Some(text) = update
            .pointer("/content/text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        if provider == ProviderKind::Fx
            && (text.starts_with("[context] ") || text.starts_with("skill discovery warning: "))
        {
            continue;
        }
        let Some(turn_id) = history.turns.last().map(|turn| turn.id) else {
            continue;
        };
        if let Some(message) = history.messages.last_mut().filter(|message| {
            message.role == MessageRole::Assistant && message.turn_id == Some(turn_id)
        }) {
            message.content.push_str(text);
        } else {
            history
                .messages
                .push(Message::new_for_turn(MessageRole::Assistant, text, turn_id));
        }
    }
    history
}

fn retain_recent_messages(history: &mut ProviderSessionHistory, limit: usize) {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn catalog_processes_use_an_isolated_temp_working_directory() {
        let directory = catalog_working_directory().unwrap();
        assert!(directory.starts_with(std::env::temp_dir()));
        assert!(directory.is_dir());
        if let Some(home) = dirs::home_dir() {
            assert_ne!(directory, home);
            for protected in [
                "Desktop",
                "Documents",
                "Downloads",
                "Movies",
                "Music",
                "Pictures",
            ] {
                assert!(!directory.starts_with(home.join(protected)));
            }
        }
    }

    #[test]
    fn imports_only_visible_acp_chunks_and_preserves_empty_turn_shells() {
        let history = history_from_updates(
            ProviderKind::Kimi,
            &[
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"one"}}),
                json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"private"}}),
                json!({"sessionUpdate":"tool_call","toolCallId":"call-1","title":"read"}),
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}),
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"two"}}),
                json!({"sessionUpdate":"tool_call","toolCallId":"call-2","title":"bash"}),
            ],
        );

        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.messages.len(), 3);
        assert_eq!(history.messages[0].content, "one");
        assert_eq!(history.messages[1].content, "done");
        assert_eq!(history.messages[2].content, "two");
        assert!(
            history
                .messages
                .iter()
                .all(|message| !message.content.contains("private"))
        );
    }

    #[test]
    fn joins_consecutive_content_blocks_without_inventing_turns() {
        let history = history_from_updates(
            ProviderKind::OpenCode,
            &[
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello "}}),
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"world"}}),
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"part 1"}}),
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":" + part 2"}}),
            ],
        );

        assert_eq!(history.turns.len(), 1);
        assert_eq!(history.messages[0].content, "hello world");
        assert_eq!(history.messages[1].content, "part 1 + part 2");
    }
}

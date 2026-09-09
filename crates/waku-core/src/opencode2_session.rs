//! Cold-path OpenCode 2 helpers: the resume catalog, transcript import,
//! conversation forking, and the model/agent catalog.
//!
//! Everything here goes through [`opencode2_service::attached`], never
//! [`opencode2_service::shared`]. Opening the Resume picker or refreshing the
//! model catalog must not start the user's background daemon, so an absent
//! service is an empty catalog — not a spawn, and not a 20s start poll. (This
//! is the deliberate divergence from `opencode_session::list_provider_sessions`,
//! which starts a server and then blocks up to 5s killing it again.)
//!
//! Every call blocks on a socket, so every caller must already be off the UI
//! thread; one of these is several frames of budget.
//!
//! Two traps are encoded here rather than left to callers:
//!
//! * `GET …/export?sanitize=true` is a SHARE sanitizer, not a state stripper.
//!   Verified against beta-19192: it replaces the user's prompt, the
//!   assistant's prose, reasoning, tool input, tool output and tool metadata
//!   alike with `[redacted:…:msg_…]` placeholders, so importing a sanitized
//!   export yields a transcript of nothing but placeholders. Waku exports
//!   unsanitized and drops the private parts itself — see
//!   [`strip_provider_state`].
//! * One agent STEP is one assistant message. A three-tool turn is four
//!   assistant messages (three `finish: "tool-calls"` then one
//!   `finish: "stop"`), so the import folds every assistant message between
//!   two user messages into a single visual turn.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use uuid::Uuid;

use crate::http_wire::Endpoint;
use crate::model::{
    AgentTurn, Message, MessageRole, ProviderAgentPreset, ProviderKind, ProviderModel,
    ProviderResumeCursor, ProviderSessionHistory, ProviderSessionSummary, TurnStatus,
};
use crate::opencode2_api::{
    self, AgentInfo, AgentMode, AssistantContent, ForkRequestBoundary, MessageInfo, ModelInfo,
    Order, SessionInfo, ToolState,
};
use crate::opencode2_service;

/// Matches `acp_session`: a catalog that needs more than this many pages is
/// either enormous or looping, and both deserve the same bound.
const MAX_PAGES: usize = 20;
/// The service caps nothing, so the page size is Waku's choice. Large enough
/// that an ordinary catalog is one round trip.
const PAGE_SIZE: usize = 100;
/// `GET /api/session` lists subagent children too. The literal string `null`
/// is how the route asks for roots only; it is echoed back inside the opaque
/// cursor as `"parentID":"null"`, so it survives pagination.
const ROOT_SESSIONS_ONLY: &str = "null";
/// OpenCode 2's own default primary agent. A session created without one comes
/// back with `agent: "build"`, and the roster carries no `isDefault` marker.
#[allow(dead_code)]
const DEFAULT_AGENT: &str = "build";

/// Lists the adopted service's root sessions across every workspace, newest
/// first.
///
/// Returns an empty catalog when no service is registered or healthy. That is
/// the whole point of the attach-only rule: the Resume picker must never be
/// the thing that starts the user's daemon.
pub fn list_provider_sessions(
    binary: &Path,
    limit: usize,
) -> anyhow::Result<Vec<ProviderSessionSummary>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(service) = opencode2_service::attached(binary)? else {
        return Ok(Vec::new());
    };
    let endpoint = service.endpoint();
    let mut summaries = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let (sessions, next) = opencode2_api::list_sessions(
            &endpoint,
            None,
            Some(ROOT_SESSIONS_ONLY),
            Order::Desc,
            PAGE_SIZE.min(limit.max(1)),
            cursor.as_deref(),
        )
        .context("OpenCode 2 could not list its sessions")?;
        summaries.extend(sessions.iter().filter_map(session_summary));
        if summaries.len() >= limit {
            break;
        }
        // `cursor.next` is minted on every page including the last; the api
        // layer already collapses the empty page into an absent cursor.
        let Some(next) = next else { break };
        cursor = Some(next);
    }
    summaries.truncate(limit);
    Ok(summaries)
}

fn session_summary(session: &SessionInfo) -> Option<ProviderSessionSummary> {
    let session_id = session.id.trim();
    if session_id.is_empty() {
        return None;
    }
    let cwd = PathBuf::from(session.location.directory.trim());
    if !cwd.is_absolute() {
        return None;
    }
    let created_at = unix_seconds(session.time.created);
    Some(ProviderSessionSummary {
        cursor: ProviderResumeCursor::OpenCode2 {
            session_id: session_id.to_owned(),
            // Kept verbatim: the list filter and `location.directory` on
            // create are compared by exact string equality, so a resume must
            // reuse the very string the service stored.
            directory: Some(session.location.directory.clone()),
        },
        title: crate::acp_session::session_title(
            ProviderKind::OpenCode2,
            session.title.as_deref(),
            session_id,
        ),
        cwd,
        created_at,
        updated_at: unix_seconds(session.time.updated).max(created_at),
    })
}

/// OpenCode 2 stamps time in Unix milliseconds as a JSON number; the catalog
/// sorts in seconds.
fn unix_seconds(millis: f64) -> u64 {
    if millis.is_finite() && millis > 0.0 {
        (millis / 1000.0) as u64
    } else {
        0
    }
}

/// Imports a native session's user-visible transcript.
///
/// One `GET …/export` covers the whole conversation, unlike the paged message
/// route. It is fetched UNSANITIZED on purpose — see the module doc — and the
/// private provider state is dropped here before anything is built from it.
pub fn provider_session_history(
    binary: &Path,
    session_id: &str,
    visible_turn_limit: usize,
) -> anyhow::Result<ProviderSessionHistory> {
    if session_id.trim().is_empty() || visible_turn_limit == 0 {
        return Ok(ProviderSessionHistory::default());
    }
    let service = opencode2_service::attached(binary)?
        .ok_or_else(|| anyhow!("the OpenCode 2 background service is not running"))?;
    let mut export = opencode2_api::export_session(&service.endpoint(), session_id, false)
        .with_context(|| format!("OpenCode 2 could not export session {session_id}"))?;
    strip_provider_state(&mut export.messages);
    let mut history = history_from_messages(&export.messages);
    retain_recent_messages(&mut history, visible_turn_limit);
    Ok(history)
}

/// Drops every private provider control marker from a decoded transcript.
///
/// Belt and braces. `providerState` and `providerResultState` never survive
/// the typed decode, but a part's `state` does, and CLAUDE.md forbids ever
/// surfacing those in the transcript. They are also the bulk of an export's
/// memory: a single reasoning blob runs to multiple kilobytes, and the tool
/// state carries whole tool inputs and outputs an imported transcript never
/// renders. Doing it unconditionally means a change in what the server
/// chooses to redact cannot leak anything into Waku.
fn strip_provider_state(messages: &mut [MessageInfo]) {
    for message in messages {
        let MessageInfo::Assistant { content, .. } = message else {
            continue;
        };
        for part in content {
            match part {
                AssistantContent::Text { state, .. }
                | AssistantContent::Reasoning { state, .. } => *state = None,
                AssistantContent::Tool { state, .. } => *state = ToolState::Unknown,
                AssistantContent::Unknown => {}
            }
        }
    }
}

/// Projects stored messages onto Waku's turn model.
///
/// Only `user` and assistant TEXT survive: reasoning is dropped from an
/// imported transcript the way `acp_session` drops it, and every other message
/// kind — synthetic, system, skill, shell, compaction and the three
/// `*-switched` records — is provider bookkeeping rather than conversation.
fn history_from_messages(messages: &[MessageInfo]) -> ProviderSessionHistory {
    let mut history = ProviderSessionHistory::default();
    let mut turn_id = None;
    // Whether the last pushed message is this turn's assistant message, and so
    // whether the next step's text folds into it instead of starting one.
    let mut assistant_open = false;

    for message in messages {
        match message {
            MessageInfo::User { text, .. } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let id = Uuid::new_v4();
                history.turns.push(AgentTurn {
                    id,
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
                    .push(Message::new_for_turn(MessageRole::User, text, id));
                turn_id = Some(id);
                assistant_open = false;
            }
            MessageInfo::Assistant { content, .. } => {
                // Assistant activity before the first user message belongs to
                // no visible turn; a forked session can legitimately start
                // that way.
                let Some(id) = turn_id else { continue };
                for part in content {
                    let AssistantContent::Text { text, .. } = part else {
                        continue;
                    };
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    match history.messages.last_mut().filter(|_| assistant_open) {
                        Some(message) => {
                            message.content.push_str("\n\n");
                            message.content.push_str(text);
                        }
                        None => {
                            history.messages.push(Message::new_for_turn(
                                MessageRole::Assistant,
                                text,
                                id,
                            ));
                            assistant_open = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    history
}

/// Keeps every turn shell so provider turn numbering stays exact, but bounds
/// the imported display text to the most recent turns.
fn retain_recent_messages(history: &mut ProviderSessionHistory, limit: usize) {
    let retained = history
        .turns
        .iter()
        .rev()
        .take(limit)
        .map(|turn| turn.id)
        .collect::<std::collections::HashSet<_>>();
    history
        .messages
        .retain(|message| message.turn_id.is_some_and(|id| retained.contains(&id)));
}

/// Branches a cold session, keeping its first `retained_turns` native turns.
///
/// Attach-only, like everything else here: rewinding an imported task after a
/// relaunch must not start the daemon.
pub fn fork_session_at_turn(
    binary: &Path,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let service = opencode2_service::attached(binary)?
        .ok_or_else(|| anyhow!("the OpenCode 2 background service is not running"))?;
    let endpoint = service.endpoint();
    let message_ids = native_user_message_ids(&endpoint, session_id)?;
    fork_at_boundary(&endpoint, session_id, &message_ids, retained_turns)
}

/// Branches a session by dropping its last `turns_to_remove` native turns.
///
/// Deliberately NOT built on `POST …/revert/stage|commit|clear`: revert
/// mutates the same session, writes files, and answers 409 `SessionBusyError`
/// while a turn is running, whereas Waku's contract here is "drop the last N
/// turns and hand back a cursor for the continuing conversation". Revert is
/// real headroom — it survives reconnect via `Session.Info.revert` and carries
/// per-file patches — and wants its own affordance rather than this one.
// Reached once the driver's rewind path lands; see the step plan.
#[allow(dead_code)]
pub(crate) fn fork_session_removing_turns(
    endpoint: &Endpoint,
    session_id: &str,
    turns_to_remove: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let message_ids = native_user_message_ids(endpoint, session_id)?;
    let retained_turns = retained_turn_count(message_ids.len(), turns_to_remove)?;
    fork_at_boundary(endpoint, session_id, &message_ids, retained_turns)
}

/// The ids of the session's real user turns, oldest first.
///
/// `synthetic`, `system`, `skill`, `shell`, `compaction` and the three
/// `*-switched` records all sit in the same message list, and counting any of
/// them as a turn would fork at the wrong boundary.
pub(crate) fn native_user_message_ids(
    endpoint: &Endpoint,
    session_id: &str,
) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let (messages, next) = opencode2_api::list_messages(
            endpoint,
            session_id,
            Order::Asc,
            Some(PAGE_SIZE),
            cursor.as_deref(),
        )
        .with_context(|| format!("OpenCode 2 could not list messages for {session_id}"))?;
        ids.extend(messages.iter().filter_map(native_user_message_id));
        let Some(next) = next else { break };
        cursor = Some(next);
    }
    Ok(ids)
}

fn native_user_message_id(message: &MessageInfo) -> Option<String> {
    match message {
        MessageInfo::User { id, .. } if !id.trim().is_empty() => Some(id.clone()),
        _ => None,
    }
}

fn fork_at_boundary(
    endpoint: &Endpoint,
    session_id: &str,
    message_ids: &[String],
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let boundary = fork_boundary(message_ids, retained_turns)?;
    let fork = opencode2_api::fork(endpoint, session_id, &boundary)
        .with_context(|| format!("OpenCode 2 could not fork session {session_id}"))?;
    if fork.id.trim().is_empty() {
        bail!("OpenCode 2 returned no forked session ID");
    }
    Ok(ProviderResumeCursor::OpenCode2 {
        session_id: fork.id,
        directory: Some(fork.location.directory),
    })
}

/// The boundary that keeps exactly `retained_turns` user turns.
///
/// `before` names the first EXCLUDED user message; keeping everything has no
/// message id at all and is `through`. The read and write shapes differ here —
/// `ForkBoundary` always echoes a `messageID` back, including for `through` —
/// which is why they are separate types.
fn fork_boundary(
    message_ids: &[String],
    retained_turns: usize,
) -> anyhow::Result<ForkRequestBoundary> {
    if retained_turns > message_ids.len() {
        bail!(
            "OpenCode 2 has only {} native turns, but Waku needs {retained_turns}",
            message_ids.len()
        );
    }
    Ok(match message_ids.get(retained_turns) {
        Some(message_id) => ForkRequestBoundary::Before {
            message_id: message_id.clone(),
        },
        None => ForkRequestBoundary::Through,
    })
}

fn retained_turn_count(total_turns: usize, turns_to_remove: usize) -> anyhow::Result<usize> {
    total_turns.checked_sub(turns_to_remove).ok_or_else(|| {
        anyhow!(
            "OpenCode 2 has only {total_turns} native turns, but Waku needs to remove {turns_to_remove}"
        )
    })
}

/// The service's model and primary-agent catalogs.
///
/// Attach-only, so a launch with no service running answers empties and
/// `model_catalog` falls back to its disk cache rather than starting the
/// user's daemon just to refresh a picker.
// Reached once `model_catalog` dispatches OpenCode 2 here.
#[allow(dead_code)]
pub(crate) fn discover_catalog(
    binary: &Path,
) -> (Vec<ProviderModel>, Option<Vec<ProviderAgentPreset>>) {
    let Ok(Some(service)) = opencode2_service::attached(binary) else {
        return (Vec::new(), None);
    };
    let endpoint = service.endpoint();
    // No directory: both catalogs are global, and scoping them would only pin
    // them to whichever workspace asked first.
    let models = opencode2_api::list_models(&endpoint, None)
        .map(|models| catalog_models(&models))
        .unwrap_or_default();
    let presets = opencode2_api::list_agents(&endpoint, None)
        .ok()
        .map(|agents| agent_presets(&agents));
    (models, presets)
}

fn catalog_models(models: &[ModelInfo]) -> Vec<ProviderModel> {
    models
        .iter()
        .filter(|model| model.enabled)
        .filter_map(|model| {
            let provider = model.provider_id.trim();
            let id = model.id.trim();
            if provider.is_empty() || id.is_empty() {
                return None;
            }
            let name = model.name.trim();
            let name = if name.is_empty() { id } else { name };
            let catalog = ProviderModel::new(format!("{provider}/{id}"), name)
                .sub_provider(crate::model_catalog::display_name_from_slug(provider));
            // A model's variants ARE its reasoning-effort ladder: the ids are
            // `low`/`medium`/`high`/`max`/`minimal`/`xhigh`/`none`/`thinking`,
            // and the chosen one rides on `ModelRef::variant`. Dropping them
            // left the effort control empty for every OpenCode 2 model.
            Some(crate::model_catalog::with_variant_efforts(
                catalog,
                model.variants.iter().map(|variant| variant.id.as_str()),
            ))
        })
        .collect()
}

/// Only agents a session can actually be STARTED as.
///
/// `subagent` entries are dispatch targets rather than session compositions,
/// and the hidden primaries (`title`, `summary`, `compaction`) are the
/// service's own internal agents.
fn agent_presets(agents: &[AgentInfo]) -> Vec<ProviderAgentPreset> {
    agents
        .iter()
        .filter(|agent| matches!(agent.mode, AgentMode::Primary | AgentMode::All) && !agent.hidden)
        .filter_map(|agent| {
            let id = agent.id.trim();
            if id.is_empty() {
                return None;
            }
            let name = agent.name.trim();
            let mut preset = ProviderAgentPreset::new(id, if name.is_empty() { id } else { name });
            preset.is_default = id == DEFAULT_AGENT;
            preset.description = agent
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_owned);
            Some(preset)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn sessions(value: Value) -> Vec<SessionInfo> {
        serde_json::from_value(value).expect("the session list should decode")
    }

    fn messages(value: Value) -> Vec<MessageInfo> {
        serde_json::from_value(value).expect("the message list should decode")
    }

    fn assistant(id: &str, content: Value) -> Value {
        json!({
            "id": id,
            "type": "assistant",
            "time": {"created": 1_785_784_477_974_u64},
            "agent": "build",
            "model": {"id": "muse-spark-1.3", "providerID": "opencode", "variant": "default"},
            "content": content,
        })
    }

    fn user(id: &str, text: &str) -> Value {
        json!({
            "id": id,
            "type": "user",
            "time": {"created": 1_785_784_473_836_u64},
            "text": text,
        })
    }

    #[test]
    fn summaries_keep_the_stored_directory_and_convert_millis_to_seconds() {
        let project_dir = std::env::temp_dir().join("waku-opencode2-project");
        let untitled_dir = std::env::temp_dir().join("waku-opencode2-untitled");
        let listed = sessions(json!([
            {
                "id": "ses_waku",
                "projectID": "prj",
                "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1_788_689_510_000_u64, "updated": 1_788_689_514_982_u64},
                "title": "Review and merge Waku PR #113",
                "location": {"directory": project_dir}
            },
            {
                "id": " ses_untitled ",
                "projectID": "global",
                "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1_786_109_016_231_u64, "updated": 0},
                "location": {"directory": untitled_dir}
            },
            {
                "id": "ses_relative",
                "projectID": "prj",
                "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1_786_109_016_231_u64, "updated": 0},
                "location": {"directory": "relative/dir"}
            }
        ]));

        let summaries = listed
            .iter()
            .filter_map(session_summary)
            .collect::<Vec<_>>();

        assert_eq!(summaries.len(), 2, "{summaries:#?}");
        assert_eq!(
            summaries[0].cursor,
            ProviderResumeCursor::OpenCode2 {
                session_id: "ses_waku".into(),
                directory: Some(project_dir.to_string_lossy().into_owned()),
            }
        );
        assert_eq!(summaries[0].cwd, project_dir);
        assert_eq!(summaries[1].cwd, untitled_dir);
        assert_eq!(summaries[0].created_at, 1_788_689_510);
        assert_eq!(summaries[0].updated_at, 1_788_689_514);
        // A session the service never updated must not sort before its own
        // creation.
        assert_eq!(summaries[1].title, "OpenCode 2 session ses_unti");
        assert_eq!(summaries[1].updated_at, summaries[1].created_at);
    }

    #[test]
    fn consecutive_assistant_steps_fold_into_one_turn() {
        let transcript = messages(json!([
            user("msg_a", "draw a dog"),
            // A three-tool turn is four assistant messages.
            assistant(
                "msg_b",
                json!([{"type": "text", "text": "Loading the skill."}])
            ),
            assistant("msg_c", json!([{"type": "text", "text": "Now drawing."}])),
            assistant("msg_d", json!([{"type": "text", "text": "Done."}])),
            user("msg_e", "thanks"),
            assistant("msg_f", json!([{"type": "text", "text": "Any time."}])),
        ]));

        let history = history_from_messages(&transcript);

        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.turns[0].turn_count, 1);
        assert_eq!(history.turns[1].turn_count, 2);
        assert_eq!(history.messages.len(), 4);
        assert_eq!(history.messages[1].role, MessageRole::Assistant);
        assert_eq!(
            history.messages[1].content,
            "Loading the skill.\n\nNow drawing.\n\nDone."
        );
        assert_eq!(history.messages[1].turn_id, Some(history.turns[0].id));
        assert_eq!(history.messages[3].turn_id, Some(history.turns[1].id));
    }

    #[test]
    fn import_drops_reasoning_tools_and_provider_bookkeeping() {
        let transcript = messages(json!([
            {"id": "msg_switch", "type": "agent-switched", "time": {"created": 1.0}, "agent": "build"},
            {"id": "msg_sys", "type": "system", "time": {"created": 1.0}, "text": "system prompt"},
            user("msg_a", "hello"),
            assistant(
                "msg_b",
                json!([
                    {"type": "reasoning", "text": "The user wants a dog."},
                    {"type": "tool", "id": "call_1", "name": "bash", "time": {"created": 1.0},
                     "state": {"status": "completed", "input": {}, "content": [{"type": "text", "text": "out"}]}},
                    {"type": "text", "text": "Hi."}
                ])
            ),
            {"id": "msg_synth", "type": "synthetic", "time": {"created": 1.0}, "text": "continue"},
            {"id": "msg_shell", "type": "shell", "time": {"created": 1.0}, "shellID": "sh_1",
             "command": "ls", "status": "exited", "exit": "NaN"},
        ]));

        let history = history_from_messages(&transcript);

        assert_eq!(history.turns.len(), 1);
        let content = history
            .messages
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            content,
            vec![
                (MessageRole::User, "hello"),
                (MessageRole::Assistant, "Hi.")
            ]
        );
    }

    /// The private markers must be gone before anything is built from the
    /// export, not merely unread.
    #[test]
    fn provider_state_is_stripped_from_every_part() {
        let mut transcript = messages(json!([assistant(
            "msg_b",
            json!([
                {"type": "text", "text": "Hi.", "state": {"providerMetadata": {"secret": 1}}},
                {"type": "reasoning", "text": "think", "state": {"reasoningEncryptedContent": "blob"}},
                {"type": "tool", "id": "call_1", "name": "bash", "time": {"created": 1.0},
                 "state": {"status": "completed", "input": {"command": "ls"},
                           "content": [{"type": "text", "text": "out"}],
                           "metadata": {"private": true}}}
            ])
        )]));

        strip_provider_state(&mut transcript);

        let MessageInfo::Assistant { content, .. } = &transcript[0] else {
            panic!("expected an assistant message");
        };
        assert_eq!(
            content[0],
            AssistantContent::Text {
                text: "Hi.".into(),
                state: None
            }
        );
        let AssistantContent::Reasoning { state, .. } = &content[1] else {
            panic!("expected a reasoning part");
        };
        assert!(state.is_none());
        let AssistantContent::Tool { state, .. } = &content[2] else {
            panic!("expected a tool part");
        };
        assert_eq!(*state, ToolState::Unknown);
    }

    #[test]
    fn only_real_user_messages_count_as_native_turns() {
        let transcript = messages(json!([
            user("msg_user", "hello"),
            {"id": "msg_synth", "type": "synthetic", "time": {"created": 1.0}, "text": "continue"},
            {"id": "msg_sys", "type": "system", "time": {"created": 1.0}, "text": "prompt"},
            {"id": "msg_skill", "type": "skill", "time": {"created": 1.0}, "skill": "s", "name": "S", "text": "t"},
            {"id": "msg_shell", "type": "shell", "time": {"created": 1.0}, "shellID": "sh",
             "command": "ls", "status": "exited"},
            {"id": "msg_compact", "type": "compaction", "time": {"created": 1.0},
             "status": "completed", "reason": "auto"},
            {"id": "msg_agent", "type": "agent-switched", "time": {"created": 1.0}, "agent": "plan"},
            {"id": "msg_model", "type": "model-switched", "time": {"created": 1.0},
             "model": {"id": "m", "providerID": "p"}},
            {"id": "msg_loc", "type": "location-switched", "time": {"created": 1.0},
             "location": {"directory": "/tmp"}},
            assistant("msg_assist", json!([{"type": "text", "text": "Hi."}])),
            user("msg_user2", "again"),
        ]));

        let ids = transcript
            .iter()
            .filter_map(native_user_message_id)
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["msg_user".to_owned(), "msg_user2".to_owned()]);
    }

    #[test]
    fn fork_boundary_excludes_the_next_user_turn() {
        let ids = vec!["msg_1".to_owned(), "msg_2".to_owned(), "msg_3".to_owned()];
        assert_eq!(
            fork_boundary(&ids, 0).unwrap(),
            ForkRequestBoundary::Before {
                message_id: "msg_1".into()
            }
        );
        assert_eq!(
            fork_boundary(&ids, 2).unwrap(),
            ForkRequestBoundary::Before {
                message_id: "msg_3".into()
            }
        );
        // Retaining everything has no first-excluded message at all.
        assert_eq!(
            fork_boundary(&ids, 3).unwrap(),
            ForkRequestBoundary::Through
        );
        assert!(fork_boundary(&ids, 4).is_err());
    }

    #[test]
    fn rollback_count_is_converted_to_the_retained_native_prefix() {
        assert_eq!(retained_turn_count(4, 1).unwrap(), 3);
        assert_eq!(retained_turn_count(4, 4).unwrap(), 0);
        assert!(retained_turn_count(4, 5).is_err());
    }

    #[test]
    fn catalog_models_are_addressed_by_provider_and_id() {
        let models: Vec<ModelInfo> = serde_json::from_value(json!([
            {
                "id": "gemini-3.8-flash",
                "modelID": "gemini-3.8-flash",
                "providerID": "github-copilot",
                "name": "Gemini 3.8 Flash",
                "status": "active",
                "enabled": true,
                "limit": {"context": 1_048_576, "output": 65_536}
            },
            {
                "id": "deepseek-v4-flash-vision-exp",
                "modelID": "deepseek-v4-flash-vision-exp",
                "providerID": "deepseek",
                "name": "DeepSeek V4 Flash Vision Exp",
                "status": "beta",
                "enabled": true,
                "limit": {"context": 128_000, "output": 8_000}
            },
            {
                "id": "retired",
                "modelID": "retired",
                "providerID": "opencode",
                "name": "Retired",
                "status": "active",
                "enabled": false,
                "limit": {"context": 0, "output": 0}
            }
        ]))
        .expect("the model catalogue should decode");

        let models = catalog_models(&models);

        assert_eq!(models.len(), 2, "{models:#?}");
        assert_eq!(models[0].id, "github-copilot/gemini-3.8-flash");
        assert_eq!(models[0].name, "Gemini 3.8 Flash");
        assert_eq!(models[0].sub_provider.as_deref(), Some("Github Copilot"));
        // A beta model is still selectable; only a disabled one is not.
        assert_eq!(models[1].id, "deepseek/deepseek-v4-flash-vision-exp");
    }

    #[test]
    fn agent_presets_keep_only_startable_primaries() {
        let agents: Vec<AgentInfo> = serde_json::from_value(json!([
            {"id": "build", "name": "Build", "mode": "primary", "hidden": false,
             "description": "The default agent."},
            {"id": "plan", "name": "Plan", "mode": "primary", "hidden": false},
            {"id": "general", "name": "General", "mode": "subagent", "hidden": false},
            {"id": "title", "name": "Title", "mode": "primary", "hidden": true},
            {"id": "review", "name": "Review", "mode": "all", "hidden": false}
        ]))
        .expect("the agent roster should decode");

        let presets = agent_presets(&agents);

        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.id.as_str())
                .collect::<Vec<_>>(),
            vec!["build", "plan", "review"]
        );
        assert!(presets[0].is_default);
        assert_eq!(
            presets[0].description.as_deref(),
            Some("The default agent.")
        );
        assert!(!presets[1].is_default);
    }

    /// The whole point of the attach-only rule. A missing service must not
    /// spawn one, and the picker must not fail either.
    #[test]
    fn a_missing_service_lists_nothing_and_starts_nothing() {
        let binary = std::env::temp_dir().join("waku-opencode2-absent");
        assert!(list_provider_sessions(&binary, 0).unwrap().is_empty());
        assert_eq!(
            provider_session_history(&binary, "ses_x", 0)
                .unwrap()
                .messages
                .len(),
            0
        );
    }

    /// Exercises the real catalog against the user's own service. Read-only:
    /// it lists sessions and never creates, prompts or deletes.
    #[test]
    #[ignore = "requires a running opencode2 background service"]
    fn lists_real_root_sessions_across_workspaces() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode2 is not installed");
        let sessions = list_provider_sessions(&binary, 50).expect("the catalog should load");
        assert!(sessions.iter().all(|session| session.cwd.is_absolute()));
        assert!(
            sessions
                .windows(2)
                .all(|pair| pair[0].updated_at >= pair[1].updated_at),
            "OpenCode 2 lists newest first"
        );
    }

    /// The service is the only source of the v2 catalog, and both halves have
    /// to arrive together or the picker degrades to the disk cache.
    #[test]
    #[ignore = "requires a running opencode2 background service"]
    fn discovers_the_real_model_and_agent_catalog() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode2 is not installed");
        let (models, presets) = discover_catalog(&binary);
        assert!(!models.is_empty(), "the service should expose models");
        assert!(models.iter().all(|model| model.id.contains('/')));
        let presets = presets.expect("the service should expose agents");
        assert!(presets.iter().any(|preset| preset.id == DEFAULT_AGENT));
    }

    /// Guards the sanitize trap: an unsanitized export carries real prose,
    /// while `sanitize=true` replaces even the user's own prompt with a
    /// `[redacted:…]` placeholder.
    #[test]
    #[ignore = "requires a running opencode2 background service and WAKU_OPENCODE2_TEST_SESSION_ID"]
    fn imports_a_real_transcript_without_redaction_placeholders() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode2 is not installed");
        let session_id = std::env::var("WAKU_OPENCODE2_TEST_SESSION_ID")
            .expect("set WAKU_OPENCODE2_TEST_SESSION_ID to a completed session");
        let history =
            provider_session_history(&binary, &session_id, 100).expect("the import should work");
        assert!(!history.messages.is_empty());
        assert!(
            history
                .messages
                .iter()
                .all(|message| !message.content.contains("[redacted:")),
            "the import must not go through the share sanitizer"
        );
    }
}

#[cfg(test)]
mod discovery_smoke {
    /// Proves the picker is actually populated against a live service, which a
    /// unit test over canned JSON cannot.
    #[test]
    #[ignore = "requires a running opencode2 background service"]
    fn discovers_models_from_the_adopted_service() {
        let (models, presets) = super::discover_catalog(std::path::Path::new("opencode2"));
        println!(
            "models={} presets={:?}",
            models.len(),
            presets.as_ref().map(Vec::len)
        );
        let with_efforts: Vec<_> = models
            .iter()
            .filter(|model| !model.reasoning_efforts.is_empty())
            .collect();
        println!("with efforts={}", with_efforts.len());
        for model in with_efforts.iter().take(4) {
            println!(
                "  {} -> {:?} (default {:?})",
                model.id,
                model
                    .reasoning_efforts
                    .iter()
                    .map(|effort| effort.id.as_str())
                    .collect::<Vec<_>>(),
                model.default_reasoning_effort
            );
        }
        assert!(!models.is_empty(), "expected a non-empty model catalog");
        assert!(
            !with_efforts.is_empty(),
            "expected some models to expose variants"
        );
    }
}

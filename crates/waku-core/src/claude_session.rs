//! Claude native transcript and fork helpers.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Seek as _, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::model::{
    AgentTurn, Message, MessageRole, ProviderResumeCursor, ProviderSessionHistory,
    ProviderSessionSummary, TurnStatus,
};

const TRANSCRIPT_TYPES: [&str; 5] = ["user", "assistant", "attachment", "system", "progress"];
const SUMMARY_EDGE_BYTES: u64 = 256 * 1024;

pub struct ForkedClaudeSession {
    pub session_id: String,
    pub message_ids: HashMap<String, String>,
}

pub struct ClaudeSessionMetadata {
    pub latest_message_id: Option<String>,
    pub title: Option<String>,
}

pub fn session_metadata(session_id: &str) -> anyhow::Result<ClaudeSessionMetadata> {
    session_metadata_in(&projects_directory()?, session_id)
}

pub fn message_id_for_turn(session_id: &str, provider_turn_count: usize) -> anyhow::Result<String> {
    message_id_for_turn_in(&projects_directory()?, session_id, provider_turn_count)
}

pub fn fork_session_at(
    session_id: &str,
    up_to_message_id: &str,
    title: &str,
) -> anyhow::Result<ForkedClaudeSession> {
    fork_session_at_in(&projects_directory()?, session_id, up_to_message_id, title)
}

/// List Claude Code conversations that can be resumed by session ID.
///
/// Claude keeps one JSONL file per top-level conversation directly beneath a
/// project directory. Sub-agent transcripts live deeper and are deliberately
/// excluded, matching the interactive `/resume` picker.
pub fn list_provider_sessions(limit: usize) -> anyhow::Result<Vec<ProviderSessionSummary>> {
    list_provider_sessions_in(&projects_directory()?, limit)
}

/// Import the visible text of one Claude Code conversation. The native JSONL
/// remains the source of truth and the returned cursor continues that exact
/// conversation on the next Waku prompt.
pub fn provider_session_history(
    session_id: &str,
    turn_limit: usize,
) -> anyhow::Result<ProviderSessionHistory> {
    provider_session_history_in(&projects_directory()?, session_id, turn_limit)
}

fn projects_directory() -> anyhow::Result<PathBuf> {
    let config_directory = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .ok_or_else(|| anyhow!("Claude's configuration directory could not be located"))?;
    Ok(config_directory.join("projects"))
}

fn find_session_file(projects_directory: &Path, session_id: &str) -> anyhow::Result<PathBuf> {
    Uuid::parse_str(session_id).context("Claude returned an invalid session ID")?;
    let filename = format!("{session_id}.jsonl");
    for entry in fs::read_dir(projects_directory).with_context(|| {
        format!(
            "could not read Claude's session directory at {}",
            projects_directory.display()
        )
    })? {
        let Ok(entry) = entry else {
            continue;
        };
        let candidate = entry.path().join(&filename);
        if candidate
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0)
        {
            return Ok(candidate);
        }
    }
    bail!("Claude session {session_id} was not found on disk")
}

fn read_entries(path: &Path) -> anyhow::Result<Vec<Value>> {
    let file = fs::File::open(path)
        .with_context(|| format!("could not open Claude session {}", path.display()))?;
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .collect())
}

fn compact_import_entry(value: Value) -> Option<Value> {
    let entry = value.as_object()?;
    let kind = entry.get("type").and_then(Value::as_str)?;
    if !TRANSCRIPT_TYPES.contains(&kind) {
        return None;
    }
    let mut compact = Map::new();
    for key in [
        "type",
        "uuid",
        "parentUuid",
        "isSidechain",
        "isMeta",
        "timestamp",
    ] {
        if let Some(value) = entry.get(key) {
            compact.insert(key.to_owned(), value.clone());
        }
    }
    if matches!(kind, "user" | "assistant")
        && let Some(content) = entry
            .get("message")
            .and_then(|message| message.get("content"))
    {
        let compact_content = match content {
            Value::String(text) => Value::String(text.clone()),
            Value::Array(blocks) if kind == "user" => {
                if blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
                {
                    json!([{ "type": "tool_result" }])
                } else {
                    let mut text = blocks
                        .iter()
                        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|block| {
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .map(|text| json!({"type": "text", "text": text}))
                        })
                        .collect::<Vec<_>>();
                    if text.is_empty() && !blocks.is_empty() {
                        // Preserve a non-text user input as a real turn without
                        // retaining its potentially large image payload.
                        text.push(json!({"type": "input"}));
                    }
                    Value::Array(text)
                }
            }
            Value::Array(blocks) => Value::Array(
                blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| {
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|text| json!({"type": "text", "text": text}))
                    })
                    .collect(),
            ),
            _ => Value::Null,
        };
        compact.insert("message".into(), json!({"content": compact_content}));
    }
    Some(Value::Object(compact))
}

fn read_import_entries(path: &Path) -> anyhow::Result<Vec<Value>> {
    let file = fs::File::open(path)
        .with_context(|| format!("could not open Claude session {}", path.display()))?;
    Ok(serde_json::Deserializer::from_reader(BufReader::new(file))
        .into_iter::<Value>()
        .filter_map(Result::ok)
        .filter_map(compact_import_entry)
        .collect())
}

/// Read bounded metadata windows from a native transcript. Titles and the
/// first prompt live near the front, while custom renames are commonly near
/// the tail; catalog discovery must not deserialize hundreds of full, often
/// multi-megabyte conversations just to draw a picker row.
fn read_summary_entries(path: &Path) -> anyhow::Result<Vec<Value>> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("could not open Claude session {}", path.display()))?;
    let len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let mut windows = Vec::new();
    if len <= SUMMARY_EDGE_BYTES * 2 {
        file.read_to_end(&mut windows)?;
    } else {
        std::io::Read::by_ref(&mut file)
            .take(SUMMARY_EDGE_BYTES)
            .read_to_end(&mut windows)?;
        file.seek(SeekFrom::Start(len - SUMMARY_EDGE_BYTES))?;
        let mut tail = Vec::with_capacity(SUMMARY_EDGE_BYTES as usize);
        file.read_to_end(&mut tail)?;
        windows.push(b'\n');
        windows.extend(tail);
    }
    Ok(windows
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .collect())
}

fn modified_at(path: &Path) -> u64 {
    path.metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn entry_timestamp(entry: &Map<String, Value>) -> Option<u64> {
    let timestamp = entry.get("timestamp").and_then(Value::as_str)?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|timestamp| u64::try_from(timestamp.timestamp()).ok())
}

fn message_text(entry: &Map<String, Value>) -> Option<String> {
    let content = entry
        .get("message")
        .and_then(|message| message.get("content"))?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

fn title_from_prompt(prompt: &str) -> Option<String> {
    let mut title = prompt
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return None;
    }
    if title.chars().count() > 54 {
        title = format!("{}…", title.chars().take(53).collect::<String>());
    }
    Some(title)
}

fn session_summary_from_path(path: &Path) -> anyhow::Result<ProviderSessionSummary> {
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("Claude session path has no UTF-8 filename"))?;
    Uuid::parse_str(session_id).context("Claude session filename is not a UUID")?;
    let entries = read_summary_entries(path)?;
    let transcript = entries
        .iter()
        .filter_map(Value::as_object)
        .collect::<Vec<_>>();
    let cwd = transcript
        .iter()
        .copied()
        .find_map(|entry| entry.get("cwd").and_then(Value::as_str))
        .map(PathBuf::from)
        .filter(|cwd| cwd.is_absolute() && cwd.is_dir())
        .ok_or_else(|| anyhow!("Claude session {session_id} has no available working directory"))?;
    let first_prompt = transcript
        .iter()
        .find(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("user") && is_user_prompt(entry)
        })
        .and_then(|entry| message_text(entry));
    let custom_title = entries
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("custom-title"))
        .filter_map(|entry| entry.get("customTitle").and_then(Value::as_str))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .next_back();
    let ai_title = entries
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("ai-title"))
        .filter_map(|entry| entry.get("aiTitle").and_then(Value::as_str))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .next_back();
    let title = custom_title
        .or(ai_title)
        .map(str::to_owned)
        .or_else(|| first_prompt.as_deref().and_then(title_from_prompt))
        .ok_or_else(|| anyhow!("Claude session {session_id} has no user prompt"))?;
    let file_timestamp = modified_at(path);
    let created_at = transcript
        .iter()
        .copied()
        .filter_map(entry_timestamp)
        .min()
        .unwrap_or(file_timestamp);
    let updated_at = entries
        .iter()
        .filter_map(Value::as_object)
        .filter_map(entry_timestamp)
        .max()
        .unwrap_or_default()
        .max(file_timestamp)
        .max(created_at);

    Ok(ProviderSessionSummary {
        cursor: ProviderResumeCursor::Claude {
            session_id: session_id.to_owned(),
            resume_at: None,
        },
        title,
        cwd,
        created_at,
        updated_at,
    })
}

#[derive(Debug)]
struct IndexedClaudeSession {
    first_prompt: String,
    cwd: PathBuf,
    created_at: u64,
    updated_at: u64,
}

fn history_timestamp(value: &Value) -> u64 {
    let timestamp = value.as_u64().unwrap_or_default();
    // Claude's terminal history currently stores unix milliseconds while its
    // transcript entries use RFC 3339. Accept seconds too for older layouts.
    if timestamp > 10_000_000_000 {
        timestamp / 1_000
    } else {
        timestamp
    }
}

fn provider_session_files(projects_directory: &Path) -> anyhow::Result<Vec<(u64, PathBuf)>> {
    let mut candidates = Vec::new();
    for project in fs::read_dir(projects_directory).with_context(|| {
        format!(
            "could not read Claude's session directory at {}",
            projects_directory.display()
        )
    })? {
        let Ok(project) = project else {
            continue;
        };
        let Ok(file_type) = project.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
                || entry.metadata().is_ok_and(|metadata| metadata.len() == 0)
            {
                continue;
            }
            candidates.push((modified_at(&path), path));
        }
    }
    Ok(candidates)
}

fn list_provider_sessions_from_history(
    projects_directory: &Path,
    history_path: &Path,
    limit: usize,
) -> anyhow::Result<Vec<ProviderSessionSummary>> {
    let file = fs::File::open(history_path).with_context(|| {
        format!(
            "could not read Claude's terminal history at {}",
            history_path.display()
        )
    })?;
    let mut indexed = HashMap::<String, IndexedClaudeSession>::new();
    for value in BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
    {
        let Some(session_id) = value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| Uuid::parse_str(id).is_ok())
        else {
            continue;
        };
        let Some(prompt) = value
            .get("display")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        else {
            continue;
        };
        let Some(cwd) = value
            .get("project")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|cwd| cwd.is_absolute() && cwd.is_dir())
        else {
            continue;
        };
        let timestamp = history_timestamp(&value["timestamp"]);
        indexed
            .entry(session_id.to_owned())
            .and_modify(|session| {
                session.cwd = cwd.clone();
                if timestamp > 0 {
                    session.created_at = if session.created_at == 0 {
                        timestamp
                    } else {
                        session.created_at.min(timestamp)
                    };
                    session.updated_at = session.updated_at.max(timestamp);
                }
            })
            .or_insert_with(|| IndexedClaudeSession {
                first_prompt: prompt.to_owned(),
                cwd,
                created_at: timestamp,
                updated_at: timestamp,
            });
    }

    let files = provider_session_files(projects_directory)?
        .into_iter()
        .filter_map(|(modified_at, path)| {
            let session_id = path.file_stem()?.to_str()?.to_owned();
            Some((session_id, (modified_at, path)))
        })
        .collect::<HashMap<_, _>>();
    let mut sessions = indexed
        .into_iter()
        .filter_map(|(session_id, indexed)| {
            let (file_updated_at, path) = files.get(&session_id)?;
            let fallback_title = title_from_prompt(&indexed.first_prompt)?;
            let mut summary = session_summary_from_path(path).unwrap_or(ProviderSessionSummary {
                cursor: ProviderResumeCursor::Claude {
                    session_id: session_id.clone(),
                    resume_at: None,
                },
                title: fallback_title,
                cwd: indexed.cwd.clone(),
                created_at: indexed.created_at,
                updated_at: indexed.updated_at,
            });
            summary.cwd = indexed.cwd;
            if indexed.created_at > 0 {
                summary.created_at = summary.created_at.min(indexed.created_at);
            }
            summary.updated_at = summary
                .updated_at
                .max(indexed.updated_at)
                .max(*file_updated_at);
            Some(summary)
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions.truncate(limit);
    Ok(sessions)
}

fn list_provider_sessions_in(
    projects_directory: &Path,
    limit: usize,
) -> anyhow::Result<Vec<ProviderSessionSummary>> {
    if limit == 0 || !projects_directory.exists() {
        return Ok(Vec::new());
    }
    // `history.jsonl` is Claude's interactive CLI index. Headless `-p` and
    // Agent SDK runs still have transcripts under `projects/` but are absent
    // from this index, which naturally keeps Waku-created sessions out of the
    // terminal resume picker even after their Waku task has been deleted.
    let history_path = projects_directory
        .parent()
        .map(|directory| directory.join("history.jsonl"));
    if let Some(history_path) = history_path.filter(|path| path.is_file()) {
        return list_provider_sessions_from_history(projects_directory, &history_path, limit);
    }
    let mut candidates = provider_session_files(projects_directory)?;
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    let mut sessions = Vec::new();
    for (_, path) in candidates {
        if let Ok(summary) = session_summary_from_path(&path) {
            sessions.push(summary);
            if sessions.len() == limit {
                break;
            }
        }
    }
    Ok(sessions)
}

fn provider_session_history_in(
    projects_directory: &Path,
    session_id: &str,
    turn_limit: usize,
) -> anyhow::Result<ProviderSessionHistory> {
    let entries = read_import_entries(&find_session_file(projects_directory, session_id)?)?;
    let chain = active_chain(&entries);
    let mut history = ProviderSessionHistory::default();

    for entry in chain {
        let kind = entry.get("type").and_then(Value::as_str);
        let native_id = entry.get("uuid").and_then(Value::as_str);
        let timestamp = entry_timestamp(entry).unwrap_or_else(crate::model::unix_time);
        if kind == Some("user") && is_user_prompt(entry) {
            let turn_id = Uuid::new_v4();
            history.turns.push(AgentTurn {
                id: turn_id,
                turn_count: history.turns.len() + 1,
                status: TurnStatus::Completed,
                provider_turn_started: true,
                provider_resume_at: native_id.map(str::to_owned),
                started_at: timestamp,
                completed_at: Some(timestamp),
                checkpoint: None,
            });
            if let Some(content) = message_text(entry) {
                let mut message = Message::new_for_turn(MessageRole::User, content, turn_id);
                message.created_at = timestamp;
                history.messages.push(message);
            }
            continue;
        }

        let Some(turn) = history.turns.last_mut() else {
            continue;
        };
        if matches!(kind, Some("user" | "assistant")) {
            if let Some(native_id) = native_id {
                turn.provider_resume_at = Some(native_id.to_owned());
            }
            turn.completed_at = Some(turn.completed_at.unwrap_or(timestamp).max(timestamp));
        }
        if kind != Some("assistant") {
            continue;
        }
        let Some(content) = message_text(entry) else {
            continue;
        };
        if let Some(previous) = history.messages.last_mut().filter(|message| {
            message.turn_id == Some(turn.id) && message.role == MessageRole::Assistant
        }) {
            if !previous.content.is_empty() {
                previous.content.push_str("\n\n");
            }
            previous.content.push_str(&content);
            previous.created_at = previous.created_at.max(timestamp);
        } else {
            let mut message = Message::new_for_turn(MessageRole::Assistant, content, turn.id);
            message.created_at = timestamp;
            history.messages.push(message);
        }
    }

    if history.turns.len() > turn_limit {
        let retained = history
            .turns
            .iter()
            .rev()
            .take(turn_limit)
            .map(|turn| turn.id)
            .collect::<std::collections::HashSet<_>>();
        history
            .messages
            .retain(|message| message.turn_id.is_some_and(|id| retained.contains(&id)));
    }
    Ok(history)
}

fn transcript_entries(entries: &[Value]) -> Vec<&Map<String, Value>> {
    entries
        .iter()
        .filter_map(Value::as_object)
        .filter(|entry| {
            entry
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| TRANSCRIPT_TYPES.contains(&kind))
                && entry.get("uuid").and_then(Value::as_str).is_some()
                && entry.get("isSidechain").and_then(Value::as_bool) != Some(true)
        })
        .collect()
}

fn active_chain(entries: &[Value]) -> Vec<&Map<String, Value>> {
    let transcript = transcript_entries(entries);
    let by_uuid = transcript
        .iter()
        .filter_map(|entry| {
            entry
                .get("uuid")
                .and_then(Value::as_str)
                .map(|uuid| (uuid, *entry))
        })
        .collect::<HashMap<_, _>>();
    let Some(mut current) = transcript.last().copied() else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    loop {
        chain.push(current);
        let Some(parent) = current.get("parentUuid").and_then(Value::as_str) else {
            break;
        };
        let Some(next) = by_uuid.get(parent).copied() else {
            break;
        };
        current = next;
    }
    chain.reverse();
    chain
}

fn session_metadata_in(
    projects_directory: &Path,
    session_id: &str,
) -> anyhow::Result<ClaudeSessionMetadata> {
    let entries = read_entries(&find_session_file(projects_directory, session_id)?)?;
    let latest_message_id = active_chain(&entries).iter().rev().find_map(|entry| {
        matches!(
            entry.get("type").and_then(Value::as_str),
            Some("user" | "assistant")
        )
        .then(|| entry.get("uuid").and_then(Value::as_str).map(str::to_owned))
        .flatten()
    });
    let title = entries.iter().filter_map(claude_title).last();
    Ok(ClaudeSessionMetadata {
        latest_message_id,
        title,
    })
}

fn claude_title(entry: &Value) -> Option<String> {
    let field = match entry.get("type").and_then(Value::as_str) {
        Some("ai-title") => "aiTitle",
        Some("custom-title") => "customTitle",
        _ => return None,
    };
    entry
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

fn message_id_for_turn_in(
    projects_directory: &Path,
    session_id: &str,
    provider_turn_count: usize,
) -> anyhow::Result<String> {
    if provider_turn_count == 0 {
        bail!("Claude has no native turn at that checkpoint");
    }
    let entries = read_entries(&find_session_file(projects_directory, session_id)?)?;
    let mut turn_points = Vec::new();
    for entry in active_chain(&entries) {
        let kind = entry.get("type").and_then(Value::as_str);
        let uuid = entry
            .get("uuid")
            .and_then(Value::as_str)
            .expect("active transcript entries have UUIDs");
        if kind == Some("user") && is_user_prompt(entry) {
            turn_points.push(uuid.to_owned());
        } else if !turn_points.is_empty() && matches!(kind, Some("user" | "assistant")) {
            *turn_points.last_mut().expect("checked above") = uuid.to_owned();
        }
    }
    turn_points
        .get(provider_turn_count - 1)
        .cloned()
        .ok_or_else(|| anyhow!("Claude's message checkpoint for that turn was not found"))
}

fn is_user_prompt(entry: &Map<String, Value>) -> bool {
    if entry.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let Some(content) = entry
        .get("message")
        .and_then(|message| message.get("content"))
    else {
        return false;
    };
    match content {
        Value::String(_) => true,
        Value::Array(blocks) => {
            !blocks.is_empty()
                && !blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        }
        _ => false,
    }
}

fn fork_session_at_in(
    projects_directory: &Path,
    session_id: &str,
    up_to_message_id: &str,
    title: &str,
) -> anyhow::Result<ForkedClaudeSession> {
    Uuid::parse_str(session_id).context("Claude returned an invalid session ID")?;
    Uuid::parse_str(up_to_message_id).context("Claude returned an invalid message checkpoint")?;
    let source_path = find_session_file(projects_directory, session_id)?;
    let raw_entries = read_entries(&source_path)?;
    let mut replacements = Vec::new();
    for entry in &raw_entries {
        if entry.get("type").and_then(Value::as_str) == Some("content-replacement")
            && entry.get("sessionId").and_then(Value::as_str) == Some(session_id)
            && let Some(items) = entry.get("replacements").and_then(Value::as_array)
        {
            replacements.extend(items.iter().cloned());
        }
    }

    let mut transcript = transcript_entries(&raw_entries);
    let cutoff = transcript
        .iter()
        .position(|entry| entry.get("uuid").and_then(Value::as_str) == Some(up_to_message_id))
        .ok_or_else(|| {
            anyhow!("Claude message checkpoint {up_to_message_id} is missing from the session")
        })?;
    transcript.truncate(cutoff + 1);
    if transcript.is_empty() {
        bail!("Claude session {session_id} has no messages to rewind");
    }

    let message_ids = transcript
        .iter()
        .map(|entry| {
            (
                entry
                    .get("uuid")
                    .and_then(Value::as_str)
                    .expect("transcript entries have UUIDs")
                    .to_owned(),
                Uuid::new_v4().to_string(),
            )
        })
        .collect::<HashMap<_, _>>();
    let by_uuid = transcript
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            (
                entry
                    .get("uuid")
                    .and_then(Value::as_str)
                    .expect("transcript entries have UUIDs")
                    .to_owned(),
                index,
            )
        })
        .collect::<HashMap<_, _>>();
    let writable = transcript
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("progress"))
        .copied()
        .collect::<Vec<_>>();
    if writable.is_empty() {
        bail!("Claude session {session_id} has no messages to rewind");
    }

    let forked_session_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut output = Vec::new();
    for (index, original) in writable.iter().enumerate() {
        let old_uuid = original
            .get("uuid")
            .and_then(Value::as_str)
            .expect("transcript entries have UUIDs");
        let mut parent_id = original
            .get("parentUuid")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut new_parent = None;
        while let Some(parent) = parent_id {
            let Some(parent_index) = by_uuid.get(&parent).copied() else {
                break;
            };
            let parent_entry = transcript[parent_index];
            if parent_entry.get("type").and_then(Value::as_str) != Some("progress") {
                new_parent = message_ids.get(&parent).cloned();
                break;
            }
            parent_id = parent_entry
                .get("parentUuid")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }

        let mut forked = (*original).clone();
        forked.insert("uuid".into(), Value::String(message_ids[old_uuid].clone()));
        forked.insert(
            "parentUuid".into(),
            new_parent.map(Value::String).unwrap_or(Value::Null),
        );
        let logical_parent = original
            .get("logicalParentUuid")
            .and_then(Value::as_str)
            .and_then(|uuid| message_ids.get(uuid))
            .cloned();
        forked.insert(
            "logicalParentUuid".into(),
            logical_parent.map(Value::String).unwrap_or(Value::Null),
        );
        forked.insert("sessionId".into(), Value::String(forked_session_id.clone()));
        if index == writable.len() - 1 || !forked.contains_key("timestamp") {
            forked.insert("timestamp".into(), Value::String(now.clone()));
        }
        forked.insert("isSidechain".into(), Value::Bool(false));
        forked.insert(
            "forkedFrom".into(),
            json!({ "sessionId": session_id, "messageUuid": old_uuid }),
        );
        for key in ["teamName", "agentName", "slug", "sourceToolAssistantUUID"] {
            forked.remove(key);
        }
        output.push(Value::Object(forked));
    }

    if !replacements.is_empty() {
        output.push(json!({
            "type": "content-replacement",
            "sessionId": forked_session_id,
            "replacements": replacements,
            "uuid": Uuid::new_v4().to_string(),
            "timestamp": now,
        }));
    }
    output.push(json!({
        "type": "custom-title",
        "sessionId": forked_session_id,
        "customTitle": if title.trim().is_empty() { "Waku rewind" } else { title.trim() },
        "uuid": Uuid::new_v4().to_string(),
        "timestamp": now,
    }));

    let fork_path = source_path
        .parent()
        .expect("Claude session files have a parent directory")
        .join(format!("{forked_session_id}.jsonl"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&fork_path).with_context(|| {
        format!(
            "could not create Claude session fork {}",
            fork_path.display()
        )
    })?;
    for entry in output {
        serde_json::to_writer(&mut file, &entry)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;

    Ok(ForkedClaudeSession {
        session_id: forked_session_id,
        message_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "11111111-1111-4111-8111-111111111111";
    const USER_ONE: &str = "22222222-2222-4222-8222-222222222222";
    const PROGRESS: &str = "33333333-3333-4333-8333-333333333333";
    const ASSISTANT_ONE: &str = "44444444-4444-4444-8444-444444444444";
    const TOOL_RESULT: &str = "55555555-5555-4555-8555-555555555555";
    const ASSISTANT_TWO: &str = "66666666-6666-4666-8666-666666666666";
    const USER_TWO: &str = "77777777-7777-4777-8777-777777777777";
    const ASSISTANT_THREE: &str = "88888888-8888-4888-8888-888888888888";

    fn fixture() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("waku-claude-session-{}", Uuid::new_v4()));
        let project = root.join("projects").join("-tmp-project");
        let workspace = root.join("workspace");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let source = project.join(format!("{SESSION}.jsonl"));
        let mut history = fs::File::create(root.join("history.jsonl")).unwrap();
        serde_json::to_writer(
            &mut history,
            &json!({
                "display": "first",
                "project": workspace,
                "sessionId": SESSION,
                "timestamp": 1_767_225_600_000_u64
            }),
        )
        .unwrap();
        history.write_all(b"\n").unwrap();
        let entries = [
            json!({"type":"user","uuid":USER_ONE,"parentUuid":null,"sessionId":SESSION,"cwd":workspace,"timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"first"}}),
            json!({"type":"ai-title","aiTitle":"Generated first task title","sessionId":SESSION}),
            json!({"type":"progress","uuid":PROGRESS,"parentUuid":USER_ONE,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:01.000Z"}),
            json!({"type":"assistant","uuid":ASSISTANT_ONE,"parentUuid":PROGRESS,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"tool_use"}]}}),
            json!({"type":"user","uuid":TOOL_RESULT,"parentUuid":ASSISTANT_ONE,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:03.000Z","message":{"role":"user","content":[{"type":"tool_result"}]}}),
            json!({"type":"assistant","uuid":ASSISTANT_TWO,"parentUuid":TOOL_RESULT,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}),
            json!({"type":"assistant","uuid":"99999999-9999-4999-8999-999999999999","parentUuid":ASSISTANT_ONE,"sessionId":SESSION,"isSidechain":true,"message":{"role":"assistant","content":[]}}),
            json!({"type":"user","uuid":USER_TWO,"parentUuid":ASSISTANT_TWO,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:05.000Z","message":{"role":"user","content":"second"}}),
            json!({"type":"assistant","uuid":ASSISTANT_THREE,"parentUuid":USER_TWO,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:06.000Z","message":{"role":"assistant","content":[{"type":"text","text":"later"}]}}),
            json!({"type":"content-replacement","sessionId":SESSION,"replacements":[{"from":"a","to":"b"}]}),
        ];
        let mut file = fs::File::create(&source).unwrap();
        for entry in entries {
            serde_json::to_writer(&mut file, &entry).unwrap();
            file.write_all(b"\n").unwrap();
        }
        (root, source)
    }

    #[test]
    fn finds_native_message_points_for_completed_turns() {
        let (root, _) = fixture();
        let projects = root.join("projects");
        let metadata = session_metadata_in(&projects, SESSION).unwrap();
        assert_eq!(metadata.latest_message_id.as_deref(), Some(ASSISTANT_THREE));
        assert_eq!(
            metadata.title.as_deref(),
            Some("Generated first task title")
        );
        assert_eq!(
            message_id_for_turn_in(&projects, SESSION, 1).unwrap(),
            ASSISTANT_TWO
        );
        assert_eq!(
            message_id_for_turn_in(&projects, SESSION, 2).unwrap(),
            ASSISTANT_THREE
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn latest_native_title_wins() {
        let (root, source) = fixture();
        let mut file = OpenOptions::new().append(true).open(source).unwrap();
        serde_json::to_writer(
            &mut file,
            &json!({"type":"custom-title","customTitle":"Renamed provider task"}),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();

        let metadata = session_metadata_in(&root.join("projects"), SESSION).unwrap();
        assert_eq!(metadata.title.as_deref(), Some("Renamed provider task"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lists_resumable_sessions_and_imports_recent_visible_turns() {
        let (root, source) = fixture();
        let projects = root.join("projects");
        let headless = source
            .parent()
            .unwrap()
            .join("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.jsonl");
        serde_json::to_writer(
            fs::File::create(headless).unwrap(),
            &json!({
                "type": "user",
                "uuid": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "parentUuid": null,
                "sessionId": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "cwd": root.join("workspace"),
                "message": {"role": "user", "content": "not from terminal history"}
            }),
        )
        .unwrap();

        let sessions = list_provider_sessions_in(&projects, 10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "Generated first task title");
        assert_eq!(sessions[0].cwd, root.join("workspace"));
        assert_eq!(sessions[0].cursor.native_id(), SESSION);

        let history = provider_session_history_in(&projects, SESSION, 1).unwrap();
        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.turns[1].turn_count, 2);
        assert_eq!(history.messages.len(), 2);
        assert_eq!(history.messages[0].content, "second");
        assert_eq!(history.messages[1].content, "later");
        assert_eq!(
            history.turns[1].provider_resume_at.as_deref(),
            Some(ASSISTANT_THREE)
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn import_compaction_drops_large_native_payloads_without_losing_turn_shape() {
        let tool_result = compact_import_entry(json!({
            "type": "user",
            "uuid": TOOL_RESULT,
            "parentUuid": ASSISTANT_ONE,
            "message": {"content": [{
                "type": "tool_result",
                "content": "x".repeat(100_000)
            }]}
        }))
        .unwrap();
        let tool_result = tool_result.as_object().unwrap();
        assert!(!is_user_prompt(tool_result));
        assert!(serde_json::to_vec(tool_result).unwrap().len() < 256);

        let image_prompt = compact_import_entry(json!({
            "type": "user",
            "uuid": USER_TWO,
            "parentUuid": ASSISTANT_TWO,
            "message": {"content": [{
                "type": "image",
                "source": {"data": "x".repeat(100_000)}
            }]}
        }))
        .unwrap();
        let image_prompt = image_prompt.as_object().unwrap();
        assert!(is_user_prompt(image_prompt));
        assert_eq!(message_text(image_prompt), None);
        assert!(serde_json::to_vec(image_prompt).unwrap().len() < 256);
    }

    #[test]
    fn forks_through_message_and_remaps_the_parent_chain() {
        let (root, source) = fixture();
        let projects = root.join("projects");
        let fork = fork_session_at_in(&projects, SESSION, ASSISTANT_TWO, "Rewound task")
            .expect("fork succeeds");
        let fork_path = source
            .parent()
            .unwrap()
            .join(format!("{}.jsonl", fork.session_id));
        let entries = read_entries(&fork_path).unwrap();
        let transcript = transcript_entries(&entries);

        assert_eq!(transcript.len(), 4);
        assert!(
            transcript
                .iter()
                .all(|entry| entry.get("sessionId").and_then(Value::as_str)
                    == Some(fork.session_id.as_str()))
        );
        assert!(
            transcript
                .iter()
                .all(|entry| entry.get("type").and_then(Value::as_str) != Some("progress"))
        );
        let first_assistant = transcript
            .iter()
            .find(|entry| {
                entry
                    .get("forkedFrom")
                    .and_then(|forked_from| forked_from.get("messageUuid"))
                    .and_then(Value::as_str)
                    == Some(ASSISTANT_ONE)
            })
            .unwrap();
        assert_eq!(
            first_assistant.get("parentUuid").and_then(Value::as_str),
            fork.message_ids.get(USER_ONE).map(String::as_str)
        );
        assert!(entries.iter().any(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("content-replacement")
        }));
        assert!(entries.iter().any(|entry| {
            entry.get("customTitle").and_then(Value::as_str) == Some("Rewound task")
        }));
        assert!(
            !fs::read_to_string(source)
                .unwrap()
                .contains(&fork.session_id)
        );
        fs::remove_dir_all(root).ok();
    }
}

//! Claude native transcript and fork helpers.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

const TRANSCRIPT_TYPES: [&str; 5] = ["user", "assistant", "attachment", "system", "progress"];

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
        fs::create_dir_all(&project).unwrap();
        let source = project.join(format!("{SESSION}.jsonl"));
        let entries = [
            json!({"type":"user","uuid":USER_ONE,"parentUuid":null,"sessionId":SESSION,"timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"first"}}),
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

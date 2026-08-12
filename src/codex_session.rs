//! Converts Codex CLI rollout files into records a Waku session can be built
//! from. Wired to `/resume` in a follow-up, so until then only its tests call
//! in.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use serde_json::Value;
use uuid::Uuid;

use crate::driver::codex::CodexStreamState;
use crate::model::{ActivityItem, ActivityKind, ReasoningBlock};

/// One piece of an imported conversation, in transcript order. Not
/// [`crate::model::Message`]: assembling a transcript needs the app's own
/// accumulators, so this carries only what a record said.
pub enum ImportedRecord {
    /// A prompt the person typed, which opens a turn.
    Prompt(String),
    Assistant(String),
    Activity(ActivityItem),
}

/// Reads `session_id`'s rollout as records a Waku session can be built from.
/// Blocking, so background executor only. A `compacted` record replaces the
/// history before it, which is how the CLI itself rebuilds context, so a
/// compacted session imports as it looks now. Tool calls and their outputs
/// are separate records, so outputs are indexed first and folded into the
/// call they answer, which is how the live stream presents them.
pub fn imported_transcript(session_id: &str) -> anyhow::Result<Vec<ImportedRecord>> {
    imported_transcript_in(&sessions_directory()?, session_id)
}

pub fn imported_transcript_in(
    sessions_directory: &Path,
    session_id: &str,
) -> anyhow::Result<Vec<ImportedRecord>> {
    let entries = read_entries(&find_session_file(sessions_directory, session_id)?)?;
    let items = effective_items(&entries);
    let outputs = items
        .iter()
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call_output" | "custom_tool_call_output")
            )
        })
        .filter_map(|item| {
            item.get("call_id")
                .and_then(Value::as_str)
                .map(|id| (id, *item))
        })
        .collect::<HashMap<_, _>>();

    // The stream's own citation rewriter, fed whole recorded messages: a
    // prompt opens the turn, a search's results define the references, and a
    // reply renders them as links.
    let mut citations = CodexStreamState::default();
    let mut imported = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => match item.get("role").and_then(Value::as_str) {
                Some("user") => {
                    if let Some(prompt) = prompt_text(item) {
                        citations.begin_turn();
                        imported.push(ImportedRecord::Prompt(prompt));
                    }
                }
                Some("assistant") => imported.extend(reply_text(item).map(|text| {
                    ImportedRecord::Assistant(citations.rewrite_citation_delta(&text))
                })),
                _ => {}
            },
            Some("reasoning") => imported.extend(reasoning_record(item)),
            Some("function_call") | Some("custom_tool_call") => {
                imported.extend(tool_record(item, &outputs));
            }
            Some("web_search_call") => {
                citations.capture_citations(item);
                imported.push(search_record(item));
            }
            _ => {}
        }
    }
    Ok(imported)
}

fn sessions_directory() -> anyhow::Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .map(|home| home.join("sessions"))
        .ok_or_else(|| anyhow!("Codex's home directory could not be located"))
}

/// Rollouts live at `sessions/YYYY/MM/DD/rollout-<started>-<session_id>.jsonl`,
/// so a session is found by walking the date tree for its filename suffix.
fn find_session_file(sessions_directory: &Path, session_id: &str) -> anyhow::Result<PathBuf> {
    Uuid::parse_str(session_id).context("Codex returned an invalid session ID")?;
    find_rollout(sessions_directory, &format!("-{session_id}.jsonl"), 3)
        .ok_or_else(|| anyhow!("Codex session {session_id} was not found on disk"))
}

/// Newest date directories first, since a resumed session is almost always
/// recent, and exactly three levels down, so a miss never scans past the
/// date tree.
fn find_rollout(directory: &Path, suffix: &str, depth: u8) -> Option<PathBuf> {
    let mut paths = fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.into_iter().rev().find_map(|path| {
        if depth > 0 {
            find_rollout(&path, suffix, depth - 1)
        } else {
            (path.to_str().is_some_and(|path| path.ends_with(suffix))
                && path.metadata().is_ok_and(|metadata| metadata.len() > 0))
            .then_some(path)
        }
    })
}

fn read_entries(path: &Path) -> anyhow::Result<Vec<Value>> {
    let file = fs::File::open(path)
        .with_context(|| format!("could not open Codex session {}", path.display()))?;
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .collect())
}

/// The response items a resumed session would replay: rollout order, with a
/// `compacted` record's replacement history standing in for everything
/// recorded before it.
fn effective_items(entries: &[Value]) -> Vec<&Value> {
    let mut items = Vec::new();
    for entry in entries {
        match entry.get("type").and_then(Value::as_str) {
            Some("response_item") => items.extend(entry.get("payload")),
            Some("compacted") => {
                items = entry
                    .pointer("/payload/replacement_history")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .collect();
            }
            _ => {}
        }
    }
    items
}

/// The typed text of a prompt item. Codex replays instructions, environment
/// context, IDE state, and resumed history through the same user role, each
/// in its own block, so blocks are filtered one by one and only the person's
/// words survive.
fn prompt_text(item: &Value) -> Option<String> {
    let text = text_blocks(item, "input_text")
        .filter_map(typed_text)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn typed_text(block: &str) -> Option<String> {
    let text = block.trim();
    // The VS Code extension wraps the typed request inside its IDE context.
    if text.starts_with("# Context from my IDE setup") {
        return text
            .split_once("## My request for Codex:")
            .map(|(_, request)| request.trim())
            .filter(|request| !request.is_empty())
            .map(str::to_owned);
    }
    (!text.is_empty()
        && !text.starts_with('<')
        && !text.starts_with("# AGENTS.md instructions")
        && !text.starts_with("The following is the Codex agent history"))
    .then(|| text.to_owned())
}

fn reply_text(item: &Value) -> Option<String> {
    let text = text_blocks(item, "output_text")
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn text_blocks<'a>(item: &'a Value, block_type: &'a str) -> impl Iterator<Item = &'a str> {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(move |block| block.get("type").and_then(Value::as_str) == Some(block_type))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
}

/// Codex encrypts its reasoning and records only the display summary, so the
/// summary is what imports, and an encrypted-only record has nothing to show.
fn reasoning_record(item: &Value) -> Option<ImportedRecord> {
    let text = ["summary", "content"]
        .iter()
        .map(|key| {
            item.get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .find(|text| !text.is_empty())?;
    Some(ImportedRecord::Activity(ActivityItem::from_reasoning(
        ReasoningBlock {
            content: text,
            started_at_ms: 0,
            finished_at_ms: 0,
        },
        true,
    )))
}

/// A recorded tool call as one finished activity row, its output folded in
/// from the `*_output` item answering its `call_id`, which is how the live
/// stream presents a completed call.
fn tool_record(item: &Value, outputs: &HashMap<&str, &Value>) -> Option<ImportedRecord> {
    let name = item.get("name").and_then(Value::as_str)?;
    let arguments = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .and_then(Value::as_str)
        .map(|raw| serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned())));
    let call_id = item.get("call_id").and_then(Value::as_str);
    let output = call_id
        .and_then(|id| outputs.get(id))
        .and_then(|item| output_value(item));
    let command = arguments.as_ref().and_then(command_text);
    let kind = if command.is_some() {
        ActivityKind::Command
    } else {
        ActivityKind::from_tool_name(name)
    };
    let title = command
        .or_else(|| {
            arguments
                .as_ref()
                .and_then(|arguments| arguments.get("title"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| name.to_owned());
    Some(ImportedRecord::Activity(
        crate::driver::activity::tool_activity(
            call_id.map(str::to_owned),
            kind,
            title,
            arguments.as_ref(),
            output.as_ref(),
            None,
            output.as_ref().is_some_and(output_failed),
            true,
        ),
    ))
}

/// The command a shell-like call runs, which titles its row the way the live
/// stream titles a command execution.
fn command_text(arguments: &Value) -> Option<String> {
    match arguments.get("cmd").or_else(|| arguments.get("command"))? {
        Value::String(command) => Some(command.clone()),
        Value::Array(words) => Some(
            words
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// A shell call's output string is itself JSON carrying the text and exit
/// metadata, while a custom tool's output arrives as text blocks.
fn output_value(item: &Value) -> Option<Value> {
    match item.get("output")? {
        Value::String(output) => {
            Some(serde_json::from_str(output).unwrap_or_else(|_| Value::String(output.clone())))
        }
        Value::Array(blocks) => Some(Value::String(
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .concat(),
        )),
        other => Some(other.clone()),
    }
}

fn output_failed(output: &Value) -> bool {
    output
        .pointer("/metadata/exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
}

fn search_record(item: &Value) -> ImportedRecord {
    ImportedRecord::Activity(crate::driver::activity::tool_activity(
        item.get("id").and_then(Value::as_str).map(str::to_owned),
        ActivityKind::Search,
        crate::driver::codex::codex_web_search_title(item),
        item.get("action"),
        item.get("results"),
        None,
        false,
        true,
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;

    const SESSION: &str = "019a0000-0000-7000-8000-000000000001";

    fn write_session(entries: &[Value]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("waku-codex-session-{}", Uuid::new_v4()));
        let day = root.join("2026").join("08").join("01");
        fs::create_dir_all(&day).unwrap();
        let mut file =
            fs::File::create(day.join(format!("rollout-2026-08-01T00-00-00-{SESSION}.jsonl")))
                .unwrap();
        for entry in entries {
            serde_json::to_writer(&mut file, entry).unwrap();
            file.write_all(b"\n").unwrap();
        }
        root
    }

    fn response(payload: Value) -> Value {
        json!({"timestamp": "2026-08-01T00:00:00.000Z", "type": "response_item", "payload": payload})
    }

    #[test]
    fn imports_typed_prompts_reasoning_tool_calls_and_replies_in_order() {
        let root = write_session(&[
            json!({"type": "session_meta", "payload": {"id": SESSION, "cwd": "/tmp"}}),
            response(json!({"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "# AGENTS.md instructions for /tmp\nnever"},
                {"type": "input_text", "text": "<environment_context>shell: zsh</environment_context>"},
                {"type": "input_text", "text": "fix the parser"},
            ]})),
            response(json!({"type": "message", "role": "developer", "content": [
                {"type": "input_text", "text": "guidance"},
            ]})),
            response(json!({"type": "reasoning", "summary": [], "encrypted_content": "opaque"})),
            response(json!({"type": "reasoning", "summary": [
                {"type": "summary_text", "text": "weigh the options"},
            ]})),
            response(json!({"type": "function_call", "name": "exec_command",
                "arguments": "{\"cmd\":\"ls\"}", "call_id": "call_1"})),
            response(json!({"type": "function_call_output", "call_id": "call_1",
                "output": "{\"output\":\"main.rs\\nlib.rs\",\"metadata\":{\"exit_code\":0}}"})),
            response(json!({"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "done"},
            ]})),
        ]);

        let imported = imported_transcript_in(&root, SESSION).unwrap();
        assert_eq!(imported.len(), 4);
        let ImportedRecord::Prompt(prompt) = &imported[0] else {
            panic!("the typed block imports as the prompt");
        };
        assert_eq!(prompt, "fix the parser");
        let ImportedRecord::Activity(reasoning) = &imported[1] else {
            panic!("the summarized reasoning imports as an activity");
        };
        assert_eq!(
            reasoning
                .reasoning
                .as_ref()
                .map(|block| block.content.as_str()),
            Some("weigh the options")
        );
        let ImportedRecord::Activity(command) = &imported[2] else {
            panic!("the tool call imports as an activity");
        };
        assert_eq!(command.kind, ActivityKind::Command);
        assert_eq!(command.title, "ls");
        assert!(command.complete && !command.failed);
        assert!(
            command
                .output
                .as_deref()
                .is_some_and(|output| output.contains("main.rs"))
        );
        let ImportedRecord::Assistant(reply) = &imported[3] else {
            panic!("the assistant message imports as a reply");
        };
        assert_eq!(reply, "done");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_nonzero_exit_code_marks_the_command_failed() {
        let root = write_session(&[
            response(json!({"type": "function_call", "name": "exec_command",
                "arguments": "{\"cmd\":\"cargo test\"}", "call_id": "call_1"})),
            response(json!({"type": "function_call_output", "call_id": "call_1",
                "output": "{\"output\":\"error[E0308]\",\"metadata\":{\"exit_code\":101}}"})),
        ]);
        let imported = imported_transcript_in(&root, SESSION).unwrap();
        let ImportedRecord::Activity(command) = &imported[0] else {
            panic!("the tool call imports as an activity");
        };
        assert!(command.failed);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_ide_wrapper_keeps_only_the_request_it_carries() {
        let root = write_session(&[response(json!({"type": "message", "role": "user",
            "content": [{"type": "input_text",
                "text": "# Context from my IDE setup:\n\n## Active file: a.rs\n\n## My request for Codex:\nwhat is this?\n"}]}))]);
        let imported = imported_transcript_in(&root, SESSION).unwrap();
        let ImportedRecord::Prompt(prompt) = &imported[0] else {
            panic!("the wrapped request imports as the prompt");
        };
        assert_eq!(prompt, "what is this?");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_compacted_record_replaces_the_history_before_it() {
        let root = write_session(&[
            response(json!({"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "dropped"},
            ]})),
            json!({"type": "compacted", "payload": {"message": "", "replacement_history": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "kept"},
                ]},
            ]}}),
            response(json!({"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "after"},
            ]})),
        ]);
        let imported = imported_transcript_in(&root, SESSION).unwrap();
        assert_eq!(imported.len(), 2);
        let ImportedRecord::Prompt(prompt) = &imported[0] else {
            panic!("the replacement history opens the transcript");
        };
        assert_eq!(prompt, "kept");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_recorded_page_open_titles_by_its_url() {
        // Rollouts keep the API's snake_case action types, unlike the live
        // stream's camelCase items.
        let root = write_session(&[response(json!({"type": "web_search_call", "id": "ws_1",
            "action": {"type": "open_page", "url": "https://zed.dev/docs"}}))]);
        let imported = imported_transcript_in(&root, SESSION).unwrap();
        let ImportedRecord::Activity(open) = &imported[0] else {
            panic!("the page open imports as an activity");
        };
        assert_eq!(open.kind, ActivityKind::Search);
        assert!(open.title.contains("https://zed.dev/docs"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn citation_markers_become_links_and_never_reach_the_transcript() {
        let root = write_session(&[
            response(
                json!({"type": "web_search_call", "id": "ws_1", "status": "completed",
                "action": {"type": "search", "query": "gpui lists"},
                "results": [{"ref_id": "turn0search0", "url": "https://zed.dev/(docs)"}]}),
            ),
            response(json!({"type": "message", "role": "assistant", "content": [
                {"type": "output_text",
                 "text": "Use list().\u{e200}cite\u{e202}turn0search0\u{e201} Done."},
            ]})),
        ]);
        let imported = imported_transcript_in(&root, SESSION).unwrap();
        let ImportedRecord::Activity(search) = &imported[0] else {
            panic!("the web search imports as an activity");
        };
        assert_eq!(search.kind, ActivityKind::Search);
        let ImportedRecord::Assistant(reply) = &imported[1] else {
            panic!("the assistant message imports as a reply");
        };
        assert_eq!(reply, "Use list(). [1](https://zed.dev/\\(docs\\)) Done.");
        assert!(!reply.contains('\u{e200}'));
        fs::remove_dir_all(root).ok();
    }
}

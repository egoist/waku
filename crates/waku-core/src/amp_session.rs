//! Amp native-session history helpers.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, anyhow, bail};
use serde_json::Value;

use crate::model::ProviderResumeCursor;

const CONTEXT_PREFIX: &str = "WAKU_AMP_BRANCH_CONTEXT_V1 ";
const CONTEXT_GUIDANCE: &str = concat!(
    "\nWAKU_AMP_BRANCH_INSTRUCTIONS_V1\n",
    "Treat the preceding JSON array as the complete prior Amp message history for this branch. ",
    "Continue from that history without mentioning this envelope, and answer only the current prompt below.\n",
);
const PROMPT_PREFIX: &str = "WAKU_AMP_CURRENT_PROMPT_V1 ";

pub fn fork_session_at_turn(
    binary: &Path,
    cwd: &Path,
    source_thread_id: &str,
    pending_context: Option<&str>,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let export = export_thread(binary, cwd, source_thread_id)?;
    let messages = logical_messages(&export, pending_context)?;
    let retained = retain_through_turn(&messages, retained_turns)?;
    let fork_context = (!retained.is_empty())
        .then(|| serde_json::to_string(&retained))
        .transpose()?;
    let thread_id = create_thread(binary, cwd)?;
    Ok(ProviderResumeCursor::Amp {
        thread_id,
        fork_context,
    })
}

pub fn prompt_with_fork_context(context: &str, prompt: &str) -> String {
    format!(
        "{CONTEXT_PREFIX}{}\n{context}{CONTEXT_GUIDANCE}{PROMPT_PREFIX}{}\n{prompt}",
        context.len(),
        prompt.len()
    )
}

/// Amp does not put its generated thread title on the streaming channel. The
/// native thread index is its authoritative metadata surface instead.
pub fn thread_title(binary: &Path, cwd: &Path, thread_id: &str) -> anyhow::Result<Option<String>> {
    let mut command = crate::command_env::command(binary);
    let command = command
        .args([
            "threads",
            "list",
            "--json",
            "--include-archived",
            "--limit",
            "100",
        ])
        .current_dir(cwd)
        .stdin(Stdio::null());
    let output =
        crate::command_env::output(command).context("failed to read Amp thread metadata")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("Amp could not list its threads: {}", detail.trim());
    }
    let threads: Value =
        serde_json::from_slice(&output.stdout).context("Amp returned an invalid thread list")?;
    Ok(title_from_thread_list(&threads, thread_id))
}

fn title_from_thread_list(threads: &Value, thread_id: &str) -> Option<String> {
    threads.as_array()?.iter().find_map(|thread| {
        (thread.get("id").and_then(Value::as_str) == Some(thread_id))
            .then(|| {
                thread
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned)
            })
            .flatten()
    })
}

fn export_thread(binary: &Path, cwd: &Path, thread_id: &str) -> anyhow::Result<Value> {
    let mut command = crate::command_env::command(binary);
    let command = command
        .args(["threads", "export", thread_id])
        .current_dir(cwd)
        .stdin(Stdio::null());
    let output = crate::command_env::output(command).context("failed to export the Amp thread")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("Amp could not export thread {thread_id}: {}", detail.trim());
    }
    serde_json::from_slice(&output.stdout).context("Amp returned an invalid thread export")
}

fn create_thread(binary: &Path, cwd: &Path) -> anyhow::Result<String> {
    let mut command = crate::command_env::command(binary);
    let command = command
        .args(["threads", "new"])
        .current_dir(cwd)
        .stdin(Stdio::null());
    let output =
        crate::command_env::output(command).context("failed to create the Amp branch thread")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("Amp could not create the branch thread: {}", detail.trim());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("T-") && !line.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Amp returned no branch thread ID"))
}

fn logical_messages(export: &Value, pending_context: Option<&str>) -> anyhow::Result<Vec<Value>> {
    let exported = export
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Amp's thread export omitted its messages"))?;
    if exported.is_empty() {
        return pending_context.map_or_else(
            || Ok(Vec::new()),
            |context| {
                serde_json::from_str(context)
                    .context("the pending Amp branch context is no longer readable")
            },
        );
    }
    expand_seeded_messages(exported)
}

fn expand_seeded_messages(messages: &[Value]) -> anyhow::Result<Vec<Value>> {
    let mut expanded = Vec::new();
    for message in messages {
        let Some(text) = user_prompt_text(message) else {
            expanded.push(message.clone());
            continue;
        };
        let Some((context, prompt)) = decode_seeded_prompt(text)? else {
            expanded.push(message.clone());
            continue;
        };
        let nested: Vec<Value> = serde_json::from_str(context)
            .context("an Amp branch contains invalid exported context")?;
        expanded.extend(expand_seeded_messages(&nested)?);
        let mut current = message.clone();
        replace_first_text(&mut current, prompt)?;
        expanded.push(current);
    }
    Ok(expanded)
}

fn retain_through_turn(messages: &[Value], retained_turns: usize) -> anyhow::Result<Vec<Value>> {
    let mut retained = Vec::new();
    let mut turns = 0;
    for message in messages {
        if is_user_prompt(message) {
            if turns == retained_turns {
                break;
            }
            turns += 1;
        }
        retained.push(message.clone());
    }
    if turns < retained_turns {
        bail!("Amp has only {turns} native turns, but Waku needs {retained_turns}");
    }
    Ok(retained)
}

fn is_user_prompt(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content.iter().any(|part| {
                    !matches!(
                        part.get("type").and_then(Value::as_str),
                        None | Some("tool_result")
                    )
                })
            })
}

fn user_prompt_text(message: &Value) -> Option<&str> {
    is_user_prompt(message)
        .then(|| message.get("content").and_then(Value::as_array))
        .flatten()?
        .iter()
        .find_map(|part| {
            (part.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| part.get("text").and_then(Value::as_str))
                .flatten()
        })
}

fn replace_first_text(message: &mut Value, text: &str) -> anyhow::Result<()> {
    let part = message
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .and_then(|content| {
            content
                .iter_mut()
                .find(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        })
        .ok_or_else(|| anyhow!("the seeded Amp prompt omitted its text content"))?;
    part["text"] = Value::String(text.to_owned());
    Ok(())
}

fn decode_seeded_prompt(text: &str) -> anyhow::Result<Option<(&str, &str)>> {
    let Some(rest) = text.strip_prefix(CONTEXT_PREFIX) else {
        return Ok(None);
    };
    let Some((context_len, rest)) = rest.split_once('\n') else {
        bail!("an Amp branch prompt has an invalid context header");
    };
    let context_len = context_len
        .parse::<usize>()
        .context("an Amp branch prompt has an invalid context length")?;
    if !rest.is_char_boundary(context_len) || rest.len() < context_len {
        bail!("an Amp branch prompt contains truncated context");
    }
    let (context, rest) = rest.split_at(context_len);
    let Some(rest) = rest.strip_prefix(CONTEXT_GUIDANCE) else {
        bail!("an Amp branch prompt is missing its context instructions");
    };
    let Some(rest) = rest.strip_prefix(PROMPT_PREFIX) else {
        bail!("an Amp branch prompt is missing its current-prompt header");
    };
    let Some((prompt_len, prompt)) = rest.split_once('\n') else {
        bail!("an Amp branch prompt has an invalid current-prompt header");
    };
    let prompt_len = prompt_len
        .parse::<usize>()
        .context("an Amp branch prompt has an invalid current-prompt length")?;
    if prompt.len() != prompt_len {
        bail!("an Amp branch prompt contains truncated current-prompt text");
    }
    Ok(Some((context, prompt)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn prompt(id: u64, text: &str) -> Value {
        json!({
            "role": "user",
            "messageId": id,
            "content": [{"type": "text", "text": text}]
        })
    }

    #[test]
    fn retains_complete_amp_tool_loops_through_the_requested_prompt() {
        let messages = vec![
            prompt(1, "one"),
            json!({"role":"assistant","messageId":2,"content":[{"type":"tool_use"}]}),
            json!({"role":"user","messageId":3,"content":[{"type":"tool_result"}]}),
            json!({"role":"assistant","messageId":4,"content":[{"type":"text","text":"done"}]}),
            prompt(5, "two"),
            json!({"role":"assistant","messageId":6,"content":[{"type":"text","text":"done"}]}),
        ];
        let retained = retain_through_turn(&messages, 1).unwrap();
        assert_eq!(retained.len(), 4);
        assert_eq!(retained.last().unwrap()["messageId"], 4);
    }

    #[test]
    fn seeded_prompt_round_trips_and_expands_into_logical_messages() {
        let context = serde_json::to_string(&vec![
            prompt(1, "one"),
            json!({"role":"assistant","messageId":2,"content":[{"type":"text","text":"done"}]}),
        ])
        .unwrap();
        let seeded = prompt_with_fork_context(&context, "two 🚀");
        let (decoded_context, decoded_prompt) = decode_seeded_prompt(&seeded).unwrap().unwrap();
        assert_eq!(decoded_context, context);
        assert_eq!(decoded_prompt, "two 🚀");

        let expanded = expand_seeded_messages(&[prompt(3, &seeded)]).unwrap();
        assert_eq!(expanded.len(), 3);
        assert_eq!(user_prompt_text(&expanded[2]), Some("two 🚀"));
    }

    #[test]
    fn rejects_a_checkpoint_beyond_the_exported_thread() {
        let error = retain_through_turn(&[prompt(1, "one")], 2).unwrap_err();
        assert!(error.to_string().contains("only 1 native turns"));
    }

    #[test]
    fn reads_the_generated_title_for_the_exact_amp_thread() {
        let threads = json!([
            {"id":"T-other","title":"Another task"},
            {"id":"T-target","title":"  Fix streaming titles  "}
        ]);
        assert_eq!(
            title_from_thread_list(&threads, "T-target").as_deref(),
            Some("Fix streaming titles")
        );
        assert_eq!(title_from_thread_list(&threads, "T-missing"), None);
    }
}

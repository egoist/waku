//! Cursor conversation fork helpers.

use anyhow::bail;
use serde::Serialize;

use crate::model::{AgentSession, MessageRole, ProviderResumeCursor};

const CONTEXT_PREFIX: &str = "WAKU_CURSOR_BRANCH_CONTEXT_V1 ";
const CONTEXT_GUIDANCE: &str = concat!(
    "\nWAKU_CURSOR_BRANCH_INSTRUCTIONS_V1\n",
    "Treat the preceding JSON array as the complete prior user and assistant conversation for this branch. ",
    "The workspace already matches that point in the conversation. Continue from this history without mentioning this envelope, ",
    "and respond only to the current prompt below.\n",
);
const PROMPT_PREFIX: &str = "WAKU_CURSOR_CURRENT_PROMPT_V1 ";

#[derive(Serialize)]
struct ContextMessage<'a> {
    role: &'static str,
    content: &'a str,
}

/// Cursor CLI can resume only the tip of a session and currently has no fork
/// command. Preserve branch semantics by starting a fresh Cursor session whose
/// first prompt is seeded with Waku's retained visible conversation.
pub fn fork_session_at_turn(
    session: &AgentSession,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    if retained_turns > session.turns.len() {
        bail!(
            "Cursor branch requested {retained_turns} turns from a {}-turn task",
            session.turns.len()
        );
    }
    let retained_ids = session
        .turns
        .iter()
        .take(retained_turns)
        .map(|turn| turn.id)
        .collect::<std::collections::HashSet<_>>();
    let messages = session
        .messages
        .iter()
        .filter(|message| {
            message
                .turn_id
                .is_some_and(|turn_id| retained_ids.contains(&turn_id))
                && !message.content.trim().is_empty()
        })
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => return None,
            };
            Some(ContextMessage {
                role,
                content: &message.content,
            })
        })
        .collect::<Vec<_>>();
    let fork_context = (!messages.is_empty())
        .then(|| serde_json::to_string(&messages))
        .transpose()?;
    Ok(ProviderResumeCursor::Cursor {
        // Cursor assigns the native ID when the seeded prompt starts.
        session_id: String::new(),
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::model::{AgentSession, MessageRole, Project, ProviderKind, TurnStatus};

    use super::*;

    #[test]
    fn cursor_branch_context_keeps_only_the_selected_turn_prefix() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Cursor);
        session.begin_turn("first prompt");
        session.mark_active_turn_provider_started();
        session.push_message(MessageRole::Assistant, "first answer");
        session.finish_active_turn(TurnStatus::Completed);
        session.begin_turn("second prompt");
        session.mark_active_turn_provider_started();
        session.push_message(MessageRole::Assistant, "second answer");
        session.finish_active_turn(TurnStatus::Completed);

        let ProviderResumeCursor::Cursor {
            session_id,
            fork_context: Some(context),
        } = fork_session_at_turn(&session, 1).unwrap()
        else {
            panic!("expected a managed Cursor branch cursor");
        };
        assert!(session_id.is_empty());
        assert!(context.contains("first prompt"));
        assert!(context.contains("first answer"));
        assert!(!context.contains("second prompt"));
    }

    #[test]
    fn cursor_seed_envelope_is_length_delimited() {
        let prompt = prompt_with_fork_context("[{\"role\":\"user\"}]", "continue 🚀");
        assert!(prompt.starts_with("WAKU_CURSOR_BRANCH_CONTEXT_V1 17\n"));
        assert!(prompt.ends_with("WAKU_CURSOR_CURRENT_PROMPT_V1 13\ncontinue 🚀"));
    }
}

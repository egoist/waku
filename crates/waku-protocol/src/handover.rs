//! Pure provider-session handover planning and context transfer.
//!
//! A handover creates a new Waku task on the source task's workspace. Settled
//! user and assistant messages remain visible in the destination transcript,
//! while the first native turn receives a bounded text bootstrap because
//! provider runtimes cannot share their private conversation state.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::model::{AgentSession, Message, MessageRole, ProviderKind, SessionStatus, unix_time};

const RECENT_MESSAGE_COUNT: usize = 6;
const EARLIER_MESSAGE_CHAR_LIMIT: usize = 320;
const RECENT_MESSAGE_CHAR_LIMIT: usize = 2_400;
pub const HANDOVER_CONTEXT_CHAR_LIMIT: usize = 32_000;

/// Durable provenance and bootstrap state for a cross-provider task.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionHandover {
    pub source_session: Uuid,
    pub source_provider: ProviderKind,
    pub imported_at: u64,
    /// Imported messages occupy the first `n` transcript positions. Keeping
    /// the boundary explicit prevents the first destination prompt from being
    /// repeated inside its own bootstrap.
    pub imported_message_count: usize,
    pub bootstrap_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoverError {
    SameProvider,
    SourceBusy,
    NoSettledContext,
    DestinationAlreadyStarted,
    NeedsNativeTurn,
}

impl fmt::Display for HandoverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::SameProvider => "the destination provider must be different",
            Self::SourceBusy => "the current task must finish before it can be handed off",
            Self::NoSettledContext => "the current task has no settled conversation to hand off",
            Self::DestinationAlreadyStarted => "the destination task has already started",
            Self::NeedsNativeTurn => {
                "the current provider must complete a turn before another handoff"
            }
        };
        formatter.write_str(message)
    }
}

impl Error for HandoverError {}

fn importable_message(message: &Message) -> bool {
    !message.streaming && matches!(message.role, MessageRole::User | MessageRole::Assistant)
}

pub fn can_handover_session(session: &AgentSession) -> bool {
    !session.status.is_busy()
        && session.messages.iter().any(importable_message)
        && session
            .handover
            .as_ref()
            .is_none_or(|_| !session.turns.is_empty())
}

/// Create the destination task without starting its provider runtime.
///
/// `destination` should be a fresh task created through the client's normal
/// preference path so its target-provider model defaults are retained.
pub fn create_handover_session(
    source: &AgentSession,
    mut destination: AgentSession,
) -> Result<AgentSession, HandoverError> {
    if source.provider == destination.provider {
        return Err(HandoverError::SameProvider);
    }
    if source.status.is_busy() {
        return Err(HandoverError::SourceBusy);
    }
    if destination.has_started() {
        return Err(HandoverError::DestinationAlreadyStarted);
    }
    if source.handover.is_some() && source.turns.is_empty() {
        return Err(HandoverError::NeedsNativeTurn);
    }

    let messages = source
        .messages
        .iter()
        .filter(|message| importable_message(message))
        .cloned()
        .map(|mut message| {
            message.id = Uuid::new_v4();
            message.turn_id = None;
            message.streaming = false;
            message
        })
        .collect::<Vec<_>>();
    if messages.is_empty() {
        return Err(HandoverError::NoSettledContext);
    }

    let now = unix_time();
    destination.title.clone_from(&source.title);
    destination.auto_title.clone_from(&source.auto_title);
    destination.project_id = source.project_id;
    destination.workspace.clone_from(&source.workspace);
    destination.runtime_mode = source.runtime_mode;
    destination.interaction_mode = source.interaction_mode;
    destination.status = SessionStatus::Idle;
    destination.created_at = now;
    destination.updated_at = now;
    destination.last_reply_at = None;
    destination.provider_cursor = None;
    destination.available_commands.clear();
    destination.context_usage = None;
    destination.runtime_event_cursor = None;
    destination.provider_session_id = None;
    destination.messages = messages;
    destination.transcript_blocks.clear();
    destination.turns.clear();
    destination.queued_messages.clear();
    destination.detail_loaded = true;
    destination.handover = Some(SessionHandover {
        source_session: source.id,
        source_provider: source.provider,
        imported_at: now,
        imported_message_count: destination.messages.len(),
        bootstrap_pending: true,
    });
    // Imported transcript messages deliberately make the destination count as
    // started. That keeps generic draft reuse/provider retargeting from
    // recycling a handoff task. Runtime attachment may miss until the first
    // native prompt starts; that miss is benign.
    debug_assert!(destination.has_started());
    Ok(destination)
}

fn normalize_message_text(value: &str) -> String {
    value
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let mut truncated = value.chars().take(max_chars - 3).collect::<String>();
    while truncated.ends_with(char::is_whitespace) {
        truncated.pop();
    }
    truncated.push_str("...");
    truncated
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::System => "System",
    }
}

/// Build the bounded context sent once with the destination's first turn.
pub fn build_handover_bootstrap_text(session: &AgentSession) -> Option<String> {
    let handover = session
        .handover
        .as_ref()
        .filter(|handover| handover.bootstrap_pending)?;
    let imported = session
        .messages
        .iter()
        .take(handover.imported_message_count)
        .filter(|message| importable_message(message))
        .collect::<Vec<_>>();
    if imported.is_empty() {
        return None;
    }

    let earlier = imported.len().saturating_sub(RECENT_MESSAGE_COUNT);
    let mut sections = vec![
        format!(
            "This task was handed off from {} to {}. Continue the same objective in the current workspace.",
            handover.source_provider.short_name(),
            session.provider.short_name()
        ),
        format!("Original task title: {}", session.display_title()),
    ];
    if earlier > 0 {
        let summaries = imported[..earlier]
            .iter()
            .map(|message| {
                format!(
                    "- {}: {}",
                    role_label(message.role),
                    truncate_chars(
                        &normalize_message_text(message.visible_content()),
                        EARLIER_MESSAGE_CHAR_LIMIT,
                    )
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("Earlier conversation summary:\n{summaries}"));
    }
    let recent = imported[earlier..]
        .iter()
        .map(|message| {
            format!(
                "{}:\n{}",
                role_label(message.role),
                truncate_chars(
                    &normalize_message_text(message.visible_content()),
                    RECENT_MESSAGE_CHAR_LIMIT,
                )
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    sections.push(format!("Most recent imported messages:\n{recent}"));

    Some(truncate_chars(
        &sections.join("\n\n"),
        HANDOVER_CONTEXT_CHAR_LIMIT,
    ))
}

/// Wrap the first native user message with its one-shot handover context.
pub fn handover_provider_prompt(session: &AgentSession, user_prompt: &str) -> Option<String> {
    let context = build_handover_bootstrap_text(session)?;
    Some(format!(
        "<handover_context>\n{context}\n</handover_context>\n\n<latest_user_message>\n{user_prompt}\n</latest_user_message>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled_source() -> AgentSession {
        let mut source = AgentSession::new(Uuid::from_u128(9), ProviderKind::Claude);
        source.begin_turn("Build the feature");
        source.push_message(MessageRole::Assistant, "The core is ready.");
        source.finish_active_turn(crate::model::TurnStatus::Completed);
        source
    }

    #[test]
    fn handover_creates_a_fresh_target_task_with_imported_messages() {
        let source = settled_source();
        let mut target = AgentSession::new(source.project_id, ProviderKind::Codex);
        target.model = Some("gpt-5.4".into());

        let handover = create_handover_session(&source, target).unwrap();

        assert_eq!(handover.provider, ProviderKind::Codex);
        assert_eq!(handover.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(handover.workspace, source.workspace);
        assert_eq!(handover.messages.len(), 2);
        assert!(
            handover
                .messages
                .iter()
                .all(|message| message.turn_id.is_none())
        );
        assert!(handover.turns.is_empty());
        assert!(handover.has_started());
        assert!(handover.provider_cursor.is_none());
        assert_eq!(
            handover.handover.as_ref().unwrap().source_session,
            source.id
        );
    }

    #[test]
    fn first_target_prompt_receives_context_once() {
        let source = settled_source();
        let target = AgentSession::new(source.project_id, ProviderKind::Codex);
        let mut handover = create_handover_session(&source, target).unwrap();

        let prompt = handover_provider_prompt(&handover, "Now wire the UI").unwrap();
        assert!(prompt.contains("handed off from Claude to Codex"));
        assert!(prompt.contains("User:\nBuild the feature"));
        assert!(prompt.contains("Assistant:\nThe core is ready."));
        assert!(prompt.contains("<latest_user_message>\nNow wire the UI"));

        handover.handover.as_mut().unwrap().bootstrap_pending = false;
        assert_eq!(handover_provider_prompt(&handover, "Again"), None);
    }

    #[test]
    fn imported_only_handover_cannot_chain_again() {
        let source = settled_source();
        let target = AgentSession::new(source.project_id, ProviderKind::Codex);
        let handover = create_handover_session(&source, target).unwrap();
        let next = AgentSession::new(source.project_id, ProviderKind::Grok);

        assert_eq!(
            create_handover_session(&handover, next).unwrap_err(),
            HandoverError::NeedsNativeTurn
        );
    }
}

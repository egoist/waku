use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::model::{AgentSession, MessageRole};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ComposerDraftAttachment {
    #[ts(type = "string")]
    pub path: PathBuf,
    pub mention: String,
    pub name: String,
    pub is_dir: bool,
    pub is_image: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_reference: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ComposerDraft {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ComposerDraftAttachment>,
}

impl ComposerDraft {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.attachments.is_empty()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ComposerDrafts {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub new_sessions: HashMap<Uuid, ComposerDraft>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub sessions: HashMap<Uuid, ComposerDraft>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerDraftKey {
    NewSession(Uuid),
    Session(Uuid),
}

/// Wire-safe identity for one independently persisted composer draft.
///
/// Draft updates are keyed so multiple connected clients cannot overwrite
/// unrelated drafts by sending stale whole-file snapshots.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ComposerDraftTarget {
    NewSession {
        #[ts(type = "string")]
        project_id: Uuid,
    },
    Session {
        #[ts(type = "string")]
        session_id: Uuid,
    },
}

impl From<ComposerDraftKey> for ComposerDraftTarget {
    fn from(key: ComposerDraftKey) -> Self {
        match key {
            ComposerDraftKey::NewSession(project_id) => Self::NewSession { project_id },
            ComposerDraftKey::Session(session_id) => Self::Session { session_id },
        }
    }
}

impl From<ComposerDraftTarget> for ComposerDraftKey {
    fn from(target: ComposerDraftTarget) -> Self {
        match target {
            ComposerDraftTarget::NewSession { project_id } => Self::NewSession(project_id),
            ComposerDraftTarget::Session { session_id } => Self::Session(session_id),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ComposerDraftChange {
    pub target: ComposerDraftTarget,
    /// `None` removes the target. Empty drafts are normalized to removal too.
    pub draft: Option<ComposerDraft>,
}

impl ComposerDraftKey {
    pub fn for_session(session: &AgentSession) -> Self {
        if session.has_started() {
            Self::Session(session.id)
        } else {
            Self::NewSession(session.project_id)
        }
    }
}

impl ComposerDrafts {
    pub fn get_for(&self, session: &AgentSession) -> Option<&ComposerDraft> {
        self.get(ComposerDraftKey::for_session(session))
    }

    pub fn get(&self, key: ComposerDraftKey) -> Option<&ComposerDraft> {
        match key {
            ComposerDraftKey::NewSession(project_id) => self.new_sessions.get(&project_id),
            ComposerDraftKey::Session(session_id) => self.sessions.get(&session_id),
        }
    }

    pub fn set(&mut self, key: ComposerDraftKey, draft: ComposerDraft) -> bool {
        let (drafts, id) = match key {
            ComposerDraftKey::NewSession(project_id) => (&mut self.new_sessions, project_id),
            ComposerDraftKey::Session(session_id) => (&mut self.sessions, session_id),
        };
        if draft.is_empty() {
            drafts.remove(&id).is_some()
        } else if drafts.get(&id) == Some(&draft) {
            false
        } else {
            drafts.insert(id, draft);
            true
        }
    }

    pub fn remove(&mut self, key: ComposerDraftKey) -> bool {
        match key {
            ComposerDraftKey::NewSession(project_id) => {
                self.new_sessions.remove(&project_id).is_some()
            }
            ComposerDraftKey::Session(session_id) => self.sessions.remove(&session_id).is_some(),
        }
    }

    pub fn move_to_empty(
        &mut self,
        source: ComposerDraftKey,
        destination: ComposerDraftKey,
    ) -> bool {
        if source == destination || self.get(destination).is_some_and(|draft| !draft.is_empty()) {
            return false;
        }
        let Some(draft) = self.get(source).cloned() else {
            return false;
        };
        self.remove(source);
        self.set(destination, draft)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct SessionMessageMatch {
    pub session_id: Uuid,
    pub source: MessageRole,
    pub snippet: String,
}

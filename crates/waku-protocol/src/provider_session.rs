use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{AgentSession, ProviderResumeCursor};

/// One provider-native session that a Waku task can resume.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ResumableSession {
    pub cursor: ProviderResumeCursor,
    pub label: String,
    pub modified_ms: i64,
}

/// One piece of a provider transcript imported into a Waku task.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum ImportedSessionRecord {
    Prompt(String),
    Assistant(String),
    Activity(crate::model::ActivityItem),
}

/// Daemon-host native-session operation used when no live driver can fork.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "provider", rename_all = "camelCase")]
pub enum ProviderSessionForkRequest {
    Claude {
        session_id: String,
        resume_at: Option<String>,
        turn_count: usize,
        title: String,
    },
    Amp {
        binary: PathBuf,
        cwd: PathBuf,
        thread_id: String,
        fork_context: Option<String>,
        turn_count: usize,
    },
    Cursor {
        source: AgentSession,
        turn_count: usize,
    },
    OpenCode {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
        turn_count: usize,
    },
    Grok {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
        turn_count: usize,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionFork {
    pub cursor: ProviderResumeCursor,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub message_ids: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_resume_at: Option<String>,
}

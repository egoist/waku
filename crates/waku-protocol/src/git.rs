use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::ProviderKind;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct BranchEntry {
    pub name: String,
    pub checked_out_elsewhere: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct BranchSnapshot {
    #[ts(type = "string")]
    pub repository: PathBuf,
    pub current: Option<String>,
    pub detached_head: Option<String>,
    pub default_branch: Option<String>,
    pub branches: Vec<BranchEntry>,
    pub additions: u64,
    pub deletions: u64,
}

impl BranchSnapshot {
    pub fn display_branch(&self) -> Option<&str> {
        self.current.as_deref().or(self.detached_head.as_deref())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct CommitSnapshot {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub staged_additions: u64,
    pub staged_deletions: u64,
    pub has_staged: bool,
    pub has_unstaged: bool,
    pub can_push: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AgentInvocation {
    pub provider: ProviderKind,
    #[ts(type = "string")]
    pub binary: PathBuf,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct CreatedWorktree {
    #[ts(type = "string")]
    pub path: PathBuf,
    pub branch: String,
}

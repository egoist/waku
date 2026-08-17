use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::composer::{FileEntry, SlashCommand};
use crate::git::{AgentInvocation, BranchSnapshot, CommitSnapshot, CreatedWorktree};
use crate::model::{Checkpoint, ProviderKind};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDiffSource {
    LastTurn {
        session_id: Uuid,
        turn_id: Uuid,
        turn_count: usize,
    },
    Uncommitted,
    Unstaged,
    Staged,
    Committed,
    Branch,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffData {
    pub source: ReviewDiffSource,
    pub numstat: String,
    pub patch: String,
    pub complete_context: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTreeEntry {
    pub relative_path: String,
    #[ts(type = "string")]
    pub absolute_path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub depth: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceOperation {
    ListTree {
        #[ts(type = "string")]
        root: PathBuf,
        #[ts(type = "string[]")]
        expanded_paths: Vec<PathBuf>,
    },
    BrowseDirectory {
        #[ts(type = "string | null")]
        path: Option<PathBuf>,
    },
    ReadTextFile {
        #[ts(type = "string")]
        root: PathBuf,
        #[ts(type = "string")]
        relative_path: PathBuf,
    },
    WriteTextFile {
        #[ts(type = "string")]
        root: PathBuf,
        #[ts(type = "string")]
        relative_path: PathBuf,
        content: String,
    },
    ListProjectFiles {
        #[ts(type = "string")]
        root: PathBuf,
        cap: usize,
    },
    DiscoverSlashCommands {
        provider: ProviderKind,
        #[ts(type = "string")]
        project_root: PathBuf,
    },
    CreateProjectlessWorkspace {
        prompt: Option<String>,
    },
    MigrateProjectlessWorkspace {
        #[ts(type = "string")]
        path: PathBuf,
    },
    InspectBranches {
        #[ts(type = "string")]
        cwd: PathBuf,
    },
    CheckoutBranch {
        #[ts(type = "string")]
        cwd: PathBuf,
        branch: String,
        create: bool,
    },
    CreateWorktree {
        #[ts(type = "string")]
        project_path: PathBuf,
        project_id: Uuid,
        session_id: Uuid,
        prompt: String,
        base_branch: Option<String>,
    },
    InspectCommit {
        #[ts(type = "string")]
        cwd: PathBuf,
    },
    GenerateCommitMessage {
        #[ts(type = "string")]
        cwd: PathBuf,
        include_unstaged: bool,
        invocation: AgentInvocation,
    },
    Commit {
        #[ts(type = "string")]
        cwd: PathBuf,
        message: String,
        include_unstaged: bool,
        push: bool,
    },
    Push {
        #[ts(type = "string")]
        cwd: PathBuf,
    },
    CaptureTurnStart {
        #[ts(type = "string")]
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    },
    CaptureTurn {
        #[ts(type = "string")]
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    },
    CaptureRef {
        #[ts(type = "string")]
        cwd: PathBuf,
        git_ref: String,
    },
    RestoreRef {
        #[ts(type = "string")]
        cwd: PathBuf,
        git_ref: String,
    },
    HasRef {
        #[ts(type = "string")]
        cwd: PathBuf,
        git_ref: String,
    },
    SessionTurnRefs {
        #[ts(type = "string")]
        cwd: PathBuf,
        session_id: Uuid,
    },
    DeleteRef {
        #[ts(type = "string")]
        cwd: PathBuf,
        git_ref: String,
    },
    DeleteTurnRefsAfter {
        #[ts(type = "string")]
        cwd: PathBuf,
        session_id: Uuid,
        retained_turn_count: usize,
        previous_turn_count: usize,
    },
    DeleteSessionRefs {
        #[ts(type = "string")]
        cwd: PathBuf,
        session_id: Uuid,
    },
    CopySessionRefs {
        #[ts(type = "string")]
        cwd: PathBuf,
        source_session_id: Uuid,
        target_session_id: Uuid,
        through_turn_count: usize,
    },
    CollectReviewDiff {
        #[ts(type = "string")]
        cwd: PathBuf,
        source: ReviewDiffSource,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceResult {
    Ack,
    WorkingTree {
        entries: Vec<WorkingTreeEntry>,
    },
    Directory {
        #[ts(type = "string")]
        path: PathBuf,
        #[ts(type = "string | null")]
        parent: Option<PathBuf>,
        #[ts(type = "string")]
        home: PathBuf,
        #[ts(type = "string")]
        filesystem_root: PathBuf,
        entries: Vec<WorkingTreeEntry>,
    },
    TextFile {
        content: String,
    },
    ProjectFiles {
        entries: Vec<FileEntry>,
    },
    SlashCommands {
        commands: Vec<SlashCommand>,
    },
    ProjectlessWorkspace {
        #[ts(type = "string")]
        cwd: PathBuf,
    },
    Branches {
        snapshot: Option<BranchSnapshot>,
    },
    BranchChanged {
        snapshot: BranchSnapshot,
    },
    WorktreeCreated {
        worktree: CreatedWorktree,
    },
    CommitSnapshot {
        snapshot: CommitSnapshot,
    },
    CommitMessage {
        message: String,
    },
    Checkpoint {
        checkpoint: Checkpoint,
    },
    Bool {
        value: bool,
    },
    TurnRefs {
        turn_counts: Vec<usize>,
    },
    ReviewDiff {
        data: ReviewDiffData,
    },
}

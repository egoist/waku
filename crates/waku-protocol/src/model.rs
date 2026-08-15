//! Provider-neutral projects, sessions, transcript items, and driver events.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Amp,
    Claude,
    #[default]
    Codex,
    Cursor,
    DeepSeek,
    OpenCode,
    Grok,
    Pi,
}

impl ProviderKind {
    pub const ALL: [Self; 8] = [
        Self::Amp,
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::DeepSeek,
        Self::OpenCode,
        Self::Grok,
        Self::Pi,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Amp => "amp",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::DeepSeek => "deepseek",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
            Self::Pi => "pi",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Amp => "Amp",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::Cursor => "Cursor CLI",
            Self::DeepSeek => "DeepSeek Harness",
            Self::OpenCode => "OpenCode",
            Self::Grok => "Grok Build",
            Self::Pi => "Pi",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::Amp => "Amp",
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::DeepSeek => "DeepSeek",
            Self::OpenCode => "OpenCode",
            Self::Grok => "Grok",
            Self::Pi => "Pi",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            Self::Amp => "amp",
            Self::Claude => "claude",
            Self::Codex => "codex",
            // Cursor documents `agent` as its primary command, but that name is
            // shared by other CLIs. The backward-compatible alias is unambiguous.
            Self::Cursor => "cursor-agent",
            Self::DeepSeek => "dsh",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
            Self::Pi => "pi",
        }
    }

    pub fn supports_conversation_rollback(self) -> bool {
        matches!(
            self,
            Self::Amp
                | Self::Claude
                | Self::Codex
                | Self::Cursor
                | Self::DeepSeek
                | Self::OpenCode
                | Self::Grok
                | Self::Pi
        )
    }

    pub fn supports_conversation_fork(self) -> bool {
        matches!(
            self,
            Self::Amp
                | Self::Claude
                | Self::Codex
                | Self::Cursor
                | Self::DeepSeek
                | Self::OpenCode
                | Self::Grok
                | Self::Pi
        )
    }

    pub fn supports_model_discovery(self) -> bool {
        matches!(
            self,
            Self::Codex | Self::Cursor | Self::DeepSeek | Self::OpenCode | Self::Grok | Self::Pi
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "provider"
)]
pub enum ProviderResumeCursor {
    Amp {
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork_context: Option<String>,
    },
    Claude {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_at: Option<String>,
    },
    Codex {
        thread_id: String,
    },
    Cursor {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork_context: Option<String>,
    },
    OpenCode {
        session_id: String,
    },
    DeepSeek {
        session_id: String,
    },
    Grok {
        session_id: String,
    },
    Pi {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_file: Option<PathBuf>,
    },
}

impl ProviderResumeCursor {
    pub fn from_session_id(provider: ProviderKind, id: String) -> Self {
        match provider {
            ProviderKind::Amp => Self::Amp {
                thread_id: id,
                fork_context: None,
            },
            ProviderKind::Claude => Self::Claude {
                session_id: id,
                resume_at: None,
            },
            ProviderKind::Codex => Self::Codex { thread_id: id },
            ProviderKind::Cursor => Self::Cursor {
                session_id: id,
                fork_context: None,
            },
            ProviderKind::DeepSeek => Self::DeepSeek { session_id: id },
            ProviderKind::OpenCode => Self::OpenCode { session_id: id },
            ProviderKind::Grok => Self::Grok { session_id: id },
            ProviderKind::Pi => Self::Pi {
                session_id: id,
                session_file: None,
            },
        }
    }

    pub fn provider(&self) -> ProviderKind {
        match self {
            Self::Amp { .. } => ProviderKind::Amp,
            Self::Claude { .. } => ProviderKind::Claude,
            Self::Codex { .. } => ProviderKind::Codex,
            Self::Cursor { .. } => ProviderKind::Cursor,
            Self::DeepSeek { .. } => ProviderKind::DeepSeek,
            Self::OpenCode { .. } => ProviderKind::OpenCode,
            Self::Grok { .. } => ProviderKind::Grok,
            Self::Pi { .. } => ProviderKind::Pi,
        }
    }

    pub fn native_id(&self) -> &str {
        match self {
            Self::Amp { thread_id, .. } => thread_id,
            Self::Claude { session_id, .. }
            | Self::Cursor { session_id, .. }
            | Self::DeepSeek { session_id }
            | Self::OpenCode { session_id }
            | Self::Grok { session_id }
            | Self::Pi { session_id, .. } => session_id,
            Self::Codex { thread_id } => thread_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeMode {
    /// Legacy combined mode. State migration moves this to `interaction_mode`.
    Plan,
    Ask,
    AutoAcceptEdits,
    Auto,
    #[default]
    FullAccess,
}

impl RuntimeMode {
    pub const ACCESS_OPTIONS: [Self; 4] = [
        Self::Ask,
        Self::AutoAcceptEdits,
        Self::Auto,
        Self::FullAccess,
    ];

    pub fn label(self) -> String {
        match self {
            Self::Plan => tr!("mode.plan"),
            Self::Ask => tr!("mode.supervised"),
            Self::AutoAcceptEdits => tr!("mode.auto_accept_edits"),
            Self::Auto => tr!("mode.auto"),
            Self::FullAccess => tr!("mode.full_access"),
        }
    }

    pub fn description(self) -> String {
        match self {
            Self::Plan => tr!("mode.plan_description"),
            Self::Ask => tr!("mode.supervised_description"),
            Self::AutoAcceptEdits => tr!("mode.auto_accept_edits_description"),
            Self::Auto => tr!("mode.auto_description"),
            Self::FullAccess => tr!("mode.full_access_description"),
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Plan | Self::Ask => "icons/lock.svg",
            Self::AutoAcceptEdits => "icons/pencil.svg",
            Self::Auto => "icons/sparkle.svg",
            Self::FullAccess => "icons/lock-open.svg",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum InteractionMode {
    #[default]
    Build,
    Plan,
}

impl InteractionMode {
    pub fn label(self) -> String {
        match self {
            Self::Build => tr!("mode.build"),
            Self::Plan => tr!("mode.plan"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ProviderModelOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ProviderModelOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        let description = description.into();
        if !description.trim().is_empty() {
            self.description = Some(description);
        }
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub reasoning_efforts: Vec<ProviderModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tiers: Vec<ProviderModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
    /// Context window sizes the provider exposes as a per-session choice.
    /// Claude Code keeps its 1M window opt-in behind a model-id suffix, so the
    /// window is a trait of the session rather than of the model.
    #[serde(default)]
    pub context_windows: Vec<ProviderModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_context_window: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct FavoriteModel {
    pub provider: ProviderKind,
    pub model: String,
}

/// One provider-owned agent composition available when a task starts.
///
/// DeepSeek Harness calls these agent presets. They are intentionally kept
/// separate from [`InteractionMode`]: a preset chooses the tools and prompt
/// composition, while Build/Plan controls what that composition should do.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ProviderAgentPreset {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub is_custom: bool,
}

impl ProviderAgentPreset {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            is_default: false,
            is_custom: false,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        let description = description.into();
        if !description.trim().is_empty() {
            self.description = Some(description);
        }
        self
    }

    pub fn default(mut self) -> Self {
        self.is_default = true;
        self
    }

    /// Harness localizes its four shipped presets in the Web client rather
    /// than in the Host roster, whose metadata may use the install language.
    /// Mirror that boundary while leaving user-authored metadata untouched.
    pub fn display_name(&self) -> String {
        if !self.is_custom {
            match self.id.as_str() {
                "standard" => return tr!("agent_preset.standard"),
                "code" => return tr!("agent_preset.code"),
                "minimal" => return tr!("agent_preset.minimal"),
                "cordis" => return tr!("agent_preset.creator"),
                _ => {}
            }
        }
        self.name.clone()
    }

    pub fn display_description(&self) -> Option<String> {
        if !self.is_custom {
            match self.id.as_str() {
                "standard" => return Some(tr!("agent_preset.standard_description")),
                "code" => return Some(tr!("agent_preset.code_description")),
                "minimal" => return Some(tr!("agent_preset.minimal_description")),
                "cordis" => return Some(tr!("agent_preset.creator_description")),
                _ => {}
            }
        }
        self.description.clone()
    }
}

impl ProviderModel {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            sub_provider: None,
            is_default: false,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            service_tiers: Vec::new(),
            default_service_tier: None,
            context_windows: Vec::new(),
            default_context_window: None,
        }
    }

    pub fn default(mut self) -> Self {
        self.is_default = true;
        self
    }

    pub fn sub_provider(mut self, sub_provider: impl Into<String>) -> Self {
        self.sub_provider = Some(sub_provider.into());
        self
    }

    pub fn reasoning(
        mut self,
        efforts: impl IntoIterator<Item = ProviderModelOption>,
        default: impl Into<String>,
    ) -> Self {
        self.reasoning_efforts = efforts.into_iter().collect();
        self.default_reasoning_effort = Some(default.into());
        self
    }

    pub fn service_tiers(
        mut self,
        tiers: impl IntoIterator<Item = ProviderModelOption>,
        default: impl Into<String>,
    ) -> Self {
        self.service_tiers = tiers.into_iter().collect();
        self.default_service_tier = Some(default.into());
        self
    }

    pub fn context_windows(
        mut self,
        windows: impl IntoIterator<Item = ProviderModelOption>,
        default: impl Into<String>,
    ) -> Self {
        self.context_windows = windows.into_iter().collect();
        self.default_context_window = Some(default.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ProviderProbe {
    pub provider: ProviderKind,
    pub installed: bool,
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    #[serde(default)]
    pub agent_presets: Vec<ProviderAgentPreset>,
}

impl ProviderProbe {
    pub fn preferred_model(&self) -> Option<&ProviderModel> {
        self.models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| self.models.first())
    }

    pub fn preferred_agent_preset(&self) -> Option<&ProviderAgentPreset> {
        self.agent_presets
            .iter()
            .find(|preset| preset.is_default)
            .or_else(|| self.agent_presets.first())
    }
}

pub fn parse_cli_version(output: &str) -> Option<String> {
    let line = output.lines().find(|line| !line.trim().is_empty())?;
    line.split_whitespace()
        .map(|token| {
            token
                .trim_start_matches('v')
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        })
        .find(|token| {
            let mut parts = token.split('.');
            let leading_number = parts
                .next()
                .is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
            leading_number
                && parts
                    .next()
                    .is_some_and(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    /// When the project was added, unix seconds.
    #[serde(default)]
    pub created_at: u64,
}

/// Filesystem context a task runs in.
///
/// Drafts may carry [`Self::NewWorktree`] until their first prompt. Waku then
/// creates the Git worktree and replaces it with [`Self::Worktree`] before any
/// checkpoint or provider process can observe the task.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum SessionWorkspace {
    /// Work directly in the project's ordinary checkout.
    #[default]
    Local,
    /// Create an isolated worktree when this draft is first submitted. A
    /// selected base branch is remembered without checking it out in the
    /// ordinary project directory.
    NewWorktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_branch: Option<String>,
    },
    /// A materialized worktree. `path` preserves a project that points at a
    /// subdirectory of its repository rather than the repository root itself.
    Worktree { path: PathBuf, branch: String },
}

impl SessionWorkspace {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn is_worktree(&self) -> bool {
        matches!(self, Self::NewWorktree { .. } | Self::Worktree { .. })
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Worktree { path, .. } => Some(path),
            Self::Local | Self::NewWorktree { .. } => None,
        }
    }
}

impl Project {
    pub const PROJECTLESS_NAME: &'static str = "No project";

    pub fn display_name(&self) -> String {
        if self.is_projectless() {
            tr!("project.no_project_name")
        } else {
            self.name.clone()
        }
    }

    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("Project")
            .to_owned();
        Self {
            id: Uuid::new_v4(),
            name,
            path,
            created_at: unix_time(),
        }
    }

    pub fn is_projectless(&self) -> bool {
        crate::projectless::is_projectless_path(&self.path)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    #[default]
    Idle,
    Connecting,
    Working,
    Waiting,
    Failed,
}

impl SessionStatus {
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Connecting | Self::Working | Self::Waiting)
    }
}

/// A follow-up message queued while the agent is busy. It becomes its own
/// turn once the current turn settles successfully.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub content: String,
    /// The text typed before Waku appended provider-facing attachment
    /// mentions. `None` is the legacy/plain-message representation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
    pub created_at: u64,
}

impl QueuedMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            content: content.into(),
            display_content: None,
            attachments: Vec::new(),
            created_at: unix_time(),
        }
    }

    pub fn with_presentation(
        content: impl Into<String>,
        display_content: Option<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Self {
        Self {
            display_content,
            attachments,
            ..Self::new(content)
        }
    }

    pub fn visible_content(&self) -> &str {
        self.display_content.as_deref().unwrap_or(&self.content)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum CheckpointStatus {
    Ready,
    Unavailable,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct CheckpointFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct Checkpoint {
    pub turn_count: usize,
    pub git_ref: String,
    pub status: CheckpointStatus,
    #[serde(default)]
    pub files: Vec<CheckpointFile>,
    /// Cached once at capture time so a visible transcript row never walks a
    /// potentially huge file list on every frame.
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    pub created_at: u64,
}

impl Checkpoint {
    pub fn refresh_totals(&mut self) {
        self.additions = self.files.iter().map(|file| file.additions).sum();
        self.deletions = self.files.iter().map(|file| file.deletions).sum();
    }

    pub fn totals_are_current(&self) -> bool {
        self.additions == self.files.iter().map(|file| file.additions).sum::<u64>()
            && self.deletions == self.files.iter().map(|file| file.deletions).sum::<u64>()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AgentTurn {
    pub id: Uuid,
    pub turn_count: usize,
    pub status: TurnStatus,
    #[serde(default)]
    pub provider_turn_started: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_resume_at: Option<String>,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub checkpoint: Option<Checkpoint>,
}

/// How full the provider's context window is, from the latest main-thread
/// model call. `tokens` is prompt + cache + output of that call; `window` is
/// the model's context size, which the provider only reports once a turn
/// settles — `None` means "not known yet", and the meter degrades to a bare
/// token count.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize, TS)]
pub struct ContextUsage {
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

/// Last daemon event incorporated into a session's persisted projection.
///
/// The daemon runtime and its replay journal can outlive any particular
/// desktop or browser connection. Persisting this cursor with the transcript
/// lets a newly attached client replay only the events the stored projection
/// has not already applied.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct RuntimeEventCursor {
    pub runtime_id: Uuid,
    pub epoch: Uuid,
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AgentSession {
    pub id: Uuid,
    /// A title explicitly chosen by the user. [`Self::DEFAULT_TITLE`] means
    /// no explicit title has been set, so [`Self::auto_title`] may be shown.
    pub title: String,
    /// Best-effort title supplied by the provider, or derived locally from the
    /// first prompt until the provider reports a better one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_title: Option<String>,
    pub project_id: Uuid,
    /// Local project checkout or an isolated Git worktree for this task.
    #[serde(default, skip_serializing_if = "SessionWorkspace::is_local")]
    pub workspace: SessionWorkspace,
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub runtime_mode: RuntimeMode,
    #[serde(default)]
    pub interaction_mode: InteractionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Selected context window, when the provider exposes more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,
    /// Provider-owned agent composition selected before the first turn.
    /// Currently populated by DeepSeek Harness; unlike Build/Plan, Harness
    /// locks this value once conversation history exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
    pub status: SessionStatus,
    /// The automation this session was spawned by, when it originated from one.
    /// A promoted column, so the sidebar can badge the row without hydrating
    /// the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originating_automation: Option<Uuid>,
    pub created_at: u64,
    /// Any mutation, including title edits and truncation. Use
    /// [`Self::last_reply_at`] for conversation recency.
    pub updated_at: u64,
    /// Activity time of the newest turn. Set as soon as the user submits it,
    /// then refreshed when the turn settles, whatever its outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply_at: Option<u64>,
    #[serde(default)]
    pub provider_cursor: Option<ProviderResumeCursor>,
    /// Slash commands the provider reported for this session's live process,
    /// kept so a resumed session still completes them before its next
    /// handshake.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_commands: Vec<ReportedCommand>,
    /// Context-window occupancy from the live stream, kept so a resumed
    /// session's meter starts where the conversation left off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_event_cursor: Option<RuntimeEventCursor>,
    /// Read-only compatibility field for v1 state files. New saves omit it.
    #[serde(default, skip_serializing)]
    pub provider_session_id: Option<String>,
    /// Not stored in the session JSON — these are rows in the `messages`
    /// table, reattached when the session is hydrated.
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub transcript_blocks: Vec<TranscriptBlock>,
    #[serde(default)]
    pub turns: Vec<AgentTurn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_messages: Vec<QueuedMessage>,
    /// Whether the transcript has been read from the database.
    ///
    /// Startup loads only the columns the session list needs, so a session
    /// begins as a skeleton with empty `messages`, `transcript_blocks` and
    /// `turns`. Those are empty because nothing fetched them, not because the
    /// session is empty — never persist a skeleton, and never conclude from one
    /// that a session has no history.
    #[serde(skip, default = "detail_loaded_default")]
    pub detail_loaded: bool,
}

/// Anything deserialized from a `data` blob carries its full detail.
fn detail_loaded_default() -> bool {
    true
}

impl AgentSession {
    pub const DEFAULT_TITLE: &'static str = "New task";

    pub fn new(project_id: Uuid, provider: ProviderKind) -> Self {
        let now = unix_time();
        Self {
            id: Uuid::new_v4(),
            title: Self::DEFAULT_TITLE.to_owned(),
            auto_title: None,
            project_id,
            workspace: SessionWorkspace::Local,
            provider,
            model: None,
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Build,
            reasoning_effort: None,
            service_tier: None,
            context_window: None,
            agent_preset: None,
            status: SessionStatus::Idle,
            originating_automation: None,
            created_at: now,
            updated_at: now,
            last_reply_at: None,
            detail_loaded: true,
            provider_cursor: None,
            available_commands: Vec::new(),
            context_usage: None,
            runtime_event_cursor: None,
            provider_session_id: None,
            messages: Vec::new(),
            transcript_blocks: Vec::new(),
            turns: Vec::new(),
            queued_messages: Vec::new(),
        }
    }

    /// Returns the lightweight projection used by task lists.
    ///
    /// A daemon can hold hydrated sessions in memory, but catalog refreshes
    /// must never clone or transmit their transcripts. Clients hydrate one
    /// selected session explicitly when they need its detail.
    pub fn list_projection(&self) -> Self {
        Self {
            id: self.id,
            title: self.title.clone(),
            auto_title: self.auto_title.clone(),
            project_id: self.project_id,
            workspace: SessionWorkspace::Local,
            provider: self.provider,
            model: self.model.clone(),
            runtime_mode: RuntimeMode::default(),
            interaction_mode: InteractionMode::default(),
            reasoning_effort: None,
            service_tier: None,
            context_window: None,
            agent_preset: None,
            status: self.status,
            // The sidebar groups runs under their automation straight from the
            // catalog, so this projection must carry the link even though it
            // drops every other detail field.
            originating_automation: self.originating_automation,
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_reply_at: self.last_reply_at,
            provider_cursor: None,
            available_commands: Vec::new(),
            context_usage: None,
            runtime_event_cursor: None,
            provider_session_id: None,
            messages: Vec::new(),
            transcript_blocks: Vec::new(),
            turns: Vec::new(),
            queued_messages: Vec::new(),
            detail_loaded: false,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.status.is_busy()
    }

    /// Derives [`Self::last_reply_at`] from the turn history when it is not
    /// already known, so a session stored before the field existed still sorts
    /// and displays correctly.
    pub fn backfill_last_reply_at(&mut self) {
        if self.last_reply_at.is_some() {
            return;
        }
        self.last_reply_at = self
            .turns
            .last()
            .map(|turn| turn.completed_at.unwrap_or(turn.started_at))
            .filter(|_| self.has_started());
    }

    pub fn has_started(&self) -> bool {
        // A skeleton came from a stored row, and only started sessions are
        // stored, so it has started even though its transcript is not loaded.
        !self.detail_loaded
            || !self.turns.is_empty()
            || !self.messages.is_empty()
            || self.provider_cursor.is_some()
    }

    pub fn display_title(&self) -> &str {
        if self.title != Self::DEFAULT_TITLE && !self.title.trim().is_empty() {
            &self.title
        } else {
            self.auto_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(Self::DEFAULT_TITLE)
        }
    }

    /// Sets the user-owned title shown ahead of any provider fallback.
    /// Empty names are rejected so a cancelled inline rename cannot hide the
    /// existing title. Returns whether the stored title changed.
    pub fn set_title(&mut self, title: impl AsRef<str>) -> bool {
        let title = title.as_ref().trim();
        if title.is_empty() || self.title == title {
            return false;
        }
        self.title = title.to_owned();
        self.updated_at = unix_time();
        true
    }

    pub fn set_title_from_prompt(&mut self, prompt: &str) {
        if self.messages.len() > 1 || self.title != Self::DEFAULT_TITLE || self.auto_title.is_some()
        {
            return;
        }
        let mut title = prompt
            .split_whitespace()
            .take(7)
            .collect::<Vec<_>>()
            .join(" ");
        if !title.is_empty() {
            if title.chars().count() > 54 {
                title = format!("{}…", title.chars().take(53).collect::<String>());
            }
            self.auto_title = Some(title);
        }
    }

    /// Replaces the provider-owned title without disturbing an explicit user
    /// title. Returns whether the stored fallback changed.
    pub fn set_auto_title(&mut self, title: Option<String>) -> bool {
        let title = title.and_then(|title| {
            let title = title.trim();
            (!title.is_empty()).then(|| title.to_owned())
        });
        if self.auto_title == title {
            return false;
        }
        self.auto_title = title;
        self.updated_at = unix_time();
        true
    }

    pub fn can_choose_model(&self, provider: ProviderKind) -> bool {
        !self.status.is_busy() && (self.messages.is_empty() || self.provider == provider)
    }

    pub fn migrate_legacy_state(&mut self) {
        if self.runtime_mode == RuntimeMode::Plan {
            self.runtime_mode = RuntimeMode::Ask;
            self.interaction_mode = InteractionMode::Plan;
        }
        if self.provider_cursor.is_none()
            && let Some(id) = self.provider_session_id.take()
        {
            self.provider_cursor = Some(ProviderResumeCursor::from_session_id(self.provider, id));
        }
        if self.provider == ProviderKind::Codex {
            for message in &mut self.messages {
                if message.role == MessageRole::Assistant && message.content.contains('\u{e200}') {
                    message.content = strip_legacy_codex_citations(&message.content);
                }
            }
        }
        let mut merged_blocks: Vec<TranscriptBlock> =
            Vec::with_capacity(self.transcript_blocks.len());
        for mut block in std::mem::take(&mut self.transcript_blocks) {
            if let Some(previous) = merged_blocks.last_mut()
                && previous.after_message == block.after_message
                && previous.turn_id == block.turn_id
            {
                previous.activities.append(&mut block.activities);
            } else {
                merged_blocks.push(block);
            }
        }
        self.transcript_blocks = merged_blocks;

        for block in &mut self.transcript_blocks {
            for activity in &mut block.activities {
                if activity.kind == ActivityKind::Search && activity.title.trim() == "Search for" {
                    activity.title = tr!("activity.browsed_web");
                }
                let named_kind = ActivityKind::from_tool_name(&activity.title);
                if named_kind != ActivityKind::Tool
                    && matches!(
                        activity.kind,
                        ActivityKind::Search | ActivityKind::Tool | ActivityKind::FileChange
                    )
                {
                    activity.kind = named_kind;
                }
                if activity.arguments.is_none()
                    && activity.output.is_none()
                    && !activity.failed
                    && activity.detail.as_deref().is_some_and(|detail| {
                        serde_json::from_str::<serde_json::Value>(detail).is_ok()
                    })
                {
                    // Older provider transcripts stored input JSON in
                    // `detail`. Promote it once so it stays expandable but no
                    // longer floods the row preview.
                    activity.arguments = activity.detail.take();
                }
                activity.refresh_activity_metadata();
            }
        }

        // Checkpoints written before cached totals were added still have the
        // complete file list. Backfill once on load rather than making every
        // transcript frame rediscover the same totals.
        for turn in &mut self.turns {
            if let Some(checkpoint) = turn.checkpoint.as_mut() {
                checkpoint.refresh_totals();
            }
        }

        if !self.turns.is_empty()
            || !self
                .messages
                .iter()
                .any(|message| message.role == MessageRole::User)
        {
            return;
        }

        let user_indexes = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| (message.role == MessageRole::User).then_some(index))
            .collect::<Vec<_>>();
        for (offset, start) in user_indexes.iter().copied().enumerate() {
            let end = user_indexes
                .get(offset + 1)
                .copied()
                .unwrap_or(self.messages.len());
            let id = Uuid::new_v4();
            let started_at = self.messages[start].created_at;
            let completed_at = self.messages[start..end]
                .iter()
                .map(|message| message.created_at)
                .max()
                .unwrap_or(started_at);
            for message in &mut self.messages[start..end] {
                message.turn_id = Some(id);
            }
            for block in &mut self.transcript_blocks {
                if block.after_message > start && block.after_message <= end {
                    block.turn_id = Some(id);
                }
            }
            self.turns.push(AgentTurn {
                id,
                turn_count: offset + 1,
                status: TurnStatus::Completed,
                provider_turn_started: true,
                provider_resume_at: None,
                started_at,
                completed_at: Some(completed_at),
                checkpoint: None,
            });
        }
    }

    #[doc(hidden)]
    pub fn begin_turn(&mut self, prompt: impl Into<String>) -> Uuid {
        self.begin_turn_with_presentation(prompt, None, Vec::new())
    }

    pub fn begin_turn_with_presentation(
        &mut self,
        prompt: impl Into<String>,
        display_content: Option<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let now = unix_time();
        self.turns.push(AgentTurn {
            id,
            turn_count: self.turns.len() + 1,
            status: TurnStatus::Running,
            provider_turn_started: false,
            provider_resume_at: None,
            started_at: now,
            completed_at: None,
            checkpoint: None,
        });
        self.messages.push(
            Message::new_for_turn(MessageRole::User, prompt, id)
                .with_presentation(display_content, attachments),
        );
        self.last_reply_at = Some(now);
        id
    }

    pub fn active_turn_id(&self) -> Option<Uuid> {
        self.turns
            .last()
            .filter(|turn| turn.status == TurnStatus::Running)
            .map(|turn| turn.id)
    }

    /// Undo [`Self::begin_turn`] for a turn whose provider never started —
    /// the submission-preparation failure path, where the prompt returns to
    /// the composer. The turn and its messages leave the transcript, and a
    /// first-prompt unwind also gives back the default title that
    /// [`Self::set_title_from_prompt`] replaced. Its submission timestamp stays
    /// as the session's latest activity.
    pub fn unwind_unstarted_turn(&mut self, turn_id: Uuid) {
        let unstarted = self.turns.last().is_some_and(|turn| {
            turn.id == turn_id && turn.status == TurnStatus::Running && !turn.provider_turn_started
        });
        if !unstarted {
            return;
        }
        self.turns.pop();
        self.messages
            .retain(|message| message.turn_id != Some(turn_id));
        if self.messages.is_empty() {
            self.auto_title = None;
        }
    }

    pub fn mark_active_turn_provider_started(&mut self) {
        if let Some(turn) = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)
        {
            turn.provider_turn_started = true;
        }
    }

    pub fn mark_active_turn_provider_resume_at(&mut self, message_id: String) {
        if let Some(turn) = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)
        {
            turn.provider_resume_at = Some(message_id);
        }
    }

    pub fn provider_turns_after(&self, turn_count: usize) -> usize {
        self.turns
            .iter()
            .skip(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count()
    }

    pub fn finish_active_turn(&mut self, status: TurnStatus) -> Option<(Uuid, usize)> {
        let turn = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)?;
        let completed_at = unix_time();
        turn.status = status;
        turn.completed_at = Some(completed_at);
        let result = (turn.id, turn.turn_count);
        self.last_reply_at = Some(completed_at);
        Some(result)
    }

    pub fn push_message(&mut self, role: MessageRole, content: impl Into<String>) -> Uuid {
        let message = match self.active_turn_id() {
            Some(turn_id) => Message::new_for_turn(role, content, turn_id),
            None => Message::new(role, content),
        };
        let id = message.id;
        self.messages.push(message);
        id
    }

    pub fn push_user_message_with_presentation(
        &mut self,
        content: impl Into<String>,
        display_content: Option<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Uuid {
        let message = match self.active_turn_id() {
            Some(turn_id) => Message::new_for_turn(MessageRole::User, content, turn_id),
            None => Message::new(MessageRole::User, content),
        }
        .with_presentation(display_content, attachments);
        let id = message.id;
        self.messages.push(message);
        id
    }

    pub fn truncate_after_turn(&mut self, turn_count: usize) {
        let retained = self
            .turns
            .iter()
            .take(turn_count)
            .map(|turn| turn.id)
            .collect::<std::collections::HashSet<_>>();
        self.turns.truncate(turn_count);
        self.messages.retain(|message| {
            message
                .turn_id
                .is_none_or(|turn_id| retained.contains(&turn_id))
        });
        self.transcript_blocks.retain(|block| {
            block
                .turn_id
                .is_none_or(|turn_id| retained.contains(&turn_id))
        });
        let message_count = self.messages.len();
        for block in &mut self.transcript_blocks {
            block.after_message = block.after_message.min(message_count);
        }
        self.updated_at = unix_time();
    }

    pub fn fork_through_turn(
        &self,
        turn_count: usize,
        provider_cursor: ProviderResumeCursor,
        fork_title: &str,
    ) -> Option<Self> {
        if turn_count == 0 || turn_count > self.turns.len() {
            return None;
        }

        let mut fork = self.clone();
        fork.truncate_after_turn(turn_count);
        let fork_id = Uuid::new_v4();
        let turn_ids = fork
            .turns
            .iter()
            .map(|turn| (turn.id, Uuid::new_v4()))
            .collect::<std::collections::HashMap<_, _>>();

        for turn in &mut fork.turns {
            turn.id = turn_ids[&turn.id];
        }
        for message in &mut fork.messages {
            message.id = Uuid::new_v4();
            if let Some(turn_id) = message.turn_id {
                message.turn_id = turn_ids.get(&turn_id).copied();
            }
            message.streaming = false;
        }
        for block in &mut fork.transcript_blocks {
            if let Some(turn_id) = block.turn_id {
                block.turn_id = turn_ids.get(&turn_id).copied();
            }
        }

        let now = unix_time();
        fork.id = fork_id;
        fork.title = Self::DEFAULT_TITLE.to_owned();
        fork.auto_title = Some(fork_title.to_owned());
        fork.status = SessionStatus::Idle;
        fork.created_at = now;
        fork.updated_at = now;
        fork.provider_cursor = Some(provider_cursor);
        fork.provider_session_id = None;
        // A fork snapshots the conversation, not the pending follow-ups its
        // source session is still holding for the live agent.
        fork.queued_messages.clear();
        Some(fork)
    }
}

fn strip_legacy_codex_citations(text: &str) -> String {
    const START: char = '\u{e200}';
    const END: char = '\u{e201}';
    const SEPARATOR: char = '\u{e202}';

    let mut remaining = text;
    let mut output = String::with_capacity(text.len());
    while let Some(start) = remaining.find(START) {
        output.push_str(&remaining[..start]);
        let marker_start = start + START.len_utf8();
        let Some(end_offset) = remaining[marker_start..].find(END) else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let marker_end = marker_start + end_offset;
        let marker = &remaining[marker_start..marker_end];
        if marker
            .split(SEPARATOR)
            .next()
            .is_some_and(|prefix| prefix != "cite")
        {
            output.push_str(&remaining[start..marker_end + END.len_utf8()]);
        }
        remaining = &remaining[marker_end + END.len_utf8()..];
    }
    output.push_str(remaining);
    output
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// A file represented by a composer chip and retained with the sent message.
///
/// Render paths consume only this cached metadata; they never stat the file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct MessageAttachment {
    /// Absolute path on the daemon host, handed to the provider. Clients must
    /// use `blob_reference` rather than opening this path themselves.
    pub path: PathBuf,
    /// Provider-facing path text, relative to the workspace when possible.
    pub mention: String,
    pub name: String,
    pub is_dir: bool,
    pub is_image: bool,
    /// Durable daemon-issued blob or attachment reference. The legacy field
    /// name is retained for storage compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_reference: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct Message {
    pub id: Uuid,
    #[serde(default)]
    pub turn_id: Option<Uuid>,
    pub role: MessageRole,
    pub content: String,
    /// User-visible text before provider-facing attachment mentions were
    /// appended. Plain and legacy messages omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
    pub created_at: u64,
    pub streaming: bool,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            turn_id: None,
            role,
            content: content.into(),
            display_content: None,
            attachments: Vec::new(),
            created_at: unix_time(),
            streaming: false,
        }
    }

    pub fn new_for_turn(role: MessageRole, content: impl Into<String>, turn_id: Uuid) -> Self {
        Self {
            turn_id: Some(turn_id),
            ..Self::new(role, content)
        }
    }

    pub fn with_presentation(
        mut self,
        display_content: Option<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Self {
        self.display_content = display_content;
        self.attachments = attachments;
        self
    }

    pub fn visible_content(&self) -> &str {
        self.display_content.as_deref().unwrap_or(&self.content)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ActivityKind {
    Reasoning,
    Command,
    FileChange,
    FileRead,
    FileSearch,
    FileList,
    Search,
    Plan,
    Tool,
}

impl ActivityKind {
    /// Classifies provider tool names without mistaking unrelated MCP tools
    /// such as `create_thread` or `read_mcp_resource` for file operations.
    pub fn from_tool_name(name: &str) -> Self {
        let normalized = name.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        let leaf = normalized
            .rsplit("__")
            .next()
            .unwrap_or(&normalized)
            .rsplit([':', '.', '/'])
            .next()
            .unwrap_or(&normalized);
        let compact = leaf.replace('_', "");

        if matches!(
            compact.as_str(),
            "todo" | "todowrite" | "updateplan" | "plan"
        ) {
            Self::Plan
        } else if matches!(
            compact.as_str(),
            "bash"
                | "command"
                | "execute"
                | "executecommand"
                | "commandexecution"
                | "runcommand"
                | "runterminalcommand"
                | "shell"
                | "shellcommand"
                | "terminal"
        ) {
            Self::Command
        } else if matches!(
            compact.as_str(),
            "applypatch"
                | "create"
                | "createfile"
                | "delete"
                | "deletefile"
                | "edit"
                | "filechange"
                | "fileedit"
                | "editfile"
                | "move"
                | "movefile"
                | "multiedit"
                | "notebookedit"
                | "patch"
                | "rename"
                | "renamefile"
                | "replace"
                | "savefile"
                | "strreplace"
                | "write"
                | "writefile"
        ) {
            Self::FileChange
        } else if matches!(
            compact.as_str(),
            "read" | "fileread" | "readfile" | "readtextfile" | "viewfile"
        ) {
            Self::FileRead
        } else if matches!(
            compact.as_str(),
            "filesearch"
                | "find"
                | "findfiles"
                | "glob"
                | "grep"
                | "ripgrep"
                | "searchfiles"
                | "searchinfiles"
        ) {
            Self::FileSearch
        } else if matches!(
            compact.as_str(),
            "directorylist"
                | "filelist"
                | "list"
                | "listdirectory"
                | "listfiles"
                | "ls"
                | "readdir"
        ) {
            Self::FileList
        } else if matches!(
            compact.as_str(),
            "search" | "searchtool" | "webfetch" | "websearch"
        ) {
            Self::Search
        } else {
            Self::Tool
        }
    }
}

#[derive(Clone, Debug)]
pub enum DriverEvent {
    /// Client-only acknowledgement that every daemon event through this
    /// sequence has been incorporated into the local session projection.
    /// Providers never emit this and the daemon never serializes it.
    RuntimeEventCursorAdvanced(RuntimeEventCursor),
    Connected {
        provider_cursor: Option<ProviderResumeCursor>,
    },
    /// The provider-owned agent composition this session actually runs. A
    /// fresh Harness session may resolve its deployment default when Waku did
    /// not name one explicitly, so the driver reports the resolved value.
    AgentPresetSelected(Option<String>),
    /// A provider-owned, automatically generated session title. `None`
    /// clears that fallback but never overwrites a user-owned title.
    AutoTitleUpdated(Option<String>),
    /// The slash commands the live process itself reports — Claude's
    /// stream-json init handshake and ACP's `available_commands_update`.
    /// Authoritative over filesystem discovery, which cannot see plugin or
    /// dynamically registered commands.
    AvailableCommands(Vec<ReportedCommand>),
    TurnStarted,
    TextDelta(String),
    ReasoningDelta(String),
    Activity {
        id: Option<String>,
        kind: ActivityKind,
        title: String,
        detail: Option<String>,
        complete: bool,
    },
    RichActivity(ActivityItem),
    /// Session-level work that can outlive the turn which created it. This is
    /// deliberately separate from transcript activities: completing a turn
    /// must not make a detached process or subagent look complete.
    BackgroundWork(BackgroundWorkEvent),
    Permission {
        request_id: String,
        title: String,
        detail: String,
        options: Vec<PermissionOption>,
    },
    /// Structured questions the provider needs answered before it can
    /// continue the active turn. Unlike a permission, this is never
    /// auto-approved: the content itself has to come from the user.
    UserInputRequested {
        request_id: String,
        questions: Vec<UserInputQuestion>,
    },
    ComputerUseUpdated(crate::computer_use::ComputerUseState),
    /// The provider accepted a steering message into the running turn.
    SteerAccepted {
        message: String,
    },
    /// The provider could not steer the running turn (for example it ended
    /// before the request arrived). The app decides the fallback.
    SteerRejected {
        message: String,
        reason: String,
    },
    /// Context-window occupancy reported by the live stream. Fields arrive at
    /// different moments — token counts with each assistant message, the
    /// window size with the settled turn — so each is optional and the app
    /// merges them into [`ContextUsage`].
    UsageUpdated {
        context_tokens: Option<u64>,
        context_window: Option<u64>,
    },
    /// Account-level rate-limit meters carried by the provider's own stream
    /// (Codex's `account/rateLimits/updated`). Same shape the OAuth fetcher
    /// produces for Claude, so the panel renders both identically.
    PlanUsageUpdated(crate::usage::PlanUsage),
    TurnFinished {
        success: bool,
        summary: Option<String>,
    },
    Error(String),
    ProcessExited,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundWorkKind {
    Process,
    Monitor,
    Subagent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundWorkStatus {
    Starting,
    Running,
    Monitoring,
    Stopping,
    Completed,
    Failed,
    Stopped,
    Lost,
}

impl BackgroundWorkStatus {
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Monitoring | Self::Stopping
        )
    }

    pub fn is_stoppable(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Monitoring)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundWorkKey {
    pub kind: BackgroundWorkKind,
    pub provider_id: String,
}

impl BackgroundWorkKey {
    pub fn new(kind: BackgroundWorkKind, provider_id: impl Into<String>) -> Self {
        Self {
            kind,
            provider_id: provider_id.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundWorkItem {
    pub key: BackgroundWorkKey,
    pub title: String,
    pub detail: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub output: Option<String>,
    pub output_truncated: bool,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    /// Whether the provider considers this detached from the foreground turn.
    pub background: bool,
    pub can_stop: bool,
    /// Provider-native identifier used for an authoritative stop request.
    pub control_id: Option<String>,
    /// Transcript activity that created this work, when the provider exposes it.
    pub origin_activity_id: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub parent_id: Option<String>,
    pub status: BackgroundWorkStatus,
}

impl BackgroundWorkItem {
    pub fn new(
        kind: BackgroundWorkKind,
        provider_id: impl Into<String>,
        title: impl Into<String>,
        status: BackgroundWorkStatus,
    ) -> Self {
        let now = unix_time_millis();
        Self {
            key: BackgroundWorkKey::new(kind, provider_id),
            title: title.into(),
            detail: None,
            command: None,
            cwd: None,
            output: None,
            output_truncated: false,
            started_at_ms: now,
            updated_at_ms: now,
            duration_ms: None,
            exit_code: None,
            background: false,
            can_stop: false,
            control_id: None,
            origin_activity_id: None,
            role: None,
            model: None,
            parent_id: None,
            status,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BackgroundWorkEvent {
    Upsert(BackgroundWorkItem),
    OutputDelta {
        key: BackgroundWorkKey,
        delta: String,
    },
    /// Authoritative snapshot of the provider's detached terminal registry.
    ReconcileProcesses {
        items: Vec<BackgroundWorkItem>,
    },
    /// Authoritative snapshot of all provider work still live. Used by
    /// transports which publish a level signal in addition to edge events.
    ReconcileLive {
        items: Vec<BackgroundWorkItem>,
    },
    StopRequested(BackgroundWorkKey),
    StopFailed {
        key: BackgroundWorkKey,
        message: String,
    },
}

/// A slash command a live provider process advertised for its session.
///
/// Claude's init handshake reports bare names; ACP agents report names with
/// descriptions. Sessions persisted by earlier builds stored plain strings,
/// which the untagged repr still accepts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ReportedCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReportedCommandRepr {
    Name(String),
    Full {
        name: String,
        #[serde(default)]
        description: String,
    },
}

impl From<ReportedCommandRepr> for ReportedCommand {
    fn from(repr: ReportedCommandRepr) -> Self {
        match repr {
            ReportedCommandRepr::Name(name) => Self {
                name,
                description: String::new(),
            },
            ReportedCommandRepr::Full { name, description } => Self { name, description },
        }
    }
}

impl<'de> Deserialize<'de> for ReportedCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ReportedCommandRepr::deserialize(deserializer).map(Into::into)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub allow: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UserInputOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<UserInputOption>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswer {
    pub question_id: String,
    pub answers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ActivityFileChange {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
}

impl ActivityFileChange {
    pub fn display_name(&self) -> &str {
        Path::new(&self.path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.path)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ActivityItem {
    pub id: Uuid,
    #[serde(default)]
    pub source_id: Option<String>,
    pub kind: ActivityKind,
    pub title: String,
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Images returned by a tool, kept separate from text so large data URLs
    /// are never truncated or treated as literal activity output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_urls: Vec<String>,
    #[serde(default)]
    pub failed: bool,
    pub complete: bool,
    /// Provider-neutral edit metadata prepared when the tool event arrives.
    /// Rendering reads this directly instead of reparsing potentially large
    /// patches on every frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_changes: Vec<ActivityFileChange>,
    /// Compact subject prepared from native tool input (a file, query,
    /// directory, or command). The row builder only formats this cached value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_target: Option<String>,
    /// Human-authored command description prepared from native tool input.
    /// This stays separate from `display_target` so the UI can prefer a short
    /// label without discarding the raw command used by detail views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_description: Option<String>,
    /// Native model reasoning carried by the same ordered activity stream as
    /// tool work. Generic provider `think` tools can still use the ordinary
    /// activity fields and leave this empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningBlock>,
}

impl ActivityItem {
    pub fn new(
        source_id: Option<String>,
        kind: ActivityKind,
        title: impl Into<String>,
        detail: Option<String>,
        complete: bool,
    ) -> Self {
        let title = title.into();
        let display_target = fallback_activity_display_target(kind, &title);
        Self {
            id: Uuid::new_v4(),
            source_id,
            kind,
            title,
            detail,
            arguments: None,
            output: None,
            image_urls: Vec::new(),
            failed: false,
            complete,
            file_changes: Vec::new(),
            display_target,
            display_description: None,
            reasoning: None,
        }
    }

    pub fn from_reasoning(reasoning: ReasoningBlock, complete: bool) -> Self {
        Self {
            reasoning: Some(reasoning),
            ..Self::new(None, ActivityKind::Reasoning, "Reasoning", None, complete)
        }
    }

    pub fn with_arguments(mut self, arguments: Option<String>) -> Self {
        self.arguments = arguments;
        self.refresh_activity_metadata();
        self
    }

    pub fn with_activity_source(mut self, source: Option<&serde_json::Value>) -> Self {
        if let Some(source) = source {
            self.refresh_activity_metadata_from_value(source);
        }
        self
    }

    pub fn with_output(mut self, output: Option<String>) -> Self {
        self.output = output;
        self.refresh_command_output();
        self
    }

    pub fn with_image_urls(mut self, image_urls: Vec<String>) -> Self {
        self.image_urls = image_urls;
        self
    }

    pub fn with_failed(mut self, failed: bool) -> Self {
        self.failed = failed;
        self
    }

    /// Extracts the common tool-input shapes emitted by every provider. This
    /// runs while handling an event (and once for legacy persisted rows), never
    /// from a transcript row builder.
    pub fn refresh_activity_metadata(&mut self) {
        if self.kind != ActivityKind::FileChange {
            self.file_changes.clear();
        }

        let source = self
            .arguments
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty());
        if let Some(source) = source {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(source) {
                self.refresh_activity_metadata_from_value(&value);
            } else if self.kind == ActivityKind::FileChange {
                let extracted = parse_patch_file_changes(source);
                if !extracted.is_empty() {
                    self.file_changes = extracted;
                }
            }
        }
        if self.kind == ActivityKind::Command {
            self.arguments = self
                .arguments
                .take()
                .and_then(normalize_command_activity_command);
            if let Some(command) = self.arguments.as_deref() {
                self.display_target = Some(compact_activity_target(command));
            }
        }
        if self.display_target.is_none() {
            self.display_target = fallback_activity_display_target(self.kind, &self.title);
        }
        self.refresh_command_output();
    }

    fn refresh_activity_metadata_from_value(&mut self, source: &serde_json::Value) {
        if self.kind == ActivityKind::FileChange {
            let mut extracted = Vec::new();
            extract_file_changes_from_value(source, &mut extracted, 0);
            if !extracted.is_empty() {
                self.file_changes = extracted;
            }
        }
        if let Some(target) = extract_activity_display_target(self.kind, source) {
            self.display_target = Some(target);
        }
        if self.kind == ActivityKind::Command
            && let Some(description) = find_activity_string(source, &["description"], 0)
        {
            self.display_description = Some(compact_activity_target(&description));
        }
    }

    fn refresh_command_output(&mut self) {
        if self.kind == ActivityKind::Command {
            self.output = self
                .output
                .take()
                .and_then(normalize_command_activity_output);
        }
    }
}

fn normalize_command_activity_command(source: String) -> Option<String> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
        return Some(source.to_owned());
    };
    match &value {
        serde_json::Value::String(command) => non_empty_activity_text(command),
        serde_json::Value::Array(parts) if parts.iter().all(|part| part.as_str().is_some()) => {
            non_empty_activity_text(
                &parts
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        serde_json::Value::Object(_) => {
            find_activity_string(&value, &["command", "cmd", "script"], 0)
                .and_then(|command| non_empty_activity_text(&command))
        }
        _ => Some(source.to_owned()),
    }
}

fn normalize_command_activity_output(output: String) -> Option<String> {
    let output = output.trim();
    if output.is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Some(output.to_owned());
    };
    if !is_command_output_envelope(&value) {
        return Some(output.to_owned());
    }
    command_output_envelope_text(&value, 0)
}

fn non_empty_activity_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn is_command_output_envelope(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.contains_key("aggregatedOutput")
        || object.contains_key("structuredContent")
        || object.contains_key("stdout")
        || object.contains_key("stderr")
        || object.contains_key("toolCallId")
        || object.contains_key("tool_call_id")
    {
        return true;
    }
    let output_field = object.contains_key("content")
        || object.contains_key("result")
        || object.contains_key("output");
    if output_field && (object.contains_key("isError") || object.contains_key("is_error")) {
        return true;
    }
    let item_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(|value| {
            value
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        });
    if item_type.as_deref().is_some_and(|item_type| {
        matches!(
            item_type,
            "toolresult" | "tooloutput" | "commandresult" | "commandoutput" | "result" | "text"
        )
    }) {
        return true;
    }
    object
        .get("content")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            !items.is_empty()
                && items.iter().all(|item| {
                    item.as_object().is_some_and(|item| {
                        item.get("type")
                            .and_then(serde_json::Value::as_str)
                            .is_some()
                    })
                })
        })
}

fn command_output_envelope_text(value: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match value {
        serde_json::Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            if let Ok(nested) = serde_json::from_str::<serde_json::Value>(text)
                && is_command_output_envelope(&nested)
            {
                return command_output_envelope_text(&nested, depth + 1);
            }
            Some(text.to_owned())
        }
        serde_json::Value::Array(items) => {
            let text = items
                .iter()
                .filter(|item| !is_command_output_image(item))
                .filter_map(|item| command_output_content_text(item, depth + 1))
                .collect::<Vec<_>>()
                .join("\n\n");
            non_empty_activity_text(&text)
        }
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(serde_json::Value::as_str) == Some("text") {
                return object
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .and_then(non_empty_activity_text);
            }
            if let Some(structured) = object
                .get("structuredContent")
                .filter(|value| !value.is_null())
            {
                return serde_json::to_string_pretty(structured)
                    .ok()
                    .and_then(|text| non_empty_activity_text(&text));
            }
            if let Some(output) = object
                .get("aggregatedOutput")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_activity_text)
            {
                return Some(output);
            }
            let streams = ["stdout", "stderr"]
                .into_iter()
                .filter_map(|key| {
                    object
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .and_then(non_empty_activity_text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(streams) = non_empty_activity_text(&streams) {
                return Some(streams);
            }
            ["content", "result", "output", "message", "text"]
                .into_iter()
                .find_map(|key| {
                    object
                        .get(key)
                        .filter(|value| !value.is_null())
                        .and_then(|value| command_output_content_text(value, depth + 1))
                })
        }
        serde_json::Value::Null => None,
        value => non_empty_activity_text(&value.to_string()),
    }
}

fn command_output_content_text(value: &serde_json::Value, depth: usize) -> Option<String> {
    if is_command_output_envelope(value) || value.is_array() || value.is_string() {
        return command_output_envelope_text(value, depth);
    }
    if is_command_output_image(value) {
        return None;
    }
    serde_json::to_string_pretty(value)
        .ok()
        .and_then(|text| non_empty_activity_text(&text))
}

fn is_command_output_image(value: &serde_json::Value) -> bool {
    let item_type = value.get("type").and_then(serde_json::Value::as_str);
    let mime = value
        .get("mime")
        .or_else(|| value.get("mimeType"))
        .or_else(|| value.get("mime_type"))
        .and_then(serde_json::Value::as_str);
    matches!(item_type, Some("image" | "inputImage"))
        || (item_type == Some("file") && mime.is_some_and(|mime| mime.starts_with("image/")))
}

fn fallback_activity_display_target(kind: ActivityKind, title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() || is_generic_activity_title(kind, title) {
        return None;
    }
    (matches!(kind, ActivityKind::FileRead | ActivityKind::FileList)
        && (title.contains('/') || title.contains('\\') || Path::new(title).extension().is_some()))
    .then(|| compact_activity_target(title))
}

pub fn is_generic_activity_title(kind: ActivityKind, title: &str) -> bool {
    // Classification falling back to `Tool` only means the name is not one of
    // the semantic kinds above. It does not make the provider-supplied tool
    // name generic: otherwise every unknown tool (for example
    // `AskUserQuestion`) loses its identity in the transcript.
    if kind != ActivityKind::Tool && ActivityKind::from_tool_name(title) == kind {
        return true;
    }
    match kind {
        ActivityKind::Command => title == tr!("activity.run_command"),
        ActivityKind::FileChange => {
            title == tr!("activity.edit_file") || title == tr!("activity.write_file")
        }
        ActivityKind::FileRead => title == tr!("activity.read_file"),
        ActivityKind::FileSearch => {
            title == tr!("activity.search_files") || title == tr!("activity.find_files")
        }
        ActivityKind::FileList => title == tr!("activity.list_files"),
        ActivityKind::Plan => title == tr!("activity.plan_updated"),
        ActivityKind::Tool => title.eq_ignore_ascii_case("tool") || title == tr!("activity.tool"),
        _ => false,
    }
}

fn extract_activity_display_target(
    kind: ActivityKind,
    source: &serde_json::Value,
) -> Option<String> {
    let keys: &[&str] = match kind {
        ActivityKind::Command => &["command", "cmd"],
        ActivityKind::FileRead => &[
            "filePath",
            "file_path",
            "path",
            "targetFile",
            "target_file",
            "notebookPath",
            "notebook_path",
        ],
        ActivityKind::FileSearch => &["pattern", "query", "regex", "glob"],
        ActivityKind::FileList => &["path", "directory", "dir", "root"],
        ActivityKind::Search => &["query", "queries"],
        ActivityKind::Tool => &["title"],
        _ => return None,
    };
    find_activity_string(source, keys, 0).map(|value| compact_activity_target(&value))
}

fn find_activity_string(value: &serde_json::Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match value {
        serde_json::Value::String(value) => serde_json::from_str::<serde_json::Value>(value)
            .ok()
            .and_then(|nested| find_activity_string(&nested, keys, depth + 1))
            .or_else(|| {
                let value = value.trim();
                (!value.is_empty()).then(|| value.to_owned())
            }),
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| find_activity_string(value, keys, depth + 1)),
        serde_json::Value::Object(object) => {
            for key in keys {
                let Some(value) = object.get(*key) else {
                    continue;
                };
                if let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) {
                    return Some(value.to_owned());
                }
                if let Some(value) = value.as_array().and_then(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .find(|value| !value.trim().is_empty())
                }) {
                    return Some(value.to_owned());
                }
            }
            for key in [
                "action",
                "arguments",
                "args",
                "input",
                "params",
                "rawInput",
                "raw_input",
                "toolInput",
                "tool_input",
            ] {
                if let Some(value) = object.get(key)
                    && let Some(found) = find_activity_string(value, keys, depth + 1)
                {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn compact_activity_target(value: &str) -> String {
    const MAX_CHARS: usize = 240;
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_CHARS {
        return compact;
    }
    compact
        .chars()
        .take(MAX_CHARS - 1)
        .chain(std::iter::once('…'))
        .collect()
}

fn extract_file_changes_from_value(
    value: &serde_json::Value,
    changes: &mut Vec<ActivityFileChange>,
    depth: usize,
) {
    if depth > 4 {
        return;
    }
    match value {
        serde_json::Value::String(text) => {
            if let Ok(nested) = serde_json::from_str::<serde_json::Value>(text) {
                extract_file_changes_from_value(&nested, changes, depth + 1);
            } else {
                extend_file_changes(changes, parse_patch_file_changes(text));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(change) = structured_file_change(item, None) {
                    merge_file_change(changes, change);
                } else {
                    extract_file_changes_from_value(item, changes, depth + 1);
                }
            }
        }
        serde_json::Value::Object(object) => {
            for key in ["changes", "fileChanges", "file_changes"] {
                let Some(collection) = object.get(key) else {
                    continue;
                };
                match collection {
                    serde_json::Value::Array(items) => {
                        for item in items {
                            if let Some(change) = structured_file_change(item, None) {
                                merge_file_change(changes, change);
                            } else {
                                extract_file_changes_from_value(item, changes, depth + 1);
                            }
                        }
                    }
                    serde_json::Value::Object(items) => {
                        for (path, item) in items {
                            if let Some(change) = structured_file_change(item, Some(path)) {
                                merge_file_change(changes, change);
                            }
                        }
                    }
                    _ => {}
                }
            }

            for key in ["patch", "patchText", "patch_text"] {
                if let Some(patch) = object.get(key).and_then(serde_json::Value::as_str) {
                    extend_file_changes(changes, parse_patch_file_changes(patch));
                }
            }

            let structured = structured_file_change(value, None);
            if structured.is_none() {
                for key in ["diff", "unifiedDiff", "unified_diff"] {
                    if let Some(patch) = object.get(key).and_then(serde_json::Value::as_str) {
                        extend_file_changes(changes, parse_patch_file_changes(patch));
                    }
                }
            }
            if let Some(change) = structured {
                merge_file_change(changes, change);
            }

            for key in [
                "arguments",
                "args",
                "input",
                "rawInput",
                "raw_input",
                "toolInput",
                "tool_input",
            ] {
                if let Some(nested) = object.get(key) {
                    extract_file_changes_from_value(nested, changes, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn structured_file_change(
    value: &serde_json::Value,
    fallback_path: Option<&str>,
) -> Option<ActivityFileChange> {
    let object = value.as_object()?;
    let path = [
        "path",
        "filePath",
        "file_path",
        "filename",
        "fileName",
        "targetFile",
        "target_file",
        "notebookPath",
        "notebook_path",
    ]
    .into_iter()
    .find_map(|key| object.get(key).and_then(serde_json::Value::as_str))
    .or(fallback_path)?
    .trim();
    if path.is_empty() {
        return None;
    }

    let diff = ["diff", "unifiedDiff", "unified_diff"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(serde_json::Value::as_str));
    let (mut additions, mut deletions) = diff
        .map(diff_line_counts)
        .map(|(additions, deletions)| (Some(additions), Some(deletions)))
        .unwrap_or((None, None));

    let old = [
        "oldString",
        "old_string",
        "oldText",
        "old_text",
        "oldContent",
        "old_content",
    ]
    .into_iter()
    .find_map(|key| object.get(key).and_then(serde_json::Value::as_str));
    let new = [
        "newString",
        "new_string",
        "newText",
        "new_text",
        "newContent",
        "new_content",
    ]
    .into_iter()
    .find_map(|key| object.get(key).and_then(serde_json::Value::as_str));
    if let (Some(old), Some(new)) = (old, new) {
        let (added, deleted) = replacement_line_counts(old, new);
        additions = Some(added);
        deletions = Some(deleted);
    } else if let Some(edits) = object.get("edits").and_then(serde_json::Value::as_array) {
        let mut added = 0;
        let mut deleted = 0;
        let mut counted = false;
        for edit in edits {
            let Some(edit) = edit.as_object() else {
                continue;
            };
            let old = ["oldString", "old_string", "oldText", "old_text"]
                .into_iter()
                .find_map(|key| edit.get(key).and_then(serde_json::Value::as_str));
            let new = ["newString", "new_string", "newText", "new_text"]
                .into_iter()
                .find_map(|key| edit.get(key).and_then(serde_json::Value::as_str));
            if let (Some(old), Some(new)) = (old, new) {
                let (edit_added, edit_deleted) = replacement_line_counts(old, new);
                added += edit_added;
                deleted += edit_deleted;
                counted = true;
            }
        }
        if counted {
            additions = Some(added);
            deletions = Some(deleted);
        }
    }

    let change_type = object
        .get("kind")
        .and_then(|kind| {
            kind.as_str()
                .or_else(|| kind.get("type").and_then(serde_json::Value::as_str))
        })
        .or_else(|| object.get("type").and_then(serde_json::Value::as_str));
    if additions.is_none()
        && deletions.is_none()
        && let Some(content) = object.get("content").and_then(serde_json::Value::as_str)
    {
        match change_type {
            Some("add" | "create") => {
                additions = Some(logical_line_count(content));
                deletions = Some(0);
            }
            Some("delete") => {
                additions = Some(0);
                deletions = Some(logical_line_count(content));
            }
            _ => {}
        }
    }

    Some(ActivityFileChange {
        path: path.to_owned(),
        additions,
        deletions,
    })
}

fn parse_patch_file_changes(patch: &str) -> Vec<ActivityFileChange> {
    #[derive(Default)]
    struct PendingChange {
        path: String,
        additions: u64,
        deletions: u64,
        count_lines: bool,
    }

    fn finish(pending: &mut Option<PendingChange>, changes: &mut Vec<ActivityFileChange>) {
        let Some(pending) = pending.take() else {
            return;
        };
        if pending.path.is_empty() || pending.path == "/dev/null" {
            return;
        }
        merge_file_change(
            changes,
            ActivityFileChange {
                path: pending.path,
                additions: Some(pending.additions),
                deletions: Some(pending.deletions),
            },
        );
    }

    let mut changes = Vec::new();
    let mut pending: Option<PendingChange> = None;
    for line in patch.lines() {
        let file_marker = ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
            .into_iter()
            .find_map(|prefix| line.strip_prefix(prefix));
        if let Some(path) = file_marker {
            finish(&mut pending, &mut changes);
            pending = Some(PendingChange {
                path: path.trim().to_owned(),
                count_lines: true,
                ..PendingChange::default()
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            if let Some(pending) = pending.as_mut() {
                pending.path = path.trim().to_owned();
            }
            continue;
        }
        if let Some(paths) = line.strip_prefix("diff --git ") {
            finish(&mut pending, &mut changes);
            let path = paths
                .split_whitespace()
                .next_back()
                .map(clean_diff_path)
                .unwrap_or_default();
            pending = Some(PendingChange {
                path,
                ..PendingChange::default()
            });
            continue;
        }
        if line.starts_with("@@") {
            if let Some(pending) = pending.as_mut() {
                pending.count_lines = true;
            }
            continue;
        }
        if pending.as_ref().is_none_or(|pending| !pending.count_lines)
            && let Some(path) = line.strip_prefix("+++ ")
        {
            let path = clean_diff_path(path);
            if path != "/dev/null" {
                if let Some(pending) = pending.as_mut() {
                    pending.path = path;
                } else {
                    pending = Some(PendingChange {
                        path,
                        ..PendingChange::default()
                    });
                }
            }
            continue;
        }
        let Some(pending) = pending.as_mut() else {
            continue;
        };
        if !pending.count_lines {
            continue;
        }
        if line.starts_with('+') {
            pending.additions += 1;
        } else if line.starts_with('-') {
            pending.deletions += 1;
        }
    }
    finish(&mut pending, &mut changes);
    changes
}

fn clean_diff_path(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .strip_prefix("a/")
        .or_else(|| path.trim().trim_matches('"').strip_prefix("b/"))
        .unwrap_or_else(|| path.trim().trim_matches('"'))
        .to_owned()
}

fn diff_line_counts(diff: &str) -> (u64, u64) {
    let mut additions = 0;
    let mut deletions = 0;
    let has_hunks = diff.lines().any(|line| line.starts_with("@@"));
    let mut count_lines = !has_hunks;
    for line in diff.lines() {
        if line.starts_with("@@") {
            count_lines = true;
            continue;
        }
        if !count_lines || line.starts_with("+++ ") || line.starts_with("--- ") {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

fn logical_line_count(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u64
    }
}

fn replacement_line_counts(old: &str, new: &str) -> (u64, u64) {
    let old = old.lines().collect::<Vec<_>>();
    let new = new.lines().collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > prefix && new_end > prefix && old[old_end - 1] == new[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    ((new_end - prefix) as u64, (old_end - prefix) as u64)
}

fn extend_file_changes(
    changes: &mut Vec<ActivityFileChange>,
    extracted: impl IntoIterator<Item = ActivityFileChange>,
) {
    for change in extracted {
        merge_file_change(changes, change);
    }
}

fn merge_file_change(changes: &mut Vec<ActivityFileChange>, change: ActivityFileChange) {
    if let Some(existing) = changes.iter_mut().find(|item| item.path == change.path) {
        match (existing.additions, change.additions) {
            (Some(existing_count), Some(change_count)) => {
                existing.additions = Some(existing_count + change_count);
            }
            (None, Some(change_count)) => existing.additions = Some(change_count),
            _ => {}
        }
        match (existing.deletions, change.deletions) {
            (Some(existing_count), Some(change_count)) => {
                existing.deletions = Some(existing_count + change_count);
            }
            (None, Some(change_count)) => existing.deletions = Some(change_count),
            _ => {}
        }
    } else {
        changes.push(change);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ReasoningBlock {
    pub content: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
}

#[derive(Clone, Debug, TS)]
pub struct TranscriptBlock {
    /// Render this block immediately after this many persisted messages.
    pub after_message: usize,
    pub turn_id: Option<Uuid>,
    /// Ordered non-message work emitted at this point in the transcript.
    /// The persisted field keeps its historical tagged shape so existing
    /// sessions remain readable while the runtime model stays activity-only.
    #[ts(rename = "content", as = "StoredTranscriptBlockContent")]
    pub activities: Vec<ActivityItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "data")]
enum StoredTranscriptBlockContentRef<'a> {
    Activities(&'a [ActivityItem]),
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase", tag = "kind", content = "data")]
pub enum StoredTranscriptBlockContent {
    Reasoning(ReasoningBlock),
    Activities(Vec<ActivityItem>),
}

#[derive(Serialize)]
struct TranscriptBlockRef<'a> {
    after_message: usize,
    turn_id: Option<Uuid>,
    content: StoredTranscriptBlockContentRef<'a>,
}

impl Serialize for TranscriptBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        TranscriptBlockRef {
            after_message: self.after_message,
            turn_id: self.turn_id,
            content: StoredTranscriptBlockContentRef::Activities(&self.activities),
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
struct TranscriptBlockRepr {
    after_message: usize,
    #[serde(default)]
    turn_id: Option<Uuid>,
    content: StoredTranscriptBlockContent,
}

impl<'de> Deserialize<'de> for TranscriptBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let repr = TranscriptBlockRepr::deserialize(deserializer)?;
        let activities = match repr.content {
            StoredTranscriptBlockContent::Reasoning(reasoning) => {
                vec![ActivityItem::from_reasoning(reasoning, true)]
            }
            StoredTranscriptBlockContent::Activities(activities) => activities,
        };
        Ok(Self {
            after_message: repr.after_message,
            turn_id: repr.turn_id,
            activities,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub request_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<PermissionOption>,
}

pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub fn compact_path(path: &Path) -> String {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if components.len() <= 3 {
        return path.display().to_string();
    }
    format!(
        "…/{}/{}",
        components[components.len() - 2],
        components[components.len() - 1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_work_snapshots_have_serializable_named_items() {
        let item = BackgroundWorkItem::new(
            BackgroundWorkKind::Process,
            "process-1",
            "server",
            BackgroundWorkStatus::Running,
        );
        let json =
            serde_json::to_value(BackgroundWorkEvent::ReconcileProcesses { items: vec![item] })
                .unwrap();

        assert_eq!(json["type"], "reconcileProcesses");
        assert_eq!(json["items"][0]["key"]["providerId"], "process-1");
        let BackgroundWorkEvent::ReconcileProcesses { items } =
            serde_json::from_value(json).unwrap()
        else {
            panic!("unexpected background-work event");
        };
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn attachment_messages_keep_transport_and_visible_content_separate() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        let attachment = MessageAttachment {
            path: PathBuf::from("/tmp/reference.png"),
            mention: "/tmp/reference.png".to_owned(),
            name: "reference.png".to_owned(),
            is_dir: false,
            is_image: true,
            blob_reference: Some("waku-blob:ab/reference.png".to_owned()),
        };

        session.begin_turn_with_presentation(
            "compare this @/tmp/reference.png",
            Some("compare this".to_owned()),
            vec![attachment.clone()],
        );

        let message = &session.messages[0];
        assert_eq!(message.content, "compare this @/tmp/reference.png");
        assert_eq!(message.visible_content(), "compare this");
        assert_eq!(message.attachments, vec![attachment]);
    }

    #[test]
    fn tool_names_are_classified_without_substring_false_positives() {
        for name in [
            "read",
            "ReadFile",
            "read_text_file",
            "mcp__filesystem__read_file",
        ] {
            assert_eq!(
                ActivityKind::from_tool_name(name),
                ActivityKind::FileRead,
                "{name}"
            );
        }
        for name in ["grep", "Glob", "fileSearch", "search_files"] {
            assert_eq!(
                ActivityKind::from_tool_name(name),
                ActivityKind::FileSearch,
                "{name}"
            );
        }
        for name in ["ls", "ListDirectory", "read_dir"] {
            assert_eq!(
                ActivityKind::from_tool_name(name),
                ActivityKind::FileList,
                "{name}"
            );
        }
        for name in ["WriteFile", "applyPatch", "move_file", "str_replace"] {
            assert_eq!(
                ActivityKind::from_tool_name(name),
                ActivityKind::FileChange,
                "{name}"
            );
        }
        for name in ["create_thread", "read_mcp_resource", "list_threads"] {
            assert_eq!(
                ActivityKind::from_tool_name(name),
                ActivityKind::Tool,
                "{name}"
            );
        }
    }

    #[test]
    fn fallback_tool_names_are_not_treated_as_generic_titles() {
        assert!(!is_generic_activity_title(
            ActivityKind::Tool,
            "AskUserQuestion"
        ));
        assert!(!is_generic_activity_title(
            ActivityKind::Tool,
            "mcp__threads__create_thread"
        ));
        assert!(is_generic_activity_title(ActivityKind::Tool, "Tool"));
    }

    #[test]
    fn activity_targets_are_normalized_when_events_arrive() {
        let cases = [
            (
                ActivityKind::FileRead,
                serde_json::json!({"input": {"file_path": "/tmp/waku/src/app.rs"}}),
                "/tmp/waku/src/app.rs",
            ),
            (
                ActivityKind::FileSearch,
                serde_json::json!({"tool_input": {"regex": "ActivityItem"}}),
                "ActivityItem",
            ),
            (
                ActivityKind::FileList,
                serde_json::json!({"arguments": {"directory": "/tmp/waku/src"}}),
                "/tmp/waku/src",
            ),
            (
                ActivityKind::Command,
                serde_json::json!({"args": {"command": "cargo test activity"}}),
                "cargo test activity",
            ),
            (
                ActivityKind::Search,
                serde_json::json!({"action": {"queries": ["Waku GPUI"]}}),
                "Waku GPUI",
            ),
            (
                ActivityKind::FileRead,
                serde_json::json!("/tmp/waku/README.md"),
                "/tmp/waku/README.md",
            ),
        ];

        for (kind, arguments, expected) in cases {
            let activity = ActivityItem::new(None, kind, "tool", None, false)
                .with_arguments(Some(arguments.to_string()));
            assert_eq!(
                activity.display_target.as_deref(),
                Some(expected),
                "{kind:?}"
            );
        }

        let described = ActivityItem::new(None, ActivityKind::Command, "bash", None, false)
            .with_arguments(Some(
                serde_json::json!({
                    "command": "python3 analyze.py",
                    "description": "Analyze color statistics"
                })
                .to_string(),
            ));
        assert_eq!(
            described.display_description.as_deref(),
            Some("Analyze color statistics")
        );
        assert_eq!(described.arguments.as_deref(), Some("python3 analyze.py"));
    }

    #[test]
    fn command_output_unwraps_provider_results_without_rewriting_real_output() {
        let wrapped = serde_json::json!({
            "type": "tool-result",
            "toolCallId": "call-1",
            "content": [{
                "type": "text",
                "text": "first line\n{\"actual\":\"command json\"}"
            }],
            "isError": false
        })
        .to_string();
        let activity = ActivityItem::new(None, ActivityKind::Command, "bash", None, true)
            .with_arguments(Some(
                serde_json::json!({
                    "command": "printf output",
                    "description": "Print output"
                })
                .to_string(),
            ))
            .with_output(Some(wrapped));

        assert_eq!(activity.arguments.as_deref(), Some("printf output"));
        assert_eq!(
            activity.output.as_deref(),
            Some("first line\n{\"actual\":\"command json\"}")
        );

        let json_output = r#"{"content":"this came from the command"}"#;
        let activity = ActivityItem::new(None, ActivityKind::Command, "bash", None, true)
            .with_output(Some(json_output.to_owned()));
        assert_eq!(activity.output.as_deref(), Some(json_output));
    }

    #[test]
    fn legacy_command_metadata_is_normalized_on_refresh() {
        let mut activity = ActivityItem::new(None, ActivityKind::Command, "bash", None, true);
        activity.arguments =
            Some(r#"{"command":"git status","description":"Check status"}"#.into());
        activity.output = Some(
            r#"{"type":"tool-result","content":[{"type":"text","text":"clean"}],"isError":false}"#
                .into(),
        );

        activity.refresh_activity_metadata();

        assert_eq!(activity.arguments.as_deref(), Some("git status"));
        assert_eq!(activity.display_target.as_deref(), Some("git status"));
        assert_eq!(
            activity.display_description.as_deref(),
            Some("Check status")
        );
        assert_eq!(activity.output.as_deref(), Some("clean"));
    }

    #[test]
    fn file_edit_metadata_is_normalized_for_every_provider_shape() {
        let cases = [
            (
                ProviderKind::Codex,
                serde_json::json!([{
                    "path": "src/codex.rs",
                    "diff": "@@ -1 +1,2 @@\n-old\n+new\n+next",
                    "kind": {"type": "update"}
                }]),
                "src/codex.rs",
                2,
                1,
            ),
            (
                ProviderKind::Claude,
                serde_json::json!({
                    "file_path": "src/claude.rs",
                    "old_string": "old\nline",
                    "new_string": "new\nline\nadded"
                }),
                "src/claude.rs",
                3,
                2,
            ),
            (
                ProviderKind::Amp,
                serde_json::json!({
                    "file_path": "src/amp.rs",
                    "old_string": "old",
                    "new_string": "new"
                }),
                "src/amp.rs",
                1,
                1,
            ),
            (
                ProviderKind::Cursor,
                serde_json::json!({
                    "input": {
                        "path": "src/cursor.rs",
                        "oldText": "old",
                        "newText": "new\nmore"
                    }
                }),
                "src/cursor.rs",
                2,
                1,
            ),
            (
                ProviderKind::DeepSeek,
                serde_json::json!({
                    "path": "src/deepseek.rs",
                    "oldText": "old",
                    "newText": "new\nmore"
                }),
                "src/deepseek.rs",
                2,
                1,
            ),
            (
                ProviderKind::OpenCode,
                serde_json::json!({
                    "filePath": "src/opencode.rs",
                    "oldString": "same\nold\nend",
                    "newString": "same\nnew\nend"
                }),
                "src/opencode.rs",
                1,
                1,
            ),
            (
                ProviderKind::Grok,
                serde_json::json!({
                    "tool_input": {
                        "patchText": "*** Begin Patch\n*** Update File: src/grok.rs\n@@\n-old\n+new\n+more\n*** End Patch"
                    }
                }),
                "src/grok.rs",
                2,
                1,
            ),
            (
                ProviderKind::Pi,
                serde_json::json!({
                    "path": "src/pi.rs",
                    "edits": [{"oldText": "old", "newText": "new\nmore"}]
                }),
                "src/pi.rs",
                2,
                1,
            ),
        ];

        for (provider, arguments, path, additions, deletions) in cases {
            let activity = ActivityItem::new(
                Some(format!("{}-edit", provider.id())),
                ActivityKind::FileChange,
                "edit",
                None,
                false,
            )
            .with_arguments(Some(arguments.to_string()));
            assert_eq!(activity.file_changes.len(), 1, "{provider:?}");
            let change = &activity.file_changes[0];
            assert_eq!(change.path, path, "{provider:?}");
            assert_eq!(change.additions, Some(additions), "{provider:?}");
            assert_eq!(change.deletions, Some(deletions), "{provider:?}");
        }
    }

    #[test]
    fn apply_patch_metadata_keeps_each_file_and_its_counts() {
        let activity = ActivityItem::new(
            Some("patch-1".into()),
            ActivityKind::FileChange,
            "apply_patch",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({
                "patch": "*** Begin Patch\n*** Update File: src/one.rs\n@@\n-old\n+new\n*** Add File: src/two.rs\n+first\n+second\n*** End Patch"
            })
            .to_string(),
        ));

        assert_eq!(activity.file_changes.len(), 2);
        assert_eq!(activity.file_changes[0].path, "src/one.rs");
        assert_eq!(activity.file_changes[0].additions, Some(1));
        assert_eq!(activity.file_changes[0].deletions, Some(1));
        assert_eq!(activity.file_changes[1].path, "src/two.rs");
        assert_eq!(activity.file_changes[1].additions, Some(2));
        assert_eq!(activity.file_changes[1].deletions, Some(0));
    }

    #[test]
    fn projectless_projects_use_projects_root_and_recognize_legacy_paths() {
        let home = dirs::home_dir().expect("test user has a home directory");
        let root = home.join(".waku");
        let legacy = Project::from_path(root.clone());
        let legacy_dated = Project::from_path(root.join("2026-08-08/new-chat"));
        let project = Project::from_path(root.join("projects/2026-08-08/new-chat"));
        let ordinary = Project::from_path(home.join("dev/waku"));

        assert!(legacy.is_projectless());
        assert!(legacy_dated.is_projectless());
        assert!(project.is_projectless());
        assert!(!ordinary.is_projectless());
    }

    #[test]
    fn prompt_generates_a_short_session_title() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.set_title_from_prompt("build a really polished local agent interface for rust");
        assert_eq!(
            session.auto_title.as_deref(),
            Some("build a really polished local agent interface")
        );
        assert_eq!(
            session.display_title(),
            "build a really polished local agent interface"
        );
        assert_eq!(session.title, AgentSession::DEFAULT_TITLE);
    }

    #[test]
    fn provider_title_replaces_prompt_fallback_but_not_an_explicit_title() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::OpenCode);
        session.set_title_from_prompt("investigate the broken provider event");

        assert!(session.set_auto_title(Some("Fix provider title events".into())));
        assert_eq!(session.display_title(), "Fix provider title events");

        assert!(session.set_title("  My title  "));
        assert!(session.set_auto_title(Some("A newer provider title".into())));
        assert_eq!(session.display_title(), "My title");
        assert!(!session.set_title("   "));
        assert_eq!(session.display_title(), "My title");
    }

    #[test]
    fn model_selection_keeps_started_sessions_on_their_provider() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        assert!(session.can_choose_model(ProviderKind::Claude));

        session.push_message(MessageRole::User, "first turn");
        assert!(session.can_choose_model(ProviderKind::Codex));
        assert!(!session.can_choose_model(ProviderKind::Claude));
    }

    #[test]
    fn model_selection_waits_for_the_active_turn_to_finish() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.push_message(MessageRole::User, "first turn");

        for status in [
            SessionStatus::Connecting,
            SessionStatus::Working,
            SessionStatus::Waiting,
        ] {
            session.status = status;
            assert!(!session.can_choose_model(ProviderKind::Codex));
        }

        session.status = SessionStatus::Idle;
        assert!(session.can_choose_model(ProviderKind::Codex));
    }

    #[test]
    fn provider_ids_are_stable() {
        assert_eq!(ProviderKind::Amp.id(), "amp");
        assert_eq!(ProviderKind::Claude.id(), "claude");
        assert_eq!(ProviderKind::Codex.command(), "codex");
        assert_eq!(ProviderKind::Cursor.command(), "cursor-agent");
        assert_eq!(ProviderKind::DeepSeek.command(), "dsh");
        assert_eq!(ProviderKind::OpenCode.command(), "opencode");
        assert_eq!(ProviderKind::Grok.command(), "grok");
        assert_eq!(ProviderKind::Pi.command(), "pi");
    }

    #[test]
    fn native_conversation_actions_include_every_provider() {
        for provider in [
            ProviderKind::Amp,
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Cursor,
            ProviderKind::DeepSeek,
            ProviderKind::OpenCode,
            ProviderKind::Grok,
            ProviderKind::Pi,
        ] {
            assert!(provider.supports_conversation_fork());
            assert!(provider.supports_conversation_rollback());
        }
    }

    #[test]
    fn only_dynamic_provider_catalogs_are_discovered() {
        assert!(!ProviderKind::Amp.supports_model_discovery());
        assert!(!ProviderKind::Claude.supports_model_discovery());
        assert!(ProviderKind::Codex.supports_model_discovery());
        assert!(ProviderKind::Cursor.supports_model_discovery());
        assert!(ProviderKind::DeepSeek.supports_model_discovery());
        assert!(ProviderKind::OpenCode.supports_model_discovery());
        assert!(ProviderKind::Grok.supports_model_discovery());
        assert!(ProviderKind::Pi.supports_model_discovery());
    }

    #[test]
    fn prompt_title_truncation_is_unicode_safe() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Claude);
        let prompt = "界".repeat(70);
        session.set_title_from_prompt(&prompt);
        let title = session.auto_title.as_deref().unwrap();
        assert_eq!(title.chars().count(), 54);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_failed_preparation_unwinds_the_turn_it_eagerly_began() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        // A first prompt: the unwind restores the default title because the
        // prompt returns to the composer, but keeps the submission activity.
        session.set_title_from_prompt("Build the thing");
        let turn_id = session.begin_turn("Build the thing");
        let submitted_at = session.last_reply_at;
        session.unwind_unstarted_turn(turn_id);
        assert!(session.turns.is_empty());
        assert!(session.messages.is_empty());
        assert_eq!(session.last_reply_at, submitted_at);
        assert_eq!(session.title, AgentSession::DEFAULT_TITLE);
        assert!(session.auto_title.is_none());

        // A follow-up prompt unwinds only itself.
        let first = session.begin_turn("first");
        session.push_message(MessageRole::Assistant, "done");
        session.finish_active_turn(TurnStatus::Completed);
        session.set_title_from_prompt("first");
        let follow_up = session.begin_turn("second");
        let follow_up_submitted_at = session.last_reply_at;
        session.unwind_unstarted_turn(follow_up);
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].id, first);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.last_reply_at, follow_up_submitted_at);

        // A turn the provider has already started never unwinds — losing a
        // live conversation turn would desync the provider transcript.
        let started = session.begin_turn("third");
        session.mark_active_turn_provider_started();
        session.unwind_unstarted_turn(started);
        assert_eq!(session.turns.len(), 2);
        assert_eq!(session.messages.len(), 3);
    }

    #[test]
    fn turn_truncation_removes_owned_messages_and_blocks() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        let first_turn = session.begin_turn("first");
        session.push_message(MessageRole::Assistant, "first answer");
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 2,
            turn_id: Some(first_turn),
            activities: Vec::new(),
        });
        session.finish_active_turn(TurnStatus::Completed);

        let second_turn = session.begin_turn("second");
        session.push_message(MessageRole::Assistant, "second answer");
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 4,
            turn_id: Some(second_turn),
            activities: Vec::new(),
        });
        session.finish_active_turn(TurnStatus::Completed);

        session.truncate_after_turn(1);

        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].id, first_turn);
        assert_eq!(session.messages.len(), 2);
        assert!(
            session
                .messages
                .iter()
                .all(|message| message.turn_id == Some(first_turn))
        );
        assert_eq!(session.transcript_blocks.len(), 1);
        assert_eq!(session.transcript_blocks[0].turn_id, Some(first_turn));
        assert_eq!(session.transcript_blocks[0].after_message, 2);
    }

    #[test]
    fn response_fork_is_a_distinct_idle_session_through_the_selected_turn() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        let first_turn = session.begin_turn("first");
        let first_message = session.push_message(MessageRole::Assistant, "first answer");
        session.finish_active_turn(TurnStatus::Completed);
        session.begin_turn("second");
        session.push_message(MessageRole::Assistant, "second answer");
        session.finish_active_turn(TurnStatus::Completed);

        let fork = session
            .fork_through_turn(
                1,
                ProviderResumeCursor::Codex {
                    thread_id: "forked-thread".into(),
                },
                "New task (2)",
            )
            .unwrap();

        assert_ne!(fork.id, session.id);
        assert_eq!(fork.title, AgentSession::DEFAULT_TITLE);
        assert_eq!(fork.auto_title.as_deref(), Some("New task (2)"));
        assert_eq!(fork.status, SessionStatus::Idle);
        assert_eq!(fork.turns.len(), 1);
        assert_eq!(fork.messages.len(), 2);
        assert_ne!(fork.turns[0].id, first_turn);
        assert_ne!(fork.messages[1].id, first_message);
        assert!(
            fork.messages
                .iter()
                .all(|message| message.turn_id == Some(fork.turns[0].id))
        );
        assert!(matches!(
            fork.provider_cursor,
            Some(ProviderResumeCursor::Codex { ref thread_id }) if thread_id == "forked-thread"
        ));
    }

    #[test]
    fn queued_follow_ups_stay_with_the_source_session_not_the_fork() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        session.begin_turn("first");
        session.push_message(MessageRole::Assistant, "first answer");
        session.finish_active_turn(TurnStatus::Completed);
        session
            .queued_messages
            .push(QueuedMessage::new("after you finish, also…"));

        let fork = session
            .fork_through_turn(
                1,
                ProviderResumeCursor::Codex {
                    thread_id: "forked-thread".into(),
                },
                "New task (2)",
            )
            .unwrap();

        assert_eq!(session.queued_messages.len(), 1);
        assert!(fork.queued_messages.is_empty());
    }

    #[test]
    fn follow_up_queue_round_trips_through_serde() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session
            .queued_messages
            .push(QueuedMessage::new("first follow-up"));
        session
            .queued_messages
            .push(QueuedMessage::new("second follow-up"));

        let value = serde_json::to_value(&session).unwrap();
        assert!(value["queued_messages"].is_array());
        let restored: AgentSession = serde_json::from_value(value).unwrap();
        assert_eq!(restored.queued_messages.len(), 2);
        assert_eq!(restored.queued_messages[0].content, "first follow-up");
        assert_eq!(restored.queued_messages[1].content, "second follow-up");
        assert_ne!(
            restored.queued_messages[0].id,
            restored.queued_messages[1].id
        );

        // Sessions without the field (older state files) deserialize as empty.
        let mut legacy = serde_json::to_value(&session).unwrap();
        legacy.as_object_mut().unwrap().remove("queued_messages");
        let legacy_session: AgentSession = serde_json::from_value(legacy).unwrap();
        assert!(legacy_session.queued_messages.is_empty());
    }

    #[test]
    fn planned_worktree_base_branch_is_optional_and_round_trips() {
        let legacy: SessionWorkspace =
            serde_json::from_value(serde_json::json!({ "kind": "newWorktree" })).unwrap();
        assert_eq!(legacy, SessionWorkspace::NewWorktree { base_branch: None });

        let selected = SessionWorkspace::NewWorktree {
            base_branch: Some("release/next".into()),
        };
        let restored = serde_json::from_value(serde_json::to_value(&selected).unwrap()).unwrap();
        assert_eq!(selected, restored);
    }

    #[test]
    fn busy_statuses_cover_connecting_working_and_waiting() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        for status in [
            SessionStatus::Connecting,
            SessionStatus::Working,
            SessionStatus::Waiting,
        ] {
            session.status = status;
            assert!(session.is_busy());
        }
        for status in [SessionStatus::Idle, SessionStatus::Failed] {
            session.status = status;
            assert!(!session.is_busy());
        }
    }

    #[test]
    fn provider_resume_cursor_is_explicitly_tagged() {
        let cursor = ProviderResumeCursor::Claude {
            session_id: "session-1".into(),
            resume_at: Some("message-9".into()),
        };
        let value = serde_json::to_value(&cursor).unwrap();
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["resumeAt"], "message-9");

        let cursor = ProviderResumeCursor::Cursor {
            session_id: String::new(),
            fork_context: Some("[]".into()),
        };
        let value = serde_json::to_value(&cursor).unwrap();
        assert_eq!(value["provider"], "cursor");
        assert_eq!(value["sessionId"], "");
        assert_eq!(value["forkContext"], "[]");
    }

    #[test]
    fn native_rollback_count_ignores_turns_that_never_reached_the_provider() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        session.begin_turn("first");
        session.mark_active_turn_provider_started();
        session.finish_active_turn(TurnStatus::Completed);
        session.begin_turn("failed locally");
        session.finish_active_turn(TurnStatus::Failed);
        session.begin_turn("third");
        session.mark_active_turn_provider_started();
        session.finish_active_turn(TurnStatus::Completed);

        assert_eq!(session.provider_turns_after(1), 1);
        assert_eq!(session.provider_turns_after(2), 1);
        assert_eq!(session.provider_turns_after(3), 0);
    }

    #[test]
    fn legacy_empty_search_titles_are_repaired() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities: vec![ActivityItem::new(
                Some("search-1".into()),
                ActivityKind::Search,
                "Search for ",
                None,
                true,
            )],
        });

        session.migrate_legacy_state();

        let activities = &session.transcript_blocks[0].activities;
        assert_eq!(activities[0].title, "Browsed the web");
    }

    #[test]
    fn legacy_reasoning_blocks_deserialize_as_reasoning_activities() {
        let legacy = serde_json::json!({
            "after_message": 2,
            "turn_id": null,
            "content": {
                "kind": "reasoning",
                "data": {
                    "content": "Checking the source",
                    "started_at_ms": 1_000,
                    "finished_at_ms": 2_500
                }
            }
        });

        let block: TranscriptBlock = serde_json::from_value(legacy).unwrap();
        assert_eq!(block.activities.len(), 1);
        let activity = &block.activities[0];
        assert_eq!(activity.kind, ActivityKind::Reasoning);
        assert!(activity.complete);
        assert_eq!(
            activity
                .reasoning
                .as_ref()
                .map(|reasoning| reasoning.content.as_str()),
            Some("Checking the source")
        );

        let stored = serde_json::to_value(block).unwrap();
        assert_eq!(stored["content"]["kind"], "activities");
        assert_eq!(
            stored["content"]["data"][0]["reasoning"]["content"],
            "Checking the source"
        );
    }

    #[test]
    fn adjacent_legacy_work_blocks_merge_during_session_migration() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.transcript_blocks.extend([
            TranscriptBlock {
                after_message: 1,
                turn_id: None,
                activities: vec![ActivityItem::from_reasoning(
                    ReasoningBlock {
                        content: "Looking around".into(),
                        started_at_ms: 1_000,
                        finished_at_ms: 2_000,
                    },
                    true,
                )],
            },
            TranscriptBlock {
                after_message: 1,
                turn_id: None,
                activities: vec![ActivityItem::new(
                    None,
                    ActivityKind::Command,
                    "Ran tests",
                    None,
                    true,
                )],
            },
        ]);

        session.migrate_legacy_state();

        assert_eq!(session.transcript_blocks.len(), 1);
        assert_eq!(session.transcript_blocks[0].activities.len(), 2);
        assert_eq!(
            session.transcript_blocks[0]
                .activities
                .iter()
                .map(|activity| activity.kind)
                .collect::<Vec<_>>(),
            [ActivityKind::Reasoning, ActivityKind::Command]
        );
    }

    #[test]
    fn legacy_file_edit_details_are_promoted_to_arguments_and_metadata() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::OpenCode);
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities: vec![ActivityItem::new(
                None,
                ActivityKind::FileChange,
                "edit",
                Some(
                    serde_json::json!({
                        "filePath": "/tmp/waku/README.md",
                        "oldString": "old",
                        "newString": "new\nmore"
                    })
                    .to_string(),
                ),
                true,
            )],
        });

        session.migrate_legacy_state();

        let activities = &session.transcript_blocks[0].activities;
        assert!(activities[0].detail.is_none());
        assert!(activities[0].arguments.is_some());
        assert_eq!(activities[0].file_changes[0].path, "/tmp/waku/README.md");
        assert_eq!(activities[0].file_changes[0].additions, Some(2));
        assert_eq!(activities[0].file_changes[0].deletions, Some(1));
    }

    #[test]
    fn legacy_file_tools_are_reclassified_and_gain_cached_targets() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::OpenCode);
        let mut cached = ActivityItem::new(None, ActivityKind::FileRead, "read", None, true);
        cached.display_target = Some("/tmp/waku/src/persisted.rs".into());
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities: vec![
                ActivityItem::new(
                    None,
                    ActivityKind::Search,
                    "read",
                    Some(r#"{"filePath":"/tmp/waku/src/model.rs"}"#.into()),
                    true,
                ),
                ActivityItem::new(
                    None,
                    ActivityKind::Tool,
                    "glob",
                    Some(r#"{"pattern":"src/**/*.rs"}"#.into()),
                    true,
                ),
                cached,
            ],
        });

        session.migrate_legacy_state();

        let activities = &session.transcript_blocks[0].activities;
        assert_eq!(activities[0].kind, ActivityKind::FileRead);
        assert_eq!(
            activities[0].display_target.as_deref(),
            Some("/tmp/waku/src/model.rs")
        );
        assert_eq!(activities[1].kind, ActivityKind::FileSearch);
        assert_eq!(activities[1].display_target.as_deref(), Some("src/**/*.rs"));
        assert_eq!(
            activities[2].display_target.as_deref(),
            Some("/tmp/waku/src/persisted.rs")
        );
        assert!(
            activities[..2]
                .iter()
                .all(|activity| activity.detail.is_none())
        );
        assert!(
            activities[..2]
                .iter()
                .all(|activity| activity.arguments.is_some())
        );
    }

    #[test]
    fn legacy_codex_citation_markers_are_removed() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.messages.push(Message::new(
            MessageRole::Assistant,
            "Claim.\u{e200}cite\u{e202}turn3view0\u{e202}turn2view2\u{e201}\nNext.",
        ));

        session.migrate_legacy_state();

        assert_eq!(session.messages[0].content, "Claim.\nNext.");
    }

    #[test]
    fn legacy_checkpoint_totals_are_backfilled_from_the_file_summary() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.begin_turn("Build it");
        session.finish_active_turn(TurnStatus::Completed);
        let mut serialized = serde_json::to_value(Checkpoint {
            turn_count: 1,
            git_ref: "refs/waku/test".into(),
            status: CheckpointStatus::Ready,
            files: vec![
                CheckpointFile {
                    path: "src/app.rs".into(),
                    additions: 7,
                    deletions: 2,
                },
                CheckpointFile {
                    path: "src/model.rs".into(),
                    additions: 3,
                    deletions: 5,
                },
            ],
            additions: 10,
            deletions: 7,
            created_at: 1,
        })
        .unwrap();
        let object = serialized.as_object_mut().unwrap();
        object.remove("additions");
        object.remove("deletions");
        let checkpoint: Checkpoint = serde_json::from_value(serialized).unwrap();
        assert_eq!((checkpoint.additions, checkpoint.deletions), (0, 0));
        session.turns[0].checkpoint = Some(checkpoint);

        session.migrate_legacy_state();

        let checkpoint = session.turns[0].checkpoint.as_ref().unwrap();
        assert_eq!((checkpoint.additions, checkpoint.deletions), (10, 7));
        assert!(checkpoint.totals_are_current());
    }

    #[test]
    fn list_projection_never_copies_session_detail() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.title = "Visible title".into();
        session.model = Some("gpt-5".into());
        session.status = SessionStatus::Working;
        session.begin_turn("A large prompt");
        session.messages.push(Message::new(
            MessageRole::Assistant,
            "A large streamed response",
        ));
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 1,
            turn_id: None,
            activities: vec![ActivityItem::new(
                None,
                ActivityKind::Tool,
                "Inspect files",
                None,
                false,
            )],
        });
        session
            .queued_messages
            .push(QueuedMessage::new("Follow up"));

        let projection = session.list_projection();

        assert_eq!(projection.id, session.id);
        assert_eq!(projection.title, "Visible title");
        assert_eq!(projection.model.as_deref(), Some("gpt-5"));
        assert_eq!(projection.status, SessionStatus::Working);
        assert!(!projection.detail_loaded);
        assert!(projection.messages.is_empty());
        assert!(projection.transcript_blocks.is_empty());
        assert!(projection.turns.is_empty());
        assert!(projection.queued_messages.is_empty());
    }
}

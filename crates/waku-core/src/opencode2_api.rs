//! Typed bindings for the OpenCode 2 background service's HTTP API.
//!
//! Every function here is a free function over an [`Endpoint`], so this module
//! knows nothing about who owns the service, how it was discovered, or which
//! session is talking. That keeps the one adopted process out of the wire layer
//! and lets the driver, the shared event reader, and the cold import path all
//! decode the same shapes.
//!
//! Everything blocks on a socket, so every caller must already be off the UI
//! thread — a single one of these calls is several frames of budget.
//!
//! Three properties of this API are traps, and are encoded here rather than
//! left to callers to remember:
//!
//! * The response envelope is per-route. `/api/session*`, `/api/model`,
//!   `/api/agent`, `/api/command` and `/api/skill` wrap their payload in
//!   `{ data }` (the catalogue routes add `{ location, data }`), while
//!   `/api/health` and `POST /api/session/{id}/interrupt` answer bare. One
//!   generic `Envelope<T>` would silently turn a bare payload into a decode
//!   failure, so unwrapping is chosen per route.
//! * Pagination terminates on an EMPTY `data` array, never on an absent
//!   cursor: both `cursor.previous` and `cursor.next` are minted even on the
//!   first and last page. [`page`] collapses that into an absent cursor so a
//!   caller cannot loop forever.
//! * Location scoping is split. `GET /api/session` filters with FLAT
//!   `?directory=`, every other location-aware route takes deepObject
//!   `?location[directory]=…&location[workspace]=…` (the key is `workspace`,
//!   not `workspaceID`). Getting it wrong returns `$HOME`-scoped results
//!   without an error, because the service itself `chdir`s to `$HOME`.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::http_wire::{self, Endpoint};
use crate::opencode_session::encode_path_segment;

/// The service answers a local request in single-digit milliseconds; a budget
/// this large only ever covers a machine under load.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Health is on the start path, where a hung probe would eat the whole start
/// budget, so it gets its own much tighter bound.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
/// A transcript page and a session export both walk stored messages, which for
/// a long task is far more work than an ordinary request.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60);
/// Forking copies every retained message and part into a new session, and a
/// long task can legitimately take minutes.
const FORK_TIMEOUT: Duration = Duration::from_secs(120);

/// The result of every call in this module.
pub(crate) type Result<T> = std::result::Result<T, ApiError>;

/// A failed call, split by what the caller can actually do about it.
///
/// The status and the server's own error tag are the only things separating
/// "this session is gone" from "this workspace does not resolve", so they are
/// recovered instead of being flattened into prose.
#[derive(Debug)]
pub(crate) enum ApiError {
    /// The service answered with its own structured error body, which always
    /// carries `_tag` and `message`.
    Tagged {
        tag: String,
        message: String,
        status: u16,
    },
    /// A non-2xx without a structured body.
    Http { status: u16, body: String },
    /// The request never produced an HTTP status: connect failure, timeout, a
    /// body that would not decode, or a locally rejected argument.
    Transport(anyhow::Error),
}

impl ApiError {
    pub(crate) fn status(&self) -> Option<u16> {
        match self {
            Self::Tagged { status, .. } | Self::Http { status, .. } => Some(*status),
            Self::Transport(_) => None,
        }
    }

    pub(crate) fn tag(&self) -> Option<&str> {
        match self {
            Self::Tagged { tag, .. } => Some(tag),
            _ => None,
        }
    }

    pub(crate) fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }

    /// A location-scoped route handed a directory the service cannot resolve
    /// answers HTTP 500 with an EMPTY body. That is a bad workspace, not a
    /// server fault, and treating it as the latter would retry forever.
    pub(crate) fn is_unresolvable_location(&self) -> bool {
        matches!(self, Self::Http { status: 500, body } if body.trim().is_empty())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tagged {
                tag,
                message,
                status,
            } => write!(formatter, "OpenCode 2 {tag} (HTTP {status}): {message}"),
            Self::Http { status, body } if body.trim().is_empty() => {
                write!(formatter, "OpenCode 2 request failed with HTTP {status}")
            }
            Self::Http { status, body } => {
                write!(
                    formatter,
                    "OpenCode 2 request failed with HTTP {status}: {body}"
                )
            }
            Self::Transport(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::Transport(error)
    }
}

/// Page order. The server defaults to `desc` on both `/api/session` and
/// `/message`, so transcript hydration must ask for `asc` explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Order {
    Asc,
    Desc,
}

impl Order {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// How the service admits a prompt into a session that is already draining.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Delivery {
    Steer,
    Queue,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct Health {
    pub healthy: bool,
    pub version: String,
    pub pid: u32,
}

/// A model as a session refers to it.
///
/// There is no `modelID` here — that field exists only on [`ModelInfo`], and
/// mistaking the two produces a reference the server rejects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ModelRef {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// The workspace a session is bound to.
///
/// One service process serves every workspace precisely because each session
/// carries its own location; the process itself sits in `$HOME`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LocationRef {
    pub directory: String,
    #[serde(
        rename = "workspaceID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct TokenCache {
    pub read: f64,
    pub write: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct TokenUsage {
    pub input: f64,
    pub output: f64,
    /// Required in v2, unlike v1 where reasoning tokens were folded into
    /// output.
    pub reasoning: f64,
    pub cache: TokenCache,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct SessionTime {
    pub created: f64,
    pub updated: f64,
    #[serde(default)]
    pub idle: Option<f64>,
    #[serde(default)]
    pub viewed: Option<f64>,
    #[serde(default)]
    pub archived: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SessionOutcome {
    Succeeded,
    Failed,
    Interrupted,
}

/// The fork boundary as the server REPORTS it, which always carries a message
/// id — including for `through`, where the request form has none. The read and
/// write shapes are deliberately two types; see [`ForkRequestBoundary`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ForkBoundary {
    #[serde(rename = "type")]
    pub kind: ForkBoundaryKind,
    #[serde(rename = "messageID")]
    pub message_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ForkBoundaryKind {
    Before,
    Through,
}

/// The fork boundary as the server ACCEPTS it. `through` takes no message id
/// at all, and sending one is a 400.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum ForkRequestBoundary {
    Before {
        #[serde(rename = "messageID")]
        message_id: String,
    },
    Through,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct SessionFork {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub boundary: ForkBoundary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FileDiffStatus {
    Added,
    Deleted,
    Modified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct FileDiff {
    pub file: String,
    pub patch: String,
    pub additions: u32,
    pub deletions: u32,
    pub status: FileDiffStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct SessionRevert {
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "partID", default)]
    pub part_id: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub files: Option<Vec<FileDiff>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct SessionInfo {
    pub id: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
    #[serde(rename = "parentID", default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub fork: Option<SessionFork>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    pub cost: f64,
    pub tokens: TokenUsage,
    #[serde(default)]
    pub outcome: Option<SessionOutcome>,
    pub time: SessionTime,
    #[serde(default)]
    pub title: Option<String>,
    pub location: LocationRef,
    #[serde(default)]
    pub subpath: Option<String>,
    #[serde(default)]
    pub revert: Option<SessionRevert>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct StructuredError {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub status: Option<u16>,
}

/// One timing block. Messages carry `created`/`streamed`/`completed`, tool
/// parts carry `created`/`ran`/`completed`; a single struct covers both
/// because serde ignores the members a given shape does not send.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct Timing {
    pub created: f64,
    #[serde(default)]
    pub streamed: Option<f64>,
    #[serde(default)]
    pub ran: Option<f64>,
    #[serde(default)]
    pub completed: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum ToolContent {
    Text {
        text: String,
    },
    File {
        uri: String,
        mime: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// A content kind a newer build introduced. Failing the whole message
    /// instead would blank a transcript that is otherwise perfectly readable.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum ToolState {
    /// `input` is a raw JSON STRING while the call is still streaming and an
    /// OBJECT from `running` onwards. Typing it as anything but a `Value`
    /// fails to decode exactly the frames a live tool call emits.
    Streaming { input: Value },
    Running {
        input: Value,
        #[serde(default)]
        metadata: Value,
    },
    Completed {
        input: Value,
        content: Vec<ToolContent>,
        #[serde(default)]
        metadata: Value,
    },
    Error {
        input: Value,
        error: StructuredError,
        #[serde(default)]
        content: Vec<ToolContent>,
        #[serde(default)]
        metadata: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum AssistantContent {
    Text {
        text: String,
        #[serde(default)]
        state: Option<Value>,
    },
    Reasoning {
        text: String,
        #[serde(default)]
        state: Option<Value>,
        #[serde(default)]
        time: Option<Timing>,
    },
    Tool {
        id: String,
        name: String,
        #[serde(default)]
        executed: Option<bool>,
        state: ToolState,
        time: Timing,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ShellStatus {
    Running,
    Exited,
    Timeout,
    Killed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShellOutput {
    pub output: String,
    pub cursor: u64,
    pub size: u64,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CompactionStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CompactionReason {
    Auto,
    Manual,
}

/// One stored message.
///
/// The three compaction states share the `compaction` tag and differ only by
/// `status`, so they collapse into one variant here.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum MessageInfo {
    AgentSwitched {
        id: String,
        time: Timing,
        agent: String,
        #[serde(default)]
        previous: Option<String>,
    },
    ModelSwitched {
        id: String,
        time: Timing,
        model: ModelRef,
        #[serde(default)]
        previous: Option<ModelRef>,
    },
    LocationSwitched {
        id: String,
        time: Timing,
        location: LocationRef,
        #[serde(rename = "projectID", default)]
        project_id: Option<String>,
        #[serde(default)]
        subpath: Option<String>,
    },
    User {
        id: String,
        time: Timing,
        text: String,
        #[serde(default)]
        files: Vec<Value>,
        #[serde(default)]
        agents: Vec<Value>,
        #[serde(default)]
        skills: Vec<Value>,
    },
    Synthetic {
        id: String,
        time: Timing,
        text: String,
        #[serde(default)]
        description: Option<String>,
    },
    System {
        id: String,
        time: Timing,
        text: String,
        #[serde(default)]
        description: Option<String>,
    },
    Skill {
        id: String,
        time: Timing,
        skill: String,
        name: String,
        text: String,
    },
    Shell {
        id: String,
        time: Timing,
        #[serde(rename = "shellID")]
        shell_id: String,
        command: String,
        status: ShellStatus,
        /// The service encodes a non-finite exit code as the literal strings
        /// `"NaN"`, `"Infinity"` or `"-Infinity"`, so an `Option<f64>` fails
        /// to decode a killed shell.
        #[serde(default)]
        exit: Option<Value>,
        #[serde(default)]
        output: Option<ShellOutput>,
    },
    Assistant {
        id: String,
        time: Timing,
        agent: String,
        model: ModelRef,
        content: Vec<AssistantContent>,
        #[serde(default)]
        finish: Option<String>,
        #[serde(default)]
        cost: Option<f64>,
        #[serde(default)]
        tokens: Option<TokenUsage>,
        #[serde(default)]
        error: Option<StructuredError>,
    },
    Compaction {
        id: String,
        time: Timing,
        status: CompactionStatus,
        reason: CompactionReason,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        recent: Option<String>,
        #[serde(default)]
        model: Option<ModelRef>,
        #[serde(default)]
        error: Option<StructuredError>,
    },
    /// A message kind a newer build introduced. A page of transcript must not
    /// fail wholesale because one entry is from the future.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct SessionExport {
    pub info: SessionInfo,
    pub messages: Vec<MessageInfo>,
}

/// The inbox entry `POST /prompt` answers with.
///
/// The beta build renamed this member from `data` to `payload`; the older name
/// is gone, not aliased.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct InboxUser {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(rename = "timeCreated")]
    pub time_created: f64,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Value,
    pub delivery: Delivery,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct PermissionSource {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "messageID")]
    pub message_id: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct PermissionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub action: String,
    pub resources: Vec<String>,
    #[serde(default)]
    pub save: Option<Vec<String>>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub source: Option<PermissionSource>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PermissionReply {
    Once,
    Always,
    Reject,
}

/// One answered form field. The numeric arm also admits the literal strings
/// `"Infinity"`/`"NaN"`, which land in [`FormValue::Text`] — the server accepts
/// them back in that form.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum FormValue {
    Bool(bool),
    Number(f64),
    List(Vec<String>),
    Text(String),
}

/// A form answer, keyed by [`FormField`]'s `key`.
///
/// Ordered rather than hashed so a replayed answer serializes identically.
pub(crate) type FormAnswer = BTreeMap<String, FormValue>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct FormOption {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum FormField {
    String {
        key: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        required: Option<bool>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        min_length: Option<u32>,
        #[serde(default)]
        max_length: Option<u32>,
        #[serde(default)]
        pattern: Option<String>,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        default: Option<String>,
        /// Present when the field is really a single-select; `custom` then says
        /// whether a value outside the list is allowed.
        #[serde(default)]
        options: Option<Vec<FormOption>>,
        #[serde(default)]
        custom: Option<bool>,
    },
    Number {
        key: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        required: Option<bool>,
        /// Bounds share the shell's non-finite encoding: a number, or one of
        /// the literal strings `"Infinity"`/`"-Infinity"`/`"NaN"`.
        #[serde(default)]
        minimum: Option<Value>,
        #[serde(default)]
        maximum: Option<Value>,
        #[serde(default)]
        default: Option<Value>,
    },
    Integer {
        key: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        required: Option<bool>,
        #[serde(default)]
        minimum: Option<Value>,
        #[serde(default)]
        maximum: Option<Value>,
        #[serde(default)]
        default: Option<Value>,
    },
    Boolean {
        key: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        required: Option<bool>,
        #[serde(default)]
        default: Option<bool>,
    },
    Multiselect {
        key: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        required: Option<bool>,
        options: Vec<FormOption>,
        #[serde(default)]
        min_items: Option<u32>,
        #[serde(default)]
        max_items: Option<u32>,
        #[serde(default)]
        custom: Option<bool>,
        #[serde(default)]
        default: Option<Vec<String>>,
    },
    External {
        key: String,
        url: String,
        #[serde(default)]
        title: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct FormInfo {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub title: String,
    #[serde(default)]
    pub metadata: Option<Value>,
    /// Never empty: the schema requires at least one field.
    pub fields: Vec<FormField>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct ModelCapabilities {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
pub(crate) struct ModelLimit {
    pub context: i64,
    pub output: i64,
    #[serde(default)]
    pub input: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ModelVariant {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct ModelInfo {
    pub id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// A model's reasoning-effort or thinking presets. Empty for models that
    /// have none, and a variant id is what [`ModelRef::variant`] carries.
    #[serde(default)]
    pub variants: Vec<ModelVariant>,
    /// `alpha` | `beta` | `deprecated` | `active`, left as a string so a new
    /// lifecycle stage does not fail the whole catalogue.
    pub status: String,
    pub enabled: bool,
    pub limit: ModelLimit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentMode {
    Primary,
    Subagent,
    All,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct AgentInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub mode: AgentMode,
    pub hidden: bool,
    #[serde(default)]
    pub model: Option<ModelRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A skill as the catalogue lists it.
///
/// The response also carries the skill's whole markdown body under `content`.
/// It is deliberately not decoded: nothing above this layer renders it, and
/// keeping it would make every skill's file resident for the life of the app.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct SkillInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub slash: Option<bool>,
    #[serde(default)]
    pub autoinvoke: Option<bool>,
    pub location: String,
}

pub(crate) fn health(endpoint: &Endpoint) -> Result<Health> {
    // Bare payload: health is one of the routes with no `{ data }` envelope.
    let response = request(endpoint, "GET", "/api/health", None, HEALTH_TIMEOUT)?;
    decode(response, "health")
}

/// Creates a session with a CLIENT-MINTED id.
///
/// The id is an argument rather than a return value because the caller must
/// already be subscribed to the shared event stream before the session exists;
/// letting the server mint it reopens the create/first-event race.
///
/// `directory` must be the canonicalized workspace path with no trailing
/// slash, and the very same string has to be used for `?directory=` when
/// listing — the server compares those by exact string equality.
pub(crate) fn create_session(
    endpoint: &Endpoint,
    id: &str,
    agent: Option<&str>,
    model: Option<&ModelRef>,
    directory: &str,
) -> Result<SessionInfo> {
    let mut body = json!({
        "id": id,
        "location": { "directory": directory },
    });
    if let Some(agent) = agent {
        body["agent"] = json!(agent);
    }
    if let Some(model) = model {
        body["model"] = serde_json::to_value(model).map_err(|error| {
            ApiError::Transport(anyhow!("could not encode the OpenCode 2 model: {error}"))
        })?;
    }
    let response = request(
        endpoint,
        "POST",
        "/api/session",
        Some(&body),
        REQUEST_TIMEOUT,
    )?;
    decode(data(response, "session")?, "session")
}

pub(crate) fn get_session(endpoint: &Endpoint, session: &str) -> Result<SessionInfo> {
    let path = format!("/api/session/{}", encode_path_segment(session));
    let response = request(endpoint, "GET", &path, None, REQUEST_TIMEOUT)?;
    decode(data(response, "session")?, "session")
}

pub(crate) fn delete_session(endpoint: &Endpoint, session: &str) -> Result<()> {
    let path = format!("/api/session/{}", encode_path_segment(session));
    request(endpoint, "DELETE", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

/// Runtime-only MCP registration, scoped to one location. This never writes
/// the shared service's configuration files.
pub(crate) fn add_mcp(
    endpoint: &Endpoint,
    directory: &str,
    server: &str,
    config: &Value,
) -> Result<()> {
    let path = format!(
        "/api/mcp/{}{}",
        encode_path_segment(server),
        location_query(Some(directory))
    );
    request(
        endpoint,
        "PUT",
        &path,
        Some(&json!({"config": config})),
        REQUEST_TIMEOUT,
    )?;
    Ok(())
}

pub(crate) fn list_mcp(endpoint: &Endpoint, directory: &str) -> Result<Vec<Value>> {
    catalogue(endpoint, "/api/mcp", Some(directory), "MCP servers")
}

pub(crate) fn remove_mcp(endpoint: &Endpoint, directory: &str, server: &str) -> Result<()> {
    let path = format!(
        "/api/mcp/{}{}",
        encode_path_segment(server),
        location_query(Some(directory))
    );
    request(endpoint, "DELETE", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn put_instruction_entry(
    endpoint: &Endpoint,
    session: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    let path = format!(
        "/api/session/{}/instructions/entries/{}",
        encode_path_segment(session),
        encode_path_segment(key)
    );
    request(
        endpoint,
        "PUT",
        &path,
        Some(&json!({"value": value})),
        REQUEST_TIMEOUT,
    )?;
    Ok(())
}

pub(crate) fn remove_instruction_entry(
    endpoint: &Endpoint,
    session: &str,
    key: &str,
) -> Result<()> {
    let path = format!(
        "/api/session/{}/instructions/entries/{}",
        encode_path_segment(session),
        encode_path_segment(key)
    );
    request(endpoint, "DELETE", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn rename_session(endpoint: &Endpoint, session: &str, title: &str) -> Result<()> {
    let path = format!("/api/session/{}/rename", encode_path_segment(session));
    let body = json!({ "title": title });
    request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    Ok(())
}

/// Lists sessions newest-first by default, returning the cursor for the next
/// page or `None` once the listing is exhausted.
pub(crate) fn list_sessions(
    endpoint: &Endpoint,
    directory: Option<&str>,
    parent_id: Option<&str>,
    order: Order,
    limit: usize,
    cursor: Option<&str>,
) -> Result<(Vec<SessionInfo>, Option<String>)> {
    let path = format!(
        "/api/session{}",
        session_list_query(directory, parent_id, order, limit, cursor)
    );
    let response = request(endpoint, "GET", &path, None, REQUEST_TIMEOUT)?;
    page(response, "session list")
}

pub(crate) fn list_messages(
    endpoint: &Endpoint,
    session: &str,
    order: Order,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<(Vec<MessageInfo>, Option<String>)> {
    let path = format!(
        "/api/session/{}/message{}",
        encode_path_segment(session),
        message_list_query(order, limit, cursor)
    );
    let response = request(endpoint, "GET", &path, None, TRANSFER_TIMEOUT)?;
    page(response, "message list")
}

pub(crate) fn export_session(
    endpoint: &Endpoint,
    session: &str,
    sanitize: bool,
) -> Result<SessionExport> {
    let path = format!(
        "/api/session/{}/export?sanitize={sanitize}",
        encode_path_segment(session)
    );
    let response = request(endpoint, "GET", &path, None, TRANSFER_TIMEOUT)?;
    decode(data(response, "session export")?, "session export")
}

/// Submits a prompt and returns the inbox entry it became.
///
/// Leaving `delivery` unset lets the server choose, which is what an idle
/// session wants; a busy session needs the caller to say whether the prompt
/// steers the running turn or queues behind it.
pub(crate) fn prompt(
    endpoint: &Endpoint,
    session: &str,
    text: &str,
    delivery: Option<Delivery>,
) -> Result<InboxUser> {
    let path = format!("/api/session/{}/prompt", encode_path_segment(session));
    let mut body = json!({ "text": text });
    if let Some(delivery) = delivery {
        body["delivery"] = json!(delivery);
    }
    let response = request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    decode(data(response, "prompt")?, "prompt")
}

/// Execute a registered command with provider-owned template expansion and
/// agent/model selection. Unlike /prompt, this route acknowledges with 204;
/// execution and completion arrive on the session event stream.
pub(crate) fn command(
    endpoint: &Endpoint,
    session: &str,
    name: &str,
    arguments: &str,
    delivery: Option<Delivery>,
) -> Result<()> {
    let path = format!("/api/session/{}/command", encode_path_segment(session));
    let mut body = json!({"command": name, "text": arguments});
    if let Some(delivery) = delivery {
        body["delivery"] = json!(delivery);
    }
    request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    Ok(())
}

/// Lists undelivered inbox entries.
///
/// The entries are left as raw JSON because the union also carries synthetic,
/// compaction and move payloads Waku has no use for; callers filter on `type`
/// and decode only what they recognize.
pub(crate) fn list_inbox(endpoint: &Endpoint, session: &str) -> Result<Vec<Value>> {
    let path = format!("/api/session/{}/inbox", encode_path_segment(session));
    let response = request(endpoint, "GET", &path, None, REQUEST_TIMEOUT)?;
    decode(data(response, "inbox")?, "inbox")
}

/// Promotes a queued entry so it interrupts the running turn.
pub(crate) fn steer_inbox(endpoint: &Endpoint, session: &str, inbox_id: &str) -> Result<()> {
    let path = format!(
        "/api/session/{}/inbox/{}/steer",
        encode_path_segment(session),
        encode_path_segment(inbox_id)
    );
    request(endpoint, "POST", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

/// Demotes an entry so it waits for the running turn to finish.
pub(crate) fn queue_inbox(endpoint: &Endpoint, session: &str, inbox_id: &str) -> Result<()> {
    let path = format!(
        "/api/session/{}/inbox/{}/queue",
        encode_path_segment(session),
        encode_path_segment(inbox_id)
    );
    request(endpoint, "POST", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn cancel_inbox(endpoint: &Endpoint, session: &str, inbox_id: &str) -> Result<()> {
    let path = format!(
        "/api/session/{}/inbox/{}",
        encode_path_segment(session),
        encode_path_segment(inbox_id)
    );
    request(endpoint, "DELETE", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

/// Interrupts the running turn, answering whether there was one to interrupt.
///
/// The route takes no body and answers a BARE `{ interrupted }` — it is inside
/// `/api/session` but outside the `{ data }` envelope.
pub(crate) fn interrupt(endpoint: &Endpoint, session: &str) -> Result<bool> {
    let path = format!("/api/session/{}/interrupt", encode_path_segment(session));
    let response = request(endpoint, "POST", &path, None, REQUEST_TIMEOUT)?;
    #[derive(Deserialize)]
    struct Interrupted {
        interrupted: bool,
    }
    let decoded: Interrupted = decode(response, "interrupt")?;
    Ok(decoded.interrupted)
}

pub(crate) fn fork(
    endpoint: &Endpoint,
    session: &str,
    boundary: &ForkRequestBoundary,
) -> Result<SessionInfo> {
    let path = format!("/api/session/{}/fork", encode_path_segment(session));
    let body = json!({ "boundary": boundary });
    let response = request(endpoint, "POST", &path, Some(&body), FORK_TIMEOUT)?;
    decode(data(response, "fork")?, "fork")
}

pub(crate) fn switch_model(endpoint: &Endpoint, session: &str, model: &ModelRef) -> Result<()> {
    let path = format!("/api/session/{}/model", encode_path_segment(session));
    let body = json!({ "model": model });
    request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn switch_agent(endpoint: &Endpoint, session: &str, agent: &str) -> Result<()> {
    let path = format!("/api/session/{}/agent", encode_path_segment(session));
    let body = json!({ "agent": agent });
    request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn list_permissions(
    endpoint: &Endpoint,
    session: &str,
) -> Result<Vec<PermissionRequest>> {
    let path = format!("/api/session/{}/permission", encode_path_segment(session));
    let response = request(endpoint, "GET", &path, None, REQUEST_TIMEOUT)?;
    decode(data(response, "permission list")?, "permission list")
}

/// Answers one permission request.
///
/// `always` is rejected here, not upstream: it writes a persistent allow rule
/// into the user's own OpenCode permission config, which is a decision Waku
/// has no mandate to make on the user's behalf from a transcript button.
pub(crate) fn reply_permission(
    endpoint: &Endpoint,
    session: &str,
    request_id: &str,
    reply: PermissionReply,
) -> Result<()> {
    if matches!(reply, PermissionReply::Always) {
        return Err(ApiError::Transport(anyhow!(
            "OpenCode 2 permission replies must not save a persistent rule"
        )));
    }
    let path = format!(
        "/api/session/{}/permission/{}/reply",
        encode_path_segment(session),
        encode_path_segment(request_id)
    );
    let body = json!({ "reply": reply });
    request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn list_forms(endpoint: &Endpoint, session: &str) -> Result<Vec<FormInfo>> {
    let path = format!("/api/session/{}/form", encode_path_segment(session));
    let response = request(endpoint, "GET", &path, None, REQUEST_TIMEOUT)?;
    decode(data(response, "form list")?, "form list")
}

pub(crate) fn reply_form(
    endpoint: &Endpoint,
    session: &str,
    form_id: &str,
    answer: &FormAnswer,
) -> Result<()> {
    let path = format!(
        "/api/session/{}/form/{}/reply",
        encode_path_segment(session),
        encode_path_segment(form_id)
    );
    let body = json!({ "answer": answer });
    request(endpoint, "POST", &path, Some(&body), REQUEST_TIMEOUT)?;
    Ok(())
}

pub(crate) fn cancel_form(endpoint: &Endpoint, session: &str, form_id: &str) -> Result<()> {
    let path = format!(
        "/api/session/{}/form/{}/cancel",
        encode_path_segment(session),
        encode_path_segment(form_id)
    );
    request(endpoint, "POST", &path, None, REQUEST_TIMEOUT)?;
    Ok(())
}

/// The ids of every session this service process is currently draining.
///
/// The payload is keyed BY session id, so the set is its keys; the values only
/// say `running`, which is the same thing membership already says.
pub(crate) fn active_sessions(endpoint: &Endpoint) -> Result<HashSet<String>> {
    let response = request(
        endpoint,
        "GET",
        "/api/session/active",
        None,
        REQUEST_TIMEOUT,
    )?;
    let active = data(response, "active sessions")?;
    let Value::Object(active) = active else {
        return Err(ApiError::Transport(anyhow!(
            "OpenCode 2 returned an unreadable active session map"
        )));
    };
    Ok(active.into_iter().map(|(id, _)| id).collect())
}

pub(crate) fn list_models(endpoint: &Endpoint, directory: Option<&str>) -> Result<Vec<ModelInfo>> {
    catalogue(endpoint, "/api/model", directory, "model catalogue")
}

pub(crate) fn list_agents(endpoint: &Endpoint, directory: Option<&str>) -> Result<Vec<AgentInfo>> {
    catalogue(endpoint, "/api/agent", directory, "agent catalogue")
}

pub(crate) fn list_commands(
    endpoint: &Endpoint,
    directory: Option<&str>,
) -> Result<Vec<CommandInfo>> {
    // A cold location publishes its registry in stages: first empty, then
    // built-ins, then configured commands and skills. Wait for those plugins
    // to finish before caching the list. The budget also bounds older builds
    // whose plugin identifiers or readiness surface differ.
    let path = format!("/api/command{}", location_query(directory));
    let plugins_path = format!("/api/plugin{}", location_query(directory));
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut latest = Vec::new();
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(latest);
        };
        let ready = request(endpoint, "GET", &plugins_path, None, remaining)
            .ok()
            .as_ref()
            .and_then(command_plugins_ready);
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(latest);
        };
        let response = request(endpoint, "GET", &path, None, remaining)?;
        latest = decode(data(response, "command catalogue")?, "command catalogue")?;
        if ready == Some(true) || (ready.is_none() && !latest.is_empty()) {
            return Ok(latest);
        }
        std::thread::sleep(
            Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn command_plugins_ready(response: &Value) -> Option<bool> {
    let plugins = response.get("data")?.as_array()?;
    Some(
        [
            ["opencode.command", "command"],
            ["opencode.config.command", "config-command"],
            ["opencode.config.skill", "config-skill"],
        ]
        .iter()
        .all(|names| {
            plugins.iter().any(|plugin| {
                plugin
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| names.contains(&id))
                    && matches!(
                        plugin.pointer("/state/status").and_then(Value::as_str),
                        Some("active" | "failed")
                    )
            })
        }),
    )
}

pub(crate) fn list_skills(endpoint: &Endpoint, directory: Option<&str>) -> Result<Vec<SkillInfo>> {
    catalogue(endpoint, "/api/skill", directory, "skill catalogue")
}

/// The catalogue routes differ only in their element type: each answers
/// `{ location, data }` and each scopes by deepObject `location[directory]`.
fn catalogue<T: DeserializeOwned>(
    endpoint: &Endpoint,
    route: &str,
    directory: Option<&str>,
    what: &str,
) -> Result<Vec<T>> {
    let path = format!("{route}{}", location_query(directory));
    let response = request(endpoint, "GET", &path, None, REQUEST_TIMEOUT)?;
    decode(data(response, what)?, what)
}

fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Value> {
    http_wire::request_json(endpoint, method, path, body, timeout).map_err(classify)
}

/// Unwraps the `{ data }` envelope.
///
/// Called per route rather than from [`request`], because the envelope is not
/// universal: `/api/health` and `POST …/interrupt` answer bare, and wrapping
/// those would turn a good response into a decode failure.
fn data(response: Value, what: &str) -> Result<Value> {
    match response {
        Value::Object(mut fields) => fields.remove("data").ok_or_else(|| {
            ApiError::Transport(anyhow!("OpenCode 2 returned no {what} in its response"))
        }),
        _ => Err(ApiError::Transport(anyhow!(
            "OpenCode 2 returned an unreadable {what} response"
        ))),
    }
}

fn decode<T: DeserializeOwned>(response: Value, what: &str) -> Result<T> {
    serde_json::from_value(response).map_err(|error| {
        ApiError::Transport(anyhow!("OpenCode 2 returned an invalid {what}: {error}"))
    })
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    #[serde(default)]
    cursor: PageCursor,
}

#[derive(Default, Deserialize)]
struct PageCursor {
    #[serde(default)]
    next: Option<String>,
}

/// Decodes one page and normalizes its continuation cursor.
///
/// `cursor.next` is minted on every page including the last, so a caller that
/// stops when it is absent never stops. The empty `data` array is the only
/// terminator the API has, and it is turned into the absent cursor here so no
/// caller can get that wrong.
fn page<T: DeserializeOwned>(response: Value, what: &str) -> Result<(Vec<T>, Option<String>)> {
    let page: Page<T> = decode(response, what)?;
    let next = (!page.data.is_empty())
        .then_some(page.cursor.next)
        .flatten();
    Ok((page.data, next))
}

/// `GET /api/session` is the ONE location-aware route that filters with a FLAT
/// `?directory=`; sending it `location[directory]` silently lists `$HOME`.
fn session_list_query(
    directory: Option<&str>,
    parent_id: Option<&str>,
    order: Order,
    limit: usize,
    cursor: Option<&str>,
) -> String {
    let mut query = Query::default();
    query.push("limit", &limit.to_string());
    if let Some(directory) = directory {
        query.push("directory", directory);
    }
    if let Some(parent_id) = parent_id {
        query.push("parentID", parent_id);
    }
    // The order is baked into the opaque cursor; sending both is a 400.
    match cursor {
        Some(cursor) => query.push("cursor", cursor),
        None => query.push("order", order.as_str()),
    }
    query.finish()
}

fn message_list_query(order: Order, limit: Option<usize>, cursor: Option<&str>) -> String {
    let mut query = Query::default();
    if let Some(limit) = limit {
        query.push("limit", &limit.to_string());
    }
    match cursor {
        Some(cursor) => query.push("cursor", cursor),
        None => query.push("order", order.as_str()),
    }
    query.finish()
}

/// Every location-aware route except `GET /api/session` scopes with deepObject
/// `location[directory]`. The sibling key is `workspace`, not `workspaceID`.
fn location_query(directory: Option<&str>) -> String {
    let mut query = Query::default();
    if let Some(directory) = directory {
        query.push("location[directory]", directory);
    }
    query.finish()
}

#[derive(Default)]
struct Query {
    text: String,
}

impl Query {
    fn push(&mut self, key: &str, value: &str) {
        self.text.push(if self.text.is_empty() { '?' } else { '&' });
        // The deepObject brackets have to survive as `%5B`/`%5D`, which is
        // exactly what the shared segment encoder produces.
        self.text.push_str(&encode_path_segment(key));
        self.text.push('=');
        self.text.push_str(&encode_path_segment(value));
    }

    fn finish(self) -> String {
        self.text
    }
}

/// The shared wire layer reports a non-2xx as one flat error string, because
/// v1 had nothing to do with the status. v2 does: a 404 means the session is
/// gone, and a bodyless 500 means the workspace does not resolve. Recover both
/// rather than propagating prose a caller cannot branch on.
const HTTP_FAILURE_PREFIX: &str = "OpenCode session request failed with HTTP ";

fn classify(error: anyhow::Error) -> ApiError {
    let text = error.to_string();
    let Some((status, body)) = text
        .strip_prefix(HTTP_FAILURE_PREFIX)
        .and_then(|rest| rest.split_once(": "))
    else {
        return ApiError::Transport(error);
    };
    let Ok(status) = status.parse::<u16>() else {
        return ApiError::Transport(error);
    };
    match serde_json::from_str::<TaggedError>(body) {
        Ok(tagged) => ApiError::Tagged {
            tag: tagged.tag,
            message: tagged.message,
            status,
        },
        Err(_) => ApiError::Http {
            status,
            body: body.to_owned(),
        },
    }
}

#[derive(Deserialize)]
struct TaggedError {
    #[serde(rename = "_tag")]
    tag: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use serde_json::json;

    use super::*;

    /// Answers exactly one request with a canned response, so the mapping from
    /// a real HTTP failure to [`ApiError`] is exercised through the shared wire
    /// layer rather than around it.
    fn serve_once(response: &'static str) -> Endpoint {
        serve_responses(vec![response.to_owned()])
    }

    fn serve_responses(responses: Vec<String>) -> Endpoint {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for response in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                // TCP reads need not align with write!'s request fragments.
                // Closing with unread request bytes can reset the connection and
                // replace the canned HTTP error with a transport error on Windows.
                {
                    let mut request = std::io::BufReader::new(&mut socket);
                    let mut line = String::new();
                    let mut content_length = 0;
                    loop {
                        line.clear();
                        assert!(
                            request.read_line(&mut line).unwrap() > 0,
                            "request ended before its headers were complete"
                        );
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                content_length = value.trim().parse::<usize>().unwrap();
                            }
                        }
                    }
                    request.read_exact(&mut vec![0; content_length]).unwrap();
                }
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        Endpoint::local(port)
    }

    #[test]
    fn command_catalog_waits_for_cold_location_builtins() {
        let responses = [
            json!({"data": []}),
            json!({"data": []}),
            json!({"data": [{"id": "opencode.command", "state": {"status": "active"}}]}),
            json!({"data": [{"name": "init"}, {"name": "review"}]}),
            json!({"data": [
                {"id": "opencode.command", "state": {"status": "active"}},
                {"id": "opencode.config.command", "state": {"status": "active"}},
                {"id": "opencode.config.skill", "state": {"status": "active"}}
            ]}),
            json!({"data": [{"name": "init"}, {"name": "review"}, {"name": "custom"}]}),
        ]
        .into_iter()
        .map(|value| {
            let body = value.to_string();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        })
        .collect();
        let endpoint = serve_responses(responses);
        let commands = list_commands(&endpoint, Some("/cold-workspace")).unwrap();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["init", "review", "custom"]
        );
    }

    #[test]
    fn native_command_accepts_an_empty_acknowledgement() {
        let endpoint = serve_once("HTTP/1.1 204 No Content\r\n\r\n");
        command(
            &endpoint,
            "ses_test",
            "review",
            "main",
            Some(Delivery::Steer),
        )
        .unwrap();
    }

    #[test]
    fn computer_use_runtime_configuration_is_location_and_session_scoped() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = Endpoint::local(listener.local_addr().unwrap().port());
        let (sent, received) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = std::io::BufReader::new(&mut socket);
                let mut request = String::new();
                input.read_line(&mut request).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    input.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = header.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                input.read_exact(&mut body).unwrap();
                sent.send((
                    request,
                    serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
                ))
                .unwrap();
                socket
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .unwrap();
            }
        });
        let config = json!({"type":"local","command":["/app/waku_js_repl"],"codemode":false});
        add_mcp(
            &endpoint,
            "/work/project with space",
            "waku_js_repl_test",
            &config,
        )
        .unwrap();
        put_instruction_entry(
            &endpoint,
            "ses_test",
            "waku-computer-use",
            "Use cua.list_apps() through js.",
        )
        .unwrap();
        remove_instruction_entry(&endpoint, "ses_test", "waku-computer-use").unwrap();
        remove_mcp(&endpoint, "/work/project with space", "waku_js_repl_test").unwrap();
        server.join().unwrap();
        let requests: Vec<_> = received.try_iter().collect();
        assert_eq!(
            requests[0].0,
            "PUT /api/mcp/waku_js_repl_test?location%5Bdirectory%5D=%2Fwork%2Fproject%20with%20space HTTP/1.1\r\n"
        );
        assert_eq!(requests[0].1, json!({"config":config}));
        assert_eq!(
            requests[1].0,
            "PUT /api/session/ses_test/instructions/entries/waku-computer-use HTTP/1.1\r\n"
        );
        assert_eq!(
            requests[1].1,
            json!({"value":"Use cua.list_apps() through js."})
        );
        assert_eq!(
            requests[2].0,
            "DELETE /api/session/ses_test/instructions/entries/waku-computer-use HTTP/1.1\r\n"
        );
        assert!(
            requests[3]
                .0
                .starts_with("DELETE /api/mcp/waku_js_repl_test?location%5Bdirectory%5D=")
        );
    }

    #[test]
    fn canned_server_waits_for_complete_request_headers_and_body() {
        let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
        let endpoint = serve_once(response);
        let mut socket = TcpStream::connect(endpoint.address()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();

        // Send the request in pieces, as TCP may deliver write!'s fragments.
        for fragment in [
            "POST /api/session HTTP/1.1\r\n",
            "Host: localhost\r\nContent-Length: 4\r\n\r\nab",
        ] {
            socket.write_all(fragment.as_bytes()).unwrap();
            let error = socket
                .read(&mut [0_u8; 1])
                .expect_err("the fixture replied before the complete request arrived");
            assert!(matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ));
        }

        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.write_all(b"cd").unwrap();
        let mut received = String::new();
        socket.read_to_string(&mut received).unwrap();
        assert_eq!(received, response);
    }

    #[test]
    fn session_list_uses_a_flat_directory_filter() {
        assert_eq!(
            session_list_query(Some("/Users/e/dev/waku"), None, Order::Desc, 50, None),
            "?limit=50&directory=%2FUsers%2Fe%2Fdev%2Fwaku&order=desc"
        );
    }

    /// The order is baked into the opaque cursor, so sending both is a 400.
    #[test]
    fn a_cursor_replaces_the_order_parameter() {
        let query = session_list_query(None, None, Order::Asc, 50, Some("Y3Vyc29y"));
        assert!(query.contains("cursor=Y3Vyc29y"));
        assert!(!query.contains("order="));
        let query = message_list_query(Order::Asc, Some(200), Some("Y3Vyc29y"));
        assert!(query.contains("limit=200"));
        assert!(!query.contains("order="));
    }

    /// Everything but `GET /api/session` scopes with deepObject brackets; a
    /// flat `directory=` there silently lists `$HOME`.
    #[test]
    fn catalogue_routes_scope_with_deep_object_brackets() {
        assert_eq!(
            location_query(Some("/Users/e/dev/waku")),
            "?location%5Bdirectory%5D=%2FUsers%2Fe%2Fdev%2Fwaku"
        );
        assert_eq!(location_query(None), "");
    }

    /// Both cursors are minted on every page, so only an empty page ends the
    /// walk.
    #[test]
    fn pagination_terminates_on_an_empty_page_not_an_absent_cursor() {
        let full = json!({
            "data": [{ "name": "init" }],
            "cursor": { "previous": "cA", "next": "bg" },
        });
        let (items, next) = page::<CommandInfo>(full, "command list").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(next.as_deref(), Some("bg"));

        let empty = json!({
            "data": [],
            "cursor": { "previous": "cA", "next": "bg" },
        });
        let (items, next) = page::<CommandInfo>(empty, "command list").unwrap();
        assert!(items.is_empty());
        assert!(next.is_none());
    }

    /// `through` carries no message id on the request side even though the
    /// server always reports one back.
    #[test]
    fn fork_boundaries_have_distinct_read_and_write_shapes() {
        assert_eq!(
            serde_json::to_value(ForkRequestBoundary::Through).unwrap(),
            json!({ "type": "through" })
        );
        assert_eq!(
            serde_json::to_value(ForkRequestBoundary::Before {
                message_id: "msg_1".to_owned(),
            })
            .unwrap(),
            json!({ "type": "before", "messageID": "msg_1" })
        );
        let reported: ForkBoundary =
            serde_json::from_value(json!({ "type": "through", "messageID": "msg_1" })).unwrap();
        assert_eq!(reported.kind, ForkBoundaryKind::Through);
        assert_eq!(reported.message_id, "msg_1");
    }

    /// A live tool call sends `input` as a raw JSON string while streaming and
    /// as an object from `running` onwards.
    #[test]
    fn tool_input_changes_json_type_mid_call() {
        let streaming: ToolState =
            serde_json::from_value(json!({ "status": "streaming", "input": "{\"comm" })).unwrap();
        assert_eq!(
            streaming,
            ToolState::Streaming {
                input: json!("{\"comm"),
            }
        );
        let completed: ToolState = serde_json::from_value(json!({
            "status": "completed",
            "input": { "command": "ls" },
            "content": [{ "type": "text", "text": "src\n" }],
        }))
        .unwrap();
        let ToolState::Completed { input, content, .. } = completed else {
            panic!("expected a completed tool state");
        };
        assert_eq!(input, json!({ "command": "ls" }));
        assert_eq!(
            content,
            vec![ToolContent::Text {
                text: "src\n".to_owned(),
            }]
        );
    }

    /// A killed shell reports its exit code as a literal string, so a numeric
    /// field would fail the whole transcript page.
    #[test]
    fn shell_exit_accepts_a_non_finite_string() {
        let message: MessageInfo = serde_json::from_value(json!({
            "type": "shell",
            "id": "msg_1",
            "time": { "created": 1.0 },
            "shellID": "sh_1",
            "command": "sleep 1",
            "status": "killed",
            "exit": "NaN",
        }))
        .unwrap();
        let MessageInfo::Shell { exit, .. } = message else {
            panic!("expected a shell message");
        };
        assert_eq!(exit, Some(json!("NaN")));
    }

    /// A message kind from a newer build must degrade, not fail the page it
    /// arrived in.
    #[test]
    fn unknown_message_kinds_degrade_instead_of_failing_the_page() {
        let messages: Vec<MessageInfo> = serde_json::from_value(json!([
            { "type": "user", "id": "msg_1", "time": { "created": 1.0 }, "text": "hi" },
            { "type": "telepathy", "id": "msg_2" },
        ]))
        .unwrap();
        assert!(matches!(messages[0], MessageInfo::User { .. }));
        assert_eq!(messages[1], MessageInfo::Unknown);
    }

    #[test]
    fn assistant_messages_decode_their_parts_and_usage() {
        let message: MessageInfo = serde_json::from_value(json!({
            "type": "assistant",
            "id": "msg_1",
            "time": { "created": 1.0, "completed": 2.0 },
            "agent": "build",
            "model": { "id": "deepseek-v4-flash", "providerID": "opencode-go", "variant": "default" },
            "content": [{ "type": "text", "text": "Done." }],
            "tokens": { "input": 3.0, "output": 5.0, "reasoning": 0.0, "cache": { "read": 1.0, "write": 0.0 } },
        }))
        .unwrap();
        let MessageInfo::Assistant {
            model,
            content,
            tokens,
            ..
        } = message
        else {
            panic!("expected an assistant message");
        };
        assert_eq!(model.provider_id, "opencode-go");
        assert_eq!(model.variant.as_deref(), Some("default"));
        assert_eq!(
            content,
            vec![AssistantContent::Text {
                text: "Done.".to_owned(),
                state: None,
            }]
        );
        assert_eq!(tokens.unwrap().reasoning, 0.0);
    }

    #[test]
    fn session_info_decodes_a_live_payload() {
        let session: SessionInfo = serde_json::from_value(json!({
            "id": "ses_02aa4e12effe65w4DQ1XVBdJFb",
            "projectID": "21df6372c8bf8d6df2d187e68a8f6fbf653fa9a7",
            "agent": "build",
            "model": { "id": "deepseek-v4-flash", "providerID": "opencode-go", "variant": "default" },
            "cost": 0.0043574664,
            "tokens": { "input": 29493.0, "output": 181.0, "reasoning": 0.0, "cache": { "read": 63488.0, "write": 0.0 } },
            "time": { "created": 1785990946513.0, "updated": 1785991003042.0 },
            "title": "Running ls command",
            "location": { "directory": "/Users/egoist/dev/waku" },
        }))
        .unwrap();
        assert_eq!(session.location.directory, "/Users/egoist/dev/waku");
        assert!(session.outcome.is_none());
        assert!(session.parent_id.is_none());
    }

    /// The inbox entry's payload member is `payload` on this build; the older
    /// `data` name is gone rather than aliased.
    #[test]
    fn inbox_user_reads_the_payload_member() {
        let entry: InboxUser = serde_json::from_value(json!({
            "id": "msg_1",
            "sessionID": "ses_1",
            "timeCreated": 1.0,
            "type": "user",
            "payload": { "text": "hi" },
            "delivery": "steer",
        }))
        .unwrap();
        assert_eq!(entry.delivery, Delivery::Steer);
        assert_eq!(entry.payload["text"], json!("hi"));
    }

    #[test]
    fn form_fields_decode_every_kind() {
        let form: FormInfo = serde_json::from_value(json!({
            "id": "frm_1",
            "sessionID": "ses_1",
            "title": "Pick one",
            "fields": [
                { "type": "string", "key": "name", "required": true, "maxLength": 40 },
                { "type": "multiselect", "key": "tags", "options": [{ "value": "a", "label": "A" }], "minItems": 1 },
                { "type": "external", "key": "auth", "url": "https://example.test" },
                { "type": "hologram", "key": "future" },
            ],
        }))
        .unwrap();
        assert!(matches!(form.fields[0], FormField::String { .. }));
        assert!(matches!(form.fields[1], FormField::Multiselect { .. }));
        assert!(matches!(form.fields[2], FormField::External { .. }));
        assert_eq!(form.fields[3], FormField::Unknown);
    }

    #[test]
    fn form_answers_serialize_by_field_key() {
        let answer = FormAnswer::from([
            ("name".to_owned(), FormValue::Text("waku".to_owned())),
            ("count".to_owned(), FormValue::Number(2.0)),
            ("agree".to_owned(), FormValue::Bool(true)),
            (
                "tags".to_owned(),
                FormValue::List(vec!["a".to_owned(), "b".to_owned()]),
            ),
        ]);
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            json!({ "agree": true, "count": 2.0, "name": "waku", "tags": ["a", "b"] })
        );
    }

    /// `always` writes a rule into the user's own permission config, so it is
    /// refused before any request goes out.
    #[test]
    fn saving_a_permission_rule_is_refused_without_a_request() {
        let error = reply_permission(
            &Endpoint::local(1),
            "ses_1",
            "per_1",
            PermissionReply::Always,
        )
        .unwrap_err();
        assert!(matches!(error, ApiError::Transport(_)));
        assert_eq!(
            serde_json::to_value(PermissionReply::Once).unwrap(),
            json!("once")
        );
    }

    #[test]
    fn a_structured_error_body_keeps_its_tag_and_status() {
        let body = r#"{"_tag":"SessionNotFoundError","sessionID":"ses_x","message":"Session not found: ses_x"}"#;
        let endpoint = serve_once(Box::leak(
            format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        ));
        let error = get_session(&endpoint, "ses_x").unwrap_err();
        assert_eq!(error.tag(), Some("SessionNotFoundError"));
        assert_eq!(error.status(), Some(404));
        assert!(error.is_not_found());
        assert!(!error.is_unresolvable_location());
    }

    /// A location-scoped route handed an unresolvable directory answers a
    /// bodyless 500, which is a bad workspace rather than a server fault.
    #[test]
    fn a_bodyless_500_reads_as_an_unresolvable_workspace() {
        let endpoint =
            serve_once("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
        let error = list_models(&endpoint, Some("/nope/nope")).unwrap_err();
        assert!(error.is_unresolvable_location(), "{error:?}");
        assert_eq!(error.status(), Some(500));
    }

    #[test]
    fn active_sessions_are_the_keys_of_the_payload() {
        let endpoint = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 41\r\n\r\n{\"data\":{\"ses_a\":{\"type\":\"running\"}}}\r\n\r\n",
        );
        let active = active_sessions(&endpoint).unwrap();
        assert_eq!(active, HashSet::from(["ses_a".to_owned()]));
    }

    /// Reads the user's own running service. Kept out of the default run
    /// because it depends on a daemon this test must never start.
    #[test]
    #[ignore = "requires a running opencode2 service"]
    fn live_service_answers_health_and_catalogues() {
        let registration = std::fs::read_to_string(
            dirs::state_dir()
                .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
                .expect("a state directory")
                .join("opencode/service.json"),
        )
        .expect("a registered opencode2 service");
        let registration: Value = serde_json::from_str(&registration).unwrap();
        let endpoint = Endpoint::basic(
            registration["url"].as_str().unwrap(),
            "opencode",
            registration["password"].as_str().unwrap(),
        )
        .unwrap();

        let health = health(&endpoint).unwrap();
        assert!(health.healthy);
        assert_eq!(health.pid, registration["pid"].as_u64().unwrap() as u32);

        let directory = std::env::current_dir().unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let directory = directory.to_str().unwrap();
        // The model catalogue is refreshed from models.dev in the background
        // and is briefly empty across a refresh, so only its decode is
        // asserted; the built-in agents are always there.
        let _ = list_models(&endpoint, Some(directory)).unwrap();
        assert!(!list_agents(&endpoint, Some(directory)).unwrap().is_empty());
        let _ = list_commands(&endpoint, Some(directory)).unwrap();
        let _ = list_skills(&endpoint, Some(directory)).unwrap();
        let _ = active_sessions(&endpoint).unwrap();

        let (sessions, _) =
            list_sessions(&endpoint, Some(directory), None, Order::Desc, 5, None).unwrap();
        if let Some(session) = sessions.first() {
            assert_eq!(get_session(&endpoint, &session.id).unwrap().id, session.id);
            let (messages, _) =
                list_messages(&endpoint, &session.id, Order::Asc, Some(5), None).unwrap();
            assert!(
                messages
                    .iter()
                    .all(|message| *message != MessageInfo::Unknown)
            );
            assert!(list_permissions(&endpoint, &session.id).is_ok());
            assert!(list_forms(&endpoint, &session.id).is_ok());
            assert!(list_inbox(&endpoint, &session.id).is_ok());
        }
    }
}

//! OpenCode 2 sessions over the one adopted background service.
//!
//! Everything structural about this driver follows from a single fact: Waku
//! does not own an OpenCode 2 process. `opencode2_service` finds the daemon
//! the user's own terminal already started, and every Waku task rides the one
//! `GET /api/event` stream it exposes. So this file has no server handle, no
//! process teardown and no exit budget — dropping a driver unsubscribes and
//! sends `Shutdown`, and that is the whole of it.
//!
//! The second structural change from v1 is that commands and events land in
//! ONE worker thread driven by `crossbeam::select!`. All mutable stream state
//! is therefore thread-local: v1's `Arc<OpenCodeStreamState>`,
//! `Arc<Mutex<OpenCodePermissionState>>` and `Arc<Mutex<bool>>` turn flag all
//! disappear, and so does its per-session usage-metadata poller — context
//! windows come from the service-level catalogue cache.
//!
//! Three traps are encoded here rather than left to a reader to remember:
//!
//! * **The execution outcome is the settle point.** `session.execution.`
//!   `{succeeded,failed,interrupted}` ends the turn and emits exactly one
//!   `TurnFinished`; settling is idempotent, so a steered second message —
//!   which joins the running execution rather than starting its own — still
//!   settles once. Do NOT wait for `session.idle`: 0.0.0-beta-19192 does not
//!   emit it (verified against a live turn; the stream ends at
//!   `session.execution.*` and nothing follows), and waiting for it pins every
//!   finished turn to Working forever. A prompt whose HTTP call fails settles
//!   the turn itself, because no execution ever starts.
//! * **A stream break is never `ProcessExited`.** v1 ends its reader by
//!   reporting the process gone; for an adopted service that would kill a
//!   perfectly healthy session every time the socket blinked. Reconnection is
//!   the service's job and arrives here as `HubFrame::Resync`.
//! * **`always` never goes on the wire.** An `always` reply writes into
//!   `/api/permission/saved`, a GLOBAL store shared with the user's terminal
//!   and every other workspace, so durable choices stay in this driver's own
//!   state and every provider reply is one-shot. See `driver::support`.
//!
//! Everything in this file blocks on sockets and runs on the worker thread or
//! a daemon request thread. Nothing here is reachable from a frame.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use serde_json::{Value, json};
use uuid::Uuid;

use super::activity;
use super::support::{self, OpenCodePermissionRequest, OpenCodePermissionState};
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::http_wire::Endpoint;
use crate::model::{
    ActivityKind, BackgroundWorkEvent, BackgroundWorkItem, BackgroundWorkKind,
    BackgroundWorkStatus, DriverEvent, PermissionOption, ProviderResumeCursor, ReportedCommand,
    RuntimeMode, UserInputAnswer, UserInputOption, UserInputQuestion,
};
use crate::opencode2_api::{
    self, ApiError, AssistantContent, Delivery, ForkRequestBoundary, FormAnswer, FormField,
    FormInfo, FormValue, MessageInfo, ModelRef, Order, PermissionReply, SessionOutcome, TokenUsage,
    ToolContent, ToolState,
};
use crate::opencode2_service::{self, HubFrame, Opencode2Service, Subscription};

/// A one-shot user action posted onto the worker waits this long before Waku
/// gives up on it. Comfortably past the API layer's own fork budget, so a slow
/// fork answers rather than being reported as a timeout twice.
const ACTION_TIMEOUT: Duration = Duration::from_secs(150);
/// An option change is a live UI interaction: a `false` answer restarts the
/// driver, which is always correct, so waiting long for a `true` is pointless.
const OPTIONS_TIMEOUT: Duration = Duration::from_secs(5);

/// Event names the pre-beta `next` channel used for payloads the beta still
/// sends under a new name.
///
/// Between `next-17028` and `beta-19192` — three weeks apart — `session.input.*`
/// became `session.inbox.*`, and the beta OpenAPI hides the whole event union
/// behind `V2EventEncoded: {"type": "string"}`. A generated exhaustive
/// `#[serde(tag = "type")]` enum would therefore have failed to deserialize on
/// upgrade day; a string match plus this table drives both channels from one
/// binary.
///
/// `question.*` is deliberately NOT aliased onto `form.*`. The beta deleted
/// every question route in favour of forms and the payloads are not the same
/// shape — a form field carries a `key` that an answer round-trips, a question
/// never had one — so a next-era build gets no forms rather than mangled ones.
const ALIASES: &[(&str, &str)] = &[
    ("session.input.enqueued", "session.inbox.enqueued"),
    ("session.input.delivered", "session.inbox.delivered"),
    ("session.input.cancelled", "session.inbox.cancelled"),
    (
        "session.input.delivery.changed",
        "session.inbox.delivery.changed",
    ),
];

/// Provider control markers, which must never reach a transcript.
///
/// `session.tool.called` carries the provider's own metadata as a SIBLING of
/// `input` (`state`), and the completion events nest it under `resultState`.
/// Verified live to hold `{itemId, reasoningEncryptedContent: <multi-KB
/// base64>}`: it is private, it is enormous, and it is not content.
///
/// The bare `state` key is deliberately absent from this list. Nothing here
/// ever hands a whole event payload to the activity layer, so a sibling
/// marker cannot leak — whereas stripping `state` recursively would blank a
/// tool argument that legitimately has a field by that name.
const PROVIDER_STATE_KEYS: [&str; 3] = ["providerState", "resultState", "providerResultState"];

/// The one thing the event decoder needs from the shared service.
///
/// A trait rather than the concrete handle so the pure-logic tests can decode
/// a whole event family without an adopted daemon; a miss already means "not
/// known yet", never "unlimited".
pub(super) trait ContextWindows {
    fn context_window(&self, key: &str) -> Option<u64>;
}

impl ContextWindows for Arc<Opencode2Service> {
    fn context_window(&self, key: &str) -> Option<u64> {
        self.model_context_window(key)
    }
}

#[cfg(test)]
impl ContextWindows for HashMap<String, u64> {
    fn context_window(&self, key: &str) -> Option<u64> {
        self.get(key).copied()
    }
}

enum DriverCommand {
    Prompt(String),
    Steer(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    RespondUserInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
    ApplyOptions(SessionOptions, Sender<bool>),
    Fork {
        turns: usize,
        reply: Sender<anyhow::Result<ProviderResumeCursor>>,
    },
    Shutdown,
}

/// A streaming assistant part.
///
/// Text and reasoning share ONE ordinal namespace per assistant message, so
/// one map holding this enum is correct; two maps would let a reasoning part
/// and a text part with the same ordinal overwrite each other's repair state.
/// The accumulated text is what the reconnect gap-fill compares against, and
/// `session.*.ended` replaces it with the server's authoritative copy.
#[derive(Debug, Eq, PartialEq)]
enum PartKind {
    Text(String),
    Reasoning(String),
}

impl PartKind {
    fn new(reasoning: bool) -> Self {
        if reasoning {
            Self::Reasoning(String::new())
        } else {
            Self::Text(String::new())
        }
    }

    fn text_mut(&mut self) -> &mut String {
        match self {
            Self::Text(text) | Self::Reasoning(text) => text,
        }
    }

    fn is_reasoning(&self) -> bool {
        matches!(self, Self::Reasoning(_))
    }
}

/// One in-flight tool call. Tools carry no ordinal — they are keyed by
/// `(assistantMessageID, id)` — and they are removed only on success or
/// failure, never on `session.tool.called`, because v2 can still emit
/// `session.tool.progress` afterwards.
#[derive(Clone, Debug)]
struct ToolSlot {
    kind: ActivityKind,
    title: String,
    /// `session.tool.input.delta` streams the argument object as JSON TEXT,
    /// so the title can only be upgraded once it parses.
    input_text: String,
    input: Option<Value>,
    metadata: Option<Value>,
}

/// The assistant message the provider is currently writing.
#[derive(Clone, Debug)]
struct StepState {
    message_id: String,
    #[allow(dead_code)]
    agent: Option<String>,
    /// A step can run a different model than the session (a subagent step
    /// does), so the context window is looked up from here first.
    model: Option<ModelRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TurnOutcome {
    success: bool,
    summary: Option<String>,
}

/// `session.execution.*` arms the outcome and then settles the turn. Settling
/// is idempotent, which is what keeps a steered turn to exactly one
/// `TurnFinished`.
#[derive(Debug, Default, Eq, PartialEq)]
enum TurnState {
    #[default]
    Idle,
    Running {
        outcome: Option<TurnOutcome>,
    },
}

/// The prompt or steer whose inbox entry has not been delivered yet.
#[derive(Clone, Debug)]
struct PendingInput {
    inbox_id: String,
    message: String,
    /// A steer reports its own acceptance from `session.inbox.enqueued`; a
    /// fresh prompt has nothing to report.
    steer: bool,
}

struct StreamState {
    session_id: String,
    mode: RuntimeMode,
    parts: HashMap<(String, u64), PartKind>,
    tools: HashMap<(String, String), ToolSlot>,
    /// Stable placement per assistant message, so a reconnect repairs tool
    /// rows in the order they were opened rather than hash order.
    tool_order: HashMap<String, Vec<String>>,
    step: Option<StepState>,
    turn: TurnState,
    /// Three-state: pending (asked the user), responding (answered, waiting
    /// for `permission.replied`), approved (remembered wildcard rules). The
    /// dedupe is what absorbs the overlap between a snapshot and the live
    /// stream.
    permissions: OpenCodePermissionState,
    /// The forms Waku has surfaced, with their fields: a reply must round-trip
    /// each field's own `key`, which is a fidelity gain over v1's positional
    /// answers.
    forms: HashMap<String, Vec<FormField>>,
    pending_input: Option<PendingInput>,
    /// The connection this state belongs to; a reconciliation answer from a
    /// superseded pass must not overwrite newer state.
    generation: u64,
    /// The session-level model, moved by `session.model.selected`.
    model: Option<ModelRef>,
    /// Unknown event types are dropped and counted, never fatal.
    unknown: HashMap<String, u64>,
}

impl StreamState {
    fn new(
        session_id: String,
        mode: RuntimeMode,
        model: Option<ModelRef>,
        generation: u64,
    ) -> Self {
        Self {
            session_id,
            mode,
            parts: HashMap::new(),
            tools: HashMap::new(),
            tool_order: HashMap::new(),
            step: None,
            turn: TurnState::Idle,
            permissions: OpenCodePermissionState::default(),
            forms: HashMap::new(),
            pending_input: None,
            generation,
            model,
            unknown: HashMap::new(),
        }
    }

    fn model_key(&self) -> Option<String> {
        self.step
            .as_ref()
            .and_then(|step| step.model.as_ref())
            .or(self.model.as_ref())
            .map(|model| format!("{}/{}", model.provider_id, model.id))
    }

    fn begin_turn(&mut self, events: &impl DriverEventSink) {
        if matches!(self.turn, TurnState::Running { .. }) {
            return;
        }
        self.turn = TurnState::Running { outcome: None };
        let _ = events.send(DriverEvent::TurnStarted);
    }

    fn arm(&mut self, success: bool, summary: Option<String>) {
        if let TurnState::Running { outcome } = &mut self.turn {
            outcome.get_or_insert(TurnOutcome { success, summary });
        }
    }

    /// The single settle point: exactly one `TurnFinished` per open turn, no
    /// matter how many execution outcomes armed it.
    fn settle(&mut self, events: &impl DriverEventSink, fallback: Option<TurnOutcome>) {
        let TurnState::Running { outcome } = std::mem::take(&mut self.turn) else {
            return;
        };
        let outcome = outcome.or(fallback).unwrap_or(TurnOutcome {
            success: true,
            summary: None,
        });
        let _ = events.send(DriverEvent::TurnFinished {
            success: outcome.success,
            summary: outcome.summary,
        });
    }

    /// Ends the turn and drops the per-turn scratch.
    ///
    /// Idempotent through [`Self::settle`], so a second execution outcome — or
    /// a `session.idle` from a build that still sends one — cannot emit a
    /// second `TurnFinished`.
    fn finish_turn(&mut self, events: &impl DriverEventSink) {
        self.permissions.pending.clear();
        self.permissions.responding.clear();
        self.parts.clear();
        self.step = None;
        self.settle(events, None);
    }
}

/// Everything the worker needs to talk back to the service and the app.
struct Worker {
    service: Arc<Opencode2Service>,
    session_id: String,
    /// The canonicalized workspace path, reused verbatim for `?directory=`:
    /// the server compares those by exact string equality.
    directory: String,
    command_names: HashSet<String>,
    events: DriverEventSender,
    commands: Sender<DriverCommand>,
}

pub(super) struct OpenCode2Driver {
    /// The service object is permanent and this is never a lease over the
    /// user's process: nothing about dropping a driver can reach it.
    #[allow(dead_code)]
    service: Arc<Opencode2Service>,
    /// Dropped before `Shutdown` is sent, so the hub stops fanning frames out
    /// to a worker that is on its way out.
    subscription: Option<Subscription>,
    /// What this driver was started with. The worker owns the live copies —
    /// a mode change is absorbed there — so these are identity, not state.
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    mode: RuntimeMode,
    commands: Sender<DriverCommand>,
    supports_steer: bool,
}

impl OpenCode2Driver {
    /// Runs on the daemon request thread. Blocking is allowed here, but every
    /// wait is bounded: discovery and the health probe live in
    /// `opencode2_service`, and everything below is one local HTTP call.
    pub(super) fn start(
        options: DriverStartOptions,
        events: DriverEventSender,
    ) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            model,
            reasoning_effort,
            service_tier: _,
            context_window: _,
            agent_preset,
            computer_use_enabled: _,
            provider_cursor,
        } = options;

        let resumed = match provider_cursor {
            Some(ProviderResumeCursor::OpenCode2 { session_id, .. }) => {
                (!session_id.is_empty()).then_some(session_id)
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume OpenCode 2 from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };

        // One canonical string with no trailing slash, reused for BOTH
        // `location.directory` on create and `?directory=` on list. The server
        // canonicalizes neither and filters by exact string equality, so a
        // trailing slash or a subdirectory silently yields zero sessions — and
        // a directory the service cannot resolve answers HTTP 500 with an
        // empty body rather than a readable error.
        let directory = std::fs::canonicalize(&cwd)
            .with_context(|| {
                format!(
                    "OpenCode 2 needs a resolvable workspace directory, but {} could not be canonicalized",
                    cwd.display()
                )
            })?
            .to_string_lossy()
            .into_owned();

        let service = opencode2_service::shared(&binary)?;
        let endpoint = service.endpoint();
        let resuming = resumed.is_some();
        let session_id = resumed.unwrap_or_else(|| format!("ses_{}", Uuid::new_v4().simple()));

        // SUBSCRIBE BEFORE CREATE. The service can emit this session's first
        // event before `POST /api/session` has answered; the create body takes
        // a client-minted id precisely so that race cannot exist.
        let subscription = service.subscribe(&session_id);
        let frames = subscription.rx.clone();

        let agents = opencode2_api::list_agents(&endpoint, Some(&directory)).unwrap_or_default();
        let requested_agent = agent_preset.filter(|preset| !preset.is_empty());
        let agent = resolve_agent(requested_agent.as_deref(), &agents);
        let model = model_ref(model.as_deref(), reasoning_effort.as_deref());

        let create = || {
            opencode2_api::create_session(
                &endpoint,
                &session_id,
                Some(&agent),
                model.as_ref(),
                &directory,
            )
            .map_err(|error| anyhow!("could not open an OpenCode 2 session: {error}"))
        };
        let session = match resuming {
            // A cursor can outlive the session it names — the user's own
            // client can delete it — so a resume that 404s starts fresh under
            // the same id rather than failing the task.
            true => match opencode2_api::get_session(&endpoint, &session_id) {
                Ok(session) => session,
                Err(error) if error.is_not_found() => create()?,
                Err(error) => {
                    return Err(anyhow!("could not read the OpenCode 2 session: {error}"));
                }
            },
            false => create()?,
        };

        // A resumed session keeps whatever the user's own client last chose,
        // so an explicit Waku selection is re-applied and everything else is
        // left alone. Neither is fatal: a session that will not switch is
        // still a session Waku can drive.
        let agent = if resuming {
            match requested_agent {
                Some(_) if session.agent.as_deref() != Some(agent.as_str()) => {
                    let _ = opencode2_api::switch_agent(&endpoint, &session_id, &agent);
                    agent
                }
                _ => session.agent.clone().unwrap_or(agent),
            }
        } else {
            agent
        };
        if resuming
            && let Some(model) = model.as_ref()
            && session.model.as_ref() != Some(model)
        {
            let _ = opencode2_api::switch_model(&endpoint, &session_id, model);
        }

        // The session's own token totals are a LIFETIME cumulative counter and
        // cannot gauge how full the window is, so read the latest assistant
        // message's per-request usage instead.
        let session_model = model.clone().or_else(|| session.model.clone());
        let context_tokens = resumed_context_tokens(&endpoint, &session_id);
        let context_window = session_model
            .as_ref()
            .and_then(|model| service.model_context_window(&model_key(model)));
        if context_tokens.is_some() || context_window.is_some() {
            let _ = events.send(DriverEvent::UsageUpdated {
                context_tokens,
                context_window,
            });
        }

        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::OpenCode2 {
                session_id: session_id.clone(),
                directory: Some(directory.clone()),
            }),
        });
        let _ = events.send(DriverEvent::AgentPresetSelected(Some(agent)));
        if let Some(title) = generated_title(session.title.as_deref()) {
            let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title)));
        }
        let native_commands =
            opencode2_api::list_commands(&endpoint, Some(&directory)).unwrap_or_default();
        let command_names = native_commands
            .iter()
            .map(|command| command.name.clone())
            .collect();
        let reported = reported_commands(
            native_commands,
            opencode2_api::list_skills(&endpoint, Some(&directory)).unwrap_or_default(),
        );
        if !reported.is_empty() {
            let _ = events.send(DriverEvent::AvailableCommands(reported));
        }

        let (commands, command_rx) = unbounded();
        let worker = Worker {
            service: Arc::clone(&service),
            session_id: session_id.clone(),
            directory,
            command_names,
            events,
            commands: commands.clone(),
        };
        let generation = service.generation();
        let mut state = StreamState::new(session_id.clone(), mode, session_model, generation);
        thread::Builder::new()
            .name(format!("waku-opencode2-{session_id}"))
            .spawn(move || {
                // The snapshot runs in the same sequential position as the
                // event loop, before a single frame is decoded. Deferring it
                // would let a concurrently decoded `session.idle` take the
                // turn flag before a pending native permission — which proves
                // the resumed turn is live — could restore it.
                reconcile(&worker, &mut state, generation);
                loop {
                    crossbeam_channel::select! {
                        recv(command_rx) -> message => {
                            let Ok(message) = message else { return };
                            if !handle_command(&worker, message, &mut state) {
                                return;
                            }
                        }
                        recv(frames) -> frame => {
                            let Ok(frame) = frame else { return };
                            match frame {
                                HubFrame::Event(envelope) => handle_event(
                                    &envelope,
                                    &mut state,
                                    &worker.events,
                                    &worker.commands,
                                    &worker.service,
                                ),
                                // Reconnection is the service's job and is
                                // already under way. A break is NEVER a
                                // process exit for an adopted daemon.
                                HubFrame::Disconnected => {}
                                HubFrame::Resync { generation } => {
                                    reconcile(&worker, &mut state, generation);
                                }
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            service,
            subscription: Some(subscription),
            session_id,
            mode,
            commands,
            // Decided by the transport, snapshotted at `Command::Start` and
            // shipped in `ResponsePayload::Started`, so it cannot change
            // mid-session.
            supports_steer: true,
        })
    }
}

impl DriverControl for OpenCode2Driver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(DriverCommand::Prompt(prompt));
    }

    fn supports_steer(&self) -> bool {
        self.supports_steer
    }

    fn steer(&self, prompt: String) {
        let _ = self.commands.send(DriverCommand::Steer(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.send(DriverCommand::Cancel);
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.send(DriverCommand::Respond {
            request_id,
            option_id,
        });
    }

    fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        let _ = self.commands.send(DriverCommand::RespondUserInput {
            request_id,
            answers,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // Unlike v1 the access mode is not installed on the session — v2 has
        // no session-local ruleset — so a mode change is absorbed in place by
        // the worker's own state rather than restarting the driver.
        let (reply, answer) = bounded(1);
        if self
            .commands
            .send(DriverCommand::ApplyOptions(options, reply))
            .is_err()
        {
            return false;
        }
        answer.recv_timeout(OPTIONS_TIMEOUT).unwrap_or(false)
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        if turns == 0 {
            return Ok(None);
        }
        self.fork(turns).map(Some)
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        let (reply, answer) = bounded(1);
        self.commands
            .send(DriverCommand::Fork {
                turns: turns_to_remove,
                reply,
            })
            .map_err(|_| anyhow!("the OpenCode 2 driver is shutting down"))?;
        answer
            .recv_timeout(ACTION_TIMEOUT)
            .map_err(|_| anyhow!("OpenCode 2 did not answer the fork request"))?
    }
}

impl Drop for OpenCode2Driver {
    fn drop(&mut self) {
        // No socket surgery, no process signal, no exit budget: the clean
        // consequence of owning no server. Unsubscribe first so the hub stops
        // fanning out, then wake the worker so it winds down.
        drop(self.subscription.take());
        let _ = self.commands.send(DriverCommand::Shutdown);
    }
}

fn model_key(model: &ModelRef) -> String {
    // Keyed on `providerID/id`. `Model.Ref` has no `modelID` — that field
    // exists only on the catalogue entry — so v1's key builder cannot be
    // reused verbatim.
    format!("{}/{}", model.provider_id, model.id)
}

/// Waku stores a model as `"provider/model"`; v2 wants the two apart, plus the
/// variant that carries reasoning effort (`low`/`high`).
fn model_ref(model: Option<&str>, reasoning_effort: Option<&str>) -> Option<ModelRef> {
    let (provider_id, id) = model?.split_once('/')?;
    (!provider_id.is_empty() && !id.is_empty()).then(|| ModelRef {
        id: id.to_owned(),
        provider_id: provider_id.to_owned(),
        variant: reasoning_effort
            .map(str::to_owned)
            .filter(|variant| !variant.is_empty()),
    })
}

/// The agent this session runs.
///
/// Waku's access modes do not name an agent — v2 has no read-only product mode
/// in this tree — so the choice is the user's own preset when the service
/// still lists it as a selectable primary, and `build` otherwise.
fn resolve_agent(preset: Option<&str>, agents: &[opencode2_api::AgentInfo]) -> String {
    let selectable = |name: &str| {
        agents.iter().any(|agent| {
            agent.id == name && !agent.hidden && agent.mode != opencode2_api::AgentMode::Subagent
        })
    };
    match preset {
        Some(preset) if !preset.is_empty() && (agents.is_empty() || selectable(preset)) => {
            preset.to_owned()
        }
        _ => "build".to_owned(),
    }
}

/// Context-window occupancy. v2 reports reasoning tokens separately instead of
/// folding them into output, so leaving them out under-reports every thinking
/// model.
fn context_tokens(tokens: &TokenUsage) -> Option<u64> {
    let total = [
        tokens.input,
        tokens.output,
        tokens.reasoning,
        tokens.cache.read,
        tokens.cache.write,
    ]
    .into_iter()
    .filter(|count| count.is_finite() && *count > 0.0)
    .fold(0_u64, |total, count| total.saturating_add(count as u64));
    (total > 0).then_some(total)
}

/// Context-window occupancy for a resumed or reconnected session.
///
/// `SessionInfo.tokens` is a *lifetime* cumulative counter — `cache.read` in
/// particular only ever grows over a session, so it can run millions of tokens
/// past the model's window while the actual context is tiny. Gauge occupancy
/// from the latest assistant message's per-request token total instead, which
/// is what opencode's own context meter reads. A stale or unknown value is
/// `None`, which the meter already degrades gracefully to.
fn resumed_context_tokens(endpoint: &Endpoint, session_id: &str) -> Option<u64> {
    let Ok((messages, _)) =
        opencode2_api::list_messages(endpoint, session_id, Order::Desc, Some(20), None)
    else {
        return None;
    };
    for message in messages {
        let MessageInfo::Assistant { tokens, .. } = message else {
            continue;
        };
        if let Some(tokens) = tokens
            && let Some(context_tokens) = context_tokens(&tokens)
        {
            return Some(context_tokens);
        }
    }
    None
}

/// Keeps v1's placeholder filter: OpenCode emits `New session - <timestamp>`
/// before its title-generation model call, and that is not a title.
fn generated_title(title: Option<&str>) -> Option<String> {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty() && !title.starts_with("New session - "))
        .map(str::to_owned)
}

fn reported_commands(
    commands: Vec<opencode2_api::CommandInfo>,
    skills: Vec<opencode2_api::SkillInfo>,
) -> Vec<ReportedCommand> {
    let mut seen = HashSet::new();
    commands
        .into_iter()
        .map(|command| ReportedCommand {
            name: command.name,
            description: command.description.unwrap_or_default(),
        })
        // Only skills the user can actually type reach the composer; the rest
        // are model-invoked and would be noise in a command palette.
        .chain(
            skills
                .into_iter()
                .filter(|skill| skill.slash.unwrap_or(false))
                .map(|skill| ReportedCommand {
                    name: skill.name,
                    description: skill.description.unwrap_or_default(),
                }),
        )
        .filter(|command| !command.name.is_empty() && seen.insert(command.name.clone()))
        .collect()
}

fn canonical_event(kind: &str) -> &str {
    ALIASES
        .iter()
        .find_map(|(from, to)| (*from == kind).then_some(*to))
        .unwrap_or(kind)
}

fn strip_provider_state(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| !PROVIDER_STATE_KEYS.contains(&key.as_str()));
            for nested in object.values_mut() {
                strip_provider_state(nested);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_provider_state(item);
            }
        }
        _ => {}
    }
}

fn stripped(value: Option<&Value>) -> Option<Value> {
    let mut value = value.filter(|value| !value.is_null()).cloned()?;
    strip_provider_state(&mut value);
    Some(value)
}

/// Waku's access mode, applied locally.
///
/// v2 exposes no session-local permission ruleset — `/api/permission/saved` is
/// a GLOBAL store shared with the user's own terminal — so unlike v1 the mode
/// cannot be installed on the session. What reaches Waku is whatever the
/// resolved agent's own rules mark `ask`; the mode only decides who answers.
fn auto_replies(mode: RuntimeMode, action: &str) -> bool {
    match mode {
        RuntimeMode::Ask => false,
        RuntimeMode::AutoAcceptEdits => matches!(action, "edit" | "write" | "patch"),
        RuntimeMode::Auto | RuntimeMode::FullAccess => true,
    }
}

fn native_command_invocation<'a>(
    text: &'a str,
    commands: &HashSet<String>,
) -> Option<(&'a str, &'a str)> {
    let invocation = text.strip_prefix('/')?;
    let (name, arguments) = invocation
        .split_once(char::is_whitespace)
        .unwrap_or((invocation, ""));
    commands.contains(name).then(|| (name, arguments.trim()))
}

fn submit_prompt(
    worker: &Worker,
    endpoint: &Endpoint,
    text: &str,
    delivery: Option<Delivery>,
) -> Result<Option<opencode2_api::InboxUser>, ApiError> {
    if let Some((name, arguments)) = native_command_invocation(text, &worker.command_names) {
        opencode2_api::command(endpoint, &worker.session_id, name, arguments, delivery)
            .map(|_| None)
    } else {
        opencode2_api::prompt(endpoint, &worker.session_id, text, delivery).map(Some)
    }
}

fn handle_command(worker: &Worker, message: DriverCommand, state: &mut StreamState) -> bool {
    let endpoint = worker.service.endpoint();
    let events = &worker.events;
    match message {
        DriverCommand::Prompt(text) => {
            state.begin_turn(events);
            match submit_prompt(worker, &endpoint, &text, None) {
                Ok(Some(inbox)) => {
                    // The EFFECTIVE delivery is read back rather than assumed:
                    // the server picks one when the caller sends none.
                    state.pending_input = Some(PendingInput {
                        inbox_id: inbox.id,
                        message: text,
                        steer: inbox.delivery == Delivery::Steer,
                    });
                }
                Ok(None) => state.pending_input = None,
                Err(error) => {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.provider_rejected_prompt_detail",
                        provider = "OpenCode 2",
                        error = error
                    )));
                    // `session.idle` never arrives for a turn that never
                    // started, so settle it here instead of spinning forever.
                    state.settle(
                        events,
                        Some(TurnOutcome {
                            success: false,
                            summary: Some(tr!(
                                "errors.provider_start_turn",
                                provider = "OpenCode 2"
                            )),
                        }),
                    );
                }
            }
        }
        DriverCommand::Steer(text) => {
            if matches!(state.turn, TurnState::Idle) {
                let _ = events.send(DriverEvent::SteerRejected {
                    message: text,
                    reason: tr!("errors.provider_no_active_turn", provider = "OpenCode 2"),
                });
                return true;
            }
            match submit_prompt(worker, &endpoint, &text, Some(Delivery::Steer)) {
                Ok(Some(inbox)) => {
                    // A server that queued the message anyway is promoted, so
                    // "steer" means the same thing on both paths.
                    if inbox.delivery != Delivery::Steer {
                        let _ =
                            opencode2_api::steer_inbox(&endpoint, &worker.session_id, &inbox.id);
                    }
                    state.pending_input = Some(PendingInput {
                        inbox_id: inbox.id,
                        message: text.clone(),
                        steer: true,
                    });
                    // The inbox event is authoritative; this 2xx is only the
                    // fallback for a response Waku never sees.
                    let _ = events.send(DriverEvent::SteerAccepted { message: text });
                }
                Ok(None) => {
                    let _ = events.send(DriverEvent::SteerAccepted { message: text });
                }
                Err(error) => {
                    let _ = events.send(DriverEvent::SteerRejected {
                        message: text,
                        reason: tr!(
                            "errors.provider_rejected_steer",
                            provider = "OpenCode 2",
                            error = error
                        ),
                    });
                }
            }
        }
        DriverCommand::Cancel => {
            if let Err(error) = opencode2_api::interrupt(&endpoint, &worker.session_id) {
                let _ = events.send(DriverEvent::Error(tr!(
                    "errors.stop_provider",
                    provider = "OpenCode 2",
                    error = error
                )));
            }
        }
        DriverCommand::Respond {
            request_id,
            option_id,
        } => {
            for (request_id, option_id) in
                support::permission_responses_in(&mut state.permissions, &request_id, &option_id)
            {
                let reply = match option_id.as_str() {
                    "reject" => PermissionReply::Reject,
                    // Never `always`: see the module doc.
                    _ => PermissionReply::Once,
                };
                if let Err(error) = opencode2_api::reply_permission(
                    &endpoint,
                    &worker.session_id,
                    &request_id,
                    reply,
                ) {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.answer_provider_permission",
                        provider = "OpenCode 2",
                        error = error
                    )));
                }
            }
        }
        DriverCommand::RespondUserInput {
            request_id,
            answers,
        } => {
            let Some(fields) = state.forms.remove(&request_id) else {
                return true;
            };
            let answer = form_answer(&fields, &answers);
            if let Err(error) =
                opencode2_api::reply_form(&endpoint, &worker.session_id, &request_id, &answer)
            {
                let _ = events.send(DriverEvent::Error(tr!(
                    "errors.answer_provider_question",
                    provider = "OpenCode 2",
                    error = error
                )));
            }
        }
        DriverCommand::ApplyOptions(options, reply) => {
            let applied = apply_options(worker, &endpoint, &options, state);
            let _ = reply.send(applied);
        }
        DriverCommand::Fork { turns, reply } => {
            let _ = reply.send(fork_session(&endpoint, worker, turns));
        }
        DriverCommand::Shutdown => return false,
    }
    true
}

fn apply_options(
    worker: &Worker,
    endpoint: &Endpoint,
    options: &SessionOptions,
    state: &mut StreamState,
) -> bool {
    let model = model_ref(
        options.model.as_deref(),
        options.reasoning_effort.as_deref(),
    );
    if model != state.model
        && let Some(model) = model.as_ref()
        && opencode2_api::switch_model(endpoint, &worker.session_id, model).is_err()
    {
        return false;
    }
    if model.is_some() {
        state.model = model;
    }
    state.mode = options.mode;
    true
}

fn fork_session(
    endpoint: &Endpoint,
    worker: &Worker,
    turns_to_remove: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    // The boundary is the first REMOVED user turn; `through` takes no message
    // id at all and sending one is a 400, so the two shapes are distinct.
    let user_messages = user_message_ids(endpoint, &worker.session_id)?;
    let retained = user_messages
        .len()
        .checked_sub(turns_to_remove)
        .ok_or_else(|| {
            anyhow!(
                "OpenCode 2 has only {} native turns, but Waku needs to remove {turns_to_remove}",
                user_messages.len()
            )
        })?;
    let boundary = match user_messages.get(retained) {
        Some(message_id) => ForkRequestBoundary::Before {
            message_id: message_id.clone(),
        },
        None => ForkRequestBoundary::Through,
    };
    let fork = opencode2_api::fork(endpoint, &worker.session_id, &boundary)
        .map_err(|error| anyhow!("could not fork the OpenCode 2 session: {error}"))?;
    Ok(ProviderResumeCursor::OpenCode2 {
        session_id: fork.id,
        directory: Some(worker.directory.clone()),
    })
}

fn user_message_ids(endpoint: &Endpoint, session_id: &str) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut cursor = None;
    loop {
        // Ascending: the server defaults to `desc` on `/message`, and a fork
        // boundary is meaningless without conversation order.
        let (messages, next) =
            opencode2_api::list_messages(endpoint, session_id, Order::Asc, None, cursor.as_deref())
                .map_err(|error| anyhow!("could not read the OpenCode 2 transcript: {error}"))?;
        for message in &messages {
            if let MessageInfo::User { id, .. } = message {
                ids.push(id.clone());
            }
        }
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(ids)
}

/// Repairs this session against the server after a stream break, and takes the
/// initial snapshot when the driver starts.
///
/// Deliberately append-only: the transcript rides the runtime event journal,
/// and replacing it wholesale from `/message` would clobber every other client
/// that already saved a projection. Full replay stays confined to the cold
/// import path.
fn reconcile(worker: &Worker, state: &mut StreamState, generation: u64) {
    if generation < state.generation {
        return;
    }
    state.generation = generation;
    let endpoint = worker.service.endpoint();
    let events = &worker.events;

    let session = match opencode2_api::get_session(&endpoint, &worker.session_id) {
        Ok(session) => Some(session),
        Err(error) if error.is_not_found() => {
            // A foreign client deleting this session is authoritative.
            let _ = events.send(DriverEvent::Error(tr!(
                "errors.provider_reported_error",
                provider = "OpenCode 2"
            )));
            let _ = events.send(DriverEvent::ProcessExited);
            return;
        }
        Err(_) => None,
    };

    if let Some(session) = session.as_ref() {
        if let Some(title) = generated_title(session.title.as_deref()) {
            let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title)));
        }
        if let Some(model) = session.model.clone() {
            state.model = Some(model);
        }
        let context_tokens = resumed_context_tokens(&endpoint, &worker.session_id);
        let context_window = state
            .model_key()
            .and_then(|key| worker.service.model_context_window(&key));
        if context_tokens.is_some() || context_window.is_some() {
            let _ = events.send(DriverEvent::UsageUpdated {
                context_tokens,
                context_window,
            });
        }
    }

    // Permissions BEFORE the turn edge: a pending native request proves the
    // resumed turn is live, and the three-state dedupe absorbs the overlap
    // with frames already buffered in the hub channel.
    let mut blocked = false;
    if let Ok(pending) = opencode2_api::list_permissions(&endpoint, &worker.session_id) {
        blocked = !pending.is_empty();
        for request in pending {
            // `PermissionRequest` is a read type; the decoder wants the wire
            // shape, so the snapshot is re-expressed rather than re-decoded.
            let value = json!({
                "id": request.id,
                "action": request.action,
                "resources": request.resources,
                "save": request.save,
                "message": request.message,
            });
            request_permission(&value, state, events, &worker.commands);
        }
    }
    if let Ok(forms) = opencode2_api::list_forms(&endpoint, &worker.session_id) {
        blocked = blocked || !forms.is_empty();
        for form in forms {
            surface_form(form, state, events);
        }
    }
    if let Ok(inbox) = opencode2_api::list_inbox(&endpoint, &worker.session_id) {
        let undelivered = state.pending_input.as_ref().is_some_and(|pending| {
            inbox
                .iter()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some(&pending.inbox_id))
        });
        if !undelivered {
            state.pending_input = None;
        }
    }

    let draining = opencode2_api::active_sessions(&endpoint)
        .map(|active| active.contains(&worker.session_id))
        .unwrap_or(false);
    if draining || blocked {
        state.begin_turn(events);
    } else {
        let failed = session
            .as_ref()
            .and_then(|session| session.outcome)
            .is_some_and(|outcome| outcome == SessionOutcome::Failed);
        state.settle(
            events,
            Some(TurnOutcome {
                success: !failed,
                summary: None,
            }),
        );
    }

    gap_fill(&endpoint, worker, state);
}

/// Appends whatever the in-flight assistant message gained while the stream
/// was down, keyed by `(assistantMessageID, ordinal)`.
fn gap_fill(endpoint: &Endpoint, worker: &Worker, state: &mut StreamState) {
    let Some(step) = state.step.clone() else {
        return;
    };
    let Ok((messages, _)) =
        opencode2_api::list_messages(endpoint, &worker.session_id, Order::Desc, Some(1), None)
    else {
        return;
    };
    let Some(MessageInfo::Assistant { id, content, .. }) = messages.into_iter().next() else {
        return;
    };
    if id != step.message_id {
        return;
    }
    let events = &worker.events;
    // The content array's own order is the ordinal namespace the events use.
    for (ordinal, part) in content.iter().enumerate() {
        let ordinal = ordinal as u64;
        match part {
            AssistantContent::Text { text, .. } => {
                append_suffix(state, events, &id, ordinal, text, false);
            }
            AssistantContent::Reasoning { text, .. } => {
                append_suffix(state, events, &id, ordinal, text, true);
            }
            AssistantContent::Tool {
                id: call_id,
                name,
                state: tool_state,
                ..
            } => {
                repair_tool(state, events, &id, call_id, name, tool_state);
            }
            AssistantContent::Unknown => {}
        }
    }
}

fn append_suffix(
    state: &mut StreamState,
    events: &impl DriverEventSink,
    message_id: &str,
    ordinal: u64,
    text: &str,
    reasoning: bool,
) {
    let part = state
        .parts
        .entry((message_id.to_owned(), ordinal))
        .or_insert_with(|| PartKind::new(reasoning));
    if part.is_reasoning() != reasoning {
        return;
    }
    let seen = part.text_mut();
    // Append-only. A body that does not extend what the transcript already
    // shows is a rewrite, and a rewrite is exactly what must not happen.
    let Some(suffix) = text.strip_prefix(seen.as_str()) else {
        return;
    };
    if suffix.is_empty() {
        return;
    }
    let delta = suffix.to_owned();
    seen.push_str(&delta);
    let _ = events.send(if reasoning {
        DriverEvent::ReasoningDelta(delta)
    } else {
        DriverEvent::TextDelta(delta)
    });
}

fn repair_tool(
    state: &mut StreamState,
    events: &impl DriverEventSink,
    message_id: &str,
    call_id: &str,
    name: &str,
    tool_state: &ToolState,
) {
    let key = (message_id.to_owned(), call_id.to_owned());
    if !state.tools.contains_key(&key) {
        return;
    }
    let (failed, output) = match tool_state {
        ToolState::Completed { content, .. } => (false, Some(tool_content(content))),
        ToolState::Error { error, .. } => (true, Some(Value::String(error.message.clone()))),
        // Still open on the server, so there is nothing to repair.
        _ => return,
    };
    let mut slot = state.tools.remove(&key).unwrap_or_else(|| ToolSlot {
        kind: support::classify_tool(name),
        title: name.to_owned(),
        input_text: String::new(),
        input: None,
        metadata: None,
    });
    slot.title = activity::input_title(slot.input.as_ref()).unwrap_or(slot.title);
    emit_tool(events, call_id, &slot, output.as_ref(), failed, true);
}

/// Flattens a completed tool's content blocks into the shape the shared
/// normalizer already knows how to render and harvest images from.
fn tool_content(content: &[ToolContent]) -> Value {
    Value::Array(
        content
            .iter()
            .filter_map(|item| match item {
                ToolContent::Text { text } => Some(json!({"type": "text", "text": text})),
                ToolContent::File { uri, mime, name } => {
                    Some(json!({"type": "file", "url": uri, "mime": mime, "name": name}))
                }
                ToolContent::Unknown => None,
            })
            .collect(),
    )
}

fn handle_event(
    envelope: &Value,
    state: &mut StreamState,
    events: &impl DriverEventSink,
    commands: &Sender<DriverCommand>,
    service: &impl ContextWindows,
) {
    let kind = canonical_event(
        envelope
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let data = envelope.get("data").unwrap_or(&Value::Null);

    match kind {
        // Turn lifecycle. The execution outcome IS the terminal event:
        // beta-19192 does not emit `session.idle` at all (verified against a
        // live turn on 0.0.0-beta-19192 — the stream ends at
        // `session.execution.*` and nothing follows), so waiting for one left
        // every turn pinned to Working forever. `session.idle` is still
        // handled below in case a build sends it; settling is idempotent.
        "session.execution.started" => state.begin_turn(events),
        "session.step.started" => {
            let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str) else {
                return;
            };
            state.step = Some(StepState {
                message_id: message_id.to_owned(),
                agent: data.get("agent").and_then(Value::as_str).map(str::to_owned),
                model: serde_json::from_value(data.get("model").cloned().unwrap_or(Value::Null))
                    .ok(),
            });
        }
        "session.step.ended" => {
            emit_usage(data.get("tokens"), state, events, service);
        }
        "session.step.failed" => {
            let _ = events.send(DriverEvent::Error(error_message(data.get("error"))));
        }
        "session.execution.succeeded" => {
            state.arm(true, None);
            state.finish_turn(events);
        }
        "session.execution.failed" => {
            let message = error_message(data.get("error"));
            let _ = events.send(DriverEvent::Error(message.clone()));
            state.arm(false, Some(message));
            state.finish_turn(events);
        }
        "session.execution.interrupted" => {
            let reason = data
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_owned();
            // `shutdown` means the service itself is going away: the stream
            // will break and the reconnect path repairs this session. Failing
            // the turn here would report a provider error the user never
            // caused. A user or superseded interrupt did stop the work, and
            // says so.
            let success = reason == "shutdown";
            state.arm(success, Some(reason));
            state.finish_turn(events);
        }
        // Retained for forward/backward compatibility only; beta-19192 never
        // sends this. Harmless after the outcome already settled the turn.
        "session.idle" => state.finish_turn(events),

        // Streaming. Text and reasoning share one ordinal namespace.
        "session.text.started" => start_part(data, state, false),
        "session.reasoning.started" => start_part(data, state, true),
        "session.text.delta" => stream_delta(data, state, events, false),
        "session.reasoning.delta" => stream_delta(data, state, events, true),
        // Emit NOTHING: the deltas already carried this text. Re-emitting it
        // doubles every assistant paragraph. It is stored because it is the
        // authoritative body the reconnect gap-fill compares against.
        "session.text.ended" => end_part(data, state, false),
        "session.reasoning.ended" => end_part(data, state, true),

        // Tools. Keyed by `(assistantMessageID, id)`, no ordinal.
        "session.tool.input.started" => {
            let Some((key, id)) = tool_key(data, state) else {
                return;
            };
            let name = data
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| tr!("activity.tool"));
            let slot = ToolSlot {
                kind: support::classify_tool(&name),
                title: name,
                input_text: String::new(),
                input: None,
                metadata: None,
            };
            state
                .tool_order
                .entry(key.0.clone())
                .or_default()
                .push(id.clone());
            // Emit at once so the row appears while the arguments stream.
            emit_tool(events, &id, &slot, None, false, false);
            state.tools.insert(key, slot);
        }
        "session.tool.input.delta" => {
            let Some((key, id)) = tool_key(data, state) else {
                return;
            };
            let Some(delta) = data.get("delta").and_then(Value::as_str) else {
                return;
            };
            let Some(slot) = state.tools.get_mut(&key) else {
                return;
            };
            slot.input_text.push_str(delta);
            let Ok(mut input) = serde_json::from_str::<Value>(&slot.input_text) else {
                return;
            };
            strip_provider_state(&mut input);
            let title = activity::input_title(Some(&input));
            slot.input = Some(input);
            // Only a title change is worth another row; a row per delta would
            // be a transcript update at token rate.
            if let Some(title) = title.filter(|title| *title != slot.title) {
                slot.title = title;
                let slot = slot.clone();
                emit_tool(events, &id, &slot, None, false, false);
            }
        }
        "session.tool.called" => {
            let Some((key, id)) = tool_key(data, state) else {
                return;
            };
            let Some(slot) = state.tools.get_mut(&key) else {
                return;
            };
            if let Some(input) = stripped(data.get("input")) {
                slot.title = activity::input_title(Some(&input)).unwrap_or(slot.title.clone());
                slot.input = Some(input);
            }
            // `executed: false` means produced-but-not-run — a denied or
            // interrupted call — and must never render as a completed one.
            let slot = slot.clone();
            emit_tool(events, &id, &slot, None, false, false);
        }
        "session.tool.progress" => {
            let Some((key, id)) = tool_key(data, state) else {
                return;
            };
            let Some(slot) = state.tools.get_mut(&key) else {
                return;
            };
            slot.metadata = stripped(data.get("metadata"));
            let slot = slot.clone();
            let metadata = slot.metadata.clone();
            // v1 has no equivalent; this is what makes a long bash or grep
            // visibly alive instead of a frozen row.
            emit_tool(events, &id, &slot, metadata.as_ref(), false, false);
        }
        "session.tool.success" => complete_tool(data, state, events, false),
        "session.tool.failed" => complete_tool(data, state, events, true),

        // Inbox and steering.
        "session.inbox.enqueued" => {
            let Some(id) = data.get("id").and_then(Value::as_str) else {
                return;
            };
            let Some(pending) = state.pending_input.as_mut() else {
                return;
            };
            if pending.inbox_id.is_empty() {
                pending.inbox_id = id.to_owned();
            }
        }
        "session.inbox.delivered" => {
            if state.pending_input.as_ref().is_some_and(|pending| {
                Some(pending.inbox_id.as_str()) == data.get("id").and_then(Value::as_str)
            }) {
                state.pending_input = None;
            }
        }
        "session.inbox.cancelled" => {
            let Some(pending) = state.pending_input.as_ref() else {
                return;
            };
            if Some(pending.inbox_id.as_str()) != data.get("id").and_then(Value::as_str) {
                return;
            }
            // The app falls back to its own follow-up queue.
            if pending.steer {
                let _ = events.send(DriverEvent::SteerRejected {
                    message: pending.message.clone(),
                    reason: tr!("errors.provider_rejected_prompt", provider = "OpenCode 2"),
                });
            }
            state.pending_input = None;
        }

        // Permissions.
        "permission.asked" => request_permission(data, state, events, commands),
        // NAME ASYMMETRY: `asked` puts the id at `data.id`, `replied` calls it
        // `data.requestID`, and the reply path takes the `asked` id.
        "permission.replied" | "permission.rejected" => {
            if let Some(request_id) = data
                .get("requestID")
                .or_else(|| data.get("id"))
                .and_then(Value::as_str)
            {
                state.permissions.pending.remove(request_id);
                state.permissions.responding.remove(request_id);
            }
        }

        // Forms. NEVER auto-cancelled: that would silently destroy a
        // structured question the agent is blocked on.
        "form.created" => {
            let Ok(form) = serde_json::from_value::<FormInfo>(
                data.get("form").cloned().unwrap_or(Value::Null),
            ) else {
                return;
            };
            surface_form(form, state, events);
        }
        "form.replied" | "form.cancelled" => {
            if let Some(id) = data.get("id").and_then(Value::as_str) {
                state.forms.remove(id);
            }
        }

        // Session level.
        // The session's LIFETIME cumulative token counter (`cache.read` never
        // shrinks) can run far past the model's window while the actual context
        // is small, so this is deliberately NOT a context-occupancy source.
        // Occupancy comes from per-step `session.step.ended` tokens above,
        // which reflect the actual request. Keep the arm so the event is
        // acknowledged rather than recorded as unknown.
        "session.usage.updated" => {}
        "session.renamed" => {
            if let Some(title) = generated_title(data.get("title").and_then(Value::as_str)) {
                let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title)));
            }
        }
        "session.agent.selected" => {
            let _ = events.send(DriverEvent::AgentPresetSelected(
                data.get("agent").and_then(Value::as_str).map(str::to_owned),
            ));
        }
        "session.model.selected" => {
            if let Ok(model) = serde_json::from_value::<ModelRef>(
                data.get("model").cloned().unwrap_or(Value::Null),
            ) {
                state.model = Some(model);
            }
        }
        "session.status" | "session.retry.scheduled" => retry_activity(kind, data, state, events),
        "session.shell.started" => shell_work(data, events, true),
        "session.shell.ended" => shell_work(data, events, false),
        // Compaction deltas carry NO `assistantMessageID` and NO ordinal, so
        // routing them as a `TextDelta` would drop the compaction summary
        // inside the assistant's own reply. One row keyed on the session.
        "session.compaction.started" => compaction_activity(state, events, None, false, false),
        "session.compaction.delta" => compaction_activity(
            state,
            events,
            data.get("text").and_then(Value::as_str),
            false,
            false,
        ),
        "session.compaction.ended" => compaction_activity(state, events, None, true, false),
        "session.compaction.failed" => compaction_activity(state, events, None, true, true),
        // Activities, never user messages: a synthetic note is the harness
        // talking to the model, not the user.
        "session.synthetic" => {
            let title = data
                .get("description")
                .or_else(|| data.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| tr!("activity.activity"));
            let item = activity::tool_activity(
                data.get("id").and_then(Value::as_str).map(str::to_owned),
                ActivityKind::Tool,
                title,
                None,
                data.get("text"),
                None,
                false,
                true,
            );
            let _ = events.send(DriverEvent::RichActivity(item));
        }
        "session.skill.activated" => {
            let title = data
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| tr!("activity.activity"));
            let item = activity::tool_activity(
                data.get("id").and_then(Value::as_str).map(str::to_owned),
                ActivityKind::Tool,
                title,
                None,
                data.get("text"),
                None,
                false,
                true,
            );
            let _ = events.send(DriverEvent::RichActivity(item));
        }
        // Driver state only: the revert edge is what the rollback path reads,
        // and it is not transcript content.
        "session.revert.staged" | "session.revert.cleared" | "session.revert.committed" => {}
        "session.deleted" => {
            let _ = events.send(DriverEvent::Error(tr!(
                "errors.provider_reported_error",
                provider = "OpenCode 2"
            )));
            // Terminal, and always last: the runtime is not reinserted after
            // it.
            let _ = events.send(DriverEvent::ProcessExited);
        }

        _ if is_ignored(kind) => {}
        // Tolerant by requirement: three weeks of upstream churn renamed a
        // whole event family, and an unknown type must never be fatal.
        unknown => {
            *state.unknown.entry(unknown.to_owned()).or_default() += 1;
        }
    }
}

/// Event families that are known and deliberately dropped.
///
/// Deny-by-default: the union has 88 members and the stream is shared with
/// the user's own terminal, so nothing unattributable may reach a transcript.
/// Note that top-level `shell.*` belongs to `/api/shell` and has no session
/// id at all — it is not `session.shell.*`.
fn is_ignored(kind: &str) -> bool {
    const PREFIXES: [&str; 12] = [
        "tui.",
        "pty.",
        "mcp.",
        "installation.",
        "vcs.",
        "plugin.",
        "integration.",
        "reference.",
        "websearch.",
        "shell.",
        "project.",
        "lsp.",
    ];
    const EXACT: [&str; 12] = [
        "server.connected",
        "filesystem.changed",
        "catalog.updated",
        "agent.updated",
        "command.updated",
        "skill.updated",
        "config.updated",
        "models-dev.refreshed",
        "session.created",
        "session.viewed",
        "session.moved",
        "session.forked",
    ];
    const UNINTERESTING: [&str; 5] = [
        "session.instructions.updated",
        "session.step.streamed",
        // The input object is already complete on `session.tool.called`.
        "session.tool.input.ended",
        "session.inbox.delivery.changed",
        // Durable-union only; it can never arrive on the live stream.
        "session.usage.recorded",
    ];
    PREFIXES.iter().any(|prefix| kind.starts_with(prefix))
        || EXACT.contains(&kind)
        || UNINTERESTING.contains(&kind)
}

fn error_message(error: Option<&Value>) -> String {
    error
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("errors.provider_reported_error", provider = "OpenCode 2"))
}

fn emit_usage(
    tokens: Option<&Value>,
    state: &StreamState,
    events: &impl DriverEventSink,
    service: &impl ContextWindows,
) {
    let context_tokens = tokens
        .cloned()
        .and_then(|tokens| serde_json::from_value::<TokenUsage>(tokens).ok())
        .as_ref()
        .and_then(context_tokens);
    let context_window = state
        .model_key()
        .and_then(|key| service.context_window(&key));
    if context_tokens.is_none() && context_window.is_none() {
        return;
    }
    let _ = events.send(DriverEvent::UsageUpdated {
        context_tokens,
        context_window,
    });
}

fn part_key(data: &Value, state: &StreamState) -> Option<(String, u64)> {
    let message_id = data
        .get("assistantMessageID")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| state.step.as_ref().map(|step| step.message_id.clone()))?;
    let ordinal = data.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
    Some((message_id, ordinal))
}

fn tool_key(data: &Value, state: &StreamState) -> Option<((String, String), String)> {
    let message_id = data
        .get("assistantMessageID")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| state.step.as_ref().map(|step| step.message_id.clone()))?;
    let id = data.get("id").and_then(Value::as_str)?.to_owned();
    Some(((message_id, id.clone()), id))
}

fn start_part(data: &Value, state: &mut StreamState, reasoning: bool) {
    let Some(key) = part_key(data, state) else {
        return;
    };
    state.parts.insert(key, PartKind::new(reasoning));
}

fn stream_delta(
    data: &Value,
    state: &mut StreamState,
    events: &impl DriverEventSink,
    reasoning: bool,
) {
    let Some(delta) = data.get("delta").and_then(Value::as_str) else {
        return;
    };
    if delta.is_empty() {
        return;
    }
    if let Some(key) = part_key(data, state) {
        state
            .parts
            .entry(key)
            .or_insert_with(|| PartKind::new(reasoning))
            .text_mut()
            .push_str(delta);
    }
    let _ = events.send(if reasoning {
        DriverEvent::ReasoningDelta(delta.to_owned())
    } else {
        DriverEvent::TextDelta(delta.to_owned())
    });
}

fn end_part(data: &Value, state: &mut StreamState, reasoning: bool) {
    let Some(key) = part_key(data, state) else {
        return;
    };
    let Some(text) = data.get("text").and_then(Value::as_str) else {
        return;
    };
    let part = state
        .parts
        .entry(key)
        .or_insert_with(|| PartKind::new(reasoning));
    *part.text_mut() = text.to_owned();
}

fn complete_tool(
    data: &Value,
    state: &mut StreamState,
    events: &impl DriverEventSink,
    failed: bool,
) {
    let Some((key, id)) = tool_key(data, state) else {
        return;
    };
    // Removed ONLY here: v2 can still emit `session.tool.progress` after
    // `called`, so removing on `called` would strand every later update.
    let Some(mut slot) = state.tools.remove(&key) else {
        return;
    };
    if let Some(metadata) = stripped(data.get("metadata")) {
        slot.metadata = Some(metadata);
    }
    let output = if failed {
        stripped(data.get("error")).or_else(|| stripped(data.get("content")))
    } else {
        stripped(data.get("content"))
    };
    emit_tool(events, &id, &slot, output.as_ref(), failed, true);
}

/// Every tool row goes through the shared normalizer, so the 16 000-character
/// truncation, image harvesting and file-change precomputation are identical
/// to every other provider. Never hand-roll an `ActivityItem` here.
fn emit_tool(
    events: &impl DriverEventSink,
    id: &str,
    slot: &ToolSlot,
    output: Option<&Value>,
    failed: bool,
    complete: bool,
) {
    let item = activity::tool_activity(
        Some(id.to_owned()),
        slot.kind,
        slot.title.clone(),
        slot.input.as_ref(),
        output,
        slot.metadata.as_ref(),
        failed,
        complete,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

fn retry_activity(kind: &str, data: &Value, state: &StreamState, events: &impl DriverEventSink) {
    if kind == "session.status" && data.get("type").and_then(Value::as_str) != Some("retry") {
        return;
    }
    let title = data
        .pointer("/action/title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("activity.provider_retrying"));
    // The reason travels as TEXT, never as colour alone.
    let detail = data
        .pointer("/action/message")
        .or_else(|| data.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let detail = detail.map(Value::String);
    let item = activity::tool_activity(
        // One row, upserted: a retry that escalates must not stack rows.
        Some(format!("retry:{}", state.session_id)),
        ActivityKind::Tool,
        title,
        None,
        detail.as_ref(),
        None,
        false,
        false,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

fn compaction_activity(
    state: &StreamState,
    events: &impl DriverEventSink,
    delta: Option<&str>,
    complete: bool,
    failed: bool,
) {
    let title = if failed {
        tr!("activity.compaction_failed")
    } else if complete {
        tr!("activity.compacted_context")
    } else {
        tr!("activity.compacting_context")
    };
    let delta = delta.map(|delta| Value::String(delta.to_owned()));
    let item = activity::tool_activity(
        Some(format!("compaction:{}", state.session_id)),
        ActivityKind::Tool,
        title,
        None,
        delta.as_ref(),
        None,
        failed,
        complete,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

/// Session-level work that outlives the turn which created it.
///
/// Deliberately `BackgroundWork` rather than a transcript activity: it
/// bypasses `accepts_turn_output` so a detached shell survives a settled turn.
/// v1 never wires this at all.
fn shell_work(data: &Value, events: &impl DriverEventSink, running: bool) {
    let shell = data.get("shell").unwrap_or(&Value::Null);
    let Some(id) = shell.get("id").and_then(Value::as_str) else {
        return;
    };
    let command = shell
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let status = match (running, shell.get("status").and_then(Value::as_str)) {
        (true, _) => BackgroundWorkStatus::Running,
        (false, Some("killed")) => BackgroundWorkStatus::Stopped,
        (false, Some("timeout")) => BackgroundWorkStatus::Failed,
        (false, _) => BackgroundWorkStatus::Completed,
    };
    let mut item = BackgroundWorkItem::new(
        BackgroundWorkKind::Process,
        id,
        command
            .clone()
            .unwrap_or_else(|| tr!("activity.background_shell")),
        status,
    );
    item.background = true;
    item.command = command;
    item.control_id = Some(id.to_owned());
    item.output = shell
        .pointer("/output/output")
        .and_then(Value::as_str)
        .map(str::to_owned);
    item.output_truncated = shell
        .pointer("/output/truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    item.exit_code = shell
        .get("exit")
        .and_then(Value::as_i64)
        .and_then(|exit| i32::try_from(exit).ok());
    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
        item,
    )));
}

fn surface_form(form: FormInfo, state: &mut StreamState, events: &impl DriverEventSink) {
    if state.forms.contains_key(&form.id) {
        return;
    }
    let questions = form_questions(&form);
    if questions.is_empty() {
        return;
    }
    state.forms.insert(form.id.clone(), form.fields);
    let _ = events.send(DriverEvent::UserInputRequested {
        request_id: form.id,
        questions,
    });
}

/// One `Form.Field` becomes one question, keyed by the field's own `key` so
/// the answer round-trips — a fidelity gain over v1, which drops the question
/// id and relies on positional order.
fn form_questions(form: &FormInfo) -> Vec<UserInputQuestion> {
    form.fields
        .iter()
        .filter_map(|field| {
            let (key, title, options, multi_select, note) = match field {
                FormField::String {
                    key,
                    title,
                    options,
                    ..
                } => (key, title.clone(), options.clone(), false, None),
                FormField::Number { key, title, .. }
                | FormField::Integer { key, title, .. }
                | FormField::Boolean { key, title, .. } => (key, title.clone(), None, false, None),
                FormField::Multiselect {
                    key,
                    title,
                    options,
                    ..
                } => (key, title.clone(), Some(options.clone()), true, None),
                // Read-only in the first cut: the URL is the whole content.
                FormField::External {
                    key, title, url, ..
                } => (key, title.clone(), None, false, Some(url.clone())),
                FormField::Unknown => return None,
            };
            let header = title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| key.clone());
            let question = note.unwrap_or_else(|| {
                let title = form.title.trim();
                if title.is_empty() {
                    header.clone()
                } else {
                    title.to_owned()
                }
            });
            Some(UserInputQuestion {
                id: key.clone(),
                header,
                question,
                options: options
                    .into_iter()
                    .flatten()
                    .map(|option| UserInputOption {
                        label: option.label,
                        description: option.description,
                    })
                    .collect(),
                multi_select,
            })
        })
        .collect()
}

fn form_answer(fields: &[FormField], answers: &[UserInputAnswer]) -> FormAnswer {
    let mut answer = FormAnswer::new();
    for field in fields {
        let Some(key) = field_key(field) else {
            continue;
        };
        let Some(given) = answers
            .iter()
            .find(|answer| answer.question_id == key)
            .map(|answer| answer.answers.as_slice())
        else {
            continue;
        };
        // The card carries option LABELS; the server wants option values.
        let resolve = |value: &String| -> String {
            field_options(field)
                .iter()
                .find(|option| &option.label == value)
                .map(|option| option.value.clone())
                .unwrap_or_else(|| value.clone())
        };
        let value = match field {
            FormField::Multiselect { .. } => {
                FormValue::List(given.iter().map(resolve).collect::<Vec<_>>())
            }
            FormField::Boolean { .. } => FormValue::Bool(matches!(
                given.first().map(String::as_str),
                Some("true" | "yes" | "1")
            )),
            FormField::Number { .. } | FormField::Integer { .. } => given
                .first()
                .and_then(|value| value.parse::<f64>().ok())
                .map(FormValue::Number)
                .unwrap_or_else(|| FormValue::Text(given.first().cloned().unwrap_or_default())),
            _ => FormValue::Text(given.first().map(resolve).unwrap_or_default()),
        };
        answer.insert(key.to_owned(), value);
    }
    answer
}

fn field_key(field: &FormField) -> Option<&str> {
    match field {
        FormField::String { key, .. }
        | FormField::Number { key, .. }
        | FormField::Integer { key, .. }
        | FormField::Boolean { key, .. }
        | FormField::Multiselect { key, .. }
        | FormField::External { key, .. } => Some(key),
        FormField::Unknown => None,
    }
}

fn field_options(field: &FormField) -> &[opencode2_api::FormOption] {
    match field {
        FormField::String { options, .. } => options.as_deref().unwrap_or_default(),
        FormField::Multiselect { options, .. } => options,
        _ => &[],
    }
}

fn request_permission(
    data: &Value,
    state: &mut StreamState,
    events: &impl DriverEventSink,
    commands: &Sender<DriverCommand>,
) {
    // `permission.asked` puts the id at `data.id`, and that is the id the
    // reply path takes. `data.source` links the card to the exact tool row,
    // which the driver event has no field for yet.
    let Some(request_id) = data.get("id").and_then(Value::as_str) else {
        return;
    };
    let strings = |name: &str| {
        data.get(name)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let request = OpenCodePermissionRequest {
        permission: data
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        patterns: strings("resources"),
        always: strings("save"),
    };

    if state.permissions.pending.contains_key(request_id)
        || state.permissions.responding.contains(request_id)
    {
        return;
    }
    if auto_replies(state.mode, &request.permission) || state.permissions.is_approved(&request) {
        state.permissions.responding.insert(request_id.to_owned());
        // Posted back through the worker's own channel rather than inline so
        // one slow reply cannot stall the decoding of the frames behind it.
        let _ = commands.send(DriverCommand::Respond {
            request_id: request_id.to_owned(),
            option_id: "once".into(),
        });
        return;
    }

    state
        .permissions
        .pending
        .insert(request_id.to_owned(), request.clone());

    let action = if request.permission.is_empty() {
        tr!("permission.run_a_tool_lower")
    } else {
        request.permission.clone()
    };
    let resources = (!request.patterns.is_empty()).then(|| request.patterns.join(", "));
    let _ = events.send(DriverEvent::Permission {
        request_id: request_id.to_owned(),
        title: resources.clone().unwrap_or_else(|| {
            tr!(
                "permission.allow_named_permission",
                permission = action.as_str()
            )
        }),
        detail: data
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| match resources {
                Some(_) => tr!(
                    "permission.agent_asks_for_named_permission",
                    permission = action.as_str()
                ),
                None => tr!("permission.agent_asks_for_permission"),
            }),
        options: vec![
            PermissionOption {
                id: "once".into(),
                label: tr!("permission.allow_once"),
                allow: true,
            },
            PermissionOption {
                id: "always".into(),
                label: tr!("permission.always_allow"),
                allow: true,
            },
            PermissionOption {
                id: "reject".into(),
                label: tr!("common.deny"),
                allow: false,
            },
        ],
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossbeam_channel::Receiver;

    struct Harness {
        events: Sender<DriverEvent>,
        seen: Receiver<DriverEvent>,
        commands: Sender<DriverCommand>,
        issued: Receiver<DriverCommand>,
        windows: HashMap<String, u64>,
        state: StreamState,
    }

    impl Harness {
        fn new(mode: RuntimeMode) -> Self {
            let (events, seen) = unbounded();
            let (commands, issued) = unbounded();
            Self {
                events,
                seen,
                commands,
                issued,
                windows: HashMap::new(),
                state: StreamState::new("ses_1".into(), mode, None, 0),
            }
        }

        fn feed(&mut self, event: Value) {
            handle_event(
                &event,
                &mut self.state,
                &self.events,
                &self.commands,
                &self.windows,
            );
        }

        fn drain(&self) -> Vec<DriverEvent> {
            self.seen.try_iter().collect()
        }
    }

    fn step_started(message_id: &str) -> Value {
        json!({
            "type": "session.step.started",
            "data": {
                "sessionID": "ses_1",
                "assistantMessageID": message_id,
                "agent": "build",
                "model": {"id": "claude-sonnet-4-5", "providerID": "anthropic"}
            }
        })
    }

    /// The regression that shipped: beta-19192 emits NO `session.idle`, so a
    /// driver that settles only there leaves every finished turn stuck on
    /// "Working" forever. Captured live on 0.0.0-beta-19192, the terminal
    /// frame is `session.execution.*` and nothing follows it.
    #[test]
    fn turn_settles_without_any_session_idle() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        for event in [
            json!({"type": "session.execution.started", "data": {"sessionID": "ses_1"}}),
            step_started("msg_1"),
            json!({"type": "session.text.delta", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "ok"}}),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_1"}}),
        ] {
            harness.feed(event);
        }
        let seen = harness.drain();
        assert!(
            seen.iter()
                .any(|event| matches!(event, DriverEvent::TurnFinished { success: true, .. })),
            "execution.succeeded must settle the turn on its own: {seen:?}"
        );
    }

    /// The exact frame sequence captured from the live service for a turn that
    /// failed on provider auth. It must settle too, or a failed turn hangs.
    #[test]
    fn live_failure_frame_sequence_settles_the_turn() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        let error =
            json!({"type": "provider.auth", "message": "Insufficient balance.", "status": 401});
        for event in [
            json!({"type": "session.inbox.enqueued", "data": {"sessionID": "ses_1"}}),
            json!({"type": "session.execution.started", "data": {"sessionID": "ses_1"}}),
            json!({"type": "session.instructions.updated", "data": {"sessionID": "ses_1"}}),
            json!({"type": "session.inbox.delivered", "data": {"sessionID": "ses_1"}}),
            step_started("msg_1"),
            json!({"type": "session.step.failed", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "error": error}}),
            json!({"type": "session.execution.failed", "data": {"sessionID": "ses_1", "error": error}}),
        ] {
            harness.feed(event);
        }
        let seen = harness.drain();
        let finished: Vec<_> = seen
            .iter()
            .filter(|event| matches!(event, DriverEvent::TurnFinished { .. }))
            .collect();
        assert_eq!(finished.len(), 1, "exactly one TurnFinished: {seen:?}");
        assert!(matches!(
            finished[0],
            DriverEvent::TurnFinished { success: false, .. }
        ));
    }

    /// Walks one whole turn across every streaming family and proves the
    /// single settle point: two `session.execution.*` outcomes and a repeated
    /// `session.idle` still produce exactly one `TurnFinished`.
    #[test]
    fn streams_text_reasoning_and_tools_then_settles_exactly_once() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        for event in [
            json!({"type": "session.execution.started", "data": {"sessionID": "ses_1"}}),
            step_started("msg_1"),
            json!({"type": "session.reasoning.started", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0}}),
            json!({"type": "session.reasoning.delta", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "thinking"}}),
            json!({"type": "session.reasoning.ended", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "text": "thinking"}}),
            json!({"type": "session.text.started", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 1}}),
            json!({"type": "session.text.delta", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 1, "delta": "OK"}}),
            // Emits NOTHING: re-emitting the body would double the paragraph.
            json!({"type": "session.text.ended", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 1, "text": "OK"}}),
            json!({"type": "session.tool.input.started", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "id": "call_1", "name": "read"}}),
            json!({"type": "session.tool.called", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "id": "call_1", "input": {"filePath": "a.txt"}, "executed": true, "state": {"reasoningEncryptedContent": "SECRET"}}}),
            json!({"type": "session.tool.success", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "id": "call_1", "content": [{"type": "text", "text": "contents"}], "metadata": {"resultState": {"reasoningEncryptedContent": "SECRET"}}}}),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_1"}}),
            json!({"type": "session.idle", "data": {"sessionID": "ses_1"}}),
            json!({"type": "session.idle", "data": {"sessionID": "ses_1"}}),
        ] {
            harness.feed(event);
        }

        let seen = harness.drain();
        assert!(matches!(&seen[0], DriverEvent::TurnStarted));
        assert!(matches!(&seen[1], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[2], DriverEvent::TextDelta(text) if text == "OK"));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
            if item.kind == ActivityKind::FileRead && !item.complete));
        assert!(matches!(&seen[4], DriverEvent::RichActivity(item)
            if !item.complete && item.display_target.as_deref() == Some("a.txt")));
        assert!(
            matches!(&seen[5], DriverEvent::RichActivity(item) if item.complete && !item.failed)
        );
        assert!(matches!(
            &seen[6],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 7, "a second idle must not settle a second turn");
        assert!(harness.state.tools.is_empty());
        assert_eq!(harness.state.turn, TurnState::Idle);

        let rendered = seen
            .iter()
            .filter_map(|event| match event {
                DriverEvent::RichActivity(item) => Some(format!(
                    "{}{}",
                    item.arguments.clone().unwrap_or_default(),
                    item.output.clone().unwrap_or_default()
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("SECRET"),
            "provider control state must never reach a transcript"
        );
    }

    /// Text and reasoning share ONE ordinal namespace per assistant message,
    /// and `ended` stores the authoritative body for reconnect repair.
    #[test]
    fn text_and_reasoning_keep_separate_ordinals_in_one_namespace() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(step_started("msg_1"));
        harness.feed(json!({"type": "session.reasoning.started", "data": {"assistantMessageID": "msg_1", "ordinal": 0}}));
        harness.feed(json!({"type": "session.text.started", "data": {"assistantMessageID": "msg_1", "ordinal": 1}}));
        harness.feed(json!({"type": "session.text.delta", "data": {"assistantMessageID": "msg_1", "ordinal": 1, "delta": "part"}}));
        harness.feed(json!({"type": "session.text.ended", "data": {"assistantMessageID": "msg_1", "ordinal": 1, "text": "partial"}}));

        assert_eq!(harness.state.parts.len(), 2);
        assert_eq!(
            harness.state.parts.get(&("msg_1".into(), 0)),
            Some(&PartKind::Reasoning(String::new()))
        );
        assert_eq!(
            harness.state.parts.get(&("msg_1".into(), 1)),
            Some(&PartKind::Text("partial".into()))
        );
        assert!(matches!(
            harness.drain().as_slice(),
            [DriverEvent::TextDelta(text)] if text == "part"
        ));
    }

    /// v2 can still emit `session.tool.progress` after `session.tool.called`,
    /// so the slot is removed only on success or failure. Removing it on
    /// `called` — which is safe on v1, where completion is terminal — would
    /// strand every later update.
    #[test]
    fn tool_slots_survive_called_and_progress_and_are_removed_on_completion() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(step_started("msg_1"));
        harness.feed(json!({"type": "session.tool.input.started", "data": {"assistantMessageID": "msg_1", "id": "call_1", "name": "bash"}}));
        harness.feed(json!({"type": "session.tool.called", "data": {"assistantMessageID": "msg_1", "id": "call_1", "input": {"command": "sleep 5"}, "executed": false}}));
        assert_eq!(harness.state.tools.len(), 1);
        harness.feed(json!({"type": "session.tool.progress", "data": {"assistantMessageID": "msg_1", "id": "call_1", "metadata": {"output": "still running"}}}));
        assert_eq!(harness.state.tools.len(), 1);

        let seen = harness.drain();
        assert!(
            seen.iter().all(|event| matches!(
                event,
                DriverEvent::RichActivity(item) if !item.complete
            )),
            "produced-but-not-run must never render as a completed call"
        );
        assert_eq!(seen.len(), 3);

        harness.feed(json!({"type": "session.tool.failed", "data": {"assistantMessageID": "msg_1", "id": "call_1", "error": {"type": "tool.execution", "message": "boom"}}}));
        assert!(harness.state.tools.is_empty());
        assert!(matches!(
            harness.drain().as_slice(),
            [DriverEvent::RichActivity(item)] if item.complete && item.failed
        ));
        assert_eq!(
            harness.state.tool_order.get("msg_1").map(Vec::as_slice),
            Some(["call_1".to_owned()].as_slice())
        );
    }

    /// `session.execution.*` only arms the outcome; `session.idle` disarms it.
    #[test]
    fn a_failed_execution_settles_once_at_idle_with_its_own_error() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness.feed(json!({"type": "session.execution.failed", "data": {"error": {"message": "provider refused"}}}));
        harness.feed(json!({"type": "session.execution.succeeded", "data": {}}));
        harness.feed(json!({"type": "session.idle", "data": {}}));

        let seen = harness.drain();
        assert!(matches!(&seen[0], DriverEvent::TurnStarted));
        assert!(matches!(&seen[1], DriverEvent::Error(error) if error == "provider refused"));
        assert!(matches!(
            &seen[2],
            DriverEvent::TurnFinished {
                success: false,
                summary: Some(summary)
            } if summary == "provider refused"
        ));
        assert_eq!(seen.len(), 3);
    }

    /// A service going down is a reconnect, not a failed turn: the stream
    /// breaks and the `Resync` reconcile repairs the session.
    #[test]
    fn a_shutdown_interrupt_does_not_fail_the_turn() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness
            .feed(json!({"type": "session.execution.interrupted", "data": {"reason": "shutdown"}}));
        harness.feed(json!({"type": "session.idle", "data": {}}));

        let seen = harness.drain();
        assert!(matches!(
            &seen[1],
            DriverEvent::TurnFinished { success: true, .. }
        ));

        let mut cancelled = Harness::new(RuntimeMode::FullAccess);
        cancelled.feed(json!({"type": "session.execution.started", "data": {}}));
        cancelled
            .feed(json!({"type": "session.execution.interrupted", "data": {"reason": "user"}}));
        cancelled.feed(json!({"type": "session.idle", "data": {}}));
        assert!(matches!(
            &cancelled.drain()[1],
            DriverEvent::TurnFinished { success: false, summary: Some(summary) } if summary == "user"
        ));
    }

    /// The id asymmetry is real: `permission.asked` carries `data.id` and the
    /// reply path takes that id, while `permission.replied` calls it
    /// `data.requestID`.
    #[test]
    fn permission_requests_dedupe_and_translate_always_into_one_shot() {
        let mut harness = Harness::new(RuntimeMode::Ask);
        let asked = json!({
            "type": "permission.asked",
            "data": {
                "id": "per_abc",
                "sessionID": "ses_1",
                "action": "bash",
                "resources": ["rm -rf *"],
                "save": ["rm -rf *"],
                "source": {"type": "tool", "messageID": "msg_1", "id": "call_1"}
            }
        });
        harness.feed(asked.clone());
        // The live event can arrive while the snapshot is still being read.
        harness.feed(asked);

        let seen = harness.drain();
        assert_eq!(seen.len(), 1, "one request must produce one card");
        let DriverEvent::Permission {
            request_id,
            title,
            options,
            ..
        } = &seen[0]
        else {
            panic!("a supervising mode must surface the request");
        };
        assert_eq!(request_id, "per_abc");
        assert_eq!(title, "rm -rf *");
        assert_eq!(
            options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["once", "always", "reject"]
        );
        assert!(harness.issued.try_recv().is_err());

        assert_eq!(
            support::permission_responses_in(&mut harness.state.permissions, "per_abc", "always"),
            [("per_abc".to_owned(), "once".to_owned())],
            "a durable choice must never go on the wire"
        );

        harness.feed(json!({
            "type": "permission.asked",
            "data": {
                "id": "per_def",
                "sessionID": "ses_1",
                "action": "bash",
                "resources": ["rm -rf /tmp/waku-cache"],
                "save": ["rm -rf *"]
            }
        }));
        let Ok(DriverCommand::Respond { option_id, .. }) = harness.issued.try_recv() else {
            panic!("the remembered rule should answer without asking again");
        };
        assert_eq!(option_id, "once");
        assert!(harness.drain().is_empty());

        harness.feed(json!({
            "type": "permission.replied",
            "data": {"sessionID": "ses_1", "requestID": "per_def", "reply": "once"}
        }));
        assert!(!harness.state.permissions.responding.contains("per_def"));
    }

    #[test]
    fn access_modes_decide_who_answers_a_permission() {
        for (mode, action, asks) in [
            (RuntimeMode::Ask, "edit", true),
            (RuntimeMode::AutoAcceptEdits, "edit", false),
            (RuntimeMode::AutoAcceptEdits, "bash", true),
            (RuntimeMode::Auto, "bash", false),
            (RuntimeMode::FullAccess, "bash", false),
        ] {
            let mut harness = Harness::new(mode);
            harness.feed(json!({
                "type": "permission.asked",
                "data": {"id": "per_1", "sessionID": "ses_1", "action": action, "resources": ["x"]}
            }));
            assert_eq!(
                harness.drain().is_empty(),
                !asks,
                "{mode:?} should {} ask about {action}",
                if asks { "" } else { "not" }
            );
            assert_eq!(harness.issued.try_recv().is_ok(), !asks);
            assert!(
                harness.state.permissions.approved.is_empty(),
                "an automatic reply must not broaden future access"
            );
        }
    }

    /// Each field's own `key` round-trips through the answer, which v1 could
    /// not do: it dropped the question id and relied on positional order.
    #[test]
    fn forms_become_keyed_questions_whose_answers_round_trip() {
        let mut harness = Harness::new(RuntimeMode::Ask);
        harness.feed(json!({
            "type": "form.created",
            "data": {
                "form": {
                    "id": "frm_1",
                    "sessionID": "ses_1",
                    "title": "Which files should change?",
                    "fields": [
                        {
                            "type": "multiselect",
                            "key": "files",
                            "title": "Files",
                            "options": [
                                {"value": "src", "label": "Source", "description": "Implementation"},
                                {"value": "test", "label": "Tests"}
                            ]
                        },
                        {"type": "boolean", "key": "confirm", "title": "Proceed"}
                    ]
                }
            }
        }));

        let seen = harness.drain();
        let DriverEvent::UserInputRequested {
            request_id,
            questions,
        } = &seen[0]
        else {
            panic!("a form is a structured question, not a permission");
        };
        assert_eq!(request_id, "frm_1");
        assert_eq!(questions[0].id, "files");
        assert_eq!(questions[0].header, "Files");
        assert_eq!(questions[0].question, "Which files should change?");
        assert!(questions[0].multi_select);
        assert_eq!(questions[0].options[0].label, "Source");
        assert_eq!(
            questions[0].options[0].description.as_deref(),
            Some("Implementation")
        );
        assert!(!questions[1].multi_select);

        // A second `form.created` for the same form must not ask twice.
        assert_eq!(seen.len(), 1);

        let fields = harness.state.forms.get("frm_1").cloned().unwrap();
        let answer = form_answer(
            &fields,
            &[
                UserInputAnswer {
                    question_id: "files".into(),
                    answers: vec!["Source".into()],
                },
                UserInputAnswer {
                    question_id: "confirm".into(),
                    answers: vec!["true".into()],
                },
            ],
        );
        // Labels are what the card shows; values are what the server wants.
        assert_eq!(
            answer.get("files"),
            Some(&FormValue::List(vec!["src".to_owned()]))
        );
        assert_eq!(answer.get("confirm"), Some(&FormValue::Bool(true)));
    }

    /// v2 reports reasoning tokens separately instead of folding them into
    /// output, so leaving them out under-reports every thinking model. The
    /// window is keyed on `providerID/id`, never `modelID`.
    #[test]
    fn usage_sums_reasoning_and_cache_and_keys_the_window_on_provider_and_id() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness
            .windows
            .insert("anthropic/claude-sonnet-4-5".into(), 200_000);
        harness.feed(step_started("msg_1"));
        harness.feed(json!({
            "type": "session.step.ended",
            "data": {
                "sessionID": "ses_1",
                "assistantMessageID": "msg_1",
                "cost": 0.01,
                "tokens": {
                    "input": 13_399.0,
                    "output": 10.0,
                    "reasoning": 5.0,
                    "cache": {"read": 1_792.0, "write": 0.0}
                }
            }
        }));

        assert!(matches!(
            harness.drain().as_slice(),
            [DriverEvent::UsageUpdated {
                context_tokens: Some(15_206),
                context_window: Some(200_000)
            }]
        ));
    }

    /// Compaction deltas carry no `assistantMessageID` and no ordinal, so
    /// routing them as text would put the summary inside the assistant reply.
    #[test]
    fn compaction_streams_into_one_row_of_its_own() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.compaction.started", "data": {"sessionID": "ses_1"}}));
        harness.feed(json!({"type": "session.compaction.delta", "data": {"sessionID": "ses_1", "text": "summarising"}}));
        harness.feed(json!({"type": "session.compaction.ended", "data": {"sessionID": "ses_1"}}));

        let seen = harness.drain();
        assert_eq!(seen.len(), 3);
        let ids = seen
            .iter()
            .filter_map(|event| match event {
                DriverEvent::RichActivity(item) => item.source_id.clone(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, ["compaction:ses_1"; 3]);
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, DriverEvent::TextDelta(_))),
            "a compaction summary is not the assistant's reply"
        );
    }

    /// A detached shell must survive its turn, so it is background work
    /// rather than a transcript activity.
    #[test]
    fn session_shells_become_background_work_keyed_on_the_shell_id() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({
            "type": "session.shell.started",
            "data": {"sessionID": "ses_1", "shell": {"id": "shl_1", "command": "npm run dev", "status": "running"}}
        }));
        harness.feed(json!({
            "type": "session.shell.ended",
            "data": {"sessionID": "ses_1", "shell": {"id": "shl_1", "command": "npm run dev", "status": "exited", "exit": 0}}
        }));

        let seen = harness.drain();
        let statuses = seen
            .iter()
            .filter_map(|event| match event {
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) => {
                    Some((item.key.provider_id.clone(), item.status))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                ("shl_1".to_owned(), BackgroundWorkStatus::Running),
                ("shl_1".to_owned(), BackgroundWorkStatus::Completed),
            ]
        );
    }

    /// Deny by default. The union has 88 members and the stream is shared with
    /// the user's own terminal, so an unrecognized type is dropped and
    /// counted, never rendered and never fatal.
    #[test]
    fn unknown_types_are_counted_and_known_noise_is_dropped_silently() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        for event in [
            json!({"type": "server.connected", "id": "evt_1", "data": {}}),
            json!({"type": "mcp.status.changed", "data": {}}),
            json!({"type": "shell.created", "data": {}}),
            json!({"type": "session.usage.recorded", "data": {}}),
            json!({"type": "session.something.new", "data": {"sessionID": "ses_1"}}),
        ] {
            harness.feed(event);
        }

        assert!(harness.drain().is_empty());
        assert_eq!(
            harness.state.unknown,
            HashMap::from([("session.something.new".to_owned(), 1)])
        );
    }

    /// One binary drives both upstream channels: the beta renamed
    /// `session.input.*` to `session.inbox.*` three weeks after the `next`
    /// dump this integration was designed against.
    #[test]
    fn next_era_event_names_reach_the_beta_handlers() {
        assert_eq!(
            canonical_event("session.input.cancelled"),
            "session.inbox.cancelled"
        );
        assert_eq!(canonical_event("session.idle"), "session.idle");

        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.state.pending_input = Some(PendingInput {
            inbox_id: "msg_1".into(),
            message: "keep going".into(),
            steer: true,
        });
        harness.feed(json!({"type": "session.input.cancelled", "data": {"sessionID": "ses_1", "id": "msg_1"}}));
        assert!(matches!(
            harness.drain().as_slice(),
            [DriverEvent::SteerRejected { message, .. }] if message == "keep going"
        ));
        assert!(harness.state.pending_input.is_none());
    }

    #[test]
    fn a_deleted_session_reports_the_error_before_the_terminal_exit() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.deleted", "data": {"sessionID": "ses_1"}}));
        let seen = harness.drain();
        assert!(matches!(&seen[0], DriverEvent::Error(_)));
        assert!(matches!(&seen[1], DriverEvent::ProcessExited));
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn stored_models_split_into_a_reference_with_its_effort_variant() {
        assert_eq!(
            model_ref(Some("anthropic/claude-sonnet-4-5"), Some("high")),
            Some(ModelRef {
                id: "claude-sonnet-4-5".into(),
                provider_id: "anthropic".into(),
                variant: Some("high".into()),
            })
        );
        assert_eq!(model_ref(Some("bare-model"), None), None);
        assert_eq!(model_ref(None, Some("high")), None);
    }

    #[test]
    fn only_selectable_primary_agents_override_the_build_default() {
        let agents = vec![
            opencode2_api::AgentInfo {
                id: "plan".into(),
                name: "Plan".into(),
                description: None,
                mode: opencode2_api::AgentMode::Primary,
                hidden: false,
                model: None,
            },
            opencode2_api::AgentInfo {
                id: "title".into(),
                name: "Title".into(),
                description: None,
                mode: opencode2_api::AgentMode::Primary,
                hidden: true,
                model: None,
            },
        ];
        assert_eq!(resolve_agent(Some("plan"), &agents), "plan");
        assert_eq!(resolve_agent(Some("title"), &agents), "build");
        assert_eq!(resolve_agent(None, &agents), "build");
        // A catalogue Waku could not read must not veto the user's choice.
        assert_eq!(resolve_agent(Some("plan"), &[]), "plan");
    }

    #[test]
    fn only_slash_skills_join_the_command_palette() {
        let reported = reported_commands(
            vec![opencode2_api::CommandInfo {
                name: "review".into(),
                description: Some("Review the diff".into()),
            }],
            vec![
                opencode2_api::SkillInfo {
                    id: "opencode".into(),
                    name: "opencode".into(),
                    description: None,
                    slash: Some(false),
                    autoinvoke: None,
                    location: "/builtin/opencode.md".into(),
                },
                opencode2_api::SkillInfo {
                    id: "deploy".into(),
                    name: "deploy".into(),
                    description: Some("Ship it".into()),
                    slash: Some(true),
                    autoinvoke: None,
                    location: "/skills/deploy.md".into(),
                },
            ],
        );
        assert_eq!(
            reported
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["review", "deploy"]
        );
    }

    #[test]
    fn native_command_dispatch_matches_only_registered_slash_names() {
        let commands = ["init".into(), "review".into(), "team/review".into()]
            .into_iter()
            .collect();
        assert_eq!(
            native_command_invocation("/init", &commands),
            Some(("init", ""))
        );
        assert_eq!(
            native_command_invocation("/review main\ncheck tests", &commands),
            Some(("review", "main\ncheck tests"))
        );
        assert_eq!(
            native_command_invocation("/team/review main", &commands),
            Some(("team/review", "main"))
        );
        assert!(native_command_invocation("/reviewer", &commands).is_none());
        assert!(native_command_invocation("Discuss /review", &commands).is_none());
    }

    #[test]
    fn generated_titles_replace_the_providers_own_placeholder() {
        assert_eq!(
            generated_title(Some("New session - 2026-09-06T18:33:35.122Z")),
            None
        );
        assert_eq!(generated_title(Some("  ")), None);
        assert_eq!(
            generated_title(Some("Wire OpenCode 2")).as_deref(),
            Some("Wire OpenCode 2")
        );
    }

    /// Drives the user's own adopted service through the real driver. Ignored
    /// by default: it needs the OpenCode 2 CLI installed and its background
    /// service healthy. Run with
    /// `cargo test -p waku-core opencode2_session_against_the_adopted_service -- --ignored`.
    #[test]
    #[ignore = "requires a healthy opencode2 background service"]
    fn opencode2_session_against_the_adopted_service() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode2 is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCode2Driver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the adopted service should open a session");

        let connected = event_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the driver should report its cursor");
        let DriverEvent::Connected {
            provider_cursor:
                Some(ProviderResumeCursor::OpenCode2 {
                    session_id,
                    directory,
                }),
        } = connected
        else {
            panic!("expected an OpenCode 2 cursor, got {connected:?}");
        };
        assert!(session_id.starts_with("ses_"));
        assert!(directory.is_some_and(|directory| !directory.ends_with('/')));
        drop(driver);
    }
}

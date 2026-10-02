//! OpenCode sessions over the one adopted background service.
//!
//! Everything structural about this driver follows from a single fact: Waku
//! does not own an OpenCode process. `opencode_service` finds the daemon the
//! user's own terminal already started, and every Waku task rides the one
//! `GET /api/event` stream it exposes. Dropping a driver unsubscribes and
//! sends `Shutdown`; the worker releases its optional Computer Use attachment
//! without terminating or reconfiguring the user's OpenCode service.
//!
//! Commands and events land in ONE worker thread driven by
//! `crossbeam::select!`, so all mutable stream state is thread-local, and
//! context windows come from the service-level catalogue cache rather than a
//! per-session poller.
//!
//! Three traps are encoded here rather than left to a reader to remember:
//!
//! * **The execution outcome is the settle point.** `session.execution.`
//!   `{succeeded,failed,interrupted}` ends the turn and emits exactly one
//!   `TurnFinished`; settling is idempotent, so a steered second message —
//!   which joins the running execution rather than starting its own — still
//!   settles once. Do NOT wait for `session.idle`: the service never publishes
//!   it (the schema keeps it only as a deprecated entry, and a live turn's
//!   stream ends at `session.execution.*`), and waiting for it pins every
//!   finished turn to Working forever. A prompt whose HTTP call fails settles
//!   the turn itself, because no execution ever starts.
//! * **A stream break is never `ProcessExited`.** For an adopted service that
//!   would kill a perfectly healthy session every time the socket blinked.
//!   Reconnection is the service's job and arrives here as `HubFrame::Resync`.
//!   Nor is a shutdown the end of a turn: the service keeps the run's claim
//!   and resumes it as it boots again, with no `idle` marker in between, so
//!   the turn stays open across the restart (see `reconcile`).
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
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use serde_json::{Value, json};

use super::activity;
use super::opencode_computer_use::{INSTRUCTION_KEY, OpenCodeComputerUse};
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
use crate::opencode_api::{
    self, ApiError, AssistantContent, Delivery, FormAnswer, FormField, FormInfo, FormValue,
    MessageInfo, MigrationStatus, ModelRef, Order, PermissionReply, SessionOutcome, TokenUsage,
    ToolContent, ToolState,
};
use crate::opencode_service::{self, HubFrame, OpenCodeService, Subscription};

/// A one-shot user action posted onto the worker waits this long before Waku
/// gives up on it. Comfortably past the API layer's own fork budget, so a slow
/// fork answers rather than being reported as a timeout twice.
const ACTION_TIMEOUT: Duration = Duration::from_secs(150);
/// An option change is a live UI interaction: a `false` answer restarts the
/// driver, which is always correct, so waiting long for a `true` is pointless.
const OPTIONS_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a resume waits for OpenCode's one-time import of OpenCode 1
/// history to reach the session it names. The import runs in the background
/// after the first service start, and a session it has not reached yet reads
/// as missing — which would otherwise start an empty session under the same
/// id and cut the task off from its history.
const MIGRATION_WAIT: Duration = Duration::from_secs(60);
const MIGRATION_POLL: Duration = Duration::from_millis(500);

/// How long after a reconnect a run the service's shutdown suspended may take
/// to resume. The boot that brings the service back resumes it within moments
/// (it was already active when `/api/info` first answered, live), so this
/// only bounds a run that is never coming back.
const RESUME_GRACE: Duration = Duration::from_secs(60);

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

impl ContextWindows for Arc<OpenCodeService> {
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
/// failure, never on `session.tool.called`, because OpenCode can still emit
/// `session.tool.progress` afterwards.
#[derive(Clone, Debug)]
struct ToolSlot {
    kind: ActivityKind,
    tool_name: Option<String>,
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
    /// Whether a cancelled entry goes back to the app's follow-up queue: only
    /// a steer does, because a prompt is already in the transcript.
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
    /// each field's own `key` rather than rely on positional answers.
    forms: HashMap<String, Vec<FormField>>,
    pending_input: Option<PendingInput>,
    /// The connection this state belongs to; a reconciliation answer from a
    /// superseded pass must not overwrite newer state.
    generation: u64,
    /// The session-level model, moved by `session.model.selected`.
    model: Option<ModelRef>,
    /// Unknown event types are dropped and counted, never fatal.
    unknown: HashMap<String, u64>,
    /// The user stopped the turn while the service was unreachable, so the
    /// interrupt never landed. A shutdown keeps the run's claim and the next
    /// boot resumes it, and that resumed run is stopped rather than shown as
    /// a turn of its own.
    stop_pending: bool,
    /// The running turn shows the "waiting for the service" row.
    reconnecting: bool,
    /// When a run the service should have resumed by now is given up on.
    resume_deadline: Option<Instant>,
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
            stop_pending: false,
            reconnecting: false,
            resume_deadline: None,
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
        self.drop_turn_scratch();
        self.settle(events, None);
    }

    /// Drops the open turn without a `TurnFinished`, because the app already
    /// ended it: the next prompt must open a fresh turn.
    fn abandon_turn(&mut self) {
        self.drop_turn_scratch();
        self.turn = TurnState::Idle;
    }

    fn drop_turn_scratch(&mut self) {
        self.permissions.pending.clear();
        self.permissions.responding.clear();
        self.parts.clear();
        self.step = None;
        self.reconnecting = false;
        self.resume_deadline = None;
    }

    /// Says why a running turn went quiet: the service is gone, possibly
    /// restarting, and OpenCode resumes the run once it is back.
    fn begin_reconnecting(&mut self, events: &impl DriverEventSink) {
        if self.reconnecting || !matches!(self.turn, TurnState::Running { .. }) {
            return;
        }
        self.reconnecting = true;
        self.reconnect_row(events, false);
    }

    fn end_reconnecting(&mut self, events: &impl DriverEventSink) {
        if !std::mem::take(&mut self.reconnecting) {
            return;
        }
        self.reconnect_row(events, true);
    }

    fn reconnect_row(&self, events: &impl DriverEventSink, reconnected: bool) {
        let title = if reconnected {
            tr!(
                "activity.provider_service_reconnected",
                provider = "OpenCode"
            )
        } else {
            tr!("activity.provider_service_waiting", provider = "OpenCode")
        };
        let item = activity::tool_activity(
            // One row, upserted: every failed reconnect attempt reports again.
            Some(format!("reconnect:{}", self.session_id)),
            ActivityKind::Tool,
            title,
            None,
            None,
            None,
            false,
            reconnected,
        );
        let _ = events.send(DriverEvent::RichActivity(item));
    }
}

/// Everything the worker needs to talk back to the service and the app.
struct Worker {
    service: Arc<OpenCodeService>,
    session_id: String,
    command_names: HashSet<String>,
    events: DriverEventSender,
    commands: Sender<DriverCommand>,
    computer_use: Option<Arc<OpenCodeComputerUse>>,
}

pub(super) struct OpenCodeDriver {
    /// The service object is permanent and this is never a lease over the
    /// user's process: nothing about dropping a driver can reach it.
    #[allow(dead_code)]
    service: Arc<OpenCodeService>,
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
    computer_use: Option<Arc<OpenCodeComputerUse>>,
}

impl OpenCodeDriver {
    /// Runs on the daemon request thread. Blocking is allowed here, but every
    /// wait is bounded: discovery and the readiness probe live in
    /// `opencode_service`, a resume waits at most [`MIGRATION_WAIT`] for the
    /// OpenCode 1 import, and everything else is one local HTTP call.
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
            computer_use_enabled,
            provider_cursor,
        } = options;

        let resumed = match provider_cursor {
            Some(ProviderResumeCursor::OpenCode { session_id, .. }) => {
                (!session_id.is_empty()).then_some(session_id)
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume OpenCode from a {} cursor",
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
                    "OpenCode needs a resolvable workspace directory, but {} could not be canonicalized",
                    cwd.display()
                )
            })?
            .to_string_lossy()
            .into_owned();

        let service = opencode_service::shared(&binary)?;
        let endpoint = service.endpoint();
        let resuming = resumed.is_some();
        let session_id = resumed.unwrap_or_else(opencode_api::new_session_id);

        // SUBSCRIBE BEFORE CREATE. The service can emit this session's first
        // event before `POST /api/session` has answered; the create body takes
        // a client-minted id precisely so that race cannot exist.
        let subscription = service.subscribe(&session_id);
        let frames = subscription.rx.clone();

        let agents = opencode_api::list_agents(&endpoint, Some(&directory)).unwrap_or_default();
        let requested_agent = agent_preset.filter(|preset| !preset.is_empty());
        let agent = resolve_agent(requested_agent.as_deref(), &agents);
        let model = model_ref(model.as_deref(), reasoning_effort.as_deref());

        let create = || {
            opencode_api::create_session(
                &endpoint,
                &session_id,
                Some(&agent),
                model.as_ref(),
                &directory,
            )
            .map_err(|error| anyhow!("could not open an OpenCode session: {error}"))
        };
        let session = match resuming {
            // A cursor can outlive the session it names — the user's own
            // client can delete it — so a resume that finds nothing starts
            // fresh under the same id rather than failing the task.
            true => match resumed_session(&endpoint, &session_id) {
                Ok(Some(session)) => session,
                Ok(None) => create()?,
                Err(error) => {
                    return Err(anyhow!("could not read the OpenCode session: {error}"));
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
                    let _ = opencode_api::switch_agent(&endpoint, &session_id, &agent);
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
            let _ = opencode_api::switch_model(&endpoint, &session_id, model);
        }

        let computer_use = if computer_use_enabled {
            let attached =
                OpenCodeComputerUse::start(&service, &directory, &session_id, events.clone());
            match attached {
                Ok(runtime) => Some(Arc::new(runtime)),
                Err(error) => {
                    if !resuming {
                        let _ = opencode_api::delete_session(&endpoint, &session_id);
                    }
                    return Err(error);
                }
            }
        } else {
            // A resumed task may retain our instructions after an interrupted
            // host shutdown. Remove only Waku's own entry when disabled.
            let _ = opencode_api::remove_instruction_entry(&endpoint, &session_id, INSTRUCTION_KEY);
            None
        };

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
            provider_cursor: Some(ProviderResumeCursor::OpenCode {
                session_id: session_id.clone(),
                directory: Some(directory.clone()),
            }),
        });
        let _ = events.send(DriverEvent::AgentPresetSelected(Some(agent)));
        if let Some(title) = generated_title(session.title.as_deref()) {
            let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title)));
        }
        let native_commands =
            opencode_api::list_commands(&endpoint, Some(&directory)).unwrap_or_default();
        let command_names = native_commands
            .iter()
            .map(|command| command.name.clone())
            .collect();
        let reported = reported_commands(native_commands);
        if !reported.is_empty() {
            let _ = events.send(DriverEvent::AvailableCommands(reported));
        }

        let (commands, command_rx) = unbounded();
        let worker = Worker {
            service: Arc::clone(&service),
            session_id: session_id.clone(),
            command_names,
            events,
            commands: commands.clone(),
            computer_use: computer_use.clone(),
        };
        let generation = service.generation();
        let mut state = StreamState::new(session_id.clone(), mode, session_model, generation);
        thread::Builder::new()
            .name(format!("waku-opencode-{session_id}"))
            .spawn(move || {
                // The snapshot runs in the same sequential position as the
                // event loop, before a single frame is decoded. Deferring it
                // would let a concurrently decoded `session.idle` take the
                // turn flag before a pending native permission — which proves
                // the resumed turn is live — could restore it.
                reconcile(&worker, &mut state, generation);
                loop {
                    let resume_deadline = state
                        .resume_deadline
                        .map_or_else(crossbeam_channel::never, crossbeam_channel::at);
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
                                HubFrame::Disconnected => {
                                    connection_lost(&mut state, &worker.events);
                                }
                                HubFrame::Resync { generation } => {
                                    if let Some(computer_use) = worker.computer_use.as_ref()
                                        && let Err(error) = computer_use.ensure_connected()
                                    {
                                        let _ = worker.events.send(DriverEvent::Error(error.to_string()));
                                    }
                                    reconcile(&worker, &mut state, generation);
                                }
                            }
                        }
                        recv(resume_deadline) -> _ => {
                            resume_overdue(&mut state, &worker.events);
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
            computer_use,
        })
    }
}

impl DriverControl for OpenCodeDriver {
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
        self.cancel_computer_use();
        let _ = self.commands.send(DriverCommand::Cancel);
    }

    fn cancel_computer_use(&self) {
        if let Some(computer_use) = self.computer_use.as_ref() {
            computer_use.stop();
        }
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
        // The access mode is not installed on the session — it decides who
        // answers a permission request, which is the worker's own state — so
        // a mode change is absorbed in place rather than restarting the
        // driver.
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
            .map_err(|_| anyhow!("the OpenCode driver is shutting down"))?;
        answer
            .recv_timeout(ACTION_TIMEOUT)
            .map_err(|_| anyhow!("OpenCode did not answer the fork request"))?
    }
}

impl Drop for OpenCodeDriver {
    fn drop(&mut self) {
        // No socket surgery, no process signal, no exit budget: the clean
        // consequence of owning no server. Unsubscribe first so the hub stops
        // fanning out, then wake the worker so it winds down.
        drop(self.subscription.take());
        // The worker retains the final lease and cleans up the MCP runtime
        // when it handles Shutdown; the user's service is never terminated.
        drop(self.computer_use.take());
        let _ = self.commands.send(DriverCommand::Shutdown);
    }
}

fn model_key(model: &ModelRef) -> String {
    // Keyed on `providerID/id`. `Model.Ref` has no `modelID` — that field
    // exists only on the catalogue entry.
    format!("{}/{}", model.provider_id, model.id)
}

/// Reads the session a resume cursor names, or `None` when it is gone.
///
/// OpenCode imports OpenCode 1 history in the background after its first
/// start, and a session the import has not reached yet reads as missing. A
/// missing session is therefore retried while the import runs, up to
/// [`MIGRATION_WAIT`], rather than replaced by an empty one under its id.
fn resumed_session(
    endpoint: &Endpoint,
    session_id: &str,
) -> Result<Option<opencode_api::SessionInfo>, ApiError> {
    let deadline = Instant::now() + MIGRATION_WAIT;
    loop {
        // Read the import's state first, so an import that finishes in
        // between has already landed by the time the session read below
        // reports the session missing.
        let importing = matches!(
            opencode_api::v1_migration_status(endpoint),
            Ok(MigrationStatus::Running)
        );
        match opencode_api::get_session(endpoint, session_id) {
            Ok(session) => return Ok(Some(session)),
            Err(error) if !error.is_not_found() => return Err(error),
            Err(_) if !importing || Instant::now() >= deadline => return Ok(None),
            Err(_) => thread::sleep(MIGRATION_POLL),
        }
    }
}

/// Waku stores a model as `"provider/model"`; OpenCode wants the two apart,
/// plus the variant that carries reasoning effort (`low`/`high`).
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
/// Waku's access modes do not name an agent — OpenCode has no read-only product
/// mode in this tree — so the choice is the user's own preset when the service
/// still lists it as a selectable primary, and `build` otherwise.
fn resolve_agent(preset: Option<&str>, agents: &[opencode_api::AgentInfo]) -> String {
    let selectable = |name: &str| {
        agents.iter().any(|agent| {
            agent.id == name && !agent.hidden && agent.mode != opencode_api::AgentMode::Subagent
        })
    };
    match preset {
        Some(preset) if !preset.is_empty() && (agents.is_empty() || selectable(preset)) => {
            preset.to_owned()
        }
        _ => "build".to_owned(),
    }
}

/// Context-window occupancy. OpenCode reports reasoning tokens separately
/// instead of folding them into output, so leaving them out under-reports
/// every thinking model.
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
        opencode_api::list_messages(endpoint, session_id, Order::Desc, Some(20), None)
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

/// OpenCode emits `New session - <timestamp>` before its title-generation model
/// call, and that placeholder is not a title.
fn generated_title(title: Option<&str>) -> Option<String> {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty() && !title.starts_with("New session - "))
        .map(str::to_owned)
}

/// The commands a user can type. Skills are not among them: OpenCode attaches
/// a skill to a prompt as an `@` mention rather than running it as a command.
fn reported_commands(commands: Vec<opencode_api::CommandInfo>) -> Vec<ReportedCommand> {
    let mut seen = HashSet::new();
    commands
        .into_iter()
        .map(|command| ReportedCommand {
            name: command.name,
            description: command.description.unwrap_or_default(),
        })
        .filter(|command| !command.name.is_empty() && seen.insert(command.name.clone()))
        .collect()
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
/// What reaches Waku is whatever the resolved agent's own rules mark `ask`;
/// the mode only decides who answers. Nothing is ever saved to
/// `/api/permission/saved`, a GLOBAL store shared with the user's own
/// terminal.
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
) -> Result<Option<opencode_api::InboxUser>, ApiError> {
    if let Some(computer_use) = worker.computer_use.as_ref() {
        computer_use.ensure_connected().map_err(ApiError::from)?;
    }
    if let Some((name, arguments)) = native_command_invocation(text, &worker.command_names) {
        opencode_api::command(endpoint, &worker.session_id, name, arguments, delivery).map(|_| None)
    } else {
        opencode_api::prompt(endpoint, &worker.session_id, text, delivery).map(Some)
    }
}

fn handle_command(worker: &Worker, message: DriverCommand, state: &mut StreamState) -> bool {
    let endpoint = worker.service.endpoint();
    let events = &worker.events;
    match message {
        DriverCommand::Prompt(text) => {
            // The user moved on; a run they stopped earlier is theirs again.
            state.stop_pending = false;
            state.begin_turn(events);
            match submit_prompt(worker, &endpoint, &text, None) {
                Ok(Some(inbox)) => {
                    // Not a steer, whatever delivery the server reports: it
                    // picks `steer` for a prompt into an idle session, and a
                    // prompt must never fall back into the app's follow-up
                    // queue — the transcript already shows it.
                    state.pending_input = Some(PendingInput {
                        inbox_id: inbox.id,
                        message: text,
                        steer: false,
                    });
                }
                Ok(None) => state.pending_input = None,
                Err(error) => {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.provider_rejected_prompt_detail",
                        provider = "OpenCode",
                        error = error
                    )));
                    // `session.idle` never arrives for a turn that never
                    // started, so settle it here instead of spinning forever.
                    state.settle(
                        events,
                        Some(TurnOutcome {
                            success: false,
                            summary: Some(tr!("errors.provider_start_turn", provider = "OpenCode")),
                        }),
                    );
                }
            }
        }
        DriverCommand::Steer(text) => {
            if matches!(state.turn, TurnState::Idle) {
                let _ = events.send(DriverEvent::SteerRejected {
                    message: text,
                    reason: tr!("errors.provider_no_active_turn", provider = "OpenCode"),
                });
                return true;
            }
            match submit_prompt(worker, &endpoint, &text, Some(Delivery::Steer)) {
                Ok(Some(inbox)) => {
                    // A server that queued the message anyway is promoted, so
                    // "steer" means the same thing on both paths.
                    if inbox.delivery != Delivery::Steer {
                        let _ = opencode_api::set_inbox_delivery(
                            &endpoint,
                            &worker.session_id,
                            &inbox.id,
                            Delivery::Steer,
                        );
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
                            provider = "OpenCode",
                            error = error
                        ),
                    });
                }
            }
        }
        DriverCommand::Cancel => {
            let interrupted = opencode_api::interrupt(&endpoint, &worker.session_id);
            stop_turn(state, interrupted, events);
        }
        DriverCommand::Respond {
            request_id,
            option_id,
        } => {
            for (request_id, option_id) in
                support::permission_responses(&mut state.permissions, &request_id, &option_id)
            {
                let reply = match option_id.as_str() {
                    "reject" => PermissionReply::Reject,
                    // Never `always`: see the module doc.
                    _ => PermissionReply::Once,
                };
                if let Err(error) = opencode_api::reply_permission(
                    &endpoint,
                    &worker.session_id,
                    &request_id,
                    reply,
                ) {
                    let _ = events.send(DriverEvent::Error(tr!(
                        "errors.answer_provider_permission",
                        provider = "OpenCode",
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
                opencode_api::reply_form(&endpoint, &worker.session_id, &request_id, &answer)
            {
                let _ = events.send(DriverEvent::Error(tr!(
                    "errors.answer_provider_question",
                    provider = "OpenCode",
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
        && opencode_api::switch_model(endpoint, &worker.session_id, model).is_err()
    {
        return false;
    }
    if model.is_some() {
        state.model = model;
    }
    state.mode = options.mode;
    true
}

/// Branches the live session by dropping its last `turns_to_remove` turns.
///
/// Shared with the cold path, because a steer is a user message of its own:
/// counting user messages as turns would fork inside a steered turn.
fn fork_session(
    endpoint: &Endpoint,
    worker: &Worker,
    turns_to_remove: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    crate::opencode_session::fork_session_removing_turns(
        endpoint,
        &worker.session_id,
        turns_to_remove,
    )
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

    let session = match opencode_api::get_session(&endpoint, &worker.session_id) {
        Ok(session) => Some(session),
        Err(error) if error.is_not_found() => {
            // A foreign client deleting this session is authoritative.
            let _ = events.send(DriverEvent::Error(tr!(
                "errors.provider_reported_error",
                provider = "OpenCode"
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
    if let Ok(pending) = opencode_api::list_permissions(&endpoint, &worker.session_id) {
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
    if let Ok(forms) = opencode_api::list_forms(&endpoint, &worker.session_id) {
        blocked = blocked || !forms.is_empty();
        for form in forms {
            surface_form(form, state, events);
        }
    }
    if let Ok(inbox) = opencode_api::list_inbox(&endpoint, &worker.session_id) {
        let undelivered = state.pending_input.as_ref().is_some_and(|pending| {
            inbox
                .iter()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some(&pending.inbox_id))
        });
        if !undelivered {
            state.pending_input = None;
        }
    }

    let draining = opencode_api::active_sessions(&endpoint)
        .map(|active| active.contains(&worker.session_id))
        .unwrap_or(false);
    // Before any settle: the app drops output for a turn it has ended.
    gap_fill(&endpoint, &worker.session_id, events, state);
    state.end_reconnecting(events);
    if state.stop_pending {
        if draining {
            // Resumed by the boot that brought the service back. Success
            // lands as the run's own interrupted outcome, which clears this.
            let _ = opencode_api::interrupt(&endpoint, &worker.session_id);
        } else if latest_run_settled(&endpoint, &worker.session_id) {
            state.stop_pending = false;
        }
    } else if draining || blocked {
        state.begin_turn(events);
    } else if matches!(state.turn, TurnState::Running { .. })
        && !latest_run_settled(&endpoint, &worker.session_id)
    {
        // Suspended by a shutdown. The boot that brought the service back
        // resumes the run, which can trail this reconnect by a moment.
        state.resume_deadline = Some(Instant::now() + RESUME_GRACE);
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
}

/// Brings the driver in line with a stop the app has already applied.
///
/// A service that is down, or has no active run because its shutdown
/// suspended it, cannot interrupt that run now. It is stopped when it next
/// starts instead, which honours the stop, so there is nothing to report.
/// Whenever the run did not stop here, the open turn is dropped, so the next
/// prompt opens a turn of its own instead of vanishing into this one.
fn stop_turn(
    state: &mut StreamState,
    interrupted: opencode_api::Result<bool>,
    events: &impl DriverEventSink,
) {
    let report = |error: &ApiError| {
        let _ = events.send(DriverEvent::Error(tr!(
            "errors.stop_provider",
            provider = "OpenCode",
            error = error
        )));
    };
    if !matches!(state.turn, TurnState::Running { .. }) {
        if let Err(error) = &interrupted {
            report(error);
        }
        return;
    }
    let deferred = match &interrupted {
        // Its own `interrupted` outcome settles the turn.
        Ok(true) => return,
        Ok(false) => true,
        Err(error) => error.is_unavailable(),
    };
    if let Err(error) = &interrupted
        && !deferred
    {
        report(error);
    }
    state.stop_pending = deferred;
    state.abandon_turn();
}

/// The event stream broke. Nothing is lost yet, but a running turn says why
/// it went quiet, and no resume is due while the service is gone.
fn connection_lost(state: &mut StreamState, events: &impl DriverEventSink) {
    state.resume_deadline = None;
    state.begin_reconnecting(events);
}

/// The service came back but never resumed the run its shutdown suspended,
/// so that run is not coming: the turn ends unfinished.
fn resume_overdue(state: &mut StreamState, events: &impl DriverEventSink) {
    state.drop_turn_scratch();
    state.settle(
        events,
        Some(TurnOutcome {
            success: false,
            summary: None,
        }),
    );
}

/// Whether the session's latest run has ended, read from its transcript.
///
/// The service exposes no "suspended" state: a run its shutdown interrupted
/// is neither active nor ended until the next boot resumes it. What tells the
/// two apart is OpenCode's own turn delimiter, the `idle` marker every ending
/// writes. An unreadable transcript counts as ended, which settles the turn.
fn latest_run_settled(endpoint: &Endpoint, session_id: &str) -> bool {
    opencode_api::list_messages(endpoint, session_id, Order::Desc, Some(20), None)
        .map_or(true, |(messages, _)| run_settled(&messages))
}

/// Whether an `idle` marker follows the newest prompt and step, newest first.
/// Only those two count: a model switch, an instructions update or a restart
/// notice can land after the marker without starting a run.
fn run_settled(newest_first: &[MessageInfo]) -> bool {
    newest_first
        .iter()
        .find_map(|message| match message {
            MessageInfo::Idle { .. } => Some(true),
            MessageInfo::User { .. } | MessageInfo::Assistant { .. } => Some(false),
            _ => None,
        })
        .unwrap_or(true)
}

/// Appends whatever the in-flight assistant message gained while the stream
/// was down, keyed by `(assistantMessageID, ordinal)`.
fn gap_fill(
    endpoint: &Endpoint,
    session_id: &str,
    events: &impl DriverEventSink,
    state: &mut StreamState,
) {
    let Some(step) = state.step.clone() else {
        return;
    };
    // The newest STEP, not the newest message: a turn that settled while the
    // stream was down ends with its `idle` marker.
    let Ok((messages, _)) =
        opencode_api::list_messages(endpoint, session_id, Order::Desc, Some(4), None)
    else {
        return;
    };
    let Some((id, content)) = messages.into_iter().find_map(|message| match message {
        MessageInfo::Assistant { id, content, .. } => Some((id, content)),
        _ => None,
    }) else {
        return;
    };
    if id != step.message_id {
        return;
    }
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
        tool_name: Some(name.to_owned()),
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
    let kind = envelope
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let data = envelope.get("data").unwrap_or(&Value::Null);

    match kind {
        // Turn lifecycle. The execution outcome IS the terminal event: the
        // service never publishes `session.idle` (the stream ends at
        // `session.execution.*` and nothing follows), so waiting for one left
        // every turn pinned to Working forever. `session.idle` is still
        // handled below in case a build sends it; settling is idempotent.
        "session.execution.started" => {
            if state.stop_pending {
                // The run the user stopped while the service was down, which
                // its next boot resumed: stop it again instead of showing it.
                let _ = commands.send(DriverCommand::Cancel);
                return;
            }
            state.resume_deadline = None;
            state.end_reconnecting(events);
            state.begin_turn(events);
        }
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
            state.stop_pending = false;
            state.arm(true, None);
            state.finish_turn(events);
        }
        "session.execution.failed" => {
            let message = error_message(data.get("error"));
            // A run the user already stopped has nothing left to report.
            if !std::mem::take(&mut state.stop_pending) {
                let _ = events.send(DriverEvent::Error(message.clone()));
            }
            state.arm(false, Some(message));
            state.finish_turn(events);
        }
        "session.execution.interrupted" => {
            let reason = data
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_owned();
            // `shutdown` does not end the turn: the service keeps the run's
            // claim and resumes it as it boots again, writing no `idle`
            // marker in between, so OpenCode counts one turn and so must
            // Waku. The stream breaks next, and `reconcile` picks the run up
            // after the reconnect. A user, superseded or inactivity interrupt
            // did stop the work, and says so.
            if reason == "shutdown" {
                state.begin_reconnecting(events);
                return;
            }
            state.stop_pending = false;
            state.arm(false, Some(reason));
            state.finish_turn(events);
        }
        // Retained for compatibility only; the service never sends this.
        // Harmless after the outcome already settled the turn.
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
                tool_name: data.get("name").and_then(Value::as_str).map(str::to_owned),
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
            // This is what makes a long bash or grep visibly alive instead of
            // a frozen row.
            emit_tool(events, &id, &slot, metadata.as_ref(), false, false);
        }
        "session.tool.success" => complete_tool(data, state, events, false),
        "session.tool.failed" => complete_tool(data, state, events, true),

        // Inbox and steering.
        // Every inbox event names its entry `inboxID`; only the prompt answer
        // calls it `id`.
        "session.inbox.enqueued" => {
            let Some(id) = data.get("inboxID").and_then(Value::as_str) else {
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
                Some(pending.inbox_id.as_str()) == data.get("inboxID").and_then(Value::as_str)
            }) {
                state.pending_input = None;
            }
        }
        // Another client cancelled the entry, or a move dropped it.
        "session.inbox.cancelled" => {
            let Some(pending) = state.pending_input.as_ref() else {
                return;
            };
            if Some(pending.inbox_id.as_str()) != data.get("inboxID").and_then(Value::as_str) {
                return;
            }
            // The app falls back to its own follow-up queue.
            if pending.steer {
                let _ = events.send(DriverEvent::SteerRejected {
                    message: pending.message.clone(),
                    reason: tr!("errors.provider_rejected_prompt", provider = "OpenCode"),
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
                provider = "OpenCode"
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
    const PREFIXES: [&str; 13] = [
        "tui.",
        "pty.",
        "mcp.",
        "installation.",
        "vcs.",
        "plugin.",
        "integration.",
        "credential.",
        "reference.",
        "websearch.",
        "shell.",
        "project.",
        "lsp.",
    ];
    const EXACT: [&str; 14] = [
        "server.connected",
        "filesystem.changed",
        "location.shutdown",
        "model.updated",
        "provider.updated",
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
    const UNINTERESTING: [&str; 7] = [
        "session.instructions.updated",
        "session.metadata.updated",
        "session.permissions",
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
        .unwrap_or_else(|| tr!("errors.provider_reported_error", provider = "OpenCode"))
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
    // Removed ONLY here: OpenCode can still emit `session.tool.progress`
    // after `called`, so removing on `called` would strand every later update.
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
    let mut item = activity::tool_activity(
        Some(id.to_owned()),
        slot.kind,
        slot.title.clone(),
        slot.input.as_ref(),
        output,
        slot.metadata.as_ref(),
        failed,
        complete,
    )
    .with_tool_name(slot.tool_name.as_deref());
    if let Some((server, tool)) = slot
        .tool_name
        .as_deref()
        .and_then(super::opencode_computer_use::tool_identity)
    {
        item = item
            .with_tool_name(Some(tool))
            .with_mcp_server(Some(server));
    }
    let _ = events.send(DriverEvent::RichActivity(item));
}

/// `session.status` nests its state under `status` (`{type: "retry",
/// message, action?}`), while `session.retry.scheduled` carries the failure
/// that caused the retry as `error`.
fn retry_activity(kind: &str, data: &Value, state: &StreamState, events: &impl DriverEventSink) {
    let retry = if kind == "session.status" {
        match data.get("status") {
            Some(status) if status.get("type").and_then(Value::as_str) == Some("retry") => status,
            _ => return,
        }
    } else {
        data
    };
    let title = retry
        .pointer("/action/title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tr!("activity.provider_retrying"));
    // The reason travels as TEXT, never as colour alone.
    let detail = retry
        .pointer("/action/message")
        .or_else(|| retry.get("message"))
        .or_else(|| retry.pointer("/error/message"))
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
/// the answer round-trips instead of relying on positional order.
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

fn field_options(field: &FormField) -> &[opencode_api::FormOption] {
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
    use uuid::Uuid;

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

    /// Answers each request with the next canned response, in order, and
    /// reports how many it served. A request that never comes ends the server
    /// after a few seconds rather than hanging the test.
    fn canned_service(responses: Vec<(u16, Value)>) -> (Endpoint, thread::JoinHandle<usize>) {
        use std::io::{BufRead as _, Read as _, Write as _};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = Endpoint::local(listener.local_addr().unwrap().port());
        let server = thread::spawn(move || {
            let mut served = 0;
            for (status, body) in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(_) if std::time::Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => return served,
                    }
                };
                socket.set_nonblocking(false).unwrap();
                let mut input = std::io::BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    input.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                input.read_exact(&mut vec![0; length]).unwrap();
                let body = body.to_string();
                write!(
                    socket,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                served += 1;
            }
            served
        });
        (endpoint, server)
    }

    fn session_info(id: &str) -> Value {
        json!({"data": {
            "id": id,
            "projectID": "prj",
            "cost": 0,
            "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
            "time": {"created": 1.0, "updated": 1.0},
            "location": {"directory": "/work"},
        }})
    }

    /// A session the OpenCode 1 import has not reached yet reads as missing.
    /// The resume waits for it instead of starting an empty session under
    /// the same id.
    #[test]
    fn a_resume_waits_for_the_opencode_one_import_to_reach_its_session() {
        let missing = json!({"_tag": "SessionNotFoundError", "message": "Session not found"});
        let (endpoint, server) = canned_service(vec![
            (
                200,
                json!({"status": "running", "progress": {"label": "Migrating sessions"}}),
            ),
            (404, missing),
            (
                200,
                json!({"status": "running", "progress": {"label": "Migrating sessions"}}),
            ),
            (200, session_info("ses_v1")),
        ]);
        let session = resumed_session(&endpoint, "ses_v1").unwrap();
        assert_eq!(session.map(|session| session.id).as_deref(), Some("ses_v1"));
        assert_eq!(server.join().unwrap(), 4);
    }

    /// Once nothing is importing, a missing session is gone for good.
    #[test]
    fn a_resume_without_an_import_gives_up_on_a_missing_session_at_once() {
        let missing = json!({"_tag": "SessionNotFoundError", "message": "Session not found"});
        let (endpoint, server) =
            canned_service(vec![(200, json!({"status": "completed"})), (404, missing)]);
        assert!(resumed_session(&endpoint, "ses_gone").unwrap().is_none());
        assert_eq!(server.join().unwrap(), 2);
    }

    /// A turn that settles while the stream is down ends with its `idle`
    /// marker, and the final step's missing tail must still be repaired.
    #[test]
    fn a_reconnect_repairs_the_final_step_behind_its_idle_marker() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        for event in [
            json!({"type": "session.execution.started", "data": {"sessionID": "ses_1"}}),
            step_started("msg_1"),
            json!({"type": "session.text.started", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0}}),
            json!({"type": "session.text.delta", "data": {"sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "Hello"}}),
        ] {
            harness.feed(event);
        }
        harness.drain();
        let (endpoint, server) = canned_service(vec![(
            200,
            json!({"data": [
                {"id": "msg_idle", "type": "idle", "time": {"created": 3.0}, "outcome": "succeeded"},
                {
                    "id": "msg_1", "type": "assistant",
                    "time": {"created": 1.0, "streamed": 2.0, "completed": 2.0},
                    "agent": "build",
                    "model": {"id": "claude-sonnet-4-5", "providerID": "anthropic"},
                    "content": [{"type": "text", "text": "Hello, world."}],
                    "finish": "stop",
                },
            ]}),
        )]);

        gap_fill(&endpoint, "ses_1", &harness.events, &mut harness.state);

        assert_eq!(server.join().unwrap(), 1);
        let seen = harness.drain();
        assert!(
            matches!(seen.as_slice(), [DriverEvent::TextDelta(text)] if text == ", world."),
            "{seen:?}"
        );
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

    /// The regression that shipped: the service emits NO `session.idle`, so
    /// a driver that settles only there leaves every finished turn stuck on
    /// "Working" forever. Captured live, the terminal frame is
    /// `session.execution.*` and nothing follows it.
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

    /// OpenCode can still emit `session.tool.progress` after
    /// `session.tool.called`, so the slot is removed only on success or
    /// failure. Removing it on `called` would strand every later update.
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

    #[test]
    fn computer_use_tools_keep_mcp_identity_and_image_results() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        let server = "waku_js_repl_0123456789abcdef0123456789abcdef";
        harness.feed(step_started("msg_cua"));
        harness.feed(json!({"type":"session.tool.input.started","data":{
            "assistantMessageID":"msg_cua","id":"call_cua","name":format!("{server}_js")
        }}));
        harness.feed(json!({"type":"session.tool.called","data":{
            "assistantMessageID":"msg_cua","id":"call_cua","input":{"code":"await setupComputerUseRuntime({ globals: globalThis });","title":"Inspect desktop"}
        }}));
        harness.feed(json!({"type":"session.tool.success","data":{
            "assistantMessageID":"msg_cua","id":"call_cua","content":[{"type":"text","text":"ready"},{"type":"file","uri":"data:image/png;base64,aGVsbG8=","mime":"image/png"}]
        }}));
        let events = harness.drain();
        let item = events
            .iter()
            .filter_map(|event| match event {
                DriverEvent::RichActivity(item) if item.complete => Some(item),
                _ => None,
            })
            .last()
            .expect("completed computer-use tool");
        assert_eq!(item.tool_name.as_deref(), Some("js"));
        assert_eq!(item.mcp_server.as_deref(), Some(server));
        assert_eq!(item.title, "Inspect desktop");
        assert!(!item.failed);
        assert!(!item.image_urls.is_empty());
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

    fn reconnect_rows(seen: &[DriverEvent]) -> Vec<(String, bool)> {
        seen.iter()
            .filter_map(|event| match event {
                DriverEvent::RichActivity(item)
                    if item
                        .source_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with("reconnect:")) =>
                {
                    Some((item.title.clone(), item.complete))
                }
                _ => None,
            })
            .collect()
    }

    /// A service going down suspends the run rather than ending it: OpenCode
    /// resumes it as the service boots again, with no `idle` marker in
    /// between, so the resumed run belongs to the same turn.
    #[test]
    fn a_shutdown_suspends_the_turn_until_its_run_resumes() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness
            .feed(json!({"type": "session.execution.interrupted", "data": {"reason": "shutdown"}}));
        let suspended = harness.drain();
        assert!(matches!(suspended[0], DriverEvent::TurnStarted));
        assert!(
            !suspended
                .iter()
                .any(|event| matches!(event, DriverEvent::TurnFinished { .. })),
            "{suspended:?}"
        );
        assert_eq!(
            reconnect_rows(&suspended),
            [("Waiting for the OpenCode service".to_owned(), false)]
        );

        // Seen live when the stream is already back as the run resumes.
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness.feed(json!({"type": "session.execution.succeeded", "data": {}}));
        let resumed = harness.drain();
        assert_eq!(
            reconnect_rows(&resumed),
            [("Reconnected to the OpenCode service".to_owned(), true)]
        );
        assert!(
            !resumed
                .iter()
                .any(|event| matches!(event, DriverEvent::TurnStarted)),
            "the resumed run continues the turn: {resumed:?}"
        );
        assert!(matches!(
            resumed.last(),
            Some(DriverEvent::TurnFinished { success: true, .. })
        ));

        let mut cancelled = Harness::new(RuntimeMode::FullAccess);
        cancelled.feed(json!({"type": "session.execution.started", "data": {}}));
        cancelled
            .feed(json!({"type": "session.execution.interrupted", "data": {"reason": "user"}}));
        assert!(matches!(
            &cancelled.drain()[1],
            DriverEvent::TurnFinished { success: false, summary: Some(summary) } if summary == "user"
        ));
    }

    /// Every failed reconnect attempt reports the break again; the running
    /// turn shows one row for all of them, and an idle task shows none.
    #[test]
    fn a_lost_connection_shows_one_waiting_row_for_a_running_turn() {
        let mut idle = Harness::new(RuntimeMode::FullAccess);
        connection_lost(&mut idle.state, &idle.events);
        assert!(idle.drain().is_empty());

        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness.state.resume_deadline = Some(Instant::now());
        connection_lost(&mut harness.state, &harness.events);
        connection_lost(&mut harness.state, &harness.events);
        assert_eq!(
            reconnect_rows(&harness.drain()),
            [("Waiting for the OpenCode service".to_owned(), false)]
        );
        assert!(
            harness.state.resume_deadline.is_none(),
            "no resume is due while the service is gone"
        );
    }

    /// A run that never resumes ends its turn unfinished.
    #[test]
    fn an_overdue_resume_ends_the_turn_unfinished() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness.state.resume_deadline = Some(Instant::now());
        resume_overdue(&mut harness.state, &harness.events);
        assert!(matches!(
            harness.drain().last(),
            Some(DriverEvent::TurnFinished {
                success: false,
                summary: None
            })
        ));
        assert!(harness.state.resume_deadline.is_none());
        assert!(matches!(harness.state.turn, TurnState::Idle));
    }

    /// The user stopped the turn while the service was down, so the interrupt
    /// never landed and the next boot resumes the run anyway. It is stopped
    /// again rather than shown as a turn of its own, and quietly.
    #[test]
    fn a_run_stopped_during_an_outage_is_stopped_again_when_it_resumes() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        harness.drain();
        // What a failed `Cancel` leaves behind.
        harness.state.stop_pending = true;
        harness.state.abandon_turn();

        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        assert!(harness.drain().is_empty(), "no turn for the stopped run");
        assert!(matches!(
            harness.issued.try_recv(),
            Ok(DriverCommand::Cancel)
        ));
        harness.feed(json!({"type": "session.execution.failed", "data": {"error": {"message": "Execution was interrupted repeatedly"}}}));
        assert!(
            harness.drain().is_empty(),
            "the stopped run reports nothing"
        );
        assert!(!harness.state.stop_pending);

        // Anything after that is a run of its own again.
        harness.feed(json!({"type": "session.execution.started", "data": {}}));
        assert!(matches!(
            harness.drain().as_slice(),
            [DriverEvent::TurnStarted]
        ));
    }

    /// What a stop leaves behind depends on what the interrupt answered.
    #[test]
    fn a_stop_the_service_cannot_apply_now_waits_for_the_run_to_resume() {
        let running = || {
            let mut harness = Harness::new(RuntimeMode::FullAccess);
            harness.feed(json!({"type": "session.execution.started", "data": {}}));
            harness.drain();
            harness
        };

        // Interrupted: the run's own outcome settles the turn.
        let mut stopped = running();
        stop_turn(&mut stopped.state, Ok(true), &stopped.events);
        assert!(matches!(stopped.state.turn, TurnState::Running { .. }));
        assert!(!stopped.state.stop_pending);

        // Down, still stopping, or holding a run its shutdown suspended.
        for interrupted in [
            Err(ApiError::Transport(anyhow!("connection refused"))),
            Err(ApiError::Http {
                status: 503,
                body: r#"{"code":"service_stopping"}"#.to_owned(),
            }),
            Ok(false),
        ] {
            let mut deferred = running();
            stop_turn(&mut deferred.state, interrupted, &deferred.events);
            assert!(deferred.drain().is_empty(), "the stop is honoured later");
            assert!(deferred.state.stop_pending);
            assert!(matches!(deferred.state.turn, TurnState::Idle));
        }

        // A live refusal is reported, and the next prompt still opens a turn.
        let mut refused = running();
        stop_turn(
            &mut refused.state,
            Err(ApiError::Http {
                status: 500,
                body: String::new(),
            }),
            &refused.events,
        );
        assert!(matches!(
            refused.drain().as_slice(),
            [DriverEvent::Error(_)]
        ));
        assert!(!refused.state.stop_pending);
        assert!(matches!(refused.state.turn, TurnState::Idle));
    }

    /// Bookkeeping can land after a marker without starting a run; only a
    /// prompt or a step reopens it.
    #[test]
    fn a_run_is_settled_once_its_marker_follows_the_last_prompt_and_step() {
        let messages =
            |value: Value| -> Vec<MessageInfo> { serde_json::from_value(value).unwrap() };
        let idle = json!({"id": "msg_idle", "type": "idle", "time": {"created": 3.0}, "outcome": "succeeded"});
        let step = json!({
            "id": "msg_step", "type": "assistant", "time": {"created": 1.0},
            "agent": "build", "model": {"id": "big-pickle", "providerID": "opencode"},
            "content": [], "finish": "error",
        });
        let restart = json!({
            "id": "msg_restart", "type": "synthetic", "time": {"created": 2.0},
            "text": "The server restarted while you were working.",
            "metadata": {"notice": "restart"},
        });
        let switched = json!({
            "id": "msg_model", "type": "model-switched", "time": {"created": 4.0},
            "model": {"id": "m", "providerID": "p"},
        });
        let prompt =
            json!({"id": "msg_user", "type": "user", "time": {"created": 5.0}, "text": "go"});

        assert!(run_settled(&messages(json!([idle, step]))));
        assert!(run_settled(&messages(json!([switched, idle, step]))));
        assert!(!run_settled(&messages(json!([restart, step]))));
        assert!(!run_settled(&messages(json!([prompt, idle]))));
        assert!(run_settled(&[]));
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
            support::permission_responses(&mut harness.state.permissions, "per_abc", "always"),
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

    /// Each field's own `key` round-trips through the answer instead of
    /// relying on positional order.
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

    /// OpenCode reports reasoning tokens separately instead of folding them
    /// into output, so leaving them out under-reports every thinking model.
    /// The window is keyed on `providerID/id`, never `modelID`.
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
            // Introduced by the 2.0 release.
            json!({"type": "model.updated", "data": {}}),
            json!({"type": "provider.updated", "data": {}}),
            json!({"type": "location.shutdown", "data": {}}),
            json!({"type": "credential.updated", "data": {}}),
            json!({"type": "session.metadata.updated", "data": {"sessionID": "ses_1", "metadata": {}}}),
            json!({"type": "session.permissions", "data": {"sessionID": "ses_1", "permissions": []}}),
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

    /// A steer the service drops falls back to the app's own follow-up queue.
    #[test]
    fn a_cancelled_steer_is_rejected_back_to_the_app() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.state.pending_input = Some(PendingInput {
            inbox_id: "msg_1".into(),
            message: "keep going".into(),
            steer: true,
        });
        // The exact payload the service sends: the entry is `inboxID`.
        harness.feed(json!({"type": "session.inbox.cancelled", "data": {"sessionID": "ses_1", "inboxID": "msg_1"}}));
        assert!(matches!(
            harness.drain().as_slice(),
            [DriverEvent::SteerRejected { message, .. }] if message == "keep going"
        ));
        assert!(harness.state.pending_input.is_none());

        // A cancelled prompt is already in the transcript; it is not re-queued.
        harness.state.pending_input = Some(PendingInput {
            inbox_id: "msg_2".into(),
            message: "first prompt".into(),
            steer: false,
        });
        harness.feed(json!({"type": "session.inbox.cancelled", "data": {"sessionID": "ses_1", "inboxID": "msg_2"}}));
        assert!(harness.drain().is_empty());
        assert!(harness.state.pending_input.is_none());
    }

    #[test]
    fn delivery_clears_the_pending_entry() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.state.pending_input = Some(PendingInput {
            inbox_id: "msg_1".into(),
            message: "also check the tests".into(),
            steer: true,
        });
        harness.feed(json!({"type": "session.inbox.delivered", "data": {"sessionID": "ses_1", "inboxID": "msg_other"}}));
        assert!(harness.state.pending_input.is_some());
        harness.feed(json!({"type": "session.inbox.delivered", "data": {"sessionID": "ses_1", "inboxID": "msg_1"}}));
        assert!(harness.state.pending_input.is_none());
        assert!(harness.drain().is_empty());
    }

    /// The exact retry payloads of the 2.0 schema: `session.status` nests
    /// its state under `status`, and `session.retry.scheduled` carries the
    /// failure as `error`. Both upsert the same row, and its reason is text.
    #[test]
    fn retries_surface_their_reason_in_one_row() {
        let mut harness = Harness::new(RuntimeMode::FullAccess);
        harness.feed(json!({"type": "session.status", "data": {"sessionID": "ses_1", "status": {"type": "busy"}}}));
        assert!(harness.drain().is_empty(), "only a retry status is shown");
        harness.feed(json!({"type": "session.status", "data": {
            "sessionID": "ses_1",
            "status": {
                "type": "retry",
                "attempt": 2,
                "message": "Rate limited",
                "next": 1_790_000_000_000_u64,
                "action": {"reason": "rate_limit", "provider": "anthropic", "title": "Anthropic is rate limiting", "message": "Retrying in 8s", "label": "Retry"}
            }
        }}));
        harness.feed(json!({"type": "session.retry.scheduled", "data": {
            "sessionID": "ses_1",
            "assistantMessageID": "msg_1",
            "attempt": 3,
            "at": 1_790_000_000_000_u64,
            "error": {"type": "api", "message": "Overloaded", "status": 529}
        }}));
        let rows = harness
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                DriverEvent::RichActivity(item) => Some(item),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "Anthropic is rate limiting");
        assert!(
            rows[0]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("Retrying in 8s"),
            "{:?}",
            rows[0].output
        );
        assert!(
            rows[1]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("Overloaded"),
            "{:?}",
            rows[1].output
        );
        assert!(
            rows.iter()
                .all(|row| row.source_id.as_deref() == Some("retry:ses_1"))
        );
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
            opencode_api::AgentInfo {
                id: "plan".into(),
                name: "Plan".into(),
                description: None,
                mode: opencode_api::AgentMode::Primary,
                hidden: false,
                model: None,
            },
            opencode_api::AgentInfo {
                id: "title".into(),
                name: "Title".into(),
                description: None,
                mode: opencode_api::AgentMode::Primary,
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
    fn registered_commands_reach_the_palette_once_each() {
        let command = |name: &str| opencode_api::CommandInfo {
            name: name.into(),
            description: Some(format!("{name} things")),
        };
        let reported = reported_commands(vec![
            command("init"),
            command("review"),
            command("review"),
            command(""),
        ]);
        assert_eq!(
            reported
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["init", "review"]
        );
        assert_eq!(reported[1].description, "review things");
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
            generated_title(Some("Wire OpenCode")).as_deref(),
            Some("Wire OpenCode")
        );
    }

    /// Drives the user's own adopted service through the real driver. Ignored
    /// by default: it needs the OpenCode CLI installed and its background
    /// service healthy. Run with
    /// `cargo test -p waku-core opencode_session_against_the_adopted_service -- --ignored`.
    #[test]
    #[ignore = "requires a healthy OpenCode background service"]
    fn opencode_session_against_the_adopted_service() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
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

        let (session_id, directory) = connected_cursor(&event_rx);
        assert!(session_id.starts_with("ses_"));
        assert!(directory.is_some_and(|directory| !directory.ends_with('/')));
        drop(driver);
    }

    /// The cursor from `Connected`, skipping the usage report that can precede
    /// it.
    fn connected_cursor(events: &Receiver<DriverEvent>) -> (String, Option<String>) {
        loop {
            match events
                .recv_timeout(Duration::from_secs(30))
                .expect("the driver should report its cursor")
            {
                DriverEvent::Connected {
                    provider_cursor:
                        Some(ProviderResumeCursor::OpenCode {
                            session_id,
                            directory,
                        }),
                } => return (session_id, directory),
                DriverEvent::Connected { provider_cursor } => {
                    panic!("expected an OpenCode cursor, got {provider_cursor:?}")
                }
                _ => {}
            }
        }
    }

    /// Everything the driver reports until the turn settles, plus a grace
    /// window that would catch a second `TurnFinished`.
    fn settle(
        events: &Receiver<DriverEvent>,
        mut on_event: impl FnMut(&DriverEvent),
    ) -> Vec<DriverEvent> {
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        let mut seen = Vec::new();
        let mut finished_at: Option<std::time::Instant> = None;
        loop {
            let wait = match finished_at {
                Some(at) => (at + Duration::from_secs(3))
                    .saturating_duration_since(std::time::Instant::now()),
                None => deadline.saturating_duration_since(std::time::Instant::now()),
            };
            match events.recv_timeout(wait) {
                Ok(event) => {
                    on_event(&event);
                    if matches!(event, DriverEvent::TurnFinished { .. }) && finished_at.is_none() {
                        finished_at = Some(std::time::Instant::now());
                    }
                    seen.push(event);
                }
                Err(_) if finished_at.is_some() => return seen,
                Err(_) => panic!("the turn never settled: {seen:?}"),
            }
        }
    }

    /// A real conversation through the driver against the adopted service: a
    /// plain turn, then a shell call with a steer that joins the running
    /// execution, a fork that drops that steered turn whole, and a resume.
    /// `WAKU_OPENCODE_TEST_MODEL` picks the model; the default is one of
    /// OpenCode's free models, whose gateway also rejects any session id
    /// OpenCode could not have minted.
    #[test]
    #[ignore = "requires a healthy OpenCode background service and a runnable model"]
    fn opencode_turn_steer_fork_and_resume_against_the_adopted_service() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let model = std::env::var("WAKU_OPENCODE_TEST_MODEL")
            .unwrap_or_else(|_| "opencode/big-pickle".to_owned());
        let workspace = std::env::temp_dir().join(format!("waku-opencode-turn-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let options = |provider_cursor| DriverStartOptions {
            binary: binary.clone(),
            cwd: workspace.clone(),
            mode: RuntimeMode::FullAccess,
            model: Some(model.clone()),
            reasoning_effort: None,
            service_tier: None,
            context_window: None,
            agent_preset: None,
            computer_use_enabled: false,
            provider_cursor,
        };
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(options(None), events).unwrap();
        let (session_id, _) = connected_cursor(&event_rx);
        let service = opencode_service::shared(&binary).unwrap();
        let endpoint = service.endpoint();

        driver.prompt("Reply with the single word FIRST.".to_owned());
        let seen = settle(&event_rx, |_| {});
        assert!(
            seen.iter()
                .any(|event| matches!(event, DriverEvent::TurnFinished { success: true, .. })),
            "{seen:?}"
        );

        driver.prompt(
            "Use your shell tool to run `sleep 6 && echo waku-probe`. When it has finished, reply with the single word DONE."
                .to_owned(),
        );
        let mut steered = false;
        let seen = settle(&event_rx, |event| {
            if !steered
                && let DriverEvent::RichActivity(item) = event
                && item.kind == ActivityKind::Command
                && !item.complete
            {
                steered = true;
                driver.steer("Also end your reply with the word STEERED.".to_owned());
            }
        });
        assert!(steered, "the shell call never surfaced: {seen:?}");
        let finished = seen
            .iter()
            .filter_map(|event| match event {
                DriverEvent::TurnFinished { success, summary } => Some((*success, summary.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(finished, [(true, None)], "one settled turn: {seen:?}");
        assert!(
            seen.iter()
                .any(|event| matches!(event, DriverEvent::SteerAccepted { .. })),
            "{seen:?}"
        );
        assert!(
            seen.iter().any(|event| matches!(
                event,
                DriverEvent::RichActivity(item)
                    if item.kind == ActivityKind::Command
                        && item.complete
                        && !item.failed
                        && item.output.as_deref().unwrap_or_default().contains("waku-probe")
            )),
            "{seen:?}"
        );
        let reply = seen
            .iter()
            .filter_map(|event| match event {
                DriverEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(reply.contains("DONE"), "{reply:?}");

        let user_messages = |session: &str| {
            let (messages, _) =
                opencode_api::list_messages(&endpoint, session, Order::Asc, None, None).unwrap();
            messages
                .into_iter()
                .filter_map(|message| match message {
                    MessageInfo::User { text, .. } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // Two prompts and a steer: three user messages in two turns.
        let original = user_messages(&session_id);
        assert_eq!(original.len(), 3, "{original:?}");
        let history =
            crate::opencode_session::provider_session_history(&binary, &session_id, 10).unwrap();
        assert_eq!(history.turns.len(), 2, "{:#?}", history.messages);
        let steer_turn = history
            .messages
            .iter()
            .find(|message| {
                message.role == crate::model::MessageRole::User
                    && message.content.contains("STEERED")
            })
            .and_then(|message| message.turn_id);
        assert_eq!(
            steer_turn,
            Some(history.turns[1].id),
            "{:#?}",
            history.messages
        );

        let ProviderResumeCursor::OpenCode {
            session_id: fork_id,
            directory,
        } = driver.fork(1).unwrap()
        else {
            panic!("expected an OpenCode cursor");
        };
        assert_ne!(fork_id, session_id);
        assert_eq!(
            user_messages(&fork_id),
            original[..1],
            "the fork drops the steered turn whole"
        );
        // A rewind with no live driver counts the same turns.
        let ProviderResumeCursor::OpenCode {
            session_id: cold_fork_id,
            ..
        } = crate::opencode_session::fork_session_at_turn(&binary, &session_id, 1).unwrap()
        else {
            panic!("expected an OpenCode cursor");
        };
        assert_eq!(user_messages(&cold_fork_id), original[..1]);
        drop(driver);

        let (events, event_rx) = crate::driver::test_event_channel();
        let resumed = OpenCodeDriver::start(
            options(Some(ProviderResumeCursor::OpenCode {
                session_id: fork_id.clone(),
                directory,
            })),
            events,
        )
        .unwrap();
        assert_eq!(connected_cursor(&event_rx).0, fork_id);
        drop(resumed);

        for session in [&session_id, &fork_id, &cold_fork_id] {
            let _ = opencode_api::delete_session(&endpoint, session);
        }
        let _ = std::fs::remove_dir_all(workspace);
    }

    /// Stops the service mid-turn and starts it again after longer than a
    /// quick restart takes. OpenCode resumes the run as it boots, with no
    /// `idle` marker in between, and Waku keeps that run in the same turn: one
    /// `TurnStarted`, one `TurnFinished`, one native turn.
    ///
    /// It stops and starts whatever service it finds, so run it only against
    /// a sandboxed one (see `opencode_turn_steer_fork_and_resume_…`).
    #[test]
    #[ignore = "stops and restarts the OpenCode service it finds; sandbox only"]
    fn opencode_turn_survives_a_service_restart() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let model = std::env::var("WAKU_OPENCODE_TEST_MODEL")
            .unwrap_or_else(|_| "opencode/big-pickle".to_owned());
        let workspace =
            std::env::temp_dir().join(format!("waku-opencode-restart-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary: binary.clone(),
                cwd: workspace.clone(),
                mode: RuntimeMode::FullAccess,
                model: Some(model),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .unwrap();
        let (session_id, _) = connected_cursor(&event_rx);
        let service = |verb: &str| {
            let status = std::process::Command::new(&binary)
                .args(["service", verb])
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "opencode service {verb} failed");
        };

        driver.prompt(
            "Use your shell tool to run `sleep 15 && echo waku-probe`. When it has finished, reply with the single word DONE."
                .to_owned(),
        );
        let mut seen = Vec::new();
        while !seen.iter().any(|event| {
            matches!(event, DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::Command && !item.complete)
        }) {
            seen.push(
                event_rx
                    .recv_timeout(Duration::from_secs(120))
                    .unwrap_or_else(|_| panic!("the shell call never surfaced: {seen:?}")),
            );
        }
        service("stop");
        // Well past the second the reader used to wait before giving up.
        thread::sleep(Duration::from_secs(5));
        service("start");
        seen.extend(settle(&event_rx, |_| {}));

        let count =
            |wanted: fn(&DriverEvent) -> bool| seen.iter().filter(|event| wanted(event)).count();
        assert_eq!(
            count(|event| matches!(event, DriverEvent::TurnStarted)),
            1,
            "{seen:?}"
        );
        assert_eq!(
            count(|event| matches!(event, DriverEvent::TurnFinished { success: true, .. })),
            1,
            "{seen:?}"
        );
        assert_eq!(
            count(|event| matches!(event, DriverEvent::TurnFinished { .. })),
            1,
            "{seen:?}"
        );
        assert_eq!(
            reconnect_rows(&seen)
                .into_iter()
                .map(|(_, complete)| complete)
                .collect::<Vec<_>>(),
            [false, true],
            "{seen:?}"
        );
        let reply = seen
            .iter()
            .filter_map(|event| match event {
                DriverEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(reply.contains("DONE"), "{reply:?}");
        let history =
            crate::opencode_session::provider_session_history(&binary, &session_id, 10).unwrap();
        assert_eq!(history.turns.len(), 1, "{:#?}", history.messages);
        drop(driver);

        let endpoint = opencode_service::shared(&binary).unwrap().endpoint();
        let _ = opencode_api::delete_session(&endpoint, &session_id);
        let _ = std::fs::remove_dir_all(workspace);
    }

    /// Nonvisual integration check: only reads Cua configuration and emits a
    /// synthetic image. Requires the signed app through WAKU_APP_EXECUTABLE.
    #[test]
    #[ignore = "requires a configured OpenCode model and a packaged Waku app"]
    fn opencode_computer_use_against_the_adopted_service() {
        struct Cleanup {
            service: Arc<OpenCodeService>,
            sessions: Vec<String>,
            directory: std::path::PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for session in &self.sessions {
                    let _ = opencode_api::delete_session(&self.service.endpoint(), session);
                }
                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }
        let binary = crate::command_env::find_executable("opencode").unwrap();
        let service = opencode_service::shared(&binary).unwrap();
        let directory = std::env::temp_dir().join(format!("waku-opencode-cua-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut cleanup = Cleanup {
            service,
            sessions: Vec::new(),
            directory,
        };
        let test_directory = cleanup.directory.clone();
        let start = || {
            let (events, received) = crate::driver::test_event_channel();
            let driver = OpenCodeDriver::start(
                DriverStartOptions {
                    binary: binary.clone(),
                    cwd: test_directory.clone(),
                    mode: RuntimeMode::FullAccess,
                    model: None,
                    reasoning_effort: None,
                    service_tier: None,
                    context_window: None,
                    agent_preset: None,
                    computer_use_enabled: true,
                    provider_cursor: None,
                },
                events,
            )
            .unwrap();
            (driver, received)
        };
        let (a, a_events) = start();
        cleanup.sessions.push(a.session_id.clone());
        let (b, b_events) = start();
        cleanup.sessions.push(b.session_id.clone());
        let directory = std::fs::canonicalize(&cleanup.directory)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let servers = opencode_api::list_mcp(&cleanup.service.endpoint(), &directory).unwrap();
        let owned: Vec<_> = servers
            .iter()
            .filter(|server| {
                server["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("waku_js_repl_"))
            })
            .collect();
        assert_eq!(
            owned.len(),
            1,
            "one connection should serve both Waku tasks"
        );
        let server = owned[0]["name"].as_str().unwrap();
        let run = |driver: &OpenCodeDriver, events: &Receiver<DriverEvent>, code: &str| {
            events.try_iter().for_each(drop);
            driver.prompt(format!("Integration test. Call the js tool on MCP server {server} exactly once with this JavaScript, then reply Done. Do not perform any other operations.\n\n{code}"));
            let deadline = std::time::Instant::now() + Duration::from_secs(120);
            let mut calls = Vec::new();
            loop {
                let event = events
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("OpenCode integration turn timed out");
                match event {
                    DriverEvent::RichActivity(item)
                        if item.complete && item.mcp_server.as_deref() == Some(server) =>
                    {
                        calls.push(item)
                    }
                    DriverEvent::TurnFinished { success, .. } => {
                        assert!(success);
                        break;
                    }
                    DriverEvent::Error(error) => panic!("OpenCode integration failed: {error}"),
                    _ => {}
                }
            }
            assert_eq!(calls.len(), 1, "one direct MCP call should complete");
            let item = calls.pop().unwrap();
            assert!(!item.failed, "{:?}", item.detail);
            item
        };
        let item = run(
            &a,
            &a_events,
            "var wakuIsolationMarker = 41; await setupComputerUseRuntime({ globals: globalThis }); var nativeConfig = await cua.get_config(); jsRepl.write('CUA_CONFIG=' + JSON.stringify(nativeConfig)); await jsRepl.emitImage('data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==');",
        );
        assert_eq!(item.image_urls.len(), 1, "{item:?}");
        assert!(
            item.output
                .as_deref()
                .unwrap_or_default()
                .contains("CUA_CONFIG=")
        );
        let item = run(
            &b,
            &b_events,
            "jsRepl.write('OTHER_SESSION=' + typeof wakuIsolationMarker);",
        );
        assert!(
            item.output
                .as_deref()
                .unwrap_or_default()
                .contains("OTHER_SESSION=undefined")
        );
        let item = run(
            &a,
            &a_events,
            "jsRepl.write('PERSISTED=' + ++wakuIsolationMarker);",
        );
        assert!(
            item.output
                .as_deref()
                .unwrap_or_default()
                .contains("PERSISTED=42")
        );
        drop(a);
        std::thread::sleep(Duration::from_millis(300));
        let item = run(
            &b,
            &b_events,
            "jsRepl.write('SURVIVED=other session closed');",
        );
        assert!(
            item.output
                .as_deref()
                .unwrap_or_default()
                .contains("SURVIVED=other session closed")
        );
        drop(b);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let servers = opencode_api::list_mcp(&cleanup.service.endpoint(), &directory).unwrap();
            if servers.iter().all(|entry| entry["name"] != server) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the final lease must unregister the MCP server"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

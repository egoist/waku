//! Discovery, adoption and event fan-out for the shared OpenCode 2 service.
//!
//! OpenCode 2 is not a per-workspace server. One background process, started
//! by whoever needed it first — usually the user's own TUI — serves every
//! workspace, because a v2 session carries its own `location.directory` and
//! the service itself sits in `$HOME`. Waku therefore *finds* that process
//! through its registration file instead of owning one, and multiplexes every
//! Waku task over the single `GET /api/event` stream it exposes.
//!
//! Three rules make that safe, and all three are load-bearing:
//!
//! * **An adopted service is never killed.** Teardown cancels the SSE socket
//!   and returns. Waku never signals the pid, never calls the service's own
//!   stop route, and never runs `opencode2 service stop` — the process it
//!   found may be driving the user's terminal, and a process Waku started with
//!   `serve --service` is indistinguishable from one it found.
//! * **The registration file is opened read-only, always.** The service polls
//!   its own descriptor every 5s and self-terminates when the contents change,
//!   so any write — including a well-meaning repair of a stale descriptor —
//!   kills the user's daemon within 5s.
//! * **The reader thread is leaseless.** It holds the hub, the stream control,
//!   an [`Endpoint`] snapshot and a `Weak<Opencode2Service>` it upgrades
//!   briefly; it must never hold a strong handle. An SSE stream only ends when
//!   the connection dies, so a strong handle there would make the parked and
//!   torn-down paths unreachable — the reader would keep itself alive forever.
//!   This generalizes the rule `opencode_pool` obeys by handing its reader a
//!   bare port.
//!
//! Demultiplexing is a security boundary rather than an optimization: the one
//! stream carries the user's own TUI sessions and every other Waku task, so a
//! frame reaches exactly the subscriber that owns its session id, the whole
//! `tui.*` remote-control family is dropped, and anything Waku does not own is
//! dropped too. See [`route`].
//!
//! Everything here blocks — file reads, HTTP, thread parks — so every caller
//! must already be off the UI thread. Driver start and the daemon's request
//! threads are.

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::{Mutex, RwLock};
use serde::Deserialize;
use serde_json::Value;

use crate::http_wire::{
    Endpoint, SseFrame, StreamControl, open_event_stream, read_sse_frames, request_json,
};

/// The Basic username the service demands. Any other username answers 401.
const SERVICE_USER: &str = "opencode";
const REGISTRATION_FILE: &str = "service.json";
const HEALTH_ROUTE: &str = "/api/health";
const EVENT_ROUTE: &str = "/api/event";
const MODEL_ROUTE: &str = "/api/model";

/// Health is a liveness question, not a work request: a probe that hangs must
/// not eat the start budget it is being polled inside.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const MODEL_TIMEOUT: Duration = Duration::from_secs(10);
/// The service heartbeats `: heartbeat` exactly 15.000s apart (measured over a
/// live idle stream), so 20s would tear down a healthy connection that merely
/// lost one beat, while 60s+ would leave a dead one undetected for a minute.
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(45);
const SERVICE_START_BUDGET: Duration = Duration::from_secs(20);
const SERVICE_START_POLL: Duration = Duration::from_millis(100);
const RECONNECT_MIN_BACKOFF: Duration = Duration::from_millis(250);
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(8);
/// A permanently dead service should produce one clean start failure instead
/// of an endless stream of per-request HTTP errors, so the reader gives up
/// rediscovery after this many consecutive failures and hands the slot back.
const MAX_REDISCOVERY_FAILURES: u32 = 3;

/// The descriptor the service writes for its clients.
///
/// `version` is the service's own build id. Waku surfaces it rather than
/// comparing it: the CLI's incumbency check compares against *its* build, and
/// Waku is not an opencode2 build, so equality there would reject every
/// perfectly healthy service.
#[derive(Clone, Deserialize)]
pub(crate) struct ServiceRegistration {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    pub url: String,
    pub pid: u32,
    pub password: String,
}

impl std::fmt::Debug for ServiceRegistration {
    /// Hand-written so the credential cannot reach a log, an error string or a
    /// panic message through a derived `Debug`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceRegistration")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("url", &self.url)
            .field("pid", &self.pid)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// `${XDG_STATE_HOME:-$HOME/.local/state}/opencode/service.json`.
pub(crate) fn registration_path() -> PathBuf {
    state_directory().join(REGISTRATION_FILE)
}

fn state_directory() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from(".local").join("state"))
        .join("opencode")
}

/// Reads the descriptor of whatever service is registered, if any.
///
/// Read-only by contract; see the module doc. Absence is an ordinary answer —
/// the user simply has no service running — so this returns `None` rather than
/// an error.
pub(crate) fn read_registration() -> Option<ServiceRegistration> {
    read_registration_file(&registration_path()).or_else(channel_registration)
}

/// A non-mainstream channel registers as `service-<channel>.json`. The
/// mainstream name is probed first so a stale channel descriptor can never
/// shadow the service the user is actually running.
fn channel_registration() -> Option<ServiceRegistration> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(state_directory())
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("service-") && name.ends_with(".json"))
        })
        .collect();
    candidates.sort();
    candidates
        .iter()
        .find_map(|path| read_registration_file(path))
}

fn read_registration_file(path: &Path) -> Option<ServiceRegistration> {
    let contents = std::fs::read(path).ok()?;
    serde_json::from_slice(&contents).ok()
}

/// Confirms a descriptor still names a live service, and returns the endpoint
/// to talk to it with.
pub(crate) fn probe(registration: &ServiceRegistration) -> anyhow::Result<Endpoint> {
    let endpoint = Endpoint::basic(&registration.url, SERVICE_USER, &registration.password)?;
    let health = request_json(&endpoint, "GET", HEALTH_ROUTE, None, HEALTH_TIMEOUT)
        .context("the OpenCode 2 service did not answer its health route")?;
    accept_health(registration, &health)?;
    Ok(endpoint)
}

fn accept_health(registration: &ServiceRegistration, health: &Value) -> anyhow::Result<()> {
    if health.get("healthy").and_then(Value::as_bool) != Some(true) {
        bail!("the OpenCode 2 service reported itself unhealthy");
    }
    // The pid comparison is the whole point of the probe: a descriptor left
    // behind by a service that died still parses, and its port can already
    // have been reused by an unrelated listener that answers 200.
    if health.get("pid").and_then(Value::as_u64) != Some(u64::from(registration.pid)) {
        bail!(
            "the OpenCode 2 service on {} is not the process its registration names",
            registration.url
        );
    }
    Ok(())
}

/// How the service Waku is talking to came to exist.
///
/// `Adopted` covers both a service Waku found and one it started with
/// `serve --service`; the two are indistinguishable by construction, and
/// neither is ever signalled. `Private` is reserved for the future Computer
/// Use path (`serve --stdio --port 0` with a Waku-generated password, which
/// prints one JSON line and exits when stdin closes) and is unreachable today;
/// wiring it up will also need the guardian-script plus `process_group(0)`
/// wrapper from `deepseek_session`, because the dev watcher SIGTERMs Waku
/// without running destructors.
#[allow(dead_code)]
pub(crate) enum Ownership {
    Adopted { pid: u32 },
    Private(StdMutex<std::process::Child>),
}

/// What a subscriber receives.
#[derive(Clone, Debug)]
pub(crate) enum HubFrame {
    /// One `/api/event` envelope, verbatim.
    Event(Value),
    /// The stream came back after a break. Drivers reconcile against the
    /// server before resuming their event loop; `generation` orders that
    /// repair against any older in-flight one.
    Resync { generation: u64 },
    /// The stream broke. Reconnection is already under way.
    Disconnected,
}

/// Fan-out from the one event stream to the sessions Waku owns.
#[derive(Default)]
pub(crate) struct EventHub {
    subscribers: Mutex<HashMap<String, Vec<(usize, Sender<HubFrame>)>>>,
    next_id: AtomicUsize,
}

impl EventHub {
    /// Channels are unbounded by requirement, not by taste: `/api/event` is
    /// volatile by contract — a slow consumer overflows and fails the stream —
    /// and with one shared connection a single slow subscriber would take down
    /// every session at once.
    ///
    /// Returns the token [`EventHub::unsubscribe`] needs, since two runtimes
    /// can legitimately watch the same session id.
    fn subscribe(&self, session_id: &str) -> (usize, Receiver<HubFrame>) {
        let (tx, rx) = unbounded();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers
            .lock()
            .entry(session_id.to_owned())
            .or_default()
            .push((id, tx));
        (id, rx)
    }

    fn unsubscribe(&self, session_id: &str, rx_id: usize) {
        let mut subscribers = self.subscribers.lock();
        let Some(session) = subscribers.get_mut(session_id) else {
            return;
        };
        session.retain(|(id, _)| *id != rx_id);
        if session.is_empty() {
            subscribers.remove(session_id);
        }
    }

    /// Routes one envelope. Frames for sessions Waku does not own — the user's
    /// own TUI work, most of what this stream carries — land here and go
    /// nowhere.
    fn publish(&self, envelope: Value) {
        match route(&envelope) {
            Route::Drop => {}
            Route::Session(session_id) => {
                let subscribers = self.subscribers.lock();
                if let Some(session) = subscribers.get(&session_id) {
                    send_to(session, HubFrame::Event(envelope));
                }
            }
            Route::Broadcast => self.broadcast(HubFrame::Event(envelope)),
        }
    }

    fn broadcast(&self, frame: HubFrame) {
        let subscribers = self.subscribers.lock();
        for session in subscribers.values() {
            send_to(session, frame.clone());
        }
    }

    /// How many session ids currently have a subscriber.
    ///
    /// Part of the hub's surface — the reader must never reach into the map
    /// itself — and exercised by this module's tests.
    #[allow(dead_code)]
    fn session_count(&self) -> usize {
        self.subscribers.lock().len()
    }
}

/// Hands the frame to every sender, moving rather than cloning into the last
/// one. Streaming text deltas arrive at frame rate and the common case is a
/// single subscriber, so the clone that a naive loop performs is a deep copy
/// of the whole envelope per frame for nothing.
fn send_to(session: &[(usize, Sender<HubFrame>)], frame: HubFrame) {
    let Some(((_, last), rest)) = session.split_last() else {
        return;
    };
    for (_, sender) in rest {
        let _ = sender.send(frame.clone());
    }
    let _ = last.send(frame);
}

/// Where one envelope belongs.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Route {
    Session(String),
    Broadcast,
    Drop,
}

/// Demultiplexes one `/api/event` envelope.
///
/// This is a security boundary. The stream is shared with the user's terminal
/// and with every other Waku task, so anything that cannot be attributed to a
/// session Waku owns must not reach a transcript.
fn route(envelope: &Value) -> Route {
    let kind = envelope
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // `tui.*` are remote-control commands the service broadcasts to every
    // connected client — append to the prompt, execute a command, select a
    // session, show a toast. Forwarding them would let a foreign opencode2
    // client drive Waku's composer and navigation.
    if kind.starts_with("tui.") {
        return Route::Drop;
    }
    // `form.created` is the ONE session event whose id is nested. A uniform
    // demultiplexer reads `data.sessionID`, finds nothing, and silently
    // broadcasts every form to every task.
    if kind == "form.created" {
        return match envelope
            .pointer("/data/form/sessionID")
            .and_then(Value::as_str)
        {
            Some(session_id) => Route::Session(session_id.to_owned()),
            None => Route::Drop,
        };
    }
    match envelope.pointer("/data/sessionID").and_then(Value::as_str) {
        Some(session_id) => Route::Session(session_id.to_owned()),
        // Session-less envelopes are catalog invalidation — `catalog.updated`,
        // `agent.updated`, `command.updated`, `skill.updated`,
        // `config.updated`, `models-dev.refreshed` — and carry no session data.
        None => Route::Broadcast,
    }
}

/// The envelope shape every `/api/event` frame must have.
///
/// Parsed to validate the frame, not to be read: routing works off the raw
/// `Value` so the whole envelope reaches the driver untouched. `created` MUST
/// stay optional — the first frame of every connection is
/// `{"id":"evt_…","type":"server.connected","data":{}}` with no `created`,
/// while every other member has one, so a required field fails the very first
/// frame of every connect.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct V2Event {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    created: Option<f64>,
}

/// The one adopted service, and everything hanging off its event stream.
pub(crate) struct Opencode2Service {
    /// Swapped in place when a CLI upgrade replaces the service: the upgrade
    /// version-checks incumbency and takes over with a new url, pid and
    /// password, which must not look like a dead provider to live sessions.
    endpoint: RwLock<Endpoint>,
    registration: RwLock<ServiceRegistration>,
    ownership: Ownership,
    hub: Arc<EventHub>,
    stream: Arc<StreamControl>,
    generation: AtomicU64,
    subscribers: AtomicUsize,
    /// `"{providerID}/{id}"` -> `limit.context`, refreshed from `/api/model`
    /// on connect and on catalog invalidation. This is why no v2 driver needs
    /// v1's per-session context-window poller.
    model_windows: RwLock<HashMap<String, u64>>,
    /// Coalesces catalog-invalidation bursts into one refresh in flight.
    models_refreshing: AtomicBool,
    /// Serializes reader start against reader exit, so a subscription that
    /// arrives while the previous reader is winding down can neither leave the
    /// hub without a reader nor put two readers on one `StreamControl`.
    reader_active: Mutex<bool>,
    binary: PathBuf,
}

impl Opencode2Service {
    fn adopt(binary: &Path, registration: ServiceRegistration, endpoint: Endpoint) -> Arc<Self> {
        Arc::new(Self {
            endpoint: RwLock::new(endpoint),
            ownership: Ownership::Adopted {
                pid: registration.pid,
            },
            registration: RwLock::new(registration),
            hub: Arc::new(EventHub::default()),
            stream: Arc::new(StreamControl::default()),
            generation: AtomicU64::new(0),
            subscribers: AtomicUsize::new(0),
            model_windows: RwLock::new(HashMap::new()),
            models_refreshing: AtomicBool::new(false),
            reader_active: Mutex::new(false),
            binary: binary.to_path_buf(),
        })
    }

    pub(crate) fn endpoint(&self) -> Endpoint {
        self.endpoint.read().clone()
    }

    /// Bumped once per stream break, so a driver can tell a reconciliation
    /// answer for the current connection from one for a superseded pass.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The service's own build id, surfaced so channel skew is visible in the
    /// provider probe rather than silently rejected.
    pub(crate) fn version(&self) -> Option<String> {
        self.registration.read().version.clone()
    }

    /// A miss means "not known yet", never "unlimited".
    pub(crate) fn model_context_window(&self, key: &str) -> Option<u64> {
        self.model_windows.read().get(key).copied()
    }

    /// The pid of the adopted service, for reporting only. Waku never signals
    /// it: see the module doc.
    #[allow(dead_code)]
    pub(crate) fn pid(&self) -> Option<u32> {
        match self.ownership {
            Ownership::Adopted { pid } => Some(pid),
            Ownership::Private(_) => None,
        }
    }

    /// Starts watching one session. Subscribe BEFORE creating the session:
    /// the service can emit that session's first event before `POST
    /// /api/session` has even answered.
    pub(crate) fn subscribe(self: &Arc<Self>, session_id: &str) -> Subscription {
        let (rx_id, rx) = self.hub.subscribe(session_id);
        if self.subscribers.fetch_add(1, Ordering::AcqRel) == 0 {
            self.ensure_reader();
        }
        Subscription {
            service: Arc::clone(self),
            session_id: session_id.to_owned(),
            rx_id,
            rx,
        }
    }

    fn adopt_registration(&self, registration: ServiceRegistration, endpoint: Endpoint) {
        *self.endpoint.write() = endpoint;
        *self.registration.write() = registration;
    }

    fn bump_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn ensure_reader(self: &Arc<Self>) {
        let mut active = self.reader_active.lock();
        if *active {
            return;
        }
        // Clears the cancel left behind by the previous park. Safe only here:
        // no reader is running while this lock is held and the flag is false.
        self.stream.reset();
        let hub = Arc::clone(&self.hub);
        let stream = Arc::clone(&self.stream);
        let endpoint = self.endpoint.read().clone();
        // Weak, never strong: the reader would otherwise keep the service —
        // and therefore itself — alive forever. See the module doc.
        let service = Arc::downgrade(self);
        if thread::Builder::new()
            .name("waku-opencode2-events".into())
            .spawn(move || run_reader(hub, stream, endpoint, service))
            .is_ok()
        {
            *active = true;
        }
    }
}

/// One session's view of the shared stream.
///
/// Dropping it stops the fan-out for that session, and dropping the last one
/// parks the reader. The service object itself is permanent either way — it is
/// never a lease over the user's process.
pub(crate) struct Subscription {
    service: Arc<Opencode2Service>,
    session_id: String,
    rx_id: usize,
    pub rx: Receiver<HubFrame>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.service.hub.unsubscribe(&self.session_id, self.rx_id);
        if self.service.subscribers.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Park the reader rather than tear anything down: with no session
            // watching, Waku has no business decoding the user's TUI traffic.
            // The adopted process is left completely untouched.
            self.service.stream.cancel();
        }
    }
}

enum SlotState {
    Vacant,
    Starting,
    Running(Arc<Opencode2Service>),
}

/// There is exactly one service, so the slot is keyed on nothing. `Running`
/// holds a STRONG handle: the service object is permanent, which is what keeps
/// the two hazards this repo has already hit — a drop-before-start bouncing
/// the shared connection, and a shutdown running under the slot mutex —
/// structurally impossible here. The `Starting` + condvar discipline exists
/// only so N concurrent driver starts do not each run the 20s spawn poll.
fn slot() -> &'static (StdMutex<SlotState>, Condvar) {
    static SERVICE: OnceLock<(StdMutex<SlotState>, Condvar)> = OnceLock::new();
    SERVICE.get_or_init(|| (StdMutex::new(SlotState::Vacant), Condvar::new()))
}

/// Returns the shared service, starting one if none is registered.
///
/// The only entry point allowed to spawn, and reached only from driver start.
/// Blocking: discovery, an HTTP probe, and up to a 20s start poll.
pub(crate) fn shared(binary: &Path) -> anyhow::Result<Arc<Opencode2Service>> {
    acquire(binary, true)?
        .ok_or_else(|| anyhow!("the OpenCode 2 background service is not running"))
}

/// Returns the shared service only if one is already healthy, NEVER starting
/// it. Opening the Resume picker or refreshing the model catalog must not
/// start the user's background daemon.
pub(crate) fn attached(binary: &Path) -> anyhow::Result<Option<Arc<Opencode2Service>>> {
    acquire(binary, false)
}

fn acquire(binary: &Path, may_spawn: bool) -> anyhow::Result<Option<Arc<Opencode2Service>>> {
    let (state, changed) = slot();
    loop {
        let mut slot = state.lock().unwrap();
        match &*slot {
            SlotState::Running(service) => {
                let service = Arc::clone(service);
                // Revalidation blocks on HTTP, so it happens outside the slot
                // lock: an acquire must never wait behind another acquire's
                // network round trip.
                drop(slot);
                return match revalidate(&service, may_spawn) {
                    Ok(true) => Ok(Some(service)),
                    Ok(false) => Ok(None),
                    Err(error) => Err(error),
                };
            }
            SlotState::Vacant => {
                *slot = SlotState::Starting;
                break;
            }
            SlotState::Starting => {
                let _unused = changed.wait(slot).unwrap();
            }
        }
    }

    let connected = connect(binary, may_spawn);
    let mut slot = state.lock().unwrap();
    match connected {
        Ok(Some(service)) => {
            *slot = SlotState::Running(Arc::clone(&service));
            changed.notify_all();
            Ok(Some(service))
        }
        // A failed or declined start resets to `Vacant` so the next session
        // simply retries.
        Ok(None) => {
            *slot = SlotState::Vacant;
            changed.notify_all();
            Ok(None)
        }
        Err(error) => {
            *slot = SlotState::Vacant;
            changed.notify_all();
            Err(error)
        }
    }
}

fn connect(binary: &Path, may_spawn: bool) -> anyhow::Result<Option<Arc<Opencode2Service>>> {
    if let Some(registration) = read_registration() {
        if let Ok(endpoint) = probe(&registration) {
            return Ok(Some(Opencode2Service::adopt(
                binary,
                registration,
                endpoint,
            )));
        }
    }
    if !may_spawn {
        return Ok(None);
    }
    let (registration, endpoint) = spawn_service(binary)?;
    Ok(Some(Opencode2Service::adopt(
        binary,
        registration,
        endpoint,
    )))
}

/// Re-checks the service the slot has been holding.
///
/// A CLI upgrade replaces the running service with a new url, pid and
/// password, and the user can stop it outright between two Waku tasks. Both
/// are absorbed by refreshing the endpoint IN PLACE — minting a second service
/// object would put a second reader on the same process and orphan every live
/// subscription.
fn revalidate(service: &Arc<Opencode2Service>, may_spawn: bool) -> anyhow::Result<bool> {
    if let Some(registration) = read_registration() {
        if let Ok(endpoint) = probe(&registration) {
            service.adopt_registration(registration, endpoint);
            return Ok(true);
        }
    }
    if !may_spawn {
        return Ok(false);
    }
    let (registration, endpoint) = spawn_service(&service.binary)?;
    service.adopt_registration(registration, endpoint);
    Ok(true)
}

/// Starts a background service and waits for it to register.
///
/// `serve --service` short-circuits on a healthy incumbent, so it is
/// idempotent and safe to race — including against a second Waku daemon, which
/// happens routinely when a release build and `Waku Debug.app` run side by
/// side. In that race the in-process condvar does nothing at all, and the
/// pid-checked rediscovery below is the only thing that converges both
/// processes on one service.
fn spawn_service(binary: &Path) -> anyhow::Result<(ServiceRegistration, Endpoint)> {
    let mut command = crate::command_env::command(binary);
    command
        .args(["serve", "--service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = crate::command_env::spawn(&mut command)
        .context("failed to start the OpenCode 2 background service")?;

    let deadline = Instant::now() + SERVICE_START_BUDGET;
    let mut exit = None;
    loop {
        if let Some(registration) = read_registration() {
            if let Ok(endpoint) = probe(&registration) {
                // Reap the launcher if it has already detached. The service
                // itself is never signalled, and dropping a `Child` does not
                // kill it, so an attached service simply outlives this handle.
                let _unused = child.try_wait();
                return Ok((registration, endpoint));
            }
        }
        if exit.is_none() {
            exit = child.try_wait().ok().flatten();
        }
        if Instant::now() >= deadline {
            // A non-zero exit is deliberately NOT fatal on its own: losing the
            // race to another Waku daemon looks exactly like that, and the
            // winner's service can still be seconds away from healthy.
            match exit {
                Some(status) => bail!(
                    "`opencode2 serve --service` exited ({status}) without registering a healthy service"
                ),
                None => bail!("timed out starting the OpenCode 2 background service"),
            }
        }
        thread::sleep(SERVICE_START_POLL);
    }
}

/// Drops the slot back to `Vacant` when `service` is still the one it holds,
/// so the next session start reports a clean failure instead of adopting a
/// service the reader has already given up on.
fn vacate(service: &Arc<Opencode2Service>) {
    let (state, changed) = slot();
    let mut slot = state.lock().unwrap();
    if let SlotState::Running(current) = &*slot {
        if !Arc::ptr_eq(current, service) {
            return;
        }
    } else {
        return;
    }
    *slot = SlotState::Vacant;
    changed.notify_all();
}

/// The single event reader.
///
/// Leaseless by construction: `service` is a `Weak` upgraded only for the
/// moment it takes to record something. THE RECONNECT LOOP MAY NOT SPAWN — a
/// user who ran `opencode2 service stop` must not have their daemon
/// resurrected by a reader thread no session needs — and the end of the stream
/// MUST NEVER become `ProcessExited`, because Waku does not own this process.
fn run_reader(
    hub: Arc<EventHub>,
    stream: Arc<StreamControl>,
    mut endpoint: Endpoint,
    service: Weak<Opencode2Service>,
) {
    let mut backoff = RECONNECT_MIN_BACKOFF;
    let mut failures = 0_u32;
    let mut pending_resync = None;
    let mut restart = true;
    loop {
        if stream.is_cancelled() {
            break;
        }
        match open_event_stream(&endpoint, EVENT_ROUTE, &stream) {
            Ok(Some(socket)) => {
                backoff = RECONNECT_MIN_BACKOFF;
                failures = 0;
                refresh_model_windows(&service, &endpoint);
                if let Some(generation) = pending_resync.take() {
                    hub.broadcast(HubFrame::Resync { generation });
                }
                pump(&hub, &stream, &endpoint, &service, socket);
            }
            // Cancelled during setup: the last subscription went away.
            Ok(None) => break,
            Err(_) => {}
        }
        stream.clear();
        if stream.is_cancelled() {
            break;
        }

        hub.broadcast(HubFrame::Disconnected);
        let Some(generation) = service.upgrade().map(|service| service.bump_generation()) else {
            break;
        };
        pending_resync = Some(generation);
        if rediscover(&service, &mut endpoint) {
            failures = 0;
        } else {
            failures += 1;
            if failures >= MAX_REDISCOVERY_FAILURES {
                if let Some(service) = service.upgrade() {
                    vacate(&service);
                }
                restart = false;
                break;
            }
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
    }

    if let Some(service) = service.upgrade() {
        {
            *service.reader_active.lock() = false;
        }
        // A subscription that arrived while this reader was winding down would
        // otherwise leave the hub with no reader at all: `subscribe` only
        // starts one on the 0 -> 1 transition.
        if restart && service.subscribers.load(Ordering::Acquire) > 0 {
            service.ensure_reader();
        }
    }
}

/// Reads frames until the stream dies. Returning means "reconnect".
fn pump(
    hub: &EventHub,
    stream: &StreamControl,
    endpoint: &Endpoint,
    service: &Weak<Opencode2Service>,
    socket: TcpStream,
) {
    let Ok(frames) = read_sse_frames(socket, Some(STREAM_READ_TIMEOUT)) else {
        return;
    };
    for frame in frames {
        if stream.is_cancelled() {
            return;
        }
        let data = match frame {
            Ok(SseFrame::Comment) => continue,
            Ok(SseFrame::Data(data)) => data,
            // The service reports a server-side stream failure as a named
            // event (`effect/httpapi/stream/failure`) and then says nothing,
            // so this is a disconnect, not a frame to skip.
            Ok(SseFrame::Named { .. }) => return,
            Err(_) => return,
        };
        let Ok(envelope) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        // A frame that is not an envelope means the stream is no longer
        // carrying what Waku thinks it is; dropping it silently would leave a
        // session waiting forever for events that already stopped.
        if V2Event::deserialize(&envelope).is_err() {
            return;
        }
        if matches!(
            envelope.get("type").and_then(Value::as_str),
            Some("catalog.updated" | "models-dev.refreshed")
        ) {
            refresh_model_windows(service, endpoint);
        }
        hub.publish(envelope);
    }
}

/// Re-reads the descriptor and re-probes after a break, swapping the endpoint
/// when a CLI upgrade has replaced the service. Never spawns.
fn rediscover(service: &Weak<Opencode2Service>, endpoint: &mut Endpoint) -> bool {
    let Some(registration) = read_registration() else {
        return false;
    };
    let Ok(next) = probe(&registration) else {
        return false;
    };
    // Brief upgrade only: see the module doc.
    let Some(service) = service.upgrade() else {
        return false;
    };
    service.adopt_registration(registration, next.clone());
    *endpoint = next;
    true
}

/// Refreshes the context-window table off the reader thread.
///
/// Off-thread on purpose: `/api/event` is volatile by contract, so the reader
/// may not make an HTTP request between two frames — one slow consumer fails
/// the stream for every session at once.
fn refresh_model_windows(service: &Weak<Opencode2Service>, endpoint: &Endpoint) {
    let Some(strong) = service.upgrade() else {
        return;
    };
    if strong.models_refreshing.swap(true, Ordering::AcqRel) {
        return;
    }
    let endpoint = endpoint.clone();
    let service = service.clone();
    if thread::Builder::new()
        .name("waku-opencode2-models".into())
        .spawn(move || {
            let windows = model_context_windows(&endpoint);
            if let Some(service) = service.upgrade() {
                if let Some(windows) = windows {
                    *service.model_windows.write() = windows;
                }
                service.models_refreshing.store(false, Ordering::Release);
            }
        })
        .is_err()
    {
        strong.models_refreshing.store(false, Ordering::Release);
    }
}

fn model_context_windows(endpoint: &Endpoint) -> Option<HashMap<String, u64>> {
    let response = request_json(endpoint, "GET", MODEL_ROUTE, None, MODEL_TIMEOUT).ok()?;
    Some(context_windows(&response))
}

/// `/api/model` answers `{location, data: [Model.Info]}`; the key drivers hold
/// is `"{providerID}/{id}"`, which is also how Waku stores a selected model.
fn context_windows(response: &Value) -> HashMap<String, u64> {
    response
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let provider = model.get("providerID").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            let context = model.pointer("/limit/context").and_then(Value::as_u64)?;
            Some((format!("{provider}/{id}"), context))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    fn registration(pid: u32) -> ServiceRegistration {
        ServiceRegistration {
            id: "796c624d-c94e-4086-b245-0a9560a14019".to_owned(),
            version: Some("0.0.0-beta-19192".to_owned()),
            url: "http://127.0.0.1:49374".to_owned(),
            pid,
            password: "secret".to_owned(),
        }
    }

    #[test]
    fn tui_remote_control_events_never_reach_a_session() {
        for kind in [
            "tui.prompt.append",
            "tui.command.execute",
            "tui.session.select",
            "tui.toast.show",
        ] {
            assert_eq!(
                route(&json!({"type": kind, "data": {"sessionID": "ses_a"}})),
                Route::Drop,
                "{kind} is a remote-control command, not session traffic"
            );
        }
    }

    #[test]
    fn form_created_is_routed_from_its_nested_session_id() {
        assert_eq!(
            route(&json!({
                "id": "evt_1",
                "type": "form.created",
                "data": {"form": {"id": "frm_1", "sessionID": "ses_a"}}
            })),
            Route::Session("ses_a".to_owned())
        );
        // Never broadcast a form Waku cannot attribute.
        assert_eq!(
            route(&json!({"type": "form.created", "data": {"form": {"id": "frm_1"}}})),
            Route::Drop
        );
    }

    #[test]
    fn session_events_are_routed_and_session_less_ones_broadcast() {
        assert_eq!(
            route(&json!({"type": "message.part.updated", "data": {"sessionID": "ses_b"}})),
            Route::Session("ses_b".to_owned())
        );
        assert_eq!(
            route(&json!({"type": "catalog.updated", "data": {}})),
            Route::Broadcast
        );
        assert_eq!(
            route(&json!({"type": "server.connected", "data": {}})),
            Route::Broadcast
        );
    }

    /// The first frame of every connection has no `created`; a required field
    /// there would fail every connect.
    #[test]
    fn server_connected_envelope_without_created_deserializes() {
        let envelope =
            json!({"id": "evt_07679afe8001k2gZiwt8bRYXqB", "type": "server.connected", "data": {}});
        let event = V2Event::deserialize(&envelope).expect("server.connected must parse");
        assert_eq!(event.kind, "server.connected");
        assert!(event.created.is_none());

        let ordinary = json!({"id": "evt_2", "type": "session.updated", "created": 1_757_000_000.0, "data": {}});
        assert_eq!(
            V2Event::deserialize(&ordinary).unwrap().created,
            Some(1_757_000_000.0)
        );
        // An envelope with no type at all is not routable traffic.
        assert!(V2Event::deserialize(&json!({"id": "evt_3"})).is_err());
    }

    /// Frames the live service actually emits: the untyped first frame, its
    /// `: heartbeat` comments, a multi-line payload, and the named failure
    /// event that is the only warning a stream has died.
    #[test]
    fn event_stream_framing_matches_the_live_service() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let _unused = socket.write_all(
                concat!(
                    "data: {\"id\":\"evt_1\",\"type\":\"server.connected\",\"data\":{}}\n\n",
                    ": heartbeat\n\n",
                    "data: {\"id\":\"evt_2\",\n",
                    "data: \"type\":\"session.updated\"}\n\n",
                    "event: effect/httpapi/stream/failure\ndata: {\"_tag\":\"Fail\"}\n\n",
                )
                .as_bytes(),
            );
        });
        let socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let frames: Vec<SseFrame> = read_sse_frames(socket, Some(Duration::from_secs(5)))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        assert!(matches!(frames[0], SseFrame::Data(_)));
        assert_eq!(frames[1], SseFrame::Comment);
        let SseFrame::Data(joined) = &frames[2] else {
            panic!(
                "expected a joined multi-line data frame, got {:?}",
                frames[2]
            );
        };
        assert_eq!(
            serde_json::from_str::<Value>(joined).unwrap()["type"],
            json!("session.updated")
        );
        assert!(matches!(frames[3], SseFrame::Named { .. }));
    }

    #[test]
    fn health_is_rejected_when_it_names_a_different_process() {
        // A stale descriptor still parses, and its port can already have been
        // reused by a healthy listener that is not this service.
        assert!(
            accept_health(
                &registration(58286),
                &json!({"healthy": true, "version": "0.0.0-beta-19192", "pid": 99999})
            )
            .is_err()
        );
        assert!(
            accept_health(
                &registration(58286),
                &json!({"healthy": false, "pid": 58286})
            )
            .is_err()
        );
        assert!(
            accept_health(
                &registration(58286),
                &json!({"healthy": true, "version": "0.0.0-beta-19192", "pid": 58286})
            )
            .is_ok()
        );
    }

    #[test]
    fn registration_debug_never_prints_the_password() {
        let rendered = format!("{:?}", registration(58286));
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn context_windows_are_keyed_the_way_a_selected_model_is_stored() {
        let response = json!({
            "location": {"directory": "/Users/egoist"},
            "data": [
                {"id": "omen-alpha", "providerID": "opencode-go", "limit": {"context": 500_000, "output": 128_000}},
                {"id": "no-limit", "providerID": "opencode-go"},
            ]
        });
        let windows = context_windows(&response);
        assert_eq!(windows.get("opencode-go/omen-alpha"), Some(&500_000));
        assert_eq!(windows.len(), 1);
    }

    #[test]
    fn hub_delivers_only_to_the_owning_session() {
        let hub = EventHub::default();
        let (mine_id, mine) = hub.subscribe("ses_a");
        let (_, theirs) = hub.subscribe("ses_b");
        assert_eq!(hub.session_count(), 2);

        hub.publish(json!({"type": "message.updated", "data": {"sessionID": "ses_a"}}));
        // The user's own TUI session shares this stream and must go nowhere.
        hub.publish(json!({"type": "message.updated", "data": {"sessionID": "ses_tui"}}));
        hub.publish(json!({"type": "tui.prompt.append", "data": {"sessionID": "ses_b"}}));
        hub.broadcast(HubFrame::Resync { generation: 7 });

        assert!(matches!(mine.try_recv(), Ok(HubFrame::Event(_))));
        assert!(matches!(
            mine.try_recv(),
            Ok(HubFrame::Resync { generation: 7 })
        ));
        assert!(mine.try_recv().is_err());
        assert!(matches!(
            theirs.try_recv(),
            Ok(HubFrame::Resync { generation: 7 })
        ));
        assert!(theirs.try_recv().is_err());

        hub.unsubscribe("ses_a", mine_id);
        assert_eq!(hub.session_count(), 1);
        hub.publish(json!({"type": "message.updated", "data": {"sessionID": "ses_a"}}));
        assert!(mine.try_recv().is_err());
    }

    #[test]
    #[ignore = "requires the user's running OpenCode 2 service"]
    fn adopts_the_registered_service() {
        let registration = read_registration().expect("no OpenCode 2 service is registered");
        let endpoint = probe(&registration).expect("the registered service is not healthy");
        assert_eq!(endpoint.host, "127.0.0.1");
        assert!(endpoint.auth.is_some());
        assert!(registration.version.is_some());

        let response = request_json(&endpoint, "GET", MODEL_ROUTE, None, MODEL_TIMEOUT).unwrap();
        assert!(
            !context_windows(&response).is_empty(),
            "the live catalog should report at least one context window"
        );
    }
}

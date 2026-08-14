use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context as _, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::Mutex;
use subtle::ConstantTimeEq as _;
use tungstenite::handshake::server::{
    ErrorResponse, Request as HandshakeRequest, Response as HandshakeResponse,
};
use tungstenite::http::{StatusCode, header::ORIGIN};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket, accept_hdr_with_config};
use uuid::Uuid;

use crate::protocol::MAX_WIRE_MESSAGE_BYTES;
use crate::protocol::{
    ClientMessage, Command, PROTOCOL_VERSION, ReplayCursor, Request, ResponseOutcome,
    ResponsePayload, RpcError, SequencedEvent, ServerMessage, WireDriverEvent,
};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_HANDSHAKE_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;
const MAX_REPLAY_EVENTS_PER_SESSION: usize = 4096;
const MAX_CACHED_RESPONSES: usize = 2048;

#[derive(Clone, Debug, Default)]
pub struct ServerOptions {
    /// Browser WebSocket handshakes always carry an Origin header. Native
    /// clients do not. An empty set therefore permits native clients only.
    pub allowed_origins: HashSet<String>,
    /// Only a daemon owned by the desktop process should accept the global
    /// shutdown control message. Service-managed daemons keep running when an
    /// authenticated client disconnects.
    pub allow_shutdown: bool,
}

struct ConnectionPermit(Arc<AtomicUsize>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub trait Backend: Send + Sync + 'static {
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload>;

    fn shutdown(&self) {}
}

#[derive(Clone)]
pub struct EventSink {
    session_id: Uuid,
    runtime_id: Uuid,
    hub: Arc<Hub>,
}

impl EventSink {
    pub fn send(&self, event: WireDriverEvent) -> anyhow::Result<()> {
        self.hub.emit(self.session_id, self.runtime_id, event);
        Ok(())
    }
}

#[derive(Default)]
struct HubState {
    next_subscriber_id: u64,
    subscribers: HashMap<u64, Sender<ServerMessage>>,
    active_runtimes: HashMap<Uuid, Uuid>,
    next_sequences: HashMap<(Uuid, Uuid), u64>,
    journal: HashMap<(Uuid, Uuid), VecDeque<SequencedEvent>>,
    responses: VecDeque<(Uuid, ResponseOutcome)>,
}

struct Hub {
    epoch: Uuid,
    state: Mutex<HubState>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            epoch: Uuid::new_v4(),
            state: Mutex::new(HubState::default()),
        }
    }
}

struct DispatchedRequest {
    request: Request,
    outgoing: Sender<ServerMessage>,
}

struct RuntimeMailbox {
    id: Uuid,
    sender: Sender<DispatchedRequest>,
}

struct RequestDispatcher {
    backend: Arc<dyn Backend>,
    hub: Arc<Hub>,
    /// A live provider runtime is an actor owned by the daemon, not by any
    /// particular WebSocket connection. One mailbox per session preserves
    /// lifecycle order across desktop and web clients without serializing
    /// unrelated sessions or read-only requests.
    runtime_mailboxes: Arc<Mutex<HashMap<Uuid, RuntimeMailbox>>>,
}

impl Hub {
    fn event_sink(self: &Arc<Self>, session_id: Uuid, runtime_id: Uuid) -> EventSink {
        EventSink {
            session_id,
            runtime_id,
            hub: self.clone(),
        }
    }

    fn begin_runtime(&self, session_id: Uuid, runtime_id: Uuid) {
        let mut state = self.state.lock();
        state.active_runtimes.insert(session_id, runtime_id);
        state
            .next_sequences
            .retain(|(candidate, _), _| *candidate != session_id);
        state
            .journal
            .retain(|(candidate, _), _| *candidate != session_id);
    }

    fn emit(&self, session_id: Uuid, runtime_id: Uuid, event: WireDriverEvent) {
        let mut state = self.state.lock();
        if state.active_runtimes.get(&session_id) != Some(&runtime_id) {
            return;
        }
        let sequence = state
            .next_sequences
            .entry((session_id, runtime_id))
            .or_default();
        *sequence = sequence.saturating_add(1);
        let event = SequencedEvent {
            session_id,
            runtime_id,
            epoch: self.epoch,
            sequence: *sequence,
            event,
        };
        let journal = state.journal.entry((session_id, runtime_id)).or_default();
        journal.push_back(event.clone());
        while journal.len() > MAX_REPLAY_EVENTS_PER_SESSION {
            journal.pop_front();
        }
        let message = ServerMessage::Event(event);
        state
            .subscribers
            .retain(|_, subscriber| subscriber.send(message.clone()).is_ok());
    }

    fn subscribe(&self, resume_from: &[ReplayCursor], sender: Sender<ServerMessage>) -> u64 {
        let mut state = self.state.lock();
        for (&(session_id, runtime_id), events) in &state.journal {
            let sequence = resume_from
                .iter()
                .find(|cursor| {
                    cursor.session_id == session_id
                        && cursor.runtime_id == runtime_id
                        && cursor.epoch == self.epoch
                })
                .map(|cursor| cursor.sequence)
                .unwrap_or_default();
            for event in events.iter().filter(|event| event.sequence > sequence) {
                let _ = sender.send(ServerMessage::Event(event.clone()));
            }
        }
        let id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.saturating_add(1);
        state.subscribers.insert(id, sender);
        id
    }

    fn unsubscribe(&self, subscriber_id: u64) {
        self.state.lock().subscribers.remove(&subscriber_id);
    }

    fn cached_response(&self, request_id: Uuid) -> Option<ResponseOutcome> {
        self.state
            .lock()
            .responses
            .iter()
            .rev()
            .find_map(|(cached_id, outcome)| (*cached_id == request_id).then(|| outcome.clone()))
    }

    fn cache_response(&self, request_id: Uuid, outcome: ResponseOutcome) {
        let mut state = self.state.lock();
        state.responses.push_back((request_id, outcome));
        while state.responses.len() > MAX_CACHED_RESPONSES {
            state.responses.pop_front();
        }
    }
}

impl RequestDispatcher {
    fn new(backend: Arc<dyn Backend>, hub: Arc<Hub>) -> Self {
        Self {
            backend,
            hub,
            runtime_mailboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn dispatch(&self, request: Request, outgoing: Sender<ServerMessage>) {
        if command_targets_runtime(&request.command) {
            self.dispatch_runtime(request, outgoing);
        } else {
            self.dispatch_independent(request, outgoing);
        }
    }

    fn dispatch_independent(&self, request: Request, outgoing: Sender<ServerMessage>) {
        let backend = self.backend.clone();
        let hub = self.hub.clone();
        let failed_request_id = request.request_id;
        let failed_outgoing = outgoing.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("waku-daemon-request".into())
            .spawn(move || {
                handle_request(request, outgoing, backend, hub);
            })
        {
            send_dispatch_error(
                failed_request_id,
                failed_outgoing,
                &self.hub,
                format!("could not start daemon request worker: {error}"),
            );
        }
    }

    fn dispatch_runtime(&self, request: Request, outgoing: Sender<ServerMessage>) {
        let session_id = request.session_id;
        let failed_request_id = request.request_id;
        let failed_outgoing = outgoing.clone();
        let mut dispatched = DispatchedRequest { request, outgoing };
        loop {
            let mut mailboxes = self.runtime_mailboxes.lock();
            if let Some(mailbox) = mailboxes.get(&session_id) {
                match mailbox.sender.send(dispatched) {
                    Ok(()) => return,
                    Err(error) => {
                        dispatched = error.0;
                        mailboxes.remove(&session_id);
                        continue;
                    }
                }
            }

            let mailbox_id = Uuid::new_v4();
            let (sender, requests) = unbounded();
            sender
                .send(dispatched)
                .expect("a new runtime mailbox still has its receiver");
            mailboxes.insert(
                session_id,
                RuntimeMailbox {
                    id: mailbox_id,
                    sender,
                },
            );

            let backend = self.backend.clone();
            let hub = self.hub.clone();
            let mailbox_registry = Arc::downgrade(&self.runtime_mailboxes);
            let worker = std::thread::Builder::new()
                .name(format!("waku-daemon-runtime-{session_id}"))
                .spawn(move || {
                    run_runtime_mailbox(
                        session_id,
                        mailbox_id,
                        requests,
                        mailbox_registry,
                        backend,
                        hub,
                    );
                });
            if let Err(error) = worker {
                if mailboxes
                    .get(&session_id)
                    .is_some_and(|mailbox| mailbox.id == mailbox_id)
                {
                    mailboxes.remove(&session_id);
                }
                drop(mailboxes);
                send_dispatch_error(
                    failed_request_id,
                    failed_outgoing,
                    &self.hub,
                    format!("could not start runtime worker: {error}"),
                );
            }
            return;
        }
    }
}

pub fn serve(
    listener: TcpListener,
    token: String,
    backend: Arc<dyn Backend>,
    shutdown: Arc<AtomicBool>,
    options: ServerOptions,
) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("could not configure Waku daemon listener")?;
    let hub = Arc::new(Hub::default());
    let dispatcher = Arc::new(RequestDispatcher::new(backend.clone(), hub.clone()));
    let options = Arc::new(options);
    let active_connections = Arc::new(AtomicUsize::new(0));
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if active_connections
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                        (active < MAX_CONNECTIONS).then_some(active + 1)
                    })
                    .is_err()
                {
                    continue;
                }
                let connection_permit = ConnectionPermit(active_connections.clone());
                let token = token.clone();
                let dispatcher = dispatcher.clone();
                let hub = hub.clone();
                let shutdown = shutdown.clone();
                let options = options.clone();
                std::thread::Builder::new()
                    .name("waku-daemon-connection".into())
                    .spawn(move || {
                        let _connection_permit = connection_permit;
                        if let Err(error) =
                            handle_connection(stream, &token, dispatcher, hub, shutdown, &options)
                        {
                            eprintln!("waku-daemon connection ended: {error:#}");
                        }
                    })
                    .context("could not start Waku daemon connection thread")?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("Waku daemon listener failed"),
        }
    }
    backend.shutdown();
    Ok(())
}

fn handle_connection(
    stream: TcpStream,
    expected_token: &str,
    dispatcher: Arc<RequestDispatcher>,
    hub: Arc<Hub>,
    shutdown: Arc<AtomicBool>,
    options: &ServerOptions,
) -> anyhow::Result<()> {
    // Accepted sockets can inherit the listener's nonblocking flag on some
    // platforms. The handshake is deliberately blocking; steady-state reads
    // get their bounded polling behavior from SO_RCVTIMEO below.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_HANDSHAKE_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_HANDSHAKE_MESSAGE_BYTES));
    let allowed_origins = options.allowed_origins.clone();
    let mut socket = accept_hdr_with_config(
        stream,
        move |request: &HandshakeRequest, response: HandshakeResponse| {
            validate_handshake(request, response, &allowed_origins)
        },
        Some(config),
    )
    .context("WebSocket handshake failed")?;
    let hello = read_client_message(&mut socket)?;
    let resume_from = match hello {
        ClientMessage::Hello {
            protocol_version,
            token,
            resume_from,
            ..
        } if protocol_version == PROTOCOL_VERSION && token_matches(expected_token, &token) => {
            resume_from
        }
        ClientMessage::Hello {
            protocol_version, ..
        } if protocol_version != PROTOCOL_VERSION => {
            write_json(
                &mut socket,
                &ServerMessage::Rejected {
                    message: format!(
                        "protocol {protocol_version} is unsupported; expected {PROTOCOL_VERSION}"
                    ),
                },
            )?;
            return Ok(());
        }
        ClientMessage::Hello { .. } => {
            write_json(
                &mut socket,
                &ServerMessage::Rejected {
                    message: "authentication failed".into(),
                },
            )?;
            return Ok(());
        }
        _ => bail!("first daemon message was not a hello"),
    };
    write_json(
        &mut socket,
        &ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").into(),
        },
    )?;
    socket.set_config(|config| {
        config.max_message_size = Some(MAX_WIRE_MESSAGE_BYTES);
        config.max_frame_size = Some(MAX_WIRE_MESSAGE_BYTES);
    });
    socket
        .get_mut()
        .set_read_timeout(Some(SOCKET_POLL_INTERVAL))?;

    let (outgoing, outgoing_rx) = unbounded();
    let subscriber_id = hub.subscribe(&resume_from, outgoing.clone());

    'connection: while !shutdown.load(Ordering::Acquire) {
        while let Ok(message) = outgoing_rx.try_recv() {
            if write_json(&mut socket, &message).is_err() {
                break 'connection;
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => match serde_json::from_str(text.as_ref()) {
                Ok(ClientMessage::Request(request)) => {
                    dispatcher.dispatch(request, outgoing.clone());
                }
                Ok(ClientMessage::Shutdown) => {
                    if options.allow_shutdown {
                        write_json(&mut socket, &ServerMessage::ShuttingDown)?;
                        shutdown.store(true, Ordering::Release);
                        break;
                    }
                    write_json(
                        &mut socket,
                        &ServerMessage::Rejected {
                            message: "daemon shutdown is managed by its service owner".into(),
                        },
                    )?;
                }
                Ok(ClientMessage::Hello { .. }) => {}
                Err(error) => {
                    eprintln!("waku-daemon ignored invalid message: {error}");
                }
            },
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if retryable_io(&error) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break,
            Err(error) => return Err(error).context("Waku daemon WebSocket failed"),
        }
    }
    hub.unsubscribe(subscriber_id);
    Ok(())
}

fn validate_handshake(
    request: &HandshakeRequest,
    response: HandshakeResponse,
    allowed_origins: &HashSet<String>,
) -> Result<HandshakeResponse, ErrorResponse> {
    if request.uri().path() != "/v1" {
        return Err(handshake_error(
            StatusCode::NOT_FOUND,
            "unknown daemon endpoint",
        ));
    }
    if let Some(origin) = request.headers().get(ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .is_some_and(|origin| allowed_origins.contains(origin));
        if !allowed {
            return Err(handshake_error(
                StatusCode::FORBIDDEN,
                "WebSocket origin is not allowed",
            ));
        }
    }
    Ok(response)
}

fn handshake_error(status: StatusCode, message: &str) -> ErrorResponse {
    tungstenite::http::Response::builder()
        .status(status)
        .body(Some(message.to_owned()))
        .expect("static WebSocket rejection is valid")
}

fn token_matches(expected: &str, candidate: &str) -> bool {
    expected.as_bytes().ct_eq(candidate.as_bytes()).into()
}

fn command_targets_runtime(command: &Command) -> bool {
    matches!(
        command,
        Command::Start { .. }
            | Command::Prompt { .. }
            | Command::Steer { .. }
            | Command::Cancel
            | Command::CancelComputerUse
            | Command::RefreshBackgroundWork
            | Command::StopBackgroundWork { .. }
            | Command::Respond { .. }
            | Command::RunComputerTool { .. }
            | Command::RejectComputerTool { .. }
            | Command::ApplyOptions { .. }
            | Command::Rollback { .. }
            | Command::Fork { .. }
            | Command::CloseSession
    )
}

fn run_runtime_mailbox(
    session_id: Uuid,
    mailbox_id: Uuid,
    requests: Receiver<DispatchedRequest>,
    mailbox_registry: Weak<Mutex<HashMap<Uuid, RuntimeMailbox>>>,
    backend: Arc<dyn Backend>,
    hub: Arc<Hub>,
) {
    let mut active_runtime_id = None;
    let mut pending = None;
    loop {
        let dispatched = match pending.take() {
            Some(request) => request,
            None => match requests.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };
        let runtime_id = dispatched.request.runtime_id;
        let starts_runtime = matches!(&dispatched.request.command, Command::Start { .. });
        let closes_runtime = matches!(&dispatched.request.command, Command::CloseSession);
        let handled = handle_request(
            dispatched.request,
            dispatched.outgoing,
            backend.clone(),
            hub.clone(),
        );

        if handled.executed {
            if starts_runtime {
                active_runtime_id = matches!(
                    &handled.outcome,
                    ResponseOutcome::Ok {
                        payload: ResponsePayload::Started { .. }
                    }
                )
                .then_some(runtime_id);
            } else if closes_runtime {
                if active_runtime_id == Some(runtime_id)
                    && matches!(&handled.outcome, ResponseOutcome::Ok { .. })
                {
                    active_runtime_id = None;
                }
            } else if active_runtime_id.is_none()
                && matches!(&handled.outcome, ResponseOutcome::Ok { .. })
            {
                // Recover the supervisor state if a previous mailbox worker
                // exited unexpectedly while the backend runtime stayed alive.
                active_runtime_id = Some(runtime_id);
            }
        }

        if active_runtime_id.is_none() {
            pending =
                take_queued_request_or_retire(session_id, mailbox_id, &requests, &mailbox_registry);
            if pending.is_none() {
                return;
            }
        }
    }
}

fn take_queued_request_or_retire(
    session_id: Uuid,
    mailbox_id: Uuid,
    requests: &Receiver<DispatchedRequest>,
    mailbox_registry: &Weak<Mutex<HashMap<Uuid, RuntimeMailbox>>>,
) -> Option<DispatchedRequest> {
    let Some(mailbox_registry) = mailbox_registry.upgrade() else {
        return requests.try_recv().ok();
    };
    // Dispatchers send while holding this same lock. Therefore an empty
    // receiver followed by removal is atomic with respect to a new command:
    // it either joins this actor before retirement or creates its successor.
    let mut mailboxes = mailbox_registry.lock();
    match requests.try_recv() {
        Ok(request) => Some(request),
        Err(crossbeam_channel::TryRecvError::Empty) => {
            if mailboxes
                .get(&session_id)
                .is_some_and(|mailbox| mailbox.id == mailbox_id)
            {
                mailboxes.remove(&session_id);
            }
            None
        }
        Err(crossbeam_channel::TryRecvError::Disconnected) => None,
    }
}

struct HandledRequest {
    outcome: ResponseOutcome,
    executed: bool,
}

fn handle_request(
    request: Request,
    outgoing: Sender<ServerMessage>,
    backend: Arc<dyn Backend>,
    hub: Arc<Hub>,
) -> HandledRequest {
    let request_id = request.request_id;
    let session_id = request.session_id;
    let runtime_id = request.runtime_id;
    let (outcome, executed) = if let Some(cached) = hub.cached_response(request_id) {
        (cached, false)
    } else {
        if matches!(&request.command, Command::Start { .. }) {
            hub.begin_runtime(session_id, runtime_id);
        }
        let outcome = match backend.handle(request, hub.event_sink(session_id, runtime_id)) {
            Ok(payload) => ResponseOutcome::Ok { payload },
            Err(error) => ResponseOutcome::Error {
                error: RpcError::from(error),
            },
        };
        hub.cache_response(request_id, outcome.clone());
        (outcome, true)
    };
    let _ = outgoing.send(ServerMessage::Response {
        request_id,
        outcome: outcome.clone(),
    });
    HandledRequest { outcome, executed }
}

fn send_dispatch_error(
    request_id: Uuid,
    outgoing: Sender<ServerMessage>,
    hub: &Arc<Hub>,
    message: String,
) {
    let outcome = hub
        .cached_response(request_id)
        .unwrap_or_else(|| ResponseOutcome::Error {
            error: RpcError { message },
        });
    hub.cache_response(request_id, outcome.clone());
    let _ = outgoing.send(ServerMessage::Response {
        request_id,
        outcome,
    });
}

fn retryable_io(error: &io::Error) -> bool {
    retryable_error(error)
}

fn retryable_error(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
        ) {
            return true;
        }
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::EAGAIN)
            || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        {
            return true;
        }
    }
    error.source().is_some_and(retryable_error)
}

fn read_client_message(socket: &mut WebSocket<TcpStream>) -> anyhow::Result<ClientMessage> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(serde_json::from_str(text.as_ref())?),
            Message::Ping(_) => socket.flush()?,
            Message::Close(_) => bail!("client closed during daemon handshake"),
            _ => {}
        }
    }
}

fn write_json<S: io::Read + io::Write, T: serde::Serialize>(
    socket: &mut WebSocket<S>,
    value: &T,
) -> anyhow::Result<()> {
    socket.send(Message::Text(serde_json::to_string(value)?.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WireDriverStartOptions;
    use crossbeam_channel::bounded;
    use serde_json::json;
    use std::path::PathBuf;
    use waku_client::DaemonClient;

    #[derive(Default)]
    struct TestBackend;

    impl Backend for TestBackend {
        fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload> {
            match request.command {
                Command::Start { .. } => {
                    events.send(WireDriverEvent::new("connected", json!({})))?;
                    Ok(ResponsePayload::Started {
                        supports_steer: true,
                    })
                }
                _ => Ok(ResponsePayload::Ack),
            }
        }
    }

    #[test]
    fn websocket_round_trip_sequences_provider_events() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = shutdown.clone();
        let server = std::thread::spawn(move || {
            serve(
                listener,
                "secret".into(),
                Arc::new(TestBackend),
                server_shutdown,
                ServerOptions {
                    allow_shutdown: true,
                    ..ServerOptions::default()
                },
            )
            .unwrap()
        });

        assert!(
            DaemonClient::connect(&address.to_string(), "wrong-secret".into()).is_err(),
            "the server must reject a client before it can issue requests"
        );
        let client = DaemonClient::connect(&address.to_string(), "secret".into()).unwrap();
        let session_id = Uuid::new_v4();
        let runtime_id = Uuid::new_v4();
        let events = client.subscribe(session_id, runtime_id);
        let response = client
            .request(
                session_id,
                runtime_id,
                Command::Start {
                    options: WireDriverStartOptions {
                        provider: "codex".into(),
                        binary: PathBuf::from("codex"),
                        cwd: PathBuf::from("."),
                        mode: "fullAccess".into(),
                        interaction_mode: "build".into(),
                        model: None,
                        reasoning_effort: None,
                        service_tier: None,
                        agent_preset: None,
                        computer_use_enabled: false,
                        provider_cursor: None,
                    },
                },
            )
            .unwrap();
        assert!(matches!(
            response,
            ResponsePayload::Started {
                supports_steer: true
            }
        ));
        let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.runtime_id, runtime_id);
        assert_eq!(event.sequence, 1);
        assert_eq!(event.event.kind, "connected");

        client.shutdown();
        let exited = events.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(exited.runtime_id, runtime_id);
        assert_eq!(exited.event.kind, "processExited");
        server.join().unwrap();
    }

    #[test]
    fn browser_origins_are_denied_unless_explicitly_allowed() {
        let request = HandshakeRequest::builder()
            .uri("/v1")
            .header(ORIGIN, "https://app.waku.test")
            .body(())
            .unwrap();
        let response = HandshakeResponse::new(());
        assert_eq!(
            validate_handshake(&request, response, &HashSet::new())
                .unwrap_err()
                .status(),
            StatusCode::FORBIDDEN
        );

        let allowed = HashSet::from(["https://app.waku.test".to_owned()]);
        assert!(validate_handshake(&request, HandshakeResponse::new(()), &allowed).is_ok());
        let native = HandshakeRequest::builder().uri("/v1").body(()).unwrap();
        assert!(validate_handshake(&native, HandshakeResponse::new(()), &HashSet::new()).is_ok());
    }

    #[test]
    fn daemon_tokens_require_an_exact_match() {
        assert!(token_matches("secret", "secret"));
        assert!(!token_matches("secret", "Secret"));
        assert!(!token_matches("secret", "secret-extra"));
    }

    #[test]
    fn replaced_runtime_ignores_late_events_from_the_old_generation() {
        let hub = Arc::new(Hub::default());
        let session_id = Uuid::new_v4();
        let old_runtime_id = Uuid::new_v4();
        let new_runtime_id = Uuid::new_v4();
        let (outgoing, events) = unbounded();
        hub.subscribe(&[], outgoing);

        hub.begin_runtime(session_id, old_runtime_id);
        let old_sink = hub.event_sink(session_id, old_runtime_id);
        old_sink
            .send(WireDriverEvent::new("old", serde_json::Value::Null))
            .unwrap();
        assert!(matches!(
            events.recv().unwrap(),
            ServerMessage::Event(event) if event.runtime_id == old_runtime_id
        ));

        hub.begin_runtime(session_id, new_runtime_id);
        old_sink
            .send(WireDriverEvent::new("stale", serde_json::Value::Null))
            .unwrap();
        hub.event_sink(session_id, new_runtime_id)
            .send(WireDriverEvent::new("new", serde_json::Value::Null))
            .unwrap();

        let ServerMessage::Event(event) = events.recv().unwrap() else {
            panic!("expected a daemon event");
        };
        assert_eq!(event.runtime_id, new_runtime_id);
        assert_eq!(event.sequence, 1);
        assert_eq!(event.event.kind, "new");
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn replay_cursor_from_an_old_daemon_epoch_does_not_hide_new_events() {
        let hub = Arc::new(Hub::default());
        let session_id = Uuid::new_v4();
        let runtime_id = Uuid::new_v4();
        hub.begin_runtime(session_id, runtime_id);
        hub.event_sink(session_id, runtime_id)
            .send(WireDriverEvent::new("new-daemon", serde_json::Value::Null))
            .unwrap();

        let (outgoing, events) = unbounded();
        hub.subscribe(
            &[ReplayCursor {
                session_id,
                runtime_id,
                epoch: Uuid::nil(),
                sequence: u64::MAX,
            }],
            outgoing,
        );

        let ServerMessage::Event(event) = events.recv().unwrap() else {
            panic!("expected the new daemon event to replay");
        };
        assert_eq!(event.epoch, hub.epoch);
        assert_eq!(event.sequence, 1);
    }

    struct BlockingProbeBackend {
        probe_started: Sender<()>,
        release_probe: Receiver<()>,
    }

    impl Backend for BlockingProbeBackend {
        fn handle(&self, request: Request, _: EventSink) -> anyhow::Result<ResponsePayload> {
            if matches!(request.command, Command::ProbeProvider { .. }) {
                self.probe_started.send(()).unwrap();
                self.release_probe.recv().unwrap();
            }
            Ok(ResponsePayload::Ack)
        }
    }

    #[test]
    fn slow_background_command_does_not_block_session_hydration() {
        let (outgoing, response_rx) = unbounded();
        let (probe_started, probe_started_rx) = bounded(1);
        let (release_probe, release_probe_rx) = bounded(1);
        let backend: Arc<dyn Backend> = Arc::new(BlockingProbeBackend {
            probe_started,
            release_probe: release_probe_rx,
        });
        let hub = Arc::new(Hub::default());
        let dispatcher = RequestDispatcher::new(backend, hub);

        let probe_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: probe_id,
                session_id: Uuid::nil(),
                runtime_id: Uuid::nil(),
                command: Command::ProbeProvider {
                    provider: crate::model::ProviderKind::Codex,
                    binary_override: None,
                    discover_models: false,
                    probe_version: false,
                },
            },
            outgoing.clone(),
        );
        probe_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let hydration_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: hydration_id,
                session_id: Uuid::nil(),
                runtime_id: Uuid::nil(),
                command: Command::HydrateSession {
                    session_id: Uuid::new_v4(),
                },
            },
            outgoing,
        );
        assert!(matches!(
            response_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ServerMessage::Response { request_id, .. } if request_id == hydration_id
        ));

        release_probe.send(()).unwrap();
        assert!(matches!(
            response_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ServerMessage::Response { request_id, .. } if request_id == probe_id
        ));
    }

    struct RuntimeOrderingBackend {
        blocked_session_id: Uuid,
        handled: Sender<(Uuid, &'static str)>,
        release_start: Receiver<()>,
    }

    impl Backend for RuntimeOrderingBackend {
        fn handle(&self, request: Request, _: EventSink) -> anyhow::Result<ResponsePayload> {
            let command = match request.command {
                Command::Start { .. } => {
                    self.handled.send((request.session_id, "start")).unwrap();
                    if request.session_id == self.blocked_session_id {
                        self.release_start.recv().unwrap();
                    }
                    return Ok(ResponsePayload::Started {
                        supports_steer: true,
                    });
                }
                Command::Prompt { .. } => "prompt",
                Command::CloseSession => "close",
                _ => "other",
            };
            self.handled.send((request.session_id, command)).unwrap();
            Ok(ResponsePayload::Ack)
        }
    }

    #[test]
    fn runtime_commands_are_ordered_per_session_without_blocking_other_sessions() {
        let blocked_session_id = Uuid::new_v4();
        let blocked_runtime_id = Uuid::new_v4();
        let other_session_id = Uuid::new_v4();
        let other_runtime_id = Uuid::new_v4();
        let (handled, handled_rx) = unbounded();
        let (release_start, release_start_rx) = bounded(1);
        let dispatcher = RequestDispatcher::new(
            Arc::new(RuntimeOrderingBackend {
                blocked_session_id,
                handled,
                release_start: release_start_rx,
            }),
            Arc::new(Hub::default()),
        );
        let (start_outgoing, start_responses) = unbounded();
        let (second_client_outgoing, second_client_responses) = unbounded();
        let (other_outgoing, other_responses) = unbounded();

        let blocked_start_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: blocked_start_id,
                session_id: blocked_session_id,
                runtime_id: blocked_runtime_id,
                command: Command::Start {
                    options: test_start_options(),
                },
            },
            start_outgoing,
        );
        assert_eq!(
            handled_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (blocked_session_id, "start")
        );

        let prompt_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: prompt_id,
                session_id: blocked_session_id,
                runtime_id: blocked_runtime_id,
                command: Command::Prompt {
                    prompt: "after start".into(),
                },
            },
            second_client_outgoing,
        );

        let other_start_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: other_start_id,
                session_id: other_session_id,
                runtime_id: other_runtime_id,
                command: Command::Start {
                    options: test_start_options(),
                },
            },
            other_outgoing.clone(),
        );
        assert_eq!(
            handled_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (other_session_id, "start")
        );
        assert!(matches!(
            other_responses
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            ServerMessage::Response { request_id, .. } if request_id == other_start_id
        ));
        assert!(handled_rx.recv_timeout(Duration::from_millis(50)).is_err());

        release_start.send(()).unwrap();
        assert!(matches!(
            start_responses
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            ServerMessage::Response { request_id, .. } if request_id == blocked_start_id
        ));
        assert_eq!(
            handled_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (blocked_session_id, "prompt")
        );
        assert!(matches!(
            second_client_responses
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            ServerMessage::Response { request_id, .. } if request_id == prompt_id
        ));

        let blocked_close_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: blocked_close_id,
                session_id: blocked_session_id,
                runtime_id: blocked_runtime_id,
                command: Command::CloseSession,
            },
            other_outgoing.clone(),
        );
        let other_close_id = Uuid::new_v4();
        dispatcher.dispatch(
            Request {
                request_id: other_close_id,
                session_id: other_session_id,
                runtime_id: other_runtime_id,
                command: Command::CloseSession,
            },
            other_outgoing,
        );
        let mut close_responses = [false; 2];
        for _ in 0..2 {
            let ServerMessage::Response { request_id, .. } = other_responses
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
            else {
                panic!("expected a close response");
            };
            if request_id == blocked_close_id {
                close_responses[0] = true;
            } else if request_id == other_close_id {
                close_responses[1] = true;
            }
        }
        assert_eq!(close_responses, [true, true]);
    }

    fn test_start_options() -> WireDriverStartOptions {
        WireDriverStartOptions {
            provider: "codex".into(),
            binary: PathBuf::from("codex"),
            cwd: PathBuf::from("."),
            mode: "fullAccess".into(),
            interaction_mode: "build".into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            agent_preset: None,
            computer_use_enabled: false,
            provider_cursor: None,
        }
    }
}

//! Resident `dsh web` Host RPC process and its loopback transport.
//!
//! DeepSeek Harness exposes client-grade unary RPC over HTTP and two
//! downlink-only WebSockets. One process hosts every Harness session; callers
//! share it through `deepseek_pool`, while this module owns startup, typed RPC
//! envelopes, stream fan-out, and process-tree teardown.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};
use uuid::Uuid;

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_RETRY_DELAY: Duration = Duration::from_millis(200);
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

// The development watcher terminates the app with SIGTERM, which does not run
// Rust destructors. Keep one pipe open in Waku and let this wrapper terminate
// the resident Host when that pipe closes, so a rebuild cannot orphan `dsh
// web`. The second watcher makes a spontaneous Host exit observable through
// the wrapper Child as well as through the ordinary process monitor.
#[cfg(unix)]
const DSH_GUARDIAN_SCRIPT: &str = r#"
parent=$$
child=
watcher=
cleanup() {
  trap - EXIT HUP INT TERM
  if [ -n "$watcher" ]; then kill -TERM "$watcher" 2>/dev/null || true; fi
  if [ -n "$child" ]; then
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  fi
}
trap cleanup EXIT HUP INT TERM
"$@" </dev/null &
child=$!
(
  while kill -0 "$child" 2>/dev/null; do sleep 1; done
  kill -TERM "$parent" 2>/dev/null || true
) &
watcher=$!
while IFS= read -r _; do :; done
"#;

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

#[derive(Default)]
struct EventHub {
    subscribers: Mutex<HashMap<String, Vec<Sender<Value>>>>,
}

impl EventHub {
    fn subscribe(&self, session_id: &str) -> Receiver<Value> {
        let (sender, receiver) = unbounded();
        self.subscribers
            .lock()
            .entry(session_id.to_owned())
            .or_default()
            .push(sender);
        receiver
    }

    fn publish(&self, envelope: Value) {
        let session_id = envelope
            .pointer("/payload/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut subscribers = self.subscribers.lock();
        match session_id {
            Some(session_id) => {
                if let Some(targets) = subscribers.get_mut(&session_id) {
                    targets.retain(|target| target.send(envelope.clone()).is_ok());
                    if targets.is_empty() {
                        subscribers.remove(&session_id);
                    }
                }
            }
            None => {
                for targets in subscribers.values_mut() {
                    targets.retain(|target| target.send(envelope.clone()).is_ok());
                }
                subscribers.retain(|_, targets| !targets.is_empty());
            }
        }
    }
}

#[derive(Default)]
struct StreamControl {
    cancelled: AtomicBool,
    sockets: Mutex<HashMap<&'static str, TcpStream>>,
}

impl StreamControl {
    fn attach(&self, name: &'static str, stream: &TcpStream) -> std::io::Result<bool> {
        let socket = stream.try_clone()?;
        if self.cancelled.load(Ordering::Acquire) {
            let _ = socket.shutdown(Shutdown::Both);
            return Ok(false);
        }
        self.sockets.lock().insert(name, socket);
        Ok(true)
    }

    fn detach(&self, name: &'static str) {
        self.sockets.lock().remove(name);
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        for (_, socket) in self.sockets.lock().drain() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

pub(crate) struct DeepSeekServer {
    child: Arc<Mutex<Child>>,
    pub(crate) port: u16,
    events: Arc<EventHub>,
    streams: Arc<StreamControl>,
}

impl DeepSeekServer {
    pub(crate) fn start(binary: &Path) -> anyhow::Result<Self> {
        Self::start_with_dsh_home(binary, None)
    }

    fn start_with_dsh_home(binary: &Path, dsh_home: Option<&Path>) -> anyhow::Result<Self> {
        let supports_no_open = web_supports_no_open(binary, dsh_home);
        #[cfg(unix)]
        let mut command = {
            let mut command = crate::command_env::command("/bin/sh");
            command
                .args(["-c", DSH_GUARDIAN_SCRIPT, "waku-dsh-guardian"])
                .arg(binary)
                .args(["web", "--host", "127.0.0.1", "--port", "0"]);
            if supports_no_open {
                command.arg("--no-open");
            }
            command
        };
        #[cfg(not(unix))]
        let mut command = {
            let mut command = crate::command_env::command(binary);
            command.args(["web", "--host", "127.0.0.1", "--port", "0"]);
            if supports_no_open {
                command.arg("--no-open");
            }
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dsh_home) = dsh_home {
            command.env("DSH_HOME", dsh_home);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = crate::command_env::spawn(&mut command)
            .context("failed to start `dsh web --host 127.0.0.1 --port 0`")?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("DeepSeek Harness provided no startup output"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("DeepSeek Harness provided no diagnostic output"))?;
        let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("waku-deepseek-stdout".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let _ = stdout_tx.send(line);
                }
            })?;
        thread::Builder::new()
            .name("waku-deepseek-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = stderr_tx.send(line);
                }
            })?;

        let started_at = Instant::now();
        let mut diagnostics = Vec::new();
        let port = 'startup: loop {
            while let Ok(line) = stdout_rx.try_recv() {
                if let Some(port) = parse_ready_port(&line) {
                    break 'startup port;
                }
                if !line.trim().is_empty() {
                    diagnostics.push(line);
                }
            }
            while let Ok(line) = stderr_rx.try_recv() {
                if !line.trim().is_empty() {
                    diagnostics.push(line);
                }
            }
            if let Some(status) = child.try_wait()? {
                let detail = diagnostics.join("\n");
                bail!(
                    "DeepSeek Harness exited during startup ({status}){}",
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {detail}")
                    }
                );
            }
            if started_at.elapsed() >= SERVER_START_TIMEOUT {
                terminate_child(&mut child, Duration::from_secs(2));
                let detail = diagnostics.join("\n");
                bail!(
                    "timed out starting DeepSeek Harness{}",
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {detail}")
                    }
                );
            }
            thread::sleep(Duration::from_millis(20));
        };

        let child = Arc::new(Mutex::new(child));
        let events = Arc::new(EventHub::default());
        let streams = Arc::new(StreamControl::default());
        for (name, path) in [("mux", "/api/events.mux"), ("host", "/api/events.host")] {
            let events = Arc::clone(&events);
            let streams = Arc::clone(&streams);
            thread::Builder::new()
                .name(format!("waku-deepseek-{name}-events"))
                .spawn(move || run_downlink(name, port, path, &events, &streams))?;
        }

        let monitor_child = Arc::clone(&child);
        let monitor_events = Arc::clone(&events);
        let monitor_streams = Arc::clone(&streams);
        thread::Builder::new()
            .name("waku-deepseek-process-monitor".into())
            .spawn(move || {
                loop {
                    if monitor_streams.is_cancelled() {
                        return;
                    }
                    let status = monitor_child.lock().try_wait();
                    match status {
                        Ok(Some(status)) => {
                            monitor_events.publish(json!({
                                "type": "server-request",
                                "rpcId": format!("waku-process-{}", Uuid::new_v4()),
                                "method": "waku/process-exited",
                                "payload": {
                                    "type": "waku/process-exited",
                                    "message": format!("DeepSeek Harness exited ({status})")
                                }
                            }));
                            return;
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(100)),
                        Err(error) => {
                            monitor_events.publish(json!({
                            "type": "server-request",
                            "rpcId": format!("waku-process-{}", Uuid::new_v4()),
                            "method": "waku/process-exited",
                            "payload": {
                                "type": "waku/process-exited",
                                "message": format!("could not observe DeepSeek Harness: {error}")
                            }
                        }));
                            return;
                        }
                    }
                }
            })?;

        Ok(Self {
            child,
            port,
            events,
            streams,
        })
    }

    pub(crate) fn subscribe(&self, session_id: &str) -> Receiver<Value> {
        self.events.subscribe(session_id)
    }

    pub(crate) fn rpc(&self, method: &str, payload: Value) -> anyhow::Result<Value> {
        let rpc_id = format!("waku-{}", Uuid::new_v4());
        let body = json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": method,
            "payload": payload,
        });
        let response = crate::opencode_session::request_json_on_port(
            self.port,
            "POST",
            &format!("/api/{method}"),
            Some(&body),
            RPC_TIMEOUT,
        )?;
        if response.get("type").and_then(Value::as_str) != Some("server-response")
            || response.get("rpcId").and_then(Value::as_str) != Some(rpc_id.as_str())
        {
            bail!("DeepSeek Harness returned an invalid {method} response");
        }
        rpc_result_value(method, &response)
    }

    pub(crate) fn respond(&self, rpc_id: &str, value: Value) -> anyhow::Result<()> {
        let body = json!({
            "type": "client-response",
            "rpcId": rpc_id,
            "result": {"ok": true, "value": value},
        });
        let response = crate::opencode_session::request_json_on_port(
            self.port,
            "POST",
            "/api/respond",
            Some(&body),
            RPC_TIMEOUT,
        )?;
        if response.get("accepted").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            bail!(
                "DeepSeek Harness rejected an interaction response ({})",
                response
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown reason")
            )
        }
    }

    pub(crate) fn reject_response(&self, rpc_id: &str, message: &str) -> anyhow::Result<()> {
        let body = json!({
            "type": "client-response",
            "rpcId": rpc_id,
            "result": {
                "ok": false,
                "error": {"code": "cancelled", "message": message, "details": {}}
            },
        });
        let response = crate::opencode_session::request_json_on_port(
            self.port,
            "POST",
            "/api/respond",
            Some(&body),
            RPC_TIMEOUT,
        )?;
        if response.get("accepted").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            bail!("DeepSeek Harness rejected a cancellation response")
        }
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.child
            .lock()
            .try_wait()
            .is_ok_and(|status| status.is_none())
    }

    pub(crate) fn shutdown(&self, timeout: Duration) {
        self.streams.cancel();
        terminate_child(&mut self.child.lock(), timeout);
    }
}

impl Drop for DeepSeekServer {
    fn drop(&mut self) {
        self.shutdown(Duration::from_secs(5));
    }
}

fn rpc_result_value(method: &str, response: &Value) -> anyhow::Result<Value> {
    let result = response
        .get("result")
        .ok_or_else(|| anyhow!("DeepSeek Harness returned no {method} result"))?;
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(result.get("value").cloned().unwrap_or(Value::Null));
    }
    let error = result.get("error").unwrap_or(&Value::Null);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    bail!("DeepSeek Harness {method} failed ({code}): {message}")
}

fn parse_ready_port(line: &str) -> Option<u16> {
    let url = line.trim().strip_prefix("dsh web: ")?;
    let url = url::Url::parse(url).ok()?;
    (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
        .then(|| url.port())
        .flatten()
}

fn web_supports_no_open(binary: &Path, dsh_home: Option<&Path>) -> bool {
    let mut command = crate::command_env::command(binary);
    command.args(["web", "--help"]);
    if let Some(dsh_home) = dsh_home {
        command.env("DSH_HOME", dsh_home);
    }
    crate::command_env::output(&mut command)
        .ok()
        .is_some_and(|output| web_help_supports_no_open(&output.stdout))
}

fn web_help_supports_no_open(output: &[u8]) -> bool {
    String::from_utf8_lossy(output)
        .lines()
        .any(|line| line.contains("--no-open"))
}

fn terminate_child(child: &mut Child, timeout: Duration) {
    if child.try_wait().is_ok_and(|status| status.is_some()) {
        return;
    }
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = child.kill();

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_downlink(
    name: &'static str,
    port: u16,
    path: &'static str,
    events: &EventHub,
    control: &StreamControl,
) {
    let mut failures = 0_u32;
    while !control.is_cancelled() {
        match WebSocketReader::connect(port, path) {
            Ok(mut reader) => {
                failures = 0;
                if control.attach(name, &reader.stream).ok() != Some(true) {
                    return;
                }
                while !control.is_cancelled() {
                    match reader.read_text_message() {
                        Ok(Some(text)) => match serde_json::from_str::<Value>(&text) {
                            Ok(envelope) => events.publish(envelope),
                            Err(error) => events.publish(stream_error(format!(
                                "DeepSeek Harness sent invalid {name} event JSON: {error}"
                            ))),
                        },
                        Ok(None) => break,
                        Err(_) if control.is_cancelled() => break,
                        Err(_) => break,
                    }
                }
                control.detach(name);
            }
            Err(_) => {
                failures = failures.saturating_add(1);
                if failures == 25 {
                    events.publish(stream_error(format!(
                        "DeepSeek Harness {name} event stream could not reconnect"
                    )));
                }
            }
        }
        if !control.is_cancelled() {
            thread::sleep(STREAM_RETRY_DELAY);
        }
    }
}

fn stream_error(message: String) -> Value {
    json!({
        "type": "server-request",
        "rpcId": format!("waku-stream-{}", Uuid::new_v4()),
        "method": "stream/error",
        "payload": {
            "type": "stream/error",
            "error": {"code": "internal", "message": message, "details": {}}
        }
    })
}

struct WebSocketReader {
    stream: TcpStream,
    fragmented: Vec<u8>,
    fragmented_opcode: Option<u8>,
}

impl WebSocketReader {
    fn connect(port: u16, path: &str) -> anyhow::Result<Self> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let key = base64::engine::general_purpose::STANDARD.encode(Uuid::new_v4().as_bytes());
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        stream.flush()?;

        let mut header = Vec::new();
        let mut byte = [0_u8; 1];
        while !header.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte)?;
            header.push(byte[0]);
            if header.len() > 64 * 1024 {
                bail!("DeepSeek Harness WebSocket response headers are too large");
            }
        }
        let header = String::from_utf8(header).context("invalid WebSocket response headers")?;
        let mut lines = header.lines();
        let status = lines.next().unwrap_or_default();
        if !status.starts_with("HTTP/1.1 101 ") && status != "HTTP/1.1 101" {
            bail!("DeepSeek Harness refused WebSocket upgrade: {status}");
        }
        let has_upgrade = lines.any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("upgrade")
                    && value.trim().eq_ignore_ascii_case("websocket")
            })
        });
        if !has_upgrade {
            bail!("DeepSeek Harness returned no WebSocket upgrade header");
        }
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(Self {
            stream,
            fragmented: Vec::new(),
            fragmented_opcode: None,
        })
    }

    fn read_text_message(&mut self) -> anyhow::Result<Option<String>> {
        loop {
            let Some((fin, opcode, payload)) = self.read_frame()? else {
                return Ok(None);
            };
            match opcode {
                0x0 => {
                    if self.fragmented_opcode.is_none() {
                        bail!("unexpected DeepSeek Harness WebSocket continuation frame");
                    }
                    self.fragmented.extend_from_slice(&payload);
                    if self.fragmented.len() > MAX_WEBSOCKET_MESSAGE_BYTES {
                        bail!("DeepSeek Harness WebSocket message is too large");
                    }
                    if fin {
                        let opcode = self.fragmented_opcode.take().unwrap_or_default();
                        let payload = std::mem::take(&mut self.fragmented);
                        if opcode == 0x1 {
                            return String::from_utf8(payload)
                                .map(Some)
                                .context("DeepSeek Harness sent non-UTF-8 WebSocket text");
                        }
                    }
                }
                0x1 | 0x2 => {
                    if fin {
                        if opcode == 0x1 {
                            return String::from_utf8(payload)
                                .map(Some)
                                .context("DeepSeek Harness sent non-UTF-8 WebSocket text");
                        }
                    } else {
                        self.fragmented_opcode = Some(opcode);
                        self.fragmented = payload;
                    }
                }
                0x8 => return Ok(None),
                0x9 => self.write_control_frame(0xA, &payload)?,
                0xA => {}
                _ => bail!("DeepSeek Harness sent unsupported WebSocket opcode {opcode}"),
            }
        }
    }

    fn read_frame(&mut self) -> anyhow::Result<Option<(bool, u8, Vec<u8>)>> {
        let mut header = [0_u8; 2];
        match self.stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
        if header[0] & 0x70 != 0 {
            bail!("DeepSeek Harness used unsupported WebSocket extensions");
        }
        let fin = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut length = u64::from(header[1] & 0x7f);
        if length == 126 {
            let mut bytes = [0_u8; 2];
            self.stream.read_exact(&mut bytes)?;
            length = u64::from(u16::from_be_bytes(bytes));
        } else if length == 127 {
            let mut bytes = [0_u8; 8];
            self.stream.read_exact(&mut bytes)?;
            length = u64::from_be_bytes(bytes);
        }
        let length =
            usize::try_from(length).context("WebSocket frame length does not fit usize")?;
        if length > MAX_WEBSOCKET_MESSAGE_BYTES {
            bail!("DeepSeek Harness WebSocket frame is too large");
        }
        let mask = if masked {
            let mut mask = [0_u8; 4];
            self.stream.read_exact(&mut mask)?;
            Some(mask)
        } else {
            None
        };
        let mut payload = vec![0_u8; length];
        self.stream.read_exact(&mut payload)?;
        if let Some(mask) = mask {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        Ok(Some((fin, opcode, payload)))
    }

    fn write_control_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        if payload.len() > 125 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "WebSocket control frame is too large",
            ));
        }
        let mask = *Uuid::new_v4().as_bytes().first_chunk::<4>().unwrap();
        self.stream
            .write_all(&[0x80 | opcode, 0x80 | payload.len() as u8])?;
        self.stream.write_all(&mask)?;
        let mut masked = payload.to_vec();
        for (index, byte) in masked.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        self.stream.write_all(&masked)?;
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_accepts_only_the_private_loopback_url() {
        assert_eq!(
            parse_ready_port("dsh web: http://127.0.0.1:59258"),
            Some(59258)
        );
        assert_eq!(parse_ready_port("dsh web: http://localhost:59258"), None);
        assert_eq!(parse_ready_port("listening on 59258"), None);
    }

    #[test]
    fn web_no_open_flag_is_capability_gated() {
        assert!(web_help_supports_no_open(
            b"  --no-open  do not open the Web UI in the default browser\n"
        ));
        assert!(!web_help_supports_no_open(
            b"  --port <port>  listen port\n"
        ));
    }

    #[test]
    fn event_hub_routes_session_frames_and_broadcasts_stream_errors() {
        let hub = EventHub::default();
        let first = hub.subscribe("one");
        let second = hub.subscribe("two");
        hub.publish(json!({"payload": {"type": "session/event", "sessionId": "one"}}));
        assert!(first.try_recv().is_ok());
        assert!(second.try_recv().is_err());

        hub.publish(stream_error("broken".into()));
        assert!(first.try_recv().is_ok());
        assert!(second.try_recv().is_ok());
    }

    /// Exercises Waku's HTTP envelope, WebSocket handshakes, and process-tree
    /// lifecycle against the locally installed Harness without creating a
    /// session or making a model request.
    #[test]
    #[ignore = "requires an installed DeepSeek Harness"]
    fn installed_harness_host_answers_rpc() {
        let binary =
            crate::command_env::find_executable("dsh").expect("DeepSeek Harness is not installed");
        let server = DeepSeekServer::start(&binary).expect("Harness Host should start");
        let listed = server
            .rpc("session.list", json!({}))
            .expect("Harness Host should answer a typed RPC");
        assert!(listed.get("items").and_then(Value::as_array).is_some());
        #[cfg(unix)]
        {
            // Simulate an abruptly terminated Waku process: the OS closes its
            // pipe without giving DeepSeekServer a chance to run Drop.
            drop(server.child.lock().stdin.take());
            let deadline = Instant::now() + Duration::from_secs(5);
            while server.is_alive() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            assert!(
                !server.is_alive(),
                "the Harness guardian should stop after its parent pipe closes"
            );
        }
        server.shutdown(Duration::from_secs(5));
    }

    /// Probes the session payloads used by the custom driver in an isolated
    /// Harness store. Commands are local configuration operations, so this
    /// needs neither credentials nor a model request.
    #[test]
    #[ignore = "requires an installed DeepSeek Harness"]
    fn installed_harness_session_contracts_round_trip() {
        struct TempHarnessHome(std::path::PathBuf);

        impl Drop for TempHarnessHome {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let binary =
            crate::command_env::find_executable("dsh").expect("DeepSeek Harness is not installed");
        let root =
            TempHarnessHome(std::env::temp_dir().join(format!("waku-dsh-test-{}", Uuid::new_v4())));
        std::fs::create_dir_all(&root.0).unwrap();
        {
            let server = DeepSeekServer::start_with_dsh_home(&binary, Some(&root.0))
                .expect("Harness Host should start");
            let session_id = format!("waku-test-{}", Uuid::new_v4());
            let events = server.subscribe(&session_id);
            let created = server
                .rpc(
                    "session.create",
                    json!({"cwd": root.0.to_string_lossy(), "sessionId": session_id}),
                )
                .expect("session.create should accept Waku's payload");
            assert_eq!(
                created.get("sessionId").and_then(Value::as_str),
                Some(session_id.as_str())
            );
            let history = server
                .rpc(
                    "session.history",
                    json!({"sessionId": session_id, "maxMessages": 200}),
                )
                .expect("session.history should accept Waku's payload");
            assert!(history.get("events").and_then(Value::as_array).is_some());
            assert!(
                server
                    .rpc("session.models", json!({"sessionId": session_id}))
                    .expect("session.models should return the session catalog")
                    .get("current")
                    .is_some()
            );
            let commands = server
                .rpc("commands/list", json!({"args": {"agentId": session_id}}))
                .expect("commands/list should return native commands");
            assert!(commands.as_array().is_some_and(|commands| {
                commands
                    .iter()
                    .any(|command| command.get("name").and_then(Value::as_str) == Some("plan"))
            }));
            for command in ["/permission workspace-write", "/plan off"] {
                let response = server
                    .rpc(
                        "commands/execute",
                        json!({"args": {"agentId": session_id, "line": command, "images": []}}),
                    )
                    .expect("native configuration command should succeed");
                assert_eq!(
                    response.pointer("/result/kind").and_then(Value::as_str),
                    Some("success")
                );
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let received_session_frame = loop {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break false;
                };
                let Ok(frame) = events.recv_timeout(remaining) else {
                    break false;
                };
                if frame
                    .pointer("/payload/sessionId")
                    .and_then(Value::as_str)
                    .is_some_and(|received| received == session_id)
                {
                    break true;
                }
            };
            assert!(
                received_session_frame,
                "the event downlink should publish session state"
            );
            server.shutdown(Duration::from_secs(5));
        }
    }
}

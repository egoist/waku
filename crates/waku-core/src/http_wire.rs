//! Minimal blocking HTTP/1.1 and server-sent-event wire shared by the OpenCode
//! providers.
//!
//! Both OpenCode majors are driven over a local HTTP server rather than stdio,
//! and both hit the same two traps, so the wire lives here once instead of
//! being copied per major:
//!
//! * A keep-alive server never EOFs, so response completion is detected at the
//!   protocol's own body boundary (`Content-Length` or the terminating chunk).
//!   Waiting for EOF turns a perfectly valid response into a macOS `EAGAIN`
//!   once the socket timeout elapses.
//! * An event stream's response head must be consumed one byte at a time. A
//!   `BufReader` reads ahead into the first event and loses those bytes when it
//!   is dropped.
//!
//! Everything here blocks, so every caller must already be off the UI thread.

use std::io::{BufRead as _, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serde_json::Value;

/// Where a request goes, and how it authenticates.
///
/// OpenCode v1 serves unauthenticated on loopback; the v2 background service
/// demands HTTP Basic with the password from its registration file. Precompute
/// the header once so it is never rebuilt per request — and never log it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub auth: Option<String>,
}

impl Endpoint {
    /// An unauthenticated loopback server, as OpenCode v1 serves.
    pub(crate) fn local(port: u16) -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port,
            auth: None,
        }
    }

    /// Parses a registration URL and precomputes its Basic credential.
    pub(crate) fn basic(url: &str, user: &str, password: &str) -> anyhow::Result<Self> {
        let parsed = url::Url::parse(url).with_context(|| format!("invalid server URL {url}"))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("server URL {url} has no host"))?
            .to_owned();
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| anyhow!("server URL {url} has no port"))?;
        let credential =
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        Ok(Self {
            host,
            port,
            auth: Some(format!("Basic {credential}")),
        })
    }

    pub(crate) fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Sends one request and returns the decoded JSON body.
///
/// A 204/205/304 — or any empty success body — answers `Value::Null`, matching
/// what routes like OpenCode v1's `prompt_async` return.
pub(crate) fn request_json(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let body = body.map(serde_json::to_vec).transpose()?;
    let response = http_request(endpoint, method, path, body.as_deref(), timeout)?;
    if response.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&response)
        .with_context(|| format!("OpenCode returned invalid JSON for {method} {path}"))
}

pub(crate) fn http_request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    let address = endpoint.address();
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .with_context(|| format!("could not connect to OpenCode on {address}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let body = body.unwrap_or_default();
    let authorization = endpoint
        .auth
        .as_deref()
        .map(|auth| format!("Authorization: {auth}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAccept: application/json\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        // HTTP/1.1 connections are allowed to stay alive after a complete
        // response, even when the client asks to close. Waiting for EOF made a
        // valid OpenCode response end as macOS EAGAIN once the socket timeout
        // elapsed. Stop at the protocol's own body boundary instead.
        if http_response_is_complete(&response)? {
            break;
        }
        let read = stream
            .read(&mut buffer)
            .with_context(|| format!("failed reading OpenCode response for {method} {path}"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
    }
    parse_http_response(&response)
}

fn http_response_is_complete(response: &[u8]) -> anyhow::Result<bool> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(false);
    };
    let headers = std::str::from_utf8(&response[..header_end])?;
    let body = &response[header_end + 4..];
    if header_value(headers, "transfer-encoding").is_some_and(|value| {
        value
            .split(',')
            .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
    }) {
        return chunked_body_is_complete(body);
    }
    if let Some(length) = header_value(headers, "content-length") {
        let length = length
            .trim()
            .parse::<usize>()
            .context("OpenCode returned an invalid HTTP content length")?;
        return Ok(body.len() >= length);
    }

    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok());
    Ok(status.is_some_and(|status| matches!(status, 204 | 205 | 304)))
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn chunked_body_is_complete(mut input: &[u8]) -> anyhow::Result<bool> {
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            return Ok(false);
        };
        let size_text = std::str::from_utf8(&input[..line_end])?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .context("OpenCode returned an invalid HTTP chunk size")?;
        input = &input[line_end + 2..];
        if size == 0 {
            return Ok(true);
        }
        if input.len() < size + 2 {
            return Ok(false);
        }
        if &input[size..size + 2] != b"\r\n" {
            bail!("OpenCode returned an invalid chunked response");
        }
        input = &input[size + 2..];
    }
}

fn parse_http_response(response: &[u8]) -> anyhow::Result<Vec<u8>> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        bail!("OpenCode returned an invalid HTTP response");
    };
    let headers = std::str::from_utf8(&response[..header_end])?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("OpenCode returned an invalid HTTP status"))?;
    let body = &response[header_end + 4..];
    let body = if headers.lines().any(|line| {
        line.eq_ignore_ascii_case("transfer-encoding: chunked")
            || line
                .to_ascii_lowercase()
                .starts_with("transfer-encoding: chunked")
    }) {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };
    if !(200..300).contains(&status) {
        let detail = String::from_utf8_lossy(&body);
        bail!("OpenCode session request failed with HTTP {status}: {detail}");
    }
    Ok(body)
}

fn decode_chunked(mut input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            bail!("OpenCode returned an invalid chunked response");
        };
        let size_text = std::str::from_utf8(&input[..line_end])?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .context("OpenCode returned an invalid HTTP chunk size")?;
        input = &input[line_end + 2..];
        if size == 0 {
            break;
        }
        if input.len() < size + 2 || &input[size..size + 2] != b"\r\n" {
            bail!("OpenCode returned a truncated HTTP chunk");
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
    Ok(output)
}

/// Shuts an open event stream and wakes whoever is reading it.
///
/// The socket handle lives here rather than with the reader so a drop during
/// response setup still cancels: see [`open_event_stream`].
#[derive(Default)]
pub(crate) struct StreamControl {
    cancelled: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
    /// Woken alongside the socket shutdown, so one cancel can release a
    /// channel-based consumer that is not itself blocked on the socket.
    waker: Mutex<Option<Sender<()>>>,
}

impl StreamControl {
    pub(crate) fn attach(&self, stream: &TcpStream) -> std::io::Result<bool> {
        let socket = stream.try_clone()?;
        let mut active = self.socket.lock();
        if self.cancelled.load(Ordering::Acquire) {
            let _ = socket.shutdown(Shutdown::Both);
            return Ok(false);
        }
        *active = Some(socket);
        Ok(true)
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(socket) = self.socket.lock().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(waker) = self.waker.lock().as_ref() {
            let _ = waker.try_send(());
        }
    }

    pub(crate) fn clear(&self) {
        self.socket.lock().take();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Lets a long-lived reader be restarted after a cancel-driven park.
    pub(crate) fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
        self.socket.lock().take();
    }

    pub(crate) fn set_waker(&self, waker: Option<Sender<()>>) {
        *self.waker.lock() = waker;
    }
}

/// Opens the server-sent event stream and leaves it open.
///
/// The shared request helper reads a whole response before returning, which a
/// stream never finishes doing.
pub(crate) fn open_event_stream(
    endpoint: &Endpoint,
    path: &str,
    control: &StreamControl,
) -> anyhow::Result<Option<TcpStream>> {
    let address = endpoint.address();
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .with_context(|| format!("could not connect to OpenCode on {address}"))?;
    // Register before reading the response head too. If this driver is dropped
    // while setup is blocked, cancellation can still close the socket and wake
    // the reader even though another pooled session keeps the server alive.
    if !control.attach(&stream)? {
        return Ok(None);
    }
    // Closing a cloned socket does not reliably wake a blocking read on every
    // Windows TCP stack. Poll during response setup so cancellation has a
    // platform-independent upper bound even when the server never replies.
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    let authorization = endpoint
        .auth
        .as_deref()
        .map(|auth| format!("Authorization: {auth}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAccept: text/event-stream\r\n{authorization}Connection: keep-alive\r\n\r\n"
    )?;
    stream.flush()?;
    // Consume exactly the response head. A BufReader could read ahead into the
    // first event and lose those buffered bytes when it is dropped here.
    let mut response_head = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err(anyhow!("OpenCode closed the event stream during setup")),
            Ok(_) => {
                response_head.push(byte[0]);
                if response_head.ends_with(b"\r\n\r\n") || response_head.ends_with(b"\n\n") {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if control.is_cancelled() {
                    return Ok(None);
                }
            }
            Err(error) => {
                if control.is_cancelled() {
                    return Ok(None);
                }
                return Err(error.into());
            }
        }
    }
    // An authenticated server answers 401 with a perfectly well-formed head and
    // then simply says nothing. Without this check that is indistinguishable
    // from a healthy stream that happens to be idle, and the reader blocks
    // until its read timeout instead of reporting the real failure.
    let head = String::from_utf8_lossy(&response_head);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("OpenCode returned an invalid HTTP status"))?;
    if !(200..300).contains(&status) {
        bail!("OpenCode refused the event stream with HTTP {status}");
    }
    stream.set_read_timeout(None)?;
    Ok(Some(stream))
}

/// One parsed server-sent-event frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SseFrame {
    /// A frame with only `data:` lines, which is every ordinary event.
    Data(String),
    /// A frame carrying an `event:` field. OpenCode 2 uses this solely to
    /// report that the stream itself failed
    /// (`event: effect/httpapi/stream/failure`), so a reader that ignores the
    /// field sits blind on a dead stream until its read timeout fires.
    Named { event: String, data: String },
    /// A `:`-prefixed comment. The v2 service heartbeats with `: heartbeat`.
    Comment,
}

/// Reads SSE frames until the stream dies.
///
/// `read_timeout` bounds a silent stream; pass `None` to block indefinitely.
pub(crate) fn read_sse_frames(
    stream: TcpStream,
    read_timeout: Option<Duration>,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<SseFrame>>> {
    stream.set_read_timeout(read_timeout)?;
    let mut reader = std::io::BufReader::new(stream);
    let mut event = String::new();
    let mut data = String::new();
    let mut sawcomment = false;
    let mut finished = false;

    Ok(std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    finished = true;
                    return None;
                }
                Ok(_) => {}
                Err(error) => {
                    finished = true;
                    return Some(Err(error.into()));
                }
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                // Blank line dispatches the frame accumulated so far.
                if !data.is_empty() || !event.is_empty() {
                    let payload = std::mem::take(&mut data);
                    let name = std::mem::take(&mut event);
                    sawcomment = false;
                    return Some(Ok(if name.is_empty() {
                        SseFrame::Data(payload)
                    } else {
                        SseFrame::Named {
                            event: name,
                            data: payload,
                        }
                    }));
                }
                if std::mem::take(&mut sawcomment) {
                    return Some(Ok(SseFrame::Comment));
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix(':') {
                let _ = rest;
                sawcomment = true;
                continue;
            }
            let (field, value) = match trimmed.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (trimmed, ""),
            };
            match field {
                // Multiple `data:` lines in one frame are joined with newlines.
                "data" => {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
                "event" => {
                    event.clear();
                    event.push_str(value);
                }
                _ => {}
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(input: &str) -> Vec<SseFrame> {
        // Exercise the parser over a real socket so framing, not a string
        // helper, is what is under test.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let payload = input.to_owned();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let _ = socket.write_all(payload.as_bytes());
        });
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        read_sse_frames(stream, Some(Duration::from_secs(5)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn heartbeat_comments_are_reported_separately_from_data() {
        assert_eq!(
            frames(": heartbeat\n\ndata: {\"a\":1}\n\n"),
            vec![SseFrame::Comment, SseFrame::Data("{\"a\":1}".to_owned())]
        );
    }

    #[test]
    fn multiline_data_fields_are_joined() {
        assert_eq!(
            frames("data: {\"a\":\ndata: 1}\n\n"),
            vec![SseFrame::Data("{\"a\":\n1}".to_owned())]
        );
    }

    /// `GET /api/event` reports a server-side stream failure as a NAMED event
    /// whose data is an Effect cause. Dropping the name loses the only signal
    /// that the stream is dead.
    #[test]
    fn named_events_preserve_their_event_field() {
        assert_eq!(
            frames("event: effect/httpapi/stream/failure\ndata: {\"_tag\":\"Fail\"}\n\n"),
            vec![SseFrame::Named {
                event: "effect/httpapi/stream/failure".to_owned(),
                data: "{\"_tag\":\"Fail\"}".to_owned(),
            }]
        );
    }

    #[test]
    fn cancelling_event_stream_unblocks_response_setup() {
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::mpsc;
        use std::thread;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let control = Arc::new(StreamControl::default());
        let reader_control = Arc::clone(&control);
        let (done, finished) = mpsc::channel();
        let reader = thread::spawn(move || {
            let _ = open_event_stream(&Endpoint::local(port), "/event", &reader_control);
            done.send(()).unwrap();
        });
        let (_peer, _) = listener.accept().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while control.socket.lock().is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(control.socket.lock().is_some());

        control.cancel();
        finished
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation should unblock the response-head read");
        reader.join().unwrap();
        assert!(control.is_cancelled());
        assert!(control.socket.lock().is_none());
    }

    #[test]
    fn parses_content_length_and_chunked_http_responses() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(),
            b"{}"
        );
        assert_eq!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"id\r\n4\r\n\":1}\r\n0\r\n\r\n"
            )
            .unwrap(),
            b"{\"id\":1}"
        );
    }

    #[test]
    fn detects_complete_http_bodies_without_waiting_for_connection_close() {
        assert!(
            !http_response_is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{").unwrap()
        );
        assert!(
            http_response_is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap()
        );
        assert!(
            !http_response_is_complete(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{}"
            )
            .unwrap()
        );
        assert!(
            http_response_is_complete(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"
            )
            .unwrap()
        );
        assert!(
            http_response_is_complete(b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n")
                .unwrap()
        );
    }

    #[test]
    fn basic_endpoint_precomputes_its_credential() {
        let endpoint = Endpoint::basic("http://127.0.0.1:49374", "opencode", "secret").unwrap();
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 49374);
        assert_eq!(endpoint.address(), "127.0.0.1:49374");
        // base64("opencode:secret")
        assert_eq!(endpoint.auth.as_deref(), Some("Basic b3BlbmNvZGU6c2VjcmV0"));
    }

    #[test]
    fn local_endpoint_is_unauthenticated_loopback() {
        let endpoint = Endpoint::local(1234);
        assert_eq!(endpoint.address(), "127.0.0.1:1234");
        assert!(endpoint.auth.is_none());
    }
}

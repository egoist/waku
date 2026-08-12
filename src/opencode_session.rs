use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};

use crate::model::ProviderResumeCursor;

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Forking copies every retained message and part into a new native session.
/// A long task can legitimately take longer than the ordinary request budget;
/// this operation already runs off the UI thread.
const FORK_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// The server binds its port about a second before the app behind it starts
/// answering, and a request accepted in that window is never answered at all.
/// A startup probe caught there must give up quickly and retry — at the full
/// `HTTP_TIMEOUT` one hung probe would eat the whole start budget.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

pub fn fork_session_at_turn(
    binary: &Path,
    cwd: &Path,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let server = OpenCodeServer::start(binary, cwd)?;
    fork_session_at_turn_on_server(&server, session_id, retained_turns)
}

/// Forks through the task's resident OpenCode server.
///
/// Starting a second `opencode serve` against the same workspace can contend
/// with the live process for OpenCode's local resources. Rewinds with a live
/// driver use this path instead, while cold sessions still use the standalone
/// helper above.
pub(crate) fn fork_session_at_turn_on_server(
    server: &OpenCodeServer,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let message_ids = native_user_message_ids(server, session_id)?;
    fork_session_with_message_ids(server, session_id, &message_ids, retained_turns)
}

pub(crate) fn fork_session_removing_turns_on_server(
    server: &OpenCodeServer,
    session_id: &str,
    turns_to_remove: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let message_ids = native_user_message_ids(server, session_id)?;
    let retained_turns = retained_turn_count(message_ids.len(), turns_to_remove)?;
    fork_session_with_message_ids(server, session_id, &message_ids, retained_turns)
}

fn retained_turn_count(total_turns: usize, turns_to_remove: usize) -> anyhow::Result<usize> {
    total_turns.checked_sub(turns_to_remove).ok_or_else(|| {
        anyhow!(
            "OpenCode has only {total_turns} native turns, but Waku needs to remove {turns_to_remove}"
        )
    })
}

fn native_user_message_ids(
    server: &OpenCodeServer,
    session_id: &str,
) -> anyhow::Result<Vec<String>> {
    let session_path = format!("/session/{}/message", encode_path_segment(session_id));
    let messages = server.request_with_timeout("GET", &session_path, None, FORK_HTTP_TIMEOUT)?;
    Ok(messages
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode returned an invalid message list"))?
        .iter()
        .filter_map(|message| {
            (is_native_user_turn(message))
                .then(|| message.pointer("/info/id").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .collect())
}

fn fork_session_with_message_ids(
    server: &OpenCodeServer,
    session_id: &str,
    message_ids: &[String],
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let fork_at = fork_message_id(&message_ids, retained_turns)?;
    let body = fork_at.map_or_else(|| json!({}), |message_id| json!({"messageID": message_id}));
    let fork_path = format!("/session/{}/fork", encode_path_segment(session_id));
    let fork = server.request_with_timeout("POST", &fork_path, Some(&body), FORK_HTTP_TIMEOUT)?;
    let fork_id = fork
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| fork.pointer("/data/id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("OpenCode returned no forked session ID"))?;
    Ok(ProviderResumeCursor::OpenCode {
        session_id: fork_id.to_owned(),
    })
}

fn fork_message_id(message_ids: &[String], retained_turns: usize) -> anyhow::Result<Option<&str>> {
    if retained_turns > message_ids.len() {
        bail!(
            "OpenCode has only {} native turns, but Waku needs {retained_turns}",
            message_ids.len()
        );
    }
    Ok(message_ids.get(retained_turns).map(String::as_str))
}

pub(crate) struct OpenCodeServer {
    child: Child,
    pub(crate) port: u16,
    pid: u32,
}

impl OpenCodeServer {
    pub(crate) fn start(binary: &Path, cwd: &Path) -> anyhow::Result<Self> {
        Self::start_with_env(binary, cwd, &[])
    }

    /// Starts the server with extra environment, so a caller can hand it the
    /// Computer Use configuration the same way a one-shot invocation got it.
    pub(crate) fn start_with_env(
        binary: &Path,
        cwd: &Path,
        environment: &[(String, String)],
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("could not reserve a local port for OpenCode")?;
        let port = listener.local_addr()?.port();
        drop(listener);

        let mut command = crate::command_env::command(binary);
        for (name, value) in environment {
            command.env(name, value);
        }
        let command = command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("OPENCODE_SERVER_PASSWORD", "")
            .env("OPENCODE_SERVER_USERNAME", "opencode")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child =
            crate::command_env::spawn(command).context("failed to start `opencode serve`")?;
        let pid = child.id();
        let mut server = Self { child, port, pid };
        let started_at = Instant::now();
        loop {
            if server
                .request_with_timeout("GET", "/global/health", None, HEALTH_PROBE_TIMEOUT)
                .is_ok()
            {
                return Ok(server);
            }
            if let Some(status) = server.child.try_wait()? {
                bail!("OpenCode session server exited during startup ({status})");
            }
            if started_at.elapsed() >= SERVER_START_TIMEOUT {
                bail!("timed out starting the OpenCode session server");
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        self.request_with_timeout(method, path, body, HTTP_TIMEOUT)
    }

    pub(crate) fn request_with_timeout(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let response = http_request(self.port, method, path, body.as_deref(), timeout)?;
        // Some routes answer 204 No Content — `prompt_async` among them — and
        // the status was already checked, so an empty success body is Null.
        if response.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&response)
            .with_context(|| format!("OpenCode returned invalid JSON for {method} {path}"))
    }
}

fn is_native_user_turn(message: &Value) -> bool {
    message.pointer("/info/role").and_then(Value::as_str) == Some("user")
        && message
            .get("parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("text")
                        && part.get("synthetic").and_then(Value::as_bool) != Some(true)
                })
            })
}

impl OpenCodeServer {
    /// Ends the server without owning it mutably.
    ///
    /// A long-lived reader blocked on the event stream keeps a handle alive, and
    /// that stream only closes when the server exits — so waiting for the last
    /// handle to drop deadlocks and leaks the process. The owner kills it
    /// directly instead, which closes the stream and releases the readers.
    pub(crate) fn shutdown(&self) {
        if self.pid == 0 {
            return;
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-TERM", &self.pid.to_string()])
                .status();
        }
    }
}

impl Drop for OpenCodeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("could not connect to OpenCode on local port {port}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let body = body.unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_fork_message_excludes_the_next_user_turn() {
        let messages = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
        assert_eq!(fork_message_id(&messages, 0).unwrap(), Some("one"));
        assert_eq!(fork_message_id(&messages, 2).unwrap(), Some("three"));
        assert_eq!(fork_message_id(&messages, 3).unwrap(), None);
        assert!(fork_message_id(&messages, 4).is_err());
    }

    #[test]
    fn rollback_count_is_converted_to_the_retained_native_prefix() {
        assert_eq!(retained_turn_count(4, 1).unwrap(), 3);
        assert_eq!(retained_turn_count(4, 4).unwrap(), 0);
        assert!(retained_turn_count(4, 5).is_err());
    }

    #[test]
    fn native_turn_filter_ignores_compaction_and_synthetic_user_messages() {
        assert!(is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "hello"}]
        })));
        assert!(!is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "compaction", "auto": true}]
        })));
        assert!(!is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "continue", "synthetic": true}]
        })));
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

    /// Exercises the same cold-session path used when an edited message is
    /// submitted after Waku has relaunched. The source session is supplied by
    /// the caller so this never creates provider traffic; it only forks the
    /// already-completed native transcript and removes the test fork again.
    #[test]
    #[ignore = "requires an installed opencode and WAKU_OPENCODE_TEST_SESSION_ID"]
    fn forks_away_a_real_single_turn_session() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let session_id = std::env::var("WAKU_OPENCODE_TEST_SESSION_ID")
            .expect("set WAKU_OPENCODE_TEST_SESSION_ID to a completed one-turn session");
        let cwd = std::env::current_dir().expect("the test working directory should exist");
        let server = OpenCodeServer::start(&binary, &cwd).expect("the server should start");
        let ProviderResumeCursor::OpenCode {
            session_id: fork_id,
        } = fork_session_at_turn_on_server(&server, &session_id, 0)
            .expect("the first turn should be excluded from the fork")
        else {
            panic!("expected an OpenCode cursor");
        };
        let messages = server
            .request(
                "GET",
                &format!("/session/{}/message", encode_path_segment(&fork_id)),
                None,
            )
            .expect("the fork should be readable");
        assert_eq!(messages.as_array().map(Vec::len), Some(0));
        server
            .request(
                "DELETE",
                &format!("/session/{}", encode_path_segment(&fork_id)),
                None,
            )
            .expect("the test fork should be removed");
    }
}

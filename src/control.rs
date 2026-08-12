//! Local app-control channel between the `waku` CLI and a running desktop app.
//!
//! The socket lives next to `app.db`. One JSON request/response per connection
//! (newline-terminated), so agents and shell scripts can drive session/project
//! management without touching the GUI.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::ProviderKind;
use crate::persistence::StateStore;

/// Filename of the control socket, sibling to `app.db`.
pub const CONTROL_SOCKET_NAME: &str = "control.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    ListProjects,
    NewProject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        path: PathBuf,
    },
    ListSessions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    NewSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    Open {
        target: String,
    },
    LinkSession {
        session: String,
        project: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ControlResponse {
    pub fn ok_data(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            error: None,
            data: Some(data),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            data: None,
        }
    }
}

/// Pending request delivered from the socket thread into the UI event pump.
pub struct PendingControl {
    pub request: ControlRequest,
    pub reply: mpsc::Sender<ControlResponse>,
}

pub fn socket_path_for_db(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(CONTROL_SOCKET_NAME)
}

pub fn default_socket_path() -> PathBuf {
    socket_path_for_db(&StateStore::default_path())
}

pub fn parse_provider(name: &str) -> Result<ProviderKind, String> {
    let normalized = name.trim().to_ascii_lowercase();
    ProviderKind::ALL
        .into_iter()
        .find(|provider| provider.id() == normalized)
        .ok_or_else(|| {
            let supported = ProviderKind::ALL
                .iter()
                .map(|provider| provider.id())
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown provider `{name}`; expected one of: {supported}")
        })
}

/// Try to reach a running app. Returns `None` when nothing is listening.
pub fn try_request(request: &ControlRequest) -> Option<ControlResponse> {
    let path = default_socket_path();
    let mut stream = UnixStream::connect(&path).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let payload = match serde_json::to_string(request) {
        Ok(payload) => payload,
        Err(error) => {
            return Some(ControlResponse::err(format!("encode request: {error}")));
        }
    };
    if let Err(error) = writeln!(stream, "{payload}") {
        return Some(ControlResponse::err(format!("write request: {error}")));
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if let Err(error) = reader.read_line(&mut line) {
        return Some(ControlResponse::err(format!("read response: {error}")));
    }
    match serde_json::from_str::<ControlResponse>(line.trim()) {
        Ok(response) => Some(response),
        Err(error) => Some(ControlResponse::err(format!("decode response: {error}"))),
    }
}

/// Bind `control.sock` and forward each request to `tx`.
///
/// Stale sockets left by a crashed app are removed on bind failure.
pub fn spawn_server(
    db_path: PathBuf,
    tx: crossbeam_channel::Sender<PendingControl>,
    event_wake: smol::channel::Sender<()>,
) {
    thread::Builder::new()
        .name("waku-control".into())
        .spawn(move || {
            let path = socket_path_for_db(&db_path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(&path);
            let listener = match UnixListener::bind(&path) {
                Ok(listener) => listener,
                Err(error) => {
                    eprintln!("waku control: bind {}: {error}", path.display());
                    return;
                }
            };
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let tx = tx.clone();
                let event_wake = event_wake.clone();
                thread::spawn(move || handle_connection(stream, tx, event_wake));
            }
        })
        .expect("spawn waku control server");
}

fn handle_connection(
    stream: UnixStream,
    tx: crossbeam_channel::Sender<PendingControl>,
    event_wake: smol::channel::Sender<()>,
) {
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let request = match serde_json::from_str::<ControlRequest>(line.trim()) {
        Ok(request) => request,
        Err(error) => {
            let _ = write_response(&stream, &ControlResponse::err(format!("bad request: {error}")));
            return;
        }
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(PendingControl {
            request,
            reply: reply_tx,
        })
        .is_err()
    {
        let _ = write_response(&stream, &ControlResponse::err("app is shutting down"));
        return;
    }
    let _ = event_wake.try_send(());
    let response = reply_rx
        .recv_timeout(Duration::from_secs(15))
        .unwrap_or_else(|_| ControlResponse::err("timed out waiting for the app"));
    let _ = write_response(&stream, &response);
}

fn write_response(mut stream: &UnixStream, response: &ControlResponse) -> std::io::Result<()> {
    let payload = serde_json::to_string(response)
        .unwrap_or_else(|error| format!(r#"{{"ok":false,"error":"{error}"}}"#));
    writeln!(stream, "{payload}")
}

pub fn parse_uuid(value: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value.trim()).map_err(|_| format!("invalid id `{value}`"))
}

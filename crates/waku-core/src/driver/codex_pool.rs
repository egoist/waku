//! Shared Codex app-server processes.
//!
//! Codex keeps thread state inside the app-server process and exposes a
//! Unix-socket transport with one JSON-RPC connection per client.  Pooling the
//! process while keeping a connection per Waku driver lets conversations share
//! the expensive app-server without sharing their thread state or event loop.

#![cfg(unix)]

use std::collections::HashMap;
use std::ops::Deref;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use tungstenite::client::{IntoClientRequest as _, client};
use tungstenite::protocol::WebSocket;
use uuid::Uuid;

use super::codex::{CodexComputerUseConfig, configure_computer_use_command};

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(15);
const SERVER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type CodexSocket = WebSocket<UnixStream>;

pub(crate) struct CodexServer {
    child: Arc<Mutex<Child>>,
    socket_path: PathBuf,
}

impl CodexServer {
    pub(crate) fn start(
        binary: &Path,
        cwd: &Path,
        computer_use: Option<&CodexComputerUseConfig>,
    ) -> anyhow::Result<Self> {
        // macOS limits Unix socket paths to SUN_LEN. TMPDIR may itself be a
        // long per-process path, so keep the rendezvous path deliberately
        // short while retaining a private directory per server.
        let socket_directory = PathBuf::from("/tmp").join(format!("w-{}", Uuid::new_v4()));
        std::fs::create_dir(&socket_directory)
            .context("failed to create private Codex socket directory")?;
        let socket_path = socket_directory.join("s");
        let mut command = crate::command_env::command(binary);
        command
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", socket_path.display()))
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        configure_computer_use_command(&mut command, computer_use);
        let mut child = match crate::command_env::spawn(&mut command) {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_dir(&socket_directory);
                return Err(error).context("failed to start `codex app-server`");
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_child(&mut child);
                let _ = std::fs::remove_dir(&socket_directory);
                bail!("Codex app-server provided no diagnostic output")
            }
        };
        let diagnostics = Arc::new(Mutex::new(Vec::<String>::new()));
        let diagnostics_for_reader = Arc::clone(&diagnostics);
        thread::Builder::new()
            .name("waku-codex-server-stderr".into())
            .spawn(move || {
                use std::io::BufRead as _;
                // Drain stderr so a noisy shared server can never block on a
                // full pipe, while retaining enough diagnostics for startup
                // failures to be actionable.
                for line in std::io::BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                {
                    let mut diagnostics = diagnostics_for_reader.lock().unwrap();
                    diagnostics.push(line);
                    if diagnostics.len() > 8 {
                        diagnostics.remove(0);
                    }
                }
            })
            .context("could not start Codex stderr reader")?;

        let started_at = Instant::now();
        loop {
            if socket_path.exists() {
                match child.try_wait()? {
                    Some(status) => {
                        let diagnostics = diagnostics.lock().unwrap().join(" | ");
                        bail!(
                            "Codex app-server exited during startup ({status}){}",
                            (!diagnostics.is_empty())
                                .then(|| format!(": {diagnostics}"))
                                .unwrap_or_default()
                        )
                    }
                    None => {
                        let child = Arc::new(Mutex::new(child));
                        let server = Self { child, socket_path };
                        // Make startup fail here rather than on the first
                        // conversation if the listener is not accepting yet.
                        let _ = server.connect()?;
                        return Ok(server);
                    }
                }
            }
            if let Some(status) = child.try_wait()? {
                let diagnostics = diagnostics.lock().unwrap().join(" | ");
                bail!(
                    "Codex app-server exited during startup ({status}){}",
                    (!diagnostics.is_empty())
                        .then(|| format!(": {diagnostics}"))
                        .unwrap_or_default()
                )
            }
            if started_at.elapsed() >= SERVER_START_TIMEOUT {
                terminate_child(&mut child);
                bail!("timed out starting Codex app-server Unix socket")
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub(crate) fn connect(&self) -> anyhow::Result<CodexSocket> {
        let started_at = Instant::now();
        loop {
            match UnixStream::connect(&self.socket_path) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
                    let request = "ws://localhost/".into_client_request()?;
                    let (socket, _) = client(request, stream)
                        .context("could not connect to Codex app-server Unix socket")?;
                    return Ok(socket);
                }
                Err(error) if started_at.elapsed() < SERVER_START_TIMEOUT => {
                    if self.child.lock().unwrap().try_wait()?.is_some() {
                        return Err(anyhow!("Codex app-server exited before accepting clients"));
                    }
                    let _ = error;
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn shutdown(&self) {
        let mut child = self.child.lock().unwrap();
        terminate_child(&mut child);
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(directory) = self.socket_path.parent() {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

fn terminate_child(child: &mut Child) {
    if child.try_wait().is_ok_and(|status| status.is_some()) {
        return;
    }
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM);
    }
    let deadline = Instant::now() + SERVER_EXIT_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Clone)]
pub(crate) struct PooledCodexServer {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    server: CodexServer,
    slot: Option<Weak<PoolSlot>>,
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.as_ref().and_then(Weak::upgrade) {
            let mut state = slot.state.lock().unwrap();
            let current = matches!(&*state, PoolState::Running(server) if std::ptr::eq(server.as_ptr(), self));
            if current {
                *state = PoolState::Stopping;
                self.server.shutdown();
                *state = PoolState::Vacant;
                slot.changed.notify_all();
                return;
            }
        }
        self.server.shutdown();
    }
}

impl Deref for PooledCodexServer {
    type Target = CodexServer;

    fn deref(&self) -> &Self::Target {
        &self.inner.server
    }
}

type PoolKey = (PathBuf, PathBuf);

enum PoolState {
    Vacant,
    Starting,
    Running(Weak<PoolInner>),
    Stopping,
}

struct PoolSlot {
    state: Mutex<PoolState>,
    changed: Condvar,
}

impl Default for PoolSlot {
    fn default() -> Self {
        Self {
            state: Mutex::new(PoolState::Vacant),
            changed: Condvar::new(),
        }
    }
}

fn pool() -> &'static Mutex<HashMap<PoolKey, Arc<PoolSlot>>> {
    static POOL: OnceLock<Mutex<HashMap<PoolKey, Arc<PoolSlot>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn acquire(
    binary: &Path,
    cwd: &Path,
    computer_use: Option<&CodexComputerUseConfig>,
) -> anyhow::Result<PooledCodexServer> {
    // Computer Use currently bakes a process directory and MCP settings into
    // the server command. Keep those sessions dedicated until that config is
    // made explicitly shareable.
    if computer_use.is_some() {
        return Ok(PooledCodexServer {
            inner: Arc::new(PoolInner {
                server: CodexServer::start(binary, cwd, computer_use)?,
                slot: None,
            }),
        });
    }

    let key = (binary.to_path_buf(), cwd.to_path_buf());
    let slot = {
        let mut pools = pool().lock().unwrap();
        Arc::clone(pools.entry(key).or_default())
    };
    let superseded = loop {
        let mut state = slot.state.lock().unwrap();
        match &*state {
            PoolState::Running(server) => {
                if let Some(inner) = server.upgrade() {
                    if inner.server.child.lock().unwrap().try_wait()?.is_none() {
                        return Ok(PooledCodexServer { inner });
                    }
                    *state = PoolState::Starting;
                    break Some(inner);
                }
                state = slot.changed.wait(state).unwrap();
                drop(state);
            }
            PoolState::Vacant => {
                *state = PoolState::Starting;
                break None;
            }
            PoolState::Starting | PoolState::Stopping => {
                state = slot.changed.wait(state).unwrap();
                drop(state);
            }
        }
    };
    drop(superseded);

    let started = CodexServer::start(binary, cwd, None);
    let mut state = slot.state.lock().unwrap();
    match started {
        Ok(server) => {
            let inner = Arc::new(PoolInner {
                server,
                slot: Some(Arc::downgrade(&slot)),
            });
            *state = PoolState::Running(Arc::downgrade(&inner));
            slot.changed.notify_all();
            Ok(PooledCodexServer { inner })
        }
        Err(error) => {
            *state = PoolState::Vacant;
            slot.changed.notify_all();
            Err(error)
        }
    }
}

//! Per-workspace pool of resident `opencode serve` processes.
//!
//! OpenCode is one long-lived process that hosts many sessions, so every
//! session in a workspace shares a single server instead of starting its own
//! — one config load, one plugin/MCP stack, and no contention between two
//! `opencode serve` instances in the same directory. The server is killed
//! when the last session using it drops.
//!
//! Handles are counted so the kill is deterministic: reader threads must hold
//! only the server's port, never a handle, because the event stream only
//! closes when the process exits — a handle held there would keep the last
//! drop from ever killing the server, deadlocking the reader against itself.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use crate::opencode_session::OpenCodeServer;

/// How long the last drop waits for the server to actually exit, so a
/// session starting right after it cannot adopt a dying process.
const SERVER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A reference to a server process, shared or dedicated.
///
/// Dropping the last handle kills the server. Clones add handles.
pub(crate) struct PooledServer {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    server: OpenCodeServer,
    handles: AtomicUsize,
}

impl Clone for PooledServer {
    fn clone(&self) -> Self {
        self.inner.handles.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for PooledServer {
    fn drop(&mut self) {
        if self.inner.handles.fetch_sub(1, Ordering::Relaxed) == 1 {
            // Last session in this workspace: kill the server so the
            // per-session event-stream readers unblock and exit.
            self.inner.server.shutdown();
            let deadline = Instant::now() + SERVER_EXIT_TIMEOUT;
            while Instant::now() < deadline && self.inner.server.is_alive() {
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

impl Deref for PooledServer {
    type Target = OpenCodeServer;

    fn deref(&self) -> &Self::Target {
        &self.inner.server
    }
}

impl PooledServer {
    /// Wraps a server a session started for itself. Computer Use bakes
    /// per-session configuration into the server environment, so those
    /// sessions cannot share a workspace server and keep this path.
    pub(crate) fn dedicated(server: OpenCodeServer) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                server,
                handles: AtomicUsize::new(1),
            }),
        }
    }
}

type PoolKey = (PathBuf, PathBuf); // (binary, workspace directory)

fn pool() -> &'static Mutex<HashMap<PoolKey, Weak<PoolInner>>> {
    static POOL: OnceLock<Mutex<HashMap<PoolKey, Weak<PoolInner>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the workspace's resident server, starting one if none is alive.
///
/// Blocking (process start plus health probe), so callers must already be off
/// the UI thread — driver start on the background executor is.
pub(crate) fn acquire(binary: &Path, cwd: &Path) -> anyhow::Result<PooledServer> {
    let key = (binary.to_path_buf(), cwd.to_path_buf());
    if let Some(server) = lookup(&key) {
        return Ok(server);
    }
    let server = OpenCodeServer::start(binary, cwd)?;
    let inner = Arc::new(PoolInner {
        server,
        handles: AtomicUsize::new(1),
    });
    let mut pool = pool().lock().unwrap();
    if let Some(existing) = pool.get(&key).and_then(Weak::upgrade) {
        // Another session won the creation race while this server was
        // starting. Adopt the winner and retire the duplicate.
        inner.server.shutdown();
        existing.handles.fetch_add(1, Ordering::Relaxed);
        return Ok(PooledServer { inner: existing });
    }
    pool.insert(key, Arc::downgrade(&inner));
    Ok(PooledServer { inner })
}

fn lookup(key: &PoolKey) -> Option<PooledServer> {
    let mut pool = pool().lock().unwrap();
    let inner = pool.get(key)?.upgrade()?;
    if inner.server.is_alive() {
        inner.handles.fetch_add(1, Ordering::Relaxed);
        Some(PooledServer { inner })
    } else {
        pool.remove(key);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    fn port_is_open(port: u16) -> bool {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    }

    /// Proves the pool's contract against a real server: two sessions in one
    /// workspace share a single process, the server outlives the first
    /// session, the last session's drop kills it, and a later session starts
    /// a fresh one. Ignored by default: needs the CLI installed. Run with
    /// `cargo test --bin waku opencode_pool -- --ignored`.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn workspace_sessions_share_one_server_until_the_last_drops() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let cwd = std::env::temp_dir();

        let first = acquire(&binary, &cwd).expect("the first session should start the server");
        let port = first.port;
        assert!(port_is_open(port), "the server should be listening");

        let second = acquire(&binary, &cwd).expect("the second session should reuse it");
        assert_eq!(second.port, port, "both sessions must share one process");

        drop(first);
        assert!(
            second.is_alive(),
            "the server must outlive the first session"
        );

        drop(second);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && port_is_open(port) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !port_is_open(port),
            "the last session's drop should kill the shared server"
        );

        let third = acquire(&binary, &cwd).expect("a later session should start a fresh server");
        assert_ne!(third.port, port, "the pool should recover from the dead entry");
        assert!(port_is_open(third.port));
    }

    /// A dedicated server — Computer Use bakes per-session configuration into
    /// the environment — dies with its handle just like a pooled one.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn dedicated_server_dies_with_its_last_handle() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let cwd = std::env::temp_dir();

        let server = OpenCodeServer::start(&binary, &cwd).expect("the server should start");
        let port = server.port;
        let handle = PooledServer::dedicated(server);
        assert!(port_is_open(port));

        let clone = handle.clone();
        drop(handle);
        assert!(clone.is_alive(), "a clone should keep the server alive");

        drop(clone);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && port_is_open(port) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!port_is_open(port), "the dedicated server should be killed");
    }
}

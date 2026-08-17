//! Per-workspace pool of resident `opencode serve` processes.
//!
//! OpenCode is one long-lived process that hosts many sessions, so every
//! session in a workspace shares a single server instead of starting its own
//! — one config load, one plugin/MCP stack, and no contention between two
//! `opencode serve` instances in the same directory. The server is killed
//! when the last session using it drops.
//!
//! The pool serializes starts and final teardown per workspace. Reader threads
//! hold only the server's port, never a handle, because the event stream only
//! closes when the process exits — a handle held there would keep the last
//! drop from ever killing the server, deadlocking the reader against itself.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

use crate::opencode_session::OpenCodeServer;

/// How long the last drop waits for the server to actually exit, so a
/// session starting right after it cannot adopt a dying process.
const SERVER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A reference to a server process, shared or dedicated.
///
/// Dropping the last handle kills the server. Clones add handles.
#[derive(Clone)]
pub(crate) struct PooledServer {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    server: OpenCodeServer,
    /// Dedicated Computer Use servers do not participate in a workspace slot.
    slot: Option<Weak<PoolSlot>>,
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.as_ref().and_then(Weak::upgrade) {
            let mut state = slot.state.lock().unwrap();
            let current = match &*state {
                PoolState::Running(server) => std::ptr::eq(server.as_ptr(), self),
                PoolState::Vacant | PoolState::Starting | PoolState::Stopping => false,
            };
            if current {
                // Acquisition uses this same slot lock. Once Arc begins this
                // destructor its weak reference cannot be upgraded, and the
                // slot stays stopping until the owned child has been reaped.
                *state = PoolState::Stopping;
                self.server.shutdown(SERVER_EXIT_TIMEOUT);
                *state = PoolState::Vacant;
                slot.changed.notify_all();
                return;
            }
        }

        // Dedicated servers and superseded dead generations do not own the
        // slot's current process, but their child still has to be reaped.
        self.server.shutdown(SERVER_EXIT_TIMEOUT);
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
            inner: Arc::new(PoolInner { server, slot: None }),
        }
    }
}

type PoolKey = (PathBuf, PathBuf); // (binary, workspace directory)

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

/// Returns the workspace's resident server, starting one if none is alive.
///
/// Blocking (process start plus health probe), so callers must already be off
/// the UI thread — driver start on the background executor is.
pub(crate) fn acquire(binary: &Path, cwd: &Path) -> anyhow::Result<PooledServer> {
    acquire_with_start(binary, cwd, || OpenCodeServer::start(binary, cwd))
}

fn acquire_with_start(
    binary: &Path,
    cwd: &Path,
    start: impl FnOnce() -> anyhow::Result<OpenCodeServer>,
) -> anyhow::Result<PooledServer> {
    let key = (binary.to_path_buf(), cwd.to_path_buf());
    let slot = {
        let mut pool = pool().lock().unwrap();
        Arc::clone(pool.entry(key).or_default())
    };

    let superseded = loop {
        let mut state = slot.state.lock().unwrap();
        if let PoolState::Running(server) = &*state {
            if let Some(inner) = server.upgrade() {
                if inner.server.is_alive() {
                    return Ok(PooledServer { inner });
                }

                // The process exited while older session handles still exist.
                // Replace this generation; its destructor compares the weak
                // identity and therefore cannot tear down the replacement.
                *state = PoolState::Starting;
                break Some(inner);
            }

            // Strong count reached zero and `PoolInner::drop` is about to mark
            // this slot as stopping. Wait releases the lock so that destructor
            // can finish teardown and notify us; never start alongside it.
            state = slot.changed.wait(state).unwrap();
            drop(state);
            continue;
        }

        match &*state {
            PoolState::Vacant => {
                *state = PoolState::Starting;
                break None;
            }
            PoolState::Starting | PoolState::Stopping => {
                state = slot.changed.wait(state).unwrap();
                drop(state);
            }
            PoolState::Running(_) => unreachable!(),
        }
    };

    // If this temporary upgrade was the exited generation's last strong
    // reference, its destructor also takes the slot lock. Drop it only after
    // the guard above has been released.
    drop(superseded);

    let started = start();
    let mut state = slot.state.lock().unwrap();
    match started {
        Ok(server) => {
            let inner = Arc::new(PoolInner {
                server,
                slot: Some(Arc::downgrade(&slot)),
            });
            *state = PoolState::Running(Arc::downgrade(&inner));
            slot.changed.notify_all();
            Ok(PooledServer { inner })
        }
        Err(error) => {
            *state = PoolState::Vacant;
            slot.changed.notify_all();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Instant;
    use uuid::Uuid;

    struct TestWorkspace {
        path: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("waku-opencode-pool-test-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).expect("the test workspace should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

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
        let cwd = TestWorkspace::new();

        let first =
            acquire(&binary, cwd.path()).expect("the first session should start the server");
        let port = first.port;
        assert!(port_is_open(port), "the server should be listening");

        let second = acquire(&binary, cwd.path()).expect("the second session should reuse it");
        assert_eq!(second.port, port, "both sessions must share one process");
        assert!(Arc::ptr_eq(&first.inner, &second.inner));

        drop(first);
        assert!(
            second.is_alive(),
            "the server must outlive the first session"
        );

        let stopping = Instant::now();
        drop(second);
        assert!(
            stopping.elapsed() < Duration::from_secs(2),
            "normal teardown should observe and reap the exited child promptly"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && port_is_open(port) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !port_is_open(port),
            "the last session's drop should kill the shared server"
        );

        let third =
            acquire(&binary, cwd.path()).expect("a later session should start a fresh server");
        assert!(port_is_open(third.port));
    }

    /// Two sessions starting together must share the same in-flight startup
    /// instead of briefly launching competing workspace servers.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn concurrent_acquires_start_one_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let workspace = TestWorkspace::new();
        let cwd = workspace.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(3));
        let starts = Arc::new(AtomicUsize::new(0));

        let acquire_concurrently = |barrier: Arc<Barrier>, starts: Arc<AtomicUsize>| {
            let binary = binary.clone();
            let cwd = cwd.clone();
            thread::spawn(move || {
                barrier.wait();
                acquire_with_start(&binary, &cwd, || {
                    starts.fetch_add(1, Ordering::Relaxed);
                    OpenCodeServer::start(&binary, &cwd)
                })
                .expect("the concurrent session should acquire a server")
            })
        };
        let first = acquire_concurrently(Arc::clone(&barrier), Arc::clone(&starts));
        let second = acquire_concurrently(Arc::clone(&barrier), Arc::clone(&starts));
        barrier.wait();

        let first = first.join().expect("the first acquire should not panic");
        let second = second.join().expect("the second acquire should not panic");
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(&first.inner, &second.inner));
        assert_eq!(first.port, second.port);
    }

    /// A dedicated server — Computer Use bakes per-session configuration into
    /// the environment — dies with its handle just like a pooled one.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn dedicated_server_dies_with_its_last_handle() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let cwd = TestWorkspace::new();

        let server = OpenCodeServer::start(&binary, cwd.path()).expect("the server should start");
        let port = server.port;
        let handle = PooledServer::dedicated(server);
        assert!(port_is_open(port));

        let clone = handle.clone();
        drop(handle);
        assert!(clone.is_alive(), "a clone should keep the server alive");

        let stopping = Instant::now();
        drop(clone);
        assert!(
            stopping.elapsed() < Duration::from_secs(2),
            "dedicated teardown should reap the exited child promptly"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && port_is_open(port) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!port_is_open(port), "the dedicated server should be killed");
    }

    /// An exited generation must be replaced even while an older session
    /// handle still references it, and that stale handle must not kill the
    /// replacement when it eventually drops.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn dead_server_recovers_without_cross_generation_teardown() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let cwd = TestWorkspace::new();

        let stale = acquire(&binary, cwd.path()).expect("the first server should start");
        stale.shutdown(SERVER_EXIT_TIMEOUT);
        assert!(!stale.is_alive());

        let replacement = acquire(&binary, cwd.path()).expect("the dead server should be replaced");
        assert!(!Arc::ptr_eq(&stale.inner, &replacement.inner));
        drop(stale);
        assert!(
            replacement.is_alive(),
            "a stale generation must not stop its replacement"
        );
    }
}

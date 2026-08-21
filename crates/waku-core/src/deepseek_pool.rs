//! Daemon-wide pool of resident `dsh web` processes.
//!
//! A Harness Host owns many sessions and every session supplies its own cwd,
//! so all Waku tasks using the same executable share one process. Starts and
//! final teardown are serialized so a new task never races a dying Host over
//! Harness's persistent session store.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

use crate::deepseek_session::DeepSeekServer;

const SERVER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct PooledDeepSeekServer {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    server: DeepSeekServer,
    slot: Weak<PoolSlot>,
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.upgrade() {
            let mut state = slot.state.lock().unwrap();
            let current = match &*state {
                PoolState::Running(server) => std::ptr::eq(server.as_ptr(), self),
                PoolState::Vacant | PoolState::Starting | PoolState::Stopping => false,
            };
            if current {
                *state = PoolState::Stopping;
                self.server.shutdown(SERVER_EXIT_TIMEOUT);
                *state = PoolState::Vacant;
                slot.changed.notify_all();
                return;
            }
        }
        self.server.shutdown(SERVER_EXIT_TIMEOUT);
    }
}

impl Deref for PooledDeepSeekServer {
    type Target = DeepSeekServer;

    fn deref(&self) -> &Self::Target {
        &self.inner.server
    }
}

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

fn pool() -> &'static Mutex<HashMap<PathBuf, Arc<PoolSlot>>> {
    static POOL: OnceLock<Mutex<HashMap<PathBuf, Arc<PoolSlot>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the resident Harness Host for `binary`, starting it when needed.
/// Blocking by design; driver construction already runs off the UI thread.
pub(crate) fn acquire(binary: &Path) -> anyhow::Result<PooledDeepSeekServer> {
    acquire_with_start(binary, || DeepSeekServer::start(binary))
}

fn acquire_with_start(
    binary: &Path,
    start: impl FnOnce() -> anyhow::Result<DeepSeekServer>,
) -> anyhow::Result<PooledDeepSeekServer> {
    let slot = {
        let mut pool = pool().lock().unwrap();
        Arc::clone(pool.entry(binary.to_path_buf()).or_default())
    };

    let superseded = loop {
        let mut state = slot.state.lock().unwrap();
        if let PoolState::Running(server) = &*state {
            if let Some(inner) = server.upgrade() {
                if inner.server.is_alive() {
                    return Ok(PooledDeepSeekServer { inner });
                }
                *state = PoolState::Starting;
                break Some(inner);
            }
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
    drop(superseded);

    let started = start();
    let mut state = slot.state.lock().unwrap();
    match started {
        Ok(server) => {
            let inner = Arc::new(PoolInner {
                server,
                slot: Arc::downgrade(&slot),
            });
            *state = PoolState::Running(Arc::downgrade(&inner));
            slot.changed.notify_all();
            Ok(PooledDeepSeekServer { inner })
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

    #[test]
    #[ignore = "requires an installed DeepSeek Harness"]
    fn sessions_using_one_binary_share_the_resident_host() {
        let binary =
            crate::command_env::find_executable("dsh").expect("DeepSeek Harness is not installed");
        let first = acquire(&binary).expect("the first Harness lease should start");
        let second = acquire(&binary).expect("the second Harness lease should attach");

        assert_eq!(first.port, second.port);
        drop(first);
        assert!(second.is_alive());
    }
}

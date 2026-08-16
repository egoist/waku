//! Provider title refresh helpers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::Duration;

use crate::driver::DriverEventSender;
use crate::model::DriverEvent;

const IDLE: u8 = 0;
const RUNNING: u8 = 1;
const RESOLVED: u8 = 2;

/// Runs a provider metadata lookup away from the UI and provider reader
/// threads. A missed lookup becomes eligible again on the next completed turn;
/// a resolved title is final for this live driver.
#[derive(Clone, Default)]
pub(super) struct NativeTitleRefresh {
    state: Arc<AtomicU8>,
}

impl NativeTitleRefresh {
    pub(super) fn start(
        &self,
        thread_name: &'static str,
        delays: Vec<Duration>,
        events: DriverEventSender,
        lookup: impl Fn() -> anyhow::Result<Option<String>> + Send + 'static,
    ) {
        if self
            .state
            .compare_exchange(IDLE, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let state = self.state.clone();
        let result = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                for delay in delays {
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                    let title = lookup()
                        .ok()
                        .flatten()
                        .map(|title| title.trim().to_owned())
                        .filter(|title| !title.is_empty());
                    if let Some(title) = title {
                        let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title)));
                        state.store(RESOLVED, Ordering::Release);
                        return;
                    }
                }
                state.store(IDLE, Ordering::Release);
            });

        if result.is_err() {
            self.state.store(IDLE, Ordering::Release);
        }
    }
}

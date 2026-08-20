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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DriverEvent;
    use std::sync::atomic::AtomicUsize;

    /// Blank and failed lookups are misses, not answers, so the schedule keeps
    /// going and the driver is left free to try again on its next turn.
    #[test]
    fn a_schedule_walks_past_misses_and_stops_on_the_first_real_title() {
        let (events, event_rx) = crate::driver::test_event_channel();
        let refresh = NativeTitleRefresh::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let lookups = attempts.clone();

        refresh.start(
            "waku-title-test",
            vec![Duration::ZERO; 4],
            events,
            move || match lookups.fetch_add(1, Ordering::AcqRel) {
                0 => Err(anyhow::anyhow!("the native store is not written yet")),
                1 => Ok(None),
                2 => Ok(Some("   ".into())),
                _ => Ok(Some("  Fix the title poll  ".into())),
            },
        );

        let event = event_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a resolved title must reach the driver's events");
        assert!(matches!(
            event,
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Fix the title poll"
        ));
        assert_eq!(attempts.load(Ordering::Acquire), 4);

        // Resolved is final: a later turn must not re-announce the same title.
        refresh.start(
            "waku-title-test",
            vec![Duration::ZERO],
            {
                let (events, _) = crate::driver::test_event_channel();
                events
            },
            || Ok(Some("Should never run".into())),
        );
        assert_eq!(attempts.load(Ordering::Acquire), 4);
    }

    /// A session whose provider never wrote a title — Claude skips prompts
    /// under ten characters entirely — must leave the refresh re-armable
    /// rather than latching, so a later prompt still gets a look.
    #[test]
    fn a_schedule_that_runs_dry_can_be_started_again() {
        let (events, event_rx) = crate::driver::test_event_channel();
        let refresh = NativeTitleRefresh::default();
        let attempts = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let lookups = attempts.clone();
            refresh.start(
                "waku-title-test",
                vec![Duration::ZERO, Duration::ZERO],
                events.clone(),
                move || {
                    lookups.fetch_add(1, Ordering::AcqRel);
                    Ok(None)
                },
            );
            // The dry run returns the state to IDLE only after its last
            // lookup, so wait on the state itself — an attempt count can be
            // final while the thread is still RUNNING, and the next start
            // would silently no-op.
            for _ in 0..400 {
                if refresh.state.load(Ordering::Acquire) == IDLE {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(refresh.state.load(Ordering::Acquire), IDLE);
        }

        assert_eq!(attempts.load(Ordering::Acquire), 4);
        assert!(
            event_rx.try_recv().is_err(),
            "a dry schedule must not announce a title"
        );
    }
}

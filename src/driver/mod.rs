mod acp;
mod activity;
mod amp;
mod claude;
mod codex;
mod computer_use;
mod deepseek;
mod opencode;
mod pi;
mod support;
mod title_refresh;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, SendError, Sender, unbounded};

use crate::computer_use::ComputerToolRequest;
use crate::model::{
    BackgroundWorkKey, DriverEvent, InteractionMode, ProviderKind, ProviderResumeCursor,
    RuntimeMode,
};

/// Provider events remain synchronous to send from reader threads, while the
/// bounded wake channel lets the UI sleep until at least one event is ready.
/// Multiple provider writes coalesce into one wake without ever blocking the
/// provider or dropping the events themselves.
#[derive(Clone)]
pub struct DriverEventSender {
    events: Sender<DriverEvent>,
    wake: smol::channel::Sender<()>,
}

impl DriverEventSender {
    pub(crate) fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        self.events.send(event)?;
        let _ = self.wake.try_send(());
        Ok(())
    }
}

pub(crate) trait DriverEventSink {
    fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>>;
}

impl DriverEventSink for DriverEventSender {
    fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        DriverEventSender::send(self, event)
    }
}

#[cfg(test)]
impl DriverEventSink for Sender<DriverEvent> {
    fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        Sender::send(self, event)
    }
}

pub(crate) fn event_channel(
    wake: smol::channel::Sender<()>,
) -> (DriverEventSender, Receiver<DriverEvent>) {
    let (events, receiver) = unbounded();
    (DriverEventSender { events, wake }, receiver)
}

#[cfg(test)]
pub(crate) fn test_event_channel() -> (DriverEventSender, Receiver<DriverEvent>) {
    let (wake, _wakes) = smol::channel::bounded(1);
    event_channel(wake)
}

#[derive(Clone)]
pub struct DriverHandle {
    inner: Arc<dyn DriverControl>,
}

impl DriverHandle {
    pub fn prompt(&self, prompt: String) {
        self.inner.prompt(prompt);
    }

    /// Whether this transport can inject a user message into the currently
    /// running turn (steering) instead of starting a new one.
    pub fn supports_steer(&self) -> bool {
        self.inner.supports_steer()
    }

    pub fn steer(&self, prompt: String) {
        self.inner.steer(prompt);
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn cancel_computer_use(&self) {
        self.inner.cancel_computer_use();
    }

    pub fn refresh_background_work(&self) {
        self.inner.refresh_background_work();
    }

    pub fn stop_background_work(&self, key: BackgroundWorkKey, control_id: String) {
        self.inner.stop_background_work(key, control_id);
    }

    pub fn respond(&self, request_id: String, option_id: String) {
        self.inner.respond(request_id, option_id);
    }

    pub fn run_computer_tool(&self, request: ComputerToolRequest) {
        self.inner.run_computer_tool(request);
    }

    pub fn reject_computer_tool(&self, request: ComputerToolRequest, reason: String) {
        self.inner.reject_computer_tool(request, reason);
    }

    pub fn apply_options(&self, options: SessionOptions) -> bool {
        self.inner.apply_options(options)
    }

    pub fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        self.inner.rollback(turns)
    }

    pub fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        self.inner.fork(turns_to_remove)
    }
}

pub trait DriverControl: Send + Sync {
    fn prompt(&self, prompt: String);
    fn supports_steer(&self) -> bool {
        false
    }
    /// Deliver a steering message to the running turn. Implementations report
    /// the outcome asynchronously through `DriverEvent::SteerAccepted` or
    /// `DriverEvent::SteerRejected`.
    fn steer(&self, _prompt: String) {}
    fn cancel(&self);
    fn cancel_computer_use(&self) {}
    fn refresh_background_work(&self) {}
    fn stop_background_work(&self, _key: BackgroundWorkKey, _control_id: String) {}
    fn respond(&self, request_id: String, option_id: String);
    fn run_computer_tool(&self, _request: ComputerToolRequest) {}
    fn reject_computer_tool(&self, _request: ComputerToolRequest, _reason: String) {}
    /// Applies changed turn options to the live session, returning whether the
    /// transport could do it without being restarted. A `false` answer is the
    /// driver asking to be torn down and recreated with the new options.
    fn apply_options(&self, _options: SessionOptions) -> bool {
        false
    }
    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>>;
    fn fork(&self, _turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        anyhow::bail!("conversation forking is not supported by this provider transport")
    }
}

pub struct DriverStartOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub agent_preset: Option<String>,
    pub computer_use_enabled: bool,
    pub provider_cursor: Option<ProviderResumeCursor>,
}

/// The subset of `DriverStartOptions` a user can change without starting a new
/// task. Transports that carry these per turn can absorb a change in place;
/// the rest have to be restarted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOptions {
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
}

pub fn start(
    provider: ProviderKind,
    options: DriverStartOptions,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    let inner: Arc<dyn DriverControl> = match provider {
        ProviderKind::Codex => Arc::new(codex::CodexDriver::start(options, events)?),
        ProviderKind::Pi => Arc::new(pi::PiDriver::start(options, events)?),
        // Cursor and Grok both serve a long-lived ACP session, which is the only
        // way their Supervised mode can actually ask the user rather than
        // silently forcing or denying.
        ProviderKind::Cursor | ProviderKind::Grok => {
            Arc::new(acp::AcpDriver::start(provider, options, events)?)
        }
        ProviderKind::DeepSeek => Arc::new(deepseek::DeepSeekDriver::start(options, events)?),
        // OpenCode's own server is its real API, and it is what exposes
        // interactive permission requests.
        ProviderKind::OpenCode => Arc::new(opencode::OpenCodeDriver::start(options, events)?),
        // Claude serves a realtime stream of user messages on stdin — the same
        // transport the Agent SDK drives — which is what lets its Supervised
        // mode ask rather than decide alone.
        ProviderKind::Claude => Arc::new(claude::ClaudeDriver::start(options, events)?),
        // Amp reads newline-delimited user messages on stdin and stays alive
        // until stdin closes, so it too serves the whole conversation.
        ProviderKind::Amp => Arc::new(amp::AmpDriver::start(options, events)?),
    };
    Ok(DriverHandle { inner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_events_coalesce_wakes_without_dropping_payloads() {
        let (wake, wakes) = smol::channel::bounded(1);
        let (events, received) = event_channel(wake);

        events.send(DriverEvent::TextDelta("one".into())).unwrap();
        events.send(DriverEvent::TextDelta("two".into())).unwrap();

        assert_eq!(wakes.try_recv(), Ok(()));
        assert!(matches!(
            wakes.try_recv(),
            Err(smol::channel::TryRecvError::Empty)
        ));
        assert!(matches!(received.try_recv(), Ok(DriverEvent::TextDelta(text)) if text == "one"));
        assert!(matches!(received.try_recv(), Ok(DriverEvent::TextDelta(text)) if text == "two"));
    }
}

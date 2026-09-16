//! Provider-neutral handle implemented by the desktop RPC adapter.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, SendError, Sender, unbounded};
use uuid::Uuid;
use waku_protocol::computer_use::ComputerToolRequest;
use waku_protocol::model::{
    BackgroundWorkKey, DriverEvent, GoalOperation, ProviderResumeCursor, RuntimeMode,
    UserInputAnswer,
};

#[derive(Clone)]
pub struct DriverEventSender {
    events: Sender<DriverEvent>,
    wake: smol::channel::Sender<()>,
}

impl DriverEventSender {
    pub fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        self.events.send(event)?;
        let _ = self.wake.try_send(());
        Ok(())
    }
}

pub fn event_channel(
    wake: smol::channel::Sender<()>,
) -> (DriverEventSender, Receiver<DriverEvent>) {
    let (events, receiver) = unbounded();
    (DriverEventSender { events, wake }, receiver)
}

#[derive(Clone)]
pub struct DriverHandle {
    inner: Arc<dyn DriverControl>,
}

impl DriverHandle {
    pub fn from_control(control: Arc<dyn DriverControl>) -> Self {
        Self { inner: control }
    }

    /// Send a prompt along with the ids this client gave the turn it opened
    /// and that turn's user message, so the daemon can publish the same
    /// identity to every other client attached to the runtime.
    pub fn prompt(&self, prompt: String, turn_id: Option<Uuid>, message_id: Option<Uuid>) {
        self.inner.prompt(prompt, turn_id, message_id);
    }

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

    pub fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        self.inner.respond_user_input(request_id, answers);
    }

    /// Read or mutate the provider-persisted thread goal. Outcomes arrive
    /// asynchronously as `DriverEvent::GoalUpdated` or `DriverEvent::Error`.
    pub fn goal(&self, operation: GoalOperation) {
        self.inner.goal(operation);
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

    pub fn close(&self) {
        self.inner.close();
    }
}

pub trait DriverControl: Send + Sync {
    fn prompt(&self, prompt: String, turn_id: Option<Uuid>, message_id: Option<Uuid>);
    fn supports_steer(&self) -> bool {
        false
    }
    fn steer(&self, _prompt: String) {}
    fn cancel(&self);
    fn cancel_computer_use(&self) {}
    fn refresh_background_work(&self) {}
    fn stop_background_work(&self, _key: BackgroundWorkKey, _control_id: String) {}
    fn respond(&self, request_id: String, option_id: String);
    fn respond_user_input(&self, _request_id: String, _answers: Vec<UserInputAnswer>) {}
    fn goal(&self, _operation: GoalOperation) {}
    fn run_computer_tool(&self, _request: ComputerToolRequest) {}
    fn reject_computer_tool(&self, _request: ComputerToolRequest, _reason: String) {}
    fn apply_options(&self, _options: SessionOptions) -> bool {
        false
    }
    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>>;
    fn fork(&self, _turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        anyhow::bail!("conversation forking is not supported by this provider transport")
    }
    fn close(&self) {}
}

pub struct DriverStartOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub mode: RuntimeMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
    pub agent_preset: Option<String>,
    pub computer_use_enabled: bool,
    pub provider_cursor: Option<ProviderResumeCursor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOptions {
    pub mode: RuntimeMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
}

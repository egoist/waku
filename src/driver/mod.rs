//! Desktop proxy for the provider runtime owned by `waku-daemon`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::computer_use::ComputerToolRequest;
use crate::model::{
    BackgroundWorkKey, DriverEvent, ProviderKind, ProviderResumeCursor, RuntimeEventCursor,
};
use crossbeam_channel::{Sender, bounded, select};
use parking_lot::Mutex;

pub use waku_client::driver::{
    DriverControl, DriverEventSender, DriverHandle, DriverStartOptions, SessionOptions,
    event_channel,
};

pub(crate) fn start_remote(
    daemon: waku_client::DaemonSupervisor,
    session_id: uuid::Uuid,
    provider: ProviderKind,
    options: DriverStartOptions,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    let client = daemon.client();
    let runtime_id = uuid::Uuid::new_v4();
    let command = waku_client::Command::Start {
        options: waku_client::WireDriverStartOptions {
            provider: waku_client::encode_enum(provider)?,
            binary: options.binary,
            cwd: options.cwd,
            mode: waku_client::encode_enum(options.mode)?,
            model: options.model,
            reasoning_effort: options.reasoning_effort,
            service_tier: options.service_tier,
            context_window: options.context_window,
            agent_preset: options.agent_preset,
            computer_use_enabled: options.computer_use_enabled,
            provider_cursor: options
                .provider_cursor
                .map(serde_json::to_value)
                .transpose()?,
        },
    };
    let supports_steer = match client.request(session_id, runtime_id, command) {
        Ok(waku_client::ResponsePayload::Started { supports_steer }) => supports_steer,
        Ok(_) => anyhow::bail!("Waku daemon returned an invalid start response"),
        Err(error) => return Err(error),
    };
    connect_remote(
        daemon,
        client,
        session_id,
        runtime_id,
        supports_steer,
        None,
        events,
    )
}

pub(crate) fn attach_remote(
    daemon: waku_client::DaemonSupervisor,
    client: waku_client::DaemonClient,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    replay_cursor: Option<RuntimeEventCursor>,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    connect_remote(
        daemon,
        client,
        session_id,
        runtime_id,
        supports_steer,
        replay_cursor,
        events,
    )
}

fn connect_remote(
    daemon: waku_client::DaemonSupervisor,
    initial_client: waku_client::DaemonClient,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    replay_cursor: Option<RuntimeEventCursor>,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    let client_updates = daemon.subscribe_clients();
    let active_client = Arc::new(Mutex::new(initial_client.clone()));
    let forwarding_client = active_client.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let forwarding_closed = closed.clone();
    let (shutdown, forwarding_shutdown) = bounded(1);
    let forwarding_events = events.clone();
    let thread_initial_client = initial_client.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("waku-daemon-session-{session_id}"))
        .spawn(move || {
            let mut client = thread_initial_client;
            let mut remote_events = client.subscribe(session_id, runtime_id);
            loop {
                let disconnected = loop {
                    select! {
                        recv(forwarding_shutdown) -> _ => return,
                        recv(remote_events) -> sequenced => {
                            let Ok(sequenced) = sequenced else {
                                break true;
                            };
                            if replay_cursor.is_some_and(|cursor| {
                                cursor.runtime_id == sequenced.runtime_id
                                    && cursor.epoch == sequenced.epoch
                                    && cursor.sequence >= sequenced.sequence
                            }) {
                                continue;
                            }
                            let cursor = RuntimeEventCursor {
                                runtime_id: sequenced.runtime_id,
                                epoch: sequenced.epoch,
                                sequence: sequenced.sequence,
                            };
                            let event = match waku_client::event_from_wire(sequenced.event) {
                                Ok(event) => event,
                                Err(error) => DriverEvent::Error(format!(
                                    "Waku daemon sent an invalid event: {error}"
                                )),
                            };
                            let process_exited = matches!(&event, DriverEvent::ProcessExited);
                            if forwarding_events.send(event).is_err()
                                || forwarding_events
                                    .send(DriverEvent::RuntimeEventCursorAdvanced(cursor))
                                    .is_err()
                            {
                                return;
                            }
                            if process_exited {
                                break false;
                            }
                        }
                    }
                };
                client.unsubscribe(session_id, runtime_id);
                if !disconnected || forwarding_closed.load(Ordering::Acquire) {
                    break;
                }

                // A closed WebSocket says nothing about the provider process.
                // Wait for the supervisor's next connection, then ask that
                // daemon whether this exact runtime survived before changing
                // the task status.
                let replacement = loop {
                    select! {
                        recv(forwarding_shutdown) -> _ => return,
                        recv(client_updates) -> replacement => {
                            let Ok(replacement) = replacement else {
                                return;
                            };
                            if !client.same_connection(&replacement) {
                                break replacement;
                            }
                        }
                    }
                };
                let attached = replacement.request(
                    session_id,
                    uuid::Uuid::nil(),
                    waku_client::Command::AttachSession,
                );
                match attached {
                    Ok(waku_client::ResponsePayload::SessionRuntime {
                        runtime_id: Some(attached_runtime_id),
                        ..
                    }) if attached_runtime_id == runtime_id => {
                        *forwarding_client.lock() = replacement.clone();
                        client = replacement;
                        remote_events = client.subscribe(session_id, runtime_id);
                    }
                    Ok(waku_client::ResponsePayload::SessionRuntime { .. }) => {
                        let _ = forwarding_events.send(DriverEvent::ProcessExited);
                        break;
                    }
                    Ok(_) => {
                        let _ = forwarding_events.send(DriverEvent::Error(
                            "Waku daemon returned an invalid runtime attachment response".into(),
                        ));
                        break;
                    }
                    Err(_) => {
                        // This replacement also disappeared. Its client
                        // channel will be followed by another supervisor
                        // publication; keep the task live until one can answer
                        // authoritatively.
                        client = replacement;
                    }
                }
            }
        });
    if let Err(error) = spawn {
        initial_client.unsubscribe(session_id, runtime_id);
        return Err(error.into());
    }
    Ok(DriverHandle::from_control(Arc::new(RemoteDriverControl {
        client: active_client,
        session_id,
        runtime_id,
        supports_steer,
        events,
        closed,
        shutdown,
    })))
}

struct RemoteDriverControl {
    client: Arc<Mutex<waku_client::DaemonClient>>,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    events: DriverEventSender,
    closed: Arc<AtomicBool>,
    shutdown: Sender<()>,
}

impl RemoteDriverControl {
    fn notify(&self, command: waku_client::Command) {
        let client = self.client.lock().clone();
        if let Err(error) = client.notify(self.session_id, self.runtime_id, command) {
            let _ = self.events.send(DriverEvent::Error(format!(
                "Waku daemon command failed: {error}"
            )));
        }
    }
}

impl DriverControl for RemoteDriverControl {
    fn prompt(&self, prompt: String, turn_id: Option<uuid::Uuid>, message_id: Option<uuid::Uuid>) {
        self.notify(waku_client::Command::Prompt {
            prompt,
            turn_id,
            message_id,
        });
    }

    fn supports_steer(&self) -> bool {
        self.supports_steer
    }

    fn steer(&self, prompt: String) {
        self.notify(waku_client::Command::Steer { prompt });
    }

    fn cancel(&self) {
        self.notify(waku_client::Command::Cancel);
    }

    fn cancel_computer_use(&self) {
        self.notify(waku_client::Command::CancelComputerUse);
    }

    fn refresh_background_work(&self) {
        self.notify(waku_client::Command::RefreshBackgroundWork);
    }

    fn stop_background_work(&self, key: BackgroundWorkKey, control_id: String) {
        match serde_json::to_value(key) {
            Ok(key) => self.notify(waku_client::Command::StopBackgroundWork { key, control_id }),
            Err(error) => {
                let _ = self.events.send(DriverEvent::Error(format!(
                    "could not encode background-work command: {error}"
                )));
            }
        }
    }

    fn respond(&self, request_id: String, option_id: String) {
        self.notify(waku_client::Command::Respond {
            request_id,
            option_id,
        });
    }

    fn respond_user_input(
        &self,
        request_id: String,
        answers: Vec<waku_protocol::model::UserInputAnswer>,
    ) {
        self.notify(waku_client::Command::RespondUserInput {
            request_id,
            answers,
        });
    }

    fn goal(&self, operation: waku_protocol::model::GoalOperation) {
        self.notify(waku_client::Command::Goal { operation });
    }

    fn run_computer_tool(&self, request: ComputerToolRequest) {
        self.notify(waku_client::Command::RunComputerTool {
            request: waku_client::WireComputerToolRequest {
                call_id: request.call_id,
                tool: request.tool,
                arguments: request.arguments,
            },
        });
    }

    fn reject_computer_tool(&self, request: ComputerToolRequest, reason: String) {
        self.notify(waku_client::Command::RejectComputerTool {
            request: waku_client::WireComputerToolRequest {
                call_id: request.call_id,
                tool: request.tool,
                arguments: request.arguments,
            },
            reason,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        let options = (|| {
            Ok::<_, anyhow::Error>(waku_client::WireSessionOptions {
                mode: waku_client::encode_enum(options.mode)?,
                model: options.model,
                reasoning_effort: options.reasoning_effort,
                service_tier: options.service_tier,
                context_window: options.context_window,
            })
        })();
        let Ok(options) = options else {
            return false;
        };
        let client = self.client.lock().clone();
        matches!(
            client.request(
                self.session_id,
                self.runtime_id,
                waku_client::Command::ApplyOptions { options }
            ),
            Ok(waku_client::ResponsePayload::OptionsApplied { applied: true })
        )
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        let client = self.client.lock().clone();
        match client.request(
            self.session_id,
            self.runtime_id,
            waku_client::Command::Rollback { turns },
        )? {
            waku_client::ResponsePayload::Cursor { cursor } => cursor
                .map(serde_json::from_value)
                .transpose()
                .map_err(Into::into),
            _ => anyhow::bail!("Waku daemon returned an invalid rollback response"),
        }
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        let client = self.client.lock().clone();
        match client.request(
            self.session_id,
            self.runtime_id,
            waku_client::Command::Fork { turns_to_remove },
        )? {
            waku_client::ResponsePayload::Cursor {
                cursor: Some(cursor),
            } => serde_json::from_value(cursor).map_err(Into::into),
            _ => anyhow::bail!("Waku daemon returned an invalid fork response"),
        }
    }

    fn close(&self) {
        self.notify(waku_client::Command::CloseSession);
    }
}

impl Drop for RemoteDriverControl {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.shutdown.try_send(());
        let client = self.client.lock().clone();
        client.unsubscribe(self.session_id, self.runtime_id);
    }
}

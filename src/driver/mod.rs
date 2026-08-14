//! Desktop proxy for the provider runtime owned by `waku-daemon`.

use std::sync::Arc;

use crate::computer_use::ComputerToolRequest;
use crate::model::{BackgroundWorkKey, DriverEvent, ProviderKind, ProviderResumeCursor};

pub use waku_client::driver::{
    DriverControl, DriverEventSender, DriverHandle, DriverStartOptions, SessionOptions,
    event_channel,
};

pub(crate) fn start_remote(
    client: waku_client::DaemonClient,
    session_id: uuid::Uuid,
    provider: ProviderKind,
    options: DriverStartOptions,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    let runtime_id = uuid::Uuid::new_v4();
    let remote_events = client.subscribe(session_id, runtime_id);
    let forwarding_events = events.clone();
    std::thread::Builder::new()
        .name(format!("waku-daemon-session-{session_id}"))
        .spawn(move || {
            let mut saw_process_exit = false;
            while let Ok(event) = remote_events.recv() {
                match waku_client::event_from_wire(event.event) {
                    Ok(event) => {
                        saw_process_exit |= matches!(&event, DriverEvent::ProcessExited);
                        if forwarding_events.send(event).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = forwarding_events.send(DriverEvent::Error(format!(
                            "Waku daemon sent an invalid event: {error}"
                        )));
                    }
                }
            }
            // A development hot-swap (or daemon crash) closes the old event
            // channel. Surface that as an ordinary provider exit so the task
            // cannot remain stuck in Working while the supervisor connects
            // subsequent work to the replacement daemon.
            if !saw_process_exit {
                let _ = forwarding_events.send(DriverEvent::ProcessExited);
            }
        })?;
    let command = waku_client::Command::Start {
        options: waku_client::WireDriverStartOptions {
            provider: waku_client::encode_enum(provider)?,
            binary: options.binary,
            cwd: options.cwd,
            mode: waku_client::encode_enum(options.mode)?,
            interaction_mode: waku_client::encode_enum(options.interaction_mode)?,
            model: options.model,
            reasoning_effort: options.reasoning_effort,
            service_tier: options.service_tier,
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
        Ok(_) => {
            client.unsubscribe(session_id, runtime_id);
            anyhow::bail!("Waku daemon returned an invalid start response")
        }
        Err(error) => {
            client.unsubscribe(session_id, runtime_id);
            return Err(error);
        }
    };
    Ok(DriverHandle::from_control(Arc::new(RemoteDriverControl {
        client,
        session_id,
        runtime_id,
        supports_steer,
        events,
    })))
}

struct RemoteDriverControl {
    client: waku_client::DaemonClient,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    events: DriverEventSender,
}

impl RemoteDriverControl {
    fn notify(&self, command: waku_client::Command) {
        if let Err(error) = self
            .client
            .notify(self.session_id, self.runtime_id, command)
        {
            let _ = self.events.send(DriverEvent::Error(format!(
                "Waku daemon command failed: {error}"
            )));
        }
    }
}

impl DriverControl for RemoteDriverControl {
    fn prompt(&self, prompt: String) {
        self.notify(waku_client::Command::Prompt { prompt });
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
                interaction_mode: waku_client::encode_enum(options.interaction_mode)?,
                model: options.model,
                reasoning_effort: options.reasoning_effort,
                service_tier: options.service_tier,
            })
        })();
        let Ok(options) = options else {
            return false;
        };
        matches!(
            self.client.request(
                self.session_id,
                self.runtime_id,
                waku_client::Command::ApplyOptions { options }
            ),
            Ok(waku_client::ResponsePayload::OptionsApplied { applied: true })
        )
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        match self.client.request(
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
        match self.client.request(
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
}

impl Drop for RemoteDriverControl {
    fn drop(&mut self) {
        let _ = self.client.notify(
            self.session_id,
            self.runtime_id,
            waku_client::Command::CloseSession,
        );
        self.client.unsubscribe(self.session_id, self.runtime_id);
    }
}

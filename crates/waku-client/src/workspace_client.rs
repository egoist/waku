use std::path::Path;

use uuid::Uuid;
use waku_protocol::model::{ProviderKind, ProviderResumeCursor};
use waku_protocol::provider_session::{
    ImportedSessionRecord, ProviderSessionFork, ProviderSessionForkRequest, ResumableSession,
};
use waku_protocol::{Command, ResponsePayload, WorkspaceOperation, WorkspaceResult};

use crate::DaemonClient;

/// Typed convenience wrapper around daemon-owned filesystem and Git RPCs.
#[derive(Clone)]
pub struct WorkspaceClient {
    client: DaemonClient,
}

impl WorkspaceClient {
    pub fn new(client: DaemonClient) -> Self {
        Self { client }
    }

    pub fn request(&self, operation: WorkspaceOperation) -> anyhow::Result<WorkspaceResult> {
        match self
            .client
            .request(Uuid::nil(), Uuid::nil(), Command::Workspace { operation })?
        {
            ResponsePayload::Workspace { result } => Ok(result),
            _ => anyhow::bail!("Waku daemon returned an invalid workspace response"),
        }
    }

    pub fn fork_provider_session(
        &self,
        request: ProviderSessionForkRequest,
    ) -> anyhow::Result<ProviderSessionFork> {
        match self.client.request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ForkProviderSession { request },
        )? {
            ResponsePayload::ProviderSessionForked { result } => Ok(result),
            _ => anyhow::bail!("Waku daemon returned an invalid provider-fork response"),
        }
    }

    pub fn list_provider_sessions(
        &self,
        provider: ProviderKind,
        project_root: &Path,
    ) -> anyhow::Result<Vec<ResumableSession>> {
        match self.client.request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ListProviderSessions {
                provider,
                project_root: project_root.to_owned(),
            },
        )? {
            ResponsePayload::ProviderSessions { sessions } => Ok(sessions),
            _ => anyhow::bail!("Waku daemon returned an invalid provider-session list"),
        }
    }

    pub fn import_provider_session(
        &self,
        cursor: ProviderResumeCursor,
    ) -> anyhow::Result<Vec<ImportedSessionRecord>> {
        match self.client.request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ImportProviderSession { cursor },
        )? {
            ResponsePayload::ImportedProviderSession { records } => Ok(records),
            _ => anyhow::bail!("Waku daemon returned an invalid provider-session import"),
        }
    }
}

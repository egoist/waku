//! Rust transport and lifecycle support for clients of `waku-daemon`.
//!
//! This crate intentionally depends only on [`waku_protocol`], so GUI and CLI
//! clients cannot accidentally reach daemon-owned filesystem, Git, database,
//! or provider implementations.

mod client;
pub mod command_env;
pub mod composer_complete;
pub mod computer_use;
pub mod driver;
pub mod persistence;
mod process;
mod workspace_client;

pub use client::DaemonClient;
pub use process::{
    DEFAULT_EXPOSED_DAEMON_PORT, DaemonExposureSettings, DaemonProcess, DaemonSupervisor,
    parse_allowed_origins,
};
pub use waku_protocol::*;
pub use workspace_client::WorkspaceClient;

pub mod git_branch {
    pub use waku_protocol::git::{BranchEntry, BranchSnapshot};
}

pub mod git_commit {
    pub use waku_protocol::git::AgentInvocation;
    pub use waku_protocol::git::CommitSnapshot as Snapshot;
}

pub mod worktree {
    pub use waku_protocol::git::CreatedWorktree;
}

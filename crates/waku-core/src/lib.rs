#![recursion_limit = "256"]

//! Waku's daemon-side core.
//!
//! Provider, database, filesystem, and Git implementations live here, behind
//! the transport-neutral contract in `waku-protocol`. Client applications
//! intentionally depend on `waku-client` instead of this crate.

rust_i18n::i18n!("../../locales", fallback = "en");

macro_rules! tr {
    ($key:expr) => {
        crate::i18n::translate($key)
    };
    ($key:expr, $($args:tt)*) => {
        rust_i18n::t!($key, $($args)*).into_owned()
    };
}

pub mod amp_session;
pub mod attachments;
pub mod automation;
pub mod blob_store;
pub mod checkpoint;
pub mod claude_session;
pub mod command_env;
pub mod composer_complete;
pub mod computer_use;
pub mod cursor_session;
pub mod daemon;
pub mod deepseek_pool;
pub mod deepseek_session;
pub mod driver;
pub mod git_branch;
pub mod git_commit;
pub mod grok_session;
pub mod i18n;
pub mod identity;
pub mod model;
pub mod model_catalog;
pub mod opencode_pool;
pub mod opencode_session;
pub mod persistence;
pub mod projectless;
pub mod settings;
pub mod skills;
pub mod terminal;
pub mod theme;
pub mod usage;
pub mod usage_history;
pub mod workspace;
pub mod worktree;

mod protocol;
mod server;

pub use protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    PROTOCOL_VERSION, ReplayCursor, Request, ResponseOutcome, ResponsePayload, RpcError,
    SequencedEvent, ServerMessage, WireComputerToolRequest, WireDriverEvent,
    WireDriverStartOptions, WireSessionOptions,
};
pub use server::{Backend, EventSink, ServerOptions, serve};
pub use settings::{DaemonSettings, DaemonSettingsStore};
pub use workspace::{WorkspaceOperation, WorkspaceResult};

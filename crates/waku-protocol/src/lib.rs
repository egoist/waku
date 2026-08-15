#![recursion_limit = "256"]

//! Waku's shared, versioned wire contract.
//!
//! This crate contains serializable data only. It performs no provider,
//! database, workspace, Git, attachment, or transport I/O, so native and web
//! clients can depend on it without pulling in the daemon implementation.

rust_i18n::i18n!("../../locales", fallback = "en");

// `i18n!` reads these files in a proc macro. Explicit includes make them
// visible to Cargo's dependency tracker, so locale-only edits rebuild this
// shared translation registry under the development watcher.
const _LOCALE_SOURCES: [&str; 3] = [
    include_str!("../../../locales/app.yml"),
    include_str!("../../../locales/zh-CN.yml"),
    include_str!("../../../locales/ja.yml"),
];

macro_rules! tr {
    ($key:expr) => {
        crate::i18n::translate($key)
    };
    ($key:expr, $($args:tt)*) => {
        rust_i18n::t!($key, $($args)*).into_owned()
    };
}

pub mod attachments;
pub mod automation;
pub mod blob;
pub mod checkpoint;
pub mod composer;
pub mod computer_use;
mod driver_wire;
pub mod git;
pub mod i18n;
pub mod identity;
pub mod model;
pub mod model_catalog;
pub mod persistence;
pub mod projectless;
pub mod provider_session;
pub mod settings;
pub mod skills;
pub mod theme;
pub mod usage;
pub mod usage_history;
pub mod workspace;

mod protocol;

pub use driver_wire::{decode_enum, encode_enum, event_from_wire, event_to_wire};
pub use protocol::{
    APP_EXECUTABLE_ENV, AutomationNotification, ClientMessage, Command, DAEMON_ADDRESS_ENV,
    DAEMON_TOKEN_ENV, DaemonReady, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, ReplayCursor, Request,
    ResponseOutcome, ResponsePayload, RpcError, SequencedEvent, ServerMessage,
    WireComputerToolRequest, WireDriverEvent, WireDriverStartOptions, WireSessionOptions,
};
pub use settings::DaemonSettings;
pub use workspace::{WorkspaceOperation, WorkspaceResult};

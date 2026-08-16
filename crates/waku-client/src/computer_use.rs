//! Client-side Computer Use presentation data.

use waku_protocol::APP_EXECUTABLE_ENV;
pub use waku_protocol::computer_use::*;

#[derive(Clone, Debug)]
pub struct PendingComputerApproval {
    pub request: ComputerToolRequest,
    pub target: ComputerTarget,
    pub sensitive: bool,
}

pub fn helper_display_name() -> String {
    std::env::var_os(APP_EXECUTABLE_ENV)
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .map(|app_name| format!("{app_name} Computer Use"))
        .unwrap_or_else(|| "Waku Computer Use".into())
}

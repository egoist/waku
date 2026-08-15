#[cfg(debug_assertions)]
pub const APP_NAME: &str = "Waku Debug";
#[cfg(not(debug_assertions))]
pub const APP_NAME: &str = "Waku";

#[cfg(debug_assertions)]
pub const APP_ID: &str = "sh.waku.dev";
#[cfg(not(debug_assertions))]
pub const APP_ID: &str = "sh.waku";

#[cfg(debug_assertions)]
pub const DATA_DIRECTORY_NAME: &str = "Waku Debug";
#[cfg(not(debug_assertions))]
pub const DATA_DIRECTORY_NAME: &str = "Waku";

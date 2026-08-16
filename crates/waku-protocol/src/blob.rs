//! Durable references for daemon-owned binary task payloads.

/// Scheme for a stored blob reference, e.g. `waku-blob:3f2a...c1.png`.
pub const SCHEME: &str = "waku-blob:";

pub fn is_reference(value: &str) -> bool {
    value.starts_with(SCHEME)
}

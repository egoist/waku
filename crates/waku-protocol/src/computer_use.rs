use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Whether this build may run Computer Use.
///
/// The feature is experimental, so only development builds expose it. Release
/// builds refuse it even when a client asks, which keeps a stale setting, a
/// hand-edited settings file, or a third-party daemon client from turning it on
/// in production.
pub const fn is_available() -> bool {
    cfg!(debug_assertions)
}

/// Clamp a requested Computer Use enablement to what this build ships.
pub const fn resolve_enabled(requested: bool) -> bool {
    requested && is_available()
}

#[derive(Clone, Debug)]
pub struct ComputerToolRequest {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
}

impl ComputerToolRequest {
    pub fn summary(&self) -> String {
        if self.tool != "use" {
            return match self.tool.as_str() {
                "status" => "Check computer-use access".into(),
                _ => self.tool.clone(),
            };
        }
        let actions = self
            .arguments
            .get("actions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if actions.is_empty() {
            return "Inspect the window".into();
        }
        let mut labels = actions
            .iter()
            .filter_map(|action| action.get("type").and_then(Value::as_str))
            .map(action_label)
            .collect::<Vec<_>>();
        labels.dedup();
        format!("{} {}", labels.join(", "), plural(actions.len(), "action"))
    }
}

fn action_label(action: &str) -> &'static str {
    match action {
        "click" | "double_click" => "Click",
        "move" => "Move the pointer",
        "drag" => "Drag",
        "scroll" => "Scroll",
        "type" => "Type text",
        "keypress" => "Press keys",
        "wait" => "Wait",
        _ => "Interact",
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputerPermissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTarget {
    pub window_id: u64,
    pub bundle_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub app_name: String,
    pub window_title: String,
    pub width: u32,
    pub height: u32,
}

impl ComputerTarget {
    pub fn grant_key(&self) -> String {
        self.bundle_id.clone()
    }

    pub fn persistable(&self) -> bool {
        !self.bundle_id.trim().is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputerAppGrant {
    pub bundle_id: String,
    pub app_name: String,
}

impl ComputerAppGrant {
    pub fn key(&self) -> String {
        self.bundle_id.clone()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ComputerUsePhase {
    AwaitingApproval,
    Running,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseState {
    pub target: Option<ComputerTarget>,
    pub phase: ComputerUsePhase,
    pub visible: bool,
    pub image_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{is_available, resolve_enabled};

    #[test]
    fn computer_use_is_development_only() {
        assert_eq!(is_available(), cfg!(debug_assertions));
        assert!(!resolve_enabled(false));
        assert_eq!(resolve_enabled(true), is_available());
    }
}

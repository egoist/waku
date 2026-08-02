use std::fs;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context as _, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const MAX_HELPER_OUTPUT_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerPermissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTarget {
    pub window_id: u32,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
                "list_targets" => "Inspect available app windows".into(),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputerUsePhase {
    AwaitingApproval,
    Running,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ComputerUseState {
    pub target: Option<ComputerTarget>,
    pub phase: ComputerUsePhase,
    pub visible: bool,
    pub screenshot: Option<Arc<gpui::Image>>,
}

#[derive(Clone, Debug)]
pub struct PendingComputerApproval {
    pub request: ComputerToolRequest,
    pub target: ComputerTarget,
    pub sensitive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelperResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    permissions: Option<ComputerPermissions>,
}

pub fn probe_permissions(prompt: bool) -> anyhow::Result<ComputerPermissions> {
    let operation = if prompt {
        json!({"operation": "requestPermissions"})
    } else {
        json!({"operation": "status"})
    };
    let helper = mcp_server_command()?;
    let active_helper_pid = AtomicU32::new(0);
    let response = invoke_helper_direct(&helper, &operation, &active_helper_pid)?;
    if !response.success {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "Could not check computer-use permissions".into())
        );
    }
    Ok(response.permissions.unwrap_or_default())
}

fn invoke_helper_direct(
    helper: &Path,
    operation: &Value,
    active_helper_pid: &AtomicU32,
) -> anyhow::Result<HelperResponse> {
    let mode = match operation.get("operation").and_then(Value::as_str) {
        Some("status") => Some("status"),
        Some("requestPermissions") => Some("request-permissions"),
        _ => None,
    };
    let mut command = Command::new(helper);
    if let Some(mode) = mode {
        command.arg(mode);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {}", helper.display()))?;
    let pid = child.id();
    active_helper_pid.store(pid, Ordering::SeqCst);
    let payload = serde_json::to_vec(operation)?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("computer-use helper stdin unavailable"))?
        .write_all(&payload)?;
    let output = child.wait_with_output()?;
    let _ = active_helper_pid.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    if output.stdout.len() > MAX_HELPER_OUTPUT_BYTES {
        bail!("computer-use helper returned too much data");
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("computer-use helper failed: {}", stderr.trim());
    }
    serde_json::from_slice(&output.stdout).context("computer-use helper returned invalid JSON")
}

fn helper_app_path() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe().context("Waku executable path is unavailable")?;
    let macos = executable
        .parent()
        .ok_or_else(|| anyhow!("Waku executable has no parent directory"))?;
    let contents = macos
        .parent()
        .ok_or_else(|| anyhow!("Waku app bundle is malformed"))?;
    let app_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Waku executable name is invalid"))?;
    let helper_name = format!("{app_name} Computer Use");
    let path = contents.join("Helpers").join(format!("{helper_name}.app"));
    if !path.is_dir() {
        bail!("Computer Use helper is missing from this Waku build")
    }
    Ok(path)
}

pub fn helper_display_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .map(|app_name| format!("{app_name} Computer Use"))
        .unwrap_or_else(|| "Waku Computer Use".into())
}

pub fn mcp_server_command() -> anyhow::Result<PathBuf> {
    let bundled_helper = helper_app_path()?;
    let helper = install_helper_app(&bundled_helper)?;
    let executable = helper
        .file_stem()
        .ok_or_else(|| anyhow!("Computer Use helper name is invalid"))?;
    Ok(helper.join("Contents").join("MacOS").join(executable))
}

/// Keep the TCC-facing helper at a stable path.
///
/// The helper is embedded in the debug/release app for distribution, but
/// launching that nested copy directly makes every rebuilt app bundle a new
/// privacy client. Installing the signed helper at the stable Application
/// Support path preserves the client macOS associates with the user's
/// Accessibility and Screen Recording grants.
fn install_helper_app(source: &Path) -> anyhow::Result<PathBuf> {
    let application_support =
        dirs::data_dir().ok_or_else(|| anyhow!("Application Support directory is unavailable"))?;
    let install_root = application_support.join("Waku").join("Computer Use");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&install_root)
        .with_context(|| format!("could not create {}", install_root.display()))?;
    let bundle_name = source
        .file_name()
        .ok_or_else(|| anyhow!("Computer Use helper bundle name is invalid"))?;
    let destination = install_root.join(bundle_name);
    if helper_install_matches(source, &destination)? {
        return Ok(destination);
    }

    let staging = install_root.join(format!(".install-{}.app", Uuid::new_v4().simple()));
    copy_directory(source, &staging)?;
    let previous = install_root.join(format!(".previous-{}.app", Uuid::new_v4().simple()));
    let had_previous = destination.exists();
    if had_previous {
        fs::rename(&destination, &previous)
            .with_context(|| format!("could not replace {}", destination.display()))?;
    }
    if let Err(error) = fs::rename(&staging, &destination) {
        if had_previous {
            let _ = fs::rename(&previous, &destination);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error).context("could not install Computer Use helper");
    }
    if had_previous {
        let _ = fs::remove_dir_all(previous);
    }
    Ok(destination)
}

fn helper_install_matches(source: &Path, destination: &Path) -> anyhow::Result<bool> {
    if !destination.is_dir() {
        return Ok(false);
    }
    let helper_name = source
        .file_stem()
        .ok_or_else(|| anyhow!("Computer Use helper name is invalid"))?;
    for relative in [
        PathBuf::from("Contents/Info.plist"),
        PathBuf::from("Contents/MacOS").join(helper_name),
    ] {
        let source_bytes = fs::read(source.join(&relative))?;
        let Ok(installed_bytes) = fs::read(destination.join(&relative)) else {
            return Ok(false);
        };
        if source_bytes != installed_bytes {
            return Ok(false);
        }
    }
    Ok(true)
}

fn copy_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    fs::create_dir(destination)?;
    fs::set_permissions(destination, metadata.permissions())?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source_path)?, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
            fs::set_permissions(
                &destination_path,
                fs::symlink_metadata(&source_path)?.permissions(),
            )?;
        }
    }
    Ok(())
}

pub fn node_repl_client_path() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe().context("Waku executable path is unavailable")?;
    let macos = executable
        .parent()
        .ok_or_else(|| anyhow!("Waku executable has no parent directory"))?;
    let contents = macos
        .parent()
        .ok_or_else(|| anyhow!("Waku app bundle is malformed"))?;
    let path = contents.join("Resources").join("Waku Computer Use Client.mjs");
    if !path.is_file() {
        bail!("Computer Use node_repl client is missing from this Waku build")
    }
    Ok(path)
}

pub fn skill_root_path() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe().context("Waku executable path is unavailable")?;
    let macos = executable
        .parent()
        .ok_or_else(|| anyhow!("Waku executable has no parent directory"))?;
    let contents = macos
        .parent()
        .ok_or_else(|| anyhow!("Waku app bundle is malformed"))?;
    let path = contents.join("Resources").join("skills");
    if !path.join("waku-computer-use").join("SKILL.md").is_file() {
        bail!("Waku Computer Use skill is missing from this Waku build")
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_grants_preserve_bundle_identity() {
        let target = ComputerTarget {
            window_id: 42,
            bundle_id: "net.imput.helium".into(),
            team_id: Some("S4Q33XPHB4".into()),
            app_name: "Helium".into(),
            window_title: "Window".into(),
            width: 1440,
            height: 823,
        };
        let grant = ComputerAppGrant {
            bundle_id: "net.imput.helium".into(),
            app_name: "Helium".into(),
        };
        assert_eq!(target.grant_key(), grant.key());
        assert!(target.persistable());
    }

}

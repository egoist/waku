use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const NAMESPACE: &str = "waku_computer";
const MAX_ACTIONS: usize = 16;
const MAX_TEXT_BYTES: usize = 10_000;
const MAX_HELPER_OUTPUT_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerPermissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

impl ComputerPermissions {
    pub fn ready(&self) -> bool {
        self.screen_recording && self.accessibility
    }
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
        match self.team_id.as_deref() {
            Some(team_id) if !team_id.is_empty() => format!("{}:{team_id}", self.bundle_id),
            _ => self.bundle_id.clone(),
        }
    }

    pub fn persistable(&self) -> bool {
        self.team_id
            .as_ref()
            .is_some_and(|team_id| !team_id.is_empty())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerAppGrant {
    pub bundle_id: String,
    pub team_id: String,
    pub app_name: String,
}

impl ComputerAppGrant {
    pub fn key(&self) -> String {
        format!("{}:{}", self.bundle_id, self.team_id)
    }
}

#[derive(Clone, Debug)]
pub struct ComputerToolRequest {
    pub rpc_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
}

impl ComputerToolRequest {
    pub fn target(&self) -> Option<ComputerTarget> {
        serde_json::from_value(self.arguments.get("target")?.clone()).ok()
    }

    pub fn requires_sensitive_confirmation(&self) -> bool {
        self.arguments
            .get("actions")
            .and_then(Value::as_array)
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    matches!(
                        action.get("type").and_then(Value::as_str),
                        Some("type") | Some("keypress")
                    )
                })
            })
    }

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
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ComputerUseState {
    pub call_id: String,
    pub target: Option<ComputerTarget>,
    pub summary: String,
    pub phase: ComputerUsePhase,
    pub visible: bool,
    pub screenshot: Option<Arc<gpui::Image>>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PendingComputerApproval {
    pub request: ComputerToolRequest,
    pub target: ComputerTarget,
    pub sensitive: bool,
}

#[derive(Clone, Debug)]
pub struct ComputerToolOutput {
    pub success: bool,
    pub content_items: Vec<Value>,
    pub state: ComputerUseState,
    pub permissions: Option<ComputerPermissions>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelperResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    permissions: Option<ComputerPermissions>,
    #[serde(default)]
    targets: Vec<ComputerTarget>,
    #[serde(default)]
    target: Option<ComputerTarget>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

pub fn dynamic_tools() -> Value {
    json!([{
        "type": "namespace",
        "name": NAMESPACE,
        "description": "Observe and control a visible macOS app window through Waku. Use list_targets first, then pass the returned target object unchanged to use. The user sees and controls every app grant.",
        "tools": [
            {
                "type": "function",
                "name": "status",
                "description": "Check whether Waku has Screen Recording and Accessibility access.",
                "inputSchema": {"type": "object", "additionalProperties": false}
            },
            {
                "type": "function",
                "name": "list_targets",
                "description": "List visible app windows that may be controlled. Returns stable app identity and the current window size. Call again if a window changes or disappears.",
                "inputSchema": {"type": "object", "additionalProperties": false}
            },
            {
                "type": "function",
                "name": "use",
                "description": "Inspect and interact with one listed app window. Coordinates are pixels in the most recent screenshot for that window. Returns a fresh screenshot after the actions.",
                "inputSchema": {
                    "type": "object",
                    "required": ["target", "actions"],
                    "additionalProperties": false,
                    "properties": {
                        "target": {
                            "type": "object",
                            "description": "Copy this object unchanged from list_targets.",
                            "required": ["windowId", "bundleId", "appName", "windowTitle", "width", "height"],
                            "additionalProperties": false,
                            "properties": {
                                "windowId": {"type": "integer", "minimum": 1},
                                "bundleId": {"type": "string"},
                                "teamId": {"type": ["string", "null"]},
                                "appName": {"type": "string"},
                                "windowTitle": {"type": "string"},
                                "width": {"type": "integer", "minimum": 1},
                                "height": {"type": "integer", "minimum": 1}
                            }
                        },
                        "actions": {
                            "type": "array",
                            "maxItems": MAX_ACTIONS,
                            "description": "A short ordered batch. Use an empty array to inspect without input.",
                            "items": {
                                "type": "object",
                                "required": ["type"],
                                "additionalProperties": false,
                                "properties": {
                                    "type": {"type": "string", "enum": ["click", "double_click", "move", "drag", "scroll", "type", "keypress", "wait"]},
                                    "x": {"type": "number"},
                                    "y": {"type": "number"},
                                    "toX": {"type": "number"},
                                    "toY": {"type": "number"},
                                    "deltaX": {"type": "number"},
                                    "deltaY": {"type": "number"},
                                    "text": {"type": "string", "maxLength": MAX_TEXT_BYTES},
                                    "key": {"type": "string"},
                                    "modifiers": {"type": "array", "items": {"type": "string", "enum": ["command", "control", "option", "shift"]}, "uniqueItems": true},
                                    "durationMs": {"type": "integer", "minimum": 0, "maximum": 2000}
                                }
                            }
                        }
                    }
                }
            }
        ]
    }])
}

pub fn validate_request(request: &ComputerToolRequest) -> anyhow::Result<()> {
    match request.tool.as_str() {
        "status" | "list_targets" => {
            if request
                .arguments
                .as_object()
                .is_none_or(|arguments| arguments.is_empty())
            {
                Ok(())
            } else {
                bail!("{} takes no arguments", request.tool)
            }
        }
        "use" => {
            let target = request
                .target()
                .ok_or_else(|| anyhow!("use requires a valid target from list_targets"))?;
            if is_blocked_bundle_id(&target.bundle_id) {
                bail!("Waku cannot control {}", target.app_name);
            }
            let actions = request
                .arguments
                .get("actions")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("use requires an actions array"))?;
            if actions.len() > MAX_ACTIONS {
                bail!("a computer-use call may contain at most {MAX_ACTIONS} actions");
            }
            for action in actions {
                validate_action(action)?;
            }
            Ok(())
        }
        _ => bail!("unknown computer-use tool `{}`", request.tool),
    }
}

fn validate_action(action: &Value) -> anyhow::Result<()> {
    let kind = action
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("every action requires a type"))?;
    let coordinate = |name: &str| {
        action
            .get(name)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| anyhow!("{kind} requires a finite {name}"))
    };
    match kind {
        "click" | "double_click" | "move" => {
            coordinate("x")?;
            coordinate("y")?;
        }
        "drag" => {
            coordinate("x")?;
            coordinate("y")?;
            coordinate("toX")?;
            coordinate("toY")?;
        }
        "scroll" => {
            coordinate("deltaX")?;
            coordinate("deltaY")?;
        }
        "type" => {
            let text = action
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("type requires text"))?;
            if text.len() > MAX_TEXT_BYTES {
                bail!("typed text is too long");
            }
        }
        "keypress" => {
            if action.get("key").and_then(Value::as_str).is_none() {
                bail!("keypress requires key");
            }
        }
        "wait" => {
            let duration = action
                .get("durationMs")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            if duration > 2_000 {
                bail!("wait is limited to 2000ms");
            }
        }
        _ => bail!("unsupported computer-use action `{kind}`"),
    }
    Ok(())
}

pub fn is_blocked_bundle_id(bundle_id: &str) -> bool {
    let normalized = bundle_id.to_ascii_lowercase();
    normalized.starts_with("codes.waku")
        || matches!(
            normalized.as_str(),
            "com.apple.loginwindow"
                | "com.apple.securityagent"
                | "com.apple.systempreferences"
                | "com.apple.systemsettings"
                | "com.openai.chat"
                | "com.apple.terminal"
                | "com.apple.keychainaccess"
                | "com.googlecode.iterm2"
                | "com.mitchellh.ghostty"
                | "org.alacritty"
                | "com.1password.1password"
                | "com.1password.1password7"
                | "com.bitwarden.desktop"
                | "com.lastpass.lastpass"
        )
}

pub fn execute_tool(
    request: ComputerToolRequest,
    active_helper_pid: Arc<AtomicU32>,
) -> ComputerToolOutput {
    let summary = request.summary();
    let target = request.target();
    let failed = |error: String| ComputerToolOutput {
        success: false,
        content_items: vec![json!({"type": "inputText", "text": error})],
        state: ComputerUseState {
            call_id: request.call_id.clone(),
            target: target.clone(),
            summary: summary.clone(),
            phase: ComputerUsePhase::Failed,
            visible: request.tool == "use",
            screenshot: None,
            error: Some(error),
        },
        permissions: None,
    };

    if let Err(error) = validate_request(&request) {
        return failed(error.to_string());
    }

    let operation = match request.tool.as_str() {
        "status" => json!({"operation": "status"}),
        "list_targets" => json!({"operation": "listTargets"}),
        "use" => json!({
            "operation": "use",
            "target": target,
            "actions": request.arguments.get("actions").cloned().unwrap_or_else(|| json!([]))
        }),
        _ => unreachable!("validated above"),
    };
    let response = match invoke_helper(&operation, active_helper_pid) {
        Ok(response) => response,
        Err(error) => return failed(error.to_string()),
    };
    if !response.success {
        return failed(
            response
                .error
                .unwrap_or_else(|| "Computer use failed".into()),
        );
    }

    let screenshot = response
        .image_url
        .as_deref()
        .and_then(decode_screenshot_data_url);

    let mut content_items = Vec::new();
    match request.tool.as_str() {
        "status" => {
            let permissions = response.permissions.clone().unwrap_or_default();
            content_items.push(json!({
                "type": "inputText",
                "text": serde_json::to_string(&json!({"permissions": permissions})).unwrap()
            }));
        }
        "list_targets" => {
            content_items.push(json!({
                "type": "inputText",
                "text": serde_json::to_string(&json!({"targets": response.targets})).unwrap()
            }));
        }
        "use" => {
            content_items.push(json!({
                "type": "inputText",
                "text": response.summary.clone().unwrap_or_else(|| "Computer actions completed.".into())
            }));
            if let Some(image_url) = response.image_url.as_ref() {
                content_items.push(json!({"type": "inputImage", "imageUrl": image_url}));
            }
        }
        _ => {}
    }

    ComputerToolOutput {
        success: true,
        content_items,
        state: ComputerUseState {
            call_id: request.call_id,
            target: response.target.or(target),
            summary,
            phase: ComputerUsePhase::Completed,
            visible: request.tool == "use",
            screenshot,
            error: None,
        },
        permissions: response.permissions,
    }
}

fn decode_screenshot_data_url(data_url: &str) -> Option<Arc<gpui::Image>> {
    const PNG_PREFIX: &str = "data:image/png;base64,";
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_url.strip_prefix(PNG_PREFIX)?)
        .ok()?;
    (!bytes.is_empty()).then(|| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes)))
}

pub fn probe_permissions(prompt: bool) -> anyhow::Result<ComputerPermissions> {
    let operation = if prompt {
        json!({"operation": "requestPermissions"})
    } else {
        json!({"operation": "status"})
    };
    let response = invoke_helper(&operation, Arc::new(AtomicU32::new(0)))?;
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

fn invoke_helper(
    operation: &Value,
    active_helper_pid: Arc<AtomicU32>,
) -> anyhow::Result<HelperResponse> {
    if let Some(helper) = std::env::var_os("WAKU_COMPUTER_USE_HELPER") {
        return invoke_helper_direct(&PathBuf::from(helper), operation, active_helper_pid);
    }
    let bundled_helper = helper_app_path()?;
    let installed_helper = install_helper_app(&bundled_helper)?;
    invoke_helper_app(&installed_helper, operation, active_helper_pid)
}

fn invoke_helper_direct(
    helper: &Path,
    operation: &Value,
    active_helper_pid: Arc<AtomicU32>,
) -> anyhow::Result<HelperResponse> {
    let mut child = Command::new(&helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {}", helper.display()))?;
    let pid = child.id();
    active_helper_pid.store(pid, Ordering::SeqCst);
    let watchdog_pid = active_helper_pid.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(30));
        if watchdog_pid
            .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }
    });
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

struct HelperSocket {
    directory: PathBuf,
    path: PathBuf,
    listener: UnixListener,
}

impl HelperSocket {
    fn bind() -> anyhow::Result<Self> {
        // Keep the path short enough for sockaddr_un and inaccessible to
        // other users. The helper authenticates the listening Waku process
        // from the socket's kernel-provided peer PID before accepting a call.
        let directory = PathBuf::from("/tmp").join(format!(
            "waku-computer-use-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .with_context(|| format!("could not create {}", directory.display()))?;
        let path = directory.join("request.sock");
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("could not bind {}", path.display()))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            directory,
            path,
            listener,
        })
    }
}

impl Drop for HelperSocket {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn invoke_helper_app(
    helper: &Path,
    operation: &Value,
    active_helper_pid: Arc<AtomicU32>,
) -> anyhow::Result<HelperResponse> {
    let socket = HelperSocket::bind()?;
    let helper_name = helper
        .file_stem()
        .ok_or_else(|| anyhow!("Computer Use helper name is invalid"))?;
    let helper_executable =
        fs::canonicalize(helper.join("Contents").join("MacOS").join(helper_name))
            .context("Computer Use helper executable is unavailable")?;
    // Launch Services gives this standalone app its own macOS privacy identity.
    // Directly spawning an embedded executable makes the containing Waku app
    // the responsible process instead.
    let mut launch = HelperLaunch::new(launch_helper_app(helper, &socket.path)?);
    let launcher_pid = launch.launcher_pid;
    let _active_helper_reset = ActiveHelperReset(active_helper_pid.clone());
    active_helper_pid.store(launcher_pid, Ordering::SeqCst);

    let accept_started = Instant::now();
    let (mut stream, helper_pid) = loop {
        match socket.listener.accept() {
            Ok((stream, _)) => {
                if let Some(peer_pid) = peer_pid(&stream)
                    && process_executable(peer_pid).as_deref() == Some(helper_executable.as_path())
                {
                    break (stream, peer_pid);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error).context("computer-use helper IPC failed"),
        }
        if let Some(status) = launch.launcher.try_wait()? {
            active_helper_pid.store(0, Ordering::SeqCst);
            bail!("computer-use helper did not connect ({status})");
        }
        if accept_started.elapsed() >= Duration::from_secs(5) {
            active_helper_pid.store(0, Ordering::SeqCst);
            bail!("computer-use helper did not connect in time");
        }
        thread::sleep(Duration::from_millis(10));
    };

    if active_helper_pid
        .compare_exchange(launcher_pid, helper_pid, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        bail!("computer-use helper was cancelled");
    }
    launch.helper_pid = Some(helper_pid);

    let watchdog_pid = active_helper_pid.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(30));
        if watchdog_pid
            .compare_exchange(helper_pid, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            terminate_process(helper_pid);
        }
    });

    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let exchange = (|| -> anyhow::Result<Vec<u8>> {
        serde_json::to_writer(&mut stream, operation)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        let mut response = Vec::new();
        (&mut stream)
            .take((MAX_HELPER_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut response)?;
        if response.len() > MAX_HELPER_OUTPUT_BYTES {
            bail!("computer-use helper returned too much data");
        }
        Ok(response)
    })();

    let response = match exchange {
        Ok(response) => response,
        Err(error) => {
            active_helper_pid.store(0, Ordering::SeqCst);
            bail!("Computer Use IPC failed: {error}");
        }
    };
    let decoded =
        serde_json::from_slice(&response).context("computer-use helper returned invalid JSON")?;
    // The response is complete once the helper closes its IPC channel. End
    // the one-shot helper immediately instead of leaving Launch Services or
    // ScreenCaptureKit time to retain its capture identity after the turn.
    terminate_process(helper_pid);
    let _ = launch.finish()?;
    active_helper_pid.store(0, Ordering::SeqCst);
    Ok(decoded)
}

fn launch_helper_app(helper: &Path, socket_path: &Path) -> anyhow::Result<Child> {
    Command::new("/usr/bin/open")
        .args(["-n", "-W", "-g"])
        .arg(helper)
        .arg("--args")
        .arg("--socket")
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {}", helper.display()))
}

struct HelperLaunch {
    launcher: Child,
    launcher_pid: u32,
    helper_pid: Option<u32>,
    finished: bool,
}

struct ActiveHelperReset(Arc<AtomicU32>);

impl Drop for ActiveHelperReset {
    fn drop(&mut self) {
        self.0.store(0, Ordering::SeqCst);
    }
}

impl HelperLaunch {
    fn new(launcher: Child) -> Self {
        let launcher_pid = launcher.id();
        Self {
            launcher,
            launcher_pid,
            helper_pid: None,
            finished: false,
        }
    }

    fn finish(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.launcher.wait()?;
        self.finished = true;
        Ok(status)
    }
}

impl Drop for HelperLaunch {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(helper_pid) = self.helper_pid {
            terminate_process(helper_pid);
        }
        terminate_process(self.launcher_pid);
        let _ = self.launcher.wait();
    }
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    unsafe extern "C" {
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
    }

    let mut buffer = [0u8; 4096];
    let length = unsafe {
        proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    let end = buffer.iter().position(|byte| *byte == 0)?;
    fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(&buffer[..end]))).ok()
}

#[cfg(not(target_os = "macos"))]
fn process_executable(_: u32) -> Option<PathBuf> {
    None
}

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

#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut length,
        )
    };
    (result == 0 && pid > 0).then_some(pid as u32)
}

#[cfg(not(target_os = "macos"))]
fn peer_pid(_: &UnixStream) -> Option<u32> {
    None
}

fn terminate_process(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status();
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

pub fn cancel_helper(active_helper_pid: &AtomicU32) {
    let pid = active_helper_pid.swap(0, Ordering::SeqCst);
    if pid != 0 {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn dynamic_tool_contract_uses_codex_namespace_shape() {
        let tools = dynamic_tools();
        assert_eq!(
            tools.pointer("/0/type").and_then(Value::as_str),
            Some("namespace")
        );
        assert_eq!(
            tools.pointer("/0/name").and_then(Value::as_str),
            Some(NAMESPACE)
        );
        assert_eq!(
            tools.pointer("/0/tools/2/name").and_then(Value::as_str),
            Some("use")
        );
        assert_eq!(
            tools
                .pointer("/0/tools/2/inputSchema/properties/actions/maxItems")
                .and_then(Value::as_u64),
            Some(MAX_ACTIONS as u64)
        );
    }

    #[test]
    fn sensitive_inputs_are_detected() {
        let request = ComputerToolRequest {
            rpc_id: "1".into(),
            call_id: "call".into(),
            tool: "use".into(),
            arguments: json!({"actions": [{"type": "type", "text": "hello"}]}),
        };
        assert!(request.requires_sensitive_confirmation());
    }

    #[test]
    fn privileged_and_self_targets_are_blocked() {
        assert!(is_blocked_bundle_id("codes.waku.dev"));
        assert!(is_blocked_bundle_id("com.apple.SecurityAgent"));
        assert!(is_blocked_bundle_id("com.apple.Terminal"));
        assert!(!is_blocked_bundle_id("com.apple.Safari"));
    }

    #[test]
    fn malformed_actions_are_rejected_before_the_helper() {
        let request = ComputerToolRequest {
            rpc_id: "1".into(),
            call_id: "call".into(),
            tool: "use".into(),
            arguments: json!({
                "target": {
                    "windowId": 1,
                    "bundleId": "com.apple.Safari",
                    "appName": "Safari",
                    "windowTitle": "Example",
                    "width": 100,
                    "height": 100
                },
                "actions": [{"type": "click", "x": 1}]
            }),
        };
        assert!(validate_request(&request).is_err());
    }

    #[test]
    #[ignore = "launches the signed macOS helper"]
    fn signed_helper_rejects_an_untrusted_socket_client_cleanly() {
        let bundled_helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/debug/Waku Debug.app/Contents/Helpers/Waku Debug Computer Use.app");
        let helper = install_helper_app(&bundled_helper).unwrap();
        let response = invoke_helper_app(
            &helper,
            &json!({"operation": "status"}),
            Arc::new(AtomicU32::new(0)),
        )
        .unwrap();
        assert!(!response.success);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("did not come from"))
        );
    }

    #[test]
    #[ignore = "launches the signed macOS helper through Launch Services"]
    fn launch_services_makes_the_installed_helper_self_responsible() {
        type GetResponsiblePid = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

        let bundled_helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/debug/Waku Debug.app/Contents/Helpers/Waku Debug Computer Use.app");
        let helper = install_helper_app(&bundled_helper).unwrap();
        let socket = HelperSocket::bind().unwrap();
        let mut launcher = Command::new("/usr/bin/open")
            .args(["-n", "-W", "-g"])
            .arg(&helper)
            .arg("--args")
            .arg("--socket")
            .arg(&socket.path)
            .spawn()
            .unwrap();

        let started = Instant::now();
        let (mut stream, _) = loop {
            match socket.listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("helper connection failed: {error}"),
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(10));
        };
        let helper_pid = peer_pid(&stream).unwrap() as libc::pid_t;
        let symbol_name = CString::new("responsibility_get_pid_responsible_for_pid").unwrap();
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol_name.as_ptr()) };
        assert!(!symbol.is_null());
        let get_responsible_pid: GetResponsiblePid = unsafe { std::mem::transmute(symbol) };
        assert_eq!(unsafe { get_responsible_pid(helper_pid) }, helper_pid);

        stream.set_nonblocking(false).unwrap();
        serde_json::to_writer(&mut stream, &json!({"operation": "status"})).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response: HelperResponse = serde_json::from_slice(&response).unwrap();
        assert!(!response.success);
        assert!(launcher.wait().unwrap().success());
    }
}

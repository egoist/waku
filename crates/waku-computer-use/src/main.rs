//! Portable Windows/Linux host for the in-process Cua Driver SDK.
//! macOS uses the same ABI from its signed Launch Services Swift host.

mod sdk;

use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const MAX_MESSAGE_BYTES: u64 = 32 * 1024 * 1024;

fn main() -> Result<()> {
    let executable = std::env::current_exe()?;
    let directory = executable.parent().context("helper has no directory")?;
    let mode = std::env::args().nth(1);
    // macOS uses the Swift AppKit host; this portable entrypoint only enables
    // native overlay threads on Windows/Linux. Diagnostics never open an overlay.
    let cursor = mode.as_deref() == Some("mcp") && !cfg!(target_os = "macos");
    let mut driver = sdk::Driver::load(&directory.join(library_name()), cursor)?;
    match mode.as_deref() {
        Some("mcp") => serve(&driver)?,
        // Diagnostic entrypoint used by packaging checks; no capture or input.
        Some("list-tools") => write_json(&mut io::stdout().lock(), &driver.list_tools()?)?,
        _ => bail!("expected mcp or list-tools"),
    }
    driver.shutdown()
}

fn library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "cua_driver_sdk.dll"
    } else if cfg!(target_os = "macos") {
        "libcua_driver_sdk.dylib"
    } else {
        "libcua_driver_sdk.so"
    }
}

fn serve(driver: &sdk::Driver) -> Result<()> {
    let registration = Registration::new()?;
    let disconnected = Arc::new(AtomicBool::new(false));
    let input_disconnected = disconnected.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("cua-stdin".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                let mut line = String::new();
                let result = input
                    .by_ref()
                    .take(MAX_MESSAGE_BYTES + 1)
                    .read_line(&mut line);
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.len() as u64 > MAX_MESSAGE_BYTES => break,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
            input_disconnected.store(true, Ordering::Release);
        })?;
    let cancelled = || disconnected.load(Ordering::Acquire) || registration.cancelled();
    let mut output = io::stdout().lock();
    while !cancelled() {
        let line = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let request: Value = serde_json::from_str(&line)?;
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = dispatch(driver, &request, &cancelled);
        let response = match result {
            Ok(result) => {
                if request.pointer("/params/name").and_then(Value::as_str)
                    == Some("get_window_state")
                {
                    registration.publish_preview(&result);
                }
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            Err(error) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": error.to_string()}})
            }
        };
        write_json(&mut output, &response)?;
    }
    Ok(())
}

fn dispatch(driver: &sdk::Driver, request: &Value, cancelled: &impl Fn() -> bool) -> Result<Value> {
    match request.get("method").and_then(Value::as_str) {
        Some("initialize") => Ok(json!({
            "protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
            "serverInfo": {"name": "Waku Cua Driver", "version": "0.28.0"}
        })),
        Some("tools/list") => driver.list_tools(),
        Some("tools/call") => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .context("tools/call requires a name")?;
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            driver.call(name, &arguments, cancelled)
        }
        Some("ping") => Ok(json!({})),
        _ => bail!("unsupported MCP method"),
    }
}

fn write_json(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

struct Registration {
    directory: Option<PathBuf>,
    pid: u32,
}

impl Registration {
    fn new() -> Result<Self> {
        let directory = std::env::var_os("WAKU_COMPUTER_USE_PROCESS_DIRECTORY").map(PathBuf::from);
        let registration = Self {
            directory,
            pid: std::process::id(),
        };
        if let Some(directory) = &registration.directory {
            fs::write(directory.join(registration.pid.to_string()), b"")?;
        }
        Ok(registration)
    }

    fn cancelled(&self) -> bool {
        self.directory
            .as_ref()
            .is_some_and(|directory| directory.join(format!("cancel-{}", self.pid)).exists())
    }

    fn publish_preview(&self, result: &Value) {
        let Some(directory) = &self.directory else {
            return;
        };
        let Some(preview) = preview_update(result) else {
            return;
        };
        let id = preview["target"]["windowId"].as_u64().unwrap();
        let path = directory.join(format!("preview-{id}.json"));
        // Windows cannot atomically rename over an existing file with std::fs.
        // The monitor ignores incomplete JSON and reads the completed revision.
        let _ = write_json_file(&path, &preview);
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(directory) = &self.directory {
            let _ = fs::remove_file(directory.join(self.pid.to_string()));
            let _ = fs::remove_file(directory.join(format!("cancel-{}", self.pid)));
        }
    }
}

fn write_json_file(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}

fn preview_update(result: &Value) -> Option<Value> {
    if result["isError"] == true {
        return None;
    }
    let state = result.get("structuredContent")?;
    if state["screenshot_frame_valid"] == false {
        return None;
    }
    let id = state["window_id"].as_u64()?;
    let width = state["screenshot_width"].as_u64()?;
    let height = state["screenshot_height"].as_u64()?;
    let image = result["content"]
        .as_array()?
        .iter()
        .find(|image| image["type"] == "image" && image["mimeType"] == "image/png")?;
    let data = image["data"].as_str()?;
    Some(json!({
        "target": {"windowId": id, "bundleId": "", "appName": state["app_name"].as_str().unwrap_or("App"),
                   "windowTitle": state["window_title"].as_str().unwrap_or(""), "width": width, "height": height},
        "imageUrl": format!("data:image/png;base64,{data}")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_preserve_native_window_handles_and_capture_dimensions() {
        let state = json!({"structuredContent": {"window_id": 8589934593_u64, "screenshot_width": 1200,
            "screenshot_height": 800, "app_name": "Editor", "screenshot_frame_valid": true},
            "content": [{"type": "image", "mimeType": "image/png", "data": "cG5n"}]});
        let preview = preview_update(&state).unwrap();
        assert_eq!(preview["target"]["windowId"], 8589934593_u64);
        assert_eq!(preview["target"]["width"], 1200);
        assert_eq!(preview["imageUrl"], "data:image/png;base64,cG5n");
        let mut failed = state.clone();
        failed["isError"] = json!(true);
        assert!(preview_update(&failed).is_none());
        failed = state;
        failed["structuredContent"]["screenshot_frame_valid"] = json!(false);
        assert!(preview_update(&failed).is_none());
    }

    #[test]
    fn missing_sdk_fails_without_searching_the_environment() {
        assert!(sdk::Driver::load(Path::new("/nonexistent/waku-cua-sdk"), false).is_err());
    }
}

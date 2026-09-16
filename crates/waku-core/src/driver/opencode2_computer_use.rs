//! One runtime MCP connection per OpenCode location, with session-scoped
//! kernels and native helper ownership behind it. The adopted service is
//! never restarted and no OpenCode configuration file is changed.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use parking_lot::Mutex;
use serde_json::{Value, json};
use uuid::Uuid;

use super::computer_use::{ComputerUseConfig, ComputerUseRuntime, create_process_directory};
use crate::driver::DriverEventSender;
use crate::opencode2_api;
use crate::opencode2_service::Opencode2Service;

pub(super) const INSTRUCTION_KEY: &str = "waku-computer-use";

pub(super) fn tool_identity(name: &str) -> Option<(&str, &str)> {
    let (server, tool) = name
        .strip_suffix("_js_reset")
        .map(|server| (server, "js_reset"))
        .or_else(|| name.strip_suffix("_js").map(|server| (server, "js")))?;
    let id = server.strip_prefix("waku_js_repl_")?;
    (id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some((server, tool))
}

type BridgeKey = (usize, String);
type Bridges = Mutex<HashMap<BridgeKey, Weak<McpBridge>>>;
static BRIDGES: OnceLock<Bridges> = OnceLock::new();

struct McpBridge {
    service: Arc<Opencode2Service>,
    directory: String,
    server: String,
    sessions_directory: PathBuf,
    config: Value,
    registration: Mutex<()>,
}

impl McpBridge {
    fn acquire(
        service: &Arc<Opencode2Service>,
        directory: &str,
        config: &ComputerUseConfig,
    ) -> anyhow::Result<Arc<Self>> {
        let key = (Arc::as_ptr(service) as usize, directory.to_owned());
        let mut bridges = BRIDGES.get_or_init(Default::default).lock();
        bridges.retain(|_, bridge| bridge.strong_count() > 0);
        if let Some(bridge) = bridges.get(&key).and_then(Weak::upgrade) {
            return Ok(bridge);
        }
        let sessions_directory = create_process_directory()?;
        let bridge = Arc::new(Self {
            service: service.clone(),
            directory: directory.to_owned(),
            server: format!("waku_js_repl_{}", Uuid::new_v4().simple()),
            config: json!({
                "type": "local",
                "command": [config.repl_path],
                "cwd": directory,
                // This is already a JavaScript tool. Keep OpenCode's extra
                // execute/codemode wrapper out of the agent-facing API.
                "codemode": false,
                "environment": {
                    "WAKU_COMPUTER_USE_SESSIONS_DIRECTORY": sessions_directory,
                },
                "timeout": {"startup": 10000, "catalog": 10000, "execution": 300000},
            }),
            sessions_directory,
            registration: Mutex::new(()),
        });
        bridge.ensure_connected()?;
        bridges.insert(key, Arc::downgrade(&bridge));
        Ok(bridge)
    }

    fn ensure_connected(&self) -> anyhow::Result<()> {
        let _registration = self.registration.lock();
        let endpoint = self.service.endpoint();
        let status = opencode2_api::list_mcp(&endpoint, &self.directory)?
            .into_iter()
            .find(|server| server["name"] == self.server);
        if status
            .as_ref()
            .is_some_and(|server| server["status"]["status"] == "connected")
        {
            return Ok(());
        }
        opencode2_api::add_mcp(&endpoint, &self.directory, &self.server, &self.config)
            .context("could not connect OpenCode 2 to Waku Computer Use")?;
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let status = opencode2_api::list_mcp(&endpoint, &self.directory)?
                .into_iter()
                .find(|server| server["name"] == self.server);
            match status
                .as_ref()
                .and_then(|server| server["status"]["status"].as_str())
            {
                Some("connected") => return Ok(()),
                Some("failed" | "disabled" | "needs_auth") => bail!(
                    "OpenCode 2 could not start Waku Computer Use: {}",
                    status
                        .as_ref()
                        .and_then(|server| server["status"]["error"].as_str())
                        .unwrap_or("MCP server unavailable")
                ),
                _ if Instant::now() >= deadline => {
                    bail!("OpenCode 2 timed out connecting Waku Computer Use")
                }
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_directory.join(format!(
            "{}.json",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(session_id),
        ))
    }
}

impl Drop for McpBridge {
    fn drop(&mut self) {
        let _ = opencode2_api::remove_mcp(&self.service.endpoint(), &self.directory, &self.server);
        let _ = fs::remove_dir_all(&self.sessions_directory);
    }
}

pub(super) struct OpenCode2ComputerUse {
    runtime: ComputerUseRuntime,
    bridge: Arc<McpBridge>,
    session_id: String,
}

impl OpenCode2ComputerUse {
    pub(super) fn start(
        service: &Arc<Opencode2Service>,
        directory: &str,
        session_id: &str,
        events: DriverEventSender,
    ) -> anyhow::Result<Self> {
        let runtime = ComputerUseRuntime::start(events)?;
        let bridge = McpBridge::acquire(service, directory, &runtime.config)?;
        let this = Self {
            runtime,
            bridge,
            session_id: session_id.to_owned(),
        };
        // Serialize registration and cleanup for a resumed native session so
        // an older driver cannot revoke the driver replacing it.
        let registration = this.bridge.registration.lock();
        let path = this.bridge.session_path(session_id);
        let temporary = path.with_extension("tmp");
        fs::write(
            &temporary,
            serde_json::to_vec(&json!({
                "server_path": this.runtime.config.server_path,
                "process_directory": this.runtime.config.process_directory,
                "cwd": directory,
            }))?,
        )?;
        fs::rename(&temporary, &path)?;
        let instructions = format!(
            "Computer Use is enabled for this Waku session. Use the `js` and `js_reset` tools from MCP server `{}`. These are direct MCP tools; call them directly. The host routes calls to this session automatically. Before using Computer Use, read the bundled skill at {} for the complete native API and operating instructions. Initialize with `await setupComputerUseRuntime({{ globals: globalThis }})`, then call methods such as `cua.list_apps()` directly. Use `jsRepl.write(...)` for output and `await jsRepl.emitImage(...)` for images. Bindings persist until `js_reset`. The skill documents all available methods; there is no public tool-discovery or generic dispatch API.",
            this.bridge.server,
            this.runtime.config.skill_path.display(),
        );
        let attached = opencode2_api::put_instruction_entry(
            &service.endpoint(),
            session_id,
            INSTRUCTION_KEY,
            &instructions,
        )
        .map_err(|error| anyhow!("could not attach OpenCode 2 Computer Use instructions: {error}"));
        drop(registration);
        attached?;
        Ok(this)
    }

    pub(super) fn ensure_connected(&self) -> anyhow::Result<()> {
        self.bridge.ensure_connected()
    }

    pub(super) fn stop(&self) {
        self.runtime.stop();
    }
}

impl Drop for OpenCode2ComputerUse {
    fn drop(&mut self) {
        // Worker ownership keeps these blocking cleanup calls off the UI
        // thread. Removing the registration revokes this session immediately;
        // the shared bridge stays up while any other Waku session uses it.
        let _registration = self.bridge.registration.lock();
        let path = self.bridge.session_path(&self.session_id);
        let owns_registration = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|config| {
                config["process_directory"]
                    == self
                        .runtime
                        .config
                        .process_directory
                        .to_string_lossy()
                        .as_ref()
            });
        if owns_registration {
            let _ = fs::remove_file(path);
            let _ = opencode2_api::remove_instruction_entry(
                &self.bridge.service.endpoint(),
                &self.session_id,
                INSTRUCTION_KEY,
            );
        }
    }
}

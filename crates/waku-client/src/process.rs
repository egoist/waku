use std::collections::HashSet;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::SystemTime;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DaemonClient;
use waku_protocol::{
    APP_EXECUTABLE_ENV, Command, DAEMON_TOKEN_ENV, DaemonReady, DaemonSettings, PROTOCOL_VERSION,
    ReplayCursor, ResponsePayload,
};
const START_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const REBUILD_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_EXPOSED_DAEMON_PORT: u16 = 34_123;

/// Desktop-owned launch configuration for the daemon it supervises.
///
/// Provider settings belong to the daemon and live in `settings.json`; this
/// is an app preference because it controls how the desktop launches its own
/// child process. The bearer token is intentionally stable across daemon-only
/// rebuilds and desktop relaunches so a configured web client keeps working.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonExposureSettings {
    pub enabled: bool,
    pub port: u16,
    pub allowed_origins: Vec<String>,
    pub token: String,
}

impl Default for DaemonExposureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_EXPOSED_DAEMON_PORT,
            allowed_origins: vec!["http://localhost:3001".into()],
            token: Self::new_token(),
        }
    }
}

impl DaemonExposureSettings {
    pub fn new_token() -> String {
        Uuid::new_v4().simple().to_string()
    }

    pub fn ensure_token(&mut self) -> bool {
        if !self.token.trim().is_empty() {
            return false;
        }
        self.token = Self::new_token();
        true
    }

    pub fn allowed_origins_text(&self) -> String {
        self.allowed_origins.join(", ")
    }

    pub fn with_allowed_origins_text(mut self, text: &str) -> anyhow::Result<Self> {
        self.allowed_origins = parse_allowed_origins(text)?;
        Ok(self)
    }

    pub fn validate(mut self) -> anyhow::Result<Self> {
        if self.port == 0 {
            bail!("daemon port must be between 1 and 65535");
        }
        if self.token.trim().is_empty() {
            bail!("daemon authentication token is empty");
        }
        self.allowed_origins = parse_allowed_origins(&self.allowed_origins_text())?;
        Ok(self)
    }

    fn bind_address(&self) -> String {
        if self.enabled {
            format!("0.0.0.0:{}", self.port)
        } else {
            "127.0.0.1:0".into()
        }
    }
}

/// Parse the comma-separated exact browser origins edited by the desktop.
/// Browser Origin headers contain only an HTTP(S) origin, never a path.
pub fn parse_allowed_origins(text: &str) -> anyhow::Result<Vec<String>> {
    let mut origins = Vec::new();
    let mut seen = HashSet::new();
    for candidate in text
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let url = url::Url::parse(candidate)
            .with_context(|| format!("invalid browser origin {candidate:?}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            bail!(
                "browser origin {candidate:?} must be an exact http:// or https:// origin without a path"
            );
        }
        let origin = url.origin().ascii_serialization();
        if origin == "null" {
            bail!("browser origin {candidate:?} is not a network origin");
        }
        if seen.insert(origin.clone()) {
            origins.push(origin);
        }
    }
    Ok(origins)
}

pub struct DaemonProcess {
    client: DaemonClient,
    child: Child,
    address: String,
}

impl DaemonProcess {
    pub fn spawn(executable: &Path) -> anyhow::Result<Self> {
        Self::spawn_configured(executable, DaemonExposureSettings::default())
    }

    fn spawn_configured(
        executable: &Path,
        settings: DaemonExposureSettings,
    ) -> anyhow::Result<Self> {
        let settings = settings.validate()?;
        let token = settings.token.clone();
        let app_executable = std::env::current_exe().context("could not locate Waku executable")?;
        let mut command = ProcessCommand::new(executable);
        // The desktop is a GUI-subsystem binary on Windows, so a console
        // child would get a console window of its own. `stderr` still reaches
        // the app's inherited handle.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command
            .arg("--bind")
            .arg(settings.bind_address())
            .arg("--parent-pid")
            .arg(std::process::id().to_string());
        if settings.enabled {
            command.arg("--allow-non-loopback");
        }
        for origin in &settings.allowed_origins {
            command.arg("--allow-origin").arg(origin);
        }
        let mut child = command
            .env(DAEMON_TOKEN_ENV, &token)
            .env(APP_EXECUTABLE_ENV, app_executable)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("could not launch {}", executable.display()))?;
        let stdout = child
            .stdout
            .take()
            .context("Waku daemon did not expose its readiness stream")?;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("waku-daemon-ready".into())
            .spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout)
                    .read_line(&mut line)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| {
                        if bytes == 0 {
                            bail!("Waku daemon exited before becoming ready")
                        }
                        serde_json::from_str::<DaemonReady>(&line).map_err(anyhow::Error::from)
                    });
                let _ = ready_tx.send(result);
            })
            .context("could not start Waku daemon readiness reader")?;
        let ready = match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("timed out waiting for Waku daemon: {error}");
            }
        };
        if ready.protocol_version != PROTOCOL_VERSION {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "daemon protocol {} does not match desktop protocol {}",
                ready.protocol_version,
                PROTOCOL_VERSION
            );
        }
        let client_address = match desktop_client_address(&ready.address) {
            Ok(address) => address,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let client = match DaemonClient::connect(&client_address, token) {
            Ok(client) => client,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            client,
            child,
            address: client_address,
        })
    }

    pub fn client(&self) -> DaemonClient {
        self.client.clone()
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub(crate) fn replace_client(&mut self, client: DaemonClient) {
        self.client = client;
    }

    #[cfg(test)]
    fn for_test(client: DaemonClient, address: String) -> Self {
        #[cfg(unix)]
        let child = std::process::Command::new("true")
            .spawn()
            .expect("`true` is required for tests");
        #[cfg(windows)]
        let child = std::process::Command::new("cmd")
            .args(["/c", "exit", "0"])
            .spawn()
            .expect("cmd is required for tests");
        Self {
            client,
            child,
            address,
        }
    }

    fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        self.client.shutdown();
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn desktop_client_address(address: &str) -> anyhow::Result<String> {
    let address = address
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("Waku daemon returned an invalid address {address:?}"))?;
    let ip = if address.ip().is_unspecified() {
        if address.is_ipv4() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        }
    } else {
        address.ip()
    };
    Ok(std::net::SocketAddr::new(ip, address.port()).to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutableStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl ExecutableStamp {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("could not inspect {}", path.display()))?;
        Ok(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}

struct SupervisorInner {
    executable: Option<PathBuf>,
    target: Mutex<DaemonTarget>,
    exposure: Mutex<Option<DaemonExposureSettings>>,
    restart: Mutex<()>,
    settings: Mutex<DaemonSettings>,
    persisted_settings: Mutex<Option<DaemonSettings>>,
    settings_updates: Sender<DaemonSettings>,
    client_updates: Mutex<Vec<Sender<DaemonClient>>>,
    running: AtomicBool,
}

enum DaemonTarget {
    Local {
        process: DaemonProcess,
        address: String,
        token: String,
    },
    Restarting(DaemonClient),
    Remote {
        client: DaemonClient,
        address: String,
        token: String,
    },
}

impl DaemonTarget {
    fn client(&self) -> DaemonClient {
        match self {
            Self::Local { process, .. } => process.client(),
            Self::Restarting(client) => client.clone(),
            Self::Remote { client, .. } => client.clone(),
        }
    }
}

/// Owns the current daemon and, in development, swaps it after a successful
/// rebuild without requiring the desktop process to relaunch.
#[derive(Clone)]
pub struct DaemonSupervisor {
    inner: Arc<SupervisorInner>,
}

impl DaemonSupervisor {
    pub fn spawn(executable: &Path, watch_for_rebuilds: bool) -> anyhow::Result<Self> {
        Self::spawn_configured(
            executable,
            watch_for_rebuilds,
            DaemonExposureSettings::default(),
        )
    }

    pub fn spawn_configured(
        executable: &Path,
        watch_for_rebuilds: bool,
        exposure: DaemonExposureSettings,
    ) -> anyhow::Result<Self> {
        let exposure = exposure.validate()?;
        let process = DaemonProcess::spawn_configured(executable, exposure.clone())?;
        let settings = read_settings(&process.client())?;
        let initial_stamp = ExecutableStamp::read(executable)?;
        let address = process.address().to_owned();
        let token = exposure.token.clone();
        let supervisor = Self::from_target(
            DaemonTarget::Local {
                process,
                address,
                token,
            },
            Some(executable.to_owned()),
            Some(exposure),
            settings,
        )?;
        let weak_inner = Arc::downgrade(&supervisor.inner);
        std::thread::Builder::new()
            .name("waku-daemon-supervisor".into())
            .spawn(move || monitor_daemon(weak_inner, Some(initial_stamp), watch_for_rebuilds))
            .context("could not start Waku daemon supervisor")?;
        Ok(supervisor)
    }

    /// Connect to a daemon managed on another host (or by an external local
    /// service manager). Dropping the desktop never shuts this daemon down.
    pub fn connect(address: &str, token: String) -> anyhow::Result<Self> {
        let client = DaemonClient::connect(address, token.clone())?;
        let settings = read_settings(&client)?;
        let supervisor = Self::from_target(
            DaemonTarget::Remote {
                client,
                address: address.to_owned(),
                token,
            },
            None,
            None,
            settings,
        )?;
        let weak_inner = Arc::downgrade(&supervisor.inner);
        std::thread::Builder::new()
            .name("waku-remote-daemon-supervisor".into())
            .spawn(move || monitor_daemon(weak_inner, None, false))
            .context("could not start remote Waku daemon supervisor")?;
        Ok(supervisor)
    }

    fn from_target(
        target: DaemonTarget,
        executable: Option<PathBuf>,
        exposure: Option<DaemonExposureSettings>,
        settings: DaemonSettings,
    ) -> anyhow::Result<Self> {
        let (settings_updates, settings_update_rx) = unbounded();
        let inner = Arc::new(SupervisorInner {
            executable,
            target: Mutex::new(target),
            exposure: Mutex::new(exposure),
            restart: Mutex::new(()),
            settings: Mutex::new(settings),
            // The desktop sends one normalized snapshot after it has migrated
            // the legacy combined settings document into app.json.
            persisted_settings: Mutex::new(None),
            settings_updates,
            client_updates: Mutex::new(Vec::new()),
            running: AtomicBool::new(true),
        });
        let weak_inner = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("waku-daemon-settings".into())
            .spawn(move || persist_settings(weak_inner, settings_update_rx))
            .context("could not start Waku daemon settings writer")?;
        Ok(Self { inner })
    }

    pub fn client(&self) -> DaemonClient {
        self.inner.target.lock().client()
    }

    /// Subscribe to the active daemon connection. The current client is sent
    /// immediately, followed by each replacement after a managed restart.
    pub fn subscribe_clients(&self) -> Receiver<DaemonClient> {
        let (updates, receiver) = unbounded();
        // Holding the target lock through registration makes the initial send
        // atomic with respect to replacement: a subscriber sees either the old
        // client followed by the new one, or the new client directly.
        let target = self.inner.target.lock();
        self.inner.client_updates.lock().push(updates.clone());
        let _ = updates.send(target.client());
        receiver
    }

    pub fn is_remote(&self) -> bool {
        self.inner.executable.is_none()
    }

    pub fn settings(&self) -> DaemonSettings {
        self.inner.settings.lock().clone()
    }

    /// Restart only the desktop-managed daemon with a new listener policy.
    /// The caller should run this off the UI thread.
    pub fn reconfigure(&self, exposure: DaemonExposureSettings) -> anyhow::Result<()> {
        let exposure = exposure.validate()?;
        let executable = self
            .inner
            .executable
            .as_ref()
            .context("the connected daemon is managed outside Waku Desktop")?
            .clone();
        let _restart = self.inner.restart.lock();
        let previous = self
            .inner
            .exposure
            .lock()
            .clone()
            .context("managed daemon launch settings are unavailable")?;
        match replace_local_daemon(&self.inner, &executable, &exposure) {
            Ok(()) => {
                *self.inner.exposure.lock() = Some(exposure);
                queue_settings_refresh(&self.inner);
                Ok(())
            }
            Err(error) => {
                let restore = replace_local_daemon(&self.inner, &executable, &previous);
                if restore.is_ok() {
                    queue_settings_refresh(&self.inner);
                    Err(error)
                } else {
                    Err(error.context(format!(
                        "the previous daemon configuration also failed to restart: {:#}",
                        restore.unwrap_err()
                    )))
                }
            }
        }
    }

    /// Queue a daemon settings update without blocking the desktop UI thread.
    pub fn update_settings(&self, settings: DaemonSettings) -> anyhow::Result<()> {
        *self.inner.settings.lock() = settings.clone();
        if self.inner.persisted_settings.lock().as_ref() == Some(&settings) {
            return Ok(());
        }
        self.inner
            .settings_updates
            .send(settings)
            .map_err(|_| anyhow::anyhow!("Waku daemon settings writer is closed"))
    }
}

impl Drop for DaemonSupervisor {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.inner.running.store(false, Ordering::Release);
        }
    }
}

fn local_reconnect_params(target: &DaemonTarget) -> Option<(String, String, Vec<ReplayCursor>)> {
    match target {
        DaemonTarget::Local {
            process,
            address,
            token,
        } if process.client().is_disconnected() => Some((
            address.clone(),
            token.clone(),
            process.client().last_sequences(),
        )),
        _ => None,
    }
}

fn replace_local_client(target: &mut DaemonTarget, replacement: DaemonClient) -> bool {
    match target {
        DaemonTarget::Local { process, .. } => {
            process.replace_client(replacement);
            true
        }
        _ => false,
    }
}

fn try_local_reconnect(
    target: &mut DaemonTarget,
    connect: &mut impl FnMut(&str, String, Vec<ReplayCursor>) -> anyhow::Result<DaemonClient>,
) -> Option<DaemonClient> {
    let (address, token, resume_from) = local_reconnect_params(target)?;
    let replacement = connect(&address, token, resume_from).ok()?;
    replace_local_client(target, replacement.clone()).then_some(replacement)
}

fn monitor_daemon(
    weak_inner: std::sync::Weak<SupervisorInner>,
    mut active_stamp: Option<ExecutableStamp>,
    watch_for_rebuilds: bool,
) {
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        let Some(inner) = weak_inner.upgrade() else {
            return;
        };
        if !inner.running.load(Ordering::Acquire) {
            return;
        }
        let remote_reconnect = {
            let target = inner.target.lock();
            match &*target {
                DaemonTarget::Remote {
                    client,
                    address,
                    token,
                } if client.is_disconnected() => Some((
                    client.clone(),
                    address.clone(),
                    token.clone(),
                    client.last_sequences(),
                )),
                _ => None,
            }
        };
        if let Some((disconnected, address, token, resume_from)) = remote_reconnect {
            let _restart = inner.restart.lock();
            let still_current = matches!(
                &*inner.target.lock(),
                DaemonTarget::Remote { client, .. }
                    if client.same_connection(&disconnected) && client.is_disconnected()
            );
            if !still_current {
                continue;
            }
            let Ok(replacement) =
                DaemonClient::connect_with_resume(&address, token.clone(), resume_from)
            else {
                continue;
            };
            *inner.target.lock() = DaemonTarget::Remote {
                client: replacement.clone(),
                address,
                token,
            };
            inner
                .client_updates
                .lock()
                .retain(|subscriber| subscriber.send(replacement.clone()).is_ok());
            continue;
        }
        let local_reconnect = {
            let _restart = inner.restart.lock();
            let mut target = inner.target.lock();
            let disconnected = match &*target {
                DaemonTarget::Local { process, .. } if process.client().is_disconnected() => {
                    Some(process.client().clone())
                }
                _ => None,
            };
            let Some(disconnected) = disconnected else {
                continue;
            };
            let still_current = matches!(
                &*target,
                DaemonTarget::Local { process, .. }
                    if process.client().same_connection(&disconnected)
                        && process.client().is_disconnected()
            );
            if !still_current {
                continue;
            }
            let mut connect = |address: &str, token: String, resume_from: Vec<ReplayCursor>| {
                DaemonClient::connect_with_resume(address, token, resume_from)
            };
            try_local_reconnect(&mut *target, &mut connect)
        };
        if let Some(replacement) = local_reconnect {
            inner
                .client_updates
                .lock()
                .retain(|subscriber| subscriber.send(replacement.clone()).is_ok());
            continue;
        }
        let process_exited = match &mut *inner.target.lock() {
            DaemonTarget::Local { process, .. } => process.has_exited(),
            DaemonTarget::Restarting(_) => true,
            DaemonTarget::Remote { .. } => continue,
        };
        let Some(executable) = inner.executable.as_ref() else {
            return;
        };
        let observed_stamp = ExecutableStamp::read(executable).ok();
        let executable_changed = watch_for_rebuilds
            && observed_stamp.is_some_and(|observed| Some(observed) != active_stamp);
        if !process_exited && !executable_changed {
            continue;
        }
        let _restart = inner.restart.lock();
        let Some(exposure) = inner.exposure.lock().clone() else {
            return;
        };
        match replace_local_daemon(&inner, executable, &exposure) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("could not restart rebuilt Waku daemon: {error:#}");
                continue;
            }
        }
        queue_settings_refresh(&inner);
        if let Some(observed_stamp) = observed_stamp {
            active_stamp = Some(observed_stamp);
        }
        drop(_restart);
        drop(inner);
    }
}

fn replace_local_daemon(
    inner: &SupervisorInner,
    executable: &Path,
    exposure: &DaemonExposureSettings,
) -> anyhow::Result<()> {
    let previous = {
        let mut target = inner.target.lock();
        match &*target {
            DaemonTarget::Remote { .. } => {
                bail!("the connected daemon is managed outside Waku Desktop")
            }
            DaemonTarget::Restarting(_) => None,
            DaemonTarget::Local { process, .. } => {
                let disconnected = process.client();
                let previous =
                    std::mem::replace(&mut *target, DaemonTarget::Restarting(disconnected));
                match previous {
                    DaemonTarget::Local { process, .. } => Some(process),
                    _ => unreachable!("local daemon target changed while locked"),
                }
            }
        }
    };
    // Dropping can wait briefly for graceful shutdown, but the target lock is
    // already released so UI actions never block behind process teardown.
    drop(previous);
    let replacement = DaemonProcess::spawn_configured(executable, exposure.clone())?;
    let client = replacement.client();
    let address = replacement.address().to_owned();
    let token = exposure.token.clone();
    *inner.target.lock() = DaemonTarget::Local {
        process: replacement,
        address,
        token,
    };
    inner
        .client_updates
        .lock()
        .retain(|subscriber| subscriber.send(client.clone()).is_ok());
    Ok(())
}

fn queue_settings_refresh(inner: &SupervisorInner) {
    let settings = inner.settings.lock().clone();
    *inner.persisted_settings.lock() = None;
    let _ = inner.settings_updates.send(settings);
}

fn read_settings(client: &DaemonClient) -> anyhow::Result<DaemonSettings> {
    match client.request(Uuid::nil(), Uuid::nil(), Command::GetSettings)? {
        ResponsePayload::Settings { settings } => Ok(settings),
        _ => bail!("Waku daemon returned an invalid settings response"),
    }
}

fn persist_settings(
    weak_inner: std::sync::Weak<SupervisorInner>,
    updates: Receiver<DaemonSettings>,
) {
    while let Ok(mut settings) = updates.recv() {
        while let Ok(newer) = updates.try_recv() {
            settings = newer;
        }
        loop {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            if !inner.running.load(Ordering::Acquire) {
                return;
            }
            let desired = inner.settings.lock().clone();
            if desired != settings {
                settings = desired;
            }
            let client = inner.target.lock().client();
            let result = client.request(
                Uuid::nil(),
                Uuid::nil(),
                Command::UpdateSettings {
                    settings: settings.clone(),
                },
            );
            match result {
                Ok(ResponsePayload::Ack) => {
                    *inner.persisted_settings.lock() = Some(settings);
                    break;
                }
                Ok(_) => {
                    eprintln!("Waku daemon returned an invalid settings update response");
                }
                Err(error) => {
                    eprintln!("could not persist Waku daemon settings: {error:#}");
                }
            }
            drop(inner);
            std::thread::sleep(REBUILD_POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_origins_are_exact_and_deduplicated() {
        assert_eq!(
            parse_allowed_origins(
                "https://app.waku.test, http://localhost:3001, https://app.waku.test"
            )
            .unwrap(),
            ["https://app.waku.test", "http://localhost:3001"]
        );
        assert!(parse_allowed_origins("https://app.waku.test/path").is_err());
        assert!(parse_allowed_origins("ws://app.waku.test").is_err());
    }

    #[test]
    fn desktop_uses_loopback_to_reach_an_unspecified_listener() {
        assert_eq!(
            desktop_client_address("0.0.0.0:34123").unwrap(),
            "127.0.0.1:34123"
        );
        assert_eq!(desktop_client_address("[::]:34123").unwrap(), "[::1]:34123");
    }

    #[test]
    fn local_reconnect_params_requires_disconnected_client() {
        let disconnected = DaemonClient::disconnected_for_test(vec![ReplayCursor {
            session_id: Uuid::from_u128(1),
            runtime_id: Uuid::from_u128(2),
            epoch: Uuid::from_u128(3),
            sequence: 5,
        }]);
        let local = DaemonTarget::Local {
            process: DaemonProcess::for_test(disconnected.clone(), "127.0.0.1:1234".into()),
            address: "127.0.0.1:1234".into(),
            token: "token-a".into(),
        };
        let (address, token, resume_from) = local_reconnect_params(&local).unwrap();
        assert_eq!(address, "127.0.0.1:1234");
        assert_eq!(token, "token-a");
        assert_eq!(resume_from.len(), 1);
        assert_eq!(resume_from[0].session_id, Uuid::from_u128(1));
        assert_eq!(resume_from[0].runtime_id, Uuid::from_u128(2));
        assert_eq!(resume_from[0].epoch, Uuid::from_u128(3));
        assert_eq!(resume_from[0].sequence, 5);
    }

    #[test]
    fn local_reconnect_params_skips_connected_and_remote_targets() {
        // Remote target is never considered for local reconnect.
        let remote = DaemonTarget::Remote {
            client: DaemonClient::disconnected_for_test(Vec::new()),
            address: "127.0.0.1:1234".into(),
            token: "token-a".into(),
        };
        assert!(local_reconnect_params(&remote).is_none());
    }

    #[test]
    fn replace_local_client_swaps_the_client() {
        let original = DaemonClient::disconnected_for_test(Vec::new());
        let replacement = DaemonClient::disconnected_for_test(Vec::new());
        let mut local = DaemonTarget::Local {
            process: DaemonProcess::for_test(original.clone(), "127.0.0.1:1234".into()),
            address: "127.0.0.1:1234".into(),
            token: "token-a".into(),
        };
        assert!(replace_local_client(&mut local, replacement.clone()));
        match local {
            DaemonTarget::Local { process, .. } => {
                assert!(process.client().same_connection(&replacement));
                assert!(!process.client().same_connection(&original));
            }
            _ => panic!("expected local target"),
        }
    }

    #[test]
    fn try_local_reconnect_uses_injected_connector() {
        let session_id = Uuid::from_u128(1);
        let runtime_id = Uuid::from_u128(2);
        let epoch = Uuid::from_u128(3);
        let original = DaemonClient::disconnected_for_test(vec![ReplayCursor {
            session_id,
            runtime_id,
            epoch,
            sequence: 5,
        }]);
        let replacement = DaemonClient::disconnected_for_test(Vec::new());
        let mut local = DaemonTarget::Local {
            process: DaemonProcess::for_test(original.clone(), "127.0.0.1:1234".into()),
            address: "127.0.0.1:1234".into(),
            token: "token-a".into(),
        };

        let mut calls: Vec<(String, String, Vec<ReplayCursor>)> = Vec::new();
        let replacement_to_return = replacement.clone();
        let mut connect = |address: &str, token: String, resume_from: Vec<ReplayCursor>| {
            calls.push((address.to_string(), token, resume_from));
            Ok(replacement_to_return.clone())
        };

        let result = try_local_reconnect(&mut local, &mut connect).unwrap();
        assert!(result.same_connection(&replacement));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "127.0.0.1:1234");
        assert_eq!(calls[0].1, "token-a");
        assert_eq!(calls[0].2.len(), 1);
        assert_eq!(calls[0].2[0].session_id, session_id);
        assert_eq!(calls[0].2[0].sequence, 5);

        match local {
            DaemonTarget::Local { process, .. } => {
                assert!(process.client().same_connection(&replacement));
            }
            _ => panic!("expected local target"),
        }
    }

    #[test]
    fn try_local_reconnect_returns_none_when_already_connected() {
        // We cannot create a genuinely connected client without a server, but we
        // can simulate the "no reconnect needed" case by using a Remote target.
        let mut remote = DaemonTarget::Remote {
            client: DaemonClient::disconnected_for_test(Vec::new()),
            address: "127.0.0.1:1234".into(),
            token: "token-a".into(),
        };
        let mut connect = |_address: &str, _token: String, _resume_from: Vec<ReplayCursor>| {
            panic!("connector should not be called")
        };
        assert!(try_local_reconnect(&mut remote, &mut connect).is_none());
    }
}

//! Signed, rollback-safe updates for Waku's managed Linux tarball install.
//!
//! Checks, downloads, signature verification, and extraction all run on a
//! worker thread. Once an archive is fully staged, `waku-updater` validates
//! both prefixes and acknowledges the handoff before the UI begins a normal
//! quit. The helper then swaps the directories and only removes the previous
//! build after the replacement opens its main window.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use flate2::read::GzDecoder;

use super::feed::{self, AppcastItem};
use super::{UpdateStatus, UpdaterEvent};

const PUBLIC_ED_KEY: &str = env!("WAKU_SPARKLE_PUBLIC_ED_KEY");
const MANAGED_MARKER: &str = "share/waku/self-update-v1";
const MANAGED_MARKER_CONTENTS: &str = "waku-self-update-v1\n";
const HELPER_EXECUTABLE: &str = "waku-updater";
const RELAUNCH_READY_ENV: &str = "WAKU_UPDATE_READY_FILE";
const MAX_FEED_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ERROR_BYTES: u64 = 16 * 1024;

static TEMPORARY_NONCE: AtomicU64 = AtomicU64::new(0);

#[cfg(target_arch = "aarch64")]
const FEED_URL: Option<&str> = Some("https://releases.waku.sh/appcast-linux-aarch64.xml");
#[cfg(target_arch = "x86_64")]
const FEED_URL: Option<&str> = Some("https://releases.waku.sh/appcast-linux-x86_64.xml");
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
const FEED_URL: Option<&str> = None;

#[derive(Clone, Debug)]
struct InstallLayout {
    prefix: PathBuf,
    parent: PathBuf,
    helper: PathBuf,
    prefix_name: String,
}

impl InstallLayout {
    fn discover() -> Option<Self> {
        // A root-owned desktop session must not turn the app into a package
        // manager. Supported self-updating installs always live under the
        // invoking user's home directory.
        if unsafe { libc::geteuid() } == 0 {
            return None;
        }

        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        if executable.file_name()? != "waku" || executable.parent()?.file_name()? != "bin" {
            return None;
        }
        let prefix = executable.parent()?.parent()?.to_path_buf();
        let parent = prefix.parent()?.canonicalize().ok()?;
        let home = dirs::home_dir()?.canonicalize().ok()?;
        if !prefix.starts_with(&home) {
            return None;
        }
        let prefix_name = prefix.file_name()?.to_str()?.to_owned();
        let helper = prefix.join("bin").join(HELPER_EXECUTABLE);
        let layout = Self {
            prefix,
            parent,
            helper,
            prefix_name,
        };
        validate_packaged_layout(&layout.prefix).ok()?;

        // The swap needs write access to the parent, not merely to files in
        // the current prefix. Probe once during initialization, never from a
        // render path.
        let probe = create_unique_sibling(&layout, "update-probe").ok()?;
        fs::remove_dir(&probe).ok()?;
        Some(layout)
    }

    fn sibling_name(&self, kind: &str, nonce: u64) -> String {
        format!(
            ".{}.{kind}-{}-{nonce}",
            self.prefix_name,
            std::process::id()
        )
    }

    fn error_path(&self) -> PathBuf {
        self.parent
            .join(format!(".{}.update-error", self.prefix_name))
    }
}

/// A fully verified and unpacked replacement prefix. Dropping an update that
/// was never handed to the helper reclaims its staging directory.
struct StagedUpdate {
    directory: Option<PathBuf>,
}

impl StagedUpdate {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
        }
    }

    fn path(&self) -> &Path {
        self.directory
            .as_deref()
            .expect("a staged update is present until handoff")
    }

    fn disarm(mut self) -> PathBuf {
        self.directory
            .take()
            .expect("a staged update can only be handed off once")
    }
}

impl Drop for StagedUpdate {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

pub struct Updater {
    layout: InstallLayout,
    status: Arc<Mutex<UpdateStatus>>,
    staged: Arc<Mutex<Option<StagedUpdate>>>,
    checking: Arc<AtomicBool>,
    explicit_check: Arc<AtomicBool>,
    automatic: Arc<AtomicBool>,
    preference_path: PathBuf,
    events: smol::channel::Sender<UpdaterEvent>,
    receiver: smol::channel::Receiver<UpdaterEvent>,
}

impl Updater {
    pub fn init() -> Option<Self> {
        let forced = std::env::var_os("WAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
        if cfg!(debug_assertions) && !forced {
            return None;
        }
        FEED_URL?;
        if verifying_key().is_none() {
            eprintln!("Waku updater: SUPublicEDKey is not a valid ed25519 key");
            return None;
        }

        let layout = InstallLayout::discover()?;
        let preference_path = preference_path()?;
        let automatic = Arc::new(AtomicBool::new(read_automatic_preference(&preference_path)));
        let (events, receiver) = smol::channel::unbounded();
        let updater = Self {
            layout,
            status: Arc::new(Mutex::new(UpdateStatus::Idle)),
            staged: Arc::new(Mutex::new(None)),
            checking: Arc::new(AtomicBool::new(false)),
            explicit_check: Arc::new(AtomicBool::new(false)),
            automatic,
            preference_path,
            events,
            receiver,
        };

        if let Some(error) = take_update_error(&updater.layout) {
            let _ = updater.events.try_send(UpdaterEvent::Failed(error));
        }
        if updater.automatically_checks_for_updates() {
            updater.start_check(false);
        }
        Some(updater)
    }

    pub fn check_for_updates(&self) {
        self.start_check(true);
    }

    fn start_check(&self, user_initiated: bool) {
        if self.status() == UpdateStatus::Updating {
            return;
        }
        if self
            .checking
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            if user_initiated {
                self.explicit_check.store(true, Ordering::Relaxed);
            }
            return;
        }
        self.explicit_check.store(user_initiated, Ordering::Relaxed);

        let layout = self.layout.clone();
        let status = self.status.clone();
        let staged = self.staged.clone();
        let checking = self.checking.clone();
        let explicit_check = self.explicit_check.clone();
        let events = self.events.clone();
        let publish_events = events.clone();
        let publish = move |next: UpdateStatus| {
            let mut status = status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *status == next {
                return;
            }
            *status = next;
            let _ = publish_events.try_send(UpdaterEvent::StatusChanged(next));
        };
        let spawned = std::thread::Builder::new()
            .name("waku-updater-check".into())
            .spawn(move || {
                let outcome = fetch_and_stage(&layout);
                let report = explicit_check.load(Ordering::Relaxed);
                match outcome {
                    Ok(Some(update)) => {
                        *staged
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(update);
                        publish(UpdateStatus::Available);
                    }
                    Ok(None) => {
                        publish(UpdateStatus::Idle);
                        if report {
                            let _ = events.try_send(UpdaterEvent::UpToDate);
                        }
                    }
                    Err(error) => {
                        publish(UpdateStatus::Idle);
                        if report {
                            let _ = events.try_send(UpdaterEvent::Failed(error.to_string()));
                        } else {
                            eprintln!("Waku updater: {error:#}");
                        }
                    }
                }
                checking.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.checking.store(false, Ordering::Release);
        }
    }

    /// Start the helper, then wait off the UI thread for its validation
    /// acknowledgement. Only that acknowledgement emits `QuitAndInstall`.
    pub fn install_available_update(&self) -> bool {
        let update = {
            let mut staged = self
                .staged
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match staged.take() {
                Some(update) => update,
                None => return false,
            }
        };

        let (handoff_tx, handoff_rx) = std::sync::mpsc::sync_channel::<(Child, StagedUpdate)>(1);
        let status = self.status.clone();
        let events = self.events.clone();
        if std::thread::Builder::new()
            .name("waku-updater-handoff".into())
            .spawn(move || {
                let Ok((mut child, update)) = handoff_rx.recv() else {
                    return;
                };
                let mut acknowledgement = String::new();
                let read = child
                    .stdout
                    .take()
                    .map(BufReader::new)
                    .and_then(|mut stdout| stdout.read_line(&mut acknowledgement).ok());
                if read.is_some() && acknowledgement.trim_end() == "READY" {
                    let _staging = update.disarm();
                    let _ = events.try_send(UpdaterEvent::QuitAndInstall);
                    let _ = child.wait();
                    return;
                }

                let _ = child.kill();
                let _ = child.wait();
                *status
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = UpdateStatus::Idle;
                let reason = acknowledgement
                    .strip_prefix("ERROR\t")
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("the update helper did not accept the staged build");
                let _ = events.try_send(UpdaterEvent::Failed(reason.to_owned()));
            })
            .is_err()
        {
            self.fail_handoff("could not start the update handoff worker");
            return false;
        }

        let mut command = Command::new(&self.layout.helper);
        command
            .arg("--install-dir")
            .arg(&self.layout.prefix)
            .arg("--staged-dir")
            .arg(update.path())
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.fail_handoff(format!("could not start the update helper: {error}"));
                return false;
            }
        };

        self.set_status(UpdateStatus::Updating);
        if let Err(error) = handoff_tx.send((child, update)) {
            let (mut child, _update) = error.0;
            let _ = child.kill();
            let _ = child.wait();
            self.fail_handoff("the update handoff worker stopped unexpectedly");
            return false;
        }
        true
    }

    pub fn status(&self) -> UpdateStatus {
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
        self.receiver.clone()
    }

    pub fn automatically_checks_for_updates(&self) -> bool {
        self.automatic.load(Ordering::Relaxed)
    }

    pub fn set_automatically_checks_for_updates(&self, enabled: bool) {
        if self.automatic.swap(enabled, Ordering::Relaxed) == enabled {
            return;
        }
        let path = self.preference_path.clone();
        let _ = std::thread::Builder::new()
            .name("waku-updater-preference".into())
            .spawn(move || write_automatic_preference(&path, enabled));
        if enabled {
            self.start_check(false);
        }
    }

    fn set_status(&self, next: UpdateStatus) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *status == next {
            return;
        }
        *status = next;
        let _ = self.events.try_send(UpdaterEvent::StatusChanged(next));
    }

    fn fail_handoff(&self, error: impl Into<String>) {
        self.set_status(UpdateStatus::Idle);
        let _ = self.events.try_send(UpdaterEvent::Failed(error.into()));
    }
}

fn fetch_and_stage(layout: &InstallLayout) -> anyhow::Result<Option<StagedUpdate>> {
    let document = http_get(FEED_URL.expect("supported Linux architecture has a feed"))?;
    let Some(item) = feed::newest_item(&document) else {
        anyhow::bail!("the update feed has no signed release");
    };
    if !feed::is_newer(&item.version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    validate_release_version(&item.version)?;
    validate_download_url(&item.url)?;
    let declared_length = item
        .length
        .filter(|length| *length > 0 && *length <= MAX_ARCHIVE_BYTES)
        .ok_or_else(|| anyhow::anyhow!("the update feed has an invalid archive length"))?;

    let directory = create_unique_sibling(layout, "update")?;
    let staged = StagedUpdate::new(directory);
    let archive_path = staged.path().join("update.tar.gz");
    download_to(&item.url, &archive_path, 600, MAX_ARCHIVE_BYTES)?;
    let actual_length = fs::metadata(&archive_path)?.len();
    anyhow::ensure!(
        actual_length == declared_length,
        "the update archive does not match the length the feed declared"
    );
    verify_archive(&archive_path, &item)?;
    extract_release_archive(&archive_path, staged.path(), &item.version)?;
    fs::remove_file(&archive_path)?;
    validate_packaged_layout(staged.path())?;
    Ok(Some(staged))
}

fn verify_archive(path: &Path, item: &AppcastItem) -> anyhow::Result<()> {
    let key = verifying_key().ok_or_else(|| anyhow::anyhow!("SUPublicEDKey is unusable"))?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(item.signature.trim())
        .ok()
        .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
        .map(|bytes| Signature::from_bytes(&bytes))
        .ok_or_else(|| anyhow::anyhow!("update signature is malformed"))?;
    let bytes = fs::read(path)?;
    key.verify_strict(&bytes, &signature)
        .map_err(|_| anyhow::anyhow!("update archive failed signature verification"))
}

fn verifying_key() -> Option<VerifyingKey> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(PUBLIC_ED_KEY.trim())
        .ok()?;
    VerifyingKey::from_bytes(&<[u8; 32]>::try_from(bytes).ok()?).ok()
}

fn extract_release_archive(path: &Path, destination: &Path, version: &str) -> anyhow::Result<()> {
    let triple =
        target_triple().ok_or_else(|| anyhow::anyhow!("unsupported Linux architecture"))?;
    let expected_root = format!("waku-{version}-{triple}");
    let decoder = GzDecoder::new(File::open(path)?);
    let mut archive = tar::Archive::new(decoder);
    let mut seen = HashSet::new();
    let mut unpacked_bytes = 0_u64;
    let mut entry_count = 0_usize;

    for entry in archive.entries()? {
        let mut entry = entry?;
        entry_count += 1;
        anyhow::ensure!(
            entry_count <= MAX_ARCHIVE_ENTRIES,
            "update archive contains too many entries"
        );
        let archived_path = entry.path()?.into_owned();
        let mut components = archived_path.components();
        let root = match components.next() {
            Some(Component::Normal(root)) => root,
            _ => anyhow::bail!("update archive contains an invalid path"),
        };
        anyhow::ensure!(
            root == OsStr::new(&expected_root),
            "update archive has an unexpected top-level directory"
        );

        let mut relative = PathBuf::new();
        for component in components {
            match component {
                Component::Normal(component) => relative.push(component),
                _ => anyhow::bail!("update archive contains an unsafe path"),
            }
        }
        if relative.as_os_str().is_empty() {
            anyhow::ensure!(
                entry.header().entry_type().is_dir(),
                "update archive root is not a directory"
            );
            continue;
        }
        anyhow::ensure!(
            seen.insert(relative.clone()),
            "update archive contains a duplicate path"
        );
        let entry_type = entry.header().entry_type();
        anyhow::ensure!(
            entry_type.is_file() || entry_type.is_dir(),
            "update archive contains a link or special file"
        );
        unpacked_bytes = unpacked_bytes
            .checked_add(entry.header().size()?)
            .ok_or_else(|| anyhow::anyhow!("update archive size overflow"))?;
        anyhow::ensure!(
            unpacked_bytes <= MAX_UNPACKED_BYTES,
            "update archive expands beyond the safety limit"
        );
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(output)?;
    }
    Ok(())
}

fn validate_packaged_layout(prefix: &Path) -> anyhow::Result<()> {
    let marker = prefix.join(MANAGED_MARKER);
    let marker_metadata = fs::symlink_metadata(&marker)?;
    anyhow::ensure!(
        marker_metadata.file_type().is_file()
            && fs::read_to_string(&marker).ok().as_deref() == Some(MANAGED_MARKER_CONTENTS),
        "the install is not marked as a Waku-managed tarball"
    );
    for executable in ["waku", "waku-daemon", HELPER_EXECUTABLE] {
        let path = prefix.join("bin").join(executable);
        let metadata = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.file_type().is_file() && metadata.permissions().mode() & 0o111 != 0,
            "the update is missing executable bin/{executable}"
        );
    }
    Ok(())
}

fn validate_release_version(version: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !version.is_empty()
            && version.len() <= 64
            && version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_')
            }),
        "the update feed contains an invalid version"
    );
    Ok(())
}

fn validate_download_url(value: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(value)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("releases.waku.sh")
            && url.username().is_empty()
            && url.password().is_none(),
        "the update feed points outside releases.waku.sh"
    );
    Ok(())
}

fn target_triple() -> Option<&'static str> {
    #[cfg(target_arch = "aarch64")]
    {
        return Some("aarch64-unknown-linux-gnu");
    }
    #[cfg(target_arch = "x86_64")]
    {
        return Some("x86_64-unknown-linux-gnu");
    }
    #[allow(unreachable_code)]
    None
}

fn create_unique_sibling(layout: &InstallLayout, kind: &str) -> anyhow::Result<PathBuf> {
    for _ in 0..100 {
        let nonce = TEMPORARY_NONCE.fetch_add(1, Ordering::Relaxed);
        let candidate = layout.parent.join(layout.sibling_name(kind, nonce));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("could not reserve an update staging directory")
}

struct TemporaryFile(PathBuf);

impl TemporaryFile {
    fn create() -> anyhow::Result<Self> {
        for _ in 0..100 {
            let nonce = TEMPORARY_NONCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "waku-update-feed-{}-{nonce}.xml",
                std::process::id()
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("could not reserve a temporary update-feed file")
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn http_get(url: &str) -> anyhow::Result<String> {
    let temporary = TemporaryFile::create()?;
    download_to(url, &temporary.0, 30, MAX_FEED_BYTES)?;
    let bytes = fs::read(&temporary.0)?;
    Ok(String::from_utf8(bytes)?)
}

fn download_to(
    url: &str,
    destination: &Path,
    timeout_seconds: u64,
    limit: u64,
) -> anyhow::Result<()> {
    let timeout = timeout_seconds.to_string();
    let limit_argument = limit.to_string();
    let curl = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            &timeout,
            "--max-filesize",
            &limit_argument,
            "-o",
        ])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match curl {
        Ok(output) if output.status.success() => {}
        Ok(output) => anyhow::bail!(
            "could not download the update: {}",
            concise_stderr(&output.stderr)
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let output = Command::new("wget")
                .arg("--quiet")
                .arg("--https-only")
                .arg(format!("--timeout={timeout}"))
                .arg("--tries=2")
                .arg("--max-redirect=10")
                .arg("-O")
                .arg(destination)
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|wget_error| {
                    anyhow::anyhow!("neither curl nor wget could start ({error}; {wget_error})")
                })?;
            anyhow::ensure!(
                output.status.success(),
                "could not download the update: {}",
                concise_stderr(&output.stderr)
            );
        }
        Err(error) => return Err(error.into()),
    }
    let length = fs::metadata(destination)?.len();
    anyhow::ensure!(
        length <= limit,
        "the downloaded update exceeds its safety limit"
    );
    Ok(())
}

fn concise_stderr(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(512)
        .collect()
}

fn preference_path() -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join(waku_protocol::identity::DATA_DIRECTORY_NAME)
            .join("updater.json"),
    )
}

fn read_automatic_preference(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return true;
    };
    serde_json::from_str::<serde_json::Value>(&contents)
        .ok()
        .and_then(|value| value.get("automatic")?.as_bool())
        .unwrap_or(true)
}

fn write_automatic_preference(path: &Path, enabled: bool) {
    let Some(directory) = path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let temporary = path.with_extension("json.tmp");
    let written = File::create(&temporary).and_then(|mut file| {
        file.write_all(format!("{{\n  \"automatic\": {enabled}\n}}\n").as_bytes())?;
        file.sync_all()
    });
    if written.is_ok() {
        let _ = fs::rename(&temporary, path);
    } else {
        let _ = fs::remove_file(&temporary);
    }
}

fn take_update_error(layout: &InstallLayout) -> Option<String> {
    let path = layout.error_path();
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_ERROR_BYTES {
        return None;
    }
    let mut contents = String::new();
    File::open(&path)
        .ok()?
        .take(MAX_ERROR_BYTES)
        .read_to_string(&mut contents)
        .ok()?;
    let _ = fs::remove_file(path);
    let contents = contents.trim();
    (!contents.is_empty()).then(|| contents.to_owned())
}

/// Signal startup success through a create-new file scoped to the managed
/// prefix's parent. Arbitrary paths supplied through the environment are
/// ignored, and an existing file is never overwritten or followed.
pub(crate) fn signal_relaunch_ready() {
    let Some(path) = std::env::var_os(RELAUNCH_READY_ENV).map(PathBuf::from) else {
        return;
    };
    let Some((parent, ready_prefix)) = current_ready_scope() else {
        return;
    };
    if path.parent() != Some(parent.as_path())
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&ready_prefix))
    {
        return;
    }
    let _ = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(b"ready\n"));
}

fn current_ready_scope() -> Option<(PathBuf, String)> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let prefix = executable.parent()?.parent()?;
    let parent = prefix.parent()?.canonicalize().ok()?;
    let prefix_name = prefix.file_name()?.to_str()?;
    Some((parent, format!(".{prefix_name}.update-ready-")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "waku-linux-updater-{label}-{}-{}",
            std::process::id(),
            TEMPORARY_NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn the_embedded_public_key_is_usable() {
        assert!(verifying_key().is_some());
    }

    #[test]
    fn release_versions_cannot_become_paths() {
        assert!(validate_release_version("1.2.3-beta.1").is_ok());
        assert!(validate_release_version("../../tmp/payload").is_err());
        assert!(validate_release_version("1.2.3/evil").is_err());
        assert!(validate_release_version("").is_err());
    }

    #[test]
    fn extraction_strips_only_the_expected_release_root() {
        let directory = temporary_directory("archive");
        let archive_path = directory.join("release.tar.gz");
        let output = directory.join("output");
        fs::create_dir(&output).unwrap();
        let triple = target_triple().unwrap();
        let root = format!("waku-9.8.7-{triple}");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        for (relative, contents, mode) in [
            ("bin/waku", b"app".as_slice(), 0o755),
            ("bin/waku-daemon", b"daemon".as_slice(), 0o755),
            ("bin/waku-updater", b"helper".as_slice(), 0o755),
            (MANAGED_MARKER, MANAGED_MARKER_CONTENTS.as_bytes(), 0o644),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive
                .append_data(&mut header, format!("{root}/{relative}"), contents)
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();

        extract_release_archive(&archive_path, &output, "9.8.7").unwrap();
        validate_packaged_layout(&output).unwrap();
        assert_eq!(fs::read(output.join("bin/waku")).unwrap(), b"app");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_absent_preference_file_keeps_checks_enabled() {
        let directory = temporary_directory("preference");
        let path = directory.join("updater.json");
        assert!(read_automatic_preference(&path));
        write_automatic_preference(&path, false);
        assert!(!read_automatic_preference(&path));
        write_automatic_preference(&path, true);
        assert!(read_automatic_preference(&path));
        fs::remove_dir_all(directory).unwrap();
    }
}
